//! COORDINATOR ORDER ONLY: inert controls for the actual fixture finish helper.
//!
//! Controlled futures represent completion and Shutdown ordering, not actor
//! admission, output accounting, native retirement, or a diagnosed shell fault.
//! No actor, backend, child process, provider, filesystem, or synthetic PID is
//! created. Every gated future is released and completed before assertions.

use std::cell::Cell;

use futures_util::FutureExt;

use super::*;
use crate::host::async_runtime::pane_io::wait_for_fixture_supervisor_event_or_stop;

#[tokio::test(flavor = "current_thread")]
/// The actual coordinator notifies before the original supervisor registers its
/// dedicated wait. The actual supervisor wait seam must consume that retained
/// permit. A controlled event fallback always releases the future before asserts.
async fn dedicated_supervisor_stop_survives_pre_registration() {
    let done = AtomicBool::new(false);
    let dedicated = tokio::sync::Notify::new();
    let used_stop = Cell::new(false);
    let (release, event) = tokio::sync::oneshot::channel();
    let mut coordinator = Box::pin(settle_fixture_supervisor(
        &done,
        &dedicated,
        async {
            let outcome = wait_for_fixture_supervisor_event_or_stop(Some(&dedicated), async {
                let _ = event.await;
                Some(())
            })
            .await;
            used_stop.set(outcome.is_none());
            Ok::<(), ()>(())
        },
        async { Ok::<(), ()>(()) },
    ));
    let early = coordinator.as_mut().now_or_never();
    let completed_without_event = early.is_some();
    let _ = release.send(());
    let result = match early {
        Some(result) => result,
        None => coordinator.await,
    };
    assert!(result.is_ok());
    assert!(done.load(Ordering::SeqCst));
    assert!(
        completed_without_event,
        "original supervisor missed its pre-registered stop permit"
    );
    assert!(used_stop.get());
}

#[tokio::test(flavor = "current_thread")]
/// A worker registered first consumes the shared event wake. The actual fixture
/// coordinator must still wake the original supervisor through its dedicated
/// wait seam, without relying on any second event or a synthetic timeout.
async fn dedicated_supervisor_stop_ignores_competing_worker_wake() {
    let done = AtomicBool::new(false);
    let shared = tokio::sync::Notify::new();
    let dedicated = tokio::sync::Notify::new();
    let used_stop = Cell::new(false);
    let mut competing_worker = Box::pin(shared.notified());
    let worker_registered = competing_worker.as_mut().now_or_never().is_none();
    let mut supervisor = Box::pin(wait_for_fixture_supervisor_event_or_stop(
        Some(&dedicated),
        async {
            shared.notified().await;
            Some(())
        },
    ));
    let supervisor_registered = supervisor.as_mut().now_or_never().is_none();
    shared.notify_one();
    let worker_consumed_shared = competing_worker.as_mut().now_or_never().is_some();
    let after_shared = supervisor.as_mut().now_or_never();
    let supervisor_still_waiting = after_shared.is_none();
    let mut coordinator = Box::pin(settle_fixture_supervisor(
        &done,
        &dedicated,
        async {
            let outcome = match after_shared {
                Some(outcome) => outcome,
                None => supervisor.await,
            };
            used_stop.set(outcome.is_none());
            Ok::<(), ()>(())
        },
        async { Ok::<(), ()>(()) },
    ));
    let early = coordinator.as_mut().now_or_never();
    let completed_without_more_events = early.is_some();
    shared.notify_waiters();
    let result = match early {
        Some(result) => result,
        None => coordinator.await,
    };
    assert!(result.is_ok());
    assert!(worker_registered && supervisor_registered);
    assert!(worker_consumed_shared);
    assert!(supervisor_still_waiting);
    assert!(
        completed_without_more_events,
        "competing worker stole the original supervisor stop wake"
    );
    assert!(used_stop.get());
}

