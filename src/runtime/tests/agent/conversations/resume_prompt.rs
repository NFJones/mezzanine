//! Agent conversation resume prompt tests.

use super::*;

/// Verifies `/resume` completion includes saved conversation ids supplied by
/// the runtime transcript store.
#[test]
fn runtime_agent_prompt_resume_autocompletes_saved_session_uuid() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-complete"));
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "018f6b3a-1b2c-7000-9000-cafebabefeed".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-saved".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "saved prompt".to_string(),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/resume 018f".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\t".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(
        service
            .agent_prompt_inputs_for_tests()
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/resume 018f6b3a-1b2c-7000-9000-cafebabefeed "
    );
}

/// Verifies `/resume <session>` replays saved transcript context into the pane
/// buffer after rebinding the pane-local agent shell. A resumed task should
/// show enough prior conversation content for the user to continue without
/// opening a separate transcript file.
#[test]
fn runtime_agent_prompt_resume_displays_saved_transcript_context() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-display"));
    let conversation_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
    for (sequence, role, content) in [
        (1, mez_agent::transcript::TranscriptRole::User, "aGVsbG8K"),
        (
            2,
            mez_agent::transcript::TranscriptRole::Assistant,
            "I inspected the repo and started the change",
        ),
        (
            3,
            mez_agent::transcript::TranscriptRole::Tool,
            r#"action_id=action-1 action_type=say status=succeeded content: ignored structured_content: {"kind":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"Implemented the change"}"#,
        ),
    ] {
        transcript_store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: conversation_id.to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%9".to_string(),
                pane_id: "%9".to_string(),
                content: content.to_string(),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service
        .execute_agent_shell_command(&primary, &format!("/resume {conversation_id}"))
        .unwrap();

    assert!(response.contains("resumed=true"), "{response}");
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("Resumed Agent Session"), "{pane_text}");
    assert!(
        pane_text.contains(&format!("Conversation ID: {conversation_id}")),
        "{pane_text}"
    );
    assert!(pane_text.contains("Entries: 3"), "{pane_text}");
    assert!(pane_text.contains("Resumed:\n▐ yes"), "{pane_text}");
    assert!(pane_text.contains("user> hello"), "{pane_text}");
    assert!(
        pane_text.contains("mez> I inspected the repo and started the change"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("agent: Implemented the change"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("aGVsbG8K"), "{pane_text}");
    assert!(!pane_text.contains("structured_content"), "{pane_text}");
    assert!(!pane_text.contains("[1 turn=turn-1]"), "{pane_text}");
}
