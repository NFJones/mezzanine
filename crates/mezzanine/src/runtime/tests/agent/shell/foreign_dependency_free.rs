//! Dependency-free foreign-shell bootstrap regressions.

use super::*;

/// Verifies pane-write progress for a correlated foreign identity probe still
/// refreshes both its transaction timeout and foreign phase idle deadline.
///
/// Primary bootstrap progress no longer depends on foreign boundary state, but
/// the existing foreign lifecycle must retain its separate phase-level clock.
#[test]
fn runtime_foreign_identity_input_progress_refreshes_both_timeout_clocks() {
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

    let identity_marker = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::ShellIdentityProbe { .. }
            )
            .then(|| marker.clone())
        })
        .expect("dependency-free identity probe should be registered");
    service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&identity_marker)
        .unwrap()
        .started_at_unix_ms = 1;

    assert!(
        service
            .apply_pane_input_written_event(&pane_id, 4096)
            .unwrap()
    );

    let transaction_started_at_unix_ms = service
        .running_shell_transactions_for_tests()
        .get(&identity_marker)
        .unwrap()
        .started_at_unix_ms;
    assert!(transaction_started_at_unix_ms > 1);
    assert_eq!(
        service
            .foreign_shell_bootstrap_phase_started_at_for_tests(&pane_id)
            .unwrap(),
        transaction_started_at_unix_ms
    );
    process.terminate(Duration::from_millis(100)).unwrap();
}

/// Verifies malformed and nonzero dependency-free identity completions fail
/// before allocating any loader or bootstrap ownership. The identity command
/// is the sole pre-loader semantic boundary, so failed evidence must not leave
/// a child command queued behind generic terminal activity.
#[test]
fn runtime_dependency_free_identity_failure_dispatches_no_loader() {
    for (label, exit_code) in [("malformed", 0), ("nonzero", 19)] {
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
            .expect("dependency-free identity probe should be registered");
        service
            .observe_agent_shell_transaction_start(
                &pane_id,
                &identity_marker,
                &identity_turn_id,
                &format!("agent-{pane_id}"),
                &pane_id,
            )
            .unwrap();
        let output = if label == "nonzero" {
            format!(
                "\u{1e}mez_shell_identity_begin={identity_marker}\n\
                 \u{1e}mez_shell_path=/bin/bash\n\
                 \u{1e}mez_shell_version=GNU bash, version 5.2\n\
                 \u{1e}mez_shell_identity_end={identity_marker}\n"
            )
        } else {
            "malformed identity output".to_string()
        };
        let transaction = service
            .running_shell_transactions_mut_for_tests()
            .get_mut(&identity_marker)
            .unwrap();
        transaction.observed_output_bytes = output.len();
        transaction.observed_output_preview = output;

        service
            .observe_agent_shell_transaction_end(
                &pane_id,
                &identity_marker,
                &identity_turn_id,
                &format!("agent-{pane_id}"),
                &pane_id,
                exit_code,
            )
            .unwrap();

        assert_eq!(
            service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
            Some("failed"),
            "{label} identity completion must fail the boundary"
        );
        assert!(
            service
                .foreign_shell_loader_marker_for_tests(&pane_id)
                .is_none(),
            "{label} identity completion must not allocate a loader"
        );
        assert!(
            service
                .running_shell_transactions_for_tests()
                .values()
                .all(|transaction| transaction.kind != RunningShellTransactionKind::Bootstrap),
            "{label} identity completion must not register bootstrap ownership"
        );
        assert!(pane_input_effects(&service.drain_pane_io_transition().side_effects).is_empty());
        process.terminate(Duration::from_millis(100)).unwrap();
    }
}

/// Verifies a Nushell identity result settles pane mode with an actionable
/// visible error rather than proceeding into the POSIX-only child bootstrap.
/// The probe itself is the last dialect-independent boundary, so no loader or
/// bootstrap transaction may be created after it identifies an unsupported shell.
#[test]
fn runtime_dependency_free_nushell_identity_reports_native_mode_error() {
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
        .expect("dependency-free identity probe should be registered");
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
         \u{1e}mez_shell_path=/usr/bin/nu\n\
         \u{1e}mez_shell_version=0.103.0\n\
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

    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("failed")
    );
    assert_eq!(
        service.pane_environment_authority(&pane_id),
        crate::runtime::processes::RuntimePaneEnvironmentAuthority::Unavailable(
            crate::runtime::processes::RuntimePaneEnvironmentAuthorityUnavailableReason::UnsupportedShell,
        )
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .all(|transaction| transaction.pane_id != pane_id),
        "unsupported shell identity must not retain bootstrap work"
    );
    assert!(
        service
            .agent_pane_screen(&pane_id)
            .unwrap()
            .normal_content_lines()
            .iter()
            .any(|line| line.contains(
                "pane shell mode does not support /usr/bin/nu; select native shell mode"
            ))
    );
    assert!(pane_input_effects(&service.drain_pane_io_transition().side_effects).is_empty());
    process.terminate(Duration::from_millis(100)).unwrap();
}

