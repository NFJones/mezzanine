//! SQLite storage for the durable project trust database.
//!
//! The store used to rewrite one TSV document per decision under an advisory
//! lock. It now keeps the same records in a private `project-trust.sqlite`
//! database: one column per validated record field, with the identity and
//! version columns (`project_root`, `trust_policy_version`,
//! `configuration_schema_version`) indexed for version-filtered lookups. Reads
//! open read-only and fall back to the legacy `project-trust.tsv` document
//! until the one-time import marker exists, so an older build's decisions are
//! never rewritten or lost; the legacy file is imported exactly once and then
//! left untouched.
//!
//! The database is deliberately separate from every session-state store: trust
//! records are authorization-adjacent, are written by both the CLI and the
//! daemon, and keep an independent failure domain, so a trust write must never
//! create or migrate another store's files.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use super::encoding::parse_record_line;
use super::{
    MezError, ProjectTrustRecord, ProjectTrustRevision, ProjectTrustSnapshot, ProjectTrustStore,
    Result, TrustDecision,
};
use crate::storage::shared_sqlite::{
    SharedSchemaState, import_legacy_file_once, migration_completed, open_shared_database,
    open_shared_database_read_only, schema_version, set_schema_version,
};

/// Database file owned by the project trust store.
pub(super) const PROJECT_TRUST_DATABASE_FILE_NAME: &str = "project-trust.sqlite";

/// Legacy TSV document imported exactly once.
pub(super) const LEGACY_TRUST_FILE_NAME: &str = "project-trust.tsv";

/// Header the legacy document carried, reused by the export shape and the
/// canonical revision encoding.
pub(super) const LEGACY_TRUST_HEADER: &str = "# Mezzanine project trust database v1";

/// Schema version owned by the trust table.
const TRUST_SCHEMA_VERSION: i64 = 1;

/// Returns the legacy TSV path that sits beside one trust database path.
fn legacy_path(database_path: &Path) -> PathBuf {
    database_path.with_file_name(LEGACY_TRUST_FILE_NAME)
}

/// Maps one rusqlite failure to an actionable trust error.
fn database_error(error: rusqlite::Error) -> MezError {
    if let rusqlite::Error::SqliteFailure(code, _) = &error
        && matches!(
            code.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        )
    {
        return MezError::conflict(format!(
            "project trust database is busy; retry the request ({error})"
        ));
    }
    MezError::config(format!("project trust database error: {error}"))
}

/// Refuses a database path that is a symbolic link.
fn reject_symlink(path: &Path) -> Result<()> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        return Err(MezError::config(format!(
            "project trust database path {} must not be a symbolic link",
            path.display()
        )));
    }
    Ok(())
}

/// Reports whether one path exists, including a dangling symbolic link.
fn path_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Loads trust records together with the revision of their persisted contents.
///
/// Reads never create, migrate, or import the store: a missing database (or a
/// database whose schema version or import marker is not published yet) is
/// served from the legacy TSV document instead.
pub(super) fn load_snapshot(database_path: &Path) -> Result<ProjectTrustSnapshot> {
    reject_symlink(database_path)?;
    if path_exists(database_path)
        && let Some(connection) = open_shared_database_read_only(database_path)?
    {
        let version = schema_version(&connection)?;
        if version == 0 {
            return legacy_snapshot(database_path);
        }
        if version != TRUST_SCHEMA_VERSION {
            return Err(MezError::config(format!(
                "project trust database schema version {version} does not match this build's version {TRUST_SCHEMA_VERSION}; restart with the build that wrote it, or delete {} and let the next write import the legacy document",
                database_path.display()
            )));
        }
        if migration_completed(&connection, LEGACY_TRUST_FILE_NAME)? {
            return Ok(snapshot(read_store(&connection)?));
        }
    }
    legacy_snapshot(database_path)
}

/// Applies one read-modify-write trust update inside a transaction.
///
/// The callback receives the latest persisted store; its successful mutation is
/// committed before the resulting snapshot is returned, so independent CLI and
/// daemon writers cannot drop each other's records.
pub(super) fn update<F>(database_path: &Path, update: F) -> Result<ProjectTrustSnapshot>
where
    F: FnOnce(&mut ProjectTrustStore) -> Result<()>,
{
    update_once(database_path, update)
}

/// Persists one complete store, importing the legacy document first when needed.
pub(super) fn save(database_path: &Path, store: &ProjectTrustStore) -> Result<()> {
    save_once(database_path, store)
}

