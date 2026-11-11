//! Reentrant spawn-status delivery must not park a previous response's source.
//! These offline responses enter the real sync/async product application paths;
//! child allocation, message delivery, joins, and scheduler cleanup are real.

use super::*;

/// Starts a pane with enough capacity to expose reentrant spawn-status recovery.
fn parking_fixture() -> (DialogueRuntime, AgentTurnRecord) {
    let mut service = DialogueRuntime(test_runtime_service());
    service.set_agent_default_shell_mode(crate::runtime::config::ShellMode::Native);
    // This matrix tests scheduler/response ownership without an archive adapter.
    // Persistent original-target retention uses dialogue_fixture below and the
    // existing joined-dialogue lifecycle/retention tests.
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(8)
        .unwrap();
    service
        .attach_primary("response-parking", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_approval_policy_override("%1", Some(ApprovalPolicy::AutoAllow));
    let started = service
        .start_agent_prompt_turn("%1", "inspect then partition the task")
        .unwrap();
    let turn = service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    (service, turn)
}

/// Models the canonical provider's planned results without network traffic or
/// executing the batch before it enters the production application boundary.
async fn parking_response(
    service: &mut RuntimeSessionService,
    turn: &AgentTurnRecord,
    mut response: mez_agent::ModelResponse,
    asynchronous: bool,
) -> Result<mez_agent::AgentTurnExecution> {
    for (index, action) in response
        .action_batch
        .as_mut()
        .unwrap()
        .actions
        .iter_mut()
        .enumerate()
    {
        action.id = format!("action-{}", index + 1);
    }
    let profile = runtime_model_profile("runtime-batch", "test");
    let provider = RuntimeBatchProvider { response };
    if !asynchronous {
        return service.execute_agent_turn_with_provider(&turn.turn_id, &provider, profile);
    }
    let allowed = service.capture_agent_session_allowed_actions_for_pane(&turn.pane_id)?;
    let policy = service.permission_policy_for_turn(turn);
    let planning = crate::security::permissions::ProductPermissionPlanning::new(
        &policy,
        service.session_approvals(),
        None,
    );
    let runner = crate::integrations::agent::actions::AgentTurnRunner {
        provider: &provider,
        model_profile: profile.clone(),
        permissions: &planning,
        subagent_scope: None,
        subagent_scope_enforcement: &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
        available_mcp_servers: Vec::new(),
        available_mcp_tools: &[],
        memory_actions_enabled: false,
        issue_actions_enabled: false,
    };
    let execution = runner.run_turn_ref_with_allowed_actions(
        &mut mez_agent::AgentTurnLedger::new(false),
        turn.clone(),
        service.agent_turn_contexts().get(&turn.turn_id).unwrap(),
        Some(allowed),
        None,
        Default::default(),
    )?;
    service
        .apply_agent_provider_execution_async_for_tests(turn, &profile, execution)
        .await
}

/// A settled non-final source remains stored when a later spawn batch starts.
/// The second child's initial status reenters message recovery after the first
/// join exists. Neither that bridge nor a repeated sweep may park the old source.
/// Cover the original Ctrl+C chronology as well as the cancellation-free case.
#[tokio::test]
async fn runtime_joined_response_parking_multiple_spawns_after_settled_response() {
    for asynchronous in [false, true] {
        for interrupted in [false, true] {
            for count in [1, 2, 3] {
                let (mut service, mut parent) = parking_fixture();
                if interrupted {
                    service.stop_agent_turn_for_pane(&parent.pane_id).unwrap();
                    let next = service
                        .start_agent_prompt_turn("%1", "complete partition prompt")
                        .unwrap();
                    parent = service
                        .agent_turn_ledger()
                        .turn(&next.turn_id)
                        .unwrap()
                        .clone();
                }
                parking_response(
                    &mut service,
                    &parent,
                    runtime_say_response(&parent.turn_id, "inspection complete", false),
                    asynchronous,
                )
                .await
                .unwrap();
                let spawned = parking_response(
                    &mut service,
                    &parent,
                    dialogue_provider(
                        (0..count)
                            .map(|index| {
                                runtime_spawn_agent_action(
                                    &format!("partition-{index}"),
                                    "inspect one partition",
                                )
                            })
                            .collect(),
                    )
                    .response,
                    asynchronous,
                )
                .await
                .unwrap_or_else(|error| {
                    panic!("async={asynchronous} interrupted={interrupted} count={count}: {error}")
                });
                assert_eq!(
                    service
                        .agent_turn_ledger()
                        .turn(&parent.turn_id)
                        .unwrap()
                        .state,
                    AgentTurnState::Blocked
                );
                assert_eq!(
                    service
                        .agent_scheduler()
                        .waiting_turns()
                        .filter(|work| work.turn_id == parent.turn_id)
                        .count(),
                    1
                );
                assert!(
                    spawned
                        .action_results
                        .iter()
                        .all(|result| result.status == ActionStatus::Running)
                );
                assert_eq!(service.joined_subagent_dependency_count(), count);
                for _ in 0..2 {
                    service
                        .deliver_pending_runtime_agent_messages(
                            crate::runtime::current_unix_millis(),
                        )
                        .unwrap();
                    assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
                }
                let children = service
                    .agent_turn_ledger()
                    .turns()
                    .iter()
                    .filter(|turn| turn.agent_id != parent.agent_id)
                    .cloned()
                    .collect::<Vec<_>>();
                assert_eq!(
                    children.len(),
                    count,
                    "accepted spawns must never be replayed"
                );
                for child in &children {
                    service
                        .validate_native_agent_surface_startup(&child.pane_id)
                        .unwrap();
                    service.set_pane_approval_policy_override(
                        &child.pane_id,
                        Some(ApprovalPolicy::AutoAllow),
                    );
                }
                service.start_ready_agent_turns().unwrap();
                for (index, child) in children.iter().enumerate() {
                    parking_response(
                        &mut service,
                        child,
                        runtime_say_response_for_agent(
                            &child.turn_id,
                            &child.agent_id,
                            &format!("partition result {index}"),
                            true,
                        ),
                        asynchronous,
                    )
                    .await
                    .unwrap();
                    assert_eq!(
                        service.joined_subagent_dependency_count(),
                        count - index - 1
                    );
                    if index + 1 < count {
                        assert_eq!(
                            service
                                .agent_turn_ledger()
                                .turn(&parent.turn_id)
                                .unwrap()
                                .state,
                            AgentTurnState::Blocked
                        );
                    }
                }
                assert_eq!(
                    service
                        .agent_turn_ledger()
                        .turn(&parent.turn_id)
                        .unwrap()
                        .state,
                    AgentTurnState::Running
                );
                assert!(service.agent_provider_task_is_pending(&parent.turn_id));
                // Reacquisition is the parent continuation boundary. Explicit
                // Stop then exercises terminal cleanup without inventing a
                // subsequent provider claim in this direct-application fixture.
                service.stop_agent_turn_for_pane(&parent.pane_id).unwrap();
                assert!(!service.agent_work_is_scheduled(&parent.turn_id));
                let next = service
                    .start_agent_prompt_turn("%1", "subsequent prompt")
                    .unwrap();
                assert_eq!(
                    service
                        .agent_turn_ledger()
                        .turn(&next.turn_id)
                        .unwrap()
                        .state,
                    AgentTurnState::Running
                );
            }
        }
    }
}

/// A local application failure may arrive after the parent genuinely parked.
/// Its terminal path must retain/retire original joins through existing gates,
/// release the waiting claim, and permit a new prompt without touching children.
#[test]
fn runtime_joined_response_parking_failure_releases_waiting_parent_claim() {
    let (mut service, parent, children, _) = dialogue_fixture(2);
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(8)
        .unwrap();
    service.start_ready_agent_turns().unwrap();
    let error = MezError::invalid_state("local response application failed after parking");
    service.fail_agent_turn_after_provider_completion_application_error(
        &parent,
        "runtime-batch",
        Some(&runtime_model_profile("runtime-batch", "test")),
        &error,
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Failed
    );
    assert!(!service.agent_work_is_scheduled(&parent.turn_id));
    for child in &children {
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&child.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Running
        );
        assert!(service.agent_work_is_scheduled(&child.turn_id));
    }
    let next = service
        .start_agent_prompt_turn("%1", "new work after failed parent")
        .unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&next.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
}

