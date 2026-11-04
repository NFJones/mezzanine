//! Canonical row data identity shared by native append and execution bookkeeping.
//!
//! Sequence and timestamp describe physical placement. Identity instead preserves
//! the actual chronology owner and exact content, normalizing native and ordinary
//! peer references to the same event. This helper confers no input/ACK authority.

use mez_agent::ContextSourceKind;
use mez_agent::transcript::{TranscriptContextEvent, TranscriptEntry, TranscriptRole};

/// Returns the durable identity of one transcript row without volatile
/// sequence or timestamp fields.
pub(crate) fn canonical_transcript_entry_identity(entry: &TranscriptEntry) -> (String, u8, String) {
    let role = match entry.role {
        TranscriptRole::User => 0,
        TranscriptRole::Assistant => 1,
        TranscriptRole::Tool => 2,
        TranscriptRole::System => 3,
    };
    let event = TranscriptContextEvent::from_transcript_content(&entry.content);
    let owner = if matches!(
        &event,
        Some(TranscriptContextEvent::McpCatalogSnapshot { .. })
    ) {
        String::new()
    } else {
        entry.turn_id.clone()
    };
    // The native commit row and later ordinary execution projection own
    // the same canonical chronology event. Hash borrowed fields directly:
    // normalization must not inherit the smaller prompt-boundary codec cap.
    let peer_identity = |sequence: u64, label: &str, content: &str| {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(b"mez-canonical-peer-identity-v1\0");
        hash.update(sequence.to_le_bytes());
        hash.update((label.len() as u64).to_le_bytes());
        hash.update(label.as_bytes());
        hash.update((content.len() as u64).to_le_bytes());
        hash.update(content.as_bytes());
        use std::fmt::Write;
        let mut key = String::from("[mez-canonical-peer-identity/v1]");
        for byte in hash.finalize() {
            let _ = write!(key, "{byte:02x}");
        }
        key
    };
    let content = match event {
        Some(TranscriptContextEvent::NativePeerMessage { event }) => {
            peer_identity(event.event_sequence, &event.label, &event.content)
        }
        Some(TranscriptContextEvent::PromptBoundary {
            source: ContextSourceKind::PeerMessage,
            event_sequence: Some(sequence),
            label,
            content,
            ..
        }) => peer_identity(sequence, &label, &content),
        _ => entry.content.clone(),
    };
    (owner, role, content)
}
