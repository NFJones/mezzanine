//! Qualified manual prefix publication preserves exact source authority.
//!
//! Planning reads an immutable epoch and physical rows before model work. A
//! later summary edit can leave every raw row unchanged, so publication must
//! compare both domains under the conversation mutation lane. Absorbed ranges
//! become the new prefix; later ranges retain their exact bytes and positions.
//! The existing crash-safe sidecar writer owns persistence and version policy.

use super::{
    AgentCompactionEpoch, AgentTranscriptStore, ConversationTranscriptRead,
    project_compaction_prefix, validate_compaction_prefix_source,
};
use crate::{MezError, Result};
use mez_agent::TranscriptEntry;

impl AgentTranscriptStore {
    /// Publishes a manual prefix only against the complete frozen source epoch
    /// and matching physical prefix. Missing/corrupt history, changed summaries,
    /// changed rows and a split range fail before metadata or archive mutation.
    /// A baseline of `None` qualifies absence, rather than skipping the check.
    /// Returns the next checked logical sequence from this publication lane,
    /// including deduplicated pending reservations. Conflicting pending rows or
    /// exhausted sequences fail before publication; no second read is needed.
    pub(crate) fn save_compaction_prefix_with_proof(
        &self,
        conversation_id: &str,
        through_sequence: u64,
        summary: &str,
        frozen_rows: &[TranscriptEntry],
        baseline: &Option<AgentCompactionEpoch>,
        pending: &[TranscriptEntry],
    ) -> Result<u64> {
        let lease = self.acquire_conversation_lock(conversation_id)?;
        let previous = self.compaction_epoch(conversation_id)?;
        if &previous != baseline {
            return Err(MezError::conflict(
                "manual compaction source epoch changed before publication",
            ));
        }
        let previous_boundary = previous.as_ref().map_or(0, |epoch| epoch.through_sequence);
        let view = self.conversation_transcript_view_locked(
            conversation_id,
            ConversationTranscriptRead::All,
            true,
            pending,
            &lease,
        )?;
        let next_sequence = view.logical.last().map_or(Ok(1), |row| {
            row.sequence.checked_add(1).ok_or_else(|| {
                MezError::invalid_state("compaction transcript sequence is exhausted")
            })
        })?;
        validate_compaction_prefix_source(
            &view.committed,
            previous_boundary,
            through_sequence,
            frozen_rows,
        )?;
        let projection = project_compaction_prefix(
            conversation_id,
            previous.as_ref(),
            through_sequence,
            summary,
        )?;
        self.save_compaction_projection_locked(projection, None, Some(baseline))?;
        Ok(next_sequence)
    }
}
