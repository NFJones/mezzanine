//! SQLite-backed session registry operations.
//!
//! The registry used to rewrite one whole TSV file per mutation. It now keeps
//! the same records in a private `sessions.sqlite` database: readers open it
//! read-only and never take the registry lock, and writers keep the historical
//! exclusive flock so an older binary running concurrently cannot interleave a
//! flat-file mutation with a database mutation. The legacy flat file is imported
//! exactly once, under that lock, and is never modified here, so a rollback to a
//! previous build still finds its data.

use super::{
    MezError, PathBuf, REGISTRY_FILE_NAME, RegistrySessionState, Result, SessionRecord,
    SessionRegistry, decode_records, ensure_private_socket_directory, fs,
};
use crate::storage::shared_sqlite::{
    SharedSchemaState, import_legacy_file_once, migration_completed, open_shared_database,
    open_shared_database_read_only, set_schema_version,
};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};

/// Database file owned by the session registry.
const REGISTRY_DATABASE_FILE_NAME: &str = "sessions.sqlite";

/// Schema version owned by the session registry database.
///
/// Version 2 added the non-negative CHECK constraints; a database created by an
/// intermediate build is rejected instead of being accepted without them.
const REGISTRY_SCHEMA_VERSION: i64 = 2;

/// One-time import marker for the legacy flat registry file.
const REGISTRY_IMPORT_MARKER: &str = "sessions.tsv";

/// Columns selected by every registry read, in row-mapping order.
const RECORD_COLUMNS: &str = "record_version, control_version, session_id, name, state, socket_path, created_at_unix_seconds, last_attach_at_unix_seconds, window_count, attached_client_count, attached_primary_count, max_attached_primaries, accepts_primary, layout_owner_client_id, authoritative_columns, authoritative_rows";

/// Returns the registry database path for one root.
pub(super) fn database_path(registry: &SessionRegistry) -> PathBuf {
    registry.root.join(REGISTRY_DATABASE_FILE_NAME)
}

/// Returns the legacy flat registry path for one root.
fn legacy_path(registry: &SessionRegistry) -> PathBuf {
    registry.root.join(REGISTRY_FILE_NAME)
}

/// Returns whether either registry representation exists yet.
fn registry_has_records(registry: &SessionRegistry) -> bool {
    database_path(registry).exists() || legacy_path(registry).exists()
}

/// Maps one rusqlite failure to a retryable registry error.
fn database_error(error: rusqlite::Error) -> MezError {
    MezError::invalid_state(format!("registry database error: {error}"))
}

/// Returns every registry record, sorted by session id.
///
/// The database is read read-only whenever it exists, so a short-lived CLI
/// reader neither creates the database nor migrates anything. Before the first
/// writer creates it, the legacy flat file is decoded instead, which keeps the
/// transition window readable without a migration side effect.
pub(super) fn list(registry: &SessionRegistry) -> Result<Vec<SessionRecord>> {
    let mut records = if database_path(registry).exists() {
        match open_shared_database_read_only(&database_path(registry))? {
            Some(connection) => {
                // The database is authoritative only once the one-time import
                // completed; a database created before a failed import must not
                // hide the legacy rows.
                if migration_completed(&connection, REGISTRY_IMPORT_MARKER)? {
                    read_records(&connection)?
                } else {
                    legacy_records(registry)?
                }
            }
            None => legacy_records(registry)?,
        }
    } else {
        legacy_records(registry)?
    };
    records.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    Ok(records)
}

/// Returns one registry record by exact session id.
#[cfg(test)]
pub(super) fn get(registry: &SessionRegistry, session_id: &str) -> Result<Option<SessionRecord>> {
    Ok(list(registry)?
        .into_iter()
        .find(|record| record.session_id == session_id))
}

/// Decodes the legacy flat registry when it exists.
fn legacy_records(registry: &SessionRegistry) -> Result<Vec<SessionRecord>> {
    let path = legacy_path(registry);
    if !path.exists() {
        return Ok(Vec::new());
    }
    decode_records(&fs::read_to_string(path)?)
}

