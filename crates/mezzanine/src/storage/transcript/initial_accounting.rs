//! Immutable inherited raw accounting and its initial append publication.
//!
//! New child sequences do not create new occurrences. This payload travels with
//! the original exact append receipt; its namespace, complete row placement and
//! descriptive costs commit atomically before initial readiness. The immutable
//! marker distinguishes that proposal from equal bytes with different origins.
//! Later cache revisions remain descriptive and never grant replay authority.

use super::{AgentTranscriptStore, TranscriptRowOrigins, TranscriptRowRecords};
use crate::{MezError, Result};
use mez_agent::{
    ContextBlockOccurrenceId, TranscriptEntry, TranscriptRole, transcript::TranscriptContextRecord,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// Exact versioned accounting source owned by one initial child append.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InitialTranscriptRowAccounting {
    version: u32,
    namespace: ContextBlockOccurrenceId,
    origins: TranscriptRowOrigins,
    records: TranscriptRowRecords,
}

impl InitialTranscriptRowAccounting {
    /// Allocates the future-row namespace once at the logical batch producer.
    /// Immutable clones/retries retain it; inherited rows keep their old IDs.
    pub(crate) fn new(origins: TranscriptRowOrigins, records: TranscriptRowRecords) -> Self {
        Self {
            version: 1,
            namespace: ContextBlockOccurrenceId::allocate(),
            origins,
            records,
        }
    }

    /// Borrows original placements indexed by the child's actual row positions.
    pub(crate) fn origins(&self) -> &TranscriptRowOrigins {
        &self.origins
    }

    /// Borrows descriptive cost proposals; actual rendering validates reuse.
    pub(crate) fn records(&self) -> &TranscriptRowRecords {
        &self.records
    }

    /// Retains costs for the actual child rendering without allocating another
    /// proposal/namespace. Changed pane framing revises estimates, never IDs.
    pub(crate) fn with_rendered_records(
        mut self,
        records: Vec<(u64, mez_agent::ContextBlockRecord)>,
    ) -> Result<Self> {
        self.records.extend(records);
        self.validate_format()?;
        Ok(self)
    }

    /// Validates framing independently of current replay membership or rows.
    fn validate_format(&self) -> Result<()> {
        if self.version != 1 || self.origins.keys().any(|sequence| *sequence == 0) {
            return Err(MezError::invalid_state(
                "unsupported initial row accounting",
            ));
        }
        for (sequence, record) in &self.records {
            record.validate_format()?;
            if self.origins.get(sequence) != Some(&record.occurrence_id()) {
                return Err(MezError::conflict("initial row cost occurrence mismatch"));
            }
        }
        Ok(())
    }

    /// Requires complete structural placement and the original authored IDs.
    /// Neither record costs nor matching text can replace row/receipt identity.
    pub(crate) fn validate(&self, rows: &[TranscriptEntry]) -> Result<()> {
        self.validate_format()?;
        if self.origins.len() != rows.len() || rows.first().is_some_and(|row| row.sequence != 1) {
            return Err(MezError::invalid_args(
                "initial row accounting requires complete initial rows",
            ));
        }
        for row in rows {
            let origin = self.origins.get(&row.sequence).ok_or_else(|| {
                MezError::invalid_state("initial transcript row origin is missing")
            })?;
            if row.role == TranscriptRole::System
                && let Some(record) =
                    TranscriptContextRecord::from_transcript_content(&row.content)?
                && record.record().occurrence_id() != *origin
            {
                return Err(MezError::conflict(
                    "initial authored row occurrence changed",
                ));
            }
        }
        Ok(())
    }
}

