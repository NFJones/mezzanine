//! Canonical pending history captured from actual immutable append owners.
//!
//! Raw bookkeeping projections retain their existing narrow API. Canonical
//! preparation additionally freezes original occurrence/cost proposals from the
//! same queued and in-flight batches. This is an immutable input projection,
//! never a new source ledger, admission capability or durability receipt.

use super::{RuntimePersistenceComponent, RuntimeSideEffect};
use crate::storage::transcript::{
    ConversationReplaySnapshot, TranscriptAppendBatch, TranscriptRowOrigins, TranscriptRowRecords,
};
use crate::{MezError, Result};
use mez_agent::TranscriptEntry;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// Physical pending rows and original descriptive accounting from their owners.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RuntimePendingTranscriptContext {
    entries: Vec<TranscriptEntry>,
    origins: TranscriptRowOrigins,
    records: TranscriptRowRecords,
}

impl RuntimePendingTranscriptContext {
    /// Captures a new child's actual immutable initial or ordinary batch.
    pub(crate) fn from_batch(batch: &TranscriptAppendBatch) -> Result<Self> {
        let mut context = Self {
            entries: batch.entries().to_vec(),
            ..Self::default()
        };
        if !context.entries.is_empty() {
            context.include(batch)?;
        }
        Ok(context)
    }

    /// Borrows exact source rows without manufacturing accounting or ownership.
    pub(crate) fn entries(&self) -> &[TranscriptEntry] {
        &self.entries
    }

    /// Reports absence of physical rows, independently of committed history.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Counts distinct physical pending rows in this immutable capture.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Iterates the exact source rows selected from existing append owners.
    pub(crate) fn iter(&self) -> std::slice::Iter<'_, TranscriptEntry> {
        self.entries.iter()
    }

    /// Provides an explicit unaccounted import for legacy-source fixtures only.
    /// New runtime producers must supply their actual immutable proposals.
    #[cfg(test)]
    pub(crate) fn unaccounted(entries: Vec<TranscriptEntry>) -> Self {
        Self {
            entries,
            ..Self::default()
        }
    }

    /// Extends only a legacy fixture, never an actual admitted proposal.
    #[cfg(test)]
    pub(crate) fn push_unaccounted(&mut self, entry: TranscriptEntry) {
        assert!(self.origins.is_empty());
        self.entries.push(entry);
    }

    /// Retires only a fixture's pending projection after its original commit.
    #[cfg(test)]
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Qualifies proposed accounting against this original checked row view.
    /// A committed overlap must already own the same occurrence; equal bytes
    /// alone cannot let a cancelled proposal adopt a later committed producer.
    pub(crate) fn projected_accounting(
        &self,
        snapshot: &ConversationReplaySnapshot,
    ) -> Result<(TranscriptRowOrigins, TranscriptRowRecords)> {
        let mut origins = snapshot.row_origins.clone();
        let mut records = snapshot.row_records.clone();
        let selected = snapshot
            .view
            .logical
            .iter()
            .map(|row| (row.sequence, row))
            .collect::<BTreeMap<_, _>>();
        let committed = snapshot
            .view
            .committed
            .iter()
            .map(|row| row.sequence)
            .collect::<BTreeSet<_>>();
        if let Some(replay) = &snapshot.replay {
            for row in snapshot.view.logical.iter().filter(|row| {
                replay.row_is_authored_arrival(row.sequence) && !committed.contains(&row.sequence)
            }) {
                let authored =
                    mez_agent::transcript::TranscriptContextRecord::from_transcript_content(
                        &row.content,
                    )?
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "pending replay arrival has no original authored source",
                        )
                    })?;
                if self.origins.get(&row.sequence) != Some(&authored.record().occurrence_id()) {
                    return Err(MezError::conflict(
                        "pending replay arrival lacks original append ownership",
                    ));
                }
            }
        }
        for (sequence, origin) in &self.origins {
            let Some(row) = selected.get(sequence) else {
                continue;
            };
            let original = self
                .entries
                .binary_search_by_key(sequence, |row| row.sequence)
                .ok()
                .and_then(|index| self.entries.get(index));
            if original != Some(*row) {
                return Err(MezError::conflict(
                    "pending canonical source changed physical rows",
                ));
            }
            if committed.contains(sequence) {
                if origins.get(sequence) != Some(origin) {
                    return Err(MezError::conflict(
                        "committed pending occurrence has a different owner",
                    ));
                }
            } else {
                origins.insert(*sequence, *origin);
                records.remove(sequence);
                if let Some(record) = self.records.get(sequence) {
                    records.insert(*sequence, *record);
                }
            }
        }
        Ok((origins, records))
    }

    /// Merges accounting only from the same full-field pending source. Overlap
    /// cannot deduplicate another proposal just because its raw bytes are equal.
    fn include(&mut self, batch: &TranscriptAppendBatch) -> Result<()> {
        if batch.entries().is_empty() {
            return Ok(());
        }
        let (origins, records) = if let Some(accounting) = batch.row_accounting() {
            accounting.validate(batch.entries())?;
            (accounting.origins(), accounting.records())
        } else if let Some(accounting) = batch.initial_row_accounting() {
            accounting.validate(batch.entries())?;
            (accounting.origins(), accounting.records())
        } else {
            return Err(MezError::invalid_state(
                "pending append is missing its original occurrence proposal",
            ));
        };
        for row in batch.entries() {
            if self
                .entries
                .binary_search_by_key(&row.sequence, |entry| entry.sequence)
                .ok()
                .and_then(|index| self.entries.get(index))
                != Some(row)
            {
                return Err(MezError::conflict(
                    "pending append proposal differs from its raw source",
                ));
            }
        }
        for (sequence, origin) in origins {
            if self
                .origins
                .insert(*sequence, *origin)
                .is_some_and(|previous| previous != *origin)
            {
                return Err(MezError::conflict(
                    "pending append occurrence proposals conflict",
                ));
            }
        }
        for (sequence, record) in records {
            if self
                .records
                .insert(*sequence, *record)
                .is_some_and(|previous| previous != *record)
            {
                return Err(MezError::conflict("pending append cost proposals conflict"));
            }
        }
        Ok(())
    }

    /// Ordinary metadata cannot smuggle initial replay authority through the
    /// wrong effect class or bypass the initial readiness owner.
    fn include_ordinary(&mut self, batch: &TranscriptAppendBatch) -> Result<()> {
        if batch.row_accounting().is_none() || batch.has_initial_authority() {
            return Err(MezError::invalid_state(
                "ordinary pending effect has invalid accounting ownership",
            ));
        }
        self.include(batch)
    }

    /// Initial metadata remains owned independently of a genuinely empty tail.
    fn include_initial(&mut self, batch: &TranscriptAppendBatch) -> Result<()> {
        if !batch.has_initial_authority() {
            return Err(MezError::invalid_state(
                "initial pending effect has no initial authority",
            ));
        }
        self.include(batch)
    }
}

