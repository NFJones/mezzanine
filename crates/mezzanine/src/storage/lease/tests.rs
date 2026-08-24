//! Durable lease repository regression coverage.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::error::MezErrorKind;

static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

/// Principal-scoped retries must return the original reservation while reuse
/// with different normalized creation inputs fails without adding authority.
#[test]
fn lease_reservation_is_idempotent_and_rejects_conflicting_reuse() {
    let root = test_root("idempotency");
    let repository = RemoteSessionLeaseRepository::new(root.clone());
    let request = reservation("lease-1", "$1", "device-1", "create-1", "fingerprint-1");

    let created = repository.reserve_pending(request.clone()).unwrap();
    assert!(matches!(created, LeaseReservation::Created(_)));
    let replay = repository.reserve_pending(request).unwrap();
    assert!(matches!(replay, LeaseReservation::Replay(_)));
    assert_eq!(created.lease(), replay.lease());

    let conflict = repository
        .reserve_pending(reservation(
            "lease-2",
            "$2",
            "device-1",
            "create-1",
            "different-fingerprint",
        ))
        .unwrap_err();
    assert_eq!(conflict.kind(), MezErrorKind::Conflict);
    assert_eq!(repository.list().unwrap().len(), 1);

    let _ = fs::remove_dir_all(root);
}

/// Legal transitions advance the lease generation, reject stale callbacks,
/// and accept only checkpoints belonging to the exact leased session.
#[test]
fn lease_transitions_are_generation_fenced_and_checkpoint_bound() {
    let root = test_root("transitions");
    let repository = RemoteSessionLeaseRepository::new(root.clone());
    let pending = repository
        .reserve_pending(reservation(
            "lease-1",
            "$1",
            "device-1",
            "create-1",
            "fingerprint-1",
        ))
        .unwrap()
        .lease()
        .clone();
    let active = repository
        .activate(
            &pending.lease_id,
            pending.boot_generation,
            pending.lease_generation,
            11,
        )
        .unwrap();
    assert_eq!(active.state, RemoteSessionLeaseState::Active);

    let stale = repository
        .mark_recoverable(
            &active.lease_id,
            active.boot_generation,
            pending.lease_generation,
            12,
        )
        .unwrap_err();
    assert_eq!(stale.kind(), MezErrorKind::Conflict);

    let mismatched = repository
        .update_checkpoint(
            &active.lease_id,
            active.boot_generation,
            active.lease_generation,
            checkpoint("snapshot-1", "$other"),
            12,
        )
        .unwrap_err();
    assert_eq!(mismatched.kind(), MezErrorKind::Conflict);

    let checkpointed = repository
        .update_checkpoint(
            &active.lease_id,
            active.boot_generation,
            active.lease_generation,
            checkpoint("snapshot-1", &active.session_id),
            12,
        )
        .unwrap();
    let recoverable = repository
        .mark_recoverable(
            &checkpointed.lease_id,
            checkpointed.boot_generation,
            checkpointed.lease_generation,
            13,
        )
        .unwrap();
    assert_eq!(recoverable.state, RemoteSessionLeaseState::Recoverable);
    assert_eq!(recoverable.checkpoint.unwrap().snapshot_id, "snapshot-1");

    let restored = repository
        .activate(
            &recoverable.lease_id,
            recoverable.boot_generation,
            recoverable.lease_generation,
            14,
        )
        .unwrap();
    assert_eq!(restored.state, RemoteSessionLeaseState::Active);

    let _ = fs::remove_dir_all(root);
}

