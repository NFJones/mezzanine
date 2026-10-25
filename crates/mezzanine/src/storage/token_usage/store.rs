//! SQLite schema, append, retention, and rolling aggregation implementation.

#[cfg(test)]
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use mez_agent::{ModelTokenUsage, ModelTokenUsageKey};
use rusqlite::{Connection, OptionalExtension, params};

use super::{
    MezError, Result, TOKEN_USAGE_RETENTION_DAYS, ensure_private_parent,
    set_private_file_permissions, sqlite_i64,
};

pub(super) const SCHEMA_VERSION: i64 = 8;
const SECONDS_PER_DAY: u64 = 86_400;

/// One immutable provider/model usage delta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenUsageEvent {
    /// Stable idempotency key for this accounting delta.
    pub(crate) id: String,
    /// Immutable project attribution; legacy/unqualified expense stays absent.
    pub(crate) project: Option<super::AccountingProjectId>,
    /// UTC Unix timestamp at which the provider result settled.
    pub(crate) observed_at_unix_seconds: u64,
    /// Provider/model identity reported by the selected profile.
    pub(crate) model: ModelTokenUsageKey,
    /// Provider-reported token counters for this delta.
    pub(crate) usage: ModelTokenUsage,
}

/// Cloneable handle to the private token-accounting SQLite database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenUsageStore {
    path: PathBuf,
}

impl TokenUsageStore {
    /// Builds a store under the standard Mezzanine configuration root.
    pub(crate) fn under_config_root(config_root: impl AsRef<Path>) -> Self {
        Self {
            path: super::default_token_usage_database_path(config_root),
        }
    }

    /// Builds a store at an explicit path for focused tests.
    #[cfg(test)]
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the configured database path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Initializes and validates the schema, then prunes expired raw events.
    pub(crate) fn initialize(&self, now_unix_seconds: u64) -> Result<()> {
        let connection = self.open()?;
        let oldest_retained = now_unix_seconds
            .saturating_sub(TOKEN_USAGE_RETENTION_DAYS.saturating_mul(SECONDS_PER_DAY));
        connection.execute(
            "DELETE FROM token_usage_events WHERE observed_at < ?1",
            [sqlite_i64(oldest_retained, "retention cutoff")?],
        )?;
        // Stream high-water marks remain as replay tombstones after bounded
        // receipt/event retention. Old sequences can never become new deltas.
        connection.execute(
            "DELETE FROM external_usage_receipts WHERE observed_at < ?1",
            [sqlite_i64(oldest_retained, "retention cutoff")?],
        )?;
        Ok(())
    }

