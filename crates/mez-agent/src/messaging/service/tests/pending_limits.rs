//! Tiny independent custody budgets through real acceptance and exact ACKs.
//!
//! Policies are construction-time narrowing, never envelope or snapshot authority.
//! These controls exercise each dimension without pressure on host resources.

use super::*;

/// Each limit rejects zero and values above the production ceiling. A supplied
/// policy must never enlarge the reviewed custody resource cap.
#[test]
fn pending_limits_reject_invalid_policy() {
    let defaults = PendingDeliveryLimits::default();
    assert_eq!(defaults.messages, 1000);
    assert_eq!(defaults.payload_bytes, 1024 * 1024);
    assert_eq!(defaults.recipient_obligations, 4096);
    assert_eq!(defaults.per_recipient, 256);
    for field in 0..5 {
        for excessive in [false, true] {
            let mut policy = defaults;
            let limit = match field {
                0 => &mut policy.messages,
                1 => &mut policy.payload_bytes,
                2 => &mut policy.recipient_obligations,
                3 => &mut policy.per_recipient,
                _ => &mut policy.auxiliary_bytes,
            };
            *limit = if excessive { *limit + 1 } else { 0 };
            assert_eq!(
                MessageService::with_pending_limits(1, 1024, policy)
                    .unwrap_err()
                    .kind(),
                MessageErrorKind::InvalidArgs
            );
            let empty = MessageService::default().snapshot_state();
            assert_eq!(
                MessageService::from_snapshot_state_with_pending_limits(&empty, policy)
                    .unwrap_err()
                    .kind(),
                MessageErrorKind::InvalidArgs
            );
        }
    }
}

