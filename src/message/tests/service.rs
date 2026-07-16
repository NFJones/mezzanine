//! Messaging service tests.

use super::*;

/// Verifies accepts message from registered sender.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn accepts_message_from_registered_sender() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());

    let delivery = service
        .accept(&sender.agent_id.clone(), envelope(sender))
        .unwrap();

    assert!(delivery.accepted);
    assert_eq!(delivery.message_id, "m1");
    assert_eq!(delivery.sequence, 1);
    assert_eq!(delivery.queued_recipients, 1);
    assert_eq!(delivery.status, DeliveryStatus::Accepted);
}

/// Verifies that the snapshot projection retains MMP identities, presence,
/// subscriptions, queued envelopes, and accepted-message idempotency state.
#[test]
fn message_service_snapshot_round_trips_delivery_state() {
    let mut service = MessageService::default();
    let sender = service.register_agent(
        Some(PaneId::opaque("%1").unwrap()),
        Some(WindowId::opaque("@1").unwrap()),
        "writer",
        vec!["code".to_string()],
    );
    let target = service.register_agent(None, None, "reviewer", vec!["review".to_string()]);
    service
        .update_presence(&target.agent_id, AgentPresenceStatus::Busy, 5)
        .unwrap();
    service.subscribe(&target.agent_id).unwrap();
    let mut message = envelope(sender.clone());
    message.id = "snapshot-message".to_string();
    message.recipient = Recipient::Agent(target.agent_id.clone());
    message.ttl_ms = Some(1_000);
    message.extension_fields = vec![("trace".to_string(), r#"{"span":"one"}"#.to_string())];

    let delivery = service
        .accept_at(&sender.agent_id, message.clone(), 10)
        .unwrap();
    let snapshot = service.snapshot_state();
    let mut restored = MessageService::from_snapshot_state(&snapshot).unwrap();

    assert_eq!(snapshot.protocol, "mmp/1");
    assert_eq!(snapshot.registered_agents.len(), 2);
    assert_eq!(snapshot.retained_messages.len(), 1);
    assert_eq!(snapshot.accepted_messages.len(), 1);
    assert_eq!(
        restored
            .presence()
            .into_iter()
            .find(|record| record.identity.agent_id == target.agent_id)
            .unwrap()
            .status,
        AgentPresenceStatus::Busy
    );
    let batch = restored
        .receive_subscribed(&target.agent_id, 20, 10)
        .unwrap();
    assert_eq!(batch.messages.len(), 1);
    assert_eq!(batch.messages[0].envelope.payload, "hello");
    assert_eq!(
        restored.accept_at(&sender.agent_id, message, 20).unwrap(),
        delivery
    );
}

/// Verifies that accepted message ids behave as idempotency keys at the service
/// layer. Retrying the exact same envelope returns the original delivery result
/// and does not enqueue a second copy for the recipient.
#[test]
fn duplicate_message_ids_are_idempotent_for_matching_envelopes() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    let mut message = envelope(sender.clone());
    message.recipient = Recipient::Agent(target.agent_id.clone());

    let first = service
        .accept_at(&sender.agent_id, message.clone(), 10)
        .unwrap();
    let second = service.accept_at(&sender.agent_id, message, 11).unwrap();
    let received = service.receive_for(&target.agent_id, 12);

    assert_eq!(second, first);
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].id, "m1");
}

/// Verifies that reusing a message id for different envelope content is
/// rejected instead of being delivered as a second logical message. This keeps
/// the session-global message identity invariant enforceable at the MMP
/// boundary.
#[test]
fn conflicting_duplicate_message_ids_are_rejected() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    let mut first = envelope(sender.clone());
    first.recipient = Recipient::Agent(target.agent_id.clone());
    let mut second = first.clone();
    second.payload = "different".to_string();

    service.accept_at(&sender.agent_id, first, 10).unwrap();
    let error = service.accept_at(&sender.agent_id, second, 11).unwrap_err();

    assert_eq!(error.kind(), MessageErrorKind::Conflict);
    assert_eq!(mmp_error_code(&error), "invalid_envelope");
    assert_eq!(service.receive_for(&target.agent_id, 12).len(), 1);
}

/// Verifies rejects sender spoofing.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_sender_spoofing() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let mut spoofed = sender.clone();
    spoofed.role = Some("other".to_string());

    let error = service
        .accept(&sender.agent_id, envelope(spoofed))
        .unwrap_err();

    assert_eq!(error.kind(), MessageErrorKind::Forbidden);
}
