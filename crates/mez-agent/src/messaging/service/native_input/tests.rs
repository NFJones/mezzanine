//! Lower source/canonical-proof boundaries; product tests must prove actual I/O.

use super::*;

/// Minimal lower state-machine port; no filesystem publication is claimed here.
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
    fn commit(&mut self, write: &MessageAcknowledgementWrite) -> Result<()> {
        let mut encoded = Vec::new();
        write.write_record(&mut encoded).unwrap();
        assert_eq!(&encoded[..8], b"MMPACK02");
        Ok(())
    }
}
/// Exercises trusted callback shape; real transcript/root qualification belongs
/// to the product port and cannot be inferred from these illustrative row facts.
struct Canonical(Option<MessageNativeInputRecord>);
impl MessageNativeInputStore for Canonical {
    fn verify(&mut self, _: &MessageNativeInputAnchor) -> Result<Option<MessageNativeInputRecord>> {
        Ok(self.0.clone())
    }
}

/// One actual lower storage service and real native registration/source owners.
struct Scene {
    service: MessageService,
    scope: MessageStorageScope,
    sender: SenderIdentity,
    native: SenderIdentity,
    registration: MessageSenderRegistration,
}
impl Scene {
    fn new() -> Self {
        let mut service = MessageService::default();
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
                "original-context",
                &MessageNativeIncarnation::default(),
            )
            .unwrap();
        service.subscribe(&native.agent_id).unwrap();
        Self {
            service,
            scope,
            sender,
            native,
            registration,
        }
    }
    fn accept(&mut self, name: &str, ttl: Option<u64>) -> u64 {
        self.accept_with_extensions(name, ttl, vec![])
    }
    /// Adds real immutable source metadata for ownership/accounting controls.
    fn accept_with_extensions(
        &mut self,
        name: &str,
        ttl: Option<u64>,
        extension_fields: Vec<(String, String)>,
    ) -> u64 {
        let envelope = Envelope {
            protocol: "mmp/1",
            id: name.into(),
            message_type: "send".into(),
            time: "original".into(),
            sender: self.sender.clone(),
            recipient: Recipient::Agent(self.native.agent_id.clone()),
            correlation_id: None,
            ttl_ms: ttl,
            content_type: "text/plain".into(),
            payload: "actual original body".into(),
            extension_fields,
        };
        let ticket = self
            .service
            .prepare_acceptance(
                &self.sender.agent_id,
                envelope,
                MessageScope::Session,
                10,
                4096,
                None,
            )
            .unwrap();
        let write = self
            .service
            .begin_storage_acceptance_write(&ticket, &self.scope)
            .unwrap();
        let MessageAcceptanceWriteOutcome::Committed(commit) = write.persist(&mut Store) else {
            panic!("lower test port must commit");
        };
        self.service.publish_acceptance(&commit).unwrap();
        ticket.sequence()
    }
    fn offer(&self, now: u64) {
        self.service
            .receive_subscribed(&self.native.agent_id, now, 100)
            .unwrap();
    }
    fn proof(&self, sequence: u64, clock: u64) -> MessageNativeInputCommit {
        self.service
            .capture_native_input_anchor(&self.native.agent_id, sequence)
            .unwrap()
            .verify(&mut Canonical(Some(record(clock))))
            .unwrap()
            .unwrap()
    }
    fn ack(&mut self, proof: &MessageNativeInputCommit) {
        let ticket = self
            .service
            .prepare_native_input_acknowledgement(proof, 512 * 1024)
            .unwrap()
            .unwrap();
        let write = self
            .service
            .begin_storage_acknowledgement_write(&ticket, &self.scope)
            .unwrap();
        let MessageAcknowledgementWriteOutcome::Committed(commit) = write.persist(&mut Store)
        else {
            panic!("lower test ACK must commit");
        };
        self.service.publish_acknowledgement(&commit).unwrap();
    }
}
fn record(clock: u64) -> MessageNativeInputRecord {
    MessageNativeInputRecord {
        store_id: [1; 16],
        occurrence: [2; 32],
        archive_sequence: 1,
        canonical_sha256: [3; 32],
        offered_at_ms: clock,
        archive_observed: false,
    }
}

