//! Agent prompt editing layout tests.

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
    service.running_shell_transactions_mut_for_tests().insert(
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
    service
        .running_shell_transactions_mut_for_tests()
        .remove("marker-1");
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
        .agent_turn_contexts()
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
        service.set_pane_screen(pane_id.to_string(), screen);
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
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-2")
            .map(|turn| turn.state),
        Some(AgentTurnState::Queued)
    );
    assert_eq!(
        service
            .agent_turn_ledger()
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
    assert_eq!(service.pane_screen("%1").unwrap().size(), agent_size);

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
    assert_eq!(service.pane_screen("%1").unwrap().size(), initial_size);
    service.terminate_all_pane_processes().unwrap();
}
