//! SQLite-backed snapshot metadata and latest-snapshot indexes.
//!
//! Snapshot payloads and manifests stay private content-addressed files; only
//! the derived indexes live in `snapshots.sqlite`. The database holds the
//! latest-snapshot winners (`latest_snapshot`) and one metadata row per
//! published manifest (`snapshot_metadata`), so listing and latest selection
//! answer from a query instead of parsing every manifest. The metadata rows are
//! ordered per session and creation time, which is the scan a future retention
//! policy needs.
//!
//! Every row is derived from a manifest that stays on disk, so the database is
//! a cache: an empty table, a deleted database, or a database written at an
//! older schema version is rebuilt from the manifests on the next read. A
//! database written by a newer schema version is rejected by the shared storage
//! helper with its actionable downgrade error rather than discarded, so a
//! downgrade cannot mix writers.

use super::super::shared_sqlite::{
    SharedSchemaState, open_shared_database, open_shared_database_read_only, schema_version,
    set_schema_version,
};
use super::repository::LatestSnapshotIndex;
use super::types::{SnapshotKind, SnapshotState};
use crate::error::{MezError, Result};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use std::path::{Path, PathBuf};

/// Database file owned by the snapshot repository.
pub(super) const SNAPSHOT_DATABASE_FILE_NAME: &str = "snapshots.sqlite";

/// Schema version owned by the snapshot metadata store.
///
/// Version 1 held only the latest-snapshot winners. Version 2 adds the metadata
/// table that serves listing queries.
const SNAPSHOT_SCHEMA_VERSION: i64 = 2;

/// Scope key holding the global winner; every other key is a session id.
const GLOBAL_SCOPE: &str = "";

/// Returns the snapshot database path for one repository directory.
pub(super) fn database_path(directory: &Path) -> PathBuf {
    directory.join(SNAPSHOT_DATABASE_FILE_NAME)
}

/// Maps one rusqlite failure to an actionable snapshot error.
fn database_error(error: rusqlite::Error) -> MezError {
    MezError::invalid_state(format!("snapshot database error: {error}"))
}

/// Converts one in-memory count into the stored integer representation.
fn stored_count(value: usize) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        MezError::invalid_state("snapshot metadata count exceeds the supported integer range")
    })
}

/// Converts one stored integer back into an in-memory count.
fn indexed_count(value: i64) -> Result<usize> {
    usize::try_from(value)
        .map_err(|_| MezError::invalid_state("snapshot metadata row carries an invalid count"))
}

/// Encodes one snapshot kind for the metadata table.
fn stored_kind(kind: &SnapshotKind) -> &'static str {
    match kind {
        SnapshotKind::Live => "live",
        SnapshotKind::Manual => "manual",
        SnapshotKind::Automatic => "automatic",
        SnapshotKind::CrashRecovery => "crash_recovery",
    }
}

/// Decodes one stored snapshot kind.
fn indexed_kind(value: &str) -> Result<SnapshotKind> {
    match value {
        "live" => Ok(SnapshotKind::Live),
        "manual" => Ok(SnapshotKind::Manual),
        "automatic" => Ok(SnapshotKind::Automatic),
        "crash_recovery" => Ok(SnapshotKind::CrashRecovery),
        other => Err(MezError::invalid_state(format!(
            "snapshot metadata row carries unknown kind `{other}`"
        ))),
    }
}

/// Encodes one limitations list for the metadata table.
fn stored_limitations(limitations: &[String]) -> Result<String> {
    serde_json::to_string(limitations)
        .map_err(|error| MezError::invalid_state(format!("encode snapshot limitations: {error}")))
}

/// Decodes one stored limitations list.
fn indexed_limitations(value: &str) -> Result<Vec<String>> {
    serde_json::from_str(value).map_err(|_| {
        MezError::invalid_state("snapshot metadata row carries an invalid limitations array")
    })
}

/// Rebuilds a database that carries an older schema version.
///
/// The manifests remain the source of truth, so dropping an older database
/// cannot lose a snapshot: the next read rebuilds every table from the
/// manifests. A newer version is left in place so the shared helper rejects it
/// with its actionable downgrade error instead of discarding another build's
/// data.
fn discard_rebuildable_schema(path: &Path) -> Result<()> {
    let Some(connection) = open_shared_database_read_only(path)? else {
        return Ok(());
    };
    let version = schema_version(&connection)?;
    drop(connection);
    if version == 0 || version >= SNAPSHOT_SCHEMA_VERSION {
        return Ok(());
    }
    for suffix in ["", "-wal", "-shm"] {
        let mut candidate = path.as_os_str().to_owned();
        candidate.push(suffix);
        let candidate = PathBuf::from(candidate);
        match std::fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(MezError::invalid_state(format!(
                    "rebuild snapshot database {} failed: {error}",
                    candidate.display()
                )));
            }
        }
    }
    Ok(())
}

