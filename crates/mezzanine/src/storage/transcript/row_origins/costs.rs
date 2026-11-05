//! Owned descriptive costs for original raw-row occurrences.
//!
//! Version two adds costs to the existing private placement database. Origins
//! remain authoritative placement; costs must be restored against actual block
//! rendering before reuse. Pure projection runs under the original read lease,
//! then only changed committed-row costs are written. Pending rows never create
//! durable cache entries, and no source archive is reread or rewritten here.

use super::{AgentTranscriptStore, TranscriptRowOrigins};
use crate::{MezError, Result};
use mez_agent::ContextBlockRecord;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeMap, BTreeSet};

/// Descriptive costs associated with exact row positions in a frozen snapshot.
pub(crate) type TranscriptRowRecords = BTreeMap<u64, ContextBlockRecord>;

impl AgentTranscriptStore {
    /// Migrates the descriptive schema only for an actual accounting operation.
    /// Ordinary lock/recovery validation accepts v1 without changing its bytes.
    fn open_row_costs_locked(&self, conversation: &str) -> Result<Connection> {
        let mut connection = self.open_row_origins_locked(conversation)?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == 1 {
            let transaction = connection.transaction()?;
            transaction.execute_batch(
                "CREATE TABLE row_costs (origin TEXT PRIMARY KEY, record TEXT NOT NULL);
                 PRAGMA user_version=2;",
            )?;
            transaction.commit()?;
        }
        Ok(connection)
    }

    /// Loads only selected occurrence records. This validates format and origin,
    /// never trusts authored costs or treats a cache as semantic/source authority.
    pub(in crate::storage::transcript) fn transcript_row_records_locked(
        &self,
        conversation: &str,
        origins: &TranscriptRowOrigins,
    ) -> Result<TranscriptRowRecords> {
        if origins.is_empty() {
            return Ok(BTreeMap::new());
        }
        let connection = self.open_row_costs_locked(conversation)?;
        let mut select =
            connection.prepare_cached("SELECT record FROM row_costs WHERE origin=?1")?;
        let mut records = BTreeMap::new();
        for (sequence, origin) in origins {
            let encoded: Option<String> = select
                .query_row([String::from(*origin)], |row| row.get(0))
                .optional()?;
            if let Some(encoded) = encoded {
                let record: ContextBlockRecord = serde_json::from_str(&encoded)
                    .map_err(|_| MezError::invalid_state("invalid transcript row cost record"))?;
                record.validate_format()?;
                if record.occurrence_id() != *origin {
                    return Err(MezError::conflict("transcript row cost origin mismatch"));
                }
                records.insert(*sequence, record);
            }
        }
        Ok(records)
    }

    /// Saves a successful pure renderer's validated records before releasing
    /// the same source lease. Unchanged records do no SQL writes; only actual
    /// committed membership can justify durable costs, never a pending slot.
    pub(in crate::storage::transcript) fn save_rendered_row_records_locked(
        &self,
        conversation: &str,
        snapshot: &super::super::replay_position::ConversationReplaySnapshot,
        updates: &[(u64, ContextBlockRecord)],
    ) -> Result<()> {
        let committed = snapshot
            .view
            .committed
            .iter()
            .map(|row| row.sequence)
            .collect::<BTreeSet<_>>();
        let mut changed = Vec::new();
        let mut seen = BTreeSet::new();
        for (sequence, record) in updates {
            record.validate_format()?;
            if !seen.insert(*sequence)
                || snapshot.row_origins.get(sequence) != Some(&record.occurrence_id())
            {
                return Err(MezError::conflict(
                    "rendered transcript row cost owner mismatch",
                ));
            }
            if committed.contains(sequence) && snapshot.row_records.get(sequence) != Some(record) {
                changed.push(*record);
            }
        }
        if changed.is_empty() {
            return Ok(());
        }
        let mut connection = self.open_row_costs_locked(conversation)?;
        let transaction = connection.transaction()?;
        for record in changed {
            let encoded = serde_json::to_string(&record)
                .map_err(|_| MezError::invalid_state("transcript row cost encoding failed"))?;
            transaction.execute(
                "INSERT INTO row_costs VALUES (?1, ?2)
                 ON CONFLICT(origin) DO UPDATE SET record=excluded.record WHERE row_costs.record<>excluded.record",
                params![String::from(record.occurrence_id()), encoded],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Copies existing original caches with saved-fork placement. Each copied
    /// record is still revalidated against the child's actual rendering on use.
    pub(in crate::storage::transcript) fn copy_fork_row_records_locked(
        &self,
        conversation: &str,
        records: &TranscriptRowRecords,
    ) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let mut connection = self.open_row_costs_locked(conversation)?;
        let transaction = connection.transaction()?;
        for record in records.values() {
            record.validate_format()?;
            let encoded = serde_json::to_string(record)
                .map_err(|_| MezError::invalid_state("transcript fork row cost encoding failed"))?;
            transaction.execute(
                "INSERT OR REPLACE INTO row_costs VALUES (?1, ?2)",
                params![String::from(record.occurrence_id()), encoded],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}
