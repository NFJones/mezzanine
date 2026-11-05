//! Source-independent projection captured from actual canonical receive state.
//!
//! The application supplies its real chronology event and original row owner.
//! Only the original-root worker adds transport provenance from the actual
//! source anchor and allocation. No caller-supplied store UUID, native tuple or
//! imported transcript frame can manufacture this storage attribution.

use super::*;
use mez_agent::transcript::{TranscriptContextRecord, TranscriptNativeInputEvent};
use mez_agent::{ContextBlock, ContextBlockRecord, ContextSourceKind};

/// Immutable application event, without a claimed transport storage identity.
#[derive(Debug, Clone)]
pub(crate) struct NativeInputProjection {
    row: TranscriptEntry,
    event_sequence: u64,
    label: String,
    record: ContextBlockRecord,
    /// Actual preceding canonical chronology, captured with this application input.
    prefix: Vec<TranscriptEntry>,
}
impl NativeInputProjection {
    /// Returns the original chronology occurrence retained by this receive owner.
    pub(crate) fn event_sequence(&self) -> u64 {
        self.event_sequence
    }

    /// Reconstructs this exact worker-owned row for in-memory position adoption
    /// after the actual joined original-root proof, without filesystem I/O.
    pub(crate) fn committed_row(
        &self,
        proof: &MessageNativeInputCommit,
    ) -> Result<TranscriptEntry> {
        let mut row = self.materialize(proof.anchor(), proof.record().store_id)?;
        row.sequence = proof.record().archive_sequence;
        Ok(row)
    }

    /// Captures an actual peer reference event. `row.content` is its plain
    /// canonical content; the worker constructs the typed archive framing.
    pub(crate) fn new(
        row: TranscriptEntry,
        event_sequence: u64,
        label: String,
        record: ContextBlockRecord,
    ) -> Result<Self> {
        row.validate()?;
        if row.conversation_id.len() > 256
            || row.agent_id.len() > 256
            || row.agent_id.chars().any(char::is_control)
            || row.role != TranscriptRole::System
            || event_sequence == 0
            || label.trim().is_empty()
            || label.len() > 4096
            || label.contains('\0')
            || row.content.trim().is_empty()
            || row.content.len() > 2 * 1024 * 1024
            || [
                &row.conversation_id,
                &row.turn_id,
                &row.agent_id,
                &row.pane_id,
            ]
            .iter()
            .map(|value| value.len())
            .sum::<usize>()
                > 16 * 1024
        {
            return Err(invalid());
        }
        let record = record.restored(&ContextBlock::reference_event(
            ContextSourceKind::PeerMessage,
            label.clone(),
            row.content.clone(),
        ))?;
        Ok(Self {
            record,
            row,
            event_sequence,
            label,
            prefix: Vec::new(),
        })
    }

    /// Attaches preceding actual canonical events before the peer row. This is
    /// an immutable projection candidate, not canonical storage/ACK evidence.
    pub(crate) fn with_prefix(mut self, prefix: Vec<TranscriptEntry>) -> Result<Self> {
        for row in &prefix {
            row.validate()?;
            if row.conversation_id != self.row.conversation_id
                || row.agent_id != self.row.agent_id
                || row.pane_id != self.row.pane_id
            {
                return Err(invalid());
            }
        }
        crate::storage::transcript::custody::validate_custody_append_prefix(&prefix)?;
        self.prefix = prefix;
        Ok(self)
    }

    /// Checks the exact event that was prepared before the live context changed.
    /// Matching descriptive data alone does not create application ownership.
    pub(crate) fn matches_event(
        &self,
        sequence: u64,
        label: &str,
        content: &str,
        record: ContextBlockRecord,
    ) -> bool {
        self.event_sequence == sequence
            && self.label == label
            && self.row.content == content
            && self.record.occurrence_id() == record.occurrence_id()
    }

    /// Borrows actual preceding chronology without another runtime lookup.
    pub(super) fn prefix(&self) -> &[TranscriptEntry] {
        &self.prefix
    }

    /// Adds only actual original-root/source provenance on the owned worker.
    pub(super) fn materialize(
        &self,
        anchor: &MessageNativeInputAnchor,
        store_id: [u8; 16],
    ) -> Result<TranscriptEntry> {
        let source =
            TranscriptNativeInputSource::from_anchor(anchor, store_id).map_err(|_| invalid())?;
        let event = TranscriptNativeInputEvent::new(
            source,
            self.event_sequence,
            self.label.clone(),
            self.row.content.clone(),
        )
        .map_err(|_| invalid())?;
        let mut row = self.row.clone();
        row.content = TranscriptContextRecord::new(
            TranscriptContextEvent::NativePeerMessage {
                event: Box::new(event),
            },
            self.record,
            None,
        )?
        .to_transcript_content()?;
        checked_event(&row, anchor, store_id)?;
        Ok(row)
    }
}
