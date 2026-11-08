//! Summary identities published atomically with their authoritative epoch.
//!
//! Prefix and selective records occupy explicit structural slots, never text
//! matches or covered-row identities. Legacy sidecars receive fresh origins at
//! one persisted import boundary. An exact old-projection witness qualifies
//! genuine pre-import receipts without treating a later equal-text publication
//! as the same occurrence. Cached estimates remain descriptive and are restored
//! against actual summary rendering before adoption; they grant no authority.

use mez_agent::{ContextBlock, ContextBlockRecord, ContextSourceKind};
use serde::{Deserialize, Serialize};

use super::AgentCompactionEpoch;
use crate::{MezError, Result};

mod persistence;
mod projection;
pub(super) use persistence::replace_summary_record_epoch;

/// Versioned sidecar's structurally aligned summary records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactionSummaryRecords {
    /// Present exactly when the epoch owns a nonempty prefix summary.
    pub(crate) prefix: Option<ContextBlockRecord>,
    /// One original record per selective range, in the same order.
    pub(crate) ranges: Vec<ContextBlockRecord>,
    /// Explicit original legacy authority, absent for new logical publications.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_import: Option<LegacyEpochImport>,
}

/// A producer's exact rendered summary and original occurrence/cost record.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct CompactionSummary<'a> {
    /// Exact prefix framing or selective body being proposed.
    pub(crate) content: &'a str,
    /// Retained original proposal record, never allocated by projection.
    pub(crate) record: ContextBlockRecord,
}

/// Exact descriptive migration source, never an equal-text publication alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyEpochImport {
    version: u64,
    projection_sha256: String,
}

impl CompactionSummaryRecords {
    /// Captures actual producer-owned records without allocating or inferring
    /// identity. Epoch restoration validates their exact structural rendering.
    pub(crate) fn new(prefix: Option<ContextBlockRecord>, ranges: Vec<ContextBlockRecord>) -> Self {
        Self {
            prefix,
            ranges,
            legacy_import: None,
        }
    }
}

/// Renders the same owned prefix block used by ordinary replay.
pub(crate) fn prefix_summary_block(conversation_id: &str, summary: &str) -> ContextBlock {
    let memory_id = mez_agent::memory::canonical_memory_uuid(&format!("compact-{conversation_id}"));
    ContextBlock::reference_event(
        ContextSourceKind::CompactionPrefix,
        format!("memory {memory_id} (conversation)"),
        summary,
    )
}

/// Renders a selective summary at its structural range slot.
pub(crate) fn selective_summary_block(summary: &str) -> ContextBlock {
    ContextBlock::reference_event(
        ContextSourceKind::CompactionSelective,
        "context compaction summary",
        summary,
    )
}

impl AgentCompactionEpoch {
    /// Keeps selected structural range slots and their original records aligned.
    /// A clipped source no longer represents the entire imported legacy epoch.
    pub(crate) fn retain_summary_ranges(
        &mut self,
        mut keep: impl FnMut(&super::AgentCompactionRange) -> bool,
    ) -> Result<()> {
        if self
            .summary_records
            .as_ref()
            .is_some_and(|records| records.ranges.len() != self.ranges.len())
            || self.version == 3 && self.summary_records.is_none()
        {
            return Err(invalid("compaction range record slots are unavailable"));
        }
        let selected = self.ranges.iter().map(&mut keep).collect::<Vec<_>>();
        let mut index = 0;
        self.ranges.retain(|_| {
            let keep = selected[index];
            index += 1;
            keep
        });
        if let Some(records) = &mut self.summary_records {
            let mut index = 0;
            records.ranges.retain(|_| {
                let keep = selected[index];
                index += 1;
                keep
            });
            if selected.iter().any(|keep| !keep) {
                records.legacy_import = None;
            }
        }
        Ok(())
    }

    /// Preserves the original encoded semantic-summary budget independently of
    /// v3's additional structural accounting space, without cloning source text.
    pub(super) fn validate_summary_payload_size(&self) -> Result<()> {
        projection::validate_summary_payload_size(self)
    }

    /// A proof for new ranges preserves original retained records and prefix
    /// authority. Both epochs have already checked actual record renderings.
    pub(super) fn validate_retained_summary_records(&self, previous: &Self) -> Result<()> {
        let records = self
            .summary_records
            .as_ref()
            .ok_or_else(|| invalid("compaction summary records are missing"))?;
        let prior = previous
            .summary_records
            .as_ref()
            .ok_or_else(|| invalid("prior compaction summary records are missing"))?;
        if self.through_sequence != previous.through_sequence
            || self.summary != previous.summary
            || records.prefix != prior.prefix
        {
            return Err(invalid(
                "range publication changed retained prefix authority",
            ));
        }
        for (index, range) in previous.ranges.iter().enumerate() {
            let target = self
                .ranges
                .binary_search_by_key(&range.first_sequence, |range| range.first_sequence)
                .map_err(|_| invalid("range publication lost a prior summary slot"))?;
            if records.ranges.get(target) != prior.ranges.get(index) {
                return Err(invalid(
                    "range publication changed retained summary occurrence",
                ));
            }
        }
        Ok(())
    }
    /// Retargets an inherited summary without changing any occurrence IDs.
    /// Only changed prefix rendering is recounted; legacy import qualification
    /// belongs to its original conversation and cannot travel as new authority.
    pub(crate) fn retarget_summary_records(&mut self, conversation_id: &str) -> Result<()> {
        self.conversation_id = conversation_id.to_string();
        if let Some(records) = &mut self.summary_records {
            records.legacy_import = None;
            self.restore_summary_records()?;
        }
        Ok(())
    }
    /// Creates records for a new logical producer, independently of equal text.
    /// Call at the proposal boundary and carry the returned epoch through retry;
    /// ordinary replay must never allocate replacements for these records.
    pub(crate) fn allocate_summary_records(&mut self) -> Result<()> {
        self.summary_records = Some(CompactionSummaryRecords {
            prefix: if self.summary.trim().is_empty() {
                None
            } else {
                Some(ContextBlockRecord::allocate(&prefix_summary_block(
                    &self.conversation_id,
                    &self.summary,
                ))?)
            },
            ranges: self
                .ranges
                .iter()
                .map(|range| ContextBlockRecord::allocate(&selective_summary_block(&range.summary)))
                .collect::<mez_agent::AgentContextResult<Vec<_>>>()?,
            legacy_import: None,
        });
        self.version = 3;
        Ok(())
    }

