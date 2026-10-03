//! Durable prompt, command, rationale, and macro source replay parity.
//!
//! Persisted semantic source must reproduce the ordinary renderer at new
//! geometry while retaining bounded previews and original styles.

use super::*;

/// Verifies user prompts persist their raw source and recompute wrapping when
/// an agent pane is rebuilt at a narrower geometry.
#[test]
fn runtime_agent_user_prompt_persists_raw_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-user-prompt-source"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    service
        .append_agent_user_prompt_to_terminal_buffer("%1", "restore this durable user prompt")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(
        entries[0]
            .source_content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("user-prompt+text")),
        "{entries:?}"
    );

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed_compact = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        replayed_compact.contains("userrestorethisdurableuserprompt"),
        "{replayed_compact}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies command previews persist their raw command and recompute their
/// syntax-aware projection when an agent pane is rebuilt at a new geometry.
#[test]
fn runtime_agent_command_preview_persists_raw_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-command-preview-source"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    service
        .append_agent_command_preview_to_terminal_buffer("%1", "printf 'durable preview'")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(
        entries[0]
            .source_content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("command-preview+text")),
        "{entries:?}"
    );

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed_compact = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        replayed_compact.contains("printfdurablepreview"),
        "{replayed_compact}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies oversized command previews persist only their bounded UTF-8 source
/// projection and retain explicit truncation when replayed after a resize.
/// Presentation persistence must not turn a bounded renderer into durable
/// multi-megabyte storage or lose the omission marker at a new geometry.
#[test]
fn runtime_agent_command_preview_persists_bounded_truncated_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-command-preview-bounded"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let command = format!(
        "printf 'start {} tail-sentinel'",
        "x".repeat(2 * 1024 * 1024)
    );

    service
        .append_agent_command_preview_to_terminal_buffer("%1", &command)
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    let source = entries[0].source_text.as_deref().unwrap();
    assert!(source.len() <= 16 * 1024, "stored {} bytes", source.len());
    assert!(!source.contains("tail-sentinel"), "{source}");
    assert!(
        entries[0]
            .source_content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("command-preview-truncated+text")),
        "{entries:?}"
    );

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(replayed.contains("preview"), "{replayed}");
    assert!(replayed.contains("truncated"), "{replayed}");
    assert!(!replayed.contains("tail-sentinel"), "{replayed}");
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies action execution headers persist their semantic text and rebuild
/// through the action-header renderer at a narrower destination geometry.
#[test]
fn runtime_agent_action_header_persists_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-action-header-source"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let action = mez_agent::AgentAction {
        id: "mcp-1".to_string(),

        payload: mez_agent::AgentActionPayload::McpCall {
            server: "github".to_string(),
            tool: "search_issues".to_string(),
            arguments_json: r#"{"query":"durable header"}"#.to_string(),
        },
    };

    service
        .append_agent_action_execution_header_to_terminal_buffer(
            "%1",
            &action,
            "mcp call: github/search_issues args={\"query\":\"durable header\"}",
        )
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(
        entries[0]
            .source_content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("action-header+text")),
        "{entries:?}"
    );

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed_compact = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        replayed_compact.contains("mcpcallgithubsearchissuesargsquerydurableheader"),
        "{replayed_compact}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies parent prompts persist their raw instruction and recompute wrapping
/// when a child agent pane is rebuilt at a narrower destination geometry.
#[test]
fn runtime_agent_parent_prompt_persists_raw_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-parent-prompt-source"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    service
        .append_agent_parent_prompt_to_terminal_buffer("%1", "restore this parent instruction")
        .unwrap();
    let parent_marker_foreground = service.ui_theme().colors.agent_transcript_parent.foreground;
    let live_parent_marker = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("parent> "))
        .and_then(|line| {
            line.style_spans
                .iter()
                .find(|span| span.rendition.foreground == Some(parent_marker_foreground))
                .copied()
        })
        .expect("live parent line must carry a name-marker span");
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(
        entries[0]
            .source_content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("parent-prompt+text")),
        "{entries:?}"
    );

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed_compact = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        replayed_compact.contains("parentrestorethisparentinstruction"),
        "{replayed_compact}"
    );
    let replayed_parent_marker = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("parent> "))
        .and_then(|line| {
            line.style_spans
                .iter()
                .find(|span| span.rendition.foreground == Some(parent_marker_foreground))
                .copied()
        })
        .expect("replayed parent line must carry a name-marker span");
    assert_eq!(
        live_parent_marker, replayed_parent_marker,
        "a replayed parent prompt must keep the live name-marker span"
    );
    assert_eq!(replayed_parent_marker.start, "▐ ".chars().count());
    assert_eq!(replayed_parent_marker.length, "parent>".chars().count());
    assert!(
        replayed_parent_marker.rendition.background.is_none(),
        "name markers must never paint a background"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies thinking-log body text retains baseline dim status styling
/// instead of resetting to the terminal's default rendition.
///
/// Thinking lines use the rich-line presentation path without explicit body
/// spans, so this regression protects the base style inherited by unspanned
/// cells after the gutter has been rendered.
#[test]
fn runtime_agent_thinking_renders_body_as_shadow_text() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(80, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Debug)
        .unwrap();

    service
        .append_agent_thinking_text_to_terminal_buffer("%1", "inspect the rendering path")
        .unwrap();

    let thinking_line = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("thinking: inspect the rendering path"))
        .expect("thinking log should be present in the terminal buffer");
    let body_column = thinking_line
        .text
        .find("thinking:")
        .expect("thinking log should include its label");
    assert!(
        thinking_line.style_spans.iter().any(|span| {
            body_column >= span.start
                && body_column < span.start.saturating_add(span.length)
                && span.rendition.dim
                && !span.rendition.bold
                && span.rendition.foreground
                    == Some(service.ui_theme().colors.agent_transcript_status.foreground)
        }),
        "thinking body should retain baseline shadow status text: {thinking_line:?}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies visible thinking text persists its raw source and reflows when the
/// agent pane is rebuilt at a narrower destination geometry.
#[test]
fn runtime_agent_thinking_persists_raw_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-thinking-source"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", AgentLogLevel::Debug)
        .unwrap();

    service
        .append_agent_thinking_text_to_terminal_buffer(
            "%1",
            "preserve this durable rationale across the reconstructed pane",
        )
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(
        entries[0]
            .source_content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("thinking+text")),
        "{entries:?}"
    );

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed_compact = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        replayed_compact.contains("thinkingpreservethisdurablerationaleacrossthereconstructedpane"),
        "{replayed_compact}"
    );
    service.terminate_all_pane_processes().unwrap();
}

/// Verifies structured macro lifecycle status persists its fields and rebuilds
/// through the macro renderer at a narrower destination geometry.
#[test]
fn runtime_agent_macro_lifecycle_persists_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-macro-lifecycle-source"));
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    service
        .append_agent_macro_status_to_terminal_buffer(
            "%1",
            "durable macro",
            Some(1),
            3,
            "waiting for child result",
        )
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let entries = transcript_store
        .inspect_presentation(&conversation_id)
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert!(
        entries[0]
            .source_content_type
            .as_deref()
            .is_some_and(|content_type| content_type.contains("macro-lifecycle+json")),
        "{entries:?}"
    );

    set_agent_pane_screen_for_test(
        &mut service,
        "%1",
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed_compact = service
        .agent_pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        replayed_compact.contains("macrodurablemacro"),
        "{replayed_compact}"
    );
    service.terminate_all_pane_processes().unwrap();
}
