//! Shared client-local render cadence, independent of transport ownership.
//!
//! Preserves the established attach scheduling contract: animation deadlines
//! follow completed renders; ordinary fetches retain one latest-state request
//! rather than a queue of captured frames. Missing/zero rate imposes no guessed
//! ceiling. Callers decide which actions bypass ordinary pacing and update only
//! after their exact output commit. This module does not render, authenticate,
//! acknowledge receipts, or drive event/input ownership.

/// Tracks the local animation refresh deadline for an interactive attach.
#[derive(Debug, Default)]
pub(crate) struct AttachAnimationRefresh {
    /// Current refresh interval advertised by the last rendered view.
    interval_ms: Option<u64>,
    /// Next local deadline for an animation-only terminal view.
    deadline: Option<tokio::time::Instant>,
}

impl AttachAnimationRefresh {
    /// Returns the next animation refresh deadline, when animation is active.
    pub(crate) fn deadline(&self) -> Option<tokio::time::Instant> {
        self.deadline
    }

    /// Updates the local refresh schedule from the latest completed view.
    pub(crate) fn update_from_rendered_view(&mut self, refresh_interval_ms: u64) {
        if refresh_interval_ms == 0 {
            self.interval_ms = None;
            self.deadline = None;
            return;
        }
        self.interval_ms = Some(refresh_interval_ms);
        self.deadline = Some(
            tokio::time::Instant::now() + std::time::Duration::from_millis(refresh_interval_ms),
        );
    }
}

/// Paces only ordinary view fetches; pending work represents latest server state,
/// not a queue of frames captured at event arrival.
#[derive(Debug, Default)]
pub(crate) struct AttachOrdinaryRenderRate {
    /// Minimum interval resolved from the latest view, if pacing is enabled.
    min_interval: Option<std::time::Duration>,
    /// Completion time of the latest rendered view or inline repaint.
    last_rendered_at: Option<tokio::time::Instant>,
}

impl AttachOrdinaryRenderRate {
    /// Applies policy from the most recent exact-client view. Missing policy on
    /// an older server means no inferred rate ceiling.
    pub(crate) fn update_from_rendered_view(&mut self, fps: Option<u64>) {
        self.min_interval = fps.filter(|fps| *fps != 0).map(|fps| {
            std::time::Duration::from_millis(1_000u64.saturating_add(fps.saturating_sub(1)) / fps)
        });
        self.last_rendered_at = Some(tokio::time::Instant::now());
    }

    /// Records an inline repaint without replacing the last advertised policy.
    pub(crate) fn mark_inline_rendered(&mut self) {
        self.last_rendered_at = Some(tokio::time::Instant::now());
    }

    /// Returns the earliest permitted pending ordinary render, if gated.
    pub(crate) fn deadline(&self) -> Option<tokio::time::Instant> {
        Some(self.last_rendered_at? + self.min_interval?)
    }

    /// Reports whether an ordinary render may fetch the current server view.
    pub(crate) fn ready(&self) -> bool {
        self.deadline()
            .is_none_or(|deadline| deadline <= tokio::time::Instant::now())
    }
}

#[cfg(test)]
mod tests;
