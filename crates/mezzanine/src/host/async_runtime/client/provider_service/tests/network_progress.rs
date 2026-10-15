//! Approved-network progress scheduling under held actor acknowledgment.
//!
//! Real pooled loopback HTTP supplies body/deadline/cancel evidence. The private
//! submit seam models an actor request accepted but awaiting acknowledgment;
//! no provider, paid search, credentials or runtime authority is fabricated.

use super::*;
use crate::integrations::http::{AsyncProviderHttpTransport, HttpExchangeResponse};
use mez_agent::{ProviderHttpRequest, ProviderHttpResult, ProviderHttpTimeouts};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Confirms that a held ingress future was dropped instead of detached.
struct IngressDrop(Arc<AtomicUsize>);
impl Drop for IngressDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Executes one real body with acknowledgment held. The peer waits for ingress
/// to start before sending its tail or waiting for local cancellation/timeout.
async fn held_ack_case(
    finish: bool,
    cancel: bool,
) -> (
    Option<ProviderHttpResult<HttpExchangeResponse>>,
    usize,
    usize,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let request = ProviderHttpRequest {
        method: "GET".into(),
        url: format!("http://{}/held-ack", listener.local_addr().unwrap()),
        headers: Default::default(),
        body: String::new(),
        timeouts: ProviderHttpTimeouts::from_total(300),
        max_response_bytes: Some(20),
    };
    let (peer_started_tx, peer_started_rx) = tokio::sync::oneshot::channel();
    let (cancel_started_tx, cancel_started_rx) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = [0; 4096];
        assert!(stream.read(&mut bytes).await.unwrap() > 0);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\na")
            .await
            .unwrap();
        peer_started_rx.await.unwrap();
        if finish {
            stream.write_all(b"b").await.unwrap();
        } else {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(3), stream.read(&mut bytes))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
        }
    });
    let (sender, receiver) = watch::channel(None);
    let mut observed = Vec::new();
    let mut on_chunk = |chunk: Vec<u8>| {
        observed.extend_from_slice(&chunk);
        sender.send_replace(Some(String::from_utf8(observed.clone()).unwrap()));
        Box::pin(async {}) as Pin<Box<dyn Future<Output = ()> + Send>>
    };
    let transport = ReqwestProviderHttpTransport;
    let network = transport.send_async_with_body_chunks(&request, &mut on_chunk);
    let calls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let mut peer_started_tx = Some(peer_started_tx);
    let mut cancel_started_tx = Some(cancel_started_tx);
    let submit = |source: String, revision| {
        assert_eq!(source, "a");
        assert_eq!(revision, 1);
        calls.fetch_add(1, Ordering::SeqCst);
        peer_started_tx.take().unwrap().send(()).unwrap();
        cancel_started_tx.take().unwrap().send(()).unwrap();
        let guard = IngressDrop(drops.clone());
        async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        }
    };
    // Exercise the same biased outer cancellation/drop boundary without
    // exposing the runtime-private cancellation implementation to this seam.
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let worker = async {
        // Ownership remains inside the cancelled worker, not beside it. A
        // borrowed scheduler alone cannot retire its caller-owned body future.
        tokio::pin!(network);
        tokio::select! {
            biased;
            _ = cancel_rx => None,
            result = drive_external_network_with_progress(network.as_mut(), receiver, submit) => Some(result),
        }
    };
    let cancel_after_ingress = async {
        cancel_started_rx.await.unwrap();
        if cancel {
            cancel_tx.send(()).unwrap();
        } else {
            // Keep cancellation pending for the canonical success/error case.
            std::future::pending::<()>().await;
        }
    };
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::pin!(cancel_after_ingress);
        tokio::pin!(worker);
        tokio::select! {
            result = &mut worker => result,
            () = &mut cancel_after_ingress => worker.await,
        }
    })
    .await
    .expect("held presentation acknowledgment must not suspend network completion/deadlines");
    peer.await.unwrap();
    (
        result,
        calls.load(Ordering::SeqCst),
        drops.load(Ordering::SeqCst),
    )
}

/// A held progress acknowledgment cannot prevent real body EOF or replace the
/// authoritative complete response with provisional source. No tail flush waits.
#[tokio::test]
async fn outer_held_ack_does_not_block_real_http_success() {
    let (result, calls, drops) = held_ack_case(true, false).await;
    let response = result.unwrap().unwrap();
    assert_eq!(response.response.body, "ab");
    assert_eq!(
        response.completion,
        crate::integrations::http::HttpCompletion::Complete
    );
    assert_eq!((calls, drops), (1, 1));
}

