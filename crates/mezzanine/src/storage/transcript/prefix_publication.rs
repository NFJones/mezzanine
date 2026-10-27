//! Qualified manual prefix publication preserves exact source authority.
//!
//! Planning reads an immutable epoch and physical rows before model work. A
//! later summary edit can leave every raw row unchanged, so publication must
//! compare both domains under the conversation mutation lane. Absorbed ranges
//! become the new prefix; later ranges retain their exact bytes and positions.
//! The existing crash-safe sidecar writer owns persistence and version policy.

use super::{AgentCompactionEpoch, AgentTranscriptStore, ConversationTranscriptRead};
use crate::{MezError, Result};
use mez_agent::TranscriptEntry;

impl AgentTranscriptStore {
    /// Publishes a manual prefix only against the complete frozen source epoch
    /// and matching physical prefix. Missing/corrupt history, changed summaries,
    /// changed rows and a split range fail before metadata or archive mutation.
    /// A baseline of `None` qualifies absence, rather than skipping the check.
    pub(crate) fn save_compaction_prefix_with_proof(
        &self,
        conversation_id: &str,
        through_sequence: u64,
        summary: &str,
        frozen_rows: &[TranscriptEntry],
        baseline: &Option<AgentCompactionEpoch>,
    ) -> Result<()> {
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
            ConversationTranscriptRead::Through(through_sequence),
            true,
            &[],
            &lease,
        )?;
        let selected = view
            .committed
            .into_iter()
            .filter(|row| row.sequence > previous_boundary)
            .collect::<Vec<_>>();
        if selected != frozen_rows {
            return Err(MezError::conflict(
                "manual compaction physical source changed before publication",
            ));
        }
        let ranges = previous.map_or(Vec::new(), |epoch| epoch.ranges);
        if ranges.iter().any(|range| {
            range.first_sequence <= through_sequence && through_sequence < range.through_sequence
        }) {
            return Err(MezError::invalid_state(
                "prefix compaction cannot split a selective execution range",
            ));
        }
        let remaining = ranges
            .into_iter()
            .filter(|range| range.first_sequence > through_sequence)
            .collect::<Vec<_>>();
        self.save_compaction_projection_locked(
            AgentCompactionEpoch {
                version: if remaining.is_empty() { 1 } else { 2 },
                conversation_id: conversation_id.to_string(),
                through_sequence,
                summary: summary.to_string(),
                ranges: remaining,
            },
            None,
            Some(baseline),
        )
    }
}
