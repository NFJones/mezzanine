//! Expiry-fenced lease evidence and real idle renewal over synthetic Unix IPC.
//!
//! Tests never mint a production capability or run a provider. Kernel peer
//! authentication and the production transport remain in use; the wall clock
//! is injected per worker so no process-global time or environment is changed.

use super::*;
use crate::integrations::bootstrap::pi_owner::LifecycleOwner;
use secrecy::SecretString;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Exact lease identity cannot change, outlive the bounded server interval, or
/// be revived by delayed acknowledgments after the local monotonic fence.
#[tokio::test(start_paused = true)]
async fn pi_renewal_lease_evidence_rejects_expiry_identity_and_extension() {
    let start = Instant::now();
    let ack = |id: &str, expires_at| LeaseAck {
        agent_id: id.into(),
        expires_at,
    };
    let lease = ActiveLease::accept(ack("agent", 160), 100, start, None).unwrap();
    assert!(lease.is_current());
    for expires in [99, 100, 101, 161] {
        assert!(ActiveLease::accept(ack("agent", expires), 100, start, None).is_err());
    }
    assert!(ActiveLease::accept(ack("other", 160), 100, start, Some(&lease)).is_err());
    assert!(ActiveLease::accept(ack("agent", 160), 100, start, Some(&lease)).is_err());
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    assert!(ActiveLease::accept(ack("other", 161), 101, Instant::now(), Some(&lease)).is_err());
    assert!(ActiveLease::accept(ack("agent", 161), 101, Instant::now(), Some(&lease)).is_ok());
    assert!(ActiveLease::accept(ack("agent", 160), 100, Instant::now(), Some(&lease)).is_err());
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    assert!(!lease.is_current());
    assert!(ActiveLease::accept(ack("agent", 220), 160, Instant::now(), Some(&lease)).is_err());
    assert!(ActiveLease::accept(ack("agent", 160), 100, start, None).is_err());
}

/// Test-owned endpoint and explicitly supplied synthetic authority.
struct Fixture {
    root: std::path::PathBuf,
    listener: tokio::net::UnixListener,
    transport: CapabilityTransport,
}

impl Fixture {
    /// Opens only this unique temporary endpoint, not a user's daemon.
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mez-pi-renew-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&root).unwrap();
        let socket = root.join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let owner = LifecycleOwner::new("bound").unwrap();
        let transport =
            CapabilityTransport::new(&socket, SecretString::from("x".repeat(43)), 1, &owner)
                .unwrap();
        Self {
            root,
            listener,
            transport,
        }
    }
}

impl Drop for Fixture {
    /// Cleanup affects only the test-owned directory after tasks settle.
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

/// Reads the original single framed request without primary initialization.
async fn request(listener: &tokio::net::UnixListener) -> (tokio::net::UnixStream, String) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0 && bytes.len() + count <= 65536);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some((frame, _)) =
            crate::protocol::framing::decode_frame_incremental(&bytes, 65536).unwrap()
        {
            let value: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
            assert_eq!(value["params"]["external_session_id"], "bound");
            return (stream, value["method"].as_str().unwrap().into());
        }
    }
}

/// Sends one exact synthetic lease response using the production wire shape.
async fn reply(stream: &mut tokio::net::UnixStream, id: &str, expires: u64) {
    let body = serde_json::json!({"jsonrpc":"2.0","id":"pi-lifecycle","result":{
        "registered":true,"agent_id":id,"generation":1,"expires_at_unix_seconds":expires,
    }})
    .to_string();
    stream
        .write_all(&crate::control::encode_control_body(&body))
        .await
        .unwrap();
}

/// Idle silence must still renew once due. Explicit cancellation clears the
/// published lease and settles the future without another request or retry.
#[tokio::test(flavor = "current_thread")]
async fn pi_renewal_idle_worker_renews_and_cancellation_clears_publication() {
    let fixture = Fixture::new();
    let (status, mut receiver) = watch::channel(None);
    let (stop, cancellation) = watch::channel(false);
    let worker = run_with_clock(&fixture.transport, "Pi", status, cancellation, || Some(100));
    let server = async {
        let (mut stream, method) = request(&fixture.listener).await;
        assert_eq!(method, "agent/external/register");
        reply(&mut stream, "agent", 102).await;
        drop(stream);
        receiver.changed().await.unwrap();
        assert!(receiver.borrow().as_ref().unwrap().is_current());
        let (mut stream, method) = request(&fixture.listener).await;
        assert_eq!(method, "agent/external/renew");
        reply(&mut stream, "agent", 103).await;
        drop(stream);
        receiver.changed().await.unwrap();
        assert!(receiver.borrow().as_ref().unwrap().is_current());
        stop.send(true).unwrap();
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(worker, server)
    })
    .await
    .unwrap();
    result.unwrap();
    assert!(receiver.borrow().is_none());
}

