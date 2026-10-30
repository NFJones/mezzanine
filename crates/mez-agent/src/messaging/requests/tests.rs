//! Admission and ownership controls for finite local tracked requests.
//!
//! Tests use real authenticated acceptance and canonical lower custody. Receipt
//! counts and metadata limits are independent of replay, ACKs and ordinary mail.

use super::*;
use crate::messaging::{Recipient, SenderIdentity};

/// Creates two ordinary registered actors and a trusted caller identity. The
/// caller origin is fixture-owned, never deserialized from the peer's payload.
fn fixture() -> (
    MessageService,
    SenderIdentity,
    SenderIdentity,
    MessageRequestOrigin,
) {
    let mut service = MessageService::with_limits(1, 4096);
    let sender = service.register_agent(None, None, "sender", vec![]);
    let child = service.register_agent(None, None, "worker", vec![]);
    let origin = MessageRequestOrigin {
        agent_id: sender.agent_id.clone(),
        conversation_id: "conversation".into(),
        turn_id: "turn".into(),
        started_at_unix_seconds: 1,
    };
    (service, sender, child, origin)
}

/// Builds a content-bearing direct occurrence through the real message owner.
fn mail(sender: &SenderIdentity, child: &SenderIdentity, id: &str) -> Envelope {
    Envelope {
        protocol: "mmp/1",
        id: id.into(),
        message_type: "send".into(),
        time: "fixture:request".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(child.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "question".into(),
        extension_fields: vec![],
    }
}

/// Sixty-four unresolved requests fill this conversation's production ceiling.
/// The sixty-fifth is rejected before any sequence or delivery state changes;
/// another conversation and ordinary untracked sends still work independently.
#[test]
fn request_conversation_limit_precedes_send_and_does_not_block_ordinary_mail() {
    let (mut service, sender, child, origin) = fixture();
    let mut requests = MessageRequests::default();
    for index in 0..64 {
        let receipt = requests
            .accept(
                &mut service,
                origin.clone(),
                mail(&sender, &child, &format!("question-{index}")),
                MessageScope::Session,
                1,
            )
            .unwrap();
        assert_eq!(receipt.generation(), index + 1);
    }
    let before = service.snapshot_state();
    let usage = requests.usage();
    let error = requests
        .accept(
            &mut service,
            origin.clone(),
            mail(&sender, &child, "over-cap"),
            MessageScope::Session,
            2,
        )
        .unwrap_err();
    assert_eq!(error.kind(), MessageErrorKind::Capacity);
    assert!(error.message().len() < 100);
    assert_eq!(service.snapshot_state(), before);
    assert_eq!(requests.usage(), usage);
    let mut other = origin.clone();
    other.conversation_id = "other".into();
    requests
        .accept(
            &mut service,
            other,
            mail(&sender, &child, "other-question"),
            MessageScope::Session,
            3,
        )
        .unwrap();
    service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &child, "ordinary"),
            MessageScope::Session,
            4,
        )
        .unwrap();
    assert_eq!(requests.usage().0, 65);
}

/// Global admission charges the receipt, its future edge, every frozen binding
/// and every terminal handoff before accepting a fanout. Failed multi-recipient
/// admission is atomic and does not consume the next dependency generation.
#[test]
fn request_global_units_include_fanout_and_future_terminal_slots() {
    let (mut service, sender, child, origin) = fixture();
    service.register_agent(None, None, "worker", vec![]);
    let mut requests = MessageRequests::with_limits(MessageRequestLimits {
        units: 4,
        ..MessageRequestLimits::default()
    })
    .unwrap();
    let mut fanout = mail(&sender, &child, "fanout");
    fanout.recipient = Recipient::Role("worker".into());
    let before = service.snapshot_state();
    assert_eq!(
        requests
            .accept(
                &mut service,
                origin.clone(),
                fanout,
                MessageScope::Session,
                1
            )
            .unwrap_err()
            .kind(),
        MessageErrorKind::Capacity
    );
    assert_eq!(service.snapshot_state(), before);
    assert_eq!(requests.usage(), (0, 0, 0));
    let receipt = requests
        .accept(
            &mut service,
            origin.clone(),
            mail(&sender, &child, "direct"),
            MessageScope::Session,
            2,
        )
        .unwrap();
    assert_eq!(receipt.generation(), 1);
    assert_eq!(
        receipt.handoff_bytes_per_recipient(),
        MESSAGE_REQUEST_HANDOFF_BYTES
    );
    assert_eq!(requests.usage().1, 4);
    let before = service.snapshot_state();
    assert_eq!(
        requests
            .accept(
                &mut service,
                origin.clone(),
                mail(&sender, &child, "second"),
                MessageScope::Session,
                3
            )
            .unwrap_err()
            .kind(),
        MessageErrorKind::Capacity
    );
    assert_eq!(service.snapshot_state(), before);
    let mut exact = MessageRequests::with_limits(MessageRequestLimits {
        units: 6,
        ..MessageRequestLimits::default()
    })
    .unwrap();
    let mut fanout = mail(&sender, &child, "exact-fanout");
    fanout.recipient = Recipient::Role("worker".into());
    exact
        .accept(&mut service, origin, fanout, MessageScope::Session, 4)
        .unwrap();
    assert_eq!(exact.usage().1, 6);
}

