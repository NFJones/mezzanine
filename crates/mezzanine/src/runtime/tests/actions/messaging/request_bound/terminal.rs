//! Acceptance-time owned-task results, bounded caches and cohort consumption.
//! Later persistent tasks cannot replace the captured attempt, and one exact
//! terminal occurrence supplies new progress at most once across its cohort.

use super::*;

/// A genuinely spawned persistent child's exact live task is frozen at send.
/// Its normal terminal execution, without any model-authored reply, supplies an
/// untrusted result reference and lets the same original parent complete.
#[test]
fn request_bound_owned_child_success_without_model_reply_completes_parent() {
    let (mut service, parent, _) = fixture();
    let child = spawn_child(&mut service, &parent);
    let message = send(&mut service, &parent, &child.agent_id, true);
    wait(&mut service, &parent, &message, None);
    execute(
        &mut service,
        &child,
        AgentAction {
            id: "child-result".into(),
            payload: AgentActionPayload::Say {
                status: SayStatus::Final,
                text: "verified child output".into(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
            },
        },
    );
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    let result = &service.agent_turn_executions()[&parent.turn_id].action_results[0];
    assert!(
        result
            .structured_content_json
            .as_ref()
            .unwrap()
            .contains("peer_terminal")
    );
    assert!(
        service.agent_turn_contexts()[&parent.turn_id]
            .blocks()
            .iter()
            .any(|block| block.content.contains("verified child output")
                && block.content.contains(&child.turn_id))
    );
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    execute(
        &mut service,
        &parent,
        AgentAction {
            id: "retire-child".into(),
            payload: AgentActionPayload::CloseAgent {
                agent_id: child.agent_id.clone(),
            },
        },
    );
    finish(&mut service, &parent);
}

/// A genuine native provider-failure settlement reuses its original failure
/// diagnostic once and adds only an exact request handoff reference. The selected
/// task wakes its parent without a fabricated peer reply or duplicate output.
#[test]
fn request_bound_owned_failure_reuses_native_source_without_duplicate_diagnostic() {
    let (mut service, parent, _) = fixture();
    let child = spawn_child(&mut service, &parent);
    let message = send(&mut service, &parent, &child.agent_id, true);
    wait(&mut service, &parent, &message, None);
    service
        .fail_agent_turn_for_provider_error(
            &child,
            "runtime-batch",
            &runtime_model_profile("runtime-batch", "test"),
            &MezError::invalid_state("offline tracked child failure"),
        )
        .unwrap();
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    let context = &service.agent_turn_contexts()[&parent.turn_id];
    assert_eq!(
        context
            .blocks()
            .iter()
            .filter(|block| block.label.starts_with("owned-child-failure:"))
            .count(),
        1
    );
    let handoff = context
        .blocks()
        .iter()
        .filter(|block| block.label.starts_with("request-handoff:"))
        .collect::<Vec<_>>();
    assert_eq!(handoff.len(), 1);
    assert!(
        handoff[0]
            .content
            .contains("existing native failure receipt")
    );
    assert!(!handoff[0].content.contains("offline tracked child failure"));
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    execute(
        &mut service,
        &parent,
        AgentAction {
            id: "close-failed-child".into(),
            payload: AgentActionPayload::CloseAgent {
                agent_id: child.agent_id.clone(),
            },
        },
    );
    finish(&mut service, &parent);
}

/// A real state-only lifecycle completion has no provider execution to bridge,
/// but it is still successful terminal work of the exact task captured at send.
/// The same parent must receive terminal occurrence and progress, rather than
/// intentional-retirement evidence, and duplicate observations must stay inert.
#[test]
fn request_bound_terminal_state_only_success_is_not_intentional_retirement() {
    let (mut service, parent, _) = fixture();
    let child = spawn_child(&mut service, &parent);
    assert!(
        service
            .validate_native_agent_surface_startup(&child.pane_id)
            .unwrap()
    );
    let message = send(&mut service, &parent, &child.agent_id, true);
    wait(&mut service, &parent, &message, None);
    assert!(!service.agent_turn_executions().contains_key(&child.turn_id));
    service
        .complete_running_agent_turn_and_start_ready(
            &child,
            AgentTurnState::Completed,
            "state-only terminal regression",
        )
        .unwrap();
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&child.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    let wait = &service.agent_turn_executions()[&parent.turn_id].action_results[0];
    let outcome: serde_json::Value =
        serde_json::from_str(wait.structured_content_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        outcome["state"], "peer_terminal",
        "state-only success was mislabeled as intentional retirement"
    );
    assert_eq!(outcome["progress_witness"], true);
    assert!(
        service.agent_turn_contexts()[&parent.turn_id]
            .blocks()
            .iter()
            .any(|b| b.label.starts_with("request-handoff:")
                && b.content.contains("completed without provider output"))
    );
    let sources = service.agent_turn_contexts()[&parent.turn_id]
        .blocks()
        .iter()
        .filter(|b| b.label.starts_with("request-handoff:"))
        .count();
    service
        .emit_subagent_task_result_for_state(&child, AgentTurnState::Completed)
        .unwrap();
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    assert_eq!(
        service.agent_turn_contexts()[&parent.turn_id]
            .blocks()
            .iter()
            .filter(|b| b.label.starts_with("request-handoff:"))
            .count(),
        sources
    );
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    execute(
        &mut service,
        &parent,
        AgentAction {
            id: "close-state-only-child".into(),
            payload: AgentActionPayload::CloseAgent {
                agent_id: child.agent_id.clone(),
            },
        },
    );
    finish(&mut service, &parent);
}

/// A real goal-loop iteration can complete while its logical controller retains
/// the result and creates later work. Only the accepted exact iteration may
/// satisfy the tracked request; its result cannot be lost at the controller's
/// early return or replaced by the new iteration. Plain waits remain parked.
#[test]
fn request_bound_terminal_loop_continuation_preserves_exact_iteration_result() {
    for tracked in [false, true] {
        let (mut service, parent, _) = fixture();
        service.configure_agent_scheduler_limit(2).unwrap();
        let initial = spawn_child(&mut service, &parent);
        assert!(
            service
                .validate_native_agent_surface_startup(&initial.pane_id)
                .unwrap()
        );
        service.start_ready_agent_turns().unwrap();
        service
            .complete_running_agent_turn_and_start_ready(
                &initial,
                AgentTurnState::Completed,
                "prepare persistent loop task",
            )
            .unwrap();
        service.execute_agent_shell_loop_command(&initial.pane_id,
            "/loop --limit 3 --goal 'complete the later iteration' produce the first iteration result").unwrap();
        let child = service
            .agent_turn_ledger()
            .latest_turn_for_pane(&initial.pane_id)
            .unwrap()
            .clone();
        assert_ne!(child.turn_id, initial.turn_id);
        let message = send(&mut service, &parent, &child.agent_id, tracked);
        if tracked {
            wait(&mut service, &parent, &message, None);
        } else {
            execute(
                &mut service,
                &parent,
                AgentAction {
                    id: "plain-loop-wait".into(),
                    payload: AgentActionPayload::Wait {
                        for_message: None,
                        from_agent: None,
                    },
                },
            );
        }
        assert!(
            service
                .validate_native_agent_surface_startup(&child.pane_id)
                .unwrap()
        );
        service.start_ready_agent_turns().unwrap();
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&child.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Running
        );
        let execution = execute(
            &mut service,
            &child,
            AgentAction {
                id: "first-loop-result".into(),
                payload: AgentActionPayload::Say {
                    status: SayStatus::Final,
                    text: "EXACT_FIRST_ITERATION_OUTPUT".into(),
                    content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
                },
            },
        );
        service
            .deliver_pending_runtime_agent_messages(current_unix_millis())
            .unwrap();
        let later = service
            .agent_turn_ledger()
            .latest_turn_for_pane(&initial.pane_id)
            .unwrap()
            .clone();
        assert_ne!(
            later.turn_id, child.turn_id,
            "the real controller must create its next iteration"
        );
        if tracked {
            assert_eq!(
                service
                    .agent_turn_ledger()
                    .turn(&parent.turn_id)
                    .unwrap()
                    .state,
                AgentTurnState::Running,
                "exact first task result was lost at the loop controller early return"
            );
            let outcome: serde_json::Value = serde_json::from_str(
                service.agent_turn_executions()[&parent.turn_id].action_results[0]
                    .structured_content_json
                    .as_deref()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(outcome["state"], "peer_terminal");
            assert_eq!(outcome["progress_witness"], true);
            let source = service.agent_turn_contexts()[&parent.turn_id]
                .blocks()
                .iter()
                .find(|b| b.label.starts_with("request-handoff:"))
                .unwrap();
            assert!(source.content.contains("EXACT_FIRST_ITERATION_OUTPUT"));
            assert!(source.content.contains(&child.turn_id));
            assert!(!source.content.contains(&later.turn_id));
            assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
            let before = service.agent_turn_contexts()[&parent.turn_id]
                .blocks()
                .to_vec();
            service
                .emit_subagent_task_result_for_execution(&child, &execution)
                .unwrap();
            assert_eq!(
                service.agent_turn_contexts()[&parent.turn_id].blocks(),
                before
            );
        } else {
            assert_eq!(
                service
                    .agent_turn_ledger()
                    .turn(&parent.turn_id)
                    .unwrap()
                    .state,
                AgentTurnState::Blocked
            );
            assert!(
                !service.agent_turn_contexts()[&parent.turn_id]
                    .blocks()
                    .iter()
                    .any(|b| b.label.starts_with("request-handoff:"))
            );
        }
        // Cleanup stays with the fixture's real lifecycle owners.
        service.terminate_all_pane_processes().unwrap();
    }
}

/// A genuine child can finish between accepted send and selected wait. Its
/// retained result must charge actual owned String capacity, not only bounded
/// text length: formatter growth can exceed the already reserved handoff bytes.
/// ASCII and multibyte full-size outputs must fit before park, retain their
/// exact attempt, then consume and release once through the normal wait owner.
#[test]
fn request_bound_terminal_cached_allocation_fits_preaccepted_handoff_headroom() {
    for output in ["x".repeat(3072), "界".repeat(1024), "界".repeat(1025)] {
        let (mut service, parent, _) = fixture();
        service.configure_agent_scheduler_limit(2).unwrap();
        let child = spawn_child(&mut service, &parent);
        assert!(
            service
                .validate_native_agent_surface_startup(&child.pane_id)
                .unwrap()
        );
        service.start_ready_agent_turns().unwrap();
        let message = send(&mut service, &parent, &child.agent_id, true);
        execute(
            &mut service,
            &child,
            AgentAction {
                id: "budgeted-terminal".into(),
                payload: AgentActionPayload::Say {
                    status: SayStatus::Final,
                    text: output,
                    content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
                },
            },
        );
        let allocations = service.tracked_request_outcome_allocations_for_tests();
        assert_eq!(
            allocations.len(),
            1,
            "pre-park result must retain actual request custody"
        );
        let (length, capacity, reserved) = allocations[0];
        assert!(
            capacity <= reserved,
            "cached terminal allocation exceeds its preaccepted headroom: length={length} capacity={capacity} reserved={reserved}"
        );
        assert_eq!(
            capacity, length,
            "cached outcome retains uncharged formatting slack"
        );
        wait(&mut service, &parent, &message, None);
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        assert!(
            service
                .tracked_request_outcome_allocations_for_tests()
                .is_empty()
        );
        let outcome: serde_json::Value = serde_json::from_str(
            service.agent_turn_executions()[&parent.turn_id].action_results[0]
                .structured_content_json
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(outcome["state"], "peer_terminal");
        assert_eq!(outcome["progress_witness"], true);
        execute(
            &mut service,
            &parent,
            AgentAction {
                id: "close-budgeted-child".into(),
                payload: AgentActionPayload::CloseAgent {
                    agent_id: child.agent_id.clone(),
                },
            },
        );
        finish(&mut service, &parent);
    }
}

/// Two genuine requests can capture the same live owned attempt before it
/// completes. Each selected request must receive that exact result and release
/// its own reservation, while consumption of the first result prevents the
/// cohort from counting the same terminal work as progress again. A duplicate
/// native terminal observation between selections cannot reset that witness.
#[test]
fn request_bound_terminal_cohort_consumes_progress_once_across_requests() {
    let (mut service, parent, _) = fixture();
    let child = spawn_child(&mut service, &parent);
    assert!(
        service
            .validate_native_agent_surface_startup(&child.pane_id)
            .unwrap()
    );
    let first = send(&mut service, &parent, &child.agent_id, true);
    let second = send(&mut service, &parent, &child.agent_id, true);
    assert_ne!(first, second);
    wait(&mut service, &parent, &first, None);
    service
        .complete_running_agent_turn_and_start_ready(
            &child,
            AgentTurnState::Completed,
            "exact cohort task terminal",
        )
        .unwrap();
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    let outcome: serde_json::Value = serde_json::from_str(
        service.agent_turn_executions()[&parent.turn_id].action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(outcome["state"], "peer_terminal");
    assert_eq!(outcome["progress_witness"], true);
    let first_generation = outcome["dependency_generation"].clone();
    assert_ne!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    service
        .emit_subagent_task_result_for_state(&child, AgentTurnState::Completed)
        .unwrap();
    wait(&mut service, &parent, &second, None);
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    let outcome: serde_json::Value = serde_json::from_str(
        service.agent_turn_executions()[&parent.turn_id].action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(outcome["state"], "peer_terminal");
    assert_eq!(outcome["progress_witness"], false);
    assert_ne!(outcome["dependency_generation"], first_generation);
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    let context = &service.agent_turn_contexts()[&parent.turn_id];
    let sources: Vec<_> = context
        .blocks()
        .iter()
        .filter(|block| block.label.starts_with("request-handoff:"))
        .collect();
    assert_eq!(sources.len(), 2);
    assert!(
        sources
            .iter()
            .all(|block| block.content.contains(&child.turn_id))
    );
    execute(
        &mut service,
        &parent,
        AgentAction {
            id: "close-cohort-child".into(),
            payload: AgentActionPayload::CloseAgent {
                agent_id: child.agent_id.clone(),
            },
        },
    );
    finish(&mut service, &parent);
}

/// Tracking an idle persistent peer freezes registration-only evidence because
/// no task is live at acceptance. Delivery may legitimately start a later
/// receive-triggered task, but that task's successful terminal work must never
/// be adopted by the original request. Only actual registration retirement
/// then settles it as opaque unavailability with no new progress witness.
#[test]
fn request_bound_idle_peer_does_not_adopt_later_receive_task_terminal() {
    let (mut service, parent, _, primary) = fixture_with_primary();
    service.configure_agent_scheduler_limit(2).unwrap();
    let initial = spawn_child(&mut service, &parent);
    assert!(
        service
            .validate_native_agent_surface_startup(&initial.pane_id)
            .unwrap()
    );
    service.start_ready_agent_turns().unwrap();
    service
        .complete_running_agent_turn_and_start_ready(
            &initial,
            AgentTurnState::Completed,
            "leave persistent peer idle before acceptance",
        )
        .unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .latest_turn_for_pane(&initial.pane_id)
            .unwrap()
            .state,
        AgentTurnState::Completed
    );
    let message = send(&mut service, &parent, &initial.agent_id, true);
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    let later = service
        .agent_turn_ledger()
        .latest_turn_for_pane(&initial.pane_id)
        .unwrap()
        .clone();
    assert_ne!(later.turn_id, initial.turn_id);
    assert_eq!(later.state, AgentTurnState::Running);
    wait(&mut service, &parent, &message, None);
    service
        .complete_running_agent_turn_and_start_ready(
            &later,
            AgentTurnState::Completed,
            "later unbound task must not satisfy earlier request",
        )
        .unwrap();
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked,
        "request adopted a task that was not live at acceptance"
    );
    assert_eq!(
        service.agent_turn_executions()[&parent.turn_id].action_results[0].status,
        ActionStatus::Running
    );
    assert!(
        !service.agent_turn_contexts()[&parent.turn_id]
            .blocks()
            .iter()
            .any(|block| block.label.starts_with("request-handoff:"))
    );
    assert_ne!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    service
        .dispatch_runtime_pane_close(
            &primary,
            &serde_json::json!({"pane_id":initial.pane_id,"force":true}).to_string(),
        )
        .unwrap();
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    let outcome: serde_json::Value = serde_json::from_str(
        service.agent_turn_executions()[&parent.turn_id].action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(outcome["state"], "peer_unavailable");
    assert_eq!(outcome["progress_witness"], false);
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    finish(&mut service, &parent);
}
