//! Compactor input separates physical publication proof from model-visible rows.
//!
//! Selected selective ranges render once at their original sequence anchors.
//! Covered raw content and unselected range summaries never enter that request;
//! physical rows remain available to the caller for count and publication proof.
//! This is an ephemeral request projection, not a new durable transcript codec.

use super::*;
use crate::storage::transcript::AgentCompactionRange;

/// Borrowed, qualified physical source and its selected summary replacements.
#[derive(Debug, Clone, Copy)]
pub(in crate::runtime::commands) struct CompactionTranscriptSource<'a> {
    /// Physical source rows, including rows represented by selected summaries.
    pub(super) rows: &'a [TranscriptEntry],
    /// Whole selected ranges in the same frozen physical source.
    ranges: &'a [AgentCompactionRange],
    /// An authoritative selective snapshot replaces cached range memory blocks,
    /// including when every range lies after this selected prefix.
    selective_history: bool,
}

impl<'a> CompactionTranscriptSource<'a> {
    /// Retains ordinary nonselective compactor input without changing its shape.
    pub(in crate::runtime::commands) fn raw(rows: &'a [TranscriptEntry]) -> Self {
        Self {
            rows,
            ranges: &[],
            selective_history: false,
        }
    }

    /// Qualifies whole selected ranges against their physical source. A partial,
    /// overlapping, absent or reversed range is an error before model dispatch.
    pub(super) fn projected(
        rows: &'a [TranscriptEntry],
        ranges: &'a [AgentCompactionRange],
    ) -> Result<Self> {
        let mut previous = 0;
        for range in ranges {
            if range.first_sequence <= previous
                || range.first_sequence > range.through_sequence
                || range.summary.trim().is_empty()
                || !rows.iter().any(|row| row.sequence == range.first_sequence)
                || !rows
                    .iter()
                    .any(|row| row.sequence == range.through_sequence)
            {
                return Err(MezError::invalid_state(
                    "compactor input splits or loses a selective range",
                ));
            }
            previous = range.through_sequence;
        }
        Ok(Self {
            rows,
            ranges,
            selective_history: true,
        })
    }

    /// Removes raw replay and cached selective summaries before rendering the
    /// selected replacements inline. The earlier prefix summary and unrelated
    /// exact references preserve their original metadata and ordering.
    pub(super) fn prepare_context(self, context: AgentContext) -> Result<AgentContext> {
        let mut context = runtime_compaction_context_without_transcript_blocks(context)?;
        if self.selective_history {
            context.retain_blocks_with_metadata(|block, _| {
                !(block.source == ContextSourceKind::Memory
                    && block.label == "context compaction summary")
            })?;
        }
        Ok(context.revalidate()?)
    }

    /// Appends ordered source lines with summaries replacing covered raw rows.
    /// Summary bytes use the same credential redaction as ordinary source text.
    pub(super) fn append_lines(self, lines: &mut Vec<String>) {
        if self.selective_history {
            let raw = self
                .rows
                .iter()
                .filter(|entry| {
                    !self.ranges.iter().any(|range| {
                        (range.first_sequence..=range.through_sequence).contains(&entry.sequence)
                    })
                })
                .count();
            lines.push(format!("Raw entries supplied for compaction: {raw}"));
            lines.push(format!(
                "Selective summaries supplied for compaction: {}",
                self.ranges.len()
            ));
        }
        for entry in self.rows {
            if let Some(range) = self
                .ranges
                .iter()
                .find(|range| range.first_sequence == entry.sequence)
            {
                lines.push(format!(
                    "summary_range={}..={} content={}",
                    range.first_sequence,
                    range.through_sequence,
                    runtime_model_compaction_entry_content(&range.summary)
                ));
            }
            if self.ranges.iter().any(|range| {
                (range.first_sequence..=range.through_sequence).contains(&entry.sequence)
            }) {
                continue;
            }
            lines.push(format!(
                "entry={} role={} turn={} pane={} content={}",
                entry.sequence,
                runtime_transcript_role_name(entry.role),
                entry.turn_id,
                entry.pane_id,
                runtime_model_compaction_entry_content(&entry.content)
            ));
        }
    }
}
