//! SQLite storage layer for durable remote-session leases.
//!
//! The repository used to rewrite one JSON document per mutation and refused to
//! grow past four megabytes. It now keeps the same records in a private
//! `session-reservations.sqlite` database: key columns carry the fields the
//! repository actually queries (`session_id`, `state`, `expires_at_unix_seconds`,
//! `boot_generation`) and a payload column carries the validated record, so a
//! mutation writes only the rows it changed. The legacy `leases.json` is
//! imported exactly once and is never
//! modified, so a rollback to a previous build still finds its data.

use super::super::shared_sqlite::{
    SharedSchemaState, export_tsv, import_legacy_file_once, migration_completed,
    open_shared_database, open_shared_database_read_only, read_private_legacy_file, schema_version,
    set_schema_version,
};
use super::repository::LeaseDatabase;
use super::{MezError, RemoteSessionLease, RemoteSessionLeaseState, Result};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Database file owned by the remote-session lease repository.
pub(super) const LEASE_DATABASE_FILE_NAME: &str = "session-reservations.sqlite";

/// Schema version owned by the lease tables.
///
/// Version 2 added the non-negative CHECK constraints on the counting columns;
/// a database written by an intermediate build is rejected instead of being read
/// without them.
const LEASE_SCHEMA_VERSION: i64 = 2;

/// One-time import marker for the legacy JSON document.
const LEASE_IMPORT_MARKER: &str = "leases.json";

/// Legacy JSON document retained for import and rollback.
const LEGACY_DATABASE_FILE_NAME: &str = "leases.json";

/// Returns the lease database path for one repository directory.
pub(super) fn database_path(directory: &Path) -> PathBuf {
    directory.join(LEASE_DATABASE_FILE_NAME)
}

/// Returns the legacy JSON path for one repository directory.
fn legacy_path(directory: &Path) -> PathBuf {
    directory.join(LEGACY_DATABASE_FILE_NAME)
}

/// Maps one rusqlite failure to an actionable lease error.
fn database_error(error: rusqlite::Error) -> MezError {
    MezError::invalid_state(format!("remote session lease database error: {error}"))
}

/// Encodes one lease state token into its stored snake_case form.
fn encode_state(state: RemoteSessionLeaseState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Reports whether one path exists, including a symbolic link that does not
/// resolve, so an inspection command reports the broken path instead of
/// treating the store as absent.
fn path_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Renders the lease store in its inspection TSV shape without creating it.
///
/// Returns `None` when neither representation exists, so an inspection command
/// never creates the store, and the rows come from the same read-only path the
/// listing command uses, so an export cannot block a daemon writer. Lease rows
/// sort by lease id; pending snapshot cleanup candidates follow them after a
/// blank line under a `snapshot_cleanup_candidate_id` header.
pub(super) fn export_tsv_read_only(directory: &Path) -> Result<Option<String>> {
    let path = database_path(directory);
    let legacy = legacy_path(directory);
    reject_symlink(&path)?;
    if !path_exists(&path) && !path_exists(&legacy) {
        return Ok(None);
    }
    let database = load_database(directory)?;
    let mut leases = database.leases;
    leases.sort_by(|left, right| left.lease_id.cmp(&right.lease_id));
    let rows = leases
        .iter()
        .map(|lease| {
            vec![
                lease.lease_id.clone(),
                lease.session_id.clone(),
                encode_state(lease.state),
                lease.boot_generation.to_string(),
                lease.lease_generation.to_string(),
                lease
                    .expires_at_unix_seconds
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                lease.updated_at_unix_seconds.to_string(),
                lease.failure.clone().unwrap_or_default(),
            ]
        })
        .collect::<Vec<_>>();
    let mut output = export_tsv(
        &[
            "lease_id",
            "session_id",
            "state",
            "boot_generation",
            "lease_generation",
            "expires_at_unix_seconds",
            "updated_at_unix_seconds",
            "failure",
        ],
        &rows,
    );
    let mut candidates = database.snapshot_cleanup_candidates;
    if !candidates.is_empty() {
        candidates.sort();
        let rows = candidates
            .into_iter()
            .map(|snapshot_id| vec![snapshot_id])
            .collect::<Vec<_>>();
        output.push('\n');
        output.push_str(&export_tsv(&["snapshot_cleanup_candidate_id"], &rows));
    }
    Ok(Some(output))
}

/// Refuses a database path that is a symbolic link.
fn reject_symlink(path: &Path) -> Result<()> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        return Err(MezError::invalid_state(format!(
            "remote session lease database path {} must not be a symbolic link",
            path.display()
        )));
    }
    Ok(())
}

