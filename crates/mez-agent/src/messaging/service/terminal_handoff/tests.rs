//! Genuine lower source/registration/terminal capabilities with fixture ports.
//! These callbacks test the contract shape, never filesystem durability.

use super::*;
use std::sync::atomic::Ordering;

struct Store;
impl MessageAcceptanceStore for Store {
    fn commit(
        &mut self,
        _: &MessageAcceptanceWrite,
    ) -> std::result::Result<(), MessageAcceptanceStoreError> {
        Ok(())
    }
}
impl MessageNativeContinuityStore for Store {
    fn commit(&mut self, _: &MessageNativeContinuityWrite) -> Result<()> {
        Ok(())
    }
}
impl MessageTerminalStore for Store {
    fn commit(&mut self, _: &MessageTerminalWrite) -> Result<()> {
        Ok(())
    }
}
struct Canonical(Option<MessageTerminalHandoffRecord>);
impl MessageTerminalHandoffStore for Canonical {
    fn verify(
        &mut self,
        _: &MessageTerminalHandoffAnchor,
    ) -> Result<Option<MessageTerminalHandoffRecord>> {
        Ok(self.0.clone())
    }
}

/// Actual native sender and accepted expired original under a trusted product
/// storage scope. No DTO or label creates any live registration or shell epoch.
struct Scene {
    service: MessageService,
    scope: MessageStorageScope,
    sender: SenderIdentity,
    recipient: SenderIdentity,
    registration: MessageSenderRegistration,
    outcome: MessageDeliveryOutcome,
}
impl Scene {
    fn new(native: bool, guarded: bool) -> Self {
        let mut service = MessageService::default();
        let sender = service.register_agent(None, None, "sender", vec![]);
        let recipient = service.register_agent(None, None, "recipient", vec![]);
        let registration = service
            .capture_sender_registration(&sender.agent_id)
            .unwrap();
        let scope = MessageStorageScope::default();
        service
            .enable_storage_custody(&scope, Default::default())
            .unwrap();
        if native {
            service
                .bind_native_conversation(
                    &registration,
                    "original-sender",
                    &MessageNativeIncarnation::default(),
                )
                .unwrap();
        }
        if guarded {
            service.require_native_continuity(&scope).unwrap();
        }
        if native {
            let request = service
                .prepare_native_continuity(&scope, &sender.agent_id)
                .unwrap();
            let verified = request.write().persist(&mut Store).unwrap();
            request.finish(&mut service, verified).unwrap();
        }
        let envelope = Envelope {
            protocol: MMP_PROTOCOL,
            id: "actual-outcome".into(),
            message_type: "send".into(),
            time: "original".into(),
            sender: sender.clone(),
            recipient: Recipient::Agent(recipient.agent_id.clone()),
            correlation_id: None,
            ttl_ms: Some(5),
            content_type: "text/plain".into(),
            payload: "original body".into(),
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
            panic!("lower source callback required");
        };
        service.publish_acceptance(&commit).unwrap();
        drop(commit);
        drop(ticket);
        let ticket = service
            .prepare_storage_expiry(&scope, 16, usize::MAX)
            .unwrap()
            .unwrap();
        let write = service
            .begin_storage_terminal_write(&ticket, &scope)
            .unwrap();
        let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut Store) else {
            panic!("lower expiry callback required");
        };
        service.publish_terminal(&commit).unwrap();
        drop(commit);
        drop(ticket);
        let outcome = service
            .delivery_outcomes_for(&sender.agent_id, 1)
            .unwrap()
            .remove(0);
        Self {
            service,
            scope,
            sender,
            recipient,
            registration,
            outcome,
        }
    }
    fn live(&self) -> MessageTerminalHandoffAnchor {
        self.service
            .capture_terminal_handoff(&self.sender.agent_id, &self.outcome)
            .unwrap()
    }
}
fn record(source: &MessageTerminalHandoffAnchor) -> MessageTerminalHandoffRecord {
    MessageTerminalHandoffRecord {
        store_id: [1; 16],
        occurrence: source.occurrence(&[1; 16]),
        archive_sequence: 7,
        canonical_sha256: [2; 32],
        archive_observed: true,
    }
}

