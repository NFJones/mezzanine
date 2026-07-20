//! Provider-independent model-context compaction and budgeting.
//!
//! This module owns deterministic bulk compaction after an explicit trigger or
//! provider context-limit response. It freezes the provider-consumed event
//! boundary, replaces only closed contiguous ranges at their original anchors,
//! keeps exact barriers and straddling causal owners raw, preserves retained
//! event identities, and emits deterministic semantic recovery indexes without
//! product runtime or persistence dependencies.

use crate::{
    AgentContext, AgentContextError, AgentContextResult, ContextBlock, ContextRetention,
    ContextSemanticKind, ContextSourceKind, ModelContextCompactionReport,
    context_block_is_compaction_summary, model_context_block_header,
};
use std::ops::Range;

/// Maximum bytes from one context block retained in a raw suffix.
const MODEL_CONTEXT_BLOCK_LIMIT_BYTES: usize = 128 * 1024;
/// Marker used for deterministic local compaction summaries.
const MODEL_CONTEXT_COMPACTED_PREFIX: &str = "[context compacted]";
/// Default raw suffix percent retained after local context compaction.
pub const DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT: usize = 10;

/// Validated replacement plan produced before typed chronology is mutated.
struct ModelContextCompactionPlan {
    blocks: Vec<ContextBlock>,
    replacement_ranges: Vec<Range<usize>>,
    replacement_summary: Option<ContextBlock>,
    report: ModelContextCompactionReport,
}

impl ModelContextCompactionPlan {
    /// Constructs a no-op plan that preserves the supplied provider projection.
    fn unchanged(blocks: &[ContextBlock], report: ModelContextCompactionReport) -> Self {
        Self {
            blocks: blocks.to_vec(),
            replacement_ranges: Vec::new(),
            replacement_summary: None,
            report,
        }
    }
}

/// Compacts provider-bound context with the default retained-tail percentage.
pub fn compact_model_context_for_budget(
    context: AgentContext,
    context_budget_words: usize,
) -> AgentContextResult<(AgentContext, ModelContextCompactionReport)> {
    compact_model_context_for_budget_with_retained_tail_percent(
        context,
        context_budget_words,
        DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT,
    )
}

/// Compacts provider-bound context with one configured raw-tail percentage.
pub fn compact_model_context_for_budget_with_retained_tail_percent(
    context: AgentContext,
    context_budget_words: usize,
    retained_tail_percent: usize,
) -> AgentContextResult<(AgentContext, ModelContextCompactionReport)> {
    let consumed_sequence_high_water = context.event_sequence_high_water_mark();
    compact_model_context_for_budget_at_consumed_sequence(
        context,
        context_budget_words,
        retained_tail_percent,
        consumed_sequence_high_water,
    )
}

/// Compacts one provider-consumed context snapshot at an explicit causal
/// boundary.
///
/// Events committed after `consumed_sequence_high_water` remain raw even when
/// the caller's stored context already contains them. Runtime recovery uses the
/// high-water mark captured by the rejected provider request so concurrent
/// steering, messages, and action results cannot be swept into an older
/// request's compaction epoch.
pub fn compact_model_context_for_budget_at_consumed_sequence(
    context: AgentContext,
    context_budget_words: usize,
    retained_tail_percent: usize,
    consumed_sequence_high_water: u64,
) -> AgentContextResult<(AgentContext, ModelContextCompactionReport)> {
    context.validate_durable()?;
    let ModelContextCompactionPlan {
        blocks,
        replacement_ranges,
        replacement_summary,
        report,
    } = compact_model_context_blocks(
        &context,
        context_budget_words,
        true,
        retained_tail_percent,
        consumed_sequence_high_water,
    )?;
    let Some(replacement_summary) = replacement_summary else {
        return Ok((context, report));
    };
    let mut compacted = context;
    compacted.compact_execution_ranges_into_summary(replacement_ranges, replacement_summary)?;
    if compacted.blocks() != blocks {
        return Err(AgentContextError::new(
            "typed compaction projection differs from the validated compaction plan",
        ));
    }
    Ok((compacted, report))
}

/// Counts whitespace-delimited words for context budgeting.
pub fn model_context_text_word_count(value: &str) -> usize {
    value.split_whitespace().count()
}

