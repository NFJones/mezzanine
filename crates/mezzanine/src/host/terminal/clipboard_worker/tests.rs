//! Owned worker cleanup and latest-value semantics without desktop mutation.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

static COPIES: AtomicUsize = AtomicUsize::new(0);

/// Synthetic backend records only invocation count, never submitted content.
fn count_copy(_: &str) -> bool {
    COPIES.fetch_add(1, Ordering::SeqCst);
    true
}

/// Before polling, multiple values collapse to one latest item. Explicit
/// shutdown closes cloned queue handles; abandoning the owner also closes the
/// worker without invoking a desktop provider. Neither proves backend delivery.
#[tokio::test]
async fn clipboard_worker_latest_value_and_owned_cleanup() {
    COPIES.store(0, Ordering::SeqCst);
    let worker = ClipboardWorker::new(HostClipboard::new(count_copy, || None));
    let sender = worker.sender().unwrap().clone();
    sender.send_replace(Some("superseded fixture".into()));
    sender.send_replace(Some("latest fixture".into()));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while COPIES.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    worker.shutdown().await;
    assert!(sender.is_closed());
    assert_eq!(COPIES.load(Ordering::SeqCst), 1);

    let worker = ClipboardWorker::new(HostClipboard::disabled());
    let sender = worker.sender().unwrap().clone();
    drop(worker);
    tokio::time::timeout(std::time::Duration::from_secs(2), sender.closed())
        .await
        .unwrap();
}
