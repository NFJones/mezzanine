//! Rebuildable SQLite discovery catalog for saved agent conversations.
//!
//! The catalog contains only bounded metadata used to find saved sessions. The
//! transcript and presentation files remain authoritative for conversation
//! content, and retained sidecars allow this database to be rebuilt. Catalog
//! initialization is serialized with an advisory lock so concurrent host
//! startups cannot observe a partially migrated database.

mod migration;
mod mutation;
mod query;
mod schema;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use mez_agent::AgentConversationKind;
use mez_agent::transcript::ConversationSummary;
use rustix::fs::{FlockOperation, flock};

use crate::error::Result;

use super::fs::{set_private_dir_permissions, set_private_file_permissions};
use super::types::{AgentTranscriptStore, SavedAgentSession, SavedSessionPage, SavedSessionQuery};

/// Current saved-conversation catalog schema version.
pub(super) const SCHEMA_VERSION: i64 = 1;
/// Private SQLite database stored beside saved-conversation directories.
const CATALOG_FILE_NAME: &str = "catalog.sqlite3";
/// Advisory lock serializing schema creation, migration, and rebuild.
const CATALOG_LOCK_FILE_NAME: &str = ".catalog-migration.lock";
/// Durable marker recording completion of the first catalog import.
const CATALOG_MIGRATION_MARKER_FILE_NAME: &str = ".catalog-migrated-v1";
/// Temporary database used for verified catalog rebuilds.
const CATALOG_REBUILD_FILE_NAME: &str = ".catalog.sqlite3.rebuild";
/// Retained previous database from the most recent explicit rebuild.
const CATALOG_BACKUP_FILE_NAME: &str = ".catalog.sqlite3.backup";

/// Filesystem payload layout associated with one catalog record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CatalogPayloadLayout {
    /// Current per-conversation directory layout.
    Directory,
    /// Legacy root-level `<conversation-id>.tsv` transcript layout.
    LegacyTsv,
}

impl CatalogPayloadLayout {
    /// Returns the stable SQLite representation for this layout.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::LegacyTsv => "legacy-tsv",
        }
    }
}

/// Canonical metadata reconstructed from retained session files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CatalogCandidate {
    /// Bounded conversation summary used by discovery queries.
    pub(super) summary: ConversationSummary,
    /// Optional user-visible durable name.
    pub(super) name: Option<String>,
    /// Timestamp of the latest naming operation, paired with `name`.
    pub(super) named_at_unix_seconds: Option<u64>,
    /// Durable root/subagent classification.
    pub(super) conversation_kind: AgentConversationKind,
    /// Whether a transcript payload exists.
    pub(super) has_transcript: bool,
    /// Whether a presentation payload exists.
    pub(super) has_presentation: bool,
    /// Layout used by the authoritative transcript payload.
    pub(super) payload_layout: CatalogPayloadLayout,
}

/// One catalog row with the payload flags needed for stale-row validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CatalogRecord {
    /// Saved-session metadata exposed to runtime discovery.
    pub(super) session: SavedAgentSession,
    /// Whether a transcript payload should exist for this row.
    pub(super) has_transcript: bool,
    /// Whether a presentation payload should exist for this row.
    pub(super) has_presentation: bool,
    /// Filesystem layout used by the authoritative transcript payload.
    pub(super) payload_layout: CatalogPayloadLayout,
}

/// Initializes the schema and performs the one-time metadata import.
pub(super) fn initialize(store: &AgentTranscriptStore, now_unix_seconds: u64) -> Result<()> {
    ensure_root(store)?;
    let _lock = acquire_lock(store)?;
    let database_existed = catalog_path(store).exists();
    let mut connection = match schema::open_for_initialization(&catalog_path(store))? {
        schema::InitializationOpen::Ready(connection) => connection,
        schema::InitializationOpen::RebuildRequired => {
            return rebuild_locked(store, now_unix_seconds);
        }
    };
    set_catalog_permissions(store)?;

    if !migration_marker_path(store).exists() || !database_existed {
        migration::import(store, &mut connection, now_unix_seconds)?;
        write_migration_marker(store)?;
        set_catalog_permissions(store)?;
    }
    Ok(())
}

