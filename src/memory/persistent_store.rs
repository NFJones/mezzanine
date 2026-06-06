//! Persistent memory store implementation.
//!
//! This module owns durable SQLite I/O, private permissions, TSV migration and
//! export compatibility, and stable record ordering.

use rusqlite::{Connection, OptionalExtension, params};

use super::{
    MemoryKind, MemoryRecord, MemoryScope, MemorySource, MemoryState, MezError, Path, PathBuf,
    PersistentMemoryStore, Result, decode_scope, encode_scope, fs, kind_name, parse_kind,
    parse_source, parse_state, set_private_dir_permissions, set_private_file_permissions,
    source_name, state_name,
};

const SCHEMA_VERSION: i64 = 1;
const LEGACY_TSV_FILE_NAME: &str = "memory.tsv";

impl PersistentMemoryStore {
    /// Runs the under config root operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn under_config_root(config_root: impl Into<PathBuf>) -> Self {
        Self {
            path: config_root.into().join("memory.sqlite"),
        }
    }

    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Runs the path operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runs the list operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn list(&self) -> Result<Vec<MemoryRecord>> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id, scope, created_at, updated_at, source, priority, kind, state,
                    last_used_at, use_count, confirmed_count, last_confirmed_at,
                    supersedes_id, expires_at, content
             FROM memory_records
             ORDER BY id ASC",
        )?;
        let rows = statement.query_map([], row_to_record)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(MezError::from)
    }

    /// Runs the inspect operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn inspect(&self, id: &str) -> Result<MemoryRecord> {
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT id, scope, created_at, updated_at, source, priority, kind, state,
                    last_used_at, use_count, confirmed_count, last_confirmed_at,
                    supersedes_id, expires_at, content
             FROM memory_records
             WHERE id = ?1",
        )?;
        statement
            .query_row(params![id], row_to_record)
            .optional()?
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "memory not found"))
    }

    /// Runs the upsert operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn upsert(&self, record: MemoryRecord) -> Result<()> {
        record.validate_for_persistence()?;
        let connection = self.open()?;
        upsert_record(&connection, &record)
    }

    /// Runs the edit content operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn edit_content(
        &self,
        id: &str,
        content: impl Into<String>,
        updated_at_unix_seconds: u64,
    ) -> Result<MemoryRecord> {
        let mut record = self.inspect(id)?;
        record.content = content.into();
        record.updated_at_unix_seconds = updated_at_unix_seconds;
        self.upsert(record.clone())?;
        Ok(record)
    }

    /// Runs the delete operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let connection = self.open()?;
        let changed =
            connection.execute("DELETE FROM memory_records WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }

    /// Runs the export tsv operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn export_tsv(&self) -> Result<String> {
        let mut output = String::new();
        for record in self.list()? {
            output.push_str(&record.encode()?);
            output.push('\n');
        }
        Ok(output)
    }

    /// Searches persistent memory with SQLite FTS and metadata filters.
    pub fn search(&self, request: &MemorySearchRequest) -> Result<Vec<MemorySearchResult>> {
        let connection = self.open()?;
        search_records(&connection, request)
    }

    /// Opens and initializes the SQLite-backed persistent memory store.
    fn open(&self) -> Result<Connection> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
            set_private_dir_permissions(parent)?;
        }
        let existed = self.path.exists();
        let mut connection = Connection::open(&self.path)?;
        initialize_schema(&mut connection)?;
        if !existed {
            self.import_legacy_tsv(&mut connection)?;
        }
        set_private_file_permissions(&self.path)?;
        Ok(connection)
    }

    /// Imports a sibling legacy TSV store into a newly-created SQLite store.
    fn import_legacy_tsv(&self, connection: &mut Connection) -> Result<()> {
        let Some(parent) = self.path.parent() else {
            return Ok(());
        };
        let legacy_path = parent.join(LEGACY_TSV_FILE_NAME);
        if !legacy_path.exists() {
            return Ok(());
        }
        let data = fs::read_to_string(legacy_path)?;
        let transaction = connection.transaction()?;
        for line in data.lines().filter(|line| !line.trim().is_empty()) {
            let record = MemoryRecord::decode(line)?;
            upsert_record(&transaction, &record)?;
        }
        transaction.commit()?;
        Ok(())
    }
}

