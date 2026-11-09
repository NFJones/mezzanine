//! Actual token capture and exact native journal publication controls.

use super::*;

/// Trusted synchronous test port; actual SQLite/fence controls live in product.
struct Store;
impl MessageNativeContinuityStore for Store {
    fn commit(&mut self, write: &MessageNativeContinuityWrite) -> Result<()> {
        write.snapshot().validate()
    }
}

/// Creates a native binding from actual registration and opaque shell tokens.
fn scene() -> (
    MessageService,
    MessageStorageScope,
    SenderIdentity,
    MessageSenderRegistration,
    MessageNativeIncarnation,
) {
    let mut service = MessageService::default();
    let target = service.register_agent(None, None, "native", vec![]);
    let scope = MessageStorageScope::default();
    service
        .enable_storage_custody(&scope, Default::default())
        .unwrap();
    let registration = service
        .capture_sender_registration(&target.agent_id)
        .unwrap();
    let incarnation = MessageNativeIncarnation::default();
    service
        .bind_native_conversation(&registration, "conversation", &incarnation)
        .unwrap();
    (service, scope, target, registration, incarnation)
}

/// Captured raw tuples remain inert; only the exact real port completion may
/// mark the current actual binding. Objective telemetry is excluded and cannot
/// replace the namespace or force another continuity event.
#[test]
fn exact_native_journal_completion_marks_only_actual_current_binding() {
    let (mut service, scope, target, _, _) = scene();
    assert!(!service.native_continuity_is_recorded(&target.agent_id));
    let request = service
        .prepare_native_continuity(&scope, &target.agent_id)
        .unwrap();
    let snapshot = request.write().snapshot().clone();
    let decoded: MessageNativeContinuitySnapshot =
        serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
    decoded.validate().unwrap();
    assert!(!service.native_continuity_is_recorded(&target.agent_id));
    let verified = request.write().persist(&mut Store).unwrap();
    assert!(request.finish(&mut service, verified).unwrap());
    service
        .update_agent_objective(&target.agent_id, Some("changed objective"), 1)
        .unwrap();
    assert!(service.native_continuity_is_recorded(&target.agent_id));
    let refreshed = service
        .prepare_native_continuity(&scope, &target.agent_id)
        .unwrap();
    assert_eq!(refreshed.write().snapshot(), &snapshot);
}

/// Enabling guarded admission must invalidate an earlier unguarded completion,
/// whether its worker has already joined or returns afterward. Only a freshly
/// captured guarded publication can open the barrier for the same native tuple.
#[test]
fn guarded_native_barrier_requires_a_guarded_capture_even_for_the_same_binding() {
    for already_joined in [false, true] {
        let (mut service, scope, target, _, _) = scene();
        let request = service
            .prepare_native_continuity(&scope, &target.agent_id)
            .unwrap();
        assert!(!request.write().snapshot().admission_guarded);
        let verified = request.write().persist(&mut Store).unwrap();
        let pending = if already_joined {
            assert!(request.finish(&mut service, verified).unwrap());
            None
        } else {
            Some((request, verified))
        };
        service.require_native_continuity(&scope).unwrap();
        if let Some((request, verified)) = pending {
            assert!(!request.finish(&mut service, verified).unwrap());
        }
        assert!(!service.native_continuity_is_recorded(&target.agent_id));
        assert_eq!(service.pending_native_continuity(), Some(&target.agent_id));
        let guarded = service
            .prepare_native_continuity(&scope, &target.agent_id)
            .unwrap();
        assert!(guarded.write().snapshot().admission_guarded);
        let verified = guarded.write().persist(&mut Store).unwrap();
        assert!(guarded.finish(&mut service, verified).unwrap());
        service.require_native_continuity(&scope).unwrap();
        assert!(service.native_continuity_is_recorded(&target.agent_id));
    }
}