/// Capture freezes actual sender/source/before-image and does not consume the
/// outcome. Lookup remains explicitly non-live, and no ordinary receiver offer
/// or ACK is created by either capability or a qualified-absence callback.
#[test]
fn terminal_handoff_capture_keeps_sender_and_recipient_roles_separate() {
    let scene = Scene::new(true, true);
    let source = scene.live();
    assert!(source.is_live());
    assert!(source.belongs_to(&scene.scope));
    assert!(!source.belongs_to(&MessageStorageScope::default()));
    assert_eq!(source.sender(), &scene.sender.agent_id);
    assert_eq!(source.recipient(), &scene.recipient.agent_id);
    assert_eq!(source.sender_registered_after_sequence(), 0);
    assert_eq!(source.registered_after_sequence(), 0);
    assert_eq!(source.sequence(), 1);
    assert_eq!(source.binding_index(), 0);
    assert_eq!(source.outcome(), scene.outcome);
    assert_eq!(source.terminal().observed_at_ms, 16);
    assert!(!source.terminal().released);
    let lookup = scene.service.capture_terminal_handoff_lookup(1, 0).unwrap();
    assert!(!lookup.is_live());
    let description =
        crate::transcript::TranscriptTerminalHandoffSource::from_anchor(&source, [1; 16]).unwrap();
    assert!(
        crate::transcript::TranscriptTerminalHandoffSource::from_anchor(&lookup, [1; 16]).is_err()
    );
    assert!(description.matches_anchor(&lookup, &[1; 16]));
    assert!(!description.matches_anchor(&lookup, &[2; 16]));
    assert_eq!(description.occurrence(), source.occurrence(&[1; 16]));
    assert_eq!(description.outcome(), scene.outcome);
    for field in ["sender", "recipient", "message_id"] {
        let mut changed = serde_json::to_value(&description).unwrap();
        changed[field] = serde_json::json!("other");
        let changed: crate::transcript::TranscriptTerminalHandoffSource =
            serde_json::from_value(changed).unwrap();
        assert!(!changed.matches_anchor(&source, &[1; 16]));
    }
    assert_eq!(lookup.occurrence(&[1; 16]), source.occurrence(&[1; 16]));
    assert!(lookup.verify(&mut Canonical(None)).unwrap().is_none());
    assert!(scene.service.terminal_handoff_anchor_is_original(&source));
    assert!(scene.service.terminal_handoff_owner_is_current(&source));
    assert_eq!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 10)
            .unwrap(),
        vec![scene.outcome]
    );
}

/// Original source protection and metadata charging survive every clone and
/// actual callback proof. Serialization discards archive observation and cannot
/// recreate the source/proof; canonical success alone does not release outcomes.
#[test]
fn terminal_handoff_proof_retains_slot_protection_and_last_metadata_owner() {
    let mut scene = Scene::new(true, true);
    let before = scene
        .service
        .storage_custody
        .as_ref()
        .unwrap()
        .checkpoint_bytes
        .load(Ordering::Acquire);
    let source = scene.live();
    let charge = scene
        .service
        .storage_custody
        .as_ref()
        .unwrap()
        .checkpoint_bytes
        .load(Ordering::Acquire);
    assert!(charge > before);
    assert!(
        scene
            .service
            .prepare_storage_horizon_release(&scene.scope, 600016, usize::MAX)
            .unwrap()
            .is_none()
    );
    let proof = source
        .clone()
        .verify(&mut Canonical(Some(record(&source))))
        .unwrap()
        .unwrap();
    assert!(proof.record().archive_observed);
    let decoded: MessageTerminalHandoffRecord =
        serde_json::from_slice(&serde_json::to_vec(proof.record()).unwrap()).unwrap();
    assert!(!decoded.archive_observed);
    drop(source);
    assert_eq!(
        scene
            .service
            .storage_custody
            .as_ref()
            .unwrap()
            .checkpoint_bytes
            .load(Ordering::Acquire),
        charge
    );
    assert!(
        scene
            .service
            .prepare_storage_horizon_release(&scene.scope, 600016, usize::MAX)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 1)
            .unwrap()
            .len(),
        1
    );
    assert!(
        scene
            .service
            .acknowledge_delivery_outcome(&scene.sender.agent_id, &scene.outcome)
            .is_err()
    );
    drop(proof);
    assert_eq!(
        scene
            .service
            .storage_custody
            .as_ref()
            .unwrap()
            .checkpoint_bytes
            .load(Ordering::Acquire),
        before
    );
    assert!(
        scene
            .service
            .prepare_storage_horizon_release(&scene.scope, 600016, usize::MAX)
            .unwrap()
            .is_some()
    );
}

/// Replacing an actual native epoch at the same CID/floor invalidates fresh
/// original-owner selection. Historical lookup still retains the old source,
/// without granting the replacement a task, sender identity or outcome handoff.
#[test]
fn terminal_handoff_replacement_preserves_only_historical_source_rights() {
    let mut scene = Scene::new(true, true);
    let original = scene.live();
    let old = original.native_owner().clone();
    scene
        .service
        .bind_native_conversation(
            &scene.registration,
            "original-sender",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    assert!(
        scene
            .service
            .capture_terminal_handoff(&scene.sender.agent_id, &scene.outcome)
            .is_err()
    );
    assert!(!scene.service.terminal_handoff_owner_is_current(&original));
    let lookup = scene.service.capture_terminal_handoff_lookup(1, 0).unwrap();
    assert!(!lookup.is_live());
    assert_eq!(lookup.native_owner(), &old);
    let proof = lookup
        .clone()
        .verify(&mut Canonical(Some(record(&lookup))))
        .unwrap()
        .unwrap();
    assert_eq!(proof.anchor().native_owner(), &old);
    let foreign = Scene::new(true, true);
    assert!(
        !foreign
            .service
            .terminal_handoff_anchor_is_original(&original)
    );
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .pending_messages,
        1
    );
}

