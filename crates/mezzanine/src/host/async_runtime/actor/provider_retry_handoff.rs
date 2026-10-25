//! Exact committed provider-retry timer handoff under transient lane pressure.
//!
//! The service reducer owns the attempt, immutable chosen backoff and pending
//! phase. This adapter retains only one original key/deadline per turn, never a
//! request, error payload or replayable failure. Enrollment requires an exact
//! committed source projection; strict producers and other timer kinds do not
//! acquire recovery custody. Admission is not worker receipt: queued delay is
//! deducted again at the actual offer. Only the normal matching timer event
//! advances the reducer and normal provider claims still authorize inference.

use super::{
    AsyncRuntimeSessionActor, Result, RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind,
};

impl AsyncRuntimeSessionActor {
    /// Captures the first deadline of a reducer-proven committed schedule.
    /// Reoffering an already retained key cannot restart its backoff clock.
    pub(super) fn retain_committed_provider_retry_deadlines(
        &mut self,
        effects: &[RuntimeSideEffect],
    ) {
        for effect in effects {
            let RuntimeSideEffect::ScheduleTimer { key, delay_ms } = effect else {
                continue;
            };
            if key.kind != RuntimeTimerKind::ProviderRetry
                || self.service.pending_agent_provider_retry_timer(&key.owner_id) != Some((key.generation, *delay_ms))
                || self.timers.pending_provider_retry.get(&key.owner_id).is_some_and(|(pending, _)| pending == key)
                // A descriptor already offered must never be enrolled again.
                || self.timers.provider_retry.get(&key.owner_id) == Some(key)
            {
                continue;
            }
            self.timers.pending_provider_retry.insert(
                key.owner_id.clone(),
                (
                    key.clone(),
                    tokio::time::Instant::now() + std::time::Duration::from_millis(*delay_ms),
                ),
            );
        }
    }

    /// Transfers fitting cancel/schedule prefixes from retained live sources.
    /// A settled/replaced attempt prunes only its own unissued deadline; any
    /// issued obsolete key remains exact cancellation work until admitted.
    pub(super) fn queue_fitting_provider_retry_timers(&mut self) -> Result<usize> {
        let owners = self
            .timers
            .pending_provider_retry
            .keys()
            .chain(self.timers.provider_retry.keys())
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let mut queued = 0;
        for owner in owners {
            let current =
                self.service
                    .pending_agent_provider_retry_timer(&owner)
                    .map(|(attempt, _)| {
                        RuntimeTimerKey::new(RuntimeTimerKind::ProviderRetry, &owner, attempt)
                    });
            if self
                .timers
                .pending_provider_retry
                .get(&owner)
                .is_some_and(|(key, _)| Some(key) != current.as_ref())
            {
                self.timers.pending_provider_retry.remove(&owner);
            }
            let previous = self.timers.provider_retry.get(&owner).cloned();
            let mut effects = Vec::new();
            if let Some(key) = previous
                .as_ref()
                .filter(|key| Some(*key) != current.as_ref())
            {
                effects.push(RuntimeSideEffect::CancelTimer { key: key.clone() });
            }
            if let Some((key, deadline)) = self.timers.pending_provider_retry.get(&owner)
                && previous.as_ref() != Some(key)
            {
                effects.push(RuntimeSideEffect::ScheduleTimer {
                    key: key.clone(),
                    delay_ms: Self::remaining_timer_delay_ms(*deadline),
                });
            }
            effects.truncate(self.available_committed_side_effect_capacity());
            queued += effects.len();
            self.admit_committed_side_effects(effects)?;
        }
        Ok(queued)
    }

    /// Deducts queued waiting and acknowledges only an exact worker offer.
    /// This is not execution evidence or a guarantee that the worker is timely.
    pub(super) fn record_provider_retry_timer_offers(&mut self, effects: &mut [RuntimeSideEffect]) {
        for effect in effects {
            if let RuntimeSideEffect::ScheduleTimer { key, delay_ms } = effect
                && key.kind == RuntimeTimerKind::ProviderRetry
                && let Some((pending, deadline)) =
                    self.timers.pending_provider_retry.get(&key.owner_id)
                && pending == key
            {
                *delay_ms = Self::remaining_timer_delay_ms(*deadline);
                self.timers.pending_provider_retry.remove(&key.owner_id);
            }
        }
    }
}
