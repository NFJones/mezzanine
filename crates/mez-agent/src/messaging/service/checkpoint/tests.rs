//! Cold-reference checkpoint capture and structural refusal at the lower boundary.

use super::*;

/// Minimal actual lower storage port; committed original bytes are irrelevant to
/// capture's no-payload invariant and product tests verify real SQLite digests.
struct Store;
impl MessageAcceptanceStore for Store {
    fn commit(
        &mut self,
        _: &MessageAcceptanceWrite,
    ) -> std::result::Result<(), MessageAcceptanceStoreError> {
        Ok(())
    }
}
impl MessageStorageSnapshotStore for Store {
    fn verify(&mut self, request: &MessageStorageSnapshotWrite) -> Result<MessageStorageBinding> {
        Ok(MessageStorageBinding {
            owner: "original".into(),
            store_id: [1; 16],
            generation: 1,
            sequence_watermark: request.next_sequence() - 1,
            directory_device: 1,
            directory_inode: 1,
            database_device: 1,
            database_inode: 2,
        })
    }
}
/// Creates actual registrations, an opaque native binding, and one accepted
/// original under storage policy. No tuple from a checkpoint becomes authority.
fn scene() -> (
    MessageService,
    MessageStorageScope,
    SenderIdentity,
    SenderIdentity,
) {
    scene_with_ttl(None)
}

/// Builds the same actual lower occurrence with an optional acceptance-relative TTL.
fn scene_with_ttl(
    ttl_ms: Option<u64>,
) -> (
    MessageService,
    MessageStorageScope,
    SenderIdentity,
    SenderIdentity,
) {
    let mut service = MessageService::default();
    let sender = service.register_agent(None, None, "sender", vec![]);
    let target = service.register_agent(None, None, "native", vec![]);
    service.subscribe(&target.agent_id).unwrap();
    let scope = MessageStorageScope::default();
    service
        .enable_storage_custody(&scope, Default::default())
        .unwrap();
    let token = service
        .capture_sender_registration(&target.agent_id)
        .unwrap();
    service
        .bind_native_conversation(
            &token,
            "original-conversation",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    let envelope = Envelope {
        protocol: MMP_PROTOCOL,
        id: "original".into(),
        message_type: "send".into(),
        time: "original".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id.clone()),
        correlation_id: None,
        ttl_ms,
        content_type: "text/plain".into(),
        payload: "x".repeat(1024),
        extension_fields: vec![],
    };
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
    let write = service
        .begin_storage_acceptance_write(&ticket, &scope)
        .unwrap();
    let MessageAcceptanceWriteOutcome::Committed(commit) = write.persist(&mut Store) else {
        panic!("actual test port must commit")
    };
    service.publish_acceptance(&commit).unwrap();
    (service, scope, sender, target)
}
fn capture(service: &MessageService, scope: &MessageStorageScope) -> MessageServiceSnapshot {
    let request = service.prepare_storage_snapshot(scope).unwrap();
    let verified = request.write().verify(&mut Store).unwrap();
    request.finish(verified).unwrap().0
}

/// Genuinely cold input cannot be generically snapshotted, but its opaque original
/// can be captured and qualified without reacquiring any canonical allocation.
/// Only actual native registry data is saved; raw discovery does not survive.
#[test]
fn cold_checkpoint_preserves_references_native_namespace_and_cache_usage() {
    let (mut service, scope, sender, target) = scene();
    service.release_stored_payload(1, &scope).unwrap();
    assert!(service.snapshot_state().is_err());
    let before = service.storage_usage().unwrap().unwrap();
    let state = capture(&service, &scope);
    state.validate_storage_snapshot().unwrap();
    let data = state.storage_snapshot.as_ref().unwrap();
    assert_eq!(data.sources.len(), 1);
    assert_eq!(data.sources[0].payload_bytes, 1024);
    assert_eq!(data.native[0].agent_id, target.agent_id.as_str());
    assert_eq!(state.registered_agents.len(), 1);
    assert!(
        state
            .registered_agents
            .iter()
            .all(|r| r.agent_id != sender.agent_id.as_str())
    );
    assert_eq!(service.storage_usage().unwrap().unwrap(), before);
    assert!(MessageService::from_snapshot_state(&state).is_err());
    assert!(state.retained_messages.is_empty() && state.accepted_messages.is_empty());
    assert_eq!(
        serde_json::from_slice::<MessageServiceSnapshot>(&serde_json::to_vec(&state).unwrap())
            .unwrap(),
        state
    );
}

