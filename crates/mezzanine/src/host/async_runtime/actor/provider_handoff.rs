//! Source-owned provider dispatch transfer under transient lane pressure.
//!
//! The service retains actual pending work; this module stores only turn IDs
//! refused after commitment or already offered during recovery. An offer lasts
//! through off-actor preparation until the actor processes its no-work result
//! or claim. It is not permission to execute, a second work queue, or a reason
//! to replay an applied event.

use super::{AgentId, AsyncRuntimeSessionActor, MezError, Result, RuntimeSideEffect};

impl AsyncRuntimeSessionActor {
    /// Retains only source identities for refused committed descriptors. Normal
    /// pending tasks remain governed by their existing poll/retry scheduling.
    pub(super) fn record_refused_provider_sources(&mut self, effects: &[RuntimeSideEffect]) {
        self.refused_provider_turns
            .retain(|turn_id| self.service.agent_provider_task_is_pending(turn_id));
        for effect in effects {
            if let RuntimeSideEffect::DispatchAgentProvider { turn_id, .. } = effect
                && self.service.agent_provider_task_is_pending(turn_id)
            {
                self.refused_provider_turns.insert(turn_id.clone());
            }
        }
    }

    /// Returns one recovered offer to its current source after preparation or
    /// claim settles. Still-pending work remains eligible for later recovery;
    /// a claimed, cancelled, or retired source cannot acquire a new descriptor.
    pub(super) fn settle_provider_offer(&mut self, turn_id: &str) {
        if self.offered_provider_turns.remove(turn_id)
            && self.service.agent_provider_task_is_pending(turn_id)
        {
            self.refused_provider_turns.insert(turn_id.to_string());
        }
    }

    /// Fences drained provider descriptors before any drain-triggered refill.
    /// Only genuine service-owned tasks acquire metadata; arbitrary adapter
    /// descriptors cannot grow the fence independently of active task owners.
    pub(super) fn record_provider_offers(&mut self, effects: &[RuntimeSideEffect]) {
        for effect in effects {
            if let RuntimeSideEffect::DispatchAgentProvider { turn_id, .. } = effect
                && self.refused_provider_turns.remove(turn_id)
                && self.service.agent_provider_task_is_owned(turn_id)
            {
                self.offered_provider_turns.insert(turn_id.clone());
            }
        }
    }

    /// Reprojects only a fitting prefix of genuinely pending provider tasks.
    /// The serialized actor keeps capacity stable between selection and enqueue.
    /// Queued/offered descriptors are excluded; task payloads never leave their
    /// existing service owner until the ordinary validated claim succeeds.
    pub(super) fn queue_fitting_provider_sources(&mut self) -> Result<usize> {
        self.offered_provider_turns
            .retain(|turn_id| self.service.agent_provider_task_is_owned(turn_id));
        self.refused_provider_turns
            .retain(|turn_id| self.service.agent_provider_task_is_pending(turn_id));
        if self.refused_provider_turns.is_empty() {
            return Ok(0);
        }
        let limit = self.available_committed_side_effect_capacity();
        let mut effects = Vec::new();
        for task in self.service.pending_agent_provider_tasks() {
            if effects.len() == limit {
                break;
            }
            if !self.refused_provider_turns.contains(&task.turn_id)
                || self.provider_dispatch_is_already_queued(&task.turn_id)
            {
                continue;
            }
            let agent_id = AgentId::opaque(task.agent_id).ok_or_else(|| {
                MezError::invalid_state("pending provider source has an invalid agent identity")
            })?;
            effects.push(RuntimeSideEffect::DispatchAgentProvider {
                agent_id,
                turn_id: task.turn_id,
            });
        }
        let count = effects.len();
        if count > 0 {
            self.admit_committed_side_effects(effects)?;
        }
        Ok(count)
    }
}
