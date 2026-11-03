//! Tracked admission tests exercise real lower reservations and private facts.
//! Trusted fixture stores qualify completion without claiming disk durability;
//! product storage and actor lifecycle integration remain separate requirements.

use super::*;
use crate::messaging::{
    MessageAcceptanceStore, MessageAcceptanceStoreError, MessageAcceptanceWrite,
    MessageAcceptanceWriteOutcome, Recipient, SenderIdentity,
};

/// Supplies an explicit storage disposition through the real trusted port.
struct Store(bool);
impl MessageAcceptanceStore for Store {
    /// Returns a qualified refusal or success; never classifies a generic I/O error.
    fn commit(
        &mut self,
        _: &MessageAcceptanceWrite,
    ) -> std::result::Result<(), MessageAcceptanceStoreError> {
        if self.0 {
            Ok(())
        } else {
            Err(MessageAcceptanceStoreError::NotCommitted(
                MessageError::invalid_state("fixture refusal"),
            ))
        }
    }
}

/// Creates real authenticated actors and one immutable original native caller.
fn fixture() -> (
    MessageService,
    MessageRequests,
    MessageRequestOrigin,
    Envelope,
) {
    let mut service = MessageService::with_limits(8, 4096);
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "worker", vec![]);
    let origin = MessageRequestOrigin {
        agent_id: sender.agent_id.clone(),
        conversation_id: "original-conversation".into(),
        turn_id: "original-turn".into(),
        started_at_unix_seconds: 7,
    };
    let envelope = mail(sender, target);
    (service, MessageRequests::default(), origin, envelope)
}

/// Constructs a direct message without relying on serialized acceptance authority.
fn mail(sender: SenderIdentity, target: SenderIdentity) -> Envelope {
    Envelope {
        protocol: "mmp/1",
        id: "original-request".into(),
        message_type: "send".into(),
        time: "original-time".into(),
        sender,
        recipient: Recipient::Agent(target.agent_id),
        correlation_id: None,
        ttl_ms: Some(10),
        content_type: "text/plain".into(),
        payload: "original payload".into(),
        extension_fields: vec![],
    }
}

/// The caller and future handoff budget are retained before storage starts, yet
/// no accepted receipt appears until the matching positive fact is adopted. Even
/// conversation retirement and TTL maintenance cannot abandon an issued write;
/// later completion remains attributed to the original caller and publishes once.
#[test]
fn committed_request_keeps_original_caller_through_retirement_and_publishes_once() {
    let (mut service, mut requests, origin, envelope) = fixture();
    let ticket = requests
        .prepare_acceptance(
            &mut service,
            origin.clone(),
            envelope,
            MessageScope::Session,
            100,
        )
        .unwrap();
    let usage = requests.usage();
    assert_eq!(usage.0, 0);
    assert_eq!(usage.1, 4);
    assert!(usage.2 > MESSAGE_REQUEST_HANDOFF_BYTES);
    assert!(requests.receipts().next().is_none());
    let write = service.begin_acceptance_write(&ticket).unwrap();
    assert!(
        requests
            .cancel_prepared_acceptance(&mut service, &ticket)
            .is_err()
    );
    requests.retire_conversation(&origin.agent_id, &origin.conversation_id);
    assert!(requests.retire_expired_at(100_000, |_| false).is_empty());
    assert_eq!(requests.usage(), usage);
    let MessageAcceptanceWriteOutcome::Committed(commit) = write.persist(&mut Store(true)) else {
        panic!("expected commit")
    };
    assert!(
        requests
            .find(
                &origin.agent_id,
                &origin.conversation_id,
                "original-request"
            )
            .is_none()
    );
    let receipt = requests.publish_acceptance(&mut service, &commit).unwrap();
    let generation = receipt.generation();
    assert_eq!(receipt.origin(), &origin);
    assert_eq!(receipt.delivery().sequence, ticket.sequence());
    assert!(receipt.delivery().accepted);
    assert!(receipt.has_finite_expiry());
    assert_eq!(
        requests
            .publish_acceptance(&mut service, &commit)
            .unwrap()
            .generation(),
        generation
    );
    assert_eq!(requests.usage(), (1, usage.1, usage.2));
    assert!(requests.consume(generation, &origin));
    assert!(requests.publish_acceptance(&mut service, &commit).is_err());
    assert_eq!(requests.usage(), (0, 0, 0));
}

