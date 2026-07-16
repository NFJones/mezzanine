//! Messaging delivery tests.

use super::*;

/// Verifies discovers registered agents in stable order.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn discovers_registered_agents_in_stable_order() {
    let mut service = MessageService::default();
    let first = service.register_agent(None, None, "default", Vec::new());
    let second = service.register_agent(None, None, "reviewer", Vec::new());

    let agents = service.discover_agents();

    assert_eq!(agents[0].agent_id, first.agent_id);
    assert_eq!(agents[1].agent_id, second.agent_id);
}

/// Verifies delivers direct and role messages.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn delivers_direct_and_role_messages() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let reviewer = service.register_agent(None, None, "reviewer", Vec::new());
    let mut message = envelope(sender.clone());
    message.recipient = Recipient::Role("reviewer".to_string());

    service.accept_at(&sender.agent_id, message, 10).unwrap();

    let received = service.receive_for(&reviewer.agent_id, 11);
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].payload, "hello");
    assert!(service.receive_for(&sender.agent_id, 11).is_empty());
}

/// Verifies delivers to pane window and capability recipients.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn delivers_to_pane_window_and_capability_recipients() {
    let mut ids = IdFactory::default();
    let pane = ids.pane();
    let window = ids.window();
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(
        Some(pane.clone()),
        Some(window.clone()),
        "worker",
        vec!["search".to_string()],
    );

    let mut pane_message = envelope(sender.clone());
    pane_message.id = "pane".to_string();
    pane_message.recipient = Recipient::Pane(pane);
    service
        .accept_at(&sender.agent_id, pane_message, 10)
        .unwrap();

    let mut window_message = envelope(sender.clone());
    window_message.id = "window".to_string();
    window_message.recipient = Recipient::Window(window.clone());
    service
        .accept_at(&sender.agent_id, window_message, 11)
        .unwrap();
    assert_eq!(service.queued_window_message_count(&window), 1);
    assert_eq!(service.queued_window_message_count(&ids.window()), 0);

    let mut capability_message = envelope(sender.clone());
    capability_message.id = "capability".to_string();
    capability_message.recipient = Recipient::Capability("search".to_string());
    service
        .accept_at(&sender.agent_id, capability_message, 12)
        .unwrap();

    let received = service.receive_for(&target.agent_id, 13);

    assert_eq!(
        received
            .iter()
            .map(|message| message.id.as_str())
            .collect::<Vec<_>>(),
        vec!["pane", "window", "capability"]
    );
}

/// Verifies subscribers receive only their own visible messages.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn subscribers_receive_only_their_own_visible_messages() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let first = service.register_agent(None, None, "worker", Vec::new());
    let second = service.register_agent(None, None, "reviewer", Vec::new());
    service.subscribe(&first.agent_id).unwrap();
    service.subscribe(&second.agent_id).unwrap();

    let mut first_message = envelope(sender.clone());
    first_message.id = "to-first".to_string();
    first_message.recipient = Recipient::Agent(first.agent_id.clone());
    let first_delivery = service
        .accept_at(&sender.agent_id, first_message, 10)
        .unwrap();

    let mut second_message = envelope(sender.clone());
    second_message.id = "to-second".to_string();
    second_message.recipient = Recipient::Agent(second.agent_id.clone());
    let second_delivery = service
        .accept_at(&sender.agent_id, second_message, 11)
        .unwrap();

    let first_batch = service
        .receive_subscribed(&first.agent_id, 12, usize::MAX)
        .unwrap();
    let second_batch = service
        .receive_subscribed(&second.agent_id, 12, usize::MAX)
        .unwrap();

    assert_eq!(first_delivery.sequence, 1);
    assert_eq!(second_delivery.sequence, 2);
    assert_eq!(first_batch.messages.len(), 1);
    assert_eq!(first_batch.messages[0].envelope.id, "to-first");
    assert_eq!(first_batch.messages[0].sequence, first_delivery.sequence);
    assert_eq!(second_batch.messages.len(), 1);
    assert_eq!(second_batch.messages[0].envelope.id, "to-second");
    assert_eq!(second_batch.messages[0].sequence, second_delivery.sequence);
}

