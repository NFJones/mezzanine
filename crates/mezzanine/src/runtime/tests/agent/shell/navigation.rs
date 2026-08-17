//! Agent shell navigation tests.

use super::*;
use crate::runtime::processes::RuntimePaneEnvironmentAuthority;

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
    screen.feed(b"history line\r\nvisible before agent");
    service.set_pane_screen("%1".to_string(), screen);
    assert!(
        service
            .process_pane_screen("%1")
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
    let after_enter_screen = service.agent_pane_screen("%1").unwrap();
    assert!(
        !after_enter_screen
            .visible_lines()
            .join("\n")
            .contains("visible before agent")
    );
    assert!(
        service
            .process_pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("visible before agent")
    );
    service
        .agent_pane_screen_mut("%1")
        .unwrap()
        .feed(b"visible inside agent");
    assert!(
        service
            .agent_pane_screen("%1")
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
    let after_exit_screen = service.process_pane_screen("%1").unwrap();
    assert!(
        after_exit_screen
            .visible_lines()
            .join("\n")
            .contains("visible before agent")
    );
    assert!(
        service
            .agent_pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("visible inside agent")
    );

    let show_again = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show_again.contains("visibility=visible"), "{show_again}");
    let after_reentry_screen = service.agent_pane_screen("%1").unwrap();
    assert!(
        after_reentry_screen
            .visible_lines()
            .join("\n")
            .contains("visible inside agent"),
        "agent reentry should restore the retained agent viewport"
    );
    assert!(
        service
            .process_pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("visible before agent")
    );
}

