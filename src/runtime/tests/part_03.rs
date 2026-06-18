/// Verifies that the primary command prompt retains submitted commands across
/// prompt openings and exposes them through the same readline Up/Down and
/// Ctrl+R reverse-search behavior used by agent prompts. The command history
/// must remain prompt-local runtime state rather than being forwarded to the
/// pane shell.
#[test]
fn runtime_primary_command_prompt_uses_readline_history_and_reverse_search() {
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-primary-command-history-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&transcript_root);
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();

    service.enter_primary_command_prompt("").unwrap();
    let first = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"help\r".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(first.forwarded_bytes, 0);
    assert!(service.primary_prompt_input.is_none());
    service.clear_primary_display_overlay();

    service.enter_primary_command_prompt("").unwrap();
    let second = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"list-buffers\r".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(second.forwarded_bytes, 0);
    assert_eq!(
        service.primary_command_prompt_history,
        vec![String::from("help"), String::from("list-buffers")]
    );
    assert_eq!(
        transcript_store.command_prompt_history().unwrap(),
        vec![String::from("help"), String::from("list-buffers")]
    );
    assert!(transcript_store.command_prompt_history_file().exists());
    service.clear_primary_display_overlay();
    service
        .primary_command_prompt_history
        .push("show list-buffers".to_string());

    service.enter_primary_command_prompt("li").unwrap();
    let search = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x12".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(search.forwarded_bytes, 0);
    let prompt = service.primary_prompt_input.as_ref().unwrap();
    assert_eq!(prompt.prompt.buffer.line(), "show list-buffers");

    let restore_draft = service
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
    assert_eq!(restore_draft.forwarded_bytes, 0);
    let prompt = service.primary_prompt_input.as_ref().unwrap();
    assert_eq!(prompt.prompt.buffer.line(), "li");
    let _ = fs::remove_dir_all(transcript_root);
}

/// Verifies MCP server ids complete in primary `:` commands that address
/// existing MCP configuration. These ids come from the live runtime registry,
/// not the static command table.
#[test]
fn runtime_primary_command_prompt_mcp_status_autocompletes_configured_server_id() {
    let mut service = test_runtime_service();
    service
        .mcp_registry_mut()
        .add_server(crate::mcp::McpServerConfig::stdio(
            "fixture",
            "Fixture MCP",
            "mcp-fixture",
            Vec::new(),
        ))
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service.enter_primary_command_prompt("").unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"mcp-status fi".to_vec()),
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
            .primary_prompt_input
            .as_ref()
            .unwrap()
            .prompt
            .buffer
            .line(),
        "mcp-status fixture "
    );
}

/// Verifies standalone Escape cancels primary command reverse search without
/// closing the prompt itself.
///
/// Reverse search is an in-prompt editing mode. Escape must restore the draft
/// that was present before search started, while a later standalone Escape from
/// normal prompt mode can still close the prompt.
#[test]
fn runtime_primary_command_prompt_escape_cancels_reverse_search() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service.primary_command_prompt_history =
        vec!["list-buffers".to_string(), "show list-buffers".to_string()];

    service.enter_primary_command_prompt("li").unwrap();
    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x12".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(
        service
            .primary_prompt_input
            .as_ref()
            .unwrap()
            .prompt
            .reverse_search_active()
    );

    let escape = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(escape.forwarded_bytes, 0);
    let prompt = service.primary_prompt_input.as_ref().unwrap();
    assert!(!prompt.prompt.reverse_search_active());
    assert_eq!(prompt.prompt.buffer.line(), "li");
}

/// Verifies that the terminal command prompt accepts encoded Ctrl+R from
/// terminals that use CSI-u for modified printable keys.
///
/// The low-level readline decoder already handles the legacy ASCII control
/// byte. This runtime-level regression keeps the active prompt path wired so
/// command history search works with terminal encodings commonly emitted by
/// modern emulators.
#[test]
fn runtime_primary_command_prompt_accepts_encoded_ctrl_r_history_search() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service.primary_command_prompt_history =
        vec!["list-buffers".to_string(), "show list-buffers".to_string()];

    service.enter_primary_command_prompt("li").unwrap();
    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"\x1b[114;5u".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    let prompt = service.primary_prompt_input.as_ref().unwrap();
    assert_eq!(prompt.prompt.buffer.line(), "show list-buffers");
}

/// Verifies that recoverable error overlays render as transient status-bar
/// notices. The next input should clear the notice as presentational state
/// without being forwarded or replayed, so repeating an error-causing action
/// does not immediately trigger the same error while dismissing the overlay.
#[test]
fn runtime_primary_error_overlay_dismisses_on_any_input() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(40, 6).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .show_primary_error_overlay(vec!["error: simulated".to_string()])
        .unwrap();

    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(40, 6).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        view.lines
            .last()
            .is_some_and(|line| line.contains("simulated")),
        "{:?}",
        view.lines
    );
    assert!(view.cursor_visible);

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"x".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert!(service.primary_error_status_overlay.is_none());
    assert!(service.primary_display_overlay.is_none());
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that destructive default prefix bindings open command prompts with
/// the explicit force flag required by live target shutdown semantics. The user
/// still has to submit the prompt, but the generated command no longer fails
/// the confirmation gate for live pane and window targets.
#[test]
fn runtime_destructive_prefix_prompts_include_explicit_force() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![
            TerminalClientLoopAction::ExecuteMux(MuxAction::KillWindowAfterConfirmation),
            TerminalClientLoopAction::ExecuteMux(MuxAction::KillPaneAfterConfirmation),
        ],
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
    assert!(!report.full_redraw_required);
    assert!(report.unsupported_actions.is_empty());
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        view.lines
            .last()
            .is_some_and(|line| line.contains("kill-pane --force ")),
        "{:?}",
        view.lines.last()
    );
}

/// Verifies that default prefix mux actions that do not open a command prompt
/// still perform a runtime side effect instead of being reported as unsupported.
#[test]
fn runtime_applies_default_prefix_mux_actions() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat"))
        .unwrap();
    service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat"))
        .unwrap();
    let active_before = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .clone();

    let cycle_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ExecuteMux(MuxAction::CyclePane)],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(cycle_report.mux_actions_applied, 1);
    assert!(cycle_report.view_refresh_required);
    assert!(!cycle_report.full_redraw_required);
    assert_ne!(
        service.session().active_window().unwrap().active_pane().id,
        active_before
    );

    let step = AttachedTerminalClientStepPlan {
        actions: vec![
            TerminalClientLoopAction::ExecuteMux(MuxAction::SendPrefixToPane),
            TerminalClientLoopAction::ExecuteMux(MuxAction::ListKeyBindings),
            TerminalClientLoopAction::ExecuteMux(MuxAction::ShowPaneIndexes),
            TerminalClientLoopAction::ExecuteMux(MuxAction::ShowMessages),
            TerminalClientLoopAction::ExecuteMux(MuxAction::EnterCopyModeAndPageUp),
            TerminalClientLoopAction::ExecuteMux(MuxAction::SwapPaneNext),
            TerminalClientLoopAction::ExecuteMux(MuxAction::SwapPanePrevious),
        ],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.mux_actions_applied, 7);
    assert!(report.unsupported_actions.is_empty());
    assert!(!service.active_copy_modes.is_empty());
    assert_eq!(service.session().active_window().unwrap().panes().len(), 3);
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events
            .iter()
            .any(|event| event.payload.contains("attached_display_command")),
        "{events:?}"
    );
    let display_panes_event = events
        .iter()
        .find(|event| {
            event
                .payload
                .contains(r#""attached_display_command":"display-panes""#)
        })
        .expect("display-panes binding should emit attached display output");
    assert!(
        display_panes_event
            .payload
            .contains("chooser=select-pane-index"),
        "{display_panes_event:?}"
    );
    assert!(
        display_panes_event
            .payload
            .contains("action=select-pane -t"),
        "{display_panes_event:?}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies runtime attached split mux action focuses new pane.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_attached_split_mux_action_focuses_new_pane() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneVertical)
            .unwrap()
    );

    let window = &service.session().windows()[0];
    assert_eq!(window.panes().len(), 2);
    assert_eq!(window.active_pane().id.as_str(), "%2");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies keyboard pane-focus actions use a diff redraw.
///
/// Focus changes move the global terminal cursor and restyle pane ownership
/// surfaces even when pane text stays unchanged. They need a fresh view, but
/// they should keep the retained output frame so the attached renderer can
/// update only changed rows and cursor state instead of clearing the viewport.
#[test]
fn runtime_keyboard_focus_pane_requests_diff_redraw() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneHorizontal)
            .unwrap()
    );
    service.session.select_pane(&primary, "%1").unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ExecuteMux(MuxAction::FocusPane(
                    PaneFocusDirection::Down,
                ))],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert_eq!(
        service.session().windows()[0].active_pane().id.as_str(),
        "%2"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies mouse focus uses the same pane-frame row accounting as rendering.
///
/// A top pane frame that is merged into an interior divider does not consume the
/// first content row of the pane below it. Mouse targeting must therefore allow a
/// click on that first rendered content row to focus the lower pane instead of
/// treating the row as an inert frame.
#[test]
fn runtime_mouse_focus_targets_content_below_merged_top_pane_frame() {
    let mut service = test_runtime_service_with_size(Size::new(20, 8).unwrap());
    service.window_frames_enabled = false;
    service.pane_frames_enabled = true;
    service.pane_frame_position = crate::terminal::TerminalFramePosition::Top;
    let primary = service
        .attach_primary("primary", true, Size::new(20, 8).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::SplitPaneHorizontal)
            .unwrap()
    );
    service.session.select_pane(&primary, "%1").unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::FocusPaneOnly(CopyPosition { line: 4, column: 0 }),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert_eq!(
        service.session().windows()[0].active_pane().id.as_str(),
        "%2"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that a repeated pane-content click copies the surrounding
/// readline-style word to the mouse paste buffer and host clipboard. This
/// protects double-click selection from using a separate whitespace-only token
/// model or leaving copy mode active after the word is copied.
#[test]
fn runtime_double_click_copies_readline_word_under_pointer() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    service.host_clipboard =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"alpha beta --flag");
    service.pane_screens.insert("%1".to_string(), screen);

    for _ in 0..2 {
        service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::HandleMouse(MouseAction::FocusPane(
                        CopyPosition { line: 0, column: 7 },
                    ))],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    }

    assert_eq!(service.paste_buffers.get("mouse"), Some("beta"));
    assert_eq!(TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().as_slice(), ["beta"]);
    assert!(!service.active_copy_modes.contains_key("%1"));
}

/// Verifies a double-click copied-word highlight remains visible across the
/// first render and only clears after its configured 500 ms lifetime expires.
/// This protects the copied-word flash from disappearing immediately on the
/// first rendered client view while still ensuring cleanup happens once the
/// timeout elapses.
#[test]
fn runtime_double_click_highlight_persists_until_cleanup_deadline() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    service.host_clipboard =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"alpha beta --flag");
    service.pane_screens.insert("%1".to_string(), screen);

    for _ in 0..2 {
        service
            .apply_attached_terminal_step_plan(
                &primary,
                &AttachedTerminalClientStepPlan {
                    actions: vec![TerminalClientLoopAction::HandleMouse(MouseAction::FocusPane(
                        CopyPosition { line: 0, column: 7 },
                    ))],
                    output_lines: Vec::new(),
                    output_line_style_spans: Vec::new(),
                    input_hangup: false,
                    output_hangup: false,
                    error_roles: Vec::new(),
                },
            )
            .unwrap();
    }

    assert!(service.deferred_word_copy_cleanup.borrow().is_some());
    let config = TerminalClientLoopConfig::default();
    let view = service
        .render_client_view(ClientViewRole::Primary, Size::new(20, 4).unwrap(), &config)
        .unwrap()
        .unwrap();
    assert!(!view.line_style_spans.iter().all(|spans| spans.is_empty()));
    assert!(service.deferred_word_copy_cleanup.borrow().is_some());

    if let Some((pane_id, copy_mode, cleanup_at_unix_ms)) =
        service.deferred_word_copy_cleanup.borrow_mut().as_mut()
    {
        *pane_id = "%1".to_string();
        *copy_mode = copy_mode.clone();
        *cleanup_at_unix_ms = 0;
    }

    service
        .render_client_view(ClientViewRole::Primary, Size::new(20, 4).unwrap(), &config)
        .unwrap()
        .unwrap();
    assert!(service.deferred_word_copy_cleanup.borrow().is_none());
}

/// Verifies that the attached-terminal detach binding runs through the runtime
/// lifecycle path rather than mutating session client state directly. The
/// lifecycle helper updates the service state and emits the client-detached
/// event that hooks, registry state, and observers use as the authoritative
/// detach signal.
#[test]
fn runtime_attached_detach_mux_action_emits_lifecycle_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    assert!(
        service
            .apply_attached_mux_action(&primary, MuxAction::DetachPrimaryClient)
            .unwrap()
    );

    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Detached);
    assert!(service.session().primary_client_id().is_none());
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events
            .iter()
            .any(|event| event.kind == EventKind::ClientDetached
                && event.payload.contains(r#""role":"primary""#)),
        "{events:?}"
    );
}

