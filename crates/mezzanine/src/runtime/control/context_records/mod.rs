//! Structural accounting transport from one actual transcript projection.
//!
//! Indices belong to the exact emitted block order, not a label/text search.
//! Legacy rows establish an explicit archive-position import origin; accounted
//! records preserve their committed origin and cached estimates. Neither path
//! changes source roles or restores instructions. Malformed reserved records
//! retain an error through preparation so partial source cannot reach a model.

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
    row_origins: crate::storage::transcript::TranscriptRowOrigins,
    row_cache: crate::storage::transcript::TranscriptRowRecords,
    rendered_rows: crate::storage::transcript::TranscriptRowRecords,
    #[cfg(test)]
    legacy_recounts: usize,
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
    ) -> Self {
        accounting.map_or_else(Self::default, |snapshot| Self {
            row_origins: snapshot.row_origins.clone(),
            row_cache: snapshot.row_records.clone(),
            ..Self::default()
        })
    }

    /// Returns exact emitted legacy-row records after full projection validation.
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

    /// Counts actual missing/stale legacy estimate constructions in one renderer.
    /// Test-only state does not affect production caching or source authority.
    #[cfg(test)]
    pub(in crate::runtime) fn legacy_recounts(&self) -> usize {
        self.legacy_recounts
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
        let authored = record.is_some();
        let record = match record {
            Some(record) => record.restored(&block),
            None => {
                let origin = match self.row_origins.get(&entry.sequence) {
                    Some(origin) => Ok(*origin),
                    None => ContextBlockOccurrenceId::for_legacy_archive_row(
                        &entry.conversation_id,
                        entry.sequence,
                        0,
                    ),
                };
                origin.and_then(|origin| {
                    let cached = self.row_cache.get(&entry.sequence).copied();
                    let restored = match cached {
                        Some(record) if record.occurrence_id() == origin => record.restored(&block),
                        Some(_) => {
                            return Err(mez_agent::AgentContextError::new(
                                "legacy row cache origin mismatch",
                            ));
                        }
                        None => ContextBlockRecord::for_occurrence(origin, &block),
                    };
                    #[cfg(test)]
                    if restored
                        .as_ref()
                        .is_ok_and(|record| Some(*record) != cached)
                    {
                        self.legacy_recounts += 1;
                    }
                    restored
                })
            }
        };
        if let Some(stable) = stable {
            self.stable = Some(stable.to_vec());
        }
        let record = match record {
            Ok(record) => {
                if !authored {
                    self.rendered_rows.insert(entry.sequence, record);
                }
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