/// Verifies that showing agent mode starts a pane-local subshell and hiding it
/// exits that subshell instead of sending redraw traffic to the user's original
/// interactive shell. This protects prompt, option, and environment mutations
/// made by agent commands from leaking back to the parent shell, and confirms
/// that hiding agent mode immediately restores unmediated process input without
/// arming hidden bootstrap work.
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
    service
        .process_pane_screen_mut(&pane_id)
        .unwrap()
        .feed(b"parent$ ");
    let parent_content_before_agent = service
        .process_pane_screen(&pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let parent_cursor_before_agent = service
        .process_pane_screen(&pane_id)
        .unwrap()
        .cursor_state();

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    let enter_input = service.drain_pane_io_transition().side_effects;
    let enter_inputs = pane_input_effects(&enter_input);
    assert_eq!(enter_inputs.len(), 1);
    assert_eq!(enter_inputs[0].pane_input_parts().0, pane_id);
    let enter_text = String::from_utf8_lossy(enter_inputs[0].pane_input_parts().1);
    let enter_source = decoded_posix_shell_wrapper_sources(&enter_text);
    assert!(
        enter_source.contains("command env \\\n  -u BASH_ENV \\\n  -u ENV \\\n  -u ZDOTDIR"),
        "{enter_source}"
    );
    assert!(
        enter_source.contains("HISTFILE=/dev/null"),
        "{enter_source}"
    );
    assert!(enter_source.contains("'/bin/sh'"), "{enter_source}");
    assert!(service.agent_subshell_is_active(&pane_id));
    service.remember_mez_wrapper_filter_command(&pane_id, "MEZ_MARKER_TOKEN='abc'");

    let hide = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(hide.contains("visibility=hidden"), "{hide}");
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
    assert!(!service.agent_subshell_is_active(&pane_id));
    assert!(!service.hidden_shell_render_retention_timer_needed());
    assert!(!service.pane_bootstrap_is_pending_for_tests(&pane_id));
    assert!(std::ptr::eq(
        service.pane_screen(&pane_id).unwrap(),
        service.process_pane_screen(&pane_id).unwrap(),
    ));
    let exit_marker = service
        .agent_subshell_exit_marker_for_tests(&pane_id)
        .unwrap()
        .to_vec();

    let marker_only = service.visible_pane_output_bytes(&pane_id, &exit_marker);
    assert!(
        marker_only.is_empty(),
        "a marker-only PTY fragment must not move the visible cursor"
    );
    service
        .apply_pane_output_bytes(
            pane_id.clone(),
            b"\x1b]133;R;mez_receiver=complete;mez_token=token;mez_marker=marker;mez_status=0\x1b\\parent$ "
                .to_vec(),
        )
        .unwrap();
    for _ in 0..64 {
        let _ = service.tick_hidden_shell_render_retention();
    }
    service
        .apply_pane_output_bytes(pane_id.clone(), b"\r\x1b[K\r".to_vec())
        .unwrap();
    assert_eq!(
        service
            .process_pane_screen(&pane_id)
            .unwrap()
            .normal_content_lines()
            .join("\n"),
        parent_content_before_agent,
        "parent prompt repaint and delayed Readline cleanup must not alter retained process content"
    );
    assert_eq!(
        service
            .process_pane_screen(&pane_id)
            .unwrap()
            .cursor_state(),
        parent_cursor_before_agent,
        "agent exit must restore the exact process cursor retained at entry"
    );
    assert!(
        std::ptr::eq(
            service.pane_screen(&pane_id).unwrap(),
            service.process_pane_screen(&pane_id).unwrap(),
        ),
        "parent output must remain on the process surface after agent exit"
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
    assert_eq!(view.cursor_column, parent_cursor_before_agent.column);

    let dispatch = service
        .write_input_to_pane(&primary, Some(&pane_id), b"echo parent\n")
        .unwrap();
    assert_eq!(dispatch.bytes_written, b"echo parent\n".len());
    assert!(!service.hidden_shell_render_retention_timer_needed());
    let user_input_effects = service.drain_pane_io_transition().side_effects;
    let user_inputs = pane_input_effects(&user_input_effects);
    assert_eq!(user_inputs.len(), 1);
    assert_eq!(user_inputs[0].pane_input_parts().0, pane_id);
    assert_eq!(user_inputs[0].pane_input_parts().1, b"echo parent\n");
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies a rapid agent-shell enter and exit releases the bootstrap input
/// lease before delivering EOF to the child shell. The async pane actor blocks
/// ordinary user input while a transaction lease is retained, so omitting this
/// release strands both the exit byte and every later parent-shell keystroke.
#[test]
fn runtime_agent_shell_rapid_toggle_releases_bootstrap_input_lease() {
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

    service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();

    let effects = service.drain_pane_io_transition().side_effects;
    let (lease_index, owner_id) = effects
        .iter()
        .enumerate()
        .find_map(|(index, effect)| match effect {
            RuntimeSideEffect::PaneProcessIo {
                effect: crate::runtime::PaneProcessIoEffect::AcquireShellInputLease { owner_id },
                ..
            } => Some((index, owner_id.as_str())),
            _ => None,
        })
        .expect("agent subshell entry should acquire a bootstrap input lease");
    let handoff_index = effects
        .iter()
        .position(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::PaneProcessIo {
                    effect: crate::runtime::PaneProcessIoEffect::WriteShellInput { delivery },
                    ..
                } if delivery.delivery_id.as_deref() == Some(owner_id)
            )
        })
        .expect("agent subshell entry should queue its leased shell handoff");
    let release_index = effects
        .iter()
        .position(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::PaneProcessIo {
                    effect:
                        crate::runtime::PaneProcessIoEffect::ReleaseShellInputLease {
                            owner_id: released_owner,
                        },
                    ..
                } if released_owner == owner_id
            )
        })
        .expect("rapid agent-shell exit should release the bootstrap input lease");
    let exit_index = effects
        .iter()
        .position(|effect| {
            matches!(
                effect,
                RuntimeSideEffect::PaneProcessIo {
                    effect: crate::runtime::PaneProcessIoEffect::WriteInput { bytes },
                    ..
                } if bytes.last() == Some(&b'\x04')
            )
        })
        .expect("rapid agent-shell exit should queue EOF for the child shell");

    assert!(lease_index < handoff_index, "{effects:?}");
    assert!(handoff_index < release_index, "{effects:?}");
    assert!(release_index < exit_index, "{effects:?}");
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies an immediate agent-shell re-entry waits for the restored parent
/// shell's current interaction epoch to be probed and bootstrapped. This
/// protects the fail-closed identity boundary while ensuring a rapid toggle is
/// resumed automatically instead of failing with an unprobed-identity error.
#[test]
fn runtime_agent_shell_immediate_reentry_resumes_after_parent_bootstrap() {
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

    service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    service.drain_pane_io_transition();
    service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    service.drain_pane_io_transition();

    let show_again = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show_again.contains("visibility=visible"), "{show_again}");
    assert!(!service.agent_subshell_is_active(&pane_id));
    assert_eq!(
        service.pane_readiness_state(&pane_id),
        PaneReadinessState::Unknown
    );
    assert!(service.pane_bootstrap_is_pending_for_tests(&pane_id));
    assert!(
        pane_input_effects(&service.drain_pane_io_transition().side_effects).is_empty(),
        "re-entry must not write to the PTY before the parent shell is ready"
    );

    service.set_pane_readiness(&pane_id, PaneReadinessState::PromptCandidate);
    assert_eq!(service.maybe_bootstrap_ready_panes().unwrap(), 1);
    assert_eq!(
        pane_input_effects(&service.drain_pane_io_transition().side_effects).len(),
        1,
        "the restored parent prompt should dispatch one identity probe"
    );
    let (identity_marker, identity_turn_id) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::ShellIdentityProbe { .. }
            )
            .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .expect("parent identity probe should be registered");
    service
        .observe_agent_shell_transaction_start(
            &pane_id,
            &identity_marker,
            &identity_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
        )
        .unwrap();
    let identity_output = format!(
        "\u{1e}mez_shell_identity_begin={identity_marker}\n\
         \u{1e}mez_shell_path=/bin/sh\n\
         \u{1e}mez_shell_version=sh\n\
         \u{1e}mez_shell_identity_end={identity_marker}\n"
    );
    let transaction = service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&identity_marker)
        .unwrap();
    transaction.observed_output_bytes = identity_output.len();
    transaction.observed_output_preview = identity_output;
    service
        .observe_agent_shell_transaction_end(
            &pane_id,
            &identity_marker,
            &identity_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
            0,
        )
        .unwrap();
    service.drain_pane_io_transition();

    let (bootstrap_marker, bootstrap_turn_id) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            (transaction.kind == RunningShellTransactionKind::Bootstrap)
                .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .expect("parent bootstrap should be registered");
    service
        .observe_agent_shell_transaction_start(
            &pane_id,
            &bootstrap_marker,
            &bootstrap_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
        )
        .unwrap();
    service.drain_pane_io_transition();
    let bootstrap_output = "env\tos\tLinux\n\
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
        .get_mut(&bootstrap_marker)
        .unwrap();
    transaction.observed_output_bytes = bootstrap_output.len();
    transaction.observed_output_preview = bootstrap_output.to_string();
    service
        .observe_agent_shell_transaction_end(
            &pane_id,
            &bootstrap_marker,
            &bootstrap_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
            0,
        )
        .unwrap();

    assert!(service.agent_subshell_is_active(&pane_id));
    assert_eq!(
        pane_input_effects(&service.drain_pane_io_transition().side_effects).len(),
        1,
        "successful parent bootstrap should enter exactly one agent subshell"
    );
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies a parent prompt observed while agent mode is hidden authorizes
/// discovery only after explicit re-entry. Normal parent-shell output must not
/// arm bootstrap work or inject a probe, while showing agent mode may consume
/// the retained prompt readiness immediately.
#[test]
fn runtime_agent_shell_reentry_uses_hidden_parent_prompt_without_hidden_probe() {
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

    service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    service.drain_pane_io_transition();
    service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    service.drain_pane_io_transition();
    let exit_marker = service
        .agent_subshell_exit_marker_for_tests(&pane_id)
        .unwrap()
        .to_vec();

    let mut parent_prompt_output = exit_marker;
    parent_prompt_output.extend_from_slice(b"\x1b]133;A\x1b\\user@host ~/repo $ \x1b]133;B\x1b\\");
    service
        .apply_pane_output_bytes(pane_id.clone(), parent_prompt_output)
        .unwrap();
    assert_eq!(
        service.pane_readiness_state(&pane_id),
        PaneReadinessState::PromptCandidate
    );
    assert!(!service.pane_bootstrap_is_pending_for_tests(&pane_id));
    assert!(
        pane_input_effects(&service.drain_pane_io_transition().side_effects).is_empty(),
        "hidden parent-shell output must not trigger generated pane input"
    );

    let show_again = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show_again.contains("visibility=visible"), "{show_again}");
    assert!(service.pane_bootstrap_is_pending_for_tests(&pane_id));
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| matches!(
                transaction.kind,
                RunningShellTransactionKind::ShellIdentityProbe { .. }
            )),
        "explicit re-entry should consume retained prompt readiness"
    );
    assert_eq!(
        pane_input_effects(&service.drain_pane_io_transition().side_effects).len(),
        1,
        "the identity probe may be written only after agent mode is shown"
    );
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies a failed restored-parent identity probe leaves rapid agent-shell
/// re-entry fail-closed. No child-shell command may be written when the new
/// interaction epoch cannot establish a valid shell identity.
#[test]
fn runtime_agent_shell_immediate_reentry_stays_closed_after_failed_identity_probe() {
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

    service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    service.drain_pane_io_transition();
    service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    service.drain_pane_io_transition();
    service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    service.drain_pane_io_transition();

    service.set_pane_readiness(&pane_id, PaneReadinessState::PromptCandidate);
    assert_eq!(service.maybe_bootstrap_ready_panes().unwrap(), 1);
    service.drain_pane_io_transition();
    let (identity_marker, identity_turn_id) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::ShellIdentityProbe { .. }
            )
            .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .expect("parent identity probe should be registered");
    service
        .observe_agent_shell_transaction_start(
            &pane_id,
            &identity_marker,
            &identity_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
        )
        .unwrap();
    service
        .observe_agent_shell_transaction_end(
            &pane_id,
            &identity_marker,
            &identity_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
            1,
        )
        .unwrap();

    assert!(!service.agent_subshell_is_active(&pane_id));
    assert_eq!(
        service.pane_readiness_state(&pane_id),
        PaneReadinessState::Degraded
    );
    assert!(
        pane_input_effects(&service.drain_pane_io_transition().side_effects).is_empty(),
        "a failed identity probe must not enter the agent subshell"
    );

    let observed = service
        .observe_passive_shell_prompt_candidate(&pane_id, "osc133-prompt")
        .unwrap();

    assert_eq!(observed, 1);
    assert_eq!(
        service.pane_readiness_state(&pane_id),
        PaneReadinessState::PromptCandidate
    );
    assert!(
        service.pane_bootstrap_is_pending_for_tests(&pane_id),
        "a fresh parent prompt must re-arm the failed identity bootstrap"
    );
    assert_eq!(service.maybe_bootstrap_ready_panes().unwrap(), 1);
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| {
                matches!(
                    transaction.kind,
                    RunningShellTransactionKind::ShellIdentityProbe { .. }
                )
            }),
        "the re-armed bootstrap must dispatch a second identity probe"
    );
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies agent entry while a foreign process owns the PTY creates one
/// bounded, generation-scoped bootstrap lifecycle without unsafe input.
///
/// SSH and container clients can expose a shell that differs from the host
/// pane shell, while password prompts and full-screen programs expose no shell
/// at all. Advisory adapter candidates must therefore wait for a prompt
/// boundary, and only a matching runtime challenge may advance discovery.
/// Provider work remains pending only while that lifecycle has a finite owner;
/// timeout fails it actionably without writing into the foreign foreground.
#[test]
fn runtime_agent_shell_entry_bootstraps_foreign_adapter_with_bounded_admission() {
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
    let primary_pid = service.pane_processes().primary_pid(&pane_id).unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test(&pane_id, None);
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    service
        .apply_pane_foreground_process_event(&pane_id, "ssh", primary_pid.saturating_add(1), None)
        .unwrap();

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();

    assert!(show.contains("visibility=visible"), "{show}");
    assert!(service.pane_has_uncertified_foreign_shell_boundary(&pane_id));
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("awaiting-adapter")
    );
    assert!(service.pane_bootstrap_has_bounded_progress_owner(&pane_id));
    assert!(service.agent_subshell_entry_is_deferred(&pane_id));
    assert!(!service.agent_subshell_is_active(&pane_id));
    assert!(
        pane_input_effects(&service.drain_pane_io_transition().side_effects).is_empty(),
        "foreign foreground ownership must block child-shell and discovery input"
    );
    let started = service
        .start_agent_prompt_turn(&pane_id, "list the current directory")
        .unwrap();
    let agent_id = AgentId::opaque(started.agent_id).unwrap();
    assert!(
        service
            .claim_configured_agent_provider_task(&agent_id, &started.turn_id)
            .unwrap()
            .is_none(),
        "provider dispatch must defer while foreign bootstrap has a deadline"
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == started.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Running),
        "bounded foreign bootstrap must retain the submitted prompt"
    );
    assert!(
        service.agent_provider_task_is_pending(&started.turn_id),
        "bounded foreign bootstrap must retain the pending provider task"
    );
    let interaction_generation =
        service.pane_foreground_process_diagnostic(&pane_id).json()["shell_interaction_generation"]
            .as_u64()
            .expect("foreign boundary should allocate an interaction generation");

    assert!(!service.enter_agent_subshell_if_needed(&pane_id).unwrap());
    assert_eq!(
        service.pane_foreground_process_diagnostic(&pane_id).json()["shell_interaction_generation"]
            .as_u64(),
        Some(interaction_generation),
        "repeated entry must not allocate another foreign interaction epoch"
    );
    assert!(
        pane_input_effects(&service.drain_pane_io_transition().side_effects).is_empty(),
        "a repeated entry attempt must remain input-free"
    );

    let candidate = TerminalOscEvent::ManagedShell {
        version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
        shell: mez_terminal::ManagedShellAdapter::Bash,
        token: "0123456789abcdef0123456789abcdef".to_string(),
        event: mez_terminal::ManagedShellProtocolEvent::ForeignAdapterCandidate {
            instance_id: "remote-bash-1".to_string(),
            trigger: None,
        },
    };
    assert_eq!(
        service
            .observe_agent_shell_transaction_events(&pane_id, std::slice::from_ref(&candidate))
            .unwrap(),
        0,
        "a candidate before the foreign prompt boundary must remain advisory"
    );
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("awaiting-adapter")
    );
    assert_eq!(
        service
            .observe_agent_shell_transaction_events(&pane_id, &[TerminalOscEvent::ShellPromptEnd])
            .unwrap(),
        1
    );
    assert_eq!(
        service
            .observe_agent_shell_transaction_events(&pane_id, &[candidate])
            .unwrap(),
        1
    );
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("challenging-adapter")
    );
    let challenge = service
        .foreign_shell_bootstrap_challenge_for_tests(&pane_id)
        .expect("an admitted candidate must receive a runtime challenge")
        .to_string();
    let challenge_effects = service.drain_pane_io_transition().side_effects;
    let challenge_inputs = pane_input_effects(&challenge_effects);
    assert_eq!(challenge_inputs.len(), 1);
    assert_eq!(challenge_inputs[0].pane_input_parts().0, pane_id);
    assert_eq!(
        challenge_inputs[0].pane_input_parts().1,
        format!(
            "\u{7}MEZ_BASH_FOREIGN_CHALLENGE 0123456789abcdef0123456789abcdef remote-bash-1 {challenge}\n"
        )
        .as_bytes(),
        "the first foreign input must contain only source-free challenge metadata"
    );
    let mismatched = TerminalOscEvent::ManagedShell {
        version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
        shell: mez_terminal::ManagedShellAdapter::Bash,
        token: "host-or-stale-token".to_string(),
        event: mez_terminal::ManagedShellProtocolEvent::ForeignChallengeCompleted {
            instance_id: "remote-bash-1".to_string(),
            challenge: challenge.clone(),
        },
    };
    assert_eq!(
        service
            .observe_agent_shell_transaction_events(&pane_id, &[mismatched])
            .unwrap(),
        0,
        "a stale or host adapter token must not cross the foreign boundary"
    );
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("challenging-adapter")
    );

    assert_eq!(
        service
            .apply_shell_transaction_timer_event(u64::MAX)
            .unwrap(),
        1
    );
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("failed")
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == started.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed),
        "foreign bootstrap timeout must settle the submitted prompt"
    );
    assert!(!service.agent_provider_task_is_pending(&started.turn_id));
    assert!(
        pane_input_effects(&service.drain_pane_io_transition().side_effects).is_empty(),
        "foreign bootstrap timeout must not interrupt or write to the foreign foreground"
    );

    service
        .apply_pane_foreground_process_event(&pane_id, "sh", primary_pid, None)
        .unwrap();

    assert!(!service.pane_has_uncertified_foreign_shell_boundary(&pane_id));
    assert!(service.pane_bootstrap_is_pending_for_tests(&pane_id));
    assert_eq!(service.maybe_bootstrap_ready_panes().unwrap(), 1);
    assert_eq!(
        pane_input_effects(&service.drain_pane_io_transition().side_effects).len(),
        1,
        "the restored primary shell prompt should dispatch one identity probe"
    );
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies a challenge-admitted foreign Bash adapter receives identity
/// discovery only through its own authenticated RX1 receiver.
///
/// The outer SSH/container process group remains the generation fence. Host
/// Bash tokens and raw shell source must never appear in the dispatched input.
#[test]
fn runtime_foreign_bash_challenge_dispatches_private_identity_probe() {
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
    let primary_pid = service.pane_processes().primary_pid(&pane_id).unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test(&pane_id, None);
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    service
        .apply_pane_foreground_process_event(&pane_id, "ssh", primary_pid.saturating_add(1), None)
        .unwrap();
    service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    service.drain_pane_io_transition();
    service
        .observe_agent_shell_transaction_events(&pane_id, &[TerminalOscEvent::ShellPromptEnd])
        .unwrap();
    service
        .observe_agent_shell_transaction_events(
            &pane_id,
            &[TerminalOscEvent::ManagedShell {
                version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                shell: mez_terminal::ManagedShellAdapter::Bash,
                token: "0123456789abcdef0123456789abcdef".to_string(),
                event: mez_terminal::ManagedShellProtocolEvent::ForeignAdapterCandidate {
                    instance_id: "remote-bash-1".to_string(),
                    trigger: None,
                },
            }],
        )
        .unwrap();
    let challenge = service
        .foreign_shell_bootstrap_challenge_for_tests(&pane_id)
        .unwrap()
        .to_string();
    service.drain_pane_io_transition();

    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ManagedShell {
                    version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                    shell: mez_terminal::ManagedShellAdapter::Bash,
                    token: "0123456789abcdef0123456789abcdef".to_string(),
                    event: mez_terminal::ManagedShellProtocolEvent::ForeignChallengeCompleted {
                        instance_id: "remote-bash-1".to_string(),
                        challenge,
                    },
                }],
            )
            .unwrap(),
        1
    );
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("identity-probing")
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| matches!(
                transaction.kind,
                RunningShellTransactionKind::ShellIdentityProbe { .. }
            ))
    );
    let identity_effects = service.drain_pane_io_transition().side_effects;
    let identity_inputs = pane_input_effects(&identity_effects);
    assert_eq!(identity_inputs.len(), 1);
    let identity_input = String::from_utf8_lossy(identity_inputs[0].pane_input_parts().1);
    assert!(
        identity_input.starts_with("\u{7}MEZ_BASH_RX1_BEGIN 0123456789abcdef0123456789abcdef "),
        "{identity_input:?}"
    );
    assert!(
        !identity_input.contains("host-or-stale-token"),
        "{identity_input:?}"
    );
    assert!(
        !identity_input.contains("mez_shell_identity_begin="),
        "{identity_input:?}"
    );

    let (identity_marker, identity_turn_id) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::ShellIdentityProbe { .. }
            )
            .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .expect("foreign identity probe should be registered");
    service
        .observe_agent_shell_transaction_start(
            &pane_id,
            &identity_marker,
            &identity_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
        )
        .unwrap();
    let identity_output = format!(
        "\u{1e}mez_shell_identity_begin={identity_marker}\n\
         \u{1e}mez_shell_path=/bin/bash\n\
         \u{1e}mez_shell_version=GNU bash, version 5.2\n\
         \u{1e}mez_shell_identity_end={identity_marker}\n"
    );
    let transaction = service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&identity_marker)
        .unwrap();
    transaction.observed_output_bytes = identity_output.len();
    transaction.observed_output_preview = identity_output;
    service
        .observe_agent_shell_transaction_end(
            &pane_id,
            &identity_marker,
            &identity_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
            0,
        )
        .unwrap();
    service
        .observe_agent_shell_transaction_events(
            &pane_id,
            &[TerminalOscEvent::ManagedShell {
                version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                shell: mez_terminal::ManagedShellAdapter::Bash,
                token: "0123456789abcdef0123456789abcdef".to_string(),
                event: mez_terminal::ManagedShellProtocolEvent::ParentReady {
                    marker: identity_marker,
                    outcome: mez_terminal::ManagedShellParentOutcome::Completed,
                    exit_code: 0,
                    proof: None,
                },
            }],
        )
        .unwrap();

    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("bootstrapping-child")
    );
    let child_token = service
        .foreign_bash_child_token_for_tests(&pane_id)
        .expect("foreign Bash child staging must allocate a fresh token")
        .to_string();
    assert_ne!(child_token, "0123456789abcdef0123456789abcdef");
    let bootstrap_marker = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            (transaction.kind == RunningShellTransactionKind::Bootstrap).then(|| marker.clone())
        })
        .expect("foreign Bash child staging must register bootstrap");
    let launch_effects = service.drain_pane_io_transition().side_effects;
    let launch_inputs = pane_input_effects(&launch_effects);
    assert_eq!(launch_inputs.len(), 1);
    let launch_input = String::from_utf8_lossy(launch_inputs[0].pane_input_parts().1);
    assert!(
        launch_input.starts_with(&format!(
            "\u{7}MEZ_BASH_RX2_BEGIN 0123456789abcdef0123456789abcdef {bootstrap_marker} "
        )),
        "{launch_input:?}"
    );
    assert!(!launch_input.contains(&child_token), "{launch_input:?}");
    assert!(
        !launch_input.contains("MEZ_BASH_USER_RCFILE"),
        "{launch_input:?}"
    );
    let staging_source = service
        .foreign_bash_child_staging_source_for_tests(&pane_id)
        .expect("foreign Bash child staging source should be retained");
    assert!(staging_source.contains("umask 077"), "{staging_source}");
    assert!(staging_source.contains("--rcfile"), "{staging_source}");
    assert!(
        staging_source.contains("command rm -rf --"),
        "{staging_source}"
    );
    assert!(staging_source.contains(&child_token), "{staging_source}");
    assert!(
        !staging_source.contains(".mez-bash-_1-"),
        "{staging_source}"
    );

    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ManagedShell {
                    version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                    shell: mez_terminal::ManagedShellAdapter::Bash,
                    token: "0123456789abcdef0123456789abcdef".to_string(),
                    event: mez_terminal::ManagedShellProtocolEvent::FrameAdmitted {
                        marker: bootstrap_marker.clone(),
                    },
                }],
            )
            .unwrap(),
        1
    );
    assert_eq!(
        pane_input_effects(&service.drain_pane_io_transition().side_effects).len(),
        1,
        "parent frame admission must release exactly one staged RX2 payload"
    );
    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ManagedShell {
                    version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                    shell: mez_terminal::ManagedShellAdapter::Bash,
                    token: child_token.clone(),
                    event: mez_terminal::ManagedShellProtocolEvent::ChildInstalled {
                        marker: bootstrap_marker.clone(),
                    },
                }],
            )
            .unwrap(),
        1
    );
    assert!(service.agent_subshell_is_active(&pane_id));
    assert_eq!(
        pane_input_effects(&service.drain_pane_io_transition().side_effects).len(),
        1,
        "child installation must release exactly one deferred bootstrap wrapper"
    );
    let bootstrap_turn_id = service
        .running_shell_transactions_for_tests()
        .get(&bootstrap_marker)
        .expect("foreign Bash bootstrap should remain registered")
        .turn_id
        .clone();
    service
        .observe_agent_shell_transaction_start(
            &pane_id,
            &bootstrap_marker,
            &bootstrap_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
        )
        .unwrap();
    let (start_instance, start_observation_id) = service
        .drain_pane_io_transition()
        .side_effects
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PaneProcessIo {
                instance,
                effect:
                    crate::runtime::PaneProcessIoEffect::ObserveForegroundProcess {
                        observation_id,
                        expected_process_group_id: None,
                    },
            } => Some((instance, observation_id)),
            _ => None,
        })
        .expect("foreign Bash bootstrap start must request correlated foreground proof");
    service
        .apply_pane_foreground_process_observation_transition(
            start_instance,
            crate::runtime::PaneForegroundProcessObservation {
                observation_id: start_observation_id,
                process_name: Some("ssh".to_string()),
                process_group_id: Some(primary_pid.saturating_add(1)),
                current_working_directory: Some("/remote/project".to_string()),
                error: None,
            },
        )
        .unwrap();
    service.drain_pane_io_transition();
    let bootstrap_output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\thost\tforeign-host\n\
