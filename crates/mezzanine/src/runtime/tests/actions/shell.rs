//! Runtime tests for actions shell behavior.

use super::*;

/// Verifies runtime control dispatches agent shell command for visible shell.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_control_dispatches_agent_shell_command_for_visible_shell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::ToggleAgentShell,
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-command","method":"agent/shell/command","params":{"idempotency_key":"agent-status","input":"/status"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains("| Visibility | visible |"), "{response}");
    assert!(response.contains(r#""turn":null"#), "{response}");

    let alias_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-command-alias","method":"agent/shell/command","params":{"idempotency_key":"agent-command-alias","command":"/status"}}"#,
        &primary,
    );
    assert!(
        alias_response.contains(r#""mezzanine_code":"invalid_params""#),
        "{alias_response}"
    );
    assert!(
        alias_response.contains("agent/shell/command params contains unknown field `command`"),
        "{alias_response}"
    );
}

/// Verifies that normal mode renders shell commands selected by the agent into
/// the same pane terminal buffer before they are sent to the PTY. Users should
/// be able to monitor the exact command stream without enabling raw shell
/// output or wrapper diagnostics.
#[test]
fn runtime_agent_shell_command_is_presented_before_pty_dispatch() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-visible-command","input":"run a harmless command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "check shell access".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: String::new(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Check shell access".to_string(),
                        command: "if true; then echo \"ok\"; fi".to_string(),
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
        !pane_text.contains("mez> Check shell access"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("agent: Check shell access"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("thinking: check shell access"),
        "{pane_text}"
    );
    assert_eq!(
        pane_text.matches("$ if true; then echo \"ok\"; fi").count(),
        1
    );
    let command_line = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("$ if true; then echo \"ok\"; fi"))
        .unwrap();
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    assert!(command_line.style_spans.iter().any(|span| {
        span.start >= 2
            && span.rendition.foreground.is_some_and(|foreground| {
                foreground != theme.colors.agent_transcript_command.foreground
            })
    }));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies hidden model shell commands expose a bounded live latest-output tail.
///
/// Normal logging hides raw PTY output, but users still need lightweight
/// progress for long-running commands. The latest cleaned stdout/stderr lines
/// should replace the previous transient preview block and disappear when the
/// next durable agent transcript line is written.
#[test]
fn runtime_hidden_model_shell_command_shows_transient_latest_output_line() {
    let mut service = test_runtime_service();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 20).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .start_agent_prompt_turn("%1", "run a command")
        .unwrap();
    assert_eq!(start.state, AgentTurnState::Running);
    service.remove_pending_agent_provider_task("turn-1");
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .cloned()
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "shell-1".to_string(),
        rationale: "run a command".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Run a command".to_string(),
            command: "sleep 1".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    service.agent_turn_executions_mut().insert(
        "turn-1".to_string(),
        mez_agent::AgentTurnExecution {
            request: mez_agent::ModelRequest {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                reasoning_effort: None,
                thinking_enabled: None,
                latency_preference: None,
                prompt_cache_retention: None,
                max_output_tokens: None,
                temperature: None,
                stop: None,
                prompt_cache_session_id: None,
                prompt_cache_lineage_id: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
                issue_actions_enabled: true,
                interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
                allowed_actions: mez_agent::AllowedActionSet::for_capability(
                    mez_agent::AgentCapability::Shell,
                ),
                recovery_input: None,
                messages: vec![mez_agent::ModelMessage {
                    role: mez_agent::ModelMessageRole::User,
                    source: ContextSourceKind::UserInstruction,
                    placement: mez_agent::ContextPlacement::ConversationAppend,
                    content: "run a command".to_string(),
                }]
                .into(),
            },
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "run shell".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "test action batch rationale".to_string(),
                    thought: None,
                    turn_id: "turn-1".to_string(),
                    agent_id: "agent-%1".to_string(),
                    actions: vec![action.clone()],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::running(
                &turn,
                &action,
                vec!["shell command accepted for pane execution".to_string()],
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    service
        .append_agent_command_preview_to_terminal_buffer("%1", "sleep 1")
        .unwrap();
    service.running_shell_transactions_mut_for_tests().insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "shell-1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "sleep 1".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    service.record_running_shell_transaction_output("%1", b"first output\n");
    let first_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(first_text.contains("first output"), "{first_text}");

    service
        .update_agent_shell_output_preview(
            "%1",
            crate::runtime::render::RuntimeAgentShellPreviewOwner {
                turn_id: "turn-1".to_string(),
                action_id: "shell-1".to_string(),
                marker: "marker-1".to_string(),
            },
            1,
            &["first output".to_string()],
        )
        .unwrap();
    let unchanged_preview_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(unchanged_preview_text, first_text);

    service.record_running_shell_transaction_output("%1", b"second output\n");
    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let second_text = styled_lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(second_text.contains("first output"), "{second_text}");
    assert!(second_text.contains("second output"), "{second_text}");
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let output_line = styled_lines
        .iter()
        .find(|line| line.text.contains("second output"))
        .unwrap();
    assert!(
        output_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground
                    == Some(theme.colors.agent_transcript_status.foreground)
                && span.rendition.dim
        }),
        "transient shell output should use muted status/thinking style: {:?}",
        output_line.style_spans
    );

    let encoded_tail =
        base64::engine::general_purpose::STANDARD.encode(b"decoded transported output\n");
    let transported_tail = format!(
        "__MEZ_SHELL_OUTPUT_BASE64_BEGIN__\n{encoded_tail}\n__MEZ_SHELL_OUTPUT_BASE64_END__\n"
    );
    service.record_running_shell_transaction_output("%1", transported_tail.as_bytes());
    let decoded_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        decoded_text.contains("decoded transported output"),
        "{decoded_text}"
    );
    assert!(
        !decoded_text.contains("__MEZ_SHELL_OUTPUT_BASE64_BEGIN__"),
        "{decoded_text}"
    );

    service.record_running_shell_transaction_output(
        "%1",
        b"final output\n\x1b]133;D;0;mez_marker=marker-1;mez_turn=turn-1;mez_agent=agent-%1;mez_pane=%1\x1b\\\r\n~/repo > ",
    );
    let final_output_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        final_output_text.contains("final output"),
        "{final_output_text}"
    );
    assert!(
        !final_output_text.contains("~/repo >"),
        "{final_output_text}"
    );
    assert!(
        !final_output_text
            .lines()
            .any(|line| line.trim_end().ends_with(">") && !line.contains("final output")),
        "{final_output_text}"
    );

    service.record_running_shell_transaction_output("%1", b"~/repo > ");
    let prompt_tail_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        prompt_tail_text.contains("final output"),
        "{prompt_tail_text}"
    );
    assert!(!prompt_tail_text.contains("~/repo >"), "{prompt_tail_text}");

    service
        .append_agent_status_text_to_terminal_buffer("%1", "agent: next stage")
        .unwrap();
    let final_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!final_text.contains("second output"), "{final_text}");
    assert!(final_text.contains("agent: next stage"), "{final_text}");
}

