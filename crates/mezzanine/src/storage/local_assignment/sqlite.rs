//! SQLite storage layer for durable local-session assignments.
//!
//! The repository used to rewrite one JSON document per mutation and refused to
//! grow past two megabytes. It now keeps the same records in a private
//! `assignments.sqlite` database: key columns carry the fields the repository
//! addresses and filters by (`session_id`, `state`, `boot_generation`) and a
//! payload column carries the validated record, so a future phase can move
//! individual updates into SQL without another format change. The database is
//! deliberately separate from the remote-session lease database and lives in a
//! directory of its own: `storage::shared_sqlite` owns one database per
//! concern, so each store keeps its own exclusive lock and one schema version
//! no other store can change. The legacy `assignments.json` is imported exactly
//! once and is never modified, so a rollback to a previous build still finds
//! its data.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, Transaction, TransactionBehavior, params};

use super::super::shared_sqlite::{
    SharedSchemaState, export_tsv, import_legacy_file_once, migration_completed,
    open_shared_database, open_shared_database_read_only, read_private_legacy_file, schema_version,
    set_schema_version,
};
use super::repository::{LocalAssignmentDatabase, validate_database};
use super::{LocalSessionAssignment, LocalSessionAssignmentState, MezError, Result};

/// Database file owned by the local session assignment repository.
pub(super) const ASSIGNMENT_DATABASE_FILE_NAME: &str = "assignments.sqlite";

/// Schema version owned by the assignment tables.
const ASSIGNMENT_SCHEMA_VERSION: i64 = 1;

/// One-time import marker for the legacy JSON document.
const ASSIGNMENT_IMPORT_MARKER: &str = "assignments.json";

/// Legacy JSON document retained for import and rollback.
const LEGACY_DATABASE_FILE_NAME: &str = "assignments.json";

/// Returns the assignment database path for one repository directory.
pub(super) fn database_path(directory: &Path) -> PathBuf {
    directory.join(ASSIGNMENT_DATABASE_FILE_NAME)
}

/// Returns the legacy JSON path for one repository directory.
fn legacy_path(directory: &Path) -> PathBuf {
    directory.join(LEGACY_DATABASE_FILE_NAME)
}

/// Maps one rusqlite failure to an actionable assignment error.
fn database_error(error: rusqlite::Error) -> MezError {
    MezError::invalid_state(format!("local session assignment database error: {error}"))
}

/// Encodes one assignment state token into its stored snake_case form.
fn encode_state(state: LocalSessionAssignmentState) -> String {
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

/// Renders the assignment store in its inspection TSV shape without creating
/// it.
///
/// Returns `None` when neither representation exists, so an inspection command
/// never creates the store, and the rows come from the same read-only path the
/// listing command uses, so an export cannot block a daemon writer. Rows sort
/// by session id.
pub(super) fn export_tsv_read_only(directory: &Path) -> Result<Option<String>> {
    let path = database_path(directory);
    let legacy = legacy_path(directory);
    reject_symlink(&path)?;
    if !path_exists(&path) && !path_exists(&legacy) {
        return Ok(None);
    }
    let database = load_database(directory)?;
    let mut assignments = database.assignments;
    assignments.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    let rows = assignments
        .iter()
        .map(|assignment| {
            vec![
                assignment.session_id.clone(),
                encode_state(assignment.state),
                assignment.default_for_host.to_string(),
                assignment.boot_generation.to_string(),
                assignment.assignment_generation.to_string(),
                assignment.created_at_unix_seconds.to_string(),
                assignment.updated_at_unix_seconds.to_string(),
                assignment
                    .checkpoint
                    .as_ref()
                    .map(|checkpoint| checkpoint.snapshot_id.clone())
                    .unwrap_or_default(),
                assignment.failure.clone().unwrap_or_default(),
            ]
        })
        .collect::<Vec<_>>();
    Ok(Some(export_tsv(
        &[
            "session_id",
            "state",
            "default_for_host",
            "boot_generation",
            "assignment_generation",
            "created_at_unix_seconds",
            "updated_at_unix_seconds",
            "checkpoint_snapshot_id",
            "failure",
        ],
        &rows,
    )))
}

/// Refuses a database path that is a symbolic link.
fn reject_symlink(path: &Path) -> Result<()> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        return Err(MezError::invalid_state(format!(
            "local session assignment database path {} must not be a symbolic link",
            path.display()
        )));
    }
    Ok(())
}

/// Loads the assignment database without creating, migrating, or importing
/// anything.
///
/// Reads open the database read-only when it exists and otherwise decode the
/// legacy JSON document, so a short-lived reader never turns a legacy file
/// into a database as a side effect of looking at it.
pub(super) fn load_database(directory: &Path) -> Result<LocalAssignmentDatabase> {
    let path = database_path(directory);
    reject_symlink(&path)?;
    if !path.exists() {
        return legacy_database(directory);
    }
    let Some(connection) = open_shared_database_read_only(&path)? else {
        return legacy_database(directory);
    };
    let version = schema_version(&connection)?;
    if version != ASSIGNMENT_SCHEMA_VERSION {
        return Err(MezError::invalid_state(format!(
            "local session assignment database schema version {version} does not match this build's version {ASSIGNMENT_SCHEMA_VERSION}; restart with the build that wrote it, or delete {} and let the next write import the legacy JSON document",
            path.display()
        )));
    }
    if !migration_completed(&connection, ASSIGNMENT_IMPORT_MARKER)? {
        return legacy_database(directory);
    }
    read_database(&connection)
}

