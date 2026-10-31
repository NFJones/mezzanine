//! Request outcome recovery across actual bridge loss and context unavailability.
//!
//! Message capacity is exhausted through genuine accepted obligations before
//! terminal bridge acceptance. Context faults move the exact original owner
//! aside and restore it unchanged; no result, receipt, ACK or readiness is forged.

use super::*;

/// Fills the real lower occurrence budget with two independent unread sends.
/// A third actual admission must fail before sequence allocation. The unrelated
/// sink has no native pane, so delivery cannot accidentally drain this pressure.
fn fill_bridge_capacity(
    service: &mut RuntimeSessionService,
    sender: &SenderIdentity,
) -> SenderIdentity {
    let now = current_unix_millis();
    let sink = service
        .ensure_runtime_message_identity("agent-bridge-capacity-sink", None, "worker", &[], now)
        .unwrap();
    let mut envelope = Envelope {
        protocol: "mmp/1",
        id: "bridge-capacity-0".into(),
        message_type: "send".into(),
        time: format!("fixture:{now}"),
        sender: sender.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(sink.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".into(),
        payload: "unrelated accepted custody must remain unread".into(),
        extension_fields: Vec::new(),
    };
    for index in 0..2 {
        envelope.id = format!("bridge-capacity-{index}");
        service
            .message_service_mut()
            .accept_at_with_scope(
                &sender.agent_id,
                envelope.clone(),
                MessageScope::Session,
                now,
            )
            .unwrap();
    }
    let sequence = service.message_service().next_message_sequence();
    envelope.id = "bridge-capacity-rejected-probe".into();
    let error = service
        .message_service_mut()
        .accept_at_with_scope(&sender.agent_id, envelope, MessageScope::Session, now)
        .unwrap_err();
    assert_eq!(
        error.kind(),
        mez_agent::messaging::MessageErrorKind::Capacity
    );
    assert_eq!(service.message_service().next_message_sequence(), sequence);
    sink
}

/// Releases only the sink's genuinely offered occurrences after qualification.
/// This cleanup cannot produce native context or settle the parent's request.
fn drain_bridge_capacity(service: &mut RuntimeSessionService, sink: &SenderIdentity) {
    service
        .message_service_mut()
        .subscribe(&sink.agent_id)
        .unwrap();
    let batch = service
        .message_service_mut()
        .receive_subscribed(&sink.agent_id, current_unix_millis(), 10)
        .unwrap();
    assert_eq!(batch.messages.len(), 2);
    let last = batch.messages.last().unwrap().sequence;
    service
        .message_service_mut()
        .advance_subscription(&sink.agent_id, last)
        .unwrap();
}

/// Runs real final output through native provider execution and lifecycle owners.
fn complete_bridge_child(
    service: &mut RuntimeSessionService,
    child: &AgentTurnRecord,
) -> mez_agent::AgentTurnExecution {
    execute(
        service,
        child,
        AgentAction {
            id: "bridge-lost-result".into(),
            payload: AgentActionPayload::Say {
                status: SayStatus::Final,
                text: "EXACT_OUTPUT_WITHOUT_TERMINAL_BRIDGE".into(),
                content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
            },
        },
    )
}

/// True request intent retains the exact native result when actual lower
/// capacity rejects its terminal bridge, before or after selected parking.
/// False intent's ordinary Wait remains parked: neither a missing bridge nor
/// real child success fabricates request ownership, progress or an inference.
#[test]
fn request_bound_bridge_capacity_loss_preserves_exact_result_without_plain_wake() {
    for before_park in [false, true] {
        for tracked in [false, true] {
            let (mut service, parent, peer, primary) = fixture_with_primary_pending_limits(Some(
                mez_agent::messaging::PendingDeliveryLimits {
                    messages: 2,
                    ..Default::default()
                },
            ));
            if before_park {
                service.configure_agent_scheduler_limit(2).unwrap();
            }
            let child = spawn_child(&mut service, &parent);
            assert!(
                service
                    .validate_native_agent_surface_startup(&child.pane_id)
                    .unwrap()
            );
            service.start_ready_agent_turns().unwrap();
            let message = send(&mut service, &parent, &child.agent_id, tracked);
            if !before_park {
                if tracked {
                    wait(&mut service, &parent, &message, None);
                } else {
                    execute(
                        &mut service,
                        &parent,
                        AgentAction {
                            id: "plain-bridge-loss-wait".into(),
                            payload: AgentActionPayload::Wait {
                                for_message: None,
                                from_agent: None,
                            },
                        },
                    );
                }
            }
            service
                .deliver_pending_runtime_agent_messages(current_unix_millis())
                .unwrap();
            let sink = fill_bridge_capacity(&mut service, &peer);
            let sequence = service.message_service().next_message_sequence();
            let completed = complete_bridge_child(&mut service, &child);
            assert_eq!(
                service
                    .agent_turn_ledger()
                    .turn(&child.turn_id)
                    .unwrap()
                    .state,
                AgentTurnState::Completed
            );
            assert_eq!(
                service.message_service().next_message_sequence(),
                sequence,
                "terminal bridge unexpectedly acquired delivery custody"
            );
            assert!(service.subagent_task_parent(&child.turn_id).is_none());
            if before_park {
                if tracked {
                    wait(&mut service, &parent, &message, None);
                } else {
                    execute(
                        &mut service,
                        &parent,
                        AgentAction {
                            id: "plain-after-lost-bridge".into(),
                            payload: AgentActionPayload::Wait {
                                for_message: None,
                                from_agent: None,
                            },
                        },
                    );
                }
            }
            service
                .emit_subagent_task_result_for_execution(&child, &completed)
                .unwrap();
            for tick in 1..=3 {
                service
                    .apply_peer_message_delivery_timer(current_unix_millis(), tick)
                    .unwrap();
            }
            assert_eq!(service.message_service().next_message_sequence(), sequence);
            assert_eq!(service.agent_turn_ledger().turns().len(), 2);
            let child_id = AgentId::opaque(child.agent_id.clone()).unwrap();
            assert_eq!(
                service
                    .message_service()
                    .receive_for(&child_id, u64::MAX)
                    .iter()
                    .filter(|mail| mail.message_type == "send"
                        && mail.sender.agent_id.as_str() == parent.agent_id
                        && mail.payload == "please answer this request")
                    .count(),
                1,
                "original parent request was resent instead of using native result custody"
            );
            let sources: Vec<_> = service.agent_turn_contexts()[&parent.turn_id]
                .blocks()
                .iter()
                .filter(|block| block.label.starts_with("request-handoff:"))
                .collect();
            if tracked {
                assert_eq!(sources.len(), 1);
                assert!(
                    sources[0]
                        .content
                        .contains("EXACT_OUTPUT_WITHOUT_TERMINAL_BRIDGE")
                );
                assert!(sources[0].content.contains(&child.turn_id));
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
                assert_eq!(
                    service
                        .agent_turn_ledger()
                        .turn(&parent.turn_id)
                        .unwrap()
                        .state,
                    AgentTurnState::Running
                );
                execute(
                    &mut service,
                    &parent,
                    AgentAction {
                        id: "close-bridge-child".into(),
                        payload: AgentActionPayload::CloseAgent {
                            agent_id: child.agent_id.clone(),
                        },
                    },
                );
                finish(&mut service, &parent);
            } else {
                assert!(sources.is_empty());
                assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
                assert_eq!(
                    service
                        .agent_turn_ledger()
                        .turn(&parent.turn_id)
                        .unwrap()
                        .state,
                    AgentTurnState::Blocked
                );
                assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
                service
                    .execute_agent_shell_command(&primary, "/stop")
                    .unwrap();
                service
                    .dispatch_runtime_pane_close(
                        &primary,
                        &serde_json::json!({"pane_id":child.pane_id,"force":true}).to_string(),
                    )
                    .unwrap();
            }
            drain_bridge_capacity(&mut service, &sink);
        }
    }
}

/// A real terminal result must remain in its charged request owner while the
/// exact parent context is unavailable and the normal terminal bridge also
/// fails capacity. Recovery restores the SAME original context, then normal
/// timer/selection handoff supplies one exact result without resend or new task.
#[test]
fn request_bound_bridge_loss_and_unavailable_context_recover_original_custody() {
    for before_park in [false, true] {
        let (mut service, parent, peer, _) = fixture_with_primary_pending_limits(Some(
            mez_agent::messaging::PendingDeliveryLimits {
                messages: 2,
                ..Default::default()
            },
        ));
        if before_park {
            service.configure_agent_scheduler_limit(2).unwrap();
        }
        let child = spawn_child(&mut service, &parent);
        assert!(
            service
                .validate_native_agent_surface_startup(&child.pane_id)
                .unwrap()
        );
        service.start_ready_agent_turns().unwrap();
        let message = send(&mut service, &parent, &child.agent_id, true);
        if !before_park {
            wait(&mut service, &parent, &message, None);
        }
        service
            .deliver_pending_runtime_agent_messages(current_unix_millis())
            .unwrap();
        let sink = fill_bridge_capacity(&mut service, &peer);
        let original = service
            .agent_turn_contexts_mut()
            .remove(&parent.turn_id)
            .unwrap();
        let reserved = service.tracked_message_request_usage();
        let sequence = service.message_service().next_message_sequence();
        let completed = complete_bridge_child(&mut service, &child);
        if !before_park {
            assert!(
                !service
                    .peer_message_delivery_timer_transition(false, 99, current_unix_millis())
                    .side_effects
                    .is_empty(),
                "retained source must arm maintenance despite lost bridge and unavailable context"
            );
        }
        assert_eq!(service.message_service().next_message_sequence(), sequence);
        for tick in 1..=3 {
            service
                .apply_peer_message_delivery_timer(current_unix_millis(), tick)
                .unwrap();
            assert_eq!(service.tracked_message_request_usage(), reserved);
            assert!(!service.agent_turn_contexts().contains_key(&parent.turn_id));
            if !before_park {
                assert_eq!(
                    service
                        .agent_turn_ledger()
                        .turn(&parent.turn_id)
                        .unwrap()
                        .state,
                    AgentTurnState::Blocked
                );
                assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
            }
        }
        service
            .agent_turn_contexts_mut()
            .insert(parent.turn_id.clone(), original);
        if before_park {
            wait(&mut service, &parent, &message, None);
        }
        service
            .apply_peer_message_delivery_timer(current_unix_millis(), 4)
            .unwrap();
        service
            .emit_subagent_task_result_for_execution(&child, &completed)
            .unwrap();
        service
            .apply_peer_message_delivery_timer(current_unix_millis(), 5)
            .unwrap();
        assert_eq!(service.message_service().next_message_sequence(), sequence);
        let context = &service.agent_turn_contexts()[&parent.turn_id];
        let sources: Vec<_> = context
            .blocks()
            .iter()
            .filter(|b| b.label.starts_with("request-handoff:"))
            .collect();
        assert_eq!(sources.len(), 1);
        let outcome: serde_json::Value = serde_json::from_str(
            service.agent_turn_executions()[&parent.turn_id].action_results[0]
                .structured_content_json
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(outcome["state"], "peer_terminal");
        assert_eq!(outcome["progress_witness"], true);
        assert_eq!(service.agent_turn_ledger().turns().len(), 2);
        assert!(
            sources[0]
                .content
                .contains("EXACT_OUTPUT_WITHOUT_TERMINAL_BRIDGE")
        );
        assert!(sources[0].content.contains(&child.turn_id));
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        execute(
            &mut service,
            &parent,
            AgentAction {
                id: "close-context-fault-child".into(),
                payload: AgentActionPayload::CloseAgent {
                    agent_id: child.agent_id.clone(),
                },
            },
        );
        finish(&mut service, &parent);
        drain_bridge_capacity(&mut service, &sink);
    }
}