/// Opens the snapshot database and creates the schema when it is fresh.
fn open(directory: &Path) -> Result<Connection> {
    let path = database_path(directory);
    discard_rebuildable_schema(&path)?;
    let (mut connection, state) = open_shared_database(&path, SNAPSHOT_SCHEMA_VERSION)?;
    if state == SharedSchemaState::Fresh {
        // The composite index orders one session's snapshots by creation time
        // and id, which is the ordering latest selection and a retention scan
        // read; listing itself uses the primary key order.
        let transaction = connection.transaction().map_err(database_error)?;
        transaction
            .execute_batch(
                "CREATE TABLE latest_snapshot (\n                    scope TEXT PRIMARY KEY,\n                    snapshot_id TEXT NOT NULL\n                );\n                CREATE TABLE snapshot_metadata (\n                    snapshot_id TEXT PRIMARY KEY,\n                    version INTEGER NOT NULL,\n                    session_id TEXT NOT NULL,\n                    name TEXT,\n                    created_at TEXT NOT NULL,\n                    kind TEXT NOT NULL,\n                    restorable INTEGER NOT NULL,\n                    window_count INTEGER NOT NULL,\n                    pane_count INTEGER NOT NULL,\n                    limitations TEXT NOT NULL,\n                    storage_ref TEXT NOT NULL\n                );\n                CREATE INDEX snapshot_metadata_session_created ON snapshot_metadata (session_id, created_at DESC, snapshot_id DESC);",
            )
            .map_err(database_error)?;
        set_schema_version(&transaction, SNAPSHOT_SCHEMA_VERSION)?;
        transaction.commit().map_err(database_error)?;
    }
    Ok(connection)
}

