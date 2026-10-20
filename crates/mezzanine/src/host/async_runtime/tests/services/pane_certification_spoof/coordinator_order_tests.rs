//! COORDINATOR ORDER ONLY: inert controls for the actual fixture finish helper.
//!
//! Controlled futures represent completion and Shutdown ordering, not actor
//! admission, output accounting, native retirement, or a diagnosed shell fault.
//! No actor, backend, child process, provider, filesystem, or synthetic PID is
//! created. Every gated future is released and completed before assertions.

use std::cell::Cell;

use futures_util::FutureExt;

use super::*;

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
