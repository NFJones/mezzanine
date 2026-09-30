//! Exact rationale/progress retention and rollback before rejected siblings.
//!
//! Accepted predecessors remain visible and durable once; provisional command
//! sources and stale screen generations cannot acquire that authority.

use super::*;

/// Verifies a matching streamed rationale survives when its command sibling
/// falls back to ordinary completion presentation.
///
/// The rationale must remain visible and durable exactly once, while the
/// unpromotable command preview is rolled back and never presented as executed.
#[test]
fn runtime_streaming_rationale_and_command_fallback_retains_only_rationale() {
    let mut streaming = test_runtime_service();
    let mut static_render = test_runtime_service();
    for service in [&mut streaming, &mut static_render] {
        service
            .attach_primary("primary", true, Size::new(48, 12).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        set_agent_pane_screen_for_test(
            service,
            "%1",
            TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
        );
        service
            .append_agent_status_text_to_terminal_buffer("%1", "baseline")
            .unwrap();
    }
    let transcript_store = AgentTranscriptStore::new(temp_root("streaming-rationale-fallback"));
    streaming.set_agent_transcript_store(transcript_store.clone());
    let conversation_id = streaming
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let rationale = "Inspect the current files";
    let command = "printf 'alpha beta\\n'";

    for event in [
        mez_agent::StreamingSayEvent::RationaleStarted,
        mez_agent::StreamingSayEvent::RationaleTextDelta {
            text: rationale.to_string(),
        },
        mez_agent::StreamingSayEvent::RationaleTextComplete,
        mez_agent::StreamingSayEvent::ShellCommandStarted { action_index: 0 },
        mez_agent::StreamingSayEvent::ShellCommandTextDelta {
            action_index: 0,
            text: command.to_string(),
        },
        mez_agent::StreamingSayEvent::ShellCommandTextComplete { action_index: 0 },
    ] {
        streaming
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
            .unwrap();
    }
    let work = streaming
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .expect("closed rationale and command source should project");
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    assert!(
        streaming
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    static_render
        .append_agent_thinking_text_to_terminal_buffer("%1", rationale)
        .unwrap();
    static_render
        .append_agent_command_preview_to_terminal_buffer("%1", command)
        .unwrap();
    assert_eq!(
        streaming
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines(),
        static_render
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines(),
        "completed provisional projection must match static display text"
    );
    assert_eq!(
        streaming
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines(),
        static_render
            .agent_pane_screen("%1")
            .unwrap()
            .normal_styled_content_lines(),
        "completed provisional projection must match static styling"
    );

    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: rationale.to_string(),

                actions: vec![mez_agent::AgentAction {
                    id: "shell-streamed".to_string(),

                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: rationale.to_string(),
                        command: command.to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    assert!(
        streaming
            .reconcile_agent_streaming_say_completion("%1", "turn-1", &execution)
            .unwrap()
            .is_empty()
    );
    let retained = streaming.agent_pane_screen("%1").unwrap().clone();
    let retained_lines = retained.normal_content_lines();
    assert!(retained_lines.iter().any(|line| line.contains(rationale)));
    assert!(retained_lines.iter().any(|line| line.contains("baseline")));
    assert!(!retained_lines.iter().any(|line| line.contains(command)));
    streaming
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    assert_eq!(streaming.agent_pane_screen("%1").unwrap(), &retained);
    assert_eq!(
        retained_lines
            .iter()
            .filter(|line| line.contains(rationale))
            .count(),
        1
    );
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.source_text.as_deref() == Some(rationale))
            .count(),
        1,
        "the fallback rationale must be persisted exactly once"
    );
}

/// A command that cannot promote must not roll back an exact earlier progress
/// say; both the rationale and say have already crossed validation and render.
#[test]
fn runtime_streaming_command_fallback_retains_matching_progress() {
    for rationale in [false, true] {
        for thinking in [false, true] {
            streaming_command_fallback_progress_case(rationale, thinking);
        }
    }
}

/// Exercises the exact progress predecessor with and without visible thinking.
fn streaming_command_fallback_progress_case(rationale: bool, thinking: bool) {
    let mut service = test_runtime_service();
    let store = AgentTranscriptStore::new(temp_root("streaming-command-progress-fallback"));
    service.set_agent_transcript_store(store.clone());
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    if !thinking {
        service
            .agent_shell_store_mut()
            .set_log_level("%1", AgentLogLevel::Normal)
            .unwrap();
    }
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
    );
    let say = mez_agent::AgentAction {
        id: "progress".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "matching progress".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let shell = mez_agent::AgentAction {
        id: "shell".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: String::new(),
            command: "printf fallback".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    if rationale {
        for event in [
            mez_agent::StreamingSayEvent::RationaleStarted,
            mez_agent::StreamingSayEvent::RationaleTextDelta {
                text: "matching rationale".to_string(),
            },
            mez_agent::StreamingSayEvent::RationaleTextComplete,
        ] {
            service
                .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
                .unwrap();
        }
    }
    for event in [
        mez_agent::StreamingSayEvent::Started {
            action_index: 0,
            status: mez_agent::SayStatus::Progress,
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
        mez_agent::StreamingSayEvent::TextDelta {
            action_index: 0,
            text: "matching progress".to_string(),
        },
        mez_agent::StreamingSayEvent::TextComplete { action_index: 0 },
        mez_agent::StreamingSayEvent::ShellCommandStarted { action_index: 1 },
        mez_agent::StreamingSayEvent::ShellCommandTextDelta {
            action_index: 1,
            text: "printf fallback".to_string(),
        },
        mez_agent::StreamingSayEvent::ShellCommandTextComplete { action_index: 1 },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
            .unwrap();
    }
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    service
        .apply_agent_streaming_say_projection_result(projection)
        .unwrap();
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: if rationale {
                    "matching rationale".to_string()
                } else {
                    String::new()
                },
                actions: vec![say.clone(), shell],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: vec![mez_agent::ActionResult {
            protocol: "maap/1".to_string(),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-%1".to_string(),
            action_id: say.id,
            action_type: "say",
            status: mez_agent::ActionStatus::Succeeded,
            content: Vec::new(),
            structured_content_json: None,
            permission_evaluation: None,
            is_error: false,
            error: None,
        }],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .reconcile_agent_streaming_say_completion("%1", "turn-1", &execution)
        .unwrap();
    let reconciled = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(
        reconciled.matches("matching progress").count(),
        1,
        "{reconciled}"
    );
    assert!(service.agent_streaming_say_action_is_promoted("%1", "turn-1", 0));
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(rows.matches("matching progress").count(), 1, "{rows}");
    assert!(!rows.contains("printf fallback"), "{rows}");
    let entries = store.inspect_presentation(&conversation_id).unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.source_text.as_deref() == Some("matching progress"))
            .count(),
        1,
        "{entries:?}"
    );
}

/// Verifies a sibling fallback cannot restore or promote streamed rationale
/// after an intervening pane generation takes ownership of the screen.
#[test]
fn runtime_streaming_rationale_fallback_preserves_stale_lineage_write() {
    let mut service = test_runtime_service();
    let transcript_store =
        AgentTranscriptStore::new(temp_root("streaming-rationale-stale-lineage"));
    service.set_agent_transcript_store(transcript_store.clone());
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
    );
    service
        .append_agent_status_text_to_terminal_buffer("%1", "baseline")
        .unwrap();
    let rationale = "owned only while its screen lineage remains current";
    let command = "printf 'stale sibling'";
    for event in [
        mez_agent::StreamingSayEvent::RationaleStarted,
        mez_agent::StreamingSayEvent::RationaleTextDelta {
            text: rationale.to_string(),
        },
        mez_agent::StreamingSayEvent::RationaleTextComplete,
        mez_agent::StreamingSayEvent::ShellCommandStarted { action_index: 0 },
        mez_agent::StreamingSayEvent::ShellCommandTextDelta {
            action_index: 0,
            text: command.to_string(),
        },
        mez_agent::StreamingSayEvent::ShellCommandTextComplete { action_index: 0 },
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
            .unwrap();
    }
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .expect("completed rationale and command should be projected");
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );

    let mut intervening = service.agent_pane_screen("%1").unwrap().clone();
    intervening.feed(b"\r\nnewer pane owner\r\n");
    service.set_agent_pane_screen("%1".to_string(), conversation_id.clone(), intervening);
    let intervening = service.agent_pane_screen("%1").unwrap().clone();
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: rationale.to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "stale-shell".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: rationale.to_string(),
                        command: command.to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };

    service
        .reconcile_agent_streaming_say_completion("%1", "turn-1", &execution)
        .unwrap();
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &intervening);
    assert!(!service.agent_streaming_rationale_is_promoted("%1", "turn-1"));
    assert!(
        transcript_store
            .inspect_presentation(&conversation_id)
            .unwrap()
            .iter()
            .all(|entry| entry.source_text.as_deref() != Some(rationale))
    );
    let text = intervening.normal_content_lines().join("\n");
    assert!(text.contains("newer pane owner"), "{text}");
}

