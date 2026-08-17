//! Foreign Bash transport progress and timeout recovery regressions.

use super::*;

/// Verifies a large foreign Bash child handoff uses bounded logical-frame
/// acknowledgements, refreshes its idle owner while correlated bytes advance,
/// and atomically recovers a partially blocked parent receiver at the absolute
/// lifecycle deadline. The timeout must cancel only the marker-owned delivery,
/// interrupt the admitted receiver, release its lease, discard child authority,
/// and settle the waiting provider turn instead of leaving bootstrapping live.
#[test]
fn runtime_foreign_bash_progress_and_partial_timeout_are_bounded() {
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
                    instance_id: "remote-bash-timeout".to_string(),
                    trigger: None,
                },
            }],
        )
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
        "provider dispatch must wait for foreign bootstrap certification"
    );
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
                    instance_id: "remote-bash-timeout".to_string(),
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
    service.drain_pane_io_transition();
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
        .unwrap();
    let payload_effects = service.drain_pane_io_transition().side_effects;
    let payload = payload_effects
        .iter()
        .find_map(|effect| match effect {
            RuntimeSideEffect::PaneProcessIo {
                effect: crate::runtime::PaneProcessIoEffect::WriteShellInput { delivery },
                ..
            } if delivery.delivery_id.as_deref() == Some(bootstrap_marker.as_str()) => {
                Some(delivery.bytes.as_slice())
            }
            _ => None,
        })
        .expect("frame admission must release the marker-owned RX2 payload");
    let data_records = payload
        .split_inclusive(|byte| *byte == b'\n')
        .filter(|record| record.starts_with(b"MEZ_BASH_RX2_DATA "))
        .count();
    let acknowledgement_records = payload
        .split_inclusive(|byte| *byte == b'\n')
        .filter(|record| mez_mux::process::receiver_input_record_requires_ack(record))
        .count();
    assert!(
        data_records > acknowledgement_records,
        "payload={}",
        payload.len()
    );
    assert!(
        acknowledgement_records <= 8,
        "large staging source must use bounded logical acknowledgements: {acknowledgement_records}"
    );

    let now_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    service.set_foreign_shell_bootstrap_times_for_tests(
        &pane_id,
        now_unix_ms.saturating_sub(1_000),
        now_unix_ms.saturating_sub(14_999),
    );
    service
        .running_shell_transactions_mut_for_tests()
        .get_mut(&bootstrap_marker)
        .unwrap()
        .started_at_unix_ms = now_unix_ms.saturating_sub(14_999);
    assert!(
        service
            .apply_pane_input_written_event(&pane_id, 4_096)
            .unwrap()
    );
    assert_eq!(
        service
            .apply_shell_transaction_timer_event(now_unix_ms.saturating_add(10_000))
            .unwrap(),
        0,
        "correlated delivery progress must refresh the bounded idle owner"
    );
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("bootstrapping-child")
    );

    service.set_foreign_shell_bootstrap_times_for_tests(
        &pane_id,
        1,
        now_unix_ms.saturating_add(10_000),
    );
    assert_eq!(
        service
            .apply_shell_transaction_timer_event(120_001)
            .unwrap(),
        1
    );
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("failed")
    );
    assert!(
        !service
            .running_shell_transactions_for_tests()
            .contains_key(&bootstrap_marker)
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
    assert!(
        service
            .foreign_bash_parent_proof_for_tests(&pane_id)
            .is_none()
    );
    assert!(!service.agent_provider_task_is_pending(&started.turn_id));
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == started.turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
    let timeout_effects = service.drain_pane_io_transition().side_effects;
    assert!(timeout_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::PaneProcessIo {
            effect: crate::runtime::PaneProcessIoEffect::CancelShellInput { delivery_id },
            ..
        } if delivery_id == &bootstrap_marker
    )));
    assert!(timeout_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::PaneProcessIo {
            effect: crate::runtime::PaneProcessIoEffect::ReleaseShellInputLease { owner_id },
            ..
        } if owner_id == &bootstrap_marker
    )));
    assert!(timeout_effects.iter().any(|effect| matches!(
        effect,
        RuntimeSideEffect::PaneProcessIo {
            effect: crate::runtime::PaneProcessIoEffect::WriteInput { bytes },
            ..
        } if bytes == b"\x03"
    )));

    let _ = process.terminate(Duration::from_millis(10));
}
