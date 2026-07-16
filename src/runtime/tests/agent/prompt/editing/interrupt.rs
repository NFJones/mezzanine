//! Agent prompt editing interrupt tests.

use super::*;

/// Verifies that agent-mode prompt submissions convert runtime errors into a
/// pane-local error log instead of letting the attached terminal step fail.
/// Invalid-state errors previously bubbled out of this path and could terminate
/// the foreground client instead of leaving the agent prompt usable.
#[test]
fn runtime_attached_agent_prompt_logs_invalid_state_errors_non_modally() {
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
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ForwardToPane(b"/stop\r".to_vec())],
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
    assert!(service.pending_agent_provider_tasks().is_empty());
    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "");
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent command error: agent shell session has no running turn"),
        "{pane_text}"
    );
    let compact_pane_text = pane_text.replace("\n▐ ", "");
    assert!(compact_pane_text.contains("(invalid_state)"), "{pane_text}");
}

/// Verifies that Ctrl+D from a visible agent prompt restores the parent shell
/// cursor after agent-authored text has been rendered into the pane. The
/// preceding agent output leaves the pane screen on a Mezzanine-rendered line,
/// so the subsequent parent prompt repaint must still advance through the
/// prompt's trailing space instead of landing one cell early.
#[test]
fn runtime_agent_shell_ctrl_d_after_agent_output_restores_prompt_cursor() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    let enter_effects = service.drain_pane_io_transition().side_effects;
    assert_eq!(pane_input_effects(&enter_effects).len(), 1);
    service
        .append_agent_assistant_text_to_terminal_buffer(&pane_id, "done")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x04".to_vec())],
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
    assert!(report.full_redraw_required);
    assert_eq!(
        service
            .agent_shell_store()
            .get(&pane_id)
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Hidden),
        "Ctrl+D should hide the agent prompt before the parent prompt repaint"
    );
    let exit_effects = service.drain_pane_io_transition().side_effects;
    let exit_inputs = pane_input_effects(&exit_effects);
    assert_eq!(exit_inputs.len(), 1);
    assert_eq!(exit_inputs[0].pane_input_parts().0, pane_id);
    assert_eq!(exit_inputs[0].pane_input_parts().1, b"\x04");

    let prompt = b"user@host ~/repo $ ";
    let prompt_repaint = service.renderable_pane_output_bytes(&pane_id, prompt);
    assert_eq!(prompt_repaint, prompt);
    service
        .apply_pane_output_bytes(pane_id.clone(), prompt.to_vec())
        .unwrap();
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig {
                window_frames_enabled: false,
                pane_frames_enabled: false,
                ..TerminalClientLoopConfig::default()
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(view.cursor_column, "user@host ~/repo $ ".chars().count());
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies Escape does not interrupt active agent work or exit agent mode.
///
/// The pane-local prompt owns Escape while visible. During active work it only
/// clears draft input, so an empty draft leaves the shell visible and the
/// running turn untouched.
#[test]
fn runtime_agent_prompt_escape_preserves_running_turn() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt-escape-stop","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
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
    assert_eq!(report.agent_prompt_inputs_applied, 0);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        Some("turn-1")
    );
    assert!(service.agent_turn_is_running("turn-1"));
}

/// Verifies Ctrl+C uses the same active-work interruption path as Escape.
///
/// Ctrl+C arrives through readline as a cancellation outcome rather than the
/// direct Escape byte path, so it needs separate coverage to ensure both input
/// routes reuse the same `/stop` behavior.
#[test]
fn runtime_agent_prompt_ctrl_c_interrupts_running_turn() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt-ctrl-c-stop","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x03".to_vec())],
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
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    assert!(!service.agent_turn_is_running("turn-1"));
}

/// Verifies Ctrl+C is idempotent when the tracked turn already terminalized.
///
/// Macro failure can mark a turn as failed while the pane-local shell session
/// still carries the turn id during unwind. Ctrl+C should clear that stale
/// binding through the stop path without trying to reclassify the ledger turn
/// as interrupted and surfacing an already-terminal conflict.
#[test]
fn runtime_agent_prompt_ctrl_c_after_failed_turn_is_idempotent() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "run macro").unwrap();
    let turn_id = started.turn_id.clone();
    let _ = service.agent_scheduler_mut().complete(&turn_id);
    service
        .agent_turn_ledger_mut()
        .finish_turn(&turn_id, AgentTurnState::Failed)
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x03".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Default::default(),
            },
        )
        .unwrap();

    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
}

/// Verifies Escape is a no-op for an empty idle pane-local agent shell.
///
/// Agent-shell exit is reserved for Ctrl+C confirmation or empty Ctrl+D, so
/// Escape with no draft input keeps the prompt visible without forwarding bytes
/// to the pane PTY.
#[test]
fn runtime_agent_prompt_escape_keeps_empty_idle_shell_visible() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
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
    assert_eq!(report.agent_prompt_inputs_applied, 0);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
}

/// Verifies idle Ctrl+C requires confirmation before exiting agent mode.
///
/// Ctrl+C is easy to hit accidentally while editing a prompt. The first press
/// should show a pane-local status message and keep the prompt visible; the
/// second press within the confirmation window exits.
#[test]
fn runtime_agent_prompt_ctrl_c_requires_second_press_when_idle() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let first = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x03".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(first.forwarded_bytes, 0);
    assert_eq!(first.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("press ctrl-c again within 3s to exit agent mode"),
        "{pane_text}"
    );

    let second = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x03".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(second.forwarded_bytes, 0);
    assert_eq!(second.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Hidden)
    );
}

/// Verifies idle Ctrl+C clears a nonempty pane-local agent prompt before using
/// the double-confirm exit path for an already empty prompt.
#[test]
fn runtime_agent_prompt_ctrl_c_clears_nonempty_buffer_when_idle() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let edit = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"draft text".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(edit.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_prompt_inputs_for_tests()
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "draft text"
    );

    let clear = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x03".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(clear.forwarded_bytes, 0);
    assert_eq!(clear.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "");
    assert!(prompt_state.display_lines.is_empty());
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );

    let confirm = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x03".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(confirm.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("press ctrl-c again within 3s to exit agent mode")
    );
}

/// Verifies Ctrl+L clears the live viewport while keeping the pane-local agent
/// prompt available and preserving prior visible content in pane history.
#[test]
fn runtime_agent_prompt_ctrl_l_clears_pane_buffer() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(50, 8).unwrap(), 120).unwrap();
    screen.feed(b"old agent output");
    service.set_pane_screen("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("old agent output")
    );

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x0c".to_vec())],
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
    assert!(
        !service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("old agent output")
    );
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("old agent output")
    );
    assert!(service.agent_shell_store().get("%1").is_some());
}
