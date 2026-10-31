//! Authenticated replies, frozen recipient selection and ordinary-mail reparking.
//! Native accepted occurrences establish authority; correlation text alone does
//! not satisfy a request, and delivery TTL uses one trusted native snapshot.

use super::*;

/// A tracked request wakes on a genuinely correlated response both before and
/// after park. The same original parent consumes one outcome, releases every
/// reservation, and completes without resending or creating another parent task.
#[test]
fn request_bound_matching_reply_before_and_after_park_completes_same_turn() {
    for before_park in [false, true] {
        let (mut service, parent, peer) = fixture();
        let message = send(&mut service, &parent, peer.agent_id.as_str(), true);
        if before_park {
            reply(&mut service, &parent, &peer, "early-reply", Some(&message));
        }
        wait(&mut service, &parent, &message, None);
        if !before_park {
            assert_eq!(
                service
                    .agent_turn_ledger()
                    .turn(&parent.turn_id)
                    .unwrap()
                    .state,
                AgentTurnState::Blocked
            );
            reply(&mut service, &parent, &peer, "late-reply", Some(&message));
        }
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Running
        );
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        let wait = &service.agent_turn_executions()[&parent.turn_id].action_results[0];
        assert!(
            wait.structured_content_json
                .as_ref()
                .unwrap()
                .contains("\"state\":\"reply\"")
        );
        finish(&mut service, &parent);
    }
}

/// Unrelated mail and a foreign sender's exact correlation may resume inference
/// but cannot consume the selected request. Reparking reuses the same generation
/// and resource charge until the original authenticated recipient replies.
#[test]
fn request_bound_irrelevant_mail_reparks_same_edge_without_progress() {
    let (mut service, parent, peer) = fixture();
    let message = send(&mut service, &parent, peer.agent_id.as_str(), true);
    wait(&mut service, &parent, &message, None);
    let usage = service.tracked_message_request_usage();
    let stranger = service
        .ensure_runtime_message_identity(
            "agent-request-stranger",
            None,
            "peer",
            &[],
            current_unix_millis(),
        )
        .unwrap();
    reply(
        &mut service,
        &parent,
        &stranger,
        "foreign-correlation",
        Some(&message),
    );
    assert_eq!(service.tracked_message_request_usage(), usage);
    wait(&mut service, &parent, &message, None);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Blocked
    );
    assert_eq!(service.tracked_message_request_usage(), usage);
    reply(
        &mut service,
        &parent,
        &peer,
        "right-correlation",
        Some(&message),
    );
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    finish(&mut service, &parent);
}

/// Actual registration retirement, unlike mere offline presence, supplies only
/// opaque unavailability and resumes the same task to choose an authorized
/// alternative. Both pre-park and post-park retirement retain the outcome.
#[test]
fn request_bound_nonowned_retirement_before_and_after_park_allows_alternative() {
    for before_park in [false, true] {
        let (mut service, parent, peer) = fixture();
        let message = send(&mut service, &parent, peer.agent_id.as_str(), true);
        if before_park {
            service
                .control
                .message_service_mut()
                .retire_observational_identity(&peer.agent_id);
        }
        wait(&mut service, &parent, &message, None);
        if !before_park {
            service
                .control
                .message_service_mut()
                .update_presence(
                    &peer.agent_id,
                    mez_agent::messaging::AgentPresenceStatus::Offline,
                    current_unix_millis(),
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
                AgentTurnState::Blocked
            );
            service
                .control
                .message_service_mut()
                .retire_observational_identity(&peer.agent_id);
            service
                .deliver_pending_runtime_agent_messages(current_unix_millis())
                .unwrap();
        }
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Running
        );
        let result = &service.agent_turn_executions()[&parent.turn_id].action_results[0];
        let result: serde_json::Value =
            serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
        assert_eq!(result["state"], "peer_unavailable");
        assert_eq!(result["progress_witness"], false);
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        finish(&mut service, &parent);
    }
}

/// Untracked and nonexistent IDs remain bounded same-turn correction rather
/// than creating lifecycle authority, parking forever, or terminating the task.
#[test]
fn request_bound_invalid_or_untracked_selection_is_correctable_without_edge() {
    let (mut service, parent, peer) = fixture();
    let message = send(&mut service, &parent, peer.agent_id.as_str(), false);
    for selected in [message.as_str(), "invented-message-id"] {
        wait(&mut service, &parent, selected, None);
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&parent.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Running
        );
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        assert!(
            service.agent_turn_executions()[&parent.turn_id].action_results[0]
                .content_text()
                .contains("request wait unavailable")
        );
    }
    finish(&mut service, &parent);
}

