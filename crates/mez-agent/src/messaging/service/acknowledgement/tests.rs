//! ACK reservation tests use the real offer/pending/terminal owner. Fixture
//! stores qualify only the typed boundary; real disk qualification lives in the
//! product adapter. No fixture result claims native application commit authority.

use super::*;

/// Configurable trusted completion; error always means uncertainty, not refusal.
struct Store(bool);
impl MessageAcknowledgementStore for Store {
    /// Returns actual fixture success or a retained diagnostic uncertainty.
    fn commit(&mut self, _: &MessageAcknowledgementWrite) -> Result<()> {
        if self.0 {
            Ok(())
        } else {
            Err(MessageError::invalid_state("fixture uncertain"))
        }
    }
}

/// Builds real actors and a canonical subscribed recipient.
fn fixture() -> (MessageService, SenderIdentity, SenderIdentity) {
    let mut service = MessageService::with_limits(1, 4096);
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "worker", vec![]);
    service.subscribe(&target.agent_id).unwrap();
    (service, sender, target)
}
/// Creates immutable direct mail with a small payload, not an ACK capability.
fn mail(sender: &SenderIdentity, target: &SenderIdentity, id: &str) -> Envelope {
    Envelope {
        protocol: "mmp/1",
        id: id.into(),
        message_type: "send".into(),
        time: "original".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "original payload".into(),
        extension_fields: vec![],
    }
}
/// Accepts and genuinely offers an original without acknowledging it.
fn offer(
    service: &mut MessageService,
    sender: &SenderIdentity,
    target: &SenderIdentity,
    id: &str,
) -> MessageSequence {
    let sequence = service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(sender, target, id),
            MessageScope::Session,
            1,
        )
        .unwrap()
        .sequence;
    service
        .receive_subscribed(&target.agent_id, 2, 100)
        .unwrap();
    sequence
}

/// A cumulative ACK may cover an earlier offer made before its TTL expired and
/// a later endpoint offered at a different clock. Durable framing must retain
/// both exact clocks; the endpoint clock alone cannot reconstruct the earlier
/// offer after a crash without inventing selection evidence.
#[test]
fn durable_ack_record_preserves_each_original_offer_clock() {
    let mut service = MessageService::with_limits(8, 4096);
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service.subscribe(&target.agent_id).unwrap();
    let mut first = mail(&sender, &target, "first");
    first.ttl_ms = Some(10);
    assert_eq!(
        service
            .accept_at_with_scope(&sender.agent_id, first, MessageScope::Session, 1)
            .unwrap()
            .sequence,
        1
    );
    assert_eq!(
        service
            .receive_subscribed(&target.agent_id, 10, 8)
            .unwrap()
            .messages
            .len(),
        1
    );
    assert_eq!(
        service
            .accept_at_with_scope(
                &sender.agent_id,
                mail(&sender, &target, "second"),
                MessageScope::Session,
                1
            )
            .unwrap()
            .sequence,
        2
    );
    let selected = service.receive_subscribed(&target.agent_id, 20, 8).unwrap();
    assert_eq!(selected.messages.len(), 1);
    assert_eq!(selected.messages[0].sequence, 2);
    let ticket = service
        .prepare_acknowledgement(&target.agent_id, 2, MAX_METADATA)
        .unwrap()
        .unwrap();
    let write = service.begin_acknowledgement_write(&ticket).unwrap();
    let mut encoded = Vec::new();
    write.write_record(&mut encoded).unwrap();
    assert_eq!(
        &encoded[..8],
        b"MMPACK02",
        "durable ACK must retain every selected offer clock"
    );
    let start = 48 + target.agent_id.as_str().len();
    assert_eq!(encoded.len(), start + 2 * 53);
    assert_eq!(
        u64::from_le_bytes(encoded[start + 13..start + 21].try_into().unwrap()),
        10
    );
    assert_eq!(
        u64::from_le_bytes(
            encoded[start + 53 + 13..start + 53 + 21]
                .try_into()
                .unwrap()
        ),
        20
    );
}

