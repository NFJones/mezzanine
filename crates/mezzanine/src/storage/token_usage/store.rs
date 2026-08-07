//! SQLite schema, append, retention, and rolling aggregation implementation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use mez_agent::{ModelTokenUsage, ModelTokenUsageKey};
use rusqlite::{Connection, params};

use super::{
    MezError, Result, TOKEN_USAGE_RETENTION_DAYS, ensure_private_parent,
    set_private_file_permissions, sqlite_i64,
};

const SCHEMA_VERSION: i64 = 1;
const SECONDS_PER_DAY: u64 = 86_400;

/// One immutable provider/model usage delta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenUsageEvent {
    /// Stable idempotency key for this accounting delta.
    pub(crate) id: String,
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
    #[cfg(test)]
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
        Ok(())
    }

    /// Appends one non-zero event, ignoring a previously recorded stable id.
    pub(crate) fn append(&self, event: &TokenUsageEvent) -> Result<bool> {
        if event.id.trim().is_empty() {
            return Err(MezError::invalid_args(
                "token usage event id must not be empty",
            ));
        }
        if event.usage.is_zero() {
            return Ok(false);
        }
        let connection = self.open()?;
        let changed = connection.execute(
            "INSERT OR IGNORE INTO token_usage_events (
                 id, observed_at, provider, model, input_tokens, output_tokens,
                 reasoning_tokens, cached_input_tokens, cache_write_input_tokens
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
            ],
        )?;
        Ok(changed == 1)
    }

    /// Aggregates all requested exact rolling windows with one indexed scan.
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
             WHERE observed_at >= ?1 AND observed_at <= ?2
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
    pub(crate) fn oldest_observed_at(&self, now_unix_seconds: u64) -> Result<Option<u64>> {
        let connection = self.open()?;
        let oldest = connection.query_row(
            "SELECT MIN(observed_at) FROM token_usage_events WHERE observed_at <= ?1",
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

    fn open(&self) -> Result<Connection> {
        ensure_private_parent(&self.path)?;
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(Duration::from_millis(250))?;
        initialize_schema(&connection)?;
        set_private_file_permissions(&self.path)?;
        Ok(connection)
    }
}

fn initialize_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch("PRAGMA journal_mode = WAL;")?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    match version {
        0 => {
            connection.execute_batch(
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
        SCHEMA_VERSION => {}
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
    Ok(())
}

fn row_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| conversion_error(index, "negative token usage value"))
}

fn row_optional_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
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
