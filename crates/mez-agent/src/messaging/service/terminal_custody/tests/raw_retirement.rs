//! Genuine raw allocation/end custody and versioned descriptive source controls.
//!
//! These tests exercise actual lower reservations and trusted-port callbacks.
//! Concrete SQLite, actor and subprocess tests separately qualify product I/O.

use super::*;

/// Captures the actual immutable encoding while sealing a lower test positive.
#[derive(Default)]
struct SourceStore(Vec<u8>);
impl MessageRegistrationEndStore for Store {
    /// Seals only this lower fixture's actual original end, not physical I/O.
    fn commit(&mut self, write: &MessageRegistrationEndWrite) -> Result<()> {
        write.snapshot().validate()
    }
}

/// Completes the real lower end capability; product tests qualify its journal.
fn journal_end(scene: &mut Scene) {
    let request = scene
        .service
        .prepare_registration_end(&scene.scope)
        .unwrap()
        .unwrap();
    let verified = request.write().persist(&mut Store::default()).unwrap();
    request.finish(&mut scene.service, verified).unwrap();
}
impl MessageAcceptanceStore for SourceStore {
    fn commit(
        &mut self,
        write: &MessageAcceptanceWrite,
    ) -> std::result::Result<(), MessageAcceptanceStoreError> {
        self.0 = serde_json::to_vec(write).unwrap();
        Ok(())
    }
}

/// Accepts a real guarded raw original and returns only its descriptive bytes.
fn accept(scene: &mut Scene, id: &str, ttl_ms: Option<u64>) -> Vec<u8> {
    let envelope = Envelope {
        protocol: MMP_PROTOCOL,
        id: id.into(),
        message_type: "send".into(),
        time: "original".into(),
        sender: scene.sender.clone(),
        recipient: Recipient::Agent(scene.target.agent_id.clone()),
        correlation_id: None,
        ttl_ms,
        content_type: "text/plain".into(),
        payload: "actual raw original".into(),
        extension_fields: vec![],
    };
    let ticket = scene
        .service
        .prepare_acceptance(
            &scene.sender.agent_id,
            envelope,
            MessageScope::Session,
            10,
            4096,
            None,
        )
        .unwrap();
    let write = scene
        .service
        .begin_storage_acceptance_write(&ticket, &scene.scope)
        .unwrap();
    let mut port = SourceStore::default();
    let MessageAcceptanceWriteOutcome::Committed(commit) = write.persist(&mut port) else {
        panic!("actual accepted source required");
    };
    scene.service.publish_acceptance(&commit).unwrap();
    port.0
}

/// Narrows decoding to this fixture's actual small source and binding budget.
fn limits() -> MessageAcceptanceDecodeLimits {
    MessageAcceptanceDecodeLimits {
        payload_bytes: 1024,
        auxiliary_bytes: 32 * 1024,
        recipient_count: 4,
        recipient_bytes: 4096,
    }
}

/// Schema3 freezes exact namespaces for raw recipients without a reconnect
/// credential, registration token or end authority. A legacy schema2 original
/// remains explicitly unknown rather than inheriting the current allocation.
#[test]
fn raw_registration_source_is_versioned_and_decodes_without_end_authority() {
    let mut scene = Scene::new(true, false);
    let bytes = accept(&mut scene, "raw-schema", None);
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["schema_version"], 3);
    assert!(json["native_bindings"]["recipients"][0].is_null());
    let source = decode_message_acceptance(bytes.as_slice(), limits()).unwrap();
    let namespace = source.recipient_registration_namespace(0).unwrap();
    assert_eq!(
        namespace,
        *scene.service.registration_tokens[&scene.target.agent_id]
            .native_namespace
            .as_bytes()
    );
    use sha2::Digest;
    let digest = sha2::Sha256::digest(&bytes).into();
    let cold = source
        .into_cold_occurrence(
            &scene.scope,
            digest,
            vec![None],
            vec![false],
            vec![None],
            None,
        )
        .unwrap();
    assert!(
        cold.registration_bindings.as_ref().unwrap().recipients[0]
            .ended
            .get()
            .is_none()
    );
    let text = String::from_utf8(bytes).unwrap();
    let split = text.find(",\"recipient_registrations\":").unwrap();
    let legacy = format!(
        "{}}}",
        text[..split].replacen("\"schema_version\":3", "\"schema_version\":2", 1)
    );
    let old = decode_message_acceptance(legacy.as_bytes(), limits()).unwrap();
    assert!(old.has_native_binding_record());
    assert_eq!(old.recipient_registration_namespace(0), None);
    assert!(
        decode_message_acceptance(
            text.replacen("\"schema_version\":3", "\"schema_version\":4", 1)
                .as_bytes(),
            limits()
        )
        .is_err()
    );
}