/// A subsequent delivery selection may update live offer evidence while an
/// acknowledgement is reserved. Its durable record must use the original
/// reservation's clock, including when the worker starts after the reoffer.
#[test]
fn durable_ack_record_freezes_offer_clock_at_reservation() {
    let (mut service, sender, target) = fixture();
    let sequence = offer(&mut service, &sender, &target, "original");
    let ticket = service
        .prepare_acknowledgement(&target.agent_id, sequence, MAX_METADATA)
        .unwrap()
        .unwrap();
    assert_eq!(
        service
            .receive_subscribed(&target.agent_id, 20, 100)
            .unwrap()
            .messages
            .len(),
        1
    );
    let write = service.begin_acknowledgement_write(&ticket).unwrap();
    let mut encoded = Vec::new();
    write.write_record(&mut encoded).unwrap();
    let start = 48 + target.agent_id.as_str().len();
    assert_eq!(u64::from_le_bytes(encoded[32..40].try_into().unwrap()), 2);
    assert_eq!(
        u64::from_le_bytes(encoded[start + 13..start + 21].try_into().unwrap()),
        2
    );
}

/// Reserving and physically committing an ACK do not free custody until actor
/// adoption. Direct same-recipient ACKs cannot bypass it, and terminal retirement
/// cannot steal its slots while a real independently gated worker is running.
#[test]
fn original_ack_stays_pending_until_actual_worker_completion_and_actor_adoption() {
    let (mut service, sender, target) = fixture();
    let sequence = offer(&mut service, &sender, &target, "original");
    let ticket = service
        .prepare_acknowledgement(&target.agent_id, sequence, MAX_METADATA)
        .unwrap()
        .unwrap();
    assert!(ticket.metadata_bytes() > 48);
    let weak_source = Arc::downgrade(service.pending.get(&sequence).unwrap());
    let before = service.snapshot_state().unwrap();
    let write = service.begin_acknowledgement_write(&ticket).unwrap();
    let gate = Arc::new(std::sync::Barrier::new(2));
    let worker_gate = gate.clone();
    let worker = std::thread::spawn(move || {
        worker_gate.wait();
        write.persist(&mut Store(true))
    });
    assert_eq!(service.snapshot_state().unwrap(), before);
    assert!(
        service
            .advance_subscription(&target.agent_id, sequence)
            .is_err()
    );
    service.retire_agent_identity(&target.agent_id);
    assert_eq!(service.settle_delivery_outcomes_at(100).unwrap(), 0);
    gate.wait();
    let MessageAcknowledgementWriteOutcome::Committed(commit) = worker.join().unwrap() else {
        panic!("expected commit")
    };
    assert_eq!(service.pending.len(), 1);
    let cursor = service.publish_acknowledgement(&commit).unwrap();
    assert_eq!(cursor.last_sequence, sequence);
    assert!(service.subscription(&target.agent_id).is_none());
    assert!(service.pending.is_empty());
    assert_eq!(service.publish_acknowledgement(&commit).unwrap(), cursor);
    let fresh = service.register_agent(None, None, "fresh", vec![]);
    service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &fresh, "evict-old-replay"),
            MessageScope::Session,
            101,
        )
        .unwrap();
    assert!(
        weak_source.upgrade().is_none(),
        "payload-free ACK facts retained an evicted source"
    );
    assert_eq!(service.publish_acknowledgement(&commit).unwrap(), cursor);
}

/// Foreign services and later registration intervals cannot adopt old facts.
/// The original occurrence may commit after retirement, while its replacement
/// subscription and fresh pending mail remain unchanged by duplicate completion.
#[test]
fn committed_original_never_advances_replacement_recipient_or_replays_evicted_mail() {
    let (mut service, sender, target) = fixture();
    let sequence = offer(&mut service, &sender, &target, "original");
    let ticket = service
        .prepare_acknowledgement(&target.agent_id, sequence, MAX_METADATA)
        .unwrap()
        .unwrap();
    let write = service.begin_acknowledgement_write(&ticket).unwrap();
    let MessageAcknowledgementWriteOutcome::Committed(commit) = write.persist(&mut Store(true))
    else {
        panic!("expected commit")
    };
    let mut foreign =
        MessageService::from_snapshot_state(&service.snapshot_state().unwrap()).unwrap();
    assert!(foreign.publish_acknowledgement(&commit).is_err());
    service.retire_agent_identity(&target.agent_id);
    service.ensure_agent_identity(target.clone(), 3).unwrap();
    let replacement = service
        .subscribe_from_retained_start(&target.agent_id)
        .unwrap();
    assert_eq!(service.settle_delivery_outcomes_at(4).unwrap(), 0);
    service.publish_acknowledgement(&commit).unwrap();
    assert_eq!(service.subscription(&target.agent_id), Some(&replacement));
    let fresh = service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &target, "replacement-mail"),
            MessageScope::Session,
            5,
        )
        .unwrap();
    service.publish_acknowledgement(&commit).unwrap();
    assert_eq!(service.subscription(&target.agent_id), Some(&replacement));
    assert!(service.pending.contains_key(&fresh.sequence));
    service.receive_subscribed(&target.agent_id, 6, 10).unwrap();
    let current = service
        .prepare_acknowledgement(&target.agent_id, fresh.sequence, MAX_METADATA)
        .unwrap()
        .unwrap();
    service.publish_acknowledgement(&commit).unwrap();
    service.cancel_acknowledgement(&current).unwrap();
}

