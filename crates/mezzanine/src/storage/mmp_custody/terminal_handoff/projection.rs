//! Immutable original application chronology, without claimed storage authority.
//!
//! The root adds actual source/allocation provenance on its owned worker. Live
//! projection requires an original live anchor; historical lookup may finish a
//! stored image but cannot manufacture a new context or task from this metadata.

use super::*;
use mez_agent::transcript::{
    TranscriptContextRecord, TranscriptTerminalHandoffEvent, TranscriptTerminalHandoffSource,
    terminal_outcome_reference,
};
use mez_agent::{ContextBlock, ContextBlockRecord, ContextSourceKind};

/// Original neutral fact and preceding chronology captured before context commit.
#[derive(Debug)]
pub(crate) struct TerminalHandoffProjection {
    row: TranscriptEntry,
    event_sequence: u64,
    label: String,
    record: ContextBlockRecord,
    prefix: Vec<TranscriptEntry>,
}
impl TerminalHandoffProjection {
    /// Checks bounded application attribution and chronology before retention.
    pub(crate) fn new(
        row: TranscriptEntry,
        event_sequence: u64,
        label: String,
        prefix: Vec<TranscriptEntry>,
        record: ContextBlockRecord,
    ) -> Result<Self> {
        row.validate()?;
        if row.role != TranscriptRole::System
            || event_sequence == 0
            || row.conversation_id.len() > 256
            || row.agent_id.len() > 256
            || label.len() > 4096
            || row.content.len() > 512 * 1024
            || [
                &row.conversation_id,
                &row.turn_id,
                &row.agent_id,
                &row.pane_id,
            ]
            .iter()
            .map(|s| s.len())
            .sum::<usize>()
                > 16 * 1024
        {
            return Err(invalid());
        }
        for before in &prefix {
            before.validate()?;
            if before.conversation_id != row.conversation_id
                || before.agent_id != row.agent_id
                || before.pane_id != row.pane_id
            {
                return Err(invalid());
            }
        }
        crate::storage::transcript::custody::validate_custody_append_prefix(&prefix)?;
        let record = record.restored(&ContextBlock::reference_event(
            ContextSourceKind::RuntimeHint,
            label.clone(),
            row.content.clone(),
        ))?;
        Ok(Self {
            record,
            row,
            event_sequence,
            label,
            prefix,
        })
    }
    /// Adds only actual original native sender provenance and fixed semantics.
    pub(super) fn materialize(
        &self,
        source: &MessageTerminalHandoffAnchor,
        store_id: [u8; 16],
    ) -> Result<(Vec<TranscriptEntry>, TranscriptEntry)> {
        let (label, content) =
            terminal_outcome_reference(&source.outcome()).map_err(|_| invalid())?;
        if label != self.label || content != self.row.content {
            return Err(invalid());
        }
        let event = TranscriptTerminalHandoffEvent::new(
            TranscriptTerminalHandoffSource::from_anchor(source, store_id)
                .map_err(|_| invalid())?,
            self.event_sequence,
        )
        .map_err(|_| invalid())?;
        let mut row = self.row.clone();
        row.content = TranscriptContextRecord::new(
            TranscriptContextEvent::TerminalHandoff {
                event: Box::new(event),
            },
            self.record,
            None,
        )?
        .to_transcript_content()?;
        checked_row(&row, source, store_id)?;
        Ok((self.prefix.clone(), row))
    }
}
