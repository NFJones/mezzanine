//! Human lifecycle and approval gates around selected request outcomes.
//! Actual primary operations retain inhibition and distinguish intentional
//! teardown from unexpected work; plain waits keep their narrower wake policy.

use super::*;

/// A primary Stop preserves direct-task redirection but actually cancels loop
/// and selected-loop tasks while their peer registration remains available.
/// A retained direct attempt must keep its selected wait parked until explicit
/// final closure; actual cancellation provides only opaque unavailability and
/// no work progress. Plain Wait stays parked. The genuine routing-selection
/// boundary must honor subagents' in-place policy and preserve the captured
/// task, rather than fabricate a managed worker or restart work.
#[test]
fn request_bound_requested_stop_and_redirection_preserve_exact_owner_gates() {
    for mode in ["direct", "loop", "selected-loop"] {
        for tracked in [false, true] {
            let (mut service, parent, _, primary) = fixture_with_primary();
            service.configure_agent_scheduler_limit(2).unwrap();
            let initial = spawn_child(&mut service, &parent);
            assert!(
                service
                    .validate_native_agent_surface_startup(&initial.pane_id)
                    .unwrap()
            );
            service.start_ready_agent_turns().unwrap();
            let child = if mode == "direct" {
                initial.clone()
            } else {
                service
                    .complete_running_agent_turn_and_start_ready(
                        &initial,
                        AgentTurnState::Completed,
                        "idle persistent child before explicit loop",
                    )
                    .unwrap();
                service.execute_agent_shell_loop_command(&initial.pane_id,
                    "/loop --limit 3 --goal 'do independent work' perform the current iteration").unwrap();
                service
                    .agent_turn_ledger()
                    .latest_turn_for_pane(&initial.pane_id)
                    .unwrap()
                    .clone()
            };
            assert!(
                service
                    .validate_native_agent_surface_startup(&child.pane_id)
                    .unwrap()
            );
            service.start_ready_agent_turns().unwrap();
            let message = send(&mut service, &parent, &child.agent_id, tracked);
            if tracked {
                wait(&mut service, &parent, &message, None);
            } else {
                execute(
                    &mut service,
                    &parent,
                    AgentAction {
                        id: "plain-stop-wait".into(),
                        payload: AgentActionPayload::Wait {
                            for_message: None,
                            from_agent: None,
                        },
                    },
                );
            }
            if mode == "selected-loop" {
                assert_eq!(
                    service.auto_sizing_routing_policy_for_turn(&child),
                    mez_agent::AutoSizingRoutingPolicy::InPlace
                );
                assert!(!service.agent_turn_routing_applied(&child.turn_id));
                let profile = service
                    .agent_turn_model_profile(&child.turn_id)
                    .unwrap()
                    .clone();
                service
                    .apply_routing_selected_transition(
                        &AgentId::opaque(child.agent_id.clone()).unwrap(),
                        &child.turn_id,
                        mez_agent::AutoSizingRoutingSelection {
                            selected_profile: profile,
                            selected_profile_name: "default".into(),
                            routing_token_usage_by_model: Default::default(),
                            decision_summary: None,
                            fallback: None,
                        },
                    )
                    .unwrap();
                assert!(service.agent_turn_routing_applied(&child.turn_id));
                assert!(!service.has_active_routed_workflow(&child.turn_id));
                assert_eq!(
                    service
                        .agent_turn_ledger()
                        .turn(&child.turn_id)
                        .unwrap()
                        .state,
                    AgentTurnState::Running
                );
            }
            service
                .execute_agent_shell_command_for_pane(&primary, &child.pane_id, "/stop")
                .unwrap();
            let redirecting = service.interrupted_subagent_awaits_redirection(&child.turn_id);
            assert_eq!(redirecting, mode == "direct");
            let tasks = service.agent_turn_ledger().turns().len();
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
                    if tracked && !redirecting {
                        AgentTurnState::Running
                    } else {
                        AgentTurnState::Blocked
                    },
                    "{mode}/{tracked}"
                );
                assert_eq!(
                    service
                        .agent_turn_ledger()
                        .turn(&child.turn_id)
                        .unwrap()
                        .state,
                    AgentTurnState::Interrupted
                );
                assert!(!service.agent_provider_task_is_pending(&child.turn_id));
                assert_eq!(service.agent_turn_ledger().turns().len(), tasks);
            }
            if tracked && redirecting {
                assert_eq!(service.tracked_message_request_usage().0.0, 1);
                assert!(
                    !service.agent_turn_contexts()[&parent.turn_id]
                        .blocks()
                        .iter()
                        .any(|block| block.label.starts_with("request-handoff:"))
                );
            }
            assert!(
                service
                    .message_service()
                    .registered_identity(&AgentId::opaque(child.agent_id.clone()).unwrap())
                    .is_some()
            );
            if redirecting {
                service
                    .session
                    .select_pane_global(&primary, &child.pane_id)
                    .unwrap();
                service
                    .execute_agent_shell_command(&primary, "perform explicitly redirected work")
                    .unwrap();
                let redirected = service
                    .agent_turn_ledger()
                    .latest_turn_for_pane(&child.pane_id)
                    .unwrap()
                    .clone();
                assert_ne!(redirected.turn_id, child.turn_id);
                assert_eq!(redirected.agent_id, child.agent_id);
                execute(
                    &mut service,
                    &redirected,
                    AgentAction {
                        id: "redirected-result".into(),
                        payload: AgentActionPayload::Say {
                            status: SayStatus::Final,
                            text: "NEW_TASK_NOT_OLD_REQUEST".into(),
                            content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
                        },
                    },
                );
                service
                    .deliver_pending_runtime_agent_messages(current_unix_millis())
                    .unwrap();
                assert_eq!(service.agent_turn_ledger().turns().len(), tasks + 1);
                if tracked
                    && service
                        .agent_turn_ledger()
                        .turn(&parent.turn_id)
                        .unwrap()
                        .state
                        == AgentTurnState::Running
                {
                    let result: serde_json::Value = serde_json::from_str(
                        service.agent_turn_executions()[&parent.turn_id].action_results[0]
                            .structured_content_json
                            .as_deref()
                            .unwrap(),
                    )
                    .unwrap();
                    assert_eq!(result["state"], "peer_unavailable");
                    assert_eq!(result["progress_witness"], false);
                } else {
                    assert_eq!(
                        service
                            .agent_turn_ledger()
                            .turn(&parent.turn_id)
                            .unwrap()
                            .state,
                        AgentTurnState::Blocked
                    );
                }
                assert!(
                    service.agent_turn_contexts()[&parent.turn_id]
                        .blocks()
                        .iter()
                        .filter(|block| block.label.starts_with("request-handoff:"))
                        .all(|block| !block.content.contains("NEW_TASK_NOT_OLD_REQUEST"))
                );
                service
                    .session
                    .select_pane_global(&primary, &parent.pane_id)
                    .unwrap();
            }
            service
                .dispatch_runtime_pane_close(
                    &primary,
                    &serde_json::json!({"pane_id":child.pane_id,"force":true}).to_string(),
                )
                .unwrap();
            service
                .deliver_pending_runtime_agent_messages(current_unix_millis())
                .unwrap();
            if tracked {
                let result: serde_json::Value = serde_json::from_str(
                    service.agent_turn_executions()[&parent.turn_id].action_results[0]
                        .structured_content_json
                        .as_deref()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(result["state"], "peer_unavailable");
                assert_eq!(result["progress_witness"], false);
                assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
                assert_eq!(
                    service.agent_turn_contexts()[&parent.turn_id]
                        .blocks()
                        .iter()
                        .filter(|block| block.label.starts_with("request-handoff:"))
                        .count(),
                    1
                );
                finish(&mut service, &parent);
            } else {
                assert_eq!(
                    service.agent_turn_executions()[&parent.turn_id].action_results[0].status,
                    ActionStatus::Running
                );
                assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
                assert!(
                    !service.agent_turn_contexts()[&parent.turn_id]
                        .blocks()
                        .iter()
                        .any(|block| block.label.starts_with("request-handoff:"))
                );
            }
            service.terminate_all_pane_processes().unwrap();
        }
    }
}