/// Criteria used to search persistent memory records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemorySearchRequest {
    /// Optional FTS query. When omitted, records use deterministic fallback order.
    pub query: Option<String>,
    /// Optional exact scope filter.
    pub scope: Option<MemoryScope>,
    /// Optional memory kind filter.
    pub kind: Option<MemoryKind>,
    /// Optional lifecycle state filter.
    pub state: Option<MemoryState>,
    /// Optional source filter.
    pub source: Option<MemorySource>,
    /// Maximum number of results to return.
    pub limit: usize,
}

/// One persistent memory search result with deterministic retrieval metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct MemorySearchResult {
    /// Matching persistent memory record.
    pub record: MemoryRecord,
    /// Combined deterministic score used for ordering.
    pub score: f64,
    /// SQLite FTS rank when a query was provided.
    pub fts_rank: Option<f64>,
    /// Human-readable reason for retrieval/debug views.
    pub reason: String,
}

/// Creates the SQLite schema and FTS index for persistent memory.
fn initialize_schema(connection: &mut Connection) -> Result<()> {
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS memory_schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL,
             applied_at INTEGER NOT NULL,
             description TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS memory_records (
             id TEXT PRIMARY KEY NOT NULL,
             scope TEXT NOT NULL,
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL,
             source TEXT NOT NULL,
             priority INTEGER NOT NULL,
             kind TEXT NOT NULL DEFAULT 'fact',
             state TEXT NOT NULL DEFAULT 'active',
             last_used_at INTEGER,
             use_count INTEGER NOT NULL DEFAULT 0,
             confirmed_count INTEGER NOT NULL DEFAULT 0,
             last_confirmed_at INTEGER,
             supersedes_id TEXT,
             expires_at INTEGER,
             content TEXT NOT NULL,
             scope_text TEXT NOT NULL,
             CHECK (priority >= 0 AND priority <= 255),
             CHECK (kind IN ('preference', 'fact', 'procedure', 'episode', 'warning', 'scratch')),
             CHECK (state IN ('active', 'stale', 'superseded', 'archived', 'expired'))
         );
         CREATE INDEX IF NOT EXISTS memory_records_scope_idx ON memory_records(scope);
         CREATE INDEX IF NOT EXISTS memory_records_state_kind_idx ON memory_records(state, kind);
         CREATE INDEX IF NOT EXISTS memory_records_priority_updated_idx
             ON memory_records(priority DESC, updated_at DESC, id ASC);
         CREATE INDEX IF NOT EXISTS memory_records_source_idx ON memory_records(source);
         CREATE VIRTUAL TABLE IF NOT EXISTS memory_records_fts USING fts5(
             id UNINDEXED,
             content,
             kind,
             source,
             scope_text,
             content='memory_records',
             content_rowid='rowid'
         );
         CREATE TRIGGER IF NOT EXISTS memory_records_ai AFTER INSERT ON memory_records BEGIN
             INSERT INTO memory_records_fts(rowid, id, content, kind, source, scope_text)
             VALUES (new.rowid, new.id, new.content, new.kind, new.source, new.scope_text);
         END;
         CREATE TRIGGER IF NOT EXISTS memory_records_ad AFTER DELETE ON memory_records BEGIN
             INSERT INTO memory_records_fts(memory_records_fts, rowid, id, content, kind, source, scope_text)
             VALUES('delete', old.rowid, old.id, old.content, old.kind, old.source, old.scope_text);
         END;
         CREATE TRIGGER IF NOT EXISTS memory_records_au AFTER UPDATE ON memory_records BEGIN
             INSERT INTO memory_records_fts(memory_records_fts, rowid, id, content, kind, source, scope_text)
             VALUES('delete', old.rowid, old.id, old.content, old.kind, old.source, old.scope_text);
             INSERT INTO memory_records_fts(rowid, id, content, kind, source, scope_text)
             VALUES (new.rowid, new.id, new.content, new.kind, new.source, new.scope_text);
         END;",
    )?;
    connection.execute(
        "INSERT OR IGNORE INTO memory_schema_migrations (version, applied_at, description)
         VALUES (?1, strftime('%s', 'now'), ?2)",
        params![SCHEMA_VERSION, "create sqlite memory store with fts"],
    )?;
    Ok(())
}

