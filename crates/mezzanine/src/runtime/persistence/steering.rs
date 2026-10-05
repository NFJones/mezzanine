//! Exact settled steering write ownership and bounded persistence-only retry.
//!
//! Visible promotion is independently occurrence-fenced. This owner retains the
//! immutable source and destination, rejects conflicting reuse, and never retries
//! input or vendor work. A stale attempt cannot retire a newer write. Exhausted
//! writes remain unavailable rather than falsely acknowledged as durable.

use super::{RuntimePersistenceComponent, RuntimeSideEffect};
use crate::error::{MezError, Result};
use crate::storage::transcript::{AgentPresentationEntry, AgentTranscriptStore};

impl RuntimePersistenceComponent {
    /// Admits one exact source, retaining the original store/path for retry.
    pub(crate) fn queue_steering_presentation(
        &mut self,
        store: AgentTranscriptStore,
        entry: AgentPresentationEntry,
    ) -> Result<()> {
        let source = crate::storage::transcript::steering::Source::decode(
            entry.source_text.as_deref().unwrap_or_default(),
        )?;
        let key = (entry.conversation_id.clone(), source.receipt.id);
        if let Some(RuntimeSideEffect::PersistSteeringPresentation {
            entry: existing, ..
        }) = self.steering_presentation_writes.get(&key)
        {
            if existing.source_text != entry.source_text || existing.turn_id != entry.turn_id {
                return Err(MezError::invalid_state("conflicting steering publication"));
            }
            return Ok(());
        }
        if self.steering_presentation_writes.len() >= 4096 {
            return Err(MezError::invalid_state(
                "steering persistence capacity exhausted",
            ));
        }
        let path = store.presentation_path(&entry.conversation_id)?;
        let generation = self
            .next_steering_presentation_generation
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("steering persistence generation exhausted"))?;
        self.next_steering_presentation_generation = generation;
        let effect = RuntimeSideEffect::PersistSteeringPresentation {
            store,
            path,
            entry,
            generation,
            retry_attempt: 0,
        };
        self.steering_presentation_writes
            .insert(key, effect.clone());
        self.queued_transcript_effects.push(effect);
        Ok(())
    }

    /// Settles only an exact attempt. One retry uses unchanged occurrence source
    /// and a new attempt fence; exhausted writes cannot be replayed by late mail.
    pub(crate) fn settle_steering_presentation(
        &mut self,
        conversation: &str,
        receipt: &str,
        generation: u64,
        path: &std::path::Path,
        success: bool,
    ) -> bool {
        let key = (conversation.to_string(), receipt.to_string());
        let Some(RuntimeSideEffect::PersistSteeringPresentation {
            generation: current,
            path: expected,
            retry_attempt,
            ..
        }) = self.steering_presentation_writes.get(&key)
        else {
            return false;
        };
        if *current != generation || expected != path {
            return false;
        }
        let retry = !success && *retry_attempt == 0;
        if success {
            self.steering_presentation_writes.remove(&key);
        } else if retry {
            let Some(next) = self.next_steering_presentation_generation.checked_add(1) else {
                return true;
            };
            self.next_steering_presentation_generation = next;
            if let Some(effect @ RuntimeSideEffect::PersistSteeringPresentation { .. }) =
                self.steering_presentation_writes.get_mut(&key)
            {
                if let RuntimeSideEffect::PersistSteeringPresentation {
                    generation,
                    retry_attempt,
                    ..
                } = effect
                {
                    *generation = next;
                    *retry_attempt = 1;
                }
                self.queued_transcript_effects.push(effect.clone());
            }
        }
        true
    }
}

#[cfg(test)]
mod tests;
