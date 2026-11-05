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
}

/// One semantic row decoded exactly once before execution-group validation.
pub(super) struct DecodedContextRow {
    pub(super) event: Option<TranscriptContextEvent>,
    pub(super) record: Option<ContextBlockRecord>,
    pub(super) stable: Option<Vec<StableContextBlockRecord>>,
}

impl RuntimeContextRecords {
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
        let record = match record {
            Some(record) => Ok(record),
            None => ContextBlockOccurrenceId::for_legacy_archive_row(
                &entry.conversation_id,
                entry.sequence,
                0,
            )
            .and_then(|origin| ContextBlockRecord::for_occurrence(origin, &block)),
        };
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