/// Creates the trust schema and publishes its version atomically.
fn create_schema(connection: &mut Connection) -> Result<()> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    transaction
        .execute_batch("CREATE TABLE IF NOT EXISTS project_trust (project_root TEXT PRIMARY KEY, state TEXT NOT NULL, trusted_at_unix_seconds INTEGER NOT NULL CHECK (trusted_at_unix_seconds >= 0), decided_by_client_id TEXT, git_marker_path TEXT, trust_policy_version INTEGER NOT NULL CHECK (trust_policy_version >= 0), configuration_schema_version INTEGER NOT NULL CHECK (configuration_schema_version >= 0), vcs_remote TEXT); CREATE INDEX IF NOT EXISTS project_trust_by_versions ON project_trust(trust_policy_version, configuration_schema_version);")
        .map_err(database_error)?;
    set_schema_version(&transaction, TRUST_SCHEMA_VERSION)?;
    transaction.commit().map_err(database_error)
}

/// Opens the store for one update attempt.
fn open_for_write(database_path: &Path) -> Result<Connection> {
    reject_symlink(database_path)?;
    let (mut connection, state) = open_shared_database(database_path, TRUST_SCHEMA_VERSION)?;
    if state == SharedSchemaState::Fresh {
        create_schema(&mut connection)?;
    }
    import_legacy_once(database_path, &mut connection)?;
    Ok(connection)
}

/// Runs one update attempt.
fn update_once<F>(database_path: &Path, update: F) -> Result<ProjectTrustSnapshot>
where
    F: FnOnce(&mut ProjectTrustStore) -> Result<()>,
{
    let mut connection = open_for_write(database_path)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    let before = read_store(&transaction)?;
    let mut store = before.clone();
    update(&mut store)?;
    write_diff(&transaction, &before, &store)?;
    transaction.commit().map_err(database_error)?;
    Ok(snapshot(store))
}

/// Runs one complete-store write attempt.
fn save_once(database_path: &Path, store: &ProjectTrustStore) -> Result<()> {
    let mut connection = open_for_write(database_path)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    let before = read_store(&transaction)?;
    write_diff(&transaction, &before, store)?;
    transaction.commit().map_err(database_error)
}

/// Imports the legacy TSV document exactly once.
fn import_legacy_once(database_path: &Path, connection: &mut Connection) -> Result<()> {
    if migration_completed(connection, LEGACY_TRUST_FILE_NAME)? {
        return Ok(());
    }
    let legacy = legacy_store(database_path)?;
    import_legacy_file_once(connection, LEGACY_TRUST_FILE_NAME, |transaction| {
        transaction
            .execute("DELETE FROM project_trust", [])
            .map_err(database_error)?;
        for record in legacy.records.values() {
            insert_row(transaction, record)?;
        }
        Ok(legacy.records.len() as i64)
    })?;
    Ok(())
}

/// Reads every stored row into the in-memory store.
fn read_store(connection: &Connection) -> Result<ProjectTrustStore> {
    let mut statement = connection
        .prepare("SELECT project_root, state, trusted_at_unix_seconds, decided_by_client_id, git_marker_path, trust_policy_version, configuration_schema_version, vcs_remote FROM project_trust")
        .map_err(database_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })
        .map_err(database_error)?;
    let mut store = ProjectTrustStore::default();
    for row in rows {
        let (project_root, state, trusted_at, client, marker, policy, schema, remote) =
            row.map_err(database_error)?;
        let record = ProjectTrustRecord {
            project_root: PathBuf::from(project_root),
            state: TrustDecision::parse(&state)?,
            git_marker_path: marker.map(PathBuf::from),
            trusted_at_unix_seconds: u64::try_from(trusted_at).map_err(|_| {
                MezError::config("project trust row has a negative decision timestamp")
            })?,
            decided_by_client_id: client,
            trust_policy_version: u32::try_from(policy)
                .map_err(|_| MezError::config("project trust row has a negative policy version"))?,
            configuration_schema_version: u32::try_from(schema).map_err(|_| {
                MezError::config("project trust row has a negative configuration schema version")
            })?,
            vcs_remote: remote,
        };
        store.records.insert(record.project_root.clone(), record);
    }
    Ok(store)
}

