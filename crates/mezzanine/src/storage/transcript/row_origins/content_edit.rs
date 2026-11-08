//! Exact content edits preserve placement and descriptive source metadata.
//!
//! The enclosing raw-edit intent owns archive/manifest recovery. This projection
//! accepts only the original or already revised row image, updates existing
//! execution descriptors without inventing any, and recomputes selected costs.

use super::*;
use crate::storage::transcript::row_origins::images::IndexedRowImage;
use mez_agent::ContextBlockRecord;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::storage::transcript) struct ContentRowRevision {
    pub(in crate::storage::transcript) sequence: u64,
    pub(in crate::storage::transcript) origin: ContextBlockOccurrenceId,
    pub(in crate::storage::transcript) before: Option<IndexedRowImage>,
    pub(in crate::storage::transcript) after_digest: String,
    pub(in crate::storage::transcript) record: Option<ContextBlockRecord>,
}

impl ContentRowRevision {
    pub(in crate::storage::transcript) fn validate(&self) -> Result<()> {
        let valid_digest = |digest: &str| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if self.sequence == 0
            || !valid_digest(&self.after_digest)
            || self.before.as_ref().is_some_and(|image| {
                image.sequence != self.sequence
                    || image.origin != self.origin
                    || !valid_digest(&image.digest)
            })
        {
            return Err(MezError::invalid_state(
                "invalid original content edit row projection",
            ));
        }
        if let Some(record) = self.record {
            record.validate_format()?;
            if record.occurrence_id() != self.origin {
                return Err(MezError::invalid_state(
                    "content edit cost occurrence changed",
                ));
            }
        }
        Ok(())
    }
    pub(in crate::storage::transcript) fn after(&self) -> Option<IndexedRowImage> {
        self.before.as_ref().map(|_| IndexedRowImage {
            sequence: self.sequence,
            origin: self.origin,
            digest: self.after_digest.clone(),
        })
    }
}

impl AgentTranscriptStore {
    /// Fails before source writes when an existing row/descriptor differs from
    /// preparation. This cannot create or repair missing execution evidence.
    pub(in crate::storage::transcript) fn validate_content_row_revision_locked(
        &self,
        conversation: &str,
        edit: &ContentRowRevision,
    ) -> Result<()> {
        edit.validate()?;
        if self.indexed_row_reference_locked(conversation, edit.sequence)? != edit.before {
            return Err(MezError::conflict(
                "content edit original row image changed before staging",
            ));
        }
        let connection = self.open_row_origins_locked(conversation)?;
        self.validate_execution_content_revision(&connection, edit)
    }

    /// Applies one exact recovery projection, never a new append/closure claim.
    pub(in crate::storage::transcript) fn apply_content_row_revision_locked(
        &self,
        conversation: &str,
        edit: &ContentRowRevision,
    ) -> Result<()> {
        edit.validate()?;
        let mut connection = self.open_row_origins_locked(conversation)?;
        let transaction = connection.transaction()?;
        let version: u32 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let current = if version >= 4 {
            images::indexed_row(&transaction, edit.sequence)?.map(|(origin, digest)| {
                IndexedRowImage {
                    sequence: edit.sequence,
                    origin,
                    digest,
                }
            })
        } else {
            None
        };
        if current != edit.before && current != edit.after() {
            return Err(MezError::conflict(
                "content edit original row image changed",
            ));
        }
        let origin: Option<String> = transaction
            .query_row(
                "SELECT origin FROM origins WHERE sequence=?1",
                [edit.sequence.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if origin.is_some_and(|origin| origin != String::from(edit.origin)) {
            return Err(MezError::conflict(
                "content edit original row placement changed",
            ));
        }
        transaction.execute(
            "INSERT INTO origins VALUES (?1, ?2) ON CONFLICT(sequence) DO NOTHING",
            params![edit.sequence.to_string(), String::from(edit.origin)],
        )?;
        if let Some(after) = edit.after() {
            transaction.execute(
                "UPDATE row_digests SET digest=?1 WHERE sequence=?2 AND origin=?3",
                params![
                    after.digest,
                    edit.sequence.to_string(),
                    String::from(edit.origin)
                ],
            )?;
        }
        self.revise_execution_image_in_transaction(&transaction, edit)?;
        if version >= 2 {
            if let Some(record) = edit.record {
                record.validate_format()?;
                if record.occurrence_id() != edit.origin {
                    return Err(MezError::conflict("content edit cost occurrence changed"));
                }
                let encoded = serde_json::to_string(&record)
                    .map_err(|_| MezError::invalid_state("content edit cost encoding failed"))?;
                transaction.execute(
                    "INSERT INTO row_costs VALUES (?1, ?2) ON CONFLICT(origin) DO UPDATE SET record=excluded.record",
                    params![String::from(edit.origin), encoded],
                )?;
            } else {
                transaction.execute(
                    "DELETE FROM row_costs WHERE origin=?1",
                    [String::from(edit.origin)],
                )?;
            }
        }
        transaction.commit()?;
        self.require_row_origins_locked(conversation)
    }
}