/// Rebuilds a verified catalog from retained payload metadata and sidecars.
///
/// The replacement is built separately and moved into place only after the
/// import transaction and integrity check succeed. One previous database is
/// retained for diagnostics and rollback.
#[cfg(test)]
pub(super) fn rebuild(store: &AgentTranscriptStore, now_unix_seconds: u64) -> Result<()> {
    ensure_root(store)?;
    let _lock = acquire_lock(store)?;
    rebuild_locked(store, now_unix_seconds)
}

/// Rebuilds the catalog while the caller holds the migration lock.
fn rebuild_locked(store: &AgentTranscriptStore, now_unix_seconds: u64) -> Result<()> {
    let rebuild_path = store.root.join(CATALOG_REBUILD_FILE_NAME);
    remove_sqlite_family(&rebuild_path)?;

    let mut connection = schema::open(&rebuild_path)?;
    migration::import(store, &mut connection, now_unix_seconds)?;
    schema::prepare_for_replacement(&connection)?;
    drop(connection);
    set_private_file_permissions(&rebuild_path)?;

    let database_path = catalog_path(store);
    let backup_path = store.root.join(CATALOG_BACKUP_FILE_NAME);
    remove_sqlite_family(&backup_path)?;
    if database_path.exists() {
        fs::rename(&database_path, &backup_path)?;
        remove_sqlite_sidecars(&database_path)?;
    }
    fs::rename(&rebuild_path, &database_path)?;
    set_catalog_permissions(store)?;
    write_migration_marker(store)?;
    Ok(())
}

/// Upserts one payload-derived record while preserving an existing name.
pub(super) fn upsert(
    store: &AgentTranscriptStore,
    candidate: &CatalogCandidate,
    now_unix_seconds: u64,
) -> Result<()> {
    let connection = schema::open(&catalog_path(store))?;
    mutation::upsert(&connection, candidate, now_unix_seconds)?;
    set_catalog_permissions(store)
}

/// Applies one explicit durable name after its filesystem sidecar is synced.
pub(super) fn set_name(
    store: &AgentTranscriptStore,
    conversation_id: &str,
    name: &str,
    named_at_unix_seconds: u64,
) -> Result<()> {
    let connection = schema::open(&catalog_path(store))?;
    mutation::set_name(&connection, conversation_id, name, named_at_unix_seconds)?;
    set_catalog_permissions(store)
}

/// Clears one catalog name after the compatibility sidecar is updated.
pub(super) fn clear_name(store: &AgentTranscriptStore, conversation_id: &str) -> Result<()> {
    let connection = schema::open(&catalog_path(store))?;
    mutation::clear_name(&connection, conversation_id)?;
    set_catalog_permissions(store)
}

/// Deletes one discovery row after its filesystem payload is removed.
pub(super) fn delete(store: &AgentTranscriptStore, conversation_id: &str) -> Result<()> {
    let connection = schema::open(&catalog_path(store))?;
    mutation::delete(&connection, conversation_id)?;
    set_catalog_permissions(store)
}

/// Loads one catalog record by exact conversation id.
pub(super) fn record(
    store: &AgentTranscriptStore,
    conversation_id: &str,
) -> Result<Option<CatalogRecord>> {
    let connection = schema::open(&catalog_path(store))?;
    query::record(&connection, conversation_id)
}

/// Loads the most recently active root-session row.
pub(super) fn latest_root_record(store: &AgentTranscriptStore) -> Result<Option<CatalogRecord>> {
    let connection = schema::open(&catalog_path(store))?;
    query::latest_root_record(&connection)
}

/// Lists all catalog sessions for temporary compatibility callers.
#[cfg(test)]
pub(super) fn saved_sessions(store: &AgentTranscriptStore) -> Result<Vec<SavedAgentSession>> {
    let connection = schema::open(&catalog_path(store))?;
    query::saved_sessions(&connection)
}

