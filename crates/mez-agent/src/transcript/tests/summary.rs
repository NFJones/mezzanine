//! Conversation summary derivation tests.

use crate::transcript::{TranscriptEntry, TranscriptRole, summarize_conversation};

/// Verifies summaries choose project context and bounded user prompts.
///
/// Summary derivation is storage-independent and must remain stable when the
/// product changes sidecar or transcript file formats.
#[test]
fn conversation_summary_uses_project_root_and_user_prompt_bounds() {
    let entries = vec![
        TranscriptEntry {
            conversation_id: "conversation-1".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::System,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            pane_id: "pane-1".to_string(),
            content: "cwd=/tmp\nproject_root=/work/repo".to_string(),
        },
        TranscriptEntry {
            conversation_id: "conversation-1".to_string(),
            sequence: 2,
            created_at_unix_seconds: 11,
            role: TranscriptRole::User,
            turn_id: "turn-2".to_string(),
            agent_id: "agent-1".to_string(),
            pane_id: "pane-1".to_string(),
            content: "inspect the repository".to_string(),
        },
    ];

    let summary = summarize_conversation(entries).unwrap();

    assert_eq!(summary.directory.as_deref(), Some("/work/repo"));
    assert_eq!(
        summary.initial_prompt.as_deref(),
        Some("inspect the repository")
    );
    assert_eq!(summary.latest_user_prompt, summary.initial_prompt);
}