/// Namespace framing is fixed to one valid UUID per original recipient.
/// Missing, wrong-width, out-of-range and non-v4 UUID arrays refuse before
/// any decoded state can enter a trusted recovery candidate.
#[test]
fn raw_registration_source_rejects_malformed_namespace_slots() {
    let mut scene = Scene::new(true, false);
    let text = String::from_utf8(accept(&mut scene, "raw-malformed", None)).unwrap();
    let split = text.find(",\"recipient_registrations\":").unwrap();
    for suffix in [
        "[]",
        "[[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]]",
        "[[256,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]]",
        "[[1,2]]",
    ] {
        let damaged = format!("{},\"recipient_registrations\":{suffix}}}", &text[..split]);
        assert!(decode_message_acceptance(damaged.as_bytes(), limits()).is_err());
    }
}

/// A source refused after raw endpoint reservation must release that transient
/// metadata. No accepted original or native publication exists, so repeatedly
/// attempting an unaccepted send cannot pin end-intent quota for a live target.
#[test]
fn raw_registration_failed_admission_releases_source_only_end_retention() {
    let mut scene = Scene::new(true, false);
    let before = scene
        .service
        .storage_usage()
        .unwrap()
        .unwrap()
        .metadata_bytes;
    let envelope = Envelope {
        protocol: MMP_PROTOCOL,
        id: "refused-raw".into(),
        message_type: "send".into(),
        time: "original".into(),
        sender: scene.sender.clone(),
        recipient: Recipient::Agent(scene.target.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "unaccepted".into(),
        extension_fields: vec![],
    };
    assert!(
        scene
            .service
            .prepare_acceptance(
                &scene.sender.agent_id,
                envelope,
                MessageScope::Session,
                10,
                0,
                None
            )
            .is_err()
    );
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .metadata_bytes,
        before
    );
    scene.service.retire_agent_identity(&scene.target.agent_id);
    assert!(!scene.service.has_pending_registration_end().unwrap());
}

