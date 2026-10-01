//! Sender-side preview eligibility, message acceptance, and durable replay.
//!
//! Provisional rows cannot authorize delivery. Accepted rows preserve captured
//! recipient labels and sent-only source without becoming receiver receipts.

use super::*;

/// Verifies a presentation-eligible outbound message appears before provider
/// completion, retains its requested recipient marker, and obeys the active
/// transcript width cap.
#[test]
fn runtime_streaming_outbound_message_projects_before_completion() {
    let mut service = test_runtime_service();
    service.set_subagent_lineage(
        "agent-%1",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-root".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 1,
            display_name: "child".to_string(),
            terminal: false,
        },
    );
    service.set_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-root".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 1,
            display_name: "nonhuman".to_string(),
            terminal: false,
        },
    );
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nagent_wrap_column_cap = 24\n".to_string(),
        }])
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 12).unwrap(), 120).unwrap(),
    );
    let turn = service
        .start_agent_prompt_turn("%1", "stream an outbound message")
        .unwrap();

    for event in [
        mez_agent::StreamingSayEvent::MessageStarted {
            action_index: 0,
            recipient: "agent-%2".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::MessagePayloadDelta {
            action_index: 0,
            text: "partial outbound payload with averyveryverylongtoken".to_string(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
            .unwrap();
    }
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .expect("partial outbound message should produce projection work"),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );

    let lines = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert!(
        lines.iter().any(|line| line.contains("nonhuman<")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("partial")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .all(|line| unicode_width::UnicodeWidthStr::width(line.as_str()) <= 24),
        "{lines:?}"
    );
}

/// Verifies normal-mode filtering prevents noncanonical outbound message media
/// from creating provisional source state or sender-pane rows.
#[test]
fn runtime_streaming_outbound_message_filters_json_in_normal_mode() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let turn = service
        .start_agent_prompt_turn("%1", "stream a filtered outbound message")
        .unwrap();

    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            &turn.turn_id,
            &mez_agent::StreamingSayEvent::MessageStarted {
                action_index: 0,
                recipient: "agent-%2".to_string(),
                content_type: "application/json".to_string(),
            },
        )
        .unwrap();
    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            &turn.turn_id,
            &mez_agent::StreamingSayEvent::MessagePayloadDelta {
                action_index: 0,
                text: "{\"hidden\":true}".to_string(),
            },
        )
        .unwrap();

    assert!(
        service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .is_none()
    );
}

