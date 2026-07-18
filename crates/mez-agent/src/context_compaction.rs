//! Provider-independent model-context compaction and budgeting.
//!
//! This module owns deterministic bulk compaction after an explicit trigger or
//! provider context-limit response. It preserves protected guidance and recent
//! evidence, retains a configurable raw tail, and emits bounded inventory
//! summaries without product runtime or persistence dependencies.

use crate::{
    AgentContext, AgentContextError, AgentContextResult, ContextBlock, ContextRetention,
    ContextSourceKind, ModelContextCompactionReport, model_context_block_header,
};
use std::ops::Range;

/// Maximum bytes from one context block retained in a raw suffix.
const MODEL_CONTEXT_BLOCK_LIMIT_BYTES: usize = 128 * 1024;
/// Marker used for deterministic local compaction summaries.
const MODEL_CONTEXT_COMPACTED_PREFIX: &str = "[context compacted]";
/// Default raw suffix percent retained after local context compaction.
pub const DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT: usize = 10;

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
    context.validate_durable()?;
    let (blocks, report) = compact_model_context_blocks(
        context.blocks(),
        context_budget_words,
        true,
        retained_tail_percent,
    )?;
    AgentContext::new_durable(blocks)
        .map(|compacted| compacted.with_metadata(context.metadata().clone()))
        .map(|context| (context, report))
}

/// Counts whitespace-delimited words for context budgeting.
pub fn model_context_text_word_count(value: &str) -> usize {
    value.split_whitespace().count()
}