/// Private allocation evidence, rather than matching public IDs or caller text,
/// controls publication. A fact from an identically populated foreign owner
/// cannot mutate either pending reservation, and cancellation releases both.
#[test]
fn foreign_commit_does_not_publish_or_release_original_request() {
    let (mut service, mut requests, origin, envelope) = fixture();
    let ticket = requests
        .prepare_acceptance(&mut service, origin, envelope, MessageScope::Session, 1)
        .unwrap();
    let usage = requests.usage();
    let (mut foreign, mut foreign_requests, other_origin, other_envelope) = fixture();
    let other_ticket = foreign_requests
        .prepare_acceptance(
            &mut foreign,
            other_origin,
            other_envelope,
            MessageScope::Session,
            1,
        )
        .unwrap();
    let MessageAcceptanceWriteOutcome::Committed(commit) = foreign
        .begin_acceptance_write(&other_ticket)
        .unwrap()
        .persist(&mut Store(true))
    else {
        panic!("expected commit")
    };
    assert!(requests.publish_acceptance(&mut service, &commit).is_err());
    assert!(service.has_unpublished_acceptance());
    assert!(foreign.has_unpublished_acceptance());
    assert_eq!(requests.usage(), usage);
    requests
        .cancel_prepared_acceptance(&mut service, &ticket)
        .unwrap();
    assert_eq!(requests.usage(), (0, 0, 0));
    assert!(!service.has_unpublished_acceptance());
    assert!(service.begin_acceptance_write(&ticket).is_err());
    foreign_requests
        .publish_acceptance(&mut foreign, &commit)
        .unwrap();
    assert!(
        foreign_requests
            .publish_acceptance(&mut service, &commit)
            .is_err()
    );
}

