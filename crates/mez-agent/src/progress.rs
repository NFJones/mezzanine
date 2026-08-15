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
/// One ordered presentation event extracted from an incomplete MAAP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamingSayEvent {
    /// A structurally established supported `say` action is ready for display.
    Started {
        /// Zero-based position in the MAAP `actions` array.
        action_index: usize,
        /// Declared lifecycle status of the visible action.
        status: SayStatus,
        /// Normalized supported media type used to render the source.
        content_type: String,
    },
    /// Newly decoded source text for one established `say` action.
    TextDelta {
        /// Zero-based position in the MAAP `actions` array.
        action_index: usize,
        /// Ordered source suffix that has not been emitted previously.
        text: String,
    },
    /// The action's JSON `text` string has closed.
    TextComplete {
        /// Zero-based position in the MAAP `actions` array.
        action_index: usize,
    },
}

/// Per-action source state already exposed by [`StreamingSayExtractor`].
#[derive(Debug, Clone, Default)]
struct EmittedStreamingSay {
    started: bool,
    text: String,
    complete: bool,
}

/// Fail-closed extractor for ordered source events from direct or fenced MAAP.
///
/// The provider HTTP response limit remains the resource bound. This extractor
/// deliberately has no presentation-specific input or visible-text limit.
#[derive(Debug, Default)]
pub struct StreamingSayExtractor {
    input: String,
    emitted: Vec<EmittedStreamingSay>,
    disabled: bool,
}

impl StreamingSayExtractor {
    /// Appends one provider fragment and returns every newly established event.
    pub fn push_delta(&mut self, delta: &str) -> Vec<StreamingSayEvent> {
        if self.disabled {
            return Vec::new();
        }
        self.input.push_str(delta);
        let Some(actions) = streaming_say_actions(&self.input) else {
            return Vec::new();
        };
        let required_states = actions
            .last()
            .map_or(0, |action| action.action_index.saturating_add(1));
        if self.emitted.len() < required_states {
            self.emitted
                .resize_with(required_states, EmittedStreamingSay::default);
        }

        let mut events = Vec::new();
        for action in actions {
            let state = &mut self.emitted[action.action_index];
            if !state.started {
                state.started = true;
                events.push(StreamingSayEvent::Started {
                    action_index: action.action_index,
                    status: action.status,
                    content_type: action.content_type,
                });
            }
            let Some(delta) = action.text.strip_prefix(&state.text) else {
                self.disable();
                return Vec::new();
            };
            if !delta.is_empty() {
                events.push(StreamingSayEvent::TextDelta {
                    action_index: action.action_index,
                    text: delta.to_string(),
                });
                state.text = action.text;
            }
            if action.complete && !state.complete {
                state.complete = true;
                events.push(StreamingSayEvent::TextComplete {
                    action_index: action.action_index,
                });
            }
        }
        events
    }

    /// Permanently suppresses extraction for the current provider response.
    pub fn disable(&mut self) {
        self.disabled = true;
        self.input.clear();
        self.emitted.clear();
    }
}

/// One currently classifiable streamed action and its cumulative source.
struct StreamingSayAction {
    action_index: usize,
    status: SayStatus,
    content_type: String,
    text: String,
    complete: bool,
}

/// Extracts all structurally established supported `say` actions.
fn streaming_say_actions(input: &str) -> Option<Vec<StreamingSayAction>> {
    let object = provisional_maap_object(input)?;
    let actions = direct_json_field_value_start(object, "actions")?
        .trim_start()
        .strip_prefix('[')?;
    let mut extracted = Vec::new();
    for (action_index, action) in direct_json_array_objects(actions).into_iter().enumerate() {
        if json_string_field(action, "type")?.as_str() != "say" {
            continue;
        }
        let status = SayStatus::parse(&json_string_field(action, "status")?)?;
        let content_type = json_string_field(action, "content_type")?;
        let content_type = crate::normalize_agent_output_content_type(Some(&content_type));
        if content_type != crate::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
            && !crate::agent_output_content_type_is_markdown(&content_type)
            && !crate::agent_output_content_type_is_diff(&content_type)
        {
            continue;
        }
        let text_start = direct_json_field_value_start(action, "text")?.trim_start();
        let (text, complete) = decode_incomplete_json_string(text_start)?;
        extracted.push(StreamingSayAction {
            action_index,
            status,
            content_type,
            text,
            complete,
        });
    }
    Some(extracted)
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

/// Finds one direct JSON object field and returns its value suffix.
fn direct_json_field_value_start<'a>(input: &'a str, key: &str) -> Option<&'a str> {
    let bytes = input.as_bytes();
    let mut index = input.find('{')?.saturating_add(1);
    let mut depth = 1_usize;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                let end = complete_json_string_end(&input[index..])?;
                if depth == 1
                    && serde_json::from_str::<String>(&input[index..index + end])
                        .ok()?
                        .as_str()
                        == key
                {
                    let suffix = input[index + end..].trim_start();
                    return suffix.strip_prefix(':');
                }
                index += end;
            }
            b'{' | b'[' => {
                depth = depth.saturating_add(1);
                index += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                index += 1;
            }
            _ => index += 1,
        }
    }
    None
}

