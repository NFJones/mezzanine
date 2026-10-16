//! Non-materializing readiness preserves canonical delivery ownership and bounds.

use super::*;

/// Exact direct lookup must not be hidden by a page of quiet subscribers, and
/// every query preserves the fanout cursor and durable receive cursor. Expiry
/// and acknowledgement exclude work before caller metadata policy runs.
#[test]
fn readiness_direct_lookup_preserves_cursors_and_expiry() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "agent", Vec::new());
    for _ in 0..80 {
        let empty = service.register_agent(None, None, "empty", Vec::new());
        service
            .subscribe_from_retained_start(&empty.agent_id)
            .unwrap();
    }
    let recipient = service.register_agent(None, None, "recipient", Vec::new());
    service
        .subscribe_from_retained_start(&recipient.agent_id)
        .unwrap();
    let delivery = service
        .accept_at_with_scope(
            &sender.agent_id,
            envelope(
                &sender,
                Recipient::Agent(recipient.agent_id.clone()),
                Some(10),
            ),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let before = service.fanout_after_recipient.clone();
    assert!(service.has_ready_delivery_matching(1, |id, _| id == &recipient.agent_id));
    assert!(!service.has_ready_delivery_matching(1, |_, _| false));
    assert_eq!(service.fanout_after_recipient, before);
    assert_eq!(
        service
            .subscription(&recipient.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    assert!(
        !service.has_ready_delivery_matching(12, |_, _| panic!("expired mail cannot reach policy"))
    );
    service
        .advance_subscription(&recipient.agent_id, delivery.sequence)
        .unwrap();
    assert!(
        !service
            .has_ready_delivery_matching(1, |_, _| panic!("acknowledged mail cannot reach policy"))
    );
}

/// More subscribers than a delivery page must neither strand a later eligible
/// recipient nor keep an idle timer forever after every cursor acknowledges.
/// The complete borrowed scan also quiets all-policy-fenced selector mail.
#[test]
fn readiness_selector_scan_quiets_acknowledged_and_fenced_mail() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "agent", Vec::new());
    for _ in 0..80 {
        let recipient = service.register_agent(None, None, "recipient", Vec::new());
        service
            .subscribe_from_retained_start(&recipient.agent_id)
            .unwrap();
    }
    let delivery = service
        .accept_at_with_scope(
            &sender.agent_id,
            envelope(&sender, Recipient::Session, None),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let mut inspected = 0;
    assert!(!service.has_ready_delivery_matching(1, |_, _| {
        inspected += 1;
        false
    }));
    assert_eq!(inspected, 80);
    let last = service.subscription_order.values().last().unwrap().clone();
    assert!(service.has_ready_delivery_matching(1, |recipient, _| recipient == &last));
    let recipients = service
        .subscription_order
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for recipient in recipients {
        service
            .advance_subscription(&recipient, delivery.sequence)
            .unwrap();
    }
    assert!(!service.has_ready_delivery_matching(1, |_, _| panic!("all mail was acknowledged")));
}

/// Produces one canonical fixture envelope; queries only borrow this payload.
fn envelope(sender: &SenderIdentity, recipient: Recipient, ttl_ms: Option<u64>) -> Envelope {
    Envelope {
        protocol: "mmp/1",
        id: "readiness-mail".into(),
        message_type: "send".into(),
        time: "runtime:1".into(),
        sender: sender.clone(),
        recipient,
        correlation_id: None,
        ttl_ms,
        content_type: "text/plain; charset=utf-8".into(),
        payload: "metadata selection does not clone source bytes".into(),
        extension_fields: Vec::new(),
    }
}
