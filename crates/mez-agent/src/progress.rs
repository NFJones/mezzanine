//! Current-turn progress and rationale de-duplication helpers.
//!
//! This module keeps current-turn progress updates and investigative rationale
//! compact and non-redundant before they are persisted back into
//! model-visible context. It is pure text normalization and ledger logic;
//! product runtime state changes remain in the root adapter.

use std::collections::BTreeSet;

use crate::{
    AgentActionPayload, AgentTurnExecution, ContextBlock, ContextSourceKind, MaapBatch, SayStatus,
};

/// Label for ephemeral active-turn context that tracks visible progress output.
pub const PROGRESS_SAY_LEDGER_LABEL: &str = "current-turn progress say ledger";
/// Label for ephemeral active-turn context that tracks already-emitted
/// investigative rationale.
pub const RATIONALE_LEDGER_LABEL: &str = "current-turn rationale ledger";
/// Maximum progress `say` entries retained for one active turn.
pub const PROGRESS_SAY_LEDGER_ENTRY_LIMIT: usize = 3;
/// Maximum rationale entries retained for one active turn.
pub const RATIONALE_LEDGER_ENTRY_LIMIT: usize = 6;
/// Maximum characters retained from one progress `say` entry.
pub const PROGRESS_SAY_LEDGER_ENTRY_CHAR_LIMIT: usize = 512;
/// Maximum characters retained from one rationale entry.
pub const RATIONALE_LEDGER_ENTRY_CHAR_LIMIT: usize = 256;
/// Minimum shared significant tokens for treating two progress updates as the
/// same sequence point.
pub const PROGRESS_SAY_REDUNDANT_SHARED_TOKEN_FLOOR: usize = 5;

/// Rationale entries removed from one provider action batch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RationaleSuppression {
    /// Whether the batch-level rationale was cleared.
    pub batch_suppressed: bool,
    /// Action identifiers whose rationale was cleared.
    pub action_ids: Vec<String>,
}

impl RationaleSuppression {
    /// Returns the total number of rationale fields cleared.
    pub fn count(&self) -> usize {
        usize::from(self.batch_suppressed).saturating_add(self.action_ids.len())
    }
}

/// Extracts normalized progress `say` text from one provider execution.
///
/// # Parameters
/// - `execution`: The provider execution whose MAAP actions may include visible
///   progress text.
pub fn progress_say_entries_for_execution(execution: &AgentTurnExecution) -> Vec<String> {
    let Some(batch) = execution.response.action_batch.as_ref() else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for action in &batch.actions {
        let AgentActionPayload::Say { status, text, .. } = &action.payload else {
            continue;
        };
        if *status != SayStatus::Progress {
            continue;
        }
        let Some(entry) = normalize_progress_say_entry(text) else {
            continue;
        };
        if !entries.iter().any(|existing| existing == &entry) {
            entries.push(entry);
        }
    }
    entries
}

/// Extracts normalized rationale text from one provider execution.
///
/// Batch rationale and action rationale are current-turn guidance only. The
/// runtime uses this ledger to avoid rendering or replaying the same
/// investigative intent repeatedly within one active turn.
pub fn rationale_entries_for_execution(execution: &AgentTurnExecution) -> Vec<String> {
    let Some(batch) = execution.response.action_batch.as_ref() else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    if let Some(entry) = normalize_rationale_entry(&batch.rationale) {
        entries.push(entry);
    }
    for action in &batch.actions {
        let Some(entry) = normalize_rationale_entry(action.rationale.as_str()) else {
            continue;
        };
        if !entries
            .iter()
            .any(|existing| rationale_entries_are_redundant(existing, &entry))
        {
            entries.push(entry);
        }
    }
    entries
}

/// Normalizes one progress `say` text for compact context reuse.
///
/// # Parameters
/// - `text`: The model-authored visible progress text.
pub fn normalize_progress_say_entry(text: &str) -> Option<String> {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    Some(truncate_context_entry(
        &normalized,
        PROGRESS_SAY_LEDGER_ENTRY_CHAR_LIMIT,
    ))
}

/// Normalizes one rationale entry for compact same-turn reuse.
pub fn normalize_rationale_entry(text: &str) -> Option<String> {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    Some(truncate_context_entry(
        &normalized,
        RATIONALE_LEDGER_ENTRY_CHAR_LIMIT,
    ))
}

