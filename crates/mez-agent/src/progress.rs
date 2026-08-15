//! Current-turn progress and rationale de-duplication helpers.
//!
//! This module normalizes model-authored progress and rationale and suppresses
//! redundant rationale fields within one response. It deliberately does not
//! serialize controller bookkeeping back into model-visible context: durable
//! assistant events already preserve the chronology needed for continuation.

use std::collections::BTreeSet;

use crate::{AgentActionPayload, AgentTurnExecution, MaapBatch, SayStatus};

/// Maximum characters retained while comparing one progress `say` entry.
const PROGRESS_ENTRY_CHAR_LIMIT: usize = 512;
/// Maximum characters retained while comparing one rationale entry.
const RATIONALE_ENTRY_CHAR_LIMIT: usize = 256;
/// Minimum shared significant tokens for treating two progress updates as the
/// same sequence point.
const PROGRESS_REDUNDANT_SHARED_TOKEN_FLOOR: usize = 5;
/// Maximum streamed MAAP characters retained while deriving one preview.
const PROVISIONAL_SAY_INPUT_CHAR_LIMIT: usize = 16 * 1_024;
/// Maximum visible characters exposed by one provisional preview.
const PROVISIONAL_SAY_TEXT_CHAR_LIMIT: usize = 1_024;

/// Display-safe provisional text extracted from an incomplete MAAP `say` action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionalSayPreview {
    /// Normalized supported media type declared by the provisional action.
    pub content_type: String,
    /// Cumulative decoded text available at the current stream boundary.
    pub text: String,
}

/// Bounded fail-closed extractor for streamed direct or fenced MAAP output.
#[derive(Debug, Default)]
pub struct ProvisionalSayExtractor {
    input: String,
    disabled: bool,
}

impl ProvisionalSayExtractor {
    /// Appends one provider delta and returns the newest cumulative safe preview.
    pub fn push_delta(&mut self, delta: &str) -> Option<ProvisionalSayPreview> {
        if self.disabled {
            return None;
        }
        if self
            .input
            .chars()
            .count()
            .saturating_add(delta.chars().count())
            > PROVISIONAL_SAY_INPUT_CHAR_LIMIT
        {
            self.disabled = true;
            self.input.clear();
            return None;
        }
        self.input.push_str(delta);
        provisional_say_preview(&self.input)
    }

    /// Permanently suppresses preview extraction for the current provider response.
    pub fn disable(&mut self) {
        self.disabled = true;
        self.input.clear();
    }
}

/// Extracts only a first-action `say` whose type and supported media type are
/// already complete before its text field begins.
fn provisional_say_preview(input: &str) -> Option<ProvisionalSayPreview> {
    let object = provisional_maap_object(input)?;
    let actions = json_key_value_start(object, "actions")?;
    let action_start = actions
        .trim_start()
        .strip_prefix('[')?
        .trim_start()
        .strip_prefix('{')?;
    if json_string_field(action_start, "type")?.as_str() != "say" {
        return None;
    }
    let content_type = json_string_field(action_start, "content_type")?;
    let content_type = crate::normalize_agent_output_content_type(Some(&content_type));
    if !crate::agent_output_content_type_is_markdown(&content_type)
        && content_type != crate::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
    {
        return None;
    }
    let text_start = json_key_value_start(action_start, "text")?.trim_start();
    let text = decode_incomplete_json_string(text_start)?;
    if text.is_empty() {
        return None;
    }
    Some(ProvisionalSayPreview {
        content_type,
        text: text.chars().take(PROVISIONAL_SAY_TEXT_CHAR_LIMIT).collect(),
    })
}

/// Returns the direct JSON object or the body of one recognized MAAP fence.
fn provisional_maap_object(input: &str) -> Option<&str> {
    let trimmed = input.trim_start();
    if trimmed.starts_with('{') {
        return Some(trimmed);
    }
    let body = trimmed.strip_prefix("```mezzanine-action-json")?;
    Some(body.trim_start_matches(['\r', '\n']))
}

