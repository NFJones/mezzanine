use std::fs;
use std::os::unix::fs::PermissionsExt;

use super::*;

fn test_root(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "mez-local-assignment-{label}-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let _ = fs::remove_dir_all(&root);
    root
}

#[test]
fn assignment_restart_fences_live_state_and_retains_checkpoint() {
    let root = test_root("restart");
    let repository = LocalSessionAssignmentRepository::new(root.clone());
    let pending = repository
        .reserve_pending(LocalAssignmentReservationRequest {
            session_id: "$1".to_string(),
            name: "one".to_string(),
            default_for_host: true,
            now_unix_seconds: 10,
        })
        .unwrap();
    let active = repository
        .activate(
            &pending.session_id,
            pending.boot_generation,
            pending.assignment_generation,
            11,
        )
        .unwrap();
    let checkpointed = repository
        .update_checkpoint(
            &active.session_id,
            active.boot_generation,
            active.assignment_generation,
            LocalAssignmentCheckpoint {
                snapshot_id: "local-one".to_string(),
                snapshot_version: 1,
                session_id: active.session_id.clone(),
                recorded_at_unix_seconds: 12,
            },
            12,
        )
        .unwrap();

    assert_eq!(repository.advance_boot_generation(20).unwrap(), 1);
    let recovered = repository.get(&checkpointed.session_id).unwrap().unwrap();
    assert_eq!(recovered.state, LocalSessionAssignmentState::Recoverable);
    assert_eq!(recovered.checkpoint, checkpointed.checkpoint);
    assert_eq!(recovered.boot_generation, 1);
    assert!(
        !root.join("assignments.json").exists(),
        "a fresh store keeps no legacy JSON document"
    );
    let metadata = fs::metadata(root.join("assignments.sqlite")).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o077, 0);
    let _ = fs::remove_dir_all(root);
}

/// Reservation and activation are one logical write, so an assignment written
/// under one injected instant records that instant for its creation and update
/// fields, while an older callback instant is still rejected.
#[test]
fn assignment_reservation_and_activation_share_one_instant() {
    let root = test_root("single-instant");
    let repository = LocalSessionAssignmentRepository::new(root.clone());
    let pending = repository
        .reserve_pending(LocalAssignmentReservationRequest {
            session_id: "$1".to_string(),
            name: "one".to_string(),
            default_for_host: true,
            now_unix_seconds: 10,
        })
        .unwrap();
    assert_eq!(pending.created_at_unix_seconds, 10);
    assert_eq!(
        pending.updated_at_unix_seconds,
        pending.created_at_unix_seconds
    );

    let active = repository
        .activate(
            &pending.session_id,
            pending.boot_generation,
            pending.assignment_generation,
            pending.updated_at_unix_seconds,
        )
        .unwrap();
    assert_eq!(active.created_at_unix_seconds, 10);
    assert_eq!(
        active.updated_at_unix_seconds,
        active.created_at_unix_seconds
    );

    // The staleness guard is unchanged: a callback carrying an instant older
    // than the stored update time is still rejected.
    let stale = repository
        .update_checkpoint(
            &active.session_id,
            active.boot_generation,
            active.assignment_generation,
            LocalAssignmentCheckpoint {
                snapshot_id: "local-stale".to_string(),
                snapshot_version: 1,
                session_id: active.session_id.clone(),
                recorded_at_unix_seconds: 9,
            },
            active.updated_at_unix_seconds.saturating_sub(1),
        )
        .unwrap_err();
    assert_eq!(stale.kind(), crate::error::MezErrorKind::Conflict);

    let _ = fs::remove_dir_all(root);
}

/// Builds one valid stored assignment for legacy-document and capacity tests.
fn stored_assignment(
    session_id: &str,
    state: LocalSessionAssignmentState,
    boot_generation: u64,
) -> LocalSessionAssignment {
    LocalSessionAssignment {
        session_id: session_id.to_string(),
        name: format!("name-{session_id}"),
        default_for_host: false,
        state,
        created_at_unix_seconds: 5,
        updated_at_unix_seconds: 6,
        checkpoint: None,
        failure: match state {
            LocalSessionAssignmentState::Failed => Some("runtime exited".to_string()),
            _ => None,
        },
        boot_generation,
        assignment_generation: 1,
    }
}