env\tuser\tforeign-user\n\
env\tshell_path\t/bin/bash\n\
env\tshell_class\tbash\n\
env\tpath\t/usr/bin:/bin\n\
env\tcwd\t/remote/project\n\
env\tgit_repo\t0\n\
bootstrap\tcomplete\t1714500000\n";
    let transaction = service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&bootstrap_marker)
        .unwrap();
    transaction.observed_output_bytes = bootstrap_output.len();
    transaction.observed_output_preview = bootstrap_output.to_string();
    service
        .observe_agent_shell_transaction_end(
            &pane_id,
            &bootstrap_marker,
            &bootstrap_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
            0,
        )
        .unwrap();
    service
        .observe_agent_shell_transaction_events(
            &pane_id,
            &[TerminalOscEvent::ManagedShell {
                version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                shell: mez_terminal::ManagedShellAdapter::Bash,
                token: child_token.clone(),
                event: mez_terminal::ManagedShellProtocolEvent::ParentReady {
                    marker: bootstrap_marker.clone(),
                    outcome: mez_terminal::ManagedShellParentOutcome::Completed,
                    exit_code: 0,
                    proof: None,
                },
            }],
        )
        .unwrap();
    let (completion_instance, completion_observation_id, expected_process_group_id) = service
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
        .expect("foreign Bash bootstrap completion must request correlated foreground proof");
    assert_eq!(
        expected_process_group_id,
        Some(primary_pid.saturating_add(1))
    );
    service
        .apply_pane_foreground_process_observation_transition(
            completion_instance,
            crate::runtime::PaneForegroundProcessObservation {
                observation_id: completion_observation_id,
                process_name: Some("ssh".to_string()),
                process_group_id: Some(primary_pid.saturating_add(1)),
                current_working_directory: Some("/remote/project".to_string()),
                error: None,
            },
        )
        .unwrap();

    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("certified")
    );
    assert!(!service.pane_has_uncertified_foreign_shell_boundary(&pane_id));
    assert!(service.pane_environment_signature(&pane_id).is_some());
    assert!(service.shell_execution_identity_for_pane(&pane_id).is_ok());
    assert!(
        service
            .foreign_bash_child_staging_source_for_tests(&pane_id)
            .is_none(),
        "certification should discard retained staging source after the child owns the pane"
    );

    service
        .apply_pane_foreground_process_event(&pane_id, "sh", primary_pid, None)
        .unwrap();
    assert!(!service.pane_has_uncertified_foreign_shell_boundary(&pane_id));
    assert!(service.pane_bootstrap_is_pending_for_tests(&pane_id));

    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies an authenticated foreign Bash parent failure terminates child