/// Identical public data from another capture cannot substitute for this exact
/// request. The original completion remains usable by its actual input owner.
#[test]
fn equal_native_snapshot_data_cannot_steal_another_capture_completion() {
    let (mut service, scope, target, _, _) = scene();
    let a = service
        .prepare_native_continuity(&scope, &target.agent_id)
        .unwrap();
    let b = service
        .prepare_native_continuity(&scope, &target.agent_id)
        .unwrap();
    assert_eq!(a.write().snapshot(), b.write().snapshot());
    let verified = a.write().persist(&mut Store).unwrap();
    assert!(b.finish(&mut service, verified).is_err());
    assert!(!service.native_continuity_is_recorded(&target.agent_id));
    let verified = a.write().persist(&mut Store).unwrap();
    assert!(a.finish(&mut service, verified).unwrap());
}

/// An independent shell binding with equal labels/floor cannot inherit a late
/// journal result. Exact opaque epoch identity is required for live publication.
#[test]
fn late_native_continuity_does_not_mark_replacement_shell() {
    let (mut service, scope, target, registration, _) = scene();
    let request = service
        .prepare_native_continuity(&scope, &target.agent_id)
        .unwrap();
    let verified = request.write().persist(&mut Store).unwrap();
    service
        .bind_native_conversation(
            &registration,
            "conversation",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    assert!(!request.finish(&mut service, verified).unwrap());
    assert!(!service.native_continuity_is_recorded(&target.agent_id));
}

/// Sparse immutable authority reconciliation can replace the actual MMP
/// namespace without changing shell epoch/CID or advancing any message floor.
/// A saved shell epoch alone therefore cannot prove registration continuity.
#[test]
fn namespace_replacement_at_same_epoch_and_floor_requires_its_own_fact() {
    let (mut service, scope, mut target, _, incarnation) = scene();
    let old = service
        .prepare_native_continuity(&scope, &target.agent_id)
        .unwrap();
    let before = old.write().snapshot().clone();
    let verified = old.write().persist(&mut Store).unwrap();
    target.window_id = Some(StableId::opaque("window").unwrap());
    service.ensure_agent_identity(target.clone(), 1).unwrap();
    let registration = service
        .capture_sender_registration(&target.agent_id)
        .unwrap();
    service
        .bind_native_conversation(&registration, "conversation", &incarnation)
        .unwrap();
    let new = service
        .prepare_native_continuity(&scope, &target.agent_id)
        .unwrap();
    assert_eq!(
        before.native.shell_incarnation,
        new.write().snapshot().native.shell_incarnation
    );
    assert_eq!(
        before.registered_after_sequence,
        new.write().snapshot().registered_after_sequence
    );
    assert_ne!(
        before.native.registration_namespace,
        new.write().snapshot().native.registration_namespace
    );
    assert!(!old.finish(&mut service, verified).unwrap());
    assert!(!service.native_continuity_is_recorded(&target.agent_id));
    let verified = new.write().persist(&mut Store).unwrap();
    assert!(new.finish(&mut service, verified).unwrap());
}

/// Worker-held metadata must stay reserved after a failed premature adoption.
/// It cannot disappear from the cap while the real input still has another owner.
#[test]
fn native_capture_lease_survives_an_unjoined_worker_copy() {
    let (mut service, scope, target, _, _) = scene();
    let before = service.storage_usage().unwrap().unwrap().metadata_bytes;
    let request = service
        .prepare_native_continuity(&scope, &target.agent_id)
        .unwrap();
    let extra = request.write();
    let verified = request.write().persist(&mut Store).unwrap();
    assert!(request.finish(&mut service, verified).is_err());
    assert!(
        service
            .storage_custody
            .as_ref()
            .unwrap()
            .worker_metadata_loan
            .load(std::sync::atomic::Ordering::Acquire)
    );
    assert!(service.storage_usage().unwrap().unwrap().metadata_bytes >= before);
    drop(extra);
    assert!(
        !service
            .storage_custody
            .as_ref()
            .unwrap()
            .worker_metadata_loan
            .load(std::sync::atomic::Ordering::Acquire)
    );
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        before
    );
    assert!(!service.native_continuity_is_recorded(&target.agent_id));
}

