//! Runtime tests for agent presentation copying behavior.

use super::*;

/// Verifies large foreground paste payloads stay intact and exit copy mode.
///
/// Host clipboard paste can deliver tens or hundreds of kilobytes as one
/// terminal input event. The runtime should preserve the logical byte stream as
/// one ordered pane-input side effect for the async pane worker to chunk, while
/// returning the target pane to the live bottom so stale copy-mode scroll state
/// cannot keep the user looking at old history.
#[test]
fn runtime_deferred_foreground_paste_stays_ordered_and_exits_copy_mode() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );
    service.ensure_active_copy_mode("%1").unwrap();

    let input = vec![b'x'; mez_mux::process::PTY_INPUT_WRITE_CHUNK_BYTES * 2 + 17];
    let (report, deferred) = service
        .apply_attached_terminal_step_transition(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(input.clone())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, input.len());
    let pane_inputs = pane_input_effects(&deferred.side_effects);
    assert_eq!(pane_inputs.len(), 1);
    assert_eq!(pane_inputs[0].pane_input_parts().1, input);
    assert!(
        pane_inputs
            .iter()
            .all(|effect| effect.pane_input_parts().0 == "%1")
    );
    assert!(
        pane_inputs
            .iter()
            .all(|effect| !effect.pane_input_parts().2)
    );
    assert!(!service.active_copy_modes().contains_key("%1"));
}

/// Verifies the terminal `copy-mode` command opens over the same live pane
/// viewport height that the attached-terminal copy-mode key path uses.
///
/// The command previously subtracted one row from the pane descriptor before
/// building `CopyMode`, which made the first copy-mode viewport start one line
/// below the live pane when no frame or prompt row was actually present.
#[test]
fn runtime_copy_mode_command_preserves_live_viewport_height() {
    let mut service = test_runtime_service_with_size(Size::new(20, 4).unwrap());
    service.set_frame_visibility_for_tests(false, false);
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"one\ntwo\nthree\nfour");
    service.set_pane_screen(pane_id.clone(), screen);

    service
        .execute_terminal_command(&primary, "copy-mode")
        .unwrap();

    let visible = service
        .active_copy_modes()
        .get(&pane_id)
        .unwrap()
        .visible_lines()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(visible, vec!["one", "two", "three", "four"]);
}

/// Verifies copy-mode key navigation marks the attached view dirty without
/// invalidating the retained terminal frame. Copy-mode scrolling only changes
/// pane content and cursor placement, so it should use the diff renderer rather
/// than clearing the whole attached terminal.
#[test]
fn runtime_copy_mode_key_navigation_requests_diff_refresh() {
    let mut service = test_runtime_service_with_size(Size::new(20, 4).unwrap());
    service.set_frame_visibility_for_tests(false, false);
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 20).unwrap();
    screen.feed(b"one\ntwo\nthree\nfour\nfive\nsix");
    service.set_pane_screen(pane_id.clone(), screen);
    service.ensure_active_copy_mode(&pane_id).unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleCopyMode(
                    mez_mux::copy::CopyModeKeyAction::PageUp,
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert!(service.active_copy_modes().contains_key(&pane_id));
}