/// Verifies runtime attached mux action toggles agent shell state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_attached_mux_action_toggles_agent_shell_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::ToggleAgentShell,
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
    assert_eq!(report.mux_actions_applied, 1);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert!(report.unsupported_actions.is_empty());
    let list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"list","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(list.contains(r#""visible":true"#), "{list}");

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();
    assert_eq!(report.mux_actions_applied, 1);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    let list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"list2","method":"agent/list","params":{}}"#,
        &primary,
    );
    assert!(list.contains(r#""visible":false"#), "{list}");
}

/// Verifies that opening the pane-local agent prompt resizes the tracked PTY
/// to only the rows available for terminal content, then restores the original
/// size when agent mode exits. This protects cursor placement and terminal
/// application sizing from drifting under the agent input region.
#[test]
fn runtime_agent_shell_toggle_syncs_process_size_with_reserved_prompt_rows() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::ToggleAgentShell,
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    let initial_size = service
        .tracked_pane_descriptors()
        .into_iter()
        .find(|descriptor| descriptor.pane_id.as_str() == "%1")
        .unwrap()
        .size;

    let enter_report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();
    let agent_size = service
        .tracked_pane_descriptors()
        .into_iter()
        .find(|descriptor| descriptor.pane_id.as_str() == "%1")
        .unwrap()
        .size;

    assert_eq!(enter_report.mux_actions_applied, 1);
    assert_eq!(agent_size.columns, initial_size.columns);
    assert!(agent_size.rows < initial_size.rows);
    assert_eq!(service.pane_screens.get("%1").unwrap().size(), agent_size);

    let exit_report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();
    let restored_size = service
        .tracked_pane_descriptors()
        .into_iter()
        .find(|descriptor| descriptor.pane_id.as_str() == "%1")
        .unwrap()
        .size;

    assert_eq!(exit_report.mux_actions_applied, 1);
    assert_eq!(restored_size, initial_size);
    assert_eq!(service.pane_screens.get("%1").unwrap().size(), initial_size);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that ordinary pane input is redirected into the pane-local agent
/// prompt while agent mode is active, without entering the older modal prompt
/// loop. Mux actions remain available because only forward-to-pane text is
/// intercepted by the runtime.
#[test]
fn runtime_attached_input_submits_visible_agent_prompt_non_modally() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ForwardToPane(
            b"summarize\nmore\r".to_vec(),
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

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert_eq!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .map(|task| task.turn_id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-1"]
    );
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "");
    assert_eq!(
        prompt_state.prompt.buffer.history(),
        &[String::from("summarize\nmore")]
    );
    assert!(prompt_state.display_lines.is_empty());
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("user> summarize"), "{pane_text}");
    assert!(pane_text.contains("more"), "{pane_text}");
    assert!(
        !pane_text.contains("agent: turn turn-1 running"),
        "{pane_text}"
    );
    let turn = service
        .agent_turn_ledger
        .turns()
        .iter()
        .find(|turn| turn.turn_id == "turn-1")
        .unwrap();
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert_eq!(turn.state, AgentTurnState::Running);
    assert!(
        context
            .blocks
            .iter()
            .any(|block| block.content.contains("summarize\nmore"))
    );
}

/// Verifies large bracketed-paste agent prompt input is displayed compactly in
/// the pane transcript while the agent turn receives the exact pasted payload.
#[test]
fn runtime_agent_prompt_displays_large_paste_as_compact_block() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.pane_screens.insert(
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
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(
        prompt_state.prompt.buffer.history(),
        &[format!("prefix {payload} suffix")]
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
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(
        context
            .blocks
            .iter()
            .any(|block| block.content.contains(&format!("prefix {payload} suffix")))
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
    service.pane_screens.insert(
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
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(
        context
            .blocks
            .iter()
            .any(|block| block.content.contains(&format!("prefix {payload} suffix")))
    );
}

/// Verifies large prompt paste blocks can exceed the visible pane area.
///
/// Bracketed paste payloads may arrive split across terminal reads and contain
/// far more text than can be rendered in the prompt area. The prompt renderer
/// should show one compact block while the submitted turn receives the exact
/// payload.
#[test]
fn runtime_agent_prompt_preserves_large_split_paste_beyond_visible_area() {
    let mut service = test_runtime_service_with_size(Size::new(50, 8).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(50, 8).unwrap(), 10).unwrap(),
    );

    let payload = (0..80)
        .map(|index| format!("line-{index:02}-{}", "x".repeat(36)))
        .collect::<Vec<_>>()
        .join("\n");
    let mut first = Vec::new();
    first.extend_from_slice(b"prefix ");
    first.extend_from_slice(b"\x1b[200~");
    first.extend_from_slice(&payload.as_bytes()[..payload.len() / 2]);
    let mut second = Vec::new();
    second.extend_from_slice(&payload.as_bytes()[payload.len() / 2..]);
    second.extend_from_slice(b"\x1b[201~ suffix\r");

    for input in [first, second] {
        service
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
    }

    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(
        prompt_state.prompt.buffer.history(),
        &[format!("prefix {payload} suffix")]
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("user> prefix [Pasted"), "{pane_text}");
    assert!(!pane_text.contains("line-79"), "{pane_text}");
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(
        context
            .blocks
            .iter()
            .any(|block| { block.content.contains(&format!("prefix {payload} suffix")) })
    );
}

/// Verifies that the pane-local agent prompt accepts encoded Ctrl+R from
/// terminals that use xterm modifyOtherKeys for modified printable keys.
///
/// Agent mode intercepts ordinary pane input before it reaches the PTY. This
/// protects that interception path so encoded reverse-search keys still edit
/// the prompt from its history instead of becoming a no-op escape sequence.
#[test]
fn runtime_agent_prompt_accepts_encoded_ctrl_r_history_search() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    {
        let prompt_state = service.agent_prompt_inputs.get_mut("%1").unwrap();
        prompt_state
            .prompt
            .buffer
            .set_history(vec!["/status".to_string(), "/help".to_string()]);
        prompt_state.prompt.buffer.set_line("/s");
    }

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"\x1b[27;5;114~".to_vec(),
                )],
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
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "/status");
}
/// Verifies standalone Escape clears pane-local agent prompt text without
/// hiding the agent shell.
///
/// Agent-shell exit is reserved for Ctrl+C confirmation or empty Ctrl+D. A
/// normal Escape press should only clear the current draft and keep the pane
/// prompt session active.
#[test]
fn runtime_agent_prompt_escape_clears_input_without_hiding_shell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    {
        let prompt_state = service.agent_prompt_inputs.get_mut("%1").unwrap();
        prompt_state.prompt.buffer.set_line("draft text");
    }
    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
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
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    let followup = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"next".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(followup.forwarded_bytes, 0);
    assert_eq!(followup.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "next");
}

/// Verifies standalone Escape cancels pane-local agent reverse search without
/// exiting the agent shell.
///
/// Agent prompts share readline behavior with the primary command prompt, but
/// Escape also has agent-mode exit semantics. This keeps the reverse-search
/// case routed to the prompt before the broader exit handling runs.
#[test]
fn runtime_agent_prompt_escape_cancels_reverse_search() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    {
        let prompt_state = service.agent_prompt_inputs.get_mut("%1").unwrap();
        prompt_state
            .prompt
            .buffer
            .set_history(vec!["/status".to_string(), "/help".to_string()]);
        prompt_state.prompt.buffer.set_line("/s");
    }

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x12".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert!(
        service
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .reverse_search_active()
    );

    let escape = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(escape.forwarded_bytes, 0);
    assert_eq!(escape.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert!(!prompt_state.prompt.reverse_search_active());
    assert_eq!(prompt_state.prompt.buffer.line(), "/s");
    assert!(service.agent_shell_store().get("%1").is_some());
}

/// Verifies Up/Down move through soft-wrapped prompt rows before history.
///
/// Long single-line drafts can occupy multiple visible rows, but ordinary Up
/// and Down keys still operate on the rendered prompt rows before falling back
/// to the submitted-prompt history contract at the first or last row.
#[test]
fn runtime_agent_prompt_up_moves_within_soft_wrapped_draft_before_history() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(24, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(24, 8).unwrap(), 10).unwrap(),
    );
    {
        let prompt_state = service.agent_prompt_inputs.get_mut("%1").unwrap();
        prompt_state.prompt.buffer.set_history(vec![
            "first saved prompt".to_string(),
            "second saved prompt".to_string(),
        ]);
        prompt_state.prompt.buffer.set_line("alpha beta gamma delta");
    }
    let original_cursor = service
        .agent_prompt_inputs
        .get("%1")
        .unwrap()
        .prompt
        .buffer
        .cursor();

    let report = service
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

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "alpha beta gamma delta");
    assert!(prompt_state.prompt.buffer.cursor() < original_cursor);

    service
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
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "alpha beta gamma delta");

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
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "second saved prompt");
}

/// Verifies pane-local agent prompt navigation uses the rendered pane width
/// after reserving the shared right divider.
///
/// Split-pane agent prompts must wrap and move vertically on the same columns
/// the terminal renderer uses. Otherwise Up can move the cursor sideways on the
/// current visual row instead of to the row above.
#[test]
fn runtime_agent_prompt_navigation_uses_split_pane_render_width() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(30, 8).unwrap(), 120)
        .unwrap();
    service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    service.session.select_pane(&primary, "%1").unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    {
        let prompt_state = service.agent_prompt_inputs.get_mut("%1").unwrap();
        prompt_state.prompt.buffer.set_line("abcde fghij klmno");
    }
    let original_cursor = service
        .agent_prompt_inputs
        .get("%1")
        .unwrap()
        .prompt
        .buffer
        .cursor();

    let report = service
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

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "abcde fghij klmno");
    assert!(prompt_state.prompt.buffer.cursor() < original_cursor);
    assert_eq!(prompt_state.prompt.buffer.cursor(), "abcde fghij".len());
}

/// Verifies wrapped agent prompt navigation scrolls the visible prompt window
/// to keep the editing cursor on-screen.
///
/// Agent prompt rendering caps visible input rows at six. Moving upward through
/// a taller multiline draft must shift the rendered prompt window instead of
/// leaving the cursor on an off-screen row that cannot be edited in place.
#[test]
fn runtime_agent_prompt_navigation_scrolls_visible_rows_with_cursor() {
    let mut service = test_runtime_service_with_size(Size::new(24, 8).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(24, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(24, 8).unwrap(), 10).unwrap(),
    );
    {
        let prompt_state = service.agent_prompt_inputs.get_mut("%1").unwrap();
        prompt_state
            .prompt
            .buffer
            .set_line("row1\nrow2\nrow3\nrow4\nrow5\nrow6\nrow7");
    }

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"\x1b[A\x1b[A\x1b[A\x1b[A\x1b[A\x1b[A".to_vec(),
                )],
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
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.cursor(), "row1".len());
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(24, 8).unwrap(),
            &config,
        )
        .unwrap()
        .unwrap();
    let view_text = view.lines.join("\n");
    assert!(view_text.contains("mez> row1"), "{view_text}");
    assert!(!view_text.contains("row7"), "{view_text}");
}

/// Verifies pane-local prompt height changes immediately resize only the owning
/// PTY. Split panes can hold prompts with different wrapped heights, so typing
/// into one pane must not leave that pane at a stale process size or borrow the
/// sibling pane's prompt reservation.
#[test]
fn runtime_agent_prompt_height_resize_is_pane_local() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(30, 8).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let second_pane = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap()
        .pane_id;
    service.session.select_pane(&primary, "%1").unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();

    let initial_first = service.find_pane_descriptor("%1").unwrap().size;
    let initial_second = service
        .find_pane_descriptor(second_pane.as_str())
        .unwrap()
        .size;
    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"alpha beta gamma delta".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(report.view_refresh_required);
    let resized_first = service.find_pane_descriptor("%1").unwrap().size;
    let resized_second = service
        .find_pane_descriptor(second_pane.as_str())
        .unwrap()
        .size;

    assert_eq!(resized_first.columns, initial_first.columns);
    assert!(
        resized_first.rows < initial_first.rows,
        "owning pane PTY should shrink when its prompt wraps: {initial_first:?} -> {resized_first:?}"
    );
    assert_eq!(resized_second, initial_second);

    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies application-cursor-mode arrows still drive agent prompt navigation.
///
/// PTY applications can leave the pane in application cursor mode, which causes
/// the attached terminal router to forward SS3 arrow sequences. The
/// Mezzanine-owned agent prompt must normalize those bytes before applying
/// readline navigation.
#[test]
fn runtime_agent_prompt_accepts_application_cursor_arrow_sequences() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(24, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(24, 8).unwrap(), 10).unwrap(),
    );
    {
        let prompt_state = service.agent_prompt_inputs.get_mut("%1").unwrap();
        prompt_state.prompt.buffer.set_line("alpha beta gamma delta");
    }
    let original_cursor = service
        .agent_prompt_inputs
        .get("%1")
        .unwrap()
        .prompt
        .buffer
        .cursor();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1bOA".to_vec())],
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
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "alpha beta gamma delta");
    assert!(prompt_state.prompt.buffer.cursor() < original_cursor);
}

/// Verifies runtime agent prompts keep Up/Down within explicit multiline draft
/// rows before recalling submitted prompt history.
#[test]
fn runtime_agent_prompt_up_moves_within_multiline_draft_before_history() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(24, 8).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.reload_agent_prompt_history_for_pane("%1").unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(24, 8).unwrap(), 10).unwrap(),
    );
    {
        let prompt_state = service.agent_prompt_inputs.get_mut("%1").unwrap();
        prompt_state.prompt.buffer.set_history(vec![
            "first saved prompt".to_string(),
            "second saved prompt".to_string(),
        ]);
        prompt_state
            .prompt
            .buffer
            .set_line("first line\nsecond line\nthird line");
    }

    let original_cursor = service
        .agent_prompt_inputs
        .get("%1")
        .unwrap()
        .prompt
        .buffer
        .cursor();
    let report = service
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

    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(
        prompt_state.prompt.buffer.line(),
        "first line\nsecond line\nthird line"
    );
    assert!(prompt_state.prompt.buffer.cursor() < original_cursor);
}