/// Verifies concurrent shell previews retain actor chronology and durable rows.
///
/// Each running action owns an independent preview slot. Updating one owner
/// must preserve first-seen ordering, stale revisions must be ignored, and a
/// durable diff appended between updates must survive later projection and
/// owner-specific retirement.
#[test]
fn runtime_shell_previews_preserve_owner_order_and_durable_output() {
    let mut service = test_runtime_service();
    let mut screen = TerminalScreen::new(Size::new(100, 20).unwrap(), 40).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let owner_a = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-a".to_string(),
        action_id: "action-a".to_string(),
        marker: "marker-a".to_string(),
    };
    let owner_b = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-b".to_string(),
        action_id: "action-b".to_string(),
        marker: "marker-b".to_string(),
    };

    service
        .update_agent_shell_output_preview(
            "%1",
            owner_a.clone(),
            1,
            &["shared owner output".to_string()],
        )
        .unwrap();
    service
        .update_agent_shell_output_preview(
            "%1",
            owner_b.clone(),
            1,
            &["shared owner output".to_string()],
        )
        .unwrap();
    let identical_previews = service.agent_shell_output_previews_for_tests("%1");
    assert_eq!(identical_previews.len(), 2, "{identical_previews:?}");
    assert_ne!(identical_previews[0].0, identical_previews[1].0);
    assert_eq!(identical_previews[0].3, identical_previews[1].3);

    service
        .update_agent_shell_output_preview(
            "%1",
            owner_a.clone(),
            2,
            &["owner A revision 2".to_string()],
        )
        .unwrap();
    service
        .update_agent_shell_output_preview(
            "%1",
            owner_a.clone(),
            1,
            &["stale owner A revision".to_string()],
        )
        .unwrap();

    let previews = service.agent_shell_output_previews_for_tests("%1");
    assert_eq!(previews.len(), 2, "{previews:?}");
    assert_eq!(
        previews[0],
        (
            owner_a.clone(),
            0,
            2,
            vec!["owner A revision 2".to_string()]
        )
    );
    assert_eq!(
        previews[1],
        (
            owner_b.clone(),
            1,
            1,
            vec!["shared owner output".to_string()]
        )
    );

    service
        .append_agent_diff_text_to_terminal_buffer(
            "%1",
            "diff --git a/note.txt b/note.txt\n--- a/note.txt\n+++ b/note.txt\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
    service
        .update_agent_shell_output_preview(
            "%1",
            owner_b.clone(),
            2,
            &["owner B revision 2".to_string()],
        )
        .unwrap();
    assert!(service.settle_agent_shell_output_preview("%1", &owner_a));
    service
        .update_agent_shell_output_preview(
            "%1",
            owner_a.clone(),
            3,
            &["post-settlement owner A revision".to_string()],
        )
        .unwrap();
    assert_eq!(
        service.agent_shell_output_previews_for_tests("%1")[0].3,
        vec!["owner A revision 2".to_string()]
    );
    service
        .append_agent_status_text_to_terminal_buffer("%1", "agent: next stage")
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("+++ note.txt"), "{pane_text}");
    assert!(pane_text.contains("+new"), "{pane_text}");
    assert!(!pane_text.contains("owner A revision"), "{pane_text}");
    assert!(pane_text.contains("owner B revision 2"), "{pane_text}");
    assert!(!pane_text.contains("stale owner A revision"), "{pane_text}");
    let diff_index = pane_text.find("+++ note.txt").unwrap();
    let preview_index = pane_text.find("owner B revision 2").unwrap();
    assert!(diff_index < preview_index, "{pane_text}");
}

