//! Current-turn progress and rationale de-duplication helpers.
//!
//! This module keeps current-turn progress updates and investigative rationale
//! compact and non-redundant before they are persisted back into
//! model-visible context. It is pure text normalization and ledger logic;
//! runtime service state changes remain in the parent agent module.

use crate::agent::{AgentActionPayload, AgentTurnExecution, SayStatus};
use std::collections::BTreeSet;

/// Label for ephemeral active-turn context that tracks visible progress output.
pub(super) const RUNTIME_PROGRESS_SAY_LEDGER_LABEL: &str = "current-turn progress say ledger";
/// Label for ephemeral active-turn context that tracks already-emitted
/// investigative rationale.
pub(super) const RUNTIME_RATIONALE_LEDGER_LABEL: &str = "current-turn rationale ledger";
/// Maximum progress `say` entries retained for one active turn.
pub(super) const RUNTIME_PROGRESS_SAY_LEDGER_ENTRY_LIMIT: usize = 3;
/// Maximum rationale entries retained for one active turn.
pub(super) const RUNTIME_RATIONALE_LEDGER_ENTRY_LIMIT: usize = 6;
/// Maximum characters retained from one progress `say` entry.
pub(super) const RUNTIME_PROGRESS_SAY_LEDGER_ENTRY_CHAR_LIMIT: usize = 512;
/// Maximum characters retained from one rationale entry.
pub(super) const RUNTIME_RATIONALE_LEDGER_ENTRY_CHAR_LIMIT: usize = 256;
/// Minimum shared significant tokens for treating two progress updates as the
/// same sequence point.
pub(super) const RUNTIME_PROGRESS_SAY_REDUNDANT_SHARED_TOKEN_FLOOR: usize = 5;

