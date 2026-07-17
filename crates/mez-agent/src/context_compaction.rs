//! Provider-independent model-context compaction and budgeting.
//!
//! This module owns deterministic bulk compaction after an explicit trigger or
//! provider context-limit response. It preserves protected guidance and recent
//! evidence, retains a configurable raw tail, and emits bounded inventory
//! summaries without product runtime or persistence dependencies.

use crate::{
    AgentContext, AgentContextResult, ContextBlock, ContextSourceKind,
    ModelContextCompactionReport, model_context_block_header,
};
use std::collections::HashSet;

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
    let (blocks, report) = compact_model_context_blocks(
        &context.blocks,
        context_budget_words,
        true,
        retained_tail_percent,
    );
    AgentContext::new(blocks).map(|context| (context, report))
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
) -> (Vec<ContextBlock>, ModelContextCompactionReport) {
    let retained_tail_percent =
        normalize_model_context_retained_tail_percent(retained_tail_percent);
    let mut report = ModelContextCompactionReport::default();
    let total_words = model_context_total_words(blocks);
    let oversized_block_present = blocks
        .iter()
        .any(|block| block.content.len() > MODEL_CONTEXT_BLOCK_LIMIT_BYTES);
    if !force && total_words <= context_budget_words && !oversized_block_present {
        return (blocks.to_vec(), report);
    }

    let tail_budget =
        model_context_retained_tail_budget_words(context_budget_words, retained_tail_percent);
    let tail_start = model_context_tail_start_index(blocks, tail_budget);
    let compacted_blocks = &blocks[..tail_start];
    let retained_tail = &blocks[tail_start..];
    if compacted_blocks.is_empty() {
        return (blocks.to_vec(), report);
    }

    let protected_indices = protected_compacted_block_indices(compacted_blocks);
    let mut protected_blocks = Vec::new();
    let mut summarizable_blocks = Vec::new();
    for (index, block) in compacted_blocks.iter().cloned().enumerate() {
        if protected_indices.contains(&index) {
            protected_blocks.push(block);
        } else {
            summarizable_blocks.push(block);
        }
    }
    if summarizable_blocks.is_empty() {
        return (blocks.to_vec(), report);
    }

    report.compacted_blocks = summarizable_blocks.len();
    let mut prepared = Vec::with_capacity(
        protected_blocks
            .len()
            .saturating_add(retained_tail.len())
            .saturating_add(1),
    );
    prepared.extend(protected_blocks);
    prepared.push(bulk_compacted_model_context_block(
        &summarizable_blocks,
        retained_tail,
        tail_budget,
        retained_tail_percent,
    ));
    let protected_floor = prepared.len();
    prepared.extend(retained_tail.iter().cloned());

    let mut total_words = model_context_total_words(&prepared);
    if total_words > context_budget_words && prepared.len() > protected_floor {
        let mut omitted_end = protected_floor;
        while total_words > context_budget_words && omitted_end < prepared.len() {
            let omitted_words = model_context_block_words(&prepared[omitted_end]);
            report.omitted_blocks += 1;
            report.omitted_original_words += omitted_words;
            total_words = total_words.saturating_sub(omitted_words);
            omitted_end += 1;
        }
        prepared.drain(protected_floor..omitted_end);
    }

    (prepared, report)
}