/// Writes one legacy JSON document with private permissions.
fn write_legacy_document(
    root: &std::path::Path,
    database: &super::repository::LocalAssignmentDatabase,
) -> Vec<u8> {
    let bytes = serde_json::to_vec_pretty(database).unwrap();
    fs::create_dir_all(root).unwrap();
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(root.join("assignments.json"), &bytes).unwrap();
    fs::set_permissions(
        root.join("assignments.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    bytes
}

/// The legacy JSON document is imported exactly once, retained on disk, and
/// never created or migrated by a read path.
#[test]
fn assignment_legacy_json_is_imported_once_and_retained() {
    let root = test_root("legacy-import");
    let legacy = super::repository::LocalAssignmentDatabase {
        version: 1,
        boot_generation: 3,
        assignments: vec![
            stored_assignment("$legacy-active", LocalSessionAssignmentState::Active, 3),
            stored_assignment("$legacy-failed", LocalSessionAssignmentState::Failed, 3),
        ],
    };
    let legacy_bytes = write_legacy_document(&root, &legacy);

    let repository = LocalSessionAssignmentRepository::new(root.clone());
    let read = repository.list().unwrap();
    assert_eq!(read.len(), 2);
    assert_eq!(read[0].session_id, "$legacy-active");
    assert!(
        !root.join("assignments.sqlite").exists(),
        "a read must not create or migrate the database"
    );

    let created = repository
        .reserve_pending(LocalAssignmentReservationRequest {
            session_id: "$new".to_string(),
            name: "new".to_string(),
            default_for_host: false,
            now_unix_seconds: 10,
        })
        .unwrap();
    assert_eq!(created.boot_generation, 3);
    assert_eq!(repository.list().unwrap().len(), 3);
    assert_eq!(
        fs::read(root.join("assignments.json")).unwrap(),
        legacy_bytes
    );

    let connection = crate::storage::shared_sqlite::open_shared_database_read_only(
        &root.join("assignments.sqlite"),
    )
    .unwrap()
    .unwrap();
    assert!(
        crate::storage::shared_sqlite::migration_completed(&connection, "assignments.json")
            .unwrap(),
        "the import marker must be recorded with the imported rows"
    );
    let rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM assignments", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 3);
    drop(connection);

    // The next write works from the imported database instead of importing
    // the legacy document a second time.
    assert_eq!(repository.advance_boot_generation(20).unwrap(), 4);
    assert_eq!(repository.list().unwrap().len(), 3);
    let _ = fs::remove_dir_all(root);
}

/// Malformed durable data fails closed without being replaced or silently
/// interpreted as an empty assignment database.
#[test]
fn malformed_assignment_database_fails_closed() {
    let root = test_root("corrupt");
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(root.join("assignments.json"), b"not-json\n").unwrap();
    fs::set_permissions(
        root.join("assignments.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let repository = LocalSessionAssignmentRepository::new(root.clone());

    assert_eq!(
        repository.list().unwrap_err().kind(),
        crate::error::MezErrorKind::InvalidState
    );
    assert!(
        repository
            .reserve_pending(LocalAssignmentReservationRequest {
                session_id: "$1".to_string(),
                name: "one".to_string(),
                default_for_host: false,
                now_unix_seconds: 10,
            })
            .is_err(),
        "a write must not replace a malformed legacy document"
    );
    assert_eq!(
        fs::read(root.join("assignments.json")).unwrap(),
        b"not-json\n"
    );
    assert_eq!(
        repository.list().unwrap_err().kind(),
        crate::error::MezErrorKind::InvalidState
    );
    let _ = fs::remove_dir_all(root);
}

/// Assignment database, legacy document, and lock paths must reject symlink
/// substitution rather than following attacker-selected files outside the
/// protected directory.
#[test]
fn assignment_repository_rejects_symlink_database_and_lock_paths() {
    use std::os::unix::fs::symlink;

    for file_name in ["assignments.json", "assignments.lock", "assignments.sqlite"] {
        let root = test_root(&format!("symlink-{file_name}"));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let target = root.join("target");
        fs::write(&target, b"{}\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, root.join(file_name)).unwrap();
        let repository = LocalSessionAssignmentRepository::new(root.clone());

        assert!(repository.list().is_err(), "{file_name} must fail closed");
        let _ = fs::remove_dir_all(root);
    }
}

/// A failed write leaves the stored assignment database unchanged.
///
/// The repository validates every mutation before writing, so a database-level
/// failure is forced directly through the storage layer: two rows carrying the
/// same new session id, with neither present in the store, make the second
/// insert violate the primary key inside the write transaction, which must roll
/// back and leave the previously stored rows and the stored boot generation
/// untouched.
#[test]
fn assignment_database_row_replacement_is_transactional() {
    let root = test_root("row-replacement-atomicity");
    let repository = LocalSessionAssignmentRepository::new(root.clone());
    let created = repository
        .reserve_pending(LocalAssignmentReservationRequest {
            session_id: "$atomic".to_string(),
            name: "atomic".to_string(),
            default_for_host: true,
            now_unix_seconds: 10,
        })
        .unwrap();
    let boot_generation = repository.advance_boot_generation(11).unwrap();
    assert_eq!(boot_generation, 1);
    let stored = repository.list().unwrap();
    assert_eq!(stored.len(), 1);

    let mut duplicate_row = stored[0].clone();
    duplicate_row.session_id = "$duplicate".to_string();
    let duplicate = super::repository::LocalAssignmentDatabase {
        version: 1,
        boot_generation,
        assignments: vec![duplicate_row.clone(), duplicate_row],
    };
    assert!(
        super::sqlite::write_database(
            &root,
            &super::repository::LocalAssignmentDatabase::default(),
            &duplicate,
        )
        .is_err(),
        "two rows with the same new session id must fail the write transaction"
    );
    assert_eq!(repository.list().unwrap(), stored);
    assert_eq!(
        repository.get(&created.session_id).unwrap(),
        Some(stored[0].clone())
    );
    assert_eq!(
        repository.advance_boot_generation(12).unwrap(),
        boot_generation + 1
    );
    let _ = fs::remove_dir_all(root);
}

/// The removed two-megabyte document cap no longer turns a large assignment
/// set into an error: tens of thousands of stored records load and mutate.
#[test]
fn assignment_database_grows_past_the_removed_document_cap() {
    let root = test_root("uncapped");
    let repository = LocalSessionAssignmentRepository::new(root.clone());
    let assignments = (0..20_000)
        .map(|index| {
            stored_assignment(
                &format!("$cap-{index:05}"),
                LocalSessionAssignmentState::Failed,
                1,
            )
        })
        .collect::<Vec<_>>();
    let database = super::repository::LocalAssignmentDatabase {
        version: 1,
        boot_generation: 1,
        assignments,
    };
    super::sqlite::write_database(
        &root,
        &super::repository::LocalAssignmentDatabase::default(),
        &database,
    )
    .unwrap();

    let mut stored_bytes = fs::metadata(root.join("assignments.sqlite")).unwrap().len();
    if let Ok(wal) = fs::metadata(root.join("assignments.sqlite-wal")) {
        stored_bytes = stored_bytes.saturating_add(wal.len());
    }
    assert!(
        stored_bytes > 2 * 1024 * 1024,
        "the fixture must exceed the removed two-megabyte document cap"
    );
    assert_eq!(repository.list().unwrap().len(), 20_000);
    assert_eq!(repository.advance_boot_generation(30).unwrap(), 2);
    assert_eq!(
        repository.get("$cap-19999").unwrap().unwrap().state,
        LocalSessionAssignmentState::Failed
    );
    let _ = fs::remove_dir_all(root);
}

/// A store read sees committed rows and never blocks on an uncommitted writer,
/// which is what lets a short-lived read-only command inspect the store while
/// the daemon holds a write transaction.
#[test]
fn assignment_reads_never_block_on_an_uncommitted_writer() {
    let root = test_root("wal-reader");
    let repository = LocalSessionAssignmentRepository::new(root.clone());
    repository
        .reserve_pending(LocalAssignmentReservationRequest {
            session_id: "$committed".to_string(),
            name: "committed".to_string(),
            default_for_host: false,
            now_unix_seconds: 10,
        })
        .unwrap();

    let connection = rusqlite::Connection::open(root.join("assignments.sqlite")).unwrap();
    connection
        .execute_batch("BEGIN IMMEDIATE; DELETE FROM assignments;")
        .unwrap();
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
    connection.execute_batch("ROLLBACK").unwrap();
    drop(connection);

    assert_eq!(repository.list().unwrap().len(), 1);
    let _ = fs::remove_dir_all(root);
}

/// The key columns are a checked mirror of the payload: a row whose state
/// column disagrees with the encoded record fails closed instead of being read
/// as trusted data.
#[test]
fn assignment_database_state_column_is_checked_on_read() {
    let root = test_root("state-column");
    let repository = LocalSessionAssignmentRepository::new(root.clone());
    repository
        .reserve_pending(LocalAssignmentReservationRequest {
            session_id: "$state".to_string(),
            name: "state".to_string(),
            default_for_host: false,
            now_unix_seconds: 10,
        })
        .unwrap();

    let connection = rusqlite::Connection::open(root.join("assignments.sqlite")).unwrap();
    connection
        .execute(
            "UPDATE assignments SET state = 'bogus' WHERE session_id = '$state'",
            [],
        )
        .unwrap();
    drop(connection);

    assert_eq!(
        repository.list().unwrap_err().kind(),
        crate::error::MezErrorKind::InvalidState
    );
    let _ = fs::remove_dir_all(root);
}

/// The inspection exporter renders stored assignment rows, including their
/// checkpoint, without creating the store or taking the repository lock.
#[test]
fn assignment_export_renders_rows_without_creating_the_store() {
    let root = test_root("export");
    let repository = LocalSessionAssignmentRepository::new(root.clone());
    assert!(
        repository.export_tsv_read_only().unwrap().is_none(),
        "an absent store reports nothing to export"
    );
    assert!(
        !root.join("assignments.sqlite").exists() && !root.join("assignments.json").exists(),
        "an inspection must not create the store"
    );
    assert!(
        !root.join("assignments.lock").exists(),
        "an inspection must not create the lock file"
    );

    let pending = repository
        .reserve_pending(LocalAssignmentReservationRequest {
            session_id: "$export".to_string(),
            name: "export".to_string(),
            default_for_host: false,
            now_unix_seconds: 10,
        })
        .unwrap();
    let active = repository
        .activate(
            &pending.session_id,
            pending.boot_generation,
            pending.assignment_generation,
            11,
        )
        .unwrap();
    let checkpointed = repository
        .update_checkpoint(
            &active.session_id,
            active.boot_generation,
            active.assignment_generation,
            LocalAssignmentCheckpoint {
                snapshot_id: "local-export".to_string(),
                snapshot_version: 1,
                session_id: active.session_id.clone(),
                recorded_at_unix_seconds: 12,
            },
            12,
        )
        .unwrap();

    repository
        .reserve_pending(LocalAssignmentReservationRequest {
            session_id: "$alpha".to_string(),
            name: "alpha".to_string(),
            default_for_host: false,
            now_unix_seconds: 13,
        })
        .unwrap();

    let export = repository.export_tsv_read_only().unwrap().unwrap();
    assert!(
        export.starts_with(
            "session_id\tstate\tdefault_for_host\tboot_generation\tassignment_generation\tcreated_at_unix_seconds\tupdated_at_unix_seconds\tcheckpoint_snapshot_id\tfailure\n"
        ),
        "the export starts with the assignment header: {export}"
    );
    let exported_ids = export
        .lines()
        .skip(1)
        .map(|line| line.split('\t').next().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        exported_ids,
        vec!["$alpha".to_string(), "$export".to_string()],
        "assignment rows sort by session id"
    );
    let row = export
        .lines()
        .find(|line| line.starts_with("$export\t"))
        .expect("the export renders the assignment row");
    let fields = row.split('\t').collect::<Vec<_>>();
    assert_eq!(fields[1], "active");
    assert_eq!(fields[2], "false");
    assert_eq!(fields[3], checkpointed.boot_generation.to_string());
    assert_eq!(fields[4], checkpointed.assignment_generation.to_string());
    assert_eq!(fields[7], "local-export");
    assert_eq!(fields[8], "");
    let _ = fs::remove_dir_all(root);
}

/// The inspection exporter fails closed on a dangling symbolic link instead of
/// reporting an absent store.
#[test]
fn assignment_export_rejects_dangling_symlinks() {
    use std::os::unix::fs::symlink;

    for file_name in ["assignments.sqlite", "assignments.json"] {
        let root = test_root(&format!("export-symlink-{file_name}"));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        symlink(root.join("missing-target"), root.join(file_name)).unwrap();
        let repository = LocalSessionAssignmentRepository::new(root.clone());

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
fn assignment_pending_writes_cover_only_changed_rows() {
    let root = test_root("pending-writes");
    let repository = LocalSessionAssignmentRepository::new(root.clone());
    for (session_id, name) in [("$a", "a"), ("$b", "b"), ("$c", "c")] {
        repository
            .reserve_pending(LocalAssignmentReservationRequest {
                session_id: session_id.to_string(),
                name: name.to_string(),
                default_for_host: false,
                now_unix_seconds: 10,
            })
            .unwrap();
    }
    let stored = repository.list().unwrap();
    let before = super::repository::LocalAssignmentDatabase {
        version: 1,
        boot_generation: stored[0].boot_generation,
        assignments: stored,
    };
    let mut after = before.clone();
    after.assignments[0].failure = Some("changed".to_string());
    after.assignments.remove(1);
    let mut added = before.assignments[2].clone();
    added.session_id = "$d".to_string();
    after.assignments.push(added);

    let rendered = super::sqlite::pending_writes(&before, &after)
        .iter()
        .map(|write| match write {
            super::sqlite::AssignmentRowWrite::Insert(assignment) => {
                format!("insert {}", assignment.session_id)
            }
            super::sqlite::AssignmentRowWrite::Update(assignment) => {
                format!("update {}", assignment.session_id)
            }
            super::sqlite::AssignmentRowWrite::Delete(session_id) => format!("delete {session_id}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rendered,
        vec![
            "delete $b".to_string(),
            "update $a".to_string(),
            "insert $d".to_string(),
        ],
        "only changed, added, and removed rows are written"
    );
    let _ = fs::remove_dir_all(root);
}