/// Source before-images and real product guard are prerequisites. Caller-owned
/// DTO changes, raw classification and unguarded construction cannot obtain a
/// native sender cap; refused capture cannot leave a leaked slot protection.
#[test]
fn terminal_handoff_refuses_foreign_outcome_raw_or_unguarded_authority() {
    let mut scene = Scene::new(true, true);
    for clock in [15, 17] {
        let mut changed = scene.outcome.clone();
        changed.observed_at_ms = clock;
        assert!(
            scene
                .service
                .capture_terminal_handoff(&scene.sender.agent_id, &changed)
                .is_err()
        );
    }
    assert!(
        scene
            .service
            .capture_terminal_handoff(&scene.recipient.agent_id, &scene.outcome)
            .is_err()
    );
    assert!(scene.service.capture_terminal_handoff_lookup(1, 1).is_err());
    assert!(
        scene
            .service
            .prepare_storage_horizon_release(&scene.scope, 600016, usize::MAX)
            .unwrap()
            .is_some()
    );
    for (native, guarded) in [(false, true), (true, false)] {
        let other = Scene::new(native, guarded);
        assert!(
            other
                .service
                .capture_terminal_handoff(&other.sender.agent_id, &other.outcome)
                .is_err()
        );
        assert!(other.service.capture_terminal_handoff_lookup(1, 0).is_err());
    }
}

/// A malformed descriptive canonical result cannot seal proof. A root's retained
/// source survives a failed/lost worker callback until actual reconciliation,
/// and neither failure nor source enumeration publishes terminal consumption.
#[test]
fn terminal_handoff_canonical_callback_rejects_wrong_original_occurrence() {
    let mut scene = Scene::new(true, true);
    let source = scene.live();
    let mut wrong = record(&source);
    wrong.occurrence[0] ^= 1;
    assert!(source.clone().verify(&mut Canonical(Some(wrong))).is_err());
    struct Failure;
    impl MessageTerminalHandoffStore for Failure {
        fn verify(
            &mut self,
            _: &MessageTerminalHandoffAnchor,
        ) -> Result<Option<MessageTerminalHandoffRecord>> {
            Err(unavailable())
        }
    }
    assert!(source.clone().verify(&mut Failure).is_err());
    assert!(
        scene
            .service
            .prepare_storage_horizon_release(&scene.scope, 600016, usize::MAX)
            .unwrap()
            .is_none()
    );
    let next = scene
        .service
        .next_terminal_handoff_lookup(None)
        .unwrap()
        .unwrap();
    assert!(!next.is_live());
    assert!(
        scene
            .service
            .next_terminal_handoff_lookup(Some((1, 0)))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 1)
            .unwrap()
            .len(),
        1
    );
}

/// The actual root must reserve a complete source decoder and metadata scratch
/// before I/O. Clones share that one charge through final worker ownership;
/// refusal of a competing reservation must release only its temporary charge.
#[test]
fn terminal_handoff_io_keeps_decoder_and_scratch_through_last_worker_owner() {
    let mut scene = Scene::new(true, true);
    let source = scene.live();
    let observer = scene.service.payload_observer().unwrap();
    let before = scene
        .service
        .storage_custody
        .as_ref()
        .unwrap()
        .checkpoint_bytes
        .load(Ordering::Acquire);
    let io = scene.service.reserve_terminal_handoff_io(&source).unwrap();
    let charged = scene
        .service
        .storage_custody
        .as_ref()
        .unwrap()
        .checkpoint_bytes
        .load(Ordering::Acquire);
    assert!(charged >= before + MESSAGE_STORAGE_RECOVERY_SCRATCH_METADATA);
    assert_eq!(
        observer.snapshot().unwrap().reserved_payload_bytes,
        io.decode_limits().payload_bytes
    );
    assert!(scene.service.reserve_terminal_handoff_io(&source).is_err());
    assert_eq!(
        scene
            .service
            .storage_custody
            .as_ref()
            .unwrap()
            .checkpoint_bytes
            .load(Ordering::Acquire),
        charged
    );
    let retained = io.clone();
    drop(io);
    assert_eq!(
        observer.snapshot().unwrap().reserved_payload_bytes,
        retained.decode_limits().payload_bytes
    );
    let mut foreign = Scene::new(true, true);
    assert!(
        foreign
            .service
            .reserve_terminal_handoff_io(&source)
            .is_err()
    );
    drop(retained);
    assert_eq!(observer.snapshot().unwrap().reserved_payload_bytes, 0);
    assert_eq!(
        scene
            .service
            .storage_custody
            .as_ref()
            .unwrap()
            .checkpoint_bytes
            .load(Ordering::Acquire),
        before
    );
}
