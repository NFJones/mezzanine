//! Atomic chronological range replacement on the canonical context owner.
//!
//! Retained events keep their identities; exact barriers and unrecoverable
//! evidence cannot be summarized. Every replacement validates an isolated
//! candidate before committing to the existing context state.

use super::{
    AgentContext, AgentContextError, AgentContextResult, ContextBlock, ContextPlacement,
    ContextRetention, ContextSemanticKind, ConversationEvent, context_block_is_compaction_summary,
};
use std::ops::Range;

impl AgentContext {
    /// Replaces closed chronological ranges with semantic summaries while
    /// preserving every retained event identity.
    ///
    /// Ranges are expressed in typed chronology indexes, must be ordered and
    /// non-overlapping, and are applied from newest to oldest. Each summary
    /// inherits the first replaced sequence so later retained events keep their
    /// original identities and strict temporal order. Exact barriers and
    /// unrecoverable events cannot be replaced.
    pub fn compact_execution_ranges(
        &mut self,
        replacements: Vec<(Range<usize>, ContextBlock)>,
    ) -> AgentContextResult<()> {
        let mut candidate = self.clone();
        candidate.compact_execution_ranges_candidate(replacements)?;
        *self = candidate;
        Ok(())
    }

    /// Replaces multiple closed chronology ranges with one rolling summary.
    ///
    /// Selected ranges must be ordered and non-overlapping. They are removed
    /// newest-first, then one recursively compactable summary is inserted at
    /// the earliest selected sequence. Exact barriers between selected ranges
    /// remain in place and retain their original event identities.
    pub fn compact_execution_ranges_into_summary(
        &mut self,
        ranges: Vec<Range<usize>>,
        summary: ContextBlock,
    ) -> AgentContextResult<()> {
        let mut candidate = self.clone();
        candidate.compact_execution_ranges_into_summary_candidate(ranges, summary)?;
        *self = candidate;
        Ok(())
    }

    /// Applies one atomic multi-range rolling-summary replacement.
    fn compact_execution_ranges_into_summary_candidate(
        &mut self,
        ranges: Vec<Range<usize>>,
        summary: ContextBlock,
    ) -> AgentContextResult<()> {
        if summary.placement != ContextPlacement::ConversationAppend
            || summary.semantic_kind() != ContextSemanticKind::ReferenceEvent
            || summary.retention() != ContextRetention::Summarizable
        {
            return Err(AgentContextError::new(
                "compaction replacement must be a summarizable chronological reference event",
            ));
        }
        let mut previous_end = 0usize;
        for (index, range) in ranges.iter().enumerate() {
            if range.is_empty()
                || range.end > self.chronology.len()
                || (index > 0 && range.start < previous_end)
            {
                return Err(AgentContextError::new(
                    "compaction ranges must be non-empty, in bounds, ordered, and non-overlapping",
                ));
            }
            if self.chronology[range.clone()].iter().any(|event| {
                event.retention == ContextRetention::Exact
                    || (!event.recoverable_for_compaction
                        && !context_block_is_compaction_summary(event.block()))
            }) {
                return Err(AgentContextError::new(
                    "compaction cannot replace exact or unrecoverable chronological events",
                ));
            }
            previous_end = range.end;
        }
        let first = ranges
            .first()
            .ok_or_else(|| AgentContextError::new("compaction requires at least one range"))?;
        let insertion_index = first.start;
        let sequence = self.chronology[insertion_index].sequence;
        for range in ranges.into_iter().rev() {
            self.chronology.drain(range);
        }
        self.chronology.insert(
            insertion_index,
            ConversationEvent {
                block: summary,
                semantic_kind: ContextSemanticKind::ReferenceEvent,
                retention: ContextRetention::Summarizable,
                sequence,
                execution_group_id: None,
                provider_owner: None,
                recoverable_for_compaction: true,
            },
        );
        self.rebuild_projections();
        self.validate_durable()
    }

    /// Applies validated compaction replacements to an isolated candidate.
    fn compact_execution_ranges_candidate(
        &mut self,
        replacements: Vec<(Range<usize>, ContextBlock)>,
    ) -> AgentContextResult<()> {
        let mut previous_end = 0usize;
        for (index, (range, summary)) in replacements.iter().enumerate() {
            if range.is_empty()
                || range.end > self.chronology.len()
                || (index > 0 && range.start < previous_end)
            {
                return Err(AgentContextError::new(
                    "compaction ranges must be non-empty, in bounds, ordered, and non-overlapping",
                ));
            }
            if summary.placement != ContextPlacement::ConversationAppend
                || summary.semantic_kind() != ContextSemanticKind::ReferenceEvent
                || summary.retention() != ContextRetention::Summarizable
            {
                return Err(AgentContextError::new(
                    "compaction replacement must be a summarizable chronological reference event",
                ));
            }
            let replaced = &self.chronology[range.clone()];
            if replaced.iter().any(|event| {
                event.retention == ContextRetention::Exact || !event.recoverable_for_compaction
            }) {
                return Err(AgentContextError::new(
                    "compaction cannot replace exact or unrecoverable chronological events",
                ));
            }
            previous_end = range.end;
        }

        for (range, summary) in replacements.into_iter().rev() {
            let sequence = self.chronology[range.start].sequence;
            let summary_event = ConversationEvent {
                block: summary,
                semantic_kind: ContextSemanticKind::ReferenceEvent,
                retention: ContextRetention::Summarizable,
                sequence,
                execution_group_id: None,
                provider_owner: None,
                recoverable_for_compaction: true,
            };
            self.chronology.splice(range, [summary_event]);
        }
        self.rebuild_projections();
        self.validate_durable()
    }
}
