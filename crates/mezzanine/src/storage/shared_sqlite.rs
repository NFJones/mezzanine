//! Shared SQLite storage groundwork for session-state stores.
//!
//! Five SQLite stores already exist in this crate and each re-implements the
//! same setup: WAL journalling, a busy timeout that mirrors the historical
//! flock waits, private file and directory permissions, a `user_version`
//! schema check, an integrity check, and a one-shot import of a legacy flat
//! file. This module owns that contract so every store conversion inherits
//! identical behaviour instead of inventing its own.
//!
//! Concurrency convention: rusqlite is synchronous, so every call runs on the
//! blocking pool (`tokio::task::spawn_blocking`) and never on the async
//! reactor. WAL allows one writer plus concurrent readers, so short-lived CLI
//! readers never block the daemon writer and writer contention is bounded by
//! [`SHARED_BUSY_TIMEOUT_MS`] instead of an unbounded lock retry loop.
//!
//! Database boundary policy: one database per concern. This module never
//! creates, migrates, or writes any store other than the one it is handed: it
//! touches the database file, that file's `-wal`/`-shm` sidecars, and the
//! database's parent directory, which it creates and restricts to 0700. The
//! parent directory is therefore required to be dedicated to that one store.
//!
//! Read-only opens: the only open entry point here is read-write and may
//! create the file and apply the WAL pragma. A conversion that must let a
//! short-lived CLI process read without writing has to add a read-only open
//! path; do not reuse [`open_shared_database`] for that case.

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::error::{MezError, Result};

/// Busy timeout applied to every shared database connection.
///
/// Five seconds matches the longest historical flock wait, so a contended
/// writer fails with a retryable database error instead of parking forever.
pub(crate) const SHARED_BUSY_TIMEOUT_MS: u64 = 5_000;

/// Migration-marker table shared by every converted store.
const MIGRATION_TABLE: &str = "storage_migrations";

/// Private file mode applied to databases and their sidecar files.
#[cfg(unix)]
const PRIVATE_FILE_MODE: u32 = 0o600;

/// Private directory mode applied to database parent directories.
#[cfg(unix)]
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;

/// Schema state observed when a shared database is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SharedSchemaState {
    /// The database carried no schema version yet, so the caller owns schema
    /// creation and must write the version inside the same transaction.
    Fresh,
    /// The database already carries the expected schema version.
    Current,
}

/// Opens one private shared database with the shared connection setup.
///
/// The parent directory and the database file are created with private
/// permissions when absent, WAL journalling and `foreign_keys` are enabled, and
/// the busy timeout is [`SHARED_BUSY_TIMEOUT_MS`]. `expected_version` is the
/// schema version the calling store owns; a version that is newer or lower is
/// rejected with an actionable error rather than silently accepted.
pub(crate) fn open_shared_database(
    path: &Path,
    expected_version: i64,
) -> Result<(Connection, SharedSchemaState)> {
    ensure_private_parent(path)?;
    let connection = Connection::open(path)
        .map_err(|error| MezError::invalid_state(format!("open database failed: {error}")))?;
    configure_connection(&connection)?;
    let state = prepare_schema(&connection, path, expected_version)?;
    enforce_private_permissions(path)?;
    Ok((connection, state))
}

/// Applies the shared pragmas to one connection.
pub(crate) fn configure_connection(connection: &Connection) -> Result<()> {
    configure_connection_read_write(connection)
}

/// Opens an existing shared database read-only for inspection commands.
///
/// Returns `Ok(None)` when the database file does not exist, so an inspection
/// command never creates a store. Nothing is created, migrated, or imported,
/// and the journal-mode pragma is deliberately skipped because it needs write
/// access; only the bounded busy timeout is applied so a concurrent daemon
/// writer cannot make an export fail instantly.
pub(crate) fn open_shared_database_read_only(path: &Path) -> Result<Option<Connection>> {
    if !path.exists() {
        return Ok(None);
    }
    let connection = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| {
            MezError::invalid_state(format!("open database read-only failed: {error}"))
        })?;
    connection
        .busy_timeout(Duration::from_millis(SHARED_BUSY_TIMEOUT_MS))
        .map_err(database_error)?;
    Ok(Some(connection))
}

