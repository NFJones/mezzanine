//! Model context compaction and budgeting.
//!
//! This module owns deterministic local compaction after an explicit trigger
//! or provider context-limit response. It keeps word budgeting, retained-tail
//! selection, protected evidence, and compacted-block inventory formatting out
//! of provider request assembly.

use super::evidence::prepare_model_context_blocks;
use super::{
    AgentContext, ContextBlock, ContextSourceKind, DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT,
    MODEL_CONTEXT_BLOCK_LIMIT_BYTES, MODEL_CONTEXT_COMPACTED_PREFIX, ModelContextCompactionReport,
    model_context_block_header,
};
use crate::error::Result;
use std::collections::HashSet;

/// Compacts provider-bound context blocks after a trigger has fired.
///
/// This uses deterministic local summarization and returns the compacted
/// context to callers that need to persist a reduced active-turn context before
/// the next provider continuation. Callers invoke this only after an explicit
/// or provider-limit trigger, so it always applies the bulk summary-plus-tail
/// shape.
pub fn compact_model_context_for_budget(
    context: AgentContext,
    context_budget_words: usize,
) -> Result<(AgentContext, ModelContextCompactionReport)> {
    compact_model_context_for_budget_with_retained_tail_percent(
        context,
        context_budget_words,
        DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT,
    )
}

/// Compacts provider-bound context while using the configured raw-tail percent.
pub fn compact_model_context_for_budget_with_retained_tail_percent(
    context: AgentContext,
    context_budget_words: usize,
    retained_tail_percent: usize,
) -> Result<(AgentContext, ModelContextCompactionReport)> {
    let blocks = prepare_model_context_blocks(context.blocks);
    let (blocks, report) =
        compact_model_context_blocks(&blocks, context_budget_words, true, retained_tail_percent);
    AgentContext::new(blocks).map(|context| (context, report))
}

/// Returns context blocks prepared for a provider request without slicing block
/// content.
///
/// When compaction is required, older blocks are folded into one memory summary
/// and only a bounded recent raw suffix is retained.
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

    while model_context_total_words(&prepared) > context_budget_words {
        if prepared.len() <= protected_floor {
            break;
        }
        if let Some(omitted) = prepared.pop() {
            report.omitted_blocks += 1;
            report.omitted_original_words += model_context_block_words(&omitted);
        }
    }

    (prepared, report)
}

/// Returns compacted-block indexes that should stay exact through compaction.
fn protected_compacted_block_indices(blocks: &[ContextBlock]) -> HashSet<usize> {
    let mut protected = HashSet::new();
    for (index, block) in blocks.iter().enumerate() {
        if matches!(
            block.source,
            ContextSourceKind::ProjectGuidance
                | ContextSourceKind::EvidenceLedger
                | ContextSourceKind::CommittedEvidence
        ) {
            protected.insert(index);
        }
    }
    for (index, block) in blocks.iter().enumerate().rev() {
        if block.source == ContextSourceKind::ActionResult {
            protected.insert(index);
        }
    }
    protected
}

/// Returns the provider-request word cost of one prepared context block.
fn model_context_block_words(block: &ContextBlock) -> usize {
    model_context_text_word_count(&model_context_block_header(block))
        .saturating_add(model_context_text_word_count(&block.content))
}

/// Returns the aggregate provider-request word cost for prepared blocks.
fn model_context_total_words(blocks: &[ContextBlock]) -> usize {
    blocks
        .iter()
        .map(model_context_block_words)
        .fold(0usize, usize::saturating_add)
}

/// Returns the retained raw-tail word budget for local bulk compaction.
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

/// Normalizes configured retained-tail percentages for defensive callers.
fn normalize_model_context_retained_tail_percent(retained_tail_percent: usize) -> usize {
    retained_tail_percent.clamp(1, 100)
}

/// Counts whitespace-delimited words for context compaction summaries and
/// bounded internal router projections.
pub fn model_context_text_word_count(value: &str) -> usize {
    value.split_whitespace().count()
}

/// Finds the first block in the retained raw suffix for local bulk compaction.
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

/// Builds the memory-style block representing locally compacted context.
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

/// Returns whether context blocks already have the local bulk compaction shape.
pub(super) fn model_context_has_bulk_compaction_summary(blocks: &[ContextBlock]) -> bool {
    blocks.first().is_some_and(|block| {
        block.source == ContextSourceKind::Memory
            && block.label == "context compaction summary"
            && block.content.starts_with(MODEL_CONTEXT_COMPACTED_PREFIX)
    })
}

/// Returns a stable source label for local compaction diagnostics.
pub(super) fn model_context_source_kind_name(source: ContextSourceKind) -> &'static str {
    match source {
        ContextSourceKind::System => "system",
        ContextSourceKind::UserInstruction => "user_instruction",
        ContextSourceKind::DeveloperInstruction => "developer_instruction",
        ContextSourceKind::Policy => "policy",
        ContextSourceKind::Configuration => "configuration",
        ContextSourceKind::LocalMessage => "local_message",
        ContextSourceKind::ProjectGuidance => "project_guidance",
        ContextSourceKind::Memory => "memory",
        ContextSourceKind::Transcript => "transcript",
        ContextSourceKind::TranscriptUser => "transcript_user",
        ContextSourceKind::TranscriptAssistant => "transcript_assistant",
        ContextSourceKind::TranscriptTool => "transcript_tool",
        ContextSourceKind::EvidenceLedger => "evidence_ledger",
        ContextSourceKind::CommittedEvidence => "committed_evidence",
        ContextSourceKind::ActionResult => "action_result",
    }
}

/// Returns a single-line value for compact diagnostics.
pub(super) fn model_context_single_line(value: &str) -> String {
    value.replace('\n', "\\n").replace('\r', "\\r")
}
