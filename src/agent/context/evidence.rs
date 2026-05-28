//! Generated current-turn read ledger and committed-evidence support.
//!
//! This module prepares provider-visible helper blocks derived from existing
//! context without rewriting raw shell/action results into a separate evidence
//! ledger. Current-turn read/search coverage is still summarized for reuse, and
//! older non-shell action results may still be promoted into stable committed
//! evidence after a later assistant response has already observed them.

use super::compaction::{model_context_single_line, model_context_source_kind_name};
use super::{ContextBlock, ContextSourceKind};
use crate::agent::{ShellReadObservation, ShellReadObservationKind, ShellReadRange};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Maximum summary size used for one committed evidence block entry.
///
/// Committed evidence stays intentionally compact because it lands in the
/// session-stable prefix one block at a time.
const COMMITTED_EVIDENCE_SUMMARY_MAX_BYTES: usize = 256 * 1024;
/// Label for generated active-turn read ledger context.
const CURRENT_TURN_READ_LEDGER_LABEL: &str = "current-turn read ledger";

/// Prepares context blocks for provider requests and compaction.
pub(super) fn prepare_model_context_blocks(blocks: Vec<ContextBlock>) -> Vec<ContextBlock> {
    let deduped = dedupe_historical_context_blocks(blocks);
    let committed = with_generated_committed_evidence(deduped);
    with_generated_current_turn_read_ledger(committed)
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

/// Adds or refreshes the generated current-turn read ledger block.
fn with_generated_current_turn_read_ledger(blocks: Vec<ContextBlock>) -> Vec<ContextBlock> {
    let mut blocks = blocks
        .into_iter()
        .filter(|block| {
            !(block.source == ContextSourceKind::RuntimeHint
                && block.label == CURRENT_TURN_READ_LEDGER_LABEL)
        })
        .collect::<Vec<_>>();
    let Some(ledger) = build_current_turn_read_ledger_block(&blocks) else {
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
            && action_result_marker_type(block.content.lines().next().unwrap_or_default().trim())
                != Some("shell_command")
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
    let entry = evidence_entry_for_block(block, Some(COMMITTED_EVIDENCE_SUMMARY_MAX_BYTES))?;
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

/// Carries one generated current-turn read ledger record.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadLedgerRecord {
    /// Display key used for de-duplication and merging.
    key: String,
    /// Read or search classifier.
    kind: ShellReadObservationKind,
    /// Best-effort target path or scope.
    target: String,
    /// Best-effort line ranges covered for this target.
    ranges: BTreeSet<(usize, usize)>,
    /// Best-effort queries observed for this target.
    queries: BTreeSet<String>,
}

impl ReadLedgerRecord {
    /// Returns the provider-visible ledger line.
    fn render(&self) -> String {
        match self.kind {
            ShellReadObservationKind::Read => {
                if self.ranges.is_empty() {
                    format!("- read target={}", model_context_single_line(&self.target))
                } else {
                    format!(
                        "- read target={} ranges={}",
                        model_context_single_line(&self.target),
                        self.ranges
                            .iter()
                            .map(|(start, end)| format!("{start}-{end}"))
                            .collect::<Vec<_>>()
                            .join(",")
                    )
                }
            }
            ShellReadObservationKind::Search => {
                let query = if self.queries.is_empty() {
                    "(unknown)".to_string()
                } else {
                    self.queries.iter().cloned().collect::<Vec<_>>().join(",")
                };
                format!(
                    "- search target={} query={}",
                    model_context_single_line(&self.target),
                    model_context_single_line(&query)
                )
            }
        }
    }

    /// Merges another record for the same target into this one.
    fn merge(&mut self, other: Self) {
        self.ranges.extend(other.ranges);
        self.queries.extend(other.queries);
    }
}

/// Builds a compact current-turn read ledger from active action results.
fn build_current_turn_read_ledger_block(blocks: &[ContextBlock]) -> Option<ContextBlock> {
    let mut retained = Vec::<ReadLedgerRecord>::new();
    let mut index_by_key = HashMap::<String, usize>::new();
    for block in blocks {
        if block.source != ContextSourceKind::ActionResult {
            continue;
        }
        for record in read_ledger_records_for_block(block) {
            if let Some(index) = index_by_key.get(&record.key).copied() {
                retained[index].merge(record);
                continue;
            }
            index_by_key.insert(record.key.clone(), retained.len());
            retained.push(record);
        }
    }
    if retained.is_empty() {
        return None;
    }
    let mut lines = vec![
        "Recent successful read/search coverage for this active turn. Reuse it when it already contains the needed current range or match; read only missing or stale ranges.".to_string(),
    ];
    lines.extend(retained.into_iter().map(|record| record.render()));
    Some(ContextBlock {
        source: ContextSourceKind::RuntimeHint,
        label: CURRENT_TURN_READ_LEDGER_LABEL.to_string(),
        content: lines.join("\n"),
    })
}

/// Builds current-turn read ledger records from an active action result.
fn read_ledger_records_for_block(block: &ContextBlock) -> Vec<ReadLedgerRecord> {
    let marker_line = block.content.lines().next().unwrap_or_default().trim();
    let Some(status) = action_result_marker_status(marker_line) else {
        return Vec::new();
    };
    if status != "succeeded" {
        return Vec::new();
    }
    let Some(action_type) = action_result_marker_type(marker_line) else {
        return Vec::new();
    };
    if action_type != "shell_command" && action_type != "tool" {
        return Vec::new();
    }
    read_ledger_observations_for_block(block)
        .into_iter()
        .map(read_ledger_descriptor)
        .collect()
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

/// Builds a generated current-turn read ledger descriptor from one observation.
fn read_ledger_descriptor(observation: ShellReadObservation) -> ReadLedgerRecord {
    let key = match observation.kind {
        ShellReadObservationKind::Read => format!("read:{}", observation.target),
        ShellReadObservationKind::Search => format!("search:{}", observation.target),
    };
    let mut ranges = BTreeSet::new();
    for range in observation.ranges {
        ranges.insert((range.start_line, range.end_line));
    }
    let mut queries = BTreeSet::new();
    if let Some(query) = observation.query {
        queries.insert(query);
    }
    ReadLedgerRecord {
        key,
        kind: observation.kind,
        target: observation.target,
        ranges,
        queries,
    }
}

/// Returns structured read/search observations recorded in one block.
fn read_ledger_observations_for_block(block: &ContextBlock) -> Vec<ShellReadObservation> {
    let command = model_context_field_value(&block.content, "command").unwrap_or_default();
    read_ledger_observations_from_content(&command, &block.content)
}

/// Returns structured read/search observations from block content or fallback command parsing.
fn read_ledger_observations_from_content(
    command: &str,
    content: &str,
) -> Vec<ShellReadObservation> {
    let observations = content
        .lines()
        .filter_map(parse_read_observation_line)
        .collect::<Vec<_>>();
    if !observations.is_empty() {
        return observations;
    }
    crate::agent::shell_read_observations_for_command(command)
}

/// Parses one structured read-observation line from model-facing action-result context.
fn parse_read_observation_line(line: &str) -> Option<ShellReadObservation> {
    if let Some(payload) = line
        .trim()
        .strip_prefix("read_observation_json:")
        .map(str::trim)
    {
        return serde_json::from_str(payload).ok();
    }
    let payload = line.trim().strip_prefix("read_observation:")?.trim();
    let mut kind = None;
    let mut target = None;
    let mut ranges = Vec::new();
    let mut query = None;
    for field in payload.split_whitespace() {
        let (key, value) = field.split_once('=')?;
        match key {
            "kind" => {
                kind = match value {
                    "read" => Some(ShellReadObservationKind::Read),
                    "search" => Some(ShellReadObservationKind::Search),
                    _ => None,
                };
            }
            "target" => target = Some(value.to_string()),
            "ranges" => {
                for range in value.split(',') {
                    let (start, end) = range.split_once('-')?;
                    ranges.push(ShellReadRange {
                        start_line: start.parse().ok()?,
                        end_line: end.parse().ok()?,
                    });
                }
            }
            "query" => query = Some(value.to_string()),
            _ => {}
        }
    }
    Some(ShellReadObservation {
        kind: kind?,
        target: target?,
        ranges,
        query,
    })
}

/// Returns one compact committed-evidence line for a retained action result.
fn evidence_entry_for_block(
    block: &ContextBlock,
    summary_max_bytes: Option<usize>,
) -> Option<String> {
    let marker_line = block.content.lines().next().unwrap_or_default().trim();
    let command = model_context_field_value(&block.content, "command");
    let exit_code = model_context_field_value(&block.content, "exit_code");
    let status = action_result_marker_status(marker_line)
        .unwrap_or("historical")
        .to_string();
    let action_type = action_result_marker_type(marker_line).unwrap_or_else(|| {
        if command.is_some() {
            "shell_command"
        } else {
            "tool"
        }
    });
    let summary = evidence_summary_for_block(block, summary_max_bytes);
    let mut line = format!("- action_type={} status={status}", action_type);
    if let Some(command) = &command {
        line.push_str(&format!(" command={}", model_context_single_line(command)));
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

/// Builds model-visible summary text with an optional byte ceiling.
fn evidence_summary_for_block(block: &ContextBlock, max_bytes: Option<usize>) -> String {
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
    evidence_summary_text(summary_source, max_bytes)
}

/// Builds single-line summary text with an optional byte ceiling.
fn evidence_summary_text(value: &str, max_bytes: Option<usize>) -> String {
    let text = match max_bytes {
        Some(max_bytes) => truncate_model_context_summary(value, max_bytes),
        None => value.trim().to_string(),
    };
    model_context_single_line(&text)
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
