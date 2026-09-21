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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use mez_agent::AgentConversationKind;
use mez_agent::transcript::ConversationSummary;
use rusqlite::Connection;
use rustix::fs::{FlockOperation, flock};

use crate::error::{MezError, Result};

use super::fs::{set_private_dir_permissions, set_private_file_permissions};
use super::types::{
    AgentTranscriptStore, SavedAgentSession, SavedSessionCatalogStatus, SavedSessionPage,
    SavedSessionQuery,
};

/// Current saved-conversation catalog schema version.
pub(super) const SCHEMA_VERSION: i64 = 3;
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
/// Maximum time an operator or startup waits for catalog migration ownership.
#[cfg(not(test))]
const CATALOG_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
/// Short bounded lock wait used by deterministic contention regressions.
#[cfg(test)]
const CATALOG_LOCK_TIMEOUT: Duration = Duration::from_millis(100);
/// Maximum wait for one interactive catalog read (picker pages, prefix
/// completion).
///
/// Interactive reads run inside the serialized runtime actor, so a contended
/// migration lock must fail fast with a retryable diagnostic instead of parking
/// every keystroke, pane write, and provider event behind the startup budget.
#[cfg(not(test))]
const CATALOG_INTERACTIVE_LOCK_TIMEOUT: Duration = Duration::from_millis(250);
/// Short bounded interactive wait used by deterministic contention regressions.
#[cfg(test)]
const CATALOG_INTERACTIVE_LOCK_TIMEOUT: Duration = Duration::from_millis(50);

/// Process-local count of indexed normal-operation queries.
static INDEXED_QUERY_COUNT: AtomicU64 = AtomicU64::new(0);
/// Process-local count of UUID-local repair attempts.
static EXACT_REPAIR_COUNT: AtomicU64 = AtomicU64::new(0);
/// Process-local count of full catalog rebuilds.
static REBUILD_COUNT: AtomicU64 = AtomicU64::new(0);
/// Process-local count of recovery-only session-root scans.
static FULL_SCAN_COUNT: AtomicU64 = AtomicU64::new(0);

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
    /// Whether `name` is the durable preferred name used by picker ranking.
    pub(super) name_preferred: bool,
    /// Durable root/subagent classification.
    pub(super) conversation_kind: AgentConversationKind,
    /// Whether a transcript payload exists.
    pub(super) has_transcript: bool,
    /// Whether a presentation payload exists.
    pub(super) has_presentation: bool,
    /// Layout used by the authoritative transcript payload.
    pub(super) payload_layout: CatalogPayloadLayout,
    /// Time at which this payload entered the archived lifecycle.
    pub(super) archived_at_unix_seconds: Option<u64>,
    /// Installed compressed archive size, when archived.
    pub(super) archive_compressed_bytes: Option<u64>,
    /// Lowercase SHA-256 digest of the installed archive, when archived.
    pub(super) archive_sha256: Option<String>,
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

    let migration_required = !migration_marker_path(store).exists() || !database_existed;
    if migration_required {
        migration::import(store, &mut connection, now_unix_seconds)?;
        write_migration_marker(store)?;
        set_catalog_permissions(store)?;
        let _ = quarantine_unrestorable_legacy_subagents(store, &connection)?;
    }
    Ok(())
}