/// Changed renewal identity terminates publication instead of extending another
/// registration. A failure has no automatic retry and cannot emit peer content.
#[tokio::test(flavor = "current_thread")]
async fn pi_renewal_changed_identity_stops_without_retry() {
    let fixture = Fixture::new();
    let (status, receiver) = watch::channel(None);
    let (_stop, cancellation) = watch::channel(false);
    let worker = run_with_clock(&fixture.transport, "Pi", status, cancellation, || Some(100));
    let server = async {
        let (mut stream, _) = request(&fixture.listener).await;
        reply(&mut stream, "agent", 102).await;
        drop(stream);
        let (mut stream, method) = request(&fixture.listener).await;
        assert_eq!(method, "agent/external/renew");
        reply(&mut stream, "other", 103).await;
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(worker, server)
    })
    .await
    .unwrap();
    assert_eq!(
        result.unwrap_err().message(),
        "Pi renewal lease unavailable"
    );
    assert!(receiver.borrow().is_none());
}

/// Dropping publication and explicit launcher cancellation clear ownership;
/// a pre-cancelled worker must not connect or register anything.
#[tokio::test(flavor = "current_thread")]
async fn pi_renewal_drop_and_precancellation_retire_availability() {
    let (status, receiver) = watch::channel(Some(ActiveLease {
        agent_id: "agent".into(),
        server_expiry: 160,
        expires: Instant::now() + std::time::Duration::from_secs(60),
    }));
    drop(Publication(status));
    assert!(receiver.borrow().is_none());
    let fixture = Fixture::new();
    let (status, receiver) = watch::channel(None);
    let (stop, cancellation) = watch::channel(true);
    run_with_clock(&fixture.transport, "Pi", status, cancellation, || Some(100))
        .await
        .unwrap();
    assert!(receiver.borrow().is_none());
    drop(stop);
}

/// Closing a live launcher's cancellation channel retires local publication
/// while idle. A renewal peer losing its reply likewise terminates the worker,
/// without a new request or falsely claiming remote nonexecution.
#[tokio::test(flavor = "current_thread")]
async fn pi_renewal_live_channel_closure_and_lost_reply_clear_publication() {
    for lost_reply in [false, true] {
        let fixture = Fixture::new();
        let (status, mut receiver) = watch::channel(None);
        let (stop, cancellation) = watch::channel(false);
        let worker = run_with_clock(&fixture.transport, "Pi", status, cancellation, || Some(100));
        let server = async {
            let (mut stream, _) = request(&fixture.listener).await;
            reply(&mut stream, "agent", 102).await;
            drop(stream);
            receiver.changed().await.unwrap();
            assert!(receiver.borrow().as_ref().unwrap().is_current());
            if lost_reply {
                let (stream, method) = request(&fixture.listener).await;
                assert_eq!(method, "agent/external/renew");
                drop(stream);
                receiver.changed().await.unwrap();
                assert!(receiver.borrow().is_none());
            }
            drop(stop);
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(worker, server)
        })
        .await
        .unwrap();
        assert_eq!(result.is_err(), lost_reply);
        assert!(receiver.borrow().is_none());
    }
}

/// Cancellation while a renewal reply is outstanding closes the exact exchange
/// and clears publication. Dropping the worker future has the same local cleanup
/// guarantee, without interpreting the remote renewal as proven unexecuted.
#[tokio::test(flavor = "current_thread")]
async fn pi_renewal_inflight_cancellation_and_drop_clear_publication() {
    for drop_worker in [false, true] {
        let fixture = Fixture::new();
        let (status, mut receiver) = watch::channel(None);
        let (stop, cancellation) = watch::channel(false);
        let worker = run_with_clock(&fixture.transport, "Pi", status, cancellation, || Some(100));
        let (issued, mut issuance) = watch::channel(false);
        let server = async {
            let (mut stream, _) = request(&fixture.listener).await;
            reply(&mut stream, "agent", 104).await;
            drop(stream);
            receiver.changed().await.unwrap();
            assert!(receiver.borrow().as_ref().unwrap().is_current());
            let (mut stream, method) = request(&fixture.listener).await;
            assert_eq!(method, "agent/external/renew");
            issued.send(true).unwrap();
            // EOF proves the caller released this exchange, not that the daemon
            // did not renew before losing its reply.
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
        };
        let client = async {
            let mut worker = Box::pin(worker);
            tokio::select! {
                result = &mut worker => panic!("worker ended before issued renewal: {result:?}"),
                result = issuance.changed() => result.unwrap(),
            }
            if drop_worker {
                drop(worker);
            } else {
                stop.send(true).unwrap();
                worker.await.unwrap();
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(6), async {
            tokio::join!(client, server)
        })
        .await
        .unwrap();
        assert!(receiver.borrow().is_none());
    }
}