/// Unknown results retain identical encoded source and cannot be cancelled.
/// Owner retry creates a fresh nonce; stale joined escrow fails before its
/// callback, and foreign validation cannot consume the retained linear original.
#[test]
fn uncertainty_keeps_original_record_and_fences_stale_or_foreign_attempts() {
    /// Counts whether stale escrow reached an authority callback at all.
    struct Joined(usize);
    impl MessageAcknowledgementJoin for Joined {
        /// Marks an explicitly invoked fixture join qualification.
        fn qualify_joined(&mut self, _: &MessageAcknowledgementAttempt) -> Result<()> {
            self.0 += 1;
            Ok(())
        }
    }
    let (mut service, sender, target) = fixture();
    let sequence = offer(&mut service, &sender, &target, "original");
    let ticket = service
        .prepare_acknowledgement(&target.agent_id, sequence, MAX_METADATA)
        .unwrap()
        .unwrap();
    let write = service.begin_acknowledgement_write(&ticket).unwrap();
    assert!(
        !format!("{write:?}").contains("original payload"),
        "ACK debug exposed retained payload"
    );
    let old = write.attempt();
    let mut bytes = Vec::new();
    write.write_record(&mut bytes).unwrap();
    assert_eq!(bytes.len(), write.encoded_bytes());
    let MessageAcknowledgementWriteOutcome::Unknown(unknown) = write.persist(&mut Store(false))
    else {
        panic!("expected unknown")
    };
    assert!(service.cancel_acknowledgement(&ticket).is_err());
    let foreign = MessageService::default();
    assert!(foreign.validate_acknowledgement_attempt(&old).is_err());
    let retry = service.retry_acknowledgement_write(unknown).unwrap();
    assert!(!retry.attempt().same_attempt(&old));
    let mut original = Vec::new();
    retry.write_record(&mut original).unwrap();
    assert_eq!(original, bytes);
    let mut joined = Joined(0);
    assert!(
        service
            .recover_joined_acknowledgement_write(&old, &mut joined)
            .is_err()
    );
    assert_eq!(joined.0, 0);
    let MessageAcknowledgementWriteOutcome::Committed(commit) = retry.persist(&mut Store(true))
    else {
        panic!("expected commit")
    };
    service.publish_acknowledgement(&commit).unwrap();
}

/// Unoffered endpoints and insufficient metadata reject without any state
/// mutation. Cancelling a never-issued reservation restores ordinary ACK use;
/// its stale ticket cannot cancel a subsequent original with equal public data.
#[test]
fn invalid_budget_and_stale_cancel_preserve_both_ledger_and_original_reservation() {
    let (mut service, sender, target) = fixture();
    let sequence = service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &target, "original"),
            MessageScope::Session,
            1,
        )
        .unwrap()
        .sequence;
    let before = service.snapshot_state().unwrap();
    assert!(
        service
            .prepare_acknowledgement(&target.agent_id, sequence, MAX_METADATA)
            .is_err()
    );
    assert_eq!(service.snapshot_state().unwrap(), before);
    service.receive_subscribed(&target.agent_id, 2, 1).unwrap();
    let before = service.snapshot_state().unwrap();
    assert!(
        service
            .prepare_acknowledgement(&target.agent_id, sequence, 1)
            .is_err()
    );
    assert_eq!(service.snapshot_state().unwrap(), before);
    let first = service
        .prepare_acknowledgement(&target.agent_id, sequence, MAX_METADATA)
        .unwrap()
        .unwrap();
    service.cancel_acknowledgement(&first).unwrap();
    let current = service
        .prepare_acknowledgement(&target.agent_id, sequence, MAX_METADATA)
        .unwrap()
        .unwrap();
    assert!(service.cancel_acknowledgement(&first).is_err());
    service.cancel_acknowledgement(&current).unwrap();
    service
        .advance_subscription(&target.agent_id, sequence)
        .unwrap();
    assert!(
        service
            .prepare_acknowledgement(&target.agent_id, sequence, 0)
            .unwrap()
            .is_none()
    );
}

