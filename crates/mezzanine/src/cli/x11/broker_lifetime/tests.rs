//! Deterministic lifetime ordering without desktop or terminal mode side effects.
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Records disposal of a polled channel or foreground owner.
struct Disposed(Arc<AtomicBool>);

impl Drop for Disposed {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// External cancellation disposes channel work before foreground restoration;
/// foreground gets its stop signal exactly once, and all owners drop before the
/// coordinator returns. Channel failure retains its causal error over restoration.
#[tokio::test]
async fn broker_x11_lifetime_disposes_channels_before_foreground_cleanup() {
    for failing in [false, true] {
        let channel_dropped = Arc::new(AtomicBool::new(false));
        let foreground_dropped = Arc::new(AtomicBool::new(false));
        let restored = Arc::new(AtomicBool::new(false));
        let (stop, stopped) = watch::channel(false);
        let release = tokio::sync::Notify::new();
        let foreground = async {
            let _owner = Disposed(foreground_dropped.clone());
            broker_attachment_cancelled(stopped).await;
            assert!(channel_dropped.load(Ordering::SeqCst));
            restored.store(true, Ordering::SeqCst);
            if failing {
                Err(MezError::invalid_state("restoration failed"))
            } else {
                Ok(())
            }
        };
        let channels = async {
            let _owner = Disposed(channel_dropped.clone());
            release.notified().await;
            Err(MezError::forbidden("channel failed"))
        };
        let cancel = tokio::sync::Notify::new();
        let mut running = Box::pin(coordinate(
            foreground,
            channels,
            cancel.notified(),
            stop,
            Duration::from_secs(1),
        ));
        assert!(matches!(
            futures_util::poll!(&mut running),
            std::task::Poll::Pending
        ));
        if failing {
            release.notify_one();
        } else {
            cancel.notify_one();
        }
        let result = running.await;
        if failing {
            assert_eq!(result.unwrap_err().message(), "channel failed");
        } else {
            result.unwrap();
        }
        assert!(restored.load(Ordering::SeqCst));
        assert!(foreground_dropped.load(Ordering::SeqCst));
        assert!(channel_dropped.load(Ordering::SeqCst));
    }
}

/// A noncooperating foreground is disposed after its finite retirement budget.
/// Abandoning the whole coordinator drops both owners without pretending that
/// asynchronous restoration ran. Persistent cancellation remains visible to a
/// receiver created before the stop signal but first polled afterwards.
#[tokio::test(start_paused = true)]
async fn broker_x11_lifetime_bounds_retirement_and_abandonment() {
    let channel_dropped = Arc::new(AtomicBool::new(false));
    let foreground_dropped = Arc::new(AtomicBool::new(false));
    let (stop, _stopped) = watch::channel(false);
    let cancel = tokio::sync::Notify::new();
    let foreground = async {
        let _owner = Disposed(foreground_dropped.clone());
        std::future::pending::<Result<()>>().await
    };
    let channels = async {
        let _owner = Disposed(channel_dropped.clone());
        std::future::pending::<Result<()>>().await
    };
    let mut running = Box::pin(coordinate(
        foreground,
        channels,
        cancel.notified(),
        stop,
        Duration::from_millis(100),
    ));
    assert!(matches!(
        futures_util::poll!(&mut running),
        std::task::Poll::Pending
    ));
    cancel.notify_one();
    assert!(matches!(
        futures_util::poll!(&mut running),
        std::task::Poll::Pending
    ));
    assert!(channel_dropped.load(Ordering::SeqCst));
    tokio::time::advance(Duration::from_millis(101)).await;
    assert!(
        running
            .await
            .unwrap_err()
            .message()
            .contains("retirement timed out")
    );
    assert!(foreground_dropped.load(Ordering::SeqCst));
    for flag in [&channel_dropped, &foreground_dropped] {
        flag.store(false, Ordering::SeqCst);
    }
    let (stop, stopped) = watch::channel(false);
    let mut abandoned = Box::pin(coordinate(
        async {
            let _owner = Disposed(foreground_dropped.clone());
            std::future::pending::<Result<()>>().await
        },
        async {
            let _owner = Disposed(channel_dropped.clone());
            std::future::pending::<Result<()>>().await
        },
        std::future::pending(),
        stop,
        Duration::from_secs(1),
    ));
    assert!(matches!(
        futures_util::poll!(&mut abandoned),
        std::task::Poll::Pending
    ));
    drop(abandoned);
    assert!(channel_dropped.load(Ordering::SeqCst));
    assert!(foreground_dropped.load(Ordering::SeqCst));
    broker_attachment_cancelled(stopped).await;
    let (stop, stopped) = watch::channel(false);
    stop.send(true).unwrap();
    broker_attachment_cancelled(stopped).await;
}