/// Verifies that pane-local agent mode does not make the primary client modal.
/// Mux navigation can still focus another pane, and ordinary text input after
/// that focus change must go to the newly active shell instead of being
/// captured by the original pane's agent prompt.
#[test]
fn runtime_agent_prompt_allows_navigation_and_other_pane_input() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let input = b"echo outside agent\n".to_vec();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![
            TerminalClientLoopAction::ExecuteMux(MuxAction::SplitPaneVertical),
            TerminalClientLoopAction::ForwardToPane(input.clone()),
        ],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    let report = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(report.mux_actions_applied, 1);
    assert_eq!(report.forwarded_bytes, input.len());
    assert_eq!(report.agent_prompt_inputs_applied, 0);
    assert!(service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        service.session().windows()[0].active_pane().id.as_str(),
        "%2"
    );
    assert!(!service.agent_prompt_inputs.contains_key("%2"));
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that agent-mode prompt submissions convert runtime errors into a
/// pane-local error log instead of letting the attached terminal step fail.
/// Invalid-state errors previously bubbled out of this path and could terminate
/// the foreground client instead of leaving the agent prompt usable.
#[test]
fn runtime_attached_agent_prompt_logs_invalid_state_errors_non_modally() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 24).unwrap(), 10).unwrap(),
    );
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ForwardToPane(b"/stop\r".to_vec())],
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
    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert!(service.pending_agent_provider_tasks().is_empty());
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "");
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent command error: agent shell session has no running turn"),
        "{pane_text}"
    );
    let compact_pane_text = pane_text.replace("\n▐ ", "");
    assert!(compact_pane_text.contains("(invalid_state)"), "{pane_text}");
}

/// Verifies that terminal command execution uses live runtime state for the
/// agent shell toggle instead of falling through to the offline no-op command
/// planner. This covers both show and hide transitions for the active pane and
/// verifies transition clears preserve prior visible content in pane history.
#[test]
fn runtime_terminal_command_toggles_agent_shell_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 2).unwrap(), 10).unwrap();
    screen.feed(b"history line\nvisible before agent");
    service.pane_screens.insert("%1".to_string(), screen);
    let history_before_enter = service.pane_screen("%1").unwrap().history().len();
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("visible before agent")
    );

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains(r#""command":"agent-shell""#), "{show}");
    assert!(show.contains(r#""kind":"display""#), "{show}");
    assert!(show.contains("pane=%1"), "{show}");
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    assert!(
        show.contains(&format!("conversation_id={conversation_id}")),
        "{show}"
    );
    assert!(show.contains("visibility=visible"), "{show}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    let after_enter_screen = service.pane_screen("%1").unwrap();
    assert!(after_enter_screen.history().len() > history_before_enter);
    assert!(
        !after_enter_screen
            .visible_lines()
            .join("\n")
            .contains("visible before agent")
    );
    assert!(
        after_enter_screen
            .normal_content_lines()
            .join("\n")
            .contains("visible before agent")
    );
    let history_before_exit = after_enter_screen.history().len();
    service
        .pane_screens
        .get_mut("%1")
        .unwrap()
        .feed(b"visible inside agent");
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("visible inside agent")
    );

    let hide = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(hide.contains("visibility=hidden"), "{hide}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Hidden)
    );
    let after_exit_screen = service.pane_screen("%1").unwrap();
    assert!(after_exit_screen.history().len() > history_before_exit);
    assert!(
        !after_exit_screen
            .visible_lines()
            .join("\n")
            .contains("visible inside agent")
    );
    assert!(
        after_exit_screen
            .normal_content_lines()
            .join("\n")
            .contains("visible inside agent")
    );

    let show_again = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show_again.contains("visibility=visible"), "{show_again}");
    let after_reentry_screen = service.pane_screen("%1").unwrap();
    assert!(
        !after_reentry_screen
            .visible_lines()
            .join("\n")
            .contains("visible inside agent"),
        "agent reentry should start from a clean viewport, not scroll old agent logs back into view"
    );
    assert!(
        after_reentry_screen
            .normal_content_lines()
            .join("\n")
            .contains("visible inside agent")
    );
}

/// Verifies that showing agent mode starts a pane-local subshell and hiding it
/// exits that subshell instead of sending redraw traffic to the user's original
/// interactive shell. This protects prompt, option, and environment mutations
/// made by agent commands from leaking back to the parent shell, and confirms
/// that retained hidden-render suppression is cleared so the parent prompt
/// repaint can advance the terminal cursor to the end of the prompt line.
#[test]
fn runtime_agent_shell_toggle_enters_and_exits_pane_subshell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_async_owner(&pane_id)
        .unwrap();

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    let enter_input = service.drain_deferred_pane_inputs();
    assert_eq!(enter_input.len(), 1);
    assert_eq!(enter_input[0].pane_id, pane_id);
    let enter_text = String::from_utf8_lossy(&enter_input[0].bytes);
    assert!(
        enter_text.contains("command env -u BASH_ENV -u ENV -u ZDOTDIR"),
        "{enter_text}"
    );
    assert!(enter_text.contains("HISTFILE=/dev/null"), "{enter_text}");
    assert!(enter_text.contains("'/bin/sh'"), "{enter_text}");
    assert!(service.agent_subshell_panes.contains(&pane_id));
    service.remember_mez_wrapper_filter_command(&pane_id, "MEZ_MARKER_TOKEN='abc'");

    let hide = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(hide.contains("visibility=hidden"), "{hide}");
    let exit_input = service.drain_deferred_pane_inputs();
    assert_eq!(exit_input.len(), 1);
    assert_eq!(exit_input[0].pane_id, pane_id);
    assert_eq!(exit_input[0].bytes, b"\x04");
    assert!(!service.agent_subshell_panes.contains(&pane_id));
    assert!(!service.hidden_shell_render_retention_timer_needed());
    let simple_prompt_repaint = service.visible_pane_output_bytes(&pane_id, b"\r$ ");
    assert_eq!(simple_prompt_repaint, b"\r$ ");
    let prompt_repaint = service.renderable_pane_output_bytes(&pane_id, b"user@host ~/repo $ ");
    assert_eq!(prompt_repaint, b"user@host ~/repo $ ");
    service
        .apply_pane_output_bytes(pane_id.clone(), b"user@host ~/repo $ ".to_vec())
        .unwrap();
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig {
                window_frames_enabled: false,
                pane_frames_enabled: false,
                ..TerminalClientLoopConfig::default()
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(view.cursor_column, "user@host ~/repo $ ".chars().count());
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies that Ctrl+D from a visible agent prompt restores the parent shell
/// cursor after agent-authored text has been rendered into the pane. The
/// preceding agent output leaves the pane screen on a Mezzanine-rendered line,
/// so the subsequent parent prompt repaint must still advance through the
/// prompt's trailing space instead of landing one cell early.
#[test]
fn runtime_agent_shell_ctrl_d_after_agent_output_restores_prompt_cursor() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_async_owner(&pane_id)
        .unwrap();

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    assert_eq!(service.drain_deferred_pane_inputs().len(), 1);
    service
        .append_agent_assistant_text_to_terminal_buffer(&pane_id, "done")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x04".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert_eq!(
        service
            .agent_shell_store()
            .get(&pane_id)
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Hidden),
        "Ctrl+D should hide the agent prompt before the parent prompt repaint"
    );
    let exit_input = service.drain_deferred_pane_inputs();
    assert_eq!(exit_input.len(), 1);
    assert_eq!(exit_input[0].pane_id, pane_id);
    assert_eq!(exit_input[0].bytes, b"\x04");

    let prompt = b"user@host ~/repo $ ";
    let prompt_repaint = service.renderable_pane_output_bytes(&pane_id, prompt);
    assert_eq!(prompt_repaint, prompt);
    service
        .apply_pane_output_bytes(pane_id.clone(), prompt.to_vec())
        .unwrap();
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig {
                window_frames_enabled: false,
                pane_frames_enabled: false,
                ..TerminalClientLoopConfig::default()
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(view.cursor_column, "user@host ~/repo $ ".chars().count());
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies that the live subshell EOF path also restores the parent prompt
/// cursor after agent-authored text has already moved the pane screen. This
/// covers the Ctrl+D path that exits the child agent shell, waits for the parent
/// shell prompt to repaint, and then presents the attached terminal cursor.
#[test]
fn runtime_agent_shell_ctrl_d_after_agent_output_restores_live_parent_cursor() {
    let shell_path = PathBuf::from("/bin/sh");
    let shell_available = fs::metadata(&shell_path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false);
    if !shell_available {
        eprintln!("skipping live cursor regression because /bin/sh is unavailable");
        return;
    }
    let mut service = RuntimeSessionService::with_event_log(
        Session::new_default(
            ResolvedShell::new(shell_path.clone(), ShellSource::ShellEnv),
            Size::new(80, 24).unwrap(),
        ),
        PathBuf::from("/tmp/mez-1000/default.sock"),
        100,
        10,
        1024,
    )
    .unwrap();
    service.host_clipboard = HostClipboard::disabled();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some(
            "/bin/sh -c 'PS1=\"parent$ \"; export PS1; exec /bin/sh -i'",
        ))
        .unwrap();
    let mut initial_screen = String::new();
    for _ in 0..200 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        initial_screen = service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n");
        if initial_screen.contains("parent$") {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(
        initial_screen.contains("parent$"),
        "parent prompt did not arrive: {initial_screen:?}"
    );

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    service
        .append_agent_assistant_text_to_terminal_buffer("%1", "done")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x04".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert!(report.full_redraw_required);

    let prompt_column = "parent$ ".chars().count();
    let mut cursor_column = None;
    let mut observed_cursor = None;
    let mut observed_screen = String::new();
    for _ in 0..100 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        let cursor = service.pane_screen("%1").unwrap().cursor_state();
        let screen_text = service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n");
        observed_cursor = Some(cursor);
        observed_screen = screen_text.clone();
        if screen_text.contains("parent$") && cursor.column == prompt_column {
            cursor_column = Some(cursor.column);
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }

    assert_eq!(
        cursor_column,
        Some(prompt_column),
        "parent prompt cursor should land after the trailing prompt space; observed_cursor={observed_cursor:?}; observed_screen={observed_screen:?}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that `/exit` from the pane-scoped agent prompt performs the same
/// subshell exit as the keyboard toggle while preserving pane-visible content in
/// history. This covers the slash-command path used by Escape, Ctrl+C, Ctrl+D
/// on an empty prompt, `/quit`, and direct `/exit` submissions through the
/// control API.
#[test]
fn runtime_agent_shell_slash_exit_exits_pane_subshell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_async_owner(&pane_id)
        .unwrap();
    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    assert_eq!(service.drain_deferred_pane_inputs().len(), 1);
    assert!(service.agent_subshell_panes.contains(&pane_id));
    service
        .pane_screens
        .get_mut(&pane_id)
        .unwrap()
        .feed(b"slash exit history\nslash exit visible text");
    assert!(
        service
            .pane_screen(&pane_id)
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("slash exit visible text")
    );

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-exit","method":"agent/shell/command","params":{"idempotency_key":"agent-exit","input":"/exit"}}"#,
        &primary,
    );
    assert!(response.contains(r#""visibility":"hidden""#), "{response}");
    let exit_input = service.drain_deferred_pane_inputs();
    assert_eq!(exit_input.len(), 1);
    assert_eq!(exit_input[0].pane_id, pane_id);
    assert_eq!(exit_input[0].bytes, b"\x04");
    assert!(!service.agent_subshell_panes.contains(&pane_id));
    let after_exit_screen = service.pane_screen(&pane_id).unwrap();
    assert!(
        !after_exit_screen
            .visible_lines()
            .join("\n")
            .contains("slash exit visible text")
    );
    assert!(
        after_exit_screen
            .normal_content_lines()
            .join("\n")
            .contains("slash exit visible text")
    );
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies `/exit` stops an active pane-local turn before hiding agent mode.
/// This protects the exit paths used by slash commands, keyboard shortcuts, and
/// control clients from leaving provider or shell-action work running unseen.
#[test]
fn runtime_agent_shell_slash_exit_stops_running_turn_before_hiding() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt-exit-stop","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-exit","method":"agent/shell/command","params":{"idempotency_key":"agent-exit-stop","input":"/exit"}}"#,
        &primary,
    );

    assert!(response.contains(r#""command":"exit""#), "{response}");
    assert!(response.contains(r#""visibility":"hidden""#), "{response}");
    assert!(response.contains("stopped_turn=turn-1"), "{response}");
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(session.visibility, AgentShellVisibility::Hidden);
    assert_eq!(session.running_turn_id, None);
    assert!(!service.agent_turn_is_running("turn-1"));
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("Stopped after"), "{pane_text}");
}

/// Verifies exiting agent mode after interrupting a live shell transaction
/// closes the nested agent subshell with a line command.
///
/// Immediate EOF can be consumed by an interrupted transaction wrapper's read
/// loop, leaving the user inside the child shell after agent mode hides. After
/// a live transaction is interrupted, the runtime should queue Ctrl+C followed
/// by `exit` so the command is read by the shell after the wrapper unwinds.
#[test]
fn runtime_agent_shell_exit_after_shell_transaction_uses_command_exit() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(Some("cat")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut process = service
        .take_running_pane_process_for_async_owner(&pane_id)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    service.agent_subshell_panes.insert(pane_id.clone());
    let started = service
        .start_agent_prompt_turn(&pane_id, "search the file")
        .unwrap();
    service.running_shell_transactions.insert(
        "marker-grep".to_string(),
        RunningShellTransactionRef {
            turn_id: started.turn_id.clone(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "shell-grep".to_string(),
            },
            pane_id: pane_id.clone(),
            command: "grep -n needle file.txt".to_string(),
            started_at_unix_ms: 1_000,
            timeout_ms: Some(10 * 60 * 1000),
            pending_input_payload: Some(b"payload\n".to_vec()),
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-exit","method":"agent/shell/command","params":{"idempotency_key":"agent-exit-live-shell","input":"/exit"}}"#,
        &primary,
    );

    assert!(response.contains(r#""visibility":"hidden""#), "{response}");
    let exit_inputs = service.drain_deferred_pane_inputs();
    assert_eq!(exit_inputs.len(), 2);
    assert_eq!(exit_inputs[0].pane_id, pane_id);
    assert_eq!(exit_inputs[0].bytes, b"\x03");
    assert_eq!(exit_inputs[1].pane_id, pane_id);
    assert_eq!(exit_inputs[1].bytes, b"exit\n");
    assert!(!service.agent_subshell_panes.contains(&pane_id));
    assert!(!service.agent_subshell_command_exit_panes.contains(&pane_id));
    let _ = process.terminate(Duration::from_millis(10));
}

/// Verifies Escape does not interrupt active agent work or exit agent mode.
///
/// The pane-local prompt owns Escape while visible. During active work it only
/// clears draft input, so an empty draft leaves the shell visible and the
/// running turn untouched.
#[test]
fn runtime_agent_prompt_escape_preserves_running_turn() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt-escape-stop","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 0);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        Some("turn-1")
    );
    assert!(service.agent_turn_is_running("turn-1"));
}

/// Verifies Ctrl+C uses the same active-work interruption path as Escape.
///
/// Ctrl+C arrives through readline as a cancellation outcome rather than the
/// direct Escape byte path, so it needs separate coverage to ensure both input
/// routes reuse the same `/stop` behavior.
#[test]
fn runtime_agent_prompt_ctrl_c_interrupts_running_turn() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt-ctrl-c-stop","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x03".to_vec())],
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
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    assert!(!service.agent_turn_is_running("turn-1"));
}

/// Verifies Escape is a no-op for an empty idle pane-local agent shell.
///
/// Agent-shell exit is reserved for Ctrl+C confirmation or empty Ctrl+D, so
/// Escape with no draft input keeps the prompt visible without forwarding bytes
/// to the pane PTY.
#[test]
fn runtime_agent_prompt_escape_keeps_empty_idle_shell_visible() {
    let mut service = test_runtime_service();
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
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x1b".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(report.forwarded_bytes, 0);
    assert_eq!(report.agent_prompt_inputs_applied, 0);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
}

/// Verifies idle Ctrl+C requires confirmation before exiting agent mode.
///
/// Ctrl+C is easy to hit accidentally while editing a prompt. The first press
/// should show a pane-local status message and keep the prompt visible; the
/// second press within the confirmation window exits.
#[test]
fn runtime_agent_prompt_ctrl_c_requires_second_press_when_idle() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let first = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x03".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(first.forwarded_bytes, 0);
    assert_eq!(first.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("press ctrl-c again within 3s to exit agent mode"),
        "{pane_text}"
    );

    let second = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x03".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(second.forwarded_bytes, 0);
    assert_eq!(second.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Hidden)
    );
}

/// Verifies idle Ctrl+C clears a nonempty pane-local agent prompt before using
/// the double-confirm exit path for an already empty prompt.
#[test]
fn runtime_agent_prompt_ctrl_c_clears_nonempty_buffer_when_idle() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let edit = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"draft text".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(edit.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "draft text"
    );

    let clear = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x03".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(clear.forwarded_bytes, 0);
    assert_eq!(clear.agent_prompt_inputs_applied, 1);
    let prompt_state = service.agent_prompt_inputs.get("%1").unwrap();
    assert_eq!(prompt_state.prompt.buffer.line(), "");
    assert!(prompt_state.display_lines.is_empty());
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );

    let confirm = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x03".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();
    assert_eq!(confirm.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("press ctrl-c again within 3s to exit agent mode")
    );
}