/// A stalled peer still expires its absolute total budget while actor ingress
/// remains unacknowledged; progress is dropped before returning the typed error.
#[tokio::test]
async fn outer_held_ack_does_not_block_real_http_deadline() {
    let (result, calls, drops) = held_ack_case(false, false).await;
    assert_eq!(
        result.unwrap().unwrap_err().kind(),
        mez_agent::ProviderHttpErrorKind::Timeout(mez_agent::ProviderHttpTimeoutPhase::Total)
    );
    assert_eq!((calls, drops), (1, 1));
}

/// Outer cancellation after ingress starts drops both scoped futures and
/// the unfinished HTTP body; peer EOF is not a claim that no remote effect ran.
#[tokio::test]
async fn outer_held_ack_cancellation_retires_ingress_and_real_http() {
    let (result, calls, drops) = held_ack_case(false, true).await;
    assert!(result.is_none());
    assert_eq!((calls, drops), (1, 1));
}

/// Closing progress does not fabricate network completion or spin a ready
/// receiver; the network future alone remains the authority for its output.
#[tokio::test]
async fn outer_progress_closed_watch_does_not_complete_network() {
    let (sender, receiver) = watch::channel(None);
    drop(sender);
    let polls = AtomicUsize::new(0);
    let (complete, mut completion) = tokio::sync::oneshot::channel();
    let wake = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(25)).await;
        complete.send(17).unwrap();
    });
    let network = std::future::poll_fn(|context| {
        polls.fetch_add(1, Ordering::SeqCst);
        Pin::new(&mut completion).poll(context)
    });
    tokio::pin!(network);
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        drive_external_network_with_progress(network.as_mut(), receiver, |_, _| async {
            panic!("closed empty watch must not submit progress");
        }),
    )
    .await
    .unwrap();
    assert_eq!(result.unwrap(), 17);
    wake.await.unwrap();
    assert!(
        polls.load(Ordering::SeqCst) < 16,
        "closed progress must park, not repeatedly poll pending network"
    );
}

/// One held ingress coalesces arbitrarily many cumulative snapshots into the
/// watch's latest value. Releasing that acknowledgment admits exactly one newer
/// revision; network completion then drops it without replaying a tail flush.
#[tokio::test]
async fn outer_progress_coalesces_while_one_ingress_is_pending() {
    let (sender, receiver) = watch::channel(None);
    sender.send_replace(Some("a".into()));
    let (release, acknowledgment) = tokio::sync::oneshot::channel::<()>();
    let (finish, completion) = tokio::sync::oneshot::channel::<u8>();
    let mut acknowledgment = Some(acknowledgment);
    let mut finish = Some(finish);
    let calls = std::sync::Mutex::new(Vec::new());
    let drops = Arc::new(AtomicUsize::new(0));
    tokio::pin!(completion);
    let result =
        drive_external_network_with_progress(completion.as_mut(), receiver, |source, revision| {
            calls.lock().unwrap().push((source, revision));
            let acknowledgment = acknowledgment.take();
            let finish = if revision == 2 { finish.take() } else { None };
            let guard = IngressDrop(drops.clone());
            async move {
                let _guard = guard;
                if let Some(acknowledgment) = acknowledgment {
                    acknowledgment.await.unwrap();
                } else {
                    finish.unwrap().send(29).unwrap();
                    std::future::pending::<()>().await;
                }
            }
        });
    let updates = async {
        // The first ingress must be polled before releasing its acknowledgment.
        while calls.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
        sender.send_replace(Some("ab".into()));
        sender.send_replace(Some("abc".into()));
        release.send(()).unwrap();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(result, updates)
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap(), 29);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![("a".into(), 1), ("abc".into(), 2)]
    );
    assert_eq!(drops.load(Ordering::SeqCst), 2);
}

/// When both terminal outcome and changed progress are ready, only the exact
/// canonical outcome wins; no obsolete callback or final-source replay runs.
#[tokio::test]
async fn outer_progress_ready_terminal_wins_over_ready_snapshot() {
    let (sender, receiver) = watch::channel(None);
    sender.send_replace(Some("obsolete".into()));
    let network = std::future::ready(23);
    tokio::pin!(network);
    let result = drive_external_network_with_progress(network.as_mut(), receiver, |_, _| async {
        panic!("terminal result must win over provisional progress");
    })
    .await;
    assert_eq!(result, 23);
}