/// Verifies aggregate turn cancellation retires only the matching owners.
///
/// Cancellation may cover managed or native work and therefore operates at the
/// turn scope. Other turns sharing the pane must remain projected in their
/// original first-seen order, and durable rows must remain untouched.
#[test]
fn runtime_shell_preview_turn_retirement_preserves_other_turns() {
    let mut service = test_runtime_service();
    let screen = TerminalScreen::new(Size::new(100, 20).unwrap(), 40).unwrap();
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .append_agent_status_text_to_terminal_buffer("%1", "durable sentinel")
        .unwrap();
    let owner_a = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-a".to_string(),
        action_id: "action-a".to_string(),
        marker: "marker-a".to_string(),
    };
    let owner_b = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-b".to_string(),
        action_id: "action-b".to_string(),
        marker: "marker-b".to_string(),
    };
    service
        .update_agent_shell_output_preview("%1", owner_a, 1, &["cancelled turn output".to_string()])
        .unwrap();
    service
        .update_agent_shell_output_preview(
            "%1",
            owner_b.clone(),
            1,
            &["surviving turn output".to_string()],
        )
        .unwrap();

    assert_eq!(
        service
            .retire_agent_shell_output_previews_for_turn("turn-a")
            .unwrap(),
        1
    );

    let previews = service.agent_shell_output_previews_for_tests("%1");
    assert_eq!(previews.len(), 1, "{previews:?}");
    assert_eq!(previews[0].0, owner_b);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("durable sentinel"), "{pane_text}");
    assert!(!pane_text.contains("cancelled turn output"), "{pane_text}");
    assert!(pane_text.contains("surviving turn output"), "{pane_text}");
}

/// Verifies stale preview cleanup cannot erase an intervening pane generation.
///
/// If a write outside the preview compositor changes the retained screen, the
/// remembered installed generation no longer owns the pane. Cleanup must then
/// discard only stale metadata and leave the intervening row untouched.
#[test]
fn runtime_shell_preview_cleanup_requires_exact_screen_lineage() {
    let mut service = test_runtime_service();
    let mut screen = TerminalScreen::new(Size::new(100, 20).unwrap(), 40).unwrap();
    screen.feed(b"ready\n");
    service.set_pane_screen("%1".to_string(), screen);
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let owner = crate::runtime::render::RuntimeAgentShellPreviewOwner {
        turn_id: "turn-a".to_string(),
        action_id: "action-a".to_string(),
        marker: "marker-a".to_string(),
    };
    service
        .update_agent_shell_output_preview("%1", owner, 1, &["live preview".to_string()])
        .unwrap();
    let mut intervening = service.agent_pane_screen("%1").unwrap().clone();
    intervening.feed(b"\r\nintervening durable row\r\n");
    service.set_agent_pane_screen("%1", conversation_id, intervening);

    service.clear_agent_shell_output_status_line("%1").unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("intervening durable row"), "{pane_text}");
    assert!(
        service
            .agent_shell_output_previews_for_tests("%1")
            .is_empty()
    );
}