/// A qualified refusal releases exactly the original request reservation and
/// lower custody, while another owner's refusal cannot free its resources.
/// Subsequent admission gets a fresh dependency generation with no receipt leak.
#[test]
fn exact_noncommit_releases_both_owners_without_reusing_generation() {
    let (mut service, mut requests, origin, envelope) = fixture();
    let ticket = requests
        .prepare_acceptance(
            &mut service,
            origin.clone(),
            envelope.clone(),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let (mut foreign, mut foreign_requests, other_origin, other_envelope) = fixture();
    let other_ticket = foreign_requests
        .prepare_acceptance(
            &mut foreign,
            other_origin,
            other_envelope,
            MessageScope::Session,
            1,
        )
        .unwrap();
    let MessageAcceptanceWriteOutcome::NotCommitted(other) = foreign
        .begin_acceptance_write(&other_ticket)
        .unwrap()
        .persist(&mut Store(false))
    else {
        panic!("expected refusal")
    };
    assert!(
        requests
            .release_rejected_acceptance(&mut service, &other)
            .is_err()
    );
    let MessageAcceptanceWriteOutcome::NotCommitted(rejected) = service
        .begin_acceptance_write(&ticket)
        .unwrap()
        .persist(&mut Store(false))
    else {
        panic!("expected refusal")
    };
    requests
        .release_rejected_acceptance(&mut service, &rejected)
        .unwrap();
    assert_eq!(requests.usage(), (0, 0, 0));
    assert!(!service.has_unpublished_acceptance());
    assert!(
        requests
            .release_rejected_acceptance(&mut service, &rejected)
            .is_err()
    );
    let next = requests
        .prepare_acceptance(&mut service, origin, envelope, MessageScope::Session, 2)
        .unwrap();
    let MessageAcceptanceWriteOutcome::Committed(commit) = service
        .begin_acceptance_write(&next)
        .unwrap()
        .persist(&mut Store(true))
    else {
        panic!("expected commit")
    };
    assert_eq!(
        requests
            .publish_acceptance(&mut service, &commit)
            .unwrap()
            .generation(),
        2
    );
}

/// Invalid caller identity and insufficient future handoff capacity reject before
/// lower reservation. A valid prepared request also rejects a second admission
/// without changing the retained original or its fully charged budget.
#[test]
fn preflight_failure_and_second_admission_leave_original_owners_intact() {
    let (mut service, mut requests, origin, envelope) = fixture();
    let before = service.snapshot_state().unwrap();
    let mut invalid = origin.clone();
    invalid.turn_id.clear();
    assert!(
        requests
            .prepare_acceptance(
                &mut service,
                invalid,
                envelope.clone(),
                MessageScope::Session,
                1
            )
            .is_err()
    );
    let mut small = MessageRequests::with_limits(MessageRequestLimits {
        metadata_bytes: 1000,
        ..MessageRequestLimits::default()
    })
    .unwrap();
    assert!(
        small
            .prepare_acceptance(
                &mut service,
                origin.clone(),
                envelope.clone(),
                MessageScope::Session,
                1
            )
            .is_err()
    );
    assert_eq!(small.usage(), (0, 0, 0));
    assert_eq!(service.snapshot_state().unwrap(), before);
    let ticket = requests
        .prepare_acceptance(
            &mut service,
            origin.clone(),
            envelope.clone(),
            MessageScope::Session,
            1,
        )
        .unwrap();
    let usage = requests.usage();
    assert!(
        requests
            .prepare_acceptance(
                &mut service,
                origin.clone(),
                envelope.clone(),
                MessageScope::Session,
                2
            )
            .is_err()
    );
    assert!(
        requests
            .accept(&mut service, origin, envelope, MessageScope::Session, 2)
            .is_err()
    );
    assert_eq!(requests.usage(), usage);
    requests
        .cancel_prepared_acceptance(&mut service, &ticket)
        .unwrap();
    assert_eq!(requests.usage(), (0, 0, 0));
}

/// An ambiguous result leaves request and message capacity charged across caller
/// teardown. Only owner-qualified reconciliation of the same immutable write can
/// later publish it; original TTL and caller identity are never renewed on retry.
#[test]
fn uncertain_write_retains_original_request_and_deadline_through_reconciliation() {
    /// Qualifies uncertainty without inventing nonexecution from a diagnostic.
    struct UncertainStore;
    impl MessageAcceptanceStore for UncertainStore {
        /// Models a joined store call whose commit outcome is not yet known.
        fn commit(
            &mut self,
            _: &MessageAcceptanceWrite,
        ) -> std::result::Result<(), MessageAcceptanceStoreError> {
            Err(MessageAcceptanceStoreError::Unknown(
                MessageError::invalid_state("lost result"),
            ))
        }
    }
    let (mut service, mut requests, origin, envelope) = fixture();
    let ticket = requests
        .prepare_acceptance(
            &mut service,
            origin.clone(),
            envelope,
            MessageScope::Session,
            100,
        )
        .unwrap();
    let usage = requests.usage();
    let MessageAcceptanceWriteOutcome::Unknown(unknown) = service
        .begin_acceptance_write(&ticket)
        .unwrap()
        .persist(&mut UncertainStore)
    else {
        panic!("expected unknown")
    };
    assert!(
        requests
            .cancel_prepared_acceptance(&mut service, &ticket)
            .is_err()
    );
    requests.retire_conversation(&origin.agent_id, &origin.conversation_id);
    assert_eq!(requests.usage(), usage);
    assert!(requests.receipts().next().is_none());
    let retry = service.retry_acceptance_write(unknown).unwrap();
    assert_eq!(retry.sequence(), ticket.sequence());
    let MessageAcceptanceWriteOutcome::Committed(commit) = retry.persist(&mut Store(true)) else {
        panic!("expected commit")
    };
    let receipt = requests.publish_acceptance(&mut service, &commit).unwrap();
    assert_eq!(receipt.origin(), &origin);
    assert_eq!(receipt.expires_at_ms, Some(110));
    assert_eq!(requests.usage(), (1, usage.1, usage.2));
}

/// A delayed cancellation from an earlier admission cannot cancel a newer one,
/// even with identical public caller and message fields. Exact private tickets
/// fence both request accounting and the underlying unpublished occurrence.
#[test]
fn stale_cancellation_cannot_release_replacement_reservation() {
    let (mut service, mut requests, origin, envelope) = fixture();
    let old = requests
        .prepare_acceptance(
            &mut service,
            origin.clone(),
            envelope.clone(),
            MessageScope::Session,
            1,
        )
        .unwrap();
    requests
        .cancel_prepared_acceptance(&mut service, &old)
        .unwrap();
    let current = requests
        .prepare_acceptance(&mut service, origin, envelope, MessageScope::Session, 2)
        .unwrap();
    let usage = requests.usage();
    assert!(
        requests
            .cancel_prepared_acceptance(&mut service, &old)
            .is_err()
    );
    assert_eq!(requests.usage(), usage);
    assert!(service.has_unpublished_acceptance());
    requests
        .cancel_prepared_acceptance(&mut service, &current)
        .unwrap();
    assert_eq!(requests.usage(), (0, 0, 0));
}

/// Receipt lookup must follow the actual prepared allocation rather than equal
/// public actor/message/origin values. Consumption removes lookup authority and
/// cannot let the original ticket recreate a dependency receipt or send again.
#[test]
fn published_receipt_lookup_requires_original_admission_and_ends_on_consumption() {
    let (mut service, mut requests, origin, envelope) = fixture();
    let mut foreign_service =
        MessageService::from_snapshot_state(&service.snapshot_state().unwrap()).unwrap();
    let mut foreign_requests = MessageRequests::default();
    let foreign = foreign_requests
        .prepare_acceptance(
            &mut foreign_service,
            origin.clone(),
            envelope.clone(),
            MessageScope::Session,
            10,
        )
        .unwrap();
    let ticket = requests
        .prepare_acceptance(
            &mut service,
            origin.clone(),
            envelope,
            MessageScope::Session,
            10,
        )
        .unwrap();
    assert!(requests.receipt_for_admission(&ticket).is_none());
    let MessageAcceptanceWriteOutcome::Committed(commit) = service
        .begin_acceptance_write(&ticket)
        .unwrap()
        .persist(&mut Store(true))
    else {
        panic!("actual port must qualify")
    };
    let generation = requests
        .publish_acceptance(&mut service, &commit)
        .unwrap()
        .generation();
    assert_eq!(
        requests
            .receipt_for_admission(&ticket)
            .unwrap()
            .generation(),
        generation
    );
    assert!(requests.receipt_for_admission(&foreign).is_none());
    assert!(foreign_requests.receipt_for_admission(&ticket).is_none());
    assert!(requests.consume(generation, &origin));
    assert!(requests.receipt_for_admission(&ticket).is_none());
    foreign_requests
        .cancel_prepared_acceptance(&mut foreign_service, &foreign)
        .unwrap();
}
