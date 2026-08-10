//! Runtime tests for actions patch dispatch behavior.

use crate::runtime::processes::RuntimeAgentSubshellCertificationRejection;

use super::*;

/// Verifies a pending shell action is recovered instead of failed when
/// `interactive-blocked` is stale and the pane shell is foreground again.
///
/// The dispatch path used to turn stale interactive-blocked readiness into a
/// hard `pane_not_ready` action failure. That was incorrect when host process
/// metadata already proved the user's shell had returned.
#[test]
fn runtime_shell_dispatch_recovers_stale_interactive_blocked_readiness() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "inspect").unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "shell-1".to_string(),
        rationale: "inspect the working directory".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect the working directory.".to_string(),
            command: "pwd".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    service.agent_turn_executions_mut().insert(
        turn.turn_id.clone(),
        mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "run shell action".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "inspect with shell".to_string(),
                    thought: None,
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    actions: vec![action.clone()],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::running(
                &turn,
                &action,
                Vec::new(),
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    let execution = service
        .agent_turn_executions()
        .get(&turn.turn_id)
        .cloned()
        .unwrap();
    append_test_execution_assistant_context(&mut service, &turn, &execution);
    service.remove_pending_agent_provider_task(&turn.turn_id);
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);

    let execution_after_dispatch = service
        .dispatch_stored_running_shell_actions(&turn.turn_id)
        .unwrap();

    assert!(execution_after_dispatch.is_some());
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Probing
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| transaction.kind == RunningShellTransactionKind::ReadinessProbe)
    );
    let execution = service.agent_turn_executions().get(&turn.turn_id).unwrap();
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert!(execution.action_results[0].error.is_none());
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies successful readiness-probe completion resumes the original shell
/// action after stale `interactive-blocked` recovery.
///
/// The stale recovery path must not stop at `Probing`. Once a successful probe
/// end marker arrives, the pending shell action should dispatch, settle, and
/// stop reporting as a still-running placeholder.
#[test]
fn runtime_shell_dispatch_completes_pending_action_after_stale_interactive_blocked_probe() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "inspect").unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "shell-1".to_string(),
        rationale: "confirm the pending shell action resumes".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Print a recovery marker.".to_string(),
            command: "printf 'STALE_INTERACTIVE_BLOCKED_RECOVERED\\n'".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    service.agent_turn_executions_mut().insert(
        turn.turn_id.clone(),
        mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "run shell action".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "inspect with shell".to_string(),
                    thought: None,
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    actions: vec![action.clone()],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::running(
                &turn,
                &action,
                Vec::new(),
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    let execution = service
        .agent_turn_executions()
        .get(&turn.turn_id)
        .cloned()
        .unwrap();
    append_test_execution_assistant_context(&mut service, &turn, &execution);
    service.remove_pending_agent_provider_task(&turn.turn_id);
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);

    let execution_after_dispatch = service
        .dispatch_stored_running_shell_actions(&turn.turn_id)
        .unwrap();

    assert!(execution_after_dispatch.is_some());
    let probe_marker = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            (transaction.kind == RunningShellTransactionKind::ReadinessProbe)
                .then(|| marker.clone())
        })
        .unwrap();

    let observed_start = service
        .observe_agent_shell_transaction_start(
            "%1",
            &probe_marker,
            &turn.turn_id,
            &turn.agent_id,
            "%1",
        )
        .unwrap();

    assert!(observed_start > 0);
    let observed = service
        .observe_agent_shell_transaction_end(
            "%1",
            &probe_marker,
            &turn.turn_id,
            &turn.agent_id,
            "%1",
            0,
        )
        .unwrap();

    assert!(observed > 0);
    assert!(matches!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Ready | PaneReadinessState::Busy
    ));

    let action_marker = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { .. }
            )
            .then(|| marker.clone())
        })
        .expect("readiness-probe completion should dispatch the pending shell action");
    let observed_start = service
        .observe_agent_shell_transaction_start(
            "%1",
            &action_marker,
            &turn.turn_id,
            &turn.agent_id,
            "%1",
        )
        .unwrap();
    assert!(observed_start > 0);
    let observed_end = service
        .observe_agent_shell_transaction_end(
            "%1",
            &action_marker,
            &turn.turn_id,
            &turn.agent_id,
            "%1",
            0,
        )
        .unwrap();
    assert!(observed_end > 0);

    assert!(
        service.running_shell_transactions_for_tests().is_empty(),
        "stale interactive-blocked recovery should settle its shell transaction"
    );
    let execution = service.agent_turn_executions().get(&turn.turn_id).unwrap();
    assert_ne!(execution.action_results[0].status, ActionStatus::Running);
    assert!(execution.action_results[0].error.is_none());
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies stale `interactive-blocked` dispatch recovery compares foreground
/// process groups with the shell process group, not only with the shell pid.
///
/// Some PTY backends and shell setups report a shell process-group leader that
/// differs from the spawned primary pid. The readiness proof should still treat
/// that process group as the foreground shell boundary so stale readiness does
/// not become a hard `pane_not_ready` failure after the user returns to the
/// prompt.
#[test]
fn runtime_shell_dispatch_recovers_stale_interactive_blocked_with_shell_process_group() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    let foreground_group = service
        .pane_processes()
        .foreground_process_group_id("%1")
        .unwrap();
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();
    service
        .pane_processes_mut()
        .set_process_group_leader_for_test("%1", i32::try_from(foreground_group).ok());
    service
        .pane_processes_mut()
        .set_primary_pid_for_test("%1", primary_pid.saturating_add(1));
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "inspect").unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "shell-1".to_string(),
        rationale: "inspect the working directory".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect the working directory.".to_string(),
            command: "pwd".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    service.agent_turn_executions_mut().insert(
        turn.turn_id.clone(),
        mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "run shell action".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "inspect with shell".to_string(),
                    thought: None,
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    actions: vec![action.clone()],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::running(
                &turn,
                &action,
                Vec::new(),
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    service.remove_pending_agent_provider_task(&turn.turn_id);
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);

    let execution_after_dispatch = service
        .dispatch_stored_running_shell_actions(&turn.turn_id)
        .unwrap();

    assert!(execution_after_dispatch.is_some());
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Probing
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| transaction.kind == RunningShellTransactionKind::ReadinessProbe)
    );
    let execution = service.agent_turn_executions().get(&turn.turn_id).unwrap();
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert!(execution.action_results[0].error.is_none());
    service.terminate_all_pane_processes().unwrap();
}

