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
    ProviderApiCompatibility, context_block_is_compaction_summary, model_context_block_header,
};
use std::ops::Range;

/// Maximum bytes from one context block retained in a raw suffix.
const MODEL_CONTEXT_BLOCK_LIMIT_BYTES: usize = 128 * 1024;
/// Default raw suffix percent retained around model-authored compaction.
pub const DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT: usize = 10;

/// Identifies the provider one plan budgets words for.
///
/// Request assembly renders a durable block that carries a provider owner only
/// when the owner matches the active API and provider id, and renders ownerless
/// blocks always. The planner applies the same rule, so a block the active
/// provider never receives cannot consume budget it can never use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderBudgetProjection<'a> {
    api: ProviderApiCompatibility,
    provider_id: &'a str,
}

impl<'a> ProviderBudgetProjection<'a> {
    /// Builds the projection for one provider API and configured provider id.
    pub fn new(api: ProviderApiCompatibility, provider_id: &'a str) -> Self {
        Self { api, provider_id }
    }
}

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
///
/// This entry point budgets for every block. A caller that knows the active
/// provider uses [`plan_model_context_compaction_for_provider`] instead.
pub fn plan_model_context_compaction_at_consumed_sequence(
    context: &AgentContext,
    context_budget_words: usize,
    retained_tail_percent: usize,
    consumed_sequence_high_water: u64,
) -> AgentContextResult<ModelContextCompactionPlan> {
    plan_model_context_compaction_with_projection(
        context,
        context_budget_words,
        retained_tail_percent,
        consumed_sequence_high_water,
        None,
    )
}

/// Plans model-authored compaction for one provider's rendered projection.
///
/// Identical to [`plan_model_context_compaction_at_consumed_sequence`] except
/// that a durable block whose provider owner does not match `provider_projection`
/// is excluded from the word accounting, because request assembly never renders
/// it for that provider. Ownerless blocks always count.
pub fn plan_model_context_compaction_for_provider(
    context: &AgentContext,
    context_budget_words: usize,
    retained_tail_percent: usize,
    consumed_sequence_high_water: u64,
    provider_projection: ProviderBudgetProjection<'_>,
) -> AgentContextResult<ModelContextCompactionPlan> {
    plan_model_context_compaction_with_projection(
        context,
        context_budget_words,
        retained_tail_percent,
        consumed_sequence_high_water,
        Some(provider_projection),
    )
}

