//! SQLite connection policy and schema lifecycle for the session catalog.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::error::{MezError, Result};

use super::SCHEMA_VERSION;

/// Result of opening and validating the catalog during host initialization.
pub(super) enum InitializationOpen {
    /// The catalog schema and bounded integrity check succeeded.
    Ready(Connection),
    /// SQLite identified the existing file as corrupt or not a database.
    RebuildRequired,
}

/// Internal schema failure preserving SQLite error codes until recovery policy
/// has distinguished corruption from operational and version failures.
enum SchemaFailure {
    Sqlite(rusqlite::Error),
    Integrity(String),
    Semantic(MezError),
}

/// Opens one short-lived configured catalog connection.
pub(super) fn open(path: &Path) -> Result<Connection> {
    open_inner(path).map_err(SchemaFailure::into_mez_error)
}

/// Opens an existing catalog read-only without creating or migrating it.
pub(super) fn open_read_only(path: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(Duration::from_secs(1))?;
    Ok(connection)
}

/// Opens and validates an existing catalog while retaining exact corruption
/// classification for the initialization recovery path.
pub(super) fn open_for_initialization(path: &Path) -> Result<InitializationOpen> {
    match open_inner(path).and_then(|connection| {
        validate_inner(&connection)?;
        Ok(connection)
    }) {
        Ok(connection) => Ok(InitializationOpen::Ready(connection)),
        Err(SchemaFailure::Integrity(_)) => Ok(InitializationOpen::RebuildRequired),
        Err(SchemaFailure::Sqlite(error)) if is_corruption(&error) => {
            Ok(InitializationOpen::RebuildRequired)
        }
        Err(error) => Err(error.into_mez_error()),
    }
}

/// Opens and configures one catalog connection without discarding SQLite codes.
fn open_inner(path: &Path) -> std::result::Result<Connection, SchemaFailure> {
    let connection = Connection::open(path).map_err(SchemaFailure::Sqlite)?;
    connection
        .busy_timeout(Duration::from_secs(1))
        .map_err(SchemaFailure::Sqlite)?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
         PRAGMA trusted_schema = OFF;
         PRAGMA journal_mode = WAL;",
        )
        .map_err(SchemaFailure::Sqlite)?;
    initialize_schema(&connection)?;
    Ok(connection)
}

/// Performs a bounded integrity check without enumerating session payloads.
pub(super) fn validate(connection: &Connection) -> Result<()> {
    validate_inner(connection).map_err(SchemaFailure::into_mez_error)
}

/// Performs the bounded integrity query while preserving corruption details.
fn validate_inner(connection: &Connection) -> std::result::Result<(), SchemaFailure> {
    let result: String = connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(SchemaFailure::Sqlite)?;
    if result != "ok" {
        return Err(SchemaFailure::Integrity(result));
    }
    Ok(())
}

/// Checkpoints a rebuilt database into one self-contained replacement file.
pub(super) fn prepare_for_replacement(connection: &Connection) -> Result<()> {
    validate(connection)?;
    connection.execute_batch(
        "PRAGMA wal_checkpoint(TRUNCATE);
         PRAGMA journal_mode = DELETE;",
    )?;
    Ok(())
}

/// Converts an unsigned metadata value into SQLite's signed integer domain.
pub(super) fn sqlite_i64(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        MezError::invalid_args(format!(
            "saved-conversation catalog {field} exceeds SQLite integer range"
        ))
    })
}