#[test]
fn runtime_shell_dispatch_recovers_stale_interactive_blocked_with_cached_foreground_group() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();
    service
        .apply_pane_foreground_process_event("%1", "sh", primary_pid, None)
        .unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test("%1", None);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "inspect").unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "shell-1".to_string(),
        rationale: "inspect the working directory".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect the working directory.".to_string(),
            command: "pwd".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    service.agent_turn_executions_mut().insert(
        turn.turn_id.clone(),
        mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "run shell action".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "inspect with shell".to_string(),
                    thought: None,
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    actions: vec![action.clone()],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::running(
                &turn,
                &action,
                Vec::new(),
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    service.remove_pending_agent_provider_task(&turn.turn_id);
    service.set_pane_readiness("%1", PaneReadinessState::InteractiveBlocked);

    let execution_after_dispatch = service
        .dispatch_stored_running_shell_actions(&turn.turn_id)
        .unwrap();

    assert!(execution_after_dispatch.is_some());
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Probing
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| transaction.kind == RunningShellTransactionKind::ReadinessProbe)
    );
    let execution = service.agent_turn_executions().get(&turn.turn_id).unwrap();
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert!(execution.action_results[0].error.is_none());
    service.terminate_all_pane_processes().unwrap();
}

/// Completes a runtime-owned agent-subshell bootstrap under one foreground
/// process group so dispatch tests can exercise certified-shell behavior.
fn certify_agent_subshell_foreground_group(
    service: &mut RuntimeSessionService,
    process_group_id: u32,
) {
    service.enter_agent_subshell("%1");
    service.begin_agent_subshell_shell_handoff("%1").unwrap();
    assert!(
        service
            .pane_agent_subshell_certification_rejection("%1")
            .is_none()
    );
    service.set_pane_agent_subshell_certification_rejection_for_tests(
        "%1",
        RuntimeAgentSubshellCertificationRejection::EnvironmentSignatureMissing,
    );
    service.dispatch_bootstrap_to_pane("%1").unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test("%1", Some(process_group_id));
    let (marker, turn_id) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            (transaction.kind == RunningShellTransactionKind::Bootstrap)
                .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .unwrap();
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
        .observe_agent_shell_transaction_start("%1", &marker, &turn_id, "agent-%1", "%1")
        .unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test("%1", Some(process_group_id.saturating_add(1)));
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test("%1", Some(process_group_id));
    service
        .observe_agent_shell_transaction_end("%1", &marker, &turn_id, "agent-%1", "%1", 0)
        .unwrap();
}

