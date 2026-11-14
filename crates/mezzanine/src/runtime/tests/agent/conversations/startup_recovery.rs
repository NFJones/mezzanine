//! Ordinary startup preserves refused bindings without installing broken history.

use super::*;

/// A retained accepted receipt whose raw source conflicts must not kill daemon
/// startup. Before normal checkpoints can omit that binding, its exact original
/// file is backed up durably. Repeated launches retain the receipt and source.
#[test]
fn runtime_startup_isolates_conflicting_receipt_and_preserves_original_checkpoint() {
    let mut store = AgentTranscriptStore::new(temp_root("startup-transcript-refusal"));
    store
        .set_saved_session_retention_policy(
            crate::storage::transcript::SavedSessionRetentionPolicy {
                retention_days: 1,
                max_active_sessions: 1,
            },
        )
        .unwrap();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    let conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let row = TranscriptEntry {
        conversation_id: conversation.clone(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role: TranscriptRole::User,
        turn_id: "original-turn".into(),
        agent_id: "agent-%1".into(),
        pane_id: "%1".into(),
        content: "accepted source".into(),
    };
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 1)
        .unwrap();
    service.checkpoint_agent_session_metadata().unwrap();
    let checkpoint = store.agent_session_metadata_checkpoint_file(service.session.id.as_str());
    let original_checkpoint = fs::read(&checkpoint).unwrap();
    store
        .accept_append_receipt(std::slice::from_ref(&row), 1)
        .unwrap();
    let mut changed = row;
    changed.content = "conflicting source".into();
    store.append(&changed).unwrap();
    let raw = fs::read(store.transcript_path(&conversation).unwrap()).unwrap();
    for _ in 0..2 {
        let mut restarted = test_runtime_service();
        restarted.session.id = service.session.id.clone();
        restarted.set_agent_transcript_store(store.clone());
        // Later launches may restore an independent display session allocated
        // by the earlier refusal notice; the failed binding must never return.
        restarted
            .restore_agent_sessions_from_transcript_store()
            .unwrap();
        assert!(
            restarted
                .agent_shell_store()
                .get("%1")
                .is_none_or(|session| session.session_id != conversation)
        );
        assert!(store.has_unsettled_append_receipts(&conversation).unwrap());
        assert_eq!(
            fs::read(store.transcript_path(&conversation).unwrap()).unwrap(),
            raw
        );
        restarted.checkpoint_agent_session_metadata().unwrap();
        let backups = fs::read_dir(checkpoint.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("startup-refusal-")
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(&backups[0]).unwrap(), original_checkpoint);
        assert!(
            store
                .lock_resume_replay_snapshot(&conversation, true)
                .is_err()
        );
        let retention = store
            .enforce_saved_session_retention(i64::MAX as u64, &Default::default())
            .unwrap();
        // Status rendering can allocate an unrelated empty display session.
        // Only the refused source is held by this recovery contract.
        assert!(!retention.deleted_conversation_ids.contains(&conversation));
        assert!(retention.failures.is_empty(), "{retention:?}");
        assert_eq!(
            fs::read(store.transcript_path(&conversation).unwrap()).unwrap(),
            raw
        );
    }
    fs::remove_dir_all(store.root()).unwrap();
}
