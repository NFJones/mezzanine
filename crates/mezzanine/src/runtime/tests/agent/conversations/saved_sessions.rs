//! Agent conversation saved sessions tests.

use super::*;

/// Verifies that saved agent conversations can be listed, resumed into the
/// current pane, exposed to prompt context, and forked while keeping readline
/// prompt history available through the shared prompt-history file.
#[test]
fn runtime_agent_shell_resume_and_fork_manage_saved_conversations() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-fork"));
    let cwd = temp_root("runtime-agent-resume-cwd");
    fs::create_dir_all(&cwd).unwrap();
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "saved".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::System,
            turn_id: "turn-old".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: format!("cwd={}", cwd.display()),
        })
        .unwrap();
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "saved".to_string(),
            sequence: 2,
            created_at_unix_seconds: 1,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-old".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "saved prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "saved".to_string(),
            sequence: 3,
            created_at_unix_seconds: 2,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-new".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "latest saved prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append_prompt_history("saved", "find files")
        .unwrap();
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "latest".to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-latest".to_string(),
            agent_id: "agent-%8".to_string(),
            pane_id: "%8".to_string(),
            content: "latest prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append_presentation(&crate::storage::transcript::AgentPresentationEntry {
            conversation_id: "saved".to_string(),
            sequence: 1,
            created_at_unix_seconds: 3,
            pane_id: "%9".to_string(),
            turn_id: Some("turn-old".to_string()),
            terminal_width: 80,
            style_names: vec!["assistant".to_string(), "status".to_string()],
            display_lines: vec![
                "mez> rendered saved response".to_string(),
                "agent: rendered saved status".to_string(),
            ],
            copy_lines: vec![
                "mez> copy saved response".to_string(),
                "agent: copy saved status".to_string(),
            ],
            ansi_text: Some(
                "\r▐ mez> rendered saved response\r\n▐ agent: rendered saved status\r\n▐ ansi-only replay marker\r\n"
                    .to_string(),
            ),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.set_pane_screen(
        "%1".to_string(),
        TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap(),
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let picker = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-list","method":"agent/shell/command","params":{"idempotency_key":"resume-list","input":"/resume"}}"#,
        &primary,
    );
    assert!(picker.contains("mez-agent:/resume%20saved"), "{picker}");
    assert!(picker.contains("mez-agent:/resume%20latest"), "{picker}");
    let saved_section = picker
        .split("\n\n")
        .find(|section| section.contains("mez-agent:/resume%20saved"))
        .expect("saved session section should exist");
    assert!(saved_section.contains("  - Prompt: latest s"), "{picker}");
    assert!(!saved_section.contains("  - Prompt: saved p"), "{picker}");

    let latest = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume-latest","method":"agent/shell/command","params":{"idempotency_key":"resume-latest","input":"/resume --latest"}}"#,
        &primary,
    );
    assert!(latest.contains("conversation_id=latest"), "{latest}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some("latest")
    );

    let resumed = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"resume","method":"agent/shell/command","params":{"idempotency_key":"resume","input":"/resume saved"}}"#,
        &primary,
    );
    assert!(resumed.contains("conversation_id=saved"), "{resumed}");
    assert_eq!(
        service.pane_current_working_directory("%1").as_deref(),
        Some(cwd.as_path())
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some("saved")
    );
    let resumed_pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        resumed_pane_text.contains("rendered sa") && resumed_pane_text.contains("response"),
        "{resumed_pane_text}"
    );
    assert!(
        resumed_pane_text.contains("agent: rendered sa")
            && resumed_pane_text.contains("ved status"),
        "{resumed_pane_text}"
    );
    assert!(
        resumed_pane_text.contains("ansi-only") && resumed_pane_text.contains("arker"),
        "{resumed_pane_text}"
    );
    assert!(
        !resumed_pane_text.contains("Resumed Agent Session"),
        "{resumed_pane_text}"
    );
    assert_eq!(
        service
            .agent_prompt_inputs_for_tests()
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .history(),
        &[
            String::from("find files"),
            String::from("/resume"),
            String::from("/resume --latest"),
            String::from("/resume saved")
        ]
    );
    let context = service
        .agent_context_for_pane_prompt("%1", "continue", 0)
        .unwrap();
    assert!(context.blocks.iter().any(|block| {
        block.source == mez_agent::ContextSourceKind::TranscriptUser
            && block.content.contains("saved prompt")
    }));
    context.validate_placement_order().unwrap();
    let (_, profile) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    let turn = mez_agent::AgentTurnRecord {
        turn_id: "saved-context-validation".to_string(),
        agent_id: "agent-%1".to_string(),
        pane_id: "%1".to_string(),
        trigger: mez_agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 3,
        policy_profile: "runtime".to_string(),
        model_profile: "test".to_string(),
        parent_turn_id: None,
        state: AgentTurnState::Running,
        cooperation_mode: None,
        initial_capability: None,
    };
    let request =
        crate::integrations::agent::context::assemble_model_request(&profile, &turn, &context)
            .unwrap();
    let replayed_user_messages = request
        .messages
        .iter()
        .filter(|message| message.source == ContextSourceKind::TranscriptUser)
        .map(|message| (message.role, message.content.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(replayed_user_messages.len(), 2);
    assert_eq!(
        replayed_user_messages[0].0,
        mez_agent::ModelMessageRole::User
    );
    assert!(replayed_user_messages[0].1.contains("saved prompt"));
    assert!(replayed_user_messages[1].1.contains("latest saved prompt"));

    let forked = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"fork","method":"agent/shell/command","params":{"idempotency_key":"fork","input":"/fork saved-fork"}}"#,
        &primary,
    );
    assert!(forked.contains("source=saved"), "{forked}");
    assert!(forked.contains("conversation_id=saved-fork"), "{forked}");
    assert!(forked.contains("source_pane=%1"), "{forked}");
    assert_eq!(transcript_store.inspect("saved-fork").unwrap().len(), 3);
    assert_eq!(
        transcript_store.inspect_presentation("saved-fork").unwrap()[0].display_lines[0],
        "mez> rendered saved response"
    );
    let forked_pane = service
        .agent_shell_store()
        .sessions()
        .find(|session| session.session_id == "saved-fork")
        .map(|session| session.pane_id.clone())
        .expect("forked conversation should be bound to a pane");
    assert_ne!(forked_pane, "%1");
    assert_eq!(
        transcript_store.prompt_history("saved-fork").unwrap(),
        vec![
            String::from("find files"),
            String::from("/resume"),
            String::from("/resume --latest"),
            String::from("/resume saved"),
            String::from("/fork saved-fork")
        ]
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.session_id.as_str()),
        Some("saved")
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get(&forked_pane)
            .map(|session| session.session_id.as_str()),
        Some("saved-fork")
    );
    assert_eq!(
        service
            .agent_prompt_inputs_for_tests()
            .get(&forked_pane)
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/resume saved"
    );
    service.terminate_all_pane_processes().unwrap();
    let _ = fs::remove_dir_all(cwd);
}

