//! Human lifecycle and approval gates around selected request outcomes.
//! Actual primary operations retain inhibition and distinguish intentional
//! teardown from unexpected work; plain waits keep their narrower wake policy.

use super::*;

/// Human inhibition must survive an exact owned terminal occurrence: repeated
/// delivery cannot commit the selected handoff, settle its wait, consume its
/// reservation or queue a model while paused. Explicit Resume may consume that
/// same evidence once. Alternatively Stop must leave the old turn interrupted;
/// replacing its conversation retires all remaining request custody and cannot
/// inject the old outcome into a genuinely new task.
#[test]
fn request_bound_owned_terminal_respects_pause_resume_and_stop_replacement() {
    for resume in [true, false] {
        let (mut service, parent, _, primary) = fixture_with_primary();
        let child = spawn_child(&mut service, &parent);
        assert!(
            service
                .validate_native_agent_surface_startup(&child.pane_id)
                .unwrap()
        );
        let message = send(&mut service, &parent, &child.agent_id, true);
        wait(&mut service, &parent, &message, None);
        let target = service
            .capture_agent_lifecycle_target(&primary, "%1")
            .unwrap();
        let generation = service
            .pause_agent_lifecycle_target(&primary, &target)
            .unwrap();
        let reserved = service.tracked_message_request_usage();
        service
            .complete_running_agent_turn_and_start_ready(
                &child,
                AgentTurnState::Completed,
                "terminal work retained behind explicit human inhibition",
            )
            .unwrap();
        for _ in 0..3 {
            service
                .deliver_pending_runtime_agent_messages(current_unix_millis())
                .unwrap();
            assert_eq!(
                service
                    .agent_turn_ledger()
                    .turn(&parent.turn_id)
                    .unwrap()
                    .state,
                AgentTurnState::Blocked
            );
            assert_eq!(
                service.agent_turn_executions()[&parent.turn_id].action_results[0].status,
                ActionStatus::Running
            );
            assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
            assert_eq!(service.tracked_message_request_usage(), reserved);
            assert!(
                !service.agent_turn_contexts()[&parent.turn_id]
                    .blocks()
                    .iter()
                    .any(|b| b.label.starts_with("request-handoff:"))
            );
        }
        if resume {
            let target = service
                .capture_agent_lifecycle_target(&primary, "%1")
                .unwrap();
            assert!(
                service
                    .resume_agent_lifecycle_target(&primary, &target, generation)
                    .unwrap()
            );
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
            assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
            assert!(service.agent_provider_task_is_pending(&parent.turn_id));
            execute(
                &mut service,
                &parent,
                AgentAction {
                    id: "close-paused-child".into(),
                    payload: AgentActionPayload::CloseAgent {
                        agent_id: child.agent_id.clone(),
                    },
                },
            );
            finish(&mut service, &parent);
        } else {
            service
                .execute_agent_shell_command(&primary, "/stop")
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
                AgentTurnState::Interrupted
            );
            assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
            assert_eq!(service.tracked_message_request_usage().1, 0);
            service
                .execute_agent_shell_command(&primary, "/new")
                .unwrap();
            service
                .deliver_pending_runtime_agent_messages(current_unix_millis())
                .unwrap();
            assert_ne!(
                service.agent_shell_store().get("%1").unwrap().session_id,
                parent.conversation_id
            );
            assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
            assert_eq!(
                service
                    .agent_turn_ledger()
                    .turn(&parent.turn_id)
                    .unwrap()
                    .state,
                AgentTurnState::Interrupted
            );
            assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
            let started = service
                .start_agent_prompt_turn("%1", "authorized independent replacement work")
                .unwrap();
            let replacement = service
                .agent_turn_ledger()
                .turn(&started.turn_id)
                .unwrap()
                .clone();
            assert_ne!(replacement.turn_id, parent.turn_id);
            assert!(
                !service.agent_turn_contexts()[&replacement.turn_id]
                    .blocks()
                    .iter()
                    .any(|b| b.label.starts_with("request-handoff:"))
            );
            service
                .dispatch_runtime_pane_close(
                    &primary,
                    &serde_json::json!({"pane_id":child.pane_id,"force":true}).to_string(),
                )
                .unwrap();
            finish(&mut service, &replacement);
        }
    }
}

