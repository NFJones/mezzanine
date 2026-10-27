//! Checked replay counters captured under the transcript's mutation lease.
//!
//! Archive/display counts, logical sequence frontiers, and raw-tail row counts
//! are separate domains. Validation proves the archive is contiguous before its
//! row count may stand for a sequence frontier. Selective summaries do not
//! remove physical tail rows; canonical replay applies their coverage later.
//! This module reads only, writes no metadata, and retains no transcript payload.

use super::{AgentPresentationEntry, AgentTranscriptStore};
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

impl AgentTranscriptStore {
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
            .compaction_epoch(conversation_id)?
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
