//! Shared ordinary-append fixtures retain the exact batch offered to a worker.
//!
//! Tests acknowledge these actual immutable owners, rather than reconstructing
//! a new proposal from equal raw rows and accidentally bypassing identity fences.

use super::{RuntimePersistenceComponent, RuntimeSideEffect};
use crate::storage::transcript::TranscriptAppendBatch;

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