/// Byte admission independently reserves future handoff metadata, including
/// sender origin string capacity. Tiny payloads cannot bypass that ceiling and
/// failed byte admission leaves both lower custody and request counters empty.
#[test]
fn request_metadata_budget_is_independent_and_accounts_owned_capacity() {
    let (mut service, sender, child, mut origin) = fixture();
    let mut requests = MessageRequests::with_limits(MessageRequestLimits {
        metadata_bytes: MESSAGE_REQUEST_HANDOFF_BYTES,
        ..MessageRequestLimits::default()
    })
    .unwrap();
    let before = service.snapshot_state();
    assert_eq!(
        requests
            .accept(
                &mut service,
                origin.clone(),
                mail(&sender, &child, "tiny"),
                MessageScope::Session,
                1
            )
            .unwrap_err()
            .kind(),
        MessageErrorKind::Capacity
    );
    assert_eq!(service.snapshot_state(), before);
    assert_eq!(requests.usage(), (0, 0, 0));
    let mut requests = MessageRequests::with_limits(MessageRequestLimits {
        metadata_bytes: 20_000,
        ..MessageRequestLimits::default()
    })
    .unwrap();
    origin.conversation_id.reserve(30_000);
    assert_eq!(
        requests
            .accept(
                &mut service,
                origin,
                mail(&sender, &child, "capacity"),
                MessageScope::Session,
                2
            )
            .unwrap_err()
            .kind(),
        MessageErrorKind::Capacity
    );
    assert_eq!(service.snapshot_state(), before);
    assert_eq!(requests.usage(), (0, 0, 0));
}

/// ACK/replay release never consumes a tracked request. Reusing its ID while
/// unresolved is rejected even when the lower idempotency row has disappeared;
/// a wrong conversation cannot discover the receipt or release its reservation.
#[test]
fn request_survives_delivery_release_and_rejects_ambiguous_outstanding_id() {
    let (mut service, sender, child, origin) = fixture();
    let mut requests = MessageRequests::default();
    service.subscribe(&child.agent_id).unwrap();
    let receipt = requests
        .accept(
            &mut service,
            origin.clone(),
            mail(&sender, &child, "request"),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let generation = receipt.generation();
    let sequence = receipt.delivery().sequence;
    service.receive_subscribed(&child.agent_id, 2, 1).unwrap();
    service
        .advance_subscription(&child.agent_id, sequence)
        .unwrap();
    service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &child, "evict"),
            MessageScope::Session,
            3,
        )
        .unwrap();
    assert!(
        service
            .owned_pending_delivery_id(&sender.agent_id, sequence)
            .is_none()
    );
    assert!(
        requests
            .find(&sender.agent_id, "wrong", "request")
            .is_none()
    );
    let receipt = requests
        .find(&sender.agent_id, &origin.conversation_id, "request")
        .unwrap();
    assert_eq!(receipt.proof().sequence(), sequence);
    let before = service.snapshot_state();
    assert_eq!(
        requests
            .accept(
                &mut service,
                origin.clone(),
                mail(&sender, &child, "request"),
                MessageScope::Session,
                4
            )
            .unwrap_err()
            .kind(),
        MessageErrorKind::Conflict
    );
    assert_eq!(service.snapshot_state(), before);
    let mut wrong = origin.clone();
    wrong.started_at_unix_seconds += 1;
    assert!(!requests.consume(generation, &wrong));
    assert_eq!(requests.usage().0, 1);
    assert!(requests.consume(generation, &origin));
    assert_eq!(requests.usage(), (0, 0, 0));
    assert!(!requests.consume(generation, &origin));
    assert_eq!(
        requests
            .accept(
                &mut service,
                origin,
                mail(&sender, &child, "next"),
                MessageScope::Session,
                5
            )
            .unwrap()
            .generation(),
        2
    );
}