/// New guarded source metadata must fit its actual bounded decoder before an
/// occurrence is reserved. Namespace and end-intent additions cannot turn a
/// valid-looking accepted source into history that recovery cannot decode.
#[test]
fn raw_registration_rejects_source_metadata_above_decode_quantum_without_reservation() {
    let mut scene = Scene::new(true, false);
    let before = scene
        .service
        .storage_usage()
        .unwrap()
        .unwrap()
        .metadata_bytes;
    let envelope = Envelope {
        protocol: MMP_PROTOCOL,
        id: "oversized-raw-metadata".into(),
        message_type: "send".into(),
        time: "t".repeat(129 * 1024),
        sender: scene.sender.clone(),
        recipient: Recipient::Agent(scene.target.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "small body".into(),
        extension_fields: vec![],
    };
    assert!(
        scene
            .service
            .prepare_acceptance(
                &scene.sender.agent_id,
                envelope,
                MessageScope::Session,
                10,
                4096,
                None
            )
            .is_err()
    );
    assert_eq!(scene.service.next_message_sequence(), 1);
    assert_eq!(
        scene
            .service
            .storage_usage()
            .unwrap()
            .unwrap()
            .metadata_bytes,
        before
    );
    assert!(!scene.service.has_unpublished_acceptance());
}

/// Live objective/presence observations never end a raw allocation. Only actual
/// removal selects its exact source, and selection/store/adoption stay distinct.
/// Its raw frame carries a descriptive end fingerprint, never a positive ACK.
#[test]
fn raw_retirement_requires_actual_end_and_original_terminal_adoption() {
    let mut scene = Scene::new(true, false);
    accept(&mut scene, "raw-terminal", None);
    scene
        .service
        .update_agent_objective(&scene.target.agent_id, Some("still live"), 12)
        .unwrap();
    assert!(
        scene
            .service
            .prepare_storage_raw_retirement(&scene.scope, 20, usize::MAX)
            .unwrap()
            .is_none()
    );
    assert!(scene.service.retire_agent_identity(&scene.target.agent_id));
    assert!(scene.service.has_pending_registration_end().unwrap());
    assert!(
        scene
            .service
            .prepare_storage_raw_retirement(&scene.scope, 5, usize::MAX)
            .unwrap()
            .is_none(),
        "raw terminal must leave the lane available for its original end journal"
    );
    let end_charge = scene
        .service
        .storage_custody
        .as_ref()
        .unwrap()
        .checkpoint_bytes
        .load(Ordering::Acquire);
    assert!(end_charge > 0);
    journal_end(&mut scene);
    assert_eq!(
        scene
            .service
            .storage_custody
            .as_ref()
            .unwrap()
            .checkpoint_bytes
            .load(Ordering::Acquire),
        0,
        "actual end adoption releases its intent while the pending source retains only its marker"
    );
    let ticket = scene
        .service
        .prepare_storage_raw_retirement(&scene.scope, 5, usize::MAX)
        .unwrap()
        .unwrap();
    let write = scene.write(&ticket);
    assert_eq!(write.kind(), MessageTerminalKind::RecipientRetirement);
    assert!(write.retired_recipient().is_none());
    assert_eq!(
        write.raw_retired_recipient().unwrap().ended_after_sequence,
        1
    );
    assert_eq!(write.observed_at_ms(), 10);
    let mut bytes = Vec::new();
    write.write_record(&mut bytes).unwrap();
    assert_eq!(&bytes[..8], b"MMPRET02");
    assert_eq!(bytes.len(), 144);
    assert_eq!(&bytes[80..112], &[0; 32]);
    assert!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 1)
            .unwrap()
            .is_empty()
    );
    let MessageTerminalWriteOutcome::Committed(commit) = write.persist(&mut Store::default())
    else {
        panic!("actual original positive required");
    };
    assert!(
        scene
            .service
            .delivery_outcomes_for(&scene.sender.agent_id, 1)
            .unwrap()
            .is_empty()
    );
    assert_eq!(scene.service.publish_terminal(&commit).unwrap(), 1);
    let outcome = scene
        .service
        .delivery_outcomes_for(&scene.sender.agent_id, 1)
        .unwrap()
        .remove(0);
    assert_eq!(outcome.reason, DeliveryTerminalReason::RecipientRetired);
    assert_eq!(outcome.observed_at_ms, 10);
    assert_eq!(
        *scene.service.pending[&1].acknowledged.lock().unwrap(),
        vec![false]
    );
}