/// Inserts or replaces a validated memory record.
fn upsert_record(connection: &Connection, record: &MemoryRecord) -> Result<()> {
    record.validate_for_persistence()?;
    connection.execute(
        "INSERT INTO memory_records (
             id, scope, created_at, updated_at, source, priority, kind, state,
             last_used_at, use_count, confirmed_count, last_confirmed_at,
             supersedes_id, expires_at, content, scope_text
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT(id) DO UPDATE SET
             scope = excluded.scope,
             created_at = excluded.created_at,
             updated_at = excluded.updated_at,
             source = excluded.source,
             priority = excluded.priority,
             kind = excluded.kind,
             state = excluded.state,
             last_used_at = excluded.last_used_at,
             use_count = excluded.use_count,
             confirmed_count = excluded.confirmed_count,
             last_confirmed_at = excluded.last_confirmed_at,
             supersedes_id = excluded.supersedes_id,
             expires_at = excluded.expires_at,
             content = excluded.content,
             scope_text = excluded.scope_text",
        params![
            record.id,
            encode_scope(&record.scope),
            record.created_at_unix_seconds,
            record.updated_at_unix_seconds,
            source_name(record.source),
            record.priority,
            kind_name(record.kind),
            state_name(record.state),
            record.last_used_at_unix_seconds,
            record.use_count,
            record.confirmed_count,
            record.last_confirmed_at_unix_seconds,
            record.supersedes_id,
            record.expires_at_unix_seconds,
            record.content,
            scope_text(&record.scope),
        ],
    )?;
    Ok(())
}

/// Converts a SQLite result row into a typed memory record.
fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryRecord> {
    let scope: String = row.get(1)?;
    let source: String = row.get(4)?;
    let kind: String = row.get(6)?;
    let state: String = row.get(7)?;
    let priority: i64 = row.get(5)?;
    Ok(MemoryRecord {
        id: row.get(0)?,
        scope: decode_scope(&scope).map_err(rusqlite_from_mez_error)?,
        created_at_unix_seconds: row.get::<_, u64>(2)?,
        updated_at_unix_seconds: row.get::<_, u64>(3)?,
        source: parse_source(&source).map_err(rusqlite_from_mez_error)?,
        priority: u8::try_from(priority).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Integer,
                Box::new(MezError::invalid_args("invalid memory priority")),
            )
        })?,
        kind: parse_kind(&kind).map_err(rusqlite_from_mez_error)?,
        state: parse_state(&state).map_err(rusqlite_from_mez_error)?,
        last_used_at_unix_seconds: row.get(8)?,
        use_count: row.get(9)?,
        confirmed_count: row.get(10)?,
        last_confirmed_at_unix_seconds: row.get(11)?,
        supersedes_id: row.get(12)?,
        expires_at_unix_seconds: row.get(13)?,
        content: row.get(14)?,
    })
}