    /// Appends one non-zero event; identical replay is inert and changed replay conflicts.
    pub(crate) fn append(&self, event: &TokenUsageEvent) -> Result<bool> {
        if event.id.trim().is_empty() {
            return Err(MezError::invalid_args(
                "token usage event id must not be empty",
            ));
        }
        let mut connection = self.open()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing = transaction.query_row(
            "SELECT observed_at, provider, model, input_tokens, output_tokens, reasoning_tokens,
                    cached_input_tokens, cache_write_input_tokens, project_id FROM token_usage_events WHERE id = ?1 AND event_source = 'native'",
            [&event.id], |row| Ok(TokenUsageEvent {
                id: event.id.clone(), project: row.get::<_, Option<String>>(8)?.map(super::AccountingProjectId::from_stored)
                    .transpose().map_err(|error| rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(error)))?,
                observed_at_unix_seconds: row_u64(row, 0)?,
                model: ModelTokenUsageKey::new(row.get::<_, String>(1)?, row.get::<_, String>(2)?),
                usage: ModelTokenUsage { input_tokens: row_u64(row, 3)?, output_tokens: row_u64(row, 4)?,
                    reasoning_tokens: row_u64(row, 5)?, cached_input_tokens: row_optional_u64(row, 6)?,
                    cache_write_input_tokens: row_optional_u64(row, 7)? },
            }),
        ).optional()?;
        if let Some(existing) = existing {
            if existing != *event {
                return Err(MezError::conflict(
                    "token usage event id has conflicting payload",
                ));
            }
            return Ok(false);
        }
        if event.usage.is_zero() {
            return Ok(false);
        }
        let changed = transaction.execute(
            "INSERT INTO token_usage_events (
                 id, observed_at, provider, model, input_tokens, output_tokens,
                 reasoning_tokens, cached_input_tokens, cache_write_input_tokens, project_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                event.id,
                sqlite_i64(event.observed_at_unix_seconds, "timestamp")?,
                event.model.provider,
                event.model.model,
                sqlite_i64(event.usage.input_tokens, "input tokens")?,
                sqlite_i64(event.usage.output_tokens, "output tokens")?,
                sqlite_i64(event.usage.reasoning_tokens, "reasoning tokens")?,
                event
                    .usage
                    .cached_input_tokens
                    .map(|value| sqlite_i64(value, "cached input tokens"))
                    .transpose()?,
                event
                    .usage
                    .cache_write_input_tokens
                    .map(|value| sqlite_i64(value, "cache-write input tokens"))
                    .transpose()?,
                event
                    .project
                    .as_ref()
                    .map(super::AccountingProjectId::as_str),
            ],
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    /// Aggregates all requested exact rolling windows with one indexed scan.
    #[cfg(test)]
    pub(crate) fn aggregate_windows(
        &self,
        now_unix_seconds: u64,
        windows_days: &[u16],
    ) -> Result<BTreeMap<u16, BTreeMap<ModelTokenUsageKey, ModelTokenUsage>>> {
        let mut aggregates = windows_days
            .iter()
            .copied()
            .map(|days| (days, BTreeMap::new()))
            .collect::<BTreeMap<_, _>>();
        let Some(oldest_days) = windows_days.iter().copied().max() else {
            return Ok(aggregates);
        };
        let oldest_cutoff =
            now_unix_seconds.saturating_sub(u64::from(oldest_days).saturating_mul(SECONDS_PER_DAY));
        let connection = self.open()?;
        let mut statement = connection.prepare(
            "SELECT observed_at, provider, model, input_tokens, output_tokens,
                    reasoning_tokens, cached_input_tokens, cache_write_input_tokens
             FROM token_usage_events
             WHERE observed_at >= ?1 AND observed_at <= ?2 AND event_source = 'native'
             ORDER BY observed_at ASC, id ASC",
        )?;
        let rows = statement.query_map(
            params![
                sqlite_i64(oldest_cutoff, "query cutoff")?,
                sqlite_i64(now_unix_seconds, "query timestamp")?,
            ],
            |row| {
                Ok((
                    row_u64(row, 0)?,
                    ModelTokenUsageKey::new(row.get::<_, String>(1)?, row.get::<_, String>(2)?),
                    ModelTokenUsage {
                        input_tokens: row_u64(row, 3)?,
                        output_tokens: row_u64(row, 4)?,
                        reasoning_tokens: row_u64(row, 5)?,
                        cached_input_tokens: row_optional_u64(row, 6)?,
                        cache_write_input_tokens: row_optional_u64(row, 7)?,
                    },
                ))
            },
        )?;
        for row in rows {
            let (observed_at, model, usage) = row?;
            for days in windows_days {
                let cutoff = now_unix_seconds
                    .saturating_sub(u64::from(*days).saturating_mul(SECONDS_PER_DAY));
                if observed_at >= cutoff {
                    aggregates
                        .entry(*days)
                        .or_default()
                        .entry(model.clone())
                        .or_default()
                        .add_assign(usage);
                }
            }
        }
        Ok(aggregates)
    }