/// Applies summary-plus-tail compaction to ordered context blocks.
fn compact_model_context_blocks(
    blocks: &[ContextBlock],
    context_budget_words: usize,
    force: bool,
    retained_tail_percent: usize,
) -> AgentContextResult<(Vec<ContextBlock>, ModelContextCompactionReport)> {
    let retained_tail_percent =
        normalize_model_context_retained_tail_percent(retained_tail_percent);
    let mut report = ModelContextCompactionReport::default();
    let total_words = model_context_total_words(blocks);
    let oversized_block_present = blocks
        .iter()
        .any(|block| block.content.len() > MODEL_CONTEXT_BLOCK_LIMIT_BYTES);
    if !force && total_words <= context_budget_words && !oversized_block_present {
        return Ok((blocks.to_vec(), report));
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
    let execution_groups = model_context_execution_group_ranges(&immutable_chronology);
    let eligible_groups = execution_groups
        .iter()
        .enumerate()
        .filter(|(_, group)| {
            !immutable_chronology[(*group).clone()]
                .iter()
                .any(model_context_block_is_protected_barrier)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if eligible_groups.is_empty() {
        return Ok((blocks.to_vec(), report));
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
    let mut prepared = Vec::with_capacity(blocks.len().saturating_add(execution_groups.len()));
    prepared.extend(stable_prefix);
    let mut pending_compacted = Vec::new();
    for (group_index, group) in execution_groups.iter().enumerate() {
        let group_blocks = &immutable_chronology[group.clone()];
        let protected = group_blocks
            .iter()
            .any(model_context_block_is_protected_barrier);
        if !protected && !retained_groups.contains(&group_index) {
            pending_compacted.extend(group_blocks.iter().cloned());
            continue;
        }
        flush_compacted_context_run(
            &mut prepared,
            &mut pending_compacted,
            &retained_tail,
            tail_budget,
            retained_tail_percent,
            &mut report,
        );
        prepared.extend(group_blocks.iter().cloned());
    }
    flush_compacted_context_run(
        &mut prepared,
        &mut pending_compacted,
        &retained_tail,
        tail_budget,
        retained_tail_percent,
        &mut report,
    );
    if report.compacted_blocks == 0 {
        return Ok((blocks.to_vec(), report));
    }
    let compacted_words = model_context_total_words(&prepared);
    if compacted_words > context_budget_words {
        return Err(AgentContextError::new(format!(
            "unrecoverable model context overflow: minimum barrier-preserving compacted context requires {compacted_words} words but provider budget is {context_budget_words}"
        )));
    }

    Ok((prepared, report))
}

/// Emits one compacted summary at the original position of its replaced run.
fn flush_compacted_context_run(
    prepared: &mut Vec<ContextBlock>,
    pending: &mut Vec<ContextBlock>,
    retained_tail: &[ContextBlock],
    tail_budget_words: usize,
    retained_tail_percent: usize,
    report: &mut ModelContextCompactionReport,
) {
    if pending.is_empty() {
        return;
    }
    report.compacted_blocks = report.compacted_blocks.saturating_add(pending.len());
    prepared.push(bulk_compacted_model_context_block(
        pending,
        retained_tail,
        tail_budget_words,
        retained_tail_percent,
    ));
    pending.clear();
}

/// Groups immutable chronology at indivisible provider-execution boundaries.
///
/// Assistant output starts an execution group and all immediately following
/// native tool events and settled action results remain in that group. Other
/// chronology blocks begin independent groups, which gives the compactor safe
/// prefix and raw-suffix boundaries without embedding product turn state.
fn model_context_execution_group_ranges(blocks: &[ContextBlock]) -> Vec<Range<usize>> {
    let mut groups = Vec::new();
    let mut start = 0usize;
    let mut has_assistant = false;
    let mut has_native_tool = false;
    for (index, block) in blocks.iter().enumerate() {
        let protected = model_context_block_is_protected_barrier(block);
        let attaches_to_previous = match block.source {
            ContextSourceKind::TranscriptTool => has_assistant,
            ContextSourceKind::ActionResult => has_assistant || has_native_tool,
            _ => false,
        };
        let current_group_protected = blocks[start..index]
            .iter()
            .any(model_context_block_is_protected_barrier);
        if index > start && (protected || current_group_protected || !attaches_to_previous) {
            groups.push(start..index);
            start = index;
            has_assistant = false;
            has_native_tool = false;
        }
        has_assistant |= block.source == ContextSourceKind::TranscriptAssistant;
        has_native_tool |= block.source == ContextSourceKind::TranscriptTool;
    }
    if start < blocks.len() {
        groups.push(start..blocks.len());
    }
    groups
}

/// Returns whether a block is an exact, non-crossable compaction barrier.
fn model_context_block_is_protected_barrier(block: &ContextBlock) -> bool {
    block.retention() == ContextRetention::Exact || model_context_block_is_compaction_summary(block)
}

/// Returns whether a block is an immutable local compaction summary epoch.
fn model_context_block_is_compaction_summary(block: &ContextBlock) -> bool {
    block.source == ContextSourceKind::Memory
        && block.label == "context compaction summary"
        && block.content.starts_with(MODEL_CONTEXT_COMPACTED_PREFIX)
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
) -> ContextBlock {
    let compacted_original_words = model_context_total_words(compacted_blocks);
    let retained_tail_words = model_context_total_words(retained_tail);
    let mut lines = vec![
        MODEL_CONTEXT_COMPACTED_PREFIX.to_string(),
        "Local context was compacted in bulk before provider request assembly.".to_string(),
        format!("compacted_blocks={}", compacted_blocks.len()),
        format!("compacted_original_words={compacted_original_words}"),
        format!("retained_tail_blocks={}", retained_tail.len()),
        format!("retained_tail_words={retained_tail_words}"),
        format!("retained_tail_budget_words={tail_budget_words}"),
        format!(
            "retained_tail_percent={}",
            normalize_model_context_retained_tail_percent(retained_tail_percent)
        ),
        "Compacted block inventory:".to_string(),
    ];
    for (index, block) in compacted_blocks.iter().take(64).enumerate() {
        lines.push(format!(
            "{}. source={} label={} original_words={}",
            index.saturating_add(1),
            model_context_source_kind_name(block.source),
            model_context_single_line(&block.label),
            model_context_block_words(block)
        ));
    }
    if compacted_blocks.len() > 64 {
        lines.push(format!(
            "... {} additional compacted blocks omitted from inventory",
            compacted_blocks.len().saturating_sub(64)
        ));
    }
    lines.push(
        "Exact compacted content was omitted. Use targeted read/search/capture actions if details are needed."
            .to_string(),
    );
    ContextBlock {
        source: ContextSourceKind::Memory,
        placement: crate::ContextPlacement::ConversationAppend,
        label: "context compaction summary".to_string(),
        content: lines.join("\n"),
    }
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
            600,
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

    /// Verifies later recovery creates a new immutable summary epoch without
    /// rewriting the exact bytes of an earlier compaction boundary.
    ///
    /// Successively smaller provider-limit budgets may reduce the raw tail,
    /// but stable summary bytes must remain reusable across those retries.
    #[test]
    fn repeated_context_compaction_preserves_immutable_summary_epochs() {
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
            .find(|block| model_context_block_is_compaction_summary(block))
            .expect("first compaction epoch should exist")
            .clone();
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
            compact_model_context_for_budget_with_retained_tail_percent(first.clone(), 600, 10)
                .unwrap();
        let (repeated, repeated_report) =
            compact_model_context_for_budget_with_retained_tail_percent(first, 600, 10).unwrap();

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
        assert!(second.blocks().contains(&first_epoch));
        assert_eq!(
            second
                .blocks()
                .iter()
                .filter(|block| model_context_block_is_compaction_summary(block))
                .count(),
            2
        );
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
            .filter(|block| model_context_block_is_compaction_summary(block))
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
        compacted
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "action result after-compaction",
                "new settled evidence",
                crate::ContextExecutionGroupId::new("after-compaction").unwrap(),
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

    /// Verifies user prompts and mid-turn steering are exact, non-crossable
    /// barriers and each summarized execution range stays at its own original
    /// chronological position.
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

        let (first, report) = compact_model_context_for_budget_with_retained_tail_percent(
            AgentContext::new_durable(blocks).unwrap(),
            1_000,
            1,
        )
        .unwrap();

        assert_eq!(report.compacted_blocks, 4);
        assert_eq!(first.blocks()[0], prompt);
        assert!(model_context_block_is_compaction_summary(
            &first.blocks()[1]
        ));
        assert_eq!(first.blocks()[2], steering);
        assert!(model_context_block_is_compaction_summary(
            &first.blocks()[3]
        ));

        let (second, repeated_report) =
            compact_model_context_for_budget_with_retained_tail_percent(first.clone(), 1_000, 1)
                .unwrap();
        assert!(!repeated_report.changed());
        assert_eq!(second.blocks(), first.blocks());
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