/// Converts a Mezzanine parse error into a SQLite row conversion error.
fn rusqlite_from_mez_error(error: MezError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

/// Builds searchable scope text for the FTS index.
fn scope_text(scope: &MemoryScope) -> String {
    match scope {
        MemoryScope::Global => "global".to_string(),
        MemoryScope::Project { root } => format!("project {root}"),
        MemoryScope::Session { session_id } => format!("session {session_id}"),
        MemoryScope::Window {
            session_id,
            window_id,
        } => format!("window {session_id} {window_id}"),
        MemoryScope::Pane {
            session_id,
            pane_id,
        } => format!("pane {session_id} {pane_id}"),
        MemoryScope::Agent {
            session_id,
            agent_id,
        } => format!("agent {session_id} {agent_id}"),
    }
}

/// Searches SQLite-backed memory records with deterministic fallback ranking.
fn search_records(
    connection: &Connection,
    request: &MemorySearchRequest,
) -> Result<Vec<MemorySearchResult>> {
    if let Some(query) = request
        .query
        .as_deref()
        .filter(|query| !query.trim().is_empty())
    {
        search_records_with_query(connection, request, query)
    } else {
        search_records_without_query(connection, request)
    }
}

/// Normalizes untrusted free text into a safe SQLite FTS5 query.
fn normalized_fts_query(query: &str) -> String {
    query
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Searches memory records through FTS5 before applying metadata filters.
fn search_records_with_query(
    connection: &Connection,
    request: &MemorySearchRequest,
    query: &str,
) -> Result<Vec<MemorySearchResult>> {
    let fts_query = normalized_fts_query(query);
    let mut statement = connection.prepare(
        "SELECT r.id, r.scope, r.created_at, r.updated_at, r.source, r.priority,
                r.kind, r.state, r.last_used_at, r.use_count, r.confirmed_count,
                r.last_confirmed_at, r.supersedes_id, r.expires_at, r.content,
                bm25(memory_records_fts) AS rank
         FROM memory_records_fts
         JOIN memory_records r ON r.rowid = memory_records_fts.rowid
         WHERE memory_records_fts MATCH ?1
         ORDER BY rank ASC, r.priority DESC, r.updated_at DESC, r.id ASC
         LIMIT ?2",
    )?;
    let rows = statement.query_map(params![fts_query, search_limit(request.limit)], |row| {
        Ok((row_to_record(row)?, row.get::<_, f64>(15)?))
    })?;
    let mut results = Vec::new();
    for row in rows {
        let (record, rank) = row?;
        if !record_matches_request(&record, request) {
            continue;
        }
        let score = deterministic_score(&record) - rank;
        results.push(MemorySearchResult {
            record,
            score,
            fts_rank: Some(rank),
            reason: "fts query match plus deterministic metadata ranking".to_string(),
        });
    }
    results.sort_by(compare_search_results);
    results.truncate(search_limit(request.limit) as usize);
    Ok(results)
}

/// Searches memory records by metadata-only deterministic fallback order.
fn search_records_without_query(
    connection: &Connection,
    request: &MemorySearchRequest,
) -> Result<Vec<MemorySearchResult>> {
    let mut statement = connection.prepare(
        "SELECT id, scope, created_at, updated_at, source, priority, kind, state,
                last_used_at, use_count, confirmed_count, last_confirmed_at,
                supersedes_id, expires_at, content
         FROM memory_records
         ORDER BY priority DESC, updated_at DESC, id ASC
         LIMIT ?1",
    )?;
    let rows = statement.query_map(params![search_limit(request.limit)], row_to_record)?;
    let mut results = Vec::new();
    for row in rows {
        let record = row?;
        if !record_matches_request(&record, request) {
            continue;
        }
        results.push(MemorySearchResult {
            score: deterministic_score(&record),
            record,
            fts_rank: None,
            reason: "deterministic priority updated_at id fallback".to_string(),
        });
    }
    results.sort_by(compare_search_results);
    results.truncate(search_limit(request.limit) as usize);
    Ok(results)
}

/// Returns the bounded search limit used for database queries and final output.
fn search_limit(limit: usize) -> i64 {
    if limit == 0 {
        50
    } else {
        limit.min(500) as i64
    }
}

/// Returns whether a record satisfies metadata filters.
fn record_matches_request(record: &MemoryRecord, request: &MemorySearchRequest) -> bool {
    request
        .scope
        .as_ref()
        .is_none_or(|scope| scope == &record.scope)
        && request.kind.is_none_or(|kind| kind == record.kind)
        && request.state.is_none_or(|state| state == record.state)
        && request.source.is_none_or(|source| source == record.source)
}

/// Computes a deterministic non-FTS score from metadata.
fn deterministic_score(record: &MemoryRecord) -> f64 {
    f64::from(record.priority)
        + (record.updated_at_unix_seconds as f64 / 86_400.0).min(10_000.0)
        + (record.use_count as f64).min(1_000.0)
        + (record.confirmed_count as f64 * 2.0).min(1_000.0)
}

/// Orders search results by score, recency, and id.
fn compare_search_results(
    left: &MemorySearchResult,
    right: &MemorySearchResult,
) -> std::cmp::Ordering {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| {
            right
                .record
                .updated_at_unix_seconds
                .cmp(&left.record.updated_at_unix_seconds)
        })
        .then_with(|| left.record.id.cmp(&right.record.id))
}
