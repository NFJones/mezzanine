//! Saved-fork continuity at the actual first provider encoding boundary.
//!
//! All history is synthetic. These tests run the real fork command and reopen
//! durable storage without sending provider requests or replaying any actions.

use super::*;

/// Failure after successful epoch publication but before pane setup completes
/// removes every new target artifact, including its epoch and required marker,
/// while keeping the compacted parent's exact durable bytes and live binding.
#[test]
fn runtime_saved_fork_epoch_setup_failure_removes_only_published_target() {
    let root = temp_root("runtime-fork-epoch-setup-fault");
    let store = AgentTranscriptStore::new(root.clone());
    store
        .append(&TranscriptEntry {
            conversation_id: "fork-source".into(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "source-turn".into(),
            agent_id: "agent-%1".into(),
            pane_id: "%1".into(),
            content: "source prompt".into(),
        })
        .unwrap();
    store
        .save_compaction_epoch("fork-source", 1, "source model summary")
        .unwrap();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "fork-source", 1)
        .unwrap();
    service.checkpoint_agent_session_metadata().unwrap();
    let before = fs::read_dir(root.join("fork-source"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (path.clone(), fs::read(path).unwrap())
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let panes = service.session().windows()[0].panes().len();
    service.fail_next_agent_fork_after_persistence_for_tests();
    let response=service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"epoch-setup-fault","method":"agent/shell/command","params":{"idempotency_key":"epoch-setup-fault","input":"/fork fork-target"}}"#,
        &primary,
    );
    service.terminate_all_pane_processes().unwrap();
    assert!(
        response.contains("injected agent fork failure after persistence"),
        "{response}"
    );
    assert_eq!(service.session().windows()[0].panes().len(), panes);
    assert!(!root.join("fork-target").exists());
    assert!(store.saved_session("fork-target").unwrap().is_none());
    assert_eq!(
        service.agent_shell_store().get("%1").unwrap().session_id,
        "fork-source"
    );
    let after = fs::read_dir(root.join("fork-source"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (path.clone(), fs::read(path).unwrap())
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(after, before);
}

/// Selective epochs without reconstructible terminal witnesses must survive a
/// real fork and the first encoded request after reopen. Inclusive ranges keep
/// their original anchors, user barriers and assistant roles remain ordered,
/// and an uncovered provider-owned native item retains its exact wire shape.
/// Later parent work cannot leak in; later target work appears exactly once.
#[test]
fn runtime_saved_fork_selective_epoch_encoded_anchors_and_native_ownership() {
    use crate::storage::transcript::{AgentCompactionEpoch, AgentCompactionRange};
    for prefix in [0, 1] {
        let root = temp_root("runtime-fork-selective-epoch");
        let store = AgentTranscriptStore::new(root.clone());
        let mut service = test_runtime_service();
        service.set_agent_transcript_store(store.clone());
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .start_initial_pane_process(Some("cat >/dev/null"))
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let (_, profile) = service
            .active_model_profile_for_pane("%1", "agent-%1", None)
            .unwrap();
        let owner = mez_agent::ProviderContinuityOwner::new(
            mez_agent::ProviderApiCompatibility::OpenAiResponses,
            &profile.provider,
        )
        .unwrap();
        let native_item = serde_json::json!({
            "type":"message", "id":"fork-native-msg", "role":"assistant",
            "content":[{"type":"output_text", "text":"FORK_ROW_010"}]
        });
        let native = mez_agent::ProviderTranscriptEvent::validated_openai_response_output(vec![
            native_item.clone(),
        ])
        .unwrap();
        let rows = (1..=11)
            .map(|sequence| {
                let mut row = TranscriptEntry {
                    conversation_id: "fork-source".into(),
                    sequence,
                    created_at_unix_seconds: sequence,
                    role: if matches!(sequence, 5 | 9) {
                        TranscriptRole::Assistant
                    } else {
                        TranscriptRole::User
                    },
                    turn_id: format!("turn-{sequence}"),
                    agent_id: "agent-%1".into(),
                    pane_id: "%1".into(),
                    content: format!("FORK_ROW_{sequence:03}"),
                };
                if matches!(sequence, 2..=3 | 6..=7 | 10..=11) {
                    row.role = TranscriptRole::System;
                    if sequence >= 10 {
                        // Native continuity requires its neutral assistant twin
                        // before provider output in the same complete group.
                        row.turn_id = "native-turn".into();
                    }
                    let (group, ordinal) = match sequence {
                        2..=3 => ("first", sequence - 1),
                        6..=7 => ("second", sequence - 5),
                        _ => ("native", sequence - 9),
                    };
                    row.content = mez_agent::TranscriptContextEvent::execution_block_with_metadata(
                        if sequence == 11 {
                            ContextSourceKind::TranscriptTool
                        } else {
                            ContextSourceKind::TranscriptAssistant
                        },
                        "answer",
                        if sequence == 11 {
                            native.to_transcript_content()
                        } else {
                            row.content
                        },
                        mez_agent::ContextExecutionGroupId::new(group).unwrap(),
                        ordinal,
                        (sequence == 11).then(|| owner.clone()),
                    )
                    .unwrap()
                    .to_transcript_content();
                }
                row
            })
            .collect::<Vec<_>>();
        store.append_many(&rows).unwrap();
        let epoch = AgentCompactionEpoch {
            version: 2,
            conversation_id: "fork-source".into(),
            through_sequence: prefix,
            summary: if prefix == 0 {
                String::new()
            } else {
                "FORK_SUMMARY_PREFIX".into()
            },
            ranges: vec![
                AgentCompactionRange {
                    first_sequence: 2,
                    through_sequence: 3,
                    summary: "FORK_SUMMARY_FIRST".into(),
                },
                AgentCompactionRange {
                    first_sequence: 6,
                    through_sequence: 7,
                    summary: "FORK_SUMMARY_SECOND".into(),
                },
            ],
        };
        let selected = rows
            .iter()
            .filter(|row| matches!(row.sequence, 2..=3 | 6..=7))
            .cloned()
            .collect::<Vec<_>>();
        store
            .save_compaction_ranges_with_proof(epoch, &selected)
            .unwrap();
        let source_path = store.transcript_path("fork-source").unwrap();
        let epoch_path = source_path.parent().unwrap().join("compaction-epoch.json");
        let source_bytes = fs::read(&source_path).unwrap();
        let epoch_bytes = fs::read(&epoch_path).unwrap();
        service
            .agent_shell_store_mut()
            .bind_conversation("%1", "fork-source", 11)
            .unwrap();
        let response = service
            .execute_agent_shell_command(&primary, "/fork fork-target")
            .unwrap();
        assert!(response.contains("forked=true"), "{response}");
        let pane = service
            .agent_shell_store()
            .sessions()
            .find(|session| session.session_id == "fork-target")
            .unwrap()
            .pane_id
            .clone();
        let actual = encoded_history(&mut service, &pane);
        let body = encoded_body(&mut service, &pane);
        let context = service
            .agent_context_for_pane_prompt(&pane, "continue", 0)
            .unwrap();
        service.terminate_all_pane_processes().unwrap();
        let expected = [
            if prefix == 0 {
                ("user", "FORK_ROW_001")
            } else {
                ("developer", "FORK_SUMMARY_PREFIX")
            },
            ("developer", "FORK_SUMMARY_FIRST"),
            ("user", "FORK_ROW_004"),
            ("assistant", "FORK_ROW_005"),
            ("developer", "FORK_SUMMARY_SECOND"),
            ("user", "FORK_ROW_008"),
            ("assistant", "FORK_ROW_009"),
            ("assistant", "FORK_ROW_010"),
        ]
        .into_iter()
        .map(|(role, text)| (role.to_string(), text.to_string()))
        .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        assert!(
            body["input"].as_array().unwrap().contains(&native_item),
            "{body}"
        );
        assert!(
            context
                .chronology()
                .iter()
                .any(|event| event.provider_owner() == Some(&owner))
        );
        assert_eq!(fs::read(&source_path).unwrap(), source_bytes);
        assert_eq!(fs::read(&epoch_path).unwrap(), epoch_bytes);
        let mut reopened = test_runtime_service();
        reopened.set_agent_transcript_store(AgentTranscriptStore::new(root));
        let primary = reopened
            .attach_primary("reopened", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        reopened
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        reopened
            .execute_agent_shell_command(&primary, "/resume fork-target")
            .unwrap();
        let settled = reopened
            .run_pending_deferred_agent_command_for_tests()
            .unwrap()
            .unwrap();
        assert!(settled.contains("resumed=true"), "{settled}");
        assert_eq!(encoded_history(&mut reopened, "%1"), expected);
        assert!(
            encoded_body(&mut reopened, "%1")["input"]
                .as_array()
                .unwrap()
                .contains(&native_item)
        );
        assert!(
            reopened
                .agent_context_for_pane_prompt("%1", "continue", 0)
                .unwrap()
                .chronology()
                .iter()
                .any(|event| event.provider_owner() == Some(&owner))
        );
        let mut later = rows[0].clone();
        later.sequence = 12;
        later.content = "FORK_ROW_PARENT_ONLY".into();
        store.append(&later).unwrap();
        later.conversation_id = "fork-target".into();
        later.content = "FORK_ROW_012".into();
        store.append(&later).unwrap();
        let mut expected = expected;
        expected.push(("user".into(), "FORK_ROW_012".into()));
        assert_eq!(encoded_history(&mut reopened, "%1"), expected);
    }
}

/// A rejected existing-target or source-equals-target request owns no durable
/// target. Runtime compensation must close only its newly created pane and
/// preserve both existing conversations byte-for-byte, including epoch markers.
#[test]
fn runtime_saved_fork_rejected_target_preserves_existing_and_source_bytes() {
    for target in ["fork-source", "existing-target"] {
        let root = temp_root("runtime-fork-existing-target");
        let store = AgentTranscriptStore::new(root.clone());
        for conversation in ["fork-source", "existing-target"] {
            store
                .append(&TranscriptEntry {
                    conversation_id: conversation.into(),
                    sequence: 1,
                    created_at_unix_seconds: 1,
                    role: TranscriptRole::User,
                    turn_id: "original-turn".into(),
                    agent_id: "agent-%1".into(),
                    pane_id: "%1".into(),
                    content: format!("original {conversation}"),
                })
                .unwrap();
            store
                .save_compaction_epoch(conversation, 1, "original summary")
                .unwrap();
        }
        let mut service = test_runtime_service();
        service.set_agent_transcript_store(store);
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .start_initial_pane_process(Some("cat >/dev/null"))
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service
            .agent_shell_store_mut()
            .bind_conversation("%1", "fork-source", 1)
            .unwrap();
        let before = service.session().windows()[0].panes().len();
        let snapshots = ["fork-source", "existing-target"].map(|conversation| {
            fs::read_dir(root.join(conversation))
                .unwrap()
                .map(|entry| {
                    let path = entry.unwrap().path();
                    (path.clone(), fs::read(path).unwrap())
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        });
        let response = service.dispatch_runtime_control_body(&format!(
            r#"{{"jsonrpc":"2.0","id":"existing-fork","method":"agent/shell/command","params":{{"idempotency_key":"existing-fork","input":"/fork {target}"}}}}"#
        ), &primary);
        service.terminate_all_pane_processes().unwrap();
        assert!(
            response.contains("target conversation already exists"),
            "{response}"
        );
        assert_eq!(service.session().windows()[0].panes().len(), before);
        assert_eq!(
            service.agent_shell_store().get("%1").unwrap().session_id,
            "fork-source"
        );
        for (conversation, expected) in ["fork-source", "existing-target"]
            .into_iter()
            .zip(snapshots)
        {
            let actual = fs::read_dir(root.join(conversation))
                .unwrap()
                .map(|entry| {
                    let path = entry.unwrap().path();
                    (path.clone(), fs::read(path).unwrap())
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(actual, expected, "{conversation}");
        }
    }
}

/// Encodes the first prompt through the real provider adapter without sending
/// it. Native items and message roles remain inspectable in the returned JSON.
fn encoded_body(service: &mut RuntimeSessionService, pane: &str) -> serde_json::Value {
    let context = service
        .agent_context_for_pane_prompt(pane, "continue", 0)
        .unwrap();
    let (_, profile) = service
        .active_model_profile_for_pane(pane, "agent-%1", None)
        .unwrap();
    let turn = mez_agent::AgentTurnRecord {
        turn_id: "fork-first-request".into(),
        conversation_id: "fork-target".into(),
        agent_id: "agent-%1".into(),
        pane_id: pane.into(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 3,
        deadline_at_unix_millis: 0,
        policy_profile: "runtime".into(),
        model_profile: "test".into(),
        parent_turn_id: None,
        state: AgentTurnState::Running,
        cooperation_mode: None,
        initial_capability: None,
    };
    let request = crate::integrations::agent::context::assemble_model_request(
        &profile,
        mez_agent::ProviderApiCompatibility::OpenAiResponses,
        &turn,
        &context,
    )
    .unwrap();
    let bytes = mez_agent::openai_responses_request_body_with_stream(&request, false).unwrap();
    serde_json::from_str(&bytes).unwrap()
}

/// Extracts exact single-line synthetic payloads and roles from encoded provider
/// messages. Only the pane-local replay label is excluded, so reopening in a new
/// pane does not create a false difference in otherwise identical history.
fn encoded_history(service: &mut RuntimeSessionService, pane: &str) -> Vec<(String, String)> {
    let body = encoded_body(service, pane);
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|item| {
            item["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|part| {
                    let text = part["text"].as_str()?;
                    (text.contains("FORK_ROW_") || text.contains("FORK_SUMMARY")).then(|| {
                        (
                            item["role"].as_str().unwrap().to_owned(),
                            text.lines().last().unwrap().to_owned(),
                        )
                    })
                })
        })
        .collect()
}

/// A saved source summarized through row 80 of 100 must fork its active
/// projection, not resurrect 80 covered rows. The real command's first encoded
/// request and a reopened/resumed first request must contain precisely the same
/// summary and ordered suffix while the full archive remains available.
#[test]
fn runtime_saved_fork_epoch_first_encoded_request_survives_reopen() {
    let root = temp_root("runtime-saved-fork-epoch");
    let store = AgentTranscriptStore::new(root.clone());
    for sequence in 1..=100 {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "fork-source".into(),
                sequence,
                created_at_unix_seconds: sequence,
                role: if sequence % 2 == 0 {
                    mez_agent::transcript::TranscriptRole::Assistant
                } else {
                    mez_agent::transcript::TranscriptRole::User
                },
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: format!("FORK_ROW_{sequence:03}"),
            })
            .unwrap();
    }
    store
        .save_compaction_epoch("fork-source", 80, "FORK_SUMMARY")
        .unwrap();
    let source_path = store.transcript_path("fork-source").unwrap();
    let epoch_path = source_path.parent().unwrap().join("compaction-epoch.json");
    let source_bytes = fs::read(&source_path).unwrap();
    let epoch_bytes = fs::read(&epoch_path).unwrap();
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let resumed = service
        .execute_agent_shell_command(&primary, "/resume fork-source")
        .unwrap();
    assert!(!resumed.contains("\"error\""), "{resumed}");
    let settled = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert!(settled.contains("resumed=true"), "{settled}");
    let forked = service
        .execute_agent_shell_command(&primary, "/fork fork-target")
        .unwrap();
    assert!(forked.contains("forked=true"), "{forked}");
    let pane = service
        .agent_shell_store()
        .sessions()
        .find(|session| session.session_id == "fork-target")
        .unwrap()
        .pane_id
        .clone();
    let encoded = encoded_history(&mut service, &pane);
    service.terminate_all_pane_processes().unwrap();
    let expected = std::iter::once(("developer".to_string(), "FORK_SUMMARY".to_string()))
        .chain((81..=100).map(|sequence| {
            (
                if sequence % 2 == 0 {
                    "assistant"
                } else {
                    "user"
                }
                .to_string(),
                format!("FORK_ROW_{sequence:03}"),
            )
        }))
        .collect::<Vec<_>>();
    assert_eq!(encoded, expected);
    assert_eq!(fs::read(&source_path).unwrap(), source_bytes);
    assert_eq!(fs::read(&epoch_path).unwrap(), epoch_bytes);
    assert_eq!(store.inspect("fork-target").unwrap().len(), 100);
    let mut reopened = test_runtime_service();
    reopened.set_agent_transcript_store(AgentTranscriptStore::new(root));
    let primary = reopened
        .attach_primary("reopened", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    reopened
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let resumed = reopened
        .execute_agent_shell_command(&primary, "/resume fork-target")
        .unwrap();
    assert!(!resumed.contains("\"error\""), "{resumed}");
    let settled = reopened
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .unwrap();
    assert!(settled.contains("resumed=true"), "{settled}");
    assert_eq!(encoded_history(&mut reopened, "%1"), encoded);
}