/// Verifies ordinary pane input is consumed while an agent-shell hide request
/// is waiting for the active turn to stop. This prevents user keystrokes from
/// leaking into the parent shell before the `/stop` contract has completed.
#[test]
fn runtime_agent_shell_exit_pending_blocks_foreground_input() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .request_hide_pending_task_completion("%1")
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"leak\r".to_vec())],
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
    assert!(
        pane_text.contains("agent: input blocked while agent shell is stopping"),
        "{pane_text}"
    );
}

/// Verifies Ctrl+L clears the live viewport while keeping the pane-local agent
/// prompt available and preserving prior visible content in pane history.
#[test]
fn runtime_agent_prompt_ctrl_l_clears_pane_buffer() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(50, 8).unwrap(), 120).unwrap();
    screen.feed(b"old agent output");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("old agent output")
    );

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x0c".to_vec())],
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
    assert!(
        !service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("old agent output")
    );
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("old agent output")
    );
    assert!(service.agent_shell_store().get("%1").is_some());
}

/// Verifies `/resume` completion includes saved conversation ids supplied by
/// the runtime transcript store.
#[test]
fn runtime_agent_prompt_resume_autocompletes_saved_session_uuid() {
    let mut service = test_runtime_service();
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-resume-complete"));
    transcript_store
        .append(&crate::transcript::TranscriptEntry {
            conversation_id: "018f6b3a-1b2c-7000-9000-cafebabefeed".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: crate::transcript::TranscriptRole::User,
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
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/resume 018f6b3a-1b2c-7000-9000-cafebabefeed "
    );
}

/// Verifies `/personality` completion includes user-configured personality
/// profile ids.
///
/// Personality profiles have no built-in names, so completion must be sourced
/// from live runtime config rather than from a static candidate list.
#[test]
fn runtime_agent_prompt_personality_autocompletes_configured_profile() {
    let mut service = test_runtime_service();
    let root = temp_root("runtime-agent-personality-complete");
    let path = root.join("config.toml");
    fs::write(
        &path,
        "[personalities.careful]\nname = \"Careful\"\nresponse_style = \"terse\"\n",
    )
    .unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&path).unwrap(),
        }])
        .unwrap();
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
                    TerminalClientLoopAction::ForwardToPane(b"/personality car".to_vec()),
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
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/personality careful "
    );
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies `/list-mcp` completion includes configured MCP server ids supplied
/// by the live runtime registry. MCP server names are dynamic configuration
/// data, so they must not be limited to static slash-command candidates.
#[test]
fn runtime_agent_prompt_list_mcp_autocompletes_configured_server_id() {
    let mut service = test_runtime_service();
    service
        .mcp_registry_mut()
        .add_server(crate::mcp::McpServerConfig::stdio(
            "fixture",
            "Fixture MCP",
            "mcp-fixture",
            Vec::new(),
        ))
        .unwrap();
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
                    TerminalClientLoopAction::ForwardToPane(b"/list-mcp fi".to_vec()),
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
            .agent_prompt_inputs
            .get("%1")
            .unwrap()
            .prompt
            .buffer
            .line(),
        "/list-mcp fixture "
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
        (1, crate::transcript::TranscriptRole::User, "aGVsbG8K"),
        (
            2,
            crate::transcript::TranscriptRole::Assistant,
            "I inspected the repo and started the change",
        ),
        (
            3,
            crate::transcript::TranscriptRole::Tool,
            r#"action_id=action-1 action_type=say status=succeeded content: ignored structured_content: {"kind":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"Implemented the change"}"#,
        ),
    ] {
        transcript_store
            .append(&crate::transcript::TranscriptEntry {
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

/// Verifies that hiding a visible agent shell through terminal command routing
/// stops the in-progress turn before returning control to the pane.
#[test]
fn runtime_terminal_command_hides_running_agent_shell_after_task_completion() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt-hide-stop","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");

    let hide = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(hide.contains("visibility=hidden"), "{hide}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Hidden)
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    assert!(!service.agent_turn_is_running("turn-1"));

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
}

/// Verifies runtime control dispatches agent shell command for visible shell.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_control_dispatches_agent_shell_command_for_visible_shell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::ToggleAgentShell,
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };
    service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-command","method":"agent/shell/command","params":{"idempotency_key":"agent-status","input":"/status"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains("| Visibility | visible |"), "{response}");
    assert!(response.contains(r#""turn":null"#), "{response}");

    let alias_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-command-alias","method":"agent/shell/command","params":{"idempotency_key":"agent-command-alias","command":"/status"}}"#,
        &primary,
    );
    assert!(
        alias_response.contains(r#""mezzanine_code":"invalid_params""#),
        "{alias_response}"
    );
    assert!(
        alias_response.contains("agent/shell/command params contains unknown field `command`"),
        "{alias_response}"
    );
}

/// Verifies that invalid runtime-backed slash command arguments are converted
/// into pane-local display responses rather than JSON-RPC errors. This keeps
/// the agent prompt alive for commands whose validation happens in runtime
/// handlers instead of the slash-command registry.
#[test]
fn runtime_control_reports_invalid_runtime_slash_args_as_agent_display() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-command-invalid","method":"agent/shell/command","params":{"idempotency_key":"agent-command-invalid","input":"/model one two three"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(
        response.contains(
            "agent command error: /model accepts at most a model name and optional reasoning level"
        ),
        "{response}"
    );
    assert!(!response.contains(r#""error""#), "{response}");
}

/// Verifies that runtime-state failures from agent slash commands are reported
/// through the agent display channel instead of surfacing as JSON-RPC errors.
/// This keeps agent-mode clients alive when a runtime-backed command hits an
/// invalid state, such as stopping when no turn is running.
#[test]
fn runtime_control_reports_invalid_state_agent_shell_errors_as_display() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-command-invalid-state","method":"agent/shell/command","params":{"idempotency_key":"agent-command-invalid-state","input":"/stop"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(
        response.contains("agent command error: agent shell session has no running turn"),
        "{response}"
    );
    assert!(response.contains("(invalid_state)"), "{response}");
    assert!(!response.contains(r#""error""#), "{response}");
}

/// Verifies that runtime `terminal/command` accepts only the spec-defined
/// `input` field. The legacy `command` alias is rejected at the params schema
/// boundary so clients cannot depend on a non-normative request shape.
#[test]
fn runtime_terminal_command_rejects_legacy_command_alias() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let alias_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"terminal-command-alias","method":"terminal/command","params":{"idempotency_key":"terminal-command-alias","command":"list-windows"}}"#,
        &primary,
    );

    assert!(
        alias_response.contains(r#""mezzanine_code":"invalid_params""#),
        "{alias_response}"
    );
    assert!(
        alias_response.contains("terminal/command params contains unknown field `command`"),
        "{alias_response}"
    );
}

/// Verifies that an unknown command submitted through the supported
/// `terminal/command` JSON-RPC method is reported as invalid command input, not
/// as JSON-RPC method-not-found. The transport method is implemented; only the
/// command language token is unknown.
#[test]
fn runtime_terminal_command_unknown_input_is_invalid_params() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"terminal-command-unknown","method":"terminal/command","params":{"idempotency_key":"terminal-command-unknown","input":"does-not-exist"}}"#,
        &primary,
    );

    assert!(
        response.contains(r#""mezzanine_code":"invalid_params""#),
        "{response}"
    );
    assert!(
        response.contains("unknown command `does-not-exist`"),
        "{response}"
    );
    assert!(
        !response.contains(r#""mezzanine_code":"method_not_found""#),
        "{response}"
    );
}

/// Verifies that the runtime `agent/shell/command` `/list-mcp` path uses the live
/// MCP registry and exposes unavailable or session-blacklisted details. This
/// protects the spec requirement that agent-shell MCP visibility match control
/// and command surfaces instead of returning a generic runtime placeholder.
#[test]
fn runtime_agent_shell_mcp_command_reports_live_registry_detail() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .mcp_registry_mut()
        .add_server(crate::mcp::McpServerConfig::stdio(
            "fs",
            "filesystem",
            "mcp-fs",
            Vec::new(),
        ))
        .unwrap();
    service
        .mcp_registry_mut()
        .mark_available(
            "fs",
            vec![crate::mcp::McpToolState {
                server_id: String::new(),
                name: "read_file".to_string(),
                available: true,
                blacklisted: false,
                permission_required: true,
                effects: crate::mcp::McpToolEffects::none(),
                approval: crate::mcp::McpApprovalSetting::Inherit,
                description: "read a file".to_string(),
                input_schema_json: "{}".to_string(),
            }],
        )
        .unwrap();
    service
        .mcp_registry_mut()
        .blacklist_for_session("fs", "failed handshake")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-mcp","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp","input":"/list-mcp"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"list-mcp""#), "{response}");
    assert!(response.contains("## MCP Servers"), "{response}");
    assert!(response.contains("Servers: 1"), "{response}");
    assert!(response.contains("Tools: 1"), "{response}");
    assert!(response.contains("Source: runtime-mcp"), "{response}");
    assert!(response.contains("### `fs` - filesystem"), "{response}");
    assert!(response.contains("- State: blacklisted"), "{response}");
    assert!(
        response.contains("- Session blacklisted: true"),
        "{response}"
    );
    assert!(response.contains("- Retryable: true"), "{response}");
    assert!(
        response.contains("- Reason: failed handshake"),
        "{response}"
    );
    assert!(
        response.contains("| `read_file` | blacklisted |"),
        "{response}"
    );
    assert!(!response.contains("requires_runtime"), "{response}");
}

