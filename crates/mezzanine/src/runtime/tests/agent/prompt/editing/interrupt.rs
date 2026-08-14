//! Agent prompt editing interrupt tests.

use super::*;

/// Verifies that agent-mode prompt submissions convert runtime errors into a
/// window status error instead of letting the attached terminal step fail.
/// Invalid-state errors previously bubbled out of this path and could terminate
/// the foreground client instead of leaving the agent prompt usable. Command
/// failures must not be retained in the pane log.
#[test]
fn runtime_attached_agent_prompt_reports_invalid_state_errors_in_status_line() {
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
        .map(|screen| screen.normal_content_lines().join("\n"))
        .unwrap_or_default();
    assert!(
        service
            .primary_error_status_overlay()
            .is_some_and(|status| status
                .contains("agent command error: agent shell session has no running turn")),
        "{:?}",
        service.primary_error_status_overlay()
    );
    assert!(
        !pane_text.contains("agent command error:") && !pane_text.contains("(invalid_state)"),
        "{pane_text}"
    );
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
    let exit_bytes = exit_inputs[0].pane_input_parts().1;
    assert!(
        !exit_bytes
            .windows(b"__MEZ_COMMAND_PAYLOAD_END_".len())
            .any(|window| window == b"__MEZ_COMMAND_PAYLOAD_END_"),
        "a prompt-gated wrapper that was never sent must not receive payload"
    );
    assert_eq!(exit_bytes.last(), Some(&b'\x04'));

    assert!(
        std::ptr::eq(
            service.pane_screen(&pane_id).unwrap(),
            service.process_pane_screen(&pane_id).unwrap(),
        ),
        "Ctrl+D must restore the process surface immediately"
    );
    let pane_log_before_exit_echo = service
        .process_pane_screen(&pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let cursor_before_exit_echo = service
        .process_pane_screen(&pane_id)
        .unwrap()
        .cursor_state();
    let exit_marker = service
        .agent_subshell_exit_marker_for_tests(&pane_id)
        .unwrap()
        .to_vec();
    service
        .apply_pane_output_bytes(
            pane_id.clone(),
            b"delayed child prompt\r\n\x1b[?2004l\r\r\nexit\r\n".to_vec(),
        )
        .unwrap();
    assert_eq!(
        service
            .process_pane_screen(&pane_id)
            .unwrap()
            .normal_content_lines()
            .join("\n"),
        pane_log_before_exit_echo,
        "all child-owned output before the parent boundary must remain out of the pane log"
    );
    let marker_split = exit_marker.len() / 2;
    service
        .apply_pane_output_bytes(pane_id.clone(), exit_marker[..marker_split].to_vec())
        .unwrap();
    assert_eq!(
        service
            .process_pane_screen(&pane_id)
            .unwrap()
            .normal_content_lines()
            .join("\n"),
        pane_log_before_exit_echo,
        "a fragmented parent boundary must not enter the pane log"
    );
    let mut completed_boundary = exit_marker[marker_split..].to_vec();
    completed_boundary.extend_from_slice(b"\x1b]133;A\x1b\\parent$ ");
    service
        .apply_pane_output_bytes(pane_id.clone(), completed_boundary)
        .unwrap();
    let pane_log_after_exit_echo = service
        .process_pane_screen(&pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_log_after_exit_echo
            .lines()
            .any(|line| line.trim() == "exit"),
        "readline cleanup and the child-shell EOF echo must not enter the pane log: {pane_log_after_exit_echo:?}"
    );
    assert!(
        pane_log_after_exit_echo.contains("parent$"),
        "parent prompt bytes following the suppressed exit must remain visible: {pane_log_after_exit_echo:?}"
    );
    assert!(
        !pane_log_after_exit_echo.contains("parent$ parent$ "),
        "the restored parent prompt must overwrite the retained prompt instead of appending at its stale cursor: {pane_log_after_exit_echo:?}; cursor_before_exit={cursor_before_exit_echo:?}"
    );
    assert_eq!(
        service
            .process_pane_screen(&pane_id)
            .unwrap()
            .cursor_state()
            .column,
        "parent$ ".chars().count(),
        "the restored prompt cursor must follow exactly one prompt"
    );
    assert_eq!(
        service.visible_pane_output_bytes(&pane_id, b"ordinary parent output\r\n"),
        b"ordinary parent output\r\n",
        "the one-shot teardown filter must release subsequent parent output"
    );
    assert!(
        std::ptr::eq(
            service.pane_screen(&pane_id).unwrap(),
            service.process_pane_screen(&pane_id).unwrap(),
        ),
        "parent prompt output must not switch back to the hidden agent surface"
    );
    service
        .apply_pane_output_bytes(pane_id.clone(), b" ~/repo $ \x1b]133;B\x1b\\".to_vec())
        .unwrap();
    assert!(
        std::ptr::eq(
            service.pane_screen(&pane_id).unwrap(),
            service.process_pane_screen(&pane_id).unwrap(),
        ),
        "the completed parent prompt must release the process presentation"
    );
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
    assert_eq!(view.cursor_column, "parent$  ~/repo $ ".chars().count());
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies Ctrl+D after fresh start proof exits once bootstrap certification settles.
///
/// The correlated start observation consumes the deferred payload, so
/// cancellation can no longer complete the wrapper inline. Bootstrap
/// completion must obtain its second correlated observation, retry the
/// already-recorded hidden-shell exit, and queue EOF only after settlement.
#[test]
fn runtime_agent_shell_ctrl_d_waits_for_started_bootstrap_completion() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = "%1".to_string();
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    let _ = service.drain_pane_io_transition();
    let (marker, turn_id) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find(|(_, transaction)| transaction.kind == RunningShellTransactionKind::Bootstrap)
        .map(|(marker, transaction)| (marker.clone(), transaction.turn_id.clone()))
        .unwrap();
    service
        .observe_agent_shell_transaction_start(&pane_id, &marker, &turn_id, "agent-%1", &pane_id)
        .unwrap();
    let start_observation = service
        .drain_pane_io_transition()
        .side_effects
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PaneProcessIo {
                instance,
                effect:
                    crate::runtime::PaneProcessIoEffect::ObserveForegroundProcess {
                        observation_id,
                        expected_process_group_id,
                    },
            } => Some((instance, observation_id, expected_process_group_id)),
            _ => None,
        })
        .expect("bootstrap start should request a fresh foreground observation");
    assert_eq!(start_observation.2, None);
    service
        .apply_pane_foreground_process_observation_transition(
            start_observation.0,
            crate::runtime::PaneForegroundProcessObservation {
                observation_id: start_observation.1,
                process_name: Some("sh".to_string()),
                process_group_id: Some(41),
                current_working_directory: Some("/tmp".to_string()),
                error: None,
            },
        )
        .unwrap();
    let payload = service.drain_pane_io_transition().side_effects;
    assert_eq!(pane_input_effects(&payload).len(), 1);

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
    let pending_exit_effects = service.drain_pane_io_transition().side_effects;
    assert!(pane_input_effects(&pending_exit_effects).is_empty());
    assert!(service.agent_subshell_is_active(&pane_id));

    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\thost\ttest-host\n\
