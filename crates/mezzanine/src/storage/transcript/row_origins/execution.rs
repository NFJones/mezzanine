//! Original execution framing in the committed append accounting projection.
//!
//! Once source escrow retires, late lifecycle controls still need the original
//! group/ordinal/producer association. Version five records only this bounded
//! framing with the full original row fingerprint, never payload bytes, model
//! membership, execution completion or an additional append journal. Lookups
//! require a separately verified archive cut and the original indexed placement.

use super::{AgentTranscriptStore, images::IndexedRowImage};
use crate::{MezError, Result};
use mez_agent::{
    ContextBlockOccurrenceId, ContextExecutionGroupId, TranscriptEntry, TranscriptRole,
    transcript::{TranscriptContextEvent, TranscriptContextRecord, TranscriptExecutionStateRecord},
};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Descriptive source framing, retained without any source payload or live fence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::storage::transcript) struct IndexedExecutionSource {
    pub(in crate::storage::transcript) image: IndexedRowImage,
    pub(in crate::storage::transcript) group: String,
    pub(in crate::storage::transcript) ordinal: u64,
    pub(in crate::storage::transcript) turn_id: String,
    pub(in crate::storage::transcript) agent_id: String,
    pub(in crate::storage::transcript) pane_id: String,
}

impl IndexedExecutionSource {
    fn validate(&self) -> Result<()> {
        ContextExecutionGroupId::new(self.group.clone())?;
        if self.ordinal == 0
            || self.image.sequence == 0
            || [&self.turn_id, &self.agent_id, &self.pane_id]
                .into_iter()
                .any(|value| {
                    value.is_empty()
                        || value.trim() != value
                        || value.len() > 4096
                        || value.chars().any(char::is_control)
                })
        {
            return Err(invalid("invalid original execution framing"));
        }
        Ok(())
    }
}

impl AgentTranscriptStore {
    /// Adds original execution descriptors in the same transaction that has
    /// just published these exact rows' placement, costs and full fingerprints.
    /// Legacy images acquire no guessed descriptor during ordinary lookup.
    pub(super) fn publish_execution_sources_in_transaction(
        &self,
        transaction: &Transaction<'_>,
        rows: &[&TranscriptEntry],
    ) -> Result<()> {
        let mut sources = Vec::new();
        for row in rows {
            if row.role != TranscriptRole::System {
                continue;
            }
            let Some(record) = TranscriptContextRecord::from_transcript_content(&row.content)?
            else {
                continue;
            };
            let TranscriptContextEvent::ExecutionBlock {
                execution_group_id: Some(group),
                ordinal: Some(ordinal),
                ..
            } = record.event()
            else {
                continue;
            };
            let source = IndexedExecutionSource {
                image: IndexedRowImage {
                    sequence: row.sequence,
                    origin: record.record().occurrence_id(),
                    digest: self.transcript_entry_revision(row)?,
                },
                group: group.as_str().into(),
                ordinal: *ordinal,
                turn_id: row.turn_id.clone(),
                agent_id: row.agent_id.clone(),
                pane_id: row.pane_id.clone(),
            };
            source.validate()?;
            sources.push(source);
        }
        if sources.is_empty() {
            return Ok(());
        }
        let version: u32 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == 4 {
            transaction.execute_batch(
                "CREATE TABLE execution_sources (origin TEXT PRIMARY KEY, sequence TEXT NOT NULL, payload TEXT NOT NULL, sha256 TEXT NOT NULL);
                 CREATE INDEX execution_sources_sequence ON execution_sources(sequence);
                 PRAGMA user_version=5;",
            )?;
        } else if version != 5 {
            return Err(invalid("original execution requires indexed accounting"));
        }
        for source in sources {
            if super::images::indexed_row(transaction, source.image.sequence)?
                != Some((source.image.origin, source.image.digest.clone()))
            {
                return Err(invalid("original execution fingerprint changed"));
            }
            let encoded = serde_json::to_string(&source)
                .map_err(|_| invalid("original execution encode failed"))?;
            let digest = descriptor_digest(&encoded);
            let previous: Option<(String, String, String)> = transaction
                .query_row(
                    "SELECT sequence, payload, sha256 FROM execution_sources WHERE origin=?1",
                    [String::from(source.image.origin)],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            if previous.is_some_and(|old| {
                old != (
                    source.image.sequence.to_string(),
                    encoded.clone(),
                    digest.clone(),
                )
            }) {
                return Err(invalid("original execution descriptor changed"));
            }
            transaction.execute(
                "INSERT INTO execution_sources VALUES (?1, ?2, ?3, ?4) ON CONFLICT(origin) DO NOTHING",
                params![String::from(source.image.origin), source.image.sequence.to_string(), encoded, digest],
            )?;
        }
        Ok(())
    }

    /// Looks up only members referenced by controls in this frozen row window.
    /// The caller owns the conversation lease and has checked `through` against
    /// the actual manifest-bound archive cut. Missing legacy descriptors provide
    /// no proof; malformed, conflicting or missing required images refuse.
    pub(in crate::storage::transcript) fn indexed_execution_sources_locked(
        &self,
        conversation: &str,
        rows: &[TranscriptEntry],
        through: u64,
    ) -> Result<BTreeMap<ContextBlockOccurrenceId, IndexedExecutionSource>> {
        let mut requested = std::collections::BTreeSet::new();
        for row in rows {
            if row.conversation_id != conversation {
                return Err(invalid("foreign execution control window"));
            }
            if row.role == TranscriptRole::System
                && let Some(control) =
                    TranscriptExecutionStateRecord::from_transcript_content(&row.content)?
            {
                requested.extend(control.state().members().iter().map(|(id, _)| *id));
            }
        }
        if requested.is_empty() || through == 0 {
            return Ok(BTreeMap::new());
        }
        let connection = self.open_row_origins_locked(conversation)?;
        let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version < 5 {
            return Ok(BTreeMap::new());
        }
        let mut found = BTreeMap::new();
        for id in requested {
            let value: Option<(String, String, String)> = connection
                .query_row(
                    "SELECT sequence, payload, sha256 FROM execution_sources WHERE origin=?1",
                    [String::from(id)],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((sequence, encoded, digest)) = value else {
                continue;
            };
            if encoded.len() > 32 * 1024 || descriptor_digest(&encoded) != digest {
                return Err(invalid("original execution descriptor integrity failed"));
            }
            let source: IndexedExecutionSource = serde_json::from_str(&encoded)
                .map_err(|_| invalid("invalid original execution descriptor"))?;
            source.validate()?;
            if source.image.origin != id
                || source.image.sequence.to_string() != sequence
                || super::images::indexed_row(&connection, source.image.sequence)?
                    != Some((id, source.image.digest.clone()))
            {
                return Err(invalid("original execution placement or image changed"));
            }
            let duplicate: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM origins WHERE origin=?1 AND sequence<>?2 UNION SELECT 1 FROM row_digests WHERE origin=?1 AND sequence<>?2)",
                params![String::from(id), sequence],
                |row| row.get(0),
            )?;
            if duplicate {
                return Err(invalid("original execution has multiple placements"));
            }
            if source.image.sequence <= through {
                found.insert(id, source);
            }
        }
        Ok(found)
    }
}

fn invalid(reason: &str) -> MezError {
    MezError::invalid_state(format!("transcript execution source: {reason}"))
}

fn descriptor_digest(encoded: &str) -> String {
    Sha256::digest(encoded.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