/// Exercise every scheduler custody shape accepted by terminal cleanup. The
/// synthetic transitions here isolate cleanup from joined-source gates; real
/// waiting-parent failure and child obligations are covered separately above.
#[test]
fn runtime_joined_response_parking_terminal_cleanup_releases_all_scheduler_states() {
    for state in [
        "running",
        "blocked",
        "waiting",
        "reacquiring",
        "queued",
        "absent",
    ] {
        let (mut service, mut turn) = parking_fixture();
        match state {
            "blocked" => {
                service
                    .agent_scheduler_mut()
                    .block_running(&turn.turn_id)
                    .unwrap();
            }
            "waiting" => {
                service
                    .agent_scheduler_mut()
                    .wait_running(&turn.turn_id)
                    .unwrap();
            }
            "reacquiring" => {
                service
                    .agent_scheduler_mut()
                    .wait_running(&turn.turn_id)
                    .unwrap();
                service
                    .agent_scheduler_mut()
                    .requeue_waiting(&turn.turn_id)
                    .unwrap();
            }
            "queued" => {
                let next = service
                    .start_agent_prompt_turn("%1", "queued owner")
                    .unwrap();
                turn = service
                    .agent_turn_ledger()
                    .turn(&next.turn_id)
                    .unwrap()
                    .clone();
            }
            "absent" => {
                service.agent_scheduler_mut().cancel(&turn.turn_id).unwrap();
            }
            _ => {}
        }
        service
            .complete_running_agent_turn_and_start_ready(
                &turn,
                AgentTurnState::Failed,
                "test_terminal_claim_retirement",
            )
            .unwrap();
        assert!(
            !service.agent_work_is_scheduled(&turn.turn_id),
            "state={state}"
        );
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&turn.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Failed
        );
        if state != "queued" {
            let next = service
                .start_agent_prompt_turn("%1", "claim must now be available")
                .unwrap();
            assert_eq!(
                service
                    .agent_turn_ledger()
                    .turn(&next.turn_id)
                    .unwrap()
                    .state,
                AgentTurnState::Running,
                "state={state}"
            );
        }
    }
}
