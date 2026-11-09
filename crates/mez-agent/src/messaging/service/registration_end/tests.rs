//! Actual registration lifecycle, bounded end capture and positive ownership.

use super::*;

/// Trusted state-machine port; product tests separately exercise real SQLite.
struct Store;
impl MessageNativeContinuityStore for Store {
    fn commit(&mut self, write: &MessageNativeContinuityWrite) -> Result<()> {
        write.snapshot().validate()
    }
}

/// Another actual worker copy prevents adoption without consuming either the
/// retained original request or its positive. Dropping that copy permits the
/// same pair to finish; a refusal never strands the physical root's input.
#[test]
fn registration_end_adoption_preflight_retains_original_with_worker_copy() {
    let (mut service, scope, agent, _) = scene();
    service.retire_agent_identity(&agent.agent_id);
    let request = service.prepare_registration_end(&scope).unwrap().unwrap();
    let retained = request.write();
    let verified = request.write().persist(&mut Store).unwrap();
    assert!(request.qualify_adoption(&service, &verified).is_err());
    assert!(service.has_pending_registration_end().unwrap());
    drop(retained);
    request.qualify_adoption(&service, &verified).unwrap();
    assert!(request.finish(&mut service, verified).unwrap());
    assert!(!service.has_pending_registration_end().unwrap());
}
impl MessageRegistrationEndStore for Store {
    fn commit(&mut self, write: &MessageRegistrationEndWrite) -> Result<()> {
        write.snapshot().validate()
    }
}

/// Creates a genuinely guarded native namespace with its live journal marker.
fn scene() -> (
    MessageService,
    MessageStorageScope,
    SenderIdentity,
    MessageNativeIncarnation,
) {
    let mut service = MessageService::default();
    let agent = service.register_agent(None, None, "native", vec![]);
    let scope = MessageStorageScope::default();
    service
        .enable_storage_custody(&scope, Default::default())
        .unwrap();
    service.require_native_continuity(&scope).unwrap();
    let incarnation = MessageNativeIncarnation::default();
    let registration = service
        .capture_sender_registration(&agent.agent_id)
        .unwrap();
    service
        .bind_native_conversation(&registration, "parent", &incarnation)
        .unwrap();
    let request = service
        .prepare_native_continuity(&scope, &agent.agent_id)
        .unwrap();
    let verified = request.write().persist(&mut Store).unwrap();
    assert!(request.finish(&mut service, verified).unwrap());
    (service, scope, agent, incarnation)
}

/// Parent/child/parent changes preserve the actual namespace and never produce
/// an end. Actual identity removal freezes its first cutoff; journal adoption
/// alone releases the retained intent without recreating the identity.
#[test]
fn registration_end_requires_actual_removal_and_never_shell_displacement() {
    let (mut service, scope, agent, parent) = scene();
    let registration = service
        .capture_sender_registration(&agent.agent_id)
        .unwrap();
    let namespace = *registration.identity.native_namespace.as_bytes();
    service
        .bind_native_conversation(&registration, "child", &MessageNativeIncarnation::default())
        .unwrap();
    service
        .bind_native_conversation(&registration, "parent", &parent)
        .unwrap();
    assert!(!service.has_pending_registration_end().unwrap());
    assert!(service.prepare_registration_end(&scope).unwrap().is_none());
    assert!(service.retire_agent_identity(&agent.agent_id));
    assert!(!service.retire_agent_identity(&agent.agent_id));
    let request = service.prepare_registration_end(&scope).unwrap().unwrap();
    let snapshot = request.write().snapshot().clone();
    assert_eq!(snapshot.registration_namespace, namespace);
    assert_eq!(snapshot.registered_after_sequence, 0);
    assert_eq!(snapshot.ended_after_sequence, 0);
    let bytes = serde_json::to_vec(&snapshot).unwrap();
    let decoded: MessageRegistrationEndSnapshot = serde_json::from_slice(&bytes).unwrap();
    decoded.validate().unwrap();
    assert!(service.has_pending_registration_end().unwrap());
    let verified = request.write().persist(&mut Store).unwrap();
    assert!(request.finish(&mut service, verified).unwrap());
    assert!(!service.has_pending_registration_end().unwrap());
    assert!(service.registered_identity(&agent.agent_id).is_none());
}