/// Opens the registry database, creating and importing it when needed.
///
/// Callers hold the exclusive registry lock, which is what keeps the one-time
/// import from racing an older binary that still writes the flat file.
fn open_registry_database(registry: &SessionRegistry) -> Result<Connection> {
    let path = database_path(registry);
    let (mut connection, state) = open_shared_database(&path, REGISTRY_SCHEMA_VERSION)?;
    if state == SharedSchemaState::Fresh {
        create_schema(&mut connection)?;
    }
    import_legacy_registry(registry, &mut connection)?;
    Ok(connection)
}

/// Creates the registry schema and publishes its version atomically.
fn create_schema(connection: &mut Connection) -> Result<()> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    transaction
        .execute_batch("CREATE TABLE sessions (record_version INTEGER NOT NULL, control_version INTEGER NOT NULL, session_id TEXT PRIMARY KEY, name TEXT NOT NULL, state TEXT NOT NULL, socket_path TEXT NOT NULL, created_at_unix_seconds INTEGER NOT NULL, last_attach_at_unix_seconds INTEGER, window_count INTEGER NOT NULL, attached_client_count INTEGER NOT NULL, attached_primary_count INTEGER NOT NULL, max_attached_primaries INTEGER NOT NULL, accepts_primary INTEGER NOT NULL, layout_owner_client_id TEXT, authoritative_columns INTEGER NOT NULL, authoritative_rows INTEGER NOT NULL, CHECK (created_at_unix_seconds >= 0 AND (last_attach_at_unix_seconds IS NULL OR last_attach_at_unix_seconds >= 0) AND window_count >= 0 AND attached_client_count >= 0 AND attached_primary_count >= 0 AND max_attached_primaries >= 0 AND authoritative_columns >= 0 AND authoritative_rows >= 0)); CREATE INDEX sessions_by_state ON sessions(state); CREATE INDEX sessions_by_last_attach ON sessions(last_attach_at_unix_seconds DESC);")
        .map_err(database_error)?;
    set_schema_version(&transaction, REGISTRY_SCHEMA_VERSION)?;
    transaction.commit().map_err(database_error)
}

/// Imports the legacy flat registry exactly once.
fn import_legacy_registry(registry: &SessionRegistry, connection: &mut Connection) -> Result<()> {
    if migration_completed(connection, REGISTRY_IMPORT_MARKER)? {
        return Ok(());
    }
    import_legacy_file_once(connection, REGISTRY_IMPORT_MARKER, |transaction| {
        let mut imported = 0_i64;
        for record in legacy_records(registry)? {
            record.validate()?;
            insert_record(transaction, &record)?;
            imported += 1;
        }
        Ok(imported)
    })?;
    Ok(())
}

/// Reads every record from an open database connection.
fn read_records(connection: &Connection) -> Result<Vec<SessionRecord>> {
    let mut statement = connection
        .prepare(&format!("SELECT {RECORD_COLUMNS} FROM sessions"))
        .map_err(database_error)?;
    let rows = statement.query_map([], raw_row).map_err(database_error)?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row.map_err(database_error)?.into_record()?);
    }
    Ok(records)
}

/// Projects one database row into owned column values.
fn raw_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRegistryRow> {
    Ok(RawRegistryRow {
        record_version: row.get(0)?,
        control_version: row.get(1)?,
        session_id: row.get(2)?,
        name: row.get(3)?,
        state: row.get(4)?,
        socket_path: row.get(5)?,
        created_at_unix_seconds: row.get::<_, i64>(6)? as u64,
        last_attach_at_unix_seconds: row.get::<_, Option<i64>>(7)?.map(|value| value as u64),
        window_count: row.get::<_, i64>(8)? as usize,
        attached_client_count: row.get::<_, i64>(9)? as usize,
        attached_primary_count: row.get::<_, i64>(10)? as usize,
        max_attached_primaries: row.get::<_, i64>(11)? as usize,
        accepts_primary: row.get(12)?,
        layout_owner_client_id: row.get(13)?,
        authoritative_columns: row.get::<_, i64>(14)? as u16,
        authoritative_rows: row.get::<_, i64>(15)? as u16,
    })
}

/// Owned column values for one registry row.
struct RawRegistryRow {
    record_version: u32,
    control_version: u32,
    session_id: String,
    name: String,
    state: String,
    socket_path: String,
    created_at_unix_seconds: u64,
    last_attach_at_unix_seconds: Option<u64>,
    window_count: usize,
    attached_client_count: usize,
    attached_primary_count: usize,
    max_attached_primaries: usize,
    accepts_primary: bool,
    layout_owner_client_id: Option<String>,
    authoritative_columns: u16,
    authoritative_rows: u16,
}

