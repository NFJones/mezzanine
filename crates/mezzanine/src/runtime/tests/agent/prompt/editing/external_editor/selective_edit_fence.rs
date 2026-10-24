//! Real editor draft capture and durable recovery SAVE across epoch publication.
//!
//! These tests use the existing native editor fixture and production target CAS;
//! they neither dispatch providers nor repeat actions to repair protected edits.

use super::*;

/// Builds a complete one-row typed group, preserving its identity when edited.
fn typed_content(body: &str) -> String {
    mez_agent::TranscriptContextEvent::execution_block_with_metadata(
        mez_agent::ContextSourceKind::TranscriptAssistant,
        "assistant",
        body,
        mez_agent::ContextExecutionGroupId::new("editor-selective-group").unwrap(),
        1,
        None,
    )
    .unwrap()
    .to_transcript_content()
}

/// Reads one private draft so SAVE rejection and reopening can prove its exact
/// bytes survive, rather than only checking that a recovery id remains listed.
fn draft_path(root: &Path, session_id: &str) -> PathBuf {
    root.join("runtime/editor-sessions")
        .join(session_id)
        .join("draft.md")
}

/// Compaction can publish after the editor captures a revision without changing
/// that revision. Explicit SAVE must nevertheless reject the now-covered row,
/// preserve the draft and current prompt, and expose the existing diagnostic.
/// An uncovered row still saves normally and reopens with the edit exactly once.
#[test]
fn runtime_transcript_editor_save_rechecks_selective_epoch_published_after_open() {
    let (mut service, primary, root) = prompt_editor_fixture("selective-epoch-editor-save");
    let store_root = root.join("agent-sessions");
    let store = AgentTranscriptStore::new(store_root.clone());
    let conversation_id = "selective-editor";
    let original = TranscriptEntry {
        conversation_id: conversation_id.to_string(),
        sequence: 1,
        created_at_unix_seconds: 10,
        role: TranscriptRole::System,
        turn_id: "turn-1".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        content: typed_content("original body"),
    };
    let mut uncovered = original.clone();
    uncovered.sequence = 2;
    uncovered.role = TranscriptRole::User;
    uncovered.content = "uncovered before".to_string();
    store
        .append_many(&[original.clone(), uncovered.clone()])
        .unwrap();
    service.set_agent_transcript_store(store.clone());
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"current prompt remains")
        .unwrap();
    service
        .start_transcript_entry_external_edit(&primary, "%1", conversation_id, 1)
        .unwrap();
    let protected = editor_transaction(&service);
    assert_eq!(
        fs::read_to_string(draft_path(&root, &protected.2)).unwrap(),
        original.content
    );

    store
        .save_compaction_ranges_with_proof(
            crate::storage::transcript::AgentCompactionEpoch {
                version: 2,
                conversation_id: conversation_id.to_string(),
                through_sequence: 0,
                summary: String::new(),
                ranges: vec![crate::storage::transcript::AgentCompactionRange {
                    first_sequence: 1,
                    through_sequence: 1,
                    summary: "authoritative group summary".to_string(),
                }],
            },
            std::slice::from_ref(&original),
        )
        .unwrap();
    let epoch = store.compaction_epoch(conversation_id).unwrap();
    let history_path = store_root.join(conversation_id).join("history.tsv");
    let history = fs::read(&history_path).unwrap();
    let edited = typed_content("private edited body");
    complete_prompt_editor(&mut service, &root, &protected, edited.as_bytes(), 9);
    let path = store.transcript_path(conversation_id).unwrap();
    let before_failed_cas = service
        .persistence
        .capture_transcript_admission_stamp(&path, conversation_id);
    let error = service
        .apply_external_editor_recovery(&primary, "%1", &protected.2)
        .unwrap_err();
    assert!(
        !service
            .persistence
            .transcript_admission_matches(&before_failed_cas)
    );
    assert!(
        error
            .message()
            .contains("already represented by a compaction epoch"),
        "{error}"
    );
    assert_eq!(fs::read(&history_path).unwrap(), history);
    assert_eq!(store.compaction_epoch(conversation_id).unwrap(), epoch);
    assert_eq!(
        store.inspect(conversation_id).unwrap(),
        vec![original.clone(), uncovered.clone()]
    );
    assert_eq!(
        fs::read_to_string(draft_path(&root, &protected.2)).unwrap(),
        edited
    );
    assert!(
        service
            .list_external_editor_recoveries(&primary)
            .unwrap()
            .contains(&protected.2)
    );
    assert_eq!(
        service.agent_prompt_inputs_for_tests()["%1"]
            .prompt
            .buffer
            .line(),
        "current prompt remains"
    );
    assert!(service.pending_agent_provider_tasks().is_empty());

    service
        .reopen_external_editor_recovery(&primary, "%1", &protected.2)
        .unwrap();
    let reopened = editor_transaction(&service);
    assert_eq!(
        fs::read_to_string(draft_path(&root, &reopened.2)).unwrap(),
        edited
    );
    complete_prompt_editor(&mut service, &root, &reopened, edited.as_bytes(), 0);
    assert_eq!(store.inspect(conversation_id).unwrap()[0], original);

    service
        .start_transcript_entry_external_edit(&primary, "%1", conversation_id, 2)
        .unwrap();
    let allowed = editor_transaction(&service);
    complete_prompt_editor(&mut service, &root, &allowed, b"uncovered after", 9);
    // A same-frontier edit revokes any checked original archive read. Another
    // target with the same CID must not lose its independent admission owner.
    let before_successful_cas = service
        .persistence
        .capture_transcript_admission_stamp(&path, conversation_id);
    let other_path = root.join("other-store").join("transcript.tsv");
    let other_target = service
        .persistence
        .capture_transcript_admission_stamp(&other_path, conversation_id);
    service
        .apply_external_editor_recovery(&primary, "%1", &allowed.2)
        .unwrap();
    assert!(
        !service
            .persistence
            .transcript_admission_matches(&before_successful_cas)
    );
    assert!(
        service
            .persistence
            .transcript_admission_matches(&other_target)
    );
    assert!(
        !service
            .list_external_editor_recoveries(&primary)
            .unwrap()
            .contains(&allowed.2)
    );
    let reopened_store = AgentTranscriptStore::new(store_root);
    let rows = reopened_store.inspect(conversation_id).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .filter(|row| row.content == "uncovered after")
            .count(),
        1
    );
    assert_eq!(
        reopened_store.compaction_epoch(conversation_id).unwrap(),
        epoch
    );
    service
        .start_transcript_entry_external_edit(&primary, "%1", conversation_id, 2)
        .unwrap();
    let visible = editor_transaction(&service);
    assert_eq!(
        fs::read_to_string(draft_path(&root, &visible.2)).unwrap(),
        "uncovered after"
    );
    complete_prompt_editor(&mut service, &root, &visible, b"uncovered after", 0);
    assert_eq!(reopened_store.inspect(conversation_id).unwrap(), rows);
    assert!(service.pending_agent_provider_tasks().is_empty());
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
}