/// Verifies async runtime config application initializes MCP transports at
/// session-start time and records human-readable lifecycle status.
///
/// MCP tools need to be available before the first model request so the model
/// can choose `mcp_call` from concrete runtime context instead of treating the
/// server as an unknown integration. The event log also needs plain status
/// messages so operators can see when startup discovery begins and when MCP
/// servers are ready to field requests.
#[tokio::test]
async fn runtime_async_config_apply_initializes_mcp_and_logs_readable_status() {
    let mut service = test_runtime_service();
    let script = runtime_mcp_fixture_script(false);

    let report = service
        .replace_config_layers_async(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[mcp_servers.fixture]\ncommand = \"/bin/sh\"\nargs = [\"-c\", {}]\napproval = \"allow\"\ntool_timeout_ms = 1000\n",
                toml_string(&script)
            ),
        }])
        .await
        .unwrap();

    assert_eq!(report.mcp_servers_configured, 1);
    assert_eq!(
        service.mcp_registry().list_servers()[0].status,
        crate::mcp::McpServerStatus::Available
    );
    assert_eq!(
        service.mcp_registry().prompt_summary().available_tools[0].tool_name,
        "echo"
    );
    let payloads = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .into_iter()
        .map(|event| event.payload)
        .collect::<Vec<_>>();
    assert!(
        payloads.iter().any(|payload| {
            payload.contains(r#""phase":"started""#)
                && payload.contains("Starting MCP initialization for 1 configured server.")
        }),
        "{payloads:#?}"
    );
    assert!(
        payloads.iter().any(|payload| {
            payload.contains(r#""server_id":"fixture""#)
                && payload.contains(r#""status":"available""#)
                && payload.contains("MCP server fixture is ready to field requests")
        }),
        "{payloads:#?}"
    );
    assert!(
        payloads.iter().any(|payload| {
            payload.contains(r#""phase":"completed""#)
                && payload.contains("MCP initialization complete: 1 enabled server ready to field requests")
        }),
        "{payloads:#?}"
    );
}

/// Verifies `/list-mcp` starts configured MCP transports after a synchronous
/// config load. Default startup paths apply configuration synchronously, so the
/// user-facing MCP listing must not require a separate config reload before the
/// server becomes available to the agent runtime.
#[test]
fn runtime_agent_shell_list_mcp_lazily_discovers_configured_server() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-agent-list-mcp-lazy-discovery");
    let script_path = root.join("mcp-fixture.sh");
    fs::write(&script_path, runtime_mcp_fixture_script(false)).unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[mcp_servers.fixture]\ncommand = \"/bin/sh\"\nargs = [{}]\napproval = \"allow\"\ntool_timeout_ms = 1000\n",
                toml_string(script_path.to_string_lossy().as_ref())
            ),
        }])
        .unwrap();
    assert_eq!(
        service.mcp_registry().list_servers()[0].status,
        crate::mcp::McpServerStatus::Configured
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-mcp","method":"agent/shell/command","params":{"idempotency_key":"agent-mcp-lazy","input":"/list-mcp"}}"#,
        &primary,
    );

    assert!(response.contains("## MCP Servers"), "{response}");
    assert!(response.contains("Servers: 1"), "{response}");
    assert!(response.contains("Tools: 1"), "{response}");
    assert!(response.contains("### `fixture` - fixture"), "{response}");
    assert!(response.contains("- Status: available"), "{response}");
    assert!(response.contains("| `echo` | available |"), "{response}");
    assert_eq!(
        service.mcp_registry().prompt_summary().available_tools[0].tool_name,
        "echo"
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies that `/new` is a live agent-shell mutation rather than a generic
/// runtime-required placeholder. A fresh conversation id with zero transcript
/// entries must replace the active pane's completed conversation while keeping
/// the shell visible for the next prompt.
#[test]
fn runtime_agent_shell_new_command_starts_fresh_conversation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .start_turn("%1", "turn-previous")
        .unwrap();
    service
        .agent_shell_store_mut()
        .finish_turn("%1", "turn-previous")
        .unwrap();
    let old_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-new","method":"agent/shell/command","params":{"idempotency_key":"agent-new","input":"/new"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"new""#), "{response}");
    assert!(response.contains("new=true"), "{response}");
    assert!(response.contains("transcript_entries=0"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_ne!(session.session_id, old_session);
    assert_eq!(session.transcript_entries, 0);
    assert_eq!(session.visibility, AgentShellVisibility::Visible);
}

/// Verifies default `/loop` reuses the current pane conversation for the first
/// work iteration.
///
/// In-place iteration is the default mode, so the first loop work turn should
/// prompt the model in the already-active session instead of rebinding to a
/// forked transcript.
#[test]
fn runtime_agent_loop_reuses_current_conversation_by_default() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-loop-reuse"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service.start_initial_pane_process(Some("cat >/dev/null")).unwrap();
    service
        .pane_screens
        .insert("%1".to_string(), TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap());
    service.agent_shell_store_mut().enter_or_resume("%1").unwrap();
    let old_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: old_session.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "parent-turn".to_string(),
            agent_id: "agent".to_string(),
            pane_id: "%1".to_string(),
            content: "review this document".to_string(),
        })
        .unwrap();

    let outcome = service
        .execute_agent_shell_loop_command("%1", "/loop review this document")
        .unwrap();

    assert!(matches!(outcome, crate::runtime::AgentShellCommandOutcome::Mutated { .. }));
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(session.session_id, old_session);
    assert_eq!(session.visibility, AgentShellVisibility::Visible);
    let pane_text = service.pane_screen("%1").unwrap().visible_lines().join("\n");
    assert!(pane_text.contains("user> /loop review this document"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `/loop --fork` rotates the pane to a fresh ephemeral conversation
/// before the first work iteration starts.
///
/// Fork-mode loop attempts need isolated model context without creating saved
/// conversations. This regression keeps the work conversation runtime-only and
/// checkpoints the parent conversation as the resumable pane binding.
#[test]
fn runtime_agent_loop_fork_option_starts_first_iteration_in_ephemeral_conversation() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-loop-fork"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service.start_initial_pane_process(Some("cat >/dev/null")).unwrap();
    service
        .pane_screens
        .insert("%1".to_string(), TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap());
    service.agent_shell_store_mut().enter_or_resume("%1").unwrap();
    let old_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: old_session.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "parent-turn".to_string(),
            agent_id: "agent".to_string(),
            pane_id: "%1".to_string(),
            content: "review this document".to_string(),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 1)
        .unwrap();

    let outcome = service
        .execute_agent_shell_loop_command("%1", "/loop --fork review this document")
        .unwrap();

    assert!(matches!(outcome, crate::runtime::AgentShellCommandOutcome::Mutated { .. }));
    let loop_session = {
        let session = service.agent_shell_store().get("%1").unwrap();
        assert_ne!(session.session_id, old_session);
        assert!(session.ephemeral);
        assert_eq!(
            session.ephemeral_transcript_source_conversation_id.as_deref(),
            Some(old_session.as_str())
        );
        assert_eq!(session.ephemeral_transcript_source_entries, 1);
        assert_eq!(session.transcript_entries, 0);
        assert_eq!(session.visibility, AgentShellVisibility::Visible);
        session.session_id.clone()
    };
    assert!(transcript_store.summary(&loop_session).unwrap().is_none());
    let saved = transcript_store.list().unwrap();
    assert!(saved.iter().any(|summary| summary.conversation_id == old_session));
    assert!(!saved.iter().any(|summary| summary.conversation_id == loop_session));
    service.checkpoint_agent_session_metadata().unwrap();
    let metadata = transcript_store
        .load_agent_session_metadata(service.session().id.as_str())
        .unwrap();
    assert_eq!(metadata.len(), 1, "{metadata:#?}");
    assert_eq!(metadata[0].conversation_id, old_session);
    assert_eq!(metadata[0].transcript_entries, 1);
    let pane_text = service.pane_screen("%1").unwrap().visible_lines().join("\n");
    assert!(pane_text.contains("user> /loop --fork review this document"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `/loop --fork` can start from a pane conversation that has no
/// persisted transcript entries yet.
///
/// The fork-mode loop controller forks each iteration from the parent pane conversation.
/// A brand-new pane may not have any saved transcript rows, so the first loop
/// iteration still needs a fresh conversation id instead of failing the fork.
#[test]
fn runtime_agent_loop_fork_option_starts_when_parent_conversation_has_no_saved_entries() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-loop-empty-parent"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service.start_initial_pane_process(Some("cat >/dev/null")).unwrap();
    service
        .pane_screens
        .insert("%1".to_string(), TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap());
    service.agent_shell_store_mut().enter_or_resume("%1").unwrap();
    let old_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();

    let outcome = service
        .execute_agent_shell_loop_command("%1", "/loop --fork review this document")
        .unwrap();

    assert!(matches!(outcome, crate::runtime::AgentShellCommandOutcome::Mutated { .. }));
    let loop_session = {
        let session = service.agent_shell_store().get("%1").unwrap();
        assert_ne!(session.session_id, old_session);
        assert!(session.ephemeral);
        assert_eq!(
            session.ephemeral_transcript_source_conversation_id.as_deref(),
            Some(old_session.as_str())
        );
        assert_eq!(session.ephemeral_transcript_source_entries, 0);
        assert_eq!(session.transcript_entries, 0);
        assert_eq!(session.visibility, AgentShellVisibility::Visible);
        session.session_id.clone()
    };
    assert!(transcript_store.summary(&loop_session).unwrap().is_none());
    service.checkpoint_agent_session_metadata().unwrap();
    let metadata = transcript_store
        .load_agent_session_metadata(service.session().id.as_str())
        .unwrap();
    assert_eq!(metadata.len(), 1, "{metadata:#?}");
    assert_eq!(metadata[0].conversation_id, old_session);
    assert_eq!(metadata[0].transcript_entries, 0);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `/loop --new` starts the first iteration in a fresh ephemeral
/// conversation with no parent transcript source and honors a per-command
/// loop-limit override.
///
/// New-mode loop attempts must isolate each work iteration from both the
/// current pane conversation and any parent transcript fork while still
/// restoring the parent conversation as the durable pane binding.
#[test]
fn runtime_agent_loop_new_option_starts_first_iteration_in_fresh_ephemeral_conversation() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-loop-new"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service.start_initial_pane_process(Some("cat >/dev/null")).unwrap();
    service
        .pane_screens
        .insert("%1".to_string(), TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap());
    service.agent_shell_store_mut().enter_or_resume("%1").unwrap();
    let old_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: old_session.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "parent-turn".to_string(),
            agent_id: "agent".to_string(),
            pane_id: "%1".to_string(),
            content: "review this document".to_string(),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 1)
        .unwrap();

    let outcome = service
        .execute_agent_shell_loop_command("%1", "/loop --new --limit 3 review this document")
        .unwrap();

    assert!(matches!(outcome, crate::runtime::AgentShellCommandOutcome::Mutated { .. }));
    let loop_state = service.agent_loops_by_pane.get("%1").unwrap();
    assert_eq!(
        loop_state.mode,
        crate::runtime::agent_state::RuntimeAgentLoopMode::NewEachIteration
    );
    assert_eq!(loop_state.max_iterations, 3);
    let loop_session = {
        let session = service.agent_shell_store().get("%1").unwrap();
        assert_ne!(session.session_id, old_session);
        assert!(session.ephemeral);
        assert!(session.ephemeral_transcript_source_conversation_id.is_none());
        assert_eq!(session.ephemeral_transcript_source_entries, 0);
        assert_eq!(session.transcript_entries, 0);
        assert_eq!(session.visibility, AgentShellVisibility::Visible);
        session.session_id.clone()
    };
    assert!(transcript_store.summary(&loop_session).unwrap().is_none());
    let saved = transcript_store.list().unwrap();
    assert!(saved.iter().any(|summary| summary.conversation_id == old_session));
    assert!(!saved.iter().any(|summary| summary.conversation_id == loop_session));
    service.checkpoint_agent_session_metadata().unwrap();
    let metadata = transcript_store
        .load_agent_session_metadata(service.session().id.as_str())
        .unwrap();
    assert_eq!(metadata.len(), 1, "{metadata:#?}");
    assert_eq!(metadata[0].conversation_id, old_session);
    assert_eq!(metadata[0].transcript_entries, 1);
    let pane_text = service.pane_screen("%1").unwrap().visible_lines().join("\n");
    assert!(pane_text.contains("user> /loop --new --limit 3 review this document"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `/loop --limit` rejects a non-positive per-command loop limit.
///
/// A zero iteration budget would prevent `/loop` from running even the first
/// work turn, so the command must fail validation before mutating pane loop
/// state.
#[test]
fn runtime_agent_loop_limit_option_rejects_zero() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-loop-limit-zero"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store);
    service.start_initial_pane_process(Some("cat >/dev/null")).unwrap();
    service
        .pane_screens
        .insert("%1".to_string(), TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap());
    service.agent_shell_store_mut().enter_or_resume("%1").unwrap();

    let error = service
        .execute_agent_shell_loop_command("%1", "/loop --limit 0 review this document")
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("/loop --limit requires a positive integer"),
        "{error}"
    );
    assert!(!service.agent_loops_by_pane.contains_key("%1"));
}

/// Verifies `/loop` schedules another work iteration after a completed turn
/// that emitted an `apply_patch` action.
///
/// The spec keys loop continuation to emitted patch actions, not to the final
/// settled action-result type. Multi-phase `apply_patch` work leaves the turn
/// running until shell settlement, so this regression confirms the controller
/// still observes the original semantic patch action and queues iteration two.
#[test]
fn runtime_agent_loop_continues_after_apply_patch_iteration() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-loop-apply-patch"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service.agent_shell_store_mut().enter_or_resume("%1").unwrap();
    mark_test_pane_ready(&mut service, "%1");
    service.permission_policy_mut().set_approval_bypass(true);
    let old_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: old_session.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "parent-turn".to_string(),
            agent_id: "agent".to_string(),
            pane_id: "%1".to_string(),
            content: "create a note".to_string(),
        })
        .unwrap();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let target_rel = format!(
        "target/mez-loop-apply-patch-{}-{unique}/note.txt",
        std::process::id()
    );
    let target = PathBuf::from(&target_rel);
    fs::create_dir_all(target.parent().unwrap()).unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-loop","method":"agent/shell/command","params":{"idempotency_key":"agent-loop","input":"/loop create a note"}}"#,
        &primary,
    );
    assert!(start.contains(r#""kind":"mutated""#), "{start}");
    assert!(start.contains("state=running"), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap semantic response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "patch-1".to_string(),
                    rationale: "create a file".to_string(),
                    payload: crate::agent::AgentActionPayload::ApplyPatch {
                        patch: format!(
                            "*** Begin Patch\n*** Add File: {target_rel}\n+alpha\n*** End Patch"
                        ),
                        strip: None,
                    },
                }],
                final_turn: false,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    service.pending_agent_provider_tasks.remove("turn-1");

    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Running);
    for _ in 0..100 {
        let _ = service.poll_pane_outputs(8192).unwrap();
        if service.running_shell_transactions.is_empty() {
            break;
        }
        wait_for_pane_process_activity(&service, "%1", Duration::from_millis(10));
    }
    assert!(service.running_shell_transactions.is_empty());
    let completion_provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "done".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(runtime_complete_batch("turn-1")),
            provider_transcript_events: Vec::new(),
        },
    };
    let completions = service
        .poll_agent_provider_tasks_with_provider(&completion_provider, 1)
        .unwrap();
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0].terminal_state, AgentTurnState::Completed);
    assert_eq!(service.agent_loops_by_pane.get("%1").unwrap().iteration, 2);
    assert_eq!(service.agent_loop_turns.get("turn-2").unwrap().iteration, 2);
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-2")
            .map(|turn| turn.state),
        Some(AgentTurnState::Running)
    );
    fs::remove_dir_all(target.parent().unwrap()).unwrap();
}

