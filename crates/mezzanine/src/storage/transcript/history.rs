//! SQLite-backed prompt history shared by agent prompts and primary commands.
//!
//! Both histories used to live in append-only TSV files guarded by an advisory
//! lock, a separate migration marker, byte-threshold compaction, and tail-read
//! recovery. They now share one private `history.sqlite` database: one row per
//! accepted entry keyed by `(scope, position)`, where `position` preserves the
//! append order the readline history depends on and `scope` separates the agent
//! history from the command history. Reads are read-only and fall back to the
//! legacy TSV files until the one-time import marker exists, so an older build's
//! data is never rewritten or lost. The legacy `prompt-history.tsv`, the
//! per-conversation `prompt-history.tsv` files, and `command-prompt-history.tsv`
//! are imported exactly once and then never touched.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use super::super::shared_sqlite::{
    SharedSchemaState, import_legacy_file_once, migration_completed, open_shared_database,
    open_shared_database_read_only, schema_version, set_schema_version,
};
use super::encoding::decode_structured_prompt_history_entry;
use super::encoding::encode_structured_prompt_history_entry;
use super::store::{DEFAULT_AGENT_PROMPT_HISTORY_LIMIT, canonicalize_structured_history};
use mez_mux::readline::{ReadlineHistoryEntry, ReadlinePasteRange};

use crate::error::{MezError, Result};

/// Serializes history writers inside one process.
///
/// A burst of submissions would otherwise herd on the per-connection pragmas
/// and the immediate transaction, turning a fast append into a busy timeout.
/// Cross-process writers still coordinate through SQLite's immediate
/// transactions and the shared busy timeout; this is a process-local mutex, not
/// the removed file lock.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Bounded retries when another process holds the write lock.
const BUSY_RETRIES: usize = 3;

/// Delay between write retries.
const BUSY_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Database file owned by the transcript store for both prompt histories.
pub(super) const HISTORY_DATABASE_FILE_NAME: &str = "history.sqlite";

/// Schema version owned by the history tables.
const HISTORY_SCHEMA_VERSION: i64 = 1;

/// Legacy shared agent history imported exactly once.
pub(super) const LEGACY_AGENT_HISTORY_FILE_NAME: &str = "prompt-history.tsv";

/// Legacy command history imported exactly once.
pub(super) const LEGACY_COMMAND_HISTORY_FILE_NAME: &str = "command-prompt-history.tsv";

/// One history's stored rows inside the shared database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HistoryScope {
    /// Submitted agent prompts shared by every conversation.
    Agent,
    /// Submitted primary command prompts.
    Command,
}

impl HistoryScope {
    /// Returns the stored scope token for this history.
    fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Command => "command",
        }
    }

    /// Returns the one-time import marker for this history.
    fn import_marker(self) -> &'static str {
        match self {
            Self::Agent => LEGACY_AGENT_HISTORY_FILE_NAME,
            Self::Command => LEGACY_COMMAND_HISTORY_FILE_NAME,
        }
    }
}

/// Returns the history database path for one transcript store root.
pub(super) fn database_path(root: &Path) -> PathBuf {
    root.join(HISTORY_DATABASE_FILE_NAME)
}

/// Renders both prompt histories in the legacy TSV shape without creating or
/// migrating the store.
///
/// Returns `None` when neither the database nor any legacy file exists, so an
/// inspection command never creates the store. Each history renders as its own
/// `# <scope>` section followed by the encoded rows the legacy files held, and
/// the rows come from the same read path the runtime serves, so an export cannot
/// block a daemon writer or observe uncommitted rows.
pub(super) fn export_tsv_read_only(root: &Path) -> Result<Option<String>> {
    let path = database_path(root);
    reject_symlink(&path)?;
    if !path_exists(&path) && !legacy_history_exists(root) {
        return Ok(None);
    }
    let mut output = String::new();
    for scope in [HistoryScope::Agent, HistoryScope::Command] {
        output.push_str("# ");
        output.push_str(scope.as_str());
        output.push('\n');
        for entry in read(root, scope)? {
            output.push_str(&encode_structured_prompt_history_entry(&entry)?);
            output.push('\n');
        }
    }
    Ok(Some(output))
}

