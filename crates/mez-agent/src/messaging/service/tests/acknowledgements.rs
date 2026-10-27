//! Recipient-qualified ACK regressions at the canonical service boundary.
//!
//! Reading a selected occurrence offers it; acceptance, another recipient's
//! selection and arbitrary global sequence knowledge do not authorize an ACK.

use super::*;

/// Creates an immutable direct occurrence without private or native fixtures.
fn mail(sender: &SenderIdentity, target: &SenderIdentity, id: &str) -> Envelope {
    Envelope {
        protocol: MMP_PROTOCOL,
        id: id.into(),
        message_type: "send".into(),
        time: "fixture:ack".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain; charset=utf-8".into(),
        payload: "ack evidence".into(),
        extension_fields: Vec::new(),
    }
}

/// Acceptance alone cannot discard unread own mail. An unrelated recipient's
/// accepted global high-water is also not an offer to this subscription. Both
/// refusals must preserve the entire canonical snapshot and original cursor.
#[test]
fn acknowledgement_rejects_unoffered_and_foreign_occurrences_without_mutation() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    let other = service.register_agent(None, None, "other", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    let own = service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &target, "own"),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let foreign = service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &other, "foreign"),
            MessageScope::Session,
            2,
        )
        .unwrap();
    let before = service.snapshot_state();
    for sequence in [own.sequence, foreign.sequence, foreign.sequence + 1] {
        assert!(
            service
                .advance_subscription(&target.agent_id, sequence)
                .is_err(),
            "unoffered sequence {sequence} was acknowledged"
        );
        assert_eq!(service.snapshot_state(), before);
    }
    let batch = service.receive_subscribed(&target.agent_id, 3, 10).unwrap();
    assert_eq!(batch.messages.len(), 1);
    service
        .advance_subscription(&target.agent_id, own.sequence)
        .unwrap();
    assert_eq!(
        service
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        own.sequence
    );
}

/// A caller-provided receive cursor can select a later occurrence but cannot
/// turn that offer into permission to skip earlier live unread OWN mail. Once
/// the normal prefix is offered, cumulative, duplicate and regressive ACKs work.
#[test]
fn acknowledgement_cannot_skip_unoffered_own_prefix() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    let first = service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &target, "first"),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let second = service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &target, "second"),
            MessageScope::Session,
            2,
        )
        .unwrap();
    let selected = service
        .receive_after(
            &DeliveryCursor {
                recipient: target.agent_id.clone(),
                last_sequence: first.sequence,
            },
            3,
            1,
        )
        .unwrap();
    assert_eq!(selected.messages[0].sequence, second.sequence);
    let before = service.snapshot_state();
    assert!(
        service
            .advance_subscription(&target.agent_id, second.sequence)
            .is_err()
    );
    assert_eq!(service.snapshot_state(), before);
    assert_eq!(
        service
            .receive_subscribed(&target.agent_id, 4, 10)
            .unwrap()
            .messages
            .len(),
        2
    );
    service
        .advance_subscription(&target.agent_id, second.sequence)
        .unwrap();
    service
        .advance_subscription(&target.agent_id, second.sequence)
        .unwrap();
    service
        .advance_subscription(&target.agent_id, first.sequence)
        .unwrap();
    assert_eq!(
        service
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        second.sequence
    );
}

/// Fanout selection belongs only to the selected registration. A fast peer's
/// ACK cannot authorize a slower peer, including after a snapshot round trip;
/// unrelated global sequence gaps do not prevent a valid cumulative own ACK.
#[test]
fn acknowledgement_fanout_offers_are_recipient_local_and_snapshot_stable() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let first = service.register_agent(None, None, "worker", Vec::new());
    let second = service.register_agent(None, None, "worker", Vec::new());
    for target in [&first, &second] {
        service.subscribe(&target.agent_id).unwrap();
    }
    let mut fanout = mail(&sender, &first, "fanout");
    fanout.recipient = Recipient::Role("worker".into());
    let delivery = service
        .accept_at_with_scope(&sender.agent_id, fanout, MessageScope::Session, 1)
        .unwrap();
    let batch = service
        .fanout_ready_for(&first.agent_id, 2, 1)
        .unwrap()
        .unwrap();
    service.acknowledge_fanout_batch(&batch).unwrap();
    let snapshot = service.snapshot_state();
    let mut restored = MessageService::from_snapshot_state(&snapshot).unwrap();
    assert_eq!(restored.snapshot_state(), snapshot);
    assert!(
        restored
            .advance_subscription(&second.agent_id, delivery.sequence)
            .is_err()
    );
    let batches = restored.fanout_ready(3, 10);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].recipient, second.agent_id);
    restored.acknowledge_fanout_batch(&batches[0]).unwrap();
    service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &sender, "global-gap"),
            MessageScope::Session,
            4,
        )
        .unwrap();
    let next = service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &first, "after-gap"),
            MessageScope::Session,
            5,
        )
        .unwrap();
    service.receive_subscribed(&first.agent_id, 6, 10).unwrap();
    service
        .advance_subscription(&first.agent_id, next.sequence)
        .unwrap();
}

