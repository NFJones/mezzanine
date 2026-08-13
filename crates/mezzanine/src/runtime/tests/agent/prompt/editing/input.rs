//! Agent prompt editing input tests.

use super::*;

/// Verifies that ordinary pane input is redirected into the pane-local agent
/// prompt while agent mode is active, without entering the older modal prompt
/// loop. Mux actions remain available because only forward-to-pane text is
/// intercepted by the runtime.
#[test]
fn runtime_attached_input_submits_visible_agent_prompt_non_modally() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert!(config.pane_bracketed_paste_mode);
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ForwardToPane(
            b"summarize\nmore\r".to_vec(),
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert_eq!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .map(|task| task.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-1"]
    );
    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "");
    assert_eq!(
        prompt_state.prompt.buffer.history(),
        &[String::from("summarize\nmore")]
    );
    assert!(prompt_state.display_lines.is_empty());
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("user> summarize"), "{pane_text}");
    assert!(pane_text.contains("more"), "{pane_text}");
    assert!(
        !pane_text.contains("agent: turn turn-1 running"),
        "{pane_text}"
    );
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .unwrap();
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert_eq!(turn.state, AgentTurnState::Running);
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content.contains("summarize\nmore"))
    );
}

/// Verifies ordinary agent typing does not run the pane-resize pipeline while
/// the prompt remains the same height.
///
/// Prompt edits still require a refreshed client frame, but issuing a PTY
/// resize and pane-change event for every keystroke adds agent-only latency and
/// unnecessary process work. Crossing a wrapped-row boundary remains covered
/// by the pane-local prompt-height resize regression.
#[test]
fn runtime_agent_prompt_same_row_edits_do_not_resize_the_pty() {
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
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service.sync_tracked_pty_sizes().unwrap();
    let resize_events_before = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .into_iter()
        .filter(|event| event.payload.contains(r#""layout":"resized""#))
        .count();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"abc".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(report.view_refresh_required);
    let resize_events_after = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .into_iter()
        .filter(|event| event.payload.contains(r#""layout":"resized""#))
        .count();
    assert_eq!(resize_events_after, resize_events_before);

    service.terminate_all_pane_processes().unwrap();
}

/// Verifies the first typed byte edits the prompt while selector discovery is
/// still unresolved. Candidate loading may traverse files and query durable
/// stores, so an in-flight refresh must never be awaited by terminal input.
#[test]
fn runtime_agent_prompt_input_remains_editable_while_selector_refresh_is_pending() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );
    let _pending_refresh = service.hold_agent_prompt_selector_refresh_for_tests("%1");

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"x".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_prompt_inputs_for_tests()
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "x"
    );
    assert!(
        !service
            .agent_prompt_inputs_for_tests()
            .get("%1")
            .unwrap()
            .selector_extra_candidates_loaded
    );
}

/// Verifies a below-threshold bracketed paste retains exact multiline text.
///
/// Small pastes stay directly editable rather than becoming collapsed paste
/// blocks, but their newlines, blank lines, tabs, and surrounding whitespace
/// must remain literal and must not submit the prompt before a later Enter.
#[test]
fn runtime_agent_prompt_preserves_below_threshold_split_paste_fidelity() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );

    let payload = "\nfirst\n\n\tsecond  \n";
    for input in [
        format!("prefix \u{1b}[200~{}", &payload[..8]).into_bytes(),
        format!("{}\u{1b}[201~ suffix", &payload[8..]).into_bytes(),
    ] {
        service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(input)],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    }

    let expected = format!("prefix {payload} suffix");
    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), expected);
    assert!(prompt_state.prompt.buffer.history().is_empty());
    assert!(service.pending_agent_provider_tasks().is_empty());

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\r".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(
        prompt_state.prompt.buffer.history(),
        std::slice::from_ref(&expected)
    );
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content.contains(&expected))
    );
}

/// Verifies large prompt paste blocks can exceed the visible pane area.
///
/// Bracketed paste payloads may arrive split across terminal reads and contain
/// far more text than can be rendered in the prompt area. The prompt renderer
/// should show one compact block while the submitted turn receives the exact
/// payload.
#[test]
fn runtime_agent_prompt_preserves_large_split_paste_beyond_visible_area() {
    let mut service = test_runtime_service_with_size(Size::new(50, 8).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(50, 8).unwrap(), 10).unwrap(),
    );

    let payload = (0..80)
        .map(|index| format!("line-{index:02}-{}", "x".repeat(36)))
        .collect::<Vec<_>>()
        .join("\n");
    let mut first = Vec::new();
    first.extend_from_slice(b"prefix ");
    first.extend_from_slice(b"\x1b[200~");
    first.extend_from_slice(&payload.as_bytes()[..payload.len() / 2]);
    let mut second = Vec::new();
    second.extend_from_slice(&payload.as_bytes()[payload.len() / 2..]);
    second.extend_from_slice(b"\x1b[201~ suffix\r");

    for input in [first, second] {
        service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(input)],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    }

    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(
        prompt_state.prompt.buffer.history(),
        &[format!("prefix {payload} suffix")]
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("user> prefix [Pasted"), "{pane_text}");
    assert!(!pane_text.contains("line-79"), "{pane_text}");
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| { block.content.contains(&format!("prefix {payload} suffix")) })
    );
}