/// Applies the read-write pragmas to one connection.
fn configure_connection_read_write(connection: &Connection) -> Result<()> {
    connection
        .busy_timeout(Duration::from_millis(SHARED_BUSY_TIMEOUT_MS))
        .map_err(database_error)?;
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;\n PRAGMA foreign_keys = ON;\n PRAGMA synchronous = NORMAL;",
        )
        .map_err(database_error)
}

/// Reads the SQLite `user_version` pragma.
pub(crate) fn schema_version(connection: &Connection) -> Result<i64> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(database_error)
}

/// Writes the schema version inside an open transaction.
///
/// Callers set the version in the same transaction that creates their schema,
/// so a failed schema creation can never publish a version for tables that do
/// not exist.
pub(crate) fn set_schema_version(transaction: &Transaction<'_>, version: i64) -> Result<()> {
    transaction
        .execute_batch(&format!("PRAGMA user_version = {version}"))
        .map_err(database_error)
}

/// Creates the shared migration-marker table when it is absent.
pub(crate) fn ensure_migration_table(connection: &Connection) -> Result<()> {
    connection
        .execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS {MIGRATION_TABLE} (\n                marker TEXT PRIMARY KEY,\n                imported_rows INTEGER NOT NULL,\n                completed_at_unix_seconds INTEGER NOT NULL\n            );"
        ))
        .map_err(database_error)
}

/// Returns whether one legacy import already completed.
///
/// A database that has never imported anything has no marker table, which is
/// reported as "not imported" instead of an error so read-only opens never
/// create schema.
pub(crate) fn migration_completed(connection: &Connection, marker: &str) -> Result<bool> {
    let marker_table_exists: Option<String> = connection
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![MIGRATION_TABLE],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?;
    if marker_table_exists.is_none() {
        return Ok(false);
    }
    let completed: Option<i64> = connection
        .query_row(
            &format!("SELECT 1 FROM {MIGRATION_TABLE} WHERE marker = ?1"),
            params![marker],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?;
    Ok(completed.is_some())
}

/// Runs one legacy flat-file import exactly once, inside one transaction.
///
/// Returns the imported row count, or `None` when the marker shows the import
/// already ran. The legacy file itself is never modified: retaining it until
/// the first successful post-migration write is the calling store's policy, so
/// a rollback to a previous build still finds its data. A failed import rolls
/// back, records no marker, and reports the original error, leaving the legacy
/// file untouched.
pub(crate) fn import_legacy_file_once(
    connection: &mut Connection,
    marker: &str,
    import: impl FnOnce(&Transaction<'_>) -> Result<i64>,
) -> Result<Option<i64>> {
    if migration_completed(connection, marker)? {
        return Ok(None);
    }
    ensure_migration_table(connection)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    // Two writing openers can both pass the pre-check above. The immediate
    // transaction serializes them, so the loser must observe the winner's
    // marker and report "already imported" instead of failing on the marker
    // primary key or running the import twice.
    if migration_completed(&transaction, marker)? {
        transaction.commit().map_err(database_error)?;
        return Ok(None);
    }
    let imported_rows = import(&transaction)?;
    transaction
        .execute(
            &format!(
                "INSERT INTO {MIGRATION_TABLE} (marker, imported_rows, completed_at_unix_seconds)\n                 VALUES (?1, ?2, ?3)"
            ),
            params![marker, imported_rows, unix_now()],
        )
        .map_err(database_error)?;
    transaction.commit().map_err(database_error)?;
    Ok(Some(imported_rows))
}

/// Runs `PRAGMA integrity_check` and reports the first problem found.
pub(crate) fn integrity_check(connection: &Connection) -> Result<()> {
    let report: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(database_error)?;
    if report == "ok" {
        return Ok(());
    }
    Err(MezError::invalid_state(format!(
        "database integrity check failed: {report}"
    )))
}

/// Creates the database parent directory with private permissions when absent.
pub(crate) fn ensure_private_parent(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    fs::create_dir_all(parent)
        .map_err(|error| MezError::invalid_state(format!("create database directory: {error}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).map_err(
            |error| MezError::invalid_state(format!("set database directory permissions: {error}")),
        )?;
    }
    Ok(())
}

/// Enforces private permissions on one database and its sidecar files.
pub(crate) fn enforce_private_permissions(path: &Path) -> Result<()> {
    set_private_file_permissions(path)?;
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        let sidecar = PathBuf::from(sidecar);
        if sidecar.exists() {
            set_private_file_permissions(&sidecar)?;
        }
    }
    Ok(())
}

/// Renders rows in the historical tab-separated shape used by export commands.
///
/// Tabs, carriage returns, and newlines inside fields are escaped so one row
/// stays one line and an operator can diff the output against the legacy file.
pub(crate) fn export_tsv(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut output = String::new();
    output.push_str(&headers.join("\t"));
    output.push('\n');
    for row in rows {
        let encoded = row
            .iter()
            .map(|field| {
                field
                    .replace('\\', "\\\\")
                    .replace('\t', "\\t")
                    .replace('\r', "\\r")
                    .replace('\n', "\\n")
            })
            .collect::<Vec<_>>();
        output.push_str(&encoded.join("\t"));
        output.push('\n');
    }
    output
}

/// Verifies the observed schema version against the version the store owns.
fn prepare_schema(
    connection: &Connection,
    path: &Path,
    expected_version: i64,
) -> Result<SharedSchemaState> {
    let found = schema_version(connection)?;
    match found.cmp(&expected_version) {
        Ordering::Equal if found == 0 => Ok(SharedSchemaState::Fresh),
        Ordering::Equal => Ok(SharedSchemaState::Current),
        Ordering::Greater => Err(MezError::invalid_state(format!(
            "database {} has schema version {found}, which is newer than the supported version {expected_version}; upgrade the binary or restore a backup",
            path.display()
        ))),
        Ordering::Less if found == 0 => Ok(SharedSchemaState::Fresh),
        Ordering::Less => Err(MezError::invalid_state(format!(
            "database {} has schema version {found} but version {expected_version} is required; run the store migration before opening it",
            path.display()
        ))),
    }
}

/// Sets private permissions on one existing file.
fn set_private_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_FILE_MODE)).map_err(
            |error| MezError::invalid_state(format!("set database permissions: {error}")),
        )?;
    }
    Ok(())
}

