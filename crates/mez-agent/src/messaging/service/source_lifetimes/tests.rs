//! Actual lower source ownership, not index presence or saved labels, fences
//! retirement evidence. No test authorizes or performs physical reclamation.

use super::*;

struct Store;
impl MessageAcceptanceStore for Store {
    fn commit(
        &mut self,
        _: &MessageAcceptanceWrite,
    ) -> std::result::Result<(), MessageAcceptanceStoreError> {
        Ok(())
    }
}
impl MessageAcknowledgementStore for Store {
    fn commit(&mut self, _: &MessageAcknowledgementWrite) -> Result<()> {
        Ok(())
    }
}
struct Canonical;
impl MessageNativeInputStore for Canonical {
    fn verify(&mut self, _: &MessageNativeInputAnchor) -> Result<Option<MessageNativeInputRecord>> {
        Ok(Some(MessageNativeInputRecord {
            store_id: [1; 16],
            occurrence: [2; 32],
            archive_sequence: 1,
            canonical_sha256: [3; 32],
            offered_at_ms: 11,
            archive_observed: false,
        }))
    }
}

/// Real first-epoch source/native tuple; no decoded metadata creates rights.
fn scene() -> (
    MessageService,
    MessageStorageScope,
    SenderIdentity,
    SenderIdentity,
) {
    let mut service = MessageService {
        retention_messages: 0,
        ..Default::default()
    };
    let sender = service.register_agent(None, None, "sender", vec![]);
    let native = service.register_agent(None, None, "native", vec![]);
    let scope = MessageStorageScope::default();
    service
        .enable_storage_custody(&scope, Default::default())
        .unwrap();
    let registration = service
        .capture_sender_registration(&native.agent_id)
        .unwrap();
    service
        .bind_native_conversation(
            &registration,
            "original",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    service.subscribe(&native.agent_id).unwrap();
    (service, scope, sender, native)
}

fn mail(sender: &SenderIdentity, native: &SenderIdentity, id: &str) -> Envelope {
    Envelope {
        protocol: MMP_PROTOCOL,
        id: id.into(),
        message_type: "send".into(),
        time: "original".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(native.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "actual source".into(),
        extension_fields: vec![],
    }
}

fn prepare(
    service: &mut MessageService,
    sender: &SenderIdentity,
    native: &SenderIdentity,
    id: &str,
) -> MessageAcceptanceTicket {
    service
        .prepare_acceptance(
            &sender.agent_id,
            mail(sender, native, id),
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap()
}

fn publish(
    service: &mut MessageService,
    scope: &MessageStorageScope,
    ticket: &MessageAcceptanceTicket,
) {
    let write = service
        .begin_storage_acceptance_write(ticket, scope)
        .unwrap();
    let MessageAcceptanceWriteOutcome::Committed(commit) = write.persist(&mut Store) else {
        panic!("actual test port must commit");
    };
    service.publish_acceptance(&commit).unwrap();
}

fn ack(
    service: &mut MessageService,
    scope: &MessageStorageScope,
    proof: &MessageNativeInputCommit,
) {
    let ticket = service
        .prepare_native_input_acknowledgement(proof, 512 * 1024)
        .unwrap()
        .unwrap();
    let write = service
        .begin_storage_acknowledgement_write(&ticket, scope)
        .unwrap();
    let MessageAcknowledgementWriteOutcome::Committed(commit) = write.persist(&mut Store) else {
        panic!("actual test port must commit");
    };
    service.publish_acknowledgement(&commit).unwrap();
}

/// A final native proof can retain its original after ACK, index removal and
/// payload release. Old fences stay conservative after the last clone exits;
/// only a fresh capture observes its actual retirement.
#[test]
fn source_retirement_excludes_evicted_original_until_last_actual_proof_drops() {
    let (mut service, scope, sender, native) = scene();
    let ticket = prepare(&mut service, &sender, &native, "native-source");
    let sequence = ticket.sequence();
    publish(&mut service, &scope, &ticket);
    service.receive_subscribed(&native.agent_id, 11, 1).unwrap();
    let proof = service
        .capture_native_input_anchor(&native.agent_id, sequence)
        .unwrap()
        .verify(&mut Canonical)
        .unwrap()
        .unwrap();
    let last = proof.clone();
    let payload_pin = proof.anchor().resident_envelope().unwrap();
    service.release_stored_payload(sequence, &scope).unwrap();
    ack(&mut service, &scope, &proof);
    assert!(service.pending.is_empty() && service.queued_by_sequence.is_empty());
    let fence = service.capture_source_retirement_fence(&scope).unwrap();
    assert!(service.source_retirement_fence_is_original(&fence));
    assert!(fence.belongs_to(&scope));
    assert!(!fence.has_no_live_source(sequence));
    drop(proof);
    assert!(
        !service
            .capture_source_retirement_fence(&scope)
            .unwrap()
            .has_no_live_source(sequence)
    );
    drop(last);
    assert!(!fence.has_no_live_source(sequence));
    let retired = service.capture_source_retirement_fence(&scope).unwrap();
    assert!(retired.has_no_live_source(sequence));
    // An envelope-only delivery pin has no source/load authority. Its actual
    // payload allocation remains independently charged after source retirement.
    assert!(
        service
            .storage_usage()
            .unwrap()
            .unwrap()
            .cache
            .live_payload_bytes
            > 0
    );
    drop(payload_pin);
    assert_eq!(
        service
            .storage_usage()
            .unwrap()
            .unwrap()
            .cache
            .live_payload_bytes,
        0
    );
    assert!(!retired.has_no_live_source(0));
    assert!(!retired.has_no_live_source(sequence + 1));
    let next = prepare(&mut service, &sender, &native, "later-source");
    assert!(!retired.has_no_live_source(next.sequence()));
    assert!(
        !service
            .capture_source_retirement_fence(&scope)
            .unwrap()
            .has_no_live_source(next.sequence())
    );
}

/// Unpublished original writes are live before physical acceptance. Cancellation
/// is possible only before issuance; it ends the row, not a proven acceptance.
/// Allocator gaps may have no source, so this evidence cannot prove settlement.
#[test]
fn source_retirement_covers_unpublished_sources_and_cancelled_gaps_without_rebinding() {
    let (mut service, scope, sender, native) = scene();
    let ticket = prepare(&mut service, &sender, &native, "cancelled");
    let before = service.capture_source_retirement_fence(&scope).unwrap();
    assert!(!before.has_no_live_source(ticket.sequence()));
    service.cancel_acceptance(&ticket).unwrap();
    let after = service.capture_source_retirement_fence(&scope).unwrap();
    assert!(after.has_no_live_source(ticket.sequence()));
    assert!(!before.has_no_live_source(ticket.sequence()));
    let mut replacement = MessageService::default();
    replacement
        .enable_storage_custody(&scope, Default::default())
        .unwrap();
    assert!(!replacement.source_retirement_fence_is_original(&after));
    assert!(!service.source_retirement_fence_is_original(
        &replacement.capture_source_retirement_fence(&scope).unwrap()
    ));
    assert!(
        service
            .capture_source_retirement_fence(&MessageStorageScope::default())
            .is_err()
    );
    assert!(
        MessageService::default()
            .capture_source_retirement_fence(&scope)
            .is_err()
    );
}

/// Narrow only the allocation-index seam, not the public pending limits. An
/// externally held original keeps its node charged after cancellation; refusal
/// reserves no sequence, and the last drop makes exactly one new slot usable.
#[test]
fn source_lifetime_capacity_refuses_before_sequence_and_releases_dead_nodes() {
    let (mut service, scope, sender, native) = scene();
    service
        .storage_custody
        .as_mut()
        .unwrap()
        .source_lifetimes
        .limit = 1;
    let ticket = prepare(&mut service, &sender, &native, "pinned-cancelled");
    let pinned = service
        .storage_custody
        .as_ref()
        .unwrap()
        .source_lifetimes
        .live()
        .unwrap()
        .get(&ticket.sequence())
        .unwrap()
        .upgrade()
        .unwrap();
    service.cancel_acceptance(&ticket).unwrap();
    let before = service.next_sequence;
    assert_eq!(
        service
            .storage_custody
            .as_ref()
            .unwrap()
            .source_lifetimes
            .metadata_bytes()
            .unwrap(),
        SOURCE_LIFETIME_METADATA
    );
    assert!(
        matches!(service.prepare_acceptance(&sender.agent_id, mail(&sender, &native, "capacity-refused"), MessageScope::Session, 10, 4096, None), Err(error) if error.kind() == MessageErrorKind::Capacity)
    );
    assert_eq!(service.next_sequence, before);
    assert!(service.admission.is_none());
    drop(pinned);
    assert_eq!(
        service
            .storage_custody
            .as_ref()
            .unwrap()
            .source_lifetimes
            .metadata_bytes()
            .unwrap(),
        0
    );
    let next = prepare(&mut service, &sender, &native, "after-drop");
    assert_eq!(next.sequence(), before);
    let baseline = service.storage_usage().unwrap().unwrap().metadata_bytes;
    let capture = service.capture_source_retirement_fence(&scope).unwrap();
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        baseline
    );
    drop(capture);
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        baseline
    );
}

/// Ordinary optional captures may fill the exact ceiling. The first retirement
/// fence must still fit independently of worker/native/ACK progress, and must
/// retain its exclusive allowance until its actual final owner drops it.
#[test]
fn source_retirement_fence_progress_at_exact_metadata_ceiling() {
    let (mut service, scope, sender, native) = scene();
    let ticket = prepare(&mut service, &sender, &native, "cancelled-gap");
    let sequence = ticket.sequence();
    service.cancel_acceptance(&ticket).unwrap();
    let policy = service.storage_custody.as_ref().unwrap();
    let baseline = service.storage_metadata_base_usage().unwrap();
    let ceiling = policy.limits.metadata_bytes;
    let optional = checkpoint::reserve_checkpoint(
        policy.checkpoint_bytes.clone(),
        ceiling - baseline,
        baseline,
        ceiling,
    )
    .unwrap();
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        ceiling
    );
    let fence = Arc::new(service.capture_source_retirement_fence(&scope).unwrap());
    assert!(fence.has_no_live_source(sequence));
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        ceiling
    );
    assert!(
        matches!(service.capture_source_retirement_fence(&scope), Err(error) if error.kind() == MessageErrorKind::Capacity)
    );
    // A fence cannot spend these other independent progress allowances.
    let worker = service
        .reserve_storage_worker_metadata(storage_policy::WORKER_METADATA_QUANTUM)
        .unwrap();
    let native_lease = checkpoint::reserve_native_input_metadata(
        policy.checkpoint_bytes.clone(),
        policy.native_input_metadata_loan.clone(),
        storage_policy::NATIVE_INPUT_METADATA_QUANTUM,
        baseline,
        ceiling,
    )
    .unwrap();
    let ack = service
        .reserve_storage_acknowledgement_metadata(512 * 1024)
        .unwrap();
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        ceiling
    );
    let final_owner = fence.clone();
    drop(fence);
    assert!(service.capture_source_retirement_fence(&scope).is_err());
    drop(final_owner);
    let again = service.capture_source_retirement_fence(&scope).unwrap();
    assert!(again.has_no_live_source(sequence));
    assert!(!again.has_no_live_source(sequence + 1));
    assert!(again.belongs_to(&scope));
    assert!(service.source_retirement_fence_is_original(&again));
    drop((again, worker, native_lease, ack, optional));
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        baseline
    );
}

