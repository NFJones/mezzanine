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