/// Reports whether any legacy history file exists below one store root.
fn legacy_history_exists(root: &Path) -> bool {
    if path_exists(&root.join(LEGACY_AGENT_HISTORY_FILE_NAME))
        || path_exists(&root.join(LEGACY_COMMAND_HISTORY_FILE_NAME))
    {
        return true;
    }
    std::fs::read_dir(root).is_ok_and(|entries| {
        entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .any(|path| path.is_dir() && path_exists(&path.join(LEGACY_AGENT_HISTORY_FILE_NAME)))
    })
}

/// Maps one rusqlite failure to an actionable history error.
fn database_error(error: rusqlite::Error) -> MezError {
    if let rusqlite::Error::SqliteFailure(code, _) = &error
        && matches!(
            code.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        )
    {
        return MezError::conflict(format!(
            "prompt history database is busy; retry the request ({error})"
        ));
    }
    MezError::invalid_state(format!("prompt history database error: {error}"))
}

/// Refuses a database path that is a symbolic link.
fn reject_symlink(path: &Path) -> Result<()> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        return Err(MezError::invalid_state(format!(
            "prompt history database path {} must not be a symbolic link",
            path.display()
        )));
    }
    Ok(())
}

/// Reports whether one path exists, including a dangling symbolic link.
fn path_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Reads one scope's history without creating, migrating, or importing.
pub(super) fn read(root: &Path, scope: HistoryScope) -> Result<Vec<ReadlineHistoryEntry>> {
    let path = database_path(root);
    reject_symlink(&path)?;
    if !path_exists(&path) {
        return legacy_entries(root, scope);
    }
    let Some(connection) = open_shared_database_read_only(&path)? else {
        return legacy_entries(root, scope);
    };
    let version = schema_version(&connection)?;
    if version == 0 {
        // A writer may have created the file but not yet published the schema,
        // so the legacy files are still the source of truth for a reader.
        return legacy_entries(root, scope);
    }
    if version != HISTORY_SCHEMA_VERSION {
        return Err(MezError::invalid_state(format!(
            "prompt history database schema version {version} does not match this build's version {HISTORY_SCHEMA_VERSION}; restart with the build that wrote it, or delete {} and let the next write import the legacy files",
            path.display()
        )));
    }
    if !migration_completed(&connection, scope.import_marker())? {
        return legacy_entries(root, scope);
    }
    read_rows(&connection, scope)
}

/// Appends one accepted entry and reports whether the store changed.
pub(super) fn append(
    root: &Path,
    scope: HistoryScope,
    entry: &ReadlineHistoryEntry,
) -> Result<bool> {
    let _guard = WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut attempt = 0usize;
    loop {
        match append_once(root, scope, entry) {
            Ok(changed) => return Ok(changed),
            Err(error)
                if attempt < BUSY_RETRIES
                    && error.kind() == crate::error::MezErrorKind::Conflict =>
            {
                attempt += 1;
                std::thread::sleep(BUSY_RETRY_DELAY);
            }
            Err(error) => return Err(error),
        }
    }
}

/// Opens the store for one write attempt.
fn append_once(root: &Path, scope: HistoryScope, entry: &ReadlineHistoryEntry) -> Result<bool> {
    let path = database_path(root);
    reject_symlink(&path)?;
    let (mut connection, state) = open_shared_database(&path, HISTORY_SCHEMA_VERSION)?;
    if state == SharedSchemaState::Fresh {
        create_schema(&mut connection)?;
    }
    import_legacy_once(root, scope, &mut connection)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    let changed = upsert_entry(&transaction, scope, entry)?;
    if changed {
        retain_within_bounds(&transaction, scope)?;
    }
    transaction.commit().map_err(database_error)?;
    Ok(changed)
}

/// Creates the history schema and publishes its version atomically.
///
/// Two writers can open a fresh database concurrently, so table and index
/// creation is idempotent and both may publish the same version.
fn create_schema(connection: &mut Connection) -> Result<()> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    transaction
        .execute_batch("CREATE TABLE IF NOT EXISTS history (scope TEXT NOT NULL, position INTEGER NOT NULL CHECK (position >= 0), text TEXT NOT NULL, collapsed_paste_ranges TEXT NOT NULL, PRIMARY KEY (scope, position)); CREATE INDEX IF NOT EXISTS history_by_scope_position ON history(scope, position DESC);")
        .map_err(database_error)?;
    set_schema_version(&transaction, HISTORY_SCHEMA_VERSION)?;
    transaction.commit().map_err(database_error)
}