/// Verifies that a shell command selected by the model is monitorable when
/// verbose mode is enabled: the command line is injected before dispatch and
/// transaction output can settle without exposing wrapper internals.
#[test]
fn runtime_agent_shell_command_output_is_visible_in_verbose_mode() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Verbose)
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-visible-output","input":"print a marker"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
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
                    rationale: "print a marker".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Print a marker".to_string(),
                        command: "printf 'agent-visible-%s\\n' output".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: None,
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

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    for _ in 0..900 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service.running_shell_transactions_for_tests().is_empty() {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
        thread::yield_now();
    }
    assert!(
        service.running_shell_transactions_for_tests().is_empty(),
        "agent shell command should settle before checking verbose presentation: transactions={:?} pane={}",
        service.running_shell_transactions_for_tests(),
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("$ printf 'agent-visible-%s"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("MEZ_MARKER_TOKEN"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_STATUS"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_COMMAND_"), "{pane_text}");
    assert!(!pane_text.contains("unset MEZ_MARKER_TOKEN"), "{pane_text}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies native shell output reaches the transient pane tail before completion.
///
/// Native execution drains stdout and stderr outside the pane PTY. A command
/// that emits one line and then sleeps must publish that line through the
/// worker progress relay while its action result is still running. Progress
/// carrying a stale marker must be rejected without changing the pane.
#[test]
fn runtime_native_agent_shell_command_shows_transient_output_before_completion() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-native-live-output","input":"print native progress"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let release_path = std::env::temp_dir().join(format!(
        "mez-native-live-output-release-{}-{}",
        std::process::id(),
        crate::runtime::current_unix_millis()
    ));
    let _ = fs::remove_file(&release_path);
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "native shell live output".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test native progress".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "shell-1".to_string(),
                    rationale: "print native progress".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Print native progress".to_string(),
                        command: format!(
                            "printf \"%s%s\\n\" \"native-live-\" \"first\"; while [ ! -e \"{}\" ]; do sleep 0.01; done; printf \"%s%s\\n\" \"native-live-\" \"last\"",
                            release_path.display()
                        ),
                        interactive: false,
                        stateful: false,
                        timeout_ms: Some(5_000),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.remove_pending_agent_provider_task("turn-1");
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get("%1").unwrap();
    assert_eq!(pane_context.agent_status.as_deref(), Some("executing"));

    let dispatch = service
        .claim_native_shell_action("turn-1", "shell-1")
        .unwrap()
        .expect("native shell action should be queued for a worker");
    let marker = dispatch.marker.clone();
    let (progress_sender, mut progress_receiver) = tokio::sync::watch::channel(None);
    let worker = thread::spawn(move || {
        crate::runtime::execute_native_shell_dispatch_with_progress(dispatch, progress_sender)
    });
    let progress_deadline = std::time::Instant::now() + Duration::from_secs(2);
    let (revision, output_preview) = loop {
        if let Some(progress) = progress_receiver.borrow_and_update().clone() {
            break progress;
        }
        assert!(
            std::time::Instant::now() < progress_deadline,
            "native worker did not publish output before completion"
        );
        thread::sleep(Duration::from_millis(5));
    };
    assert!(
        !worker.is_finished(),
        "native worker completed before progress was observed"
    );

    assert!(
        service
            .apply_native_shell_progress(crate::runtime::RuntimeNativeShellProgress {
                turn_id: "turn-1".to_string(),
                action_id: "shell-1".to_string(),
                marker: marker.clone(),
                revision,
                output_preview,
            })
            .unwrap()
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("native-live-first"), "{pane_text}");
    let accepted = service.agent_shell_output_previews_for_tests("%1");
    assert_eq!(accepted.len(), 1, "{accepted:?}");
    assert_eq!(accepted[0].2, revision);

    service
        .apply_native_shell_progress(crate::runtime::RuntimeNativeShellProgress {
            turn_id: "turn-1".to_string(),
            action_id: "shell-1".to_string(),
            marker: marker.clone(),
            revision: revision.saturating_sub(1),
            output_preview: "older-native-tail".to_string(),
        })
        .unwrap();
    let stale_revision_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !stale_revision_text.contains("older-native-tail"),
        "{stale_revision_text}"
    );
    assert_eq!(
        service.agent_shell_output_previews_for_tests("%1")[0].2,
        revision
    );

    assert!(
        !service
            .apply_native_shell_progress(crate::runtime::RuntimeNativeShellProgress {
                turn_id: "turn-1".to_string(),
                action_id: "shell-1".to_string(),
                marker: format!("{marker}-stale"),
                revision: revision.saturating_add(1),
                output_preview: "stale-native-tail".to_string(),
            })
            .unwrap()
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!pane_text.contains("stale-native-tail"), "{pane_text}");

    fs::write(&release_path, b"release").unwrap();
    let outcome = worker.join().unwrap();
    assert!(service.complete_native_shell_action(outcome).unwrap());
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("native-live-first"), "{pane_text}");
    assert!(pane_text.contains("native-live-last"), "{pane_text}");
    assert!(
        !service
            .apply_native_shell_progress(crate::runtime::RuntimeNativeShellProgress {
                turn_id: "turn-1".to_string(),
                action_id: "shell-1".to_string(),
                marker,
                revision: revision.saturating_add(1),
                output_preview: "post-completion-native-tail".to_string(),
            })
            .unwrap()
    );
    let settled_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !settled_text.contains("post-completion-native-tail"),
        "{settled_text}"
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_file(release_path);
}