/// Verifies a transient isolated transaction child does not replace the
/// persistent agent-subshell identity sampled at the certification boundaries.
#[test]
fn runtime_agent_subshell_bootstrap_accepts_transient_isolated_child_group() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();
    let subshell_group = primary_pid.saturating_add(1);
    service.set_pane_agent_subshell_certification_rejection_for_tests(
        "%1",
        RuntimeAgentSubshellCertificationRejection::TransactionFailed,
    );

    certify_agent_subshell_foreground_group(&mut service, subshell_group);

    assert_eq!(
        service.pane_foreground_certified_shell_state("%1"),
        Some(true)
    );
    assert!(service.pane_environment_signature("%1").is_some());
    let diagnostic = service.pane_foreground_process_diagnostic("%1").json();
    assert_eq!(
        diagnostic["certified_shell_process_group_id"],
        subshell_group
    );
    assert_eq!(
        diagnostic["certified_shell_source"],
        "agent-subshell-bootstrap"
    );
    assert_eq!(
        diagnostic["agent_subshell_certification_rejection"],
        serde_json::Value::Null
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies one certified shell identity keeps executable path, dialect,
/// version evidence, primary process, and interaction epoch together. A later
/// interaction generation must invalidate the entire identity rather than
/// allowing callers to combine its dialect with the primary session shell.
#[test]
fn runtime_shell_execution_identity_is_atomic_and_epoch_scoped() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();
    let subshell_group = primary_pid.saturating_add(1);

    certify_agent_subshell_foreground_group(&mut service, subshell_group);

    let identity = service.shell_execution_identity_for_pane("%1").unwrap();
    assert_eq!(identity.shell_path(), std::path::Path::new("/bin/sh"));
    assert_eq!(
        identity.classification(),
        mez_agent::ShellClassification::PosixSh
    );
    assert_eq!(identity.version_probe(), None);
    assert_eq!(identity.primary_process_id(), Some(primary_pid));
    assert!(identity.interaction_generation().is_some());

    service.advance_pane_shell_interaction_generation_for_tests("%1");
    let error = service.shell_execution_identity_for_pane("%1").unwrap_err();
    assert!(error.message().contains("stale"), "{error}");
    service.terminate_all_pane_processes().unwrap();
}

/// Adapter-owned bootstrap fixture stopped at pending completion certification.
struct PendingAgentSubshellCertificationFixture {
    /// Runtime service retaining the unpublished bootstrap environment.
    service: RuntimeSessionService,
    /// Detached process handle kept alive for the duration of the test.
    process: mez_mux::process::PaneProcess,
    /// Exact process generation required by the pending observation.
    instance: crate::runtime::PaneProcessInstance,
    /// Exact observation correlation generated by the runtime.
    observation_id: String,
    /// Persistent agent-subshell process group captured at transaction start.
    subshell_group: u32,
    /// Timer snapshot owned by the bootstrap transaction before completion.
    original_timer: crate::runtime::RuntimeShellTransactionTimerRef,
}

/// Builds an adapter-owned bootstrap that has completed its shell transaction
/// and is waiting for the exact fresh foreground-process observation.
fn pending_agent_subshell_certification_fixture() -> PendingAgentSubshellCertificationFixture {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();
    let subshell_group = primary_pid.saturating_add(1);
    service.enter_agent_subshell("%1");
    service.begin_agent_subshell_shell_handoff("%1").unwrap();
    service.dispatch_bootstrap_to_pane("%1").unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test("%1", Some(subshell_group));
    let (marker, turn_id) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            (transaction.kind == RunningShellTransactionKind::Bootstrap)
                .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .unwrap();
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
        .observe_agent_shell_transaction_start("%1", &marker, &turn_id, "agent-%1", "%1")
        .unwrap();
    let process = service.take_running_pane_process_for_adapter("%1").unwrap();
    let original_timer = service
        .running_shell_transaction_timers()
        .into_iter()
        .find(|timer| timer.marker == marker)
        .expect("running bootstrap should own its transaction timer");
    service
        .apply_pane_foreground_process_event("%1", "setsid", subshell_group.saturating_add(1), None)
        .unwrap();
    service
        .observe_agent_shell_transaction_end("%1", &marker, &turn_id, "agent-%1", "%1", 0)
        .unwrap();
    let (instance, observation_id, expected_process_group_id) = service
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
        .expect("bootstrap completion should request a correlated foreground observation");
    assert_eq!(expected_process_group_id, Some(subshell_group));
    PendingAgentSubshellCertificationFixture {
        service,
        process,
        instance,
        observation_id,
        subshell_group,
        original_timer,
    }
}

