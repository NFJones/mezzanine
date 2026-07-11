//! Runtime tests for agent prompt editing behavior.

use super::*;

/// Verifies that the initial primary attach applies the attached terminal size
/// to existing window geometry. The first pane is created at bootstrap size
/// before a client is attached, so agent prompt rendering must depend on the
/// post-attach resize path instead of stale default geometry.
#[test]
fn runtime_primary_attach_resizes_initial_window_for_agent_prompt() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(120, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    assert_eq!(
        service.session().active_window().unwrap().size,
        Size::new(120, 40).unwrap()
    );

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(120, 40).unwrap(),
            &config,
        )
        .unwrap()
        .unwrap();
    let region = view.agent_prompt_region.unwrap();

    assert_eq!(view.lines.len(), 40);
    assert_eq!(view.authoritative_size, Size::new(120, 40).unwrap());
    assert_eq!(region.columns, 120);
    assert_eq!(region.rows, 38);
    assert!(
        view.cursor_row >= 38,
        "agent prompt cursor should render at attached terminal bottom: {view:?}"
    );
}

/// Verifies that shell prompt bytes arriving after a hidden Mezzanine-owned
/// shell transaction is removed are still suppressed for a short retention
/// window. This covers shells that repaint PS1 in a later PTY read after the
/// transaction marker has already settled the action.
#[test]
fn runtime_hidden_agent_shell_rendering_retains_prompt_suppression_after_transaction() {
    let mut service = test_runtime_service();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.running_shell_transactions.insert(
        "marker-1".to_string(),
        RunningShellTransactionRef {
            turn_id: "turn-1".to_string(),
            kind: RunningShellTransactionKind::AgentAction {
                action_id: "a1".to_string(),
            },
            pane_id: "%1".to_string(),
            command: "ls".to_string(),
            started_at_unix_ms: 0,
            timeout_ms: None,
            pending_input_payload: None,
            observed_output_bytes: 0,
            observed_output_preview: String::new(),
            observed_output_truncated: false,
        },
    );

    let command_output = service.renderable_pane_output_bytes("%1", b"file-a\n");
    service.running_shell_transactions.remove("marker-1");
    let prompt_repaint = service.renderable_pane_output_bytes("%1", b"user@host ~/repo $ ");
    let mut aged = 0usize;
    for _ in 0..64 {
        aged = aged.saturating_add(
            service
                .apply_idle_cleanup_timer_event_with_actor_progress(
                    &std::collections::BTreeSet::new(),
                )
                .unwrap(),
        );
    }
    let later_shell_output = service.renderable_pane_output_bytes("%1", b"later\n");

    assert!(command_output.is_empty());
    assert!(prompt_repaint.is_empty());
    assert!(aged > 0);
    assert_eq!(later_shell_output, b"later\n");
}

/// Verifies runtime config reload applies custom system prompts and default
/// personality profiles.
///
/// These values are intentionally runtime-owned preferences: configured system
/// prompt text must enter the provider request as system context, while a
/// default personality profile can supply response-style and planning guidance
/// without requiring a user to run `/personality` in every pane.
#[test]
fn runtime_config_reload_applies_agent_prompt_and_personality_profiles() {
    let mut service = test_runtime_service();
    let root = temp_root("runtime-agent-personality-config");
    let path = root.join("config.toml");
    fs::write(
        &path,
        "[agents]\ncustom_system_prompt = \"Always preserve user work.\"\ndefault_personality = \"careful\"\n[personalities.careful]\nname = \"Careful\"\nsystem_prompt = \"Be exact about evidence.\"\nresponse_style = \"terse\"\nplanning_enabled = true\nrouting_enabled = true\n",
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

    assert_eq!(
        service.custom_agent_system_prompt.as_deref(),
        Some("Always preserve user work.")
    );
    assert_eq!(
        service.default_agent_personality.as_deref(),
        Some("careful")
    );
    assert_eq!(service.agent_personality_profiles.len(), 1);

    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let started = service
        .start_agent_prompt_turn("%1", "summarize the change")
        .unwrap();
    let context = service
        .agent_turn_contexts
        .get(&started.turn_id)
        .expect("started turn should retain provider context");
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::System
            && block.label == "configured agent system prompt"
            && block.content.contains("Always preserve user work")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.source == ContextSourceKind::System
            && block.label == "agent personality system prompt"
            && block.content.contains("Be exact about evidence")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.label == "agent shell plan mode" && block.content.contains("Planning mode is active")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.label == "agent shell plan mode"
            && block
                .content
                .contains("Do not use a visible plan when the next safe inspection")
    }));
    assert!(!context.blocks.iter().any(|block| {
        block.label == "agent shell plan mode"
            && block.content.contains("Start by presenting a concise")
    }));
    assert!(context.blocks.iter().any(|block| {
        block.label == "agent shell personality"
            && block.content.contains("Response style preference")
            && block.content.contains("terse")
    }));

    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies that live prompt submission drains scheduler capacity through the
