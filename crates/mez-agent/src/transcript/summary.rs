//! Conversation summary helpers.
//!
//! Summaries are derived from decoded entries rather than separate index files
//! so listing reflects the durable transcript contents.

use super::{TranscriptEntry, TranscriptRole};

/// Summary of one saved conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationSummary {
    /// Conversation identity.
    pub conversation_id: String,
    /// Number of entries in the conversation.
    pub entries: usize,
    /// Creation time of the first entry.
    pub first_created_at_unix_seconds: u64,
    /// Creation time of the last entry.
    pub last_created_at_unix_seconds: u64,
    /// Last turn id in the conversation.
    pub last_turn_id: String,
    /// Agent id from the last entry.
    pub agent_id: String,
    /// Pane id from the last entry.
    pub pane_id: String,
    /// Best-known project root or working directory for the conversation.
    pub directory: Option<String>,
    /// Bounded text from the first user-authored prompt in the conversation.
    pub initial_prompt: Option<String>,
    /// Bounded text from the most recent user-authored prompt in the conversation.
    pub latest_user_prompt: Option<String>,
}

/// Runs the summarize conversation operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn summarize_conversation(entries: Vec<TranscriptEntry>) -> Option<ConversationSummary> {
    let first = entries.first()?;
    let last = entries.last()?;
    let directory = conversation_directory(&entries);
    let initial_prompt = entries
        .iter()
        .find(|entry| entry.role == TranscriptRole::User)
        .map(|entry| bounded_summary_text(&entry.content, 120));
    let latest_user_prompt = entries
        .iter()
        .rev()
        .find(|entry| entry.role == TranscriptRole::User)
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
        latest_user_prompt,
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
