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

/// The actual default global ceiling admits exactly 1,024 direct requests,
/// charging four units each across sixteen independently bounded conversations.
/// The next tracked request must fail before acceptance while ordinary mail
/// remains available. Only the exact originating owner can release capacity;
/// release/re-admission keeps generations monotonic and never ACKs lower mail.
/// Real delivery ACKs during setup free the separate pending-message budget
/// without consuming any of the 1,024 retained request receipts.
#[test]
fn request_production_global_ceiling_is_inclusive_and_reusable() {
    let (mut service, sender, child, origin) = fixture();
    let mut requests = MessageRequests::default();
    service.subscribe(&child.agent_id).unwrap();
    let mut first = None;
    for index in 0..1024 {
        let mut caller = origin.clone();
        caller.conversation_id = format!("conversation-{:02}", index / 64);
        let receipt = requests
            .accept(
                &mut service,
                caller,
                mail(&sender, &child, &format!("question-{index:04}")),
                MessageScope::Session,
                index + 1,
            )
            .unwrap();
        if index == 0 {
            first = Some((receipt.generation(), receipt.origin().clone()));
        }
        let sequence = receipt.delivery().sequence;
        assert_eq!(
            service
                .receive_subscribed(&child.agent_id, index + 1, 1)
                .unwrap()
                .messages
                .len(),
            1
        );
        service
            .advance_subscription(&child.agent_id, sequence)
            .unwrap();
        assert_eq!(requests.usage().1, (index as usize + 1) * 4);
    }
    assert_eq!(requests.usage().0, 1024);
    assert_eq!(requests.usage().1, 4096);
    assert!(requests.usage().2 < MessageRequestLimits::default().metadata_bytes);
    let before = service.snapshot_state();
    let usage = requests.usage();
    assert_eq!(
        requests
            .accept(
                &mut service,
                origin.clone(),
                mail(&sender, &child, "over-global"),
                MessageScope::Session,
                1025,
            )
            .unwrap_err()
            .kind(),
        MessageErrorKind::Capacity
    );
    assert_eq!(service.snapshot_state(), before);
    assert_eq!(requests.usage(), usage);
    service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &child, "ordinary-at-global-cap"),
            MessageScope::Session,
            1026,
        )
        .unwrap();
    let (generation, caller) = first.unwrap();
    let mut foreign = caller.clone();
    foreign.started_at_unix_seconds += 1;
    assert!(!requests.consume(generation, &foreign));
    assert_eq!(requests.usage(), usage);
    let transport = service.snapshot_state();
    assert!(requests.consume(generation, &caller));
    assert_eq!(requests.usage().1, 4092);
    assert_eq!(service.snapshot_state(), transport);
    let receipt = requests
        .accept(
            &mut service,
            caller,
            mail(&sender, &child, "after-exact-owner-consumption"),
            MessageScope::Session,
            1027,
        )
        .unwrap();
    assert_eq!(receipt.generation(), 1025);
    assert_eq!(requests.usage().1, 4096);
    let transport = service.snapshot_state();
    for index in 0..16 {
        requests.retire_conversation(&sender.agent_id, &format!("conversation-{index:02}"));
    }
    assert_eq!(requests.usage(), (0, 0, 0));
    assert_eq!(service.snapshot_state(), transport);
}

/// The default 8 MiB metadata bound is independent of the count ceiling.
/// Measure a real compact receipt, then vary only trusted origin allocation to
/// reach exactly that byte charge or one byte beyond it. This is lower-owner
/// accounting coverage, not model authority for arbitrary native identities.
/// At exact capacity ordinary sends still work; rejection and consumption must
/// preserve lower transport and only genuine acceptance advances generation.
#[test]
fn request_production_metadata_ceiling_is_inclusive_and_independent() {
    let (mut service, sender, child, origin) = fixture();
    let mut requests = MessageRequests::default();
    let receipt = requests
        .accept(
            &mut service,
            origin.clone(),
            mail(&sender, &child, "byte-budget1"),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let generation = receipt.generation();
    let capacity = receipt.origin().conversation_id.capacity();
    let limit = MessageRequestLimits::default().metadata_bytes;
    let exact_capacity = limit - requests.usage().2 + capacity;
    assert!(requests.consume(generation, &origin));
    let mut too_large = origin.clone();
    too_large.conversation_id = "c"
        .repeat(exact_capacity + 1)
        .into_boxed_str()
        .into_string();
    assert_eq!(too_large.conversation_id.capacity(), exact_capacity + 1);
    let before = service.snapshot_state();
    assert_eq!(
        requests
            .accept(
                &mut service,
                too_large,
                mail(&sender, &child, "byte-budget2"),
                MessageScope::Session,
                2
            )
            .unwrap_err()
            .kind(),
        MessageErrorKind::Capacity
    );
    assert_eq!(requests.usage(), (0, 0, 0));
    assert_eq!(service.snapshot_state(), before);
    let mut exact = origin.clone();
    exact.conversation_id = "c".repeat(exact_capacity).into_boxed_str().into_string();
    assert_eq!(exact.conversation_id.capacity(), exact_capacity);
    let receipt = requests
        .accept(
            &mut service,
            exact.clone(),
            mail(&sender, &child, "byte-budget3"),
            MessageScope::Session,
            3,
        )
        .unwrap();
    let generation = receipt.generation();
    assert_eq!(generation, 2);
    assert_eq!(requests.usage(), (1, 4, limit));
    let before = service.snapshot_state();
    assert_eq!(
        requests
            .accept(
                &mut service,
                origin,
                mail(&sender, &child, "byte-budget4"),
                MessageScope::Session,
                4
            )
            .unwrap_err()
            .kind(),
        MessageErrorKind::Capacity
    );
    assert_eq!(requests.usage(), (1, 4, limit));
    assert_eq!(service.snapshot_state(), before);
    service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &child, "ordinary-at-byte-cap"),
            MessageScope::Session,
            5,
        )
        .unwrap();
    let before = service.snapshot_state();
    assert!(requests.consume(generation, &exact));
    assert_eq!(requests.usage(), (0, 0, 0));
    assert_eq!(service.snapshot_state(), before);
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

