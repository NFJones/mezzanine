//! Conversation summary helpers.
//!
//! Summaries are derived from decoded entries rather than separate index files
//! so listing reflects the durable transcript contents.

use super::types::{ConversationSummary, TranscriptEntry};

/// Runs the summarize conversation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn summarize_conversation(entries: Vec<TranscriptEntry>) -> Option<ConversationSummary> {
    let first = entries.first()?;
    let last = entries.last()?;
    let directory = conversation_directory(&entries);
    let initial_prompt = entries
        .iter()
        .find(|entry| entry.role == super::types::TranscriptRole::User)
        .map(|entry| bounded_summary_text(&entry.content, 120));
    Some(ConversationSummary {
        conversation_id: first.conversation_id.clone(),
        entries: entries.len(),
        first_created_at_unix_seconds: first.created_at_unix_seconds,
        last_created_at_unix_seconds: last.created_at_unix_seconds,
        last_turn_id: last.turn_id.clone(),
        agent_id: last.agent_id.clone(),
        pane_id: last.pane_id.clone(),
        directory,
        initial_prompt,
    })
}

/// Returns the best-known project root or working directory from transcript
/// context entries.
fn conversation_directory(entries: &[TranscriptEntry]) -> Option<String> {
    let mut cwd = None;
    for entry in entries {
        for line in entry.content.lines() {
            if let Some(value) = line.strip_prefix("project_root=")
                && !value.trim().is_empty()
            {
                return Some(value.trim().to_string());
            }
            if cwd.is_none()
                && let Some(value) = line
                    .strip_prefix("cwd=")
                    .or_else(|| line.strip_prefix("working_directory="))
                && !value.trim().is_empty()
            {
                cwd = Some(value.trim().to_string());
            }
        }
    }
    cwd
}

/// Bounds a prompt preview without splitting UTF-8 code points.
fn bounded_summary_text(text: &str, max_chars: usize) -> String {
    let mut preview = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if preview.chars().count() <= max_chars {
        return preview;
    }
    preview = preview.chars().take(max_chars.saturating_sub(1)).collect();
    preview.push('…');
    preview
}
