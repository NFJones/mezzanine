//! Delivery expiry, timer admission, and peer-triggered loop-limit coverage.
//!
//! These tests distinguish accepted mail from committed provider work and keep
//! the durable recipient cursor authoritative across delivery lifecycle phases.

use super::*;

/// Expired mail starts no turn, creates no context, and stays behind the cursor.
#[test]
fn runtime_expired_peer_message_is_not_injected_or_acknowledged() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let now_ms = current_unix_seconds().saturating_mul(1000);
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
    let envelope = Envelope {
        protocol: "mmp/1",
        id: "expired-message-1".to_string(),
        message_type: "send".to_string(),
        time: format!("runtime:{now_ms}"),
        sender: sender.clone(),
        recipient: mez_agent::messaging::Recipient::Agent(recipient_identity.agent_id.clone()),
        correlation_id: None,
        ttl_ms: Some(1),
        content_type: "text/plain; charset=utf-8".to_string(),
        payload: "expired peer evidence".to_string(),
        extension_fields: Vec::new(),
    };
    service
        .control
        .message_service_mut()
        .accept_at_with_scope(&sender.agent_id, envelope, MessageScope::Session, now_ms)
        .unwrap();
    let after_expiry = now_ms.saturating_add(5_000);
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(after_expiry)
            .unwrap(),
        0
    );
    assert!(service.agent_turn_ledger().turns().is_empty());
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&recipient_identity.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
}

/// The delivery timer arms once, commits available mail, and stops after drain.
#[test]
fn runtime_peer_message_delivery_timer_arms_delivers_and_stops() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let now_ms = current_unix_seconds().saturating_mul(1000);
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
                id: "timed-message-1".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(
                    recipient_identity.agent_id.clone(),
                ),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "timer-driven peer evidence".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    let armed = service.peer_message_delivery_timer_transition(false, 7, now_ms);
    assert_eq!(armed.side_effects.len(), 1);
    match &armed.side_effects[0] {
        crate::runtime::RuntimeSideEffect::ScheduleTimer { key, delay_ms } => {
            assert_eq!(
                key.kind,
                crate::runtime::RuntimeTimerKind::PeerMessageDelivery
            );
            assert_eq!(key.generation, 7);
            assert!(*delay_ms > 0);
        }
        other => panic!("unexpected peer-message timer side effect: {other:?}"),
    }
    assert!(
        service
            .peer_message_delivery_timer_transition(true, 8, now_ms)
            .side_effects
            .is_empty(),
        "an active delivery timer must not be armed twice"
    );
    let applied = service
        .apply_peer_message_delivery_timer(now_ms, 7)
        .unwrap();
    assert!(applied.applied);
    assert!(
        applied.side_effects.is_empty(),
        "a drained inbox must not keep the delivery timer armed"
    );
    assert!(
        service
            .agent_turn_ledger()
            .turns()
            .iter()
            .any(|turn| turn.trigger == mez_agent::AgentTurnTrigger::LocalMessage),
        "timer delivery must start the idle agent's message-triggered turn"
    );
}

/// Loop limits refuse new message-triggered work without acknowledging its mail.
#[test]
fn runtime_peer_message_loop_limit_stops_message_turns() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_agent_peer_message_loop_limit(1);
    service.set_agent_peer_message_turn_count("agent-%1", 1);
    let now_ms = current_unix_seconds().saturating_mul(1000);
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
                id: "limited-message-1".to_string(),
                message_type: "send".to_string(),
                time: format!("runtime:{now_ms}"),
                sender: sender.clone(),
                recipient: mez_agent::messaging::Recipient::Agent(
                    recipient_identity.agent_id.clone(),
                ),
                correlation_id: None,
                ttl_ms: None,
                content_type: "text/plain; charset=utf-8".to_string(),
                payload: "loop-limited peer evidence".to_string(),
                extension_fields: Vec::new(),
            },
            MessageScope::Session,
            now_ms,
        )
        .unwrap();
    assert_eq!(
        service
            .deliver_pending_runtime_agent_messages(now_ms)
            .unwrap(),
        0
    );
    assert!(service.agent_turn_ledger().turns().is_empty());
    assert_eq!(
        service
            .control
            .message_service()
            .subscription(&recipient_identity.agent_id)
            .unwrap()
            .last_sequence,
        0,
        "loop-limited mail stays behind the durable cursor"
    );
}
