//! Generated evidence ledger support for model context.
//!
//! This module summarizes prior action/tool evidence into compact
//! provider-visible ledger lines before request assembly. The ledger helps
//! continuations avoid repeating reads, tests, and patch recovery checks while
//! keeping large raw tool outputs out of the stable prompt prefix.

use super::compaction::{model_context_single_line, model_context_source_kind_name};
use super::{ContextBlock, ContextSourceKind, MODEL_CONTEXT_HOT_ACTION_LIMIT_BYTES};
use std::collections::HashSet;

/// Prepares context blocks for provider requests and compaction.
pub(super) fn prepare_model_context_blocks(blocks: Vec<ContextBlock>) -> Vec<ContextBlock> {
    let deduped = dedupe_historical_context_blocks(blocks);
    let committed = with_generated_committed_evidence(deduped);
    with_generated_evidence_ledger(committed)
}

/// Removes repeated historical transcript blocks before request assembly.
fn dedupe_historical_context_blocks(blocks: Vec<ContextBlock>) -> Vec<ContextBlock> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::with_capacity(blocks.len());
    for block in blocks {
        let should_dedupe = matches!(
            block.source,
            ContextSourceKind::Transcript
                | ContextSourceKind::TranscriptUser
                | ContextSourceKind::TranscriptAssistant
                | ContextSourceKind::TranscriptTool
        );
        if should_dedupe {
            let key = (
                model_context_source_kind_name(block.source),
                block.label.clone(),
                block.content.clone(),
            );
            if !seen.insert(key) {
                continue;
            }
        }
        deduped.push(block);
    }
    deduped
}

/// Adds or refreshes the generated evidence ledger block.
fn with_generated_evidence_ledger(blocks: Vec<ContextBlock>) -> Vec<ContextBlock> {
    let mut blocks = blocks
        .into_iter()
        .filter(|block| block.source != ContextSourceKind::EvidenceLedger)
        .collect::<Vec<_>>();
    let Some(ledger) = build_evidence_ledger_block(&blocks) else {
        return blocks;
    };
    let insert_at = blocks
        .iter()
        .rposition(|block| block.source == ContextSourceKind::UserInstruction)
        .map(|index| index.saturating_add(1))
        .or_else(|| {
            blocks
                .iter()
                .position(|block| block.source == ContextSourceKind::ActionResult)
        })
        .unwrap_or(blocks.len());
    blocks.insert(insert_at, ledger);
    blocks
}

/// Promotes action results already observed by a later assistant response.
///
/// The newest action results after the latest assistant response stay raw and
/// volatile so the next provider call receives full execution evidence. Older
/// results have already been available to a model continuation, so subsequent
/// requests receive compact immutable summaries in the stable prefix instead of
/// replaying large raw outputs indefinitely.
fn with_generated_committed_evidence(blocks: Vec<ContextBlock>) -> Vec<ContextBlock> {
    let blocks = blocks
        .into_iter()
        .filter(|block| block.source != ContextSourceKind::CommittedEvidence)
        .collect::<Vec<_>>();
    let Some(latest_assistant_index) = blocks
        .iter()
        .rposition(|block| block.source == ContextSourceKind::TranscriptAssistant)
    else {
        return blocks;
    };
    let mut promoted = Vec::with_capacity(blocks.len());
    for (index, block) in blocks.into_iter().enumerate() {
        if block.source == ContextSourceKind::ActionResult
            && index < latest_assistant_index
            && let Some(committed) = committed_evidence_block_for_action_result(&block)
        {
            promoted.push(committed);
            continue;
        }
        promoted.push(block);
    }
    promoted
}

/// Builds a compact stable-prefix block for one settled action result.
fn committed_evidence_block_for_action_result(block: &ContextBlock) -> Option<ContextBlock> {
    let entry = evidence_ledger_entry_for_block(block)?;
    let action_id = action_result_marker_id(block.content.lines().next().unwrap_or_default())
        .unwrap_or("unknown");
    Some(ContextBlock {
        source: ContextSourceKind::CommittedEvidence,
        label: format!("committed evidence {action_id}"),
        content: [
            "[committed_evidence]".to_string(),
            "Compact immutable current-turn evidence already observed by a later assistant response. Use it to avoid repeating completed work; raw action output may be omitted from the volatile suffix.".to_string(),
            entry,
        ]
        .join("\n"),
    })
}

/// Builds a compact provider-visible evidence ledger from action/tool blocks.
fn build_evidence_ledger_block(blocks: &[ContextBlock]) -> Option<ContextBlock> {
    let mut lines = vec![
        "Use this ledger before repeating reads, tests, validation, or patch recovery.".to_string(),
    ];
    let mut entries = Vec::new();
    for block in blocks {
        if !matches!(
            block.source,
            ContextSourceKind::ActionResult | ContextSourceKind::TranscriptTool
        ) {
            continue;
        }
        let Some(entry) = evidence_ledger_entry_for_block(block) else {
            continue;
        };
        entries.push(entry);
        if entries.len() >= 24 {
            break;
        }
    }
    if entries.is_empty() {
        return None;
    }
    lines.extend(entries);
    Some(ContextBlock {
        source: ContextSourceKind::EvidenceLedger,
        label: "evidence ledger".to_string(),
        content: lines.join("\n"),
    })
}