/// Writes the assignment database inside one immediate transaction.
///
/// The caller holds the repository's exclusive lock, which is what makes the
/// one-time legacy import safe against an older binary still writing the JSON
/// document.
pub(super) fn write_database(directory: &Path, database: &LocalAssignmentDatabase) -> Result<()> {
    let path = database_path(directory);
    reject_symlink(&path)?;
    let (mut connection, state) = open_shared_database(&path, ASSIGNMENT_SCHEMA_VERSION)?;
    if state == SharedSchemaState::Fresh {
        create_schema(&mut connection)?;
    }
    import_legacy_database(directory, &mut connection)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    replace_rows(&transaction, database)?;
    transaction.commit().map_err(database_error)
}

/// Creates the assignment schema and publishes its version atomically.
fn create_schema(connection: &mut Connection) -> Result<()> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    transaction
        .execute_batch("CREATE TABLE assignments (session_id TEXT PRIMARY KEY, state TEXT NOT NULL, boot_generation INTEGER NOT NULL CHECK (boot_generation >= 0), payload TEXT NOT NULL); CREATE INDEX assignments_by_state ON assignments(state); CREATE INDEX assignments_by_boot_generation ON assignments(boot_generation); CREATE TABLE assignment_state (id INTEGER PRIMARY KEY CHECK (id = 1), boot_generation INTEGER NOT NULL CHECK (boot_generation >= 0));")
        .map_err(database_error)?;
    set_schema_version(&transaction, ASSIGNMENT_SCHEMA_VERSION)?;
    transaction.commit().map_err(database_error)
}

/// Imports the legacy JSON document exactly once.
fn import_legacy_database(directory: &Path, connection: &mut Connection) -> Result<()> {
    if migration_completed(connection, ASSIGNMENT_IMPORT_MARKER)? {
        return Ok(());
    }
    let legacy = legacy_database(directory)?;
    import_legacy_file_once(connection, ASSIGNMENT_IMPORT_MARKER, |transaction| {
        replace_rows(transaction, &legacy)?;
        Ok(legacy.assignments.len() as i64)
    })?;
    Ok(())
}

/// Decodes and validates the legacy JSON document when it exists.
///
/// The legacy document keeps the private-file checks the JSON repository
/// applied to it - a regular file owned by the current user without group or
/// other access - and is read through the same no-follow descriptor that was
/// validated, so replacing the path between the check and the read cannot
/// substitute another file. Its declared size is no longer capped, because
/// that cap is the failure mode this conversion removes.
fn legacy_database(directory: &Path) -> Result<LocalAssignmentDatabase> {
    let path = legacy_path(directory);
    let Some(bytes) = read_private_legacy_file(&path)? else {
        return Ok(LocalAssignmentDatabase::default());
    };
    let database: LocalAssignmentDatabase = serde_json::from_slice(&bytes).map_err(|error| {
        MezError::invalid_state(format!(
            "local session assignment database is malformed: {error}"
        ))
    })?;
    validate_database(&database)?;
    Ok(database)
}

/// Reads every stored row back into the in-memory database model.
fn read_database(connection: &Connection) -> Result<LocalAssignmentDatabase> {
    let mut database = LocalAssignmentDatabase::default();
    let boot_generation: Option<i64> = connection
        .query_row(
            "SELECT boot_generation FROM assignment_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .ok();
    if let Some(boot_generation) = boot_generation {
        database.boot_generation = boot_generation as u64;
    }
    let mut statement = connection
        .prepare("SELECT state, payload FROM assignments")
        .map_err(database_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(database_error)?;
    for row in rows {
        let (state, payload) = row.map_err(database_error)?;
        let assignment: LocalSessionAssignment =
            serde_json::from_str(&payload).map_err(|error| {
                MezError::invalid_state(format!(
                    "local session assignment row is malformed: {error}"
                ))
            })?;
        let payload_state = encode_state(assignment.state);
        if state != payload_state {
            return Err(MezError::invalid_state(format!(
                "local session assignment row {} state {state} does not match its payload state {payload_state}",
                assignment.session_id
            )));
        }
        database.assignments.push(assignment);
    }
    validate_database(&database)?;
    Ok(database)
}

/// Replaces every stored row with the in-memory database state.
fn replace_rows(transaction: &Transaction<'_>, database: &LocalAssignmentDatabase) -> Result<()> {
    transaction
        .execute("DELETE FROM assignments", [])
        .map_err(database_error)?;
    for assignment in &database.assignments {
        let payload = serde_json::to_string(assignment).map_err(|error| {
            MezError::invalid_state(format!(
                "failed to encode local session assignment row: {error}"
            ))
        })?;
        let state = encode_state(assignment.state);
        transaction
            .execute(
                "INSERT INTO assignments (session_id, state, boot_generation, payload) VALUES (?1, ?2, ?3, ?4)",
                params![
                    assignment.session_id,
                    state,
                    assignment.boot_generation as i64,
                    payload,
                ],
            )
            .map_err(database_error)?;
    }
    transaction
        .execute(
            "INSERT INTO assignment_state (id, boot_generation) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET boot_generation = excluded.boot_generation",
            params![database.boot_generation as i64],
        )
        .map_err(database_error)?;
    Ok(())
}
