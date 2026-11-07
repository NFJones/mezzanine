//! Immutable row and initial-projection ownership for transcript admission.
//!
//! A forked child owns new row sequences independently of its parent's archive.
//! Legacy prefixes use a zero-boundary summary, with selective ranges mapped
//! over child rows. Typed copies retain their canonical epoch in an independent
//! planned storage root. The batch keeps either authority with
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
    /// Exact planned typed root, owned by the same initial receipt as its rows.
    initial_replay_epoch: Option<std::sync::Arc<super::replay_epoch::InitialReplayEpoch>>,
    /// Original raw occurrences/caches, independent of optional summary authority.
    initial_row_accounting: Option<std::sync::Arc<super::InitialTranscriptRowAccounting>>,
    /// Once-allocated ordinary occurrences; absent only at explicit raw imports.
    row_accounting: Option<std::sync::Arc<super::TranscriptAppendAccounting>>,
}

impl TranscriptAppendBatch {
    /// Owns an ordinary producer's exact row occurrence proposal. Unaccounted
    /// rows receive new IDs once; clones retain them and authored rows retain
    /// their actual canonical producer records.
    pub(crate) fn owned_rows(entries: Vec<TranscriptEntry>) -> Result<Self> {
        let batch = Self::rows(entries)?;
        let accounting = super::TranscriptAppendAccounting::new(batch.entries())?;
        batch.with_row_accounting(accounting)
    }

    /// Validates a nonempty raw span at an explicit unaccounted import/recovery
    /// boundary or before attaching inherited accounting. New ordinary producers
    /// use `owned_rows`. Invalid rows or ordering fail without mutation.
    pub(crate) fn rows(entries: Vec<TranscriptEntry>) -> Result<Self> {
        let conversation_id = entries
            .first()
            .ok_or_else(|| {
                MezError::invalid_args("empty transcript append requires an initial projection")
            })?
            .conversation_id
            .clone();
        Self::validated(conversation_id, entries, None, None)
    }

    /// Captures a child-owned initial projection and its complete raw archive.
    /// Rows start at one; a zero-boundary prefix may accompany no rows at all.
    /// Invalid sidecar shape, incomplete/selectively protected source, oversized
    /// metadata or a foreign row fails before any receipt can be accepted.
    pub(crate) fn initial_projection(
        epoch: AgentCompactionEpoch,
        entries: Vec<TranscriptEntry>,
    ) -> Result<Self> {
        Self::validated(epoch.conversation_id.clone(), entries, Some(epoch), None)
    }

    /// Binds independent raw placement to one immutable planned typed root.
    pub(crate) fn initial_replay(
        initial: super::replay_epoch::InitialReplayEpoch,
        entries: Vec<TranscriptEntry>,
    ) -> Result<Self> {
        Self::validated(
            initial.conversation_id().into(),
            entries,
            None,
            Some(initial),
        )
    }

    /// Validates common row identity before checking optional initial authority.
    fn validated(
        conversation_id: String,
        entries: Vec<TranscriptEntry>,
        initial_compaction_epoch: Option<AgentCompactionEpoch>,
        initial_replay_epoch: Option<super::replay_epoch::InitialReplayEpoch>,
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
        if let Some(initial) = &initial_replay_epoch {
            if initial_compaction_epoch.is_some()
                || entries.first().is_some_and(|row| row.sequence != 1)
            {
                return Err(MezError::invalid_args(
                    "initial typed projection has invalid row origin",
                ));
            }
            initial.validate_rows(&entries)?;
        } else if let Some(epoch) = initial_compaction_epoch.as_ref() {
            if epoch.conversation_id != conversation_id
                || epoch.through_sequence != 0
                || !matches!(epoch.version, 1..=3)
                || (epoch.version == 1
                    && (epoch.summary.trim().is_empty() || !epoch.ranges.is_empty()))
                || (epoch.version != 1
                    && epoch.summary.trim().is_empty()
                    && epoch.ranges.is_empty())
                || entries.first().is_some_and(|row| row.sequence != 1)
                || epoch.summary.len() as u64 > COMPACTION_EPOCH_MAX_BYTES
                || (epoch.version != 3 && epoch.summary_records.is_some())
                || (epoch.version == 3 && epoch.summary_records.is_none())
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
            let encoded_limit = if epoch.version == 3 {
                super::store::ACCOUNTED_COMPACTION_EPOCH_MAX_BYTES
            } else {
                COMPACTION_EPOCH_MAX_BYTES
            };
            if encoded.len().saturating_add(1) as u64 > encoded_limit {
                return Err(MezError::invalid_args(
                    "initial transcript projection exceeds its accepted size limit",
                ));
            }
            let next = (entries.len() as u64).checked_add(1).ok_or_else(|| {
                MezError::invalid_args("initial transcript projection sequence is exhausted")
            })?;
            validate_compaction_ranges(&epoch.ranges, 0, next)?;
            validate_compaction_range_sources(&epoch.ranges, &entries)?;
            epoch.validate_summary_payload_size()?;
            if epoch.version == 3 {
                // Validate descriptive data against source without changing the
                // exact journal payload whose digest owns this append receipt.
                epoch.clone().restore_summary_records()?;
            }
        } else if entries.is_empty() {
            return Err(MezError::invalid_args(
                "empty transcript append has no projection",
            ));
        }
        Ok(Self {
            conversation_id,
            entries,
            initial_compaction_epoch,
            initial_replay_epoch: initial_replay_epoch.map(std::sync::Arc::new),
            initial_row_accounting: None,
            row_accounting: None,
        })
    }