/// Product's guarded storage setup must not discard original evidence through
/// volatile expiry/retirement before the durable terminal producer exists.
/// Since those passes cannot settle the work, they must not spin a timer either.
#[test]
fn guarded_native_storage_withholds_volatile_terminal_mutation_and_timer() {
    let mut scene = Scene::new();
    let sequence = scene.accept("guarded-expiry", Some(10));
    scene
        .service
        .require_native_continuity(&scene.scope)
        .unwrap();
    assert_eq!(
        scene.service.settle_delivery_outcomes_at(10_000).unwrap(),
        0
    );
    assert!(!scene.service.needs_delivery_outcome_maintenance().unwrap());
    assert_eq!(
        scene
            .service
            .owned_pending_delivery_id(&scene.sender.agent_id, sequence),
        Some("guarded-expiry")
    );
}

/// A canonical commit proof can outlive transport ACK and replay eviction.
/// Its separate reservation must keep the entire retained original metadata
/// charged until the final proof owner retires, even with no ledger row left.
#[test]
fn native_input_proof_charges_source_metadata_after_ack_and_replay_eviction() {
    let mut scene = Scene::new();
    scene.service.retention_messages = 0;
    let sequence = scene.accept_with_extensions(
        "retained-proof",
        None,
        vec![(
            "source_metadata".into(),
            serde_json::to_string(&"x".repeat(8192)).unwrap(),
        )],
    );
    scene.offer(11);
    // One actual anchor owns the precharged allowance; this second proof must
    // pay its full charge and retain that charge after canonical row eviction.
    let loan_holder = scene
        .service
        .capture_native_input_anchor(&scene.native.agent_id, sequence)
        .unwrap();
    let proof = scene.proof(sequence, 11);
    let source_bytes = super::super::storage_policy::source_metadata_bytes(&proof.anchor.0.row);
    assert!(source_bytes > 16 * 1024);
    scene
        .service
        .release_stored_payload(sequence, &scene.scope)
        .unwrap();
    scene.ack(&proof);
    assert!(scene.service.pending.is_empty());
    assert!(scene.service.queue.is_empty());
    let retained = scene
        .service
        .storage_usage()
        .unwrap()
        .unwrap()
        .metadata_bytes;
    drop(proof);
    let retired = scene
        .service
        .storage_usage()
        .unwrap()
        .unwrap()
        .metadata_bytes;
    assert!(retained.saturating_sub(retired) >= source_bytes);
    drop(loan_holder);
}

/// Optional captures cannot consume the admission-to-proof progress allowance.
/// The original proof must remain charged through its last actual clone, while
/// a separate worker capture and the original ACK can still settle this source.
#[test]
fn native_input_proof_progress_survives_full_optional_metadata() {
    let mut scene = Scene::new();
    scene.service.retention_messages = 0;
    let sequence = scene.accept("proof-under-pressure", None);
    scene.offer(11);
    let policy = scene.service.storage_custody.as_ref().unwrap();
    let budget = policy.checkpoint_bytes.clone();
    let limit = policy.limits.metadata_bytes;
    assert_eq!(limit, 8 * 1024 * 1024);
    let base = scene.service.storage_metadata_base_usage().unwrap();
    let optional =
        checkpoint::reserve_checkpoint(budget.clone(), limit - base, base, limit).unwrap();
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .metadata_bytes,
        limit
    );
    let proof = scene.proof(sequence, 11);
    let last = proof.clone();
    let worker = scene
        .service
        .reserve_storage_worker_metadata(super::super::storage_policy::WORKER_METADATA_QUANTUM)
        .unwrap();
    assert!(matches!(
        scene.service.capture_native_input_anchor(&scene.native.agent_id, sequence),
        Err(error) if error.kind() == MessageErrorKind::Capacity
    ));
    scene
        .service
        .release_stored_payload(sequence, &scene.scope)
        .unwrap();
    scene.ack(&proof);
    assert!(scene.service.pending.is_empty());
    assert_eq!(
        scene
            .service
            .subscription(&scene.native.agent_id)
            .unwrap()
            .last_sequence,
        sequence
    );
    drop(proof);
    assert!(
        scene
            .service
            .storage_custody
            .as_ref()
            .unwrap()
            .native_input_metadata_loan
            .load(std::sync::atomic::Ordering::Acquire)
    );
    assert!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .metadata_bytes
            <= limit
    );
    drop(last);
    assert!(
        !scene
            .service
            .storage_custody
            .as_ref()
            .unwrap()
            .native_input_metadata_loan
            .load(std::sync::atomic::Ordering::Acquire)
    );
    drop(worker);
    drop(optional);
    let next = scene.accept("proof-after-pressure", None);
    scene.offer(12);
    let next_proof = scene.proof(next, 12);
    scene.ack(&next_proof);
    assert!(scene.service.pending.is_empty());
}

