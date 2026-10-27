//! Atomic immutable-metadata reservation and byte-bounded terminal handoff.
//!
//! Tiny destination budgets exercise the canonical replay/custody union without
//! stressing allocator resources. Large IDs isolate outcome result bytes from
//! the independent record bound and prove consumption can make forward progress.

use super::super::auxiliary::{OUTCOME_BYTES, envelope_bytes, outcome_bytes};
use super::*;

/// Constructs a one-byte role delivery with otherwise stable metadata charge.
fn envelope(sender: &SenderIdentity, id: &str) -> Envelope {
    Envelope {
        protocol: MMP_PROTOCOL,
        id: id.into(),
        message_type: "send".into(),
        time: "fixture:auxiliary".into(),
        sender: sender.clone(),
        recipient: Recipient::Role("target".into()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "x".into(),
        extension_fields: vec![],
    }
}

/// A shared fanout occurrence pays once, but partial ACK cannot free metadata.
/// Full commit permits ordinary replay eviction to admit exactly one replacement;
/// failed reservations preserve sequence, idempotency, offer, and custody state.
#[test]
fn auxiliary_capacity_is_atomic_shared_and_released_after_final_ack() {
    let mut service = MessageService::with_limits(1, 1024);
    let sender = service.register_agent(None, None, "source", vec![]);
    let recipients = (0..2)
        .map(|_| {
            let recipient = service.register_agent(None, None, "target", vec![]);
            service.subscribe(&recipient.agent_id).unwrap();
            recipient
        })
        .collect::<Vec<_>>();
    let first = envelope(&sender, "first");
    let second = envelope(&sender, "other");
    let policy = PendingDeliveryLimits {
        auxiliary_bytes: envelope_bytes(&first),
        ..PendingDeliveryLimits::default()
    };
    service.pending_limits = policy;
    let accepted = service
        .accept_at_with_scope(&sender.agent_id, first, MessageScope::Session, 0)
        .unwrap();
    assert_eq!(accepted.queued_recipients, 2);
    for (index, recipient) in recipients.iter().enumerate() {
        let before = service.snapshot_state();
        assert_eq!(
            service
                .accept_at_with_scope(&sender.agent_id, second.clone(), MessageScope::Session, 0)
                .unwrap_err()
                .kind(),
            MessageErrorKind::Capacity
        );
        assert_eq!(service.snapshot_state(), before);
        assert_eq!(
            MessageService::from_snapshot_state_with_pending_limits(&before, policy)
                .unwrap()
                .snapshot_state(),
            before
        );
        service
            .receive_subscribed(&recipient.agent_id, 0, 1)
            .unwrap();
        service
            .advance_subscription(&recipient.agent_id, accepted.sequence)
            .unwrap();
        assert_eq!(service.pending.len(), usize::from(index == 0));
    }
    let replacement = service
        .accept_at_with_scope(&sender.agent_id, second, MessageScope::Session, 0)
        .unwrap();
    assert_eq!(replacement.sequence, accepted.sequence + 1);
    let snapshot = service.snapshot_state();
    assert_eq!(
        MessageService::from_snapshot_state_with_pending_limits(&snapshot, policy)
            .unwrap()
            .snapshot_state(),
        snapshot
    );
    assert_eq!(
        MessageService::from_snapshot_state_with_pending_limits(
            &snapshot,
            PendingDeliveryLimits {
                auxiliary_bytes: policy.auxiliary_bytes - 1,
                ..policy
            }
        )
        .unwrap_err()
        .kind(),
        MessageErrorKind::InvalidArgs
    );
}

/// Replay eviction must not hide the charge of an older pending occurrence.
/// Committing that custody-only occurrence releases its metadata, enabling a new
/// acceptance without evicting or silently acknowledging any other obligation.
#[test]
fn auxiliary_capacity_charges_custody_only_occurrences() {
    let mut service = MessageService::with_limits(1, 1024);
    let sender = service.register_agent(None, None, "source", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service.subscribe(&target.agent_id).unwrap();
    let first = envelope(&sender, "one");
    service.pending_limits.auxiliary_bytes = envelope_bytes(&first) * 2;
    for id in ["one", "two"] {
        service
            .accept_at_with_scope(
                &sender.agent_id,
                envelope(&sender, id),
                MessageScope::Session,
                0,
            )
            .unwrap();
    }
    assert_eq!(service.queue.len(), 1);
    assert_eq!(service.pending.len(), 2);
    let before = service.snapshot_state();
    assert_eq!(
        service
            .accept_at_with_scope(
                &sender.agent_id,
                envelope(&sender, "new"),
                MessageScope::Session,
                0
            )
            .unwrap_err()
            .kind(),
        MessageErrorKind::Capacity
    );
    assert_eq!(service.snapshot_state(), before);
    let offered = service.receive_subscribed(&target.agent_id, 0, 1).unwrap();
    assert_eq!(offered.messages[0].sequence, 1);
    service.advance_subscription(&target.agent_id, 1).unwrap();
    assert_eq!(
        service
            .accept_at_with_scope(
                &sender.agent_id,
                envelope(&sender, "new"),
                MessageScope::Session,
                0
            )
            .unwrap()
            .sequence,
        3
    );
}

/// Empty variable fields still allocate container entries and must consume the
/// logical reservation. A hostile enlarged snapshot must fail before adoption,
/// even if its payload and recipient counts remain small.
#[test]
fn auxiliary_capacity_counts_empty_entries_and_validates_snapshot_projections() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "source", vec![]);
    service.register_agent(None, None, "target", vec![]);
    let mut source = envelope(&sender, "one");
    let charge = envelope_bytes(&source);
    source.extension_fields = vec![(String::new(), String::new()); 10];
    assert_eq!(envelope_bytes(&source), charge + 10 * 128);
    service.pending_limits.auxiliary_bytes = charge;
    let before = service.snapshot_state();
    assert_eq!(
        service
            .accept_at_with_scope(&sender.agent_id, source, MessageScope::Session, 0)
            .unwrap_err()
            .kind(),
        MessageErrorKind::Capacity
    );
    assert_eq!(service.snapshot_state(), before);
    service
        .accept_at_with_scope(
            &sender.agent_id,
            envelope(&sender, "one"),
            MessageScope::Session,
            0,
        )
        .unwrap();
    let snapshot = service.snapshot_state();
    for accepted_projection in [false, true] {
        let mut hostile = snapshot.clone();
        let envelope = if accepted_projection {
            &mut hostile.accepted_messages[0].envelope
        } else {
            &mut hostile.retained_messages[0].envelope
        };
        envelope.content_type.push('x');
        assert_eq!(
            MessageService::from_snapshot_state_with_pending_limits(
                &hostile,
                service.pending_limits
            )
            .unwrap_err()
            .kind(),
            MessageErrorKind::InvalidArgs
        );
    }
}