impl RawRegistryRow {
    /// Validates and converts one row into a registry record.
    fn into_record(self) -> Result<SessionRecord> {
        let record = SessionRecord {
            record_version: self.record_version,
            control_version: self.control_version,
            session_id: self.session_id,
            name: self.name,
            state: RegistrySessionState::parse(&self.state)?,
            socket_path: PathBuf::from(self.socket_path),
            created_at_unix_seconds: self.created_at_unix_seconds,
            last_attach_at_unix_seconds: self.last_attach_at_unix_seconds,
            window_count: self.window_count,
            attached_client_count: self.attached_client_count,
            attached_primary_count: self.attached_primary_count,
            max_attached_primaries: self.max_attached_primaries,
            accepts_primary: self.accepts_primary,
            layout_owner_client_id: self.layout_owner_client_id,
            authoritative_columns: self.authoritative_columns,
            authoritative_rows: self.authoritative_rows,
        };
        record.validate()?;
        Ok(record)
    }
}

/// Inserts or replaces one record inside an open transaction.
fn insert_record(transaction: &Transaction<'_>, record: &SessionRecord) -> Result<()> {
    let socket_path = record
        .socket_path
        .to_str()
        .ok_or_else(|| MezError::invalid_args("socket path must be valid UTF-8"))?;
    transaction
        .execute(
            "INSERT OR REPLACE INTO sessions (record_version, control_version, session_id, name, state, socket_path, created_at_unix_seconds, last_attach_at_unix_seconds, window_count, attached_client_count, attached_primary_count, max_attached_primaries, accepts_primary, layout_owner_client_id, authoritative_columns, authoritative_rows) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                record.record_version,
                record.control_version,
                record.session_id,
                record.name,
                record.state.as_str(),
                socket_path,
                record.created_at_unix_seconds as i64,
                record.last_attach_at_unix_seconds.map(|value| value as i64),
                record.window_count as i64,
                record.attached_client_count as i64,
                record.attached_primary_count as i64,
                record.max_attached_primaries as i64,
                record.accepts_primary,
                record.layout_owner_client_id,
                record.authoritative_columns as i64,
                record.authoritative_rows as i64,
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

/// Stores one record, creating and importing the database when needed.
pub(super) fn upsert(registry: &SessionRegistry, record: SessionRecord) -> Result<()> {
    record.validate()?;
    let _lock = registry.acquire_exclusive_lock()?;
    ensure_private_socket_directory(&registry.root, registry.owner_uid)?;
    let mut connection = open_registry_database(registry)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    insert_record(&transaction, &record)?;
    transaction.commit().map_err(database_error)
}

/// Deletes one record by exact session id.
pub(super) fn remove(registry: &SessionRegistry, session_id: &str) -> Result<bool> {
    if !registry_has_records(registry) {
        return Ok(false);
    }
    let _lock = registry.acquire_exclusive_lock()?;
    ensure_private_socket_directory(&registry.root, registry.owner_uid)?;
    let mut connection = open_registry_database(registry)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    let removed = transaction
        .execute(
            "DELETE FROM sessions WHERE session_id = ?1",
            params![session_id],
        )
        .map_err(database_error)?;
    transaction.commit().map_err(database_error)?;
    Ok(removed > 0)
}

/// Deletes every record whose socket path no longer exists.
pub(super) fn prune_stale(registry: &SessionRegistry) -> Result<usize> {
    if !registry_has_records(registry) {
        return Ok(0);
    }
    let _lock = registry.acquire_exclusive_lock()?;
    ensure_private_socket_directory(&registry.root, registry.owner_uid)?;
    let mut connection = open_registry_database(registry)?;
    let stale = read_records(&connection)?
        .into_iter()
        .filter(|record| !record.socket_path.exists())
        .map(|record| record.session_id)
        .collect::<Vec<_>>();
    if stale.is_empty() {
        return Ok(0);
    }
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    for session_id in &stale {
        transaction
            .execute(
                "DELETE FROM sessions WHERE session_id = ?1",
                params![session_id],
            )
            .map_err(database_error)?;
    }
    transaction.commit().map_err(database_error)?;
    Ok(stale.len())
}
