//! Shared render cadence exercised with virtual time, without terminal effects.
use super::*;

/// Verifies the first render is immediate, configured cadence bounds subsequent
/// ordinary renders, and disabled/absent policy does not infer a guessed delay.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn ordinary_render_rate_tracks_live_policy() {
    let mut rate = AttachOrdinaryRenderRate::default();
    assert!(rate.ready());
    rate.update_from_rendered_view(Some(10));
    assert!(!rate.ready());
    assert_eq!(
        rate.deadline().unwrap() - tokio::time::Instant::now(),
        std::time::Duration::from_millis(100)
    );
    tokio::time::advance(std::time::Duration::from_millis(99)).await;
    assert!(!rate.ready());
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    assert!(rate.ready());
    rate.update_from_rendered_view(Some(0));
    assert!(rate.ready());
    assert!(rate.deadline().is_none());
    rate.update_from_rendered_view(Some(30));
    assert!(!rate.ready());
    tokio::time::advance(std::time::Duration::from_millis(10)).await;
    rate.mark_inline_rendered();
    assert_eq!(
        rate.deadline().unwrap() - tokio::time::Instant::now(),
        std::time::Duration::from_millis(34)
    );
    rate.update_from_rendered_view(None);
    assert!(rate.ready());
}

/// Animation scheduling resets from the latest committed view, and disabling it
/// clears the deadline rather than leaving stale scheduled work behind.
#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn animation_render_cadence_tracks_completed_views() {
    let mut animation = AttachAnimationRefresh::default();
    assert!(animation.deadline().is_none());
    animation.update_from_rendered_view(100);
    tokio::time::advance(std::time::Duration::from_millis(50)).await;
    animation.update_from_rendered_view(200);
    assert_eq!(
        animation.deadline().unwrap() - tokio::time::Instant::now(),
        std::time::Duration::from_millis(200)
    );
    animation.update_from_rendered_view(0);
    assert!(animation.deadline().is_none());
}
