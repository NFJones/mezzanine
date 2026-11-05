//! Canonical-proof-only consumption controls through actual lower reservations.
//! Fixture callback success tests the contract, never physical durability.

use super::*;

/// Only the actual trusted callback seals a proof; the descriptive record alone
/// has no constructor for a consumption ticket or canonical positive.
fn proof(scene: &Scene) -> MessageTerminalHandoffCommit {
    let source = scene.live();
    source
        .clone()
        .verify(&mut Canonical(Some(record(&source))))
        .unwrap()
        .unwrap()
}

/// Canonical proof alone does not consume. An actual separate terminal write
/// and adoption releases only the frozen slot, preserving reason/first clock,
/// with no ACK, cursor or horizon observation. Repeated adoption is inert.
#[test]
fn canonical_sender_consumption_requires_separate_write_and_original_adoption() {
    let mut scene = Scene::new(true, true);
    let proof = proof(&scene);
    assert_eq!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 1)
            .unwrap()
            .len(),
        1
    );
    let ticket = scene
        .service
        .prepare_storage_sender_consumption(&scene.scope, &proof, usize::MAX)
        .unwrap();
    assert_eq!(ticket.kind(), MessageTerminalKind::SenderConsumption);
    let write = scene
        .service
        .begin_storage_terminal_write(&ticket, &scene.scope)
        .unwrap();
    assert_eq!(write.kind(), MessageTerminalKind::SenderConsumption);
    assert_eq!(write.observed_at_ms(), 16);
    let mut bytes = Vec::new();
    write.write_record(&mut bytes).unwrap();
    assert_eq!(bytes.len(), 133);
    assert_eq!(write.encoded_bytes(), 133);
    assert_eq!(&bytes[..8], b"MMPCON01");
    assert_eq!(write.canonical_handoff().unwrap().record(), proof.record());
    let row = proof.anchor().original_row().clone();
    let MessageTerminalWriteOutcome::Committed(committed) = write.persist(&mut Store) else {
        panic!("trusted lower callback required");
    };
    assert_eq!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 1)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(scene.service.publish_terminal(&committed).unwrap(), 1);
    assert_eq!(scene.service.publish_terminal(&committed).unwrap(), 0);
    let terminal = row.terminal.lock().unwrap()[0].unwrap();
    assert!(terminal.released);
    assert_eq!(terminal.observed_at_ms, 16);
    assert_eq!(terminal.reason, DeliveryTerminalReason::Expired);
    assert!(!row.acknowledged.lock().unwrap()[0]);
    assert_eq!(*row.horizon_observed_at_ms.lock().unwrap(), None);
    assert!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 1)
            .unwrap()
            .is_empty()
    );
    assert!(
        scene
            .service
            .prepare_storage_sender_consumption(&scene.scope, &proof, usize::MAX)
            .is_err()
    );
}