/// Returns compacted-block indexes that stay exact through compaction.
fn protected_compacted_block_indices(blocks: &[ContextBlock]) -> HashSet<usize> {
    let mut protected = HashSet::new();
    for (index, block) in blocks.iter().enumerate() {
        if matches!(
            block.source,
            ContextSourceKind::ProjectGuidance
                | ContextSourceKind::EvidenceLedger
                | ContextSourceKind::CommittedEvidence
        ) || model_context_block_is_compaction_summary(block)
        {
            protected.insert(index);
        }
    }
    for (index, block) in blocks.iter().enumerate().rev() {
        if block.source == ContextSourceKind::ActionResult {
            protected.insert(index);
            break;
        }
    }
    protected
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

/// Finds the first block in the retained raw suffix.
fn model_context_tail_start_index(blocks: &[ContextBlock], tail_budget_words: usize) -> usize {
    let mut retained_words = 0usize;
    let mut tail_start = blocks.len();
    for (index, block) in blocks.iter().enumerate().rev() {
        if block.content.len() > MODEL_CONTEXT_BLOCK_LIMIT_BYTES {
            break;
        }
        let block_words = model_context_block_words(block);
        if retained_words.saturating_add(block_words) > tail_budget_words {
            break;
        }
        retained_words = retained_words.saturating_add(block_words);
        tail_start = index;
    }
    tail_start
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
        ContextSourceKind::EvidenceLedger => "evidence_ledger",
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
                label: format!("transcript {index}"),
                content: format!("transcript-{index} {}", "history-word ".repeat(7_000)),
            });
        }

        let (context, report) =
            compact_model_context_for_budget(AgentContext::new(blocks).unwrap(), 80 * 1024)
                .unwrap();

        assert!(report.changed());
        let summary = context
            .blocks
            .iter()
            .find(|block| block.source == ContextSourceKind::Memory)
            .expect("oldest transcript should be present in summary inventory");
        let recent_history = context
            .blocks
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
        let mut blocks = vec![
            ContextBlock {
                source: ContextSourceKind::ProjectGuidance,
                label: "project guidance".to_string(),
                content: "run just test before handoff".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                label: "action result".to_string(),
                content: format!(
                    "[action_result a1 shell_command succeeded]\ncommand: rg cache\noutput: fresh evidence large-action-marker {}",
                    "large exact evidence ".repeat(2_000)
                ),
            },
        ];
        for index in 0..40 {
            blocks.push(ContextBlock {
                source: ContextSourceKind::Memory,
                label: format!("old memory {index}"),
                content: "old unrelated context ".repeat(20),
            });
        }

        let (context, report) = compact_model_context_for_budget_with_retained_tail_percent(
            AgentContext::new(blocks).unwrap(),
            600,
            10,
        )
        .unwrap();

        assert!(report.changed());
        assert!(context.blocks.iter().any(|block| {
            block.source == ContextSourceKind::ProjectGuidance
                && block.content.contains("run just test")
        }));
        assert!(context.blocks.iter().any(|block| {
            block.source == ContextSourceKind::ActionResult
                && block.content.contains("fresh evidence")
        }));
        assert!(context.blocks.iter().any(|block| {
            block.source == ContextSourceKind::ActionResult
                && block.content.contains("large-action-marker")
        }));
        assert!(context.blocks.iter().any(|block| {
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
            .blocks
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
            .blocks
            .iter()
            .find(|block| model_context_block_is_compaction_summary(block))
            .expect("first compaction epoch should exist")
            .clone();
        for index in 24..32 {
            first.blocks.push(ContextBlock {
                source: ContextSourceKind::Transcript,
                label: format!("transcript {index}"),
                content: format!("entry-{index} {}", "new history word ".repeat(80)),
            });
        }

        let (second, second_report) =
            compact_model_context_for_budget_with_retained_tail_percent(first.clone(), 600, 10)
                .unwrap();
        let (repeated, repeated_report) =
            compact_model_context_for_budget_with_retained_tail_percent(first, 600, 10).unwrap();

        assert!(second_report.changed());
        assert_eq!(second.blocks, repeated.blocks);
        assert_eq!(
            second_report.compacted_blocks,
            repeated_report.compacted_blocks
        );
        assert_eq!(second_report.omitted_blocks, repeated_report.omitted_blocks);
        assert_eq!(
            second_report.omitted_original_words,
            repeated_report.omitted_original_words
        );
        assert!(second.blocks.contains(&first_epoch));
        assert_eq!(
            second
                .blocks
                .iter()
                .filter(|block| model_context_block_is_compaction_summary(block))
                .count(),
            2
        );
    }
}