/// Loads the lease database without creating, migrating, or importing anything.
///
/// Reads open the database read-only when it exists and otherwise decode the
/// legacy JSON document, so a short-lived reader never turns a legacy file into
/// a database as a side effect of looking at it.
pub(super) fn load_database(directory: &Path) -> Result<LeaseDatabase> {
    let path = database_path(directory);
    reject_symlink(&path)?;
    if !path.exists() {
        return legacy_database(directory);
    }
    let Some(connection) = open_shared_database_read_only(&path)? else {
        return legacy_database(directory);
    };
    let version = schema_version(&connection)?;
    if version != LEASE_SCHEMA_VERSION {
        return Err(MezError::invalid_state(format!(
            "remote session lease database schema version {version} does not match this build's version {LEASE_SCHEMA_VERSION}; restart with the build that wrote it, or delete {} and let the next write import the legacy JSON document",
            path.display()
        )));
    }
    if !migration_completed(&connection, LEASE_IMPORT_MARKER)? {
        return legacy_database(directory);
    }
    read_database(&connection)
}

/// Writes the lease database inside one immediate transaction.
///
/// The caller holds the repository's exclusive lock, which is what makes the
/// one-time legacy import safe against an older binary still writing the JSON
/// document.
pub(super) fn write_database(
    directory: &Path,
    before: &LeaseDatabase,
    after: &LeaseDatabase,
) -> Result<()> {
    let path = database_path(directory);
    reject_symlink(&path)?;
    let (mut connection, state) = open_shared_database(&path, LEASE_SCHEMA_VERSION)?;
    if state == SharedSchemaState::Fresh {
        create_schema(&mut connection)?;
    }
    import_legacy_database(directory, &mut connection)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    synchronize_rows(&transaction, before, after)?;
    transaction.commit().map_err(database_error)
}

/// Creates the lease schema and publishes its version atomically.
fn create_schema(connection: &mut Connection) -> Result<()> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    transaction
        .execute_batch("CREATE TABLE leases (lease_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, state TEXT NOT NULL, expires_at_unix_seconds INTEGER CHECK (expires_at_unix_seconds IS NULL OR expires_at_unix_seconds >= 0), boot_generation INTEGER NOT NULL CHECK (boot_generation >= 0), payload TEXT NOT NULL); CREATE INDEX leases_by_session ON leases(session_id); CREATE INDEX leases_by_state ON leases(state); CREATE INDEX leases_by_expiry ON leases(expires_at_unix_seconds); CREATE TABLE lease_state (id INTEGER PRIMARY KEY CHECK (id = 1), boot_generation INTEGER NOT NULL CHECK (boot_generation >= 0)); CREATE TABLE snapshot_cleanup_candidates (snapshot_id TEXT PRIMARY KEY);")
        .map_err(database_error)?;
    set_schema_version(&transaction, LEASE_SCHEMA_VERSION)?;
    transaction.commit().map_err(database_error)
}

/// Imports the legacy JSON document exactly once.
fn import_legacy_database(directory: &Path, connection: &mut Connection) -> Result<()> {
    if migration_completed(connection, LEASE_IMPORT_MARKER)? {
        return Ok(());
    }
    let legacy = legacy_database(directory)?;
    import_legacy_file_once(connection, LEASE_IMPORT_MARKER, |transaction| {
        clear_rows(transaction)?;
        synchronize_rows(transaction, &LeaseDatabase::default(), &legacy)?;
        Ok(legacy.leases.len() as i64)
    })?;
    Ok(())
}