/// Verifies a shell-interaction generation change invalidates completed
/// identity evidence before loader launch. Recovery may issue a fresh identity
/// probe for the new epoch, but it must never dispatch a loader rendered from
/// the stale process identity.
#[test]
fn runtime_dependency_free_stale_identity_dispatches_no_loader() {
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
        .expect("dependency-free identity probe should be registered");
    service
        .observe_agent_shell_transaction_start(
            &pane_id,
            &identity_marker,
            &identity_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
        )
        .unwrap();
    let output = format!(
        "\u{1e}mez_shell_identity_begin={identity_marker}\n\
         \u{1e}mez_shell_path=/bin/bash\n\
         \u{1e}mez_shell_version=GNU bash, version 5.2\n\
         \u{1e}mez_shell_identity_end={identity_marker}\n"
    );
    let transaction = service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&identity_marker)
        .unwrap();
    transaction.observed_output_bytes = output.len();
    transaction.observed_output_preview = output;
    service.advance_pane_shell_interaction_generation_for_tests(&pane_id);

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

    assert!(
        service
            .foreign_shell_loader_marker_for_tests(&pane_id)
            .is_none(),
        "stale identity evidence must not allocate a loader"
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .all(|transaction| transaction.kind != RunningShellTransactionKind::Bootstrap),
        "stale identity evidence must not register bootstrap ownership"
    );
    let recovery_transition = service.drain_pane_io_transition();
    let recovery_inputs = pane_input_effects(&recovery_transition.side_effects);
    assert!(recovery_inputs.iter().all(|effect| {
        String::from_utf8_lossy(effect.pane_input_parts().1)
            .lines()
            .filter(|line| line.starts_with("/bin/sh -c "))
            .count()
            <= 1
    }));
    process.terminate(Duration::from_millis(100)).unwrap();
}