/// Verifies stopping a `/loop`-owned turn clears the pane loop controller
/// state before the turn finishes interrupted.
///
/// Early stop previously bypassed the normal loop follow-up cleanup, leaving
/// stale `agent_loops_by_pane` and `agent_loop_turns` entries that blocked the
/// next `/loop` command in the same pane.
#[test]
fn runtime_agent_loop_stop_clears_interrupted_loop_state() {
    let transcript_store = AgentTranscriptStore::new(temp_root("runtime-agent-loop-stop"));
    let mut service = test_runtime_service();
    service.set_agent_transcript_store(transcript_store.clone());
    service.start_initial_pane_process(Some("cat >/dev/null")).unwrap();
    service
        .pane_screens
        .insert("%1".to_string(), TerminalScreen::new(Size::new(80, 24).unwrap(), 100).unwrap());
    service.agent_shell_store_mut().enter_or_resume("%1").unwrap();
    let old_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    transcript_store
        .append(&TranscriptEntry {
            conversation_id: old_session.clone(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: TranscriptRole::User,
            turn_id: "parent-turn".to_string(),
            agent_id: "agent".to_string(),
            pane_id: "%1".to_string(),
            content: "review this document".to_string(),
        })
        .unwrap();
    service
        .agent_shell_store_mut()
        .record_transcript_entries("%1", 1)
        .unwrap();

    let outcome = service
        .execute_agent_shell_loop_command("%1", "/loop --fork review this document")
        .unwrap();

    assert!(matches!(outcome, crate::runtime::AgentShellCommandOutcome::Mutated { .. }));
    let loop_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    assert_ne!(loop_session, old_session);
    assert!(service.agent_loops_by_pane.contains_key("%1"));
    assert!(service.agent_loop_turns.contains_key("turn-1"));

    let stopped = service.stop_agent_turn_for_pane("%1").unwrap();

    assert_eq!(stopped.turn_id, "turn-1");
    assert!(!service.agent_loops_by_pane.contains_key("%1"));
    assert!(!service.agent_loop_turns.contains_key("turn-1"));
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(session.session_id, old_session);
    assert!(!session.ephemeral);
    assert!(session.ephemeral_transcript_source_conversation_id.is_none());
    assert_eq!(session.ephemeral_transcript_source_entries, 0);
    assert!(transcript_store.summary(&loop_session).unwrap().is_none());
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .map(|turn| turn.state),
        Some(AgentTurnState::Interrupted)
    );

    let restarted = service
        .execute_agent_shell_loop_command("%1", "/loop review this document")
        .unwrap();

    assert!(matches!(restarted, crate::runtime::AgentShellCommandOutcome::Mutated { .. }));
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that `/clear` follows the spec-level behavior of clearing the live
/// viewport while preserving pane logs and starting a fresh visible
/// conversation.
#[test]
fn runtime_agent_shell_clear_command_resets_conversation_and_terminal_view() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 3).unwrap(), 10).unwrap();
    screen.feed(b"old visible text");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .start_turn("%1", "turn-previous")
        .unwrap();
    service
        .agent_shell_store_mut()
        .finish_turn("%1", "turn-previous")
        .unwrap();
    let old_session = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-clear","method":"agent/shell/command","params":{"idempotency_key":"agent-clear","input":"/clear"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"clear""#), "{response}");
    assert!(response.contains("new=true"), "{response}");
    assert!(
        response.contains("terminal_view_cleared=true"),
        "{response}"
    );
    let session = service.agent_shell_store().get("%1").unwrap();
    assert_ne!(session.session_id, old_session);
    assert_eq!(session.transcript_entries, 0);
    assert_eq!(session.visibility, AgentShellVisibility::Visible);
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .iter()
            .all(|line| line.trim().is_empty()),
        "{:?}",
        service.pane_screen("%1").unwrap().visible_lines()
    );
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("old visible text")
    );
}

/// Verifies that `/status` is backed by live runtime state rather than only
/// the shell session fallback. The status view is a user-visible conformance
/// surface, so it must include model selection, policy, identity, writable
/// scope state, current context tracking, and provider token counters in one
/// response.
#[test]
fn runtime_agent_shell_status_reports_live_runtime_state() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-fast\"]\ndefault_model = \"gpt-fast\"\n\n[permissions]\npreset = \"auto\"\napproval_policy = \"full-access\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let second_pane = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("cat >/dev/null"))
        .unwrap()
        .pane_id;
    service.session.select_pane(&primary, "%1").unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(second_pane.as_str())
        .unwrap();
    service.record_agent_provider_token_usage(
        "%1",
        crate::agent::ModelTokenUsage {
            input_tokens: 120,
            output_tokens: 34,
            reasoning_tokens: 9,
            cached_input_tokens: Some(80),
        },
    );
    service.record_agent_provider_token_usage(
        "%1",
        crate::agent::ModelTokenUsage {
            input_tokens: 40,
            output_tokens: 0,
            reasoning_tokens: 0,
            cached_input_tokens: None,
        },
    );
    let deepseek_profile = runtime_model_profile("deepseek", "deepseek-chat");
    service.record_agent_provider_token_usage_with_profile(
        "%1",
        crate::agent::ModelTokenUsage {
            input_tokens: 200,
            output_tokens: 50,
            reasoning_tokens: 20,
            cached_input_tokens: Some(100),
        },
        crate::agent::ModelTokenUsage {
            input_tokens: 200,
            output_tokens: 50,
            reasoning_tokens: 20,
            cached_input_tokens: Some(100),
        },
        Some(&deepseek_profile),
    );
    service.record_agent_provider_token_usage(
        second_pane.as_str(),
        crate::agent::ModelTokenUsage {
            input_tokens: 60,
            output_tokens: 10,
            reasoning_tokens: 4,
            cached_input_tokens: Some(30),
        },
    );
    service.runtime_metrics.record_provider_token_usage(
        crate::agent::ModelTokenUsage {
            input_tokens: 300,
            output_tokens: 75,
            reasoning_tokens: 15,
            cached_input_tokens: Some(120),
        },
        crate::agent::ModelTokenUsage {
            input_tokens: 300,
            output_tokens: 75,
            reasoning_tokens: 15,
            cached_input_tokens: Some(120),
        },
        &crate::agent::ModelTokenUsageKey::new("runtime-metrics", "metrics-only"),
    );
    service
        .subagent_scopes
        .register(
            "agent-%1",
            CooperationMode::OwnedWrite,
            &["src".to_string()],
            None,
        )
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "summarize the pane")
        .unwrap();
    assert_eq!(started.state, AgentTurnState::Running);

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-status","method":"agent/shell/command","params":{"idempotency_key":"agent-status","input":"/status"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"status""#), "{response}");
    assert!(
        response.contains(r#""content_type":"text/markdown; charset=utf-8""#),
        "{response}"
    );
    assert!(response.contains("## Agent Status"), "{response}");
    assert!(response.contains("| Field | Value |"), "{response}");
    assert!(response.contains("| Agent id | agent-%1 |"), "{response}");
    assert!(response.contains("| Window id | @1 |"), "{response}");
    assert!(
        response.contains("| Model | gpt-fast via openai (profile: default"),
        "{response}"
    );
    assert!(
        response.contains("| Prompt profile | default v26 |"),
        "{response}"
    );
    assert!(
        response.contains("| Permissions | preset auto, approval full-access"),
        "{response}"
    );
    assert!(
        response.contains("| src | agent-%1 | owned-write |"),
        "{response}"
    );
    assert!(response.contains("| Context | 6 blocks"), "{response}");
    assert!(
        response.contains("| Pane agent tokens | 2 models; see Pane Agent Token Usage |"),
        "{response}"
    );
    assert!(
        response.contains("### Pane Agent Token Usage"),
        "{response}"
    );
    let session_heading = response
        .find("### Pane Agent Token Usage")
        .expect("session token usage heading should be present");
    let instance_heading = response
        .find("### Mez Session Token Usage")
        .expect("instance token usage heading should be present");
    assert!(
        session_heading < instance_heading,
        "{response}"
    );
    assert!(
        response.contains("| openai | gpt-fast | 80 | 80 | 34 | 9 | 50.00% |"),
        "{response}"
    );
    assert!(
        response.contains("| deepseek | deepseek-chat | 100 | 100 | 50 | 20 | 50.00% |"),
        "{response}"
    );
    assert!(
        response.contains("| openai | gpt-fast | 110 | 110 | 44 | 13 | 50.00% |"),
        "{response}"
    );
    assert!(!response.contains("| runtime-metrics | metrics-only |"), "{response}");
    assert!(!response.contains("Provider rate limits"), "{response}");
    assert!(!response.contains("### Quota Usage"), "{response}");
    assert!(
        response.contains("| Latest turn | turn-1 (running) |"),
        "{response}"
    );
    assert!(!response.contains("requires_runtime"), "{response}");
}

/// Verifies runtime-wide metrics preserve provider token counters per model.
///
/// Aggregate token counts remain available for operational totals, but the
/// metrics display must also expose provider/model buckets so cost-oriented
/// readers do not have to infer mixed-model usage from one combined counter.
#[test]
fn runtime_show_metrics_reports_provider_tokens_by_model() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.runtime_metrics.record_provider_token_usage(
        crate::agent::ModelTokenUsage {
            input_tokens: 120,
            output_tokens: 34,
            reasoning_tokens: 9,
            cached_input_tokens: Some(80),
        },
        crate::agent::ModelTokenUsage {
            input_tokens: 120,
            output_tokens: 34,
            reasoning_tokens: 9,
            cached_input_tokens: Some(80),
        },
        &crate::agent::ModelTokenUsageKey::new("openai", "gpt-fast"),
    );
    service.runtime_metrics.record_provider_token_usage(
        crate::agent::ModelTokenUsage {
            input_tokens: 200,
            output_tokens: 50,
            reasoning_tokens: 20,
            cached_input_tokens: Some(100),
        },
        crate::agent::ModelTokenUsage {
            input_tokens: 200,
            output_tokens: 50,
            reasoning_tokens: 20,
            cached_input_tokens: Some(100),
        },
        &crate::agent::ModelTokenUsageKey::new("deepseek", "deepseek-chat"),
    );

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"show-metrics","method":"terminal/command","params":{"idempotency_key":"show-metrics","input":"show-metrics"}}"#,
        &primary,
    );

    assert!(
        response.contains("provider_input_tokens = 320"),
        "{response}"
    );
    assert!(
        response.contains("[runtime provider tokens by model]"),
        "{response}"
    );
    assert!(
        response.contains(
            "provider_model_tokens[gpt-fast via openai] = provider=openai model=gpt-fast input=40 cached_input=80 output=34 reasoning=9 cache_hit=66.67% total=154"
        ),
        "{response}"
    );
    assert!(
        response.contains(
            "provider_model_tokens[deepseek-chat via deepseek] = provider=deepseek model=deepseek-chat input=100 cached_input=100 output=50 reasoning=20 cache_hit=50.00% total=250"
        ),
        "{response}"
    );
}

