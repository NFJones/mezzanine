//! Runtime tests for agent presentation logging behavior.

use super::*;

/// Verifies progress `say` messages continue through durable assistant
/// chronology without a request-local ledger.
///
/// Progress text is already an assistant event at its occurrence boundary.
/// Replaying a second controller-generated copy would duplicate information and
/// invalidate the reusable prefix.
#[test]
fn runtime_progress_say_chronology_reaches_provider_continuation_without_ledger() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-progress-ledger","input":"fix the repeated progress updates"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let first_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "record the first sequence point".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "say-progress".to_string(),
                    rationale: "tell the user the owner changed".to_string(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Progress,
                        text: "The redundant updates are coming from repeated progress says."
                            .to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let first_execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &first_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(first_execution.terminal_state, AgentTurnState::Running);
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    let assistant_block = context
        .blocks()
        .iter()
        .find(|block| block.source == ContextSourceKind::TranscriptAssistant)
        .expect("progress say should be preserved as assistant chronology");
    assert_eq!(
        assistant_block.placement,
        mez_agent::ContextPlacement::ConversationAppend
    );
    assert!(
        assistant_block
            .content
            .contains("The redundant updates are coming from repeated progress says."),
        "{}",
        assistant_block.content
    );
    assert!(context.validate_durable().is_ok());
    assert!(!context.blocks().iter().any(|block| {
        block.label.contains("ledger") || block.content.contains("progress_say:")
    }));

    let second_provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch("turn-1")),
            provider_transcript_events: Vec::new(),
        },
        last_request: RefCell::new(None),
    };
    let executions = service
        .poll_agent_provider_tasks_with_provider(&second_provider, 1)
        .unwrap();

    assert_eq!(executions.len(), 1);
    let request = second_provider.last_request.borrow().clone().unwrap();
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::TranscriptAssistant
            && message
                .content
                .contains("The redundant updates are coming from repeated progress says.")
    }));
    assert!(!request.messages.iter().any(|message| {
        message
            .content
            .contains("[current-turn progress say ledger]")
            || message.content.contains("progress_say:")
    }));
    assert!(!service.agent_turn_contexts().contains_key("turn-1"));
}

/// Verifies runtime keeps repeated progress `say` updates visible during a turn.
///
/// Progress messages are user-visible sequence points, and repeated provider
/// updates should still render as ordinary progress output instead of being
/// silently transformed into a suppression marker.
#[test]
fn runtime_agent_keeps_redundant_progress_say_updates_visible() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 8).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-redundant-progress","input":"fix the selector"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let first_progress = "The selector bug is in the real resume pager path.";
    let first_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "progress".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "record the owner".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "say-progress-1".to_string(),
                    rationale: "tell the user the selector owner".to_string(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Progress,
                        text: first_progress.to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let first_execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &first_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(first_execution.terminal_state, AgentTurnState::Running);

    let duplicate_progress = "The surviving selector bug is still in the real resume pager path.";
    let final_text = "The fix is complete.";
    let second_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: duplicate_progress.to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![
                    mez_agent::AgentAction {
                        id: "say-progress-2".to_string(),
                        rationale: "repeat the selector owner".to_string(),
                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Progress,
                            text: duplicate_progress.to_string(),
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                    mez_agent::AgentAction {
                        id: "say-final".to_string(),
                        rationale: "finish the reply".to_string(),
                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Final,
                            text: final_text.to_string(),
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                ],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let executions = service
        .poll_agent_provider_tasks_with_provider(&second_provider, 1)
        .unwrap();
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0].terminal_state, AgentTurnState::Completed);

    let pane_text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains(first_progress), "{pane_text}");
    assert!(pane_text.contains(duplicate_progress), "{pane_text}");
    assert!(pane_text.contains(final_text), "{pane_text}");
    assert!(
        executions[0]
            .action_results
            .iter()
            .any(|result| result.action_id == "say-progress-2" && !result.is_error),
        "{:?}",
        executions[0].action_results
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies agent presentation appended while its surface is hidden never
/// changes the process terminal and remains available for later reentry.
#[test]
fn runtime_hidden_agent_presentation_isolated_from_process_screen() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut process_screen = TerminalScreen::new(Size::new(80, 24).unwrap(), 120).unwrap();
    process_screen.feed(b"process-only sentinel\r\n");
    service.set_process_pane_screen("%1", process_screen);
    service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap();

    service
        .append_agent_status_text_to_terminal_buffer("%1", "hidden agent sentinel")
        .unwrap();

    let process_text = service
        .process_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let agent_text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        process_text.contains("process-only sentinel"),
        "{process_text}"
    );
    assert!(
        !process_text.contains("hidden agent sentinel"),
        "{process_text}"
    );
    assert!(agent_text.contains("hidden agent sentinel"), "{agent_text}");
    assert!(
        !agent_text.contains("process-only sentinel"),
        "{agent_text}"
    );
}

/// Verifies sandbox mapping warnings are visible without verbose mode and one
/// stable mapping outcome is retained only once in the affected pane log.
#[test]
fn runtime_sandbox_mapping_warning_is_visible_and_deduplicated() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap();

    for _ in 0..2 {
        service
            .append_sandbox_mapping_warning_once(
                "%1",
                "supplementary-group:docker:not-active",
                "supplementary-group `docker` (not active in the pane shell)",
            )
            .unwrap();
    }

    let pane_text = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(
        pane_text.matches("agent warning:").count(),
        1,
        "{pane_text}"
    );
    assert!(pane_text.contains("sandbox remains active"), "{pane_text}");
}

/// Verifies persisted presentation from another pane conversation is rejected
/// before replay can mutate either retained screen.
#[test]
fn runtime_agent_presentation_replay_rejects_mismatched_conversation() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut process_screen = TerminalScreen::new(Size::new(80, 24).unwrap(), 120).unwrap();
    process_screen.feed(b"process replay sentinel\r\n");
    service.set_process_pane_screen("%1", process_screen);
    service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "current-conversation", 0)
        .unwrap();
    service
        .append_agent_status_text_to_terminal_buffer("%1", "current agent sentinel")
        .unwrap();
    let process_before = service.process_pane_screen("%1").unwrap().clone();
    let agent_before = service.agent_pane_screen("%1").unwrap().clone();
    let stale = crate::storage::transcript::AgentPresentationEntry {
        conversation_id: "stale-conversation".to_string(),
        sequence: 1,
        created_at_unix_seconds: 1,
        pane_id: "%1".to_string(),
        turn_id: None,
        terminal_width: 80,
        style_names: vec!["assistant".to_string()],
        display_lines: vec!["stale presentation sentinel".to_string()],
        copy_lines: vec!["stale presentation sentinel".to_string()],
        ansi_text: None,
        source_text: Some("stale presentation sentinel".to_string()),
        source_content_type: Some(mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string()),
    };

    let error = service
        .replay_agent_presentation_entries_to_terminal_buffer("%1", &[stale])
        .unwrap_err();

    assert!(error.message().contains("active conversation"), "{error}");
    assert_eq!(service.process_pane_screen("%1").unwrap(), &process_before);
    assert_eq!(service.agent_pane_screen("%1").unwrap(), &agent_before);
}
