//! Storage-scale logical custody with a deliberately smaller allocation cache.
//!
//! Exact lower-issued originals use a synchronous test port here; real SQLite
//! and actor coverage live in the product. No test constructs a receipt or
//! fingerprint directly, and accepted mail is never deleted to satisfy pressure.

use super::super::payload_load::{MessagePayloadLoadAttempt, MessagePayloadLoadJoin};
use super::*;

mod elapsed_clock;

/// Retains encoded originals and actual ACK records for the trusted unit port.
#[derive(Default)]
struct Store {
    originals: HashMap<MessageSequence, Vec<u8>>,
    acks: Vec<Vec<u8>>,
    fail_read: bool,
}
impl MessageAcceptanceStore for Store {
    fn commit(
        &mut self,
        record: &MessageAcceptanceWrite,
    ) -> std::result::Result<(), MessageAcceptanceStoreError> {
        self.originals
            .insert(record.sequence(), serde_json::to_vec(record).unwrap());
        Ok(())
    }
}
impl MessagePayloadLoadStore for Store {
    fn load(&mut self, request: &MessagePayloadLoadWrite) -> Result<MessageAcceptanceSource> {
        if self.fail_read {
            return Err(MessageError::invalid_state(
                "injected original read failure",
            ));
        }
        decode_message_acceptance(
            self.originals[&request.sequence()].as_slice(),
            request.decode_limits(),
        )
    }
}
impl MessageAcknowledgementStore for Store {
    fn commit(&mut self, record: &MessageAcknowledgementWrite) -> Result<()> {
        let mut bytes = Vec::new();
        record
            .write_record(&mut bytes)
            .map_err(|_| MessageError::invalid_state("ACK encoding failed"))?;
        self.acks.push(bytes);
        Ok(())
    }
}

/// Represents the actual completed synchronous test load, not an error label.
struct Joined(MessagePayloadLoadAttempt);
impl MessagePayloadLoadJoin for Joined {
    fn qualify_joined(&mut self, attempt: &MessagePayloadLoadAttempt) -> Result<()> {
        if self.0.same_attempt(attempt) {
            Ok(())
        } else {
            Err(MessageError::invalid_state("wrong actual read"))
        }
    }
}

/// Narrows logical and cache budgets independently; fixed metadata headroom is
/// still reserved under the real production ceiling.
fn limits() -> MessageStorageLimits {
    MessageStorageLimits {
        messages: 8,
        payload_bytes: 1024,
        recipient_obligations: 16,
        per_recipient: 8,
        per_recipient_payload_bytes: 1024,
        cache_messages: 4,
        cache_payload_bytes: 64,
        ..Default::default()
    }
}

/// Builds an empty original owner, then enables its nonserialized root policy.
fn fixture(
    policy: MessageStorageLimits,
) -> (
    MessageService,
    MessageStorageScope,
    SenderIdentity,
    SenderIdentity,
    Store,
) {
    let mut service = MessageService::with_limits(1, 1024);
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service.subscribe(&target.agent_id).unwrap();
    let scope = MessageStorageScope::default();
    service.enable_storage_custody(&scope, policy).unwrap();
    (service, scope, sender, target, Store::default())
}

/// Uses complete authenticated input without manufacturing a canonical source.
fn mail(sender: &SenderIdentity, target: &SenderIdentity, id: &str, bytes: usize) -> Envelope {
    Envelope {
        protocol: MMP_PROTOCOL,
        id: id.into(),
        message_type: "send".into(),
        time: "original".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "x".repeat(bytes),
        extension_fields: vec![],
    }
}