/// Verifies the live `/resume` picker view starts selected-link styling on the
/// first visible session-id cell rather than the preceding list separator.
///
/// Helper-level overlay span tests can still miss attached-client regressions
/// if the visible picker row shifts styling after command submission. This
/// regression opens the real `/resume` picker through the agent-shell prompt
/// and inspects the rendered client-view row the user actually sees.
#[test]
fn runtime_resume_picker_view_keeps_selected_link_styling_off_previous_cell() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-picker-view"));
    let session_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: session_id.to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-saved".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "saved prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "latest".to_string(),
            sequence: 1,
            created_at_unix_seconds: 11,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-latest".to_string(),
            agent_id: "agent-%8".to_string(),
            pane_id: "%8".to_string(),
            content: "latest prompt".to_string(),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let visibility = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    let show = if visibility.contains("visibility=visible") {
        visibility
    } else {
        assert!(visibility.contains("visibility=hidden"), "{visibility}");
        service
            .execute_terminal_command(&primary, "agent-shell")
            .unwrap()
    };
    assert!(show.contains("visibility=visible"), "{show}");
    let _ = service.drain_pane_io_transition().side_effects;

    let submitted = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"/resume\r".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(submitted.forwarded_bytes, 0);
    assert!(submitted.view_refresh_required);
    assert!(service.primary_display_overlay().is_some());

    let moved = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[B".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(moved.forwarded_bytes, 0);
    assert!(moved.view_refresh_required);
    assert_eq!(
        service
            .primary_display_overlay()
            .and_then(|overlay| overlay.active_selection_index),
        Some(1)
    );

    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(120, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let row = view
        .lines
        .iter()
        .position(|line| line.contains(session_id))
        .expect("resume picker should render the saved session id");
    let line = &view.lines[row];
    let start = display_column_for_fragment(line, session_id);
    let previous_rendition = styled_line_rendition_at(
        &TerminalStyledLine {
            text: line.clone(),
            style_spans: view.line_style_spans[row].clone(),
            copy_text: None,
        },
        start.saturating_sub(1),
    );
    let first_rendition = styled_line_rendition_at(
        &TerminalStyledLine {
            text: line.clone(),
            style_spans: view.line_style_spans[row].clone(),
            copy_text: None,
        },
        start,
    );

    assert_ne!(
        previous_rendition.foreground,
        Some(
            service
                .ui_theme()
                .colors
                .agent_transcript_command
                .foreground
        ),
        "resume picker link foreground shifted left in live view: {view:?}"
    );
    assert!(
        !previous_rendition.underline,
        "resume picker underline shifted left in live view: {view:?}"
    );
    assert_ne!(
        previous_rendition.background,
        Some(service.ui_theme().colors.agent_model.background),
        "resume picker active background shifted left in live view: {view:?}"
    );
    assert_eq!(
        first_rendition.foreground,
        Some(
            service
                .ui_theme()
                .colors
                .agent_transcript_command
                .foreground
        ),
        "resume picker first session-id cell lost link foreground: {view:?}"
    );
    assert!(
        first_rendition.underline,
        "resume picker first session-id cell lost underline: {view:?}"
    );
    assert_eq!(
        first_rendition.background,
        Some(service.ui_theme().colors.agent_model.background),
        "resume picker first session-id cell lost active background: {view:?}"
    );
}

/// Verifies the full attached-terminal presentation path preserves the
/// selected-link boundary on the live `/resume` picker row.
///
/// The picker's rendered client view is only half the path shown to the user.
/// The attached client converts that view into presentation rows and row-diff
/// frames before a terminal screen applies the result. This regression covers
/// that full round trip using the real previous/current picker views so a
/// one-cell-left shift in the attached output path cannot hide behind helper
///-level overlay tests.
#[test]
fn runtime_resume_picker_attached_frame_keeps_selected_link_styling_off_previous_cell() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-resume-picker-frame"));
    let session_id = "018f6b3a-1b2c-7000-9000-cafebabefeed";
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: session_id.to_string(),
            sequence: 1,
            created_at_unix_seconds: 10,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-saved".to_string(),
            agent_id: "agent-%9".to_string(),
            pane_id: "%9".to_string(),
            content: "saved prompt".to_string(),
        })
        .unwrap();
    transcript_store
        .append(&mez_agent::transcript::TranscriptEntry {
            conversation_id: "latest".to_string(),
            sequence: 1,
            created_at_unix_seconds: 11,
            role: mez_agent::transcript::TranscriptRole::User,
            turn_id: "turn-latest".to_string(),
            agent_id: "agent-%8".to_string(),
            pane_id: "%8".to_string(),
            content: "latest prompt".to_string(),
        })
        .unwrap();
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(120, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let visibility = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    let show = if visibility.contains("visibility=visible") {
        visibility
    } else {
        assert!(visibility.contains("visibility=hidden"), "{visibility}");
        service
            .execute_terminal_command(&primary, "agent-shell")
            .unwrap()
    };
    assert!(show.contains("visibility=visible"), "{show}");
    let _ = service.drain_pane_io_transition().side_effects;

    let submitted = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"/resume\r".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(submitted.forwarded_bytes, 0);
    assert!(submitted.view_refresh_required);
    let previous_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(120, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();

    let moved = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b[B".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(moved.forwarded_bytes, 0);
    assert!(moved.view_refresh_required);
    let current_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(120, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();

    let modes = mez_mux::presentation::AttachedTerminalOutputModes {
        cursor_visible: current_view.cursor_visible,
        cursor_blink: current_view.cursor_blink,
        cursor_blink_interval_ms: current_view.cursor_blink_interval_ms,
        cursor_row: current_view.cursor_row,
        cursor_column: current_view.cursor_column,
        application_keypad: current_view.application_keypad,
        bracketed_paste: current_view.bracketed_paste,
        host_mouse_reporting: current_view.host_mouse_reporting,
        ..mez_mux::presentation::AttachedTerminalOutputModes::default()
    };
    let (previous_lines, previous_spans) =
        mez_mux::presentation::compose_client_presentation_with_styles(&previous_view, None);
    let (current_lines, current_spans) =
        mez_mux::presentation::compose_client_presentation_with_styles(&current_view, None);
    let previous_frame =
        mez_mux::attached_client::encode_attached_terminal_output_update_frame_with_styles(
            &previous_lines,
            &previous_spans,
            None,
            modes,
            None,
        );
    let previous_state = mez_mux::attached_client::AttachedTerminalOutputFrameState::new_with_modes(
        &previous_lines,
        &previous_spans,
        modes,
    );
    let update_frame =
        mez_mux::attached_client::encode_attached_terminal_output_update_frame_with_styles(
            &current_lines,
            &current_spans,
            None,
            modes,
            Some(&previous_state),
        );
    let mut screen = TerminalScreen::new(Size::new(120, 24).unwrap(), 10).unwrap();
    screen.feed(&previous_frame);
    screen.feed(&update_frame);

    let styled_lines = screen.visible_styled_lines();
    let row = styled_lines
        .iter()
        .find(|line| line.text.contains(session_id))
        .unwrap();
    let start = display_column_for_fragment(&row.text, session_id);
    let previous_rendition = styled_line_rendition_at(row, start.saturating_sub(1));
    let first_rendition = styled_line_rendition_at(row, start);

    assert_ne!(
        previous_rendition.foreground,
        Some(
            service
                .ui_theme()
                .colors
                .agent_transcript_command
                .foreground
        ),
        "resume picker link foreground shifted left after attached frame update: {styled_lines:?}"
    );
    assert!(
        !previous_rendition.underline,
        "resume picker underline shifted left after attached frame update: {styled_lines:?}"
    );
    assert_ne!(
        previous_rendition.background,
        Some(service.ui_theme().colors.agent_model.background),
        "resume picker active background shifted left after attached frame update: {styled_lines:?}"
    );
    assert_eq!(
        first_rendition.foreground,
        Some(
            service
                .ui_theme()
                .colors
                .agent_transcript_command
                .foreground
        ),
        "resume picker first session-id cell lost link foreground after attached frame update: {styled_lines:?}"
    );
    assert!(
        first_rendition.underline,
        "resume picker first session-id cell lost underline after attached frame update: {styled_lines:?}"
    );
    assert_eq!(
        first_rendition.background,
        Some(service.ui_theme().colors.agent_model.background),
        "resume picker first session-id cell lost active background after attached frame update: {styled_lines:?}"
    );
}
