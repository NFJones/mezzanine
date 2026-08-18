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
        2,
        "identity discovery and loader startup should be pipelined in one pane write"
    );
    let loader_command = identity_input
        .lines()
        .nth(1)
        .expect("the pipelined write should contain the loader command");
    assert!(loader_command.len() <= 700, "{loader_command:?}");

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
    let loader_marker = service
        .foreign_shell_loader_marker_for_tests(&pane_id)
        .expect("dependency-free loader should retain its bounded nonce")
        .to_string();
    assert_eq!(loader_marker.len(), 32);
    assert_eq!(
        service
            .observe_agent_shell_transaction_events(
                &pane_id,
                &[
                    TerminalOscEvent::ShellTransactionEnd {
                        marker: identity_marker.clone(),
                        turn_id: identity_turn_id.clone(),
                        agent_id: format!("agent-{pane_id}"),
                        pane_id: pane_id.clone(),
                        exit_code: 0,
                    },
                    TerminalOscEvent::ForeignShellLoaderReady {
                        marker: loader_marker.clone(),
                    },
                ],
            )
            .unwrap(),
        2,
        "one SSH output batch should settle identity and admit the queued loader"
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
        "dependency-free child bootstrap should be registered"
    );
    let launch_effects = service.drain_pane_io_transition().side_effects;
    let launch_inputs = pane_input_effects(&launch_effects);
    assert_eq!(
        launch_inputs.len(),
        2,
        "same-batch loader admission should release its payload and prebuffer the bootstrap trigger"
    );
    let payload = launch_inputs
        .iter()
        .map(|effect| String::from_utf8_lossy(effect.pane_input_parts().1))
        .find(|input| input.contains(&format!("MEZ_LOADER_END_{loader_marker}")))
        .expect("one released input should contain the loader payload");
    assert!(payload.contains(&format!("MEZ_LOADER_END_{loader_marker}")));
    assert!(payload.lines().all(|line| line.len() <= 700));
    let loader_delivery = launch_inputs
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
    assert!(launch_inputs.iter().any(|effect| {
        let input = String::from_utf8_lossy(effect.pane_input_parts().1);
        input.starts_with('\u{7}') && input.contains("MEZ_BASH_RX1_BEGIN")
    }));

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
    restored_prompt_batch.extend_from_slice(b"\rforeign$ ");

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
        service.renderable_pane_output_bytes(&pane_id, b"foreign output\r\n"),
        b"foreign output\r\n",
        "foreign-parent output must remain visible immediately after loader settlement"
    );

    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies exiting an unmanaged dependency-free child retains the loader's
/// interaction generation until its correlated exit restores the foreign
/// parent. Once restoration settles, direct user input must reach that parent
/// instead of remaining queued behind stale runtime ownership.
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
