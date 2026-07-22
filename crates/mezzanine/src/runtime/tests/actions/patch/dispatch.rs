//! Runtime tests for actions patch dispatch behavior.

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

    for expected_attempts in 1..=3 {
        service.recover_stranded_agent_shell_dispatches().unwrap();
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