/// Unrelated mail may resume a selected wait, but its continuation's genuinely
/// blocked Ask-mode action still owns approval. An exact owned task completing
/// meanwhile cannot settle that action, dispatch its send or queue another
/// provider. Only the actual primary decision releases the ordinary action;
/// the same request can then consume its retained terminal evidence once.
#[test]
fn request_bound_owned_terminal_retains_active_approval_until_primary_decision() {
    let (mut service, parent, peer, primary) = fixture_with_primary();
    service.configure_agent_scheduler_limit(2).unwrap();
    let child = spawn_child(&mut service, &parent);
    assert!(
        service
            .validate_native_agent_surface_startup(&child.pane_id)
            .unwrap()
    );
    service.start_ready_agent_turns().unwrap();
    let message = send(&mut service, &parent, &child.agent_id, true);
    wait(&mut service, &parent, &message, None);
    reply(
        &mut service,
        &parent,
        &peer,
        "approval-unrelated-mail",
        None,
    );
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    service.set_pane_approval_policy_override("%1", Some(mez_agent::ApprovalPolicy::Ask));
    let execution = execute(
        &mut service,
        &parent,
        AgentAction {
            id: "approval-owner".into(),
            payload: AgentActionPayload::SendMessage {
                expect_reply: false,
                recipient: format!("agent:{}", peer.agent_id),
                scope: Some("session".into()),
                content_type: "text/plain; charset=utf-8".into(),
                payload: "EXACT_PRIMARY_APPROVED_SEND".into(),
                correlation_id: None,
            },
        },
    );
    assert_eq!(execution.action_results[0].status, ActionStatus::Blocked);
    let approval = service
        .blocked_approvals()
        .pending()
        .into_iter()
        .find(|approval| {
            approval.pane_id == parent.pane_id && approval.action_kind == "send_message"
        })
        .unwrap()
        .clone();
    let reserved = service.tracked_message_request_usage();
    service
        .complete_running_agent_turn_and_start_ready(
            &child,
            AgentTurnState::Completed,
            "terminal outcome held behind primary approval",
        )
        .unwrap();
    for _ in 0..3 {
        service
            .deliver_pending_runtime_agent_messages(current_unix_millis())
            .unwrap();
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Blocked
        );
        assert_eq!(
            service.agent_turn_executions()[&parent.turn_id].action_results[0].status,
            ActionStatus::Blocked
        );
        assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
        assert_eq!(service.tracked_message_request_usage(), reserved);
        assert!(
            service
                .message_service()
                .receive_for(&peer.agent_id, u64::MAX)
                .iter()
                .all(|mail| mail.payload != "EXACT_PRIMARY_APPROVED_SEND")
        );
    }
    service
        .integration
        .blocked_approvals_mut()
        .decide_with_client_at(
            &approval.id,
            mez_agent::permissions::ApprovalDecision::Approve,
            None,
            Some(primary.as_str().to_string()),
            crate::runtime::current_unix_seconds(),
        )
        .unwrap();
    let decided = service
        .blocked_approvals()
        .get(&approval.id)
        .unwrap()
        .clone();
    assert_eq!(
        service
            .resume_approved_blocked_agent_action(&approval.id, &decided, &primary)
            .unwrap(),
        Some(1)
    );
    assert_eq!(
        service
            .message_service()
            .receive_for(&peer.agent_id, u64::MAX)
            .iter()
            .filter(|mail| mail.payload == "EXACT_PRIMARY_APPROVED_SEND")
            .count(),
        1
    );
    wait(&mut service, &parent, &message, None);
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
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    service.set_pane_approval_policy_override("%1", Some(mez_agent::ApprovalPolicy::AutoAllow));
    execute(
        &mut service,
        &parent,
        AgentAction {
            id: "close-approved-child".into(),
            payload: AgentActionPayload::CloseAgent {
                agent_id: child.agent_id.clone(),
            },
        },
    );
    finish(&mut service, &parent);
}

/// Requested owned-child teardown is neither successful work nor unexpected
/// failure progress. The actual primary pane-close path may settle a selected
/// request only as opaque unavailable, and must leave an unrelated plain wait
/// parked. Repeated sweeps cannot repeat the handoff or expose private output.
#[test]
fn request_bound_requested_child_close_only_wakes_explicit_selected_wait() {
    for tracked in [true, false] {
        let (mut service, parent, _, primary) = fixture_with_primary();
        let child = spawn_child(&mut service, &parent);
        assert!(
            service
                .validate_native_agent_surface_startup(&child.pane_id)
                .unwrap()
        );
        let message = send(&mut service, &parent, &child.agent_id, tracked);
        if tracked {
            wait(&mut service, &parent, &message, None);
        } else {
            execute(
                &mut service,
                &parent,
                AgentAction {
                    id: "plain-child-retirement-wait".into(),
                    payload: AgentActionPayload::Wait {
                        for_message: None,
                        from_agent: None,
                    },
                },
            );
        }
        service
            .dispatch_runtime_pane_close(
                &primary,
                &serde_json::json!({"pane_id":child.pane_id,"force":true}).to_string(),
            )
            .unwrap();
        for _ in 0..3 {
            service
                .deliver_pending_runtime_agent_messages(current_unix_millis())
                .unwrap();
            assert_eq!(
                service
                    .agent_turn_ledger()
                    .turn(&parent.turn_id)
                    .unwrap()
                    .state,
                if tracked {
                    AgentTurnState::Running
                } else {
                    AgentTurnState::Blocked
                }
            );
            assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        }
        let sources: Vec<_> = service.agent_turn_contexts()[&parent.turn_id]
            .blocks()
            .iter()
            .filter(|block| block.label.starts_with("request-handoff:"))
            .collect();
        if tracked {
            let outcome: serde_json::Value = serde_json::from_str(
                service.agent_turn_executions()[&parent.turn_id].action_results[0]
                    .structured_content_json
                    .as_deref()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(outcome["state"], "peer_unavailable");
            assert_eq!(outcome["progress_witness"], false);
            assert_eq!(sources.len(), 1);
            assert!(!sources[0].content.contains(&child.turn_id));
            finish(&mut service, &parent);
        } else {
            assert!(sources.is_empty());
            assert_eq!(
                service.agent_turn_executions()[&parent.turn_id].action_results[0].status,
                ActionStatus::Running
            );
            assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
            service
                .execute_agent_shell_command(&primary, "/stop")
                .unwrap();
            assert_eq!(
                service
                    .agent_turn_ledger()
                    .turn(&parent.turn_id)
                    .unwrap()
                    .state,
                AgentTurnState::Interrupted
            );
        }
    }
}
