//! Durable recovery integration without input or provider replay.

use super::*;

/// Persistent metadata publication failure must not turn public steering
/// acceptance into an error after canonical insertion and receipt retention.
#[test]
fn steering_recovery_public_ingress_preserves_acceptance_on_persistent_failure() {
    let root = std::env::temp_dir().join(format!(
        "mez-steering-public-failure-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    let store = crate::storage::transcript::AgentTranscriptStore::new(root.clone());
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    let primary = service
        .attach_primary(
            "primary",
            true,
            mez_mux::layout::Size::new(80, 24).unwrap(),
            120,
        )
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "initial").unwrap();
    service.set_agent_transcript_store(store.clone());
    service.checkpoint_agent_session_metadata().unwrap();
    let path = store.agent_session_metadata_path_for_tests();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let result =
        service.execute_agent_shell_command(&primary, "accepted despite checkpoint failure");
    assert_eq!(
        service.agent_turn_contexts()[&started.turn_id]
            .blocks()
            .iter()
            .filter(|block| block.content == "accepted despite checkpoint failure")
            .count(),
        1
    );
    assert_eq!(
        service.steering_receipts_for_tests(&started.turn_id).len(),
        1
    );
    std::fs::remove_dir_all(root).unwrap();
    let response = result.expect("accepted steering must not return a publication error");
    assert!(response.contains("injected_user_input=true"), "{response}");
}

/// A failed automatic recovery checkpoint cannot undo canonical acceptance or
/// ask the caller to resend. The live occurrence remains available for a later
/// checkpoint, while the durable snapshot truthfully remains older.
#[test]
fn steering_recovery_publication_failure_keeps_accepted_input() {
    let root = std::env::temp_dir().join(format!(
        "mez-steering-publication-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    let store = crate::storage::transcript::AgentTranscriptStore::new(root.clone());
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "initial").unwrap();
    service.set_agent_transcript_store(store.clone());
    service.checkpoint_agent_session_metadata().unwrap();
    store.fail_next_agent_session_metadata_write();
    assert_eq!(
        service
            .inject_agent_steering_with_display("%1", "exact input", "display")
            .unwrap(),
        Some(started.turn_id.clone())
    );
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let captured = service
        .steering_recovery_checkpoint("%1", &conversation)
        .unwrap();
    assert_eq!(captured.len(), 1);
    assert!(
        store
            .load_agent_session_metadata(service.session().id.as_str())
            .unwrap()[0]
            .steering_recovery
            .is_empty()
    );
    assert_eq!(
        service.agent_turn_contexts()[&started.turn_id]
            .blocks()
            .iter()
            .filter(|block| block.content == "exact input")
            .count(),
        1
    );
    service.checkpoint_agent_session_metadata().unwrap();
    assert_eq!(
        store
            .load_agent_session_metadata(service.session().id.as_str())
            .unwrap()[0]
            .steering_recovery,
        captured
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// Captures equal occurrences in real session metadata, rejects a failed newer
/// checkpoint without losing live receipts, then hydrates a fresh runtime.
/// Pending evidence becomes uncertainty and never creates execution ownership.
#[test]
fn steering_recovery_checkpoint_restart_is_inert_and_failure_preserves_receipts() {
    let root = std::env::temp_dir().join(format!(
        "mez-steering-recovery-{}",
        crate::storage::token_usage::new_token_usage_event_id()
    ));
    let store = crate::storage::transcript::AgentTranscriptStore::new(root.clone());
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "initial").unwrap();
    service.set_agent_transcript_store(store.clone());
    for display in ["first display", "second display"] {
        service
            .inject_agent_steering_with_display("%1", "same model input", display)
            .unwrap();
    }
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let captured = service
        .steering_recovery_checkpoint("%1", &conversation)
        .unwrap();
    assert_eq!(captured.len(), 2);
    assert_ne!(captured[0].id, captured[1].id);
    assert_eq!(captured[1].display, "second display");
    let saved = store
        .load_agent_session_metadata(service.session().id.as_str())
        .unwrap();
    assert_eq!(saved[0].steering_recovery, captured);
    store.fail_next_agent_session_metadata_write();
    assert!(service.checkpoint_agent_session_metadata().is_err());
    assert_eq!(
        service
            .steering_recovery_checkpoint("%1", &conversation)
            .unwrap(),
        captured
    );
    let mut restored = crate::test_support::runtime::RuntimeServiceFixture::new()
        .build_with_session(service.session().clone());
    restored.set_agent_transcript_store(store.clone());
    assert_eq!(
        restored
            .restore_agent_sessions_from_transcript_store()
            .unwrap(),
        1
    );
    let recovered = restored
        .steering_recovery_checkpoint("%1", &conversation)
        .unwrap();
    assert_eq!(
        recovered.iter().map(|entry| &entry.id).collect::<Vec<_>>(),
        captured.iter().map(|entry| &entry.id).collect::<Vec<_>>()
    );
    assert!(
        recovered
            .iter()
            .all(|entry| entry.status == SteeringRecoveryStatus::AdmissionUnknown)
    );
    assert!(restored.pending_agent_provider_tasks().is_empty());
    assert!(
        !restored
            .agent_turn_contexts()
            .contains_key(&started.turn_id)
    );
    assert_eq!(
        restored
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .running_turn_id,
        None
    );
    assert_eq!(
        store
            .load_agent_session_metadata(restored.session().id.as_str())
            .unwrap()[0]
            .steering_recovery,
        recovered
    );
    std::fs::remove_dir_all(root).unwrap();
}
