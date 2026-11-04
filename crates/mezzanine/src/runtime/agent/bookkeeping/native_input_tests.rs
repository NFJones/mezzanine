//! Canonical native input and later execution projection identity.

use super::*;
use mez_agent::transcript::{TranscriptNativeInputEvent, TranscriptNativeInputSource};

/// Descriptive canonical archive data; no native acceptance/ACK is claimed.
fn row(content: &str) -> TranscriptEntry {
    let source = TranscriptNativeInputSource {
        version: 1,
        store_id: [1; 16],
        source_sha256: [2; 32],
        message_sequence: 1,
        binding_index: 0,
        recipient: "receiver".into(),
        registered_after_sequence: 0,
        native_owner: serde_json::from_value(
            serde_json::json!({"conversation_id":"native", "incarnation":"1".repeat(64)}),
        )
        .unwrap(),
        offered_at_ms: 10,
    };
    let event =
        TranscriptNativeInputEvent::new(source, 10, "actual peer event".into(), content.into())
            .unwrap();
    TranscriptEntry {
        conversation_id: "native".into(),
        sequence: 1,
        created_at_unix_seconds: 10,
        role: TranscriptRole::System,
        turn_id: "turn-original".into(),
        agent_id: "receiver".into(),
        pane_id: "pane".into(),
        content: TranscriptContextEvent::NativePeerMessage {
            event: Box::new(event),
        }
        .to_transcript_content(),
    }
}

/// Later provider bookkeeping must recognize the native canonical input as the
/// same chronology event even though the ordinary projection lacks provenance.
/// A second accepted event with equal text and a new chronology ID stays unique.
#[test]
fn native_input_identity_matches_later_peer_projection_without_duplicating_input() {
    let native = row("actual untrusted input");
    let mut ordinary = native.clone();
    ordinary.sequence = 99;
    ordinary.created_at_unix_seconds = 99;
    ordinary.content = TranscriptContextEvent::prompt_boundary_with_event_sequence(
        ContextSourceKind::PeerMessage,
        10,
        "actual peer event",
        "actual untrusted input",
    )
    .unwrap()
    .to_transcript_content();
    assert_eq!(
        RuntimeSessionService::runtime_transcript_entry_identity(&native),
        RuntimeSessionService::runtime_transcript_entry_identity(&ordinary)
    );
    ordinary.content = TranscriptContextEvent::prompt_boundary_with_event_sequence(
        ContextSourceKind::PeerMessage,
        11,
        "actual peer event",
        "actual untrusted input",
    )
    .unwrap()
    .to_transcript_content();
    assert_ne!(
        RuntimeSessionService::runtime_transcript_entry_identity(&native),
        RuntimeSessionService::runtime_transcript_entry_identity(&ordinary)
    );
}

/// Identity normalization hashes the complete native content directly rather
/// than falling back to a different identity above the legacy codec's 256 KiB
/// cap. Exact large peer data remains distinguishable without truncation.
#[test]
fn native_input_identity_keeps_complete_large_peer_content() {
    let first = row(&"x".repeat(300 * 1024));
    let second = row(&format!("{}y", "x".repeat(300 * 1024)));
    let key = RuntimeSessionService::runtime_transcript_entry_identity(&first);
    assert!(key.2.starts_with("[mez-canonical-peer-identity/v1]"));
    assert_ne!(
        key,
        RuntimeSessionService::runtime_transcript_entry_identity(&second)
    );
}