/// Replacing a checkpoint and collecting its terminal lease must preserve
/// durable snapshot cleanup work until deletion is acknowledged, while an
/// identifier still referenced by another lease remains fenced from cleanup.
#[test]
fn checkpoint_replacement_and_gc_persist_cleanup_candidates() {
    let root = test_root("snapshot-cleanup");
    let repository = RemoteSessionLeaseRepository::new(root.clone());
    let first_pending = repository
        .reserve_pending(reservation(
            "lease-first",
            "$1",
            "device-1",
            "create-first",
            "fingerprint-first",
        ))
        .unwrap()
        .lease()
        .clone();
    let first = repository
        .activate(
            &first_pending.lease_id,
            first_pending.boot_generation,
            first_pending.lease_generation,
            11,
        )
        .unwrap();
    let first = repository
        .update_checkpoint(
            &first.lease_id,
            first.boot_generation,
            first.lease_generation,
            checkpoint("snapshot-old", &first.session_id),
            12,
        )
        .unwrap();
    let first = repository
        .update_checkpoint(
            &first.lease_id,
            first.boot_generation,
            first.lease_generation,
            checkpoint("snapshot-shared", &first.session_id),
            13,
        )
        .unwrap();
    assert_eq!(
        repository.snapshot_cleanup_candidates().unwrap(),
        vec!["snapshot-old"]
    );

    let second_pending = repository
        .reserve_pending(reservation(
            "lease-second",
            "$2",
            "device-2",
            "create-second",
            "fingerprint-second",
        ))
        .unwrap()
        .lease()
        .clone();
    let second = repository
        .activate(
            &second_pending.lease_id,
            second_pending.boot_generation,
            second_pending.lease_generation,
            14,
        )
        .unwrap();
    let cleanup_race = repository
        .update_checkpoint(
            &second.lease_id,
            second.boot_generation,
            second.lease_generation,
            checkpoint("snapshot-old", &second.session_id),
            15,
        )
        .unwrap_err();
    assert_eq!(cleanup_race.kind(), MezErrorKind::Conflict);
    repository
        .update_checkpoint(
            &second.lease_id,
            second.boot_generation,
            second.lease_generation,
            checkpoint("snapshot-shared", &second.session_id),
            15,
        )
        .unwrap();
    let released = repository
        .release(
            &first.lease_id,
            first.boot_generation,
            first.lease_generation,
            16,
        )
        .unwrap();
    assert_eq!(released.state, RemoteSessionLeaseState::Released);
    repository
        .apply_gc(LeaseGarbageCollectionPolicy {
            released_before_unix_seconds: 16,
            revoked_before_unix_seconds: 16,
            failed_before_unix_seconds: 16,
        })
        .unwrap();
    assert_eq!(
        repository.snapshot_cleanup_candidates().unwrap(),
        vec!["snapshot-old", "snapshot-shared"]
    );
    assert!(
        !repository
            .acknowledge_snapshot_cleanup("snapshot-shared")
            .unwrap()
    );
    assert!(
        repository
            .acknowledge_snapshot_cleanup("snapshot-old")
            .unwrap()
    );
    assert_eq!(
        repository.snapshot_cleanup_candidates().unwrap(),
        vec!["snapshot-shared"]
    );

    let _ = fs::remove_dir_all(root);
}

/// Advancing the boot generation deterministically fails interrupted pending
/// work, makes formerly active leases recoverable, and fences prior actors.
#[test]
fn boot_reconciliation_fences_prior_generation_mutations() {
    let root = test_root("restart");
    let repository = RemoteSessionLeaseRepository::new(root.clone());
    let pending = repository
        .reserve_pending(reservation(
            "lease-pending",
            "$1",
            "device-1",
            "create-1",
            "fingerprint-1",
        ))
        .unwrap()
        .lease()
        .clone();
    let pending_active = repository
        .reserve_pending(reservation(
            "lease-active",
            "$2",
            "device-1",
            "create-2",
            "fingerprint-2",
        ))
        .unwrap()
        .lease()
        .clone();
    let active = repository
        .activate(
            &pending_active.lease_id,
            pending_active.boot_generation,
            pending_active.lease_generation,
            11,
        )
        .unwrap();

    assert_eq!(repository.advance_boot_generation(20).unwrap(), 1);
    let interrupted = repository.get(&pending.lease_id).unwrap().unwrap();
    let recoverable = repository.get(&active.lease_id).unwrap().unwrap();
    assert_eq!(interrupted.state, RemoteSessionLeaseState::Failed);
    assert_eq!(recoverable.state, RemoteSessionLeaseState::Recoverable);
    assert_eq!(interrupted.boot_generation, 1);
    assert_eq!(recoverable.boot_generation, 1);

    assert_eq!(repository.advance_boot_generation(30).unwrap(), 2);
    let still_recoverable = repository.get(&active.lease_id).unwrap().unwrap();
    assert_eq!(
        still_recoverable.state,
        RemoteSessionLeaseState::Recoverable
    );
    assert_eq!(still_recoverable.boot_generation, 2);

    let stale = repository
        .mark_failed(
            &active.lease_id,
            active.boot_generation,
            active.lease_generation,
            21,
            "stale actor".to_string(),
        )
        .unwrap_err();
    assert_eq!(stale.kind(), MezErrorKind::Conflict);

    let _ = fs::remove_dir_all(root);
}