impl AgentTranscriptStore {
    /// Checks nonrepairable initial conflicts before any admission journal or
    /// global archive replay. Matching marker bytes do not justify replacing
    /// already-published placement. Missing costs and an incomplete required
    /// marker remain recoverable by their exact original receipt. Returns true
    /// only when that immutable proposal is already published. No schema,
    /// metadata marker, placement or cost writes occur in this preflight.
    pub(super) fn preflight_initial_row_accounting_locked(
        &self,
        conversation: &str,
        expected: Option<&InitialTranscriptRowAccounting>,
    ) -> Result<bool> {
        let current = self.initial_row_accounting_locked(conversation)?;
        if current
            .as_ref()
            .is_some_and(|current| Some(current) != expected)
        {
            return Err(MezError::conflict(
                "initial transcript row owner differs from existing authority",
            ));
        }
        let Some(expected) = expected else {
            return Ok(false);
        };
        if !self
            .root
            .join(conversation)
            .join("row-origins.sqlite3")
            .exists()
        {
            return Ok(false);
        }
        let connection = self.open_row_origins_locked(conversation)?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let (costs, inheritance): (bool, bool) = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='row_costs'),
                    EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='initial_inheritance')",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if (version == 1 && costs) || (version < 3 && inheritance) || (version >= 2 && !costs) {
            return Err(MezError::invalid_state(
                "initial row accounting schema conflicts with its version",
            ));
        }
        drop(connection.prepare("SELECT sequence, origin FROM origins LIMIT 0")?);
        if current.is_some() {
            let mut select =
                connection.prepare_cached("SELECT origin FROM origins WHERE sequence=?1")?;
            for (sequence, origin) in &expected.origins {
                let actual: Option<String> = select
                    .query_row([sequence.to_string()], |row| row.get(0))
                    .optional()?;
                if actual != Some(String::from(*origin)) {
                    return Err(MezError::conflict(
                        "committed initial row placement changed",
                    ));
                }
            }
        }
        if version >= 2 {
            drop(connection.prepare("SELECT origin, record FROM row_costs LIMIT 0")?);
            drop(connection);
            // Known formats validate without migration or publication. An absent
            // cache entry is repairable; malformed/foreign/future entries are not.
            self.transcript_row_records_locked(conversation, &expected.origins)?;
        }
        Ok(current.is_some())
    }

    /// Reads an existing immutable marker without creating/migrating anything
    /// during admission preflight. Older projection formats have no such owner.
    pub(super) fn initial_row_accounting_locked(
        &self,
        conversation: &str,
    ) -> Result<Option<InitialTranscriptRowAccounting>> {
        if !self
            .root
            .join(conversation)
            .join("row-origins.sqlite3")
            .exists()
        {
            return Ok(None);
        }
        let connection = self.open_row_origins_locked(conversation)?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < 3 {
            return Ok(None);
        }
        let encoded: Option<String> = connection
            .query_row(
                "SELECT payload FROM initial_inheritance WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        encoded
            .map(|encoded| {
                let payload: InitialTranscriptRowAccounting = serde_json::from_str(&encoded)
                    .map_err(|_| {
                        MezError::invalid_state("invalid initial row accounting marker")
                    })?;
                payload.validate_format()?;
                Ok(payload)
            })
            .transpose()
    }

    /// Installs only the exact receipt-owned initial proposal. Mapping, future
    /// namespace, missing initial costs and immutable marker share one SQLite
    /// transaction. Retrying never overwrites a valid later rendering cache.
    pub(super) fn publish_initial_row_accounting_locked(
        &self,
        conversation: &str,
        rows: &[TranscriptEntry],
        payload: &InitialTranscriptRowAccounting,
    ) -> Result<()> {
        payload.validate(rows)?;
        let existing = self.initial_row_accounting_locked(conversation)?;
        if existing.as_ref().is_some_and(|current| current != payload) {
            return Err(MezError::conflict("initial transcript row owner changed"));
        }
        if existing.is_some()
            && self.transcript_row_origins_locked(conversation, rows)? != payload.origins
        {
            return Err(MezError::conflict(
                "committed initial row placement changed",
            ));
        }
        // Accounting-only migration establishes the cost table before schema3.
        let costs = self.transcript_row_records_locked(conversation, &payload.origins)?;
        let mut connection = self.open_row_origins_locked(conversation)?;
        let transaction = connection.transaction()?;
        let version: u32 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < 3 {
            if version == 1 {
                transaction.execute(
                    "CREATE TABLE row_costs (origin TEXT PRIMARY KEY, record TEXT NOT NULL)",
                    [],
                )?;
            }
            transaction.execute_batch(
                "CREATE TABLE initial_inheritance (singleton INTEGER PRIMARY KEY CHECK(singleton=1), payload TEXT NOT NULL);
                 PRAGMA user_version=3;",
            )?;
        }
        if existing.is_none() {
            transaction.execute("DELETE FROM origins", [])?;
            for (sequence, origin) in &payload.origins {
                transaction.execute(
                    "INSERT INTO origins VALUES (?1, ?2)",
                    params![sequence.to_string(), String::from(*origin)],
                )?;
            }
            transaction.execute(
                "UPDATE origin_meta SET namespace=?1 WHERE singleton=1",
                [String::from(payload.namespace)],
            )?;
            let encoded = serde_json::to_string(payload)
                .map_err(|_| MezError::invalid_state("initial row accounting encode failed"))?;
            transaction.execute("INSERT INTO initial_inheritance VALUES (1, ?1)", [encoded])?;
        }
        for (sequence, record) in &payload.records {
            if !costs.contains_key(sequence) {
                let encoded = serde_json::to_string(record)
                    .map_err(|_| MezError::invalid_state("initial row cost encode failed"))?;
                transaction.execute(
                    "INSERT INTO row_costs VALUES (?1, ?2) ON CONFLICT(origin) DO NOTHING",
                    params![String::from(record.occurrence_id()), encoded],
                )?;
            }
        }
        transaction.commit()?;
        self.require_row_origins_locked(conversation)
    }

    /// Requires the original complete identity marker and durable accounting
    /// before initial receipt retirement. Costs may have a newer valid rendering
    /// revision; their original occurrence must still be present and formatted.
    pub(super) fn verify_initial_row_accounting_locked(
        &self,
        conversation: &str,
        rows: &[TranscriptEntry],
        payload: &InitialTranscriptRowAccounting,
    ) -> Result<()> {
        if self.initial_row_accounting_locked(conversation)?.as_ref() != Some(payload)
            || !self.row_origins_required_locked(conversation)?
            || self.transcript_row_origins_locked(conversation, rows)? != payload.origins
        {
            return Err(MezError::invalid_state(
                "initial row accounting is not fully committed",
            ));
        }
        let records = self.transcript_row_records_locked(conversation, &payload.origins)?;
        if payload
            .records
            .keys()
            .any(|sequence| !records.contains_key(sequence))
        {
            return Err(MezError::invalid_state(
                "initial row costs are not fully committed",
            ));
        }
        Ok(())
    }
}