/// Removes indexed rows whose current version-one child sidecar cannot prove a
/// durable delegation contract, without rebuilding the catalog or disturbing
/// healthy rows.
///
/// Every indexed row is inspected because the indexed conversation kind can be
/// stale: a row written while its sidecar was absent, or one whose sidecar was
/// later replaced by a version-one document, still has to be reclaimed. Healthy
/// startup never enumerates sidecars - this runs at import time and in the
/// retention pass - so ordinary reads pay nothing for it.
fn quarantine_unrestorable_legacy_subagents(
    store: &AgentTranscriptStore,
    connection: &Connection,
) -> Result<usize> {
    let conversation_ids = {
        let mut statement =
            connection.prepare("SELECT conversation_id FROM saved_conversations")?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut quarantined = 0usize;
    for conversation_id in conversation_ids {
        if store.is_unrestorable_legacy_subagent(&conversation_id)? {
            mutation::delete(connection, &conversation_id)?;
            quarantined += 1;
        }
    }
    Ok(quarantined)
}

/// Reclaims unresumable legacy child rows during the retention pass.
///
/// Exact-access reads report such rows as absent without deleting them, so the
/// retention pass owns the reclaim for rows that became unresumable after the
/// one-time import quarantine ran.
pub(super) fn quarantine_unrestorable_legacy_subagents_at_retention(
    store: &AgentTranscriptStore,
) -> Result<usize> {
    let _lock = acquire_shared_lock(store)?;
    let connection = schema::open(&catalog_path(store))?;
    quarantine_unrestorable_legacy_subagents(store, &connection)
}

/// Rebuilds a verified catalog from retained payload metadata and sidecars.
///
/// The replacement is built separately and moved into place only after the
/// import transaction and integrity check succeed. One previous database is
/// retained for diagnostics and rollback.
pub(super) fn rebuild(store: &AgentTranscriptStore, now_unix_seconds: u64) -> Result<()> {
    ensure_root(store)?;
    let _lock = acquire_lock(store)?;
    reject_future_schema_rebuild(store)?;
    rebuild_locked(store, now_unix_seconds)
}

/// Refuses to replace a readable catalog created by a newer release.
fn reject_future_schema_rebuild(store: &AgentTranscriptStore) -> Result<()> {
    let path = catalog_path(store);
    if !path.is_file() {
        return Ok(());
    }
    let Ok(connection) = schema::open_read_only(&path) else {
        return Ok(());
    };
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(MezError::invalid_state(format!(
            "saved-session catalog schema version {version} is newer than supported version {SCHEMA_VERSION}; refusing to rebuild or downgrade it"
        )));
    }
    Ok(())
}

/// Rebuilds the catalog while the caller holds the migration lock.
fn rebuild_locked(store: &AgentTranscriptStore, now_unix_seconds: u64) -> Result<()> {
    REBUILD_COUNT.fetch_add(1, Ordering::Relaxed);
    let rebuild_path = store.root.join(CATALOG_REBUILD_FILE_NAME);
    remove_sqlite_family(&rebuild_path)?;

    let build_result = (|| {
        let mut connection = schema::open(&rebuild_path)?;
        migration::import(store, &mut connection, now_unix_seconds)?;
        schema::prepare_for_replacement(&connection)?;
        drop(connection);
        Ok(())
    })();
    if let Err(error) = build_result {
        let _ = remove_sqlite_family(&rebuild_path);
        return Err(error);
    }
    set_private_file_permissions(&rebuild_path)?;
    remove_sqlite_sidecars(&rebuild_path)?;

    let database_path = catalog_path(store);
    let backup_path = store.root.join(CATALOG_BACKUP_FILE_NAME);
    remove_sqlite_family(&backup_path)?;
    if database_path.exists() {
        fs::rename(&database_path, &backup_path)?;
        remove_sqlite_sidecars(&database_path)?;
    }
    if let Err(error) = fs::rename(&rebuild_path, &database_path) {
        if backup_path.exists() && !database_path.exists() {
            let _ = fs::rename(&backup_path, &database_path);
        }
        let _ = remove_sqlite_family(&rebuild_path);
        return Err(error.into());
    }
    set_catalog_permissions(store)?;
    write_migration_marker(store)?;
    Ok(())
}