/// Exact request allocation, not equal scope or repeated descriptive fields,
/// owns a positive verification. A running worker still holding the original
/// metadata prevents movement until its actual ownership is gone.
#[test]
fn checkpoint_qualification_rejects_foreign_capture_and_unjoined_worker() {
    let (service, scope, _, _) = scene();
    assert!(
        service
            .prepare_storage_snapshot(&MessageStorageScope::default())
            .is_err()
    );
    let first = service.prepare_storage_snapshot(&scope).unwrap();
    let second = service.prepare_storage_snapshot(&scope).unwrap();
    let verified = second.write().verify(&mut Store).unwrap();
    assert!(first.finish(verified).is_err());
    let request = service.prepare_storage_snapshot(&scope).unwrap();
    let still_running = request.write();
    let verified = request.write().verify(&mut Store).unwrap();
    assert!(request.finish(verified).is_err());
    drop(still_running);
}

/// Static decode validation must reject forged namespace/slot/registry/version
/// facts; valid serialized metadata remains data and never live restore proof.
#[test]
fn checkpoint_rejects_inconsistent_versions_bindings_slots_and_registry() {
    let (service, scope, _, _) = scene();
    let good = capture(&service, &scope);
    let mut candidates = Vec::new();
    let mut s = good.clone();
    s.storage_snapshot.as_mut().unwrap().format_version = 2;
    candidates.push(s);
    let mut s = good.clone();
    s.storage_snapshot.as_mut().unwrap().native[0].shell_incarnation[0] ^= 1;
    candidates.push(s);
    let mut s = good.clone();
    s.storage_snapshot.as_mut().unwrap().sources[0].acknowledged[0] = true;
    candidates.push(s);
    let mut s = good.clone();
    s.storage_snapshot.as_mut().unwrap().binding = None;
    candidates.push(s);
    let mut s = good.clone();
    s.presence[0].identity.role = Some("forged".into());
    candidates.push(s);
    let mut s = good.clone();
    s.registration_floors.as_mut().unwrap()[0].registered_after_sequence = s.next_sequence;
    candidates.push(s);
    let mut s = good.clone();
    s.subscriptions.push(s.subscriptions[0].clone());
    candidates.push(s);
    let mut s = good.clone();
    s.storage_snapshot.as_mut().unwrap().sources[0]
        .delivery
        .status = "forged".into();
    candidates.push(s);
    let mut s = good.clone();
    s.storage_snapshot.as_mut().unwrap().sources[0].pending = false;
    candidates.push(s);
    let mut s = good.clone();
    s.storage_snapshot.as_mut().unwrap().sources[0]
        .delivery
        .status = "expired".into();
    candidates.push(s);
    let mut s = good.clone();
    s.storage_snapshot.as_mut().unwrap().sources[0].ttl_ms = Some(0);
    candidates.push(s);
    for candidate in candidates {
        assert!(candidate.validate_storage_snapshot().is_err());
    }
    let mut downgraded = good;
    downgraded.schema_version = 9;
    assert!(MessageService::from_snapshot_state(&downgraded).is_err());
}

/// Captures stay in the same finite metadata total through worker verification
/// and external snapshot publication. Dropping an observer/request cannot free
/// a worker's shared capture; releasing the actual final lease restores capacity.
#[test]
fn checkpoint_metadata_reservation_survives_worker_and_publication_ownership() {
    let (mut service, scope, sender, target) = scene();
    let before = service.storage_usage().unwrap().unwrap();
    let request = service.prepare_storage_snapshot(&scope).unwrap();
    let charged = service.storage_usage().unwrap().unwrap().metadata_bytes;
    assert!(charged > before.metadata_bytes);
    let write = request.write();
    drop(request);
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        charged
    );
    drop(write);
    assert_eq!(service.storage_usage().unwrap().unwrap(), before);
    let request = service.prepare_storage_snapshot(&scope).unwrap();
    let verified = request.write().verify(&mut Store).unwrap();
    let (snapshot, lease) = request.finish(verified).unwrap();
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        charged
    );
    drop(snapshot);
    drop(lease);
    assert_eq!(service.storage_usage().unwrap().unwrap(), before);
    let mut captures = Vec::new();
    let refused = loop {
        match service.prepare_storage_snapshot(&scope) {
            Ok(request) => {
                captures.push(request);
                assert!(captures.len() < 1024);
            }
            Err(error) => break error,
        }
    };
    assert_eq!(refused.kind(), MessageErrorKind::Capacity);
    assert_eq!(service.next_message_sequence(), 2);
    assert!(service.storage_usage().unwrap().unwrap().metadata_bytes <= 8 * 1024 * 1024);
    let incoming = Envelope {
        protocol: MMP_PROTOCOL,
        id: "checkpoint-pressure".into(),
        message_type: "send".into(),
        time: "new".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "new".into(),
        extension_fields: vec![(
            "budget".into(),
            serde_json::to_string(&"x".repeat(128 * 1024)).unwrap(),
        )],
    };
    let error = service
        .prepare_acceptance(
            &sender.agent_id,
            incoming.clone(),
            MessageScope::Session,
            20,
            4096,
            None,
        )
        .unwrap_err();
    assert_eq!(error.kind(), MessageErrorKind::Capacity);
    assert_eq!(service.next_message_sequence(), 2);
    drop(captures);
    assert_eq!(service.storage_usage().unwrap().unwrap(), before);
    let ticket = service
        .prepare_acceptance(
            &sender.agent_id,
            incoming,
            MessageScope::Session,
            20,
            4096,
            None,
        )
        .unwrap();
    assert_eq!(ticket.sequence(), 2);
    let budget = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lease = reserve_checkpoint(budget.clone(), 100, 50, 150).unwrap();
    assert!(reserve_checkpoint(budget.clone(), 1, 50, 150).is_err());
    drop(lease);
    assert!(reserve_checkpoint(budget, 100, 50, 150).is_ok());
}