    /// Returns the oldest stored event at or before the supplied time.
    #[cfg(test)]
    pub(crate) fn oldest_observed_at(&self, now_unix_seconds: u64) -> Result<Option<u64>> {
        let connection = self.open()?;
        let oldest = connection.query_row(
            "SELECT MIN(observed_at) FROM token_usage_events WHERE observed_at <= ?1 AND event_source = 'native'",
            [sqlite_i64(now_unix_seconds, "query timestamp")?],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        oldest
            .map(|value| {
                u64::try_from(value).map_err(|_| {
                    MezError::invalid_args("token usage timestamp must not be negative")
                })
            })
            .transpose()
    }

    pub(super) fn open(&self) -> Result<Connection> {
        self.open_with_external_admission(&mut super::ExternalUsageAdmissionWitness::default())
    }

    /// Preserves ordinary store opening while observing only a final WAL
    /// refusal for this invocation. Exhausted schema admission and any write
    /// remain terminal; only pre-acquisition initialization is retried, inside
    /// one bounded deadline.
    pub(super) fn open_with_external_admission(
        &self,
        admission: &mut super::ExternalUsageAdmissionWitness,
    ) -> Result<Connection> {
        admission.clear();
        ensure_private_parent(&self.path)?;
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(Duration::from_millis(250))?;
        let deadline = std::time::Instant::now() + STORE_INITIALIZATION_BUDGET;
        initialize_schema(&connection, admission, deadline)?;
        set_private_file_permissions(&self.path)?;
        Ok(connection)
    }
}

/// Bounded total budget for concurrent store initialization.
///
/// Unlike an ordinary statement on an already-initialized store, the winner of
/// initialization performs WAL setup and a full migration, so a concurrent
/// opener waits for that work before it can take writer ownership. Both WAL
/// setup and schema admission draw on this single deadline, which exists so a
/// genuinely stuck writer still fails truthfully instead of blocking an opener
/// forever. This is a shared contention-retry horizon, not a hard total timeout:
/// the guard is evaluated between attempts, so an in-flight statement may still
/// finish after it, and migration execution is not retried by either loop.
const STORE_INITIALIZATION_BUDGET: Duration = Duration::from_millis(1_000);

/// Enables WAL before writer admission. SQLite can reject concurrent journal
/// mode changes without invoking its busy handler, so retry only this idempotent
/// setup operation within the ordinary connection contention budget. No usage
/// transaction or committed observation is replayed by this owner.
fn initialize_wal(
    connection: &Connection,
    admission: &mut super::ExternalUsageAdmissionWitness,
    deadline: std::time::Instant,
) -> Result<()> {
    loop {
        let result = connection.query_row("PRAGMA journal_mode = WAL", [], |row| {
            row.get::<_, String>(0)
        });
        match result {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") => return Ok(()),
            Ok(_) => return Err(MezError::invalid_state("token usage WAL mode unavailable")),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(
                    error.extended_code,
                    rusqlite::ffi::SQLITE_BUSY | rusqlite::ffi::SQLITE_LOCKED
                ) && std::time::Instant::now() < deadline =>
            {
                // Retry only while the one initialization deadline allows. A
                // failure after it falls through to the terminal arm and becomes
                // the recorded refusal; an attempt that succeeds late is still
                // accepted, since this is a retry horizon rather than a fence.
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => {
                // Record only this final return. Earlier busy results followed
                // by successful WAL setup never become replay authority.
                admission.refused(super::ExternalPrecommitPhase::WalSetup, &error);
                return Err(MezError::invalid_state(format!(
                    "token usage WAL initialization failed: {error}"
                )));
            }
        }
    }
}

/// Migrates schema transactionally after bounded WAL setup succeeds.
fn initialize_schema(
    connection: &Connection,
    admission: &mut super::ExternalUsageAdmissionWitness,
    deadline: std::time::Instant,
) -> Result<()> {
    initialize_wal(connection, admission, deadline)?;
    // Current-schema inspection needs no writer ownership. Migrations still
    // reread the version after acquiring their immediate transaction below.
    let observed_version: i64 =
        connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if observed_version == SCHEMA_VERSION {
        return Ok(());
    }
    // Concurrent openers of a fresh store must serialize through schema
    // creation: the loser of this admission is expected to wait, not fail, and
    // the winner's work is a full WAL setup plus migration rather than a single
    // statement. SQLite also does not invoke its busy handler for every
    // writer-admission conflict (a would-deadlock BEGIN IMMEDIATE returns BUSY
    // immediately), so retry this one idempotent admission within a budget
    // sized for that initialization work and drawn from the same deadline WAL
    // setup uses. No usage transaction or committed observation is replayed by
    // this owner.
    let transaction = loop {
        match rusqlite::Transaction::new_unchecked(
            connection,
            rusqlite::TransactionBehavior::Immediate,
        ) {
            Ok(transaction) => break transaction,
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(
                    error.extended_code,
                    rusqlite::ffi::SQLITE_BUSY | rusqlite::ffi::SQLITE_LOCKED
                ) && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => {
                return Err(MezError::invalid_state(format!(
                    "token usage schema writer admission failed: {error}"
                )));
            }
        }
    };
    let version: i64 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    match version {
        0 => {
            transaction.execute_batch(
                "CREATE TABLE token_usage_events (
                     id TEXT PRIMARY KEY NOT NULL,
                     observed_at INTEGER NOT NULL CHECK (observed_at >= 0),
                     provider TEXT NOT NULL,
                     model TEXT NOT NULL,
                     input_tokens INTEGER NOT NULL CHECK (input_tokens >= 0),
                     output_tokens INTEGER NOT NULL CHECK (output_tokens >= 0),
                     reasoning_tokens INTEGER NOT NULL CHECK (reasoning_tokens >= 0),
                     cached_input_tokens INTEGER NULL CHECK (cached_input_tokens >= 0),
                     cache_write_input_tokens INTEGER NULL CHECK (cache_write_input_tokens >= 0)
                 );
                 CREATE INDEX token_usage_events_observed_model
                     ON token_usage_events(observed_at, provider, model);
                 PRAGMA user_version = 1;",
            )?;
        }
        1 | 2 | 3 | 4 | 5 | 6 | 7 | SCHEMA_VERSION => {}
        future if future > SCHEMA_VERSION => {
            return Err(MezError::invalid_state(format!(
                "token usage database schema version {future} is newer than supported version {SCHEMA_VERSION}"
            )));
        }
        other => {
            return Err(MezError::invalid_state(format!(
                "unsupported token usage database schema version {other}"
            )));
        }
    }
    if version < 2 {
        transaction.execute_batch("ALTER TABLE token_usage_events ADD COLUMN harness TEXT NOT NULL DEFAULT 'mez';
            ALTER TABLE token_usage_events ADD COLUMN event_source TEXT NOT NULL DEFAULT 'native' CHECK(event_source IN ('native','external'));
            ALTER TABLE token_usage_events ADD COLUMN reasoning_known INTEGER NOT NULL DEFAULT 1 CHECK(reasoning_known IN (0,1));
            CREATE TABLE external_usage_streams(id TEXT PRIMARY KEY NOT NULL,harness TEXT NOT NULL,provider TEXT NOT NULL,model TEXT NOT NULL,
                mode TEXT NOT NULL,revision INTEGER NOT NULL CHECK(revision>0),sample TEXT NOT NULL,totals TEXT NOT NULL);
            CREATE TABLE external_usage_receipts(stream_id TEXT NOT NULL,event_id TEXT NOT NULL,fingerprint TEXT NOT NULL,
                observed_at INTEGER NOT NULL CHECK(observed_at>=0),PRIMARY KEY(stream_id,event_id));
            CREATE INDEX external_usage_receipts_time ON external_usage_receipts(observed_at);
            PRAGMA user_version=2;")?;
    }
    if version < 3 {
        transaction.execute_batch(
            "CREATE TABLE accounting_projects (
            id TEXT PRIMARY KEY NOT NULL, root BLOB NOT NULL, object TEXT NOT NULL,
            UNIQUE(root,object)); PRAGMA user_version=3;",
        )?;
    }
    if version < 4 {
        transaction.execute_batch("ALTER TABLE token_usage_events ADD COLUMN project_id TEXT NULL;
            ALTER TABLE external_usage_streams ADD COLUMN project_id TEXT NULL;
            CREATE INDEX token_usage_events_project_time ON token_usage_events(project_id,observed_at);
            PRAGMA user_version=4;")?;
    }
    if version < 5 {
        transaction.execute_batch(
            "CREATE TABLE external_usage_sources (
            witness TEXT PRIMARY KEY NOT NULL CHECK(length(witness)=64),
            harness TEXT NOT NULL,
            owner TEXT UNIQUE NOT NULL CHECK(length(owner)=36));
            PRAGMA user_version=5;",
        )?;
    }
    if version < 6 {
        transaction.execute_batch(
            "CREATE TABLE external_usage_observations (
            owner TEXT NOT NULL,event_id TEXT NOT NULL CHECK(length(event_id)=64),
            fingerprint TEXT NOT NULL CHECK(length(fingerprint)=64),
            stream_id TEXT NOT NULL CHECK(length(stream_id)=64),PRIMARY KEY(owner,event_id));
            PRAGMA user_version=6;",
        )?;
    }
    if version < 7 {
        transaction.execute_batch("CREATE TABLE external_usage_requests (
            owner TEXT NOT NULL,request_id TEXT NOT NULL CHECK(length(request_id)=64),
            harness TEXT NOT NULL,project_id TEXT NULL,admitted_at INTEGER NOT NULL CHECK(admitted_at>0),
            observation_id TEXT NULL CHECK(observation_id IS NULL OR length(observation_id)=64),
            PRIMARY KEY(owner,request_id),UNIQUE(owner,observation_id));
            ALTER TABLE external_usage_observations ADD COLUMN request_id TEXT NULL;
            PRAGMA user_version=7;")?;
    }
    if version < 8 {
        transaction.execute_batch(
            "CREATE TABLE external_usage_source_activations (
            owner TEXT PRIMARY KEY NOT NULL CHECK(length(owner)=36),
            attempt BLOB NOT NULL CHECK(typeof(attempt)='blob' AND length(attempt)=32),
            ancestry BLOB NOT NULL CHECK(typeof(ancestry)='blob' AND length(ancestry)=32),
            prepared_at_boot_ms INTEGER NOT NULL CHECK(prepared_at_boot_ms>0),
            expires_at_boot_ms INTEGER NOT NULL CHECK(expires_at_boot_ms>prepared_at_boot_ms
                AND expires_at_boot_ms-prepared_at_boot_ms<=60000),
            revision INTEGER NOT NULL CHECK(revision>0),
            state INTEGER NOT NULL CHECK(state IN (0,1,2)),
            FOREIGN KEY(owner) REFERENCES external_usage_sources(owner));
            PRAGMA user_version=8;",
        )?;
    }
    transaction.commit()?;
    Ok(())
}

pub(super) fn row_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| conversion_error(index, "negative token usage value"))
}

pub(super) fn row_optional_u64(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(index)?
        .map(|value| {
            u64::try_from(value)
                .map_err(|_| conversion_error(index, "negative optional token usage value"))
        })
        .transpose()
}

fn conversion_error(index: usize, message: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Integer,
        Box::new(MezError::invalid_args(message)),
    )
}