/// Original TTL observation has a strict boundary and its own retained window,
/// even after transport no longer owns the payload. Only unprotected finite
/// receipts age out: genuine outcome custody stays charged, no-TTL ownership
/// survives, counters release exactly once and generations never restart.
#[test]
fn request_expiry_releases_only_unprotected_finite_receipts_after_handoff() {
    let (mut service, sender, child, origin) = fixture();
    let mut requests = MessageRequests::default();
    let mut finite = mail(&sender, &child, "finite");
    finite.ttl_ms = Some(1);
    let first = requests
        .accept(
            &mut service,
            origin.clone(),
            finite.clone(),
            MessageScope::Session,
            10,
        )
        .unwrap()
        .generation();
    let charge = requests.receipt(first).unwrap().metadata_bytes;
    requests
        .accept(
            &mut service,
            origin.clone(),
            mail(&sender, &child, "indefinite"),
            MessageScope::Session,
            10,
        )
        .unwrap();
    finite.id = "protected".into();
    let protected = requests
        .accept(
            &mut service,
            origin.clone(),
            finite,
            MessageScope::Session,
            10,
        )
        .unwrap()
        .generation();
    let original = requests.usage();
    let transport = service.snapshot_state();
    assert!(requests.needs_expiry_maintenance(|_| false));
    assert!(requests.retire_expired_at(11, |_| false).is_empty());
    assert!(
        requests
            .receipt(first)
            .unwrap()
            .expiry_observed_at_ms
            .is_none()
    );
    assert!(
        requests
            .retire_expired_at(12, |id| id == protected)
            .is_empty()
    );
    assert!(
        requests
            .retire_expired_at(600_011, |id| id == protected)
            .is_empty()
    );
    assert_eq!(requests.usage(), original);
    assert_eq!(
        requests.retire_expired_at(600_012, |id| id == protected),
        vec![first]
    );
    assert_eq!(requests.usage(), (2, original.1 - 4, original.2 - charge));
    assert!(!requests.needs_expiry_maintenance(|id| id == protected));
    assert_eq!(
        requests.retire_expired_at(600_012, |_| false),
        vec![protected]
    );
    assert!(!requests.needs_expiry_maintenance(|_| false));
    assert!(requests.retire_expired_at(u64::MAX, |_| false).is_empty());
    assert_eq!(requests.usage().0, 1);
    assert_eq!(
        service.snapshot_state(),
        transport,
        "tracking expiry cannot invent transport offers or ACKs"
    );
    let next = requests
        .accept(
            &mut service,
            origin,
            mail(&sender, &child, "later"),
            MessageScope::Session,
            600_013,
        )
        .unwrap();
    assert_eq!(next.generation(), protected + 1);
}

/// The first expiry observation is immutable across backward clock snapshots,
/// while saturating TTL arithmetic cannot turn an unrepresentable expiration
/// into an immediate loss or an endless maintenance request. Neither case
/// fabricates receipt consumption before its exact retention boundary.
#[test]
fn request_expiry_observation_survives_rollback_and_saturated_deadlines() {
    let (mut service, sender, child, origin) = fixture();
    let mut requests = MessageRequests::default();
    let mut finite = mail(&sender, &child, "rollback");
    finite.ttl_ms = Some(1);
    let generation = requests
        .accept(
            &mut service,
            origin.clone(),
            finite,
            MessageScope::Session,
            10,
        )
        .unwrap()
        .generation();
    assert!(requests.retire_expired_at(12, |_| false).is_empty());
    assert!(requests.retire_expired_at(11, |_| false).is_empty());
    assert_eq!(
        requests.receipt(generation).unwrap().expiry_observed_at_ms,
        Some(12)
    );
    assert!(requests.retire_expired_at(600_011, |_| false).is_empty());
    assert_eq!(
        requests.retire_expired_at(600_012, |_| false),
        vec![generation]
    );
    assert_eq!(requests.usage(), (0, 0, 0));
    let mut saturated = mail(&sender, &child, "saturated");
    saturated.ttl_ms = Some(2);
    requests
        .accept(
            &mut service,
            origin,
            saturated,
            MessageScope::Session,
            u64::MAX - 1,
        )
        .unwrap();
    assert!(!requests.needs_expiry_maintenance(|_| false));
    assert!(requests.retire_expired_at(u64::MAX, |_| false).is_empty());
    assert_eq!(requests.usage().0, 1);
}
