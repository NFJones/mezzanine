//! Pure rendering of ordinary raw transcript rows beside canonical envelopes.
//!
//! Runtime replay and content revision need identical role, omission, privacy
//! and UTF-8 rules. Callers independently qualify occurrence and producer custody
//! and suppress display copies before using this projection. Rendering grants
//! no native ownership, task lifecycle, append receipt or publication authority.

use super::{TranscriptEntry, TranscriptRole};
use crate::{ContextBlock, ContextPlacement, ContextSourceKind, ProviderTranscriptEvent};

const TOOL_CONTEXT_LIMIT_BYTES: usize = 256 * 1024;
const LEGACY_MAAP_ASSISTANT_CONTEXT: &str =
    "[legacy MAAP assistant execution omitted from transcript replay]";

/// Renders one ordinary row with a caller-owned presentation label. Empty or
/// omitted metadata yields None. Tool history omits original private bodies and
/// bounds sanitized output at 256 KiB on a UTF-8 boundary, with an explicit
/// original-length marker. Ordinary user/assistant bytes have no summary target.
/// Canonical semantic envelopes must use their own typed rendering instead.
pub fn transcript_literal_context_block(
    entry: &TranscriptEntry,
    label: String,
) -> Option<ContextBlock> {
    if entry.content.trim().is_empty() {
        return None;
    }
    let source = match entry.role {
        TranscriptRole::User => ContextSourceKind::TranscriptUser,
        TranscriptRole::Assistant => ContextSourceKind::TranscriptAssistant,
        TranscriptRole::Tool if entry.content.trim_start().starts_with("[action_result ") => {
            ContextSourceKind::ActionResult
        }
        TranscriptRole::Tool => ContextSourceKind::TranscriptTool,
        TranscriptRole::System
            if ProviderTranscriptEvent::from_transcript_content(&entry.content).is_some() =>
        {
            ContextSourceKind::TranscriptTool
        }
        TranscriptRole::System => ContextSourceKind::Transcript,
    };
    let content = match entry.role {
        TranscriptRole::System => ProviderTranscriptEvent::from_transcript_content(&entry.content)
            .and_then(|event| event.sanitized_for_historical_replay())
            .map(|event| event.to_transcript_content()),
        TranscriptRole::Tool => tool_content(&entry.content),
        TranscriptRole::User if is_expanded_skill(&entry.content) => None,
        TranscriptRole::Assistant if is_maap_action_json(&entry.content) => {
            Some(LEGACY_MAAP_ASSISTANT_CONTEXT.to_string())
        }
        _ => Some(entry.content.clone()),
    }?;
    Some(ContextBlock {
        source,
        placement: ContextPlacement::ConversationAppend,
        label,
        content,
    })
}

/// Historical tool output remains secret-safe independently of current-turn
/// exact evidence. Omissions are applied before any truncation or accounting.
fn tool_content(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if trimmed.is_empty()
        || (trimmed.starts_with("[action_result ")
            && [" fetch_url ", " web_search "]
                .iter()
                .any(|needle| trimmed.contains(needle)))
        || trimmed.contains("action_type=request_skills")
        || trimmed.contains("action_type=call_skill")
    {
        return None;
    }
    let sanitized = crate::historical_tool_result_context_content(trimmed)?;
    Some(truncate_tool_content(&sanitized))
}

fn truncate_tool_content(content: &str) -> String {
    if content.len() <= TOOL_CONTEXT_LIMIT_BYTES {
        return content.to_string();
    }
    let mut end = TOOL_CONTEXT_LIMIT_BYTES;
    while !content.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!(
        "{}...[mez: transcript tool context truncated; original_bytes={}]",
        &content[..end],
        content.len()
    )
}

fn is_expanded_skill(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with("# Skill: ")
        && trimmed.contains("\nSource: ")
        && trimmed.contains("\nPath: ")
        && trimmed.contains("\nInvocation state: this skill is already loaded")
}

fn is_maap_action_json(content: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content.trim()) else {
        return false;
    };
    value
        .as_object()
        .is_some_and(|object| object.contains_key("actions") || object.contains_key("action_batch"))
}

#[cfg(test)]
mod tests;