/// Garbage collection must preview exactly the eligible terminal records and
/// retain active or recoverable leases regardless of age.
#[test]
fn lease_gc_is_previewable_and_preserves_live_authority() {
    let root = test_root("gc");
    let repository = RemoteSessionLeaseRepository::new(root.clone());
    let terminal_pending = repository
        .reserve_pending(reservation(
            "lease-released",
            "$1",
            "device-1",
            "create-1",
            "fingerprint-1",
        ))
        .unwrap()
        .lease()
        .clone();
    repository
        .release(
            &terminal_pending.lease_id,
            terminal_pending.boot_generation,
            terminal_pending.lease_generation,
            10,
        )
        .unwrap();
    let live_pending = repository
        .reserve_pending(reservation(
            "lease-active",
            "$2",
            "device-1",
            "create-2",
            "fingerprint-2",
        ))
        .unwrap()
        .lease()
        .clone();
    repository
        .activate(
            &live_pending.lease_id,
            live_pending.boot_generation,
            live_pending.lease_generation,
            11,
        )
        .unwrap();
    let policy = LeaseGarbageCollectionPolicy {
        released_before_unix_seconds: 10,
        revoked_before_unix_seconds: 10,
        failed_before_unix_seconds: 10,
    };

    let preview = repository.preview_gc(policy).unwrap();
    assert_eq!(preview.lease_ids, vec!["lease-released"]);
    assert_eq!(repository.list().unwrap().len(), 2);
    assert_eq!(repository.apply_gc(policy).unwrap(), preview);
    let retained = repository.list().unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].state, RemoteSessionLeaseState::Active);

    let _ = fs::remove_dir_all(root);
}

/// Finite lease lifetimes revoke due live authority atomically while retaining
/// unlimited and already-terminal records unchanged.
#[test]
fn lease_expiry_revokes_only_due_non_terminal_authority() {
    let root = test_root("expiry");
    let repository = RemoteSessionLeaseRepository::new(root.clone());
    let mut finite_request = reservation(
        "lease-finite",
        "$1",
        "device-1",
        "create-finite",
        "fingerprint-finite",
    );
    finite_request.expires_at_unix_seconds = Some(20);
    let finite = repository
        .reserve_pending(finite_request)
        .unwrap()
        .lease()
        .clone();
    let finite = repository
        .activate(
            &finite.lease_id,
            finite.boot_generation,
            finite.lease_generation,
            11,
        )
        .unwrap();
    let unlimited = repository
        .reserve_pending(reservation(
            "lease-unlimited",
            "$2",
            "device-2",
            "create-unlimited",
            "fingerprint-unlimited",
        ))
        .unwrap()
        .lease()
        .clone();

    assert!(repository.expire_due(19).unwrap().is_empty());
    let expired = repository.expire_due(20).unwrap();
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].lease_id, finite.lease_id);
    assert_eq!(expired[0].state, RemoteSessionLeaseState::Revoked);
    assert_eq!(
        expired[0].failure.as_deref(),
        Some("remote session lease lifetime expired")
    );
    assert_eq!(expired[0].terminal_at_unix_seconds, Some(20));
    assert_eq!(expired[0].lease_generation, finite.lease_generation + 1);
    assert_eq!(
        repository.get(&unlimited.lease_id).unwrap().unwrap().state,
        RemoteSessionLeaseState::Pending
    );
    assert!(repository.expire_due(21).unwrap().is_empty());

    let _ = fs::remove_dir_all(root);
}