/// The reserved quantum is allocation custody, not free space for competitors.
/// Excess must fit atomically, failed acquisition must return the loan, and the
/// final actual lease owner alone releases both excess and reserved custody.
#[test]
fn worker_metadata_loan_bounds_excess_and_last_owner() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let quantum = super::super::storage_policy::WORKER_METADATA_QUANTUM;
    let budget = Arc::new(AtomicUsize::new(0));
    let loan = Arc::new(AtomicBool::new(false));
    let base = quantum + 100;
    let limit = base + 20;
    assert!(
        reserve_worker_metadata(budget.clone(), loan.clone(), quantum + 21, base, limit).is_err()
    );
    assert!(!loan.load(Ordering::Acquire));
    assert_eq!(budget.load(Ordering::Acquire), 0);
    let lease = Arc::new(
        reserve_worker_metadata(budget.clone(), loan.clone(), quantum + 20, base, limit).unwrap(),
    );
    assert!(loan.load(Ordering::Acquire));
    assert_eq!(budget.load(Ordering::Acquire), 20);
    assert!(reserve_checkpoint(budget.clone(), 1, base, limit).is_err());
    assert!(reserve_worker_metadata(budget.clone(), loan.clone(), 1, base, limit).is_err());
    let last = lease.clone();
    drop(lease);
    assert!(loan.load(Ordering::Acquire));
    drop(last);
    assert!(!loan.load(Ordering::Acquire));
    assert_eq!(budget.load(Ordering::Acquire), 0);

    let lease = reserve_worker_metadata(budget.clone(), loan.clone(), quantum, base, base).unwrap();
    assert_eq!(budget.load(Ordering::Acquire), 0);
    assert!(reserve_worker_metadata(budget.clone(), loan.clone(), 1, base, base).is_err());
    drop(lease);
    assert!(!loan.load(Ordering::Acquire));
    assert!(reserve_worker_metadata(budget, loan, quantum, base, base).is_ok());
}

/// A genuine post-TTL duplicate lookup updates the retained acceptance receipt,
/// not the immutable original. Capture must preserve that legitimate expired
/// status even after its payload becomes cold; it must not reject historical mail
/// or rewrite the receipt to accepted merely to satisfy checkpoint validation.
#[test]
fn checkpoint_preserves_genuine_expired_duplicate_receipt_for_cold_original() {
    let (mut service, scope, sender, _) = scene_with_ttl(Some(5));
    let original = service.accepted_messages["original"]
        .source
        .envelope
        .resident()
        .unwrap();
    let duplicate = service
        .storage_qualified_duplicate(
            &sender.agent_id,
            &original,
            &scope,
            MessageScope::Session,
            16,
        )
        .unwrap()
        .unwrap();
    assert_eq!(duplicate.status, DeliveryStatus::Expired);
    drop(original);
    service.release_stored_payload(1, &scope).unwrap();
    let before = service.storage_usage().unwrap().unwrap();
    let snapshot = capture(&service, &scope);
    assert_eq!(
        snapshot.storage_snapshot.as_ref().unwrap().sources[0]
            .delivery
            .status,
        "expired"
    );
    snapshot.validate_storage_snapshot().unwrap();
    let source = &snapshot.storage_snapshot.as_ref().unwrap().sources[0];
    assert_eq!(source.accepted_at_ms, 10);
    assert_eq!(source.ttl_ms, Some(5));
    let mut impossible = snapshot.clone();
    impossible.storage_snapshot.as_mut().unwrap().sources[0].ttl_ms = Some(u64::MAX);
    assert!(impossible.validate_storage_snapshot().is_err());
    assert_eq!(service.storage_usage().unwrap().unwrap(), before);
}