/// Returns one compact ledger line for a raw action or historical tool block.
fn evidence_ledger_entry_for_block(block: &ContextBlock) -> Option<String> {
    let marker_line = block.content.lines().next().unwrap_or_default().trim();
    let command = model_context_field_value(&block.content, "command")
        .or_else(|| historical_tool_command_hint(&block.content));
    let exit_code = model_context_field_value(&block.content, "exit_code");
    let status = action_result_marker_status(marker_line).unwrap_or("historical");
    let action_type = action_result_marker_type(marker_line).unwrap_or_else(|| {
        if command.is_some() {
            "shell_command"
        } else {
            "tool"
        }
    });
    let summary = evidence_summary_for_block(block);
    let category = evidence_category(action_type, command.as_deref(), &block.content);
    let mut line = format!(
        "- category={} source={} status={}",
        category,
        model_context_source_kind_name(block.source),
        status
    );
    if let Some(command) = command {
        line.push_str(&format!(" command={}", model_context_single_line(&command)));
    }
    if let Some(exit_code) = exit_code {
        line.push_str(&format!(
            " exit_code={}",
            model_context_single_line(&exit_code)
        ));
    }
    if !summary.is_empty() {
        line.push_str(&format!(" summary={summary}"));
    }
    Some(line)
}

/// Returns a compact category for a ledger entry.
fn evidence_category(action_type: &str, command: Option<&str>, content: &str) -> &'static str {
    if action_type == "apply_patch" {
        return "patch";
    }
    let haystack = format!(
        "{} {}",
        command.unwrap_or_default().to_ascii_lowercase(),
        content.to_ascii_lowercase()
    );
    if haystack.contains("cargo test")
        || haystack.contains("just test")
        || haystack.contains("cargo check")
        || haystack.contains("just check")
        || haystack.contains("cargo clippy")
        || haystack.contains("just clippy")
        || haystack.contains("cargo fmt")
        || haystack.contains("just fmt")
        || haystack.contains("git diff --check")
    {
        "validation"
    } else if haystack.contains("sed -n")
        || haystack.contains("rg ")
        || haystack.contains("rg\t")
        || haystack.contains("read ")
    {
        "read"
    } else {
        "command"
    }
}

/// Extracts a simple `key: value` field from model-facing action context.
fn model_context_field_value(content: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    content.lines().find_map(|line| {
        line.trim()
            .strip_prefix(&prefix)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

/// Extracts command hints from older transcript tool content.
fn historical_tool_command_hint(content: &str) -> Option<String> {
    content.lines().find_map(|line| {
        line.trim()
            .strip_prefix("command:")
            .or_else(|| line.trim().strip_prefix("command="))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

/// Extracts an action type from a standard action-result marker.
fn action_result_marker_type(marker: &str) -> Option<&str> {
    let marker = marker.strip_prefix("[action_result ")?;
    let mut fields = marker.trim_end_matches(']').split_whitespace();
    let _action_id = fields.next()?;
    fields.next()
}

/// Extracts an action id from a standard action-result marker.
fn action_result_marker_id(marker: &str) -> Option<&str> {
    let marker = marker.trim().strip_prefix("[action_result ")?;
    marker.trim_end_matches(']').split_whitespace().next()
}

/// Extracts an action status from a standard action-result marker.
fn action_result_marker_status(marker: &str) -> Option<&str> {
    let marker = marker.strip_prefix("[action_result ")?;
    let mut fields = marker.trim_end_matches(']').split_whitespace();
    let _action_id = fields.next()?;
    let _action_type = fields.next()?;
    fields.next()
}

/// Builds a short action-output summary for the evidence ledger.
fn evidence_summary_for_block(block: &ContextBlock) -> String {
    if block.content.len() > MODEL_CONTEXT_HOT_ACTION_LIMIT_BYTES {
        return "oversized output omitted; see compacted inventory or rerun a bounded query if needed"
            .to_string();
    }
    let summary_source = block
        .content
        .split_once("output:")
        .map(|(_, output)| output)
        .or_else(|| {
            block
                .content
                .split_once("content:")
                .map(|(_, output)| output)
        })
        .unwrap_or(&block.content);
    model_context_single_line(&truncate_model_context_summary(summary_source, 220))
}

/// Truncates a summary to a byte ceiling without splitting UTF-8.
fn truncate_model_context_summary(value: &str, max_bytes: usize) -> String {
    let trimmed = value.trim();
    if trimmed.len() <= max_bytes {
        return trimmed.to_string();
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !trimmed.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}...", &trimmed[..boundary])
}
