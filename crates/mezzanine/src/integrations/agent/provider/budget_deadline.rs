//! Original-worker active-time projection. This carries no policy or admission
//! authority; only an actor-acknowledged budget park may open its inactive span.
//! Cumulative duration survives coalesced notifications, and dropping the exact
//! guard closes that span on resume, cancellation, or unwind.

use std::{future::Future, time::Duration};
use tokio::{sync::watch, time::Instant};

#[derive(Clone, Copy, Default)]
struct State {
    excluded: Duration,
    parked: Option<Instant>,
}

impl State {
    fn excluded_at(self, now: Instant) -> Duration {
        self.excluded.saturating_add(
            self.parked
                .map_or(Duration::ZERO, |start| now.saturating_duration_since(start)),
        )
    }
}

#[derive(Clone)]
pub(crate) struct ProviderBudgetDeadline(watch::Sender<State>);

impl Default for ProviderBudgetDeadline {
    fn default() -> Self {
        Self(watch::channel(State::default()).0)
    }
}

pub(crate) struct Parked {
    clock: ProviderBudgetDeadline,
    start: Instant,
}

impl ProviderBudgetDeadline {
    pub(crate) fn park(&self) -> crate::Result<Parked> {
        let start = Instant::now();
        if !self.0.send_if_modified(|state| {
            if state.parked.is_some() {
                return false;
            }
            state.parked = Some(start);
            true
        }) {
            return Err(crate::MezError::invalid_state(
                "original budget deadline already parked",
            ));
        }
        Ok(Parked {
            clock: self.clone(),
            start,
        })
    }

    /// Same finite allowance and future throughout. Historic router waits before
    /// this runner starts are subtracted from the baseline, not granted again.
    pub(crate) async fn timeout<F: Future>(
        &self,
        allowance: Duration,
        future: F,
    ) -> Result<F::Output, ()> {
        let mut changes = self.0.subscribe();
        let start = Instant::now();
        let baseline = changes.borrow().excluded_at(start);
        let mut future = Box::pin(future);
        loop {
            let state = *changes.borrow_and_update();
            let now = Instant::now();
            let excluded = state.excluded_at(now).saturating_sub(baseline);
            let active = now
                .saturating_duration_since(start)
                .saturating_sub(excluded);
            let remaining = allowance.saturating_sub(active);
            if remaining.is_zero() {
                return Err(());
            }
            tokio::select! {
                biased;
                changed = changes.changed() => { if changed.is_err() { return Err(()); } }
                result = &mut future => return Ok(result),
                _ = tokio::time::sleep(remaining), if state.parked.is_none() => return Err(()),
            }
        }
    }
}

impl Drop for Parked {
    fn drop(&mut self) {
        self.clock.0.send_modify(|state| {
            if state.parked == Some(self.start) {
                state.excluded = state
                    .excluded
                    .saturating_add(Instant::now().saturating_duration_since(self.start));
                state.parked = None;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn budget_deadline_excludes_only_original_park_and_expires_after_resume() {
        let clock = ProviderBudgetDeadline::default();
        let guarded = clock.clone();
        let task = tokio::spawn(async move {
            guarded
                .timeout(Duration::from_secs(10), std::future::pending::<()>())
                .await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(3)).await;
        let parked = clock.park().unwrap();
        assert!(clock.park().is_err());
        tokio::time::advance(Duration::from_secs(100)).await;
        assert!(!task.is_finished());
        drop(parked);
        tokio::time::advance(Duration::from_secs(6)).await;
        assert!(!task.is_finished());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(task.await.unwrap(), Err(()));
    }
}