/// Applies summary-plus-tail compaction to ordered context blocks.
fn compact_model_context_blocks(
    context: &AgentContext,
    context_budget_words: usize,
    force: bool,
    retained_tail_percent: usize,
    consumed_sequence_high_water: u64,
) -> AgentContextResult<ModelContextCompactionPlan> {
    let blocks = context.blocks();
    let retained_tail_percent =
        normalize_model_context_retained_tail_percent(retained_tail_percent);
    let report = ModelContextCompactionReport::default();
    let total_words = model_context_total_words(blocks);
    let oversized_block_present = blocks
        .iter()
        .any(|block| block.content.len() > MODEL_CONTEXT_BLOCK_LIMIT_BYTES);
    if !force && total_words <= context_budget_words && !oversized_block_present {
        return Ok(ModelContextCompactionPlan::unchanged(blocks, report));
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
        return Ok(ModelContextCompactionPlan::unchanged(blocks, report));
    }
    let tail_budget =
        model_context_retained_tail_budget_words(context_budget_words, retained_tail_percent);
    let retained_groups = model_context_retained_group_indexes(
        &immutable_chronology,
        &execution_groups,
        &eligible_groups,
        tail_budget,
    );
    let retained_tail = retained_groups
        .iter()
        .flat_map(|index| {
            immutable_chronology[execution_groups[*index].clone()]
                .iter()
                .cloned()
        })
        .collect::<Vec<_>>();
    let replacement_ranges = eligible_groups
        .iter()
        .copied()
        .filter(|group_index| !retained_groups.contains(group_index))
        .map(|group_index| execution_groups[group_index].clone())
        .collect::<Vec<_>>();
    if replacement_ranges.is_empty() {
        return Ok(ModelContextCompactionPlan::unchanged(blocks, report));
    }
    let compacted_blocks = replacement_ranges
        .iter()
        .flat_map(|range| immutable_chronology[range.clone()].iter().cloned())
        .collect::<Vec<_>>();
    if compacted_blocks.len() == 1
        && compacted_blocks
            .first()
            .is_some_and(context_block_is_compaction_summary)
    {
        return Ok(ModelContextCompactionPlan::unchanged(blocks, report));
    }
    let retained_chronology = immutable_chronology
        .iter()
        .enumerate()
        .filter(|(index, _)| !replacement_ranges.iter().any(|range| range.contains(index)))
        .map(|(_, block)| block.clone())
        .collect::<Vec<_>>();
    let fixed_words = model_context_total_words(&stable_prefix)
        .saturating_add(model_context_total_words(&retained_chronology));
    let summary_budget_words = context_budget_words.saturating_sub(fixed_words);
    let (summary, omitted_blocks, omitted_original_words) = bulk_compacted_model_context_block(
        &compacted_blocks,
        &retained_tail,
        tail_budget,
        retained_tail_percent,
        summary_budget_words,
    );
    let insertion_index = replacement_ranges
        .first()
        .expect("replacement ranges are non-empty")
        .start;
    let mut prepared = Vec::with_capacity(
        stable_prefix
            .len()
            .saturating_add(retained_chronology.len())
            .saturating_add(1),
    );
    prepared.extend(stable_prefix);
    for (index, block) in immutable_chronology.iter().enumerate() {
        if index == insertion_index {
            prepared.push(summary.clone());
        }
        if replacement_ranges
            .iter()
            .any(|range| range.contains(&index))
        {
            continue;
        }
        prepared.push(block.clone());
    }
    let report = ModelContextCompactionReport {
        compacted_blocks: compacted_blocks.len(),
        omitted_blocks,
        omitted_original_words,
    };
    let compacted_words = model_context_total_words(&prepared);
    if compacted_words > context_budget_words {
        return Err(AgentContextError::new(format!(
            "unrecoverable model context overflow: minimum barrier-preserving compacted context requires {compacted_words} words but provider budget is {context_budget_words}"
        )));
    }

    Ok(ModelContextCompactionPlan {
        blocks: prepared,
        replacement_ranges,
        replacement_summary: Some(summary),
        report,
    })
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

/// Builds the memory-style block representing compacted context.
fn bulk_compacted_model_context_block(
    compacted_blocks: &[ContextBlock],
    retained_tail: &[ContextBlock],
    tail_budget_words: usize,
    retained_tail_percent: usize,
    summary_budget_words: usize,
) -> (ContextBlock, usize, usize) {
    let compacted_original_words = model_context_total_words(compacted_blocks);
    let retained_tail_words = model_context_total_words(retained_tail);
    let mut lines = vec![
        MODEL_CONTEXT_COMPACTED_PREFIX.to_string(),
        "Local context was recursively compacted before provider request assembly.".to_string(),
        format!("compacted_blocks={}", compacted_blocks.len()),
        format!("compacted_original_words={compacted_original_words}"),
        format!("retained_tail_blocks={}", retained_tail.len()),
        format!("retained_tail_words={retained_tail_words}"),
        format!("retained_tail_budget_words={tail_budget_words}"),
        format!(
            "retained_tail_percent={}",
            normalize_model_context_retained_tail_percent(retained_tail_percent)
        ),
        "Semantic recovery records:".to_string(),
    ];
    let recovery_lines = compacted_blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            format!(
                "{}. source={} label={} categories={} summary={:?} original_words={}",
                index.saturating_add(1),
                model_context_source_kind_name(block.source),
                model_context_single_line(&block.label),
                model_context_semantic_categories(block),
                model_context_semantic_excerpt(&block.content),
                model_context_block_words(block)
            )
        })
        .collect::<Vec<_>>();
    let footer =
        "Every replaced source record is indexed above. These records preserve decision-relevant semantics, not exact source bytes. Recover exact details from transcript, action, memory, or artifact storage with targeted read/search/capture actions before relying on an excerpted detail."
            .to_string();
    let fixed_words = lines
        .iter()
        .map(|line| model_context_text_word_count(line))
        .fold(
            model_context_text_word_count(&footer),
            usize::saturating_add,
        );
    let mut selected = Vec::new();
    let mut selected_words = fixed_words;
    let mut omitted_blocks = 0usize;
    let mut omitted_original_words = 0usize;
    for (block, line) in compacted_blocks.iter().zip(&recovery_lines).rev() {
        let line_words = model_context_text_word_count(line);
        if selected_words.saturating_add(line_words) <= summary_budget_words {
            selected.push(line.clone());
            selected_words = selected_words.saturating_add(line_words);
        } else {
            omitted_blocks = omitted_blocks.saturating_add(1);
            omitted_original_words =
                omitted_original_words.saturating_add(model_context_block_words(block));
        }
    }
    selected.reverse();
    lines.extend(selected);
    lines.push(footer);
    (
        ContextBlock {
            source: ContextSourceKind::Memory,
            placement: crate::ContextPlacement::ConversationAppend,
            label: "context compaction summary".to_string(),
            content: lines.join("\n"),
        },
        omitted_blocks,
        omitted_original_words,
    )
}

