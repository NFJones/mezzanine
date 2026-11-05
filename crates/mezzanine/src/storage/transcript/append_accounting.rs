//! Original occurrence proposals owned by ordinary append batches.
//!
//! A pending row is not an archive slot: cancelling one append and admitting a
//! new owner at the same sequence must not reuse its occurrence. The producer
//! allocates unaccounted row identities once; immutable clones, journal retries
//! and eventual raw publication carry the same complete proposal. These records
//! are descriptive and confer no execution, source or settlement authority.

use super::{TranscriptRowOrigins, TranscriptRowRecords};
use crate::{MezError, Result};
use mez_agent::transcript::TranscriptContextRecord;
use mez_agent::{ContextBlockOccurrenceId, TranscriptEntry, TranscriptRole};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Versioned identity/cost proposal for one exact ordinary raw append span.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TranscriptAppendAccounting {
    version: u32,
    origins: TranscriptRowOrigins,
    records: TranscriptRowRecords,
}

impl TranscriptAppendAccounting {
    /// Captures actual authored IDs and allocates each remaining occurrence
    /// once at its original producer, never from text or a reusable row slot.
    pub(crate) fn new(rows: &[TranscriptEntry]) -> Result<Self> {
        let mut origins = BTreeMap::new();
        let mut records = BTreeMap::new();
        for row in rows {
            let authored = if row.role == TranscriptRole::System {
                TranscriptContextRecord::from_transcript_content(&row.content)?
            } else {
                None
            };
            let origin = if let Some(authored) = authored {
                let record = authored.record();
                records.insert(row.sequence, record);
                record.occurrence_id()
            } else {
                ContextBlockOccurrenceId::allocate()
            };
            if origins.insert(row.sequence, origin).is_some() {
                return Err(MezError::invalid_args(
                    "append accounting repeats a row sequence",
                ));
            }
        }
        let result = Self {
            version: 1,
            origins,
            records,
        };
        result.validate(rows)?;
        Ok(result)
    }

    /// Borrows original occurrences indexed by this append's exact row span.
    pub(crate) fn origins(&self) -> &TranscriptRowOrigins {
        &self.origins
    }

    /// Borrows available original estimates; actual rendering validates reuse.
    pub(crate) fn records(&self) -> &TranscriptRowRecords {
        &self.records
    }

    /// Requires complete unique placement and actual authored identity. Costs
    /// remain descriptive; unsupported formats and foreign associations fail.
    pub(crate) fn validate(&self, rows: &[TranscriptEntry]) -> Result<()> {
        if self.version != 1
            || rows.is_empty()
            || self.origins.len() != rows.len()
            || self.origins.keys().any(|sequence| *sequence == 0)
            || self.origins.values().collect::<BTreeSet<_>>().len() != self.origins.len()
        {
            return Err(MezError::invalid_state(
                "invalid ordinary append accounting shape",
            ));
        }
        for row in rows {
            let origin = self
                .origins
                .get(&row.sequence)
                .ok_or_else(|| MezError::invalid_state("ordinary append occurrence is missing"))?;
            if row.role == TranscriptRole::System
                && let Some(authored) =
                    TranscriptContextRecord::from_transcript_content(&row.content)?
                && authored.record().occurrence_id() != *origin
            {
                return Err(MezError::conflict(
                    "ordinary append authored occurrence changed",
                ));
            }
        }
        for (sequence, record) in &self.records {
            record.validate_format()?;
            if self.origins.get(sequence) != Some(&record.occurrence_id()) {
                return Err(MezError::conflict(
                    "ordinary append cost occurrence mismatch",
                ));
            }
        }
        Ok(())
    }
}