/// Malformed durable data fails closed without being replaced or silently
/// interpreted as an empty lease database.
#[test]
fn malformed_lease_database_fails_closed() {
    let root = test_root("corrupt");
    fs::write(root.join("leases.json"), b"not-json\n").unwrap();
    fs::set_permissions(root.join("leases.json"), fs::Permissions::from_mode(0o600)).unwrap();
    let repository = RemoteSessionLeaseRepository::new(root.clone());

    let error = repository.list().unwrap_err();
    assert_eq!(error.kind(), MezErrorKind::InvalidState);
    assert_eq!(fs::read(root.join("leases.json")).unwrap(), b"not-json\n");

    let _ = fs::remove_dir_all(root);
}

/// Injected lease database publication failures preserve either the complete
/// previous database or the complete replacement and never retain a temp file.
#[test]
fn lease_database_publication_is_complete_at_every_failure_phase() {
    let root = test_root("publication-phases");
    let old = b"old complete database\n";
    let new = b"new complete database\n";
    for (index, phase) in [
        LeasePublicationFailurePhase::AfterWrite,
        LeasePublicationFailurePhase::AfterFileSync,
        LeasePublicationFailurePhase::BeforeRename,
        LeasePublicationFailurePhase::BeforeDirectorySync,
    ]
    .into_iter()
    .enumerate()
    {
        let path = root.join(format!("leases-{index}.json"));
        fs::write(&path, old).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        write_private_atomic_failing(&path, new, phase).unwrap_err();
        let expected = if phase == LeasePublicationFailurePhase::BeforeDirectorySync {
            new.as_slice()
        } else {
            old.as_slice()
        };
        assert_eq!(fs::read(&path).unwrap(), expected, "{phase:?}");
        assert!(fs::read_dir(&root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    let _ = fs::remove_dir_all(root);
}

/// Lease database and lock paths must reject symlink substitution rather than
/// following attacker-selected files outside the protected lease directory.
#[test]
fn lease_repository_rejects_symlink_database_and_lock_paths() {
    use std::os::unix::fs::symlink;

    for file_name in ["leases.json", "leases.lock"] {
        let root = test_root(&format!("symlink-{file_name}"));
        let target = root.join("target");
        fs::write(&target, b"{}\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, root.join(file_name)).unwrap();
        let repository = RemoteSessionLeaseRepository::new(root.clone());

        assert!(repository.list().is_err(), "{file_name} must fail closed");
        let _ = fs::remove_dir_all(root);
    }
}

fn reservation(
    lease_id: &str,
    session_id: &str,
    principal: &str,
    idempotency_key: &str,
    fingerprint: &str,
) -> LeaseReservationRequest {
    LeaseReservationRequest {
        lease_id: lease_id.to_string(),
        session_id: session_id.to_string(),
        owner_principal_id: principal.to_string(),
        owner_live_session_limit: usize::MAX,
        name: None,
        default_for_owner: false,
        expires_at_unix_seconds: None,
        idempotency_key: idempotency_key.to_string(),
        creation_fingerprint: fingerprint.to_string(),
        now_unix_seconds: 10,
    }
}

fn checkpoint(snapshot_id: &str, session_id: &str) -> LeaseCheckpointReference {
    LeaseCheckpointReference {
        snapshot_id: snapshot_id.to_string(),
        snapshot_version: 1,
        session_id: session_id.to_string(),
        recorded_at_unix_seconds: 12,
    }
}

fn test_root(name: &str) -> PathBuf {
    let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("mez-lease-test-{}-{name}-{id}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    root
}