/// Verifies explicit agent entry at an ordinary foreign prompt immediately
/// probes shell identity and launches one ephemeral loader without a retained
/// adapter. Generated child source must remain withheld until the loader emits
/// the exact bootstrap marker, and stale or duplicate ready records must not
/// release payload bytes.
#[test]
fn runtime_dependency_free_foreign_bash_loader_is_ready_gated() {
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
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("identity-probing")
    );
    let identity_effects = service.drain_pane_io_transition().side_effects;
    let identity_inputs = pane_input_effects(&identity_effects);
    assert_eq!(identity_inputs.len(), 1);
    let identity_input = String::from_utf8_lossy(identity_inputs[0].pane_input_parts().1);
    assert!(
        identity_input.starts_with("/bin/sh -c "),
        "{identity_input:?}"
    );
    assert_eq!(
        identity_input
            .lines()
            .filter(|line| line.starts_with("/bin/sh -c "))
            .count(),
        1,
        "identity discovery must be the only interactive command in its pane write"
    );
    assert!(
        service
            .foreign_shell_loader_marker_for_tests(&pane_id)
            .is_none(),
        "identity discovery must not allocate loader ownership"
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .all(|transaction| transaction.kind != RunningShellTransactionKind::Bootstrap),
        "identity discovery must not register bootstrap ownership"
    );
    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ForeignShellLoaderReady {
                    marker: "premature-loader".to_string(),
                }],
            )
            .unwrap(),
        0,
        "loader readiness before identity settlement must be ignored"
    );
    assert!(pane_input_effects(&service.drain_pane_io_transition().side_effects).is_empty());

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
        .expect("dependency-free identity probe should be registered");
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
    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ShellTransactionEnd {
                    marker: identity_marker.clone(),
                    turn_id: identity_turn_id.clone(),
                    agent_id: format!("agent-{pane_id}"),
                    pane_id: pane_id.clone(),
                    exit_code: 0,
                }],
            )
            .unwrap(),
        1,
        "identity settlement should observe exactly one transaction end event"
    );
    assert_eq!(
        service.maybe_bootstrap_ready_panes().unwrap(),
        1,
        "the reconciliation pump should launch the dependency-free child loader"
    );

    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("bootstrapping-child")
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| transaction.kind == RunningShellTransactionKind::Bootstrap),
        "dependency-free child bootstrap should be registered after identity settlement"
    );
    let loader_marker = service
        .foreign_shell_loader_marker_for_tests(&pane_id)
        .expect("dependency-free loader should retain its bounded nonce")
        .to_string();
    assert_eq!(loader_marker.len(), 32);
    let launch_effects = service.drain_pane_io_transition().side_effects;
    let launch_inputs = pane_input_effects(&launch_effects);
    assert_eq!(
        launch_inputs.len(),
        1,
        "identity settlement should emit only the separate loader command"
    );
    let loader_command = String::from_utf8_lossy(launch_inputs[0].pane_input_parts().1);
    assert!(
        loader_command.starts_with("/bin/sh -c "),
        "{loader_command:?}"
    );
    assert!(
        loader_command.contains(&loader_marker),
        "{loader_command:?}"
    );
    assert!(loader_command.len() <= 700, "{loader_command:?}");

    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ForeignShellLoaderReady {
                    marker: loader_marker.clone(),
                }],
            )
            .unwrap(),
        1
    );
    let release_effects = service.drain_pane_io_transition().side_effects;
    let release_inputs = pane_input_effects(&release_effects);
    assert_eq!(
        release_inputs.len(),
        1,
        "correlated loader readiness should release only the loader payload before managed child installation"
    );
    let payload = release_inputs
        .iter()
        .map(|effect| String::from_utf8_lossy(effect.pane_input_parts().1))
        .find(|input| input.contains(&format!("MEZ_LOADER_END_{loader_marker}")))
        .expect("one released input should contain the loader payload");
    assert!(payload.contains(&format!("MEZ_LOADER_END_{loader_marker}")));
    assert!(payload.lines().all(|line| line.len() <= 700));
    let loader_delivery = release_inputs
        .iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PaneProcessIo {
                effect: crate::runtime::PaneProcessIoEffect::WriteShellInput { delivery },
                ..
            } if delivery
                .bytes
                .windows(format!("MEZ_LOADER_END_{loader_marker}").len())
                .any(|window| window == format!("MEZ_LOADER_END_{loader_marker}").as_bytes()) =>
            {
                Some(delivery)
            }
            _ => None,
        })
        .expect("loader payload must retain a typed shell delivery");
    assert_eq!(
        loader_delivery.pacing,
        mez_mux::process::ShellInputPacing::LoaderAcknowledged,
        "loader payload data must stream until its terminating acknowledgement"
    );
    assert!(release_inputs.iter().all(|effect| {
        let input = String::from_utf8_lossy(effect.pane_input_parts().1);
        !input.starts_with('\u{7}') && !input.contains("MEZ_BASH_RX1_BEGIN")
    }));

    let bootstrap_marker = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            (transaction.kind == RunningShellTransactionKind::Bootstrap).then(|| marker.clone())
        })
        .expect("dependency-free child bootstrap should remain registered");
    let child_token = service
        .foreign_child_token_for_tests(&pane_id)
        .expect("dependency-free Bash child should retain its token")
        .to_string();
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
        1
    );
    let installed_effects = service.drain_pane_io_transition().side_effects;
    let installed_inputs = pane_input_effects(&installed_effects);
    assert_eq!(
        installed_inputs.len(),
        1,
        "authenticated child installation should release the deferred bootstrap wrapper"
    );
    let bootstrap_wrapper = String::from_utf8_lossy(installed_inputs[0].pane_input_parts().1);
    assert!(bootstrap_wrapper.starts_with('\u{7}'));
    assert!(bootstrap_wrapper.contains("MEZ_BASH_RX1_BEGIN"));

    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ForeignShellLoaderReady {
                    marker: loader_marker,
                }],
            )
            .unwrap(),
        0
    );
    assert!(pane_input_effects(&service.drain_pane_io_transition().side_effects).is_empty());

    let loader_marker = service
        .foreign_shell_loader_marker_for_tests(&pane_id)
        .expect("the active loader nonce should remain until loader exit")
        .to_string();
    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ForeignShellLoaderExited {
                    marker: loader_marker,
                    exit_code: 73,
                }],
            )
            .unwrap(),
        1
    );
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("failed")
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .all(|transaction| transaction.kind != RunningShellTransactionKind::Bootstrap),
        "premature loader exit must settle the bootstrap transaction"
    );

    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies certification material replayed from an earlier interaction