/// Verifies that `/diff` reads the active pane's Git repository and includes
/// both modified tracked content and untracked files. This covers the spec
/// requirement that the agent shell diff view expose the working tree rather
/// than returning a generic runtime-required placeholder.
#[test]
fn runtime_agent_shell_diff_reports_git_worktree_and_untracked_files() {
    let root = temp_root("runtime-agent-diff");
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init"]);
    fs::write(root.join("tracked.txt"), "before\n").unwrap();
    git(&["add", "tracked.txt"]);
    fs::write(root.join("tracked.txt"), "before\nafter\n").unwrap();
    fs::write(root.join("new.txt"), "untracked\n").unwrap();

    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let descriptor = service.initial_pane_descriptor().unwrap();
    service
        .start_pane_process_with_start_directory(descriptor, Some("sleep 30"), Some(&root))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-diff","method":"agent/shell/command","params":{"idempotency_key":"agent-diff","input":"/diff"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(response.contains(r#""command":"diff""#), "{response}");
    assert!(response.contains("source=runtime-vcs-diff"), "{response}");
    assert!(response.contains("untracked_files=1"), "{response}");
    assert!(response.contains("tracked.txt"), "{response}");
    assert!(response.contains("+after"), "{response}");
    assert!(response.contains("file=new.txt"), "{response}");
    assert!(response.contains("+untracked"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    service.kill_session(&primary, true).unwrap();
    let _ = fs::remove_dir_all(root);
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
    service
        .agent_modified_files
        .entry("%1".to_string())
        .or_default()
        .insert(
            "src/lib.rs".to_string(),
            RuntimeAgentModifiedFileSummary {
                path: "src/lib.rs".to_string(),
                added: 12,
                removed: 3,
            },
        );

    let response = service
        .execute_agent_shell_command(&primary, "/list-modified-files")
        .unwrap();

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
}

/// Verifies that `/init` creates a project instruction scaffold in the active
/// pane's working directory and leaves an existing scaffold intact. This covers
/// the baseline file-mutation slash command without writing to the repository
/// root used by the test harness.
#[test]
fn runtime_agent_shell_init_creates_project_instruction_scaffold() {
    let root = temp_root("runtime-agent-init");
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let descriptor = service.initial_pane_descriptor().unwrap();
    service
        .start_pane_process_with_start_directory(descriptor, Some("sleep 30"), Some(&root))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-init","method":"agent/shell/command","params":{"idempotency_key":"agent-init","input":"/init"}}"#,
        &primary,
    );

    let scaffold = root.join("AGENTS.md");
    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"init""#), "{response}");
    assert!(response.contains("created=true"), "{response}");
    assert!(response.contains("source=runtime-init"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    let text = fs::read_to_string(&scaffold).unwrap();
    assert!(text.contains("# Repository Guidelines"), "{text}");
    assert!(
        text.contains("## Build, Test, and Development Commands"),
        "{text}"
    );

    let existing = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-init-existing","method":"agent/shell/command","params":{"idempotency_key":"agent-init-existing","input":"/init"}}"#,
        &primary,
    );

    assert!(existing.contains(r#""kind":"display""#), "{existing}");
    assert!(existing.contains(r#""command":"init""#), "{existing}");
    assert!(existing.contains("created=false"), "{existing}");
    assert!(existing.contains("existing=true"), "{existing}");
    assert!(!existing.contains("requires_runtime"), "{existing}");
    service.kill_session(&primary, true).unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies that `/copy` uses retained model-authored `say` text and supports
/// the same pane, buffer, and clipboard targets as other copy commands.
///
/// The raw provider response can contain transport or protocol scaffolding, so
/// the command must copy the latest explicit `say.text` rather than raw model
/// text or an action-summary substitute.
#[test]
fn runtime_agent_shell_copy_writes_latest_say_text_to_destinations() {
    let _clipboard_guard = TEST_HOST_CLIPBOARD_TEST_LOCK.lock().unwrap();
    TEST_HOST_CLIPBOARD_WRITES.lock().unwrap().clear();
    let mut service = test_runtime_service();
    service.host_clipboard =
        HostClipboard::new(record_host_clipboard_copy, empty_host_clipboard_read);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "produce final answer")
        .unwrap();
    assert_eq!(started.state, AgentTurnState::Running);
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "raw transport envelope should not be copied".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![
                    crate::agent::AgentAction {
                        id: "say-1".to_string(),
                        rationale: "give an earlier answer".to_string(),
                        payload: crate::agent::AgentActionPayload::Say {
                            status: crate::agent::SayStatus::Final,
                            text: "Earlier say text.".to_string(),
                            content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                    crate::agent::AgentAction {
                        id: "say-2".to_string(),
                        rationale: "give the answer that should be copied".to_string(),
                        payload: crate::agent::AgentActionPayload::Say {
                            status: crate::agent::SayStatus::Final,
                            text: "Latest say text.".to_string(),
                            content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                                .to_string(),
                        },
                    },
                ],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            ModelProfile {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                reasoning_profile: None,
                latency_preference: None,
                multimodal_required: false,
                provider_options: std::collections::BTreeMap::new(),
                safety_tier: None,
            },
        )
        .unwrap();
    assert_eq!(execution.terminal_state, AgentTurnState::Completed);

    let buffer_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-copy-buffer","method":"agent/shell/command","params":{"idempotency_key":"agent-copy-buffer","input":"/copy buffer retained-say"}}"#,
        &primary,
    );

    assert!(
        buffer_response.contains(r#""kind":"mutated""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains(r#""command":"copy""#),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains("destination=buffer"),
        "{buffer_response}"
    );
    assert!(
        buffer_response.contains("source=runtime-agent-say"),
        "{buffer_response}"
    );
    assert_eq!(
        service.paste_buffers.get("retained-say"),
        Some("Latest say text.")
    );
    assert_ne!(
        service.paste_buffers.get("retained-say"),
        Some("raw transport envelope should not be copied")
    );
    let buffers = service.paste_buffers.list();
    assert!(
        buffers.iter().any(|buffer| {
            buffer.name == "retained-say" && buffer.origin.as_deref() == Some("agent:turn-1:say")
        }),
        "{buffers:?}"
    );

    let clipboard_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-copy-clipboard","method":"agent/shell/command","params":{"idempotency_key":"agent-copy-clipboard","input":"/copy clipboard"}}"#,
        &primary,
    );
    assert!(
        clipboard_response.contains("destination=clipboard"),
        "{clipboard_response}"
    );
    assert_eq!(
        service.paste_buffers.get("clipboard"),
        Some("Latest say text.")
    );
    assert!(
        TEST_HOST_CLIPBOARD_WRITES
            .lock()
            .unwrap()
            .last()
            .is_some_and(|text| text == "Latest say text.")
    );

    service.pane_screens.insert(
        "%1".to_string(),
        TerminalScreen::new(Size::new(80, 6).unwrap(), 20).unwrap(),
    );
    let pane_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-copy-pane","method":"agent/shell/command","params":{"idempotency_key":"agent-copy-pane","input":"/copy"}}"#,
        &primary,
    );
    assert!(
        pane_response.contains("destination=pane"),
        "{pane_response}"
    );
    let pane_text_after = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text_after.contains("Latest say text."),
        "{pane_text_after}"
    );
}

/// Verifies that `/logout` executes through the runtime auth store and removes
/// stored credentials without exposing a duplicate terminal logout command.
#[test]
fn runtime_agent_shell_logout_uses_attached_auth_store() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-agent-logout");
    let auth_store = AuthStore::new(crate::auth::AuthPaths::under_config_root(&root));
    auth_store
        .login_provider_api_key_with_selected_store("openai", "work", "sk-runtime-secret", Some("file"))
        .unwrap();
    service.set_auth_store(auth_store);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-logout","method":"agent/shell/command","params":{"idempotency_key":"agent-logout","input":"/logout"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"logout""#), "{response}");
    assert!(response.contains("logged_out=true"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    assert!(!response.contains("sk-runtime-secret"), "{response}");
    let status = service
        .execute_terminal_command(&primary, "auth-status")
        .unwrap();
    assert!(status.contains("authenticated=false"), "{status}");
    let _ = fs::remove_dir_all(root);
}

/// Verifies that `/approval` arguments are applied through the live runtime
/// approval-mode command path. The no-argument slash command already displays
/// policy state; this covers mutation through the agent shell surface.
#[test]
fn runtime_agent_shell_approval_command_mutates_live_policy() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-permissions","method":"agent/shell/command","params":{"idempotency_key":"agent-permissions","input":"/approval full-access"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"approval""#), "{response}");
    assert!(response.contains("field=approval_policy"), "{response}");
    assert!(response.contains("requested=full-access"), "{response}");
    assert!(response.contains("changed=true"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
}

/// Verifies terse slash-command display output uses transient status feedback.
///
/// One-line status acknowledgements should stay out of the durable agent pane
/// transcript while still giving brief feedback in the window status bar.
#[test]
fn runtime_agent_shell_single_line_display_uses_transient_status_without_overlay() {
    let mut service = test_runtime_service();
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
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"/approval\r".to_vec(),
                )],
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
    assert!(service.primary_display_overlay.is_none());
    assert!(
        service
            .primary_error_status_overlay
            .as_deref()
            .is_some_and(|message| message.contains("approval policy: ask")),
        "{:?}",
        service.primary_error_status_overlay
    );
    let pane_text = service
        .pane_screen("%1")
        .map(|screen| screen.normal_content_lines().join("\n"))
        .unwrap_or_default();
    assert!(!pane_text.contains("approval policy: ask"), "{pane_text}");
    assert!(!pane_text.contains("source: runtime-policy"), "{pane_text}");
}

/// Verifies an explicit `/approval` choice is stored as a live override and
/// therefore survives unrelated configuration reloads from disk.
///
/// This protects full-access mode from being silently reset when a config
/// reload reapplies an older `permissions.approval_policy` value.
#[test]
fn runtime_agent_shell_approval_command_survives_config_reload() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-approval-live-override");
    let path = root.join("config.toml");
    fs::write(
        &path,
        "[history]\nlines = 7\n[permissions]\napproval_policy = \"ask\"\n",
    )
    .unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&path).unwrap(),
        }])
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-approval","method":"agent/shell/command","params":{"idempotency_key":"agent-approval-live","input":"/approval full-access"}}"#,
        &primary,
    );

    assert!(response.contains("requested=full-access"), "{response}");
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );

    fs::write(
        &path,
        "[history]\nlines = 11\n[permissions]\napproval_policy = \"ask\"\n",
    )
    .unwrap();
    let reload = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload-approval","method":"config/reload","params":{"idempotency_key":"reload-approval-live"}}"#,
        &primary,
    );

    assert!(reload.contains(r#""operation":"reload""#), "{reload}");
    assert_eq!(service.terminal_history_limit(), 11);
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies that `/statusline` mutates the live pane status-line rendering
/// fields. The command should configure existing frame state instead of
/// returning a runtime-required slash placeholder.
#[test]
fn runtime_agent_shell_statusline_configures_pane_frame_fields() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-statusline","method":"agent/shell/command","params":{"idempotency_key":"agent-statusline","input":"/statusline agent.status agent.model pane.mode"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"mutated""#), "{response}");
    assert!(response.contains(r#""command":"statusline""#), "{response}");
    assert!(response.contains("enabled=true"), "{response}");
    assert!(response.contains("agent.status"), "{response}");
    assert!(response.contains("agent.model"), "{response}");
    assert!(response.contains("pane.mode"), "{response}");
    assert!(response.contains("changed=true"), "{response}");
    assert!(response.contains("source=runtime-statusline"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
    assert!(service.pane_frames_enabled);
    assert_eq!(
        service.pane_frame_visible_fields,
        vec![
            "agent.status".to_string(),
            "agent.model".to_string(),
            "pane.mode".to_string()
        ]
    );
    assert_eq!(
        service.pane_frame_template,
        "#{agent.status} #{agent.model} #{pane.mode}"
    );
}

/// Verifies that `/title` reads and mutates the active runtime window title
/// through the live command path. This covers the agent shell title command
/// without allowing the slash surface to target or rename unrelated windows.
#[test]
fn runtime_agent_shell_title_displays_and_renames_active_window() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let display = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-title-display","method":"agent/shell/command","params":{"idempotency_key":"agent-title-display","input":"/title"}}"#,
        &primary,
    );

    assert!(display.contains(r#""kind":"display""#), "{display}");
    assert!(display.contains(r#""command":"title""#), "{display}");
    assert!(display.contains("source=runtime-title"), "{display}");
    assert!(display.contains("window_id=@1"), "{display}");
    assert!(display.contains("window_title=shell"), "{display}");
    assert!(display.contains("pane=%1"), "{display}");
    assert!(display.contains("pane_title=shell"), "{display}");
    assert!(!display.contains("requires_runtime"), "{display}");

    let rename = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-title-rename","method":"agent/shell/command","params":{"idempotency_key":"agent-title-rename","input":"/title build shell"}}"#,
        &primary,
    );

    assert!(rename.contains(r#""kind":"mutated""#), "{rename}");
    assert!(rename.contains(r#""command":"title""#), "{rename}");
    assert!(rename.contains("source=runtime-title"), "{rename}");
    assert!(rename.contains("changed=true"), "{rename}");
    assert!(rename.contains("window_title=build shell"), "{rename}");
    assert!(!rename.contains("requires_runtime"), "{rename}");
    assert_eq!(
        service.session().active_window().unwrap().name,
        "build shell"
    );
}

/// Verifies that `/debug-config` reports live effective configuration, layer
/// order, and policy diagnostics from runtime state instead of the generic
/// runtime-required slash placeholder.
#[test]
fn runtime_agent_shell_debug_config_reports_live_runtime_config() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 7\n[permissions]\npreset = \"auto\"\napproval_policy = \"full-access\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"debug-config","method":"agent/shell/command","params":{"idempotency_key":"debug-config","input":"/debug-config history.lines"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"display""#), "{response}");
    assert!(
        response.contains(r#""command":"debug-config""#),
        "{response}"
    );
    assert!(response.contains("source=runtime-config"), "{response}");
    assert!(response.contains("layers=1"), "{response}");
    assert!(response.contains("applied_layers=1"), "{response}");
    assert!(response.contains("permission_preset=auto"), "{response}");
    assert!(
        response.contains("approval_policy=full-access"),
        "{response}"
    );
    assert!(response.contains("layer=primary"), "{response}");
    assert!(response.contains("scope=primary"), "{response}");
    assert!(response.contains("format=toml"), "{response}");
    assert!(response.contains("value path=history.lines"), "{response}");
    assert!(response.contains("value=7"), "{response}");
    assert!(!response.contains("requires_runtime"), "{response}");
}

/// Verifies runtime agent shell prompt starts live turn lifecycle.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_agent_shell_prompt_starts_live_turn_lifecycle() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt","input":"summarize the pane"}}"#,
        &primary,
    );

    assert!(response.contains(r#""kind":"turn_started""#), "{response}");
    assert!(response.contains(r#""command":null"#), "{response}");
    assert!(response.contains(r#""body":null"#), "{response}");
    assert!(response.contains(r#""state":"running""#), "{response}");
    let response_json: serde_json::Value = serde_json::from_str(&response).unwrap();
    let turn = &response_json["result"]["turn"];
    assert_eq!(turn["id"], "turn-1", "{response}");
    assert_eq!(turn["version"], serde_json::json!(1), "{response}");
    assert_eq!(turn["agent_id"], "agent-%1", "{response}");
    assert_eq!(turn["state"], "running", "{response}");
    assert!(turn["created_at"].as_str().is_some(), "{response}");
    assert!(turn["started_at"].as_str().is_some(), "{response}");
    assert_eq!(turn["finished_at"], serde_json::Value::Null, "{response}");
    assert_eq!(turn["prompt_preview"], "summarize the pane", "{response}");
    assert_eq!(turn["approval_ids"], serde_json::json!([]), "{response}");
    assert_eq!(
        turn["result_summary"],
        serde_json::Value::Null,
        "{response}"
    );
    assert!(
        turn["extensions"]["context_blocks"].as_u64().is_some(),
        "{response}"
    );
    let tasks = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"tasks","method":"agent/task/list","params":{"target":{"pane_id":"%1"}}}"#,
        &primary,
    );
    assert!(tasks.contains(r#""id":"turn-1""#), "{tasks}");
    assert!(tasks.contains(r#""state":"running""#), "{tasks}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].turn_id, "turn-1");
    assert_eq!(pending[0].model_profile.provider, "openai");
    assert_eq!(pending[0].model_profile.model, "gpt-5.5");
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("agent: working on"), "{pane_text}");
}

/// Verifies that a user prompt and a non-command agent response are written
/// into the pane's normal terminal buffer instead of a transient prompt
/// overlay. This preserves the Codex-like interaction transcript as copyable
/// terminal text while still retaining terminal style spans for user-facing
/// color. Each injected line keeps the same Mezzanine UI prefix used by the
/// pane-local prompt so message boundaries are visible in the terminal buffer.
#[test]
fn runtime_agent_prompt_and_say_response_are_interleaved_in_pane_buffer() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(80, 12).unwrap(), 100).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-visible-say","input":"summarize visible output"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "maap say response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: "answer in the pane".to_string(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Final,
                        text: "The pane is ready.".to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };
    let execution = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    assert_eq!(execution.terminal_state, AgentTurnState::Completed);
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("user> summarize visible output"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("▐ user> summarize visible output"),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("mez> The pane is ready."),
        "{pane_text}"
    );
    assert!(
        pane_text.contains("▐ mez> The pane is ready."),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("mez> answer in the pane"),
        "{pane_text}"
    );
    assert!(!pane_text.contains("agent: turn turn-1"), "{pane_text}");
    let theme = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap()
        .ui_theme;
    let assistant_line = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("mez> The pane is ready."))
        .unwrap();
    assert!(assistant_line.text.starts_with("▐ "));
    assert!(!assistant_line.style_spans.is_empty());
    let assistant_body_start = "▐ mez> ".chars().count();
    assert!(
        assistant_line
            .style_spans
            .iter()
            .all(|span| span.start.saturating_add(span.length) <= assistant_body_start),
        "assistant body text should use default terminal color: {:?}",
        assistant_line.style_spans
    );
    assert!(
        assistant_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground
                    == Some(theme.colors.agent_transcript_assistant.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "assistant gutter and label should use themed foreground without a background: {:?}",
        assistant_line.style_spans
    );
    let user_line = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines()
        .into_iter()
        .find(|line| line.text.contains("user> summarize visible output"))
        .unwrap();
    let user_body_start = "▐ user> ".chars().count();
    assert!(
        user_line
            .style_spans
            .iter()
            .all(|span| span.start.saturating_add(span.length) <= user_body_start),
        "user prompt body text should use default terminal color: {:?}",
        user_line.style_spans
    );
    assert!(
        user_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground == Some(theme.colors.agent_transcript_user.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "user gutter and label should use themed foreground without a background: {:?}",
        user_line.style_spans
    );
    service
        .append_agent_error_text_to_terminal_buffer("%1", "agent error: failed")
        .unwrap();
    service
        .append_agent_command_preview_to_terminal_buffer("%1", "ls -la")
        .unwrap();
    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let error_line = styled_lines
        .iter()
        .find(|line| line.text.contains("agent error: failed"))
        .unwrap();
    assert!(
        error_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground == Some(theme.colors.agent_transcript_error.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "error transcript lines should use themed error foreground without a background: {:?}",
        error_line.style_spans
    );
    let command_line = styled_lines
        .iter()
        .find(|line| line.text.contains("$ ls -la"))
        .unwrap();
    assert!(
        command_line.style_spans.iter().any(|span| {
            span.start == 0
                && span.rendition.foreground
                    == Some(theme.colors.agent_transcript_command.foreground)
                && span.rendition.background.is_none()
                && span.rendition.bold
        }),
        "command transcript lines should use themed command foreground without a background: {:?}",
        command_line.style_spans
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies visible-pane user prompt transcript lines wrap to the bounded pane
/// width with a sixth-column hanging indent for continuation rows.
///
/// Long user-entered transcript lines should use the same bounded renderer as
/// other visible pane logs so they stay within the pane width or the 120-column
/// cap. Wrapped continuation rows align with the `mez> ` continuation column
/// instead of repeating the `user> ` label so the copied transcript remains
/// readable.
#[test]
fn runtime_user_prompt_logs_wrap_with_sixth_column_hanging_indent() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(24, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(24, 12).unwrap(), 100).unwrap();
    screen.feed(b"ready\n");
    service.pane_screens.insert("%1".to_string(), screen);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    service
        .append_agent_user_prompt_to_terminal_buffer("%1", "alpha beta gamma delta epsilon")
        .unwrap();

    let user_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .into_iter()
        .filter(|line| line.starts_with("▐ "))
        .collect::<Vec<_>>();
    assert!(
        user_lines.iter().any(|line| line == "▐ user> alpha beta"),
        "{user_lines:#?}"
    );
    assert!(
        user_lines.iter().any(|line| line == "▐      gamma delta"),
        "{user_lines:#?}"
    );
    assert!(
        user_lines.iter().any(|line| line == "▐      epsilon"),
        "{user_lines:#?}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies plain text `say` output does not receive markdown block framing.
///
/// Plain `say` output is ordinary assistant transcript text, so it should keep
/// the `mez> ` speaker prefix while avoiding the synthetic markdown divider
/// row that is reserved for `text/markdown` presentation blocks.
#[test]
fn runtime_agent_plain_say_does_not_render_markdown_divider() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-plain","method":"agent/shell/command","params":{"idempotency_key":"agent-plain-say","input":"render plain text"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let plain = "Plain say output without markdown framing.";
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "plain say response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: String::new(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Final,
                        text: plain.to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    assert!(
        styled_lines
            .iter()
            .any(|line| line.text.contains("mez> Plain say output without markdown framing.")),
        "{styled_lines:?}"
    );
    let expected_divider = expected_markdown_block_divider_line(80);
    assert!(
        styled_lines
            .iter()
            .all(|line| line.text != expected_divider),
        "{styled_lines:?}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies markdown `say` output is rendered as presentation-only styling.
///
/// The display path should remove visual markdown delimiters and add terminal
/// style spans for readability, while copy mode must still return the raw
/// markdown authored by the model. This protects markdown as the first
/// content-type renderer without hard-coding future media types into copy mode.
#[test]
fn runtime_agent_markdown_say_renders_styled_presentation_and_copies_raw_markdown() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-markdown","method":"agent/shell/command","params":{"idempotency_key":"agent-markdown-say","input":"render markdown"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    let markdown = "**Important** and <u>underlined</u>\n- first";
    let provider = RuntimeBatchProvider {
        response: crate::agent::ModelResponse {
            provider: "runtime-batch".to_string(),
            model: "test".to_string(),
            raw_text: "markdown say response".to_string(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Default::default(),
            action_batch: Some(crate::agent::MaapBatch {
                protocol: "maap/1".to_string(),
                rationale: "test action batch rationale".to_string(),
                thought: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                actions: vec![crate::agent::AgentAction {
                    id: "say-1".to_string(),
                    rationale: String::new(),
                    payload: crate::agent::AgentActionPayload::Say {
                        status: crate::agent::SayStatus::Final,
                        text: markdown.to_string(),
                        content_type: crate::agent::AGENT_OUTPUT_TEXT_MARKDOWN_CONTENT_TYPE
                            .to_string(),
                    },
                }],
                final_turn: true,
            }),
            provider_transcript_events: Vec::new(),
        },
    };

    service
        .execute_agent_turn_with_provider(
            "turn-1",
            &provider,
            runtime_model_profile("runtime-batch", "test"),
        )
        .unwrap();

    let styled_lines = service
        .pane_screen("%1")
        .unwrap()
        .normal_styled_content_lines();
    let assistant_line = styled_lines
        .iter()
        .find(|line| line.text.contains("mez> Important and underlined"))
        .unwrap();
    let assistant_index = styled_lines
        .iter()
        .position(|line| line.text == assistant_line.text)
        .unwrap();
    let expected_divider = expected_markdown_block_divider_line(80);
    assert!(
        assistant_index == 0 || styled_lines[assistant_index - 1].text != expected_divider,
        "{styled_lines:?}"
    );
    assert!(
        !assistant_line.text.contains("**") && !assistant_line.text.contains("<u>"),
        "{assistant_line:?}"
    );
    assert!(
        assistant_line
            .style_spans
            .iter()
            .any(|span| span.rendition.bold && span.start >= "▐ mez> ".chars().count()),
        "{assistant_line:?}"
    );
    assert!(
        assistant_line
            .style_spans
            .iter()
            .any(|span| span.rendition.underline && span.start >= "▐ mez> ".chars().count()),
        "{assistant_line:?}"
    );
    assert!(
        styled_lines
            .iter()
            .any(|line| line.text.contains("• first")),
        "{styled_lines:?}"
    );
    assert!(
        styled_lines
            .iter()
            .all(|line| {
                line.text != expected_divider
                    && !line.text.contains("mez> ---------")
                    && !line.text.contains("mez> ─")
            }),
        "{styled_lines:?}"
    );

    let copy_mode = service.ensure_active_copy_mode("%1").unwrap();
    let scroll_top = copy_mode.scroll_top();
    let visible_lines = copy_mode.visible_lines();
    assert!(
        visible_lines
            .iter()
            .all(|line| {
                line != &expected_divider
                    && !line.contains("mez> ---------")
                    && !line.contains("mez> ─")
            }),
        "{visible_lines:?}"
    );
    let first_line = visible_lines
        .iter()
        .position(|line| line.contains("mez> Important and underlined"))
        .map(|line| line + scroll_top)
        .unwrap();
    let second_line = visible_lines
        .iter()
        .position(|line| line.contains("• first"))
        .map(|line| line + scroll_top)
        .unwrap();
    let second_column = visible_lines[second_line.saturating_sub(scroll_top)]
        .chars()
        .count();
    copy_mode
        .select_range(
            CopyPosition {
                line: first_line,
                column: 0,
            },
            CopyPosition {
                line: second_line,
                column: second_column,
            },
        )
        .unwrap();

    assert_eq!(copy_mode.copy_selection().unwrap(), markdown);
    service.pane_processes_mut().terminate_all().unwrap();
}