/// Verifies stranded-shell recovery defers to pane-scoped bootstrap
/// certification instead of exhausting foreground-process recovery attempts.
///
/// A routed child can request a shell action while its pane is still proving
/// the persistent agent-subshell process group. That bounded certification is
/// not owned by the child turn, so idle recovery must neither increment the
/// fail-closed counter nor queue a premature provider retry. Once the exact
/// certification observation settles, bootstrap resumption must advance the
/// original shell action.
#[test]
fn runtime_shell_recovery_waits_for_pane_subshell_certification() {
    let mut fixture = pending_agent_subshell_certification_fixture();
    fixture
        .service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = fixture
        .service
        .start_agent_prompt_turn("%1", "inspect after certification")
        .unwrap();
    let turn = fixture
        .service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "shell-awaiting-certification".to_string(),
        rationale: "inspect through the pending agent subshell".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect the working directory.".to_string(),
            command: "pwd".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    fixture.service.agent_turn_executions_mut().insert(
        turn.turn_id.clone(),
        mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "run shell action after certification".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "inspect with the certified shell".to_string(),
                    thought: None,
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    actions: vec![action.clone()],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::running(
                &turn,
                &action,
                Vec::new(),
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    fixture
        .service
        .remove_pending_agent_provider_task(&turn.turn_id);

    for _ in 0..3 {
        assert_eq!(
            fixture
                .service
                .recover_stranded_agent_shell_dispatches()
                .unwrap(),
            0
        );
        assert_eq!(
            fixture
                .service
                .pending_shell_dispatch_blocked_recovery_attempts(&turn.turn_id, &action.id),
            0
        );
        assert!(
            !fixture
                .service
                .agent_provider_task_is_pending(&turn.turn_id)
        );
    }

    let settled = fixture
        .service
        .apply_pane_foreground_process_observation_transition(
            fixture.instance,
            crate::runtime::PaneForegroundProcessObservation {
                observation_id: fixture.observation_id,
                process_name: Some("sh".to_string()),
                process_group_id: Some(fixture.subshell_group),
                current_working_directory: Some("/tmp".to_string()),
                error: None,
            },
        )
        .unwrap();

    assert!(settled.applied);
    assert!(!fixture.service.pane_bootstrap_is_pending_for_tests("%1"));
    assert!(fixture.service.pane_environment_signature("%1").is_some());
    assert!(
        fixture
            .service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| transaction.turn_id == turn.turn_id)
    );
    let execution = fixture
        .service
        .agent_turn_executions()
        .get(&turn.turn_id)
        .unwrap();
    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert!(execution.action_results[0].error.is_none());
    let _ = fixture
        .process
        .terminate(std::time::Duration::from_millis(10));
}

/// Verifies an adapter-owned bootstrap withholds its environment until the
/// pane worker returns the explicitly correlated persistent receiver group.
///
/// Periodic foreground metadata may still describe the isolated `setsid`
/// bootstrap child when the end marker is parsed. That cache update must not
/// reject certification, publish path authority, or settle readiness. Only the
/// observation id and process generation emitted by the pending side effect
/// may complete the second certification phase.
#[test]
fn runtime_agent_subshell_bootstrap_waits_for_correlated_worker_observation() {
    let mut fixture = pending_agent_subshell_certification_fixture();
    let certification_timer_owner = fixture.observation_id.clone();
    assert!(fixture.service.pane_environment_signature("%1").is_none());
    assert!(fixture.service.pane_bootstrap_is_pending_for_tests("%1"));
    assert_eq!(
        fixture.service.pane_readiness_state("%1"),
        PaneReadinessState::Busy
    );
    assert!(
        fixture
            .service
            .running_shell_transaction_timers()
            .iter()
            .any(|timer| timer.marker == certification_timer_owner)
    );

    fixture
        .service
        .set_pane_readiness("%1", PaneReadinessState::Ready);
    assert_eq!(fixture.service.maybe_bootstrap_ready_panes().unwrap(), 0);
    assert!(fixture.service.pane_bootstrap_is_pending_for_tests("%1"));
    assert!(
        fixture
            .service
            .running_shell_transactions_for_tests()
            .values()
            .all(|transaction| transaction.kind != RunningShellTransactionKind::Bootstrap)
    );

    let stale = fixture
        .service
        .apply_pane_foreground_process_observation_transition(
            fixture.instance.clone(),
            crate::runtime::PaneForegroundProcessObservation {
                observation_id: "stale-observation".to_string(),
                process_name: Some("sh".to_string()),
                process_group_id: Some(fixture.subshell_group),
                current_working_directory: Some("/tmp".to_string()),
                error: None,
            },
        )
        .unwrap();
    assert!(!stale.applied);
    assert!(fixture.service.pane_environment_signature("%1").is_none());

    let settled = fixture
        .service
        .apply_pane_foreground_process_observation_transition(
            fixture.instance,
            crate::runtime::PaneForegroundProcessObservation {
                observation_id: fixture.observation_id,
                process_name: Some("sh".to_string()),
                process_group_id: Some(fixture.subshell_group),
                current_working_directory: Some("/tmp".to_string()),
                error: None,
            },
        )
        .unwrap();
    assert!(settled.applied);
    assert!(fixture.service.pane_environment_signature("%1").is_some());
    assert!(!fixture.service.pane_bootstrap_is_pending_for_tests("%1"));
    assert_eq!(
        fixture.service.pane_readiness_state("%1"),
        PaneReadinessState::Ready
    );
    assert!(
        fixture
            .service
            .pane_agent_subshell_certification_rejection("%1")
            .is_none()
    );
    assert!(
        fixture
            .service
            .running_shell_transaction_timers()
            .iter()
            .all(|timer| timer.marker != certification_timer_owner)
    );
    let _ = fixture
        .process
        .terminate(std::time::Duration::from_millis(10));
}

/// Verifies a stale-only completion observation cannot leave bootstrap and
/// provider authority pending indefinitely.
///
/// The completed bootstrap no longer exists in the running transaction map, so
/// this regression proves the replacement certification timer is visible,
/// expires the withheld environment, and fences a late exact worker event.
#[test]
fn runtime_agent_subshell_bootstrap_certification_times_out_after_stale_observation() {
    let mut fixture = pending_agent_subshell_certification_fixture();
    let certification_timer = fixture
        .service
        .running_shell_transaction_timers()
        .into_iter()
        .find(|timer| timer.marker == fixture.observation_id)
        .expect("pending completion certification should own a bootstrap timer");
    assert_eq!(
        certification_timer.kind,
        crate::runtime::RuntimeShellTransactionTimerKind::Bootstrap
    );
    assert!(certification_timer.timeout_ms > 0);
    let original_key = crate::runtime::RuntimeTimerKey::new(
        crate::runtime::RuntimeTimerKind::Bootstrap,
        fixture.original_timer.marker.clone(),
        fixture.original_timer.started_at_unix_ms,
    );
    let timer_reconciliation = fixture.service.shell_transaction_timer_transition(
        &std::collections::HashSet::from([original_key.clone()]),
        certification_timer.started_at_unix_ms,
    );
    assert!(timer_reconciliation.side_effects.iter().any(|effect| {
        matches!(
            effect,
            RuntimeSideEffect::CancelTimer { key } if key == &original_key
        )
    }));
    assert!(timer_reconciliation.side_effects.iter().any(|effect| {
        matches!(
            effect,
            RuntimeSideEffect::ScheduleTimer { key, .. }
                if key.owner_id == fixture.observation_id
                    && key.kind == crate::runtime::RuntimeTimerKind::Bootstrap
        )
    }));

    let stale = fixture
        .service
        .apply_pane_foreground_process_observation_transition(
            fixture.instance.clone(),
            crate::runtime::PaneForegroundProcessObservation {
                observation_id: "stale-observation".to_string(),
                process_name: Some("sh".to_string()),
                process_group_id: Some(fixture.subshell_group),
                current_working_directory: Some("/tmp".to_string()),
                error: None,
            },
        )
        .unwrap();
    assert!(!stale.applied);
    let deadline = certification_timer
        .started_at_unix_ms
        .saturating_add(certification_timer.timeout_ms);
    assert_eq!(
        fixture
            .service
            .apply_shell_transaction_timer_event(deadline)
            .unwrap(),
        1
    );

    assert!(!fixture.service.pane_bootstrap_is_pending_for_tests("%1"));
    assert!(fixture.service.pane_environment_signature("%1").is_none());
    assert_eq!(
        fixture
            .service
            .pane_agent_subshell_certification_rejection("%1"),
        Some("foreground_observation_timed_out")
    );
    assert_eq!(
        fixture.service.pane_readiness_state("%1"),
        PaneReadinessState::Degraded
    );
    assert!(
        fixture
            .service
            .running_shell_transaction_timers()
            .iter()
            .all(|timer| timer.marker != fixture.observation_id)
    );

    let late = fixture
        .service
        .apply_pane_foreground_process_observation_transition(
            fixture.instance,
            crate::runtime::PaneForegroundProcessObservation {
                observation_id: fixture.observation_id,
                process_name: Some("sh".to_string()),
                process_group_id: Some(fixture.subshell_group),
                current_working_directory: Some("/tmp".to_string()),
                error: None,
            },
        )
        .unwrap();
    assert!(!late.applied);
    assert!(fixture.service.pane_environment_signature("%1").is_none());
    let _ = fixture
        .process
        .terminate(std::time::Duration::from_millis(10));
}

/// Verifies a Mezzanine-owned agent subshell that completes a registered
/// bootstrap is accepted for stale-busy recovery without trusting its name.
#[test]
fn runtime_shell_dispatch_recovers_for_certified_agent_subshell_group() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();
    let subshell_group = primary_pid.saturating_add(1);
    certify_agent_subshell_foreground_group(&mut service, subshell_group);

    assert_eq!(
        service.pane_foreground_certified_shell_state("%1"),
        Some(true)
    );
    let diagnostic = service.pane_foreground_process_diagnostic("%1").json();
    assert_eq!(
        diagnostic["certified_shell_process_group_id"],
        subshell_group
    );
    assert_eq!(
        diagnostic["certified_shell_source"],
        "agent-subshell-bootstrap"
    );
    assert_eq!(diagnostic["certified_shell_is_foreground"], true);

    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "inspect").unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "shell-certified".to_string(),
        rationale: "inspect through the certified agent subshell".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect the working directory.".to_string(),
            command: "pwd".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    service.agent_turn_executions_mut().insert(
        turn.turn_id.clone(),
        mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "run shell action".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "inspect with the certified shell".to_string(),
                    thought: None,
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    actions: vec![action.clone()],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::running(
                &turn,
                &action,
                Vec::new(),
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    service.remove_pending_agent_provider_task(&turn.turn_id);
    service.set_pane_readiness("%1", PaneReadinessState::Busy);

    let execution = service
        .dispatch_stored_running_shell_actions(&turn.turn_id)
        .unwrap()
        .unwrap();

    assert_eq!(execution.action_results[0].status, ActionStatus::Running);
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Probing
    );
    assert!(
        service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| transaction.kind == RunningShellTransactionKind::ReadinessProbe)
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a certified agent subshell remains recognized through worker-cached
/// foreground metadata, then loses authority when agent mode restores the parent.
#[test]
fn runtime_agent_subshell_exit_invalidates_cached_certified_group() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();
    let subshell_group = primary_pid.saturating_add(1);
    certify_agent_subshell_foreground_group(&mut service, subshell_group);
    service
        .apply_pane_foreground_process_event("%1", "bash", subshell_group, None)
        .unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test("%1", None);
    let mut process = service.take_running_pane_process_for_adapter("%1").unwrap();

    assert_eq!(
        service.pane_foreground_certified_shell_state("%1"),
        Some(true)
    );
    service.set_pane_agent_subshell_certification_rejection_for_tests(
        "%1",
        RuntimeAgentSubshellCertificationRejection::TransactionFailed,
    );
    assert!(service.exit_agent_subshell_if_active("%1").unwrap());
    assert!(!service.agent_subshell_is_active("%1"));
    assert_eq!(
        service.pane_foreground_certified_shell_state("%1"),
        Some(false)
    );
    assert!(service.pane_environment_signature("%1").is_none());
    assert!(service.pane_bootstrap_is_pending_for_tests("%1"));
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Unknown
    );
    assert!(
        service
            .pane_agent_subshell_certification_rejection("%1")
            .is_none()
    );

    service
        .apply_pane_foreground_process_event("%1", "sh", primary_pid, None)
        .unwrap();
    assert_eq!(
        service.pane_foreground_certified_shell_state("%1"),
        Some(true)
    );
    let _ = process.terminate(std::time::Duration::from_millis(10));
}

