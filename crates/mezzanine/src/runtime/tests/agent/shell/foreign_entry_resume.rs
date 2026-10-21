//! Pre-probe agent-entry readiness contracts.
//!
//! Agent entry admits its dependency-free identity probe only at
//! product-observed prompt readiness, never from foreground metadata alone. The
//! native unmanaged-Zsh fixture therefore waits for the runtime's own readiness
//! observation before entering. These tests pin both halves of that contract at
//! the production owner: entry at observed readiness dispatches exactly one
//! identity probe, and entry before readiness stays deferred with no input until
//! a real prompt observation arrives.

use super::*;

/// Builds an attached pane whose primary shell is a live local process and
/// detaches its adapter without observing a foreground group.
fn entry_readiness_fixture() -> (
    RuntimeSessionService,
    mez_core::ClientId,
    String,
    u32,
    mez_mux::process::PaneProcess,
) {
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
    let process = service
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();
    (service, primary, pane_id, primary_pid, process)
}

/// Counts registered dependency-free identity-probe transactions.
fn identity_probe_transactions(service: &RuntimeSessionService) -> usize {
    service
        .running_shell_transactions_for_tests()
        .iter()
        .filter(|(_, transaction)| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::ShellIdentityProbe { .. }
            )
        })
        .count()
}

/// Verifies entry at product-observed prompt readiness dispatches one identity
/// probe.
///
/// The fixture's precondition is the runtime's own readiness observation, which
/// this test supplies through the ordinary foreground owner before entry. Entry
/// at that boundary must start exactly one generation-fenced probe without
/// publishing environment authority, child activation, or a second interaction
/// epoch.
#[test]
fn runtime_agent_entry_at_observed_prompt_readiness_dispatches_identity_probe() {
    let (mut service, primary, pane_id, primary_pid, mut process) = entry_readiness_fixture();
    let process_group_id = service.pane_primary_process_group_id(&pane_id, primary_pid);
    service
        .apply_pane_foreground_process_event(&pane_id, "cat", process_group_id, None)
        .unwrap();
    assert_eq!(
        service.pane_readiness_state(&pane_id),
        PaneReadinessState::PromptCandidate,
        "the foreground owner must publish the readiness the fixture waits for"
    );
    service.drain_pane_io_transition();

    let shown = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(shown.contains("visibility=visible"), "{shown}");
    let entry_transition = service.drain_pane_io_transition();
    let writes = pane_input_effects(&entry_transition.side_effects);
    assert_eq!(
        writes.len(),
        1,
        "entry at observed prompt readiness must dispatch exactly one identity probe: {writes:?}"
    );
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("identity-probing")
    );
    assert_eq!(
        service.pane_shell_interaction_generation_for_tests(&pane_id),
        Some(1),
        "the entry epoch must not advance twice for one entry"
    );
    assert_eq!(identity_probe_transactions(&service), 1);
    assert!(service.pane_environment_signature(&pane_id).is_none());
    assert!(!service.pane_environment_authority_is_certified_for_tests(&pane_id));
    assert!(!service.agent_subshell_is_active(&pane_id));
    process.terminate(Duration::from_millis(100)).unwrap();
}

/// Verifies entry before prompt readiness never probes from foreground metadata
/// alone and releases only on a real prompt observation.
///
/// This is the contract the native fixture relies on: a first process-group
/// observation may fence the shell boundary, but it must not authorize hidden
/// probe input. Only a genuine prompt observation releases the single deferred
/// probe, which keeps rendered prompt text from being treated as admission.
#[test]
fn runtime_agent_entry_before_prompt_readiness_withholds_probe_until_prompt_observation() {
    let (mut service, primary, pane_id, primary_pid, mut process) = entry_readiness_fixture();
    let process_group_id = service.pane_primary_process_group_id(&pane_id, primary_pid);
    service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    let entry_transition = service.drain_pane_io_transition();
    let entry_writes = pane_input_effects(&entry_transition.side_effects);
    assert!(
        entry_writes.is_empty(),
        "entry before prompt readiness must not write shell input: {entry_writes:?}"
    );
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("awaiting-prompt")
    );
    assert_eq!(
        service.pane_readiness_state(&pane_id),
        PaneReadinessState::Unknown
    );

    service
        .apply_pane_foreground_process_event(&pane_id, "cat", process_group_id, None)
        .unwrap();
    assert_eq!(
        service.pane_readiness_state(&pane_id),
        PaneReadinessState::Unknown,
        "foreground metadata alone must never publish prompt readiness"
    );
    let metadata_transition = service.drain_pane_io_transition();
    assert!(
        pane_input_effects(&metadata_transition.side_effects).is_empty(),
        "foreground metadata alone must not dispatch the identity probe"
    );
    assert_eq!(identity_probe_transactions(&service), 0);

    service
        .observe_passive_shell_prompt_candidate(&pane_id, "test-prompt")
        .unwrap();
    assert_eq!(service.maybe_bootstrap_ready_panes().unwrap(), 1);
    assert_eq!(
        service.foreign_shell_bootstrap_phase_for_tests(&pane_id),
        Some("identity-probing")
    );
    let prompt_transition = service.drain_pane_io_transition();
    assert_eq!(
        pane_input_effects(&prompt_transition.side_effects).len(),
        1,
        "a real prompt observation must release exactly one identity probe"
    );
    assert_eq!(identity_probe_transactions(&service), 1);
    process.terminate(Duration::from_millis(100)).unwrap();
}