/// Truncates one context entry without splitting UTF-8.
///
/// # Parameters
/// - `text`: The context entry to bound.
/// - `limit`: The maximum number of Unicode scalar values to retain before
///   adding an ASCII truncation marker.
pub fn truncate_context_entry(text: &str, limit: usize) -> String {
    let mut output = text.chars().take(limit).collect::<String>();
    if text.chars().count() > limit {
        output.push_str("...");
    }
    output
}

/// Parses progress entries from an existing active-turn ledger block.
///
/// # Parameters
/// - `content`: The previous ledger block content.
pub fn progress_say_entries_from_ledger(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| line.strip_prefix("progress_say: "))
        .filter_map(normalize_progress_say_entry)
        .collect()
}

/// Parses rationale entries from an existing active-turn ledger block.
pub fn rationale_entries_from_ledger(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| line.strip_prefix("rationale: "))
        .filter_map(normalize_rationale_entry)
        .collect()
}

/// Extracts already-visible rationale entries from active context ledgers.
pub fn rationale_entries_from_context_blocks(blocks: &[ContextBlock]) -> Vec<String> {
    blocks
        .iter()
        .filter(|block| {
            block.source == ContextSourceKind::RuntimeHint && block.label == RATIONALE_LEDGER_LABEL
        })
        .flat_map(|block| rationale_entries_from_ledger(&block.content))
        .collect()
}

/// Clears batch and action rationale that repeats already-visible turn intent.
///
/// New rationale in the same batch becomes visible to later action rationale,
/// preserving the original deterministic suppression order. The returned
/// record lets product runtimes trace each mutation without owning the policy.
pub fn suppress_redundant_batch_rationale(
    batch: &mut MaapBatch,
    visible_entries: &[String],
) -> RationaleSuppression {
    let mut visible_entries = visible_entries.to_vec();
    let mut suppression = RationaleSuppression::default();
    if let Some(entry) = normalize_rationale_entry(&batch.rationale)
        && rationale_entry_repeats_existing(&entry, &visible_entries)
    {
        batch.rationale.clear();
        suppression.batch_suppressed = true;
    } else if let Some(entry) = normalize_rationale_entry(&batch.rationale) {
        visible_entries.push(entry);
    }
    for action in &mut batch.actions {
        let Some(entry) = normalize_rationale_entry(&action.rationale) else {
            continue;
        };
        if rationale_entry_repeats_existing(&entry, &visible_entries) {
            action.rationale.clear();
            suppression.action_ids.push(action.id.clone());
            continue;
        }
        visible_entries.push(entry);
    }
    suppression
}

/// Merges previous and newly emitted progress entries under the active-turn cap.
///
/// # Parameters
/// - `previous`: The previous ledger entries.
/// - `new_entries`: The progress entries emitted by the latest provider
///   execution.
pub fn merge_progress_say_entries(previous: Vec<String>, new_entries: Vec<String>) -> Vec<String> {
    let mut entries = previous;
    for entry in new_entries {
        if let Some(position) = entries
            .iter()
            .position(|existing| progress_say_entries_are_redundant(existing, &entry))
        {
            entries.remove(position);
        }
        entries.push(entry);
    }
    if entries.len() > PROGRESS_SAY_LEDGER_ENTRY_LIMIT {
        entries.split_off(entries.len() - PROGRESS_SAY_LEDGER_ENTRY_LIMIT)
    } else {
        entries
    }
}

/// Merges previous and newly emitted rationale entries under the active-turn cap.
pub fn merge_rationale_entries(previous: Vec<String>, new_entries: Vec<String>) -> Vec<String> {
    let mut entries = previous;
    for entry in new_entries {
        if let Some(position) = entries
            .iter()
            .position(|existing| rationale_entries_are_redundant(existing, &entry))
        {
            entries.remove(position);
        }
        entries.push(entry);
    }
    if entries.len() > RATIONALE_LEDGER_ENTRY_LIMIT {
        entries.split_off(entries.len() - RATIONALE_LEDGER_ENTRY_LIMIT)
    } else {
        entries
    }
}

/// Reports whether a rationale entry repeats one already visible in the turn.
pub fn rationale_entry_repeats_existing(entry: &str, existing_entries: &[String]) -> bool {
    existing_entries
        .iter()
        .any(|existing| rationale_entries_are_redundant(existing, entry))
}