/// A later offered endpoint can exclude an earlier actually expired unoffered
/// prefix. Its frozen clock prevents new rollback offers during storage, and
/// adoption records an expiry exclusion rather than fabricating recipient ACK.
#[test]
fn frozen_expiry_exclusion_cannot_be_reoffered_on_clock_rollback_or_become_ack() {
    let (mut service, sender, target) = fixture();
    let mut expired_mail = mail(&sender, &target, "expired");
    expired_mail.ttl_ms = Some(5);
    let first = service
        .accept_at_with_scope(&sender.agent_id, expired_mail, MessageScope::Session, 1)
        .unwrap()
        .sequence;
    let second = service
        .accept_at_with_scope(
            &sender.agent_id,
            mail(&sender, &target, "endpoint"),
            MessageScope::Session,
            2,
        )
        .unwrap()
        .sequence;
    service
        .receive_after(
            &DeliveryCursor {
                recipient: target.agent_id.clone(),
                last_sequence: first,
            },
            20,
            1,
        )
        .unwrap();
    let ticket = service
        .prepare_acknowledgement(&target.agent_id, second, MAX_METADATA)
        .unwrap()
        .unwrap();
    let rollback = service
        .receive_subscribed(&target.agent_id, 2, 100)
        .unwrap();
    assert!(
        rollback
            .messages
            .iter()
            .all(|message| message.sequence != first)
    );
    let write = service.begin_acknowledgement_write(&ticket).unwrap();
    let entries: Vec<_> = write.entries().collect();
    assert!(!entries[0].acknowledged);
    assert!(entries[1].acknowledged);
    let MessageAcknowledgementWriteOutcome::Committed(commit) = write.persist(&mut Store(true))
    else {
        panic!("expected commit")
    };
    service.publish_acknowledgement(&commit).unwrap();
    let first = service.pending.get(&first).unwrap();
    assert!(!first.acknowledged.lock().unwrap()[0]);
    assert_eq!(*first.ack_expiry_observed_at_ms.lock().unwrap(), Some(20));
    MessageService::from_snapshot_state(&service.snapshot_state().unwrap()).unwrap();
}

/// An ACK reservation protects only its own target slots while another accepted
/// recipient can still drain. Shared fanout payload remains charged until the
/// final original recipient's durable ACK is positively adopted.
#[test]
fn independent_recipient_drains_while_original_ack_is_reserved() {
    let (mut service, sender, target) = fixture();
    let other = service.register_agent(None, None, "worker", vec![]);
    service.subscribe(&other.agent_id).unwrap();
    let mut input = mail(&sender, &target, "fanout");
    input.recipient = Recipient::Role("worker".into());
    let sequence = service
        .accept_at_with_scope(&sender.agent_id, input, MessageScope::Session, 1)
        .unwrap()
        .sequence;
    service.receive_subscribed(&target.agent_id, 2, 1).unwrap();
    service.receive_subscribed(&other.agent_id, 2, 1).unwrap();
    let ticket = service
        .prepare_acknowledgement(&target.agent_id, sequence, MAX_METADATA)
        .unwrap()
        .unwrap();
    service
        .advance_subscription(&other.agent_id, sequence)
        .unwrap();
    assert_eq!(service.pending.len(), 1);
    let write = service.begin_acknowledgement_write(&ticket).unwrap();
    let MessageAcknowledgementWriteOutcome::Committed(commit) = write.persist(&mut Store(true))
    else {
        panic!("expected commit")
    };
    service.publish_acknowledgement(&commit).unwrap();
    assert!(service.pending.is_empty());
}