/// Foreign services/scopes, exhausted pre-issue metadata and stale after-images
/// refuse without losing the original proof. Historical original proof retains
/// its consumption right after native sender replacement, without live rights.
#[test]
fn canonical_sender_consumption_refuses_foreign_scope_and_preserves_historical_rights() {
    let mut scene = Scene::new(true, true);
    let proof = proof(&scene);
    let mut foreign = Scene::new(true, true);
    assert!(
        foreign
            .service
            .prepare_storage_sender_consumption(&foreign.scope, &proof, usize::MAX)
            .is_err()
    );
    assert!(
        scene
            .service
            .prepare_storage_sender_consumption(&MessageStorageScope::default(), &proof, usize::MAX)
            .is_err()
    );
    assert!(
        scene
            .service
            .prepare_storage_sender_consumption(&scene.scope, &proof, 1)
            .is_err()
    );
    assert!(!scene.service.has_unpublished_terminal());
    scene
        .service
        .bind_native_conversation(
            &scene.registration,
            "original-sender",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    assert!(
        !scene
            .service
            .terminal_handoff_owner_is_current(proof.anchor())
    );
    let ticket = scene
        .service
        .prepare_storage_sender_consumption(&scene.scope, &proof, usize::MAX)
        .unwrap();
    scene.service.cancel_storage_terminal(&ticket).unwrap();
    assert!(!scene.service.has_unpublished_terminal());
    assert!(!proof.anchor().terminal().released);
    assert!(
        scene
            .service
            .prepare_storage_horizon_release(&scene.scope, 600016, usize::MAX)
            .unwrap()
            .is_none()
    );
    let ticket = scene
        .service
        .prepare_storage_sender_consumption(&scene.scope, &proof, usize::MAX)
        .unwrap();
    let write = scene
        .service
        .begin_storage_terminal_write(&ticket, &scene.scope)
        .unwrap();
    let MessageTerminalWriteOutcome::Committed(committed) = write.persist(&mut Store) else {
        panic!("lower positive required");
    };
    assert_eq!(scene.service.publish_terminal(&committed).unwrap(), 1);
}

/// Uncertainty and failed cancellation keep the same original proof, source and
/// slot protection. A retry can never become a refreshed terminal observation
/// or release its metadata decoder lease before actual write ownership ends.
#[test]
fn canonical_sender_consumption_unknown_keeps_original_proof_and_decoder() {
    struct Failed;
    impl MessageTerminalStore for Failed {
        fn commit(&mut self, _: &MessageTerminalWrite) -> Result<()> {
            Err(unavailable())
        }
    }
    let mut scene = Scene::new(true, true);
    let proof = proof(&scene);
    let metadata = scene
        .service
        .storage_custody
        .as_ref()
        .unwrap()
        .checkpoint_bytes
        .clone();
    let before = metadata.load(Ordering::Acquire);
    let ticket = scene
        .service
        .prepare_storage_sender_consumption(&scene.scope, &proof, usize::MAX)
        .unwrap();
    let charge = ticket.metadata_bytes();
    let write = scene
        .service
        .begin_storage_terminal_write(&ticket, &scene.scope)
        .unwrap();
    let mut first = Vec::new();
    write.write_record(&mut first).unwrap();
    let MessageTerminalWriteOutcome::Unknown(uncertain) = write.persist(&mut Failed) else {
        panic!("fixture uncertainty required");
    };
    drop(proof);
    assert!(scene.service.cancel_storage_terminal(&ticket).is_err());
    let observer = scene.service.payload_observer().unwrap();
    assert_eq!(observer.snapshot().unwrap().reserved_payload_bytes, 0);
    assert!(metadata.load(Ordering::Acquire) >= before + charge);
    let retry = scene.service.retry_terminal_write(uncertain).unwrap();
    assert!(retry.canonical_handoff().is_some());
    assert!(metadata.load(Ordering::Acquire) >= before + charge);
    let mut repeated = Vec::new();
    retry.write_record(&mut repeated).unwrap();
    assert_eq!(first, repeated);
    let MessageTerminalWriteOutcome::Committed(committed) = retry.persist(&mut Store) else {
        panic!("lower positive required");
    };
    assert_eq!(scene.service.publish_terminal(&committed).unwrap(), 1);
}

/// One actual sender handoff settles exactly one recipient of a frozen fanout.
/// Every other terminal before-image and pending obligation remains intact,
/// including its first clock; consumption grants no shared horizon witness.
#[test]
fn canonical_sender_consumption_releases_only_its_original_fanout_slot() {
    let mut scene = Scene::with_recipients(true, true, 3);
    let proof = proof(&scene);
    let row = proof.anchor().original_row().clone();
    let ticket = scene
        .service
        .prepare_storage_sender_consumption(&scene.scope, &proof, usize::MAX)
        .unwrap();
    let write = scene
        .service
        .begin_storage_terminal_write(&ticket, &scene.scope)
        .unwrap();
    let MessageTerminalWriteOutcome::Committed(committed) = write.persist(&mut Store) else {
        panic!("lower positive required");
    };
    assert_eq!(scene.service.publish_terminal(&committed).unwrap(), 1);
    let terminals = row.terminal.lock().unwrap();
    assert_eq!(terminals.iter().flatten().filter(|v| v.released).count(), 1);
    assert!(terminals.iter().flatten().all(|v| v.observed_at_ms == 16));
    drop(terminals);
    assert_eq!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 10)
            .unwrap()
            .len(),
        2
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
    assert_eq!(*row.horizon_observed_at_ms.lock().unwrap(), None);
}