/// Reads the stored winners, or `None` while the table has no rows.
pub(super) fn read(directory: &Path) -> Result<Option<LatestSnapshotIndex>> {
    let connection = open(directory)?;
    let mut statement = connection
        .prepare("SELECT scope, snapshot_id FROM latest_snapshot")
        .map_err(database_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(database_error)?;
    let mut index = LatestSnapshotIndex::default();
    let mut any = false;
    for row in rows {
        let (scope, snapshot_id) = row.map_err(database_error)?;
        any = true;
        if scope == GLOBAL_SCOPE {
            index.latest_all = Some(snapshot_id);
        } else {
            index.latest_by_session.insert(scope, snapshot_id);
        }
    }
    Ok(any.then_some(index))
}

/// Replaces the stored winners with one complete index.
pub(super) fn write(directory: &Path, index: &LatestSnapshotIndex) -> Result<()> {
    let mut connection = open(directory)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    transaction
        .execute("DELETE FROM latest_snapshot", [])
        .map_err(database_error)?;
    if let Some(latest_all) = index.latest_all.as_deref() {
        transaction
            .execute(
                "INSERT INTO latest_snapshot (scope, snapshot_id) VALUES (?1, ?2)",
                params![GLOBAL_SCOPE, latest_all],
            )
            .map_err(database_error)?;
    }
    for (session_id, snapshot_id) in &index.latest_by_session {
        transaction
            .execute(
                "INSERT INTO latest_snapshot (scope, snapshot_id) VALUES (?1, ?2)",
                params![session_id, snapshot_id],
            )
            .map_err(database_error)?;
    }
    transaction.commit().map_err(database_error)
}

/// One raw metadata row before it is converted back into snapshot state.
struct StoredMetadataRow {
    snapshot_id: String,
    version: i64,
    session_id: String,
    name: Option<String>,
    created_at: String,
    kind: String,
    restorable: i64,
    window_count: i64,
    pane_count: i64,
    limitations: String,
    storage_ref: String,
}

impl StoredMetadataRow {
    /// Converts one stored row into the snapshot state the repository serves.
    fn into_state(self) -> Result<SnapshotState> {
        Ok(SnapshotState {
            id: self.snapshot_id,
            version: u32::try_from(self.version).map_err(|_| {
                MezError::invalid_state("snapshot metadata row carries an invalid version")
            })?,
            session_id: self.session_id,
            name: self.name,
            created_at: self.created_at,
            kind: indexed_kind(&self.kind)?,
            restorable: self.restorable != 0,
            window_count: indexed_count(self.window_count)?,
            pane_count: indexed_count(self.pane_count)?,
            limitations: indexed_limitations(&self.limitations)?,
            storage_ref: self.storage_ref,
        })
    }
}

/// Inserts one metadata row inside an open transaction.
fn insert_row(transaction: &Transaction<'_>, state: &SnapshotState) -> Result<()> {
    transaction
        .execute(
            "INSERT INTO snapshot_metadata (snapshot_id, version, session_id, name, created_at, kind, restorable, window_count, pane_count, limitations, storage_ref)\n             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                state.id,
                i64::from(state.version),
                state.session_id,
                state.name,
                state.created_at,
                stored_kind(&state.kind),
                i64::from(state.restorable),
                stored_count(state.window_count)?,
                stored_count(state.pane_count)?,
                stored_limitations(&state.limitations)?,
                state.storage_ref,
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

/// Reads the metadata rows ordered like a manifest listing, or `None` while the
/// table has no rows.
///
/// Rows are ordered by snapshot id so a listing served from the index matches
/// the manifest scan it replaces, including the deterministic ordering of
/// snapshots written inside the same second.
pub(super) fn read_metadata(directory: &Path) -> Result<Option<Vec<SnapshotState>>> {
    let connection = open(directory)?;
    let mut statement = connection
        .prepare(
            "SELECT snapshot_id, version, session_id, name, created_at, kind, restorable, window_count, pane_count, limitations, storage_ref\n             FROM snapshot_metadata ORDER BY snapshot_id",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok(StoredMetadataRow {
                snapshot_id: row.get(0)?,
                version: row.get(1)?,
                session_id: row.get(2)?,
                name: row.get(3)?,
                created_at: row.get(4)?,
                kind: row.get(5)?,
                restorable: row.get(6)?,
                window_count: row.get(7)?,
                pane_count: row.get(8)?,
                limitations: row.get(9)?,
                storage_ref: row.get(10)?,
            })
        })
        .map_err(database_error)?;
    let mut states = Vec::new();
    for row in rows {
        states.push(row.map_err(database_error)?.into_state()?);
    }
    Ok((!states.is_empty()).then_some(states))
}

/// Replaces every metadata row with one complete set.
///
/// The rebuild path calls this after scanning the manifests, so the replacement
/// is one transaction: a reader either sees the previous index or the complete
/// new one, never a half-rebuilt listing.
pub(super) fn write_metadata(directory: &Path, states: &[SnapshotState]) -> Result<()> {
    let mut connection = open(directory)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    transaction
        .execute("DELETE FROM snapshot_metadata", [])
        .map_err(database_error)?;
    for state in states {
        insert_row(&transaction, state)?;
    }
    transaction.commit().map_err(database_error)
}

/// Inserts or replaces one metadata row after a successful manifest write.
pub(super) fn insert_metadata_row(directory: &Path, state: &SnapshotState) -> Result<()> {
    let mut connection = open(directory)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    transaction
        .execute(
            "DELETE FROM snapshot_metadata WHERE snapshot_id = ?1",
            params![state.id],
        )
        .map_err(database_error)?;
    insert_row(&transaction, state)?;
    transaction.commit().map_err(database_error)
}

/// Removes one metadata row after a successful manifest deletion.
pub(super) fn remove_metadata_row(directory: &Path, snapshot_id: &str) -> Result<()> {
    let connection = open(directory)?;
    connection
        .execute(
            "DELETE FROM snapshot_metadata WHERE snapshot_id = ?1",
            params![snapshot_id],
        )
        .map_err(database_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates one isolated repository directory for a store test.
    fn test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("mez-snapshot-store-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// Builds one snapshot state for a metadata store test.
    fn stored_state(id: &str, session_id: &str, created_at: &str) -> SnapshotState {
        SnapshotState {
            id: id.to_string(),
            version: 3,
            session_id: session_id.to_string(),
            name: Some(format!("{id}-name")),
            created_at: created_at.to_string(),
            kind: SnapshotKind::Automatic,
            restorable: false,
            window_count: 2,
            pane_count: 3,
            limitations: vec!["pane processes must be restarted".to_string()],
            storage_ref: format!("{id}.payload"),
        }
    }

    /// Verifies the latest index round-trips and an empty table reads as absent.
    ///
    /// The global winner and every session winner must survive a write and read, a
    /// replacement must remove stale rows instead of appending, and a database whose
    /// table is empty - a fresh install, a deleted database, or a store that
    /// predates this build - must report no index so the repository rebuilds it from
    /// the manifests instead of trusting a partial table.
    #[test]
    fn latest_index_round_trips_and_reports_an_empty_table_as_absent() {
        let root = test_root("round-trip");
        assert!(
            read(&root).unwrap().is_none(),
            "a fresh database holds no winners"
        );
        let index = LatestSnapshotIndex {
            latest_all: Some("snap-0001".to_string()),
            latest_by_session: std::collections::BTreeMap::from([
                ("session-a".to_string(), "snap-0001".to_string()),
                ("session-b".to_string(), "snap-0002".to_string()),
            ]),
        };
        write(&root, &index).unwrap();
        assert_eq!(read(&root).unwrap(), Some(index));

        let replacement = LatestSnapshotIndex {
            latest_all: Some("snap-0002".to_string()),
            latest_by_session: std::collections::BTreeMap::new(),
        };
        write(&root, &replacement).unwrap();
        assert_eq!(read(&root).unwrap(), Some(replacement));

        std::fs::remove_file(database_path(&root)).unwrap();
        assert!(
            read(&root).unwrap().is_none(),
            "a deleted database reads as an absent index"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Verifies the metadata index round-trips every snapshot state field and
    /// replaces, upserts, and removes rows without leaving stale entries.
    ///
    /// The listing served from this table has to be indistinguishable from the
    /// manifest scan it replaces, so the test covers the fields that ordering,
    /// resume plans, and cleanup read back, plus the three maintenance shapes the
    /// repository uses: a complete rebuild, one row after a write, and one row
    /// after a delete.
    #[test]
    fn metadata_index_round_trips_upserts_and_removes_rows() {
        let root = test_root("metadata");
        assert!(
            read_metadata(&root).unwrap().is_none(),
            "a fresh database holds no metadata rows"
        );
        let first = stored_state("snap-0001", "$1", "2026-04-30T00:00:00Z");
        let second = stored_state("snap-0002", "$2", "2026-04-30T00:00:01Z");
        write_metadata(&root, &[first.clone(), second.clone()]).unwrap();
        assert_eq!(
            read_metadata(&root).unwrap(),
            Some(vec![first.clone(), second.clone()]),
            "a complete rebuild preserves every field and the id ordering"
        );

        let mut updated = first.clone();
        updated.name = Some("renamed".to_string());
        updated.kind = SnapshotKind::CrashRecovery;
        updated.restorable = true;
        updated.window_count = 4;
        updated.pane_count = 5;
        updated.limitations = vec!["restart pane primary processes".to_string()];
        updated.storage_ref = "elsewhere.payload".to_string();
        insert_metadata_row(&root, &updated).unwrap();
        assert_eq!(
            read_metadata(&root).unwrap(),
            Some(vec![updated.clone(), second.clone()]),
            "one write replaces the row for its snapshot id"
        );

        write_metadata(&root, std::slice::from_ref(&second)).unwrap();
        assert_eq!(
            read_metadata(&root).unwrap(),
            Some(vec![second.clone()]),
            "a rebuild replaces the previous row set"
        );

        remove_metadata_row(&root, "snap-0002").unwrap();
        assert!(
            read_metadata(&root).unwrap().is_none(),
            "a removed row leaves the table empty"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Verifies a database written at an older schema version is rebuilt instead
    /// of being opened with the missing tables.
    ///
    /// Version-1 databases only hold the latest-snapshot winners. They cannot
    /// answer a listing, so the store discards the file - every row is derived
    /// from a manifest that stays on disk - and creates the current schema. A
    /// database written by a newer build stays untouched so the shared helper
    /// can reject the downgrade.
    #[test]
    fn older_schema_version_is_rebuilt_before_the_database_is_used() {
        let root = test_root("schema-upgrade");
        let path = database_path(&root);
        let (mut connection, _) = open_shared_database(&path, 1).unwrap();
        let transaction = connection.transaction().unwrap();
        transaction
            .execute_batch(
                "CREATE TABLE latest_snapshot (scope TEXT PRIMARY KEY, snapshot_id TEXT NOT NULL);",
            )
            .unwrap();
        transaction
            .execute(
                "INSERT INTO latest_snapshot (scope, snapshot_id) VALUES ('', 'snap-stale')",
                [],
            )
            .unwrap();
        set_schema_version(&transaction, 1).unwrap();
        transaction.commit().unwrap();
        drop(connection);

        assert!(
            read(&root).unwrap().is_none(),
            "the stale version-1 winner is discarded"
        );
        assert!(read_metadata(&root).unwrap().is_none());

        let index = LatestSnapshotIndex {
            latest_all: Some("snap-0001".to_string()),
            latest_by_session: std::collections::BTreeMap::new(),
        };
        write(&root, &index).unwrap();
        assert_eq!(read(&root).unwrap(), Some(index));

        let recreated = open_shared_database_read_only(&path).unwrap().unwrap();
        assert_eq!(
            schema_version(&recreated).unwrap(),
            SNAPSHOT_SCHEMA_VERSION,
            "the rebuilt database carries the current schema version"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