/// Reports whether two progress entries communicate the same sequence point.
///
/// This intentionally stays conservative: exact normalized matches are always
/// redundant, while paraphrases need substantial significant-token overlap so a
/// later update can still mention the same component when it adds a new result.
///
/// # Parameters
/// - `left`: Previously emitted progress text.
/// - `right`: Candidate progress text.
pub fn progress_say_entries_are_redundant(left: &str, right: &str) -> bool {
    let Some(left) = normalize_progress_say_entry(left) else {
        return false;
    };
    let Some(right) = normalize_progress_say_entry(right) else {
        return false;
    };
    let left = left.to_ascii_lowercase();
    let right = right.to_ascii_lowercase();
    if left == right {
        return true;
    }
    if left.chars().count().min(right.chars().count()) >= 48
        && (left.contains(&right) || right.contains(&left))
    {
        return true;
    }
    let left_tokens = progress_say_significant_tokens(&left);
    let right_tokens = progress_say_significant_tokens(&right);
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return false;
    }
    let shared = left_tokens.intersection(&right_tokens).count();
    if shared < PROGRESS_SAY_REDUNDANT_SHARED_TOKEN_FLOOR {
        return false;
    }
    let smaller = left_tokens.len().min(right_tokens.len());
    let total = left_tokens.len().saturating_add(right_tokens.len());
    shared.saturating_mul(100) >= smaller.saturating_mul(72)
        && shared.saturating_mul(200) >= total.saturating_mul(55)
}

/// Reports whether two rationale entries communicate the same investigative
/// intent.
pub fn rationale_entries_are_redundant(left: &str, right: &str) -> bool {
    let Some(left) = normalize_rationale_entry(left) else {
        return false;
    };
    let Some(right) = normalize_rationale_entry(right) else {
        return false;
    };
    let left = left.to_ascii_lowercase();
    let right = right.to_ascii_lowercase();
    if left == right {
        return true;
    }
    if left.chars().count().min(right.chars().count()) >= 24
        && (left.contains(&right) || right.contains(&left))
    {
        return true;
    }
    let left_tokens = progress_say_significant_tokens(&left);
    let right_tokens = progress_say_significant_tokens(&right);
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return false;
    }
    let shared = left_tokens.intersection(&right_tokens).count();
    if shared < 4 {
        return false;
    }
    let smaller = left_tokens.len().min(right_tokens.len());
    let total = left_tokens.len().saturating_add(right_tokens.len());
    shared.saturating_mul(100) >= smaller.saturating_mul(70)
        && shared.saturating_mul(200) >= total.saturating_mul(54)
}

/// Extracts significant comparison tokens from one progress update.
///
/// # Parameters
/// - `text`: Normalized progress text to tokenize.
pub fn progress_say_significant_tokens(text: &str) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    let mut token = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() {
            for lowered in character.to_lowercase() {
                token.push(lowered);
            }
        } else {
            push_progress_say_token(&mut tokens, &mut token);
        }
    }
    push_progress_say_token(&mut tokens, &mut token);
    tokens
}

/// Adds one pending token to a progress comparison set when significant.
///
/// # Parameters
/// - `tokens`: The token set being built.
/// - `token`: The pending token buffer.
pub fn push_progress_say_token(tokens: &mut BTreeSet<String>, token: &mut String) {
    if token.is_empty() {
        return;
    }
    let stemmed = progress_say_stem_token(token);
    token.clear();
    if stemmed.len() < 3 || progress_say_token_is_stopword(&stemmed) {
        return;
    }
    tokens.insert(stemmed);
}

/// Builds the provider-visible content for the active-turn rationale ledger.
pub fn rationale_ledger_content(entries: &[String]) -> String {
    let mut lines = vec![
        "Already-emitted same-turn investigative intent. Avoid repeating these rationale lines unless the next action batch materially changes the reason."
            .to_string(),
    ];
    lines.extend(entries.iter().map(|entry| format!("rationale: {entry}")));
    lines.join("\n")
}

/// Applies light suffix normalization for progress comparison tokens.
///
/// # Parameters
/// - `token`: Lowercase token extracted from progress text.
pub fn progress_say_stem_token(token: &str) -> String {
    let mut stemmed = token.to_string();
    for suffix in ["ing", "ed", "es", "s"] {
        if stemmed.len() > suffix.len().saturating_add(4) && stemmed.ends_with(suffix) {
            stemmed.truncate(stemmed.len() - suffix.len());
            break;
        }
    }
    stemmed
}

