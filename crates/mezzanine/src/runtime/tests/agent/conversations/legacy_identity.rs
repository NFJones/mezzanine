//! Real legacy raw replay preserves occurrence through storage mutations.
//!
//! Equal bodies deliberately belong to separate rows. Store, prompt worker and
//! cold replay must preserve their identities across changed CID and ordering.

use super::*;

/// Creates equal legacy rows before the actual prompt-history read.
fn fixture() -> (RuntimeSessionService, AgentTranscriptStore, String) {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("legacy-raw-identities"));
    service.set_agent_transcript_store(store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation = "legacy-origin-source".to_string();
    for sequence in 1..=3 {
        store
            .append(&TranscriptEntry {
                conversation_id: conversation.clone(),
                sequence,
                created_at_unix_seconds: 1,
                role: TranscriptRole::Assistant,
                turn_id: "original-turn".into(),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: "equal legacy body 雪 {\"code\":[1,2,3]}".into(),
            })
            .unwrap();
    }
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", &conversation, 3)
        .unwrap();
    (service, store, conversation)
}

/// Selects actual emitted assistant record slots, excluding prompt/guidance.
fn records(service: &mut RuntimeSessionService) -> Vec<mez_agent::ContextBlockRecord> {
    service
        .agent_context_for_pane_prompt("%1", "inspect source", 100)
        .unwrap()
        .chronology()
        .iter()
        .filter(|event| event.block().source == ContextSourceKind::TranscriptAssistant)
        .map(|event| event.record())
        .collect()
}

/// Fork and cold replay must preserve all distinct original records without
/// rewriting source raw bytes or assigning child-CID-derived identities.
#[test]
fn legacy_raw_fork_preserves_original_record_identities() {
    let (mut service, store, conversation) = fixture();
    let mut typed = store.inspect(&conversation).unwrap()[0].clone();
    typed.sequence = 4;
    typed.turn_id = "separate-typed-turn".into();
    typed.role = TranscriptRole::System;
    typed.content = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
        ContextSourceKind::TranscriptAssistant,
        "legacy assistant",
        "legacy typed execution",
        mez_agent::ContextExecutionGroupId::new("original-group").unwrap(),
        1,
        None,
    )
    .unwrap()
    .to_transcript_content();
    store.append(&typed).unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", &conversation, 4)
        .unwrap();
    let raw = std::fs::read(store.transcript_path(&conversation).unwrap()).unwrap();
    let original = records(&mut service);
    assert_eq!(original.len(), 4);
    assert_ne!(original[0].occurrence_id(), original[1].occurrence_id());
    assert_eq!(
        std::fs::read(store.transcript_path(&conversation).unwrap()).unwrap(),
        raw
    );
    store.fork(&conversation, "legacy-origin-child", 7).unwrap();
    service.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "legacy-origin-child", 4)
        .unwrap();
    assert_eq!(records(&mut service), original);
}

/// A real content CAS changes only that row's rendered cost, retaining its
/// original occurrence and the exact untouched duplicate records after reopen.
#[test]
fn legacy_raw_content_edit_preserves_origin_and_revises_only_affected_record() {
    let (mut service, store, conversation) = fixture();
    let original = records(&mut service);
    let row = store.inspect(&conversation).unwrap()[0].clone();
    let revision = store.transcript_entry_revision(&row).unwrap();
    store
        .compare_and_swap_entry_content(
            &conversation,
            1,
            "%1",
            &revision,
            "changed legacy body 雪 {\"large\":[1,2,3,4,5,6,7,8,9]}".into(),
        )
        .unwrap();
    service.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    let changed = records(&mut service);
    assert_eq!(changed[0].occurrence_id(), original[0].occurrence_id());
    assert_ne!(changed[0], original[0]);
    assert_eq!(&changed[1..], &original[1..]);
}

/// Deleting the first equal row changes position, not surviving occurrence.
/// A subsequent append cannot reuse an origin shifted by that deletion.
#[test]
fn legacy_raw_delete_resequence_and_append_preserve_original_occurrences() {
    let (mut service, store, conversation) = fixture();
    let original = records(&mut service);
    assert!(store.delete_entry(&conversation, 1).unwrap());
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", &conversation, 2)
        .unwrap();
    assert_eq!(records(&mut service), original[1..]);
    let mut row = store.inspect(&conversation).unwrap()[1].clone();
    row.sequence = 3;
    store.append(&row).unwrap();
    service.set_agent_transcript_store(AgentTranscriptStore::new(store.root()));
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", &conversation, 3)
        .unwrap();
    let after = records(&mut service);
    assert_eq!(&after[..2], &original[1..]);
    assert!(
        !original
            .iter()
            .any(|record| record.occurrence_id() == after[2].occurrence_id())
    );
}
