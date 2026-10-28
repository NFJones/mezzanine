//! Immutable row and initial-projection ownership for transcript admission.
//!
//! A forked child owns new row sequences independently of its parent's archive.
//! Its inherited prefix is therefore a zero-boundary summary, with selective
//! ranges mapped over the child's raw rows. The batch keeps that authority with
//! the rows throughout admission, recovery and settlement. Empty rows are valid
//! only when a nonempty authoritative projection supplies the conversation id;
//! sequence zero denotes that metadata-only receipt, never a transcript row.
//! Construction is pure and validates the complete payload before any I/O.

use super::AgentCompactionEpoch;
use super::store::{
    COMPACTION_EPOCH_MAX_BYTES, validate_compaction_range_sources, validate_compaction_ranges,
};
use crate::{MezError, Result};
use mez_agent::TranscriptEntry;
use mez_agent::transcript::validate_conversation_id;

/// One exact append owner, including optional initial child replay authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TranscriptAppendBatch {
    /// Exact conversation owner, retained independently of physical row count.
    conversation_id: String,
    /// Validated ordered physical rows; initial archives start at sequence one.
    entries: Vec<TranscriptEntry>,
    /// Optional zero-boundary prefix and selective authority for a new child.
    initial_compaction_epoch: Option<AgentCompactionEpoch>,
}

impl TranscriptAppendBatch {
    /// Captures a nonempty contiguous ordinary append. Invalid entries, mixed
    /// conversations, reordered rows or exhausted spans fail without mutation.
    pub(crate) fn rows(entries: Vec<TranscriptEntry>) -> Result<Self> {
        let conversation_id = entries
            .first()
            .ok_or_else(|| {
                MezError::invalid_args("empty transcript append requires an initial projection")
            })?
            .conversation_id
            .clone();
        Self::validated(conversation_id, entries, None)
    }

    /// Captures a child-owned initial projection and its complete raw archive.
    /// Rows start at one; a zero-boundary prefix may accompany no rows at all.
    /// Invalid sidecar shape, incomplete/selectively protected source, oversized
    /// metadata or a foreign row fails before any receipt can be accepted.
    pub(crate) fn initial_projection(
        epoch: AgentCompactionEpoch,
        entries: Vec<TranscriptEntry>,
    ) -> Result<Self> {
        Self::validated(epoch.conversation_id.clone(), entries, Some(epoch))
    }

    /// Validates common row identity before checking optional initial authority.
    fn validated(
        conversation_id: String,
        entries: Vec<TranscriptEntry>,
        initial_compaction_epoch: Option<AgentCompactionEpoch>,
    ) -> Result<Self> {
        validate_conversation_id(&conversation_id)?;
        for entry in &entries {
            entry.validate()?;
            if entry.conversation_id != conversation_id {
                return Err(MezError::invalid_args(
                    "transcript append contains a foreign conversation",
                ));
            }
        }
        if entries
            .windows(2)
            .any(|pair| pair[0].sequence.checked_add(1) != Some(pair[1].sequence))
        {
            return Err(MezError::invalid_args(
                "transcript append requires contiguous ordered rows",
            ));
        }
        if let Some(epoch) = initial_compaction_epoch.as_ref() {
            if epoch.conversation_id != conversation_id
                || epoch.through_sequence != 0
                || !matches!(epoch.version, 1 | 2)
                || (epoch.version == 1
                    && (epoch.summary.trim().is_empty() || !epoch.ranges.is_empty()))
                || (epoch.version == 2
                    && epoch.summary.trim().is_empty()
                    && epoch.ranges.is_empty())
                || entries.first().is_some_and(|row| row.sequence != 1)
            {
                return Err(MezError::invalid_args(
                    "initial transcript projection has invalid identity, shape or row origin",
                ));
            }
            let encoded = serde_json::to_vec(epoch).map_err(|error| {
                MezError::invalid_args(format!(
                    "initial transcript projection encode failed: {error}"
                ))
            })?;
            if encoded.len().saturating_add(1) as u64 > COMPACTION_EPOCH_MAX_BYTES {
                return Err(MezError::invalid_args(
                    "initial transcript projection exceeds its accepted size limit",
                ));
            }
            let next = (entries.len() as u64).checked_add(1).ok_or_else(|| {
                MezError::invalid_args("initial transcript projection sequence is exhausted")
            })?;
            validate_compaction_ranges(&epoch.ranges, 0, next)?;
            validate_compaction_range_sources(&epoch.ranges, &entries)?;
        } else if entries.is_empty() {
            return Err(MezError::invalid_args(
                "empty transcript append has no projection",
            ));
        }
        Ok(Self {
            conversation_id,
            entries,
            initial_compaction_epoch,
        })
    }

    /// Returns the exact conversation owner, even for a metadata-only batch.
    pub(crate) fn conversation_id(&self) -> &str {
        &self.conversation_id
    }

    /// Returns the first real sequence, or the reserved metadata-only key zero.
    /// This receipt key must never be interpreted as an admitted raw row.
    pub(crate) fn first_sequence(&self) -> u64 {
        self.entries.first().map_or(0, |row| row.sequence)
    }

    /// Borrows the exact physical append source without dropping its owner.
    pub(crate) fn entries(&self) -> &[TranscriptEntry] {
        &self.entries
    }

    /// Borrows initial replay authority; ordinary appends carry none.
    pub(crate) fn initial_compaction_epoch(&self) -> Option<&AgentCompactionEpoch> {
        self.initial_compaction_epoch.as_ref()
    }
}