/// generation cannot certify a remote or foreign pane.
///
/// The replayed marker, start frame, and end frame are well formed and were all
/// observed on this pane's own input, but they belong to a superseded epoch.
/// They must not allocate loader or bootstrap ownership, publish certified
/// shell, environment, or path authority, or wedge the pane; recovery must
/// re-observe the foreground through a fresh identity probe instead.
#[test]
fn runtime_stale_generation_replay_cannot_certify_foreign_pane() {
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

    let (stale_marker, stale_turn_id) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::ShellIdentityProbe { .. }
            )
            .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .expect("dependency-free identity probe should be registered");
    service
        .observe_agent_shell_transaction_start(
            &pane_id,
            &stale_marker,
            &stale_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
        )
        .unwrap();
    let stale_output = format!(
        "\u{1e}mez_shell_identity_begin={stale_marker}\n\
         \u{1e}mez_shell_path=/bin/bash\n\
         \u{1e}mez_shell_version=GNU bash, version 5.2\n\
         \u{1e}mez_shell_identity_end={stale_marker}\n"
    );
    {
        let transaction = service
            .running_shell_transactions_mut_for_tests()
            .get_mut(&stale_marker)
            .unwrap();
        transaction.observed_output_bytes = stale_output.len();
        transaction.observed_output_preview = stale_output;
    }
    // The replayed frame set belongs to the superseded epoch.
    service.advance_pane_shell_interaction_generation_for_tests(&pane_id);
    service
        .observe_agent_shell_transaction_end(
            &pane_id,
            &stale_marker,
            &stale_turn_id,
            &format!("agent-{pane_id}"),
            &pane_id,
            0,
        )
        .unwrap();

    assert!(
        service
            .foreign_shell_loader_marker_for_tests(&pane_id)
            .is_none(),
        "a superseded generation must not allocate loader ownership"
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .all(|transaction| transaction.kind != RunningShellTransactionKind::Bootstrap),
        "a superseded generation must not register bootstrap ownership"
    );
    assert_ne!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("certified")
    );
    assert_ne!(
        service.pane_foreground_certified_shell_state(&pane_id),
        Some(true)
    );
    assert!(
        service.pane_environment_signature(&pane_id).is_none(),
        "a superseded generation must not publish certified environment authority"
    );
    assert!(!service.pane_environment_authority_is_certified_for_tests(&pane_id));
    let request = mez_agent::shell::PanePathResolutionRequest::new(
        vec![".".to_string()],
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    assert!(
        service
            .path_scopes_for_pane_request(&pane_id, &request)
            .map(|scopes| scopes.is_none())
            .unwrap_or(true),
        "a superseded generation must not publish pane path authority"
    );

    // Recovery: returning to the primary shell must re-observe the foreground
    // with a fresh interaction generation instead of reusing the stale frame.
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test(&pane_id, Some(primary_pid));
    service
        .apply_pane_foreground_process_event(&pane_id, "sh", primary_pid, None)
        .unwrap();
    service.drain_pane_io_transition();
    service.enter_agent_subshell_if_needed(&pane_id).unwrap();
    let reobserved_marker = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::ShellIdentityProbe { .. }
            )
            .then(|| marker.clone())
        });
    assert!(
        reobserved_marker.is_some(),
        "recovery must re-observe the pane foreground through a fresh identity probe"
    );
    assert_ne!(
        reobserved_marker.as_deref(),
        Some(stale_marker.as_str()),
        "recovery must not reuse a superseded identity marker"
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .all(|transaction| transaction.kind != RunningShellTransactionKind::Bootstrap),
        "recovery must not trust the stale completion"
    );
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies remote/SSH certification still requires the authenticated managed
/// receiver dialect event. A completed foreign bootstrap that never installed
/// its managed child must not publish shell, environment, or path authority.
///
/// The dependency-free loader serves the documented remote-control workflow, so
/// a local-only certification mechanism must not be able to remove that support
/// and spoofable completion frames must not replace the authenticated install.
#[test]
fn runtime_remote_certification_requires_authenticated_managed_install() {
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
        .expect("dependency-free identity probe should be registered");
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
    {
        let transaction = service
            .running_shell_transactions_mut_for_tests()
            .get_mut(&identity_marker)
            .unwrap();
        transaction.observed_output_bytes = identity_output.len();
        transaction.observed_output_preview = identity_output;
    }
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
    assert_eq!(
        service.maybe_bootstrap_ready_panes().unwrap(),
        1,
        "the reconciliation pump should launch the dependency-free child loader"
    );

    let loader_marker = service
        .foreign_shell_loader_marker_for_tests(&pane_id)
        .expect("dependency-free loader should retain its nonce")
        .to_string();
    let bootstrap_marker = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            (transaction.kind == RunningShellTransactionKind::Bootstrap).then(|| marker.clone())
        })
        .expect("dependency-free child bootstrap should be registered");
    service.drain_pane_io_transition();
    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ForeignShellLoaderReady {
                    marker: loader_marker,
                }],
            )
            .unwrap(),
        1
    );
    service.drain_pane_io_transition();

    let bootstrap_turn_id = service
        .running_shell_transactions_for_tests()
        .get(&bootstrap_marker)
        .expect("dependency-free child bootstrap should remain registered")
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
    let bootstrap_output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\thost\tremote-host\n\
