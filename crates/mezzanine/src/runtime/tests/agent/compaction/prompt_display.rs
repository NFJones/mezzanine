//! Prompt display and compact file-list presentation.

use super::*;

/// Verifies large bracketed-paste agent prompt input is displayed compactly in
/// the pane transcript while the agent turn receives the exact pasted payload.
#[test]
fn runtime_agent_prompt_displays_large_paste_as_compact_block() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-paste-history"));
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );

    let payload = "z".repeat(1229);
    let mut input = Vec::new();
    input.extend_from_slice(b"prefix ");
    input.extend_from_slice(b"\x1b[200~");
    input.extend_from_slice(payload.as_bytes());
    input.extend_from_slice(b"\x1b[201~ suffix\r");
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ForwardToPane(input)],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs_for_tests().get("%1").unwrap();
    assert_eq!(
        prompt_state.prompt.buffer.history(),
        &[format!("prefix {payload} suffix")]
    );
    let persisted_history = transcript_store
        .structured_prompt_history("conversation-1")
        .unwrap();
    assert_eq!(persisted_history.len(), 1);
    assert_eq!(
        persisted_history[0].text,
        format!("prefix {payload} suffix")
    );
    assert_eq!(
        persisted_history[0].rendered(),
        "prefix [Pasted 1.2 KiB] suffix"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("user> prefix [Pasted 1.2 KiB] suffix"),
        "{pane_text}"
    );
    assert!(!pane_text.contains(&payload), "{pane_text}");
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content.contains(&format!("prefix {payload} suffix")))
    );
}

/// Verifies a mixed prompt keeps its typed prefix and suffix around one large
/// paste after durable history reload and Up-arrow recall.
///
/// Real terminals deliver typing and bracketed paste as separate input batches.
/// Reloading that submission in a later agent session must reconstruct only the
/// pasted payload as an opaque block; it must not hide or absorb adjacent text.
#[test]
fn runtime_agent_prompt_recalls_mixed_paste_display_after_reload() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-mixed-paste-recall"));
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );

    let payload = "z".repeat(70 * 1024);
    let expected = format!("typed before {payload} typed after");
    let pasted = format!("\x1b[200~{payload}\x1b[201~");
    for input in [
        b"typed before ".as_slice(),
        pasted.as_bytes(),
        b" typed after\r".as_slice(),
    ] {
        service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::ForwardToPane(input.to_vec())],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    }

    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[A".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    let prompt = &service
        .agent_prompt_inputs_for_tests()
        .get("%1")
        .unwrap()
        .prompt;
    assert_eq!(prompt.buffer.expanded_line(), expected);
    assert_eq!(
        prompt.buffer.rendered_line(),
        "typed before [Pasted 70.0 KiB] typed after"
    );
    assert_eq!(
        prompt.render(),
        "⟩ typed before [Pasted 70.0 KiB] typed after"
    );
}

/// Verifies compact pasted placeholders are used for bracketed paste payloads
/// that exceed the visible agent prompt height even when the byte size is small.
///
/// Agent prompt rendering only shows up to six input rows. A seven-line
/// bracketed paste must collapse to the same inline placeholder form as a
/// large byte paste so surrounding prompt text remains editable and readable.
#[test]
fn runtime_agent_prompt_displays_over_height_paste_as_compact_block() {
    let mut service = test_runtime_service_with_size(Size::new(50, 8).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(50, 8).unwrap(), 10).unwrap(),
    );

    let payload = (1..=7)
        .map(|index| format!("tiny-line-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut input = Vec::new();
    input.extend_from_slice(b"prefix ");
    input.extend_from_slice(b"\x1b[200~");
    input.extend_from_slice(payload.as_bytes());
    input.extend_from_slice(b"\x1b[201~ suffix\r");

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(input)],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("user> prefix [Pasted "), "{pane_text}");
    assert!(pane_text.contains(" suffix"), "{pane_text}");
    assert!(!pane_text.contains("tiny-line-7"), "{pane_text}");
    let context = service.agent_turn_contexts().get("turn-1").unwrap();
    assert!(
        context
            .blocks()
            .iter()
            .any(|block| block.content.contains(&format!("prefix {payload} suffix")))
    );
}

/// Verifies `/list-modified-files` renders compact modified-file rows.
///
/// Agent mutation previews already show `edited path (+N -M)` style summaries;
/// the slash command should expose the tracked aggregate in the same compact
/// form instead of a verbose nested object list.
#[test]
fn runtime_agent_shell_list_modified_files_reports_compact_rows() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.record_agent_modified_file_delta("%1", "src/lib.rs".to_string(), 12, 3);

    let response = service
        .execute_agent_shell_command(&primary, "/list-modified-files")
        .unwrap();
    assert!(
        response.contains(r#""body":null"#),
        "the deferred lane acknowledges /list-modified-files: {response}"
    );
    let response = service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap()
        .expect("the deferred /list-modified-files applies its page");

    assert!(response.contains("## modified files"), "{response}");
    assert!(response.contains("edited `src/lib.rs`"), "{response}");
    assert!(
        response.contains(r#"<span class=\"mez-diff-addition\">+12</span>"#),
        "{response}"
    );
    assert!(
        response.contains(r#"<span class=\"mez-diff-deletion\">-3</span>"#),
        "{response}"
    );
    assert!(!response.contains("Added:"), "{response}");
    assert!(!response.contains("Removed:"), "{response}");
    assert!(!response.contains("`summary`"), "{response}");
    assert_eq!(
        crate::runtime::commands::lists::runtime_agent_modified_files_body(None),
        "## modified files\n\nno modified files tracked for this agent conversation."
    );
    assert_eq!(
        crate::runtime::commands::lists::runtime_agent_modified_files_body(Some(
            &std::collections::BTreeMap::new()
        )),
        "## modified files\n\nno modified files tracked for this agent conversation.",
        "an empty tracked map renders the same empty state as no map at all"
    );
}
