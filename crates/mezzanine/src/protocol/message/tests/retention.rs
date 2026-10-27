//! Messaging retention tests.

use super::*;

/// Verifies expired messages are not delivered.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn expired_messages_are_not_delivered() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let mut message = envelope(sender.clone());
    message.recipient = Recipient::Agent(sender.agent_id.clone());
    message.ttl_ms = Some(5);

    service
        .accept_at_with_scope(&sender.agent_id, message, MessageScope::Session, 10)
        .unwrap();

    assert!(service.receive_for(&sender.agent_id, 16).is_empty());
}

/// Verifies that messages with an immediate TTL do not enter the delivery
/// queue. This covers the protocol case where the sender can be told that a
/// message expired before delivery instead of receiving an accepted ack for a
/// payload that can never be delivered.
#[test]
fn zero_ttl_messages_are_rejected_before_delivery() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    let mut message = envelope(sender.clone());
    message.recipient = Recipient::Agent(target.agent_id.clone());
    message.ttl_ms = Some(0);

    let error = service
        .accept_at_with_scope(&sender.agent_id, message, MessageScope::Session, 10)
        .unwrap_err();

    assert_eq!(error.kind(), MessageErrorKind::InvalidState);
    assert_eq!(mmp_error_code(&error), "expired");
    assert!(service.receive_for(&target.agent_id, 10).is_empty());
}

/// Verifies queue retention evicts oldest messages.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn queue_retention_evicts_oldest_messages() {
    let mut service = MessageService::with_limits(1, 1024);
    let sender = service.register_agent(None, None, "default", Vec::new());
    let mut first = envelope(sender.clone());
    first.id = "m1".to_string();
    let mut second = envelope(sender.clone());
    second.id = "m2".to_string();

    service
        .accept_at_with_scope(&sender.agent_id, first, MessageScope::Session, 0)
        .unwrap();
    service
        .accept_at_with_scope(&sender.agent_id, second, MessageScope::Session, 0)
        .unwrap();

    let received = service.receive_for(&sender.agent_id, 0);
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].id, "m2");
}

/// Accepted-message deduplication follows canonical pending custody as well as
/// optional history. Evicting unread replay cannot permit a conflicting retry;
/// a real recipient ACK releases custody without replaying the sender's effect.
#[test]
fn accepted_message_retention_tracks_retained_queue() {
    let mut service = MessageService::with_limits(1, 1024);
    let sender = service.register_agent(None, None, "default", Vec::new());
    service.subscribe(&sender.agent_id).unwrap();
    let mut first = envelope(sender.clone());
    first.id = "m1".to_string();
    let mut second = envelope(sender.clone());
    second.id = "m2".to_string();

    service
        .accept_at_with_scope(&sender.agent_id, first, MessageScope::Session, 0)
        .unwrap();
    service
        .accept_at_with_scope(&sender.agent_id, second, MessageScope::Session, 0)
        .unwrap();
    let snapshot = service.snapshot_state();

    assert_eq!(snapshot.retained_messages.len(), 1);
    assert_eq!(snapshot.accepted_messages.len(), 2);
    assert_eq!(snapshot.custody_messages.as_ref().unwrap().len(), 1);
    let selected = service.receive_subscribed(&sender.agent_id, 1, 10).unwrap();
    assert_eq!(
        selected
            .messages
            .iter()
            .map(|row| row.envelope.id.as_str())
            .collect::<Vec<_>>(),
        ["m1", "m2"]
    );
    service
        .advance_subscription(&sender.agent_id, selected.messages[0].sequence)
        .unwrap();
    let committed = service.snapshot_state();
    assert!(committed.custody_messages.as_ref().unwrap().is_empty());
    assert_eq!(committed.accepted_messages.len(), 1);
    assert_eq!(committed.accepted_messages[0].envelope.id, "m2");
}