/// bootstrap immediately instead of leaving the pane in its bounded waiting
/// phase. The failure must settle a deferred provider turn, discard every
/// child-owned payload and token, release the transaction input lease, and
/// reject a stale child-installed event without reviving the boundary.
#[test]
fn runtime_foreign_bash_staging_failure_settles_bootstrap_and_pending_turn() {
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
    let primary_pid = service.pane_processes().primary_pid(&pane_id).unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test(&pane_id, None);
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    service
        .apply_pane_foreground_process_event(&pane_id, "ssh", primary_pid.saturating_add(1), None)
        .unwrap();
    service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    service.drain_pane_io_transition();

    let started = service
        .start_agent_prompt_turn(&pane_id, "list the current directory")
        .unwrap();
    let agent_id = AgentId::opaque(started.agent_id).unwrap();
    assert!(
        service
            .claim_configured_agent_provider_task(&agent_id, &started.turn_id)
            .unwrap()
            .is_none(),
        "provider dispatch must wait for the bounded foreign bootstrap"
    );

    service
        .observe_agent_shell_transaction_events(&pane_id, &[TerminalOscEvent::ShellPromptEnd])
        .unwrap();
    service
        .observe_agent_shell_transaction_events(
            &pane_id,
            &[TerminalOscEvent::ManagedShell {
                version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                shell: mez_terminal::ManagedShellAdapter::Bash,
                token: "0123456789abcdef0123456789abcdef".to_string(),
                event: mez_terminal::ManagedShellProtocolEvent::ForeignAdapterCandidate {
                    instance_id: "remote-bash-failure".to_string(),
                    trigger: None,
                },
            }],
        )
        .unwrap();
    let challenge = service
        .foreign_shell_bootstrap_challenge_for_tests(&pane_id)
        .unwrap()
        .to_string();
    service.drain_pane_io_transition();
    service
        .observe_agent_shell_transaction_events(
            &pane_id,
            &[TerminalOscEvent::ManagedShell {
                version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                shell: mez_terminal::ManagedShellAdapter::Bash,
                token: "0123456789abcdef0123456789abcdef".to_string(),
                event: mez_terminal::ManagedShellProtocolEvent::ForeignChallengeCompleted {
                    instance_id: "remote-bash-failure".to_string(),
                    challenge,
                },
            }],
        )
        .unwrap();
    service.drain_pane_io_transition();

    let (identity_marker, identity_turn_id) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::ShellIdentityProbe { .. }
            )
            .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .expect("foreign identity probe should be registered");
    service
        .observe_agent_shell_transaction_start(
            &pane_id,
            &identity_marker,
            &identity_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
        )
        .unwrap();
    let identity_output = format!(
        "\u{1e}mez_shell_identity_begin={identity_marker}\n\
         \u{1e}mez_shell_path=/bin/bash\n\
         \u{1e}mez_shell_version=GNU bash, version 5.2\n\
         \u{1e}mez_shell_identity_end={identity_marker}\n"
    );
    let transaction = service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&identity_marker)
        .unwrap();
    transaction.observed_output_bytes = identity_output.len();
    transaction.observed_output_preview = identity_output;
    service
        .observe_agent_shell_transaction_end(
            &pane_id,
            &identity_marker,
            &identity_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
            0,
        )
        .unwrap();
    service
        .observe_agent_shell_transaction_events(
            &pane_id,
            &[TerminalOscEvent::ManagedShell {
                version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                shell: mez_terminal::ManagedShellAdapter::Bash,
                token: "0123456789abcdef0123456789abcdef".to_string(),
                event: mez_terminal::ManagedShellProtocolEvent::ParentReady {
                    marker: identity_marker,
                    outcome: mez_terminal::ManagedShellParentOutcome::Completed,
                    exit_code: 0,
                    proof: None,
                },
            }],
        )
        .unwrap();

    let bootstrap_marker = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            (transaction.kind == RunningShellTransactionKind::Bootstrap).then(|| marker.clone())
        })
        .expect("foreign Bash child staging must register bootstrap");
    let child_token = service
        .foreign_bash_child_token_for_tests(&pane_id)
        .expect("foreign Bash child staging must allocate a fresh token")
        .to_string();
    let parent_proof = service
        .foreign_bash_parent_proof_for_tests(&pane_id)
        .expect("foreign Bash child staging must retain its parent proof")
        .to_string();
    service.drain_pane_io_transition();

    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ManagedShell {
                    version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                    shell: mez_terminal::ManagedShellAdapter::Bash,
                    token: "0123456789abcdef0123456789abcdef".to_string(),
                    event: mez_terminal::ManagedShellProtocolEvent::FrameAdmitted {
                        marker: bootstrap_marker.clone(),
                    },
                }],
            )
            .unwrap(),
        1
    );
    assert_eq!(
        pane_input_effects(&service.drain_pane_io_transition().side_effects).len(),
        1,
        "frame admission must release the staged RX2 source before it can fail"
    );

    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ManagedShell {
                    version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                    shell: mez_terminal::ManagedShellAdapter::Bash,
                    token: "0123456789abcdef0123456789abcdef".to_string(),
                    event: mez_terminal::ManagedShellProtocolEvent::ParentReady {
                        marker: bootstrap_marker.clone(),
                        outcome: mez_terminal::ManagedShellParentOutcome::SourceFailed,
                        exit_code: 23,
                        proof: Some(parent_proof),
                    },
                }],
            )
            .unwrap(),
        1
    );
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("failed")
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == started.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    assert!(!service.agent_provider_task_is_pending(&started.turn_id));
    assert!(
        !service
            .running_shell_transactions_for_tests()
            .contains_key(&bootstrap_marker),
        "failed staging must remove the bootstrap transaction"
    );
    assert!(
        service
            .foreign_bash_child_token_for_tests(&pane_id)
            .is_none()
    );
    assert!(
        service
            .foreign_bash_child_staging_source_for_tests(&pane_id)
            .is_none()
    );
    let failure_effects = service.drain_pane_io_transition().side_effects;
    assert!(failure_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::PaneProcessIo {
            effect: crate::runtime::PaneProcessIoEffect::ReleaseShellInputLease { owner_id },
            ..
        } if owner_id == &bootstrap_marker
    )));
    assert!(
        pane_input_effects(&failure_effects).is_empty(),
        "known staging failure must not dispatch a deferred bootstrap wrapper"
    );

    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ManagedShell {
                    version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                    shell: mez_terminal::ManagedShellAdapter::Bash,
                    token: child_token,
                    event: mez_terminal::ManagedShellProtocolEvent::ChildInstalled {
                        marker: bootstrap_marker,
                    },
                }],
            )
            .unwrap(),
        0,
        "a stale child-installed event must not revive failed bootstrap"
    );
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("failed")
    );
    assert!(
        pane_input_effects(&service.drain_pane_io_transition().side_effects).is_empty(),
        "stale child installation must not write to the pane"
    );

    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies foreign Fish and Zsh candidates receive only their shell-native,