/// Imports this scope's legacy files exactly once.
fn import_legacy_once(root: &Path, scope: HistoryScope, connection: &mut Connection) -> Result<()> {
    if migration_completed(connection, scope.import_marker())? {
        return Ok(());
    }
    let entries = legacy_entries(root, scope)?;
    import_legacy_file_once(connection, scope.import_marker(), |transaction| {
        transaction
            .execute(
                "DELETE FROM history WHERE scope = ?1",
                params![scope.as_str()],
            )
            .map_err(database_error)?;
        insert_all(transaction, scope, &entries)?;
        Ok(entries.len() as i64)
    })?;
    Ok(())
}

/// Decodes, merges, and canonicalizes this scope's legacy files.
///
/// The agent scope reproduces the historical per-conversation merge: the shared
/// file rows come first, then every `<root>/<conversation>/prompt-history.tsv`
/// in sorted path order, and the concatenation is canonicalized exactly like a
/// read, so the imported history is byte-for-byte the history the TSV build
/// would have served after its own migration.
fn legacy_entries(root: &Path, scope: HistoryScope) -> Result<Vec<ReadlineHistoryEntry>> {
    let mut entries = Vec::new();
    match scope {
        HistoryScope::Agent => {
            entries.extend(read_legacy_file(
                &root.join(LEGACY_AGENT_HISTORY_FILE_NAME),
            )?);
            let mut legacy_paths = std::fs::read_dir(root)?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.is_dir())
                .map(|path| path.join(LEGACY_AGENT_HISTORY_FILE_NAME))
                .filter(|path| path.is_file())
                .collect::<Vec<_>>();
            legacy_paths.sort();
            for path in legacy_paths {
                entries.extend(read_legacy_file(&path)?);
            }
        }
        HistoryScope::Command => {
            entries.extend(read_legacy_file(
                &root.join(LEGACY_COMMAND_HISTORY_FILE_NAME),
            )?);
        }
    }
    Ok(canonicalize_structured_history(entries))
}

/// Decodes every non-empty legacy history row in one file.
fn read_legacy_file(path: &Path) -> Result<Vec<ReadlineHistoryEntry>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let data = String::from_utf8(bytes)
        .map_err(|_| MezError::invalid_args("prompt history file is not valid UTF-8"))?;
    data.lines()
        .filter(|line| !line.trim().is_empty())
        .map(decode_structured_prompt_history_entry)
        .collect()
}