/// Lists transcript-backed summaries for the temporary compatibility API.
pub(super) fn transcript_summaries(
    store: &AgentTranscriptStore,
) -> Result<Vec<ConversationSummary>> {
    let connection = schema::open(&catalog_path(store))?;
    query::transcript_summaries(&connection)
}

/// Returns oldest unnamed payload-backed ids beyond the retention limit.
pub(super) fn unnamed_prune_candidates(
    store: &AgentTranscriptStore,
    limit: usize,
) -> Result<Vec<String>> {
    let connection = schema::open(&catalog_path(store))?;
    query::unnamed_prune_candidates(&connection, limit)
}

/// Returns whether one catalog record is currently named.
pub(super) fn is_named(store: &AgentTranscriptStore, conversation_id: &str) -> Result<bool> {
    let connection = schema::open(&catalog_path(store))?;
    query::is_named(&connection, conversation_id)
}

/// Returns bounded root-session completion rows for one UUID prefix.
pub(super) fn root_session_completions(
    store: &AgentTranscriptStore,
    prefix: &str,
    limit: usize,
) -> Result<Vec<SavedAgentSession>> {
    let connection = schema::open(&catalog_path(store))?;
    query::root_session_completions(&connection, prefix, limit)
}

/// Returns one bounded keyset page of saved sessions.
pub(super) fn query_saved_sessions(
    store: &AgentTranscriptStore,
    query: &SavedSessionQuery,
) -> Result<SavedSessionPage> {
    let connection = schema::open(&catalog_path(store))?;
    query::query_saved_sessions(&connection, query)
}

/// Returns the catalog path for tests and diagnostics.
pub(super) fn catalog_path(store: &AgentTranscriptStore) -> PathBuf {
    store.root.join(CATALOG_FILE_NAME)
}

/// Creates the private catalog root when needed.
fn ensure_root(store: &AgentTranscriptStore) -> Result<()> {
    fs::create_dir_all(&store.root)?;
    set_private_dir_permissions(&store.root)
}

/// Acquires the cross-process catalog migration lock.
fn acquire_lock(store: &AgentTranscriptStore) -> Result<fs::File> {
    let path = store.root.join(CATALOG_LOCK_FILE_NAME);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    set_private_file_permissions(&path)?;
    flock(&file, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
    Ok(file)
}

/// Atomically records completion of schema-v1 metadata migration.
fn write_migration_marker(store: &AgentTranscriptStore) -> Result<()> {
    let marker = migration_marker_path(store);
    let temporary = store.root.join(".catalog-migrated-v1.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&temporary)?;
    file.write_all(b"mez-saved-conversation-catalog/1\n")?;
    file.sync_all()?;
    set_private_file_permissions(&temporary)?;
    fs::rename(&temporary, &marker)?;
    set_private_file_permissions(&marker)
}

/// Returns the durable migration-marker path.
fn migration_marker_path(store: &AgentTranscriptStore) -> PathBuf {
    store.root.join(CATALOG_MIGRATION_MARKER_FILE_NAME)
}

/// Applies private permissions to the database and any active SQLite sidecars.
fn set_catalog_permissions(store: &AgentTranscriptStore) -> Result<()> {
    let path = catalog_path(store);
    for candidate in sqlite_family(&path) {
        if candidate.exists() {
            set_private_file_permissions(&candidate)?;
        }
    }
    Ok(())
}

/// Removes a SQLite database and its WAL/SHM sidecars when present.
fn remove_sqlite_family(path: &Path) -> Result<()> {
    for candidate in sqlite_family(path) {
        match fs::remove_file(candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Removes only WAL/SHM sidecars associated with a database path.
fn remove_sqlite_sidecars(path: &Path) -> Result<()> {
    for candidate in sqlite_family(path).into_iter().skip(1) {
        match fs::remove_file(candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Returns the main database, WAL, and shared-memory paths.
fn sqlite_family(path: &Path) -> [PathBuf; 3] {
    [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ]
}