/// Verifies verbose mode renders an established noncanonical message payload
/// literally and retains its exact provisional source through provider
/// reconciliation until message-service settlement decides its final fate.
#[test]
fn runtime_streaming_outbound_message_renders_verbose_json_until_settlement() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "verbose-peer-log".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\npeer_message_log_mode = \"verbose\"\n".to_string(),
        }])
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 12).unwrap(), 120).unwrap(),
    );
    let turn = service
        .start_agent_prompt_turn("%1", "stream a verbose outbound message")
        .unwrap();
    let baseline = service.agent_pane_screen("%1").unwrap().clone();
    let payload = r#"{\"visible\":true}"#;

    for event in [
        mez_agent::StreamingSayEvent::MessageStarted {
            action_index: 0,
            recipient: "agent-%2".to_string(),
            content_type: "application/json".to_string(),
        },
        mez_agent::StreamingSayEvent::MessagePayloadDelta {
            action_index: 0,
            text: payload.to_string(),
        },
        mez_agent::StreamingSayEvent::MessagePayloadComplete { action_index: 0 },
        mez_agent::StreamingSayEvent::MessageStarted {
            action_index: 1,
            recipient: "agent-%3".to_string(),
            content_type: "application/json".to_string(),
        },
        mez_agent::StreamingSayEvent::MessagePayloadDelta {
            action_index: 1,
            text: "rejected sibling payload".to_string(),
        },
        mez_agent::StreamingSayEvent::MessagePayloadComplete { action_index: 1 },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
            .unwrap();
    }
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .expect("verbose outbound payload should produce projection work"),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    let preview = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(preview.contains("agent-%2<"), "{preview}");
    assert!(preview.contains(payload), "{preview}");

    let action = mez_agent::AgentAction {
        id: "streamed-message".to_string(),
        payload: mez_agent::AgentActionPayload::SendMessage {
            recipient: "agent-%2".to_string(),
            scope: None,
            content_type: "application/json".to_string(),
            payload: payload.to_string(),
            correlation_id: None,
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: payload.to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![
                    action,
                    mez_agent::AgentAction {
                        id: "rejected-streamed-message".to_string(),
                        payload: mez_agent::AgentActionPayload::SendMessage {
                            recipient: "agent-%3".to_string(),
                            scope: None,
                            content_type: "application/json".to_string(),
                            payload: "rejected sibling payload".to_string(),
                            correlation_id: None,
                        },
                    },
                ],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };
    assert!(
        service
            .reconcile_agent_streaming_say_completion("%1", &turn.turn_id, &execution)
            .unwrap()
            .is_empty()
    );
    assert_ne!(service.agent_pane_screen("%1").unwrap(), &baseline);
    let reconciled = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(reconciled.contains("agent-%2<"), "{reconciled}");
    assert!(reconciled.contains(payload), "{reconciled}");

    let ledger_turn = service
        .agent_turn_ledger()
        .turn(&turn.turn_id)
        .expect("streamed message turn remains in the ledger")
        .clone();
    let mut blocked_execution = execution.clone();
    let blocked_batch = blocked_execution.response.action_batch.as_ref().unwrap();
    blocked_execution.action_results = vec![
        mez_agent::ActionResult::succeeded(
            &ledger_turn,
            &blocked_batch.actions[0],
            Vec::new(),
            None,
        ),
        mez_agent::ActionResult::blocked(
            &ledger_turn,
            &blocked_batch.actions[1],
            Vec::new(),
            "{\"approval\":{}}".to_string(),
        ),
    ];
    let blocked_action = &blocked_execution
        .response
        .action_batch
        .as_ref()
        .unwrap()
        .actions[0];
    service
        .settle_accepted_outbound_message_preview("%1", &turn.turn_id, 0, blocked_action)
        .unwrap();
    service
        .finalize_settled_outbound_message_previews("%1", &turn.turn_id, &blocked_execution)
        .unwrap();
    let blocked = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(blocked.contains("rejected sibling payload"), "{blocked}");

    let mut accepted_execution = execution.clone();
    let accepted_batch = accepted_execution.response.action_batch.as_ref().unwrap();
    accepted_execution.action_results = vec![
        mez_agent::ActionResult::succeeded(
            &ledger_turn,
            &accepted_batch.actions[0],
            Vec::new(),
            None,
        ),
        mez_agent::ActionResult::failed(
            &ledger_turn,
            &accepted_batch.actions[1],
            mez_agent::ActionStatus::Failed,
            "transport_error",
            "recipient unavailable",
        )
        .unwrap(),
    ];
    let accepted_action = &accepted_execution
        .response
        .action_batch
        .as_ref()
        .unwrap()
        .actions[0];
    service
        .settle_accepted_outbound_message_preview("%1", &turn.turn_id, 0, accepted_action)
        .unwrap();
    service
        .finalize_settled_outbound_message_previews("%1", &turn.turn_id, &accepted_execution)
        .unwrap();
    service
        .settle_accepted_outbound_message_preview("%1", &turn.turn_id, 0, accepted_action)
        .unwrap();
    let settled = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(settled.matches("agent-%2<").count(), 1, "{settled}");
    assert!(!settled.contains("rejected sibling payload"), "{settled}");
}

