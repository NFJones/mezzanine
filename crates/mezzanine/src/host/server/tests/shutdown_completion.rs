//! Original-deadline effect fencing for deferred host shutdown receipts.
//!
//! A writable local stream is deliberately immediately ready, so timeout's
//! inner-future polling order cannot be mistaken for an expired-budget fence.

use super::*;
use tokio::io::AsyncReadExt;

/// An already expired original connection must publish no bytes even if both
/// write and flush could complete without yielding. Failure is observation of
/// unproven delivery, not permission to renew or fabricate a healthy receipt.
#[tokio::test(flavor = "current_thread")]
async fn expired_writable_shutdown_completion_publishes_nothing() {
    let (stream, mut peer) = tokio::net::UnixStream::pair().unwrap();
    let reply = HostShutdownCompletion {
        stream,
        id: json!("expired-exact-host"),
        deadline: tokio::time::Instant::now(),
    };
    let result = reply.publish(false, &Ok(())).await;
    let mut bytes = [0; 4096];
    let received = peer.read(&mut bytes).await.unwrap();
    assert!(result.unwrap_err().message().contains("deadline expired"));
    assert_eq!(received, 0, "expired host reply wrote a healthy receipt");
}