/// Builds role-scoped mail with an exact byte charge, avoiding incidental
/// ID/text sizes in the payload-budget oracle.
fn envelope(sender: &SenderIdentity, id: &str, bytes: usize) -> Envelope {
    Envelope {
        protocol: MMP_PROTOCOL,
        id: id.into(),
        message_type: "send".into(),
        time: "fixture:limits".into(),
        sender: sender.clone(),
        recipient: Recipient::Role("target".into()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "x".repeat(bytes),
        extension_fields: Vec::new(),
    }
}

/// Isolate count, unique bytes, aggregate slots and exact-recipient slots. Both
/// live and restored owners reject before changing ANY captured state; partial
/// fanout ACK keeps the reservation, and final ACK admits exactly one new
/// occurrence. Optional history does not hold pending capacity.
#[test]
fn pending_limits_independent_budgets_are_atomic_and_ack_reusable() {
    let default = PendingDeliveryLimits::default();
    let cases = [
        (
            PendingDeliveryLimits {
                messages: 1,
                ..default
            },
            1,
            1,
            1,
        ),
        (
            PendingDeliveryLimits {
                payload_bytes: 5,
                ..default
            },
            2,
            4,
            2,
        ),
        (
            PendingDeliveryLimits {
                recipient_obligations: 3,
                ..default
            },
            2,
            1,
            1,
        ),
        (
            PendingDeliveryLimits {
                per_recipient: 1,
                ..default
            },
            1,
            1,
            1,
        ),
    ];
    for (policy, recipients, first_bytes, next_bytes) in cases {
        let mut service = MessageService::with_pending_limits(1, 1024, policy).unwrap();
        let sender = service.register_agent(None, None, "sender", Vec::new());
        let targets = (0..recipients)
            .map(|_| {
                let target = service.register_agent(None, None, "target", Vec::new());
                service.subscribe(&target.agent_id).unwrap();
                target
            })
            .collect::<Vec<_>>();
        let original = envelope(&sender, "original", first_bytes);
        let accepted = service
            .accept_at_with_scope(&sender.agent_id, original.clone(), MessageScope::Session, 1)
            .unwrap();
        let snapshot = service.snapshot_state();
        let mut restored =
            MessageService::from_snapshot_state_with_pending_limits(&snapshot, policy).unwrap();
        let next = envelope(&sender, "next", next_bytes);
        for owner in [&mut service, &mut restored] {
            assert_eq!(
                owner
                    .accept_at_with_scope(
                        &sender.agent_id,
                        original.clone(),
                        MessageScope::Session,
                        2
                    )
                    .unwrap(),
                accepted,
                "duplicate uses original reservation"
            );
            assert_eq!(
                owner
                    .accept_at_with_scope(&sender.agent_id, next.clone(), MessageScope::Session, 2)
                    .unwrap_err()
                    .kind(),
                MessageErrorKind::Capacity
            );
            assert_eq!(
                owner.snapshot_state(),
                snapshot,
                "refusal must not change sequence, receipts, source, offers or cursors"
            );
            for (index, target) in targets.iter().enumerate() {
                let selected = owner.receive_subscribed(&target.agent_id, 2, 10).unwrap();
                assert_eq!(selected.messages.len(), 1);
                assert_eq!(selected.messages[0].envelope.as_ref(), &original);
                owner
                    .advance_subscription(&target.agent_id, accepted.sequence)
                    .unwrap();
                if index + 1 < targets.len() {
                    let before = owner.snapshot_state();
                    assert_eq!(
                        owner
                            .accept_at_with_scope(
                                &sender.agent_id,
                                next.clone(),
                                MessageScope::Session,
                                2
                            )
                            .unwrap_err()
                            .kind(),
                        MessageErrorKind::Capacity
                    );
                    assert_eq!(owner.snapshot_state(), before);
                }
            }
            assert!(owner.pending.is_empty());
            assert_eq!(owner.queue.len(), 1, "committed replay may remain");
            let admitted = owner
                .accept_at_with_scope(&sender.agent_id, next.clone(), MessageScope::Session, 2)
                .unwrap();
            assert_eq!(admitted.sequence, accepted.sequence + 1);
            assert_eq!(admitted.queued_recipients, recipients);
            let before = owner.snapshot_state();
            assert_eq!(
                owner
                    .accept_at_with_scope(
                        &sender.agent_id,
                        envelope(&sender, "extra", first_bytes),
                        MessageScope::Session,
                        2
                    )
                    .unwrap_err()
                    .kind(),
                MessageErrorKind::Capacity
            );
            assert_eq!(owner.snapshot_state(), before);
        }
    }
}

/// Snapshots contain facts, not requested policy. Each narrowed dimension
/// refuses genuine over-cap source without dropping recipients; exact caps
/// succeed. Default restoration deliberately uses the unchanged default policy.
#[test]
fn pending_limits_restore_refuses_each_over_cap_dimension() {
    let mut source = MessageService::with_limits(1, 1024);
    let sender = source.register_agent(None, None, "sender", Vec::new());
    for _ in 0..2 {
        source.register_agent(None, None, "target", Vec::new());
    }
    for id in ["first", "second"] {
        source
            .accept_at_with_scope(
                &sender.agent_id,
                envelope(&sender, id, 4),
                MessageScope::Session,
                1,
            )
            .unwrap();
    }
    let snapshot = source.snapshot_state();
    assert_eq!(snapshot.custody_messages.as_ref().unwrap().len(), 1);
    // Legacy6 has replay-only source and no explicit commits. Its exact
    // bindings plus unadvanced cursors still prove these unread obligations.
    let mut legacy = snapshot.clone();
    legacy.schema_version = 6;
    legacy.retention_messages = 2;
    legacy
        .retained_messages
        .extend(legacy.custody_messages.take().unwrap());
    legacy.retained_messages.sort_by_key(|row| row.sequence);
    for row in &mut legacy.retained_messages {
        row.acknowledged_recipients = None;
        row.ack_expiry_observed_at_ms = None;
        row.terminal_recipients = None;
        row.sender_registered_after_sequence = None;
    }
    assert_eq!(
        MessageService::from_snapshot_state(&legacy)
            .unwrap()
            .pending
            .len(),
        2
    );
    let default = PendingDeliveryLimits::default();
    for policy in [
        PendingDeliveryLimits {
            messages: 1,
            ..default
        },
        PendingDeliveryLimits {
            payload_bytes: 7,
            ..default
        },
        PendingDeliveryLimits {
            recipient_obligations: 3,
            ..default
        },
        PendingDeliveryLimits {
            per_recipient: 1,
            ..default
        },
    ] {
        for candidate in [&snapshot, &legacy] {
            assert_eq!(
                MessageService::from_snapshot_state_with_pending_limits(candidate, policy)
                    .unwrap_err()
                    .kind(),
                MessageErrorKind::InvalidArgs
            );
        }
        assert_eq!(source.snapshot_state(), snapshot);
    }
    assert_eq!(
        MessageService::from_snapshot_state(&snapshot)
            .unwrap()
            .snapshot_state(),
        snapshot
    );
    let exact = PendingDeliveryLimits {
        messages: 2,
        payload_bytes: 8,
        recipient_obligations: 4,
        per_recipient: 2,
        ..PendingDeliveryLimits::default()
    };
    assert_eq!(
        MessageService::from_snapshot_state_with_pending_limits(&snapshot, exact)
            .unwrap()
            .snapshot_state(),
        snapshot
    );
    let restored = MessageService::from_snapshot_state_with_pending_limits(&legacy, exact).unwrap();
    assert_eq!(restored.pending.len(), 2);
    assert_eq!(restored.pending_limits, exact);
}

/// Optional replay retention and individual envelope validation do not inherit
/// the custody policy. A byte-cap refusal is typed Capacity, whereas a payload
/// above the separately selected envelope limit remains InvalidArgs. Neither
/// path allocates a sequence or receipt, and a smaller legitimate send works.
#[test]
fn pending_limits_do_not_retarget_replay_or_payload_validation() {
    for (replay_bytes, pending_bytes, expected) in [
        (8, 3, MessageErrorKind::Capacity),
        (3, 8, MessageErrorKind::InvalidArgs),
    ] {
        let policy = PendingDeliveryLimits {
            payload_bytes: pending_bytes,
            ..PendingDeliveryLimits::default()
        };
        let mut service = MessageService::with_pending_limits(1, replay_bytes, policy).unwrap();
        let sender = service.register_agent(None, None, "sender", Vec::new());
        service.register_agent(None, None, "target", Vec::new());
        let before = service.snapshot_state();
        assert_eq!(
            service
                .accept_at_with_scope(
                    &sender.agent_id,
                    envelope(&sender, "too-large", 4),
                    MessageScope::Session,
                    1
                )
                .unwrap_err()
                .kind(),
            expected
        );
        assert_eq!(service.snapshot_state(), before);
        assert_eq!(
            service
                .accept_at_with_scope(
                    &sender.agent_id,
                    envelope(&sender, "fits", 3),
                    MessageScope::Session,
                    1
                )
                .unwrap()
                .sequence,
            1
        );
    }
}

/// Recovery can inspect only genuine offers for the current recipient interval,
/// including custody-only mail after replay eviction and TTL expiry. Inspection
/// must not create an offer for another row or advance the cursor.
#[test]
fn pending_offered_reconciliation_survives_replay_eviction_and_expiry() {
    let mut service =
        MessageService::with_pending_limits(1, 1024, PendingDeliveryLimits::default()).unwrap();
    let sender = service.register_agent(None, None, "sender", Vec::new());
    let target = service.register_agent(None, None, "target", Vec::new());
    service
        .subscribe_from_retained_start(&target.agent_id)
        .unwrap();
    let mut first = envelope(&sender, "offered-expiring", 1);
    first.ttl_ms = Some(10);
    let accepted = service
        .accept_at_with_scope(&sender.agent_id, first, MessageScope::Session, 100)
        .unwrap();
    service
        .receive_subscribed(&target.agent_id, 100, 1)
        .unwrap();
    service
        .accept_at_with_scope(
            &sender.agent_id,
            envelope(&sender, "unoffered", 1),
            MessageScope::Session,
            100,
        )
        .unwrap();
    assert_eq!(service.queue.len(), 1);
    let before = service.snapshot_state();
    let offered = service
        .offered_uncommitted_deliveries(&target.agent_id)
        .unwrap();
    assert_eq!(offered.len(), 1);
    assert_eq!(offered[0].sequence, accepted.sequence);
    assert_eq!(service.snapshot_state(), before);
    assert!(
        service
            .receive_subscribed(&target.agent_id, 111, 10)
            .unwrap()
            .messages
            .iter()
            .all(|row| row.sequence != accepted.sequence)
    );
    service
        .advance_subscription(&target.agent_id, accepted.sequence)
        .unwrap();
    assert!(
        service
            .offered_uncommitted_deliveries(&target.agent_id)
            .unwrap()
            .iter()
            .all(|row| row.sequence != accepted.sequence)
    );
}
