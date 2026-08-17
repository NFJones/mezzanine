//! Dependency-free foreign-shell bootstrap regressions.

use super::*;

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
    assert!(
        service
            .foreign_shell_bootstrap_challenge_for_tests(&pane_id)
            .is_none(),
        "dependency-free bootstrap must not require an adapter challenge"
    );
    let identity_effects = service.drain_pane_io_transition().side_effects;
    let identity_inputs = pane_input_effects(&identity_effects);
    assert_eq!(identity_inputs.len(), 1);
    let identity_input = String::from_utf8_lossy(identity_inputs[0].pane_input_parts().1);
    assert!(
        identity_input.starts_with("/bin/sh -c "),
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
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("bootstrapping-child")
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| transaction.kind == RunningShellTransactionKind::Bootstrap),
        "dependency-free child bootstrap should be registered"
    );
    let loader_marker = service
        .foreign_shell_loader_marker_for_tests(&pane_id)
        .expect("dependency-free loader should retain its bounded nonce")
        .to_string();
    assert_eq!(loader_marker.len(), 32);
    let launch_effects = service.drain_pane_io_transition().side_effects;
    let launch_inputs = pane_input_effects(&launch_effects);
    assert_eq!(launch_inputs.len(), 1);
    let launch_input = String::from_utf8_lossy(launch_inputs[0].pane_input_parts().1);
    assert!(launch_input.starts_with("/bin/sh -c "), "{launch_input:?}");
    assert!(launch_input.len() <= 700, "{launch_input:?}");
    assert!(
        !launch_input.contains("MEZ_BASH_RX2_DATA"),
        "{launch_input:?}"
    );

    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ForeignShellLoaderReady {
                    marker: "stale-loader-marker".to_string(),
                }],
            )
            .unwrap(),
        0
    );
    assert!(
        pane_input_effects(&service.drain_pane_io_transition().side_effects).is_empty(),
        "a stale loader marker must not release generated source"
    );

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
    let payload_effects = service.drain_pane_io_transition().side_effects;
    let payload_inputs = pane_input_effects(&payload_effects);
    assert_eq!(payload_inputs.len(), 1);
    let payload = String::from_utf8_lossy(payload_inputs[0].pane_input_parts().1);
    assert!(payload.contains(&format!("MEZ_LOADER_END_{loader_marker}")));
    assert_eq!(payload_inputs[0].pane_input_parts().1.len(), payload.len());
    assert!(payload.lines().all(|line| line.len() <= 700));

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
        .expect("bootstrap start should request correlated foreground proof");
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
    let (completion_instance, completion_observation_id) = service
        .drain_pane_io_transition()
        .side_effects
        .into_iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PaneProcessIo {
                instance,
                effect:
                    crate::runtime::PaneProcessIoEffect::ObserveForegroundProcess {
                        observation_id,
                        expected_process_group_id: Some(_),
                    },
            } => Some((instance, observation_id)),
            _ => None,
        })
        .expect("receiver completion should request correlated foreground proof");
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

    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[TerminalOscEvent::ForeignShellLoaderExited {
                    marker: loader_marker,
                    exit_code: 0,
                }],
            )
            .unwrap(),
        1
    );
    assert!(!service.agent_subshell_is_active(&pane_id));
    assert!(!service.pane_has_uncertified_foreign_shell_boundary(&pane_id));

    let _ = process.terminate(Duration::from_millis(10));
}