/// Classifies decision-relevant semantics retained by one compacted record.
fn model_context_semantic_categories(block: &ContextBlock) -> String {
    let normalized = block.content.to_ascii_lowercase();
    let mut categories = Vec::new();
    match block.source {
        ContextSourceKind::TranscriptAssistant => categories.push("decision"),
        ContextSourceKind::TranscriptTool
        | ContextSourceKind::ActionResult
        | ContextSourceKind::CommittedEvidence
        | ContextSourceKind::RoutedHandoff => categories.push("outcome"),
        _ => categories.push("fact"),
    }
    if ["error", "failed", "failure", "denied", "blocked"]
        .iter()
        .any(|needle| normalized.contains(needle))
    {
        categories.push("error");
    }
    if [
        "artifact", "path=", "file=", "commit", "created ", "updated ",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
    {
        categories.push("artifact");
    }
    if ["unresolved", "remaining", "pending", "todo", "follow up"]
        .iter()
        .any(|needle| normalized.contains(needle))
    {
        categories.push("unresolved");
    }
    categories.join(",")
}

/// Extracts a bounded semantic excerpt, prioritizing lines that encode errors,
/// decisions, artifacts, outcomes, or unresolved obligations.
fn model_context_semantic_excerpt(content: &str) -> String {
    const MAX_EXCERPT_CHARS: usize = 120;
    const MAX_EXCERPT_LINES: usize = 6;
    const IMPORTANT_TERMS: [&str; 20] = [
        "decision",
        "error",
        "failed",
        "failure",
        "denied",
        "blocked",
        "status",
        "result",
        "output",
        "command",
        "artifact",
        "path",
        "file",
        "commit",
        "created",
        "updated",
        "unresolved",
        "remaining",
        "pending",
        "summary=",
    ];
    let normalized_lines = content
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let mut selected = Vec::new();
    for line in &normalized_lines {
        let normalized = line.to_ascii_lowercase();
        if IMPORTANT_TERMS
            .iter()
            .any(|needle| normalized.contains(needle))
            && !selected.contains(line)
        {
            selected.push(line.clone());
            if selected.len() == MAX_EXCERPT_LINES {
                break;
            }
        }
    }
    for line in &normalized_lines {
        if selected.len() == MAX_EXCERPT_LINES {
            break;
        }
        if !selected.contains(line) {
            selected.push(line.clone());
        }
    }
    let excerpt = selected.join(" | ");
    if excerpt.chars().count() <= MAX_EXCERPT_CHARS {
        return excerpt;
    }
    let mut bounded = excerpt.chars().take(MAX_EXCERPT_CHARS).collect::<String>();
    bounded.push('…');
    bounded
}

/// Returns a stable source label for compaction diagnostics.
fn model_context_source_kind_name(source: ContextSourceKind) -> &'static str {
    match source {
        ContextSourceKind::System => "system",
        ContextSourceKind::UserInstruction => "user_instruction",
        ContextSourceKind::SkillInstruction => "skill_instruction",
        ContextSourceKind::DeveloperInstruction => "developer_instruction",
        ContextSourceKind::Policy => "policy",
        ContextSourceKind::Configuration => "configuration",
        ContextSourceKind::LocalMessage => "local_message",
        ContextSourceKind::RuntimeHint => "runtime_hint",
        ContextSourceKind::ProjectGuidance => "project_guidance",
        ContextSourceKind::Memory => "memory",
        ContextSourceKind::Transcript => "transcript",
        ContextSourceKind::TranscriptUser => "transcript_user",
        ContextSourceKind::TranscriptAssistant => "transcript_assistant",
        ContextSourceKind::TranscriptTool => "transcript_tool",
        ContextSourceKind::CommittedEvidence => "committed_evidence",
        ContextSourceKind::RoutedHandoff => "routed_handoff",
        ContextSourceKind::ActionResult => "action_result",
    }
}