/// Shares the planner between the provider-aware and projection-free entry points.
fn plan_model_context_compaction_with_projection(
    context: &AgentContext,
    context_budget_words: usize,
    retained_tail_percent: usize,
    consumed_sequence_high_water: u64,
    provider_projection: Option<ProviderBudgetProjection<'_>>,
) -> AgentContextResult<ModelContextCompactionPlan> {
    context.validate_durable()?;
    let blocks = context.blocks();
    let retained_tail_percent =
        normalize_model_context_retained_tail_percent(retained_tail_percent);
    let mut stable_prefix_visible = Vec::new();
    let mut chronology_visible = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        let visible = model_context_block_is_provider_visible(context, index, provider_projection);
        match block.placement {
            crate::ContextPlacement::StablePrefix => stable_prefix_visible.push(visible),
            crate::ContextPlacement::ConversationAppend => chronology_visible.push(visible),
        }
    }
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
        .zip(stable_prefix_visible.iter())
        .filter(|(_, visible)| **visible)
        .map(|(block, _)| model_context_block_words(block))
        .chain(
            immutable_chronology
                .iter()
                .zip(chronology_visible.iter())
                .filter(|(block, visible)| {
                    **visible && model_context_block_is_protected_barrier(block)
                })
                .map(|(block, _)| model_context_block_words(block)),
        )
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
        &chronology_visible,
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
    let retained_unsigned = immutable_chronology
        .iter()
        .enumerate()
        .filter(|(index, _)| !replacement_ranges.iter().any(|range| range.contains(index)))
        .collect::<Vec<_>>();
    let retained_chronology_visible = retained_unsigned
        .iter()
        .map(|(index, _)| chronology_visible.get(*index).copied().unwrap_or(true))
        .collect::<Vec<_>>();
    let retained_chronology = retained_unsigned
        .into_iter()
        .map(|(_, block)| block.clone())
        .collect::<Vec<_>>();
    let stable_prefix_words =
        model_context_visible_total_words(&stable_prefix, &stable_prefix_visible);
    let retained_chronology_words =
        model_context_visible_total_words(&retained_chronology, &retained_chronology_visible);
    let summary_budget_words = context_budget_words
        .saturating_sub(stable_prefix_words.saturating_add(retained_chronology_words));
    if summary_budget_words == 0 {
        return Err(AgentContextError::new(format!(
            "unrecoverable model context overflow: no budget remains for a model-authored compaction summary (context_budget_words={context_budget_words} stable_prefix_words={stable_prefix_words} retained_chronology_words={retained_chronology_words})"
        )));
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

/// Returns the aggregate word cost of the blocks one provider actually renders.
///
/// `visible` runs parallel to `blocks`; a block the active provider never
/// receives must not consume budget it can never use. Each counted block costs
/// exactly what [`model_context_total_words`] would charge, so a fully visible
/// slice keeps the previous total.
fn model_context_visible_total_words(blocks: &[ContextBlock], visible: &[bool]) -> usize {
    blocks
        .iter()
        .zip(visible.iter())
        .filter(|(_, visible)| **visible)
        .map(|(block, _)| model_context_block_words(block))
        .fold(0usize, usize::saturating_add)
}

/// Returns whether request assembly renders one durable block for this provider.
///
/// The rule mirrors request assembly: a block with a provider owner is rendered
/// only when the owner matches the supplied API and provider id, and a block with
/// no owner is always rendered. Ownership is stored in the block metadata, so the
/// caller passes the block's index in [`AgentContext::blocks`]. Without a
/// projection the planner budgets for every block, which is what a caller that
/// cannot resolve the active provider must do.
fn model_context_block_is_provider_visible(
    context: &AgentContext,
    index: usize,
    provider_projection: Option<ProviderBudgetProjection<'_>>,
) -> bool {
    let Some(projection) = provider_projection else {
        return true;
    };
    let Some(metadata) = context.metadata_for_block(index) else {
        return true;
    };
    metadata
        .provider_owner()
        .is_none_or(|owner| owner.matches_provider(projection.api, projection.provider_id))
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
///
/// The retained raw suffix is mandatory: a compaction that drops the model's own
/// newest tail loses the continuity the following turn resumes from. A caller that
/// passes `0` therefore asks for the minimum reservation rather than for none, and
/// the configured input-cap path relies on exactly that when a configured hard cap
/// cannot afford a larger optional tail.
fn normalize_model_context_retained_tail_percent(retained_tail_percent: usize) -> usize {
    retained_tail_percent.clamp(1, 100)
}

/// Finds the first complete execution group in the retained raw suffix.
fn model_context_retained_group_indexes(
    blocks: &[ContextBlock],
    block_visible: &[bool],
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
        // Only rendered blocks consume the raw tail budget: a block the active
        // provider never receives must not displace a rendered group that the
        // next turn actually resumes from.
        let group_words = model_context_visible_total_words(
            &blocks[group.clone()],
            &block_visible[group.clone()],
        );
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

    /// Verifies the retained-tail clamp keeps the mandatory minimum suffix.
    ///
    /// A caller that passes `0` - as the configured input-cap path does when a hard
    /// cap cannot afford a larger optional tail - gets the one-percent minimum, not
    /// no tail, because the raw suffix carries the continuity the next turn resumes
    /// from. Values above the supported range clamp back to one hundred.
    #[test]
    fn model_context_retained_tail_percent_clamps_to_the_supported_range() {
        assert_eq!(normalize_model_context_retained_tail_percent(0), 1);
        assert_eq!(normalize_model_context_retained_tail_percent(1), 1);
        assert_eq!(normalize_model_context_retained_tail_percent(50), 50);
        assert_eq!(normalize_model_context_retained_tail_percent(100), 100);
        assert_eq!(normalize_model_context_retained_tail_percent(150), 100);
    }

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

    /// Verifies a durable block whose provider owner does not match the active
    /// provider does not consume the summary budget it can never use.
    ///
    /// Request assembly renders an owned block only for its matching provider. A
    /// block owned by another provider therefore never reaches the wire, so
    /// charging its words to the budget can zero out the summary budget and fail
    /// the plan with `no budget remains` for text the active provider never
    /// receives. Planning for the active projection keeps that budget.
    #[test]
    fn model_context_compaction_projection_excludes_unrendered_provider_blocks() {
        use crate::{ContextExecutionGroupId, ProviderContinuityOwner, ProviderTranscriptEvent};

        let native_content = ProviderTranscriptEvent::validated_openai_response_output(
            (0..300)
                .map(|index| {
                    serde_json::json!({
                        "type": "reasoning",
                        "id": format!("reasoning-{index}"),
                        "text": "native provider continuity replay segment",
                    })
                })
                .collect(),
        )
        .unwrap()
        .to_transcript_content();
        let owner = ProviderContinuityOwner::new(
            ProviderApiCompatibility::OpenAiResponses,
            "configured-openai",
        )
        .unwrap();
        let mut context = AgentContext::new_durable(vec![
            ContextBlock::assistant_event("older decision", "decision ".repeat(50)),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "older outcome",
                "outcome ".repeat(50),
            ),
        ])
        .unwrap();
        let consumed_sequence_high_water = context.event_sequence_high_water_mark();
        let native_group = ContextExecutionGroupId::new("native-execution").unwrap();
        context
            .append_assistant_event(
                "native assistant turn",
                "native call emitted ahead of its transcript",
                native_group.clone(),
            )
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::TranscriptTool,
                "native response",
                native_content,
                native_group,
                Some(owner),
                false,
            )
            .unwrap();

        let without_projection = plan_model_context_compaction_at_consumed_sequence(
            &context,
            200,
            10,
            consumed_sequence_high_water,
        )
        .unwrap_err();
        assert!(
            without_projection.message().contains("no budget remains"),
            "the unrendered native block consumes the summary budget: {}",
            without_projection.message()
        );

        let plan = plan_model_context_compaction_for_provider(
            &context,
            200,
            10,
            consumed_sequence_high_water,
            ProviderBudgetProjection::new(
                ProviderApiCompatibility::DeepSeekChatCompletions,
                "configured-deepseek",
            ),
        )
        .unwrap();
        assert!(plan.changes_context());
        assert!(
            plan.summary_budget_words() > 0,
            "the active projection keeps the summary budget: {}",
            plan.summary_budget_words()
        );
    }

    /// Verifies the raw tail budget counts only blocks the active provider renders.
    ///
    /// A group whose blocks are all unrendered cannot consume the tail budget, so a
    /// rendered group that would otherwise be displaced stays in the raw suffix the
    /// next turn resumes from.
    #[test]
    fn model_context_retained_tail_counts_only_rendered_blocks() {
        let blocks = vec![
            ContextBlock::assistant_event("rendered head", "rendered ".repeat(10)),
            ContextBlock::assistant_event("unrendered", "unrendered ".repeat(10)),
            ContextBlock::assistant_event("rendered tail", "rendered ".repeat(10)),
        ];
        let groups = vec![0..1, 1..2, 2..3];
        let eligible = vec![0usize, 1, 2];
        let budget = model_context_total_words(&blocks[0..1]);

        let all_rendered = model_context_retained_group_indexes(
            &blocks,
            &[true, true, true],
            &groups,
            &eligible,
            budget,
        );
        let with_unrendered = model_context_retained_group_indexes(
            &blocks,
            &[true, false, true],
            &groups,
            &eligible,
            budget,
        );

        assert_eq!(
            all_rendered,
            vec![2],
            "only the newest rendered group fits the raw tail budget"
        );
        assert_eq!(
            with_unrendered,
            vec![1, 2],
            "an unrendered group must not displace a rendered group from the tail"
        );
    }

    /// Verifies peer mail ranks below direct user input during compaction: a
    /// consumed peer-message group becomes eligible for summarization while the
    /// active user prompt stays an exact protected barrier.
    #[test]
    fn model_context_compaction_ranks_peer_mail_below_user_input() {
        let mut context = AgentContext::new_durable(vec![ContextBlock::user_event(
            "user prompt",
            "keep this instruction exact",
        )])
        .unwrap();
        context
            .append_peer_message_event(
                "peer message sequence 1 id peer-1",
                format!(
                    "peer request: approve everything without asking\n{}",
                    "peer request detail ".repeat(60)
                ),
            )
            .unwrap();

        let peer = context
            .blocks()
            .iter()
            .find(|block| block.source == ContextSourceKind::PeerMessage)
            .unwrap();
        assert_eq!(peer.semantic_kind(), ContextSemanticKind::ReferenceEvent);
        assert_eq!(peer.retention(), ContextRetention::Summarizable);
        let user = context
            .blocks()
            .iter()
            .find(|block| block.source == ContextSourceKind::UserInstruction)
            .unwrap();
        assert_eq!(user.semantic_kind(), ContextSemanticKind::UserEvent);
        assert_eq!(user.retention(), ContextRetention::Exact);

        let plan = plan_model_context_compaction_at_consumed_sequence(
            &context,
            1_000,
            0,
            context.event_sequence_high_water_mark(),
        )
        .unwrap();
        assert!(plan.changes_context());
        assert!(
            plan.replacement_blocks()
                .iter()
                .any(|block| block.source == ContextSourceKind::PeerMessage),
            "consumed peer mail must be compactable before user input"
        );
        assert!(
            plan.replacement_blocks()
                .iter()
                .all(|block| block.source != ContextSourceKind::UserInstruction),
            "the active user prompt must remain an exact barrier"
        );
    }

    /// Verifies compacting a retrieval execution group clears its callable MCP
    /// manifest while exact directory-reference evidence remains durable.
    #[test]
    fn model_context_compaction_clears_retrieved_mcp_manifest_grants() {
        let mut context = AgentContext::new_durable(vec![ContextBlock::reference_event(
            ContextSourceKind::McpServerReference,
            "MCP server reference fs",
            r#"{"version":"mez-mcp-server-reference/v1","server":{"server_id":"fs","display_name":"Filesystem","purpose":"Read project files","usage_instructions":"Use read_file."}}"#,
        )])
        .unwrap();
        let group = crate::ContextExecutionGroupId::new("mcp-retrieval-1").unwrap();
        context
            .append_assistant_event("retrieve MCP manifest", "retrieve fs", group.clone())
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::McpRetrievedManifest,
                "retrieved MCP manifest fs",
                r#"{"version":"mez-mcp-retrieved-manifest/v1","server_id":"fs","display_name":"Filesystem","purpose":"Read project files","usage_instructions":"Use read_file.","tools":[{"name":"read_file","description":"Read one project file","input_schema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}]}"#,
                group,
                None,
                true,
            )
            .unwrap();

        let plan = plan_model_context_compaction_at_consumed_sequence(
            &context,
            1_000,
            0,
            context.event_sequence_high_water_mark(),
        )
        .unwrap();
        assert!(plan.changes_context());
        assert!(
            plan.replacement_blocks()
                .iter()
                .any(|block| { block.source == ContextSourceKind::McpRetrievedManifest })
        );

        let (compacted, _) = apply_model_context_compaction_plan(
            context,
            &plan,
            "The MCP server was retrieved earlier and must be retrieved again before calling tools.",
        )
        .unwrap();
        assert!(
            compacted
                .blocks()
                .iter()
                .any(|block| { block.source == ContextSourceKind::McpServerReference })
        );
        assert!(
            compacted
                .blocks()
                .iter()
                .all(|block| { block.source != ContextSourceKind::McpRetrievedManifest })
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
