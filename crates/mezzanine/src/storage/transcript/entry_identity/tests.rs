//! Occurrence deduplication must ignore descriptive caches and local placement
//! without merging equal text from independent canonical commits.

use super::*;
use mez_agent::{AgentContext, ContextBlock, ContextBlockRecord};

fn row(content: String) -> TranscriptEntry {
    TranscriptEntry {
        conversation_id: "identity-conversation".to_owned(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::System,
        turn_id: "original-turn".to_owned(),
        agent_id: "agent".to_owned(),
        pane_id: "pane".to_owned(),
        content,
    }
}

fn user(sequence: u64, text: &str, record: ContextBlockRecord) -> TranscriptEntry {
    row(TranscriptContextRecord::new(
        TranscriptContextEvent::user_event(sequence, "user", text).unwrap(),
        record,
        None,
    )
    .unwrap()
    .to_transcript_content()
    .unwrap())
}

/// Accounting repair and local sequence reassignment cannot create a second
/// copy of one occurrence. A genuine content revision still changes the row
/// identity used for append deduplication, and equal independent events differ.
#[test]
fn accounted_identity_preserves_occurrence_and_semantic_revision() {
    let block = ContextBlock::user_event("user", "equal é😀 source");
    let record = ContextBlockRecord::allocate(&block).unwrap();
    let original = user(7, &block.content, record);
    let mut recounted = original.clone();
    let (marker, payload) = original.content.split_once('\n').unwrap();
    let mut payload: serde_json::Value = serde_json::from_str(payload).unwrap();
    payload["record"]["framed_input_tokens"] = 999.into();
    recounted.content = format!("{marker}\n{payload}");
    let identity = canonical_transcript_entry_identity(&original);
    assert_eq!(identity, canonical_transcript_entry_identity(&recounted));
    let mut moved = user(900, &block.content, record);
    moved.sequence = 9000;
    moved.created_at_unix_seconds = 9000;
    moved.pane_id = "different-pane".to_owned();
    assert_eq!(identity, canonical_transcript_entry_identity(&moved));
    let changed = ContextBlock::user_event("user", "revised source");
    assert_ne!(
        identity,
        canonical_transcript_entry_identity(&user(
            7,
            &changed.content,
            record.revised(&changed).unwrap()
        ))
    );
    assert_ne!(
        identity,
        canonical_transcript_entry_identity(&user(
            7,
            &block.content,
            ContextBlockRecord::allocate(&block).unwrap()
        ))
    );
    let mut literal = original.clone();
    literal.role = TranscriptRole::User;
    assert_eq!(
        canonical_transcript_entry_identity(&literal).2,
        literal.content
    );
}

/// Stable snapshots are descriptive state captured at a user boundary. A later
/// snapshot must not make the same canonical prompt appear newly appendable.
#[test]
fn stable_snapshot_changes_do_not_duplicate_canonical_prompt() {
    let mut context = AgentContext::empty();
    context.append_user_event("user", "source").unwrap();
    let event = &context.chronology()[0];
    let semantic = TranscriptContextEvent::user_event(1, "user", "source").unwrap();
    let without = row(
        TranscriptContextRecord::new(semantic.clone(), event.record(), None)
            .unwrap()
            .to_transcript_content()
            .unwrap(),
    );
    let with = row(
        TranscriptContextRecord::new(semantic, event.record(), Some(vec![]))
            .unwrap()
            .to_transcript_content()
            .unwrap(),
    );
    assert_eq!(
        canonical_transcript_entry_identity(&without),
        canonical_transcript_entry_identity(&with)
    );
}

/// Accounting framing is reserved for system rows. A user, assistant or tool
/// may quote that exact spelling; its owner and literal bytes must remain part
/// of ordinary row identity rather than acquiring native-reference semantics.
#[test]
fn marker_shaped_non_system_content_keeps_literal_identity() {
    let block = ContextBlock::reference_event(ContextSourceKind::PeerMessage, "peer", "source");
    let content = TranscriptContextRecord::new(
        TranscriptContextEvent::prompt_boundary_with_event_sequence(
            ContextSourceKind::PeerMessage,
            1,
            "peer",
            "source",
        )
        .unwrap(),
        ContextBlockRecord::allocate(&block).unwrap(),
        None,
    )
    .unwrap()
    .to_transcript_content()
    .unwrap();
    for role in [
        TranscriptRole::User,
        TranscriptRole::Assistant,
        TranscriptRole::Tool,
    ] {
        let mut literal = row(content.clone());
        literal.role = role;
        let identity = canonical_transcript_entry_identity(&literal);
        assert_eq!(identity.0, literal.turn_id);
        assert_eq!(identity.2, content);
    }
}
