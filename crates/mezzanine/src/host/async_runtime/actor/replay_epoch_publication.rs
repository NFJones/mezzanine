//! Reuses the admitted compactor watchdog through physical publication/adoption.
//! Exact keys retire only their own generation; persistence callbacks cannot
//! release a later provider claim or restart an ambiguous inference request.

use super::*;

impl AsyncRuntimeSessionActor {
    /// Replaces the existing admitted claim timer, without reserving another
    /// worker claim or creating a second publication execution lane.
    pub(super) fn refresh_replay_epoch_publication_timer(
        &mut self,
        pane: &str,
        generation: u64,
    ) -> Vec<RuntimeSideEffect> {
        let key = RuntimeTimerKey::new(RuntimeTimerKind::CompactionClaim, pane, generation);
        self.timers.compaction_claim.remove(&key);
        let mut effects = vec![RuntimeSideEffect::CancelTimer { key: key.clone() }];
        if self
            .service
            .replay_epoch_publication(pane, generation)
            .is_some()
            && self
                .service
                .agent_compaction_task_is_claimed(pane, generation)
        {
            self.timers.compaction_claim.insert(key.clone());
            effects.push(RuntimeSideEffect::ScheduleTimer {
                key,
                delay_ms: 1000,
            });
        }
        effects
    }
}