/// Verifies a changed foreground group between bootstrap start and completion
/// cannot certify a nested shell, even when the reported process name is shell-like.
#[test]
fn runtime_agent_subshell_bootstrap_group_mismatch_fails_closed() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();
    let candidate_group = primary_pid.saturating_add(1);
    service.enter_agent_subshell("%1");
    service.begin_agent_subshell_shell_handoff("%1").unwrap();
    service.dispatch_bootstrap_to_pane("%1").unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test("%1", Some(candidate_group));
    let (marker, turn_id) = service
        .running_shell_transactions_for_tests()
        .iter()
        .find_map(|(marker, transaction)| {
            (transaction.kind == RunningShellTransactionKind::Bootstrap)
                .then(|| (marker.clone(), transaction.turn_id.clone()))
        })
        .unwrap();
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
        .observe_agent_shell_transaction_start("%1", &marker, &turn_id, "agent-%1", "%1")
        .unwrap();
    service
        .apply_pane_foreground_process_event("%1", "bash", candidate_group.saturating_add(1), None)
        .unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test("%1", Some(candidate_group.saturating_add(1)));
    service
        .observe_agent_shell_transaction_end("%1", &marker, &turn_id, "agent-%1", "%1", 0)
        .unwrap();

    assert_eq!(
        service.pane_foreground_certified_shell_state("%1"),
        Some(false)
    );
    assert!(service.pane_environment_signature("%1").is_none());
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Degraded
    );
    assert_eq!(
        service.pane_agent_subshell_certification_rejection("%1"),
        Some("foreground_process_group_changed")
    );
    let diagnostic = service.pane_foreground_process_diagnostic("%1").json();
    assert_eq!(
        diagnostic["agent_subshell_certification_rejection"],
        "foreground_process_group_changed"
    );
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event
            .payload
            .contains(r#""bootstrap":"certification_failed""#)
            && event
                .payload
                .contains(r#""reason":"foreground_process_group_changed""#)
    }));
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies a shell action blocked behind a persistent foreground program
/// settles after bounded idle recovery without injecting input into that program.
#[test]
fn runtime_shell_dispatch_fails_closed_after_persistent_foreground_block() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(None).unwrap();
    wait_until_primary_shell_foreground(&mut service, "%1");
    let primary_pid = service.pane_processes().primary_pid("%1").unwrap();
    service
        .pane_processes_mut()
        .set_foreground_process_group_id_for_test("%1", Some(primary_pid.saturating_add(1)));
    let _adapter_process = service.take_running_pane_process_for_adapter("%1").unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "inspect").unwrap();
    let turn = service
        .agent_turn_ledger()
        .turns()
        .iter()
        .find(|turn| turn.turn_id == started.turn_id)
        .cloned()
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "shell-blocked".to_string(),
        rationale: "inspect without disturbing the foreground program".to_string(),
        payload: mez_agent::AgentActionPayload::ShellCommand {
            summary: "Inspect the working directory.".to_string(),
            command: "pwd".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    service.agent_turn_executions_mut().insert(
        turn.turn_id.clone(),
        mez_agent::AgentTurnExecution {
            request: runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id),
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "run shell action".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "inspect with shell".to_string(),
                    thought: None,
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    actions: vec![action.clone()],
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![mez_agent::ActionResult::running(
                &turn,
                &action,
                Vec::new(),
                None,
            )],
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        },
    );
    service.remove_pending_agent_provider_task(&turn.turn_id);
    service.set_pane_readiness("%1", PaneReadinessState::Busy);

    service.recover_stranded_agent_shell_dispatches().unwrap();
    let recovery_observation_timer = service
        .running_shell_transaction_timers()
        .into_iter()
        .find(|timer| timer.kind == crate::runtime::RuntimeShellTransactionTimerKind::Bootstrap)
        .expect("blocked recovery should own a bounded foreground observation timer");
    assert_eq!(
        service
            .apply_shell_transaction_timer_event(
                recovery_observation_timer
                    .started_at_unix_ms
                    .saturating_add(recovery_observation_timer.timeout_ms),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        service.pane_readiness_state("%1"),
        PaneReadinessState::Degraded
    );
    assert_eq!(
        service.pending_shell_dispatch_blocked_recovery_attempts(&turn.turn_id, &action.id),
        0
    );
    let _stale_observation_effects = service.drain_pane_io_transition();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    service.set_pane_readiness("%1", PaneReadinessState::Busy);

    let foreign_process_groups = [
        primary_pid.saturating_add(1),
        primary_pid.saturating_add(2),
        primary_pid.saturating_add(2),
        primary_pid.saturating_add(2),
    ];
    let expected_confirmations = [1, 1, 2, 3];
    for (foreign_process_group, expected_attempts) in foreign_process_groups
        .into_iter()
        .zip(expected_confirmations)
    {
        service.recover_stranded_agent_shell_dispatches().unwrap();
        service.recover_stranded_agent_shell_dispatches().unwrap();
        let observations = service
            .drain_pane_io_transition()
            .side_effects
            .into_iter()
            .filter_map(|effect| match effect {
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
            .collect::<Vec<_>>();
        assert_eq!(
            observations.len(),
            1,
            "repeated recovery ticks must leave exactly one foreground observation outstanding"
        );
        let (instance, observation_id) = observations.into_iter().next().unwrap();
        service
            .apply_pane_foreground_process_observation_transition(
                instance,
                crate::runtime::PaneForegroundProcessObservation {
                    observation_id,
                    process_name: Some("foreground-program".to_string()),
                    process_group_id: Some(foreign_process_group),
                    current_working_directory: Some("/tmp".to_string()),
                    error: None,
                },
            )
            .unwrap();
        assert_eq!(
            service.pending_shell_dispatch_blocked_recovery_attempts(&turn.turn_id, &action.id,),
            expected_attempts
        );
    }
    assert!(service.agent_provider_task_is_pending(&turn.turn_id));

    let settled = service
        .dispatch_stored_running_shell_actions(&turn.turn_id)
        .unwrap()
        .unwrap();

    assert_eq!(settled.action_results[0].status, ActionStatus::Denied);
    assert_eq!(
        settled.action_results[0].error.as_ref().unwrap().code,
        "foreground_process_blocked_dispatch"
    );
    let diagnostic: serde_json::Value = serde_json::from_str(
        settled.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    let foreground_process = &diagnostic["foreground_process"];
    assert_eq!(diagnostic["reason"], "uncertified_foreground_process");
    assert_eq!(foreground_process["metadata_available"], true);
    assert_eq!(foreground_process["foreground_process_group_source"], "pty");
    assert_eq!(
        foreground_process["foreground_process_group_id"],
        primary_pid.saturating_add(1)
    );
    assert_eq!(foreground_process["primary_process_id"], primary_pid);
    assert_eq!(foreground_process["primary_shell_is_foreground"], false);
    assert_eq!(
        foreground_process["certified_shell_process_group_id"],
        serde_json::Value::Null
    );
    assert_eq!(foreground_process["certified_shell_is_foreground"], false);
    assert!(service.running_shell_transactions_for_tests().is_empty());
    assert_eq!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|record| record.turn_id == turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Failed
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies runtime shell dispatch honors per-action shell timeouts.
///
/// The MAAP parser and semantic lowering preserve `timeout_ms`; the runtime
/// must carry that bound into the live shell transaction instead of replacing it
/// with the enclosing turn's full timeout budget.
#[test]
fn runtime_shell_command_dispatch_uses_action_timeout() {
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
    mark_test_pane_ready(&mut service, &pane_id);
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-timeout","input":"run bounded grep"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    service.remove_pending_agent_provider_task("turn-1");
    let provider = RuntimeBatchProvider {
        response: mez_agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "shell action".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(mez_agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![mez_agent::AgentAction {
                    id: "shell-timeout".to_string(),
                    rationale: "run a bounded command".to_string(),
                    payload: mez_agent::AgentActionPayload::ShellCommand {
                        summary: "Run bounded grep".to_string(),
                        command: "grep -n needle file.txt".to_string(),
                        interactive: false,
                        stateful: false,
                        timeout_ms: Some(1500),
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();
    let transaction = service
        .running_shell_transactions_for_tests()
        .values()
        .find(|transaction| {
            matches!(
                transaction.kind,
                RunningShellTransactionKind::AgentAction { ref action_id }
                    if action_id == "shell-timeout"
            )
        })
        .unwrap();

    assert_eq!(transaction.timeout_ms, Some(1500));
    let _ = process.terminate(Duration::from_millis(10));
}