/// Immutable authority replacement can preserve a numeric floor. The new raw
/// allocation may neither receive the earlier source nor become its end owner;
/// the original shared marker remains sufficient for exact retirement capture.
#[test]
fn raw_registration_replacement_with_equal_floor_cannot_receive_original() {
    let mut scene = Scene::new(true, false);
    let source = decode_message_acceptance(
        accept(&mut scene, "raw-old-allocation", None).as_slice(),
        limits(),
    )
    .unwrap();
    let original = source.recipient_registration_namespace(0).unwrap();
    let floor = scene.service.registration_floors[&scene.target.agent_id];
    let mut replacement = scene.target.clone();
    replacement.capabilities.push("new-authority".into());
    scene
        .service
        .ensure_agent_identity(replacement, 12)
        .unwrap();
    assert_eq!(
        scene.service.registration_floors[&scene.target.agent_id],
        floor
    );
    assert_ne!(
        *scene.service.registration_tokens[&scene.target.agent_id]
            .native_namespace
            .as_bytes(),
        original
    );
    assert!(
        scene
            .service
            .receive_subscribed(&scene.target.agent_id, 12, 1)
            .unwrap()
            .messages
            .is_empty()
    );
    journal_end(&mut scene);
    let ticket = scene
        .service
        .prepare_storage_raw_retirement(&scene.scope, 12, usize::MAX)
        .unwrap()
        .unwrap();
    let write = scene.write(&ticket);
    assert_eq!(
        write
            .raw_retired_recipient()
            .unwrap()
            .registration_namespace,
        original
    );
}

/// An issued genuine positive ACK remains the original owner through endpoint
/// removal. Retirement cannot select around that reservation or reinterpret
/// its exact original positive as a terminal disposition.
#[test]
fn raw_retirement_yields_to_original_ack_reservation() {
    let mut scene = Scene::new(true, false);
    accept(&mut scene, "raw-ack-first", None);
    scene.offer();
    let ticket = scene
        .service
        .prepare_acknowledgement(&scene.target.agent_id, 1, 512 * 1024)
        .unwrap()
        .unwrap();
    scene.service.retire_agent_identity(&scene.target.agent_id);
    assert!(
        scene
            .service
            .prepare_storage_raw_retirement(&scene.scope, 12, usize::MAX)
            .is_err()
    );
    let write = scene
        .service
        .begin_storage_acknowledgement_write(&ticket, &scene.scope)
        .unwrap();
    let MessageAcknowledgementWriteOutcome::Committed(commit) =
        write.persist(&mut Store::default())
    else {
        panic!("actual ACK required");
    };
    scene.service.publish_acknowledgement(&commit).unwrap();
    assert!(
        scene
            .service
            .prepare_storage_raw_retirement(&scene.scope, 12, usize::MAX)
            .unwrap()
            .is_none()
    );
}

/// A real later cumulative ACK observes an unoffered earlier source's expiry.
/// That source-wide fact wins before raw retirement and preserves its first
/// strict clock even when later expiry selection sees wall time roll backward.
#[test]
fn raw_retirement_yields_to_known_source_expiry_across_clock_rollback() {
    let mut scene = Scene::new(true, false);
    accept(&mut scene, "raw-expired-prefix", Some(5));
    accept(&mut scene, "raw-live-endpoint", None);
    assert_eq!(
        scene
            .service
            .receive_subscribed(&scene.target.agent_id, 16, 2)
            .unwrap()
            .messages
            .len(),
        1
    );
    let ticket = scene
        .service
        .prepare_acknowledgement(&scene.target.agent_id, 2, 512 * 1024)
        .unwrap()
        .unwrap();
    let write = scene
        .service
        .begin_storage_acknowledgement_write(&ticket, &scene.scope)
        .unwrap();
    let MessageAcknowledgementWriteOutcome::Committed(commit) =
        write.persist(&mut Store::default())
    else {
        panic!("actual endpoint ACK required");
    };
    scene.service.publish_acknowledgement(&commit).unwrap();
    scene.service.retire_agent_identity(&scene.target.agent_id);
    journal_end(&mut scene);
    assert!(
        scene
            .service
            .prepare_storage_raw_retirement(&scene.scope, 11, usize::MAX)
            .unwrap()
            .is_none()
    );
    let ticket = scene
        .service
        .prepare_storage_expiry(&scene.scope, 11, usize::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(scene.write(&ticket).observed_at_ms(), 16);
}