/// A fanout requires one original non-self recipient before allocating an edge.
/// Correcting the selector resumes the same conversation and consumes only the
/// authenticated selected response; an unselected peer cannot stand in for it.
#[test]
fn request_bound_fanout_requires_exact_selection_and_self_is_rejected() {
    let (mut service, parent, peer) = fixture();
    service
        .ensure_runtime_message_identity(
            "agent-second-worker",
            None,
            "worker",
            &[],
            current_unix_millis(),
        )
        .unwrap();
    let execution = execute(
        &mut service,
        &parent,
        AgentAction {
            id: "fanout".into(),
            payload: AgentActionPayload::SendMessage {
                expect_reply: true,
                recipient: "role:worker".into(),
                scope: Some("session".into()),
                content_type: "text/plain; charset=utf-8".into(),
                payload: "one selected review".into(),
                correlation_id: None,
            },
        },
    );
    let accepted: serde_json::Value = serde_json::from_str(
        execution.action_results[0]
            .structured_content_json
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    let message = accepted["message_id"].as_str().unwrap();
    assert_eq!(accepted["queued_recipients"], 2);
    wait(&mut service, &parent, message, None);
    assert_eq!(service.tracked_message_request_usage().1, 0);
    wait(&mut service, &parent, message, Some(peer.agent_id.as_str()));
    assert_eq!(service.tracked_message_request_usage().1, 1);
    reply(&mut service, &parent, &peer, "fanout-reply", Some(message));
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    let own_message = send(&mut service, &parent, &parent.agent_id, true);
    wait(&mut service, &parent, &own_message, None);
    assert_eq!(service.tracked_message_request_usage().1, 0);
    assert_eq!(
        service
            .agent_turn_ledger()
            .turn(&parent.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
    service
        .retire_runtime_message_transfers_for_restore()
        .unwrap();
    assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
    finish(&mut service, &parent);
}

/// A reverse response accepted by the original recipient before retirement
/// remains authentic while awaiting canonical commit. Availability recovery
/// must not consume the receipt ahead of that real reply, both before parking
/// and after an actual targeted wait; IDs and correlation alone grant no proof.
#[test]
fn request_bound_reply_accepted_before_sender_retirement_reaches_selected_wait() {
    for before_park in [false, true] {
        let (mut service, parent, peer) = fixture();
        let message = send(&mut service, &parent, peer.agent_id.as_str(), true);
        if !before_park {
            wait(&mut service, &parent, &message, None);
        }
        let reply = accept_reply(
            &mut service,
            &parent,
            &peer,
            "original-interval-reply",
            Some(&message),
        );
        assert!(
            service
                .retire_runtime_message_identity_after_native_commits(&peer.agent_id)
                .unwrap()
        );
        assert!(
            service
                .message_service()
                .live_acceptance_proof(reply)
                .is_some()
        );
        if before_park {
            wait(&mut service, &parent, &message, None);
        }
        service
            .deliver_pending_runtime_agent_messages(current_unix_millis())
            .unwrap();
        let result = &service.agent_turn_executions()[&parent.turn_id].action_results[0];
        let outcome: serde_json::Value =
            serde_json::from_str(result.structured_content_json.as_deref().unwrap()).unwrap();
        assert_eq!(
            outcome["state"], "reply",
            "registration retirement overtook an authentic accepted reverse reply"
        );
        assert_eq!(outcome["progress_witness"], true);
        let context = &service.agent_turn_contexts()[&parent.turn_id];
        assert_eq!(
            context
                .blocks()
                .iter()
                .filter(|b| b.label.starts_with("request-handoff:")
                    && b.content
                        .contains("Authenticated matching response committed"))
                .count(),
            1
        );
        assert!(
            context
                .blocks()
                .iter()
                .any(|b| b.content.contains("original-interval-reply"))
        );
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        finish(&mut service, &parent);
    }
}

/// Receive selection and retirement deferral must use the same trusted delivery
/// clock snapshot. A response still live at that snapshot's inclusive TTL edge
/// cannot be discarded because a later wall-clock read classifies it expired.
/// Only actual canonical commit qualifies the original registration's reply.
#[test]
fn request_bound_reply_retirement_uses_delivery_snapshot_for_ttl() {
    for (now_ms, expected_state, progress, terminal_first) in [
        (2, "reply", true, false),
        (3, "peer_unavailable", false, false),
        (2, "peer_unavailable", false, true),
    ] {
        let (mut service, parent, peer) = fixture();
        let message = send(&mut service, &parent, peer.agent_id.as_str(), true);
        wait(&mut service, &parent, &message, None);
        service
            .message_service_mut()
            .accept_at_with_scope(
                &peer.agent_id,
                Envelope {
                    protocol: "mmp/1",
                    id: "delivery-clock-reply".into(),
                    message_type: "send".into(),
                    time: "fixture:1".into(),
                    sender: peer.clone(),
                    recipient: mez_agent::messaging::Recipient::Agent(
                        AgentId::opaque(parent.agent_id.clone()).unwrap(),
                    ),
                    correlation_id: Some(message),
                    ttl_ms: Some(1),
                    content_type: "text/plain".into(),
                    payload: "reply live at selected delivery clock".into(),
                    extension_fields: Vec::new(),
                },
                MessageScope::Session,
                1,
            )
            .unwrap();
        assert!(
            service
                .retire_runtime_message_identity_after_native_commits(&peer.agent_id)
                .unwrap()
        );
        if terminal_first {
            service
                .message_service_mut()
                .settle_delivery_outcomes_at(3)
                .unwrap();
        }
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap();
        let outcome: serde_json::Value = serde_json::from_str(
            service.agent_turn_executions()[&parent.turn_id].action_results[0]
                .structured_content_json
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            outcome["state"], expected_state,
            "retirement recovery used a different clock than receive selection"
        );
        assert_eq!(outcome["progress_witness"], progress);
        assert_eq!(service.tracked_message_request_usage(), ((0, 0, 0), 0));
        finish(&mut service, &parent);
    }
}