/// Returns bounded read-only catalog health without scanning session payloads.
pub(super) fn status(store: &AgentTranscriptStore) -> SavedSessionCatalogStatus {
    let database_path = catalog_path(store);
    let database_exists = database_path.is_file();
    let mut status = SavedSessionCatalogStatus {
        database_exists,
        migration_complete: migration_marker_path(store).is_file(),
        backup_exists: store.root.join(CATALOG_BACKUP_FILE_NAME).is_file(),
        rebuild_temporary_exists: store.root.join(CATALOG_REBUILD_FILE_NAME).is_file(),
        schema_version: None,
        indexed_conversations: None,
        integrity_ok: false,
        lock_available: catalog_lock_available(store),
        indexed_queries: INDEXED_QUERY_COUNT.load(Ordering::Relaxed),
        exact_repairs: EXACT_REPAIR_COUNT.load(Ordering::Relaxed),
        rebuilds: REBUILD_COUNT.load(Ordering::Relaxed),
        full_scans: FULL_SCAN_COUNT.load(Ordering::Relaxed),
        diagnostic: None,
    };
    if !database_exists {
        status.diagnostic =
            Some("saved-session catalog is missing; run `mez session-catalog rebuild`".to_string());
        return status;
    }
    let connection = match schema::open_read_only(&database_path) {
        Ok(connection) => connection,
        Err(_) => {
            status.diagnostic = Some(
                "saved-session catalog is unreadable; run `mez session-catalog rebuild`"
                    .to_string(),
            );
            return status;
        }
    };
    status.schema_version = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .ok();
    if status
        .schema_version
        .is_some_and(|version| version > SCHEMA_VERSION)
    {
        status.diagnostic = Some(
            "saved-session catalog was created by a newer Mezzanine version; do not rebuild or downgrade it"
                .to_string(),
        );
        return status;
    }
    status.integrity_ok = connection
        .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
        .is_ok_and(|result| result == "ok");
    if status.integrity_ok {
        status.indexed_conversations = connection
            .query_row("SELECT COUNT(*) FROM saved_conversations", [], |row| {
                row.get(0)
            })
            .ok()
            .and_then(|count: i64| u64::try_from(count).ok());
    } else {
        status.diagnostic = Some(
            "saved-session catalog integrity check failed; run `mez session-catalog rebuild`"
                .to_string(),
        );
    }
    if !status.migration_complete && status.diagnostic.is_none() {
        status.diagnostic = Some(
            "saved-session catalog migration marker is missing; run `mez session-catalog rebuild`"
                .to_string(),
        );
    }
    status
}

