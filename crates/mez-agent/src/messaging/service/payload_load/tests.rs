//! Exact original qualification, frozen-source checking and zero-copy adoption.

use super::*;

/// Trusted synchronous test port retaining the exact serialized original bytes.
#[derive(Default)]
struct Store {
    bytes: Vec<u8>,
    decoded_payload: usize,
}
impl MessageAcceptanceStore for Store {
    fn commit(
        &mut self,
        record: &MessageAcceptanceWrite,
    ) -> std::result::Result<(), MessageAcceptanceStoreError> {
        self.bytes = serde_json::to_vec(record).unwrap();
        Ok(())
    }
}
impl MessagePayloadLoadStore for Store {
    fn load(&mut self, request: &MessagePayloadLoadWrite) -> Result<MessageAcceptanceSource> {
        let source = decode_message_acceptance(&self.bytes[..], request.decode_limits())?;
        self.decoded_payload = source.envelope().payload.as_ptr() as usize;
        Ok(source)
    }
}

/// Models a completed synchronous test read for exact-identity reissue tests.
struct Joined(MessagePayloadLoadAttempt);
impl MessagePayloadLoadJoin for Joined {
    fn qualify_joined(&mut self, attempt: &MessagePayloadLoadAttempt) -> Result<()> {
        if self.0.same_attempt(attempt) {
            Ok(())
        } else {
            Err(changed())
        }
    }
}

/// Qualifies a source only through actual lower reservation/port/adoption. A
/// cancelled first reservation makes interval tampering independently valid to
/// the decoder, so the reload must still compare original frozen intervals.
fn accepted() -> (
    MessageService,
    SenderIdentity,
    SenderIdentity,
    MessageStorageScope,
    Store,
    MessageSequence,
) {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "target", vec![]);
    service.subscribe(&target.agent_id).unwrap();
    let envelope = Envelope {
        protocol: MMP_PROTOCOL,
        id: "one".into(),
        message_type: "send".into(),
        time: "frozen".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id.clone()),
        correlation_id: Some("request".into()),
        ttl_ms: Some(123),
        content_type: "text/plain".into(),
        payload: "original".into(),
        extension_fields: vec![],
    };
    let unused = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope.clone(),
            MessageScope::Session,
            9,
            4096,
            None,
        )
        .unwrap();
    service.cancel_acceptance(&unused).unwrap();
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            envelope,
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    let scope = MessageStorageScope::default();
    let mut store = Store::default();
    let write = service
        .begin_storage_acceptance_write(&ticket, &scope)
        .unwrap();
    let MessageAcceptanceWriteOutcome::Committed(commit) = write.persist(&mut store) else {
        panic!("trusted test port must commit")
    };
    service.publish_acceptance(&commit).unwrap();
    (service, sender, target, scope, store, ticket.sequence())
}

/// Validated payload ownership moves from decoder to result to canonical cache,
/// preserving the same string allocation. Before publication all projections
/// still see a cold source; afterward complete schema9 state is byte-for-byte
/// equivalent. The settled result retains no source or payload cache pin.
#[test]
fn loaded_payload_moves_into_original_cache_without_copy_or_fact_changes() {
    let (mut service, _, _, scope, mut store, sequence) = accepted();
    let before = service.snapshot_state().unwrap();
    service.release_stored_payload(sequence, &scope).unwrap();
    let write = service
        .begin_payload_load(sequence, &scope)
        .unwrap()
        .unwrap();
    assert_eq!(service.pending[&sequence].envelope.resident_bytes(), 8);
    let mut result = write.load(&mut store);
    assert_eq!(service.pending[&sequence].envelope.resident_bytes(), 8);
    assert!(result.error().is_none());
    assert_eq!(
        result.envelope.as_ref().unwrap().payload.as_ptr() as usize,
        store.decoded_payload
    );
    assert!(service.snapshot_state().is_err());
    assert!(service.publish_payload_load(&mut result).unwrap());
    assert!(result.source.is_none());
    assert!(result.envelope.is_none());
    assert_eq!(service.snapshot_state().unwrap(), before);
    assert_eq!(
        service.pending[&sequence]
            .envelope
            .resident()
            .unwrap()
            .payload
            .as_ptr() as usize,
        store.decoded_payload
    );
    assert!(service.publish_payload_load(&mut result).unwrap());
}