/// Verifies that `/copy` uses retained model-authored `say` text and supports
/// the same pane, buffer, and clipboard targets as other copy commands.
///
/// The raw provider response can contain transport or protocol scaffolding, so
/// the command must copy the latest explicit `say.text` rather than raw model
/// text or an action-summary substitute.
#[test]
fn runtime_agent_shell_copy_writes_latest_say_text_to_destinations() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    *service.host_clipboard_mut_for_tests() =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
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
    let started = service
        .start_agent_prompt_turn("%1", "produce final answer")
        .unwrap();
    assert_eq!(started.state, AgentTurnState::Running);
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "raw transport envelope should not be copied".to_string(),
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
                        id: "say-1".to_string(),
                        rationale: "give an earlier answer".to_string(),
                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Final,
                            text: "Earlier say text.".to_string(),
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                    mez_agent::AgentAction {
                        id: "say-2".to_string(),
                        rationale: "give the answer that should be copied".to_string(),
                        payload: mez_agent::AgentActionPayload::Say {
                            status: mez_agent::SayStatus::Final,
                            text: "Latest say text.".to_string(),
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
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            ModelProfile {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Completed);

    let buffer_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-copy-buffer","method":"agent/shell/command","params":{"idempotency_key":"agent-copy-buffer","input":"/copy buffer retained-say"}}"#,
        &primary,
    );

    assert!(
        buffer_response.contains(r#""kind":"mutated""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains(r#""command":"copy""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains("destination=buffer"),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains("source=runtime-agent-say"),
        "{buffer_response}"
    );
    assert_eq!(
        service.paste_buffers().get("retained-say"),
        Some("Latest say text.")
    );
    assert_ne!(
        service.paste_buffers().get("retained-say"),
        Some("raw transport envelope should not be copied")
    );
    let buffers = service.paste_buffers().list();
    assert!(
        buffers.iter().any(|buffer| {
            buffer.name == "retained-say" && buffer.origin.as_deref() == Some("agent:turn-1:say")
        }),
        "{buffers:?}"
    );

    let clipboard_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-copy-clipboard","method":"agent/shell/command","params":{"idempotency_key":"agent-copy-clipboard","input":"/copy clipboard"}}"#,
        &primary,
    );
    assert!(
        clipboard_response.contains("destination=clipboard"),
        "{clipboard_response}"
    );
    assert_eq!(
        service.paste_buffers().get("clipboard"),
        Some("Latest say text.")
    );
    assert!(
        TEST_HOST_CLIPBOARD_WRITES
            .lock()
            .unwrap()
            .last()
            .is_some_and(|text| text == "Latest say text.")
    );

    let default_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-copy-default","method":"agent/shell/command","params":{"idempotency_key":"agent-copy-default","input":"/copy"}}"#,
        &primary,
    );
    assert!(
        default_response.contains("destination=clipboard"),
        "{default_response}"
    );
    assert_eq!(
        service.paste_buffers().get("clipboard"),
        Some("Latest say text.")
    );

    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 6).unwrap(), 20).unwrap(),
    );
    let pane_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-copy-pane","method":"agent/shell/command","params":{"idempotency_key":"agent-copy-pane","input":"/copy pane"}}"#,
        &primary,
    );
    assert!(
        pane_response.contains("destination=pane"),
        "{pane_response}"
    );
    let pane_text_after = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text_after.contains("Latest say text."),
        "{pane_text_after}"
    );
}

