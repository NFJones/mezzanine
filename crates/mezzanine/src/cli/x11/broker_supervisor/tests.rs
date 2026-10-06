//! Deterministic owned-future tests without desktop or credential side effects.
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Counts live polled operations and their disposal, not application delivery.
/// Drop evidence permits cancellation tests to prove no worker escapes ownership.
struct Active(Arc<AtomicUsize>);

impl Active {
    /// Registers one live operation for the fixture's structural bound.
    fn new(count: Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count)
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// All finite slots can progress independently; successful completion admits
/// fresh demand without exceeding the bound. Cancellation disposes every active
/// operation, and already-ready cancellation starts no work.
#[tokio::test]
async fn broker_x11_supervisor_bounds_owned_work_and_cancellation() {
    let active = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let stop = Arc::new(tokio::sync::Notify::new());
    let mut serving = Box::pin(supervise(
        2,
        || async {
            let _active = Active::new(active.clone());
            started.fetch_add(1, Ordering::SeqCst);
            let permit = release.acquire().await.unwrap();
            permit.forget();
            Ok(BrokerRelayOutcome::Completed)
        },
        stop.notified(),
    ));
    assert!(matches!(
        futures_util::poll!(&mut serving),
        std::task::Poll::Pending
    ));
    assert_eq!(active.load(Ordering::SeqCst), 2);
    assert_eq!(started.load(Ordering::SeqCst), 2);
    release.add_permits(1);
    assert!(matches!(
        futures_util::poll!(&mut serving),
        std::task::Poll::Pending
    ));
    assert_eq!(active.load(Ordering::SeqCst), 2);
    assert_eq!(started.load(Ordering::SeqCst), 3);
    stop.notify_one();
    serving.await.unwrap();
    assert_eq!(active.load(Ordering::SeqCst), 0);
    supervise(
        2,
        || async {
            started.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<Result<BrokerRelayOutcome>>().await
        },
        std::future::ready(()),
    )
    .await
    .unwrap();
    assert_eq!(started.load(Ordering::SeqCst), 3);
}

/// One failed operation retires all siblings without retrying that operation.
/// Whole-future disposal also releases active work; invalid limits start none.
#[tokio::test]
async fn broker_x11_supervisor_errors_and_abandonment_dispose_siblings() {
    let active = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(AtomicUsize::new(0));
    let fail = tokio::sync::Notify::new();
    let mut serving = Box::pin(supervise(
        2,
        || async {
            let _active = Active::new(active.clone());
            let index = started.fetch_add(1, Ordering::SeqCst);
            if index == 0 {
                fail.notified().await;
                Err(MezError::invalid_state("synthetic channel failure"))
            } else {
                std::future::pending().await
            }
        },
        std::future::pending(),
    ));
    assert!(matches!(
        futures_util::poll!(&mut serving),
        std::task::Poll::Pending
    ));
    fail.notify_one();
    assert!(serving.await.is_err());
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert_eq!(started.load(Ordering::SeqCst), 2);
    let mut abandoned = Box::pin(supervise(
        2,
        || async {
            let _active = Active::new(active.clone());
            std::future::pending::<Result<BrokerRelayOutcome>>().await
        },
        std::future::pending(),
    ));
    assert!(matches!(
        futures_util::poll!(&mut abandoned),
        std::task::Poll::Pending
    ));
    drop(abandoned);
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert!(
        supervise(
            0,
            || async { panic!("invalid capacity started work") },
            std::future::pending()
        )
        .await
        .is_err()
    );
}
