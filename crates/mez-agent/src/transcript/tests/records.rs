//! Canonical transcript entry tests.

use crate::transcript::{TranscriptEntry, TranscriptRole, validate_conversation_id};

fn valid_entry() -> TranscriptEntry {
    TranscriptEntry {
        conversation_id: "conversation-1".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::Assistant,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-1".to_string(),
        pane_id: "pane-1".to_string(),
        content: "done".to_string(),
    }
}

/// Verifies a complete one-based transcript record is accepted.
///
/// Product persistence adapters may rely on canonical validation before
/// applying their own filesystem and compatibility encoding rules.
#[test]
fn transcript_entry_validation_accepts_complete_records() {
    valid_entry().validate().unwrap();
}

/// Verifies malformed transcript identity and required fields are rejected.
///
/// Zero sequence metadata, empty content, and path-like conversation ids must
/// fail at the dependency-neutral agent boundary.
#[test]
fn transcript_entry_validation_rejects_invalid_records() {
    let mut entry = valid_entry();
    entry.sequence = 0;
    assert!(entry.validate().is_err());

    entry.sequence = 1;
    entry.content.clear();
    assert!(entry.validate().is_err());

    entry.content = "done".to_string();
    entry.conversation_id = "../conversation".to_string();
    assert!(entry.validate().is_err());
}

/// Verifies product stores can use the canonical conversation-ID grammar.
///
/// Direct validation keeps filesystem and compatibility adapters from copying
/// the byte predicate owned by the provider-independent transcript contract.
#[test]
fn conversation_id_validation_is_public_and_canonical() {
    validate_conversation_id("conversation_A-19").unwrap();
    assert!(validate_conversation_id("").is_err());
    assert!(validate_conversation_id("../conversation").is_err());
    assert!(validate_conversation_id("conversation space").is_err());
}