/// source-free editor challenge before adapter admission.
///
/// Fish uses its bound command-line callback with bounded metadata. Zsh uses
/// the candidate-selected ZLE trigger twice (clear, then receiver) followed by
/// bounded challenge metadata. Neither path may contain generated source.
#[test]
fn runtime_foreign_fish_and_zsh_candidates_receive_native_challenges() {
    for (shell, token, instance_id, trigger, expected_prefix) in [
        (
            mez_terminal::ManagedShellAdapter::Fish,
            "11111111111111111111111111111111",
            "remote-fish-1",
            None,
            "\u{1b}\u{7}MEZ_FISH_FOREIGN_CHALLENGE ",
        ),
        (
            mez_terminal::ManagedShellAdapter::Zsh,
            "22222222222222222222222222222222",
            "remote-zsh-1",
            Some("escape-m".to_string()),
            "\u{1b}[27;9;109~\u{1b}[27;9;109~MEZ_ZSH_FOREIGN_CHALLENGE ",
        ),
    ] {
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
        let primary_pid = service.pane_processes().primary_pid(&pane_id).unwrap();
        service
            .pane_processes_mut()
            .set_foreground_process_group_id_for_test(&pane_id, None);
        let mut process = service
            .take_running_pane_process_for_adapter(&pane_id)
            .unwrap();
        service
            .apply_pane_foreground_process_event(
                &pane_id,
                "ssh",
                primary_pid.saturating_add(1),
                None,
            )
            .unwrap();
        service
            .execute_terminal_command(&primary, "agent-shell")
            .unwrap();
        service.drain_pane_io_transition();
        service
            .observe_agent_shell_transaction_events(&pane_id, &[TerminalOscEvent::ShellPromptEnd])
            .unwrap();

        assert_eq!(
            service
                .observe_agent_shell_transaction_events(
                    &pane_id,
                    &[TerminalOscEvent::ManagedShell {
                        version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                        shell,
                        token: token.to_string(),
                        event: mez_terminal::ManagedShellProtocolEvent::ForeignAdapterCandidate {
                            instance_id: instance_id.to_string(),
                            trigger,
                        },
                    }],
                )
                .unwrap(),
            1
        );
        assert_eq!(
            service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
            Some("challenging-adapter")
        );
        let challenge = service
            .foreign_shell_bootstrap_challenge_for_tests(&pane_id)
            .unwrap()
            .to_string();
        let effects = service.drain_pane_io_transition().side_effects;
        let inputs = pane_input_effects(&effects);
        assert_eq!(inputs.len(), 1);
        let input = String::from_utf8_lossy(inputs[0].pane_input_parts().1);
        assert!(input.starts_with(expected_prefix), "{shell:?}: {input:?}");
        assert!(input.contains(token), "{shell:?}: {input:?}");
        assert!(input.contains(instance_id), "{shell:?}: {input:?}");
        assert!(input.contains(&challenge), "{shell:?}: {input:?}");
        assert!(
            !input.contains("mez_shell_identity_begin="),
            "{shell:?}: {input:?}"
        );

        assert_eq!(
            service
                .observe_agent_shell_transaction_events(
                    &pane_id,
                    &[TerminalOscEvent::ManagedShell {
                        version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                        shell,
                        token: token.to_string(),
                        event: mez_terminal::ManagedShellProtocolEvent::ForeignChallengeCompleted {
                            instance_id: instance_id.to_string(),
                            challenge,
                        },
                    }],
                )
                .unwrap(),
            1
        );
        assert_eq!(
            service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
            Some("identity-probing")
        );
        assert_eq!(
            service
                .running_shell_transactions_for_tests()
                .values()
                .filter(|transaction| matches!(
                    transaction.kind,
                    RunningShellTransactionKind::ShellIdentityProbe { .. }
                ))
                .count(),
            1
        );
        let identity_effects = service.drain_pane_io_transition().side_effects;
        let identity_inputs = pane_input_effects(&identity_effects);
        assert_eq!(identity_inputs.len(), 1);
        let identity_input = String::from_utf8_lossy(identity_inputs[0].pane_input_parts().1);
        match shell {
            mez_terminal::ManagedShellAdapter::Fish => assert!(
                identity_input.starts_with(&format!("\u{1b}\u{7}MEZ_FISH_RX1_HOLD {token} ")),
                "{identity_input:?}"
            ),
            mez_terminal::ManagedShellAdapter::Zsh => assert_eq!(
                identity_input, "\u{1b}[27;9;109~",
                "Zsh identity discovery must start with only the admitted ZLE trigger"
            ),
            mez_terminal::ManagedShellAdapter::Bash => unreachable!(),
        }
        assert!(
            !identity_input.contains("mez_shell_identity_begin="),
            "{identity_input:?}"
        );

        let (identity_marker, identity_turn_id) = service
            .running_shell_transactions_for_tests()
            .iter()
            .find_map(|(marker, transaction)| {
                matches!(
                    transaction.kind,
                    RunningShellTransactionKind::ShellIdentityProbe { .. }
                )
                .then(|| (marker.clone(), transaction.turn_id.clone()))
            })
            .expect("foreign Fish/Zsh identity probe should be registered");
        match shell {
            mez_terminal::ManagedShellAdapter::Fish => {
                for event in [
                    mez_terminal::ManagedShellProtocolEvent::EditorClearRequested {
                        marker: Some(identity_marker.clone()),
                    },
                    mez_terminal::ManagedShellProtocolEvent::EditorCleared {
                        marker: Some(identity_marker.clone()),
                    },
                    mez_terminal::ManagedShellProtocolEvent::EditorHeld {
                        marker: identity_marker.clone(),
                    },
                    mez_terminal::ManagedShellProtocolEvent::FrameAdmitted {
                        marker: identity_marker.clone(),
                    },
                ] {
                    assert_eq!(
                        service
                            .observe_agent_shell_transaction_events(
                                &pane_id,
                                &[TerminalOscEvent::ManagedShell {
                                    version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                                    shell,
                                    token: token.to_string(),
                                    event,
                                }],
                            )
                            .unwrap(),
                        1
                    );
                    service.drain_pane_io_transition();
                }
            }
            mez_terminal::ManagedShellAdapter::Zsh => {
                for event in [
                    mez_terminal::ManagedShellProtocolEvent::EditorCleared { marker: None },
                    mez_terminal::ManagedShellProtocolEvent::ReceiverAwaiting,
                    mez_terminal::ManagedShellProtocolEvent::EditorHeld {
                        marker: identity_marker.clone(),
                    },
                    mez_terminal::ManagedShellProtocolEvent::FrameAdmitted {
                        marker: identity_marker.clone(),
                    },
                ] {
                    assert_eq!(
                        service
                            .observe_agent_shell_transaction_events(
                                &pane_id,
                                &[TerminalOscEvent::ManagedShell {
                                    version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                                    shell,
                                    token: token.to_string(),
                                    event,
                                }],
                            )
                            .unwrap(),
                        1
                    );
                    service.drain_pane_io_transition();
                }
            }
            mez_terminal::ManagedShellAdapter::Bash => unreachable!(),
        }
        service
            .observe_agent_shell_transaction_start(
                &pane_id,
                &identity_marker,
                &identity_turn_id,
                &format!("agent-{pane_id}"),
                &pane_id,
            )
            .unwrap();
        let (shell_path, shell_version) = match shell {
            mez_terminal::ManagedShellAdapter::Fish => ("/usr/bin/fish", "fish, version 3.7"),
            mez_terminal::ManagedShellAdapter::Zsh => ("/bin/zsh", "zsh 5.9"),
            mez_terminal::ManagedShellAdapter::Bash => unreachable!(),
        };
        let identity_output = format!(
            "\u{1e}mez_shell_identity_begin={identity_marker}\n\
             \u{1e}mez_shell_path={shell_path}\n\
             \u{1e}mez_shell_version={shell_version}\n\
             \u{1e}mez_shell_identity_end={identity_marker}\n"
        );
        let transaction = service
            .running_shell_transactions_mut_for_tests()
            .get_mut(&identity_marker)
            .unwrap();
        transaction.observed_output_bytes = identity_output.len();
        transaction.observed_output_preview = identity_output;
        service
            .observe_agent_shell_transaction_end(
                &pane_id,
                &identity_marker,
                &identity_turn_id,
                &format!("agent-{pane_id}"),
                &pane_id,
                0,
            )
            .unwrap();
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ManagedShell {
                    version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                    shell,
                    token: token.to_string(),
                    event: mez_terminal::ManagedShellProtocolEvent::ParentReady {
                        marker: identity_marker,
                        outcome: mez_terminal::ManagedShellParentOutcome::Completed,
                        exit_code: 0,
                        proof: None,
                    },
                }],
            )
            .unwrap();

        assert_eq!(
            service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
            Some("bootstrapping-child")
        );
        let child_token = service
            .foreign_child_token_for_tests(&pane_id)
            .expect("foreign Fish/Zsh child staging must allocate a fresh token")
            .to_string();
        assert_ne!(child_token, token);
        let staging_source = service
            .foreign_child_staging_source_for_tests(&pane_id)
            .expect("foreign Fish/Zsh child staging source should be retained");
        assert!(staging_source.contains(&child_token), "{staging_source}");
        assert!(
            !staging_source.contains("host-or-stale-token"),
            "{staging_source}"
        );
        match shell {
            mez_terminal::ManagedShellAdapter::Fish => {
                assert!(
                    staging_source.contains("--init-command"),
                    "{staging_source}"
                );
            }
            mez_terminal::ManagedShellAdapter::Zsh => {
                assert!(
                    staging_source.contains("MEZ_FOREIGN_ZSH_DIR='/tmp/.mez-zsh-"),
                    "{staging_source}"
                );
                assert!(
                    staging_source.contains("command rm -rf --"),
                    "{staging_source}"
                );
            }
            mez_terminal::ManagedShellAdapter::Bash => unreachable!(),
        }

        let bootstrap_marker = service
            .running_shell_transactions_for_tests()
            .iter()
            .find_map(|(marker, transaction)| {
                (transaction.kind == RunningShellTransactionKind::Bootstrap).then(|| marker.clone())
            })
            .expect("foreign Fish/Zsh child staging must register bootstrap");
        let parent_events = match shell {
            mez_terminal::ManagedShellAdapter::Fish => vec![
                mez_terminal::ManagedShellProtocolEvent::EditorClearRequested {
                    marker: Some(bootstrap_marker.clone()),
                },
                mez_terminal::ManagedShellProtocolEvent::EditorCleared {
                    marker: Some(bootstrap_marker.clone()),
                },
                mez_terminal::ManagedShellProtocolEvent::EditorHeld {
                    marker: bootstrap_marker.clone(),
                },
                mez_terminal::ManagedShellProtocolEvent::FrameAdmitted {
                    marker: bootstrap_marker.clone(),
                },
            ],
            mez_terminal::ManagedShellAdapter::Zsh => vec![
                mez_terminal::ManagedShellProtocolEvent::EditorCleared { marker: None },
                mez_terminal::ManagedShellProtocolEvent::ReceiverAwaiting,
                mez_terminal::ManagedShellProtocolEvent::EditorHeld {
                    marker: bootstrap_marker.clone(),
                },
                mez_terminal::ManagedShellProtocolEvent::FrameAdmitted {
                    marker: bootstrap_marker.clone(),
                },
            ],
            mez_terminal::ManagedShellAdapter::Bash => unreachable!(),
        };
        for event in parent_events {
            assert_eq!(
                service
                    .observe_agent_shell_transaction_events(
                        &pane_id,
                        &[TerminalOscEvent::ManagedShell {
                            version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                            shell,
                            token: token.to_string(),
                            event,
                        }],
                    )
                    .unwrap(),
                1
            );
            service.drain_pane_io_transition();
        }
        assert_eq!(
            service
                .observe_agent_shell_transaction_events(
                    &pane_id,
                    &[TerminalOscEvent::ManagedShell {
                        version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                        shell,
                        token: token.to_string(),
                        event: mez_terminal::ManagedShellProtocolEvent::ChildInstalled {
                            marker: bootstrap_marker.clone(),
                        },
                    }],
                )
                .unwrap(),
            0,
            "the admitted parent token must not authenticate child installation"
        );
        assert!(
            pane_input_effects(&service.drain_pane_io_transition().side_effects).is_empty(),
            "a forged child installation must not release the bootstrap wrapper"
        );
        assert_eq!(
            service
                .observe_agent_shell_transaction_events(
                    &pane_id,
                    &[TerminalOscEvent::ManagedShell {
                        version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                        shell,
                        token: child_token.clone(),
                        event: mez_terminal::ManagedShellProtocolEvent::ChildInstalled {
                            marker: bootstrap_marker.clone(),
                        },
                    }],
                )
                .unwrap(),
            1
        );
        assert!(service.agent_subshell_is_active(&pane_id));
        assert_eq!(
            pane_input_effects(&service.drain_pane_io_transition().side_effects).len(),
            1,
            "child installation must release exactly one deferred bootstrap wrapper"
        );

        let bootstrap_turn_id = service
            .running_shell_transactions_for_tests()
            .get(&bootstrap_marker)
            .expect("foreign Fish/Zsh bootstrap should remain registered")
            .turn_id
            .clone();
        service
            .observe_agent_shell_transaction_start(
                &pane_id,
                &bootstrap_marker,
                &bootstrap_turn_id,
                &format!("agent-{pane_id}"),
                &pane_id,
            )
            .unwrap();
        let (start_instance, start_observation_id) = service
            .drain_pane_io_transition()
            .side_effects
            .into_iter()
            .find_map(|effect| match effect {
                RuntimeSideEffect::PaneProcessIo {
                    instance,
                    effect:
                        crate::runtime::PaneProcessIoEffect::ObserveForegroundProcess {
                            observation_id,
                            expected_process_group_id: None,
                        },
                } => Some((instance, observation_id)),
                _ => None,
            })
            .expect("foreign Fish/Zsh bootstrap start must request foreground proof");
        service
            .apply_pane_foreground_process_observation_transition(
                start_instance,
                crate::runtime::PaneForegroundProcessObservation {
                    observation_id: start_observation_id,
                    process_name: Some("ssh".to_string()),
                    process_group_id: Some(primary_pid.saturating_add(1)),
                    current_working_directory: Some("/remote/project".to_string()),
                    error: None,
                },
            )
            .unwrap();
        service.drain_pane_io_transition();
        if shell == mez_terminal::ManagedShellAdapter::Fish {
            service
                .observe_shell_transaction_payload_receiver_ready(
                    &pane_id,
                    &bootstrap_marker,
                    &bootstrap_turn_id,
                    &format!("agent-{pane_id}"),
                    &pane_id,
                )
                .unwrap();
            service.drain_pane_io_transition();
        }
        let shell_class = match shell {
            mez_terminal::ManagedShellAdapter::Fish => "fish",
            mez_terminal::ManagedShellAdapter::Zsh => "zsh",
            mez_terminal::ManagedShellAdapter::Bash => unreachable!(),
        };
        let bootstrap_output = format!(
            "env\tos\tLinux\n\
             env\tarch\tx86_64\n\
             env\thost\tforeign-host\n\
             env\tuser\tforeign-user\n\
             env\tshell_path\t{shell_path}\n\
             env\tshell_class\t{shell_class}\n\
             env\tpath\t/usr/bin:/bin\n\
             env\tcwd\t/remote/project\n\
             env\tgit_repo\t0\n\
             bootstrap\tcomplete\t1714500000\n"
        );
        let transaction = service
            .running_shell_transactions_mut_for_tests()
            .get_mut(&bootstrap_marker)
            .unwrap();
        transaction.observed_output_bytes = bootstrap_output.len();
        transaction.observed_output_preview = bootstrap_output;
        service
            .observe_agent_shell_transaction_end(
                &pane_id,
                &bootstrap_marker,
                &bootstrap_turn_id,
                &format!("agent-{pane_id}"),
                &pane_id,
                0,
            )
            .unwrap();
        let (completion_instance, completion_observation_id, expected_process_group_id) = service
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
            .expect("foreign Fish/Zsh bootstrap completion must request foreground proof");
        assert_eq!(
            expected_process_group_id,
            Some(primary_pid.saturating_add(1))
        );
        service
            .apply_pane_foreground_process_observation_transition(
                completion_instance,
                crate::runtime::PaneForegroundProcessObservation {
                    observation_id: completion_observation_id,
                    process_name: Some("ssh".to_string()),
                    process_group_id: Some(primary_pid.saturating_add(1)),
                    current_working_directory: Some("/remote/project".to_string()),
                    error: None,
                },
            )
            .unwrap();
        assert_eq!(
            service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
            Some("certified")
        );
        assert!(!service.pane_has_uncertified_foreign_shell_boundary(&pane_id));
        assert!(service.pane_environment_signature(&pane_id).is_some());
        assert!(service.shell_execution_identity_for_pane(&pane_id).is_ok());
        assert!(
            service
                .foreign_child_staging_source_for_tests(&pane_id)
                .is_none(),
            "certification should discard retained staging source"
        );

        service
            .apply_pane_foreground_process_event(&pane_id, "sh", primary_pid, None)
            .unwrap();
        assert!(!service.pane_has_uncertified_foreign_shell_boundary(&pane_id));
        assert!(service.pane_bootstrap_is_pending_for_tests(&pane_id));

        let _ = process.terminate(Duration::from_millis(10));
    }
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
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .write_input_to_pane(&primary, Some("%1"), b"PS1='parent$ '; export PS1\n")
        .unwrap();
    let prompt_column = "parent$ ".chars().count();
    let mut initial_screen = String::new();
    for _ in 0..200 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        let screen = service.pane_screen("%1").unwrap();
        initial_screen = screen.visible_lines().join("\n");
        if initial_screen.contains("parent$") && screen.cursor_state().column == prompt_column {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        initial_screen.contains("parent$"),
        "parent prompt did not arrive: {initial_screen:?}"
    );
    let parent_content_before_agent = service
        .process_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let parent_cursor_before_agent = service.process_pane_screen("%1").unwrap().cursor_state();

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

    for _ in 0..300 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service.pane_foreground_certified_shell_state("%1") == Some(true) {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }

    assert_eq!(
        service
            .process_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n"),
        parent_content_before_agent,
        "agent exit must restore the exact process presentation retained at entry"
    );
    assert_eq!(
        service.process_pane_screen("%1").unwrap().cursor_state(),
        parent_cursor_before_agent,
        "agent exit must restore the exact process cursor retained at entry"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a managed Bash pane can leave agent mode, execute ordinary parent
/// shell commands, and re-enter agent mode through a real identity probe.
///
/// This exercises the PTY, private Bash receiver, transaction output parser,
/// and deferred re-entry path together. State-only probe fixtures cannot catch
/// a receiver or prompt-boundary failure that drops the identity frame.
#[test]
fn runtime_agent_shell_reentry_after_parent_bash_commands_completes_identity_probe() {
    let shell_path = PathBuf::from("/bin/bash");
    let shell_available = fs::metadata(&shell_path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false);
    if !shell_available {
        eprintln!("skipping live Bash re-entry regression because /bin/bash is unavailable");
        return;
    }
    let root = temp_root("agent-shell-bash-reentry");
    let mut service = RuntimeSessionService::with_event_log(
        Session::new_default(
            ResolvedShell::new(shell_path, ShellSource::ShellEnv),
            Size::new(80, 24).unwrap(),
        ),
        root.join("control.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    *service.host_clipboard_mut_for_tests() = HostClipboard::disabled();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .write_input_to_pane(
            &primary,
            Some("%1"),
            b"PROMPT_COMMAND=; PS1='parent$ '; export PS1; printf '__MEZ_PARENT_PROMPT_INSTALLED__\\n'\n",
        )
        .unwrap();
    let mut parent_prompt_installed = false;
    for _ in 0..200 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        let screen = service.process_pane_screen("%1").unwrap();
        if screen
            .visible_lines()
            .join("\n")
            .contains("__MEZ_PARENT_PROMPT_INSTALLED__")
            && screen.cursor_state().column == "parent$ ".chars().count()
        {
            parent_prompt_installed = true;
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        parent_prompt_installed,
        "managed Bash did not display the configured parent prompt; screen={:?}; cursor={:?}",
        service.process_pane_screen("%1").unwrap().visible_lines(),
        service.process_pane_screen("%1").unwrap().cursor_state()
    );

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    let mut first_bootstrap_completed = false;
    for _ in 0..400 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service.pane_environment_signature("%1").is_some()
            && !service.pane_bootstrap_is_pending_for_tests("%1")
        {
            first_bootstrap_completed = true;
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        first_bootstrap_completed,
        "initial Bash agent-subshell bootstrap did not complete; authority={:?}",
        service.pane_environment_authority("%1")
    );

    let hide = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(hide.contains("visibility=hidden"), "{hide}");
    for _ in 0..200 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service.pane_foreground_certified_shell_state("%1") == Some(true) {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert_eq!(
        service.pane_foreground_certified_shell_state("%1"),
        Some(true),
        "parent Bash did not regain the foreground after agent exit"
    );
    let pane_log_after_exit = service
        .process_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_log_after_exit
            .lines()
            .any(|line| line.trim() == "exit"),
        "managed Bash child-shell exit must not enter the pane log: {pane_log_after_exit:?}"
    );
    let parent_prompt_column = "parent$ ".chars().count();
    let mut restored_prompt_cursor = None;
    for _ in 0..200 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        let screen = service.process_pane_screen("%1").unwrap();
        if screen.visible_lines().join("\n").contains("parent$")
            && screen.cursor_state().column == parent_prompt_column
        {
            restored_prompt_cursor = Some(screen.cursor_state().column);
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert_eq!(
        restored_prompt_cursor,
        Some(parent_prompt_column),
        "managed Bash must leave the cursor immediately after its restored prompt; screen={:?}; cursor={:?}",
        service.process_pane_screen("%1").unwrap().visible_lines(),
        service.process_pane_screen("%1").unwrap().cursor_state()
    );
    for _ in 0..50 {
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
        let _ = service.poll_pane_outputs(8192).unwrap();
    }
    assert_eq!(
        service
            .process_pane_screen("%1")
            .unwrap()
            .cursor_state()
            .column,
        parent_prompt_column,
        "managed Bash must keep the cursor after its restored prompt once delayed exit output settles; screen={:?}; cursor={:?}",
        service.process_pane_screen("%1").unwrap().visible_lines(),
        service.process_pane_screen("%1").unwrap().cursor_state()
    );

    service
        .write_input_to_pane(
            &primary,
            Some("%1"),
            b"printf '__MEZ_PARENT_COMMAND_ONE__\\n'; printf '__MEZ_PARENT_COMMAND_TWO__\\n'\n",
        )
        .unwrap();
    let mut parent_commands_completed = false;
    for _ in 0..200 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        let screen = service
            .process_pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n");
        if screen.contains("__MEZ_PARENT_COMMAND_ONE__")
            && screen.contains("__MEZ_PARENT_COMMAND_TWO__")
        {
            parent_commands_completed = true;
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        parent_commands_completed,
        "parent Bash commands did not complete"
    );

    service.set_pane_readiness("%1", PaneReadinessState::PromptCandidate);
    let show_again = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show_again.contains("visibility=visible"), "{show_again}");
    let mut reentry_completed = false;
    for _ in 0..500 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service.agent_subshell_is_active("%1")
            && service.pane_environment_signature("%1").is_some()
            && !service.pane_bootstrap_is_pending_for_tests("%1")
        {
            reentry_completed = true;
            break;
        }
        if matches!(
            service.pane_environment_authority("%1"),
            RuntimePaneEnvironmentAuthority::Unavailable(_)
        ) {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    let diagnostic_events =
        service
            .event_log()
            .unwrap()
            .replay_after_for(&EventAudience::Primary, 0, 4096);
    let process_screen = service
        .process_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        reentry_completed,
        "Bash agent-shell re-entry did not complete; authority={:?}; readiness={:?}; transactions={:?}; events={diagnostic_events:?}; process_screen={process_screen:?}",
        service.pane_environment_authority("%1"),
        service.pane_readiness_state("%1"),
        service.running_shell_transactions_for_tests()
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
    let mut service = RuntimeSessionService::with_event_log(
        test_session(),
        PathBuf::from("/tmp/mez-agent-shell-exit.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
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
    assert!(service.agent_subshell_is_active(&pane_id));
    service
        .agent_pane_screen_mut(&pane_id)
        .unwrap()
        .feed(b"slash exit history\r\nslash exit visible text");
    assert!(
        service
            .agent_pane_screen(&pane_id)
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("slash exit visible text")
    );
    let last_event_id = service.event_log().unwrap().latest_event_id();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-exit","method":"agent/shell/command","params":{"idempotency_key":"agent-exit","input":"/exit"}}"#,
        &primary,
    );
    assert!(response.contains(r#""visibility":"hidden""#), "{response}");
    let exit_events =
        service
            .event_log()
            .unwrap()
            .replay_after_for(&EventAudience::Primary, last_event_id, 10);
    assert!(
        exit_events
            .iter()
            .all(|event| !event.payload.contains(r#""agent_shell_command":"/exit""#)),
        "{exit_events:?}"
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
    assert!(!service.agent_subshell_is_active(&pane_id));
    let after_exit_screen = service.agent_pane_screen(&pane_id).unwrap();
    assert!(
        after_exit_screen
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
    let pane_text_before_exit = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");

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
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert_eq!(pane_text, pane_text_before_exit, "{pane_text}");
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
