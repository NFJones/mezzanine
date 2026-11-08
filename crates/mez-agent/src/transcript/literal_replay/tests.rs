//! Literal history projection contracts needed by both replay and raw revision.

use super::*;

fn row(role: TranscriptRole, content: &str) -> TranscriptEntry {
    TranscriptEntry {
        conversation_id: "conversation".into(),
        sequence: 1,
        created_at_unix_seconds: 1,
        role,
        turn_id: "turn".into(),
        agent_id: "agent".into(),
        pane_id: "pane".into(),
        content: content.into(),
    }
}

/// Plain conversation bytes and their roles survive without receiving a
/// compactor-summary limit or interpreting user action-shaped text as commands.
#[test]
fn literal_projection_preserves_user_and_assistant_bytes_and_roles() {
    for (role, source) in [
        (TranscriptRole::User, ContextSourceKind::TranscriptUser),
        (
            TranscriptRole::Assistant,
            ContextSourceKind::TranscriptAssistant,
        ),
    ] {
        let text = format!("literal 界\n{}", "界".repeat(128 * 1024 / 3 + 1));
        let block =
            transcript_literal_context_block(&row(role, &text), "original label".into()).unwrap();
        assert_eq!(block.source, source);
        assert_eq!(block.label, "original label");
        assert_eq!(block.placement, ContextPlacement::ConversationAppend);
        assert_eq!(block.content, text);
    }
    let user = row(TranscriptRole::User, r#"{"actions":["literal user data"]}"#);
    assert_eq!(
        transcript_literal_context_block(&user, "user".into())
            .unwrap()
            .content,
        user.content
    );
    let assistant = row(TranscriptRole::Assistant, r#"{"action_batch":{}}"#);
    assert_eq!(
        transcript_literal_context_block(&assistant, "assistant".into())
            .unwrap()
            .content,
        "[legacy MAAP assistant execution omitted from transcript replay]"
    );
}

/// Legacy tool bodies cannot become disclosure channels; metadata-only rows,
/// expanded skills, raw unknown MCP bodies and empty rows stay omitted.
#[test]
fn literal_projection_omits_private_tool_bodies_and_non_model_rows() {
    let block = transcript_literal_context_block(
        &row(
            TranscriptRole::Tool,
            "[action_result shell-1 shell_command succeeded]\noutput:\nshell-secret",
        ),
        "tool".into(),
    )
    .unwrap();
    assert_eq!(block.source, ContextSourceKind::ActionResult);
    assert!(block.content.contains("historical_output: omitted"));
    assert!(!block.content.contains("shell-secret"));
    for entry in [
        row(TranscriptRole::Tool, "mcp-secret"),
        row(
            TranscriptRole::Tool,
            "[action_result web-1 web_search succeeded]\nbody",
        ),
        row(TranscriptRole::Tool, "action_type=request_skills"),
        row(
            TranscriptRole::User,
            "# Skill: example\nSource: original\nPath: /fixture\nInvocation state: this skill is already loaded",
        ),
        row(TranscriptRole::System, "ordinary audit metadata"),
        row(TranscriptRole::Assistant, " \n"),
    ] {
        assert!(transcript_literal_context_block(&entry, "omitted".into()).is_none());
    }
}

/// The existing tool limit retains valid UTF-8 and exactly describes the
/// original byte count, including when the boundary crosses a code point.
#[test]
fn literal_tool_truncation_preserves_utf8_boundary_and_original_size() {
    let text = "界".repeat(TOOL_CONTEXT_LIMIT_BYTES / 3 + 2);
    let actual = truncate_tool_content(&text);
    let prefix = "界".repeat(TOOL_CONTEXT_LIMIT_BYTES / 3);
    assert_eq!(
        actual,
        format!(
            "{prefix}...[mez: transcript tool context truncated; original_bytes={}]",
            text.len()
        )
    );
    assert_eq!(truncate_tool_content("short 界"), "short 界");
}