/// Reads one scope's rows in append order and canonicalizes them.
fn read_rows(connection: &Connection, scope: HistoryScope) -> Result<Vec<ReadlineHistoryEntry>> {
    let mut statement = connection
        .prepare("SELECT text, collapsed_paste_ranges FROM history WHERE scope = ?1 ORDER BY position ASC")
        .map_err(database_error)?;
    let rows = statement
        .query_map(params![scope.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(database_error)?;
    let mut entries = Vec::new();
    for row in rows {
        let (text, ranges) = row.map_err(database_error)?;
        entries.push(ReadlineHistoryEntry {
            text,
            collapsed_paste_ranges: decode_ranges(&ranges)?,
        });
    }
    Ok(canonicalize_structured_history(entries))
}

/// One stored row for the latest-entry comparison.
struct StoredEntry {
    position: i64,
    text: String,
    collapsed_paste_ranges: String,
}

/// Stores one accepted entry, replacing only representation metadata when the
/// raw text matches the current tail, and reports whether the store changed.
fn upsert_entry(
    transaction: &Transaction<'_>,
    scope: HistoryScope,
    entry: &ReadlineHistoryEntry,
) -> Result<bool> {
    let latest = transaction
        .query_row(
            "SELECT position, text, collapsed_paste_ranges FROM history WHERE scope = ?1 ORDER BY position DESC LIMIT 1",
            params![scope.as_str()],
            |row| {
                Ok(StoredEntry {
                    position: row.get(0)?,
                    text: row.get(1)?,
                    collapsed_paste_ranges: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(database_error)?;
    let ranges = encode_ranges(&entry.collapsed_paste_ranges);
    if let Some(latest) = &latest
        && latest.text == entry.text
    {
        if latest.collapsed_paste_ranges == ranges {
            return Ok(false);
        }
        transaction
            .execute(
                "UPDATE history SET collapsed_paste_ranges = ?3 WHERE scope = ?1 AND position = ?2",
                params![scope.as_str(), latest.position, ranges],
            )
            .map_err(database_error)?;
        return Ok(true);
    }
    let position = latest.map(|latest| latest.position).unwrap_or(0) + 1;
    transaction
        .execute(
            "INSERT INTO history (scope, position, text, collapsed_paste_ranges) VALUES (?1, ?2, ?3, ?4)",
            params![scope.as_str(), position, entry.text, ranges],
        )
        .map_err(database_error)?;
    Ok(true)
}

/// Inserts imported rows in append order from position one.
fn insert_all(
    transaction: &Transaction<'_>,
    scope: HistoryScope,
    entries: &[ReadlineHistoryEntry],
) -> Result<()> {
    for (index, entry) in entries.iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO history (scope, position, text, collapsed_paste_ranges) VALUES (?1, ?2, ?3, ?4)",
                params![
                    scope.as_str(),
                    index as i64 + 1,
                    entry.text,
                    encode_ranges(&entry.collapsed_paste_ranges),
                ],
            )
            .map_err(database_error)?;
    }
    Ok(())
}

/// Drops the oldest rows while the scope exceeds its count or byte budget.
///
/// The budget mirrors the read-side canonicalization exactly: at most
/// `DEFAULT_AGENT_PROMPT_HISTORY_LIMIT` entries and at most
/// `MAX_READLINE_HISTORY_BYTES` of raw prompt text, oldest first.
fn retain_within_bounds(transaction: &Transaction<'_>, scope: HistoryScope) -> Result<()> {
    let entries = {
        let mut statement = transaction
            .prepare(
                "SELECT position, LENGTH(CAST(text AS BLOB)) FROM history WHERE scope = ?1 ORDER BY position ASC",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map(params![scope.as_str()], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(database_error)?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row.map_err(database_error)?);
        }
        entries
    };
    let mut retained_bytes = entries
        .iter()
        .map(|(_, bytes)| *bytes as usize)
        .sum::<usize>();
    let mut retained = entries.len();
    let mut removed = 0usize;
    while retained > DEFAULT_AGENT_PROMPT_HISTORY_LIMIT
        || retained_bytes > mez_mux::readline::MAX_READLINE_HISTORY_BYTES
    {
        let Some((_, bytes)) = entries.get(removed) else {
            break;
        };
        retained_bytes = retained_bytes.saturating_sub(*bytes as usize);
        removed += 1;
        retained = retained.saturating_sub(1);
    }
    if removed > 0 {
        match entries.get(removed) {
            Some((cutoff, _)) => {
                transaction
                    .execute(
                        "DELETE FROM history WHERE scope = ?1 AND position < ?2",
                        params![scope.as_str(), cutoff],
                    )
                    .map_err(database_error)?;
            }
            None => {
                // The scope is smaller than one retained row's budget, so the
                // only state inside the bounds is an empty scope.
                transaction
                    .execute(
                        "DELETE FROM history WHERE scope = ?1",
                        params![scope.as_str()],
                    )
                    .map_err(database_error)?;
            }
        }
    }
    Ok(())
}

/// Encodes collapsed-paste ranges in the legacy field format.
fn encode_ranges(ranges: &[ReadlinePasteRange]) -> String {
    ranges
        .iter()
        .map(|range| format!("{}:{}", range.start, range.end))
        .collect::<Vec<_>>()
        .join(",")
}

/// Decodes collapsed-paste ranges stored in the legacy field format.
fn decode_ranges(encoded: &str) -> Result<Vec<ReadlinePasteRange>> {
    if encoded.is_empty() {
        return Ok(Vec::new());
    }
    encoded
        .split(',')
        .map(|range| {
            let (start, end) = range.split_once(':').ok_or_else(|| {
                MezError::invalid_state("prompt history paste range is malformed")
            })?;
            Ok(ReadlinePasteRange {
                start: start.parse::<usize>().map_err(|_| {
                    MezError::invalid_state("prompt history paste range start is malformed")
                })?,
                end: end.parse::<usize>().map_err(|_| {
                    MezError::invalid_state("prompt history paste range end is malformed")
                })?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Allocates one unique temporary directory for a focused history test.
    fn test_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mez-history-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// A stored paste range that no longer decodes fails closed instead of
    /// being served as history.
    #[test]
    fn history_read_fails_closed_on_a_malformed_stored_range() {
        let root = test_root("malformed-range");
        let entry = ReadlineHistoryEntry {
            text: "prompt with a range".to_string(),
            collapsed_paste_ranges: vec![ReadlinePasteRange { start: 7, end: 11 }],
        };
        assert!(append(&root, HistoryScope::Agent, &entry).unwrap());

        let connection = rusqlite::Connection::open(database_path(&root)).unwrap();
        connection
            .execute(
                "UPDATE history SET collapsed_paste_ranges = 'bogus' WHERE scope = 'agent'",
                [],
            )
            .unwrap();
        drop(connection);

        let error = read(&root, HistoryScope::Agent).unwrap_err();
        assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A symlinked database path is refused rather than followed, for reads and
    /// writes alike.
    #[test]
    fn history_rejects_a_symlinked_database_path() {
        use std::os::unix::fs::symlink;

        let root = test_root("symlink");
        symlink(root.join("missing-target"), database_path(&root)).unwrap();
        let entry = ReadlineHistoryEntry::literal("prompt");

        assert!(append(&root, HistoryScope::Agent, &entry).is_err());
        assert!(read(&root, HistoryScope::Agent).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    /// A scope whose single retained row already exceeds the byte budget is
    /// emptied instead of keeping a row the read path would always trim.
    #[test]
    fn history_retention_empties_a_scope_that_cannot_fit_one_row() {
        let root = test_root("oversized-row");
        let oversized = "x".repeat(mez_mux::readline::MAX_READLINE_HISTORY_BYTES + 1);
        assert!(
            append(
                &root,
                HistoryScope::Agent,
                &ReadlineHistoryEntry::literal("seed")
            )
            .unwrap()
        );

        let connection = rusqlite::Connection::open(database_path(&root)).unwrap();
        connection
            .execute(
                "UPDATE history SET text = ?1, collapsed_paste_ranges = '0:1' WHERE scope = 'agent'",
                params![oversized],
            )
            .unwrap();
        drop(connection);

        // The tail text matches, so the append only replaces the representation
        // and then trims a scope that cannot hold even one such row.
        assert!(
            append(
                &root,
                HistoryScope::Agent,
                &ReadlineHistoryEntry::literal(&oversized)
            )
            .unwrap()
        );
        // The read view already filters an over-entry row, so the database
        // state is what proves the trim deleted it instead of leaving it.
        let connection = rusqlite::Connection::open(database_path(&root)).unwrap();
        let rows: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM history WHERE scope = 'agent'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(connection);
        assert_eq!(rows, 0, "the unfittable row is deleted, not filtered");
        assert!(read(&root, HistoryScope::Agent).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    /// A database file whose schema version is not published yet keeps readers
    /// on the legacy files instead of failing them.
    #[test]
    fn history_read_falls_back_while_the_schema_version_is_unpublished() {
        let root = test_root("unpublished-schema");
        let entry = ReadlineHistoryEntry::literal("shared prompt");
        let row = super::super::encoding::encode_structured_prompt_history_entry(&entry).unwrap();
        std::fs::write(
            root.join(LEGACY_AGENT_HISTORY_FILE_NAME),
            format!("{row}\n"),
        )
        .unwrap();
        let connection = rusqlite::Connection::open(database_path(&root)).unwrap();
        connection
            .execute_batch("CREATE TABLE history (scope TEXT);")
            .unwrap();
        drop(connection);

        let entries = read(&root, HistoryScope::Agent).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "shared prompt");
        let _ = std::fs::remove_dir_all(root);
    }
}
