//! Bounded callback admission and assembled Pi lifecycle/renewal IPC fixtures.
//!
//! Synthetic capabilities target only test-owned Unix endpoints. No vendor or
//! provider runs; tests exercise production worker ordering and failure fences.

use super::*;
use secrecy::SecretString;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Exclusive test endpoint and lifecycle owner with explicitly supplied authority.
struct Fixture {
    root: std::path::PathBuf,
    listener: tokio::net::UnixListener,
    owner: LifecycleOwner,
    transport: CapabilityTransport,
}

impl Fixture {
    /// Creates only a unique test socket, never discovering a user daemon.
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mez-pi-session-{}",
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
            owner,
            transport,
        }
    }
}

impl Drop for Fixture {
    /// Removes only the fixture directory after its futures settle.
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

/// Reads a bounded request and validates its restricted schema and authority.
async fn request(
    listener: &tokio::net::UnixListener,
) -> (tokio::net::UnixStream, serde_json::Value) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = stream.read(&mut buffer).await.unwrap();
        assert!(count > 0 && bytes.len() + count <= 65536);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some((frame, consumed)) =
            crate::protocol::framing::decode_frame_incremental(&bytes, 65536).unwrap()
        {
            assert_eq!(consumed, bytes.len());
            let parsed = crate::control::parse_json_rpc_request(&frame.body).unwrap();
            crate::control::validate_control_method_params_schema(&parsed).unwrap();
            assert_ne!(parsed.method, "control/initialize");
            let value: serde_json::Value = serde_json::from_str(&frame.body).unwrap();
            assert_eq!(value["params"]["external_session_id"], "bound");
            return (stream, value);
        }
    }
}

/// Supplies one typed acknowledgment, not fabricated provider evidence.
async fn reply(stream: &mut tokio::net::UnixStream, result: serde_json::Value) {
    stream
        .write_all(&crate::control::encode_control_body(
            &serde_json::json!({
                "jsonrpc":"2.0", "id":"pi-lifecycle", "result":result,
            })
            .to_string(),
        ))
        .await
        .unwrap();
}

/// A reload proposal can remain unconfirmed across an idle renewal. The
/// worker must use the new acknowledged lease fence, not expire the session
/// at the original deadline while the launcher still holds a valid proposal.
#[tokio::test(flavor = "current_thread")]
async fn pi_session_reload_offer_tracks_renewed_lease_deadline() {
    let mut fixture = Fixture::new();
    let (ingress, inputs) = channel();
    let (_stop, cancellation) = watch::channel(false);
    ingress
        .observe(
            1,
            "bound",
            Observation::SessionShutdown { reason: "reload" },
        )
        .unwrap();
    let proposal = ingress.attach_after_reload("bound").unwrap();
    let (renewed, notice) = oneshot::channel();
    let worker = run_with_clock(
        &mut fixture.owner,
        &fixture.transport,
        "Pi",
        inputs,
        cancellation,
        || Some(100),
    );
    let client = async {
        let offer = proposal.await.unwrap().unwrap();
        notice.await.unwrap();
        // Initial expiry 102 is conservatively fenced at request start + 1s.
        // Renewal occurs at ~0.5s; this crosses that old fence but not the new.
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        let epoch = offer.confirm().await.unwrap();
        assert_eq!(epoch, 2);
        ingress
            .observe(
                epoch,
                "bound",
                Observation::SessionShutdown { reason: "quit" },
            )
            .unwrap();
    };
    let server = async {
        let (mut stream, value) = request(&fixture.listener).await;
        assert_eq!(value["method"], "agent/external/register");
        reply(&mut stream, serde_json::json!({"registered":true,"agent_id":"agent","generation":1,"expires_at_unix_seconds":102})).await;
        drop(stream);
        let (mut stream, value) = request(&fixture.listener).await;
        assert_eq!(value["method"], "agent/external/renew");
        reply(
            &mut stream,
            serde_json::json!({"agent_id":"agent","generation":1,"expires_at_unix_seconds":104}),
        )
        .await;
        drop(stream);
        renewed.send(()).unwrap();
        let (mut stream, value) = request(&fixture.listener).await;
        assert_eq!(value["method"], "agent/external/deregister");
        reply(
            &mut stream,
            serde_json::json!({"retired":true,"changed":true}),
        )
        .await;
    };
    let (result, (), ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(worker, client, server)
    })
    .await
    .unwrap();
    result.unwrap();
    assert!(fixture.owner.pending().is_none());
}

