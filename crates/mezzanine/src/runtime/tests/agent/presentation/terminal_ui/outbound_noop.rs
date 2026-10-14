//! Real message acceptance across metadata-only streamed preview settlement.
//!
//! Closing a streamed payload can dirty its source without changing any screen
//! bytes. Settlement must accept that generation, retire ownership and continue
//! normally, never repeating transport to repair presentation.

use super::*;

/// Exercises genuine message-service acceptance after the complete payload was
/// already displayed while open. Plain/Markdown, hidden normal-mode JSON and
/// verbose JSON retain their eligibility at wide/narrow widths with timestamps
/// on and off. Repeated finalization cannot resend, repersist or alter receipts.
#[test]
fn runtime_streamed_outbound_noop_settlement_continues_exactly_once() {
    for (content_type, verbose, payload) in [
        ("text/plain; charset=utf-8", false, "settled payload evidence"),
        ("text/markdown; charset=utf-8", false, "**settled** payload evidence"),
        ("application/json", false, "{\"settled\":true}"),
        ("application/json", true, "{\"settled\":true}"),
    ] {
        for cap in [24, 80] {
            for timestamps in [false, true] {
                streamed_outbound_noop_case(content_type, verbose, payload, cap, timestamps);
            }
        }
    }
}

/// Runs one real sender/receiver pair without a provider or external transport.
fn streamed_outbound_noop_case(
    content_type: &str,
    verbose: bool,
    payload: &str,
    cap: usize,
    timestamps: bool,
) {
    let mut service = test_runtime_service();
    super::timestamp_settings::replace_timestamp_settings(
        &mut service,
        timestamps,
        cap,
        if verbose { "[agents]\npeer_message_log_mode = \"verbose\"\n" } else { "" },
    );
    let primary = service
        .attach_primary("primary", true, Size::new(160, 40).unwrap(), 200)
        .unwrap();
    service.start_initial_pane_process(Some("cat >/dev/null")).unwrap();
    let peer = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap();
    let peer_pane = peer.pane_id.as_str();
    let peer_agent = format!("agent-{peer_pane}");
    let store = AgentTranscriptStore::new(temp_root("outbound-noop"));
    service.set_agent_transcript_store(store.clone());
    let conversation = service.agent_shell_store_mut().enter_or_resume("%1").unwrap().session_id.clone();
    service.agent_shell_store_mut().enter_or_resume(peer_pane).unwrap();
    service.ensure_runtime_message_identity(&peer_agent, None, "agent", &[], crate::runtime::current_unix_millis()).unwrap();
    set_agent_pane_screen_for_test(&mut service, "%1", TerminalScreen::new(Size::new(80, 24).unwrap(), 200).unwrap());
    let started = service.start_agent_prompt_turn("%1", "send once and continue").unwrap();
    service.remove_pending_agent_provider_task(&started.turn_id);
    let turn = service.agent_turn_ledger().turn(&started.turn_id).unwrap().clone();
    service.presentation.occurrence_clock_millis = Some(Some(3_723_000));
    let visible = verbose || content_type != "application/json";
    for event in [
        mez_agent::StreamingSayEvent::MessageStarted { action_index: 0, recipient: peer_agent.clone(), content_type: content_type.into() },
        mez_agent::StreamingSayEvent::MessagePayloadDelta { action_index: 0, text: payload.into() },
    ] {
        service.apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event).unwrap();
    }
    let work = service.take_agent_streaming_say_projection_work("%1", &turn.turn_id).unwrap();
    assert_eq!(work.is_some(), visible);
    if let Some(work) = work {
        assert!(service.apply_agent_streaming_say_projection_result(RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap()).unwrap());
    }
    let displayed = service.agent_pane_screen("%1").unwrap().clone();
    let lineage = service.agent_pane_screen_lineage("%1", &conversation);
    let original_entry = displayed.normal_styled_content_lines().iter().find_map(|row| row.presentation.clone());
    for event in [
        mez_agent::StreamingSayEvent::MessagePayloadComplete { action_index: 0 },
        mez_agent::StreamingSayEvent::ActionComplete { action_index: 0 },
    ] {
        service.apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event).unwrap();
    }
    let action = mez_agent::AgentAction {
        id: "accepted-noop-message".into(),
        payload: mez_agent::AgentActionPayload::SendMessage {
            recipient: peer_agent.clone(), scope: None, content_type: content_type.into(),
            payload: payload.into(), correlation_id: Some("noop-correlation".into()),
        },
    };
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".into(), model: "test".into(), raw_text: String::new(),
            usage: Default::default(), latest_request_usage: None, quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch { rationale: String::new(), actions: vec![action.clone()] }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(), routing_token_usage_by_model: Default::default(),
        action_results: vec![mez_agent::ActionResult::running(&turn, &action, Vec::new(), None)],
        final_turn: false, terminal_state: AgentTurnState::Running,
    };
    service.reconcile_agent_streaming_say_completion("%1", &turn.turn_id, &execution).unwrap();
    // This must not pass vacuously because the finalizer found no dirty work.
    let work = service.take_agent_streaming_say_projection_work("%1", &turn.turn_id).unwrap();
    assert_eq!(work.is_some(), visible);
    if let Some(work) = work {
        let candidate = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
        assert_eq!(candidate.screen, displayed, "fixture must reach an identical current projection");
        assert_eq!(Some(candidate.installed_lineage), lineage);
        assert_eq!(candidate.turn_id, turn.turn_id);
        assert_eq!(candidate.conversation_id, conversation);
    }
    let sequence = service.message_service().next_message_sequence();
    let metrics_before = service.runtime_metrics().clone();
    service.presentation.occurrence_clock_millis = Some(Some(99_999_000));
    // Baseline fails here AFTER message-service acceptance; do not retry sends.
    assert_eq!(service.execute_running_message_actions_for_turn(&turn, &mut execution).unwrap(), 1);
    assert_eq!(execution.action_results[0].status, mez_agent::ActionStatus::Succeeded);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let result: serde_json::Value = serde_json::from_str(execution.action_results[0].structured_content_json.as_deref().unwrap()).unwrap();
    let message_id = format!("{}:{}", turn.turn_id, action.id);
    assert_eq!(result["message_id"], message_id);
    assert_eq!(result["delivery_status"], "accepted");
    assert_eq!(result["queued_recipients"], 1);
    assert_eq!(result["sequence"], sequence);
    assert!(!service.finalize_agent_streaming_say_presentation("%1", Some(&turn.turn_id)).unwrap(), "settlement must already have retired streaming ownership");
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &displayed);
    assert_eq!(service.agent_pane_screen_lineage("%1", &conversation), lineage);
    let metrics = service.runtime_metrics();
    assert_eq!(metrics.agent_streaming_projection_results, metrics_before.agent_streaming_projection_results + u64::from(visible));
    assert_eq!(metrics.agent_streaming_projection_installs, metrics_before.agent_streaming_projection_installs);
    assert_eq!(metrics.agent_streaming_projection_rejections, metrics_before.agent_streaming_projection_rejections);
    let messages = service.message_service().snapshot_state();
    assert_eq!(messages.next_sequence, sequence + 1);
    let receiver_cursor = messages.subscriptions.iter().find(|cursor| cursor.recipient == peer_agent).expect("receiver committed a delivery cursor");
    assert_eq!(receiver_cursor.last_sequence, sequence);
    let accepted: Vec<_> = messages.accepted_messages.iter().filter(|entry| entry.envelope.id == message_id).collect();
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0].envelope.payload, payload);
    assert_eq!(accepted[0].envelope.correlation_id.as_deref(), Some("noop-correlation"));
    let entries = store.inspect_presentation(&conversation).unwrap();
    let sent: Vec<_> = entries.iter().filter(|entry| entry.source_text.as_deref().is_some_and(|source| source.contains("accepted-noop-message"))).collect();
    assert_eq!(sent.len(), usize::from(visible));
    if let Some(sent) = sent.first() {
        assert_eq!(sent.presentation, original_entry, "sent persistence retains the first streamed occurrence, not settlement time");
        assert!(sent.source_text.as_deref().unwrap().contains("\"direction\":\"sent\""));
        assert_eq!(crate::runtime::render::peer_message_presentation_receive_identity(sent.source_content_type.as_deref().unwrap(), sent.source_text.as_deref().unwrap()), None);
    }
    let receiver_conversation = &service.agent_shell_store().get(peer_pane).unwrap().session_id;
    let received = store.inspect_presentation(receiver_conversation).unwrap();
    let received_ids: Vec<_> = received.iter().filter_map(|entry| {
        crate::runtime::render::peer_message_presentation_receive_identity(
            entry.source_content_type.as_deref()?, entry.source_text.as_deref()?,
        )
    }).collect();
    assert_eq!(received_ids.len(), usize::from(visible));
    if let Some(identity) = received_ids.first() {
        assert!(identity.contains(&message_id), "{identity}");
    }
    for _ in 0..2 {
        service.finalize_settled_outbound_message_previews("%1", &turn.turn_id, &execution).unwrap();
    }
    assert_eq!(service.message_service().snapshot_state(), messages, "presentation finalization cannot alter messages or cursors");
    assert_eq!(store.inspect_presentation(&conversation).unwrap(), entries);
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &displayed, "source/copy/time metadata must remain exact");
    service.append_agent_status_text_to_terminal_buffer("%1", "normal continuation").unwrap();
    assert!(service.agent_pane_screen("%1").unwrap().normal_content_lines().join("\n").contains("normal continuation"));
    service.complete_running_agent_turn_and_start_ready(&turn, AgentTurnState::Completed, "accepted message settled").unwrap();
    let next = service.start_agent_prompt_turn("%1", "next ordinary turn").unwrap();
    assert_ne!(next.turn_id, turn.turn_id);
    assert_eq!(service.message_service().next_message_sequence(), sequence + 1);
    service.terminate_all_pane_processes().unwrap();
}
