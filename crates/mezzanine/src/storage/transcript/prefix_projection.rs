//! Pure prospective prefix replacement shared by sizing and durable publication.
//!
//! A new prefix absorbs only complete earlier selective ranges. Later ranges
//! retain their exact anchors and bytes; a split range, regressing boundary or
//! foreign conversation fails before I/O. This data projection grants no write,
//! retry or source authority. Publication still validates frozen rows and epoch
//! under the existing conversation mutation lane before persisting the result.

use super::{AgentCompactionEpoch, CompactionSummary, CompactionSummaryRecords};
use crate::{MezError, Result};
use mez_agent::TranscriptEntry;

/// Projects a replacement prefix over one already validated conversation epoch.
/// Preserves ranges and original records strictly after `through_sequence`,
/// carries the producer's prefix record into v3, and leaves `previous` unchanged. Foreign ownership,
/// backwards progress and a prefix cutting through a selective range fail.
pub(crate) fn project_compaction_prefix(
    conversation_id: &str,
    previous: Option<&AgentCompactionEpoch>,
    through_sequence: u64,
    summary: CompactionSummary<'_>,
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
    let retained = previous
        .into_iter()
        .flat_map(|epoch| {
            epoch
                .ranges
                .iter()
                .enumerate()
                .map(move |(index, range)| (epoch, index, range))
        })
        .filter(|(_, _, range)| range.first_sequence > through_sequence);
    let mut ranges = Vec::new();
    let mut records = Vec::new();
    for (epoch, index, range) in retained {
        let record = epoch
            .summary_records
            .as_ref()
            .and_then(|records| records.ranges.get(index))
            .ok_or_else(|| {
                MezError::invalid_state("retained compaction range record is unavailable")
            })?;
        ranges.push(range.clone());
        records.push(*record);
    }
    Ok(AgentCompactionEpoch {
        summary_records: Some(CompactionSummaryRecords::new(Some(summary.record), records)),
        version: 3,
        conversation_id: conversation_id.to_string(),
        through_sequence,
        summary: summary.content.to_string(),
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
