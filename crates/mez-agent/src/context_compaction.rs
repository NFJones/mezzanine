//! Provider-independent model-context compaction and budgeting.
//!
//! This module owns deterministic planning and validated application for
//! model-authored compaction after an explicit trigger or provider context-limit
//! response. It freezes the provider-consumed event boundary, replaces only
//! closed contiguous ranges at their original anchors, keeps exact barriers and
//! straddling causal owners raw, and preserves retained event identities without
//! product runtime or persistence dependencies.

use crate::{
    AgentContext, AgentContextError, AgentContextResult, ContextBlock, ContextEventSequence,
    ContextRetention, ContextSemanticKind, ContextSourceKind, ModelContextCompactionReport,
    context_block_is_compaction_summary, model_context_block_header,
};
use std::ops::Range;

/// Maximum bytes from one context block retained in a raw suffix.
const MODEL_CONTEXT_BLOCK_LIMIT_BYTES: usize = 128 * 1024;
/// Default raw suffix percent retained around model-authored compaction.
pub const DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT: usize = 10;

/// Deterministic replacement plan awaiting a model-authored summary.
///
/// The plan freezes selected chronology by stable event sequence, leaves every
/// retained barrier and raw tail in place, and contains no locally generated
/// semantic prose. Applying it validates the same event identities and source
/// blocks before one atomic summary insertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelContextCompactionPlan {
    consumed_sequence_high_water: u64,
    replacement_event_sequences: Vec<ContextEventSequence>,
    replacement_blocks: Vec<ContextBlock>,
    replacement_group_lengths: Vec<usize>,
    retained_tail: Vec<ContextBlock>,
    summary_budget_words: usize,
    report: ModelContextCompactionReport,
}

impl ModelContextCompactionPlan {
    /// Returns whether this plan replaces any closed chronology events.
    pub fn changes_context(&self) -> bool {
        !self.replacement_event_sequences.is_empty()
    }

    /// Returns the provider-consumed chronology boundary frozen by this plan.
    pub fn consumed_sequence_high_water(&self) -> u64 {
        self.consumed_sequence_high_water
    }

    /// Returns exact blocks the compactor model must summarize in chronology order.
    pub fn replacement_blocks(&self) -> &[ContextBlock] {
        &self.replacement_blocks
    }

    /// Returns exact recent blocks intentionally retained outside model summary input.
    pub fn retained_tail(&self) -> &[ContextBlock] {
        &self.retained_tail
    }

    /// Returns the maximum provider-visible word budget for the model summary.
    pub fn summary_budget_words(&self) -> usize {
        self.summary_budget_words
    }

    /// Returns deterministic accounting for the selected replacement blocks.
    pub fn report(&self) -> ModelContextCompactionReport {
        self.report
    }

    /// Moves the newest complete selected execution group into the exact tail.
    ///
    /// Returns `false` when removing another group would leave no model input.
    /// The operation preserves block bytes and chronology while shrinking only
    /// the material submitted to the compactor model.
    pub fn exclude_newest_replacement_group(&mut self) -> bool {
        let Some(group_len) = self.replacement_group_lengths.last().copied() else {
            return false;
        };
        if group_len == 0 || group_len >= self.replacement_blocks.len() {
            return false;
        }
        let split_at = self.replacement_blocks.len().saturating_sub(group_len);
        let excluded_blocks = self.replacement_blocks.split_off(split_at);
        self.replacement_event_sequences.truncate(split_at);
        self.replacement_group_lengths.pop();
        let excluded_words = model_context_total_words(&excluded_blocks);
        self.summary_budget_words = self.summary_budget_words.saturating_sub(excluded_words);
        self.retained_tail.splice(0..0, excluded_blocks);
        self.report.compacted_blocks = self.replacement_blocks.len();
        true
    }

    /// Constructs a no-op plan that preserves the supplied provider projection.
    fn unchanged(
        _blocks: &[ContextBlock],
        report: ModelContextCompactionReport,
        consumed_sequence_high_water: u64,
    ) -> Self {
        Self {
            consumed_sequence_high_water,
            replacement_event_sequences: Vec::new(),
            replacement_blocks: Vec::new(),
            replacement_group_lengths: Vec::new(),
            retained_tail: Vec::new(),
            summary_budget_words: 0,
            report,
        }
    }
}

