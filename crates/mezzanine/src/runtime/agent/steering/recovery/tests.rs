//! Durable recovery integration without input or provider replay.

use super::*;

/// Presentation order follows accepted occurrences, not turn-map ordering,
/// content equality, or status partitioning. Restored order advances the live
/// allocator, and replacement conversations cannot inherit another owner's view.
#[test]
fn steering_recovery_presentation_order_survives_restore_and_new_acceptance() {
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let history = vec![SteeringRecoveryReceipt {
        id: "restored-occurrence".into(),
        acceptance_order: 70,
        turn_id: None,
        event_sequence: None,
        display: "same display".into(),
        status: SteeringRecoveryStatus::Pending,
    }];
    service
        .restore_steering_recovery("%1", &conversation, &history)
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "initial").unwrap();
    for _ in 0..2 {
        service
            .inject_agent_steering_with_display("%1", "exact input", "same display")
            .unwrap();
    }
    let view = service.steering_presentation_receipts("%1").unwrap();
    assert_eq!(
        view.iter()
            .map(|entry| entry.acceptance_order)
            .collect::<Vec<_>>(),
        vec![70, 71, 72]
    );
    assert_eq!(view[0].status, SteeringRecoveryStatus::AdmissionUnknown);
    assert_ne!(view[1].id, view[2].id);
    assert_eq!(view[1].turn_id.as_deref(), Some(started.turn_id.as_str()));
    assert!(view.iter().all(|entry| entry.display == "same display"));
    let before = service.agent_turn_contexts()[&started.turn_id].clone();
    assert_eq!(service.steering_presentation_receipts("%1").unwrap(), view);
    assert_eq!(service.agent_turn_contexts()[&started.turn_id], before);
    service
        .agent_shell_store_mut()
        .finish_turn("%1", &started.turn_id)
        .unwrap();
    service
        .agent_shell_store_mut()
        .start_new_conversation("%1")
        .unwrap();
    assert!(
        service
            .steering_presentation_receipts("%1")
            .unwrap()
            .is_empty()
    );
}

/// Pending occurrences take precedence over bounded terminal recovery history.
/// Duplicate actor copies project once, while excessive pending pressure rejects
/// the checkpoint without dropping accepted source or creating execution work.
#[test]
fn steering_recovery_projection_preserves_pending_and_deduplicates_transfer() {
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "initial").unwrap();
    service
        .inject_agent_steering_with_display("%1", "model input", "pending display")
        .unwrap();
    let conversation = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let occurrence = service.steering_receipts_for_tests(&started.turn_id)[0].clone();
    service.retain_deferred_history_receipts(
        "%1",
        &conversation,
        7,
        std::slice::from_ref(&occurrence),
    );
    let terminal = (0..STEERING_RECOVERY_ENTRIES)
        .map(|index| SteeringRecoveryReceipt {
            id: format!("terminal-{index}"),
            acceptance_order: index as u64 + 2,
            turn_id: None,
            event_sequence: None,
            display: "historical display".into(),
            status: SteeringRecoveryStatus::NotSent,
        })
        .collect::<Vec<_>>();
    service
        .restore_steering_recovery("%1", &conversation, &terminal)
        .unwrap();
    let projected = service
        .steering_recovery_checkpoint("%1", &conversation)
        .unwrap();
    assert_eq!(projected.len(), STEERING_RECOVERY_ENTRIES);
    assert_eq!(projected[0].id, occurrence.id);
    assert_eq!(
        projected[0].turn_id.as_deref(),
        Some(started.turn_id.as_str())
    );
    assert_eq!(projected[0].status, SteeringRecoveryStatus::Pending);
    assert_eq!(
        projected
            .iter()
            .filter(|entry| entry.id == occurrence.id)
            .count(),
        1
    );
    let overflow = (0..STEERING_RECOVERY_ENTRIES)
        .map(|index| {
            let mut entry = super::super::Receipt::deferred(
                "input".into(),
                "display".into(),
                None,
                index as u64 + 200,
            );
            entry.id = format!("pending-{index}");
            entry
        })
        .collect::<Vec<_>>();
    service.retain_deferred_history_receipts("%1", &conversation, 8, &overflow);
    assert!(
        service
            .steering_recovery_checkpoint("%1", &conversation)
            .is_err()
    );
    assert_eq!(
        service.steering_receipts_for_tests(&started.turn_id)[0],
        occurrence
    );
    assert_eq!(
        service.agent.pending_deferred_steering[&("%1".into(), conversation, 8)].len(),
        STEERING_RECOVERY_ENTRIES
    );
    assert_eq!(service.agent_turn_ledger().turns().len(), 1);
}