impl RuntimePersistenceComponent {
    /// Retry must retain the same complete actual proposal at its original path.
    pub(super) fn pending_transcript_proposal_matches(
        &self,
        path: &Path,
        batch: &TranscriptAppendBatch,
    ) -> bool {
        self.in_flight_transcript_entries
            .values()
            .flatten()
            .any(|(owner_path, owner)| owner_path == path && owner == batch)
            || self.queued_transcript_effects.iter().any(|effect| {
                matches!(effect,
                RuntimeSideEffect::PersistTranscriptEntries { path: owner_path, batch: owner, .. }
                    if owner_path == path && owner == batch)
            })
    }

    /// Captures canonical metadata from the exact same original-target queues
    /// as raw bookkeeping, retaining original values before asynchronous work.
    pub(crate) fn pending_transcript_context_at_path(
        &self,
        path: &Path,
        cid: &str,
    ) -> Result<RuntimePendingTranscriptContext> {
        let mut context = RuntimePendingTranscriptContext {
            entries: self.pending_transcript_entries_at_path(path, cid)?,
            ..RuntimePendingTranscriptContext::default()
        };
        for ((owner, _), batches) in &self.in_flight_transcript_entries {
            if owner == cid {
                for (owned_path, batch) in batches {
                    if owned_path == path {
                        context.include_ordinary(batch)?;
                    }
                }
            }
        }
        if let Some(batches) = self.in_flight_initial_transcripts.get(cid) {
            for (owned_path, batch) in batches {
                if owned_path == path {
                    context.include_initial(batch)?;
                }
            }
        }
        for effect in &self.queued_transcript_effects {
            match effect {
                RuntimeSideEffect::PersistTranscriptEntries {
                    path: owned_path,
                    batch,
                    ..
                } if owned_path == path && batch.conversation_id() == cid => {
                    context.include_ordinary(batch)?
                }
                RuntimeSideEffect::PersistInitialTranscript {
                    path: owned_path,
                    batch,
                    ..
                } if owned_path == path && batch.conversation_id() == cid => {
                    context.include_initial(batch)?
                }
                _ => {}
            }
        }
        Ok(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::transcript::{AgentCompactionEpoch, AgentTranscriptStore};

    /// A real metadata-only initial projection remains a valid pending owner
    /// with no manufactured physical row. Its effect class still determines
    /// authority; moving that same payload into an ordinary effect must fail.
    #[test]
    fn metadata_only_initial_pending_context_preserves_effect_authority() {
        let root = std::env::temp_dir().join(format!(
            "mez-pending-initial-{}",
            String::from(mez_agent::ContextBlockOccurrenceId::allocate())
        ));
        let store = AgentTranscriptStore::new(&root);
        let cid = "metadata-only-pending";
        let path = store.transcript_path(cid).unwrap();
        let batch = TranscriptAppendBatch::initial_projection(
            AgentCompactionEpoch {
                version: 1,
                conversation_id: cid.into(),
                through_sequence: 0,
                summary: "inherited summary".into(),
                ranges: Vec::new(),
                summary_records: None,
            },
            Vec::new(),
        )
        .unwrap();
        let mut component = RuntimePersistenceComponent::default();
        component.queue_transcript(RuntimeSideEffect::PersistInitialTranscript {
            store: store.clone(),
            path: path.clone(),
            batch: batch.clone(),
        });
        assert!(
            component
                .pending_transcript_context_at_path(&path, cid)
                .unwrap()
                .is_empty()
        );
        assert!(!store.transcript_path(cid).unwrap().exists());
        let mut wrong = RuntimePersistenceComponent::default();
        wrong.queue_transcript(RuntimeSideEffect::PersistTranscriptEntries {
            store,
            path: path.clone(),
            batch,
        });
        assert!(
            wrong
                .pending_transcript_context_at_path(&path, cid)
                .is_err()
        );
        assert!(!root.exists());
    }
}