/// The full source/journal inspection needs charged slot space in addition to
/// its first fence. Refusal must return both loans; actual last input ownership
/// keeps scratch charged even after another observer stops using it.
#[test]
fn source_retirement_inspection_reserves_slots_and_releases_failed_input() {
    let (service, scope, _, _) = scene();
    let policy = service.storage_custody.as_ref().unwrap();
    let baseline = service.storage_metadata_base_usage().unwrap();
    let ceiling = policy.limits.metadata_bytes;
    let optional = checkpoint::reserve_checkpoint(
        policy.checkpoint_bytes.clone(),
        ceiling - baseline,
        baseline,
        ceiling,
    )
    .unwrap();
    assert!(
        matches!(service.prepare_source_retirement_inspection(&scope), Err(error) if error.kind() == MessageErrorKind::Capacity)
    );
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        ceiling
    );
    let fence = service.capture_source_retirement_fence(&scope).unwrap();
    let worker = service
        .reserve_storage_worker_metadata(storage_policy::WORKER_METADATA_QUANTUM)
        .unwrap();
    drop((fence, worker, optional));
    let input = Arc::new(
        service
            .prepare_source_retirement_inspection(&scope)
            .unwrap(),
    );
    assert!(service.source_retirement_inspection_is_original(&input));
    assert!(input.belongs_to(&scope));
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        baseline + 256 * 1024
    );
    let final_owner = input.clone();
    drop(input);
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        baseline + 256 * 1024
    );
    drop(final_owner);
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        baseline
    );
    assert!(
        service
            .prepare_source_retirement_inspection(&MessageStorageScope::default())
            .is_err()
    );
}
