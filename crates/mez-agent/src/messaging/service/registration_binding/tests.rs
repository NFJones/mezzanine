//! Actual lower source/cold/native/end capabilities with trusted fixture ports.
//! Physical original-store and private-context qualification belong to product
//! controls; decoding namespaces alone never supplies an end or live owner.

use super::*;

/// Captures real accepted bytes and seals only the explicit lower fixture ports.
#[derive(Default)]
struct Store {
    source: Vec<u8>,
}
impl MessageAcceptanceStore for Store {
    fn commit(
        &mut self,
        write: &MessageAcceptanceWrite,
    ) -> std::result::Result<(), MessageAcceptanceStoreError> {
        self.source = serde_json::to_vec(write).unwrap();
        Ok(())
    }
}
impl MessageNativeContinuityStore for Store {
    fn commit(&mut self, write: &MessageNativeContinuityWrite) -> Result<()> {
        write.snapshot().validate()
    }
}
impl MessageRegistrationEndStore for Store {
    fn commit(&mut self, write: &MessageRegistrationEndWrite) -> Result<()> {
        write.snapshot().validate()
    }
}
impl MessageStorageRecoveryStore for Store {
    fn recover(&mut self, target: &mut MessageStorageRecovery) -> Result<u64> {
        use sha2::Digest;
        target.restore_original(
            MessageStorageRecoveredRecord {
                sequence: 1,
                source_sha256: sha2::Sha256::digest(&self.source).into(),
                expired_receipt: false,
                expiry_observed_at_ms: None,
                terminal_observations: vec![],
                terminal_releases: vec![],
                terminal_consumptions: vec![],
                horizon_observed_at_ms: None,
            },
            [],
            |limits| decode_message_acceptance(self.source.as_slice(), limits),
        )?;
        Ok(2)
    }
}
/// Supplies an actual prior native publication through a trusted mock port.
struct Native(MessageNativeContinuitySnapshot);
impl MessageNativeRecoveryStore for Native {
    fn recover(&mut self, target: &mut MessageNativeRecovery) -> Result<()> {
        target.restore_binding(MessageNativeRecoveryRecord {
            snapshot: self.0.clone(),
            admission_available: true,
            delivery_available: true,
            observed_at_ms: 200,
        })
    }
}

/// A namespace can have a raw source before becoming a native application in
/// the same actual registration. Qualified native recovery keeps it live, so
/// raw absence alone cannot end it. Its subsequent genuine end adoption must
/// reach that old cold source marker and permit exact raw retirement afterward.
#[test]
fn cold_raw_source_observes_later_actual_end_of_restored_native_namespace() {
    let mut original = MessageService::default();
    let sender = original.register_agent(None, None, "sender", vec![]);
    let target = original.register_agent(None, None, "target", vec![]);
    let original_scope = MessageStorageScope::default();
    original
        .enable_storage_custody(&original_scope, Default::default())
        .unwrap();
    original.require_native_continuity(&original_scope).unwrap();
    let envelope = Envelope {
        protocol: MMP_PROTOCOL,
        id: "raw-before-native".into(),
        message_type: "send".into(),
        time: "original".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "original".into(),
        extension_fields: vec![],
    };
    let accepted = original
        .prepare_acceptance(
            &sender.agent_id,
            envelope,
            MessageScope::Session,
            100,
            4096,
            None,
        )
        .unwrap();
    let mut store = Store::default();
    let write = original
        .begin_storage_acceptance_write(&accepted, &original_scope)
        .unwrap();
    let MessageAcceptanceWriteOutcome::Committed(commit) = write.persist(&mut store) else {
        panic!("actual original required");
    };
    original.publish_acceptance(&commit).unwrap();
    drop(accepted);
    drop(commit);
    let registration = original
        .capture_sender_registration(&target.agent_id)
        .unwrap();
    original
        .bind_native_conversation(
            &registration,
            "converted-native",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    let native = original
        .prepare_native_continuity(&original_scope, &target.agent_id)
        .unwrap();
    let snapshot = native.write().snapshot().clone();
    let verified = native.write().persist(&mut store).unwrap();
    native.finish(&mut original, verified).unwrap();
    drop(registration);
    drop(original);
    let scope = MessageStorageScope::default();
    let mut recovered = MessageService::recover_storage_custody(
        &scope,
        Default::default(),
        1000,
        1024 * 1024,
        &mut store,
    )
    .unwrap();
    recovered.require_native_continuity(&scope).unwrap();
    assert!(
        recovered
            .prepare_storage_raw_retirement(&scope, 200, usize::MAX)
            .unwrap()
            .is_none(),
        "generic decoded absence is not an end"
    );
    let (mut recovered, bindings) = recovered
        .recover_native_context(&scope, &mut Native(snapshot))
        .unwrap();
    drop(bindings);
    assert!(recovered.registered_identity(&target.agent_id).is_some());
    assert!(!recovered.has_pending_registration_end().unwrap());
    assert!(
        recovered
            .prepare_storage_raw_retirement(&scope, 200, usize::MAX)
            .unwrap()
            .is_none()
    );
    recovered.retire_agent_identity(&target.agent_id);
    let end = recovered.prepare_registration_end(&scope).unwrap().unwrap();
    let verified = end.write().persist(&mut store).unwrap();
    end.finish(&mut recovered, verified).unwrap();
    let ticket = recovered
        .prepare_storage_raw_retirement(&scope, 200, usize::MAX)
        .unwrap()
        .unwrap();
    let write = recovered
        .begin_storage_terminal_write(&ticket, &scope)
        .unwrap();
    assert_eq!(
        write.raw_retired_recipient().unwrap().agent_id,
        target.agent_id.as_str()
    );
    assert_eq!(
        write.raw_retired_recipient().unwrap().ended_after_sequence,
        1
    );
}
