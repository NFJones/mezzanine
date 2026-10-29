//! Pure prospective prefix replacement shared by sizing and durable publication.
//!
//! A new prefix absorbs only complete earlier selective ranges. Later ranges
//! retain their exact anchors and bytes; a split range, regressing boundary or
//! foreign conversation fails before I/O. This data projection grants no write,
//! retry or source authority. Publication still validates frozen rows and epoch
//! under the existing conversation mutation lane before persisting the result.

use super::AgentCompactionEpoch;
use crate::{MezError, Result};
use mez_agent::TranscriptEntry;

/// Projects a replacement prefix over one already validated conversation epoch.
/// Preserves all ranges strictly after `through_sequence`, chooses the existing
/// v1/v2 representation, and leaves `previous` unchanged. Foreign ownership,
/// backwards progress and a prefix cutting through a selective range fail.
pub(crate) fn project_compaction_prefix(
    conversation_id: &str,
    previous: Option<&AgentCompactionEpoch>,
    through_sequence: u64,
    summary: &str,
) -> Result<AgentCompactionEpoch> {
    if let Some(previous) = previous {
        if previous.conversation_id != conversation_id {
            return Err(MezError::conflict(
                "compaction projection conversation changed",
            ));
        }
        if through_sequence < previous.through_sequence {
            return Err(MezError::invalid_state(
                "compaction prefix cannot move backwards",
            ));
        }
        if previous.ranges.iter().any(|range| {
            range.first_sequence <= through_sequence && through_sequence < range.through_sequence
        }) {
            return Err(MezError::invalid_state(
                "prefix compaction cannot split a selective execution range",
            ));
        }
    }
    let ranges = previous
        .into_iter()
        .flat_map(|epoch| &epoch.ranges)
        .filter(|range| range.first_sequence > through_sequence)
        .cloned()
        .collect::<Vec<_>>();
    Ok(AgentCompactionEpoch {
        version: if ranges.is_empty() { 1 } else { 2 },
        conversation_id: conversation_id.to_string(),
        through_sequence,
        summary: summary.to_string(),
        ranges,
    })
}

/// Requires the complete physical prefix to equal its frozen planning source.
/// The caller chooses the logical candidate view or committed publication view;
/// this comparison grants neither freshness nor durable publication authority.
pub(crate) fn validate_compaction_prefix_source(
    rows: &[TranscriptEntry],
    previous_boundary: u64,
    through_sequence: u64,
    frozen_rows: &[TranscriptEntry],
) -> Result<()> {
    if !rows
        .iter()
        .filter(|row| row.sequence > previous_boundary && row.sequence <= through_sequence)
        .eq(frozen_rows.iter())
    {
        return Err(MezError::conflict(
            "manual compaction physical source changed before publication",
        ));
    }
    Ok(())
}
