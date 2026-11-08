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

/// The real editor/recovery SAVE reaches the single-head content transaction,
/// normalizes cached costs and reopens the exact saved row. A user-facing edit
/// preserves the original summary/epoch and does not dispatch provider work.
#[test]
fn runtime_transcript_editor_saves_retained_single_head_raw_member() {
    use mez_agent::transcript::{
        TranscriptContextEvent as Event, TranscriptContextRecord as Record,
    };
    use mez_agent::{
        AgentContext, ReplayCompactionPreparation, ReplayConversationId, ReplayEpochContext,
        ReplayHeadPublication, ReplayTailPolicy,
    };
    let (mut service, primary, root) = prompt_editor_fixture("single-head-raw-editor-save");
    let store = AgentTranscriptStore::new(root.join("agent-sessions"));
    let parent = "typed-editor-parent";
    let cid = "typed-editor-child";
    let mut owner = ReplayEpochContext::new(
        ReplayConversationId::new(parent).unwrap(),
        AgentContext::empty(),
    )
    .unwrap();
    owner
        .append_fact("covered", "original source represented by the head")
        .unwrap();
    let ReplayCompactionPreparation::Ready(plan) = owner
        .prepare_compaction(ReplayTailPolicy::new(1000, 0).unwrap())
        .unwrap()
    else {
        panic!("expected source");
    };
    owner
        .append_fact("tail", "original editable source")
        .unwrap();
    let origin = crate::storage::transcript::replay_epoch::ReplaySourceOrigin::new(
        "editor-turn",
        "agent-%1",
        "%1",
    )
    .unwrap();
    let proposal = store
        .prepare_replay_epoch(
            origin.capture(None),
            parent,
            None,
            &owner,
            plan,
            ReplayHeadPublication::Summary("original model summary".into()),
        )
        .unwrap();
    let receipt = store.publish_replay_epoch(&proposal).unwrap();
    let snapshot = receipt.snapshot(&store, &proposal).unwrap();
    let (_, owner) = store
        .load_replay_epoch(parent, AgentContext::empty())
        .unwrap()
        .unwrap();
    let source = snapshot.fork_source(&owner, &[]).unwrap();
    let batch = source.initial_batch(cid, 100).unwrap();
    store.accept_transcript_append_receipt(&batch, 1).unwrap();
    store.settle_transcript_append_receipt(&batch, 1).unwrap();
    let (identity, before) = store
        .load_replay_epoch(cid, AgentContext::empty())
        .unwrap()
        .unwrap();
    let original = store.inspect(cid).unwrap().remove(0);
    let record = Record::from_transcript_content(&original.content)
        .unwrap()
        .unwrap();
    let Event::PromptBoundary {
        source,
        event_sequence: Some(sequence),
        label,
        ..
    } = record.event()
    else {
        panic!("expected retained fact");
    };
    let event = Event::prompt_boundary_with_event_sequence(
        *source,
        *sequence,
        label.clone(),
        "edited through the actual recovery SAVE 界",
    )
    .unwrap();
    let edited = Record::new(
        event,
        record.record(),
        record.stable_records().map(<[_]>::to_vec),
    )
    .unwrap()
    .to_transcript_content()
    .unwrap();
    service.set_agent_transcript_store(store.clone());
    service
        .apply_attached_agent_prompt_input_for_pane(&primary, "%1", b"current prompt remains")
        .unwrap();
    service
        .start_transcript_entry_external_edit(&primary, "%1", cid, original.sequence)
        .unwrap();
    let transaction = editor_transaction(&service);
    complete_prompt_editor(&mut service, &root, &transaction, edited.as_bytes(), 9);
    let path = store.transcript_path(cid).unwrap();
    let admission = service
        .persistence
        .capture_transcript_admission_stamp(&path, cid);
    service
        .apply_external_editor_recovery(&primary, "%1", &transaction.2)
        .unwrap();
    assert!(!service.persistence.transcript_admission_matches(&admission));
    assert!(
        !service
            .list_external_editor_recoveries(&primary)
            .unwrap()
            .contains(&transaction.2)
    );
    let cold = AgentTranscriptStore::new(root.join("agent-sessions"));
    let (saved_identity, saved) = cold
        .load_replay_epoch(cid, AgentContext::empty())
        .unwrap()
        .unwrap();
    assert_ne!(saved_identity, identity);
    assert_eq!(saved.revision(), before.revision());
    assert_eq!(saved.head(), before.head());
    assert_eq!(saved.members(), before.members());
    assert_eq!(
        saved.context().chronology()[0],
        before.context().chronology()[0]
    );
    assert_eq!(
        saved.context().chronology()[1].block().content,
        "edited through the actual recovery SAVE 界"
    );
    let rows = cold.inspect(cid).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        Record::from_transcript_content(&rows[0].content)
            .unwrap()
            .unwrap()
            .record(),
        saved.context().chronology()[1].record()
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
        .start_transcript_entry_external_edit(&primary, "%1", cid, original.sequence)
        .unwrap();
    let reopened = editor_transaction(&service);
    assert_eq!(
        fs::read_to_string(draft_path(&root, &reopened.2)).unwrap(),
        rows[0].content
    );
    complete_prompt_editor(
        &mut service,
        &root,
        &reopened,
        rows[0].content.as_bytes(),
        0,
    );
    assert_eq!(
        cold.load_replay_epoch(cid, AgentContext::empty())
            .unwrap()
            .unwrap()
            .0,
        saved_identity
    );
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(root).unwrap();
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
        .install_legacy_compaction_fixture(crate::storage::transcript::AgentCompactionEpoch {
            summary_records: None,
            version: 2,
            conversation_id: conversation_id.to_string(),
            through_sequence: 0,
            summary: String::new(),
            ranges: vec![crate::storage::transcript::AgentCompactionRange {
                first_sequence: 1,
                through_sequence: 1,
                summary: "authoritative group summary".to_string(),
            }],
        })
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