/// Publishes only a positive result from the exact original storage issuance.
fn accept(
    service: &mut MessageService,
    scope: &MessageStorageScope,
    store: &mut Store,
    envelope: Envelope,
) -> Delivery {
    let ticket = service
        .prepare_acceptance(
            &envelope.sender.agent_id.clone(),
            envelope,
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    let write = service
        .begin_storage_acceptance_write(&ticket, scope)
        .unwrap();
    let MessageAcceptanceWriteOutcome::Committed(commit) = write.persist(store) else {
        panic!("test port must commit")
    };
    service.publish_acceptance(&commit).unwrap().0
}

/// Commits only the real offered prefix through a storage-qualified ACK record.
fn acknowledge(
    service: &mut MessageService,
    scope: &MessageStorageScope,
    store: &mut Store,
    target: &SenderIdentity,
    sequence: MessageSequence,
) {
    let ticket = service
        .prepare_acknowledgement(&target.agent_id, sequence, 512 * 1024)
        .unwrap()
        .unwrap();
    let write = service
        .begin_storage_acknowledgement_write(&ticket, scope)
        .unwrap();
    let MessageAcknowledgementWriteOutcome::Committed(commit) = write.persist(store) else {
        panic!("test port must commit")
    };
    service.publish_acknowledgement(&commit).unwrap();
}

/// Two unread bodies exceed the 64-byte cache but fit logical custody. Reload
/// and ACK preserve exact order; an external batch still pins bytes after its
/// canonical row is settled/evicted, preventing another allocation until drop.
#[test]
fn logical_spill_reload_and_external_pins_share_one_bounded_cache() {
    let policy = limits();
    let (mut service, scope, sender, target, mut store) = fixture(policy);
    let first = accept(
        &mut service,
        &scope,
        &mut store,
        mail(&sender, &target, "first", 40),
    );
    let second = accept(
        &mut service,
        &scope,
        &mut store,
        mail(&sender, &target, "second", 40),
    );
    let usage = service.storage_usage().unwrap().unwrap();
    assert_eq!(
        (usage.pending_messages, usage.pending_payload_bytes),
        (2, 80)
    );
    assert_eq!(usage.cache.live_payload_bytes, 40);
    assert!(usage.metadata_bytes <= policy.metadata_bytes);
    assert_eq!(
        service.payload_needed_for(&target.agent_id, 11).unwrap(),
        Some(first.sequence)
    );
    assert!(
        service
            .receive_subscribed(&target.agent_id, 11, 64)
            .unwrap()
            .messages
            .is_empty()
    );
    let write = service
        .begin_payload_load(first.sequence, &scope)
        .unwrap()
        .unwrap();
    assert_eq!(
        service
            .payload_observer()
            .unwrap()
            .snapshot()
            .unwrap()
            .reserved_payload_bytes,
        40
    );
    let mut result = write.load(&mut store);
    let observer = service.payload_observer().unwrap();
    assert_eq!(observer.snapshot().unwrap().live_payload_bytes, 40);
    assert!(service.publish_payload_load(&mut result).unwrap());
    let batch = service
        .receive_subscribed(&target.agent_id, 11, 64)
        .unwrap();
    assert_eq!(
        batch.messages.len(),
        1,
        "resident prefix must not be discarded at the next cold row"
    );
    assert_eq!(batch.messages[0].envelope.id, "first");
    assert!(
        service
            .advance_subscription(&target.agent_id, first.sequence)
            .is_err()
    );
    acknowledge(&mut service, &scope, &mut store, &target, first.sequence);
    assert!(!service.queued_by_sequence.contains_key(&first.sequence));
    assert_eq!(observer.snapshot().unwrap().live_payload_bytes, 40);
    assert!(service.begin_payload_load(second.sequence, &scope).is_err());
    drop(batch);
    let mut result = service
        .begin_payload_load(second.sequence, &scope)
        .unwrap()
        .unwrap()
        .load(&mut store);
    assert!(service.publish_payload_load(&mut result).unwrap());
    let batch = service
        .receive_subscribed(&target.agent_id, 12, 64)
        .unwrap();
    assert_eq!(batch.messages[0].envelope.id, "second");
    acknowledge(&mut service, &scope, &mut store, &target, second.sequence);
    drop(batch);
    let usage = service.storage_usage().unwrap().unwrap();
    assert_eq!(usage.pending_messages, 0);
    assert!(usage.cache.live_payload_bytes + usage.cache.reserved_payload_bytes <= 64);
    assert_eq!(store.acks.len(), 2);
}

/// Decoder capacity survives lost input via payload-free actual-worker escrow.
/// Qualified joined recovery retires the old unbound reservation exactly once,
/// then reserves the same original size without double charging or new mail.
#[test]
fn lost_read_reservation_stays_charged_until_exact_joined_recovery() {
    let (mut service, scope, sender, target, mut store) = fixture(limits());
    let first = accept(
        &mut service,
        &scope,
        &mut store,
        mail(&sender, &target, "first", 40),
    );
    accept(
        &mut service,
        &scope,
        &mut store,
        mail(&sender, &target, "second", 40),
    );
    let write = service
        .begin_payload_load(first.sequence, &scope)
        .unwrap()
        .unwrap();
    let attempt = write.attempt();
    store.fail_read = true;
    drop(write.load(&mut store));
    let observer = service.payload_observer().unwrap();
    assert_eq!(observer.snapshot().unwrap().reserved_payload_bytes, 40);
    assert!(
        service
            .prepare_acceptance(
                &sender.agent_id,
                mail(&sender, &target, "third", 40),
                MessageScope::Session,
                11,
                4096,
                None
            )
            .is_err()
    );
    assert!(
        MessageService::default()
            .recover_payload_load(&attempt, &mut Joined(attempt.clone()))
            .is_err()
    );
    assert_eq!(observer.snapshot().unwrap().reserved_payload_bytes, 40);
    let write = service
        .recover_payload_load(&attempt, &mut Joined(attempt.clone()))
        .unwrap()
        .unwrap();
    assert_eq!(observer.snapshot().unwrap().reserved_payload_bytes, 40);
    store.fail_read = false;
    let mut result = write.load(&mut store);
    assert!(service.publish_payload_load(&mut result).unwrap());
    assert_eq!(observer.snapshot().unwrap().reserved_payload_bytes, 0);
    assert_eq!(observer.snapshot().unwrap().live_payload_bytes, 40);
    assert_eq!(service.last_sequence(), 2);
}

/// Scoped ACK fingerprints remain usable after cache eviction. A generic ACK
/// port cannot bypass storage-mode commit, and a foreign scope cannot issue or
/// consume its reservation even when the immutable occurrence IDs match.
#[test]
fn cold_offered_source_ack_requires_original_root_fingerprint() {
    let (mut service, scope, sender, target, mut store) = fixture(limits());
    let first = accept(
        &mut service,
        &scope,
        &mut store,
        mail(&sender, &target, "first", 40),
    );
    drop(service.receive_subscribed(&target.agent_id, 11, 1).unwrap());
    accept(
        &mut service,
        &scope,
        &mut store,
        mail(&sender, &target, "second", 40),
    );
    assert!(
        service.queued_by_sequence[&first.sequence]
            .envelope
            .resident()
            .is_err()
    );
    let ticket = service
        .prepare_acknowledgement(&target.agent_id, first.sequence, 512 * 1024)
        .unwrap()
        .unwrap();
    assert!(service.begin_acknowledgement_write(&ticket).is_err());
    assert!(
        service
            .begin_storage_acknowledgement_write(&ticket, &MessageStorageScope::default())
            .is_err()
    );
    let write = service
        .begin_storage_acknowledgement_write(&ticket, &scope)
        .unwrap();
    let MessageAcknowledgementWriteOutcome::Committed(commit) = write.persist(&mut store) else {
        panic!("cold ACK must encode its qualified source hash")
    };
    service.publish_acknowledgement(&commit).unwrap();
    assert_eq!(
        service
            .subscription(&target.agent_id)
            .unwrap()
            .last_sequence,
        first.sequence
    );
}

/// Orphan external headers count against metadata even for an empty body. Cache
/// release cannot hide them; after the last external pin drops, dead history is
/// reclaimable and the same unaccepted candidate can fit without quota inflation.
#[test]
fn external_header_after_row_retirement_remains_in_metadata_budget() {
    let mut policy = limits();
    policy.metadata_bytes = WORKER_HEADROOM + 448 * 1024;
    let (mut service, scope, sender, target, mut store) = fixture(policy);
    let mut original = mail(&sender, &target, "first", 0);
    original.time = "t".repeat(70_000);
    let first = accept(&mut service, &scope, &mut store, original);
    let external = service.receive_subscribed(&target.agent_id, 11, 1).unwrap();
    acknowledge(&mut service, &scope, &mut store, &target, first.sequence);
    let mut next = mail(&sender, &target, "second", 0);
    next.time = "t".repeat(70_000);
    assert!(
        service
            .prepare_acceptance(
                &sender.agent_id,
                next.clone(),
                MessageScope::Session,
                12,
                4096,
                None
            )
            .is_err()
    );
    assert_eq!(service.last_sequence(), first.sequence);
    drop(external);
    accept(&mut service, &scope, &mut store, next);
    assert!(service.storage_usage().unwrap().unwrap().metadata_bytes <= policy.metadata_bytes);
}

/// Per-recipient byte limits remain independent of the larger shared logical
/// budget. Another recipient may use free disk/cache capacity without receiving
/// the old target's mail, and a refused original allocates no new occurrence.
#[test]
fn per_recipient_payload_ceiling_does_not_consume_unrelated_allowance() {
    let mut policy = limits();
    policy.per_recipient_payload_bytes = 64;
    let (mut service, scope, sender, target, mut store) = fixture(policy);
    accept(
        &mut service,
        &scope,
        &mut store,
        mail(&sender, &target, "first", 40),
    );
    assert!(
        service
            .prepare_acceptance(
                &sender.agent_id,
                mail(&sender, &target, "refused", 40),
                MessageScope::Session,
                11,
                4096,
                None
            )
            .is_err()
    );
    assert_eq!(service.last_sequence(), 1);
    let other = service.register_agent(None, None, "other", vec![]);
    accept(
        &mut service,
        &scope,
        &mut store,
        mail(&sender, &other, "healthy", 40),
    );
    assert_eq!(
        service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_payload_bytes,
        80
    );
}

/// Storage mode cannot be selected by invalid values, a generic memory accept,
/// or a foreign issuing root. Refused issuance leaves the original unissued
/// ticket cancellable and releases the cache allocation when cancelled.
#[test]
fn storage_mode_rejects_invalid_policy_and_memory_or_foreign_issuance() {
    let mut invalid = limits();
    invalid.cache_payload_bytes = 1024 * 1024 + 1;
    let mut empty = MessageService::default();
    assert!(
        empty
            .enable_storage_custody(&MessageStorageScope::default(), invalid)
            .is_err()
    );
    let (mut service, scope, sender, target, _) = fixture(limits());
    let original = mail(&sender, &target, "one", 40);
    assert!(
        service
            .accept_at_with_scope(
                &sender.agent_id,
                original.clone(),
                MessageScope::Session,
                10
            )
            .is_err()
    );
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            original,
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    assert!(service.begin_acceptance_write(&ticket).is_err());
    assert!(
        service
            .begin_storage_acceptance_write(&ticket, &MessageStorageScope::default())
            .is_err()
    );
    service.cancel_acceptance(&ticket).unwrap();
    assert_eq!(
        service.storage_usage().unwrap().unwrap().cache,
        MessagePayloadUsage::default()
    );
    assert!(service.enable_storage_custody(&scope, limits()).is_err());
}

/// An unfinished read owns the full frozen source, including recipient slots,
/// after terminal custody expires. Reserving a replacement must keep that orphan
/// metadata charged, even though no payload allocation exists yet. Once the
/// original worker actually returns and releases its source, the same candidate
/// can fit without raising the logical metadata ceiling.
#[test]
fn unfinished_read_keeps_orphan_recipient_metadata_charged() {
    let mut policy = limits();
    policy.metadata_bytes = WORKER_HEADROOM + 192 * 1024;
    policy.recipient_obligations = 2000;
    let (mut service, scope, sender, target, mut store) = fixture(policy);
    for _ in 0..600 {
        service.register_agent(None, None, "fanout", vec![]);
    }
    let mut original = mail(&sender, &target, "expiring", 0);
    original.recipient = Recipient::Role("fanout".into());
    original.ttl_ms = Some(1);
    let first = accept(&mut service, &scope, &mut store, original);
    service
        .release_stored_payload(first.sequence, &scope)
        .unwrap();
    let write = service
        .begin_payload_load(first.sequence, &scope)
        .unwrap()
        .unwrap();
    assert_eq!(service.settle_delivery_outcomes_at(12).unwrap(), 600);
    service.settle_delivery_outcomes_at(600_013).unwrap();
    assert_eq!(
        service.storage_usage().unwrap().unwrap().pending_messages,
        0
    );
    let mut next = mail(&sender, &target, "replacement", 0);
    next.recipient = Recipient::Role("fanout".into());
    let attempt = service.prepare_acceptance(
        &sender.agent_id,
        next.clone(),
        MessageScope::Session,
        600_014,
        4096,
        None,
    );
    assert!(
        attempt.is_err(),
        "unfinished read's frozen recipient slots must remain charged"
    );
    assert_eq!(service.last_sequence(), first.sequence);
    let mut result = write.load(&mut store);
    service.publish_payload_load(&mut result).unwrap();
    drop(result);
    service
        .release_stored_payload(first.sequence, &scope)
        .unwrap();
    accept(&mut service, &scope, &mut store, next);
    assert!(service.storage_usage().unwrap().unwrap().metadata_bytes <= policy.metadata_bytes);
}