/// Raw endpoints and foreign roots cannot be promoted by a continuity capture.
/// This refusal creates no native owner and leaves metadata accounting unchanged.
#[test]
fn raw_or_foreign_scope_cannot_capture_native_continuity() {
    let (mut service, scope, target, _, _) = scene();
    let raw = service.register_agent(None, None, "raw", vec![]);
    let before = service.storage_usage().unwrap().unwrap();
    assert!(
        service
            .prepare_native_continuity(&scope, &raw.agent_id)
            .is_err()
    );
    assert!(
        service
            .prepare_native_continuity(&MessageStorageScope::default(), &target.agent_id)
            .is_err()
    );
    assert_eq!(service.storage_usage().unwrap().unwrap(), before);
}

/// A real native sender/recipient cannot consume sequence, idempotency, payload
/// or metadata reservations while its registration journal is missing. Recording
/// the exact interval opens admission; a replacement immediately closes it again.
#[test]
fn required_native_journal_precedes_every_new_source_reservation() {
    let (mut service, scope, native, registration, _) = scene();
    let raw = service.register_agent(None, None, "raw", vec![]);
    assert!(
        service
            .require_native_continuity(&MessageStorageScope::default())
            .is_err()
    );
    assert!(service.pending_native_continuity().is_none());
    service.require_native_continuity(&scope).unwrap();
    assert_eq!(service.pending_native_continuity(), Some(&native.agent_id));
    for (sender, target) in [(&raw, &native), (&native, &raw), (&raw, &raw)] {
        let before = service.storage_usage().unwrap().unwrap();
        assert!(
            service
                .prepare_acceptance(
                    &sender.agent_id,
                    mail(sender, target),
                    MessageScope::Session,
                    1,
                    4096,
                    None
                )
                .is_err()
        );
        assert_eq!(service.storage_usage().unwrap().unwrap(), before);
        assert_eq!(service.next_message_sequence(), 1);
    }
    let request = service
        .prepare_native_continuity(&scope, &native.agent_id)
        .unwrap();
    let verified = request.write().persist(&mut Store).unwrap();
    assert!(request.finish(&mut service, verified).unwrap());
    assert!(service.pending_native_continuity().is_none());
    let ticket = service
        .prepare_acceptance(
            &raw.agent_id,
            mail(&raw, &native),
            MessageScope::Session,
            1,
            4096,
            None,
        )
        .unwrap();
    assert_eq!(ticket.sequence(), 1);
    service.cancel_acceptance(&ticket).unwrap();
    service
        .bind_native_conversation(
            &registration,
            "conversation",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    assert_eq!(service.pending_native_continuity(), Some(&native.agent_id));
    assert!(
        service
            .prepare_acceptance(
                &raw.agent_id,
                mail(&raw, &native),
                MessageScope::Session,
                1,
                4096,
                None
            )
            .is_err()
    );
    assert_eq!(service.next_message_sequence(), 2);
}

/// Barrier setup must not change the semantics of an already reserved source.
/// It is a trusted settled-runtime transition, not a saved/configurable flag.
#[test]
fn native_barrier_enable_refuses_an_unsettled_original_without_mutation() {
    let (mut service, scope, native, _, _) = scene();
    let raw = service.register_agent(None, None, "raw", vec![]);
    let ticket = service
        .prepare_acceptance(
            &raw.agent_id,
            mail(&raw, &native),
            MessageScope::Session,
            1,
            4096,
            None,
        )
        .unwrap();
    let before = service.storage_usage().unwrap().unwrap();
    assert!(service.require_native_continuity(&scope).is_err());
    assert!(service.pending_native_continuity().is_none());
    assert_eq!(service.storage_usage().unwrap().unwrap(), before);
    service.cancel_acceptance(&ticket).unwrap();
    service.require_native_continuity(&scope).unwrap();
    assert!(service.pending_native_continuity().is_some());
}

/// Small immutable envelope shared by both barrier admission controls.
fn mail(sender: &SenderIdentity, target: &SenderIdentity) -> Envelope {
    Envelope {
        protocol: "mmp/1",
        id: "barrier-original".into(),
        message_type: "send".into(),
        time: "fixture".into(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".into(),
        payload: "original".into(),
        extension_fields: vec![],
    }
}