/// Returns each complete or currently incomplete direct object in one array.
fn direct_json_array_objects(input: &str) -> Vec<&str> {
    let bytes = input.as_bytes();
    let mut objects = Vec::new();
    let mut index = 0_usize;
    let mut array_depth = 1_usize;
    while index < bytes.len() && array_depth == 1 {
        match bytes[index] {
            b'"' => {
                let Some(end) = complete_json_string_end(&input[index..]) else {
                    break;
                };
                index += end;
            }
            b'[' => {
                array_depth += 1;
                index += 1;
            }
            b']' => break,
            b'{' => {
                let start = index;
                let mut object_depth = 1_usize;
                index += 1;
                while index < bytes.len() && object_depth > 0 {
                    match bytes[index] {
                        b'"' => {
                            let Some(end) = complete_json_string_end(&input[index..]) else {
                                index = bytes.len();
                                break;
                            };
                            index += end;
                        }
                        b'{' => {
                            object_depth += 1;
                            index += 1;
                        }
                        b'}' => {
                            object_depth -= 1;
                            index += 1;
                        }
                        _ => index += 1,
                    }
                }
                objects.push(&input[start..index]);
            }
            _ => index += 1,
        }
    }
    objects
}

/// Decodes one complete JSON string field.
fn json_string_field(input: &str, key: &str) -> Option<String> {
    let value = direct_json_field_value_start(input, key)?.trim_start();
    let end = complete_json_string_end(value)?;
    serde_json::from_str(&value[..end]).ok()
}

/// Decodes the longest valid prefix and reports whether the string has closed.
fn decode_incomplete_json_string(input: &str) -> Option<(String, bool)> {
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
            return Some((decoded, complete_end.is_some()));
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

    /// Verifies fragmented direct and fenced MAAP emit ordered source events.
    #[test]
    fn streaming_say_extractor_handles_fragmented_direct_and_fenced_maap() {
        for prefix in ["", "```mezzanine-action-json\n"] {
            let mut extractor = StreamingSayExtractor::default();
            assert!(extractor.push_delta(prefix).is_empty());
            assert_eq!(
                extractor.push_delta(
                    r#"{"rationale":"stream","actions":[{"type":"say","status":"final","content_type":"text/markdown; charset=utf-8","text":"Hello "#,
                ),
                vec![
                    StreamingSayEvent::Started {
                        action_index: 0,
                        status: SayStatus::Final,
                        content_type: crate::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE.to_string(),
                    },
                    StreamingSayEvent::TextDelta {
                        action_index: 0,
                        text: "Hello ".to_string(),
                    },
                ]
            );
            assert_eq!(
                extractor.push_delta(r#"**wörld**\nnext"}] }"#),
                vec![
                    StreamingSayEvent::TextDelta {
                        action_index: 0,
                        text: "**wörld**\nnext".to_string(),
                    },
                    StreamingSayEvent::TextComplete { action_index: 0 },
                ]
            );
        }
    }

    /// Verifies unsupported fields fail closed without leaking nested text.
    #[test]
    fn streaming_say_extractor_rejects_unsafe_streams() {
        for raw in [
            r#"{"actions":[{"type":"shell_command","content_type":"text/plain","text":"secret"}]}"#,
            r#"{"rationale":"{\"actions\":[{\"type\":\"say\",\"status\":\"final\",\"content_type\":\"text/plain\",\"text\":\"secret\"}]}","actions":[]}"#,
            r#"{"actions":[{"type":"say","status":"unknown","content_type":"text/plain","text":"secret"}]}"#,
        ] {
            assert!(StreamingSayExtractor::default().push_delta(raw).is_empty());
        }
    }

    /// Verifies all supported media types, multiple actions, and long text are untruncated.
    #[test]
    fn streaming_say_extractor_supports_multiple_untruncated_actions() {
        let long_text = "x".repeat(20_000);
        let input = format!(
            r#"{{"actions":[{{"type":"say","status":"progress","content_type":"text/x-diff","text":"--- a\n+++ b"}},{{"type":"shell_command","summary":"skip","command":"true"}},{{"type":"say","status":"final","content_type":"text/plain","text":"{long_text}"}}]}}"#
        );
        let events = StreamingSayExtractor::default().push_delta(&input);
        assert_eq!(
            events.first(),
            Some(&StreamingSayEvent::Started {
                action_index: 0,
                status: SayStatus::Progress,
                content_type: crate::AGENT_OUTPUT_TEXT_DIFF_CONTENT_TYPE.to_string(),
            })
        );
        assert!(events.iter().any(|event| matches!(
            event,
            StreamingSayEvent::TextDelta { action_index: 2, text } if text == &long_text
        )));
    }

    /// Verifies incomplete JSON escapes emit only after they decode to full characters.
    #[test]
    fn streaming_say_extractor_waits_for_complete_json_escapes() {
        let mut extractor = StreamingSayExtractor::default();
        let events = extractor.push_delta(
            r#"{"actions":[{"type":"say","status":"final","content_type":"text/plain","text":"a\uD83D"#,
        );
        assert_eq!(
            events,
            vec![
                StreamingSayEvent::Started {
                    action_index: 0,
                    status: SayStatus::Final,
                    content_type: crate::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
                },
                StreamingSayEvent::TextDelta {
                    action_index: 0,
                    text: "a".to_string(),
                },
            ]
        );
        assert_eq!(
            extractor.push_delta(r#"\uDE00\n"}] }"#),
            vec![
                StreamingSayEvent::TextDelta {
                    action_index: 0,
                    text: "😀\n".to_string(),
                },
                StreamingSayEvent::TextComplete { action_index: 0 },
            ]
        );
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