/// Escapes line breaks for compact single-line inventory fields.
fn model_context_single_line(value: &str) -> String {
    value.replace('\n', "\\n").replace('\r', "\\r")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContextExecutionGroupId;

    /// Verifies an older provider request cannot compact events committed after
    /// the exact chronology boundary that request consumed.
    #[test]
    fn context_compaction_freezes_at_explicit_provider_consumed_sequence() {
        let mut context = AgentContext::new_durable(vec![ContextBlock::user_event(
            "user prompt",
            "continue the task",
        )])
        .unwrap();
        let consumed_group = ContextExecutionGroupId::new("consumed-group").unwrap();
        context
            .append_assistant_event(
                "consumed assistant action",
                format!("inspect old state {}", "history ".repeat(3_000)),
                consumed_group.clone(),
            )
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "consumed action result",
                format!("old result {}", "evidence ".repeat(3_000)),
                consumed_group,
                None,
                true,
            )
            .unwrap();
        let consumed_sequence_high_water = context.event_sequence_high_water_mark();
        let newer_group = ContextExecutionGroupId::new("newer-group").unwrap();
        context
            .append_assistant_event(
                "newer assistant action",
                "inspect state committed after the rejected request",
                newer_group.clone(),
            )
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "newer action result",
                "new result must remain byte-exact",
                newer_group,
                None,
                true,
            )
            .unwrap();
        let newer_events = context.chronology()[3..].to_vec();

        let (compacted, report) = compact_model_context_for_budget_at_consumed_sequence(
            context,
            1_000,
            1,
            consumed_sequence_high_water,
        )
        .unwrap();

        assert!(report.changed());
        assert_eq!(&compacted.chronology()[2..], newer_events.as_slice());
        assert_eq!(
            compacted.chronology()[1].block().label,
            "context compaction summary"
        );
    }

    /// Verifies one causal execution owner may straddle steering while the
    /// compactor refuses to join either side into a replacement range.
    #[test]
    fn context_compaction_never_crosses_steering_for_straddling_execution_owner() {
        let mut context = AgentContext::new_durable(vec![ContextBlock::user_event(
            "user prompt",
            "run the action",
        )])
        .unwrap();
        let group = ContextExecutionGroupId::new("straddling-action").unwrap();
        context
            .append_assistant_event(
                "assistant action",
                format!("long action request {}", "request ".repeat(2_000)),
                group.clone(),
            )
            .unwrap();
        context
            .append_user_event("user steering", "preserve executed action evidence")
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "action result after steering",
                format!("executed result {}", "result ".repeat(2_000)),
                group,
                None,
                true,
            )
            .unwrap();
        let original = context.clone();

        let (compacted, report) =
            compact_model_context_for_budget_with_retained_tail_percent(context, 500, 1).unwrap();

        assert!(!report.changed());
        assert_eq!(compacted, original);
        assert_eq!(
            compacted
                .chronology()
                .iter()
                .map(|event| event.block().label.as_str())
                .collect::<Vec<_>>(),
            [
                "user prompt",
                "assistant action",
                "user steering",
                "action result after steering",
            ]
        );
    }

    /// Verifies active execution rationale remains byte-exact until the owning
    /// action result settles.
    ///
    /// Compaction cannot preserve causal meaning by retaining a future result
    /// after discarding the assistant rationale that selected the action. An
    /// assistant-only execution group is therefore ineligible for replacement.
    #[test]
    fn context_compaction_preserves_open_execution_rationale() {
        let mut context = AgentContext::new_durable(vec![ContextBlock::user_event(
            "user prompt",
            "fix the issue backlog",
        )])
        .unwrap();
        let group = ContextExecutionGroupId::new("active-issue-execution").unwrap();
        let rationale = format!(
            "rationale: Continue active issue iss-42\nthinking: Active issue: iss-42\n{}",
            "implementation evidence ".repeat(3_000)
        );
        context
            .append_assistant_event("active issue action", rationale.clone(), group)
            .unwrap();
        let original = context.clone();

        let (compacted, report) =
            compact_model_context_for_budget_with_retained_tail_percent(context, 500, 1).unwrap();

        assert!(!report.changed());
        assert_eq!(compacted, original);
        assert_eq!(compacted.chronology()[1].block().content, rationale);
    }

    /// Verifies explicit bulk compaction prefers older recoverable history
    /// before the recent context tail.
    ///
    /// Provider-limit recovery and manual compaction both use this helper after
    /// a concrete trigger has fired, which keeps fresh correction signals
    /// visible while summarizing older recoverable history.
    #[test]
    fn explicit_context_compaction_preserves_recent_recoverable_tail_when_possible() {
        let mut blocks = Vec::new();
        for index in 0..20 {
            blocks.push(ContextBlock {
                source: ContextSourceKind::Transcript,
                placement: crate::ContextPlacement::ConversationAppend,
                label: format!("transcript {index}"),
                content: format!("transcript-{index} {}", "history-word ".repeat(7_000)),
            });
        }

        let (context, report) =
            compact_model_context_for_budget(AgentContext::new(blocks).unwrap(), 80 * 1024)
                .unwrap();

        assert!(report.changed());
        let summary = context
            .blocks()
            .iter()
            .find(|block| block.source == ContextSourceKind::Memory)
            .expect("oldest transcript should be present in summary inventory");
        let recent_history = context
            .blocks()
            .iter()
            .find(|block| block.label == "transcript 19")
            .expect("recent transcript should remain present");

        assert!(summary.content.contains("[context compacted]"));
        assert!(summary.content.contains("label=transcript 0"));
        assert!(recent_history.content.contains("transcript-19"));
        assert!(!recent_history.content.contains("[context compacted]"));
    }

    /// Verifies explicit compaction keeps current execution evidence and repo
    /// guidance exact while folding older unrelated context into a summary.
    ///
    /// Removing generated provider-visible evidence summaries must not make
    /// provider-limit compaction drop the newest raw action-result evidence.
    #[test]
    fn explicit_context_compaction_protects_guidance_and_recent_action_result() {
        let mut blocks = vec![ContextBlock {
            source: ContextSourceKind::ProjectGuidance,
            placement: crate::ContextPlacement::StablePrefix,
            label: "project guidance".to_string(),
            content: "run just test before handoff".to_string(),
        }];
        for index in 0..40 {
            blocks.push(ContextBlock {
                source: ContextSourceKind::Memory,
                placement: crate::ContextPlacement::ConversationAppend,
                label: format!("old memory {index}"),
                content: "old unrelated context ".repeat(20),
            });
        }
        blocks.push(ContextBlock::assistant_event(
            "assistant response a1",
            "run the current verification action",
        ));
        blocks.push(ContextBlock {
            source: ContextSourceKind::ActionResult,
            placement: crate::ContextPlacement::ConversationAppend,
            label: "action result".to_string(),
            content: format!(
                "[action_result a1 shell_command succeeded]\ncommand: rg cache\noutput: fresh evidence large-action-marker {}",
                "recent exact evidence ".repeat(8)
            ),
        });

        let (context, report) = compact_model_context_for_budget_with_retained_tail_percent(
            AgentContext::new(blocks).unwrap(),
            1_200,
            10,
        )
        .unwrap();

        assert!(report.changed());
        assert!(context.blocks().iter().any(|block| {
            block.source == ContextSourceKind::ProjectGuidance
                && block.content.contains("run just test")
        }));
        assert!(context.blocks().iter().any(|block| {
            block.source == ContextSourceKind::ActionResult
                && block.content.contains("fresh evidence")
        }));
        assert!(context.blocks().iter().any(|block| {
            block.source == ContextSourceKind::ActionResult
                && block.content.contains("large-action-marker")
        }));
        assert!(context.blocks().iter().any(|block| {
            block.source == ContextSourceKind::Memory
                && block.content.contains("[context compacted]")
        }));
    }

    /// Verifies explicit context compaction reports the configured retained
    /// tail instead of a hard-coded default percentage.
    #[test]
    fn explicit_context_compaction_uses_configured_retained_tail_percent() {
        let (context, report) = compact_model_context_for_budget_with_retained_tail_percent(
            AgentContext::new(vec![ContextBlock {
                source: ContextSourceKind::Transcript,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "older transcript".to_string(),
                content: "x".repeat(2 * 1024 * 1024),
            }])
            .unwrap(),
            8 * 1024,
            25,
        )
        .unwrap();

        assert!(report.changed());
        let memory_block = context
            .blocks()
            .iter()
            .find(|block| block.source == ContextSourceKind::Memory)
            .expect("bulk compaction memory should be present");

        assert!(memory_block.content.contains("retained_tail_percent=25"));
    }

    /// Verifies later recovery recursively replaces an earlier summary with
    /// one deterministic bounded rolling summary.
    ///
    /// Successively smaller provider-limit budgets may reduce the raw tail,
    /// but older and newer recovery markers must remain represented without
    /// accumulating multiple model-visible summary epochs.
    #[test]
    fn repeated_context_compaction_preserves_one_rolling_summary() {
        let blocks = (0..24)
            .map(|index| ContextBlock {
                source: ContextSourceKind::Transcript,
                placement: crate::ContextPlacement::ConversationAppend,
                label: format!("transcript {index}"),
                content: format!("entry-{index} {}", "history word ".repeat(80)),
            })
            .collect();
        let (mut first, first_report) =
            compact_model_context_for_budget_with_retained_tail_percent(
                AgentContext::new(blocks).unwrap(),
                1_200,
                20,
            )
            .unwrap();
        assert!(first_report.changed());
        let first_epoch = first
            .blocks()
            .iter()
            .find(|block| context_block_is_compaction_summary(block))
            .expect("first compaction epoch should exist")
            .clone();
        assert!(first_epoch.content.contains("entry-0"));
        for index in 24..32 {
            first
                .insert_typed_block(
                    ContextBlock {
                        source: ContextSourceKind::Transcript,
                        placement: crate::ContextPlacement::ConversationAppend,
                        label: format!("transcript {index}"),
                        content: format!("entry-{index} {}", "new history word ".repeat(80)),
                    },
                    crate::ContextSemanticKind::ReferenceEvent,
                    crate::ContextRetention::Summarizable,
                    true,
                )
                .unwrap();
        }

        let (second, second_report) =
            compact_model_context_for_budget_with_retained_tail_percent(first.clone(), 1_200, 10)
                .unwrap();
        let (repeated, repeated_report) =
            compact_model_context_for_budget_with_retained_tail_percent(first, 1_200, 10).unwrap();

        assert!(second_report.changed());
        assert_eq!(second.blocks(), repeated.blocks());
        assert_eq!(
            second_report.compacted_blocks,
            repeated_report.compacted_blocks
        );
        assert_eq!(second_report.omitted_blocks, repeated_report.omitted_blocks);
        assert_eq!(
            second_report.omitted_original_words,
            repeated_report.omitted_original_words
        );
        assert_eq!(
            second
                .blocks()
                .iter()
                .filter(|block| context_block_is_compaction_summary(block))
                .count(),
            1
        );
        let rolling_summary = second
            .blocks()
            .iter()
            .find(|block| context_block_is_compaction_summary(block))
            .expect("second compaction should retain one rolling summary");
        assert_ne!(rolling_summary, &first_epoch);
        assert!(rolling_summary.content.contains("entry-0"));
        assert!(
            second
                .blocks()
                .iter()
                .any(|block| block.content.contains("entry-31"))
        );
        assert!(model_context_total_words(second.blocks()) <= 1_200);
    }

    /// Verifies compaction never tears assistant/native-tool/result groups,
    /// retains the selected raw group byte-for-byte, and excludes volatile
    /// controller state from the immutable summary epoch.
    #[test]
    fn context_compaction_retains_complete_execution_groups_and_excludes_volatile_state() {
        let old_group = vec![
            ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "old assistant".to_string(),
                content: format!("old plan {}", "history ".repeat(120)),
            },
            ContextBlock {
                source: ContextSourceKind::TranscriptTool,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "old native tool event".to_string(),
                content: format!("native call {}", "arguments ".repeat(40)),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "action result old".to_string(),
                content: format!("old output {}", "evidence ".repeat(80)),
            },
        ];
        let retained_group = vec![
            ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "recent assistant".to_string(),
                content: "run the focused verification".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::TranscriptTool,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "recent native tool event".to_string(),
                content: "shell_command action-new".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "action result new".to_string(),
                content: "focused verification passed".to_string(),
            },
        ];
        let volatile = ContextBlock {
            source: ContextSourceKind::RuntimeHint,
            placement: crate::ContextPlacement::EphemeralTail,
            label: "scheduler readiness".to_string(),
            content: "volatile-secret-state=waiting".to_string(),
        };
        let mut blocks = vec![ContextBlock {
            source: ContextSourceKind::System,
            placement: crate::ContextPlacement::StablePrefix,
            label: "system".to_string(),
            content: "stable policy".to_string(),
        }];
        blocks.extend(old_group.clone());
        blocks.extend(retained_group.clone());
        let original = AgentContext::new_durable(blocks).unwrap();
        let previous = crate::context_continuity_snapshot(&original, "openai", "gpt", "turn-1");

        let (mut compacted, report) =
            compact_model_context_for_budget_with_retained_tail_percent(original, 320, 20).unwrap();

        assert_eq!(report.compacted_blocks, old_group.len());
        assert!(
            !old_group
                .iter()
                .any(|block| compacted.blocks().contains(block))
        );
        let retained_start = compacted
            .blocks()
            .windows(retained_group.len())
            .position(|window| window == retained_group.as_slice())
            .expect("complete recent execution group should remain exact");
        assert!(retained_start > 0);
        assert!(!compacted.blocks().contains(&volatile));
        let prepared = crate::PreparedModelContext::new(compacted.clone(), vec![volatile.clone()])
            .expect("live state should attach only after durable compaction");
        assert_eq!(prepared.live_state(), &[volatile]);
        let summaries = compacted
            .blocks()
            .iter()
            .filter(|block| context_block_is_compaction_summary(block))
            .collect::<Vec<_>>();
        assert_eq!(summaries.len(), 1);
        assert!(!summaries[0].content.contains("volatile-secret-state"));
        let compacted_diagnostics = crate::context_continuity_diagnostics(
            &compacted,
            "openai",
            "gpt",
            "turn-1",
            Some(&previous),
        );
        assert_eq!(
            compacted_diagnostics.break_reason,
            crate::ContextContinuityBreakReason::Compaction
        );
        let compacted_snapshot = compacted_diagnostics.snapshot;
        let after_compaction_group =
            crate::ContextExecutionGroupId::new("after-compaction").unwrap();
        compacted
            .append_assistant_event(
                "assistant response after-compaction",
                "record newly settled evidence",
                after_compaction_group.clone(),
            )
            .unwrap();
        compacted
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "action result after-compaction",
                "new settled evidence",
                after_compaction_group,
                None,
                true,
            )
            .unwrap();
        let appended_diagnostics = crate::context_continuity_diagnostics(
            &compacted,
            "openai",
            "gpt",
            "turn-1",
            Some(&compacted_snapshot),
        );
        assert_eq!(
            appended_diagnostics.break_reason,
            crate::ContextContinuityBreakReason::AppendOnly
        );
        assert!(appended_diagnostics.immutable_append_only);
    }

    /// Verifies user prompts and mid-turn steering remain exact, ordered
    /// barriers when execution ranges on both sides roll into one summary.
    #[test]
    fn context_compaction_preserves_prompt_and_steering_barriers_in_place() {
        let prompt = ContextBlock::user_event("user prompt", "implement the chronology change");
        let steering = ContextBlock::user_event("user steering 1", "also preserve exact evidence");
        let first_execution = vec![
            ContextBlock::assistant_event(
                "assistant response 1",
                format!("first action {}", "reason ".repeat(80)),
            ),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "action result 1",
                format!("first evidence {}", "output ".repeat(80)),
            ),
        ];
        let second_execution = vec![
            ContextBlock::assistant_event(
                "assistant response 2",
                format!("second action {}", "reason ".repeat(80)),
            ),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "action result 2",
                format!("second evidence {}", "output ".repeat(80)),
            ),
        ];
        let mut blocks = vec![prompt.clone()];
        blocks.extend(first_execution);
        blocks.push(steering.clone());
        blocks.extend(second_execution);

        let original = AgentContext::new_durable(blocks).unwrap();
        let original_sequences = original
            .chronology()
            .iter()
            .map(|event| event.sequence().get())
            .collect::<Vec<_>>();
        let (first, report) =
            compact_model_context_for_budget_with_retained_tail_percent(original, 1_000, 1)
                .unwrap();

        assert_eq!(report.compacted_blocks, 4);
        assert_eq!(first.blocks()[0], prompt);
        assert!(context_block_is_compaction_summary(&first.blocks()[1]));
        assert_eq!(first.blocks()[2], steering);
        assert_eq!(
            first
                .blocks()
                .iter()
                .filter(|block| context_block_is_compaction_summary(block))
                .count(),
            1
        );
        assert_eq!(
            first
                .chronology()
                .iter()
                .map(|event| event.sequence().get())
                .collect::<Vec<_>>(),
            vec![
                original_sequences[0],
                original_sequences[1],
                original_sequences[3]
            ]
        );

        let (second, repeated_report) =
            compact_model_context_for_budget_with_retained_tail_percent(first.clone(), 1_000, 1)
                .unwrap();
        assert!(!repeated_report.changed());
        assert_eq!(second.blocks(), first.blocks());
    }

    /// Verifies deterministic compaction preserves decision-relevant semantics
    /// instead of emitting only labels and word counts.
    #[test]
    fn context_compaction_summary_retains_semantic_recovery_records() {
        let blocks = vec![
            ContextBlock::assistant_event(
                "assistant decision",
                format!(
                    "Decision: use the actor reducer. Updated file=src/runtime.rs commit=abc123. {}",
                    "reasoning ".repeat(120)
                ),
            ),
            ContextBlock::evidence_event(
                ContextSourceKind::ActionResult,
                "action result failed-check",
                format!(
                    "status=failed error=clippy rejected the change. Remaining: fix the pending lint. artifact path=src/runtime.rs {}",
                    "diagnostic ".repeat(120)
                ),
            ),
            ContextBlock::user_event("user steering", "preserve the exact causal boundary"),
        ];

        let (compacted, report) = compact_model_context_for_budget_with_retained_tail_percent(
            AgentContext::new_durable(blocks).unwrap(),
            1_000,
            1,
        )
        .unwrap();

        assert_eq!(report.compacted_blocks, 2);
        let summary = &compacted.blocks()[0].content;
        assert!(summary.contains("Semantic recovery records:"));
        assert!(summary.contains("categories=decision,artifact"));
        assert!(summary.contains("categories=outcome,error,artifact,unresolved"));
        assert!(summary.contains("Decision: use the actor reducer"));
        assert!(summary.contains("error=clippy rejected the change"));
        assert!(summary.contains("Remaining: fix the pending lint"));
        assert!(summary.contains("path=src/runtime.rs"));
        assert_eq!(compacted.blocks()[1].label, "user steering");
    }

    /// Verifies protected task context that cannot fit fails explicitly.
    ///
    /// Direct user text is never truncated or summarized merely to force a
    /// provider retry through a smaller context window.
    #[test]
    fn context_compaction_rejects_unrecoverable_exact_context_overflow() {
        let context = AgentContext::new_durable(vec![ContextBlock::user_event(
            "user prompt",
            "required exact instruction ".repeat(100),
        )])
        .unwrap();

        let error = compact_model_context_for_budget(context, 10).unwrap_err();

        assert!(
            error
                .message()
                .contains("unrecoverable model context overflow")
        );
        assert!(
            error
                .message()
                .contains("cannot be truncated or summarized")
        );
    }
}