/// same fairness policy as the scheduler queue. A blocked same-pane turn at
/// the head of the queue must not prevent a later prompt for an independent
/// pane from starting when the global concurrency limit still has capacity.
#[test]
fn runtime_prompt_submission_starts_ready_work_behind_blocked_queue_head() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(2)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let second_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    service.session.select_pane(&primary, "%1").unwrap();
    for pane_id in ["%1", second_pane.as_str()] {
        let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
        screen.feed(b"ready\n");
        service.pane_screens.insert(pane_id.to_string(), screen);
        service
            .agent_shell_store_mut()
            .enter_or_resume(pane_id)
            .unwrap();
    }

    let first = service.start_agent_prompt_turn("%1", "first").unwrap();
    let blocked_same_pane = service.start_agent_prompt_turn("%1", "second").unwrap();
    let independent = service
        .start_agent_prompt_turn(second_pane.as_str(), "third")
        .unwrap();

    assert_eq!(first.state, AgentTurnState::Running);
    assert_eq!(blocked_same_pane.state, AgentTurnState::Queued);
    assert_eq!(independent.state, AgentTurnState::Running);
    assert_eq!(service.agent_scheduler().snapshot().running, 2);
    assert_eq!(service.agent_scheduler().snapshot().queued, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        Some("turn-1")
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get(second_pane.as_str())
            .and_then(|session| session.running_turn_id.as_deref()),
        Some("turn-3")
    );
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-2")
            .map(|turn| turn.state),
        Some(AgentTurnState::Queued)
    );
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-3")
            .map(|turn| turn.state),
        Some(AgentTurnState::Running)
    );
    let pending = service.pending_agent_provider_tasks();
    assert!(pending.iter().any(|task| task.turn_id == "turn-1"));
    assert!(pending.iter().any(|task| task.turn_id == "turn-3"));
    assert!(!pending.iter().any(|task| task.turn_id == "turn-2"));
    service.kill_session(&primary, true).unwrap();
}

/// Verifies pane-local agent shell prompt rows remain inside mouse ownership.
///
/// The agent prompt is rendered as part of the pane content even though copy-mode
/// overlay rows reserve less height above it. Mouse drag selection must keep the
/// agent shell active when the pointer reaches the prompt rows instead of
/// falling through to the underlying pane.
#[test]
fn runtime_mouse_pane_regions_include_agent_prompt_rows() {
    let mut service = test_runtime_service();
    service
        .attach_primary("primary", true, Size::new(30, 4).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let view = service
        .render_client_view(ClientViewRole::Primary, Size::new(30, 4).unwrap(), &config)
        .unwrap()
        .unwrap();
    let prompt_region = view.agent_prompt_region.unwrap();
    let prompt_row = u16::try_from(
        prompt_region
            .row
            .saturating_add(prompt_region.rows.saturating_sub(1)),
    )
    .unwrap();
    let region = config
        .mouse_pane_regions
        .iter()
        .find(|region| region.pane_id == "%1")
        .unwrap();

    assert!(
        region.contains(u16::try_from(prompt_region.column).unwrap(), prompt_row),
        "agent prompt row should remain inside pane mouse ownership: {region:?}"
    );
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
        prompt_state
            .prompt
            .buffer
            .set_line("alpha beta gamma delta");
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
        .render_client_view(ClientViewRole::Primary, Size::new(24, 8).unwrap(), &config)
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
        prompt_state
            .prompt
            .buffer
            .set_line("alpha beta gamma delta");
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
        .take_running_pane_process_for_adapter(&pane_id)
        .unwrap();

    let show = service
        .execute_terminal_command(&primary, "agent-shell")
        .unwrap();
    assert!(show.contains("visibility=visible"), "{show}");
    let enter_effects = service.drain_pane_io_transition().side_effects;
    assert_eq!(pane_input_effects(&enter_effects).len(), 1);
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
    let exit_effects = service.drain_pane_io_transition().side_effects;
    let exit_inputs = pane_input_effects(&exit_effects);
    assert_eq!(exit_inputs.len(), 1);
    assert_eq!(exit_inputs[0].pane_input_parts().0, pane_id);
    assert_eq!(exit_inputs[0].pane_input_parts().1, b"\x04");

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

/// Verifies Ctrl+C is idempotent when the tracked turn already terminalized.
///
/// Macro failure can mark a turn as failed while the pane-local shell session
/// still carries the turn id during unwind. Ctrl+C should clear that stale
/// binding through the stop path without trying to reclassify the ledger turn
/// as interrupted and surfacing an already-terminal conflict.
#[test]
fn runtime_agent_prompt_ctrl_c_after_failed_turn_is_idempotent() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service.start_agent_prompt_turn("%1", "run macro").unwrap();
    let turn_id = started.turn_id.clone();
    let _ = service.agent_scheduler.complete(&turn_id);
    service
        .agent_turn_ledger
        .finish_turn(&turn_id, AgentTurnState::Failed)
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"\x03".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Default::default(),
            },
        )
        .unwrap();

    assert_eq!(report.agent_prompt_inputs_applied, 1);
    assert_eq!(
        service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .map(|turn| turn.state),
        Some(AgentTurnState::Failed)
    );
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

/// Verifies that `/personality` mutates live pane-scoped agent preferences and
/// that those preferences are appended to the next prompt context. This makes
/// the slash command affect provider input instead of only acknowledging a
/// runtime placeholder.
#[test]
fn runtime_agent_shell_personality_feeds_prompt_context() {
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

    let personality = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"personality","method":"agent/shell/command","params":{"idempotency_key":"personality","input":"/personality concise"}}"#,
        &primary,
    );
    assert!(personality.contains(r#""kind":"mutated""#), "{personality}");
    assert!(
        personality.contains(r#""command":"personality""#),
        "{personality}"
    );
    assert!(personality.contains("style=concise"), "{personality}");
    assert!(
        personality.contains("source=runtime-personality"),
        "{personality}"
    );
    assert!(!personality.contains("requires_runtime"), "{personality}");

    let prompt = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"preference-prompt","method":"agent/shell/command","params":{"idempotency_key":"preference-prompt","input":"prepare work"}}"#,
        &primary,
    );
    assert!(prompt.contains(r#""state":"running""#), "{prompt}");
    let context = service.agent_turn_contexts.get("turn-1").unwrap();
    assert!(
        context
            .blocks
            .iter()
            .any(|block| block.label == "agent shell personality"
                && block.content.contains("concise"))
    );
}
