//! Source-independent projection captured from actual canonical receive state.
//!
//! The application supplies its real chronology event and original row owner.
//! Only the original-root worker adds transport provenance from the actual
//! source anchor and allocation. No caller-supplied store UUID, native tuple or
//! imported transcript frame can manufacture this storage attribution.

use super::*;
use mez_agent::transcript::TranscriptNativeInputEvent;

/// Immutable application event, without a claimed transport storage identity.
#[derive(Debug, Clone)]
pub(crate) struct NativeInputProjection {
    row: TranscriptEntry,
    event_sequence: u64,
    label: String,
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
    pub(crate) fn new(row: TranscriptEntry, event_sequence: u64, label: String) -> Result<Self> {
        row.validate()?;
        if row.role != TranscriptRole::System
            || event_sequence == 0
            || label.trim().is_empty()
            || label.len() > 4096
            || label.contains('\0')
            || row.content.trim().is_empty()
            || row.content.len() > 2 * 1024 * 1024
        {
            return Err(invalid());
        }
        Ok(Self {
            row,
            event_sequence,
            label,
        })
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
        row.content = TranscriptContextEvent::NativePeerMessage {
            event: Box::new(event),
        }
        .to_transcript_content();
        checked_event(&row, anchor, store_id)?;
        Ok(row)
    }
}