/// Verifies validated rationale-only output remains on the pane through
/// completion instead of briefly returning to the pre-stream baseline.
#[tokio::test]
async fn runtime_streaming_rationale_only_keeps_visible_generation() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("streaming-rationale-complete"));
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .attach_primary("primary", true, Size::new(48, 12).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let started = service
        .start_agent_prompt_turn("%1", "finish the rationale")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
    );
    for event in [
        mez_agent::StreamingSayEvent::RationaleStarted,
        mez_agent::StreamingSayEvent::RationaleTextDelta {
            text: "Inspect validated output".to_string(),
        },
        mez_agent::StreamingSayEvent::RationaleTextComplete,
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", &turn.turn_id, &event)
            .unwrap();
    }
    let work = service
        .take_agent_streaming_say_projection_work("%1", &turn.turn_id)
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    let visible = service.agent_pane_screen("%1").unwrap().clone();
    assert!(
        visible
            .normal_content_lines()
            .join("\n")
            .contains("Inspect validated output")
    );

    let action = mez_agent::AgentAction {
        id: "complete".to_string(),
        payload: mez_agent::AgentActionPayload::Complete,
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "Inspect validated output".to_string(),
                actions: vec![action.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![mez_agent::ActionResult::succeeded(
            &turn,
            &action,
            vec!["turn complete".to_string()],
            Some(r#"{"complete":true}"#.to_string()),
        )],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };
    let transition = service
        .apply_agent_provider_completed_transition(
            &AgentId::opaque(turn.agent_id.clone()).unwrap(),
            &turn.turn_id,
            execution,
        )
        .await
        .unwrap();
    assert!(transition.applied);
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines();
    assert_eq!(
        rows.iter()
            .filter(|line| line.contains("Inspect validated output"))
            .count(),
        1
    );
    assert!(
        rows.iter()
            .any(|line| line.contains("thinking: Inspect validated output"))
    );
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.source_text.as_deref() == Some("Inspect validated output"))
            .count(),
        1
    );
    assert!(
        visible
            .normal_content_lines()
            .join("\n")
            .contains("Inspect validated output")
    );
}