/// A release cannot claim externally pinned bytes were freed. The exact old Arc
/// remains usable and a load request returns None rather than duplicating it.
/// Dropping the final external pin makes the same source eligible for one load.
#[test]
fn external_pin_is_reused_and_concurrent_missing_load_is_refused() {
    let (mut service, _, target, scope, _, sequence) = accepted();
    let batch = service.receive_subscribed(&target.agent_id, 11, 1).unwrap();
    service.release_stored_payload(sequence, &scope).unwrap();
    assert!(
        service
            .begin_payload_load(sequence, &scope)
            .unwrap()
            .is_none()
    );
    assert!(Arc::ptr_eq(
        &service.pending[&sequence].envelope.resident().unwrap(),
        &batch.messages[0].envelope
    ));
    drop(batch);
    let _write = service
        .begin_payload_load(sequence, &scope)
        .unwrap()
        .unwrap();
    assert!(service.begin_payload_load(sequence, &scope).is_err());
    assert!(service.release_stored_payload(sequence, &scope).is_err());
}

/// Valid independently decoded records with changed payload, metadata or outer
/// frozen fields cannot hydrate an original source. Byte length and IDs alone
/// are not fingerprints; every failure leaves original custody/sequence intact.
#[test]
fn altered_valid_source_fields_cannot_qualify_original_bytes() {
    for (old, new) in [
        ("\"payload\":\"original\"", "\"payload\":\"tampered\""),
        ("\"ttl_ms\":123", "\"ttl_ms\":124"),
        ("\"role\":\"sender\"", "\"role\":\"forged\""),
        ("\"accepted_at_ms\":10", "\"accepted_at_ms\":11"),
        (
            "\"sender_registered_after_sequence\":0",
            "\"sender_registered_after_sequence\":1",
        ),
        (
            "\"registered_after_sequence\":0",
            "\"registered_after_sequence\":1",
        ),
        (
            "\"correlation_id\":\"request\"",
            "\"correlation_id\":\"changed\"",
        ),
    ] {
        let (mut service, _, _, scope, mut store, sequence) = accepted();
        let bytes = String::from_utf8(store.bytes).unwrap();
        assert!(bytes.contains(old), "fixture must alter the intended field");
        store.bytes = bytes.replace(old, new).into_bytes();
        service.release_stored_payload(sequence, &scope).unwrap();
        let write = service
            .begin_payload_load(sequence, &scope)
            .unwrap()
            .unwrap();
        // Semantic decoding alone must not be the reason for this rejection.
        assert!(decode_message_acceptance(&store.bytes[..], write.decode_limits()).is_ok());
        let mut result = write.load(&mut store);
        assert!(result.error().is_some());
        assert!(service.publish_payload_load(&mut result).is_err());
        assert!(service.snapshot_state().is_err());
        assert_eq!(service.last_sequence(), sequence);
        assert!(service.accepted_messages["one"].delivery.accepted);
    }
}

/// Snapshot text does not recreate an original live root. Rejected foreign
/// publication leaves the valid result available to its original service; even
/// a matching saved source ID cannot consume the read or overwrite authority.
#[test]
fn foreign_scope_and_restored_owner_cannot_consume_loaded_original() {
    let (mut service, _, _, scope, mut store, sequence) = accepted();
    let mut restored =
        MessageService::from_snapshot_state(&service.snapshot_state().unwrap()).unwrap();
    assert!(restored.release_stored_payload(sequence, &scope).is_err());
    assert!(
        service
            .release_stored_payload(sequence, &MessageStorageScope::default())
            .is_err()
    );
    service.release_stored_payload(sequence, &scope).unwrap();
    assert!(
        service
            .begin_payload_load(sequence, &MessageStorageScope::default())
            .is_err()
    );
    let mut result = service
        .begin_payload_load(sequence, &scope)
        .unwrap()
        .unwrap()
        .load(&mut store);
    assert!(restored.publish_payload_load(&mut result).is_err());
    assert!(result.envelope.is_some());
    assert!(service.publish_payload_load(&mut result).unwrap());
}