/// Verifies that the pane-local agent prompt accepts encoded Ctrl+R from
/// terminals that use xterm modifyOtherKeys for modified printable keys.
///
/// Agent mode intercepts ordinary pane input before it reaches the PTY. This
/// protects that interception path so encoded reverse-search keys still edit
/// the prompt from its history instead of becoming a no-op escape sequence.
#[test]
fn runtime_agent_prompt_accepts_encoded_ctrl_r_history_search() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    {
        let prompt_state = service
            .agent_prompt_inputs_mut_for_tests()
            .get_mut("%1")
            .unwrap();
        prompt_state
            .prompt
            .buffer
            .set_history(vec!["/status".to_string(), "/help".to_string()]);
        prompt_state.prompt.buffer.set_line("/s");
    }

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"\x1b[27;5;114~".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "/status");
}

/// Verifies standalone Escape clears pane-local agent prompt text without
/// hiding the agent shell.
///
/// Agent-shell exit is reserved for Ctrl+C confirmation or empty Ctrl+D. A
/// normal Escape press should only clear the current draft and keep the pane
/// prompt session active.
#[test]
fn runtime_agent_prompt_escape_clears_input_without_hiding_shell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    {
        let prompt_state = service
            .agent_prompt_inputs_mut_for_tests()
            .get_mut("%1")
            .unwrap();
        prompt_state.prompt.buffer.set_line("draft text");
    }
    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    let followup = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"next".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(followup.forwarded_bytes, 0);
    assert_eq!(followup.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "next");
}

/// Verifies standalone Escape cancels pane-local agent reverse search without
/// exiting the agent shell.
///
/// Agent prompts share readline behavior with the primary command prompt, but
/// Escape also has agent-mode exit semantics. This keeps the reverse-search
/// case routed to the prompt before the broader exit handling runs.
#[test]
fn runtime_agent_prompt_escape_cancels_reverse_search() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    {
        let prompt_state = service
            .agent_prompt_inputs_mut_for_tests()
            .get_mut("%1")
            .unwrap();
        prompt_state
            .prompt
            .buffer
            .set_history(vec!["/status".to_string(), "/help".to_string()]);
        prompt_state.prompt.buffer.set_line("/s");
    }

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x12".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(
        service
            .agent_prompt_inputs_for_tests()
            .get("%1")
            .unwrap()
            .prompt
            .reverse_search_active()
    );

    let escape = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(escape.forwarded_bytes, 0);
    assert_eq!(escape.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert!(!prompt_state.prompt.reverse_search_active());
    assert_eq!(prompt_state.prompt.buffer.line(), "/s");
    assert!(service.agent_shell_store().get("%1").is_some());
}

/// Verifies Ctrl+V reads the host clipboard as one bracketed paste operation.
///
/// Raw clipboard CRLF bytes must never pass through the ordinary key decoder,
/// where their carriage returns would submit the prompt. Legacy, CSI-u, and
/// modifyOtherKeys Ctrl+V sequences must each consume only the paste command,
/// leaving coalesced suffix text editable until a later explicit Enter submits
/// exactly one agent turn.
#[test]
fn runtime_agent_prompt_ctrl_v_preserves_multiline_clipboard_until_enter() {
    let mut service = test_runtime_service();
    *service.host_clipboard_mut_for_tests() =
        HostClipboard::new(ignored_host_clipboard_copy, ctrl_v_host_clipboard_read);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );

    let expected = "  first\r\n\r\n\tsecond  \r\n";
    for input in [
        b"\x16suffix".as_slice(),
        b"\x1b[118;5usuffix".as_slice(),
        b"\x1b[27;5;118~suffix".as_slice(),
    ] {
        let pasted = service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(input.to_vec())],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
        complete_agent_prompt_clipboard_read(&mut service);

        assert_eq!(pasted.forwarded_bytes, 0);
        assert_eq!(pasted.agent_prompt_inputs_applied, 1);
        assert!(service.pending_agent_provider_tasks().is_empty());
        assert_eq!(
            service
                .agent_prompt_inputs_for_tests()
                .get("%1")
                .unwrap()
                .prompt
                .buffer
                .line(),
            format!("{expected}suffix")
        );
        service
            .agent_prompt_inputs_mut_for_tests()
            .get_mut("%1")
            .unwrap()
            .prompt
            .buffer
            .set_line("");
    }

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x16".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    let submitted = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\r".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    complete_agent_prompt_clipboard_read(&mut service);

    assert_eq!(submitted.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_prompt_inputs_for_tests()
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .history(),
        std::slice::from_ref(&expected.to_string())
    );
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
}

/// Executes the typed bounded clipboard completion used by synchronous runtime tests.
fn complete_agent_prompt_clipboard_read(service: &mut RuntimeSessionService) {
    let mut effects = service.drain_host_clipboard_read_transition().side_effects;
    assert_eq!(effects.len(), 1);
    let RuntimeSideEffect::ReadHostClipboard { generation, plan } = effects.remove(0) else {
        panic!("expected one host clipboard read side effect");
    };
    let content = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(crate::host::terminal::read_host_clipboard_plan_async(plan));
    service
        .apply_host_clipboard_event(HostClipboardEvent::ReadCompleted {
            generation,
            content,
        })
        .unwrap();
}

/// Supplies multiline clipboard text for the Ctrl+V prompt regression.
fn ctrl_v_host_clipboard_read() -> Option<String> {
    Some("  first\r\n\r\n\tsecond  \r\n".to_string())
}

/// Ignores copy requests because this regression exercises only clipboard reads.
fn ignored_host_clipboard_copy(_: &str) -> bool {
    true
}