/// Verifies a mismatched completed rationale is not retained or marked as an
/// accepted action: only the authoritative text may replace provisional rows.
#[test]
fn runtime_streaming_rationale_mismatch_restores_baseline() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(48, 12).unwrap(), 120).unwrap(),
    );
    service
        .append_agent_status_text_to_terminal_buffer("%1", "baseline")
        .unwrap();
    let baseline = service.agent_pane_screen("%1").unwrap().clone();
    for event in [
        mez_agent::StreamingSayEvent::RationaleStarted,
        mez_agent::StreamingSayEvent::RationaleTextDelta {
            text: "unvalidated rationale".to_string(),
        },
        mez_agent::StreamingSayEvent::RationaleTextComplete,
    ] {
        service
            .apply_agent_streaming_say_event_to_terminal_buffer("%1", "turn-1", &event)
            .unwrap();
    }
    let work = service
        .take_agent_streaming_say_projection_work("%1", "turn-1")
        .unwrap()
        .unwrap();
    let projection = RuntimeSessionService::build_agent_streaming_say_projection(work).unwrap();
    assert!(
        service
            .apply_agent_streaming_say_projection_result(projection)
            .unwrap()
    );
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-1"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "authoritative rationale".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "complete".to_string(),
                    payload: mez_agent::AgentActionPayload::Complete,
                }],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    assert!(
        service
            .reconcile_agent_streaming_say_completion("%1", "turn-1", &execution)
            .unwrap()
            .is_empty()
    );
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &baseline);
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    let rows = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(rows.contains("authoritative rationale"), "{rows}");
    assert!(!rows.contains("unvalidated rationale"), "{rows}");
}