/// Creates schema v1 or rejects unsupported database versions.
fn initialize_schema(connection: &Connection) -> std::result::Result<(), SchemaFailure> {
    connection
        .execute_batch("BEGIN IMMEDIATE;")
        .map_err(SchemaFailure::Sqlite)?;
    let result = initialize_schema_locked(connection);
    match result {
        Ok(()) => connection
            .execute_batch("COMMIT;")
            .map_err(SchemaFailure::Sqlite),
        Err(error) => {
            let _ = connection.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

/// Creates or validates the schema while the caller holds a write transaction.
fn initialize_schema_locked(connection: &Connection) -> std::result::Result<(), SchemaFailure> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(SchemaFailure::Sqlite)?;
    match version {
        0 => connection
            .execute_batch(
                "CREATE TABLE saved_conversations (
                 conversation_id TEXT PRIMARY KEY NOT NULL,
                 conversation_kind TEXT NOT NULL DEFAULT 'root'
                     CHECK (conversation_kind IN ('root', 'subagent')),
                 name TEXT,
                 named_at INTEGER CHECK (named_at >= 0),
                 entry_count INTEGER NOT NULL DEFAULT 0 CHECK (entry_count >= 0),
                 first_created_at INTEGER NOT NULL CHECK (first_created_at >= 0),
                 last_created_at INTEGER NOT NULL CHECK (last_created_at >= 0),
                 last_turn_id TEXT NOT NULL DEFAULT '',
                 agent_id TEXT NOT NULL DEFAULT '',
                 pane_id TEXT NOT NULL DEFAULT '',
                 directory TEXT,
                 initial_prompt TEXT,
                 latest_user_prompt TEXT,
                 has_transcript INTEGER NOT NULL DEFAULT 0
                     CHECK (has_transcript IN (0, 1)),
                 has_presentation INTEGER NOT NULL DEFAULT 0
                     CHECK (has_presentation IN (0, 1)),
                 payload_layout TEXT NOT NULL DEFAULT 'directory'
                     CHECK (payload_layout IN ('directory', 'legacy-tsv')),
                 catalog_updated_at INTEGER NOT NULL CHECK (catalog_updated_at >= 0),
                 CHECK ((name IS NULL) = (named_at IS NULL))
             );
             CREATE INDEX saved_conversations_latest_root
                 ON saved_conversations(
                     conversation_kind,
                     last_created_at DESC,
                     first_created_at DESC,
                     conversation_id
                 );
             CREATE INDEX saved_conversations_picker
                 ON saved_conversations(
                     conversation_kind,
                     (name IS NOT NULL) DESC,
                     last_created_at DESC,
                     first_created_at DESC,
                     conversation_id
                 );
             CREATE INDEX saved_conversations_directory_picker
                 ON saved_conversations(
                     directory,
                     conversation_kind,
                     (name IS NOT NULL) DESC,
                     last_created_at DESC,
                     first_created_at DESC,
                     conversation_id
                 );
             CREATE INDEX saved_conversations_pruning
                 ON saved_conversations(
                     (name IS NULL),
                     last_created_at,
                     first_created_at,
                     conversation_id
                 );
             CREATE INDEX saved_conversations_name_nocase
                 ON saved_conversations(name COLLATE NOCASE)
                 WHERE name IS NOT NULL;
             PRAGMA user_version = 1;",
            )
            .map_err(SchemaFailure::Sqlite)?,
        SCHEMA_VERSION => {}
        future if future > SCHEMA_VERSION => {
            return Err(SchemaFailure::Semantic(MezError::invalid_state(format!(
                "saved-conversation catalog schema version {future} is newer than supported version {SCHEMA_VERSION}"
            ))));
        }
        other => {
            return Err(SchemaFailure::Semantic(MezError::invalid_state(format!(
                "unsupported saved-conversation catalog schema version {other}"
            ))));
        }
    }
    Ok(())
}

/// Returns whether one SQLite failure proves the file needs reconstruction.
fn is_corruption(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
    )
}

impl SchemaFailure {
    /// Converts a non-recoverable schema failure into the shared product error.
    fn into_mez_error(self) -> MezError {
        match self {
            Self::Sqlite(error) => error.into(),
            Self::Integrity(result) => MezError::invalid_state(format!(
                "saved-conversation catalog integrity check failed: {result}"
            )),
            Self::Semantic(error) => error,
        }
    }
}