/// Callback queue pressure fails synchronously without overwriting accepted
/// facts, awaiting IPC, or retaining oversized untrusted session identifiers.
#[test]
fn pi_session_ingress_is_bounded_and_nonblocking() {
    let (ingress, inputs) = channel();
    assert!(ingress.observe(0, "bound", Observation::Running).is_err());
    assert!(
        ingress
            .observe(1, &"x".repeat(129), Observation::Running)
            .is_err()
    );
    for _ in 0..CAPACITY {
        ingress.observe(1, "bound", Observation::Running).unwrap();
    }
    assert!(ingress.observe(1, "bound", Observation::Settled).is_err());
    assert!(ingress.attach_after_reload("bound").is_err());
    drop(inputs);
    assert!(ingress.observe(1, "bound", Observation::Running).is_err());
}

/// Registration precedes presentation; reload retains sequence identity and
/// only explicitly acknowledged same-session attachment accepts a new epoch.
/// Old callbacks and provisional outcomes cannot publish terminal success.
#[tokio::test(flavor = "current_thread")]
async fn pi_session_orders_delivery_and_fences_reload_callbacks() {
    let mut fixture = Fixture::new();
    let (ingress, inputs) = channel();
    let (_stop, cancellation) = watch::channel(false);
    ingress.observe(1, "bound", Observation::Running).unwrap();
    ingress
        .observe(
            1,
            "bound",
            Observation::SessionShutdown { reason: "reload" },
        )
        .unwrap();
    ingress
        .observe(
            1,
            "bound",
            Observation::CandidateOutcome {
                outcome: "completed",
            },
        )
        .unwrap();
    let replacement = ingress.attach_after_reload("bound").unwrap();
    let worker = run_with_clock(
        &mut fixture.owner,
        &fixture.transport,
        "Pi",
        inputs,
        cancellation,
        || Some(100),
    );
    let client = async {
        let epoch = replacement.await.unwrap().unwrap().confirm().await.unwrap();
        assert_eq!(epoch, 2);
        ingress.observe(1, "bound", Observation::Running).unwrap();
        ingress
            .observe(epoch, "bound", Observation::Settled)
            .unwrap();
        ingress
            .observe(
                epoch,
                "bound",
                Observation::SessionShutdown { reason: "quit" },
            )
            .unwrap();
    };
    let server = async {
        let (mut stream, value) = request(&fixture.listener).await;
        assert_eq!(value["method"], "agent/external/register");
        reply(&mut stream, serde_json::json!({"registered":true,"agent_id":"agent","generation":1,"expires_at_unix_seconds":160})).await;
        for (sequence, state) in [(1, "running"), (2, "ready")] {
            let (mut stream, value) = request(&fixture.listener).await;
            assert_eq!(value["method"], "agent/external/presentation");
            assert_eq!(value["params"]["sequence"], sequence);
            assert_eq!(value["params"]["state"], state);
            reply(
                &mut stream,
                serde_json::json!({"sequence":sequence,"changed":true}),
            )
            .await;
        }
        let (mut stream, value) = request(&fixture.listener).await;
        assert_eq!(value["method"], "agent/external/deregister");
        reply(
            &mut stream,
            serde_json::json!({"retired":true,"changed":true}),
        )
        .await;
    };
    let (result, (), ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(worker, client, server)
    })
    .await
    .unwrap();
    result.unwrap();
    assert!(fixture.owner.pending().is_none());
}