/// Extracts normalized progress `say` text from one provider execution.
///
/// # Parameters
/// - `execution`: The provider execution whose MAAP actions may include visible
///   progress text.
pub(super) fn runtime_progress_say_entries_for_execution(
    execution: &AgentTurnExecution,
) -> Vec<String> {
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
        let Some(entry) = runtime_normalize_progress_say_entry(text) else {
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
pub(super) fn runtime_rationale_entries_for_execution(
    execution: &AgentTurnExecution,
) -> Vec<String> {
    let Some(batch) = execution.response.action_batch.as_ref() else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    if let Some(entry) = runtime_normalize_rationale_entry(&batch.rationale) {
        entries.push(entry);
    }
    for action in &batch.actions {
        let Some(entry) = runtime_normalize_rationale_entry(action.rationale.as_str()) else {
            continue;
        };
        if !entries
            .iter()
            .any(|existing| runtime_rationale_entries_are_redundant(existing, &entry))
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
pub(super) fn runtime_normalize_progress_say_entry(text: &str) -> Option<String> {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    Some(runtime_truncate_context_entry(
        &normalized,
        RUNTIME_PROGRESS_SAY_LEDGER_ENTRY_CHAR_LIMIT,
    ))
}

/// Normalizes one rationale entry for compact same-turn reuse.
pub(super) fn runtime_normalize_rationale_entry(text: &str) -> Option<String> {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    Some(runtime_truncate_context_entry(
        &normalized,
        RUNTIME_RATIONALE_LEDGER_ENTRY_CHAR_LIMIT,
    ))
}

/// Truncates one context entry without splitting UTF-8.
///
/// # Parameters
/// - `text`: The context entry to bound.
/// - `limit`: The maximum number of Unicode scalar values to retain before
///   adding an ASCII truncation marker.
pub(super) fn runtime_truncate_context_entry(text: &str, limit: usize) -> String {
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
pub(super) fn runtime_progress_say_entries_from_ledger(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| line.strip_prefix("progress_say: "))
        .filter_map(runtime_normalize_progress_say_entry)
        .collect()
}

/// Parses rationale entries from an existing active-turn ledger block.
pub(super) fn runtime_rationale_entries_from_ledger(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| line.strip_prefix("rationale: "))
        .filter_map(runtime_normalize_rationale_entry)
        .collect()
}

/// Merges previous and newly emitted progress entries under the active-turn cap.
///
/// # Parameters
/// - `previous`: The previous ledger entries.
/// - `new_entries`: The progress entries emitted by the latest provider
///   execution.
pub(super) fn runtime_merge_progress_say_entries(
    previous: Vec<String>,
    new_entries: Vec<String>,
) -> Vec<String> {
    let mut entries = previous;
    for entry in new_entries {
        if let Some(position) = entries
            .iter()
            .position(|existing| runtime_progress_say_entries_are_redundant(existing, &entry))
        {
            entries.remove(position);
        }
        entries.push(entry);
    }
    if entries.len() > RUNTIME_PROGRESS_SAY_LEDGER_ENTRY_LIMIT {
        entries.split_off(entries.len() - RUNTIME_PROGRESS_SAY_LEDGER_ENTRY_LIMIT)
    } else {
        entries
    }
}

/// Merges previous and newly emitted rationale entries under the active-turn cap.
pub(super) fn runtime_merge_rationale_entries(
    previous: Vec<String>,
    new_entries: Vec<String>,
) -> Vec<String> {
    let mut entries = previous;
    for entry in new_entries {
        if let Some(position) = entries
            .iter()
            .position(|existing| runtime_rationale_entries_are_redundant(existing, &entry))
        {
            entries.remove(position);
        }
        entries.push(entry);
    }
    if entries.len() > RUNTIME_RATIONALE_LEDGER_ENTRY_LIMIT {
        entries.split_off(entries.len() - RUNTIME_RATIONALE_LEDGER_ENTRY_LIMIT)
    } else {
        entries
    }
}

/// Reports whether a progress entry repeats one already visible in the turn.
///
/// # Parameters
/// - `entry`: The candidate progress text.
/// - `existing_entries`: Bounded progress entries already shown this turn.
pub(super) fn runtime_progress_say_entry_repeats_existing(
    entry: &str,
    existing_entries: &[String],
) -> bool {
    existing_entries
        .iter()
        .any(|existing| runtime_progress_say_entries_are_redundant(existing, entry))
}

/// Reports whether a rationale entry repeats one already visible in the turn.
pub(super) fn runtime_rationale_entry_repeats_existing(
    entry: &str,
    existing_entries: &[String],
) -> bool {
    existing_entries
        .iter()
        .any(|existing| runtime_rationale_entries_are_redundant(existing, entry))
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
pub(super) fn runtime_progress_say_entries_are_redundant(left: &str, right: &str) -> bool {
    let Some(left) = runtime_normalize_progress_say_entry(left) else {
        return false;
    };
    let Some(right) = runtime_normalize_progress_say_entry(right) else {
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
    let left_tokens = runtime_progress_say_significant_tokens(&left);
    let right_tokens = runtime_progress_say_significant_tokens(&right);
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return false;
    }
    let shared = left_tokens.intersection(&right_tokens).count();
    if shared < RUNTIME_PROGRESS_SAY_REDUNDANT_SHARED_TOKEN_FLOOR {
        return false;
    }
    let smaller = left_tokens.len().min(right_tokens.len());
    let total = left_tokens.len().saturating_add(right_tokens.len());
    shared.saturating_mul(100) >= smaller.saturating_mul(72)
        && shared.saturating_mul(200) >= total.saturating_mul(55)
}

/// Reports whether two rationale entries communicate the same investigative
/// intent.
pub(super) fn runtime_rationale_entries_are_redundant(left: &str, right: &str) -> bool {
    let Some(left) = runtime_normalize_rationale_entry(left) else {
        return false;
    };
    let Some(right) = runtime_normalize_rationale_entry(right) else {
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
    let left_tokens = runtime_progress_say_significant_tokens(&left);
    let right_tokens = runtime_progress_say_significant_tokens(&right);
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
pub(super) fn runtime_progress_say_significant_tokens(text: &str) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    let mut token = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() {
            for lowered in character.to_lowercase() {
                token.push(lowered);
            }
        } else {
            runtime_push_progress_say_token(&mut tokens, &mut token);
        }
    }
    runtime_push_progress_say_token(&mut tokens, &mut token);
    tokens
}

/// Adds one pending token to a progress comparison set when significant.
///
/// # Parameters
/// - `tokens`: The token set being built.
/// - `token`: The pending token buffer.
pub(super) fn runtime_push_progress_say_token(tokens: &mut BTreeSet<String>, token: &mut String) {
    if token.is_empty() {
        return;
    }
    let stemmed = runtime_progress_say_stem_token(token);
    token.clear();
    if stemmed.len() < 3 || runtime_progress_say_token_is_stopword(&stemmed) {
        return;
    }
    tokens.insert(stemmed);
}

/// Builds the provider-visible content for the active-turn rationale ledger.
pub(super) fn runtime_rationale_ledger_content(entries: &[String]) -> String {
    let mut lines = vec![
        "[current-turn rationale ledger]".to_string(),
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
pub(super) fn runtime_progress_say_stem_token(token: &str) -> String {
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
pub(super) fn runtime_progress_say_token_is_stopword(token: &str) -> bool {
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
pub(super) fn runtime_progress_say_ledger_content(entries: &[String]) -> String {
    let mut content = vec![
        "This is a bounded ledger of user-visible progress say messages already emitted during the current turn.".to_string(),
        "It is not a user request. Before emitting another progress say, compare against these lines and omit progress if it would restate the same owner, diagnosis, direction, phase, blocker, or validation result.".to_string(),
    ];
    content.extend(entries.iter().map(|entry| format!("progress_say: {entry}")));
    content.join("\n")
}
