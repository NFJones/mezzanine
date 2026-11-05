//! Structural accounting transport from one actual transcript projection.
//!
//! Indices belong to the exact emitted block order, not a label/text search.
//! Legacy rows establish an explicit archive-position import origin; accounted
//! records preserve their committed origin and cached estimates. Neither path
//! changes source roles or restores instructions. Malformed reserved records
//! retain an error through preparation so partial source cannot reach a model.
//! Repaired authored costs use the same occurrence-bound row projection as
//! legacy costs; stale raw envelopes remain immutable source, not a reason to
//! repeat estimation after each cold replay.

use crate::error::{MezError, Result};
use mez_agent::transcript::{TranscriptContextEvent, TranscriptContextRecord, TranscriptEntry};
use mez_agent::{
    AgentContext, ContextBlock, ContextBlockOccurrenceId, ContextBlockRecord,
    StableContextBlockRecord,
};

/// Non-model-visible records for the exact projected history prefix.
#[derive(Debug, Clone, Default)]
pub(in crate::runtime) struct RuntimeContextRecords {
    records: Vec<(usize, ContextBlockRecord)>,
    stable: Option<Vec<StableContextBlockRecord>>,
    invalid: bool,
    /// Content-free rejection from the original pending source qualification.
    pending_rejection: Option<(crate::MezErrorKind, String)>,
    row_origins: crate::storage::transcript::TranscriptRowOrigins,
    row_cache: crate::storage::transcript::TranscriptRowRecords,
    rendered_rows: crate::storage::transcript::TranscriptRowRecords,
    #[cfg(test)]
    estimate_recomputations: usize,
}

/// One semantic row decoded exactly once before execution-group validation.
pub(super) struct DecodedContextRow {
    pub(super) event: Option<TranscriptContextEvent>,
    pub(super) record: Option<ContextBlockRecord>,
    pub(super) stable: Option<Vec<StableContextBlockRecord>>,
}

impl RuntimeContextRecords {
    /// Captures original placement and descriptive costs before rendering any
    /// blocks. Structural row positions select caches; actual rendering restores
    /// them before use, so no temporary estimated records are constructed.
    pub(super) fn for_rows(
        accounting: Option<&crate::storage::transcript::ConversationReplaySnapshot>,
        pending: Option<&crate::runtime::persistence::RuntimePendingTranscriptContext>,
    ) -> Self {
        if let (Some(snapshot), Some(pending)) = (accounting, pending) {
            return match pending.projected_accounting(snapshot) {
                Ok((row_origins, row_cache)) => Self {
                    row_origins,
                    row_cache,
                    ..Self::default()
                },
                Err(error) => Self {
                    invalid: true,
                    pending_rejection: Some((error.kind(), error.message().to_owned())),
                    ..Self::default()
                },
            };
        }
        accounting.map_or_else(Self::default, |snapshot| Self {
            row_origins: snapshot.row_origins.clone(),
            row_cache: snapshot.row_records.clone(),
            ..Self::default()
        })
    }

    /// Returns row records needing publication or retaining an existing cache.
    /// The storage owner separately filters committed versus pending membership.
    pub(in crate::runtime) fn rendered_row_records(
        &self,
    ) -> Result<Vec<(u64, ContextBlockRecord)>> {
        self.ensure_valid()?;
        Ok(self
            .rendered_rows
            .iter()
            .map(|(sequence, record)| (*sequence, *record))
            .collect())
    }

    /// Returns only actual committed repairs to the original leased publisher;
    /// proposed pending IDs never become durable cache authority prematurely.
    pub(in crate::runtime) fn committed_row_records(
        &self,
        snapshot: &crate::storage::transcript::ConversationReplaySnapshot,
    ) -> Result<Vec<(u64, ContextBlockRecord)>> {
        let committed = snapshot
            .view
            .committed
            .iter()
            .map(|row| row.sequence)
            .collect::<std::collections::BTreeSet<_>>();
        Ok(self
            .rendered_row_records()?
            .into_iter()
            .filter(|(sequence, _)| committed.contains(sequence))
            .collect())
    }

    /// Original checked placement used by this actual canonical render.
    pub(super) fn source_row_origins(&self) -> &crate::storage::transcript::TranscriptRowOrigins {
        &self.row_origins
    }

    /// Descriptive source caches selected before constructing this projection.
    pub(super) fn source_row_records(&self) -> &crate::storage::transcript::TranscriptRowRecords {
        &self.row_cache
    }

    /// Counts actual missing/stale row estimate constructions in one renderer.
    /// Test-only state does not affect production caching or source authority.
    #[cfg(test)]
    pub(in crate::runtime) fn estimate_recomputations(&self) -> usize {
        self.estimate_recomputations
    }

    /// Associates an actual summary record with its exact emitted block slot.
    pub(super) fn push_indexed_record(&mut self, index: usize, record: ContextBlockRecord) {
        let position = self
            .records
            .partition_point(|(existing, _)| *existing < index);
        if self
            .records
            .get(position)
            .is_some_and(|(existing, _)| *existing == index)
        {
            self.invalid = true;
        } else {
            self.records.insert(position, (index, record));
        }
    }

    /// Withholds adoption when a required typed summary slot is absent.
    pub(super) fn mark_invalid(&mut self) {
        self.invalid = true;
    }

