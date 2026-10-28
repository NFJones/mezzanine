//! Initial fork publication retains whole-payload ownership through settlement.
//!
//! Ordinary row appends and initial publication have different admission
//! boundaries: the latter must commit its epoch with the rows before readiness.
//! These owners therefore retain one validated batch, including metadata-only
//! payloads, until exact settlement. Pending raw projection and allocation are
//! conveniences, never substitutes for this owner or proof of disk durability.

use super::{RuntimePersistenceComponent, RuntimeSideEffect};
use crate::storage::transcript::TranscriptAppendBatch;
use crate::{MezError, Result};
use mez_agent::TranscriptEntry;
use std::path::Path;

impl RuntimePersistenceComponent {
    /// Reports queued or worker-owned initial authority independently of rows.
    pub(crate) fn initial_transcript_pending(&self, cid: &str) -> bool {
        self.in_flight_initial_transcripts.get(cid).is_some_and(|owners| !owners.is_empty())
            || self.queued_transcript_effects.iter().any(|effect| matches!(effect,
                RuntimeSideEffect::PersistInitialTranscript {batch,..} if batch.conversation_id() == cid))
    }

    /// Captures one complete initial owner when its effect leaves the queue.
    pub(super) fn retain_initial_transcript_effect(&mut self, effect: &RuntimeSideEffect) {
        if let RuntimeSideEffect::PersistInitialTranscript { path, batch, .. } = effect {
            self.in_flight_initial_transcripts
                .entry(batch.conversation_id().into())
                .or_default()
                .push((path.clone(), batch.clone()));
        }
    }

    /// Borrows physical pending rows without treating their metadata as a row.
    pub(super) fn pending_initial_transcript_entries(&self, cid: &str) -> Vec<TranscriptEntry> {
        let mut rows = self
            .in_flight_initial_transcripts
            .get(cid)
            .into_iter()
            .flatten()
            .flat_map(|(_, batch)| batch.entries().iter().cloned())
            .collect::<Vec<_>>();
        rows.extend(
            self.queued_transcript_effects
                .iter()
                .filter_map(|effect| match effect {
                    RuntimeSideEffect::PersistInitialTranscript { batch, .. }
                        if batch.conversation_id() == cid =>
                    {
                        Some(batch.entries())
                    }
                    _ => None,
                })
                .flatten()
                .cloned(),
        );
        rows
    }

    /// Checks the original path and CID before exposing pending physical rows.
    /// Metadata remains owned even when the returned row vector is empty.
    pub(super) fn pending_initial_transcript_entries_at_path(
        &self,
        path: &Path,
        cid: &str,
    ) -> Result<Vec<TranscriptEntry>> {
        let mut rows = Vec::new();
        if let Some(owners) = self.in_flight_initial_transcripts.get(cid) {
            for (owner_path, batch) in owners {
                if owner_path == path {
                    rows.extend_from_slice(batch.entries());
                }
            }
        }
        for effect in &self.queued_transcript_effects {
            if let RuntimeSideEffect::PersistInitialTranscript {
                store,
                path: owner_path,
                batch,
            } = effect
                && owner_path == path
            {
                if batch.conversation_id() != cid || store.transcript_path(cid)? != path {
                    return Err(MezError::invalid_state(
                        "initial transcript pending owner changed its original target",
                    ));
                }
                rows.extend_from_slice(batch.entries());
            }
        }
        Ok(rows)
    }

    /// Tests exact payload/path identity, including every initial epoch byte.
    pub(crate) fn owns_initial_transcript_write_at_path(
        &self,
        batch: &TranscriptAppendBatch,
        path: &Path,
    ) -> bool {
        self.in_flight_initial_transcripts
            .get(batch.conversation_id())
            .is_some_and(|owners| {
                owners
                    .iter()
                    .any(|(owner_path, owned)| owner_path == path && owned == batch)
            })
    }

    /// Retires exactly one successful owner after checked whole-batch settlement.
    /// An empty projection advances no raw frontier and still unblocks startup.
    pub(crate) fn settle_initial_transcript_write_at_path(
        &mut self,
        batch: &TranscriptAppendBatch,
        path: &Path,
    ) -> bool {
        if !self.owns_initial_transcript_write_at_path(batch, path) {
            return false;
        }
        if let Some(last) = batch.entries().last() {
            self.observe_committed_transcript_at_path(path, batch.conversation_id(), last.sequence);
        }
        if let Some(owners) = self
            .in_flight_initial_transcripts
            .get_mut(batch.conversation_id())
        {
            if let Some(index) = owners
                .iter()
                .position(|(owner_path, owned)| owner_path == path && owned == batch)
            {
                owners.remove(index);
            }
            if owners.is_empty() {
                self.in_flight_initial_transcripts
                    .remove(batch.conversation_id());
            }
        }
        true
    }
}