/// Repeated long IDs in fanout outcomes hit the byte cap before the100-record
/// cap. Both broad and exact queries return a prefix that can be consumed to
/// reach every terminal recipient, without cloning the accepted payload.
#[test]
fn terminal_outcome_query_bytes_are_bounded_and_consumption_progresses() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "source", vec![]);
    for _ in 0..5 {
        service.register_agent(None, None, "target", vec![]);
    }
    let mut source = envelope(&sender, &"x".repeat(20_000));
    source.ttl_ms = Some(1);
    let accepted = service
        .accept_at_with_scope(&sender.agent_id, source, MessageScope::Session, 0)
        .unwrap();
    service.settle_delivery_outcomes_at(2).unwrap();
    let mut consumed = 0;
    loop {
        let outcomes = service
            .delivery_outcomes_for(&sender.agent_id, 100)
            .unwrap();
        assert_eq!(
            outcomes,
            service
                .delivery_outcomes_for_occurrence(&sender.agent_id, accepted.sequence, 100)
                .unwrap()
        );
        if outcomes.is_empty() {
            break;
        }
        assert!(outcomes.len() <= 3);
        assert!(
            outcomes
                .iter()
                .map(|row| outcome_bytes(&row.message_id, &row.recipient.recipient))
                .sum::<usize>()
                <= OUTCOME_BYTES
        );
        for outcome in outcomes {
            assert!(
                service
                    .acknowledge_delivery_outcome(&sender.agent_id, &outcome)
                    .unwrap()
            );
            consumed += 1;
        }
    }
    assert_eq!(consumed, 5);
}