/// Persists the difference between two stores inside one transaction.
fn write_diff(
    transaction: &Transaction<'_>,
    before: &ProjectTrustStore,
    after: &ProjectTrustStore,
) -> Result<()> {
    for project_root in before.records.keys() {
        if !after.records.contains_key(project_root) {
            let removed = transaction
                .execute(
                    "DELETE FROM project_trust WHERE project_root = ?1",
                    params![project_root.to_string_lossy().into_owned()],
                )
                .map_err(database_error)?;
            if removed == 0 {
                return Err(MezError::config(
                    "project trust row is missing from the store",
                ));
            }
        }
    }
    for (project_root, record) in &after.records {
        match before.records.get(project_root) {
            Some(existing) if existing == record => {}
            Some(_) => {
                let project_root_text = project_root.to_string_lossy().into_owned();
                let state = record.state.as_str();
                let git_marker_path = record
                    .git_marker_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned());
                let updated = transaction
                    .execute(
                        "UPDATE project_trust SET state = ?2, trusted_at_unix_seconds = ?3, decided_by_client_id = ?4, git_marker_path = ?5, trust_policy_version = ?6, configuration_schema_version = ?7, vcs_remote = ?8 WHERE project_root = ?1",
                        params![
                            project_root_text,
                            state,
                            record.trusted_at_unix_seconds as i64,
                            record.decided_by_client_id,
                            git_marker_path,
                            record.trust_policy_version,
                            record.configuration_schema_version,
                            record.vcs_remote,
                        ],
                    )
                    .map_err(database_error)?;
                if updated == 0 {
                    return Err(MezError::config(
                        "project trust row is missing from the store",
                    ));
                }
            }
            None => insert_row(transaction, record)?,
        }
    }
    Ok(())
}

/// Inserts one record row.
fn insert_row(transaction: &Transaction<'_>, record: &ProjectTrustRecord) -> Result<()> {
    let project_root = record.project_root.to_string_lossy().into_owned();
    let state = record.state.as_str();
    let git_marker_path = record
        .git_marker_path
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());
    transaction
        .execute(
            "INSERT INTO project_trust (project_root, state, trusted_at_unix_seconds, decided_by_client_id, git_marker_path, trust_policy_version, configuration_schema_version, vcs_remote) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                project_root,
                state,
                record.trusted_at_unix_seconds as i64,
                record.decided_by_client_id,
                git_marker_path,
                record.trust_policy_version,
                record.configuration_schema_version,
                record.vcs_remote,
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

/// Renders the canonical persisted contents of one store.
///
/// The text is exactly what the legacy writer produced, so the revision digest
/// is stable across the conversion for unchanged records and a reader reloads
/// only when the logical contents change.
fn canonical_contents(store: &ProjectTrustStore) -> String {
    let mut text = String::new();
    text.push_str(LEGACY_TRUST_HEADER);
    text.push('\n');
    for record in store.records.values() {
        text.push_str(&record.to_line());
        text.push('\n');
    }
    text
}

/// Builds a snapshot with the digest revision of one store's contents.
fn snapshot(store: ProjectTrustStore) -> ProjectTrustSnapshot {
    let revision = ProjectTrustRevision::Sha256(
        Sha256::digest(canonical_contents(&store).as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    );
    ProjectTrustSnapshot { store, revision }
}

/// Loads the legacy TSV document when it exists, keeping the revision of its
/// exact bytes.
fn legacy_snapshot(database_path: &Path) -> Result<ProjectTrustSnapshot> {
    let path = legacy_path(database_path);
    reject_symlink(&path)?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProjectTrustSnapshot {
                store: ProjectTrustStore::default(),
                revision: ProjectTrustRevision::Missing,
            });
        }
        Err(error) => return Err(error.into()),
    };
    let revision = ProjectTrustRevision::Sha256(
        Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    );
    Ok(ProjectTrustSnapshot {
        store: decode_legacy_store(&bytes)?,
        revision,
    })
}

/// Decodes one legacy TSV document into a store.
fn legacy_store(database_path: &Path) -> Result<ProjectTrustStore> {
    let path = legacy_path(database_path);
    reject_symlink(&path)?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProjectTrustStore::default());
        }
        Err(error) => return Err(error.into()),
    };
    decode_legacy_store(&bytes)
}

/// Decodes legacy TSV bytes into a store.
fn decode_legacy_store(bytes: &[u8]) -> Result<ProjectTrustStore> {
    let text = String::from_utf8(bytes.to_vec())
        .map_err(|_| MezError::config("project trust database is not valid UTF-8"))?;
    let mut store = ProjectTrustStore::default();
    for line in text.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let fields = parse_record_line(line)?;
        let record = ProjectTrustRecord::from_fields(&fields)?;
        store.records.insert(record.project_root.clone(), record);
    }
    Ok(store)
}

/// Renders the trust database in the legacy TSV shape without creating it.
pub(super) fn export_tsv_read_only(database_path: &Path) -> Result<Option<String>> {
    let snapshot = load_snapshot(database_path)?;
    if snapshot.revision == ProjectTrustRevision::Missing {
        return Ok(None);
    }
    Ok(Some(canonical_contents(&snapshot.store)))
}
