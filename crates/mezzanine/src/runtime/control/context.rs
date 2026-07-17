//! Runtime control model-context helpers.
//!
//! This module owns the transcript replay, compaction-refresh filtering, and
//! local message payload formatting helpers used by the runtime control adapter.
//! Keeping these routines behind a focused child-module boundary prevents the
//! control request dispatcher from also owning model-context shaping details.

use super::super::{ContextBlock, ContextSourceKind, Envelope, TranscriptEntry, TranscriptRole};
use mez_agent::ProviderTranscriptEvent;

const AGENT_LOCAL_MESSAGE_CONTEXT_PAYLOAD_CHARS: usize = 256 * 1024;
const AGENT_TRANSCRIPT_TOOL_CONTEXT_LIMIT_BYTES: usize = 256 * 1024;

/// Builds model-context blocks from durable transcript entries for one runtime pane.
pub(super) fn runtime_agent_transcript_context_blocks(
    pane_id: &str,
    entries: &[TranscriptEntry],
) -> Vec<ContextBlock> {
    let context_entries = entries
        .iter()
        .filter(|entry| {
            entry.role != TranscriptRole::System
                || ProviderTranscriptEvent::from_transcript_content(&entry.content).is_some()
        })
        .collect::<Vec<_>>();
    let mut blocks = Vec::new();
    for entry in context_entries {
        let Some(content) = runtime_transcript_entry_context_content(entry) else {
            continue;
        };
        blocks.push(ContextBlock {
            source: runtime_transcript_context_source_kind(entry.role),
            label: format!(
                "previous {} message for pane {pane_id}",
                runtime_context_transcript_role_name(entry.role)
            ),
            content,
        });
    }
    blocks
}

/// Returns true for context blocks owned by transcript replay or compact memory.
pub(super) fn runtime_context_block_is_compaction_refresh_owned(block: &ContextBlock) -> bool {
    match block.source {
        ContextSourceKind::Transcript
        | ContextSourceKind::TranscriptUser
        | ContextSourceKind::TranscriptTool => true,
        ContextSourceKind::TranscriptAssistant => block
            .label
            .starts_with("previous assistant message for pane "),
        ContextSourceKind::Memory => {
            block.label == "conversation compaction notice"
                || block.label.starts_with("memory compact-")
        }
        _ => false,
    }
}

/// Maps a stored transcript role to a model-context source that preserves the
/// role across request assembly.
fn runtime_transcript_context_source_kind(role: TranscriptRole) -> ContextSourceKind {
    match role {
        TranscriptRole::User => ContextSourceKind::TranscriptUser,
        TranscriptRole::Assistant => ContextSourceKind::TranscriptAssistant,
        TranscriptRole::Tool => ContextSourceKind::TranscriptTool,
        TranscriptRole::System => ContextSourceKind::Transcript,
    }
}

/// Returns model-facing transcript content after removing protocol scaffolding
/// that is useful for durable audit but harmful as future prompt context.
fn runtime_transcript_entry_context_content(entry: &TranscriptEntry) -> Option<String> {
    match entry.role {
        TranscriptRole::System
            if ProviderTranscriptEvent::from_transcript_content(&entry.content).is_some() =>
        {
            Some(entry.content.clone())
        }
        TranscriptRole::System => None,
        TranscriptRole::Tool => runtime_transcript_tool_context_content(&entry.content),
        TranscriptRole::User if transcript_content_looks_like_skill_context(&entry.content) => None,
        TranscriptRole::Assistant
            if transcript_content_looks_like_maap_action_json(&entry.content) =>
        {
            None
        }
        _ => Some(entry.content.clone()),
    }
}

/// Returns transcript tool output for model-facing replay.
///
/// Previous action results are often the user's freshest evidence, especially
/// failed file reads and shell observations. Historical replay should stay
/// byte-stable so later turns see the same durable tool context they already
/// observed.
fn runtime_transcript_tool_context_content(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    if transcript_tool_content_is_omitted_for_replay(trimmed) {
        return None;
    }
    Some(truncate_runtime_context_text(
        trimmed,
        AGENT_TRANSCRIPT_TOOL_CONTEXT_LIMIT_BYTES,
        "transcript tool context",
    ))
}

/// Returns whether one durable tool transcript payload should stay out of
/// later model context because it is metadata or workflow body rather than
/// execution evidence.
fn transcript_tool_content_is_omitted_for_replay(content: &str) -> bool {
    content.starts_with("[action_result ")
        && [" fetch_url ", " web_search "]
            .iter()
            .any(|needle| content.contains(needle))
        || content.contains("action_type=request_skills")
        || content.contains("action_type=call_skill")
}

/// Reports whether transcript text is an expanded skill body rather than the
/// user's original prompt.
fn transcript_content_looks_like_skill_context(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with("# Skill: ")
        && trimmed.contains("\nSource: ")
        && trimmed.contains("\nPath: ")
        && trimmed.contains("\nInvocation state: this skill is already loaded")
}

/// Reports whether transcript text is a raw MAAP action object rather than
/// conversational assistant content.
fn transcript_content_looks_like_maap_action_json(content: &str) -> bool {
    let trimmed = content.trim();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    object.contains_key("actions") || object.contains_key("action_batch")
}

/// Returns the display name used for a transcript role in replay labels.
fn runtime_context_transcript_role_name(role: TranscriptRole) -> &'static str {
    match role {
        TranscriptRole::User => "user",
        TranscriptRole::Assistant => "assistant",
        TranscriptRole::Tool => "tool",
        TranscriptRole::System => "system",
    }
}

/// Returns bounded context text without splitting UTF-8 characters.
fn truncate_runtime_context_text(content: &str, max_bytes: usize, label: &str) -> String {
    if content.len() <= max_bytes {
        return content.to_string();
    }
    let mut end = max_bytes;
    while !content.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!(
        "{}...[mez: {label} truncated; original_bytes={}]",
        &content[..end],
        content.len()
    )
}

/// Returns bounded local-message context including the message payload.
pub(super) fn runtime_local_message_context_content(envelope: &Envelope) -> String {
    let mut lines = vec![format!(
        "from={} id={} type={} content_type={} ttl_ms={}",
        envelope.sender.agent_id,
        envelope.id,
        envelope.message_type,
        envelope.content_type,
        envelope
            .ttl_ms
            .map_or("none".to_string(), |ms| ms.to_string())
    )];
    if let Some(correlation_id) = &envelope.correlation_id {
        lines.push(format!("correlation_id={correlation_id}"));
    }
    lines.push("payload:".to_string());
    lines.push(truncate_runtime_context_text(
        &envelope.payload,
        AGENT_LOCAL_MESSAGE_CONTEXT_PAYLOAD_CHARS,
        "local message payload",
    ));
    lines.join("\n")
}