/// Reports the current wall-clock time in whole unix seconds.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

/// Maps a rusqlite failure onto the crate error type.
fn database_error(error: rusqlite::Error) -> MezError {
    if let rusqlite::Error::SqliteFailure(code, _) = &error
        && matches!(
            code.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        )
    {
        return MezError::conflict(format!("database is busy; retry the request ({error})"));
    }
    MezError::invalid_state(format!("database error: {error}"))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    use super::*;

    /// Allocates one unique temporary directory for a focused database test.
    fn unique_temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let sequence = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "mez-shared-sqlite-{label}-{}-{nanos}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Reads one integer-valued pragma from an open connection.
    fn pragma_i64(connection: &Connection, pragma: &str) -> i64 {
        connection.query_row(pragma, [], |row| row.get(0)).unwrap()
    }

    /// Reads one text-valued pragma from an open connection.
    fn pragma_text(connection: &Connection, pragma: &str) -> String {
        connection.query_row(pragma, [], |row| row.get(0)).unwrap()
    }

    /// Reads the unix permission mode of one path.
    #[cfg(unix)]
    fn file_mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// Verifies the shared setup enables WAL, foreign keys, the bounded busy
    /// timeout, and private file and directory permissions on a fresh database.
    #[test]
    fn shared_database_applies_pragmas_and_private_permissions() {
        let dir = unique_temp_dir("pragmas");
        let path = dir.join("store.sqlite");
        let (connection, state) = open_shared_database(&path, 1).unwrap();
        assert_eq!(state, SharedSchemaState::Fresh);
        assert_eq!(pragma_text(&connection, "PRAGMA journal_mode"), "wal");
        assert_eq!(pragma_i64(&connection, "PRAGMA foreign_keys"), 1);
        assert_eq!(
            pragma_i64(&connection, "PRAGMA busy_timeout"),
            SHARED_BUSY_TIMEOUT_MS as i64
        );
        drop(connection);
        #[cfg(unix)]
        {
            assert_eq!(file_mode(&path), PRIVATE_FILE_MODE);
            assert_eq!(file_mode(&dir), PRIVATE_DIRECTORY_MODE);
        }
    }

    /// Verifies schema versions are published atomically with schema creation
    /// and that mismatched versions are rejected with actionable errors.
    #[test]
    fn shared_database_rejects_mismatched_schema_versions() {
        let dir = unique_temp_dir("schema");
        let path = dir.join("store.sqlite");
        let (mut connection, state) = open_shared_database(&path, 3).unwrap();
        assert_eq!(state, SharedSchemaState::Fresh);
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        transaction
            .execute_batch("CREATE TABLE records (id INTEGER PRIMARY KEY);")
            .unwrap();
        set_schema_version(&transaction, 3).unwrap();
        transaction.commit().unwrap();
        drop(connection);

        let (connection, state) = open_shared_database(&path, 3).unwrap();
        assert_eq!(state, SharedSchemaState::Current);
        drop(connection);

        let (connection, _) = open_shared_database(&path, 3).unwrap();
        connection.execute_batch("PRAGMA user_version = 7").unwrap();
        drop(connection);
        let newer = open_shared_database(&path, 3).unwrap_err();
        assert!(newer.message().contains("newer than the supported version"));

        let (connection, _) = open_shared_database(&path, 7).unwrap();
        connection.execute_batch("PRAGMA user_version = 2").unwrap();
        drop(connection);
        let older = open_shared_database(&path, 7).unwrap_err();
        assert!(older.message().contains("run the store migration"));
    }

    /// Verifies a healthy database passes the integrity check and a corrupted
    /// page file is reported instead of being read as valid data.
    #[test]
    fn shared_database_integrity_check_detects_corruption() {
        let dir = unique_temp_dir("integrity");
        let path = dir.join("store.sqlite");
        let (connection, _) = open_shared_database(&path, 1).unwrap();
        connection
            .execute_batch("CREATE TABLE records (id INTEGER PRIMARY KEY);")
            .unwrap();
        integrity_check(&connection).unwrap();
        drop(connection);

        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push("-wal");
        let _ = fs::remove_file(PathBuf::from(sidecar));
        fs::write(&path, vec![0u8; 16_384]).unwrap();
        let corrupted =
            open_shared_database(&path, 1).and_then(|(connection, _)| integrity_check(&connection));
        assert!(
            corrupted.is_err(),
            "a corrupted database must not report a healthy integrity check"
        );
    }

    /// Verifies one legacy import runs exactly once, records its marker, and
    /// leaves both the database and the legacy file untouched when it fails.
    #[test]
    fn shared_database_imports_legacy_file_once() {
        let dir = unique_temp_dir("import");
        let path = dir.join("store.sqlite");
        let legacy = dir.join("store.tsv");
        fs::write(&legacy, "id\tname\n1\tfirst\n").unwrap();
        let (mut connection, _) = open_shared_database(&path, 1).unwrap();
        connection
            .execute_batch("CREATE TABLE records (id INTEGER PRIMARY KEY, name TEXT NOT NULL);")
            .unwrap();
        assert!(
            !migration_completed(&connection, "store-tsv").unwrap(),
            "a database without a marker table has not imported anything"
        );
        let imported = import_legacy_file_once(&mut connection, "store-tsv", |transaction| {
            transaction
                .execute("INSERT INTO records (id, name) VALUES (1, 'first')", [])
                .unwrap();
            Ok(1)
        })
        .unwrap();
        assert_eq!(imported, Some(1));
        assert!(migration_completed(&connection, "store-tsv").unwrap());
        assert!(
            import_legacy_file_once(&mut connection, "store-tsv", |_| Ok(0))
                .unwrap()
                .is_none(),
            "a completed import is never repeated"
        );

        let failure = import_legacy_file_once(&mut connection, "store-tsv-failed", |transaction| {
            transaction
                .execute("INSERT INTO records (id, name) VALUES (2, 'second')", [])
                .unwrap();
            Err(MezError::invalid_state("legacy file is truncated"))
        });
        assert!(failure.is_err());
        assert!(!migration_completed(&connection, "store-tsv-failed").unwrap());
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1, "a failed import must roll back its rows");
        assert!(
            legacy.exists(),
            "the legacy file is never modified or removed"
        );
    }

    /// Verifies WAL readers observe committed state without blocking on an
    /// uncommitted writer transaction.
    #[test]
    fn shared_database_wal_reader_does_not_block_writer() {
        let dir = unique_temp_dir("wal");
        let path = dir.join("store.sqlite");
        let (writer, _) = open_shared_database(&path, 1).unwrap();
        writer
            .execute_batch(
                "CREATE TABLE items (id INTEGER PRIMARY KEY);\n PRAGMA user_version = 1;",
            )
            .unwrap();
        let (reader, state) = open_shared_database(&path, 1).unwrap();
        assert_eq!(state, SharedSchemaState::Current);

        writer
            .execute_batch("BEGIN IMMEDIATE;\n INSERT INTO items (id) VALUES (1);")
            .unwrap();
        let visible: i64 = reader
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            visible, 0,
            "an uncommitted writer row is invisible and does not block the reader"
        );
        writer.execute_batch("COMMIT;").unwrap();
        let visible: i64 = reader
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            visible, 1,
            "committed writer rows become visible to readers"
        );
    }

    /// Verifies the export shape stays one line per row even when a field
    /// contains tabs or newlines.
    #[test]
    fn shared_database_exports_escaped_tsv_rows() {
        let rendered = export_tsv(
            &["id", "text"],
            &[vec!["1".to_string(), "tab\tand\nnewline".to_string()]],
        );
        assert_eq!(rendered, "id\ttext\n1\ttab\\tand\\nnewline\n");
        assert_eq!(rendered.lines().count(), 2);
    }

    /// Verifies a contended writer is reported as a retryable busy failure so
    /// callers can distinguish lock contention from corruption or misuse.
    #[test]
    fn shared_database_reports_busy_writes_as_retryable() {
        let dir = unique_temp_dir("busy");
        let path = dir.join("store.sqlite");
        let (holder, _) = open_shared_database(&path, 1).unwrap();
        holder
            .execute_batch(
                "CREATE TABLE items (id INTEGER PRIMARY KEY);\n PRAGMA user_version = 1;\n BEGIN IMMEDIATE;\n INSERT INTO items (id) VALUES (1);",
            )
            .unwrap();
        let (contender, _) = open_shared_database(&path, 1).unwrap();
        contender.busy_timeout(Duration::from_millis(1)).unwrap();
        let error = contender
            .execute("INSERT INTO items (id) VALUES (2)", [])
            .map_err(database_error)
            .unwrap_err();
        assert!(
            error.message().contains("retry the request"),
            "busy contention must stay retryable: {}",
            error.message()
        );
        holder.execute_batch("COMMIT;").unwrap();
    }

    /// Verifies the read-only open used by inspection commands never creates a
    /// database and rejects writes, so an export cannot create or migrate the
    /// store it is printing.
    #[test]
    fn shared_database_read_only_open_never_writes() {
        let dir = unique_temp_dir("read-only");
        let path = dir.join("store.sqlite");
        assert!(
            open_shared_database_read_only(&path).unwrap().is_none(),
            "a missing database reports nothing to inspect"
        );
        assert!(
            !path.exists(),
            "a read-only open must not create the database file"
        );

        let (connection, _) = open_shared_database(&path, 1).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE items (id INTEGER PRIMARY KEY);\n PRAGMA user_version = 1;",
            )
            .unwrap();
        drop(connection);

        let reader = open_shared_database_read_only(&path).unwrap().unwrap();
        let rows: i64 = reader
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0);
        assert!(
            reader
                .execute("INSERT INTO items (id) VALUES (1)", [])
                .is_err(),
            "a read-only connection rejects writes"
        );
    }
}