/// Cancellation and successful publication retire pending state, not retained
/// ticket/result custody. An uncertain write retries the same original while
/// another ACK cannot reuse its allowance until its final actual owner exits.
#[test]
fn native_ack_allowance_survives_cancel_uncertainty_and_publication() {
    struct Refuse;
    impl MessageAcknowledgementStore for Refuse {
        fn commit(&mut self, _: &MessageAcknowledgementWrite) -> Result<()> {
            Err(MessageError::invalid_state(
                "injected original ACK uncertainty",
            ))
        }
    }
    let mut scene = Scene::new();
    let first = scene.accept("first-ack", None);
    let second = scene.accept("second-ack", None);
    scene.offer(11);
    let first_proof = scene.proof(first, 11);
    let second_proof = scene.proof(second, 11);
    let policy = scene.service.storage_custody.as_ref().unwrap();
    let loan = policy.acknowledgement_metadata_loan.clone();
    let limit = policy.limits.metadata_bytes;
    let base = scene.service.storage_metadata_base_usage().unwrap();
    let held = policy
        .checkpoint_bytes
        .load(std::sync::atomic::Ordering::Acquire);
    let optional = checkpoint::reserve_checkpoint(
        policy.checkpoint_bytes.clone(),
        limit - base - held,
        base,
        limit,
    )
    .unwrap();
    let ticket = scene
        .service
        .prepare_native_input_acknowledgement(&first_proof, 512 * 1024)
        .unwrap()
        .unwrap();
    scene.service.cancel_acknowledgement(&ticket).unwrap();
    assert!(loan.load(std::sync::atomic::Ordering::Acquire));
    assert!(
        matches!(scene.service.prepare_native_input_acknowledgement(&first_proof, 512 * 1024),
        Err(error) if error.kind() == MessageErrorKind::Capacity)
    );
    drop(ticket);
    assert!(!loan.load(std::sync::atomic::Ordering::Acquire));
    let ticket = scene
        .service
        .prepare_native_input_acknowledgement(&first_proof, 512 * 1024)
        .unwrap()
        .unwrap();
    let write = scene
        .service
        .begin_storage_acknowledgement_write(&ticket, &scene.scope)
        .unwrap();
    let MessageAcknowledgementWriteOutcome::Unknown(unknown) = write.persist(&mut Refuse) else {
        panic!("injected refusal must retain original write");
    };
    assert!(scene.service.cancel_acknowledgement(&ticket).is_err());
    drop(ticket);
    assert!(loan.load(std::sync::atomic::Ordering::Acquire));
    let write = scene.service.retry_acknowledgement_write(unknown).unwrap();
    let MessageAcknowledgementWriteOutcome::Committed(commit) = write.persist(&mut Store) else {
        panic!("same original ACK must reconcile");
    };
    scene.service.publish_acknowledgement(&commit).unwrap();
    scene.service.publish_acknowledgement(&commit).unwrap();
    assert!(loan.load(std::sync::atomic::Ordering::Acquire));
    assert!(
        matches!(scene.service.prepare_native_input_acknowledgement(&second_proof, 512 * 1024),
        Err(error) if error.kind() == MessageErrorKind::Capacity)
    );
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .metadata_bytes,
        limit
    );
    drop(commit);
    assert!(!loan.load(std::sync::atomic::Ordering::Acquire));
    scene.ack(&second_proof);
    assert_eq!(
        scene
            .service
            .subscription(&scene.native.agent_id)
            .unwrap()
            .last_sequence,
        second
    );
    assert!(scene.service.pending.is_empty());
    drop(optional);
}

/// Metadata scan describes frozen native slots without selecting their bodies
/// or inventing offer clocks. Current selection remains a separate real lower
/// operation, and stable scan progress terminates after the final slot.
#[test]
fn native_input_scan_requires_separate_genuine_selection() {
    let mut scene = Scene::new();
    scene.accept("scan-original", None);
    let lookup = scene
        .service
        .next_native_input_lookup(None)
        .unwrap()
        .unwrap();
    assert_eq!(lookup.offered_at_ms(), None);
    assert!(
        scene
            .service
            .next_native_input_lookup(Some((lookup.sequence(), lookup.binding_index())))
            .unwrap()
            .is_none()
    );
    assert!(scene.service.native_input_can_receive(&lookup, 11));
    let selected = scene
        .service
        .select_native_input(&lookup, 11)
        .unwrap()
        .unwrap();
    assert_eq!(selected.sequence(), lookup.sequence());
    assert_eq!(selected.offered_at_ms(), Some(11));
}