/// Cumulative ACK may pass an unoffered own row that had genuinely expired at
/// selection, but not a live row. TTL equality remains live, and a clock rollback
/// after a valid offer cannot erase its stronger observed expiry evidence.
#[test]
fn acknowledgement_expired_prefix_and_ttl_boundary_use_actual_offer_clock() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    let mut first = mail(&sender, &target, "expires");
    first.ttl_ms = Some(5);
    let first = service
        .accept_at_with_scope(&sender.agent_id, first, MessageScope::Session, 1)
        .unwrap();
    let second = service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &target, "later"),
            MessageScope::Session,
            2,
        )
        .unwrap();
    let skip = DeliveryCursor {
        recipient: target.agent_id.clone(),
        last_sequence: first.sequence,
    };
    service.receive_after(&skip, 6, 1).unwrap();
    assert!(
        service
            .advance_subscription(&target.agent_id, second.sequence)
            .is_err()
    );
    let batch = service.receive_subscribed(&target.agent_id, 7, 10).unwrap();
    assert_eq!(batch.messages.len(), 1);
    service.receive_after(&skip, 3, 1).unwrap();
    let mut restored = MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
    restored
        .advance_subscription(&target.agent_id, second.sequence)
        .unwrap();

    let mut boundary = mail(&sender, &target, "boundary");
    boundary.ttl_ms = Some(5);
    let boundary = service
        .accept_at_with_scope(&sender.agent_id, boundary, MessageScope::Session, 10)
        .unwrap();
    service
        .receive_subscribed(&target.agent_id, 15, 10)
        .unwrap();
    let mut restored = MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
    restored
        .advance_subscription(&target.agent_id, boundary.sequence)
        .unwrap();
}

/// Schema6 offers must be a unique subset of frozen accepted recipients, with
/// valid live selection times. Missing/forged/future-schema evidence rejects
/// without changing the source. Schema5 restores known recipients but cannot
/// infer offers; fresh selection enables the original stable occurrence's ACK.
#[test]
fn acknowledgement_snapshot_validates_offers_and_upgrades_schema5_fail_closed() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    let mut message = mail(&sender, &target, "snapshot");
    message.ttl_ms = Some(10);
    let delivery = service
        .accept_at_with_scope(&sender.agent_id, message, MessageScope::Session, 1)
        .unwrap();
    service.receive_subscribed(&target.agent_id, 2, 10).unwrap();
    let before = service.snapshot_state();
    let mut corruptions = Vec::new();
    let mut bad = before.clone();
    bad.retained_messages[0].offered_recipients = None;
    corruptions.push(bad);
    let mut bad = before.clone();
    bad.retained_messages[0]
        .offered_recipients
        .as_mut()
        .unwrap()[0]
        .recipient = sender.agent_id.to_string();
    corruptions.push(bad);
    let mut bad = before.clone();
    let duplicate = bad.retained_messages[0]
        .offered_recipients
        .as_ref()
        .unwrap()[0]
        .clone();
    bad.retained_messages[0]
        .offered_recipients
        .as_mut()
        .unwrap()
        .push(duplicate);
    corruptions.push(bad);
    let mut bad = before.clone();
    bad.retained_messages[0]
        .offered_recipients
        .as_mut()
        .unwrap()[0]
        .observed_at_ms = 12;
    corruptions.push(bad);
    let mut bad = before.clone();
    bad.schema_version = 5;
    corruptions.push(bad);
    let mut bad = before.clone();
    bad.schema_version = 7;
    corruptions.push(bad);
    for bad in corruptions {
        let decoded =
            serde_json::from_str::<MessageServiceSnapshot>(&serde_json::to_string(&bad).unwrap())
                .unwrap();
        assert!(MessageService::from_snapshot_state(&decoded).is_err());
        assert_eq!(service.snapshot_state(), before);
    }
    let mut legacy = before.clone();
    legacy.schema_version = 5;
    for row in &mut legacy.retained_messages {
        row.offered_recipients = None;
    }
    let mut restored = MessageService::from_snapshot_state(&legacy).unwrap();
    assert!(
        restored
            .advance_subscription(&target.agent_id, delivery.sequence)
            .is_err()
    );
    assert_eq!(
        restored
            .receive_subscribed(&target.agent_id, 3, 10)
            .unwrap()
            .messages[0]
            .sequence,
        delivery.sequence
    );
    restored
        .advance_subscription(&target.agent_id, delivery.sequence)
        .unwrap();
    assert_eq!(legacy.schema_version, 5);
}

/// An old offer remains evidence of the former binding only. Reusing the exact
/// opaque ID cannot ACK old mail or a fresh occurrence that has not been offered.
/// The new owner's normal receive then enables only its own occurrence.
#[test]
fn acknowledgement_offer_does_not_transfer_to_reused_registration() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "worker", Vec::new());
    service.subscribe(&target.agent_id).unwrap();
    let old = service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &target, "old"),
            MessageScope::Session,
            1,
        )
        .unwrap();
    service.receive_subscribed(&target.agent_id, 2, 10).unwrap();
    service.retire_observational_identity(&target.agent_id);
    service.ensure_agent_identity(target.clone(), 3).unwrap();
    service
        .subscribe_from_retained_start(&target.agent_id)
        .unwrap();
    let new = service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &target, "new"),
            MessageScope::Session,
            4,
        )
        .unwrap();
    let mut restored = MessageService::from_snapshot_state(&service.snapshot_state()).unwrap();
    assert!(
        restored
            .advance_subscription(&target.agent_id, new.sequence)
            .is_err()
    );
    // The old retirement high-water is an inert regressive ACK, not a new offer.
    restored
        .advance_subscription(&target.agent_id, old.sequence)
        .unwrap();
    let batch = restored
        .receive_subscribed(&target.agent_id, 5, 10)
        .unwrap();
    assert_eq!(batch.messages.len(), 1);
    assert_eq!(batch.messages[0].sequence, new.sequence);
    restored
        .advance_subscription(&target.agent_id, new.sequence)
        .unwrap();
}