/// Deferred terminal evidence is published at persistence drain in both direct
/// and adapter modes. A failed direct publication retains the dirty fence for
/// a later drain, without recreating a command or resubmitting accepted input.
#[test]
fn steering_recovery_deferred_settlement_publishes_at_persistence_drain() {
    for adapter in [false, true] {
        for history in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "mez-steering-drain-{}",
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
            service.set_agent_transcript_store(store.clone());
            if adapter {
                service.persistence.enable_transcript_adapter();
            }
            service.mark_agent_compacting_for_tests("%1", 1);
            service
                .execute_agent_shell_command(&primary, "accepted guidance")
                .unwrap();
            service.drain_transcript_persistence_transition();
            if history {
                service
                    .execute_agent_shell_command(&primary, "/stop")
                    .unwrap();
                let dispatch = service.take_pending_agent_prompt_history().remove(0);
                assert!(service.claim_agent_prompt_history_preparation(&dispatch));
                assert!(
                    service
                        .complete_agent_prompt_history_preparation(
                            &dispatch,
                            Err(crate::error::MezError::invalid_state(
                                "history fixture failure"
                            ))
                        )
                        .is_err()
                );
            } else {
                service.discard_agent_compaction_steering("%1");
            }
            assert!(service.steering_recovery_needs_publication());
            if !adapter {
                store.fail_next_agent_session_metadata_write();
                service.drain_transcript_persistence_transition();
                assert!(service.steering_recovery_needs_publication());
            }
            let effects = service
                .drain_transcript_persistence_transition()
                .side_effects;
            assert!(!service.steering_recovery_needs_publication());
            let records = if adapter {
                effects
                    .into_iter()
                    .find_map(|effect| match effect {
                        crate::runtime::RuntimeSideEffect::PersistAgentSessionMetadata {
                            records,
                            ..
                        } => Some(records),
                        _ => None,
                    })
                    .expect("dirty receipts must enqueue a metadata checkpoint")
            } else {
                store
                    .load_agent_session_metadata(service.session().id.as_str())
                    .unwrap()
            };
            assert_eq!(records[0].steering_recovery.len(), 1);
            assert_eq!(
                records[0].steering_recovery[0].status,
                SteeringRecoveryStatus::NotSent
            );
            assert!(service.agent_turn_ledger().turns().is_empty());
            assert!(service.pending_agent_provider_tasks().is_empty());
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}

/// Manual resume reconciles pending checkpoint evidence as uncertainty, never
/// schedules old work, and restores the entire previous inert map on failure.
#[test]
fn steering_recovery_manual_resume_is_inert_and_transactional() {
    for fail in [false, true] {
        let root = std::env::temp_dir().join(format!(
            "mez-steering-resume-{}",
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
        service.set_agent_transcript_store(store.clone());
        service.checkpoint_agent_session_metadata().unwrap();
        let mut target = store
            .load_agent_session_metadata(service.session().id.as_str())
            .unwrap()
            .remove(0);
        target.conversation_id = "receipt-resume-target".into();
        target.primary_display_name = None;
        target.prompt_cache_lineage_id = "target-lineage".into();
        target.transcript_entries = 1;
        target.steering_recovery = vec![SteeringRecoveryReceipt {
            id: "target-occurrence".into(),
            acceptance_order: if fail { u64::MAX } else { 1 },
            turn_id: Some("old-turn".into()),
            event_sequence: Some(9),
            display: "exact display".into(),
            status: SteeringRecoveryStatus::Pending,
        }];
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: target.conversation_id.clone(),
                sequence: 1,
                created_at_unix_seconds: 1,
                role: mez_agent::transcript::TranscriptRole::User,
                turn_id: "old-turn".into(),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: "historical prompt".into(),
            })
            .unwrap();
        store
            .save_agent_session_metadata(service.session().id.as_str(), &[target])
            .unwrap();
        let previous = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .session_id
            .clone();
        service
            .restore_steering_recovery(
                "%1",
                &previous,
                &[SteeringRecoveryReceipt {
                    id: "prior-occurrence".into(),
                    acceptance_order: 1,
                    turn_id: None,
                    event_sequence: None,
                    display: "prior display".into(),
                    status: SteeringRecoveryStatus::NotSent,
                }],
            )
            .unwrap();
        let before = service.snapshot_restored_steering_recovery();
        if fail {
            service.fail_next_agent_resume_after_authority_restore_for_tests();
        }
        let response = service
            .execute_agent_shell_control_command(&primary, "/resume receipt-resume-target")
            .unwrap();
        if fail {
            assert!(response.contains("error"), "{response}");
            assert_eq!(service.snapshot_restored_steering_recovery(), before);
            assert_eq!(
                service.agent_shell_store().get("%1").unwrap().session_id,
                previous
            );
        } else {
            assert!(response.contains("\"kind\":\"mutated\""), "{response}");
            assert_eq!(
                service.agent_shell_store().get("%1").unwrap().session_id,
                "receipt-resume-target"
            );
            let recovered = service
                .steering_recovery_checkpoint("%1", "receipt-resume-target")
                .unwrap();
            assert_eq!(recovered.len(), 1);
            assert_eq!(recovered[0].id, "target-occurrence");
            assert_eq!(
                recovered[0].status,
                SteeringRecoveryStatus::AdmissionUnknown
            );
        }
        assert!(service.pending_agent_provider_tasks().is_empty());
        assert!(service.agent_turn_ledger().turns().is_empty());
        if fail {
            let started = service
                .start_agent_prompt_turn("%1", "continue old conversation")
                .unwrap();
            assert_eq!(
                service
                    .inject_agent_steering_with_display("%1", "new guidance", "display")
                    .unwrap(),
                Some(started.turn_id)
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

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
