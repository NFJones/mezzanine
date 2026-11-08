//! Shared ordinary-append fixtures retain the exact batch offered to a worker.
//!
//! Tests acknowledge these actual immutable owners, rather than reconstructing
//! a new proposal from equal raw rows and accidentally bypassing identity fences.

use super::{RuntimePersistenceComponent, RuntimeSideEffect};
use crate::storage::transcript::TranscriptAppendBatch;

impl RuntimePersistenceComponent {
    /// Selects the genuine queued epoch effect without dispatching unrelated
    /// ordinary batches or creating in-flight owners for work that did not run.
    pub(crate) fn take_replay_epoch_attempt_for_tests(
        &mut self,
    ) -> Option<crate::runtime::RuntimeReplayEpochAttempt> {
        let index = self
            .queued_transcript_effects
            .iter()
            .position(|effect| matches!(effect, RuntimeSideEffect::PersistReplayEpoch { .. }))?;
        match self.queued_transcript_effects.remove(index) {
            RuntimeSideEffect::PersistReplayEpoch { attempt } => Some(attempt),
            _ => unreachable!("the selected queue position was an epoch attempt"),
        }
    }
}

/// Drains only the ordinary append fixtures used by the owning persistence
/// tests. The returned batches are the actual source of subsequent receipts.
pub(super) fn drain_batches(
    component: &mut RuntimePersistenceComponent,
) -> Vec<TranscriptAppendBatch> {
    component
        .take_transcript_effects()
        .into_iter()
        .map(|effect| match effect {
            RuntimeSideEffect::PersistTranscriptEntries { batch, .. } => batch,
            _ => panic!("ordinary append fixture queued a different effect"),
        })
        .collect()
}