/// Finds one complete JSON string key and returns its value suffix.
fn json_key_value_start<'a>(input: &'a str, key: &str) -> Option<&'a str> {
    let needle = serde_json::to_string(key).ok()?;
    let mut search = input;
    while let Some(index) = search.find(&needle) {
        let suffix = &search[index + needle.len()..];
        if let Some(value) = suffix.trim_start().strip_prefix(':') {
            return Some(value);
        }
        search = &suffix[1.min(suffix.len())..];
    }
    None
}

/// Decodes one complete JSON string field.
fn json_string_field(input: &str, key: &str) -> Option<String> {
    let value = json_key_value_start(input, key)?.trim_start();
    let end = complete_json_string_end(value)?;
    serde_json::from_str(&value[..end]).ok()
}

/// Decodes the longest valid prefix of an incomplete JSON string.
fn decode_incomplete_json_string(input: &str) -> Option<String> {
    let input = input.strip_prefix('"')?;
    let mut escaped = false;
    let mut complete_end = None;
    for (index, character) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '"' => {
                complete_end = Some(index);
                break;
            }
            _ => {}
        }
    }
    let candidate = &input[..complete_end.unwrap_or(input.len())];
    for end in candidate
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(candidate.len()))
        .rev()
    {
        let encoded = format!("\"{}\"", &candidate[..end]);
        if let Ok(decoded) = serde_json::from_str::<String>(&encoded) {
            return Some(decoded);
        }
    }
    None
}

/// Returns the byte end immediately after one complete JSON string.
fn complete_json_string_end(input: &str) -> Option<usize> {
    let rest = input.strip_prefix('"')?;
    let mut escaped = false;
    for (index, character) in rest.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            return Some(index + character.len_utf8() + 1);
        }
    }
    None
}

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
        PROGRESS_ENTRY_CHAR_LIMIT,
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
        RATIONALE_ENTRY_CHAR_LIMIT,
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

/// Clears batch and action rationale that repeats earlier intent in the same
/// response or an explicitly supplied controller-side comparison set.
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
    if shared < PROGRESS_REDUNDANT_SHARED_TOKEN_FLOOR {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies fragmented direct and fenced MAAP expose only established say text.
    #[test]
    fn provisional_say_extractor_handles_fragmented_direct_and_fenced_maap() {
        for prefix in ["", "```mezzanine-action-json\n"] {
            let mut extractor = ProvisionalSayExtractor::default();
            assert_eq!(extractor.push_delta(prefix), None);
            assert_eq!(
                extractor.push_delta(
                    r#"{"rationale":"stream","actions":[{"type":"say","status":"final","content_type":"text/markdown; charset=utf-8","text":"Hello "#,
                ),
                Some(ProvisionalSayPreview {
                    content_type: crate::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
                    text: "Hello ".to_string(),
                })
            );
            assert_eq!(
                extractor
                    .push_delta(r#"**wörld**\nnext"}] }"#)
                    .unwrap()
                    .text,
                "Hello **wörld**\nnext"
            );
        }
    }

    /// Verifies non-say, unsupported, and oversized streams fail closed.
    #[test]
    fn provisional_say_extractor_rejects_unsafe_streams() {
        for raw in [
            r#"{"actions":[{"type":"shell_command","content_type":"text/plain","text":"secret"}]}"#,
            r#"{"actions":[{"type":"say","content_type":"text/x-diff","text":"secret"}]}"#,
        ] {
            assert_eq!(ProvisionalSayExtractor::default().push_delta(raw), None);
        }
        let mut oversized = ProvisionalSayExtractor::default();
        assert_eq!(
            oversized.push_delta(&"x".repeat(PROVISIONAL_SAY_INPUT_CHAR_LIMIT + 1)),
            None
        );
        assert_eq!(oversized.push_delta("safe"), None);
    }

    /// Verifies normalization collapses whitespace and bounds progress entries.
    #[test]
    fn progress_say_normalization_is_compact_and_bounded() {
        let text = format!("  checking   {}  ", "x".repeat(600));
        let normalized = normalize_progress_say_entry(&text).unwrap();
        assert!(normalized.starts_with("checking "));
        assert!(normalized.ends_with("..."));
        assert_eq!(normalized.chars().count(), PROGRESS_ENTRY_CHAR_LIMIT + 3);
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
