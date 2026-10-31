//! Native tracking expiry independent of payload delivery and replay retention.
//!
//! Trusted envelope TTL uses the actual admission transaction. Maintenance can
//! release stale unconsumed tracking without inventing progress or a timer wake;
//! committed source custody continues through the existing fair handoff gates.

use super::*;
use mez_agent::messaging::Delivery;

/// Accepts a real native tracked wire occurrence with an optional finite TTL.
/// MAAP supplies no deadline: this exercises the trusted adapter's lower wire
/// contract without installing a receipt, registration or task ownership flag.
fn accept_wire_request(
    service: &mut RuntimeSessionService,
    parent: &AgentTurnRecord,
    peer: &SenderIdentity,
    id: &str,
    ttl_ms: Option<u64>,
    now_ms: u64,
) -> Delivery {
    let sender = service.runtime_message_sender_identity(parent).unwrap();
    service
        .accept_tracked_message_envelope_for_tests(
            parent,
            Envelope {
                protocol: "mmp/1",
                id: id.into(),
                message_type: "send".into(),
                time: format!("fixture:{now_ms}"),
                sender,
                recipient: mez_agent::messaging::Recipient::Agent(peer.agent_id.clone()),
                correlation_id: None,
                ttl_ms,
                content_type: "text/plain; charset=utf-8".into(),
                payload: "real accepted request with bounded retention".into(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap()
}

/// ACK releases the lower payload, but not its original request reservation.
/// Strict TTL expiry followed by the normal retained handoff window must retire
/// an unanswered finite request. An otherwise identical no-TTL request remains
/// valid, expiry grants no provider wake, and selecting the retired ID returns
/// bounded correction rather than recreating authority from ACK/history.
#[test]
fn request_bound_wire_ttl_retires_tracking_after_ack_and_handoff_window() {
    for selected in [false, true] {
        let (mut service, parent, peer) = fixture();
        let accepted_at = current_unix_millis();
        let expiry = accepted_at + 60_000;
        let finite = accept_wire_request(
            &mut service,
            &parent,
            &peer,
            "finite-request",
            Some(60_000),
            accepted_at,
        );
        let indefinite = accept_wire_request(
            &mut service,
            &parent,
            &peer,
            "indefinite-request",
            None,
            accepted_at,
        );
        service
            .message_service_mut()
            .subscribe(&peer.agent_id)
            .unwrap();
        let offered = service
            .message_service_mut()
            .receive_subscribed(&peer.agent_id, expiry, 10)
            .unwrap();
        assert_eq!(offered.messages.len(), 2);
        service
            .message_service_mut()
            .advance_subscription(&peer.agent_id, indefinite.sequence)
            .unwrap();
        assert!(
            service
                .message_service()
                .live_acceptance_proof(finite.sequence)
                .is_none()
        );
        assert_eq!(service.tracked_message_request_usage().0.0, 2);
        if selected {
            wait(&mut service, &parent, &finite.message_id, None);
        } else {
            execute(
                &mut service,
                &parent,
                AgentAction {
                    id: "ordinary-wait-during-expiry".into(),
                    payload: AgentActionPayload::Wait {
                        for_message: None,
                        from_agent: None,
                    },
                },
            );
        }
        for now in [expiry, expiry + 1, expiry + 600_000] {
            let transition = service.apply_peer_message_delivery_timer(now, 1).unwrap();
            assert!(matches!(
                transition.side_effects.as_slice(),
                [RuntimeSideEffect::ScheduleTimer { .. }]
            ));
            assert_eq!(
                service.tracked_message_request_usage().1,
                usize::from(selected)
            );
            assert_eq!(service.tracked_message_request_usage().0.0, 2);
            assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
        }
        let transition = service
            .apply_peer_message_delivery_timer(expiry + 600_001, 2)
            .unwrap();
        assert!(transition.side_effects.is_empty());
        assert!(!service.has_pending_peer_messages(expiry + 600_001));
        assert_eq!(service.tracked_message_request_usage().1, 0);
        assert_eq!(
            service.tracked_message_request_usage().0.0,
            1,
            "original TTL plus handoff window failed to release tracking after actual payload ACK"
        );
        assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Blocked
        );
        reply(
            &mut service,
            &parent,
            &peer,
            "ordinary-mail-after-expiry",
            None,
        );
        wait(&mut service, &parent, &finite.message_id, None);
        assert!(
            service.agent_turn_executions()[&parent.turn_id].action_results[0]
                .content_text()
                .contains("request wait unavailable")
        );
        assert_eq!(service.tracked_message_request_usage().0.0, 1);
        wait(&mut service, &parent, &indefinite.message_id, None);
        service
            .retire_runtime_message_identity_after_native_commits(&peer.agent_id)
            .unwrap();
        service
            .deliver_pending_runtime_agent_messages(current_unix_millis())
            .unwrap();
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        finish(&mut service, &parent);
    }
}

/// A real committed matching response and typed handoff may await scheduler
/// capacity beyond original request expiry. Both protected source occurrences
/// and their original resource charge must survive the expiry window, then be
/// consumed exactly once when normal fair admission becomes available.
#[test]
fn request_bound_wire_expiry_preserves_committed_queue_full_handoff_sources() {
    let (mut service, parent, peer) = fixture();
    let now = current_unix_millis();
    let sent = accept_wire_request(
        &mut service,
        &parent,
        &peer,
        "queued-finite",
        Some(60_000),
        now,
    );
    wait(&mut service, &parent, &sent.message_id, None);
    service
        .configure_agent_scheduler_queue_limits(1, usize::MAX)
        .unwrap();
    service
        .agent_scheduler_mut()
        .enqueue(ScheduledWork {
            turn_id: "expiry-queue-holder".into(),
            conversation_id: "expiry-queue-holder".into(),
            agent_id: "agent-expiry-queue-holder".into(),
            pane_id: None,
            kind: mez_agent::ScheduledWorkKind::BackgroundTask,
        })
        .unwrap();
    reply(
        &mut service,
        &parent,
        &peer,
        "queued-expiry-answer",
        Some(&sent.message_id),
    );
    let reserved = service.tracked_message_request_usage();
    let context = &service.agent_turn_contexts()[&parent.turn_id];
    let pins: Vec<_> = context
        .chronology()
        .iter()
        .filter(|event| context.peer_message_is_pending(event.sequence()))
        .map(|event| event.sequence())
        .collect();
    assert_eq!(pins.len(), 2);
    for tick in [now + 60_001, now + 660_001] {
        service
            .deliver_pending_runtime_agent_messages(tick)
            .unwrap();
        assert_eq!(service.tracked_message_request_usage(), reserved);
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Blocked
        );
        assert!(pins.iter().all(|pin| {
            service.agent_turn_contexts()[&parent.turn_id].peer_message_is_pending(*pin)
        }));
    }
    service
        .agent_scheduler_mut()
        .cancel("expiry-queue-holder")
        .unwrap();
    service
        .deliver_pending_runtime_agent_messages(current_unix_millis())
        .unwrap();
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    assert!(
        pins.iter().all(
            |pin| !service.agent_turn_contexts()[&parent.turn_id].peer_message_is_pending(*pin)
        )
    );
    finish(&mut service, &parent);
}

/// Expiry cleanup cannot overtake an authentic original reverse acceptance
/// that still awaits canonical delivery at the end of the retained window.
/// Pending custody only defers retirement; the actual delivery sweep must
/// commit the reply and exact handoff before any progress or wait consumption.
#[test]
fn request_bound_wire_expiry_defers_to_pending_original_reply() {
    let (mut service, parent, peer) = fixture();
    let now = current_unix_millis();
    let sent = accept_wire_request(
        &mut service,
        &parent,
        &peer,
        "pending-finite",
        Some(60_000),
        now,
    );
    wait(&mut service, &parent, &sent.message_id, None);
    service
        .deliver_pending_runtime_agent_messages(now + 60_001)
        .unwrap();
    accept_reply(
        &mut service,
        &parent,
        &peer,
        "pending-expiry-answer",
        Some(&sent.message_id),
    );
    assert_eq!(service.tracked_message_request_usage().0.0, 1);
    assert!(
        !service.agent_turn_contexts()[&parent.turn_id]
            .blocks()
            .iter()
            .any(|block| block.label.starts_with("request-handoff:"))
    );
    service
        .deliver_pending_runtime_agent_messages(now + 660_001)
        .unwrap();
    let outcome: serde_json::Value = serde_json::from_str(
        service.agent_turn_executions()[&parent.turn_id].action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(outcome["state"], "reply");
    assert_eq!(outcome["progress_witness"], true);
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    finish(&mut service, &parent);
}

/// Genuine terminal evidence cached behind human inhibition is still outcome
/// custody before any canonical request handoff exists. Original request TTL
/// and its elapsed window cannot erase that evidence or bypass Pause. Explicit
/// primary Resume must consume the original task result once, then release all
/// reservations through the ordinary same-parent continuation.
#[test]
fn request_bound_wire_expiry_preserves_cached_terminal_behind_human_pause() {
    let (mut service, parent, _, primary) = fixture_with_primary();
    let child = spawn_child(&mut service, &parent);
    assert!(
        service
            .validate_native_agent_surface_startup(&child.pane_id)
            .unwrap()
    );
    let recipient = service.runtime_message_sender_identity(&child).unwrap();
    let now = current_unix_millis();
    let sent = accept_wire_request(
        &mut service,
        &parent,
        &recipient,
        "paused-finite",
        Some(60_000),
        now,
    );
    wait(&mut service, &parent, &sent.message_id, None);
    let target = service
        .capture_agent_lifecycle_target(&primary, "%1")
        .unwrap();
    let generation = service
        .pause_agent_lifecycle_target(&primary, &target)
        .unwrap();
    service
        .complete_running_agent_turn_and_start_ready(
            &child,
            AgentTurnState::Completed,
            "TTL must preserve this exact paused terminal outcome",
        )
        .unwrap();
    let reserved = service.tracked_message_request_usage();
    for tick in [now + 60_001, now + 660_001] {
        service
            .deliver_pending_runtime_agent_messages(tick)
            .unwrap();
        assert_eq!(service.tracked_message_request_usage(), reserved);
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Blocked
        );
        assert!(!service.agent_provider_task_is_pending(&parent.turn_id));
        assert!(
            !service.agent_turn_contexts()[&parent.turn_id]
                .blocks()
                .iter()
                .any(|block| block.label.starts_with("request-handoff:"))
        );
    }
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
    execute(
        &mut service,
        &parent,
        AgentAction {
            id: "close-paused-finite-child".into(),
            payload: AgentActionPayload::CloseAgent {
                agent_id: child.agent_id.clone(),
            },
        },
    );
    finish(&mut service, &parent);
}