/// Records one indexed normal-operation catalog query.
pub(super) fn note_indexed_query() {
    INDEXED_QUERY_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Records one UUID-local repair attempt.
pub(super) fn note_exact_repair() {
    EXACT_REPAIR_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Records one recovery-only full session-root scan.
pub(super) fn note_full_scan() {
    FULL_SCAN_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Upserts one payload-derived record while preserving an existing name.
pub(super) fn upsert(
    store: &AgentTranscriptStore,
    candidate: &CatalogCandidate,
    now_unix_seconds: u64,
) -> Result<()> {
    let _lock = acquire_shared_lock(store)?;
    let connection = schema::open(&catalog_path(store))?;
    mutation::upsert(&connection, candidate, now_unix_seconds)?;
    set_catalog_permissions(store)
}

/// Marks one existing catalog row archived after the archive and sidecar are installed.
pub(super) fn mark_archived(
    store: &AgentTranscriptStore,
    conversation_id: &str,
    archived_at_unix_seconds: u64,
    archive_compressed_bytes: u64,
    archive_sha256: &str,
) -> Result<()> {
    let _lock = acquire_shared_lock(store)?;
    let connection = schema::open(&catalog_path(store))?;
    mutation::mark_archived(
        &connection,
        conversation_id,
        archived_at_unix_seconds,
        archive_compressed_bytes,
        archive_sha256,
    )?;
    set_catalog_permissions(store)
}

/// Applies one explicit durable name after its filesystem sidecar is synced.
pub(super) fn set_name(
    store: &AgentTranscriptStore,
    conversation_id: &str,
    name: &str,
    named_at_unix_seconds: u64,
    name_preferred: bool,
) -> Result<()> {
    let _lock = acquire_shared_lock(store)?;
    let connection = schema::open(&catalog_path(store))?;
    mutation::set_name(
        &connection,
        conversation_id,
        name,
        named_at_unix_seconds,
        name_preferred,
    )?;
    set_catalog_permissions(store)
}

/// Clears one catalog name after the compatibility sidecar is updated.
pub(super) fn clear_name(store: &AgentTranscriptStore, conversation_id: &str) -> Result<()> {
    let _lock = acquire_shared_lock(store)?;
    let connection = schema::open(&catalog_path(store))?;
    mutation::clear_name(&connection, conversation_id)?;
    set_catalog_permissions(store)
}

/// Deletes one discovery row after its filesystem payload is removed.
pub(super) fn delete(store: &AgentTranscriptStore, conversation_id: &str) -> Result<()> {
    let _lock = acquire_shared_lock(store)?;
    let connection = schema::open(&catalog_path(store))?;
    mutation::delete(&connection, conversation_id)?;
    set_catalog_permissions(store)
}

/// Loads one catalog record by exact conversation id.
///
/// The picker's cursor and detail views resolve rows through this read, so it
/// takes the interactive budget like the page rebuilds: a contended catalog
/// answers with the retryable busy diagnostic instead of parking the serialized
/// actor for the longer startup and writer wait.
pub(super) fn record(
    store: &AgentTranscriptStore,
    conversation_id: &str,
) -> Result<Option<CatalogRecord>> {
    note_indexed_query();
    let connection = open_catalog_for_interactive_read(store)?;
    query::record(&connection, conversation_id)
}

/// Loads one catalog record for a durable mutation path.
///
/// Unlike picker reads, callers already own a persistence operation whose
/// result must not be discarded merely because a concurrent catalog writer
/// briefly holds SQLite. Use the standard bounded catalog wait while retaining
/// the interactive helper above for actor-owned discovery requests.
pub(super) fn record_for_mutation(
    store: &AgentTranscriptStore,
    conversation_id: &str,
) -> Result<Option<CatalogRecord>> {
    note_indexed_query();
    let _lock = acquire_shared_lock(store)?;
    let connection = schema::open(&catalog_path(store))?;
    query::record(&connection, conversation_id)
}

/// Keyset position for one walk of the latest-root ordering.
///
/// The ordering is `(last_created_at DESC, first_created_at DESC,
/// conversation_id ASC)`. Carrying the last examined row's keys lets a caller
/// walk past a row it cannot use without deleting that row, so the walk stays
/// bounded even when the indexed conversation kind is stale.
#[derive(Debug, Clone)]
pub(super) struct RootRecordCursor {
    /// SQLite `last_created_at` of the last examined row.
    last_created_at: i64,
    /// SQLite `first_created_at` of the last examined row.
    first_created_at: i64,
    /// `conversation_id` of the last examined row.
    conversation_id: String,
}

impl RootRecordCursor {
    /// Builds the keyset position of one examined row.
    pub(super) fn after(record: &CatalogRecord) -> Result<Self> {
        Ok(Self {
            last_created_at: schema::sqlite_i64(
                record.session.summary.last_created_at_unix_seconds,
                "last_created_at",
            )?,
            first_created_at: schema::sqlite_i64(
                record.session.summary.first_created_at_unix_seconds,
                "first_created_at",
            )?,
            conversation_id: record.session.summary.conversation_id.clone(),
        })
    }
}

/// Loads the most recently active root-session row after one keyset position.
pub(super) fn latest_root_record(
    store: &AgentTranscriptStore,
    after: Option<&RootRecordCursor>,
) -> Result<Option<CatalogRecord>> {
    note_indexed_query();
    let connection = schema::open(&catalog_path(store))?;
    query::latest_root_record(&connection, after)
}

/// Lists all catalog sessions for temporary compatibility callers.
#[cfg(test)]
pub(super) fn saved_sessions(store: &AgentTranscriptStore) -> Result<Vec<SavedAgentSession>> {
    let connection = schema::open(&catalog_path(store))?;
    query::saved_sessions(&connection)
}

/// Lists transcript-backed summaries for the temporary compatibility API.
#[cfg(test)]
pub(super) fn transcript_summaries(
    store: &AgentTranscriptStore,
) -> Result<Vec<ConversationSummary>> {
    let connection = schema::open(&catalog_path(store))?;
    query::transcript_summaries(&connection)
}

/// Returns one bounded oldest-first age-expiry batch.
pub(super) fn age_retention_candidates(
    store: &AgentTranscriptStore,
    cutoff_unix_seconds: u64,
    excluded_conversation_ids: &std::collections::BTreeSet<String>,
    limit: usize,
) -> Result<Vec<String>> {
    note_indexed_query();
    let connection = schema::open(&catalog_path(store))?;
    query::age_retention_candidates(
        &connection,
        cutoff_unix_seconds,
        excluded_conversation_ids,
        limit,
    )
}

/// Retryable diagnostic for a contended startup or migration lock.
const CATALOG_MIGRATION_BUSY_MESSAGE: &str = "saved-session catalog migration lock is busy; retry after the active startup or rebuild completes";
/// Retryable diagnostic for a contended interactive read.
const CATALOG_INTERACTIVE_BUSY_MESSAGE: &str = "saved-session catalog is busy; retry the request";

/// Opens the catalog for one interactive read.
///
/// Interactive reads run inside the serialized runtime actor, so they take the
/// short shared-lock budget and the short database busy wait, and a contended
/// writer surfaces the retryable interactive diagnostic instead of parking key
/// handling behind the startup or standard wait.
fn open_catalog_for_interactive_read(store: &AgentTranscriptStore) -> Result<Connection> {
    let _lock = acquire_interactive_shared_lock(store)?;
    schema::open_interactive(&catalog_path(store), CATALOG_INTERACTIVE_BUSY_MESSAGE)
}

/// Returns the active payload-backed session count.
pub(super) fn active_payload_session_count(store: &AgentTranscriptStore) -> Result<usize> {
    note_indexed_query();
    let connection = schema::open(&catalog_path(store))?;
    query::active_payload_session_count(&connection)
}

/// Returns one bounded oldest-first count-enforcement batch.
pub(super) fn count_retention_candidates(
    store: &AgentTranscriptStore,
    excluded_conversation_ids: &std::collections::BTreeSet<String>,
    limit: usize,
) -> Result<Vec<String>> {
    note_indexed_query();
    let connection = schema::open(&catalog_path(store))?;
    query::count_retention_candidates(&connection, excluded_conversation_ids, limit)
}

/// Returns bounded root-session completion rows for one UUID prefix.
pub(super) fn root_session_completions(
    store: &AgentTranscriptStore,
    prefix: &str,
    limit: usize,
) -> Result<Vec<SavedAgentSession>> {
    note_indexed_query();
    let connection = open_catalog_for_interactive_read(store)?;
    query::root_session_completions(&connection, prefix, limit)
}

/// Returns one bounded keyset page of saved sessions.
pub(super) fn query_saved_sessions(
    store: &AgentTranscriptStore,
    query: &SavedSessionQuery,
) -> Result<SavedSessionPage> {
    note_indexed_query();
    let connection = open_catalog_for_interactive_read(store)?;
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
    acquire_lock_with_operation(
        store,
        FlockOperation::NonBlockingLockExclusive,
        CATALOG_LOCK_TIMEOUT,
        CATALOG_MIGRATION_BUSY_MESSAGE,
    )
}

/// Acquires shared catalog ownership for one ordinary metadata mutation.
fn acquire_shared_lock(store: &AgentTranscriptStore) -> Result<fs::File> {
    acquire_lock_with_operation(
        store,
        FlockOperation::NonBlockingLockShared,
        CATALOG_LOCK_TIMEOUT,
        CATALOG_MIGRATION_BUSY_MESSAGE,
    )
}

/// Acquires shared catalog ownership for one interactive read.
///
/// The shorter interactive budget keeps a contended migration lock from parking
/// the serialized actor for the full startup timeout; the returned error is
/// retryable and is surfaced by the picker/prefix completion path.
fn acquire_interactive_shared_lock(store: &AgentTranscriptStore) -> Result<fs::File> {
    acquire_lock_with_operation(
        store,
        FlockOperation::NonBlockingLockShared,
        CATALOG_INTERACTIVE_LOCK_TIMEOUT,
        CATALOG_INTERACTIVE_BUSY_MESSAGE,
    )
}

/// Acquires catalog ownership with a bounded wait and actionable diagnostic.
fn acquire_lock_with_operation(
    store: &AgentTranscriptStore,
    operation: FlockOperation,
    wait: Duration,
    busy_message: &str,
) -> Result<fs::File> {
    let path = store.root.join(CATALOG_LOCK_FILE_NAME);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    set_private_file_permissions(&path)?;
    let deadline = Instant::now() + wait;
    loop {
        match flock(&file, operation) {
            Ok(()) => return Ok(file),
            Err(error) => {
                let error = std::io::Error::from(error);
                if error.kind() != std::io::ErrorKind::WouldBlock {
                    return Err(error.into());
                }
                if Instant::now() >= deadline {
                    return Err(MezError::invalid_state(busy_message));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
}

fn catalog_lock_available(store: &AgentTranscriptStore) -> bool {
    let path = store.root.join(CATALOG_LOCK_FILE_NAME);
    let file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    flock(&file, FlockOperation::NonBlockingLockExclusive).is_ok()
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