/// Decodes and validates the legacy JSON document when it exists.
fn legacy_database(directory: &Path) -> Result<LeaseDatabase> {
    let path = legacy_path(directory);
    let Some(bytes) = read_private_legacy_file(&path)? else {
        return Ok(LeaseDatabase::default());
    };
    let database: LeaseDatabase = serde_json::from_slice(&bytes).map_err(|error| {
        MezError::invalid_state(format!(
            "remote session lease database is malformed: {error}"
        ))
    })?;
    super::repository::validate_database(&database)?;
    Ok(database)
}

/// Reads every stored row back into the in-memory database model.
fn read_database(connection: &Connection) -> Result<LeaseDatabase> {
    let mut database = LeaseDatabase::default();
    let boot_generation: Option<i64> = connection
        .query_row(
            "SELECT boot_generation FROM lease_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .ok();
    if let Some(boot_generation) = boot_generation {
        database.boot_generation = boot_generation as u64;
    }
    let mut statement = connection
        .prepare("SELECT state, payload FROM leases")
        .map_err(database_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(database_error)?;
    for row in rows {
        let (state, payload) = row.map_err(database_error)?;
        let lease: RemoteSessionLease = serde_json::from_str(&payload).map_err(|error| {
            MezError::invalid_state(format!("remote session lease row is malformed: {error}"))
        })?;
        let payload_state = encode_state(lease.state);
        if state != payload_state {
            return Err(MezError::invalid_state(format!(
                "remote session lease row {} state {state} does not match its payload state {payload_state}",
                lease.lease_id
            )));
        }
        database.leases.push(lease);
    }
    let mut statement = connection
        .prepare("SELECT snapshot_id FROM snapshot_cleanup_candidates")
        .map_err(database_error)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(database_error)?;
    for row in rows {
        database
            .snapshot_cleanup_candidates
            .push(row.map_err(database_error)?);
    }
    super::repository::validate_database(&database)?;
    Ok(database)
}

/// One stored lease row's required write to turn one in-memory state into
/// another.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum LeaseRowWrite<'a> {
    /// The row is new and must be inserted.
    Insert(&'a RemoteSessionLease),
    /// The row exists and its validated record changed.
    Update(&'a RemoteSessionLease),
    /// The row is gone and must be removed.
    Delete(&'a str),
}

/// Returns the row-level writes that turn `before` into `after`.
///
/// Unchanged rows produce no write, so a mutation that touches one lease
/// writes one row instead of rewriting the whole store. Removals come first so
/// a key-changing mutation can never collide an insert with the row the same
/// plan removes, and the plan follows the caller's in-memory order, which the
/// repository keeps sorted by lease id, so it is deterministic.
pub(super) fn pending_writes<'a>(
    before: &'a LeaseDatabase,
    after: &'a LeaseDatabase,
) -> Vec<LeaseRowWrite<'a>> {
    let before_by_id: HashMap<&str, &RemoteSessionLease> = before
        .leases
        .iter()
        .map(|lease| (lease.lease_id.as_str(), lease))
        .collect();
    let after_ids: HashSet<&str> = after
        .leases
        .iter()
        .map(|lease| lease.lease_id.as_str())
        .collect();
    let mut writes = Vec::new();
    // Removals come first so a mutation that changed a lease id can never
    // collide its insert with the row this plan deletes.
    for lease in &before.leases {
        if !after_ids.contains(lease.lease_id.as_str()) {
            writes.push(LeaseRowWrite::Delete(lease.lease_id.as_str()));
        }
    }
    for lease in &after.leases {
        match before_by_id.get(lease.lease_id.as_str()) {
            Some(existing) if *existing == lease => {}
            Some(_) => writes.push(LeaseRowWrite::Update(lease)),
            None => writes.push(LeaseRowWrite::Insert(lease)),
        }
    }
    writes
}

/// Persists one lease row, inserting a new lease or updating a changed one.
fn write_row(
    transaction: &Transaction<'_>,
    lease: &RemoteSessionLease,
    update_existing: bool,
) -> Result<()> {
    let payload = serde_json::to_string(lease).map_err(|error| {
        MezError::invalid_state(format!(
            "failed to encode remote session lease row: {error}"
        ))
    })?;
    let state = encode_state(lease.state);
    let expires_at = lease.expires_at_unix_seconds.map(|value| value as i64);
    let boot_generation = lease.boot_generation as i64;
    let changed = if update_existing {
        transaction
            .execute(
                "UPDATE leases SET session_id = ?2, state = ?3, expires_at_unix_seconds = ?4, boot_generation = ?5, payload = ?6 WHERE lease_id = ?1",
                params![
                    lease.lease_id,
                    lease.session_id,
                    state,
                    expires_at,
                    boot_generation,
                    payload,
                ],
            )
            .map_err(database_error)?
    } else {
        transaction
            .execute(
                "INSERT INTO leases (lease_id, session_id, state, expires_at_unix_seconds, boot_generation, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    lease.lease_id,
                    lease.session_id,
                    state,
                    expires_at,
                    boot_generation,
                    payload,
                ],
            )
            .map_err(database_error)?
    };
    if changed == 0 {
        return Err(MezError::invalid_state(
            "remote session lease row is missing from the store",
        ));
    }
    Ok(())
}

/// Persists added and removed snapshot cleanup candidates.
fn synchronize_candidates(
    transaction: &Transaction<'_>,
    before: &LeaseDatabase,
    after: &LeaseDatabase,
) -> Result<()> {
    let before_candidates: HashSet<&str> = before
        .snapshot_cleanup_candidates
        .iter()
        .map(String::as_str)
        .collect();
    let after_candidates: HashSet<&str> = after
        .snapshot_cleanup_candidates
        .iter()
        .map(String::as_str)
        .collect();
    for snapshot_id in &after.snapshot_cleanup_candidates {
        if !before_candidates.contains(snapshot_id.as_str()) {
            transaction
                .execute(
                    "INSERT INTO snapshot_cleanup_candidates (snapshot_id) VALUES (?1)",
                    params![snapshot_id],
                )
                .map_err(database_error)?;
        }
    }
    for snapshot_id in &before.snapshot_cleanup_candidates {
        if !after_candidates.contains(snapshot_id.as_str()) {
            // Candidate removal is idempotent: an id that is already absent is
            // simply nothing to delete, so no changed-row guard is needed.
            transaction
                .execute(
                    "DELETE FROM snapshot_cleanup_candidates WHERE snapshot_id = ?1",
                    params![snapshot_id],
                )
                .map_err(database_error)?;
        }
    }
    Ok(())
}

/// Persists the difference between two in-memory states in one transaction.
///
/// The caller validated `after`, so only added, changed, and removed rows are
/// written; the boot generation row is a single row and is always upserted.
fn synchronize_rows(
    transaction: &Transaction<'_>,
    before: &LeaseDatabase,
    after: &LeaseDatabase,
) -> Result<()> {
    for write in pending_writes(before, after) {
        match write {
            LeaseRowWrite::Insert(lease) => write_row(transaction, lease, false)?,
            LeaseRowWrite::Update(lease) => write_row(transaction, lease, true)?,
            LeaseRowWrite::Delete(lease_id) => {
                let changed = transaction
                    .execute("DELETE FROM leases WHERE lease_id = ?1", params![lease_id])
                    .map_err(database_error)?;
                if changed == 0 {
                    return Err(MezError::invalid_state(
                        "remote session lease row is missing from the store",
                    ));
                }
            }
        }
    }
    synchronize_candidates(transaction, before, after)?;
    transaction
        .execute(
            "INSERT INTO lease_state (id, boot_generation) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET boot_generation = excluded.boot_generation",
            params![after.boot_generation as i64],
        )
        .map_err(database_error)?;
    Ok(())
}

/// Removes every stored row so a legacy import replaces the whole store.
fn clear_rows(transaction: &Transaction<'_>) -> Result<()> {
    transaction
        .execute("DELETE FROM leases", [])
        .map_err(database_error)?;
    transaction
        .execute("DELETE FROM snapshot_cleanup_candidates", [])
        .map_err(database_error)?;
    Ok(())
}