/// Verifies an accepted outbound row persists a sent-only semantic source and
/// replays through the outbound renderer without becoming a receiver receipt.
#[test]
fn runtime_accepted_outbound_message_persists_sent_source_for_replay() {
    let mut service = test_runtime_service();
    service.set_subagent_lineage(
        "agent-%1",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-root".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 1,
            display_name: "child".to_string(),
            terminal: false,
        },
    );
    service.set_subagent_lineage(
        "agent-%2",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-root".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 1,
            display_name: "nonhuman".to_string(),
            terminal: false,
        },
    );
    let transcript_store = AgentTranscriptStore::new(temp_root("accepted-outbound-source"));
    service
        .attach_primary("primary", true, Size::new(40, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let turn = service
        .start_agent_prompt_turn("%1", "persist accepted outbound presentation")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    let action = mez_agent::AgentAction {
        id: "accepted-outbound-action".to_string(),
        payload: mez_agent::AgentActionPayload::SendMessage {
            recipient: "agent-%2".to_string(),
            scope: None,
            content_type: "text/plain".to_string(),
            payload: "accepted outbound replay evidence".to_string(),
            correlation_id: None,
        },
    };

    service
        .settle_accepted_outbound_message_preview("%1", &turn.turn_id, 0, &action)
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    let entry = entries
        .iter()
        .find(|entry| {
            entry
                .source_text
                .as_deref()
                .is_some_and(|source| source.contains("accepted outbound replay evidence"))
        })
        .expect("accepted outbound presentation entry");
    let source = entry.source_text.as_deref().unwrap();
    assert!(source.contains("\"direction\":\"sent\""), "{source}");
    assert!(source.contains("\"peer\":\"nonhuman\""), "{source}");
    assert!(source.contains("accepted-outbound-action"), "{source}");
    assert_eq!(
        crate::runtime::render::peer_message_presentation_receive_identity(
            entry.source_content_type.as_deref().unwrap(),
            source,
        ),
        None
    );

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(40, 12).unwrap())
            .unwrap()
    );
    let replayed = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        replayed.contains("nonhuman< accepted outbound replay"),
        "{replayed}"
    );
    assert!(replayed.contains("evidence"), "{replayed}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies settlement preserves a direct-parent label captured by streaming
/// even if the sender's lineage becomes fenced before message acceptance.
#[test]
fn runtime_streamed_outbound_parent_label_persists_after_lineage_fence() {
    let mut service = test_runtime_service();
    service.set_subagent_lineage(
        "agent-%1",
        RuntimeSubagentLineage {
            parent_agent_id: "agent-%2".to_string(),
            root_agent_id: "agent-root".to_string(),
            depth: 2,
            display_name: "child".to_string(),
            terminal: false,
        },
    );
    let transcript_store = AgentTranscriptStore::new(temp_root("streamed-outbound-parent"));
    service
        .attach_primary("primary", true, Size::new(40, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    let turn = service
        .start_agent_prompt_turn("%1", "stream direct-parent message")
        .unwrap();
    let payload = "stable direct-parent evidence";
    for event in [
        mez_agent::StreamingSayEvent::MessageStarted {
            action_index: 0,
            recipient: "agent-%2".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::MessagePayloadDelta {
            action_index: 0,
            text: payload.to_string(),
        },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
            .unwrap();
    }
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(
        service
            .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
            .unwrap()
            .expect("complete outbound message should produce projection work"),
    )
    .unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    assert!(
        service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .iter()
            .any(|line| line.contains("parent< stable direct-parent"))
    );

    service
        .apply_agent_streaming_say_event_to_terminal_buffer(
            "%1",
            &turn.turn_id,
            &mez_agent::StreamingSayEvent::MessagePayloadComplete { action_index: 0 },
        )
        .unwrap();

    service.fence_subagent_descendants_for_parent_conversation("agent-%2", "replaced");
    let action = mez_agent::AgentAction {
        id: "streamed-parent-action".to_string(),
        payload: mez_agent::AgentActionPayload::SendMessage {
            recipient: "agent-%2".to_string(),
            scope: None,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
            payload: payload.to_string(),
            correlation_id: None,
        },
    };
    service
        .settle_accepted_outbound_message_preview("%1", &turn.turn_id, 0, &action)
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    let source = entries
        .iter()
        .filter_map(|entry| entry.source_text.as_deref())
        .find(|source| source.contains(payload))
        .expect("accepted outbound presentation source");
    assert!(source.contains("\"peer\":\"agent-%2\""), "{source}");
    assert!(source.contains("\"direct_parent\":true"), "{source}");

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(40, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(40, 12).unwrap())
            .unwrap()
    );
    let replayed = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        replayed.contains("parent< stable direct-parent evidence"),
        "{replayed}"
    );
    service.terminate_all_pane_processes().unwrap();
}
