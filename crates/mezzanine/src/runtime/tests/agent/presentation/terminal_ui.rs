//! Runtime tests for agent presentation terminal ui behavior.

use super::*;

/// Verifies that terminal cursor presentation settings are parsed from runtime
/// configuration layers and applied to attached-terminal render configuration.
#[test]
fn runtime_applies_cursor_presentation_options_from_config_layers() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\ncursor_style = \"bar\"\ncursor_blink = false\ncursor_blink_interval_ms = 250\nresize_debounce_ms = 125\nrender_rate_limit_fps = 8\nreduced_motion = true\n"
                .to_string(),
        }])
        .unwrap();

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();

    assert_eq!(
        config.cursor_style,
        mez_mux::presentation::TerminalCursorStyle::Bar
    );
    assert!(!config.cursor_blink);
    assert_eq!(config.cursor_blink_interval_ms, 250);
    assert_eq!(config.resize_debounce_ms, 125);
    assert_eq!(config.render_rate_limit_fps, 8);
    assert!(config.frame_context.reduced_motion);
    assert_eq!(config.frame_context.animation_tick_ms, 0);
}

/// Verifies that pane split actions which cannot fit inside the active window
/// become transient status-line errors instead of escaping as runtime errors.
/// The failing action must be consumed with no partial pane/process side
/// effects, and the next action while the error is visible must only dismiss
/// the presentational error instead of replaying the same split request.
#[test]
fn runtime_attached_split_error_is_presentational_and_not_replayed_on_dismiss() {
    let mut service = test_runtime_service_with_size(Size::new(3, 8).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(3, 8).unwrap(), 120)
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::SplitPaneVertical,
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.mux_actions_applied, 0);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert_eq!(service.session().windows()[0].panes().len(), 1);
    assert!(service.pane_processes().is_empty());
    assert!(
        service
            .primary_error_status_overlay()
            .is_some_and(|message| message.contains("cannot split vertically")),
        "{:?}",
        service.primary_error_status_overlay()
    );

    let dismiss = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(dismiss.mux_actions_applied, 0);
    assert!(dismiss.view_refresh_required);
    assert!(dismiss.full_redraw_required);
    assert_eq!(service.session().windows()[0].panes().len(), 1);
    assert!(service.pane_processes().is_empty());
    assert!(service.primary_error_status_overlay().is_none());

    let retried = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(retried.mux_actions_applied, 0);
    assert!(service.primary_error_status_overlay().is_some());
    assert_eq!(service.session().windows()[0].panes().len(), 1);
    assert!(service.pane_processes().is_empty());
}

/// Verifies plain `mez>` output wraps under the assistant indicator.
///
/// Markdown output already has element-aware continuation indentation. Plain
/// assistant text should use the same transcript geometry instead of relying
/// on terminal soft wrapping, whose continuation starts too far left.
#[test]
fn runtime_agent_plain_say_wraps_under_agent_indicator() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(28, 12).unwrap(), 120)
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(28, 12).unwrap(), 120).unwrap(),
    );

    service
        .append_agent_assistant_content_to_terminal_buffer(
            "%1",
            "alpha beta gamma delta epsilon",
            mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE,
        )
        .unwrap();

    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("▐ mez> alpha beta gamma"), "{pane_text}");
    assert!(pane_text.contains("▐      delta epsilon"), "{pane_text}");
}

/// Verifies user-visible status rows persist typed source and replay through
/// their original presentation style after a geometry-aware rebuild.
#[test]
fn runtime_agent_status_presentation_persists_typed_source_for_replay() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-status-source"));
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
        .append_agent_status_text_to_terminal_buffer("%1", "agent: restoring durable status")
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
            .is_some_and(|content_type| content_type.contains("styled-lines+json")),
        "{entries:?}"
    );

    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    let replayed_compact = replayed
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(
        replayed_compact.contains("agentrestoringdurablestatus"),
        "{replayed}"
    );
    service.terminate_all_pane_processes().unwrap();
}

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

    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed_compact = service
        .pane_screen("%1")
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

    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed_compact = service
        .pane_screen("%1")
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
        rationale: String::new(),
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

    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(20, 12).unwrap(), 120).unwrap(),
    );
    assert!(
        service
            .rebuild_agent_presentation_after_resize("%1", Size::new(20, 12).unwrap())
            .unwrap()
    );
    let replayed_compact = service
        .pane_screen("%1")
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

/// Verifies a live width change rebuilds a source-backed agent screen instead
/// of reflowing its stale cached terminal rows. This keeps Markdown rendering
/// semantic across pane geometry changes while preserving legacy resize
/// behavior for panes that do not retain presentation source.
#[test]
fn runtime_agent_resize_rebuilds_source_backed_presentation_at_new_width() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("agent-resize-source"));
    let primary = service
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
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id,
            sequence: 1,
            created_at_unix_seconds: 1,
            pane_id: "%1".to_string(),
            turn_id: None,
            terminal_width: 28,
            style_names: vec!["assistant".to_string()],
            display_lines: vec!["mez> stale cached projection".to_string()],
            copy_lines: vec!["stale cached projection".to_string()],
            ansi_text: None,
            source_text: Some(
                "# Rebuilt heading\n\n- source layout changes with width".to_string(),
            ),
            source_content_type: Some("text/markdown; charset=utf-8".to_string()),
        })
        .unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(28, 12).unwrap(), 120).unwrap(),
    );

    service
        .resize_attached_primary_terminal(&primary, Size::new(20, 12).unwrap())
        .unwrap();

    let rebuilt = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n")
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<String>();
    assert!(rebuilt.contains("Rebuiltheading"), "{rebuilt}");
    assert!(
        rebuilt.contains("sourcelayoutchangeswithwidth"),
        "{rebuilt}"
    );
    assert!(!rebuilt.contains("stalecachedprojection"), "{rebuilt}");
    assert_eq!(
        transcript_store
            .inspect_presentation(
                service
                    .agent_shell_store()
                    .get("%1")
                    .unwrap()
                    .session_id
                    .as_str()
            )
            .unwrap()
            .len(),
        1
    );
    service.terminate_all_pane_processes().unwrap();
}
