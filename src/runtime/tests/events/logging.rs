//! Runtime tests for events logging behavior.

use super::*;

/// Verifies that model-authored thinking text is not rendered a second time
/// when another action in the same response already presents the same text as
/// a `say` action. Models commonly emit a short `say` plus a matching
/// batch-level `thinking:` rationale; the pane should show the user-visible
/// answer once rather than adding a grey duplicate.
#[test]
fn runtime_agent_suppresses_batch_rationale_that_duplicates_say_text() {
    let mut service = test_runtime_service();
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

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-duplicate-thinking","input":"respond once"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let visible = "I will handle the next step.";
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap say and complete response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: format!("thinking: {visible}"),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: String::new(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: visible.to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(pane_text.matches(visible).count(), 1, "{pane_text}");
    assert!(
        pane_text.contains(&format!("mez> {visible}")),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that `/log-level verbose` is an explicit opt-in for low-level agent lifecycle
/// chatter. Normal mode keeps the pane buffer focused on prompts, assistant
/// text, concise progress, and errors; verbose mode restores provider,
/// protocol, command, and command-output diagnostics for debugging without
/// enabling thinking.
#[test]
fn runtime_agent_verbose_mode_injects_low_level_status_lines() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let verbose = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"verbose","method":"agent/shell/command","params":{"idempotency_key":"agent-verbose","input":"/log-level verbose"}}"#,
        &primary,
    );
    assert!(
        verbose.contains("agent log level for pane %1 is now verbose."),
        "{verbose}"
    );

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-verbose-say","input":"summarize visible output"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap say response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: "answer in the pane".to_string(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: "The pane is ready.".to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: thinking with runtime-batch model test"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("mez> answer in the pane"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent: turn turn-1 completed"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that `/log-level debug` exposes model introspection and action
/// rationales while still hiding the full shell view that verbose and trace show.
#[test]
fn runtime_agent_thinking_mode_injects_action_rationales() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let thinking = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"thinking","method":"agent/shell/command","params":{"idempotency_key":"agent-thinking","input":"/log-level debug"}}"#,
        &primary,
    );
    assert!(
        thinking.contains("agent log level for pane %1 is now debug."),
        "{thinking}"
    );

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-thinking-say","input":"summarize visible output"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap say response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: "answer in the pane".to_string(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: "The pane is ready.".to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent debug: turn turn-1: MAAP action_results"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("mez> answer in the pane"),
        "{pane_text}"
    );
    assert!(pane_text.contains("mez> The pane is ready."), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that trace mode exposes the full MAAP exchange in the pane buffer:
/// the model request messages, raw provider response with the parsed action
/// batch, and action results. Summary-only tracing made auto-allow/full-access
/// hangs difficult to diagnose because the user could not copy the actual MAAP
/// messages that drove the state machine.
#[test]
fn runtime_agent_trace_mode_prints_maap_request_response_and_results() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(100, 16).unwrap(), 500).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Trace)
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-trace-maap","input":"trace maap please"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "trace-maap-raw-response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: "show trace details".to_string(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: "Trace visible.".to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent trace: turn turn-1: MAAP request"),
        "{pane_text}"
    );
    assert!(pane_text.contains(r#""role": "user""#), "{pane_text}");
    assert!(pane_text.contains("trace maap please"), "{pane_text}");
    assert!(
        pane_text.contains("agent trace: turn turn-1: MAAP response"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains(r#""raw_text": "trace-maap-raw-response""#),
        "{pane_text}"
    );
    assert!(pane_text.contains(r#""action_batch""#), "{pane_text}");
    assert!(pane_text.contains(r#""type": "say""#), "{pane_text}");
    assert!(
        pane_text.contains("agent trace: turn turn-1: MAAP action_results"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains(r#""status": "succeeded""#),
        "{pane_text}"
    );
    assert!(pane_text.contains(r#""structured_content""#), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that `/log-level debug` exposes MAAP and state-machine diagnostics
/// without exposing the raw shell view. Debug should show the same diagnostic
/// categories as trace and preserve command fields inside MAAP objects, while
/// raw provider text and output previews stay hidden until the pane is
/// explicitly moved to trace.
#[test]
fn runtime_agent_debug_mode_prints_maap_without_shell_view() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Debug)
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-debug-maap","input":"debug maap please"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "debug-maap-raw-response with debug-secret-command".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "run a command for debug redaction".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Run a debug redaction command".to_string(),
                        command: "printf 'debug-secret-command\\n'".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent debug: turn turn-1: MAAP response"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent debug: turn turn-1: MAAP request"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("hidden at debug log level; use /log-level trace"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains(r#""command": "printf 'debug-secret-command\\n'""#),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("debug-maap-raw-response"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("$ printf 'debug-secret-command"),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies repeated same-turn investigative rationale is suppressed after the
/// first provider continuation records it in the active-turn rationale ledger.
///
/// The model should not keep replaying short owner-localization intent such as
/// "check exact selector owner" on later continuations when the reason has not
/// materially changed.
#[test]
fn runtime_agent_suppresses_redundant_same_turn_rationale() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-rationale-ledger","method":"agent/shell/command","params":{"idempotency_key":"agent-rationale-ledger","input":"fix the selector"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let first_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "owner located".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "Check exact selector owner".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "say-progress".to_string(),
                    rationale: "tell the user the owner is narrowed".to_string(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Progress,
                        text: "The selector owner is narrowed to the resume path.".to_string(),
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

    let second_provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "Check exact selector owner".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "say-final".to_string(),
                    rationale: "finish the user reply".to_string(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: "The selector fix is complete.".to_string(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        last_request: RefCell::new(None),
    };
    let executions = service
        .poll_agent_provider_tasks_with_provider(&second_provider, 1)
        .unwrap();
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0].terminal_state, AgentTurnState::Completed);

    let request = second_provider.last_request.borrow().clone().unwrap();
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::RuntimeHint
            && message.content.contains("[current-turn rationale ledger]")
            && message
                .content
                .contains("rationale: Check exact selector owner")
    }));

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(
        pane_text
            .matches("thinking: Check exact selector owner")
            .count(),
        1
    );
    assert!(
        pane_text.contains("The selector fix is complete."),
        "{pane_text}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies batch thoughts are durable context notes, not normal-mode pane
/// chatter.
///
/// A model can emit a longer `thought` when that note should help future turns,
/// but normal users should not see that long-form internal context in routine
/// logs. Verbose-or-higher logs still render it as `thinking:` text for
/// diagnostics.
#[test]
fn runtime_batch_thought_is_hidden_until_verbose_logging() {
    fn pane_text_after_thought_response(level: AgentLogLevel) -> String {
        let mut service = test_runtime_service();
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        let mut screen = TerminalScreen::new(Size::new(80, 10).unwrap(), 30).unwrap();
        screen.feed(b"ready\n");
        service.pane_screens.insert("%1".to_string(), screen);
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service
            .agent_shell_store_mut()
            .set_log_level("%1", level)
            .unwrap();
        let start = service.dispatch_runtime_control_body(
            r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-thought-display","input":"respond with durable context"}}"#,
            &primary,
        );
        assert!(start.contains(r#""state":"running""#), "{start}");
        let provider = RuntimeBatchProvider {
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "done".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "respond with the final message".to_string(),
                    thought: Some(
                        "The durable note should only be visible in verbose logs.".to_string(),
                    ),
                    turn_id: "turn-1".to_string(),
                    agent_id: "agent-%1".to_string(),
                    actions: vec![mez_agent::AgentAction {
                        id: "say-final".to_string(),
                        rationale: String::new(),
                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Final,
                            text: "Done.".to_string(),
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    }],
                    final_turn: true,
                }),
                provider_transcript_events: Vec::new(),
            },
        };
        service
            .execute_agent_turn_with_provider(
                "turn-1",
                &provider,
                runtime_model_profile("runtime-batch", "test"),
            )
            .unwrap();
        let pane_text = service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        service.pane_processes_mut().terminate_all().unwrap();
        pane_text
    }

    let normal_text = pane_text_after_thought_response(AgentLogLevel::Normal);
    assert!(
        normal_text.contains("thinking: respond with the final message"),
        "{normal_text}"
    );
    assert!(!normal_text.contains("durable note"), "{normal_text}");
    assert!(normal_text.contains("Done."), "{normal_text}");

    let verbose_text = pane_text_after_thought_response(AgentLogLevel::Verbose);
    assert!(
        verbose_text.contains("thinking: respond with the final message"),
        "{verbose_text}"
    );
    assert!(
        verbose_text.contains("thinking: The durable note should only be visible"),
        "{verbose_text}"
    );
    assert!(verbose_text.contains("Done."), "{verbose_text}");
}