env\tuser\tremote-user\n\
env\tshell_path\t/bin/bash\n\
env\tshell_class\tbash\n\
env\tpath\t/remote/bin:/usr/bin:/bin\n\
env\tcwd\t/remote/project\n\
env\tgit_repo\t0\n\
bootstrap\tcomplete\t1714500000\n";
    {
        let transaction = service
            .running_shell_transactions_mut_for_tests()
            .get_mut(&bootstrap_marker)
            .unwrap();
        transaction.observed_output_bytes = bootstrap_output.len();
        transaction.observed_output_preview = bootstrap_output.to_string();
    }
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

    assert_ne!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("certified"),
        "a remote bootstrap without the authenticated managed child must not certify"
    );
    assert_ne!(
        service.pane_foreground_certified_shell_state(&pane_id),
        Some(true)
    );
    assert!(
        service.pane_environment_signature(&pane_id).is_none(),
        "an unauthenticated remote completion must not publish pane environment authority"
    );
    assert!(!service.pane_environment_authority_is_certified_for_tests(&pane_id));
    let request = mez_agent::shell::PanePathResolutionRequest::new(
        vec![".".to_string()],
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    assert!(
        service
            .path_scopes_for_pane_request(&pane_id, &request)
            .map(|scopes| scopes.is_none())
            .unwrap_or(true),
        "an unauthenticated remote completion must not publish pane path authority"
    );
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies a failed write of the separately dispatched dependency-free loader
/// settles every staged child/bootstrap owner. A queued bootstrap wrapper or
/// retained loader marker would otherwise leave pane input leased after the
/// interactive loader command never reached the shell.
#[test]
fn runtime_dependency_free_loader_write_failure_clears_staged_bootstrap() {
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
        .expect("dependency-free identity probe should be registered");
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
    assert_eq!(
        service.maybe_bootstrap_ready_panes().unwrap(),
        1,
        "the reconciliation pump should launch the dependency-free child loader"
    );

    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("bootstrapping-child")
    );
    assert!(
        service
            .foreign_shell_loader_marker_for_tests(&pane_id)
            .is_some()
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| transaction.kind == RunningShellTransactionKind::Bootstrap)
    );
    service.drain_pane_io_transition();

    assert!(
        service
            .apply_pane_write_failure_event(&pane_id, "injected loader write failure")
            .unwrap()
    );

    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("failed")
    );
    assert!(
        service
            .foreign_shell_loader_marker_for_tests(&pane_id)
            .is_none()
    );
    assert!(service.foreign_child_token_for_tests(&pane_id).is_none());
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .all(|transaction| transaction.pane_id != pane_id)
    );
    assert!(pane_input_effects(&service.drain_pane_io_transition().side_effects).is_empty());

    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies a dependency-free Bash child treats its proof-less completion as