/// Verifies that normal-mode panes retain a bounded hidden trace log that can
/// later be dumped to the pane, an internal paste buffer, or the clipboard.
///
/// This protects post-failure diagnostics: users should not have to predict in
/// advance that trace mode will be needed, but the retained trace remains
/// bounded and explicit to export.
#[test]
fn runtime_agent_copy_trace_log_retains_hidden_trace_and_writes_destinations() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    *service.host_clipboard_mut_for_tests() =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-trace-log","input":"trace retention sentinel"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "trace raw sentinel".to_string(),
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
                    rationale: "retain trace details".to_string(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: "Trace retained.".to_string(),
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
    let pane_text_before = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_text_before.contains("agent trace: turn turn-1: MAAP request"),
        "{pane_text_before}"
    );

    let buffer_response = service
        .execute_agent_shell_command(&primary, "/copy-trace-log buffer retained-trace")
        .unwrap();
    assert!(
        buffer_response.contains(r#""command":"copy-trace-log""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains(r#""kind":"mutated""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains("destination=buffer"),
        "{buffer_response}"
    );
    let buffer = service.paste_buffers().get("retained-trace").unwrap();
    assert!(buffer.contains("trace raw sentinel"), "{buffer}");
    assert!(
        buffer.contains("agent trace: turn turn-1: MAAP response"),
        "{buffer}"
    );

    let clipboard_response = service
        .execute_agent_shell_command(&primary, "/copy-trace-log clipboard")
        .unwrap();
    assert!(
        clipboard_response.contains("destination=clipboard"),
        "{clipboard_response}"
    );
    let clipboard = service.paste_buffers().get("clipboard").unwrap();
    assert!(clipboard.contains("trace raw sentinel"), "{clipboard}");
    assert!(
        TEST_HOST_CLIPBOARD_WRITES
            .lock()
            .unwrap()
            .last()
            .is_some_and(|text| text.contains("trace raw sentinel"))
    );

    let pane_response = service
        .execute_agent_shell_command(&primary, "/copy-trace-log pane")
        .unwrap();
    assert!(
        pane_response.contains("destination=pane"),
        "{pane_response}"
    );
    let pane_text_after = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text_after.contains("agent trace log for pane %1"),
        "{pane_text_after}"
    );
    assert!(
        pane_text_after.contains("trace raw sentinel"),
        "{pane_text_after}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies `/copy-context` exports the assembled provider request context
/// through the same pane, buffer, and clipboard targets as the other copy
/// commands.
///
/// The idle path is intentionally covered here because users invoke this
/// diagnostic command when they need to inspect the next prompt's context
/// before a turn is running.
#[test]
fn runtime_agent_copy_context_writes_idle_context_to_destinations() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    *service.host_clipboard_mut_for_tests() =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let buffer_response = service
        .execute_agent_shell_command(&primary, "/copy-context buffer retained-context")
        .unwrap();
    assert!(
        buffer_response.contains(r#""command":"copy-context""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains(r#""kind":"mutated""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains("destination=buffer"),
        "{buffer_response}"
    );
    let buffer = service.paste_buffers().get("retained-context").unwrap();
    assert!(
        buffer.contains(r#""kind": "model_request_context_dump""#),
        "{buffer}"
    );
    assert!(buffer.contains("idle-context-preview-%1"), "{buffer}");

    let clipboard_response = service
        .execute_agent_shell_command(&primary, "/copy-context clipboard")
        .unwrap();
    assert!(
        clipboard_response.contains("destination=clipboard"),
        "{clipboard_response}"
    );
    let clipboard = service.paste_buffers().get("clipboard").unwrap();
    assert!(
        clipboard.contains(r#""kind": "model_request_context_dump""#),
        "{clipboard}"
    );
    assert!(
        TEST_HOST_CLIPBOARD_WRITES
            .lock()
            .unwrap()
            .last()
            .is_some_and(|text| text.contains("idle-context-preview-%1"))
    );

    let pane_response = service
        .execute_agent_shell_command(&primary, "/copy-context pane")
        .unwrap();
    assert!(
        pane_response.contains("destination=pane"),
        "{pane_response}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("model_request_context_dump"),
        "{pane_text}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies `/copy-patches` exports exact retained patch payloads and statuses
/// through the same pane, buffer, and clipboard targets as `/copy-trace-log`.
///
/// Patch bodies are deliberately omitted from durable transcript summaries, so
/// this command must use the runtime's structured patch ledger rather than
/// scraping rendered pane text or compact transcript entries.
#[test]
fn runtime_agent_copy_patches_writes_retained_patches_to_destinations() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    *service.host_clipboard_mut_for_tests() =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "create a note")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");
    let target_rel = "target/mez-copy-patches-export/note.txt".to_string();
    let patch = format!("*** Begin Patch\n*** Add File: {target_rel}\n+alpha\n*** End Patch");
    let action = mez_agent::AgentAction {
        id: "patch-1".to_string(),
        rationale: "write a note".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: patch.clone(),
            strip: None,
        },
    };
    let result =
        mez_agent::ActionResult::succeeded(&turn, &action, vec!["patch applied".to_string()], None);
    let execution = mez_agent::AgentTurnExecution {
        request: runtime_model_request_fixture(&turn.turn_id),
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap patch response".to_string(),
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
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
        latest_response_usage: Default::default(),
        routing_token_usage_by_model: std::collections::BTreeMap::new(),
        action_results: vec![result],
        final_turn: true,
        terminal_state: AgentTurnState::Completed,
    };
    service.record_runtime_agent_patch_results_for_turn(&turn, &execution);

    let buffer_response = service
        .execute_agent_shell_command(&primary, "/copy-patches buffer retained-patches")
        .unwrap();
    assert!(
        buffer_response.contains(r#""command":"copy-patches""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains("destination=buffer"),
        "{buffer_response}"
    );
    let buffer = service.paste_buffers().get("retained-patches").unwrap();
    assert!(buffer.contains("agent patches for pane %1"), "{buffer}");
    assert!(
        buffer.contains("patch 1: turn=turn-1 action=patch-1 status=succeeded"),
        "{buffer}"
    );
    assert!(buffer.contains("*** Begin Patch"), "{buffer}");
    assert!(buffer.contains(&target_rel), "{buffer}");
    assert!(buffer.contains("+alpha"), "{buffer}");

    let clipboard_response = service
        .execute_agent_shell_command(&primary, "/copy-patches clipboard")
        .unwrap();
    assert!(
        clipboard_response.contains("destination=clipboard"),
        "{clipboard_response}"
    );
    let clipboard = service.paste_buffers().get("clipboard").unwrap();
    assert!(clipboard.contains("status=succeeded"), "{clipboard}");
    assert!(
        TEST_HOST_CLIPBOARD_WRITES
            .lock()
            .unwrap()
            .last()
            .is_some_and(|text| text.contains(&patch))
    );

    let pane_response = service
        .execute_agent_shell_command(&primary, "/copy-patches pane")
        .unwrap();
    assert!(
        pane_response.contains("destination=pane"),
        "{pane_response}"
    );
    let pane_text_after = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text_after.contains("agent patches for pane %1"),
        "{pane_text_after}"
    );
    assert!(
        pane_text_after.contains("status=succeeded"),
        "{pane_text_after}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies `/copy-patches` keeps every patch attempt for a session even when
/// recovery reuses the same turn id and model-authored action id.
///
/// Patch recovery often happens inside one agent turn, and models frequently
/// reuse simple action ids such as `patch`. The export ledger must therefore
/// treat a new running patch after a settled patch as a new attempt rather than
/// overwriting the earlier failed or successful attempt.
#[test]
fn runtime_agent_copy_patches_retains_reused_action_id_attempts() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    let started = service
        .start_agent_prompt_turn("%1", "patch the file")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .expect("started turn should be recorded");

    let first_patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch";
    let second_patch =
        "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-current\n+updated\n*** End Patch";
    let build_execution =
        |patch: &str, result: mez_agent::ActionResult| mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture(&turn.turn_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: format!("patch attempt for {}", result.action_id),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "test action batch rationale".to_string(),
                    thought: None,
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    actions: vec![mez_agent::AgentAction {
                        id: result.action_id.clone(),
                        rationale: "apply a source patch".to_string(),
                        payload: mez_agent::AgentActionPayload::ApplyPatch {
                            patch: patch.to_string(),
                            strip: None,
                        },
                    }],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![result],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        };
    let action_for_result = |patch: &str| mez_agent::AgentAction {
        id: "patch-retry".to_string(),
        rationale: "apply a source patch".to_string(),
        payload: mez_agent::AgentActionPayload::ApplyPatch {
            patch: patch.to_string(),
            strip: None,
        },
    };

    let first_action = action_for_result(first_patch);
    let first_running = mez_agent::ActionResult::running(
        &turn,
        &first_action,
        vec!["shell command accepted for pane execution".to_string()],
        None,
    );
    service.record_runtime_agent_patch_results_for_turn(
        &turn,
        &build_execution(first_patch, first_running),
    );
    let first_failed = mez_agent::ActionResult::failed(
        &turn,
        &first_action,
        ActionStatus::Failed,
        "shell_command_failed",
        "shell command exited with status 1",
    )
    .unwrap();
    service.record_runtime_agent_patch_results_for_turn(
        &turn,
        &build_execution(first_patch, first_failed),
    );

    let second_action = action_for_result(second_patch);
    let second_running = mez_agent::ActionResult::running(
        &turn,
        &second_action,
        vec!["shell command accepted for pane execution".to_string()],
        None,
    );
    service.record_runtime_agent_patch_results_for_turn(
        &turn,
        &build_execution(second_patch, second_running),
    );
    let second_succeeded = mez_agent::ActionResult::succeeded(
        &turn,
        &second_action,
        vec!["patch applied".to_string()],
        None,
    );
    service.record_runtime_agent_patch_results_for_turn(
        &turn,
        &build_execution(second_patch, second_succeeded),
    );

    let copy_response = service
        .execute_agent_shell_command(&primary, "/copy-patches buffer all-patches")
        .unwrap();
    assert!(
        copy_response.contains(r#""command":"copy-patches""#),
        "{copy_response}"
    );
    assert!(copy_response.contains("patches=2"), "{copy_response}");
    let retained = service.paste_buffers().get("all-patches").unwrap();
    assert!(
        retained.contains("patch 1: turn=turn-1 action=patch-retry status=failed"),
        "{retained}"
    );
    assert!(
        retained.contains("patch 2: turn=turn-1 action=patch-retry status=succeeded"),
        "{retained}"
    );
    assert!(retained.contains("-old"), "{retained}");
    assert!(retained.contains("+new"), "{retained}");
    assert!(retained.contains("-current"), "{retained}");
    assert!(retained.contains("+updated"), "{retained}");
    service.terminate_all_pane_processes().unwrap();
}
