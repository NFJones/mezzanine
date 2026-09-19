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

/// Verifies canonical user-event records retain initial and latest prompt
/// previews after chronology persistence replaces legacy user-role rows.
///
/// Saved-session listing reads these summaries without replaying the whole
/// model context, so typed prompt records must remain summary-compatible.
#[test]
fn conversation_summary_uses_typed_canonical_user_events() {
    let first = crate::TranscriptContextEvent::user_event(
        1,
        "user prompt",
        "inspect the original repository",
    )
    .unwrap();
    let latest = crate::TranscriptContextEvent::user_event(
        3,
        "user steering",
        "preserve the latest instruction",
    )
    .unwrap();
    let entries = vec![
        TranscriptEntry {
            conversation_id: "conversation-typed-user".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: TranscriptRole::System,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            pane_id: "pane-1".to_string(),
            content: first.to_transcript_content(),
        },
        TranscriptEntry {
            conversation_id: "conversation-typed-user".to_string(),
            sequence: 2,
            created_at_unix_seconds: 11,
            role: TranscriptRole::Assistant,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            pane_id: "pane-1".to_string(),
            content: "assistant work".to_string(),
        },
        TranscriptEntry {
            conversation_id: "conversation-typed-user".to_string(),
            sequence: 3,
            created_at_unix_seconds: 12,
            role: TranscriptRole::System,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            pane_id: "pane-1".to_string(),
            content: latest.to_transcript_content(),
        },
    ];

    let summary = summarize_conversation(entries).unwrap();

    assert_eq!(
        summary.initial_prompt.as_deref(),
        Some("inspect the original repository")
    );
    assert_eq!(
        summary.latest_user_prompt.as_deref(),
        Some("preserve the latest instruction")
    );
}

/// Verifies marker-shaped user-event payloads in assistant or tool rows cannot
/// become saved-session prompt metadata.
///
/// Replay accepts typed user events only from system transcript rows. Summary
/// extraction must enforce that same role contract so untrusted assistant or
/// tool content cannot affect picker previews or saved-session search.
#[test]
fn conversation_summary_ignores_typed_user_events_in_non_system_rows() {
    let typed = crate::TranscriptContextEvent::user_event(
        1,
        "user prompt",
        "must not become a summary prompt",
    )
    .unwrap()
    .to_transcript_content();
    let entries = [TranscriptRole::Assistant, TranscriptRole::Tool]
        .into_iter()
        .enumerate()
        .map(|(index, role)| TranscriptEntry {
            conversation_id: "conversation-role-confusion".to_string(),
            sequence: u64::try_from(index + 1).unwrap(),
            created_at_unix_seconds: 10,
            role,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            pane_id: "pane-1".to_string(),
            content: typed.clone(),
        })
        .collect();

    let summary = summarize_conversation(entries).unwrap();

    assert_eq!(summary.initial_prompt, None);
    assert_eq!(summary.latest_user_prompt, None);
}