/// Idle renewal progresses with no callbacks. After renewal a final quit drains
/// retirement and stops the whole session future, not a detached lease task.
#[tokio::test(flavor = "current_thread")]
async fn pi_session_idle_renewal_and_retirement_share_worker_lifetime() {
    let mut fixture = Fixture::new();
    let (ingress, inputs) = channel();
    let (_stop, cancellation) = watch::channel(false);
    let worker = run_with_clock(
        &mut fixture.owner,
        &fixture.transport,
        "Pi",
        inputs,
        cancellation,
        || Some(100),
    );
    let server = async {
        let (mut stream, value) = request(&fixture.listener).await;
        assert_eq!(value["method"], "agent/external/register");
        reply(&mut stream, serde_json::json!({"registered":true,"agent_id":"agent","generation":1,"expires_at_unix_seconds":102})).await;
        let (mut stream, value) = request(&fixture.listener).await;
        assert_eq!(value["method"], "agent/external/renew");
        reply(
            &mut stream,
            serde_json::json!({"agent_id":"agent","generation":1,"expires_at_unix_seconds":103}),
        )
        .await;
        ingress
            .observe(1, "bound", Observation::SessionShutdown { reason: "quit" })
            .unwrap();
        let (mut stream, value) = request(&fixture.listener).await;
        assert_eq!(value["method"], "agent/external/deregister");
        reply(
            &mut stream,
            serde_json::json!({"retired":true,"changed":true}),
        )
        .await;
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(worker, server)
    })
    .await
    .unwrap();
    result.unwrap();
    assert!(fixture.owner.pending().is_none());
}

/// Preclosed ingress ends ownership before registration, even with buffered
/// observations or an already accepted report. No buffered work is drained
/// after ownership loss, and exact reducer evidence remains available.
#[tokio::test(flavor = "current_thread")]
async fn pi_session_preclosed_ingress_does_not_register_or_drain() {
    let mut fixture = Fixture::new();
    fixture
        .owner
        .observe(1, "bound", Observation::Running)
        .unwrap();
    let original = fixture.owner.pending().unwrap().clone();
    let (ingress, inputs) = channel();
    ingress.observe(1, "bound", Observation::Settled).unwrap();
    drop(ingress);
    let (_stop, cancellation) = watch::channel(false);
    run_with_clock(
        &mut fixture.owner,
        &fixture.transport,
        "Pi",
        inputs,
        cancellation,
        || Some(100),
    )
    .await
    .unwrap();
    assert_eq!(fixture.owner.pending(), Some(&original));
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(30),
            fixture.listener.accept()
        )
        .await
        .is_err()
    );
}

/// Loss of the final ingress handle cancels an issued presentation exchange.
/// EOF is only local transport release: the original report is retained even
/// if the peer may have applied it before its acknowledgment was lost.
#[tokio::test(flavor = "current_thread")]
async fn pi_session_inflight_ingress_closure_preserves_pending() {
    let mut fixture = Fixture::new();
    let (ingress, inputs) = channel();
    ingress.observe(1, "bound", Observation::Running).unwrap();
    let (_stop, cancellation) = watch::channel(false);
    let worker = run_with_clock(
        &mut fixture.owner,
        &fixture.transport,
        "Pi",
        inputs,
        cancellation,
        || Some(100),
    );
    let server = async {
        let (mut stream, _) = request(&fixture.listener).await;
        reply(&mut stream, serde_json::json!({"registered":true,"agent_id":"agent","generation":1,"expires_at_unix_seconds":160})).await;
        let (mut stream, value) = request(&fixture.listener).await;
        assert_eq!(value["method"], "agent/external/presentation");
        drop(ingress);
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(worker, server)
    })
    .await
    .unwrap();
    result.unwrap();
    assert_eq!(
        fixture.owner.pending().unwrap().operation,
        Operation::Present("running")
    );
    assert_eq!(fixture.owner.pending().unwrap().sequence, 1);
}

/// Abandoning either an unread proposal or a received-but-unconfirmed offer
/// leaves the observer suspended. A subsequent attachment proposal is a FIFO
/// barrier proving guessed new-epoch callbacks were not accepted in between.
#[tokio::test(flavor = "current_thread")]
async fn pi_session_abandoned_reload_proposals_do_not_activate() {
    let mut fixture = Fixture::new();
    let (ingress, inputs) = channel();
    ingress
        .observe(
            1,
            "bound",
            Observation::SessionShutdown { reason: "reload" },
        )
        .unwrap();
    let unread = ingress.attach_after_reload("bound").unwrap();
    drop(unread);
    let proposed = ingress.attach_after_reload("bound").unwrap();
    let (stop, cancellation) = watch::channel(false);
    let worker = run_with_clock(
        &mut fixture.owner,
        &fixture.transport,
        "Pi",
        inputs,
        cancellation,
        || Some(100),
    );
    let client = async {
        drop(proposed.await.unwrap().unwrap());
        ingress.observe(2, "bound", Observation::Running).unwrap();
        let next = ingress.attach_after_reload("bound").unwrap();
        drop(next.await.unwrap().unwrap());
        stop.send(true).unwrap();
    };
    let server = async {
        let (mut stream, _) = request(&fixture.listener).await;
        reply(&mut stream, serde_json::json!({"registered":true,"agent_id":"agent","generation":1,"expires_at_unix_seconds":160})).await;
    };
    let (result, (), ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(worker, client, server)
    })
    .await
    .unwrap();
    result.unwrap();
    assert!(fixture.owner.can_attach_after_reload("bound"));
    assert!(fixture.owner.pending().is_none());
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(30),
            fixture.listener.accept()
        )
        .await
        .is_err()
    );
}

