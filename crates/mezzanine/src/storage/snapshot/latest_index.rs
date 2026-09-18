//! SQLite-backed latest-snapshot index.
//!
//! The latest-snapshot winners used to live in one TSV file (`latest.index`) that
//! had to be parsed, validated, and rewritten on every mutation. They now live in
//! `snapshots.sqlite`, which the later phases of the same migration extend with the
//! full snapshot metadata table. The index is a cache: an empty table is rebuilt
//! from the manifests on the next read, so the database can be deleted at any time
//! and the legacy file index stays untouched for rollback.

use super::super::shared_sqlite::{SharedSchemaState, open_shared_database, set_schema_version};
use super::repository::LatestSnapshotIndex;
use crate::error::{MezError, Result};
use rusqlite::{Connection, TransactionBehavior, params};
use std::path::{Path, PathBuf};

/// Database file owned by the snapshot repository.
pub(super) const SNAPSHOT_DATABASE_FILE_NAME: &str = "snapshots.sqlite";

/// Schema version owned by the latest-index table.
const SNAPSHOT_SCHEMA_VERSION: i64 = 1;

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

/// Opens the snapshot database and creates the latest-index schema when fresh.
fn open(directory: &Path) -> Result<Connection> {
    let (mut connection, state) =
        open_shared_database(&database_path(directory), SNAPSHOT_SCHEMA_VERSION)?;
    if state == SharedSchemaState::Fresh {
        let transaction = connection.transaction().map_err(database_error)?;
        transaction
            .execute_batch(
                "CREATE TABLE latest_snapshot (\n                    scope TEXT PRIMARY KEY,\n                    snapshot_id TEXT NOT NULL\n                );",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates one isolated repository directory for a store test.
    fn test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("mez-snapshot-latest-index-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
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
}