/// Verifies native shell execution bypasses occupied-pane readiness and presents
/// captured command output through the shell-view renderer rather than
/// suppressing it with the normal result-preview policy.
///
/// Pane execution writes child output through the PTY, while native execution
/// captures it directly. An alternate-screen program may leave the pane in
/// `interactive-blocked`, but native actions must still execute without pane
/// input and expose cleaned stdout and stderr in shell view without wrapper
/// traffic or a duplicate action-result header.
#[test]
fn runtime_native_agent_shell_command_output_is_visible_in_shell_view() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Verbose)
        .unwrap();
    service.set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Native));
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-native-visible-output","input":"print native markers"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "native shell output".to_string(),
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
                    rationale: "print native markers".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Print native markers".to_string(),
                        command: "printf 'native-stdout\\n'; printf 'native-stderr\\n' >&2"
                            .to_string(),
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
    service.remove_pending_agent_provider_task("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let dispatch = service
        .claim_native_shell_action("turn-1", "shell-1")
        .unwrap()
        .expect("native shell action should be queued for a worker");
    let outcome = crate::runtime::execute_native_shell_dispatch(dispatch);
    assert!(service.complete_native_shell_action(outcome).unwrap());
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("native-stdout"), "{pane_text}");
    assert!(pane_text.contains("native-stderr"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_MARKER_TOKEN"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_STATUS"), "{pane_text}");
    assert!(
        !pane_text.contains("mez> Print native markers"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies native `apply_patch` hunk failures leave recovery shadow text as
/// their sole pane-visible failure presentation.
///
/// Native patch execution first snapshots the target, then dispatches a
/// generated write phase. A failing write already queues the recovery shadow;
/// the generic failed-action outcome must not add a second red error row.
#[test]
fn runtime_native_apply_patch_failure_shows_only_recovery_shadow_text() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.set_agent_native_shell_mode_for_tests("%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-native-patch-failure","input":"patch Cargo.toml"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "native patch failure".to_string(),
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
                    id: "patch-1".to_string(),
                    rationale: "apply a deliberately stale patch".to_string(),
                    payload: mez_agent::AgentActionPayload::ApplyPatch {
                        patch: "*** Begin Patch\n*** Update File: Cargo.toml\n@@\n-__MEZ_NATIVE_PATCH_MISSING_CONTEXT__\n+updated\n*** End Patch".to_string(),
                        strip: None,
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

    let read_dispatch = service
        .claim_native_shell_action("turn-1", "patch-1")
        .unwrap()
        .expect("native apply-patch read should be queued");
    assert!(
        service
            .complete_native_shell_action(crate::runtime::execute_native_shell_dispatch(
                read_dispatch
            ))
            .unwrap()
    );
    let write_dispatch = service
        .claim_native_shell_action("turn-1", "patch-1")
        .unwrap()
        .expect("native apply-patch write should be queued");
    assert!(
        service
            .complete_native_shell_action(crate::runtime::execute_native_shell_dispatch(
                write_dispatch
            ))
            .unwrap()
    );

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: action failed; asking model to recover (patch hunk mismatch)"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("agent: apply patch (apply_patch failed"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that default agent command execution keeps one bounded command
/// preview while routing decoded command output into provider context. Raw
/// shell output may be base64-transported in the pane, but the model-facing
/// action result must still receive the decoded child-command output.
#[test]
fn runtime_agent_shell_command_output_keeps_decoded_context() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-hidden-output","input":"print a hidden marker"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
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
                    rationale: "print a hidden marker".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Print a hidden marker".to_string(),
                        command: "printf 'agent-hidden-%s\\n' output".to_string(),
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
    service.remove_pending_agent_provider_task("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    let mut context_text = String::new();
    for _ in 0..900 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        context_text = service
            .agent_turn_contexts()
            .get("turn-1")
            .unwrap()
            .blocks()
            .iter()
            .map(|block| block.content.as_str())
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        if context_text.contains("agent-hidden-output") {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
        thread::yield_now();
    }
    assert!(
        context_text.contains("agent-hidden-output"),
        "context={context_text} transactions={:?} pane={}",
        service.running_shell_transactions_for_tests(),
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_text.contains("mez> Print a hidden marker"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("$ printf 'agent-hidden-%s"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent-hidden-output"),
        "decoded command output should be visible as the transient tail line: {pane_text}"
    );
    assert!(
        !pane_text.contains("__MEZ_SHELL_OUTPUT_BASE64_BEGIN__"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("MEZ_MARKER_TOKEN"), "{pane_text}");
    assert!(!pane_text.contains("unset MEZ_MARKER_TOKEN"), "{pane_text}");
    let context_text = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        context_text.contains("agent-hidden-output"),
        "{context_text}"
    );
    assert!(context_text.contains("output:\n"), "{context_text}");
    service.terminate_all_pane_processes().unwrap();
}

/// through native execution and leaves the result visible to the continuation.
///
/// expose `shell_command` on the provider action surface, execute the command
/// Verifies shell-command pane logging stays empty when the wrapped command
/// Verifies shell-command pane logging stays empty when the wrapped command
/// leaking into the pane log because the success path preserved raw PTY preview
/// text when the cleaned command output was empty.
#[test]
fn runtime_agent_shell_command_without_output_keeps_mez_framing_out_of_logs() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-hidden-empty-output","input":"print nothing"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
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
                    rationale: "print nothing".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Print nothing".to_string(),
                        command: ":".to_string(),
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
    service.remove_pending_agent_provider_task("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    for _ in 0..600 {
        let _ = service.poll_pane_outputs(4096).unwrap();
        if service.agent_provider_task_is_pending("turn-1") {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
        thread::yield_now();
    }
    assert!(
        service.agent_provider_task_is_pending("turn-1"),
        "transactions={:?} pane={}",
        service.running_shell_transactions_for_tests(),
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("$ :"), "{pane_text}");
    assert!(
        !pane_text.contains("__MEZ_SHELL_OUTPUT_BASE64_BEGIN__"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("MEZ_MARKER_TOKEN"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_STATUS"), "{pane_text}");
    assert!(!pane_text.contains("MEZ_COMMAND_"), "{pane_text}");
    assert!(!pane_text.contains("unset MEZ_MARKER_TOKEN"), "{pane_text}");
    let context_text = service
        .agent_turn_contexts()
        .get("turn-1")
        .unwrap()
        .blocks()
        .iter()
        .map(|block| block.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(context_text.contains("command: :"), "{context_text}");
    assert!(context_text.contains("exit_code: 0"), "{context_text}");
    assert!(!context_text.contains("MEZ_MARKER_TOKEN"), "{context_text}");
    assert!(
        !context_text.contains("__MEZ_SHELL_OUTPUT_BASE64_BEGIN__"),
        "{context_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that the normal command preview is bounded using the pane's
/// display width. Long generated commands should remain inspectable without
/// flooding the pane buffer or hiding the fact that more wrapped lines exist.
#[test]
fn runtime_agent_shell_command_preview_is_wrapped_and_capped() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(24, 8).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(24, 8).unwrap(), 20).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-command-preview","input":"run a long command"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let command = "printf 'alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu\\n'";
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap shell response".to_string(),
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
                    rationale: "run a long command".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Run a long command".to_string(),
                        command: command.to_string(),
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
    service.remove_pending_agent_provider_task("turn-1");

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
    assert!(pane_text.contains("▐ $ printf 'alpha"), "{pane_text}");
    assert!(pane_text.contains("▐   ["), "{pane_text}");
    let command_preview_line_count = pane_text
        .lines()
        .skip_while(|line| !line.contains("▐ $ "))
        .take_while(|line| line.contains("▐ $ ") || line.starts_with("▐   "))
        .count();
    assert_eq!(command_preview_line_count, 10, "{pane_text}");
    assert!(
        !pane_text.contains("epsilon zeta eta theta iota kappa lambda mu"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies command previews on wide panes cap their display width at 120 cells.
///
/// The command preview renderer should avoid pane-width lines that are too long
/// to scan while still preserving the existing `$ ` prompt and continuation
/// indentation.
#[test]
fn runtime_agent_shell_command_preview_caps_wide_panes_at_120_cells() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(200, 24).unwrap(), 120)
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(200, 24).unwrap(), 120).unwrap(),
    );
    service
        .append_agent_command_preview_to_terminal_buffer(
            "%1",
            &format!("printf '{}'", "abcdef ".repeat(40)),
        )
        .unwrap();

    let styled_lines = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let command_lines = styled_lines
        .iter()
        .filter(|line| line.text.starts_with("▐ $ ") || line.text.starts_with("▐   "))
        .collect::<Vec<_>>();

    assert!(command_lines.len() > 1, "{styled_lines:?}");
    assert!(
        command_lines
            .iter()
            .all(|line| line.text.chars().count() <= 120),
        "{command_lines:?}"
    );
    assert!(
        command_lines[0].text.starts_with("▐ $ "),
        "{command_lines:?}"
    );
    assert!(
        command_lines
            .iter()
            .skip(1)
            .all(|line| line.text.starts_with("▐   ")),
        "{command_lines:?}"
    );
}

/// Verifies terminal failures without a pane-local running shell marker still
/// drain scheduler capacity.
///
/// Some runtime failure paths settle a turn after its pane shell session was
/// already detached or removed. Those paths still release a global scheduler
/// slot, so they must immediately start queued independent work instead of
/// leaving it parked until unrelated input arrives.
#[test]
fn runtime_no_shell_session_provider_failure_starts_queued_turn() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(90, 30).unwrap(), 120)
        .unwrap();
    let pane2 = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    for pane in ["%1", pane2.as_str()] {
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane)
            .unwrap();
        let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.set_pane_screen(pane.to_string(), screen);
    }

    service.start_agent_prompt_turn("%1", "first").unwrap();
    service
        .start_agent_prompt_turn(pane2.as_str(), "second")
        .unwrap();
    assert_eq!(service.agent_scheduler().snapshot().running, 1);
    assert_eq!(service.agent_scheduler().snapshot().queued, 1);
    service.agent_shell_store_mut().remove_session("%1");

    let error = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeBatchFailingProvider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert_eq!(
        service
            .agent_scheduler()
            .running_turns()
            .map(|running| running.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-2"]
    );
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(
        service
            .agent_shell_store()
            .get(pane2.as_str())
            .and_then(|session| session.running_turn_id.as_deref()),
        Some("turn-2")
    );
}

/// Verifies a failed action cannot terminally clean up a live shell sibling's
/// execution owner.
///
/// Mixed batches may fail one action synchronously before dispatching a later
/// shell-backed sibling. The aggregate failure must remain runtime-running
/// until the pane transaction settles, or its end marker cannot find the
/// execution record that owns the action result.
#[test]
fn runtime_failed_action_retains_live_shell_sibling_execution_until_settlement() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "run a mixed action batch")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.remove_pending_agent_provider_task(&turn.turn_id);

    let failed_action = mez_agent::AgentAction {
        id: "patch-invalid".to_string(),
        rationale: "exercise synchronous action failure".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: "invalid patch".to_string(),
            strip: None,
        },
    };
    let shell_action = mez_agent::AgentAction {
        id: "shell-live".to_string(),
        rationale: "exercise live sibling ownership".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Run the live sibling".to_string(),
            command: "printf 'live sibling settled\\n'".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let failed_result = mez_agent::ActionResult::failed(
        &turn,
        &failed_action,
        ActionStatus::Failed,
        "invalid_params",
        "apply_patch requires a valid Mezzanine patch block",
    )
    .unwrap();
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "mixed action batch".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "exercise mixed action ownership".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![failed_action, shell_action.clone()],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![
            failed_result,
            mez_agent::ActionResult {
                protocol: "maap/1".to_string(),
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                action_id: shell_action.id.clone(),
                action_type: "shell_command",
                status: ActionStatus::Running,
                content: Vec::new(),
                structured_content_json: None,
                permission_evaluation: None,
                is_error: false,
                error: None,
            },
        ],
        final_turn: false,
        terminal_state: AgentTurnState::Running,
    };
    append_test_execution_assistant_context(&mut service, &turn, &execution);
    service
        .agent_turn_executions_mut()
        .insert(turn.turn_id.clone(), execution);

    service
        .dispatch_stored_running_shell_actions(&turn.turn_id)
        .unwrap();
    let marker = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| match &transaction.kind {
            RunningShellTransactionKind::AgentAction { action_id }
                if action_id == &shell_action.id =>
            {
                Some(marker.clone())
            }
            _ => None,
        })
        .expect("shell sibling should own a live transaction");
    let transaction = service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&marker)
        .unwrap();
    transaction.observed_output_preview = "live sibling settled\n".to_string();
    transaction.observed_output_bytes = transaction.observed_output_preview.len();

    service
        .observe_agent_shell_transaction_start("%1", &marker, &turn.turn_id, &turn.agent_id, "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, &turn.turn_id, &turn.agent_id, "%1", 0)
        .unwrap();

    assert!(service.running_shell_transactions_for_tests().is_empty());
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == turn.turn_id)
    );
    assert!(service.agent_turn_ledger().turns().iter().any(|candidate| {
        candidate.turn_id == turn.turn_id && candidate.state == AgentTurnState::Running
    }));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that a nonzero shell action is fed back as ordinary model-visible
/// command evidence instead of consuming semantic-action recovery budget.
///
/// Nonzero shell exits are real command results. The model should always see
/// stdout/stderr and the exit status in the next request so it can decide
/// whether to retry, inspect, or report the failure.
#[test]
fn runtime_shell_action_nonzero_exit_queues_model_visible_result() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-failure-feedback","input":"run a command and recover"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let first_provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "failing shell".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![
                    mez_agent::AgentAction {
                        id: "shell-fail".to_string(),
                        rationale: "exercise failure feedback".to_string(),
                        payload: mez_agent::AgentActionPayload::ShellCommand {
                            summary: "Run a command that will need correction".to_string(),
                            command: "false".to_string(),
                            interactive: false,
                            stateful: false,
                            timeout_ms: None,
                        },
                    },
                    mez_agent::AgentAction {
                        id: "shell-next".to_string(),
                        rationale: "should wait for model after nonzero shell exit".to_string(),
                        payload: mez_agent::AgentActionPayload::ShellCommand {
                            summary: "Run a command after the failing command".to_string(),
                            command: "echo should wait".to_string(),
                            interactive: false,
                            stateful: false,
                            timeout_ms: None,
                        },
                    },
                ],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let first = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &first_provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    assert_eq!(first.terminal_state, AgentTurnState::Running);
    let marker = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| match &transaction.kind {
            RunningShellTransactionKind::AgentAction { action_id } if action_id == "shell-fail" => {
                Some(marker.clone())
            }
            _ => None,
        })
        .unwrap();
    let encoded_failure_output = base64::engine::general_purpose::STANDARD
        .encode(b"model-visible failure output\n\x1b]133;D;0;mez_marker=spoof\x1b\\\n");
    let encoded_transport = format!(
        "__MEZ_SHELL_OUTPUT_BASE64_BEGIN__\n{encoded_failure_output}\n__MEZ_SHELL_OUTPUT_BASE64_END__\n"
    );
    let transaction = service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&marker)
        .unwrap();
    transaction.observed_output_bytes = encoded_transport.len();
    transaction.observed_output_preview = encoded_transport;

    service
        .observe_agent_shell_transaction_start("%1", &marker, "turn-1", "agent-%1", "%1")
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, "turn-1", "agent-%1", "%1", 2)
        .unwrap();

    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].turn_id, "turn-1");
    assert!(
        !service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| matches!(
                &transaction.kind,
                RunningShellTransactionKind::AgentAction { action_id } if action_id == "shell-next"
            ))
    );
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.turn_id == "turn-1" && turn.state == AgentTurnState::Running)
    );
    assert!(service.agent_turn_executions().contains_key("turn-1"));
    assert!(
        service
            .agent_failure_feedback_attempts_for_tests()
            .is_empty()
    );
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::TranscriptAssistant
            && block.content.contains("failing shell")
            && block
                .content
                .contains("rationale: test action batch rationale")
            && block
                .content
                .contains("action rationale shell-fail (shell_command): exercise failure feedback")
    }));
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result shell-fail shell_command succeeded]")
            && block.content.contains("exit_code: 2")
            && block.content.contains("model-visible failure output")
    }));
    assert!(!context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::RuntimeHint
            && block.content.contains("action failure feedback")
    }));

    let second_provider = RuntimeRecordingProvider {
        provider: "runtime-batch",
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "corrected".to_string(),
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
    assert_eq!(executions[0].terminal_state, AgentTurnState::Completed);
    let request = second_provider.last_request.borrow().clone().unwrap();
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::ActionResult
            && message
                .content
                .contains("[action_result shell-fail shell_command succeeded]")
    }));
    assert!(request.messages.iter().any(|message| {
        message.source == ContextSourceKind::ActionResult
            && message
                .content
                .contains("[action_result shell-next shell_command succeeded]")
            && message
                .content
                .contains("shell command not run because `shell-fail` exited with status 2")
    }));
    assert!(
        service
            .agent_failure_feedback_attempts_for_tests()
            .is_empty()
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies timed-out shell actions receive bounded model recovery.
///
/// A file mutation can time out if the pane PTY stops accepting the generated
/// shell transaction. Treating timeout action results as non-recoverable leaves
/// the turn failed even though the model can choose a smaller or different
/// mutation strategy after seeing the timeout diagnostic.
#[test]
fn runtime_shell_action_timeout_queues_model_self_correction() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "write a file")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    service.remove_pending_agent_provider_task(&turn.turn_id);

    let action = mez_agent::AgentAction {
        id: "patch-timeout".to_string(),
        rationale: "write a file through the pane shell".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Add File: note.txt\n+hello\n*** End Patch".to_string(),
            strip: None,
        },
    };
    let timed_out = mez_agent::ActionResult::failed(
        &turn,
        &action,
        ActionStatus::TimedOut,
        "shell_timeout",
        "shell command timed out after 30000 ms",
    )
    .unwrap();
    let mut execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "write file timed out".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                actions: vec![action],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![timed_out],
        final_turn: false,
        terminal_state: AgentTurnState::Failed,
    };

    append_test_execution_assistant_context(&mut service, &turn, &execution);
    let queued = service
        .queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "shell_timeout_recovery",
        )
        .unwrap();

    assert!(queued);
    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == turn.turn_id)
    );
    let context = service.agent_turn_contexts().get(&turn.turn_id).unwrap();
    assert!(context.blocks().iter().any(|block| {
        block.source == ContextSourceKind::ActionResult
            && block
                .content
                .contains("[action_result patch-timeout apply_patch timed_out]")
            && block
                .content
                .contains("shell command timed out after 30000 ms")
    }));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies agent-authored heredoc shell commands fail before pane dispatch.
///
/// MAAP validation rejects heredocs before runtime execution. This protects the
/// pane from receiving an unterminated shell construct and ensures that a fixed
/// provider response surfaces a repairable diagnostic instead of attempting to
/// execute the invalid command.
#[test]
fn runtime_shell_command_heredoc_is_rejected_before_pane_dispatch() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-heredoc-feedback","input":"write a file"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "heredoc shell".to_string(),
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
                    id: "shell-heredoc".to_string(),
                    rationale: "write a file with a heredoc".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Write a file with a heredoc".to_string(),
                        command: "cat > /tmp/mez-heredoc.rs <<'EOF'\nfn main() {}\nEOF".to_string(),
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

    assert_eq!(execution.terminal_state, AgentTurnState::Failed);
    assert!(service.running_shell_transactions_for_tests().is_empty());
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert!(
        execution
            .response
            .raw_text
            .contains("maap_validation_error"),
        "{}",
        execution.response.raw_text
    );
    assert!(
        execution
            .response
            .raw_text
            .contains("heredoc redirection is disabled"),
        "{}",
        execution.response.raw_text
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!pane_text.contains("MEZ_COMMAND_"), "{pane_text}");
    service.terminate_all_pane_processes().unwrap();
}