/// receiver cleanup rather than as restoration of the uninstrumented parent.
/// The correlated loader exit must remain admissible after certification so it
/// can release the private child and restore the foreign prompt without a
/// bootstrap timeout.
#[test]
fn runtime_dependency_free_foreign_bash_completion_preserves_loader_handoff() {
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
        .expect("dependency-free identity probe should be registered");
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
    assert_eq!(
        service.maybe_bootstrap_ready_panes().unwrap(),
        1,
        "the reconciliation pump should launch the dependency-free child loader"
    );

    let loader_marker = service
        .foreign_shell_loader_marker_for_tests(&pane_id)
        .expect("dependency-free loader should retain its nonce")
        .to_string();
    let bootstrap_marker = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            (transaction.kind == RunningShellTransactionKind::Bootstrap).then(|| marker.clone())
        })
        .expect("dependency-free child bootstrap should be registered");
    let child_token = service
        .foreign_child_token_for_tests(&pane_id)
        .expect("dependency-free Bash child should have a fresh token")
        .to_string();
    service.drain_pane_io_transition();
    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ForeignShellLoaderReady {
                    marker: loader_marker.clone(),
                }],
            )
            .unwrap(),
        1
    );
    service.drain_pane_io_transition();
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
    service.drain_pane_io_transition();

    let bootstrap_turn_id = service
        .running_shell_transactions_for_tests()
        .get(&bootstrap_marker)
        .expect("dependency-free Bash bootstrap should remain registered")
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
    assert!(
        service
            .drain_pane_io_transition()
            .side_effects
            .into_iter()
            .all(|effect| !matches!(
                effect,
                RuntimeSideEffect::PaneProcessIo {
                    effect: crate::runtime::PaneProcessIoEffect::ObserveForegroundProcess { .. },
                    ..
                }
            )),
        "a live dependency-free loader should replace the aliased SSH start observation"
    );
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

    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ManagedShell {
                    version: mez_terminal::MANAGED_SHELL_PROTOCOL_VERSION,
                    shell: mez_terminal::ManagedShellAdapter::Bash,
                    token: child_token,
                    event: mez_terminal::ManagedShellProtocolEvent::ParentReady {
                        marker: bootstrap_marker,
                        outcome: mez_terminal::ManagedShellParentOutcome::Completed,
                        exit_code: 0,
                        proof: None,
                    },
                }],
            )
            .unwrap(),
        1
    );
    assert!(
        service.maybe_bootstrap_ready_panes().unwrap() >= 1,
        "the reconciliation pump should settle the completed dependency-free bootstrap"
    );
    assert!(
        service
            .drain_pane_io_transition()
            .side_effects
            .into_iter()
            .all(|effect| !matches!(
                effect,
                RuntimeSideEffect::PaneProcessIo {
                    effect: crate::runtime::PaneProcessIoEffect::ObserveForegroundProcess { .. },
                    ..
                }
            )),
        "a live dependency-free loader should replace the aliased SSH completion observation"
    );
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("certified")
    );
    assert_eq!(
        service.pane_foreground_certified_shell_state(&pane_id),
        Some(true),
        "the authenticated managed receiver must still certify a remote pane shell"
    );
    assert!(
        service.pane_environment_authority_is_certified_for_tests(&pane_id),
        "a certified remote shell must still publish environment authority"
    );
    assert!(service.pane_environment_signature(&pane_id).is_some());

    service
        .apply_pane_foreground_process_event(
            &pane_id,
            "ssh",
            primary_pid.saturating_add(1),
            Some("/remote/project".to_string()),
        )
        .unwrap();
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("certified"),
        "a routine outer-SSH foreground poll must not restore the remote parent while the managed child remains live"
    );
    assert!(service.agent_subshell_is_active(&pane_id));
    service.remember_hidden_shell_render_suppression(&pane_id);
    let mut restored_prompt_batch = service
        .agent_subshell_exit_marker_for_tests(&pane_id)
        .expect("dependency-free Bash should retain its child-exit boundary")
        .to_vec();
    restored_prompt_batch.extend_from_slice(
        format!(
            "\u{1b}]133;R;mez_foreign_loader=exited;mez_marker={loader_marker};mez_status=0\u{1b}\\"
        )
        .as_bytes(),
    );
    restored_prompt_batch.extend_from_slice(b"foreign$ ");

    service
        .apply_pane_process_output(
            mez_mux::process::PaneProcessOutput {
                pane_id: pane_id.clone(),
                primary_pid,
                bytes: restored_prompt_batch,
            },
            &mut std::collections::BTreeSet::new(),
        )
        .unwrap();

    assert!(!service.agent_subshell_is_active(&pane_id));
    assert!(!service.pane_has_uncertified_foreign_shell_boundary(&pane_id));
    assert!(!service.hidden_shell_render_retention_timer_needed());
    let process_content = service
        .process_pane_screen(&pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        process_content.contains("foreign$"),
        "the restored foreign prompt in the loader-exit batch must be visible: {process_content:?}"
    );
    assert_eq!(
        service
            .process_pane_screen(&pane_id)
            .unwrap()
            .cursor_state()
            .column,
        "foreign$ ".chars().count(),
        "a prompt without an explicit carriage return must replace the retained prompt"
    );
    assert_eq!(
        service.renderable_pane_output_bytes(&pane_id, b"foreign output\r\n"),
        b"foreign output\r\n",
        "foreign-parent output must remain visible immediately after loader settlement"
    );

    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies exiting an unmanaged dependency-free child after a model shell
/// action retains the loader's interaction generation until its correlated
/// exit restores the foreign parent. The action's temporary process group can
/// remain in the cached foreground observation, but direct user input must
/// still reach the parent after restoration settles.
#[test]
fn runtime_unmanaged_foreign_loader_exit_releases_parent_input() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let primary_pid = service.pane_processes().primary_pid(&pane_id).unwrap();
    let foreign_group = primary_pid.saturating_add(1);
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test(&pane_id, Some(foreign_group));
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();

    assert!(
        service.begin_uncertified_foreign_shell_boundary(&pane_id, primary_pid, foreign_group,)
    );
    let loader_marker = "unmanaged-loader-restoration";
    assert!(service.certify_unmanaged_foreign_loader_for_tests(&pane_id, loader_marker,));
    service.enter_agent_subshell(pane_id.clone());
    let transient_action_group = foreign_group.saturating_add(1);
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test(&pane_id, Some(transient_action_group));

    assert!(service.exit_agent_subshell_if_active(&pane_id).unwrap());
    assert!(!service.agent_subshell_is_active(&pane_id));
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("certified"),
        "agent exit must retain the loader boundary until parent restoration"
    );
    service.drain_pane_io_transition();

    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ForeignShellLoaderExited {
                    marker: loader_marker.to_string(),
                    exit_code: 0,
                }],
            )
            .unwrap(),
        1
    );
    assert!(!service.pane_has_uncertified_foreign_shell_boundary(&pane_id));
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        None
    );

    let input = b"echo foreign-parent\n";
    let dispatch = service
        .write_input_to_pane(&primary, Some(&pane_id), input)
        .unwrap();
    assert_eq!(dispatch.bytes_written, input.len());
    let effects = service.drain_pane_io_transition().side_effects;
    let pane_inputs = pane_input_effects(&effects);
    assert_eq!(pane_inputs.len(), 1);
    assert_eq!(pane_inputs[0].pane_input_parts().0, pane_id);
    assert_eq!(pane_inputs[0].pane_input_parts().1, input);

    let _ = process.terminate(Duration::from_millis(10));
}