/// Inherited callback-stream facts pass through strict parsing and the real
/// coordinator before restricted daemon delivery. Stream EOF is kept separate
/// from launcher lifetime so an accepted quit can settle without replay.
#[tokio::test(flavor = "current_thread")]
async fn pi_session_inherited_stream_delivers_ordered_lifecycle() {
    let mut fixture = Fixture::new();
    let (ingress, inputs) = channel();
    let launcher = ingress.clone();
    let (_stop, cancellation) = watch::channel(false);
    let (stream, mut writer) = tokio::net::UnixStream::pair().unwrap();
    let bridge = super::super::pi_ipc::serve(stream, "bound", 1, ingress, cancellation.clone());
    let worker = run_with_clock(
        &mut fixture.owner,
        &fixture.transport,
        "Pi",
        inputs,
        cancellation,
        || Some(100),
    );
    let producer = async {
        let frames = b"{\"type\":\"agent_start\"}\n{\"type\":\"agent_before_settle\",\"outcome\":\"completed\"}\n{\"type\":\"agent_settled\"}\n{\"type\":\"session_shutdown\",\"reason\":\"quit\"}\n";
        for chunk in frames.chunks(11) {
            writer.write_all(chunk).await.unwrap();
            tokio::task::yield_now().await;
        }
        writer.shutdown().await.unwrap();
    };
    let server = async {
        let (mut stream, _) = request(&fixture.listener).await;
        reply(&mut stream, serde_json::json!({"registered":true,"agent_id":"agent","generation":1,"expires_at_unix_seconds":160})).await;
        for (sequence, state) in [(1, "running"), (2, "complete")] {
            let (mut stream, value) = request(&fixture.listener).await;
            assert_eq!(value["method"], "agent/external/presentation");
            assert_eq!(value["params"]["sequence"], sequence);
            assert_eq!(value["params"]["state"], state);
            reply(
                &mut stream,
                serde_json::json!({"sequence":sequence,"changed":true}),
            )
            .await;
        }
        let (mut stream, value) = request(&fixture.listener).await;
        assert_eq!(value["method"], "agent/external/deregister");
        reply(
            &mut stream,
            serde_json::json!({"retired":true,"changed":true}),
        )
        .await;
    };
    let (result, bridge_result, (), ()) =
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(worker, bridge, producer, server)
        })
        .await
        .unwrap();
    result.unwrap();
    bridge_result.unwrap();
    drop(launcher);
    assert!(fixture.owner.pending().is_none());
}

