//! Whole initial fork claims settle before their child can dispatch a provider.
//!
//! The router retains the complete payload through admission and worker replay.
//! An event must match claim generation, original path and every projection byte.
//! Failed or stale events cannot release a replacement owner. Successful service
//! settlement removes the startup fence before ready work is dispatched again.

use super::coalesce::async_runtime_current_unix_millis;
use super::{AsyncRuntimeSessionActor, PersistenceEvent, Result, RuntimeTransition};

impl AsyncRuntimeSessionActor {
    /// Applies only exact initial-publication outcomes. Unsupported events and
    /// stale generations are no-ops; errors preserve the owned startup fence.
    pub(super) fn apply_initial_transcript_persistence_event(
        &mut self,
        event: PersistenceEvent,
        claim_id: Option<u64>,
    ) -> Result<RuntimeTransition> {
        let Some(claim_id) = claim_id.filter(|id| *id != 0) else {
            // Initial publication has no untagged compatibility outcome.
            return Ok(RuntimeTransition::default());
        };
        let (batch, path, completed, retryable) = match &event {
            PersistenceEvent::InitialTranscriptCompleted { batch, path } => {
                (batch.clone(), path.clone(), true, true)
            }
            PersistenceEvent::InitialTranscriptFailed {
                batch,
                path,
                retryable,
                ..
            } => (batch.clone(), path.clone(), false, *retryable),
            _ => return Ok(RuntimeTransition::default()),
        };
        if !self
            .side_effect_routes
            .owns_claimed_initial_transcript(claim_id, &batch, &path)
        {
            return Ok(RuntimeTransition::default());
        }
        if !completed {
            if retryable {
                self.side_effect_routes.recover_claimed_transcripts();
            } else {
                self.side_effect_routes
                    .block_claimed_initial_transcript(claim_id, &batch, &path);
            }
        }
        let mut transition = self.service.apply_persistence_transition(event)?;
        if completed && transition.applied {
            self.side_effect_routes
                .settle_claimed_initial_transcript(claim_id, &batch, &path);
            self.service.start_ready_agent_turns()?;
            self.dispatch_pending_agent_prompt_history();
            transition
                .side_effects
                .extend(self.pending_provider_dispatch_side_effects()?);
        }
        self.queue_peer_message_delivery_timer_if_needed(async_runtime_current_unix_millis())?;
        Ok(transition)
    }
}