/// Settles one dependency-free foreign identity probe into the deferred
/// child-launch phase.
///
/// Identity settlement records `child-launch-pending` instead of launching the
/// child, so every regression that needs a staged boundary settles the probe the
/// same way production does: through the grouped transaction-end event.
fn settle_dependency_free_identity_probe(
    service: &mut RuntimeSessionService,
    pane_id: &str,
    shell_path: &str,
    shell_version: &str,
) {
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
        .expect("dependency-free identity probe should be registered");
    service
        .observe_agent_shell_transaction_start(
            pane_id,
            &identity_marker,
            &identity_turn_id,
            &format!("agent-{pane_id}"),
            pane_id,
        )
        .unwrap();
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
    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                pane_id,
                &[TerminalOscEvent::ShellTransactionEnd {
                    marker: identity_marker.clone(),
                    turn_id: identity_turn_id.clone(),
                    agent_id: format!("agent-{pane_id}"),
                    pane_id: pane_id.to_string(),
                    exit_code: 0,
                }],
            )
            .unwrap(),
        1,
        "identity settlement should observe exactly one transaction end event"
    );
}

/// Starts one pane whose live foreground process group is a foreign shell.
fn start_foreign_shell_pane(
    service: &mut RuntimeSessionService,
) -> (String, mez_mux::process::PaneProcess) {
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
    let process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    service
        .apply_pane_foreground_process_event(&pane_id, "ssh", primary_pid.saturating_add(1), None)
        .unwrap();
    service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    (pane_id, process)
}

/// Verifies the managed-Fish prompt-end observation handler cannot run the
/// deferred dependency-free child launch nested inside its own frame.
///
/// Fish admits prompt readiness from an observation handler, so a pane whose
/// identity probe already settled must only observe that readiness there. The
/// post-unwind application frame owns the child handoff; entering it from the
/// handler is exactly the nesting shape that overflowed the observation stack.
#[test]
fn runtime_dependency_free_fish_prompt_end_defers_child_launch_off_observation_frame() {
    let mut service = test_runtime_service();
    let (pane_id, mut process) = start_foreign_shell_pane(&mut service);
    settle_dependency_free_identity_probe(
        &mut service,
        &pane_id,
        "/bin/bash",
        "GNU bash, version 5.2",
    );
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("child-launch-pending"),
        "identity settlement must leave the child launch to the deferred pass"
    );

    service.mark_pane_fish_admission_awaiting_prompt_for_tests(&pane_id);
    assert!(
        service
            .observe_agent_shell_transaction_events(&pane_id, &[TerminalOscEvent::ShellPromptEnd])
            .unwrap()
            >= 1,
        "the Fish prompt-end handler must observe the admitted prompt"
    );
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("child-launch-pending"),
        "the prompt-end observation handler must not run the dependency-free child launch"
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .all(|transaction| transaction.kind != RunningShellTransactionKind::Bootstrap),
        "the prompt-end observation handler must not register bootstrap ownership"
    );
    let nested_effects = service.drain_pane_io_transition().side_effects;
    assert!(
        pane_input_effects(&nested_effects)
            .iter()
            .all(
                |effect| !String::from_utf8_lossy(effect.pane_input_parts().1).starts_with('\u{7}')
            ),
        "no managed child wrapper may be written from the observation handler"
    );

    assert_eq!(
        service.settle_deferred_foreign_bootstrap_work().unwrap(),
        1,
        "the post-unwind apply frame owns the deferred child launch"
    );
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("bootstrapping-child")
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| transaction.kind == RunningShellTransactionKind::Bootstrap),
        "the deferred pass must register bootstrap ownership outside the observation frame"
    );

    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies a replayed receiver end cannot override the settlement that the