/// A future terminal record that cannot fit one query must be refused before
/// sequence/acceptance mutation, rather than creating permanently unqueryable
/// custody. Oversized recipient IDs are subject to the same reservation.
#[test]
fn terminal_outcome_single_record_fit_is_reserved_at_acceptance() {
    for large_recipient in [false, true] {
        let mut service = MessageService::default();
        let sender = service.register_agent(None, None, "source", vec![]);
        let target = service.register_agent(None, None, "target", vec![]);
        if large_recipient {
            let mut identity = target;
            identity.agent_id = AgentId::opaque("x".repeat(OUTCOME_BYTES)).unwrap();
            service.ensure_agent_identity(identity, 0).unwrap();
        }
        let message_id = if large_recipient {
            "one".to_string()
        } else {
            "x".repeat(OUTCOME_BYTES)
        };
        let source = envelope(&sender, &message_id);
        let before = service.snapshot_state();
        assert_eq!(
            service
                .accept_at_with_scope(&sender.agent_id, source, MessageScope::Session, 0)
                .unwrap_err()
                .kind(),
            MessageErrorKind::Capacity
        );
        assert_eq!(service.snapshot_state(), before);
    }
}

/// Borrowed live and snapshot representations must charge the same UTF-8 data,
/// including optional identity/envelope fields and vector entries. An exact-fit
/// policy therefore accepts and restores a rich envelope without rounding drift.
#[test]
fn auxiliary_exact_fit_roundtrips_optional_utf8_metadata() {
    let mut service = MessageService::with_limits(1, 1024);
    let mut sender = service
        .register_agent_with_objective(
            PaneId::opaque("%source"),
            WindowId::opaque("@source"),
            "source",
            vec!["mmp".into(), "tools".into()],
            Some("Inspect 🦀 metadata"),
        )
        .unwrap();
    sender.project_scope = Some(ProjectScopeId::from_canonical_root_bytes(b"/fixture"));
    service.ensure_agent_identity(sender.clone(), 0).unwrap();
    service.register_agent(None, None, "target", vec![]);
    let mut source = envelope(&sender, "optional-🦀");
    source.correlation_id = Some("thread-🦀".into());
    source.extension_fields = vec![("x-fixture".into(), "\"🦀\"".into())];
    let policy = PendingDeliveryLimits {
        auxiliary_bytes: envelope_bytes(&source),
        ..PendingDeliveryLimits::default()
    };
    service.pending_limits = policy;
    service
        .accept_at_with_scope(&sender.agent_id, source, MessageScope::Session, 0)
        .unwrap();
    let snapshot = service.snapshot_state();
    assert_eq!(
        MessageService::from_snapshot_state_with_pending_limits(&snapshot, policy)
            .unwrap()
            .snapshot_state(),
        snapshot
    );
}

/// The one-record reservation includes its fixed overhead and recipient ID,
/// allowing the exact64KiB boundary while rejecting the next byte atomically.
#[test]
fn terminal_outcome_single_record_exact_byte_boundary_is_queryable() {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "source", vec![]);
    let recipient = service.register_agent(None, None, "target", vec![]);
    let id = "x".repeat(OUTCOME_BYTES - 128 - recipient.agent_id.as_str().len());
    let mut source = envelope(&sender, &id);
    source.ttl_ms = Some(1);
    service
        .accept_at_with_scope(&sender.agent_id, source, MessageScope::Session, 0)
        .unwrap();
    service.settle_delivery_outcomes_at(2).unwrap();
    let outcomes = service
        .delivery_outcomes_for(&sender.agent_id, 100)
        .unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcome_bytes(&outcomes[0].message_id, &outcomes[0].recipient.recipient),
        OUTCOME_BYTES
    );
    assert!(
        service
            .acknowledge_delivery_outcome(&sender.agent_id, &outcomes[0])
            .unwrap()
    );
    assert!(service.pending.is_empty());
}