/// Reports whether one token is too common to prove progress-update identity.
///
/// # Parameters
/// - `token`: Lowercase token extracted from progress text.
pub fn progress_say_token_is_stopword(token: &str) -> bool {
    matches!(
        token,
        "about"
            | "after"
            | "again"
            | "already"
            | "also"
            | "and"
            | "are"
            | "before"
            | "being"
            | "but"
            | "can"
            | "current"
            | "does"
            | "doing"
            | "done"
            | "for"
            | "from"
            | "has"
            | "have"
            | "here"
            | "into"
            | "its"
            | "just"
            | "more"
            | "need"
            | "now"
            | "only"
            | "rather"
            | "same"
            | "should"
            | "still"
            | "than"
            | "that"
            | "the"
            | "then"
            | "there"
            | "this"
            | "through"
            | "with"
            | "without"
            | "would"
    )
}

/// Formats the active-turn progress ledger for model context.
///
/// # Parameters
/// - `entries`: The bounded progress entries to include.
pub fn progress_say_ledger_content(entries: &[String]) -> String {
    let mut content = vec![
        "This is a bounded ledger of user-visible progress say messages already emitted during the current turn.".to_string(),
        "It is not a user request. Before emitting another progress say, compare against these lines and omit progress if it would restate the same owner, diagnosis, direction, phase, blocker, or validation result.".to_string(),
    ];
    content.extend(entries.iter().map(|entry| format!("progress_say: {entry}")));
    content.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies normalization collapses whitespace and bounds progress entries.
    #[test]
    fn progress_say_normalization_is_compact_and_bounded() {
        let text = format!("  checking   {}  ", "x".repeat(600));
        let normalized = normalize_progress_say_entry(&text).unwrap();
        assert!(normalized.starts_with("checking "));
        assert!(normalized.ends_with("..."));
        assert_eq!(
            normalized.chars().count(),
            PROGRESS_SAY_LEDGER_ENTRY_CHAR_LIMIT + 3
        );
    }

    /// Verifies semantically overlapping progress updates replace older entries.
    #[test]
    fn progress_say_merge_replaces_redundant_sequence_points() {
        let previous = vec![
            "Inspecting provider routing and model profile fallback behavior".to_string(),
            "Checking unrelated terminal rendering".to_string(),
        ];
        let new_entries =
            vec!["Inspected provider routing and model profile fallback behavior".to_string()];
        let merged = merge_progress_say_entries(previous, new_entries);
        assert_eq!(merged.len(), 2);
        assert_eq!(
            merged.last().unwrap(),
            "Inspected provider routing and model profile fallback behavior"
        );
    }

    /// Verifies ledger parsing and formatting preserve bounded rationale entries.
    #[test]
    fn rationale_ledger_round_trip_preserves_entries() {
        let entries = vec![
            "Trace the state owner before changing the adapter".to_string(),
            "Validate the lower contract directly".to_string(),
        ];
        let content = rationale_ledger_content(&entries);
        assert_eq!(rationale_entries_from_ledger(&content), entries);
    }

    /// Verifies canonical suppression clears rationale repeated from prior
    /// context and from earlier fields in the same action batch.
    #[test]
    fn rationale_suppression_mutates_batch_and_reports_trace_facts() {
        let mut batch = crate::parse_fenced_maap_action_batch(
            r#"```mezzanine-action-json
{"protocol":"maap/1","turn_id":"turn-1","agent_id":"agent-1","rationale":"Inspect the provider retry owner","actions":[{"id":"a1","type":"say","rationale":"Inspect the provider retry owner","status":"progress","content_type":"text/plain","text":"Checking ownership"},{"id":"a2","type":"say","rationale":"Validate the moved retry policy","status":"final","content_type":"text/plain","text":"Done"}],"final":true}
```"#,
        )
        .unwrap()
        .unwrap();
        let first_action_id = batch.actions[0].id.clone();
        let suppression = suppress_redundant_batch_rationale(
            &mut batch,
            &["Inspect the provider retry owner".to_string()],
        );

        assert!(batch.rationale.is_empty());
        assert!(batch.actions[0].rationale.is_empty());
        assert_eq!(
            batch.actions[1].rationale,
            "Validate the moved retry policy"
        );
        assert!(suppression.batch_suppressed);
        assert_eq!(suppression.action_ids, [first_action_id]);
        assert_eq!(suppression.count(), 2);
    }
}