env\tuser\ttest-user\n\
env\tshell_path\t/bin/sh\n\
env\tshell_class\tposix-sh\n\
env\tpath\t/usr/bin:/bin\n\
env\tcwd\t/tmp\n\
env\tgit_repo\t0\n\
bootstrap\tcomplete\t1714500000\n";
    let transaction = service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&marker)
        .unwrap();
    transaction.observed_output_preview = output.to_string();
    transaction.observed_output_bytes = output.len();
    service
        .observe_agent_shell_transaction_end(&pane_id, &marker, &turn_id, "agent-%1", &pane_id, 0)
        .unwrap();

    let completion_observation = service
        .drain_pane_io_transition()
        .side_effects
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PaneProcessIo {
                instance,
                effect:
                    crate::runtime::PaneProcessIoEffect::ObserveForegroundProcess {
                        observation_id,
                        expected_process_group_id,
                    },
            } => Some((instance, observation_id, expected_process_group_id)),
            _ => None,
        })
        .expect("bootstrap completion should request a fresh foreground observation");
    assert_eq!(completion_observation.2, Some(41));
    service
        .apply_pane_foreground_process_observation_transition(
            completion_observation.0,
            crate::runtime::PaneForegroundProcessObservation {
                observation_id: completion_observation.1,
                process_name: Some("sh".to_string()),
                process_group_id: Some(41),
                current_working_directory: Some("/tmp".to_string()),
                error: None,
            },
        )
        .unwrap();
    let exit_effects = service.drain_pane_io_transition().side_effects;
    let exit_inputs = pane_input_effects(&exit_effects);
    assert_eq!(exit_inputs.len(), 1);
    assert_eq!(exit_inputs[0].pane_input_parts().1, b"\x04");
    assert!(!service.agent_subshell_is_active(&pane_id));
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies Escape interrupts active agent work without exiting agent mode.
///
/// Escape is an active-work interrupt equivalent to Ctrl+C, so it must submit
/// `/stop` while keeping the pane-local agent shell visible.
#[test]
fn runtime_agent_prompt_escape_interrupts_running_turn() {
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
/// should show a transient window status message without changing pane history
/// and keep the prompt visible; the second press within the confirmation
/// window exits.
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
        .map(|screen| screen.normal_content_lines().join("\n"))
        .unwrap_or_default();
    assert!(
        service.primary_error_status_overlay().is_some_and(
            |status| status.contains("press ctrl-c again within 3s to exit agent mode")
        ),
        "{:?}",
        service.primary_error_status_overlay()
    );
    assert!(!pane_text.contains("press ctrl-c again"), "{pane_text}");

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
/// the double-confirm exit path for an already empty prompt. Confirmation
/// feedback must use the window status line rather than pane history.
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
        service.primary_error_status_overlay().is_some_and(
            |status| status.contains("press ctrl-c again within 3s to exit agent mode")
        ),
        "{:?}",
        service.primary_error_status_overlay()
    );
    let pane_text = service
        .pane_screen("%1")
        .map(|screen| screen.normal_content_lines().join("\n"))
        .unwrap_or_default();
    assert!(!pane_text.contains("press ctrl-c again"), "{pane_text}");
}

/// Verifies Ctrl+L clears the live viewport while keeping the pane-local agent
/// prompt available and preserving prior visible content in pane history.
#[test]
fn runtime_agent_prompt_ctrl_l_clears_pane_buffer() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    let conversation_id = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    let mut screen = TerminalScreen::new(Size::new(50, 8).unwrap(), 120).unwrap();
    screen.feed(b"old agent output");
    service.set_agent_pane_screen("%1", conversation_id, screen);
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