    /// Captures malformed reserved envelopes as an explicit pending failure.
    /// Unrelated/legacy rows remain readable and supply no authored cost claim.
    pub(super) fn decode_row(&mut self, entry: &TranscriptEntry) -> DecodedContextRow {
        if entry.role != mez_agent::transcript::TranscriptRole::System {
            return DecodedContextRow {
                event: None,
                record: None,
                stable: None,
            };
        }
        match TranscriptContextRecord::from_transcript_content(&entry.content) {
            Ok(Some(record)) => {
                let (event, record, stable) = record.into_parts();
                DecodedContextRow {
                    event: Some(event),
                    record: Some(record),
                    stable,
                }
            }
            Ok(None) => DecodedContextRow {
                event: TranscriptContextEvent::from_transcript_content(&entry.content),
                record: None,
                stable: None,
            },
            Err(_) => {
                self.invalid = true;
                DecodedContextRow {
                    event: None,
                    record: None,
                    stable: None,
                }
            }
        }
    }

    /// Adds one exact structural source. Legacy position is the durable archive
    /// row, never the newly allocated local context sequence or current pane.
    pub(super) fn push_block(
        &mut self,
        blocks: &mut Vec<ContextBlock>,
        block: ContextBlock,
        entry: &TranscriptEntry,
        record: Option<ContextBlockRecord>,
        stable: Option<&[StableContextBlockRecord]>,
    ) -> Option<ContextBlockRecord> {
        let record = self.restore_row_record(&block, entry, record);
        if let Some(stable) = stable {
            self.stable = Some(stable.to_vec());
        }
        let record = match record {
            Ok(record) => {
                self.records.push((blocks.len(), record));
                Some(record)
            }
            Err(_) => {
                self.invalid = true;
                None
            }
        };
        blocks.push(block);
        record
    }

    /// Validates source identity before consulting descriptive cached costs.
    /// A valid raw authored record needs no duplicate cache; a repaired one is
    /// retained by the leased publisher without rewriting its original bytes.
    fn restore_row_record(
        &mut self,
        block: &ContextBlock,
        entry: &TranscriptEntry,
        authored: Option<ContextBlockRecord>,
    ) -> mez_agent::AgentContextResult<ContextBlockRecord> {
        let placement = self.row_origins.get(&entry.sequence).copied();
        let origin = if let Some(record) = authored {
            record.validate_format()?;
            record.occurrence_id()
        } else if let Some(origin) = placement {
            origin
        } else {
            ContextBlockOccurrenceId::for_legacy_archive_row(
                &entry.conversation_id,
                entry.sequence,
                0,
            )?
        };
        if placement.is_some_and(|placed| placed != origin) {
            return Err(mez_agent::AgentContextError::new(
                "authored row placement origin mismatch",
            ));
        }
        let cached = self.row_cache.get(&entry.sequence).copied();
        let prior = cached.or(authored);
        let restored = match prior {
            Some(record) if record.occurrence_id() == origin => record.restored(block)?,
            Some(_) => {
                return Err(mez_agent::AgentContextError::new(
                    "transcript row cache origin mismatch",
                ));
            }
            None => ContextBlockRecord::for_occurrence(origin, block)?,
        };
        #[cfg(test)]
        if Some(restored) != prior {
            self.estimate_recomputations += 1;
        }
        if authored.is_none() || cached.is_some() || Some(restored) != authored {
            self.rendered_rows.insert(entry.sequence, restored);
        }
        Ok(restored)
    }

    /// Shifts history indices when a real memory/head prefix is prepended.
    /// Overflow withholds later adoption instead of wrapping an association.
    pub(super) fn prepend_blocks(&mut self, count: usize) {
        for (index, _) in &mut self.records {
            match index.checked_add(count) {
                Some(value) => *index = value,
                None => self.invalid = true,
            }
        }
    }

    /// Returns records for an actual final flat block projection. All source
    /// history is chronological; stable-prefix authority is assembled separately.
    pub(in crate::runtime) fn indices_with_offset(
        &self,
        offset: usize,
    ) -> Result<Vec<(usize, ContextBlockRecord)>> {
        self.ensure_valid()?;
        self.records
            .iter()
            .map(|(index, record)| {
                index
                    .checked_add(offset)
                    .map(|index| (index, *record))
                    .ok_or_else(invalid)
            })
            .collect()
    }

    /// Constructs a real provider context without recounting valid history
    /// records before their adoption. Actual authority is still supplied by
    /// the freshly composed stable prefix, never by saved slot text.
    pub(in crate::runtime) fn import_context(
        &self,
        blocks: Vec<ContextBlock>,
    ) -> Result<AgentContext> {
        let prefix = blocks
            .iter()
            .take_while(|block| block.placement == mez_agent::ContextPlacement::StablePrefix)
            .count();
        let records = self.indices_with_offset(prefix)?;
        let mut context = AgentContext::import_durable_blocks_with_records(blocks, &records)?;
        if let Some(stable) = &self.stable {
            context.restore_stable_block_records(stable)?;
        }
        Ok(context)
    }

    /// Rejects malformed/future reserved records before a source is published.
    pub(in crate::runtime) fn ensure_valid(&self) -> Result<()> {
        if let Some((kind, message)) = &self.pending_rejection {
            return Err(MezError::new(
                *kind,
                format!("pending transcript accounting: {message}"),
            ));
        }
        if self.invalid {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Emits no source, identity, paths or authored metadata in diagnostics.
fn invalid() -> MezError {
    MezError::invalid_state("invalid accounted context history projection")
}

#[cfg(test)]
mod tests;
