//! Pre-probe agent-entry ordering regressions.
//!
//! Explicit agent entry can run before the pane worker delivers its first
//! foreground-process observation. These tests pin that ordering for the
//! dependency-free bootstrap boundary: the first exact-owner foreground
//! observation must supply the epoch's prompt evidence and release exactly one
//! identity probe, while a foreign foreground group still withholds it.

use super::*;

/// Verifies a display-first entry boundary resumes its first identity probe on
/// the first exact-owner foreground observation.
///
/// The fixture shows the agent shell while the pane worker has not yet reported
/// any foreground group, so entry creates the dependency-free boundary from the
/// primary process group with readiness Unknown. Displayed prompt text is never
/// readiness evidence by itself. The observation that later proves the same
/// live primary process, process group, and interaction generation owns the PTY
/// is the epoch's real prompt evidence, so it must release exactly one bounded
/// identity probe without a new interaction generation, user input, or any
/// child, environment, or readiness authority.
#[test]
fn runtime_agent_entry_resumes_identity_probe_for_exact_owner_foreground() {
    let mut service = test_runtime_service();
    service.disable_legacy_managed_startup_for_tests();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = "%1".to_string();
    let primary_pid = service.pane_processes().primary_pid(&pane_id).unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test(&pane_id, None);
    let primary_process_group_id = service.pane_primary_process_group_id(&pane_id, primary_pid);
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();

    let shown = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(shown.contains("visibility=visible"), "{shown}");
    service.drain_pane_io_transition();

    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("awaiting-prompt"),
        "entry before the first foreground observation must retain its boundary"
    );
    assert!(service.pane_bootstrap_is_pending(&pane_id));
    assert_eq!(
        service.pane_readiness_state(&pane_id),
        PaneReadinessState::Unknown,
        "displayed prompt text alone must never publish runtime readiness"
    );
    let generation = service.pane_shell_interaction_generation_for_tests(&pane_id);
    assert_eq!(generation, Some(1));
    let before_foreground_transition = service.drain_pane_io_transition();
    let before_foreground = pane_input_effects(&before_foreground_transition.side_effects);
    assert!(
        before_foreground.is_empty(),
        "entry without its foreground observation must not write shell input: {before_foreground:?}"
    );

    service
        .apply_pane_foreground_process_event(&pane_id, "cat", primary_process_group_id, None)
        .unwrap();

    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("identity-probing"),
        "the exact owner foreground observation must release bounded identity discovery"
    );
    assert_eq!(
        service.pane_shell_interaction_generation_for_tests(&pane_id),
        generation,
        "resuming entry must not allocate a new shell interaction epoch"
    );
    let identity_transition = service.drain_pane_io_transition();
    let identity_writes = pane_input_effects(&identity_transition.side_effects);
    assert_eq!(
        identity_writes.len(),
        1,
        "resuming entry must queue exactly one identity probe: {identity_writes:?}"
    );
    let identity_transactions = service
        .running_shell_transactions_for_tests()
        .iter()
        .filter(|(_, transaction)| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::ShellIdentityProbe { .. }
            )
        })
        .count();
    assert_eq!(identity_transactions, 1);
    assert!(
        service.pane_environment_signature(&pane_id).is_none(),
        "probe dispatch must not publish environment authority"
    );
    assert!(!service.pane_environment_authority_is_certified_for_tests(&pane_id));
    assert!(!service.agent_subshell_is_active(&pane_id));
    process.terminate(Duration::from_millis(100)).unwrap();
}

/// Verifies a contrary readiness state is never overwritten by the entry wake.
///
/// An interactive-blocked pane has real evidence that an application owns the
/// terminal. Even a foreground observation for the boundary's own process
/// group must not convert that evidence into a prompt candidate, because the
/// boundary's wake exists only to supply the missing first prompt observation
/// and never to outvote a password, full-screen, interactive-blocked, busy, or
/// ready state.
#[test]
fn runtime_agent_entry_never_overrides_contrary_readiness_state() {
    let mut service = test_runtime_service();
    service.disable_legacy_managed_startup_for_tests();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = "%1".to_string();
    let primary_pid = service.pane_processes().primary_pid(&pane_id).unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test(&pane_id, None);
    let primary_process_group_id = service.pane_primary_process_group_id(&pane_id, primary_pid);
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    service.drain_pane_io_transition();
    service.set_pane_readiness(&pane_id, PaneReadinessState::InteractiveBlocked);

    service
        .apply_pane_foreground_process_event(&pane_id, "cat", primary_process_group_id, None)
        .unwrap();

    assert_eq!(
        service.pane_readiness_state(&pane_id),
        PaneReadinessState::InteractiveBlocked,
        "the entry wake must never overwrite contrary readiness evidence"
    );
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("awaiting-prompt")
    );
    let transition = service.drain_pane_io_transition();
    let writes = pane_input_effects(&transition.side_effects);
    assert!(
        writes.is_empty(),
        "an interactive-blocked pane must never receive shell input: {writes:?}"
    );
    process.terminate(Duration::from_millis(100)).unwrap();
}

/// Verifies a foreign foreground group never releases a display-first entry
/// boundary's identity probe.
///
/// Prompt evidence must come from the exact process and interaction epoch that
/// owns the boundary. A different foreground group is the shape where an
/// application, not the selected shell, owns the terminal, so the boundary must
/// remain awaiting its prompt with no input written and no transaction
/// allocated.
#[test]
fn runtime_agent_entry_withholds_identity_probe_for_foreign_foreground_group() {
    let mut service = test_runtime_service();
    service.disable_legacy_managed_startup_for_tests();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = "%1".to_string();
    let primary_pid = service.pane_processes().primary_pid(&pane_id).unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test(&pane_id, None);
    let mut process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    service.drain_pane_io_transition();

    service
        .apply_pane_foreground_process_event(&pane_id, "vim", primary_pid.saturating_add(1), None)
        .unwrap();

    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("awaiting-prompt"),
        "a foreign foreground group must not release entry discovery"
    );
    assert_eq!(
        service.pane_shell_interaction_generation_for_tests(&pane_id),
        Some(1)
    );
    assert_eq!(
        service.pane_readiness_state(&pane_id),
        PaneReadinessState::Unknown,
        "a foreign foreground group must not publish prompt readiness"
    );
    let foreign_transition = service.drain_pane_io_transition();
    let writes = pane_input_effects(&foreign_transition.side_effects);
    assert!(
        writes.is_empty(),
        "a foreign foreground group must never receive shell input: {writes:?}"
    );
    assert_eq!(
        service
            .running_shell_transactions_for_tests()
            .iter()
            .filter(|(_, transaction)| {
                matches!(
                    transaction.kind,
                    RunningShellTransactionKind::ShellIdentityProbe { .. }
                )
            })
            .count(),
        0
    );
    process.terminate(Duration::from_millis(100)).unwrap();
}