/// A real join does not free a still-retained positive output buffer. Reissue
/// must wait until those bytes actually drop, then replace the original nonce;
/// stale attempts and mismatched join witnesses cannot issue another allocation.
#[test]
fn retained_positive_read_prevents_duplicate_allocation_and_stale_reissue() {
    let (mut service, _, _, scope, mut store, sequence) = accepted();
    service.release_stored_payload(sequence, &scope).unwrap();
    let write = service
        .begin_payload_load(sequence, &scope)
        .unwrap()
        .unwrap();
    let old = write.attempt();
    let result = write.load(&mut store);
    assert!(
        service
            .recover_payload_load(&old, &mut Joined(old.clone()))
            .is_err()
    );
    assert!(result.envelope.is_some());
    drop(result);
    let second = service
        .recover_payload_load(&old, &mut Joined(old.clone()))
        .unwrap()
        .unwrap();
    let current = second.attempt();
    assert!(
        service
            .recover_payload_load(&current, &mut Joined(old.clone()))
            .is_err()
    );
    assert!(
        service
            .recover_payload_load(&old, &mut Joined(old.clone()))
            .is_err()
    );
    let mut result = second.load(&mut store);
    assert!(service.publish_payload_load(&mut result).unwrap());
}

/// Exact ACK reservation protects its hashing sources from cache release. A
/// completed application/recipient offer is not permission to lose worker bytes
/// before its storage ACK has actually been persisted and adopted.
#[test]
fn active_ack_reservation_prevents_cache_release() {
    let (mut service, _, target, scope, _, sequence) = accepted();
    drop(service.receive_subscribed(&target.agent_id, 11, 1).unwrap());
    let _ticket = service
        .prepare_acknowledgement(&target.agent_id, sequence, 512 * 1024)
        .unwrap()
        .unwrap();
    assert!(service.release_stored_payload(sequence, &scope).is_err());
    assert!(service.pending[&sequence].envelope.resident().is_ok());
}

/// Maintenance may release the last canonical owner while a read is active.
/// Simulate that completed-maintenance boundary directly: late bytes must be
/// discarded, and retaining the settled result/attempt must not retain a cache.
#[test]
fn obsolete_read_discards_bytes_and_releases_strong_source_ownership() {
    let (mut service, _, _, scope, mut store, sequence) = accepted();
    service.release_stored_payload(sequence, &scope).unwrap();
    let write = service
        .begin_payload_load(sequence, &scope)
        .unwrap()
        .unwrap();
    let attempt = write.attempt();
    let weak = attempt.0.source.clone();
    let mut result = write.load(&mut store);
    service.pending.clear();
    service.queue.clear();
    service.accepted_messages.clear();
    service.rebuild_queue_indexes();
    assert!(!service.publish_payload_load(&mut result).unwrap());
    assert!(result.envelope.is_none());
    assert!(result.source.is_none());
    assert!(weak.upgrade().is_none());
    assert!(
        MessageService::default()
            .validate_payload_load_owner(&attempt)
            .is_err()
    );
}

/// Actual lost input can outlive removal of its source. Exact joined recovery
/// then returns None rather than reconstructing pending mail or holding a dead
/// source allocation. A foreign service remains unable to settle that original.
#[test]
fn joined_recovery_of_removed_source_does_not_reconstruct_mail() {
    let (mut service, _, _, scope, mut store, sequence) = accepted();
    service.release_stored_payload(sequence, &scope).unwrap();
    let write = service
        .begin_payload_load(sequence, &scope)
        .unwrap()
        .unwrap();
    let attempt = write.attempt();
    drop(write.load(&mut store));
    service.pending.clear();
    service.queue.clear();
    service.accepted_messages.clear();
    service.rebuild_queue_indexes();
    assert!(
        service
            .recover_payload_load(&attempt, &mut Joined(attempt.clone()))
            .unwrap()
            .is_none()
    );
    assert!(service.pending.is_empty());
}
