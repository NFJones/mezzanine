//! Peer-loop episode suppression and direct-user reset coverage.
//!
//! Pending mail remains behind its cursor while a limit blocks admission;
//! only the documented user reset re-arms message-triggered work.

use super::*;

/// Builds one pending model-originated peer message for the loop-limit tests.
fn limited_peer_message(service: &mut crate::runtime::RuntimeSessionService, now_ms: u64) {
    let recipient_identity = service
        .ensure_runtime_message_identity(
            "agent-%1",
            PaneId::opaque("%1".to_string()),
            "agent",
            &[],
            now_ms,
        )
        .unwrap();
    service
        .control
        .message_service_mut()
        .subscribe_from_retained_start(&recipient_identity.agent_id)
        .unwrap();
    let sender = service
        .ensure_runtime_message_identity("agent-sender", None, "agent", &[], now_ms)
        .unwrap();
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(
            &sender.agent_id,
            Envelope {
                protocol: "mmp/1",
                id: "loop-limit-episode-message".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(
                    recipient_identity.agent_id.clone(),
                ),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "loop-limit episode evidence".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
}

/// Verifies the peer-message loop limit is a stable episode: no turn starts, the
/// delivery timer stops re-arming, and the limit diagnostic is emitted once
/// rather than once per tick.
#[test]
fn runtime_peer_message_loop_limit_is_a_stable_episode() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_peer_message_loop_limit(1);
    service.set_agent_peer_message_turn_count("agent-%1", 1);
    let now_ms = current_unix_seconds().saturating_mul(1000);
    limited_peer_message(&mut service, now_ms);
    for _ in 0..2 {
        assert_eq!(
            service
                .deliver_pending_runtime_agent_messages(now_ms)
                .unwrap(),
            0
        );
    }
    assert!(service.agent_turn_ledger().turns().is_empty());
    assert!(service.agent_peer_message_limit_reported("agent-%1"));
    assert!(
        service
            .peer_message_delivery_timer_transition(false, 11, now_ms)
            .side_effects
            .is_empty(),
        "limit-blocked mail must not keep re-arming the delivery timer"
    );
    let diagnostics = service
        .event_log()
        .expect("event log")
        .replay_for(&crate::protocol::event::EventAudience::AllPrimaries)
        .into_iter()
        .filter(|event| event.payload.contains("peer_message_loop_limit"))
        .count();
    assert_eq!(
        diagnostics, 1,
        "the limit diagnostic must be emitted once per episode"
    );
}

/// Verifies peer mail flows again after the defined reset trigger: direct user
/// input clears the counter and the limit episode, so delivery re-arms and a
/// message-triggered turn starts.
#[test]
fn runtime_peer_mail_flows_again_after_direct_user_input_reset() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_peer_message_loop_limit(1);
    service.set_agent_peer_message_turn_count("agent-%1", 1);
    let now_ms = current_unix_seconds().saturating_mul(1000);
    limited_peer_message(&mut service, now_ms);
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0
    );
    assert!(
        service
            .peer_message_delivery_timer_transition(false, 12, now_ms)
            .side_effects
            .is_empty()
    );
    service.reset_agent_peer_message_turns("agent-%1");
    assert!(!service.agent_peer_message_limit_reported("agent-%1"));
    assert_eq!(
        service
            .peer_message_delivery_timer_transition(false, 12, now_ms)
            .side_effects
            .len(),
        1,
        "pending peer mail must re-arm delivery once the limit episode ends"
    );
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        1
    );
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.trigger == mez_agent::AgentTurnTrigger::LocalMessage),
        "peer mail must start a message-triggered turn after the reset"
    );
}