/// deferral window already recorded.
///
/// Receiver completion removes the completion requirement and retains the
/// original end until the deferred pass settles it. A duplicate end for the same
/// marker inside that window must not settle the transaction with its own exit
/// code and discard the recorded end.
#[test]
fn runtime_deferred_receiver_end_ignores_duplicate_end_exit_code() {
    let mut service = test_runtime_service();
    let (pane_id, mut process) = start_foreign_shell_pane(&mut service);
    settle_dependency_free_identity_probe(
        &mut service,
        &pane_id,
        "/bin/bash",
        "GNU bash, version 5.2",
    );
    assert_eq!(service.settle_deferred_foreign_bootstrap_work().unwrap(), 1);

    let (bootstrap_marker, bootstrap_turn_id) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            (transaction.kind == RunningShellTransactionKind::Bootstrap)
                .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .expect("dependency-free child bootstrap should be registered");
    let agent_id = format!("agent-{pane_id}");
    service
        .observe_agent_shell_transaction_start(
            &pane_id,
            &bootstrap_marker,
            &bootstrap_turn_id,
            &agent_id,
            &pane_id,
        )
        .unwrap();
    let child_token = service
        .foreign_child_token_for_tests(&pane_id)
        .expect("the staged foreign child should retain its token")
        .to_string();
    service.register_shell_receiver_payload(
        &bootstrap_marker,
        mez_mux::process::ShellInputDelivery::receiver_acknowledged(
            b"managed foreign child source\n".to_vec(),
            &bootstrap_marker,
            true,
        ),
    );
    let preview = "bootstrap\tcomplete\t1714500000\n".to_string();
    {
        let transaction = service
            .running_shell_transactions_mut_for_tests()
            .get_mut(&bootstrap_marker)
            .unwrap();
        transaction.observed_output_bytes = preview.len();
        transaction.observed_output_preview = preview;
    }

    assert_eq!(
        service
            .observe_agent_shell_transaction_end(
                &pane_id,
                &bootstrap_marker,
                &bootstrap_turn_id,
                &agent_id,
                &pane_id,
                0,
            )
            .unwrap(),
        1,
        "the inner end marker must be retained until receiver completion"
    );
    assert_eq!(
        service
            .observe_shell_receiver_complete(&pane_id, &child_token, &bootstrap_marker, 0)
            .unwrap(),
        1
    );
    assert_eq!(
        service.pending_receiver_end_exit_code_for_tests(&bootstrap_marker),
        Some(0),
        "receiver completion must retain the recorded end for the deferred pass"
    );

    assert_eq!(
        service
            .observe_agent_shell_transaction_end(
                &pane_id,
                &bootstrap_marker,
                &bootstrap_turn_id,
                &agent_id,
                &pane_id,
                99,
            )
            .unwrap(),
        0,
        "a duplicate end inside the deferral window must be ignored"
    );
    assert_eq!(
        service.pending_receiver_end_exit_code_for_tests(&bootstrap_marker),
        Some(0),
        "a duplicate end must not override the recorded settlement exit code"
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .contains_key(&bootstrap_marker),
        "a duplicate end must not settle the deferred transaction"
    );

    assert_eq!(service.settle_ready_receiver_ends().unwrap(), 1);
    assert_eq!(
        service.pending_receiver_end_exit_code_for_tests(&bootstrap_marker),
        None
    );
    assert!(
        !service
            .running_shell_transactions_for_tests()
            .contains_key(&bootstrap_marker)
    );

    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies a failed deferred child launch clears the bounded handoff state that
/// the launch had already recorded.
///
/// The launch records its child token and shell before its fallible staging
/// steps. A failure there is terminal, so no bounded owner remains to expire the
/// leaked state: a retained Zsh `child_shell` would keep advertising an EscapeM
/// trigger for a failed boundary.
#[test]
fn runtime_deferred_child_launch_failure_clears_leaked_handoff_state() {
    let mut service = test_runtime_service();
    let (pane_id, mut process) = start_foreign_shell_pane(&mut service);
    settle_dependency_free_identity_probe(&mut service, &pane_id, "/bin/zsh", "zsh 5.9");
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("child-launch-pending")
    );
    assert!(
        service.active_zsh_trigger_for_pane(&pane_id).is_none(),
        "a staged boundary must not advertise a Zsh trigger before its child is recorded"
    );

    // One unrelated transaction for the pane makes the staged launch fail after
    // the child token and shell were already recorded on the boundary.
    service.register_running_shell_transaction(
        "blocker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::ReadinessProbe,
            pane_id: pane_id.clone(),
            command: "printf blocker".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
        false,
    );
    assert_eq!(
        service.dispatch_pending_foreign_child_launches().unwrap(),
        0
    );

    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("failed")
    );
    assert!(
        service.foreign_child_token_for_tests(&pane_id).is_none(),
        "a failed deferred launch must not leak its child token"
    );
    assert!(
        service.active_zsh_trigger_for_pane(&pane_id).is_none(),
        "a failed boundary must not advertise a stale Zsh trigger"
    );
    assert!(
        service
            .foreign_shell_loader_marker_for_tests(&pane_id)
            .is_none(),
        "a failed deferred launch must not leak loader ownership"
    );
    assert!(
        !service.pane_bootstrap_is_pending_for_tests(&pane_id),
        "a failed deferred launch must not leave bootstrap pending"
    );
    assert!(
        !service.pane_environment_authority_is_certified_for_tests(&pane_id),
        "a failed deferred launch must not publish certifications"
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .contains_key("blocker-1"),
        "the unrelated transaction that failed the launch must remain owned by its own settlement"
    );

    let _ = process.terminate(Duration::from_millis(10));
}