    /// Imports a genuine v1/v2 persisted authority once. The caller owns the
    /// mutation lease and must persist the whole resulting v3 epoch atomically
    /// before returning it or acknowledging an old receipt.
    pub(super) fn import_legacy_summary_records(&mut self) -> Result<()> {
        if !matches!(self.version, 1 | 2) || self.summary_records.is_some() {
            return Err(invalid("invalid legacy compaction accounting boundary"));
        }
        let witness = LegacyEpochImport {
            version: self.version,
            projection_sha256: self.legacy_projection_digest(self.version)?,
        };
        self.allocate_summary_records()?;
        if let Some(records) = &mut self.summary_records {
            records.legacy_import = Some(witness);
        }
        Ok(())
    }

    /// Restores untrusted costs without changing occurrence identity. Invalid
    /// slots, duplicate origins, unknown formats or altered legacy witnesses
    /// fail; stale costs/methods/renderings are recomputed from actual blocks.
    /// Returns whether descriptive cache metadata needs atomic replacement.
    pub(super) fn restore_summary_records(&mut self) -> Result<bool> {
        if self.version != 3 {
            return Err(invalid("unsupported accounted compaction epoch version"));
        }
        let before = self.summary_records.clone();
        let records = self
            .summary_records
            .as_ref()
            .ok_or_else(|| invalid("compaction summary records are missing"))?;
        if records.prefix.is_some() == self.summary.trim().is_empty()
            || records.ranges.len() != self.ranges.len()
        {
            return Err(invalid(
                "compaction summary record slots do not match the epoch",
            ));
        }
        if let Some(import) = &records.legacy_import
            && (!matches!(import.version, 1 | 2)
                || self.legacy_projection_digest(import.version)? != import.projection_sha256)
        {
            return Err(invalid("compaction legacy import witness changed"));
        }
        let mut origins = std::collections::BTreeSet::new();
        let mut restored = records.clone();
        if let Some(record) = &mut restored.prefix {
            if !origins.insert(record.occurrence_id()) {
                return Err(invalid("duplicate compaction summary occurrence"));
            }
            *record =
                record.restored(&prefix_summary_block(&self.conversation_id, &self.summary))?;
        }
        for (record, range) in restored.ranges.iter_mut().zip(&self.ranges) {
            if !origins.insert(record.occurrence_id()) {
                return Err(invalid("duplicate compaction summary occurrence"));
            }
            *record = record.restored(&selective_summary_block(&range.summary))?;
        }
        self.summary_records = Some(restored);
        Ok(self.summary_records != before)
    }

    /// Compares full typed authority, or qualifies an actual old persisted
    /// snapshot against exactly its imported projection. A new equal-text epoch
    /// has no import witness and cannot satisfy a pre-import expectation.
    pub(crate) fn matches_expected(&self, expected: &Self) -> bool {
        if self == expected {
            return true;
        }
        if expected.version == 3 {
            let mut normalized = expected.clone();
            return normalized.restore_summary_records().is_ok() && self == &normalized;
        }
        let Some(import) = self
            .summary_records
            .as_ref()
            .and_then(|records| records.legacy_import.as_ref())
        else {
            return false;
        };
        expected.summary_records.is_none()
            && expected.version == import.version
            && self.legacy_projection_digest(import.version).ok().as_ref()
                == Some(&import.projection_sha256)
            && expected
                .legacy_projection_digest(import.version)
                .ok()
                .as_ref()
                == Some(&import.projection_sha256)
    }

    /// Hashes the exact semantic legacy shape only for explicit migration
    /// qualification. Routine typed equality includes all occurrence records.
    fn legacy_projection_digest(&self, version: u64) -> Result<String> {
        projection::legacy_projection_digest(self, version)
    }
}

/// Compares absence exactly and otherwise uses the qualified epoch authority.
pub(crate) fn compaction_epochs_match(
    current: &Option<AgentCompactionEpoch>,
    expected: &Option<AgentCompactionEpoch>,
) -> bool {
    match (current, expected) {
        (None, None) => true,
        (Some(current), Some(expected)) => current.matches_expected(expected),
        _ => false,
    }
}

/// Keeps malformed persisted accounting errors content-free.
fn invalid(message: impl Into<String>) -> MezError {
    MezError::invalid_state(message)
}
