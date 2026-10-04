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

const SCHEMA_VERSION: i64 = 4;
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
        ensure_private_parent(&self.path)?;
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(Duration::from_millis(250))?;
        initialize_schema(&connection)?;
        set_private_file_permissions(&self.path)?;
        Ok(connection)
    }
}

/// Enables WAL before writer admission. SQLite can reject concurrent journal
/// mode changes without invoking its busy handler, so retry only this idempotent
/// setup operation within the ordinary connection contention budget. No usage
/// transaction or committed observation is replayed by this owner.
fn initialize_wal(connection: &Connection) -> Result<()> {
    let started = std::time::Instant::now();
    let budget = Duration::from_millis(250);
    loop {
        let result = connection.query_row("PRAGMA journal_mode = WAL", [], |row| {
            row.get::<_, String>(0)
        });
        match result {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") => return Ok(()),
            Ok(_) => return Err(MezError::invalid_state("token usage WAL mode unavailable")),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(
                    error.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) && started.elapsed() < budget =>
            {
                std::thread::sleep(
                    Duration::from_millis(2).min(budget.saturating_sub(started.elapsed())),
                );
            }
            Err(error) => {
                return Err(MezError::invalid_state(format!(
                    "token usage WAL initialization failed: {error}"
                )));
            }
        }
    }
}

/// Migrates schema transactionally after bounded WAL setup succeeds.
fn initialize_schema(connection: &Connection) -> Result<()> {
    initialize_wal(connection)?;
    let transaction =
        rusqlite::Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| {
                MezError::invalid_state(format!(
                    "token usage schema writer admission failed: {error}"
                ))
            })?;
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
        1 | 2 | 3 | SCHEMA_VERSION => {}
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
