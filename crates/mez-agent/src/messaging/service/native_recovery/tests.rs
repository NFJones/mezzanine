//! One-use native target/epoch ownership; product tests prove physical contexts.

use super::*;

/// Minimal trusted cold port; no fabricated source is added to this state test.
struct Cold;
impl MessageStorageRecoveryStore for Cold {
    fn recover(&mut self, _: &mut MessageStorageRecovery) -> Result<u64> {
        Ok(11)
    }
}

/// Exercises the trusted callback contract without claiming filesystem proof.
struct Native {
    snapshot: MessageNativeContinuitySnapshot,
    calls: usize,
    swallow: bool,
    duplicate: bool,
}
impl MessageNativeRecoveryStore for Native {
    fn recover(&mut self, target: &mut MessageNativeRecovery) -> Result<()> {
        self.calls += 1;
        let mut result = target.restore_binding(MessageNativeRecoveryRecord {
            snapshot: self.snapshot.clone(),
            admission_available: true,
            delivery_available: true,
            observed_at_ms: 50,
        });
        if result.is_ok() && self.duplicate {
            result = target.restore_binding(MessageNativeRecoveryRecord {
                snapshot: self.snapshot.clone(),
                admission_available: true,
                delivery_available: true,
                observed_at_ms: 50,
            });
        }
        if self.swallow { Ok(()) } else { result }
    }
}

fn native(role: &str) -> Native {
    let mut service = MessageService::default();
    let identity = service.register_agent(None, None, role, vec![]);
    let scope = MessageStorageScope::default();
    service
        .enable_storage_custody(&scope, Default::default())
        .unwrap();
    service.require_native_continuity(&scope).unwrap();
    let registration = service
        .capture_sender_registration(&identity.agent_id)
        .unwrap();
    service
        .bind_native_conversation(
            &registration,
            "original",
            &MessageNativeIncarnation::default(),
        )
        .unwrap();
    let capture = service
        .prepare_native_continuity(&scope, &identity.agent_id)
        .unwrap();
    let snapshot = capture.write().snapshot().clone();
    Native {
        snapshot,
        calls: 0,
        swallow: false,
        duplicate: false,
    }
}

fn cold(scope: &MessageStorageScope, limits: MessageStorageLimits) -> MessageService {
    MessageService::recover_storage_custody(scope, limits, 1000, 1024 * 1024, &mut Cold).unwrap()
}

/// Only a one-use actual cold target can call the trusted native port. Fresh,
/// foreign-root and already-consumed owners cannot manufacture epochs from DTOs.
#[test]
fn native_restore_requires_one_use_cold_owner_and_exact_scope() {
    let scope = MessageStorageScope::default();
    let mut port = native("native");
    let mut fresh = MessageService::default();
    fresh
        .enable_storage_custody(&scope, Default::default())
        .unwrap();
    assert!(fresh.recover_native_context(&scope, &mut port).is_err());
    assert_eq!(port.calls, 0);
    assert!(
        cold(&scope, Default::default())
            .recover_native_context(&MessageStorageScope::default(), &mut port)
            .is_err()
    );
    assert_eq!(port.calls, 0);
    let (restored, epochs) = cold(&scope, Default::default())
        .recover_native_context(&scope, &mut port)
        .unwrap();
    assert!(restored.recover_native_context(&scope, &mut port).is_err());
    assert_eq!(port.calls, 1);
    drop(epochs);
}

/// Native restoration preserves original namespace, shell and interval floor
/// rather than allocating at the recovered high-water. No raw identity returns,
/// and subsequent ID allocation cannot collide with a restored native endpoint.
#[test]
fn qualified_native_endpoint_preserves_original_epoch_namespace_floor_and_ids() {
    let scope = MessageStorageScope::default();
    let mut port = native("native");
    let original = port.snapshot.clone();
    let (mut restored, epochs) = cold(&scope, Default::default())
        .recover_native_context(&scope, &mut port)
        .unwrap();
    assert_eq!(epochs.len(), 1);
    let id = epochs[0].agent_id();
    assert_eq!(
        epochs[0].incarnation().snapshot_identity(),
        original.native.shell_incarnation
    );
    let registration = restored.capture_sender_registration(id).unwrap();
    assert_eq!(
        restored.native_owner_for_registration(&registration, epochs[0].incarnation()),
        Some(&original.native.owner)
    );
    let checkpoint = restored.prepare_native_continuity(&scope, id).unwrap();
    assert_eq!(
        checkpoint.write().snapshot().native.registration_namespace,
        original.native.registration_namespace
    );
    assert_eq!(checkpoint.write().snapshot().registered_after_sequence, 0);
    assert_eq!(restored.next_message_sequence(), 11);
    assert!(restored.native_continuity_is_recorded(id));
    assert!(restored.pending_native_continuity().is_none());
    assert_ne!(
        restored
            .register_agent(None, None, "new raw", vec![])
            .agent_id,
        *id
    );
}

/// A mishandled port error cannot publish a partial registry. Legacy observations,
/// unguarded captures, impossible floors and duplicate endpoints poison the whole
/// result even when the callback attempts to report a successful operation.
#[test]
fn native_restore_refusal_is_sticky_across_a_swallowed_port_error() {
    let scope = MessageStorageScope::default();
    for fault in ["legacy", "unguarded", "floor", "duplicate"] {
        let mut port = native("native");
        port.swallow = true;
        match fault {
            "legacy" => {
                port.snapshot.format_version = 1;
                port.snapshot.admission_guarded = false;
            }
            "unguarded" => port.snapshot.admission_guarded = false,
            "floor" => port.snapshot.registered_after_sequence = 11,
            "duplicate" => port.duplicate = true,
            _ => unreachable!(),
        }
        assert!(
            cold(&scope, Default::default())
                .recover_native_context(&scope, &mut port)
                .is_err(),
            "{fault}"
        );
    }
}

/// Result handles retain their own finite metadata reservation until actual
/// product adoption/drop; they cannot disappear from accounting while live.
#[test]
fn recovered_native_epoch_handle_retains_metadata_until_drop() {
    let scope = MessageStorageScope::default();
    let mut port = native("native");
    let (service, handles) = cold(&scope, Default::default())
        .recover_native_context(&scope, &mut port)
        .unwrap();
    let before = service.storage_usage().unwrap().unwrap().metadata_bytes;
    drop(handles);
    assert!(service.storage_usage().unwrap().unwrap().metadata_bytes < before);
}

/// Narrowed policy refuses copied native identity metadata before validation or
/// registry allocation, while the original backend/observations remain intact.
#[test]
fn native_restore_respects_destination_metadata_budget() {
    let scope = MessageStorageScope::default();
    let mut port = native(&"n".repeat(64 * 1024));
    let original = port.snapshot.clone();
    let limits = MessageStorageLimits {
        metadata_bytes: super::super::storage_policy::WORKER_HEADROOM + 512 * 1024,
        ..Default::default()
    };
    assert!(
        cold(&scope, limits)
            .recover_native_context(&scope, &mut port)
            .is_err()
    );
    assert_eq!(port.snapshot, original);
}
