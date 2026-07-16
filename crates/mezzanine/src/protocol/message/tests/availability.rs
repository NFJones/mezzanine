//! Messaging availability tests.

use super::*;

/// Verifies undeliverable messages are rejected.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn undeliverable_messages_are_rejected() {
    let mut ids = IdFactory::default();
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let mut message = envelope(sender.clone());
    let _registered_id = ids.agent();
    message.recipient = Recipient::Agent(ids.agent());

    let error = service.accept(&sender.agent_id, message).unwrap_err();

    assert_eq!(error.kind(), MessageErrorKind::NotFound);
    assert_eq!(mmp_error_code(&error), "undeliverable");
}

/// Verifies that offline presence makes a registered recipient unavailable for
/// both acceptance-time matching and later receive-time delivery. Other
/// presence states can still be addressable, but `offline` must not receive new
/// local messages.
#[test]
fn offline_recipients_are_rejected_and_not_delivered() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    service
        .update_presence(&target.agent_id, AgentPresenceStatus::Offline, 10)
        .unwrap();
    let mut rejected = envelope(sender.clone());
    rejected.recipient = Recipient::Agent(target.agent_id.clone());

    let error = service
        .accept_at(&sender.agent_id, rejected, 11)
        .unwrap_err();

    assert_eq!(mmp_error_code(&error), "undeliverable");

    service
        .update_presence(&target.agent_id, AgentPresenceStatus::Available, 12)
        .unwrap();
    let mut accepted = envelope(sender.clone());
    accepted.id = "accepted-before-offline".to_string();
    accepted.recipient = Recipient::Agent(target.agent_id.clone());
    service.accept_at(&sender.agent_id, accepted, 13).unwrap();
    service
        .update_presence(&target.agent_id, AgentPresenceStatus::Offline, 14)
        .unwrap();

    let batch = service
        .receive_subscribed(&target.agent_id, 15, usize::MAX)
        .unwrap();

    assert!(batch.messages.is_empty());
}

/// Verifies responses can be filtered by correlation id.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn responses_can_be_filtered_by_correlation_id() {
    let mut service = MessageService::default();
    let requester = service.register_agent(None, None, "default", Vec::new());
    let responder = service.register_agent(None, None, "worker", Vec::new());
    let mut response = envelope(responder.clone());
    response.id = "response-1".to_string();
    response.recipient = Recipient::Agent(requester.agent_id.clone());
    response.correlation_id = Some("request-1".to_string());

    service
        .accept_at(&responder.agent_id, response, 10)
        .unwrap();

    let responses = service.responses_for(&requester.agent_id, "request-1", 11);

    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].id, "response-1");
}