/// Explicit released-loader qualification passes a real Unix descriptor to a
/// Node child, then drives its extension facts through the production callback
/// bridge and session worker. Daemon authority stays in the parent, and no
/// provider, user configuration or saved-session discovery occurs in the child.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit trusted Pi 1.0.2 package and Node >=22.19.0 required"]
async fn pi_session_released_extension_uses_inherited_descriptor() {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let node =
        std::path::PathBuf::from(std::env::var_os("MEZ_PI_NODE").expect("explicit Node required"));
    let package = std::path::PathBuf::from(
        std::env::var_os("MEZ_PI_PACKAGE").expect("explicit Pi package required"),
    );
    assert!(node.is_absolute() && package.is_absolute());
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let mut fixture = Fixture::new();
    let home = fixture.root.join("home");
    std::fs::create_dir(&home).unwrap();
    let (parent, child) = std::os::unix::net::UnixStream::pair().unwrap();
    parent.set_nonblocking(true).unwrap();
    let parent = tokio::net::UnixStream::from_std(parent).unwrap();
    // SAFETY: fcntl duplicates a live descriptor into a distinct owned CLOEXEC
    // descriptor above the fixed destination. This avoids same-fd dup2 retaining
    // CLOEXEC and keeps the source away from standard child pipe descriptors.
    let source = unsafe { libc::fcntl(child.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    assert!(source >= 10);
    // SAFETY: successful fcntl returned a newly owned descriptor.
    let source = unsafe { OwnedFd::from_raw_fd(source) };
    let raw = source.as_raw_fd();
    let mut command = tokio::process::Command::new(&node);
    command
        .arg(repository.join("scripts/qualify-pi-inherited-stream.mjs"))
        .arg(&package)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", node.parent().unwrap())
        .current_dir(repository)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // SAFETY: after fork this closure only uses async-signal-safe dup2 with
    // captured descriptor integers. The source remains alive through spawn;
    // dup2 clears CLOEXEC on destination 3, while all other copies keep it.
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if libc::dup2(raw, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut process = command.spawn().unwrap();
    drop(source);
    drop(child);
    let (ingress, inputs) = channel();
    let launcher = ingress.clone();
    let (_stop, cancellation) = watch::channel(false);
    let bridge = super::super::pi_ipc::serve(parent, "bound", 1, ingress, cancellation.clone());
    let worker = run_with_clock(
        &mut fixture.owner,
        &fixture.transport,
        "Pi",
        inputs,
        cancellation,
        || Some(100),
    );
    let server = async {
        let (mut stream, value) = request(&fixture.listener).await;
        assert_eq!(value["method"], "agent/external/register");
        reply(&mut stream, serde_json::json!({"registered":true,"agent_id":"agent","generation":1,"expires_at_unix_seconds":160})).await;
        for (sequence, state) in [
            (1, "ready"),
            (2, "running"),
            (3, "input-wait"),
            (4, "running"),
            (5, "complete"),
        ] {
            let (mut stream, value) = request(&fixture.listener).await;
            assert_eq!(value["method"], "agent/external/presentation");
            assert_eq!(value["params"]["sequence"], sequence);
            assert_eq!(value["params"]["state"], state);
            reply(
                &mut stream,
                serde_json::json!({"sequence":sequence,"changed":true}),
            )
            .await;
        }
        let (mut stream, value) = request(&fixture.listener).await;
        assert_eq!(value["method"], "agent/external/deregister");
        reply(
            &mut stream,
            serde_json::json!({"retired":true,"changed":true}),
        )
        .await;
    };
    let (result, bridge_result, (), status) =
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            tokio::join!(worker, bridge, server, process.wait())
        })
        .await
        .unwrap();
    result.unwrap();
    bridge_result.unwrap();
    assert!(status.unwrap().success());
    drop(launcher);
    assert!(fixture.owner.pending().is_none());
}

/// Missing acknowledgments and explicit cancellation preserve the original
/// pending report. Neither path reconnects automatically or fabricates success.
#[tokio::test(flavor = "current_thread")]
async fn pi_session_failure_and_cancellation_preserve_exact_pending_work() {
    for cancel in [false, true] {
        let mut fixture = Fixture::new();
        let (ingress, inputs) = channel();
        let (stop, cancellation) = watch::channel(false);
        ingress.observe(1, "bound", Observation::Running).unwrap();
        let worker = run_with_clock(
            &mut fixture.owner,
            &fixture.transport,
            "Pi",
            inputs,
            cancellation,
            || Some(100),
        );
        let server = async {
            let (mut stream, _) = request(&fixture.listener).await;
            reply(&mut stream, serde_json::json!({"registered":true,"agent_id":"agent","generation":1,"expires_at_unix_seconds":160})).await;
            let (mut stream, value) = request(&fixture.listener).await;
            assert_eq!(value["params"]["sequence"], 1);
            if cancel {
                stop.send(true).unwrap();
                let mut byte = [0];
                assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
            }
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(worker, server)
        })
        .await
        .unwrap();
        assert_eq!(result.is_ok(), cancel);
        let head = fixture.owner.pending().unwrap();
        assert_eq!(head.sequence, 1);
        assert_eq!(head.operation, Operation::Present("running"));
    }
}
