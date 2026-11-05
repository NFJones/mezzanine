//! Checked replay counters captured under the transcript's mutation lease.
//!
//! Archive/display counts, logical sequence frontiers, and raw-tail row counts
//! are separate domains. Validation proves the archive is contiguous before its
//! row count may stand for a sequence frontier. Selective summaries do not
//! remove physical tail rows; canonical replay applies their coverage later.
//! Replay readers also capture the epoch and checked row projection under one
//! lease, so policy cannot combine an old summary with a separately read tail.
//! Legacy summary accounting may be imported atomically under that same lease;
//! this never changes replay coverage or raw source. Returned snapshots own
//! their payload but do not retain the mutation lane beyond the coherent read.

use super::store::{ConversationTranscriptRead, ConversationTranscriptView};
use super::{AgentCompactionEpoch, AgentPresentationEntry, AgentTranscriptStore};
use crate::{MezError, Result};
use mez_agent::AgentTranscriptPosition;

/// One leased resume source with independently qualified replay counters.
pub(crate) struct ResumeReplaySnapshot {
    /// Existing mutation lease held until the caller adopts or drops the source.
    pub(crate) lease: std::fs::File,
    /// Complete durable terminal presentation captured under the same lease.
    pub(crate) presentation_entries: Vec<AgentPresentationEntry>,
    /// Checked logical frontier and retained raw-tail count, not model blocks.
    pub(crate) position: AgentTranscriptPosition,
}

/// Authoritative epoch and checked rows decoded in one mutation lane.
/// Only the committed rows can justify durable publication; logical rows also
/// contain checked, deduplicated actor-captured pending appends.
pub(crate) struct ConversationReplaySnapshot {
    /// Validated durable prefix/selective projection from this exact read.
    pub(crate) epoch: Option<AgentCompactionEpoch>,
    /// Checked archive and logical rows selected using the captured epoch.
    pub(crate) view: ConversationTranscriptView,
    /// Legacy origins captured with the same physical/logical row window.
    pub(crate) row_origins: super::TranscriptRowOrigins,
    /// Descriptive costs loaded for the exact original selected occurrences.
    pub(crate) row_records: super::TranscriptRowRecords,
}

impl AgentTranscriptStore {
    /// Reads one epoch and its policy-selected checked row window under the same
    /// mutation lease. The pure selector receives only that epoch and may skip
    /// row decoding when the captured owner has no replay work. Malformed epoch,
    /// missing required history, conflicting pending rows, and gaps remain errors.
    pub(crate) fn conversation_replay_snapshot(
        &self,
        conversation_id: &str,
        pending: &[mez_agent::TranscriptEntry],
        select: impl FnOnce(Option<&AgentCompactionEpoch>) -> Option<(ConversationTranscriptRead, bool)>,
    ) -> Result<ConversationReplaySnapshot> {
        let (snapshot, ()) =
            self.conversation_replay_projection(conversation_id, pending, select, |_| {
                Ok(((), Vec::new()))
            })?;
        Ok(snapshot)
    }

    /// Projects checked source once under its original mutation lease and
    /// persists only successful rendered committed-row accounting. The callback
    /// must be pure: it may not reenter storage or acquire another owner lease.
    /// Returned data owns its payload, never the mutation lane. Errors withhold
    /// cache publication; costs grant no semantic or execution authority.
    pub(crate) fn conversation_replay_projection<T>(
        &self,
        conversation_id: &str,
        pending: &[mez_agent::TranscriptEntry],
        select: impl FnOnce(Option<&AgentCompactionEpoch>) -> Option<(ConversationTranscriptRead, bool)>,
        project: impl FnOnce(
            &ConversationReplaySnapshot,
        ) -> Result<(T, Vec<(u64, mez_agent::ContextBlockRecord)>)>,
    ) -> Result<(ConversationReplaySnapshot, T)> {
        let lease = self.acquire_conversation_lock(conversation_id)?;
        let epoch = self.compaction_epoch_locked(conversation_id)?;
        let view = match select(epoch.as_ref()) {
            Some((read, required)) => self.conversation_transcript_view_locked(
                conversation_id,
                read,
                required,
                pending,
                &lease,
            )?,
            None => ConversationTranscriptView {
                committed: Vec::new(),
                logical: Vec::new(),
            },
        };
        let row_origins = self.transcript_row_origins_locked(conversation_id, &view.logical)?;
        let row_records = self.transcript_row_records_locked(conversation_id, &row_origins)?;
        let snapshot = ConversationReplaySnapshot {
            epoch,
            view,
            row_origins,
            row_records,
        };
        let (value, records) = project(&snapshot)?;
        self.save_rendered_row_records_locked(conversation_id, &snapshot, &records)?;
        Ok((snapshot, value))
    }

    /// Captures presentation and replay position without racing a transcript
    /// append or epoch publication. Missing required history, malformed rows,
    /// and a boundary beyond the archive fail rather than shorten replay.
    pub(crate) fn lock_resume_replay_snapshot(
        &self,
        conversation_id: &str,
        required_archive: bool,
    ) -> Result<ResumeReplaySnapshot> {
        let (lease, presentation_entries, _) = self.lock_presentation_snapshot(conversation_id)?;
        let logical_high_water = match self.validate_restored_transcript(conversation_id) {
            Ok(count) => count,
            Err(error)
                if !required_archive
                    && (error.kind() == crate::error::MezErrorKind::NotFound
                        || error.io_kind() == Some(std::io::ErrorKind::NotFound)) =>
            {
                0
            }
            Err(error) => return Err(error),
        };
        let prefix = self
            .compaction_epoch_locked(conversation_id)?
            .map_or(0, |epoch| epoch.through_sequence);
        let active_raw_entries = logical_high_water.checked_sub(prefix).ok_or_else(|| {
            MezError::invalid_state("compaction prefix exceeds checked transcript frontier")
        })?;
        Ok(ResumeReplaySnapshot {
            lease,
            presentation_entries,
            position: AgentTranscriptPosition {
                logical_high_water,
                active_raw_entries,
            },
        })
    }
}