/// Sparse immutable reconciliation ends the old allocation even at equal
/// public id, floor and native shell epoch. A native worker still holding the
/// old capture cannot mark its successor or erase the original end intent.
#[test]
fn registration_end_survives_authority_replacement_during_native_capture() {
    let (mut service, scope, mut agent, epoch) = scene();
    let old = service
        .prepare_native_continuity(&scope, &agent.agent_id)
        .unwrap();
    let snapshot = old.write().snapshot().clone();
    let verified = old.write().persist(&mut Store).unwrap();
    service
        .update_agent_objective(&agent.agent_id, Some("telemetry"), 1)
        .unwrap();
    assert!(!service.has_pending_registration_end().unwrap());
    agent.window_id = Some(StableId::opaque("other-window").unwrap());
    service.ensure_agent_identity(agent.clone(), 2).unwrap();
    let replacement = service
        .capture_sender_registration(&agent.agent_id)
        .unwrap();
    service
        .bind_native_conversation(&replacement, "parent", &epoch)
        .unwrap();
    assert!(!old.finish(&mut service, verified).unwrap());
    assert!(!service.native_continuity_is_recorded(&agent.agent_id));
    let end = service.prepare_registration_end(&scope).unwrap().unwrap();
    assert_eq!(
        end.write().snapshot().registration_namespace,
        snapshot.native.registration_namespace
    );
    assert_ne!(
        end.write().snapshot().registration_namespace,
        *replacement.identity.native_namespace.as_bytes()
    );
    let verified = end.write().persist(&mut Store).unwrap();
    assert!(end.finish(&mut service, verified).unwrap());
    assert!(service.sender_registration_is_current(&replacement));
}

/// Equal descriptive end tuples from two captures cannot exchange exact
/// completions. Foreign original roots cannot prepare or adopt this allocation.
#[test]
fn registration_end_requires_original_scope_and_exact_capture_completion() {
    let (mut service, scope, agent, _) = scene();
    service.retire_agent_identity(&agent.agent_id);
    assert!(
        service
            .prepare_registration_end(&MessageStorageScope::default())
            .is_err()
    );
    let first = service.prepare_registration_end(&scope).unwrap().unwrap();
    let other = service.prepare_registration_end(&scope).unwrap().unwrap();
    assert_eq!(first.write().snapshot(), other.write().snapshot());
    assert!(!first.belongs_to_service(&MessageService::default()));
    let verified = first.write().persist(&mut Store).unwrap();
    assert!(other.finish(&mut service, verified).is_err());
    assert!(service.has_pending_registration_end().unwrap());
    let verified = first.write().persist(&mut Store).unwrap();
    assert!(first.finish(&mut service, verified).unwrap());
}

/// The registry reserves exactly once per actual namespace, and keeps that
/// charge after the live identity disappears. An unjoined write keeps its full
/// scratch reservation after failed early adoption; the final real owner frees
/// it, and later exact journal adoption releases the original intent as well.
#[test]
fn registration_end_metadata_survives_last_live_owner_and_unjoined_worker() {
    let (mut service, scope, agent, _) = scene();
    let baseline = service.storage_usage().unwrap().unwrap().metadata_bytes;
    let request = service
        .prepare_native_continuity(&scope, &agent.agent_id)
        .unwrap();
    drop(request);
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        baseline
    );
    service.retire_agent_identity(&agent.agent_id);
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        baseline
    );
    let request = service.prepare_registration_end(&scope).unwrap().unwrap();
    let extra = request.write();
    let verified = request.write().persist(&mut Store).unwrap();
    assert!(request.finish(&mut service, verified).is_err());
    assert!(
        service
            .storage_custody
            .as_ref()
            .unwrap()
            .worker_metadata_loan
            .load(Ordering::Acquire)
    );
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        baseline
    );
    drop(extra);
    assert!(
        !service
            .storage_custody
            .as_ref()
            .unwrap()
            .worker_metadata_loan
            .load(Ordering::Acquire)
    );
    assert_eq!(
        service.storage_usage().unwrap().unwrap().metadata_bytes,
        baseline
    );
    let request = service.prepare_registration_end(&scope).unwrap().unwrap();
    let verified = request.write().persist(&mut Store).unwrap();
    request.finish(&mut service, verified).unwrap();
    assert!(service.storage_usage().unwrap().unwrap().metadata_bytes < baseline);
}

/// Primitive unguarded publication has no reserved retirement intent. Guarding
/// and actually capturing that native namespace establishes the bounded owner;
/// missing already-ended historical registrations are never synthesized.
#[test]
fn registration_end_does_not_invent_unguarded_or_missing_native_history() {
    let mut service = MessageService::default();
    let id = service
        .register_agent(None, None, "native", vec![])
        .agent_id;
    let scope = MessageStorageScope::default();
    service
        .enable_storage_custody(&scope, Default::default())
        .unwrap();
    let registration = service.capture_sender_registration(&id).unwrap();
    service
        .bind_native_conversation(
            &registration,
            "parent",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    drop(service.prepare_native_continuity(&scope, &id).unwrap());
    service.retire_agent_identity(&id);
    service.require_native_continuity(&scope).unwrap();
    assert!(!service.has_pending_registration_end().unwrap());
    assert!(service.prepare_registration_end(&scope).unwrap().is_none());
}