    /// Couples complete inherited raw accounting to this immutable initial owner.
    /// Pure validation rejects missing placement or changed authored identities.
    pub(crate) fn with_initial_row_accounting(
        mut self,
        accounting: super::InitialTranscriptRowAccounting,
    ) -> Result<Self> {
        if self.row_accounting.is_some() {
            return Err(MezError::invalid_args(
                "ordinary accounting cannot own initial authority",
            ));
        }
        accounting.validate(&self.entries)?;
        self.initial_row_accounting = Some(std::sync::Arc::new(accounting));
        Ok(self)
    }

    /// Restores an original ordinary proposal at the explicit receipt boundary.
    /// Initial replay authority uses its separate whole-conversation contract.
    pub(crate) fn with_row_accounting(
        mut self,
        accounting: super::TranscriptAppendAccounting,
    ) -> Result<Self> {
        if self.has_initial_authority() {
            return Err(MezError::invalid_args(
                "initial authority cannot own ordinary accounting",
            ));
        }
        accounting.validate(&self.entries)?;
        self.row_accounting = Some(std::sync::Arc::new(accounting));
        Ok(self)
    }

    /// Borrows the original ordinary proposal without allocating on retry.
    pub(crate) fn row_accounting(&self) -> Option<&super::TranscriptAppendAccounting> {
        self.row_accounting.as_deref()
    }

    /// Borrows the exact initial raw record proposal without resampling storage.
    pub(crate) fn initial_row_accounting(&self) -> Option<&super::InitialTranscriptRowAccounting> {
        self.initial_row_accounting.as_deref()
    }

    /// Identifies initial publication even when the source has no summary head.
    pub(crate) fn has_initial_authority(&self) -> bool {
        self.initial_compaction_epoch.is_some()
            || self.initial_replay_epoch.is_some()
            || self.initial_row_accounting.is_some()
    }

    /// Constructs a pure initial render source. No rows are claimed committed;
    /// original receipt settlement still owns durability and all real authority.
    pub(crate) fn initial_replay_snapshot(&self) -> Option<super::ConversationReplaySnapshot> {
        self.initial_row_accounting
            .as_ref()
            .map(|accounting| super::ConversationReplaySnapshot {
                stable_records: accounting
                    .stable_records()
                    .map(super::TranscriptStableSnapshot::from_initial),
                initial_stable_records: accounting
                    .stable_records()
                    .map(super::TranscriptStableSnapshot::from_initial),
                epoch: self.initial_compaction_epoch.clone(),
                replay: None,
                settled_pending_origins: Default::default(),
                view: super::store::ConversationTranscriptView {
                    committed: Vec::new(),
                    logical: self.entries.clone(),
                },
                row_origins: accounting.origins().clone(),
                row_records: accounting.records().clone(),
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

    pub(crate) fn initial_replay_epoch(&self) -> Option<&super::replay_epoch::InitialReplayEpoch> {
        self.initial_replay_epoch.as_deref()
    }
}
