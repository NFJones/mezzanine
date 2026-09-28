//! Runtime tests for agent presentation logging behavior.

use super::*;

/// A validated progress say after a runtime-owned action waits for its log.
/// The action header is emitted by its executor, not the batch presenter.
#[test]
fn runtime_mixed_action_progress_waits_for_preceding_header() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
    );
    let action = mez_agent::AgentAction {
        id: "discovery".to_string(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: Some("subagent".to_string()),
            scope: Some("project".to_string()),
        },
    };
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture("turn-mixed-log"),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![
                    action.clone(),
                    mez_agent::AgentAction {
                        id: "progress".to_string(),
                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Progress,
                            text: "later progress".to_string(),
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                ],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: Vec::new(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    let before = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!before.contains("later progress"), "{before}");
    service
        .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
        .unwrap();
    let mut settled = execution.clone();
    settled.action_results.push(mez_agent::ActionResult {
        protocol: "maap/1".to_string(),
        turn_id: "turn-mixed-log".to_string(),
        agent_id: "agent-%1".to_string(),
        action_id: action.id.clone(),
        action_type: "list_agents",
        status: ActionStatus::Succeeded,
        content: Vec::new(),
        structured_content_json: None,
        permission_evaluation: None,
        is_error: false,
        error: None,
    });
    service
        .present_deferred_agent_say_actions_to_terminal_buffer("%1", &settled)
        .unwrap();
    let after = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(after.contains("list agents:"), "{after}");
    assert!(after.contains("later progress"), "{after}");
    assert!(
        after.find("list agents:").unwrap() < after.find("later progress").unwrap(),
        "{after}"
    );
    assert_eq!(after.matches("later progress").count(), 1, "{after}");
    settled.terminal_state = AgentTurnState::Completed;
    service
        .present_deferred_agent_say_actions_to_terminal_buffer("%1", &settled)
        .unwrap();
    let after_completion = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(
        after_completion.matches("later progress").count(),
        1,
        "{after_completion}"
    );
}

/// Executor traversal must not move a later discovery header past an
/// interleaved progress say whose predecessor has just settled.
#[test]
fn runtime_discovery_headers_preserve_interleaved_progress_order() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "discover peers")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
    );
    let discover = |id: &str| mez_agent::AgentAction {
        id: id.to_string(),
        payload: mez_agent::AgentActionPayload::ListAgents {
            agent_type: Some("subagent".to_string()),
            scope: Some("project".to_string()),
        },
    };
    let first = discover("first-discovery");
    let last = discover("last-discovery");
    let progress = mez_agent::AgentAction {
        id: "middle-progress".to_string(),
        payload: mez_agent::AgentActionPayload::Say {
            status: mez_agent::SayStatus::Progress,
            text: "middle response".to_string(),
            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: String::new(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: String::new(),
                actions: vec![first.clone(), progress.clone(), last.clone()],
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: Default::default(),
        action_results: [&first, &progress, &last]
            .into_iter()
            .map(|action| mez_agent::ActionResult::running(&turn, action, Vec::new(), None))
            .collect(),
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    service
        .present_agent_response_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    service
        .execute_running_list_agents_actions_for_turn(&turn, &mut execution)
        .unwrap();
    service
        .present_deferred_agent_say_actions_to_terminal_buffer("%1", &execution)
        .unwrap();
    let lines = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let first_row = lines.find("list agents:").unwrap();
    let progress_row = lines.find("middle response").unwrap();
    let last_row = lines.rfind("list agents:").unwrap();
    assert!(
        first_row < progress_row && progress_row < last_row,
        "{lines}"
    );
}

/// Issue and close-agent actions must release a later progress say after
/// their runtime-owned results settle, even when no static header exists.
#[test]
fn runtime_issue_and_close_headers_release_deferred_progress() {
    for (kind, payload, header) in [
        (
            "issue_query",
            mez_agent::AgentActionPayload::IssueQuery {
                kind: None,
                state: None,
                text: None,
                limit: None,
                refresh: false,
            },
            "issue query",
        ),
        (
            "close_agent",
            mez_agent::AgentActionPayload::CloseAgent {
                agent_id: "agent-%2".to_string(),
            },
            "close agent",
        ),
    ] {
        let mut service = test_runtime_service();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        set_agent_pane_screen_for_test(
            &mut service,
            "%1",
            TerminalScreen::new(Size::new(80, 16).unwrap(), 120).unwrap(),
        );
        let action = mez_agent::AgentAction {
            id: kind.to_string(),
            payload,
        };
        let mut execution = mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture("turn-deferred-kind"),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: String::new(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: String::new(),
                    actions: vec![
                        action.clone(),
                        mez_agent::AgentAction {
                            id: "progress".to_string(),
                            payload: mez_agent::AgentActionPayload::Say {
                                status: mez_agent::SayStatus::Progress,
                                text: "later progress".to_string(),
                                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                    .to_string(),
                            },
                        },
                    ],
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: Default::default(),
            action_results: Vec::new(),
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        };
        service
            .present_agent_response_actions_to_terminal_buffer("%1", &execution)
            .unwrap();
        assert!(
            !service
                .agent_pane_screen("%1")
                .unwrap()
                .normal_content_lines()
                .join("\n")
                .contains("later progress")
        );
        let has_header = service
            .append_agent_action_execution_text_to_terminal_buffer("%1", &action)
            .unwrap();
        execution.action_results.push(mez_agent::ActionResult {
            protocol: "maap/1".to_string(),
            turn_id: "turn-deferred-kind".to_string(),
            agent_id: "agent-%1".to_string(),
            action_id: action.id.clone(),
            action_type: kind,
            status: ActionStatus::Succeeded,
            content: Vec::new(),
            structured_content_json: None,
            permission_evaluation: None,
            is_error: false,
            error: None,
        });
        service
            .present_deferred_agent_say_actions_to_terminal_buffer("%1", &execution)
            .unwrap();
        let lines = service
            .agent_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        assert_eq!(lines.contains(header), has_header, "{kind}: {lines}");
        assert_eq!(
            lines.matches("later progress").count(),
            1,
            "{kind}: {lines}"
        );
        if has_header {
            assert!(
                lines.find(header).unwrap() < lines.find("later progress").unwrap(),
                "{kind}: {lines}"
            );
        }
    }
}

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
                rationale: "record the first sequence point".to_string(),

                actions: vec![mez_agent::AgentAction {
                    id: "say-progress".to_string(),

                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Progress,
                        text: "The redundant updates are coming from repeated progress says."
                            .to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                    },
                }],
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
                rationale: "record the owner".to_string(),

                actions: vec![mez_agent::AgentAction {
                    id: "say-progress-1".to_string(),

                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Progress,
                        text: first_progress.to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                    },
                }],
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
                rationale: duplicate_progress.to_string(),

                actions: vec![
                    mez_agent::AgentAction {
                        id: "say-progress-2".to_string(),

                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Progress,
                            text: duplicate_progress.to_string(),
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                    mez_agent::AgentAction {
                        id: "say-final".to_string(),

                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Final,
                            text: final_text.to_string(),
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                ],
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let executions = service
        .poll_agent_provider_tasks_with_provider(&second_provider, 1)
        .unwrap();
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0].terminal_state, AgentTurnState::Running);

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