/// A current owner may need a cold read, but reading descriptive metadata is
/// insufficient to select it. A successor native epoch cannot revive that
/// original, although historical canonical-proof lookup remains possible.
#[test]
fn native_input_scan_preserves_cold_and_replaced_originals_without_selection() {
    let mut scene = Scene::new();
    let sequence = scene.accept("scan-cold-original", None);
    scene
        .service
        .release_stored_payload(sequence, &scene.scope)
        .unwrap();
    let lookup = scene
        .service
        .next_native_input_lookup(None)
        .unwrap()
        .unwrap();
    assert!(lookup.resident_envelope().is_err());
    assert!(
        scene
            .service
            .select_native_input(&lookup, 11)
            .unwrap()
            .is_none()
    );
    scene
        .service
        .bind_native_conversation(
            &scene.registration,
            "successor-context",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    assert!(!scene.service.native_input_can_receive(&lookup, 11));
    assert!(
        scene
            .service
            .next_native_input_lookup(None)
            .unwrap()
            .is_some()
    );
}

/// Source capture cannot invent an offer, select a replacement native epoch or
/// turn another service into its owner. Lookup preserves old attribution but
/// qualified absence still yields no canonical commit or ACK capability.
#[test]
fn native_input_requires_original_scope_source_offer_and_canonical_callback() {
    let mut s = Scene::new();
    s.accept("one", None);
    assert!(
        s.service
            .capture_native_input_anchor(&s.native.agent_id, 1)
            .is_err()
    );
    s.offer(11);
    let anchor = s
        .service
        .capture_native_input_anchor(&s.native.agent_id, 1)
        .unwrap();
    assert!(anchor.belongs_to(&s.scope));
    assert!(!anchor.belongs_to(&MessageStorageScope::default()));
    assert!(s.service.native_input_anchor_is_original(&anchor));
    let other = Scene::new();
    assert!(!other.service.native_input_anchor_is_original(&anchor));
    assert!(
        anchor
            .clone()
            .verify(&mut Canonical(None))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        anchor.resident_envelope().unwrap().payload,
        "actual original body"
    );
    let original = anchor.native_owner().clone();
    s.service
        .bind_native_conversation(
            &s.registration,
            "replacement",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    assert!(
        s.service
            .capture_native_input_anchor(&s.native.agent_id, 1)
            .is_err()
    );
    let lookup = s.service.capture_native_input_lookup(1, 0).unwrap();
    assert!(lookup.offered_at_ms().is_none());
    assert_eq!(lookup.native_owner(), &original);
}

/// An actual trusted canonical result settles only its frozen old slot after a
/// conversation replacement. It cannot move the replacement cursor, and a
/// second service cannot consume that proof even with equal public IDs/floors.
#[test]
fn sealed_native_input_ack_survives_replacement_without_rebinding_cursor() {
    let mut s = Scene::new();
    s.accept("one", None);
    s.offer(11);
    let proof = s.proof(1, 11);
    let mut foreign = Scene::new();
    foreign.accept("one", None);
    assert!(
        foreign
            .service
            .prepare_native_input_acknowledgement(&proof, 512 * 1024)
            .is_err()
    );
    s.service
        .bind_native_conversation(
            &s.registration,
            "replacement",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    s.ack(&proof);
    assert_eq!(
        s.service
            .subscription(&s.native.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    assert!(s.service.pending.is_empty());
    assert!(
        s.service
            .prepare_native_input_acknowledgement(&proof, 512 * 1024)
            .unwrap()
            .is_none()
    );
}

/// A later canonical occurrence cannot cumulatively ACK an earlier unread own
/// slot. Once the earlier real commit settles, the scan cursor can include both
/// explicit positives without reinjecting the second input or requiring a fake ACK.
#[test]
fn native_input_single_slot_ack_preserves_uncommitted_prefix_and_reconciles_later() {
    let mut s = Scene::new();
    s.accept("one", None);
    s.accept("two", None);
    s.offer(11);
    let second = s.proof(2, 11);
    s.ack(&second);
    assert_eq!(
        s.service
            .subscription(&s.native.agent_id)
            .unwrap()
            .last_sequence,
        0
    );
    assert!(!s.service.pending[&1].acknowledged.lock().unwrap()[0]);
    let first = s.proof(1, 11);
    s.ack(&first);
    assert_eq!(
        s.service
            .subscription(&s.native.agent_id)
            .unwrap()
            .last_sequence,
        2
    );
    assert!(s.service.pending.is_empty());
}

/// Anchor copies share a small exact-slot protection token and retain their own
/// metadata until the final owner drops. A transient lookup cannot replace the
/// older protection; ordinary unactivated maintenance resumes after all retire.
#[test]
fn native_input_anchor_retains_metadata_and_exact_terminal_protection() {
    let mut s = Scene::new();
    s.accept("one", Some(5));
    s.offer(11);
    let baseline = s.service.storage_usage().unwrap().unwrap().metadata_bytes;
    let anchor = s
        .service
        .capture_native_input_anchor(&s.native.agent_id, 1)
        .unwrap();
    let lookup = s.service.capture_native_input_lookup(1, 0).unwrap();
    assert!(s.service.storage_usage().unwrap().unwrap().metadata_bytes > baseline);
    drop(lookup);
    assert_eq!(s.service.settle_delivery_outcomes_at(16).unwrap(), 0);
    drop(anchor);
    assert_eq!(
        s.service.storage_usage().unwrap().unwrap().metadata_bytes,
        baseline
    );
    assert_eq!(s.service.settle_delivery_outcomes_at(16).unwrap(), 1);
}

/// Stored clocks may genuinely predate acceptance after rollback, including
/// zero, but cannot exceed the immutable TTL boundary. Malformed descriptive
/// row facts never become canonical commit proof or alter recipient ACK slots.
#[test]
fn native_input_canonical_result_rejects_impossible_facts_and_keeps_zero_offer() {
    let mut s = Scene::new();
    s.accept("one", Some(5));
    s.offer(0);
    for fault in [
        "future-clock",
        "zero-row",
        "empty-store",
        "empty-key",
        "empty-digest",
    ] {
        let mut invalid = record(0);
        match fault {
            "future-clock" => invalid.offered_at_ms = 16,
            "zero-row" => invalid.archive_sequence = 0,
            "empty-store" => invalid.store_id = [0; 16],
            "empty-key" => invalid.occurrence = [0; 32],
            "empty-digest" => invalid.canonical_sha256 = [0; 32],
            _ => unreachable!(),
        }
        let anchor = s
            .service
            .capture_native_input_anchor(&s.native.agent_id, 1)
            .unwrap();
        assert!(
            anchor.verify(&mut Canonical(Some(invalid))).is_err(),
            "{fault}"
        );
    }
    let proof = s.proof(1, 0);
    s.ack(&proof);
    assert_eq!(
        s.service
            .subscription(&s.native.agent_id)
            .unwrap()
            .last_sequence,
        1
    );
}

/// An actual port observation is a transient fact. Serializing a descriptive
/// journal record must not let imported data claim current physical backing.
#[test]
fn native_input_archive_observation_cannot_round_trip_through_data() {
    let mut observed = record(10);
    observed.archive_observed = true;
    let encoded = serde_json::to_string(&observed).unwrap();
    assert!(!encoded.contains("archive_observed"));
    let restored: MessageNativeInputRecord = serde_json::from_str(&encoded).unwrap();
    assert!(!restored.archive_observed);
    assert_eq!(restored.archive_sequence, observed.archive_sequence);
}

/// Original current-owner observation survives actual lower ACK settlement and
/// removal from pending custody. It never reopens delivery, crosses service
/// allocations or adopts a new private incarnation at the same public CID.
/// The illustrative lower callback is not a filesystem qualification claim.
#[test]
fn native_current_owner_survives_ack_without_reopening_delivery_or_rebinding() {
    let mut scene = Scene::new();
    let sequence = scene.accept("current-owner-after-ack", None);
    scene.offer(11);
    let proof = scene.proof(sequence, 11);
    assert!(scene.service.native_input_owner_is_current(proof.anchor()));
    assert!(
        !Scene::new()
            .service
            .native_input_owner_is_current(proof.anchor())
    );
    scene.ack(&proof);
    assert!(
        !scene
            .service
            .native_input_anchor_is_original(proof.anchor())
    );
    assert!(scene.service.native_input_owner_is_current(proof.anchor()));
    assert!(!scene.service.native_input_can_receive(proof.anchor(), 12));
    scene
        .service
        .bind_native_conversation(
            &scene.registration,
            "original-context",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    assert!(!scene.service.native_input_owner_is_current(proof.anchor()));
}
