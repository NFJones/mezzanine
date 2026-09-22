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

/// Reservation and activation are one logical write, so a lease written under
/// one injected instant records that instant for its creation, update, and
/// activation fields, while an older callback instant is still rejected.
#[test]
fn lease_reservation_and_activation_share_one_instant() {
    let root = test_root("single-instant");
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
    assert_eq!(pending.created_at_unix_seconds, 10);
    assert_eq!(
        pending.updated_at_unix_seconds,
        pending.created_at_unix_seconds
    );

    let active = repository
        .activate(
            &pending.lease_id,
            pending.boot_generation,
            pending.lease_generation,
            pending.updated_at_unix_seconds,
        )
        .unwrap();
    assert_eq!(active.created_at_unix_seconds, 10);
    assert_eq!(
        active.updated_at_unix_seconds,
        active.created_at_unix_seconds
    );
    assert_eq!(active.activated_at_unix_seconds, Some(10));

    // The staleness guard is unchanged: a callback carrying an instant older
    // than the stored update time is still rejected.
    let stale = repository
        .mark_failed(
            &active.lease_id,
            active.boot_generation,
            active.lease_generation,
            active.updated_at_unix_seconds.saturating_sub(1),
            "stale callback".to_string(),
        )
        .unwrap_err();
    assert_eq!(stale.kind(), MezErrorKind::Conflict);

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

/// Restart reconciliation must retain an active lease's persisted timestamp
/// when a regressed wall-clock sample would otherwise invalidate its lifecycle
/// record during durable database validation.
#[test]
fn boot_reconciliation_clamps_regressed_wall_clock_to_lease_timestamp() {
    let root = test_root("restart-clock-regression");
    let repository = RemoteSessionLeaseRepository::new(root.clone());
    let pending = repository
        .reserve_pending(reservation(
            "lease-clock-regression",
            "$1",
            "device-1",
            "create-clock-regression",
            "fingerprint-clock-regression",
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

    assert_eq!(repository.advance_boot_generation(5).unwrap(), 1);
    let recovered = repository.get(&active.lease_id).unwrap().unwrap();
    assert_eq!(recovered.state, RemoteSessionLeaseState::Recoverable);
    assert_eq!(
        recovered.updated_at_unix_seconds,
        active.updated_at_unix_seconds
    );
    assert_eq!(recovered.activated_at_unix_seconds, Some(11));

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

/// A failed write leaves the stored lease database unchanged.
///
/// The repository validates every mutation before writing, so a database-level
/// failure is forced directly through the storage layer: two rows carrying the
/// same new lease id, with neither present in the store, make the second insert
/// violate the primary key inside the write transaction, which must roll back
/// and leave the previously stored rows and the boot generation untouched.
#[test]
fn lease_database_row_replacement_is_transactional() {
    let root = test_root("row-replacement-atomicity");
    let repository = RemoteSessionLeaseRepository::new(root.clone());
    let created = repository
        .reserve_pending(reservation(
            "lease-atomic",
            "$1",
            "device-1",
            "create-atomic",
            "fingerprint-atomic",
        ))
        .unwrap();
    let stored = repository.list().unwrap();
    assert_eq!(stored.len(), 1);
    let boot_generation = repository.boot_generation().unwrap();

    let mut duplicate_row = stored[0].clone();
    duplicate_row.lease_id = "lease-duplicate".to_string();
    let duplicate = super::repository::LeaseDatabase {
        version: 1,
        boot_generation,
        leases: vec![duplicate_row.clone(), duplicate_row],
        snapshot_cleanup_candidates: Vec::new(),
    };
    assert!(
        super::sqlite::write_database(
            &root,
            &super::repository::LeaseDatabase::default(),
            &duplicate,
        )
        .is_err(),
        "two rows with the same new lease id must fail the write transaction"
    );
    assert_eq!(repository.list().unwrap(), stored);
    assert_eq!(repository.boot_generation().unwrap(), boot_generation);
    assert_eq!(
        repository.get(&created.lease().lease_id).unwrap(),
        Some(stored[0].clone())
    );
    let _ = fs::remove_dir_all(root);
}

/// The key columns are a checked mirror of the payload: a row whose state
/// column disagrees with the encoded record fails closed instead of being read
/// as trusted data.
#[test]
fn lease_database_state_column_is_checked_on_read() {
    let root = test_root("state-column");
    let repository = RemoteSessionLeaseRepository::new(root.clone());
    repository
        .reserve_pending(reservation(
            "lease-state",
            "$1",
            "device-1",
            "create-state",
            "fingerprint-state",
        ))
        .unwrap();

    let connection = rusqlite::Connection::open(root.join("session-reservations.sqlite")).unwrap();
    connection
        .execute(
            "UPDATE leases SET state = 'bogus' WHERE lease_id = 'lease-state'",
            [],
        )
        .unwrap();
    drop(connection);

    assert_eq!(
        repository.list().unwrap_err().kind(),
        MezErrorKind::InvalidState
    );
    let _ = fs::remove_dir_all(root);
}

/// The inspection exporter renders stored lease rows and pending cleanup
/// candidates without creating the store or taking the repository lock.
#[test]
fn lease_export_renders_rows_without_creating_the_store() {
    let root = test_root("export");
    let repository = RemoteSessionLeaseRepository::new(root.clone());
    assert!(
        repository.export_tsv_read_only().unwrap().is_none(),
        "an absent store reports nothing to export"
    );
    assert!(
        !root.join("session-reservations.sqlite").exists() && !root.join("leases.json").exists(),
        "an inspection must not create the store"
    );
    assert!(
        !root.join("leases.lock").exists(),
        "an inspection must not create the lock file"
    );

    repository
        .reserve_pending(reservation(
            "lease-export",
            "$1",
            "device-1",
            "create-export",
            "fingerprint-export",
        ))
        .unwrap();
    repository
        .reserve_pending(reservation(
            "lease-alpha",
            "$0",
            "device-1",
            "create-alpha",
            "fingerprint-alpha",
        ))
        .unwrap();
    let stored = repository.list().unwrap();
    assert_eq!(stored.len(), 2);
    let stored_export = stored
        .iter()
        .find(|lease| lease.lease_id == "lease-export")
        .expect("the reservation is stored");
    let before = super::repository::LeaseDatabase {
        version: 1,
        boot_generation: stored_export.boot_generation,
        leases: stored.clone(),
        snapshot_cleanup_candidates: Vec::new(),
    };
    let after = super::repository::LeaseDatabase {
        version: 1,
        boot_generation: stored_export.boot_generation,
        leases: stored.clone(),
        snapshot_cleanup_candidates: vec!["snap-export".to_string()],
    };
    super::sqlite::write_database(&root, &before, &after).unwrap();

    let export = repository.export_tsv_read_only().unwrap().unwrap();
    assert!(
        export.starts_with(
            "lease_id\tsession_id\tstate\tboot_generation\tlease_generation\texpires_at_unix_seconds\tupdated_at_unix_seconds\tfailure\n"
        ),
        "the export starts with the lease header: {export}"
    );
    let lease_block = export.split("\n\n").next().unwrap();
    let exported_ids = lease_block
        .lines()
        .skip(1)
        .map(|line| line.split('\t').next().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        exported_ids,
        vec!["lease-alpha".to_string(), "lease-export".to_string()],
        "lease rows sort by lease id"
    );
    let row = lease_block
        .lines()
        .find(|line| line.starts_with("lease-export\t"))
        .expect("the export renders the lease row");
    let fields = row.split('\t').collect::<Vec<_>>();
    assert_eq!(fields[1], "$1");
    assert_eq!(
        fields[2],
        serde_json::to_value(stored_export.state)
            .unwrap()
            .as_str()
            .unwrap()
    );
    assert_eq!(fields[3], stored_export.boot_generation.to_string());
    assert_eq!(fields[5], "", "an unbounded lease has no expiry");
    assert!(
        export.contains("\nsnapshot_cleanup_candidate_id\nsnap-export\n"),
        "the export renders the pending cleanup candidates: {export}"
    );
    let _ = fs::remove_dir_all(root);
}

/// The inspection exporter fails closed on a dangling symbolic link instead of
/// reporting an absent store.
#[test]
fn lease_export_rejects_dangling_symlinks() {
    use std::os::unix::fs::symlink;

    for file_name in ["session-reservations.sqlite", "leases.json"] {
        let root = test_root(&format!("export-symlink-{file_name}"));
        symlink(root.join("missing-target"), root.join(file_name)).unwrap();
        let repository = RemoteSessionLeaseRepository::new(root.clone());

        assert!(
            repository.export_tsv_read_only().is_err(),
            "a dangling {file_name} must not be reported as an absent store"
        );
        let _ = fs::remove_dir_all(root);
    }
}

/// A write persists only the rows a mutation changed: an unchanged row
/// produces no statement, a changed row updates, a new row inserts, and a
/// removed row deletes.
#[test]
fn lease_pending_writes_cover_only_changed_rows() {
    let root = test_root("pending-writes");
    let repository = RemoteSessionLeaseRepository::new(root.clone());
    for (lease_id, session_id, idempotency_key, creation_fingerprint) in [
        ("lease-a", "$a", "create-a", "fingerprint-a"),
        ("lease-b", "$b", "create-b", "fingerprint-b"),
        ("lease-c", "$c", "create-c", "fingerprint-c"),
    ] {
        repository
            .reserve_pending(reservation(
                lease_id,
                session_id,
                "device-1",
                idempotency_key,
                creation_fingerprint,
            ))
            .unwrap();
    }
    let before = super::repository::LeaseDatabase {
        version: 1,
        boot_generation: repository.boot_generation().unwrap(),
        leases: repository.list().unwrap(),
        snapshot_cleanup_candidates: Vec::new(),
    };
    let mut after = before.clone();
    after.leases[0].name = Some("renamed".to_string());
    after.leases.remove(1);
    let mut added = before.leases[2].clone();
    added.lease_id = "lease-d".to_string();
    added.session_id = "$d".to_string();
    added.idempotency_key = "create-d".to_string();
    added.creation_fingerprint = "fingerprint-d".to_string();
    after.leases.push(added);

    let rendered = super::sqlite::pending_writes(&before, &after)
        .iter()
        .map(|write| match write {
            super::sqlite::LeaseRowWrite::Insert(lease) => format!("insert {}", lease.lease_id),
            super::sqlite::LeaseRowWrite::Update(lease) => format!("update {}", lease.lease_id),
            super::sqlite::LeaseRowWrite::Delete(lease_id) => format!("delete {lease_id}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rendered,
        vec![
            "delete lease-b".to_string(),
            "update lease-a".to_string(),
            "insert lease-d".to_string(),
        ],
        "only changed, added, and removed rows are written"
    );
    let _ = fs::remove_dir_all(root);
}

/// The removed four-megabyte document cap no longer turns a large lease set
/// into an error: twenty thousand stored leases still load, expire, and roll
/// their boot generation.
#[test]
fn lease_database_grows_past_the_removed_document_cap() {
    let root = test_root("uncapped");
    let repository = RemoteSessionLeaseRepository::new(root.clone());
    repository
        .reserve_pending(reservation(
            "lease-seed",
            "$seed",
            "device-1",
            "create-seed",
            "fingerprint-seed",
        ))
        .unwrap();
    let seed = repository.get("lease-seed").unwrap().unwrap();
    let before = super::repository::LeaseDatabase {
        version: 1,
        boot_generation: repository.boot_generation().unwrap(),
        leases: repository.list().unwrap(),
        snapshot_cleanup_candidates: Vec::new(),
    };
    let leases = (0..20_000)
        .map(|index| {
            let mut lease = seed.clone();
            lease.lease_id = format!("lease-cap-{index:05}");
            lease.session_id = format!("$cap-{index:05}");
            lease.idempotency_key = format!("create-cap-{index:05}");
            lease.creation_fingerprint = format!("fingerprint-cap-{index:05}");
            // The reservation instant is 10, so the expiry must not precede it
            // and must still be due when the sweep runs at 100.
            lease.expires_at_unix_seconds = Some(11);
            lease
        })
        .collect::<Vec<_>>();
    super::sqlite::write_database(
        &root,
        &before,
        &super::repository::LeaseDatabase {
            version: 1,
            boot_generation: seed.boot_generation,
            leases,
            snapshot_cleanup_candidates: Vec::new(),
        },
    )
    .unwrap();

    let mut stored_bytes = fs::metadata(root.join("session-reservations.sqlite"))
        .unwrap()
        .len();
    if let Ok(wal) = fs::metadata(root.join("session-reservations.sqlite-wal")) {
        stored_bytes = stored_bytes.saturating_add(wal.len());
    }
    assert!(
        stored_bytes > 4 * 1024 * 1024,
        "the fixture must exceed the removed four-megabyte document cap"
    );

    let expired = repository.expire_due(100).unwrap();
    assert_eq!(expired.len(), 20_000);
    assert_eq!(repository.list().unwrap().len(), 20_000);
    assert_eq!(repository.advance_boot_generation(200).unwrap(), 1);
    assert_eq!(
        repository.get("lease-cap-19999").unwrap().unwrap().state,
        RemoteSessionLeaseState::Revoked
    );
    let _ = fs::remove_dir_all(root);
}

/// A store read sees committed rows and never blocks on an uncommitted writer,
/// which is what lets a short-lived read-only command inspect the store while
/// the daemon holds a write transaction.
#[test]
fn lease_reads_never_block_on_an_uncommitted_writer() {
    let root = test_root("wal-reader");
    let repository = RemoteSessionLeaseRepository::new(root.clone());
    repository
        .reserve_pending(reservation(
            "lease-committed",
            "$1",
            "device-1",
            "create-committed",
            "fingerprint-committed",
        ))
        .unwrap();

    let connection = rusqlite::Connection::open(root.join("session-reservations.sqlite")).unwrap();
    connection
        .execute_batch("BEGIN IMMEDIATE; DELETE FROM leases;")
        .unwrap();
    let started = std::time::Instant::now();
    let read = repository.list().unwrap();
    assert_eq!(
        read.len(),
        1,
        "an uncommitted delete must stay invisible to a reader"
    );
    assert!(
        repository.export_tsv_read_only().unwrap().is_some(),
        "an export must read through the same uncommitted writer"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "a reader must not wait on an uncommitted writer"
    );
    connection.execute_batch("ROLLBACK").unwrap();
    drop(connection);

    assert_eq!(repository.list().unwrap().len(), 1);
    let _ = fs::remove_dir_all(root);
}

/// The inspection export takes no repository lock: while another writer holds
/// the store's exclusive lock, the export still completes and reports the
/// committed rows instead of parking behind the writer.
#[test]
fn lease_export_ignores_the_exclusive_write_lock() {
    let root = test_root("export-unlocked");
    let repository = RemoteSessionLeaseRepository::new(root.clone());
    repository
        .reserve_pending(reservation(
            "lease-export",
            "$1",
            "device-1",
            "create-export",
            "fingerprint-export",
        ))
        .unwrap();

    let lock = rustix::fs::open(
        root.join("leases.lock"),
        rustix::fs::OFlags::RDWR | rustix::fs::OFlags::CREATE | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .unwrap();
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();

    let (sender, receiver) = std::sync::mpsc::channel();
    let export_repository = RemoteSessionLeaseRepository::new(root.clone());
    let worker = std::thread::spawn(move || {
        sender
            .send(export_repository.export_tsv_read_only())
            .expect("the export worker reports its result");
    });
    let export = receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("the export must not wait on the repository lock")
        .unwrap()
        .unwrap();
    assert!(
        export.contains("lease-export\t$1\t"),
        "the export renders the committed row while the lock is held: {export}"
    );
    worker.join().unwrap();

    rustix::fs::flock(&lock, rustix::fs::FlockOperation::Unlock).unwrap();
    drop(lock);
    let _ = fs::remove_dir_all(root);
}