/// Native process uncertainty or a mismatched lifetime observation must hold
/// an exact selected edge without context, progress or provider admission.
/// Actual same-root observation permits recovery after the injected read fault;
/// the injected replacement identity is only a negative comparison control.
/// Separately, a real root appearing after process-free acceptance cannot inherit
/// that old edge at all. Primary Stop/new retires it without resurrecting work.
#[test]
fn request_bound_parent_process_fences_uncertainty_and_new_root_inheritance() {
    use crate::runtime::processes::{
        RuntimePaneProcessIdentityInjection, RuntimePaneProcessIdentityUnavailable,
    };
    for mode in [
        "unreadable",
        "changed-token",
        "replacement-observation",
        "new-root",
    ] {
        let (mut service, parent, peer, primary) = if mode == "new-root" {
            fixture_with_primary()
        } else {
            fixture_with_primary_provider_claims()
        };
        let baseline =
            (mode != "new-root").then(|| service.pane_process_identity(&parent.pane_id).unwrap());
        let message = send(&mut service, &parent, peer.agent_id.as_str(), true);
        wait(&mut service, &parent, &message, None);
        let reserved = service.tracked_message_request_usage();
        assert!(
            service
                .control
                .message_service_mut()
                .retire_observational_identity(&peer.agent_id)
        );
        if mode == "new-root" {
            service.start_initial_pane_process(Some("cat")).unwrap();
            assert!(
                service
                    .primary_pid_for_live_pane_process(&parent.pane_id)
                    .is_some()
            );
        }
        let sequence = service.message_service().next_message_sequence();
        for _ in 0..3 {
            for _ in 0..2 {
                let fault = match mode {
                    "unreadable" => Some(RuntimePaneProcessIdentityInjection::Unavailable(
                        RuntimePaneProcessIdentityUnavailable::ExecutableUnreadable,
                    )),
                    "changed-token" => Some(RuntimePaneProcessIdentityInjection::Unavailable(
                        RuntimePaneProcessIdentityUnavailable::StartTokenChanged,
                    )),
                    "replacement-observation" => {
                        let actual = baseline.as_ref().unwrap();
                        Some(RuntimePaneProcessIdentityInjection::Identity {
                            role: actual.role,
                            generation: actual.generation,
                            process_id: actual.process_id,
                            start_token: actual.start_token.checked_add(1).unwrap(),
                            executable_path: actual.executable_path.clone(),
                            live_start_token: None,
                        })
                    }
                    "new-root" => None,
                    _ => unreachable!(),
                };
                if let Some(fault) = fault {
                    service.inject_pane_process_identity_for_tests(&parent.pane_id, fault);
                }
            }
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
                "{mode}"
            );
            assert_eq!(
                service.agent_turn_executions()[&parent.turn_id].action_results[0].status,
                ActionStatus::Running,
                "{mode}"
            );
            assert!(
                !service.agent_provider_task_is_pending(&parent.turn_id),
                "{mode}"
            );
            assert_eq!(service.tracked_message_request_usage(), reserved, "{mode}");
            assert!(
                !service.agent_turn_contexts()[&parent.turn_id]
                    .blocks()
                    .iter()
                    .any(|block| block.label.starts_with("request-handoff:")),
                "{mode}"
            );
            assert_eq!(service.message_service().next_message_sequence(), sequence);
        }
        if mode == "new-root" {
            service
                .execute_agent_shell_command(&primary, "/stop")
                .unwrap();
            service
                .execute_agent_shell_command(&primary, "/new")
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
            assert_ne!(
                service
                    .agent_shell_store()
                    .get(&parent.pane_id)
                    .unwrap()
                    .session_id,
                parent.conversation_id
            );
        } else {
            let actual = baseline.as_ref().unwrap();
            assert_eq!(
                mez_mux::process::process_start_token_for_pid(actual.process_id),
                Some(actual.start_token)
            );
            service
                .deliver_pending_runtime_agent_messages(current_unix_millis())
                .unwrap();
            let result: serde_json::Value = serde_json::from_str(
                service.agent_turn_executions()[&parent.turn_id].action_results[0]
                    .structured_content_json
                    .as_deref()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(result["state"], "peer_unavailable");
            assert_eq!(result["progress_witness"], false);
            assert_eq!(
                service.agent_turn_contexts()[&parent.turn_id]
                    .blocks()
                    .iter()
                    .filter(|block| block.label.starts_with("request-handoff:"))
                    .count(),
                1
            );
            claim_native_provider_continuation(&mut service, &parent);
            finish(&mut service, &parent);
        }
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        assert_eq!(service.agent_turn_ledger().turns().len(), 1);
        service.terminate_all_pane_processes().unwrap();
    }
}

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
    let (mut service, parent, peer, primary) = fixture_with_primary_provider_claims();
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
    claim_native_provider_continuation(&mut service, &parent);
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
    service.terminate_all_pane_processes().unwrap();
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
