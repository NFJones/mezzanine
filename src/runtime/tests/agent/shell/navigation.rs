//! Agent shell navigation tests.

use super::*;

/// Verifies runtime attached mux action toggles agent shell state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_attached_mux_action_toggles_agent_shell_state() {
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

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();
    assert_eq!(report.mux_actions_applied, 1);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert!(report.unsupported_actions.is_empty());
    let list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"list","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(list.contains(r#""visible":true"#), "{list}");

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();
    assert_eq!(report.mux_actions_applied, 1);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    let list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"list2","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(list.contains(r#""visible":false"#), "{list}");
}

/// Verifies that terminal command execution uses live runtime state for the
/// agent shell toggle instead of falling through to the offline no-op command
/// planner. This covers both show and hide transitions for the active pane and
/// verifies transition clears preserve prior visible content in pane history.
#[test]
fn runtime_terminal_command_toggles_agent_shell_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();
    screen.feed(b"history line\nvisible before agent");
    service.pane_screens.insert("%1".to_string(), screen);
    let history_before_enter = service.pane_screen("%1").unwrap().history().len();
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("visible before agent")
    );

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains(r#""command":"agent-shell""#), "{show}");
    assert!(show.contains(r#""kind":"display""#), "{show}");
    assert!(show.contains("pane=%1"), "{show}");
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    assert!(
        show.contains(&format!("conversation_id={conversation_id}")),
        "{show}"
    );
    assert!(show.contains("visibility=visible"), "{show}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    let after_enter_screen = service.pane_screen("%1").unwrap();
    assert!(after_enter_screen.history().len() > history_before_enter);
    assert!(
        !after_enter_screen
            .visible_lines()
            .join("\n")
            .contains("visible before agent")
    );
    assert!(
        after_enter_screen
            .normal_content_lines()
            .join("\n")
            .contains("visible before agent")
    );
    let history_before_exit = after_enter_screen.history().len();
    service
        .pane_screens
        .get_mut("%1")
        .unwrap()
        .feed(b"visible inside agent");
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("visible inside agent")
    );

    let hide = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(hide.contains("visibility=hidden"), "{hide}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Hidden)
    );
    let after_exit_screen = service.pane_screen("%1").unwrap();
    assert!(after_exit_screen.history().len() > history_before_exit);
    assert!(
        !after_exit_screen
            .visible_lines()
            .join("\n")
            .contains("visible inside agent")
    );
    assert!(
        after_exit_screen
            .normal_content_lines()
            .join("\n")
            .contains("visible inside agent")
    );

    let show_again = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show_again.contains("visibility=visible"), "{show_again}");
    let after_reentry_screen = service.pane_screen("%1").unwrap();
    assert!(
        !after_reentry_screen
            .visible_lines()
            .join("\n")
            .contains("visible inside agent"),
        "agent reentry should start from a clean viewport, not scroll old agent logs back into view"
    );
    assert!(
        after_reentry_screen
            .normal_content_lines()
            .join("\n")
            .contains("visible inside agent")
    );
}

/// Verifies that showing agent mode starts a pane-local subshell and hiding it
/// exits that subshell instead of sending redraw traffic to the user's original
/// interactive shell. This protects prompt, option, and environment mutations
/// made by agent commands from leaking back to the parent shell, and confirms
/// that retained hidden-render suppression is cleared so the parent prompt
/// repaint can advance the terminal cursor to the end of the prompt line.
#[test]
fn runtime_agent_shell_toggle_enters_and_exits_pane_subshell() {
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
    let enter_input = service.drain_pane_io_transition().side_effects;
    assert_eq!(pane_input_effects(&enter_input).len(), 1);
    assert_eq!(enter_input[0].pane_input_parts().0, pane_id);
    let enter_text = String::from_utf8_lossy(enter_input[0].pane_input_parts().1);
    assert!(
        enter_text.contains("command env -u BASH_ENV -u ENV -u ZDOTDIR"),
        "{enter_text}"
    );
    assert!(enter_text.contains("HISTFILE=/dev/null"), "{enter_text}");
    assert!(enter_text.contains("'/bin/sh'"), "{enter_text}");
    assert!(service.agent_subshell_panes.contains(&pane_id));
    service.remember_mez_wrapper_filter_command(&pane_id, "MEZ_MARKER_TOKEN='abc'");

    let hide = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(hide.contains("visibility=hidden"), "{hide}");
    let exit_effects = service.drain_pane_io_transition().side_effects;
    let exit_inputs = pane_input_effects(&exit_effects);
    assert_eq!(exit_inputs.len(), 1);
    assert_eq!(exit_inputs[0].pane_input_parts().0, pane_id);
    assert_eq!(exit_inputs[0].pane_input_parts().1, b"\x04");
    assert!(!service.agent_subshell_panes.contains(&pane_id));
    assert!(!service.hidden_shell_render_retention_timer_needed());
    let simple_prompt_repaint = service.visible_pane_output_bytes(&pane_id, b"\r$ ");
    assert_eq!(simple_prompt_repaint, b"\r$ ");
    let prompt_repaint = service.renderable_pane_output_bytes(&pane_id, b"user@host ~/repo $ ");
    assert_eq!(prompt_repaint, b"user@host ~/repo $ ");
    service
        .apply_pane_output_bytes(pane_id.clone(), b"user@host ~/repo $ ".to_vec())
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

/// Verifies that the live subshell EOF path also restores the parent prompt
/// cursor after agent-authored text has already moved the pane screen. This
/// covers the Ctrl+D path that exits the child agent shell, waits for the parent
/// shell prompt to repaint, and then presents the attached terminal cursor.
#[test]
fn runtime_agent_shell_ctrl_d_after_agent_output_restores_live_parent_cursor() {
    let shell_path = PathBuf::from("/bin/sh");
    let shell_available = fs::metadata(&shell_path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false);
    if !shell_available {
        eprintln!("skipping live cursor regression because /bin/sh is unavailable");
        return;
    }
    let mut service = RuntimeSessionService::with_event_log(
        Session::new_default(
            ResolvedShell::new(shell_path.clone(), ShellSource::ShellEnv),
            Size::new(80, 24).unwrap(),
        ),
        PathBuf::from("/tmp/mez-1000/default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    *service.host_clipboard_mut_for_tests() = HostClipboard::disabled();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some(
            "/bin/sh -c 'PS1=\"parent$ \"; export PS1; exec /bin/sh -i'",
        ))
        .unwrap();
    let mut initial_screen = String::new();
    for _ in 0..200 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        initial_screen = service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n");
        if initial_screen.contains("parent$") {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        initial_screen.contains("parent$"),
        "parent prompt did not arrive: {initial_screen:?}"
    );

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    service
        .append_agent_assistant_text_to_terminal_buffer("%1", "done")
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
    assert!(report.full_redraw_required);

    let prompt_column = "parent$ ".chars().count();
    let mut cursor_column = None;
    let mut observed_cursor = None;
    let mut observed_screen = String::new();
    for _ in 0..300 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        let cursor = service.pane_screen("%1").unwrap().cursor_state();
        let screen_text = service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n");
        observed_cursor = Some(cursor);
        observed_screen = screen_text.clone();
        if screen_text.contains("parent$") && cursor.column == prompt_column {
            cursor_column = Some(cursor.column);
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }

    assert_eq!(
        cursor_column,
        Some(prompt_column),
        "parent prompt cursor should land after the trailing prompt space; observed_cursor={observed_cursor:?}; observed_screen={observed_screen:?}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies that `/exit` from the pane-scoped agent prompt performs the same
/// subshell exit as the keyboard toggle while preserving pane-visible content in
/// history. This covers the slash-command path used by Escape, Ctrl+C, Ctrl+D
/// on an empty prompt, `/quit`, and direct `/exit` submissions through the
/// control API.
#[test]
fn runtime_agent_shell_slash_exit_exits_pane_subshell() {
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
    assert!(service.agent_subshell_panes.contains(&pane_id));
    service
        .pane_screens
        .get_mut(&pane_id)
        .unwrap()
        .feed(b"slash exit history\nslash exit visible text");
    assert!(
        service
            .pane_screen(&pane_id)
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("slash exit visible text")
    );

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-exit","method":"agent/shell/command","params":{"idempotency_key":"agent-exit","input":"/exit"}}"#,
        &primary,
    );
    assert!(response.contains(r#""visibility":"hidden""#), "{response}");
    let exit_effects = service.drain_pane_io_transition().side_effects;
    let exit_inputs = pane_input_effects(&exit_effects);
    assert_eq!(exit_inputs.len(), 1);
    assert_eq!(exit_inputs[0].pane_input_parts().0, pane_id);
    assert_eq!(exit_inputs[0].pane_input_parts().1, b"\x04");
    assert!(!service.agent_subshell_panes.contains(&pane_id));
    let after_exit_screen = service.pane_screen(&pane_id).unwrap();
    assert!(
        !after_exit_screen
            .visible_lines()
            .join("\n")
            .contains("slash exit visible text")
    );
    assert!(
        after_exit_screen
            .normal_content_lines()
            .join("\n")
            .contains("slash exit visible text")
    );
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies `/exit` stops an active pane-local turn before hiding agent mode.
/// This protects the exit paths used by slash commands, keyboard shortcuts, and
/// control clients from leaving provider or shell-action work running unseen.
#[test]
fn runtime_agent_shell_slash_exit_stops_running_turn_before_hiding() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt-exit-stop","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-exit","method":"agent/shell/command","params":{"idempotency_key":"agent-exit-stop","input":"/exit"}}"#,
        &primary,
    );

    assert!(response.contains(r#""command":"exit""#), "{response}");
    assert!(response.contains(r#""visibility":"hidden""#), "{response}");
    assert!(response.contains("stopped_turn=turn-1"), "{response}");
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(session.visibility, AgentShellVisibility::Hidden);
    assert_eq!(session.running_turn_id, None);
    assert!(!service.agent_turn_is_running("turn-1"));
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("Stopped after"), "{pane_text}");
}

/// Verifies ordinary pane input is consumed while an agent-shell hide request
/// is waiting for the active turn to stop. This prevents user keystrokes from
/// leaking into the parent shell before the `/stop` contract has completed.
#[test]
fn runtime_agent_shell_exit_pending_blocks_foreground_input() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .request_hide_pending_task_completion("%1")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"leak\r".to_vec())],
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
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: input blocked while agent shell is stopping"),
        "{pane_text}"
    );
}

/// Verifies that runtime-state failures from agent slash commands are reported
/// through the agent display channel instead of surfacing as JSON-RPC errors.
/// This keeps agent-mode clients alive when a runtime-backed command hits an
/// invalid state, such as stopping when no turn is running.
#[test]
fn runtime_control_reports_invalid_state_agent_shell_errors_as_display() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-command-invalid-state","method":"agent/shell/command","params":{"idempotency_key":"agent-command-invalid-state","input":"/stop"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(
        response.contains("agent command error: agent shell session has no running turn"),
        "{response}"
    );
    assert!(response.contains("(invalid_state)"), "{response}");
    assert!(!response.contains(r#""error""#), "{response}");
}