/// Verifies subscribed delivery excludes expired messages.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn subscribed_delivery_excludes_expired_messages() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    service.subscribe(&target.agent_id).unwrap();

    let mut expired = envelope(sender.clone());
    expired.id = "expired".to_string();
    expired.recipient = Recipient::Agent(target.agent_id.clone());
    expired.ttl_ms = Some(5);
    let expired_delivery = service
        .accept_at(&sender.agent_id, expired.clone(), 10)
        .unwrap();

    let mut live = envelope(sender.clone());
    live.id = "live".to_string();
    live.recipient = Recipient::Agent(target.agent_id.clone());
    service.accept_at(&sender.agent_id, live, 11).unwrap();

    let batch = service
        .receive_subscribed(&target.agent_id, 16, usize::MAX)
        .unwrap();

    assert_eq!(batch.messages.len(), 1);
    assert_eq!(batch.messages[0].envelope.id, "live");
    let expired_retry = service.accept_at(&sender.agent_id, expired, 16).unwrap();
    assert_eq!(expired_retry.sequence, expired_delivery.sequence);
    assert_eq!(expired_retry.status, DeliveryStatus::Expired);
}

/// Verifies cursor advance limits delivery to newer messages.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn cursor_advance_limits_delivery_to_newer_messages() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    service.subscribe(&target.agent_id).unwrap();

    let mut first = envelope(sender.clone());
    first.id = "first".to_string();
    first.recipient = Recipient::Agent(target.agent_id.clone());
    let first_delivery = service.accept_at(&sender.agent_id, first, 10).unwrap();

    let mut second = envelope(sender.clone());
    second.id = "second".to_string();
    second.recipient = Recipient::Agent(target.agent_id.clone());
    let second_delivery = service.accept_at(&sender.agent_id, second, 11).unwrap();

    let initial = service
        .receive_subscribed(&target.agent_id, 12, usize::MAX)
        .unwrap();
    assert_eq!(
        initial
            .messages
            .iter()
            .map(|message| message.envelope.id.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );

    service
        .advance_subscription(&target.agent_id, first_delivery.sequence)
        .unwrap();
    let after_first_ack = service
        .receive_subscribed(&target.agent_id, 13, usize::MAX)
        .unwrap();
    assert_eq!(after_first_ack.messages.len(), 1);
    assert_eq!(after_first_ack.messages[0].envelope.id, "second");

    service
        .advance_subscription(&target.agent_id, second_delivery.sequence)
        .unwrap();
    assert!(
        service
            .receive_subscribed(&target.agent_id, 14, usize::MAX)
            .unwrap()
            .messages
            .is_empty()
    );
}

/// Verifies cursor advance retains messages for other subscribers.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn cursor_advance_retains_messages_for_other_subscribers() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "default", Vec::new());
    let first = service.register_agent(None, None, "worker", Vec::new());
    let second = service.register_agent(None, None, "reviewer", Vec::new());
    service.subscribe(&first.agent_id).unwrap();
    service.subscribe(&second.agent_id).unwrap();

    let mut fanout = envelope(sender.clone());
    fanout.id = "fanout".to_string();
    fanout.recipient = Recipient::Session;
    let delivery = service.accept_at(&sender.agent_id, fanout, 10).unwrap();

    service
        .advance_subscription(&first.agent_id, delivery.sequence)
        .unwrap();

    assert!(
        service
            .receive_subscribed(&first.agent_id, 11, usize::MAX)
            .unwrap()
            .messages
            .is_empty()
    );
    let second_batch = service
        .receive_subscribed(&second.agent_id, 11, usize::MAX)
        .unwrap();
    assert_eq!(second_batch.messages.len(), 1);
    assert_eq!(second_batch.messages[0].envelope.id, "fanout");
    assert_eq!(second_batch.messages[0].sequence, delivery.sequence);
}