#[tokio::test(flavor = "current_thread")]
/// The actual helper must not poll Shutdown before its original completion
/// future resolves. One explicit poll with completion gated records the order
/// deterministically; release and final completion happen before assertions.
async fn supervisor_completion_precedes_shutdown() {
    let done = AtomicBool::new(false);
    let wake = tokio::sync::Notify::new();
    let drained = Cell::new(false);
    let shutdown_polled = Cell::new(false);
    let shutdown_before_drain = Cell::new(false);
    let (release, completion) = tokio::sync::oneshot::channel();
    let mut coordinator = Box::pin(settle_fixture_supervisor(
        &done,
        &wake,
        async {
            completion.await.map_err(|_| ())?;
            drained.set(true);
            Ok::<(), ()>(())
        },
        async {
            shutdown_polled.set(true);
            shutdown_before_drain.set(!drained.get());
            Ok::<(), ()>(())
        },
    ));
    let early = coordinator.as_mut().now_or_never();
    let early_shutdown = shutdown_polled.get();
    let released = release.send(()).is_ok();
    let result = match early {
        Some(result) => result,
        None => coordinator.await,
    };

    assert!(
        released,
        "controlled completion must remain owned until release"
    );
    assert!(result.is_ok());
    assert!(done.load(Ordering::SeqCst));
    assert!(drained.get());
    assert!(
        !early_shutdown,
        "Shutdown was polled before original supervisor completion"
    );
    assert!(!shutdown_before_drain.get());
    assert!(shutdown_polled.get());
}

#[tokio::test(flavor = "current_thread")]
/// With no waiter registered beforehand, completion must observe stop intent
/// and the retained Notify permit immediately. This tests coordinator ordering
/// only, not production worker wakeup or a fabricated settlement receipt.
async fn stop_intent_and_durable_permit_precede_completion_wait() {
    let done = AtomicBool::new(false);
    let wake = tokio::sync::Notify::new();
    let observed_stop = Cell::new(false);
    let observed_permit = Cell::new(false);
    let result = settle_fixture_supervisor(
        &done,
        &wake,
        async {
            observed_stop.set(done.load(Ordering::SeqCst));
            observed_permit.set(wake.notified().now_or_never().is_some());
            Ok::<(), ()>(())
        },
        async { Ok::<(), ()>(()) },
    )
    .await;
    assert!(result.is_ok());
    assert!(observed_stop.get());
    assert!(observed_permit.get());
}

/// Non-Clone coordinator error whose allocation and release stay observable.
struct OwnedError(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl Drop for OwnedError {
    /// Records only the final owned release; there is no external cleanup.
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "current_thread")]
/// A failed original completion notification must transfer the same error and
/// skip both Shutdown and the caller's later close step. No notification error
/// is normalized into healthy completion and no owned error is dropped early.
async fn failed_completion_preserves_error_and_skips_shutdown_and_close() {
    let done = AtomicBool::new(false);
    let wake = tokio::sync::Notify::new();
    let shutdown_polled = Cell::new(false);
    let close_polled = Cell::new(false);
    let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let error = Box::new(OwnedError(drops.clone()));
    let original = error.as_ref() as *const OwnedError;
    let result = async {
        settle_fixture_supervisor(&done, &wake, async { Err::<(), _>(error) }, async {
            shutdown_polled.set(true);
            Ok(())
        })
        .await?;
        close_polled.set(true);
        Ok::<(), Box<OwnedError>>(())
    }
    .await;
    let returned = result.err().unwrap();
    assert_eq!(returned.as_ref() as *const OwnedError, original);
    assert!(!shutdown_polled.get());
    assert!(!close_polled.get());
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(returned);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
/// Shutdown errors remain original errors after successful completion and must
/// skip the caller's close. The controlled drain marker proves the order, not
/// actor admission or native retirement; arbitrary reducer errors stay fatal.
async fn shutdown_error_after_completion_is_not_normalized_or_closed() {
    let done = AtomicBool::new(false);
    let wake = tokio::sync::Notify::new();
    let drained = Cell::new(false);
    let drained_before_shutdown = Cell::new(false);
    let close_polled = Cell::new(false);
    let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let error = Box::new(OwnedError(drops.clone()));
    let original = error.as_ref() as *const OwnedError;
    let result = async {
        settle_fixture_supervisor(
            &done,
            &wake,
            async {
                drained.set(true);
                Ok(())
            },
            async {
                drained_before_shutdown.set(drained.get());
                Err::<(), _>(error)
            },
        )
        .await?;
        close_polled.set(true);
        Ok::<(), Box<OwnedError>>(())
    }
    .await;
    let returned = result.err().unwrap();
    assert_eq!(returned.as_ref() as *const OwnedError, original);
    assert!(drained_before_shutdown.get());
    assert!(!close_polled.get());
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(returned);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