/// Plans model-authored compaction without synthesizing semantic summary text.
///
/// The returned plan freezes complete closed execution groups at
/// `consumed_sequence_high_water`. Call
/// [`apply_model_context_compaction_plan`] only after a provider returns a
/// validated summary for [`ModelContextCompactionPlan::replacement_blocks`].
pub fn plan_model_context_compaction_at_consumed_sequence(
    context: &AgentContext,
    context_budget_words: usize,
    retained_tail_percent: usize,
    consumed_sequence_high_water: u64,
) -> AgentContextResult<ModelContextCompactionPlan> {
    context.validate_durable()?;
    let blocks = context.blocks();
    let retained_tail_percent =
        normalize_model_context_retained_tail_percent(retained_tail_percent);
    let stable_prefix = blocks
        .iter()
        .filter(|block| block.placement == crate::ContextPlacement::StablePrefix)
        .cloned()
        .collect::<Vec<_>>();
    let immutable_chronology = blocks
        .iter()
        .filter(|block| block.placement == crate::ContextPlacement::ConversationAppend)
        .cloned()
        .collect::<Vec<_>>();
    let protected_words = stable_prefix
        .iter()
        .chain(
            immutable_chronology
                .iter()
                .filter(|block| model_context_block_is_protected_barrier(block)),
        )
        .map(model_context_block_words)
        .fold(0usize, usize::saturating_add);
    if protected_words > context_budget_words {
        return Err(AgentContextError::new(format!(
            "unrecoverable model context overflow: protected exact context requires {protected_words} words but provider budget is {context_budget_words}; direct user and task instructions cannot be truncated or summarized"
        )));
    }
    let execution_groups = model_context_execution_group_ranges(context);
    let eligible_groups = execution_groups
        .iter()
        .enumerate()
        .filter(|(_, group)| {
            !immutable_chronology[(*group).clone()]
                .iter()
                .any(model_context_block_is_protected_barrier)
                && model_context_group_is_closed_and_consumed(
                    context,
                    group,
                    consumed_sequence_high_water,
                )
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if eligible_groups.is_empty() {
        return Ok(ModelContextCompactionPlan::unchanged(
            blocks,
            ModelContextCompactionReport::default(),
            consumed_sequence_high_water,
        ));
    }
    let tail_budget =
        model_context_retained_tail_budget_words(context_budget_words, retained_tail_percent);
    let retained_groups = model_context_retained_group_indexes(
        &immutable_chronology,
        &execution_groups,
        &eligible_groups,
        tail_budget,
    );
    let replacement_ranges = eligible_groups
        .iter()
        .copied()
        .filter(|group_index| !retained_groups.contains(group_index))
        .map(|group_index| execution_groups[group_index].clone())
        .collect::<Vec<_>>();
    if replacement_ranges.is_empty() {
        return Ok(ModelContextCompactionPlan::unchanged(
            blocks,
            ModelContextCompactionReport::default(),
            consumed_sequence_high_water,
        ));
    }
    let replacement_blocks = replacement_ranges
        .iter()
        .flat_map(|range| immutable_chronology[range.clone()].iter().cloned())
        .collect::<Vec<_>>();
    if replacement_blocks.len() == 1
        && replacement_blocks
            .first()
            .is_some_and(context_block_is_compaction_summary)
    {
        return Ok(ModelContextCompactionPlan::unchanged(
            blocks,
            ModelContextCompactionReport::default(),
            consumed_sequence_high_water,
        ));
    }
    let retained_tail = retained_groups
        .iter()
        .flat_map(|index| {
            immutable_chronology[execution_groups[*index].clone()]
                .iter()
                .cloned()
        })
        .collect::<Vec<_>>();
    let retained_chronology = immutable_chronology
        .iter()
        .enumerate()
        .filter(|(index, _)| !replacement_ranges.iter().any(|range| range.contains(index)))
        .map(|(_, block)| block.clone())
        .collect::<Vec<_>>();
    let summary_budget_words = context_budget_words.saturating_sub(
        model_context_total_words(&stable_prefix)
            .saturating_add(model_context_total_words(&retained_chronology)),
    );
    if summary_budget_words == 0 {
        return Err(AgentContextError::new(
            "unrecoverable model context overflow: no budget remains for a model-authored compaction summary",
        ));
    }
    Ok(ModelContextCompactionPlan {
        consumed_sequence_high_water,
        replacement_event_sequences: replacement_ranges
            .iter()
            .flat_map(|range| {
                context.chronology()[range.clone()]
                    .iter()
                    .map(|event| event.sequence())
            })
            .collect(),
        replacement_blocks: replacement_blocks.clone(),
        replacement_group_lengths: replacement_ranges.iter().map(Range::len).collect(),
        retained_tail,
        summary_budget_words,
        report: ModelContextCompactionReport {
            compacted_blocks: replacement_blocks.len(),
            omitted_blocks: 0,
            omitted_original_words: 0,
        },
    })
}

/// Applies one validated model-authored summary to a previously frozen plan.
pub fn apply_model_context_compaction_plan(
    mut context: AgentContext,
    plan: &ModelContextCompactionPlan,
    model_summary: impl Into<String>,
) -> AgentContextResult<(AgentContext, ModelContextCompactionReport)> {
    context.validate_durable()?;
    let model_summary = model_summary.into();
    if !plan.changes_context() {
        if model_summary.trim().is_empty() {
            return Ok((context, plan.report));
        }
        return Err(AgentContextError::new(
            "model compaction summary was supplied for a no-op plan",
        ));
    }
    if model_summary.trim().is_empty()
        || model_summary.len() > MODEL_CONTEXT_BLOCK_LIMIT_BYTES
        || model_context_text_word_count(&model_summary) > plan.summary_budget_words
    {
        return Err(AgentContextError::new(
            "model compaction summary must be nonempty, bounded, and fit the planned summary budget",
        ));
    }
    let selected = context
        .chronology()
        .iter()
        .enumerate()
        .filter(|(_, event)| plan.replacement_event_sequences.contains(&event.sequence()))
        .collect::<Vec<_>>();
    if selected.len() != plan.replacement_event_sequences.len()
        || selected
            .iter()
            .map(|(_, event)| event.sequence())
            .ne(plan.replacement_event_sequences.iter().copied())
        || selected
            .iter()
            .map(|(_, event)| event.block().clone())
            .ne(plan.replacement_blocks.iter().cloned())
        || selected.iter().any(|(_, event)| {
            event.sequence().get() > plan.consumed_sequence_high_water
                || event.retention() == ContextRetention::Exact
                || !event.recoverable_for_compaction()
        })
    {
        return Err(AgentContextError::new(
            "model compaction plan no longer matches the durable chronology",
        ));
    }
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for (index, _) in selected {
        match ranges.last_mut() {
            Some(range) if range.end == index => range.end = range.end.saturating_add(1),
            _ => ranges.push(index..index.saturating_add(1)),
        }
    }
    context.compact_execution_ranges_into_summary(
        ranges,
        ContextBlock::reference_event(
            ContextSourceKind::Memory,
            "context compaction summary",
            model_summary,
        ),
    )?;
    Ok((context, plan.report))
}

/// Counts whitespace-delimited words for context budgeting.
pub fn model_context_text_word_count(value: &str) -> usize {
    value.split_whitespace().count()
}

/// Returns whether one compactable segment is causally closed at the frozen
/// provider-consumed sequence boundary.
fn model_context_group_is_closed_and_consumed(
    context: &AgentContext,
    group: &Range<usize>,
    consumed_sequence_high_water: u64,
) -> bool {
    let events = context.chronology();
    if events[group.clone()].iter().any(|event| {
        event.sequence().get() > consumed_sequence_high_water || !event.recoverable_for_compaction()
    }) {
        return false;
    }
    let Some(group_id) = events[group.start].execution_group_id() else {
        return true;
    };
    if group.end >= events.len()
        && !events[group.clone()]
            .iter()
            .any(|event| event.semantic_kind() == ContextSemanticKind::EvidenceEvent)
    {
        return false;
    }
    !events[..group.start]
        .iter()
        .chain(events[group.end..].iter())
        .any(|event| event.execution_group_id() == Some(group_id))
}

/// Groups typed chronology at indivisible provider-execution boundaries.
///
/// Explicit execution-group identity is the sole ownership signal. Exact task,
/// prompt, steering, and message events have no group and form their own
/// barriers, so the compactor cannot infer attachment from labels or source
/// adjacency after an event has committed.
fn model_context_execution_group_ranges(context: &AgentContext) -> Vec<Range<usize>> {
    let events = context.chronology();
    let mut groups = Vec::new();
    let mut start = 0usize;
    for index in 1..events.len() {
        let previous = &events[index - 1];
        let current = &events[index];
        let same_execution_group = previous.execution_group_id().is_some()
            && previous.execution_group_id() == current.execution_group_id();
        if !same_execution_group {
            groups.push(start..index);
            start = index;
        }
    }
    if start < events.len() {
        groups.push(start..events.len());
    }
    groups
}

/// Returns whether a block is an exact, non-crossable compaction barrier.
fn model_context_block_is_protected_barrier(block: &ContextBlock) -> bool {
    block.retention() == ContextRetention::Exact
}

/// Returns the provider-request word cost of one block.
fn model_context_block_words(block: &ContextBlock) -> usize {
    model_context_text_word_count(&model_context_block_header(block))
        .saturating_add(model_context_text_word_count(&block.content))
}

/// Returns the aggregate provider-request word cost for blocks.
fn model_context_total_words(blocks: &[ContextBlock]) -> usize {
    blocks
        .iter()
        .map(model_context_block_words)
        .fold(0usize, usize::saturating_add)
}

/// Returns the retained raw-tail word budget.
fn model_context_retained_tail_budget_words(
    context_budget_words: usize,
    retained_tail_percent: usize,
) -> usize {
    context_budget_words
        .saturating_mul(normalize_model_context_retained_tail_percent(
            retained_tail_percent,
        ))
        .saturating_div(100)
        .max(1)
}

/// Clamps retained-tail percentages to the supported range.
fn normalize_model_context_retained_tail_percent(retained_tail_percent: usize) -> usize {
    retained_tail_percent.clamp(1, 100)
}

/// Finds the first complete execution group in the retained raw suffix.
fn model_context_retained_group_indexes(
    blocks: &[ContextBlock],
    groups: &[Range<usize>],
    eligible_groups: &[usize],
    tail_budget_words: usize,
) -> Vec<usize> {
    let mut retained_words = 0usize;
    let mut retained = Vec::new();
    for group_index in eligible_groups.iter().copied().rev() {
        let group = &groups[group_index];
        if blocks[group.clone()]
            .iter()
            .any(context_block_is_compaction_summary)
        {
            continue;
        }
        if blocks[group.clone()]
            .iter()
            .any(|block| block.content.len() > MODEL_CONTEXT_BLOCK_LIMIT_BYTES)
        {
            continue;
        }
        let group_words = model_context_total_words(&blocks[group.clone()]);
        if retained_words.saturating_add(group_words) > tail_budget_words {
            continue;
        }
        retained_words = retained_words.saturating_add(group_words);
        retained.push(group_index);
    }
    retained.sort_unstable();
    retained
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies model-summary planning does not mutate durable chronology and
    /// applies one validated model-authored summary at the frozen event anchor.
    #[test]
    fn model_context_compaction_plan_applies_model_summary_without_local_epoch() {
        let context = AgentContext::new_durable(vec![
            ContextBlock::assistant_event("older decision", "decision ".repeat(300)),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "older outcome",
                "outcome ".repeat(300),
            ),
        ])
        .unwrap();
        let original = context.clone();
        let plan = plan_model_context_compaction_at_consumed_sequence(
            &context,
            1_000,
            1,
            context.event_sequence_high_water_mark(),
        )
        .unwrap();

        assert!(plan.changes_context());
        assert_eq!(context, original);
        assert_eq!(plan.replacement_blocks().len(), 2);
        assert!(
            !plan.replacement_blocks()[0]
                .content
                .contains("[context compacted]")
        );

        let (compacted, report) = apply_model_context_compaction_plan(
            context,
            &plan,
            "The model-authored summary preserves the earlier decision and outcome.",
        )
        .unwrap();
        assert_eq!(report, plan.report());
        assert_eq!(compacted.chronology().len(), 1);
        assert_eq!(
            compacted.chronology()[0].block().content,
            "The model-authored summary preserves the earlier decision and outcome."
        );
    }

    /// Verifies application resolves selected history by stable event identity
    /// and preserves a post-boundary event byte-for-byte.
    #[test]
    fn model_context_compaction_plan_preserves_post_boundary_events() {
        let mut context = AgentContext::new_durable(vec![
            ContextBlock::assistant_event("older decision", "decision ".repeat(300)),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "older outcome",
                "outcome ".repeat(300),
            ),
        ])
        .unwrap();
        let plan = plan_model_context_compaction_at_consumed_sequence(
            &context,
            1_000,
            1,
            context.event_sequence_high_water_mark(),
        )
        .unwrap();
        let post_boundary = "preserve this later steering exactly".to_string();
        context
            .append_user_event("later steering", post_boundary.clone())
            .unwrap();

        let (compacted, _) = apply_model_context_compaction_plan(
            context,
            &plan,
            "A model-authored summary of the earlier closed history.",
        )
        .unwrap();
        assert_eq!(compacted.chronology().len(), 2);
        assert_eq!(compacted.chronology()[1].block().content, post_boundary);
    }

    /// Verifies progressive model-request backoff moves exactly the newest
    /// complete selected group into the exact retained suffix.
    #[test]
    fn model_context_compaction_plan_excludes_newest_complete_group() {
        let context = AgentContext::new_durable(vec![
            ContextBlock::assistant_event("older decision", "decision ".repeat(300)),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "older outcome",
                "outcome ".repeat(300),
            ),
            ContextBlock::assistant_event("newer decision", "newer ".repeat(300)),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "newer outcome",
                "result ".repeat(300),
            ),
        ])
        .unwrap();
        let mut plan = plan_model_context_compaction_at_consumed_sequence(
            &context,
            2_000,
            1,
            context.event_sequence_high_water_mark(),
        )
        .unwrap();
        let original_blocks = plan.replacement_blocks().to_vec();

        assert!(plan.exclude_newest_replacement_group());
        assert_eq!(plan.replacement_blocks(), &original_blocks[..2]);
        assert_eq!(plan.retained_tail(), &original_blocks[2..]);
        assert!(!plan.exclude_newest_replacement_group());
    }

    /// Verifies a model-authored summary cannot apply after selected source
    /// history changes, preserving the current durable chronology unchanged.
    #[test]
    fn model_context_compaction_plan_rejects_stale_selected_history() {
        let mut context = AgentContext::new_durable(vec![
            ContextBlock::assistant_event("older decision", "decision ".repeat(300)),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "older outcome",
                "outcome ".repeat(300),
            ),
        ])
        .unwrap();
        let plan = plan_model_context_compaction_at_consumed_sequence(
            &context,
            1_000,
            1,
            context.event_sequence_high_water_mark(),
        )
        .unwrap();
        context
            .replace_after_compaction(vec![ContextBlock::user_event(
                "new exact prompt",
                "do not replace this context",
            )])
            .unwrap();
        let original = context.clone();

        let error = apply_model_context_compaction_plan(
            context,
            &plan,
            "A summary that no longer matches its selected source history.",
        )
        .unwrap_err();

        assert!(error.message().contains("no longer matches"));
        assert_eq!(original.blocks()[0].content, "do not replace this context");
    }
}