/// Conversation retirement releases precisely its reservations; completion of
/// another task does not infer consumption. Invalid origin and lower rejection
/// remain atomic, and another conversation's request stays discoverable.
#[test]
fn request_retirement_and_failed_acceptance_preserve_exact_ownership() {
    let (mut service, sender, child, origin) = fixture();
    let mut requests = MessageRequests::default();
    let before = service.snapshot_state();
    let mut foreign = origin.clone();
    foreign.agent_id = child.agent_id.clone();
    assert_eq!(
        requests
            .accept(
                &mut service,
                foreign,
                mail(&sender, &child, "foreign"),
                MessageScope::Session,
                1
            )
            .unwrap_err()
            .kind(),
        MessageErrorKind::InvalidArgs
    );
    let mut oversized = mail(&sender, &child, "oversized");
    oversized.payload = "x".repeat(4097);
    assert!(
        requests
            .accept(
                &mut service,
                origin.clone(),
                oversized,
                MessageScope::Session,
                1
            )
            .is_err()
    );
    assert_eq!(service.snapshot_state(), before);
    assert_eq!(requests.usage(), (0, 0, 0));
    requests
        .accept(
            &mut service,
            origin.clone(),
            mail(&sender, &child, "first"),
            MessageScope::Session,
            2,
        )
        .unwrap();
    let mut other = origin.clone();
    other.conversation_id = "other".into();
    requests
        .accept(
            &mut service,
            other.clone(),
            mail(&sender, &child, "second"),
            MessageScope::Session,
            3,
        )
        .unwrap();
    requests.retire_conversation(&child.agent_id, &origin.conversation_id);
    assert_eq!(requests.usage().0, 2);
    requests.retire_conversation(&origin.agent_id, &origin.conversation_id);
    assert_eq!(requests.usage().0, 1);
    assert!(
        requests
            .find(&sender.agent_id, &other.conversation_id, "second")
            .is_some()
    );
    requests.retire_conversation(&other.agent_id, &other.conversation_id);
    assert_eq!(requests.usage(), (0, 0, 0));
}

/// Trusted construction cannot silently enlarge a hard ceiling or select zero
/// budgets. Exhausted local generations reject without sending an occurrence;
/// generation allocation is a dependency identity, never a progress witness.
#[test]
fn request_limits_and_generation_exhaustion_fail_before_send() {
    for limits in [
        MessageRequestLimits {
            per_conversation: 65,
            ..MessageRequestLimits::default()
        },
        MessageRequestLimits {
            units: 4097,
            ..MessageRequestLimits::default()
        },
        MessageRequestLimits {
            metadata_bytes: 0,
            ..MessageRequestLimits::default()
        },
    ] {
        assert_eq!(
            MessageRequests::with_limits(limits).unwrap_err().kind(),
            MessageErrorKind::InvalidArgs
        );
    }
    let (mut service, sender, child, origin) = fixture();
    let mut requests = MessageRequests {
        next_generation: u64::MAX,
        ..MessageRequests::default()
    };
    let before = service.snapshot_state();
    assert_eq!(
        requests
            .accept(
                &mut service,
                origin,
                mail(&sender, &child, "exhausted"),
                MessageScope::Session,
                1
            )
            .unwrap_err()
            .kind(),
        MessageErrorKind::InvalidState
    );
    assert_eq!(service.snapshot_state(), before);
    assert_eq!(requests.usage(), (0, 0, 0));
}
