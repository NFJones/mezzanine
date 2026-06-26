/// Verifies runtime approval disapproval focuses blocked agent pane.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_approval_disapproval_focuses_blocked_agent_pane() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let blocked_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    service.session.select_pane(&primary, "%1").unwrap();
    service
        .agent_shell_store_mut()
        .ensure_session(blocked_pane.as_str())
        .unwrap();
    let approval_id = service
        .queue_blocked_approval(BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: format!("agent-{blocked_pane}"),
            pane_id: blocked_pane.to_string(),
            parent_agent_chain: vec![format!("agent-{blocked_pane}")],
            action_kind: "shell_command".to_string(),
            action_summary: "env".to_string(),
            declared_effects: vec!["approval required".to_string()],
            matched_rules: vec!["runtime.agent_action_blocked".to_string()],
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: crate::permissions::BlockedApprovalState::Pending,
            decision: None,
            redirect_instruction: None,
        })
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"deny","method":"approval/decide","params":{{"approval_id":"{}","decision":"disapprove","idempotency_key":"deny-blocked-agent"}}}}"#,
            approval_id
        ),
        &primary,
    );

    assert!(response.contains(r#""state":"disapproved""#), "{response}");
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .id
            .as_str(),
        blocked_pane.as_str()
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get(blocked_pane.as_str())
            .map(|session| session.visibility),
        Some(AgentShellVisibility::Visible)
    );
}

/// Verifies runtime applies permission and mcp state from config layers.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_applies_permission_and_mcp_state_from_config_layers() {
    let mut service = test_runtime_service();
    let report = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[permissions]\napproval_policy = \"full-access\"\nbypass_mode = false\n[[permissions.command_rules]]\npattern = [\"cargo\", \"test\"]\ndecision = \"allow\"\nscope = \"session\"\nmatch = \"prefix\"\n[mcp_servers.fs]\nname = \"filesystem\"\ncommand = \"mcp-fs\"\nargs = [\"--root\", \".\"]\nenv_vars = [\"MEZ_TEST_MISSING_TOKEN\"]\n".to_string(),
        }])
        .unwrap();

    assert_eq!(report.applied_layers, vec!["primary".to_string()]);
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );
    assert!(!service.permission_policy().approval_bypass());
    assert_eq!(
        service
            .permission_policy()
            .evaluate_shell_command("cargo test --all-targets"),
        RuleDecision::Allow
    );
    assert_eq!(service.mcp_registry().list_servers().len(), 1);
    assert_eq!(
        service.mcp_registry().prompt_summary().unavailable_servers[0].server_id,
        "fs"
    );
    assert_eq!(report.providers_configured, 1);
    assert_eq!(report.model_profiles_configured, 7);
    assert_eq!(report.default_model_profile.as_deref(), Some("default"));
    let profile = service
        .provider_registry()
        .resolve_profile("default")
        .unwrap();
    assert_eq!(profile.provider, "openai");
    assert_eq!(profile.model, "gpt-5.5");
    assert!(
        service
            .provider_registry()
            .resolve_profile("gpt-5.2")
            .is_ok(),
        "built-in OpenAI model profiles should be available when no provider list is configured"
    );
}

/// Verifies runtime applies explicit host clipboard pipe commands from
/// configuration. Users on systems where the default auto-detection order is
/// wrong need deterministic copy and paste commands without replacing the
/// internal paste-buffer behavior. Clipboard copy must not block the runtime
/// thread while a long-lived host clipboard helper keeps selection ownership.
#[test]
fn runtime_applies_host_clipboard_pipe_commands_from_config_layers() {
    let root = std::env::temp_dir().join(format!(
        "mez-runtime-clipboard-config-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let copy_path = root.join("copied.txt");
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[terminal]\nclipboard_copy_command = [\"sh\", \"-c\", \"sleep 1; cat > '{}'\"]\nclipboard_paste_command = [\"sh\", \"-c\", \"printf configured-paste\"]\n",
                copy_path.display()
            ),
        }])
        .unwrap();

    let started = Instant::now();
    assert!(service.host_clipboard.copy("configured-copy"));
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "clipboard copy blocked for {:?}",
        started.elapsed()
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut copied = String::new();
    while Instant::now() < deadline {
        if let Ok(content) = fs::read_to_string(&copy_path) {
            copied = content;
            if copied == "configured-copy" {
                break;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(copied, "configured-copy");
    assert_eq!(
        service.host_clipboard.read(),
        Some("configured-paste".to_string())
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies that configured named model profiles populate the full
/// specification-facing profile fields and that configured fallback profiles
/// are filtered through safety, privacy, residency, and approval
/// characteristics before they can be offered after provider failure.
#[test]
fn runtime_applies_named_model_profile_fields_and_safe_fallbacks() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-work\", \"gpt-safe\", \"gpt-weak\", \"gpt-external\"]\ndefault_model = \"gpt-work\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"gpt-work\"\nreasoning_profile = \"high\"\nlatency_preference = \"default\"\nmultimodal_required = true\nsafety_tier = \"high\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\nfallback_profiles = [\"safe\", \"weak\", \"external\"]\n[model_profiles.work.provider_options]\nreasoning_effort = \"high\"\n[model_profiles.safe]\nprovider = \"openai\"\nmodel = \"gpt-safe\"\nsafety_tier = \"high\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\n[model_profiles.weak]\nprovider = \"openai\"\nmodel = \"gpt-weak\"\nsafety_tier = \"medium\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\n[model_profiles.external]\nprovider = \"openai\"\nmodel = \"gpt-external\"\nsafety_tier = \"high\"\nprivacy_tier = \"external\"\nresidency = \"eu\"\napproval_policy = \"full-access\"\n"
                .to_string(),
        }])
        .unwrap();

    let registry = service.provider_registry();
    let profile = registry.resolve_profile("work").unwrap();
    assert_eq!(profile.provider, "openai");
    assert_eq!(profile.model, "gpt-work");
    assert_eq!(profile.reasoning_profile.as_deref(), Some("high"));
    assert_eq!(profile.latency_preference.as_deref(), Some("default"));
    assert!(profile.multimodal_required);
    assert_eq!(profile.safety_tier.as_deref(), Some("high"));
    assert_eq!(
        profile
            .provider_options
            .get("reasoning_effort")
            .map(String::as_str),
        Some("high")
    );
    assert_eq!(
        registry.safe_fallback_profiles("work").unwrap(),
        vec!["safe".to_string()]
    );
}

/// Verifies that provider failure reporting only offers configured fallback
/// profiles whose safety, privacy, residency, and approval characteristics are
/// non-weaker than the active model profile.
#[test]
fn runtime_provider_failure_reports_only_safe_model_fallbacks() {
    let mut service = test_runtime_service();
    let transcript_root = temp_root("runtime-provider-safe-fallback-transcript");
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    service.set_agent_transcript_store(transcript_store.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"runtime-fail\"\ndefault_model_profile = \"work\"\n[providers.runtime-fail]\nkind = \"runtime-fail\"\napi = \"openai-responses\"\nmodels = [\"primary\", \"safe\", \"weak\"]\ndefault_model = \"primary\"\n[model_profiles.work]\nprovider = \"runtime-fail\"\nmodel = \"primary\"\nsafety_tier = \"high\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\nfallback_profiles = [\"safe\", \"weak\"]\n[model_profiles.safe]\nprovider = \"runtime-fail\"\nmodel = \"safe\"\nsafety_tier = \"high\"\nprivacy_tier = \"strict\"\nresidency = \"us\"\napproval_policy = \"ask\"\n[model_profiles.weak]\nprovider = \"runtime-fail\"\nmodel = \"weak\"\nsafety_tier = \"medium\"\nprivacy_tier = \"external\"\nresidency = \"eu\"\napproval_policy = \"full-access\"\n"
                .to_string(),
        }])
        .unwrap();
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
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let start = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"agent-provider-safe-fallback","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(start.contains(r#""state":"running""#), "{start}");
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-1")
            .map(|turn| turn.model_profile.as_str()),
        Some("work")
    );

    let error = service
        .execute_agent_turn_with_provider(
            "turn-1",
            &RuntimeFailingProvider,
            service.provider_registry().resolve_profile("work").unwrap(),
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    let entries = transcript_store.inspect(&conversation_id).unwrap();
    let failure = entries
        .iter()
        .find(|entry| {
            entry.role == crate::transcript::TranscriptRole::Assistant
                && entry.content.contains("provider_error")
        })
        .unwrap();
    assert!(failure.content.contains("safe_fallback_profiles: safe"));
    assert!(!failure.content.contains("weak"));
    let _ = fs::remove_dir_all(transcript_root);
}

/// Verifies that frame position, style, and visible-field fallback templates
/// are applied from runtime config layers instead of being accepted but ignored.
#[test]
fn runtime_applies_frame_display_options_from_config_layers() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[frames.window]\nenabled = true\nposition = \"bottom\"\nstyle = \"inverse\"\ntemplate = \"\"\nright_status = \"#{datetime.local}\"\nvisible_fields = [\"session.id\", \"window.index\"]\n[frames.pane]\nenabled = true\nposition = \"bottom\"\nstyle = \"bold\"\ntemplate = \"\"\nvisible_fields = [\"pane.index\", \"agent.status\"]\n".to_string(),
        }])
        .unwrap();

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();

    assert!(service.window_frames_enabled);
    assert!(config.window_frames_enabled);
    assert_eq!(
        config.window_frame_position,
        crate::terminal::TerminalFramePosition::Bottom
    );
    assert_eq!(
        config.window_frame_style,
        crate::terminal::TerminalFrameStyle::Inverse
    );
    assert_eq!(
        config.window_frame_template,
        "#{session.id} #{window.index}"
    );
    assert_eq!(
        config.window_frame_visible_fields,
        vec!["session.id".to_string(), "window.index".to_string()]
    );
    assert_eq!(
        service.window_frame_right_status_template,
        "#{datetime.local}"
    );
    assert!(config.pane_frames_enabled);
    assert_eq!(
        config.pane_frame_position,
        crate::terminal::TerminalFramePosition::Bottom
    );
    assert_eq!(
        config.pane_frame_style,
        crate::terminal::TerminalFrameStyle::Bold
    );
    assert_eq!(config.pane_frame_template, "#{pane.index} #{agent.status}");
    assert_eq!(
        config.pane_frame_visible_fields,
        vec!["pane.index".to_string(), "agent.status".to_string()]
    );
}

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
        crate::terminal::TerminalCursorStyle::Bar
    );
    assert!(!config.cursor_blink);
    assert_eq!(config.cursor_blink_interval_ms, 250);
    assert_eq!(config.resize_debounce_ms, 125);
    assert_eq!(config.render_rate_limit_fps, 8);
    assert!(config.frame_context.reduced_motion);
    assert_eq!(config.frame_context.animation_tick_ms, 0);
}
/// Verifies that frame-context animation stays static when no live agent footer
/// is visible in the active window. This keeps idle redraws from paying for
/// animated footer state when agent mode is inactive or quiescent.
#[test]
fn runtime_frame_context_disables_animation_without_live_agent_footer() {
    let service = test_runtime_service();
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert_eq!(config.frame_context.animation_tick_ms, 0);
}
/// Verifies that a live agent footer re-enables animated frame ticks so active
/// agent progress indicators keep their motion while work is still running.
#[test]
fn runtime_frame_context_animates_live_agent_footer() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service.agent_compacting_panes.insert(pane_id, 1);
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert!(config.frame_context.animation_tick_ms > 0);
}

/// Verifies that active pane-frame agent status enables animation even when
/// the agent shell footer is not visible. Pane headers and live footers share
/// the same frame tick, so header-only status indicators must not freeze when
/// no prompt overlay is being rendered.
#[test]
fn runtime_frame_context_animates_active_agent_status_without_live_footer() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_turn_ledger
        .start_turn(crate::agent::AgentTurnRecord {
            turn_id: "turn-running".to_string(),
            agent_id: format!("agent-{pane_id}"),
            pane_id: pane_id.clone(),
            trigger: crate::agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: AgentTurnState::Running,
        })
        .unwrap();

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.agent_status.as_deref(), Some("running"));
    assert!(pane_context.agent_prompt.is_none());
    assert!(config.frame_context.animation_tick_ms > 0);
}

/// Verifies that callers with an already-resolved terminal loop config can
/// render the same primary view without rebuilding frame context and mouse hit
/// regions. This protects the optimized hot path used by control requests that
/// need both config and a rendered frame.
#[test]
fn runtime_render_client_view_with_resolved_config_matches_public_render() {
    let service = test_runtime_service();
    let client_size = Size::new(80, 24).unwrap();
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let direct = service
        .render_client_view(ClientViewRole::Primary, client_size, &config)
        .unwrap();
    let resolved = service
        .render_client_view_with_resolved_config(ClientViewRole::Primary, client_size, &config)
        .unwrap();
    assert_eq!(resolved, direct);
}

/// Verifies that runtime frame context sources `pane.process_name` from the
/// live host process metadata instead of only echoing the configured shell path.
#[cfg(target_os = "linux")]
/// Verifies runtime frame context uses host process name when available.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_frame_context_uses_host_process_name_when_available() {
    let mut service = test_runtime_service();
    service.start_initial_pane_process(Some("sleep 2")).unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();

    let mut process_name = None;
    for _ in 0..10_000 {
        process_name = service.pane_processes().process_name(&pane_id);
        if process_name.as_deref() == Some("sleep") {
            break;
        }
        thread::yield_now();
    }

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(process_name.as_deref(), Some("sleep"));
    assert_eq!(pane_context.process_name.as_deref(), Some("sleep"));
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that frame context renders the real normalized exit status when a
/// non-live pane has known exit metadata. This prevents pane frames from
/// collapsing all exited processes into a generic `exited` placeholder.
#[test]
fn runtime_frame_context_uses_known_pane_exit_status() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .session
        .set_pane_live_state(&pane_id, false)
        .unwrap();
    service.pane_exit_records.insert(
        pane_id.clone(),
        PaneExitRecord {
            exit_status: crate::process::PaneExitStatus {
                code: Some(7),
                signal: None,
                success: false,
            },
        },
    );

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.exit_status.as_deref(), Some("exit=7"));
}

/// Verifies that a visible pane agent shell publishes the active model profile,
/// reasoning profile, and idle status into pane frame context before any turn
/// has started. The default header relies on these fields for agent mode.
#[test]
fn runtime_frame_context_reports_visible_agent_shell_metadata() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-work\"]\ndefault_model = \"gpt-work\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"gpt-work\"\nreasoning_profile = \"high\"\n"
                .to_string(),
        }])
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.mode.as_deref(), Some("agent"));
    assert_eq!(pane_context.agent_name.as_deref(), Some("manager"));
    assert_eq!(pane_context.agent_status.as_deref(), Some("idle"));
    assert_eq!(pane_context.agent_model.as_deref(), Some("gpt-work"));
    assert_eq!(pane_context.agent_reasoning.as_deref(), Some("high"));
}

/// Verifies that pane-frame runtime context includes the best known current
/// working directory in the compact home-relative form used by the status
/// pill. This keeps the renderer independent from process probing while still
/// giving users location context when shell prompts are hidden or overwritten.
#[test]
fn runtime_frame_context_reports_home_relative_pane_working_directory() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let path = home
        .as_ref()
        .map(|home| home.join("Documents/repos/mezzanine"))
        .unwrap_or_else(|| PathBuf::from("/tmp/mezzanine"));
    let expected = home
        .as_ref()
        .map(|_| "~/Documents/repos/mezzanine".to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    service
        .pane_current_working_directories
        .insert(pane_id.clone(), path);

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(
        pane_context.current_working_directory.as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(
        config
            .frame_context
            .window_status
            .as_ref()
            .and_then(|status| status.active_pane_working_directory.as_deref()),
        Some(expected.as_str())
    );
}

/// Verifies that deep pane working directories collapse to the last three path
/// segments in the default window status. This keeps the footer compact while
/// still surfacing the most actionable cwd context for narrow frame rows.
#[test]
fn runtime_frame_context_compacts_deep_pane_working_directory() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let path = home
        .as_ref()
        .map(|home| home.join("Documents/repos/mezzanine/src/runtime"))
        .unwrap_or_else(|| PathBuf::from("/tmp/worktrees/mez/src/runtime"));
    service
        .pane_current_working_directories
        .insert(pane_id.clone(), path);

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(
        pane_context.current_working_directory.as_deref(),
        Some("…/mezzanine/src/runtime")
    );
    assert_eq!(
        config
            .frame_context
            .window_status
            .as_ref()
            .and_then(|status| status.active_pane_working_directory.as_deref()),
        Some("…/mezzanine/src/runtime")
    );
}

/// Verifies that frame context leaves unused dynamic right-status fields empty
/// when the configured template only references pane working-directory data.
/// This avoids repeated uptime and datetime formatting work on redraws that do
/// not display those fields.
#[test]
fn runtime_frame_context_skips_unused_dynamic_window_status_fields() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[frames.window]\nright_status = \"#{pane.pwd}\"\n".to_string(),
        }])
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    let path = home
        .as_ref()
        .map(|home| home.join("Documents/repos/mezzanine"))
        .unwrap_or_else(|| PathBuf::from("/tmp/mezzanine"));
    let expected = home
        .as_ref()
        .map(|_| "~/Documents/repos/mezzanine".to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    service
        .pane_current_working_directories
        .insert(pane_id, path);
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let status = config.frame_context.window_status.as_ref().unwrap();
    assert_eq!(
        status.active_pane_working_directory.as_deref(),
        Some(expected.as_str())
    );
    assert!(status.system_uptime.is_empty());
    assert!(status.datetime_local.is_empty());
}

/// Verifies that the pane-frame status reports compaction as its own active
/// running substate. Compaction is provider work, but it is distinct enough
/// from ordinary response generation that users need a direct state label.
#[test]
fn runtime_frame_context_reports_agent_compacting_substate() {
    let mut service = test_runtime_service();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    service
        .agent_turn_ledger
        .start_turn(crate::agent::AgentTurnRecord {
            turn_id: "turn-completed".to_string(),
            agent_id: format!("agent-{pane_id}"),
            pane_id: pane_id.clone(),
            trigger: crate::agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            cooperation_mode: None,
            state: AgentTurnState::Queued,
        })
        .unwrap();
    service
        .agent_turn_ledger
        .finish_turn("turn-completed", AgentTurnState::Completed)
        .unwrap();
    service.agent_compacting_panes.insert(pane_id.clone(), 1);

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.agent_status.as_deref(), Some("compacting"));
    assert_eq!(
        config
            .frame_context
            .window_agent_active_counts
            .get(service.session().active_window().unwrap().id.as_str())
            .copied(),
        Some(1)
    );
}

/// Verifies that an active agent turn reports the provider model name rather
/// than the selected profile name in pane-frame metadata. The pane status area
/// is constrained, so showing the concrete provider model and keeping reasoning
/// in its own field preserves both accuracy and space.
#[test]
fn runtime_frame_context_reports_running_agent_provider_model_name() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-work\"]\ndefault_model = \"gpt-work\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"gpt-work\"\nreasoning_profile = \"high\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt","method":"agent/shell/command","params":{"idempotency_key":"frame-provider-model","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(response.contains(r#""state":"running""#), "{response}");

    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.agent_status.as_deref(), Some("thinking"));
    assert_eq!(pane_context.agent_name.as_deref(), Some("manager"));
    assert_eq!(pane_context.agent_model.as_deref(), Some("gpt-work"));
    assert_eq!(pane_context.agent_reasoning.as_deref(), Some("high"));
    assert_eq!(pane_context.agent_context_usage, None);
    assert!(
        pane_context
            .agent_display_lines
            .iter()
            .any(|line| line.starts_with("thinking (") && line.contains(" • esc to interrupt")),
        "{pane_context:?}"
    );

    service
        .finish_agent_turn(&pane_id, "turn-1", AgentTurnState::Completed)
        .unwrap();
    let pane_text = service
        .pane_screen(&pane_id)
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("Worked for "), "{pane_text}");
}

/// Verifies that the pane frame reports only the latest provider-backed input
/// context percentage instead of replacing it with a local preflight estimate
/// while another turn is running. This keeps the status pill tied to the same
/// token accounting that the provider returns, while still allowing the runtime
/// to use internal byte estimates for compaction decisions separately.
#[test]
fn runtime_frame_context_reports_last_provider_context_usage() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-work\"]\ndefault_model = \"gpt-work\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"gpt-work\"\ncontext_window_tokens = 1000\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();

    let initial_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let initial_pane_context = initial_config.frame_context.panes.get(&pane_id).unwrap();
    assert_eq!(initial_pane_context.agent_context_usage, None);

    service.record_agent_provider_token_usage(
        &pane_id,
        crate::agent::ModelTokenUsage {
            input_tokens: 251,
            output_tokens: 34,
            reasoning_tokens: 9,
            cached_input_tokens: Some(80),
        },
    );
    let recorded_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let recorded_pane_context = recorded_config.frame_context.panes.get(&pane_id).unwrap();
    assert_eq!(
        recorded_pane_context.agent_context_usage.as_deref(),
        Some("25%")
    );

    let (_, profile) = service
        .active_model_profile_for_pane(&pane_id, &format!("agent-{pane_id}"), None)
        .unwrap();
    service.record_agent_provider_token_usage_with_profile(
        &pane_id,
        crate::agent::ModelTokenUsage {
            input_tokens: 1_200,
            output_tokens: 10,
            reasoning_tokens: 5,
            cached_input_tokens: Some(100),
        },
        crate::agent::ModelTokenUsage {
            input_tokens: 251,
            output_tokens: 10,
            reasoning_tokens: 5,
            cached_input_tokens: Some(80),
        },
        Some(&profile),
    );
    let cumulative_recorded_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let cumulative_recorded_pane_context = cumulative_recorded_config
        .frame_context
        .panes
        .get(&pane_id)
        .unwrap();
    assert_eq!(
        cumulative_recorded_pane_context
            .agent_context_usage
            .as_deref(),
        Some("25%")
    );

    service.record_agent_provider_token_usage_with_profile(
        &pane_id,
        crate::agent::ModelTokenUsage {
            input_tokens: 1_500,
            output_tokens: 10,
            reasoning_tokens: 5,
            cached_input_tokens: Some(100),
        },
        crate::agent::ModelTokenUsage {
            input_tokens: 1_200,
            output_tokens: 10,
            reasoning_tokens: 5,
            cached_input_tokens: Some(80),
        },
        Some(&profile),
    );
    let saturated_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let saturated_pane_context = saturated_config.frame_context.panes.get(&pane_id).unwrap();
    assert_eq!(
        saturated_pane_context.agent_context_usage.as_deref(),
        Some("100%")
    );

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"agent-prompt-context-usage","method":"agent/shell/command","params":{"idempotency_key":"agent-prompt-context-usage","input":"summarize the pane"}}"#,
        &primary,
    );
    assert!(response.contains(r#""state":"running""#), "{response}");

    let running_config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let running_pane_context = running_config.frame_context.panes.get(&pane_id).unwrap();
    assert_eq!(
        running_pane_context.agent_context_usage.as_deref(),
        Some("100%")
    );
}

/// Verifies that the agent frame context percentage uses the effective model
/// context-window denominator when a profile omits an explicit token count. This
/// protects the status area from reporting OpenAI GPT-5.5 usage against the
/// small local fallback window instead of the provider model's documented window.
#[test]
fn runtime_frame_context_uses_known_openai_model_context_window() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"work\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\"]\ndefault_model = \"gpt-5.5\"\n[model_profiles.work]\nprovider = \"openai\"\nmodel = \"gpt-5.5\"\n"
                .to_string(),
        }])
        .unwrap();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();

    service.record_agent_provider_token_usage(
        &pane_id,
        crate::agent::ModelTokenUsage {
            input_tokens: 10_500,
            output_tokens: 34,
            reasoning_tokens: 9,
            cached_input_tokens: Some(80),
        },
    );
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.agent_context_usage.as_deref(), Some("1%"));
}

/// Verifies pane context usage percentages for named OpenAI-compatible
/// providers use live provider-catalog context windows instead of the generic
/// fallback denominator.
#[test]
fn runtime_frame_context_uses_cached_catalog_context_window_for_named_compatible_provider() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"compat\"\ndefault_model_profile = \"work\"\n[providers.compat]\nkind = \"openai-compatible\"\nmodels = [\"baseline-model\"]\ndefault_model = \"baseline-model\"\n[model_profiles.work]\nprovider = \"compat\"\nmodel = \"baseline-model\"\n"
                .to_string(),
        }])
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "compat",
        vec![crate::agent::ProviderModelInfo {
            id: "catalog-only-model".to_string(),
            display_name: None,
            reasoning_levels: vec!["low".to_string()],
            context_window_tokens: Some(2_000_000),
            capabilities: Vec::new(),
        }],
        vec!["low".to_string()],
    );
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    service
        .agent_shell_store_mut()
        .enter_or_resume(&pane_id)
        .unwrap();
    let model_response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"compatible-model","method":"agent/shell/command","params":{"idempotency_key":"compatible-model","input":"/model catalog-only-model"}}"#,
        &primary,
    );
    assert!(model_response.contains("catalog-only-model"), "{model_response}");

    service.record_agent_provider_token_usage(
        &pane_id,
        crate::agent::ModelTokenUsage {
            input_tokens: 500_000,
            output_tokens: 34,
            reasoning_tokens: 9,
            cached_input_tokens: Some(80),
        },
    );
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let pane_context = config.frame_context.panes.get(&pane_id).unwrap();

    assert_eq!(pane_context.agent_context_usage.as_deref(), Some("25%"));
}

/// Verifies that runtime config application fails closed when a layer attempts
/// to enter approval bypass directly. Bypass activation must stay tied to the
/// explicit primary-authorized command path rather than a passive config load
/// or live config reload.
#[test]
fn runtime_rejects_config_enabled_approval_bypass_mode() {
    let mut service = test_runtime_service();
    let error = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[permissions]\nbypass_mode = true\n".to_string(),
        }])
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Config);
    assert!(
        error
            .message()
            .contains("permissions.bypass_mode cannot be enabled from configuration"),
        "{}",
        error.message()
    );
    assert!(!service.permission_policy().approval_bypass());
}

/// Verifies runtime applies configured lifecycle hooks.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_applies_configured_lifecycle_hooks() {
    let root = temp_root("configured-hooks");
    let payload_path = root.join("attach-payload.json");
    let mut service = test_runtime_service();
    let report = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[hooks.attach]\nevent = \"client_attach\"\nprogram = \"/bin/sh\"\nargs = [\"-c\", \"cat > \\\"$1\\\"\", \"hook\", \"{}\"]\n\n[hooks.focused]\nevent = \"client_attach\"\ncommand = \"printf hook-from-config\"\nagent_hook = true\non_failure = \"warn\"\n",
                payload_path.display()
            ),
        }])
        .unwrap();

    assert_eq!(report.hooks_configured, 2);
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();

    let payload = fs::read_to_string(&payload_path).unwrap();
    assert!(payload.contains(r#""client_id":"#), "{payload}");
    assert!(payload.contains(primary.as_str()), "{payload}");
    assert_eq!(service.focused_shell_hook_queue_len(), 1);
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime config parses hook matcher groups.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_config_parses_hook_matcher_groups() {
    let mut service = test_runtime_service();
    let report = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[hooks.prompt]\nevent = \"user_prompt_submit\"\nprogram = \"/bin/echo\"\n[hooks.prompt.match.pane_id]\nprefix = \"pane-\"\n[[hooks.prompt.matches]]\npath = \"agent_id\"\nequals = \"agent-1\"\n".to_string(),
        }])
        .unwrap();

    let matching = crate::hooks::plan_event(
        &service.hook_definitions,
        HookEvent::UserPromptSubmit,
        r#"{"pane_id":"pane-2"}"#,
    )
    .unwrap();
    let fallback = crate::hooks::plan_event(
        &service.hook_definitions,
        HookEvent::UserPromptSubmit,
        r#"{"agent_id":"agent-1"}"#,
    )
    .unwrap();
    let filtered = crate::hooks::plan_event(
        &service.hook_definitions,
        HookEvent::UserPromptSubmit,
        r#"{"pane_id":"other","agent_id":"agent-2"}"#,
    )
    .unwrap();

    assert_eq!(report.hooks_configured, 1);
    assert_eq!(service.hook_definitions[0].matcher_groups.len(), 2);
    assert_eq!(matching.plans.len(), 1);
    assert_eq!(fallback.plans.len(), 1);
    assert!(filtered.plans.is_empty());
}

/// Verifies runtime config reload reloads layers and applies live policy.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_config_reload_reloads_layers_and_applies_live_policy() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-config-reload");
    let path = root.join("config.toml");
    fs::write(&path, "[permissions]\napproval_policy = \"full-access\"\n").unwrap();
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
    let audit_root = temp_root("runtime-config-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::FullAccess
    );

    fs::write(
        &path,
        "[permissions]\napproval_policy = \"ask\"\n[[permissions.command_rules]]\npattern = [\"cargo\", \"test\"]\ndecision = \"allow\"\nscope = \"session\"\nmatch = \"prefix\"\n",
    )
    .unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload","method":"config/reload","params":{"idempotency_key":"reload-live-config"}}"#,
        &primary,
    );

    assert!(response.contains(r#""operation":"reload""#), "{response}");
    assert_eq!(
        service.permission_policy().approval_policy,
        ApprovalPolicy::Ask
    );
    assert_eq!(
        service
            .permission_policy()
            .evaluate_shell_command("cargo test --all-targets"),
        RuleDecision::Allow
    );
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"configuration""#), "{audit}");
    assert!(audit.contains(r#""action":"reload""#), "{audit}");
    assert!(audit.contains(r#""outcome":"started""#), "{audit}");
    assert!(audit.contains(r#""outcome":"applied""#), "{audit}");
    assert!(audit.contains(r#""event_type":"permission""#), "{audit}");
    assert!(
        audit.contains(r#""permission_id":"permissions.approval_policy""#),
        "{audit}"
    );
    assert!(
        audit.contains(r#""permission_id":"permissions.command_rules""#),
        "{audit}"
    );
    assert!(
        audit.contains(r#""action_kind":"config_reload""#),
        "{audit}"
    );
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(audit_root);
    let _ = fs::remove_dir_all(root);
}

/// Verifies that live runtime `config/set` and `config/unset` requests apply
/// the spec-defined `PersistTarget` vocabulary directly to the running service.
/// This protects the control API from returning offline planning placeholders
/// when a primary client asks for a non-persistent live configuration change.
#[test]
fn runtime_control_config_live_persist_target_mutates_live_override() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let audit_root = temp_root("runtime-live-config-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));

    let set = r#"{"jsonrpc":"2.0","id":"live-set","method":"config/set","params":{"path":"history.lines","value":5,"persist":{"scope":"live"},"idempotency_key":"live-history"}}"#;
    let first = service.dispatch_runtime_control_body(set, &primary);
    let first_json: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(first_json["result"]["applied"], true, "{first}");
    assert_eq!(first_json["result"]["persisted"], false, "{first}");
    assert_eq!(first_json["result"]["plan"]["scope"], "live", "{first}");
    assert_eq!(
        first_json["result"]["plan"]["target"]["scope"], "live",
        "{first}"
    );
    assert_eq!(service.terminal_history_limit(), 5);
    assert_eq!(service.session.config_generation, 1);

    let second = service.dispatch_runtime_control_body(set, &primary);
    assert_eq!(first, second);
    assert_eq!(service.control_idempotency().len(), 1);
    assert_eq!(service.session.config_generation, 1);

    let conflict = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"live-conflict","method":"config/set","params":{"path":"history.lines","value":6,"persist":{"scope":"live"},"idempotency_key":"live-history"}}"#,
        &primary,
    );
    assert!(
        conflict.contains(r#""mezzanine_code":"conflict""#),
        "{conflict}"
    );
    assert_eq!(service.terminal_history_limit(), 5);
    assert_eq!(service.session.config_generation, 1);

    let null_persist = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"live-null","method":"config/set","params":{"path":"history.lines","value":6,"persist":null,"idempotency_key":"live-null-history"}}"#,
        &primary,
    );
    assert!(
        null_persist.contains(r#""target":{"scope":"live","path":null}"#),
        "{null_persist}"
    );
    assert!(
        null_persist.contains(r#""persisted":false"#),
        "{null_persist}"
    );
    assert_eq!(service.terminal_history_limit(), 6);
    assert_eq!(service.session.config_generation, 2);

    let unset = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"live-unset","method":"config/unset","params":{"path":"history.lines","persist":{"scope":"live"},"idempotency_key":"live-history-unset"}}"#,
        &primary,
    );
    assert!(unset.contains(r#""applied":true"#), "{unset}");
    assert_eq!(service.session.config_generation, 3);
    assert_ne!(service.terminal_history_limit(), 6);

    let primary_scope = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"primary-scope","method":"config/set","params":{"path":"history.lines","value":7,"persist":{"scope":"primary"},"idempotency_key":"primary-scope"}}"#,
        &primary,
    );
    assert!(primary_scope.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(primary_scope.contains("must be live, user, or project"));

    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"configuration""#), "{audit}");
    assert!(audit.contains(r#""action":"set""#), "{audit}");
    assert!(audit.contains(r#""outcome":"started""#), "{audit}");
    assert!(audit.contains(r#""outcome":"applied""#), "{audit}");
    assert!(audit.contains(r#""scope":"live""#), "{audit}");
    let _ = fs::remove_dir_all(audit_root);
}

/// Verifies that runtime user config persistence is confined to the configured
/// private config root or the active primary layer. This prevents control
/// clients from using `scope = user` as a general-purpose file write primitive.
#[test]
fn runtime_control_config_user_persistence_requires_user_private_target() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-user-config-persist");
    let config_root = root.join("config");
    let config_path = config_root.join("config.toml");
    fs::create_dir_all(&config_root).unwrap();
    fs::write(&config_path, "[history]\nlines = 10\n").unwrap();
    service.set_config_root(config_root.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(config_path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: fs::read_to_string(&config_path).unwrap(),
        }])
        .unwrap();

    let outside_path = root.join("outside.toml");
    fs::write(&outside_path, "[history]\nlines = 10\n").unwrap();
    let rejected = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"user-outside","method":"config/set","params":{{"path":"history.lines","value":7,"persist":{{"scope":"user","path":"{}"}},"idempotency_key":"user-outside"}}}}"#,
            json_escape(&outside_path.to_string_lossy())
        ),
        &primary,
    );
    assert!(
        rejected.contains(r#""mezzanine_code":"invalid_params""#),
        "{rejected}"
    );
    assert!(
        rejected.contains("configured user-private config root"),
        "{rejected}"
    );
    assert!(
        fs::read_to_string(&outside_path)
            .unwrap()
            .contains("lines = 10")
    );
    assert_eq!(service.terminal_history_limit(), 10);

    let allowed = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"user-inside","method":"config/set","params":{{"path":"history.lines","value":7,"persist":{{"scope":"user","path":"{}"}},"idempotency_key":"user-inside"}}}}"#,
            json_escape(&config_path.to_string_lossy())
        ),
        &primary,
    );
    assert!(allowed.contains(r#""applied":true"#), "{allowed}");
    assert!(allowed.contains(r#""persisted":true"#), "{allowed}");
    assert_eq!(service.terminal_history_limit(), 7);
    assert!(
        fs::read_to_string(&config_path)
            .unwrap()
            .contains("lines = 7")
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies that runtime project config persistence blocks until the target
/// path is covered by a trusted project-root decision. This keeps project
/// overlays from being written before the primary client has accepted the
/// project trust boundary.
#[test]
fn runtime_control_config_project_persistence_requires_trusted_root() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-project-config-persist");
    fs::create_dir_all(root.join(".git")).unwrap();
    let project_config_dir = root.join(".mezzanine");
    let project_path = project_config_dir.join("config.toml");
    fs::create_dir_all(&project_config_dir).unwrap();
    fs::write(&project_path, "version = 17\n[history]\nlines = 10\n").unwrap();
    service.set_project_trust_store(ProjectTrustStore::default(), None);
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "project".to_string(),
            path: Some(project_path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::ProjectOverlay,
            trusted: true,
            text: fs::read_to_string(&project_path).unwrap(),
        }])
        .unwrap();

    let pending = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"project-pending","method":"config/set","params":{{"path":"history.lines","value":7,"persist":{{"scope":"project","path":"{}"}},"idempotency_key":"project-pending"}}}}"#,
            json_escape(&project_path.to_string_lossy())
        ),
        &primary,
    );
    assert!(
        pending.contains(r#""mezzanine_code":"conflict""#),
        "{pending}"
    );
    assert!(
        pending.contains("blocked until project trust is decided"),
        "{pending}"
    );
    assert!(
        fs::read_to_string(&project_path)
            .unwrap()
            .contains("lines = 10")
    );

    let mut trust_store = ProjectTrustStore::default();
    trust_store
        .decide_at(root.clone(), TrustDecision::Trusted, None, 42)
        .unwrap();
    service.set_project_trust_store(trust_store, None);
    let trusted = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"project-trusted","method":"config/set","params":{{"path":"history.lines","value":7,"persist":{{"scope":"project","path":"{}"}},"idempotency_key":"project-trusted"}}}}"#,
            json_escape(&project_path.to_string_lossy())
        ),
        &primary,
    );
    assert!(trusted.contains(r#""applied":true"#), "{trusted}");
    assert!(trusted.contains(r#""persisted":true"#), "{trusted}");
    assert_eq!(service.terminal_history_limit(), 7);
    assert!(
        fs::read_to_string(&project_path)
            .unwrap()
            .contains("lines = 7")
    );

    let outside_path = temp_root("runtime-project-config-outside").join("config.toml");
    let outside = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"project-outside","method":"config/set","params":{{"path":"history.lines","value":5,"persist":{{"scope":"project","path":"{}"}},"idempotency_key":"project-outside"}}}}"#,
            json_escape(&outside_path.to_string_lossy())
        ),
        &primary,
    );
    assert!(
        outside.contains(r#""mezzanine_code":"conflict""#),
        "{outside}"
    );
    assert!(
        outside.contains("blocked until project trust is decided"),
        "{outside}"
    );
    let _ = fs::remove_dir_all(outside_path.parent().unwrap());
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime config reload applies history limit to live screens.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_config_reload_applies_history_limit_to_live_screens() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-history-reload");
    let path = root.join("config.toml");
    fs::write(&path, "[history]\nlines = 4\nrotate_lines = 2\n").unwrap();
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
    let mut screen = TerminalScreen::new(Size::new(12, 2).unwrap(), 4).unwrap();
    screen.restore_normal_content(
        &["one".to_string(), "two".to_string(), "three".to_string()],
        &[],
    );
    service.pane_screens.insert("%1".to_string(), screen);

    fs::write(&path, "[history]\nlines = 2\nrotate_lines = 3\n").unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload","method":"config/reload","params":{"idempotency_key":"reload-history-limit"}}"#,
        &primary,
    );

    assert!(response.contains(r#""operation":"reload""#), "{response}");
    assert_eq!(service.terminal_history_limit(), 2);
    assert_eq!(service.terminal_history_rotate_lines(), 3);
    let screen = service.pane_screen("%1").unwrap();
    assert_eq!(screen.history_limit(), 2);
    assert_eq!(screen.history_rotate_lines(), 3);
    assert_eq!(
        screen.history().lines().collect::<Vec<_>>(),
        vec!["two", "three"]
    );
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime config reload applies agent scheduler limit.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_config_reload_applies_agent_scheduler_limit() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-scheduler-reload");
    let path = root.join("config.toml");
    fs::write(&path, "[agents]\nmax_concurrent_agents = 2\n").unwrap();
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
        .agent_scheduler_mut()
        .enqueue(ScheduledWork {
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            pane_id: Some("%1".to_string()),
            kind: ScheduledWorkKind::ShellCapable,
        })
        .unwrap();
    service
        .agent_scheduler_mut()
        .enqueue(ScheduledWork {
            turn_id: "turn-2".to_string(),
            agent_id: "agent-2".to_string(),
            pane_id: Some("%2".to_string()),
            kind: ScheduledWorkKind::ShellCapable,
        })
        .unwrap();
    assert_eq!(
        service.agent_scheduler_mut().start_ready().unwrap().turn_id,
        "turn-1"
    );
    assert_eq!(
        service.agent_scheduler_mut().start_ready().unwrap().turn_id,
        "turn-2"
    );

    fs::write(&path, "[agents]\nmax_concurrent_agents = 1\n").unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload","method":"config/reload","params":{"idempotency_key":"reload-scheduler-limit"}}"#,
        &primary,
    );

    assert!(response.contains(r#""operation":"reload""#), "{response}");
    let snapshot = service.agent_scheduler().snapshot();
    assert_eq!(snapshot.max_concurrent_agents, 1);
    assert_eq!(snapshot.running, 2);
    assert!(service.agent_scheduler_mut().start_ready().is_none());
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime config reload applies the model-correction retry budget.
///
/// Action-failure recovery is intentionally bounded so a repeated bad action
/// cannot loop forever, but the bound must be configurable for providers and
/// tasks that need more than the default repair attempts.
#[test]
fn runtime_config_reload_applies_action_failure_retry_limit() {
    let mut service = test_runtime_service();
    assert_eq!(service.agent_action_failure_retry_limit, 5);
    let root = temp_root("runtime-action-failure-retry-limit");
    let path = root.join("config.toml");
    fs::write(&path, "[agents]\naction_failure_retry_limit = 2\n").unwrap();

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

    assert_eq!(service.agent_action_failure_retry_limit, 2);
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime config reload applies implementation-pressure thresholds.
///
/// The pressure hint is intentionally runtime-owned and turn-local: changing
/// the setting should take effect for future provider continuations without
/// restarting the session or changing action-failure retry behavior.
#[test]
fn runtime_config_reload_applies_implementation_pressure_threshold() {
    let mut service = test_runtime_service();
    assert_eq!(service.agent_implementation_pressure_after_shell_actions, 3);
    let root = temp_root("runtime-implementation-pressure-threshold");
    let path = root.join("config.toml");
    fs::write(
        &path,
        "[agents]\nimplementation_pressure_after_shell_actions = 3\n",
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

    assert_eq!(service.agent_implementation_pressure_after_shell_actions, 3);
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
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

/// Verifies that subagent wait policy is a validated live agent option.
///
/// The default must remain join-and-wait so parent turns do not race ahead of
/// delegated work, while explicit `detach` configuration remains available for
/// workflows that want fire-and-forget delegation. Invalid values must fail
/// config application with a diagnosable error rather than silently changing
/// scheduler semantics.
#[test]
fn runtime_config_reload_applies_subagent_wait_policy() {
    let mut service = test_runtime_service();
    assert_eq!(service.subagent_wait_policy, SubagentWaitPolicy::Join);

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nsubagent_wait_policy = \"detach\"\n".to_string(),
        }])
        .unwrap();
    assert_eq!(service.subagent_wait_policy, SubagentWaitPolicy::Detach);

    let error = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nsubagent_wait_policy = \"invalid\"\n".to_string(),
        }])
        .unwrap_err();
    assert!(
        error.message().contains("unsupported subagent wait policy"),
        "{error}"
    );
}

/// Verifies that subagent width and depth limits are live agent options.
///
/// Delegation capacity is runtime scheduling policy rather than static config
/// metadata. Reloading these values must update the service immediately so
/// subsequent control and MAAP spawns apply the same current limits without
/// restarting the session.
#[test]
fn runtime_config_reload_applies_subagent_capacity_limits() {
    let mut service = test_runtime_service();

    assert_eq!(service.max_root_subagents, 4);
    assert_eq!(service.max_subagents_per_subagent, 2);
    assert_eq!(service.max_subagent_depth, 2);

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text:
                "[agents]\nmax_root_subagents = 6\nmax_subagents_per_subagent = 3\nmax_depth = 4\n"
                    .to_string(),
        }])
        .unwrap();

    assert_eq!(service.max_root_subagents, 6);
    assert_eq!(service.max_subagents_per_subagent, 3);
    assert_eq!(service.max_subagent_depth, 4);

    let error = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nmax_root_subagents = 0\n".to_string(),
        }])
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("agents.max_root_subagents must be a positive integer"),
        "{error}"
    );
}

/// Verifies the runtime applies raw-retention config for compaction recovery.
///
/// Provider context-limit recovery and manual compaction both use the
/// raw-retention percentage to decide how much exact recent context remains
/// after compaction.
#[test]
fn runtime_config_reload_applies_compaction_raw_retention() {
    let mut service = test_runtime_service();

    assert_eq!(service.agent_compaction_raw_retention_percent, 10);

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ncompaction_raw_retention_percent = 25\n".to_string(),
        }])
        .unwrap();

    assert_eq!(service.agent_compaction_raw_retention_percent, 25);
}

/// Verifies that a live config reload starts queued agent work when the new
/// scheduler limit makes that work runnable. Updating the limit without
/// draining newly available scheduler capacity would leave prompt turns queued
/// until some unrelated turn completion nudged the scheduler.
#[test]
fn runtime_config_reload_starts_newly_runnable_agent_turns() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-scheduler-reload-start-ready");
    let path = root.join("config.toml");
    fs::write(&path, "[agents]\nmax_concurrent_agents = 1\n").unwrap();
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
    let second = service
        .start_agent_prompt_turn(second_pane.as_str(), "second")
        .unwrap();
    assert_eq!(first.state, AgentTurnState::Running);
    assert_eq!(second.state, AgentTurnState::Queued);
    assert_eq!(service.agent_scheduler().snapshot().running, 1);
    assert_eq!(service.agent_scheduler().snapshot().queued, 1);

    fs::write(&path, "[agents]\nmax_concurrent_agents = 2\n").unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload","method":"config/reload","params":{"idempotency_key":"reload-scheduler-start-ready"}}"#,
        &primary,
    );

    assert!(response.contains(r#""operation":"reload""#), "{response}");
    assert_eq!(service.agent_scheduler().snapshot().running, 2);
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(
        service
            .agent_shell_store()
            .get(second_pane.as_str())
            .and_then(|session| session.running_turn_id.as_deref()),
        Some("turn-2")
    );
    assert!(
        service
            .pending_agent_provider_tasks()
            .iter()
            .any(|task| task.turn_id == "turn-2"),
    );
    service.kill_session(&primary, true).unwrap();
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

/// Verifies that stopping a queued pane-local agent turn does not depend on the
/// pane shell store having that queued turn as the active running turn. This
/// covers the queued cleanup path used when global scheduler capacity is full.
#[test]
fn runtime_stop_agent_turn_cleans_up_queued_turn_without_shell_running_marker() {
    let mut service = test_runtime_service();
    service
        .agent_scheduler_mut()
        .set_max_concurrent_agents(1)
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let second_pane = service
        .session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
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
    let second = service
        .start_agent_prompt_turn(second_pane.as_str(), "second")
        .unwrap();
    assert_eq!(first.state, AgentTurnState::Running);
    assert_eq!(second.state, AgentTurnState::Queued);

    let stopped = service
        .stop_agent_turn_for_pane(second_pane.as_str())
        .unwrap();

    assert_eq!(stopped.turn_id, "turn-2");
    assert!(stopped.scheduler_cancelled);
    assert_eq!(service.agent_scheduler().snapshot().queued, 0);
    assert_eq!(
        service
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == "turn-2")
            .map(|turn| turn.state),
        Some(AgentTurnState::Interrupted)
    );
    assert_eq!(
        service
            .agent_shell_store()
            .get(second_pane.as_str())
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    service.kill_session(&primary, true).unwrap();
}

/// Verifies that runtime configuration can initialize the audit writer from
/// `[audit]` settings. The path is resolved under the configured Mezzanine
/// config root when relative, and subsequent auditable runtime actions write
/// JSONL records through the configured hash-chain and retention modes.
#[test]
fn runtime_applies_audit_log_from_config_layers() {
    let mut service = test_runtime_service();
    let root = temp_root("runtime-audit-config");
    let config_root = root.join("config");
    service.set_config_root(config_root.clone());
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[audit]\nenabled = true\npath = \"security/audit.jsonl\"\nformat = \"jsonl\"\nretention_days = 1\nhash_chain = true\nrequired = true\n".to_string(),
        }])
        .unwrap();
    let audit_path = config_root.join("security/audit.jsonl");
    assert_eq!(service.audit_log().unwrap().path(), audit_path.as_path());
    fs::create_dir_all(audit_path.parent().unwrap()).unwrap();
    fs::write(
        &audit_path,
        "{\"timestamp\":\"unix:1\",\"action\":\"old\"}\n",
    )
    .unwrap();

    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let output = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"audit-approval","method":"agent/shell/command","params":{"idempotency_key":"audit-approval","input":"/approval full-access"}}"#,
        &primary,
    );

    assert!(output.contains("changed=true"), "{output}");
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"permission""#), "{audit}");
    assert!(audit.contains(r#""hash":"#), "{audit}");
    assert!(!audit.contains(r#""action":"old""#), "{audit}");
    let _ = fs::remove_dir_all(root);
}

/// Verifies that invalid audit retention configuration fails before replacing
/// the runtime audit writer. A zero-day retention window would immediately
/// discard useful audit history, so the config layer is rejected instead of
/// silently enabling destructive pruning.
#[test]
fn runtime_rejects_invalid_audit_retention_days() {
    let mut service = test_runtime_service();
    let error = service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[audit]\nenabled = true\nretention_days = 0\n".to_string(),
        }])
        .unwrap_err();

    assert!(error.message().contains("audit.retention_days"), "{error}");
    assert!(service.audit_log().is_none());
}

/// Verifies that unknown project-trust method names do not enter the runtime's
/// project-trust dispatcher. Unsupported names must remain ordinary JSON-RPC
/// method-not-found errors rather than reporting a project-trust implementation
/// placeholder, because only the advertised project trust methods are valid.
#[test]
fn runtime_unknown_project_trust_method_uses_generic_method_not_found() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"unknown","method":"project/trust/archive","params":{}}"#,
        &primary,
    );

    assert!(
        response.contains(r#""mezzanine_code":"method_not_found""#),
        "{response}"
    );
    assert!(
        response.contains("unknown control method `project/trust/archive`"),
        "{response}"
    );
    assert!(!response.contains("project trust method"), "{response}");
}

/// Verifies runtime project trust decision applies and removes project overlays.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_project_trust_decision_applies_and_removes_project_overlays() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-project-trust");
    let audit_root = temp_root("runtime-project-trust-audit");
    let audit_path = audit_root.join("audit.jsonl");
    service.set_audit_log(AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    }));
    fs::create_dir_all(root.join(".git")).unwrap();
    let overlay_dir = root.join(".mezzanine");
    fs::create_dir_all(&overlay_dir).unwrap();
    let overlay_path = overlay_dir.join("config.toml");
    fs::write(
        &overlay_path,
        "version = 17\n[history]\nlines = 7\n[permissions]\napproval_policy = \"ask\"\n",
    )
    .unwrap();
    let trust_path = root.join("trust.tsv");
    service.set_project_trust_store(ProjectTrustStore::default(), Some(trust_path.clone()));
    let initial_report = service
        .replace_config_layers(vec![
            ConfigLayer {
                name: "primary".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: "[history]\nlines = 3\n".to_string(),
            },
            ConfigLayer {
                name: "project".to_string(),
                path: Some(overlay_path.clone()),
                format: ConfigFormat::Toml,
                scope: ConfigScope::ProjectOverlay,
                trusted: false,
                text: fs::read_to_string(&overlay_path).unwrap(),
            },
        ])
        .unwrap();
    assert_eq!(initial_report.project_trust_prompts_announced, 1);
    assert_eq!(service.terminal_history_limit(), 3);
    let primary_events = service
        .event_log
        .as_ref()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(primary_events.iter().any(|event| {
        event.kind == EventKind::ConfigChanged
            && event.payload.contains(r#""state":"pending""#)
            && event
                .payload
                .contains(r#""blocks_until_primary_decision":true"#)
            && event
                .payload
                .contains(&json_escape(&root.to_string_lossy()))
    }));
    assert_eq!(
        service
            .apply_runtime_config_layers()
            .unwrap()
            .project_trust_prompts_announced,
        0
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let blocked_prompt = service
        .execute_agent_shell_command(&primary, "summarize this project")
        .unwrap();
    assert!(
        blocked_prompt.contains(r#""kind":"display""#)
            && blocked_prompt.contains("agent command error: project trust decision pending")
            && blocked_prompt.contains("(conflict)"),
        "{blocked_prompt}"
    );
    assert!(
        blocked_prompt.contains("project trust decision pending"),
        "{blocked_prompt}"
    );
    assert!(service.agent_turn_ledger.turns().is_empty());

    let trust = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"trust","method":"project/trust/decide","params":{{"project_root":"{}","decision":"trust","idempotency_key":"trust-project"}}}}"#,
            json_escape(&root.to_string_lossy())
        ),
        &primary,
    );

    assert!(trust.contains(r#""state":"trusted""#), "{trust}");
    assert!(trust.contains(r#""trusted_at":""#), "{trust}");
    assert!(
        trust.contains(&format!(r#""decided_by_client_id":"{}""#, primary)),
        "{trust}"
    );
    assert!(!trust.contains(r#""trusted_at":"unix:"#), "{trust}");
    assert!(trust.contains(r#""changed_layers":["project"]"#), "{trust}");
    assert!(
        trust.contains(&json_escape(&overlay_path.to_string_lossy())),
        "{trust}"
    );
    assert!(
        trust.contains(&format!(
            r#""overlay_files":[{{"path":"{}","format":"toml","applied":true,"diagnostics":[]}}]"#,
            json_escape(&overlay_path.to_string_lossy())
        )),
        "{trust}"
    );
    assert!(
        trust.contains(r#""capability_expansion_summary":["permissions"]"#),
        "{trust}"
    );
    assert_eq!(service.terminal_history_limit(), 7);
    assert!(service.config_layers()[1].trusted);
    assert!(trust_path.exists());

    let trusted_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"trusted-list","method":"project/trust/list","params":{"state":"trusted"}}"#,
        &primary,
    );
    assert!(
        trusted_list.contains(&json_escape(&root.to_string_lossy())),
        "{trusted_list}"
    );

    let pending_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"pending-list","method":"project/trust/list","params":{"state":"pending"}}"#,
        &primary,
    );
    assert!(
        !pending_list.contains(&json_escape(&root.to_string_lossy())),
        "{pending_list}"
    );

    let invalid_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"invalid-list","method":"project/trust/list","params":{"state":"unknown"}}"#,
        &primary,
    );
    assert!(
        invalid_list.contains(r#""mezzanine_code":"invalid_params""#),
        "{invalid_list}"
    );

    let revoke = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"revoke","method":"project/trust/revoke","params":{{"project_root":"{}","idempotency_key":"revoke-project"}}}}"#,
            json_escape(&root.to_string_lossy())
        ),
        &primary,
    );

    assert!(revoke.contains(r#""state":"revoked""#), "{revoke}");
    assert!(
        revoke.contains(&format!(r#""decided_by_client_id":"{}""#, primary)),
        "{revoke}"
    );

    let revoked_list = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"revoked-list","method":"project/trust/list","params":{"state":"revoked"}}"#,
        &primary,
    );
    assert!(
        revoked_list.contains(&json_escape(&root.to_string_lossy())),
        "{revoked_list}"
    );

    assert_eq!(service.terminal_history_limit(), 3);
    assert!(!service.config_layers()[1].trusted);

    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"configuration""#), "{audit}");
    assert!(audit.contains(r#""scope":"project_trust""#), "{audit}");
    assert!(audit.contains(r#""decision":"trusted""#), "{audit}");
    assert!(audit.contains(r#""decision":"revoked""#), "{audit}");
    assert!(audit.contains(r#""project_root""#), "{audit}");
    let _ = fs::remove_dir_all(audit_root);
    let _ = fs::remove_dir_all(root);
}

/// Verifies agent work refreshes project overlays from the active pane's cwd.
///
/// The daemon may start outside the repository. Before an agent prompt runs,
/// the runtime should discover `.mezzanine/config.*` under the pane project,
/// block for trust, apply the trusted overlay, and expose trusted project
/// skills through the same catalog used by `/list-skills` and `$skill`.
#[test]
fn runtime_agent_prompt_refreshes_project_overlay_and_project_skills_from_pane_cwd() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-project-refresh");
    let config_root = root.join("config-root");
    let project_root = root.join("repo");
    let nested = project_root.join("src");
    let overlay_dir = project_root.join(".mezzanine");
    let skill_dir = overlay_dir.join("skills/review");
    fs::create_dir_all(project_root.join(".git")).unwrap();
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir_all(&skill_dir).unwrap();
    let overlay_path = overlay_dir.join("config.toml");
    fs::write(&overlay_path, "version = 17\n[history]\nlines = 11\n").unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: review\ndescription: Project review workflow\n---\n\nReview this repository.\n",
    )
    .unwrap();
    service.set_config_root(config_root.clone());
    service.set_project_trust_store(
        ProjectTrustStore::default(),
        Some(config_root.join("project-trust.tsv")),
    );
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 3\n".to_string(),
        }])
        .unwrap();
    service
        .pane_current_working_directories
        .insert("%1".to_string(), nested.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let blocked_prompt = service
        .execute_agent_shell_command(&primary, "summarize this project")
        .unwrap();
    assert!(
        blocked_prompt.contains("project trust decision pending"),
        "{blocked_prompt}"
    );
    assert!(service.agent_turn_ledger.turns().is_empty());
    assert_eq!(service.terminal_history_limit(), 3);
    assert!(
        service
            .config_layers()
            .iter()
            .any(|layer| layer.path.as_ref() == Some(&overlay_path) && !layer.trusted)
    );

    let trust = service.dispatch_runtime_control_body(
        &format!(
            r#"{{"jsonrpc":"2.0","id":"trust-refresh","method":"project/trust/decide","params":{{"project_root":"{}","decision":"trust","idempotency_key":"trust-refresh"}}}}"#,
            json_escape(&project_root.to_string_lossy())
        ),
        &primary,
    );
    assert!(trust.contains(r#""state":"trusted""#), "{trust}");
    assert_eq!(service.terminal_history_limit(), 11);

    let skills = service
        .execute_agent_shell_command(&primary, "/list-skills")
        .unwrap();
    assert!(skills.contains("Project review workflow"), "{skills}");
    assert!(
        skills.contains("| `$review` | project | Project review workflow |"),
        "{skills}"
    );
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime agent trust command logs and persists project trust request.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_agent_trust_command_logs_and_persists_project_trust_request() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-agent-trust-command");
    let config_root = root.join("config-root");
    let trust_path = config_root.join("project-trust.tsv");
    service.set_config_root(config_root.clone());
    fs::create_dir_all(root.join(".git")).unwrap();
    let overlay_dir = root.join(".mezzanine");
    fs::create_dir_all(&overlay_dir).unwrap();
    let overlay_path = overlay_dir.join("config.toml");
    fs::write(
        &overlay_path,
        "version = 17\n[history]\nlines = 11\n[permissions]\napproval_policy = \"ask\"\n",
    )
    .unwrap();
    service.set_project_trust_store(ProjectTrustStore::default(), None);
    let initial_report = service
        .replace_config_layers(vec![
            ConfigLayer {
                name: "primary".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: "[history]\nlines = 3\n".to_string(),
            },
            ConfigLayer {
                name: "project".to_string(),
                path: Some(overlay_path.clone()),
                format: ConfigFormat::Toml,
                scope: ConfigScope::ProjectOverlay,
                trusted: false,
                text: fs::read_to_string(&overlay_path).unwrap(),
            },
        ])
        .unwrap();
    assert_eq!(initial_report.project_trust_prompts_announced, 1);
    let primary_events = service
        .event_log
        .as_ref()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        primary_events.iter().any(|event| {
            event.kind == EventKind::ConfigChanged
                && event.payload.contains(r#""trust_command":"/trust "#)
                && event
                    .payload
                    .contains(&json_escape(&root.to_string_lossy()))
        }),
        "{primary_events:?}"
    );
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let blocked_prompt = service
        .execute_agent_shell_command(&primary, "summarize this project")
        .unwrap();
    assert!(
        blocked_prompt.contains(r#""kind":"display""#)
            && blocked_prompt.contains("agent command error: project trust decision pending")
            && blocked_prompt.contains("(conflict)"),
        "{blocked_prompt}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("project trust pending:"), "{pane_text}");
    let collapsed_agent_wraps = pane_text.replace("\n▐ ", "");
    assert!(collapsed_agent_wraps.contains("/trust"), "{pane_text}");

    let trust = service
        .execute_agent_shell_command(&primary, "/trust")
        .unwrap();

    assert!(trust.contains(r#""kind":"mutated""#), "{trust}");
    assert!(trust.contains(r#""command":"trust""#), "{trust}");
    assert!(trust.contains("project trust granted"), "{trust}");
    assert!(trust.contains("persisted=true"), "{trust}");
    assert_eq!(service.terminal_history_limit(), 11);
    assert!(service.config_layers()[1].trusted);
    assert!(trust_path.exists());
    let persisted = ProjectTrustStore::load_from_file(&trust_path).unwrap();
    assert_eq!(persisted.get(&root).unwrap().state, TrustDecision::Trusted);
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime config applies safe terminal term to new panes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_config_applies_safe_terminal_term_to_new_panes() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nterm = \"screen-256color\"\n".to_string(),
        }])
        .unwrap();
    let output = std::env::temp_dir().join(format!("mez-runtime-term-test-{}", std::process::id()));
    let _ = fs::remove_file(&output);
    let command = format!("printf %s \"$TERM\" > {}", output.display());

    let started = service
        .create_window_with_pane_process(&primary, "term", true, Some(&command))
        .unwrap();
    let updates = poll_until_exit(&mut service);
    let observed = fs::read_to_string(&output).unwrap();

    assert_eq!(service.terminal_term(), "screen-256color");
    assert_eq!(started.pane_id, updates[0].pane_id);
    assert_eq!(observed, "screen-256color");
    let _ = fs::remove_file(output);
}

/// Verifies that a failed new-window process spawn is transactional. The window
/// is inserted before the PTY spawn path runs, so a spawn-layer failure must
/// restore the previous window list and active-window selection instead of
/// leaving a processless pane behind for later rendering or input dispatch.
#[test]
fn runtime_new_window_spawn_failure_rolls_back_window_creation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let active_window_id = service.session().active_window().unwrap().id.clone();
    service.session.shell = ResolvedShell::new(
        PathBuf::from("/tmp/mez-runtime-missing-shell-new-window"),
        ShellSource::FallbackBinSh,
    );

    let error = service
        .create_window_with_pane_process(&primary, "bad", true, None)
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Io);
    assert_eq!(service.session().windows().len(), 1);
    assert_eq!(
        service.session().active_window().unwrap().id,
        active_window_id
    );
    assert!(service.pane_processes().is_empty());
}

/// Verifies that a failed split process spawn restores the pre-split layout.
/// Existing panes are resized before the new pane process is started, so the
/// rollback must also return the active pane geometry to its original size and
/// leave only the already-running process tracked by the runtime.
#[test]
fn runtime_split_spawn_failure_rolls_back_layout_creation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let active_pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .clone();
    service.session.shell = ResolvedShell::new(
        PathBuf::from("/tmp/mez-runtime-missing-shell-split"),
        ShellSource::FallbackBinSh,
    );

    let error = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, None)
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Io);
    let window = service.session().active_window().unwrap();
    assert_eq!(window.panes().len(), 1);
    assert_eq!(window.active_pane().id, active_pane_id);
    assert_eq!(window.active_pane().size, Size::new(80, 24).unwrap());
    assert_eq!(service.pane_processes().tracked_pane_ids(), vec!["%1"]);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that terminal-command splits use the same transactional runtime
/// helper as direct mux/control splits. A failed process spawn must restore the
/// pre-split layout instead of leaving a processless command-created pane with
/// stale geometry behind.
#[test]
fn runtime_terminal_command_split_spawn_failure_rolls_back_layout_creation() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let active_pane_id = service
        .session()
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .clone();
    service.session.shell = ResolvedShell::new(
        PathBuf::from("/tmp/mez-runtime-missing-shell-command-split"),
        ShellSource::FallbackBinSh,
    );

    let error = service
        .execute_terminal_command(&primary, "split-window")
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Io);
    let window = service.session().active_window().unwrap();
    assert_eq!(window.panes().len(), 1);
    assert_eq!(window.active_pane().id, active_pane_id);
    assert_eq!(window.active_pane().size, Size::new(80, 24).unwrap());
    assert_eq!(service.pane_processes().tracked_pane_ids(), vec!["%1"]);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies runtime control initialize can reattach primary without existing primary.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_control_initialize_can_reattach_primary_without_existing_primary() {
    let mut service = test_runtime_service();
    let mut connection = ControlConnectionState::new(true, true);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"primary","requested_version":1,"client_name":"mez-cli","client":{"name":"mez-cli","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}}}}"#,
    );
    let get =
        encode_control_body(r#"{"jsonrpc":"2.0","id":"get","method":"session/get","params":{}}"#);
    let mut input = initialize;
    input.extend_from_slice(&get);

    let (output, consumed) = service
        .handle_control_input_for_connection(&input, 4096, &mut connection)
        .unwrap();
    let (first_body, first_consumed) = decode_control_frame(&output, 4096).unwrap();
    let (second_body, _) = decode_control_frame(&output[first_consumed..], 4096).unwrap();

    assert_eq!(consumed, input.len());
    assert!(first_body.contains(r#""granted_role":"primary""#));
    assert!(second_body.contains(r#""session_id":"$1""#));
    assert!(connection.caller_client_id().is_some());
    assert!(service.session().primary_client_id().is_some());
    assert_eq!(
        service.session().authoritative_size,
        Size::new(100, 40).unwrap()
    );
    assert!(service.last_attach_at_unix_seconds().is_some());
}

/// Verifies that the live control attach path applies the primary terminal size
/// to an already-started initial pane. The daemon starts the first pane before
/// the CLI sends `control/initialize`, so the initialize side effect must use
/// the same resize/sync path as direct attaches instead of only recording the
/// authoritative size.
#[test]
fn runtime_control_initialize_resizes_started_initial_pane_for_primary_terminal() {
    let mut service = test_runtime_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let initial_descriptor = service
        .tracked_pane_descriptors()
        .into_iter()
        .find(|descriptor| descriptor.pane_id.as_str() == "%1")
        .unwrap();
    assert_eq!(initial_descriptor.size, Size::new(80, 22).unwrap());

    let mut connection = ControlConnectionState::new(true, true);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"primary","requested_version":1,"client_name":"mez-cli","client":{"name":"mez-cli","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}}}}"#,
    );

    let (output, consumed) = service
        .handle_control_input_for_connection(&initialize, 4096, &mut connection)
        .unwrap();
    let (body, _) = decode_control_frame(&output, 4096).unwrap();

    assert_eq!(consumed, initialize.len());
    assert!(body.contains(r#""granted_role":"primary""#), "{body}");
    assert_eq!(
        service.session().authoritative_size,
        Size::new(100, 40).unwrap()
    );
    assert_eq!(
        service.session().active_window().unwrap().size,
        Size::new(100, 40).unwrap()
    );
    assert_eq!(
        service
            .session()
            .active_window()
            .unwrap()
            .active_pane()
            .size,
        Size::new(100, 40).unwrap()
    );
    let resized_descriptor = service
        .tracked_pane_descriptors()
        .into_iter()
        .find(|descriptor| descriptor.pane_id.as_str() == "%1")
        .unwrap();
    assert_eq!(resized_descriptor.size, Size::new(100, 38).unwrap());
    assert_eq!(
        service.pane_screen("%1").unwrap().size(),
        Size::new(100, 38).unwrap()
    );

    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let config = service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(100, 40).unwrap(),
            &config,
        )
        .unwrap()
        .unwrap();
    let region = view.agent_prompt_region.unwrap();
    assert_eq!(view.lines.len(), 40);
    assert_eq!(region.columns, 100);
    assert_eq!(region.rows, 38);
    assert!(
        view.cursor_row >= 38,
        "agent prompt cursor should render at attached terminal bottom: {view:?}"
    );

    service.pane_processes_mut().terminate_all().unwrap();
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
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(30, 4).unwrap(),
            &config,
        )
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

/// Verifies observer `control/initialize` requests are visible immediately.
///
/// The control dispatcher already creates the pending observer record. The
/// runtime side effect must also log the request, write a visible active-pane
/// status line with the request id, and make `:list-observers` usable as the
/// same pager/action surface as `:choose-observer`.
#[test]
fn runtime_control_initialize_observer_logs_and_lists_pending_request() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut connection = ControlConnectionState::new(true, true);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"observer","requested_version":1,"client_name":"observer-cli","client":{"name":"observer-cli","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}}}}"#,
    );

    let (output, consumed) = service
        .handle_control_input_for_connection(&initialize, 4096, &mut connection)
        .unwrap();
    let (body, _) = decode_control_frame(&output, 4096).unwrap();
    let observer = service.session().observers().first().unwrap();
    let observer_id = observer.id.to_string();

    assert_eq!(consumed, initialize.len());
    assert!(
        body.contains(r#""granted_role":"pending_observer""#),
        "{body}"
    );
    assert!(body.contains(&observer_id), "{body}");
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events.iter().any(|event| {
            event.kind == EventKind::ObserverRequested && event.payload.contains(&observer_id)
        }),
        "{events:?}"
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .visible_lines()
        .join("\n");
    assert!(
        pane_text.contains(&format!("observer request {observer_id}")),
        "{pane_text}"
    );

    service
        .execute_attached_display_command(&primary, "list-observers")
        .unwrap();
    let overlay = service
        .primary_display_overlay
        .as_ref()
        .expect("list-observers should open a command display overlay");
    assert!(
        overlay
            .selections
            .iter()
            .any(|selection| selection.command == format!("approve-observer -t {observer_id}")),
        "{overlay:?}"
    );
    assert!(
        overlay
            .selections
            .iter()
            .any(|selection| selection.command == format!("reject-observer -t {observer_id}")),
        "{overlay:?}"
    );
}

/// Verifies that the runtime service refreshes the filesystem registry when a
/// control connection claims the primary role. Without this write, `mez list`
/// could advertise a detached session as primary-available after an attach, and
/// default attach resolution could pick that busy session instead of another
/// attachable live daemon.
#[test]
fn runtime_control_initialize_persists_attached_registry_state() {
    let root = std::env::temp_dir().join(format!(
        "mez-runtime-registry-initialize-{}-{:?}",
        std::process::id(),
        thread::current().id()
    ));
    let _ = fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), effective_uid());
    let mut service = test_runtime_service();
    service.set_session_registry(registry.clone());
    service.persist_registry_update().unwrap();
    let mut connection = ControlConnectionState::new(true, true);
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"primary","requested_version":1,"client_name":"mez-cli","client":{"name":"mez-cli","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}}}}"#,
    );

    service
        .handle_control_input_for_connection(&initialize, 4096, &mut connection)
        .unwrap();

    let records = registry.list().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].session_id, service.session().id.to_string());
    assert_eq!(records[0].state, RegistrySessionState::Running);
    assert!(!records[0].primary_available);
    assert_eq!(records[0].authoritative_columns, 100);
    assert_eq!(records[0].authoritative_rows, 40);
    assert!(records[0].last_attach_at_unix_seconds.is_some());

    let _ = fs::remove_dir_all(root);
}

/// Verifies that primary detach actions issued by the attached terminal loop
/// update the registry immediately. This covers the default prefix escape path,
/// which mutates runtime state outside the framed control request loop and
/// otherwise could leave `mez list` showing the session as still busy.
#[test]
fn attached_terminal_detach_action_persists_available_registry_state() {
    let root = std::env::temp_dir().join(format!(
        "mez-runtime-registry-detach-action-{}-{:?}",
        std::process::id(),
        thread::current().id()
    ));
    let _ = fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), effective_uid());
    let mut service = test_runtime_service();
    service.set_session_registry(registry.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let busy_records = registry.list().unwrap();
    assert_eq!(busy_records.len(), 1);
    assert!(!busy_records[0].primary_available);
    let detach_step = AttachedTerminalClientStepPlan {
        actions: vec![TerminalClientLoopAction::ExecuteMux(
            MuxAction::DetachPrimaryClient,
        )],
        output_lines: Vec::new(),
        output_line_style_spans: Vec::new(),
        input_hangup: false,
        output_hangup: false,
        error_roles: Vec::new(),
    };

    service
        .apply_attached_terminal_step_plan(&primary, &detach_step)
        .unwrap();

    let records = registry.list().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].state, RegistrySessionState::Detached);
    assert!(records[0].primary_available);
    assert_eq!(records[0].last_attach_at_unix_seconds, Some(120));

    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime service registry plan preserves authoritative detached size.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_registry_plan_preserves_authoritative_detached_size() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();

    service
        .detach_primary(&primary, Size::new(132, 43).unwrap())
        .unwrap();

    let RuntimeRegistryUpdatePlan::Upsert(record) = service.registry_update_plan() else {
        panic!("detached live service must plan a registry upsert");
    };
    assert_eq!(record.state, RegistrySessionState::Detached);
    assert_eq!(record.last_attach_at_unix_seconds, Some(120));
    assert!(record.primary_available);
    assert_eq!(record.authoritative_columns, 132);
    assert_eq!(record.authoritative_rows, 43);
}

/// Verifies runtime service kill requires force and plans registry removal.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_kill_requires_force_and_plans_registry_removal() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let error = service.kill_session(&primary, false).unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);

    service.kill_session(&primary, true).unwrap();

    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Killed);
    assert!(service.session().windows().is_empty());
    assert!(matches!(
        service.registry_update_plan(),
        RuntimeRegistryUpdatePlan::Remove { .. }
    ));

    let error = service
        .attach_primary("late", true, Size::new(80, 24).unwrap(), 200)
        .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
}

/// Verifies the interactive `:exit` command shuts down the runtime service.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_exit_command_kills_session_and_plans_registry_removal() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    service.execute_terminal_command(&primary, "exit").unwrap();

    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Killed);
    assert!(service.session().windows().is_empty());
    assert!(matches!(
        service.registry_update_plan(),
        RuntimeRegistryUpdatePlan::Remove { .. }
    ));
}

/// Verifies runtime service owns session memory and clears it on kill.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_owns_session_memory_and_clears_it_on_kill() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    service
        .upsert_session_memory(MemoryRecord::new_with_defaults(
            "runtime-note",
            crate::memory::MemoryScope::Session {
                session_id: service.session().id.to_string(),
            },
            120,
            120,
            crate::memory::MemorySource::User,
            20,
            "prefer focused regression tests",
        ))
        .unwrap();

    assert_eq!(service.memory_records().len(), 1);
    assert_eq!(
        service
            .session_memory()
            .inspect("runtime-note")
            .unwrap()
            .content,
        "prefer focused regression tests"
    );

    service.kill_session(&primary, true).unwrap();

    assert!(service.memory_records().is_empty());
}

/// Verifies runtime service starts initial pane process through resolved shell.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_starts_initial_pane_process_through_resolved_shell() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let started = service.start_initial_pane_process(Some("true")).unwrap();

    assert_eq!(started.session_id, service.session().id.to_string());
    assert_eq!(started.window_id, "@1");
    assert_eq!(started.pane_id, "%1");
    assert!(started.primary_pid > 0);
    assert_eq!(
        service.pane_processes().primary_pid("%1"),
        Some(started.primary_pid)
    );
    assert!(matches!(
        started.registry_update,
        RuntimeRegistryUpdatePlan::Upsert(_)
    ));

    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events
            .iter()
            .any(|event| event.kind == EventKind::PaneChanged
                && event.payload.contains(r#""process_state":"running""#))
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == EventKind::Diagnostic
                && event.payload.contains("fell back to /bin/sh"))
    );

    let _ = primary;
    poll_until_exit(&mut service);
}

/// Verifies that runtime services can hand a running pane process to an async
/// owner and restore it if the handoff is cancelled. The service keeps session
/// and terminal metadata while only the process/PTY handle leaves the
/// synchronous manager.
#[test]
fn runtime_service_can_handoff_running_pane_process_to_async_owner() {
    let mut service = test_runtime_service();
    let started = service
        .start_initial_pane_process(Some("sleep 30"))
        .unwrap();

    let process = service
        .take_running_pane_process_for_async_owner(&started.pane_id)
        .unwrap();

    assert!(!service.pane_processes().contains_pane(&started.pane_id));
    let window = service.session().active_window().unwrap();
    let pane_state = service.runtime_control_pane_state_json(window, window.active_pane());
    assert!(
        pane_state.contains(&format!(r#""primary_pid":{}"#, started.primary_pid)),
        "{pane_state}"
    );
    assert!(
        pane_state.contains(r#""process_state":"running""#),
        "{pane_state}"
    );
    service
        .apply_pane_foreground_process_event(
            &started.pane_id,
            "vim",
            started.primary_pid.saturating_add(1),
            Some("/tmp/mez-async-cwd".to_string()),
        )
        .unwrap();
    assert_eq!(
        service
            .pane_current_working_directory(&started.pane_id)
            .as_deref(),
        Some(Path::new("/tmp/mez-async-cwd"))
    );
    assert_eq!(
        service
            .restore_running_pane_process_from_async_owner(&started.pane_id, process)
            .unwrap(),
        started.primary_pid
    );
    assert_eq!(
        service.pane_processes().primary_pid(&started.pane_id),
        Some(started.primary_pid)
    );
    service
        .pane_processes_mut()
        .terminate_pane_with_grace(&started.pane_id, Duration::from_millis(50))
        .unwrap();
}

/// Verifies stale async process-exit events cannot close a pane after its id is reused.
///
/// `load-layout` can restart a fresh process for a restored pane id while an
/// older async watcher still holds a late exit event for the previous process.
/// The runtime must compare the event's primary PID with the currently live
/// primary PID and ignore mismatches so the new pane generation remains live.
#[test]
fn runtime_service_ignores_stale_process_exit_with_mismatched_primary_pid() {
    let mut service = test_runtime_service();
    let started = service
        .start_initial_pane_process(Some("sleep 30"))
        .unwrap();
    let stale_primary_pid = started.primary_pid.saturating_add(1);

    let update = service
        .apply_pane_process_exit_event(
            &started.pane_id,
            stale_primary_pid,
            crate::process::PaneExitStatus {
                code: Some(0),
                signal: None,
                success: true,
            },
        )
        .unwrap();

    assert_eq!(update, None);
    assert_eq!(
        service.pane_processes().primary_pid(&started.pane_id),
        Some(started.primary_pid)
    );
    assert!(
        service
            .session()
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .any(|pane| pane.id.as_str() == started.pane_id.as_str() && pane.live)
    );
    service
        .pane_processes_mut()
        .terminate_pane_with_grace(&started.pane_id, Duration::from_millis(50))
        .unwrap();
}

/// Verifies runtime service restarts restored panes with fresh primary pids.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_restarts_restored_panes_with_fresh_primary_pids() {
    let mut original = test_session();
    let primary = original.attach_primary("primary", true).unwrap();
    original
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let payload = crate::snapshot::SessionSnapshotPayload::from_session(&original);
    let restored = Session::from_snapshot_payload(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        &payload,
    )
    .unwrap();
    assert!(
        restored
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .all(|pane| !pane.live)
    );
    let mut service = RuntimeSessionService::with_event_log(
        restored,
        PathBuf::from("/tmp/mez-1000/restored.sock"),
        100,
        10,
        1024,
    )
    .unwrap();

    let starts = service
        .restart_restored_pane_processes(Some("true"))
        .unwrap();

    assert_eq!(starts.len(), 2);
    assert!(starts.iter().all(|start| start.primary_pid > 0));
    assert_ne!(starts[0].primary_pid, starts[1].primary_pid);
    assert_eq!(service.pane_processes().len(), 2);
    assert!(starts.iter().all(|start| {
        service.pane_readiness_state(&start.pane_id) == PaneReadinessState::PromptCandidate
    }));
    assert!(
        service
            .session()
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .all(|pane| pane.live)
    );
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(
        events
            .iter()
            .any(|event| event.payload.contains(r#""restarted":true"#))
    );
    poll_until_exit(&mut service);
}

/// Verifies runtime service restarts restored panes at the rendered PTY size
/// instead of the raw saved layout pane size.
///
/// Restored shells must start with the same content-area dimensions used by
/// normal pane creation so cursor placement and shell redraws stay aligned with
/// framed and split layouts immediately after `load-layout`.
#[test]
fn runtime_service_restarts_restored_panes_with_rendered_process_sizes() {
    let mut original = test_session();
    let primary = original.attach_primary("primary", true).unwrap();
    original
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let payload = crate::snapshot::SessionSnapshotPayload::from_session(&original);
    let restored = Session::from_snapshot_payload(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        &payload,
    )
    .unwrap();
    let restored_pane_sizes: Vec<Size> = restored
        .windows()
        .iter()
        .flat_map(|window| window.panes().iter().map(|pane| pane.size))
        .collect();
    let mut service = RuntimeSessionService::with_event_log(
        restored,
        PathBuf::from("/tmp/mez-1000/restored-size-alignment.sock"),
        100,
        10,
        1024,
    )
    .unwrap();

    let starts = service
        .restart_restored_pane_processes(Some("true"))
        .unwrap();
    let started_sizes: Vec<Size> = starts.iter().map(|start| start.size).collect();
    let tracked_sizes: Vec<Size> = service
        .tracked_pane_descriptors()
        .into_iter()
        .map(|descriptor| descriptor.size)
        .collect();

    assert_eq!(started_sizes.len(), restored_pane_sizes.len());
    assert_eq!(started_sizes, tracked_sizes);
    assert_ne!(started_sizes, restored_pane_sizes);
    poll_until_exit(&mut service);
}

/// Verifies layout-loaded panes drain their first prompt output before redraw.
///
/// `:load-layout` synchronously recreates panes and then asks the attached
/// client for a full redraw. The restored shell prompt must already be in the
/// pane screen at that point, otherwise users see blank panes until the next
/// keypress happens to poll shell output.
#[test]
fn runtime_service_restarts_restored_panes_drain_initial_prompt_output() {
    let original = test_session();
    let payload = crate::snapshot::SessionSnapshotPayload::from_session(&original);
    let restored = Session::from_snapshot_payload(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        &payload,
    )
    .unwrap();
    let pane_id = restored.active_window().unwrap().active_pane().id.to_string();
    let mut service = RuntimeSessionService::with_event_log(
        restored,
        PathBuf::from("/tmp/mez-1000/restored-prompt-drain.sock"),
        100,
        10,
        1024,
    )
    .unwrap();

    let starts = service
        .restart_restored_pane_processes(Some("printf 'restored-ps1$ '; sleep 30"))
        .unwrap();
    let visible = service.pane_screen(&pane_id).unwrap().visible_lines().join("\n");

    assert_eq!(starts.len(), 1);
    assert!(visible.contains("restored-ps1$"), "{visible:?}");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies runtime snapshot resume treats saved pane working directories as
/// best-effort metadata when fresh pane process startup cannot use them.
///
/// A snapshot can contain a directory that existed during restore planning but
/// becomes unusable when the fresh pane process starts. Resume must keep the
/// restored layout and names, retry the pane from the user's home directory,
/// and leave the restored session usable instead of unwinding after topology
/// installation.
#[test]
fn runtime_service_restarts_restored_panes_from_home_when_saved_cwd_fails() {
    let root = temp_root("runtime-restored-pane-cwd-fallback");
    let inaccessible_cwd = root.join("inaccessible-cwd");
    fs::create_dir_all(&inaccessible_cwd).unwrap();
    let original_permissions = fs::metadata(&inaccessible_cwd).unwrap().permissions();
    fs::set_permissions(&inaccessible_cwd, fs::Permissions::from_mode(0o000)).unwrap();
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
    assert!(home.is_dir());

    let original = test_session();
    let mut payload = crate::snapshot::SessionSnapshotPayload::from_session(&original);
    payload.name = "restored-name".to_string();
    payload.windows[0].name = "saved-window".to_string();
    payload.windows[0].panes[0].title = "saved-pane".to_string();
    payload.windows[0].panes[0].current_working_directory =
        Some(inaccessible_cwd.to_string_lossy().into_owned());
    let restored = Session::from_snapshot_payload(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        &payload,
    )
    .unwrap();
    let pane_id = restored.active_window().unwrap().active_pane().id.clone();
    let mut service = RuntimeSessionService::with_event_log(
        restored,
        PathBuf::from("/tmp/mez-1000/restored-cwd-fallback.sock"),
        100,
        10,
        1024,
    )
    .unwrap();

    let starts = service
        .restart_restored_pane_processes(Some("true"))
        .unwrap();

    assert_eq!(starts.len(), 1);
    assert_eq!(service.session().name, "restored-name");
    assert_eq!(service.session().active_window().unwrap().name, "saved-window");
    assert_eq!(
        service.session().active_window().unwrap().active_pane().title,
        "saved-pane"
    );
    assert_eq!(
        service.pane_current_working_directory(pane_id.as_str()).as_deref(),
        Some(home.as_path())
    );
    let events = service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains("snapshot resume pane cwd unavailable; retrying from home")
    }));

    poll_until_exit(&mut service);
    fs::set_permissions(&inaccessible_cwd, original_permissions).unwrap();
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime service starts processes for created windows and panes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_service_starts_processes_for_created_windows_and_panes() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();

    let window_start = service
        .create_window_with_pane_process(&primary, "build", true, Some("true"))
        .unwrap();
    assert_eq!(window_start.window_id, "@2");
    assert_eq!(window_start.pane_id, "%2");
    assert_eq!(
        service.pane_processes().primary_pid(&window_start.pane_id),
        Some(window_start.primary_pid)
    );

    let split_start = service
        .split_pane_with_process(&primary, SplitDirection::Vertical, Some("true"))
        .unwrap();
    assert_eq!(split_start.window_id, "@2");
    assert_eq!(split_start.pane_id, "%3");
    assert_eq!(
        service.pane_processes().primary_pid(&split_start.pane_id),
        Some(split_start.primary_pid)
    );

    let mut exited = poll_until_exit(&mut service).len();
    while exited < 2 {
        exited += poll_until_exit(&mut service).len();
    }
    assert_eq!(exited, 2);
}

/// Verifies runtime applies attached terminal step actions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn runtime_applies_attached_terminal_step_actions() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let step = AttachedTerminalClientStepPlan {
        actions: vec![
            TerminalClientLoopAction::ForwardToPane(b"hello\n".to_vec()),
            TerminalClientLoopAction::ExecuteMux(MuxAction::SplitPaneVertical),
            TerminalClientLoopAction::ExecuteMux(MuxAction::FocusPane(PaneFocusDirection::Left)),
            TerminalClientLoopAction::EnterPrefixKeyMode,
            TerminalClientLoopAction::ExecuteMux(MuxAction::EnterCopyMode),
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

    assert_eq!(report.forwarded_bytes, 6);
    assert_eq!(report.mux_actions_applied, 3);
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert!(report.unsupported_actions.is_empty());
    assert!(!service.active_copy_modes.is_empty());
    assert_eq!(service.session().windows()[0].panes().len(), 2);
    assert_eq!(service.pane_processes().len(), 2);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies runtime keeps a lone escape key as pending prefix state until the
/// next terminal action consumes it.
///
/// This regression scenario protects the split between entering prefix-key
/// state and explicitly requesting the command prompt through the prefix table.
#[test]
fn runtime_applies_lone_prefix_key_as_pending_state() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();

    let prefix_report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::EnterPrefixKeyMode],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(prefix_report.view_refresh_required);
    assert!(service.primary_prefix_key_pending);
    assert!(service.primary_prompt_input.is_none());
    assert!(
        service
            .terminal_client_loop_config(TerminalClientLoopConfig::default())
            .unwrap()
            .prefix_key_pending
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ExecuteMux(
                    MuxAction::EnterCommandPrompt,
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(!service.primary_prefix_key_pending);
    assert!(service.primary_prompt_input.is_some());
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies the terminal `copy-mode` command opens over the same live pane
/// viewport height that the attached-terminal copy-mode key path uses.
///
/// The command previously subtracted one row from the pane descriptor before
/// building `CopyMode`, which made the first copy-mode viewport start one line
/// below the live pane when no frame or prompt row was actually present.
#[test]
fn runtime_copy_mode_command_preserves_live_viewport_height() {
    let mut service = test_runtime_service_with_size(Size::new(20, 4).unwrap());
    service.window_frames_enabled = false;
    service.pane_frames_enabled = false;
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"one\ntwo\nthree\nfour");
    service.pane_screens.insert(pane_id.clone(), screen);

    service
        .execute_terminal_command(&primary, "copy-mode")
        .unwrap();

    let visible = service
        .active_copy_modes
        .get(&pane_id)
        .unwrap()
        .visible_lines()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(visible, vec!["one", "two", "three", "four"]);
}

/// Verifies mouse drag selection copies the visible alternate-screen grid.
///
/// Full-screen terminal applications are intentionally excluded from normal
/// history and copy-mode buffers, but an explicit mouse drag is a user copy
/// operation over the displayed pane body. This regression protects less/nano
/// style alternate-screen copying without making alternate-screen content part
/// of scrollback or default agent context.
#[test]
fn runtime_mouse_drag_copies_visible_alternate_screen_content() {
    let mut service = test_runtime_service_with_size(Size::new(20, 4).unwrap());
    service.window_frames_enabled = false;
    service.pane_frames_enabled = false;
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 10).unwrap();
    screen.feed(b"normal-only\n\x1b[?1049halpha beta\nsecond row");
    assert!(screen.alternate_screen_active());
    assert!(!screen.normal_content_lines().iter().any(|line| line.contains("alpha beta")));
    service.pane_screens.insert(pane_id.clone(), screen);

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::HandleMouse(MouseAction::CopySelectionStart(
                        CopyPosition { line: 0, column: 0 },
                    )),
                    TerminalClientLoopAction::HandleMouse(MouseAction::CopySelectionFinish(
                        CopyPosition { line: 1, column: 6 },
                    )),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(service.paste_buffers.get("mouse"), Some("alpha beta\nsecond"));
    assert!(!service.active_copy_modes.contains_key(&pane_id));
}

/// Verifies copy-mode key navigation marks the attached view dirty without
/// invalidating the retained terminal frame. Copy-mode scrolling only changes
/// pane content and cursor placement, so it should use the diff renderer rather
/// than clearing the whole attached terminal.
#[test]
fn runtime_copy_mode_key_navigation_requests_diff_refresh() {
    let mut service = test_runtime_service_with_size(Size::new(20, 4).unwrap());
    service.window_frames_enabled = false;
    service.pane_frames_enabled = false;
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 20).unwrap();
    screen.feed(b"one\ntwo\nthree\nfour\nfive\nsix");
    service.pane_screens.insert(pane_id.clone(), screen);
    service.ensure_active_copy_mode(&pane_id).unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleCopyMode(
                    crate::terminal::CopyModeKeyAction::PageUp,
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
    assert!(service.active_copy_modes.contains_key(&pane_id));
}

/// Verifies mouse-wheel history scrolling updates the pane through a diff
/// refresh. Scrollback movement changes the copy-mode viewport but not the
/// terminal geometry, so preserving the retained output frame avoids visible
/// flicker over slower terminal links.
#[test]
fn runtime_mouse_history_scroll_requests_diff_refresh() {
    let mut service = test_runtime_service_with_size(Size::new(20, 4).unwrap());
    service.window_frames_enabled = false;
    service.pane_frames_enabled = false;
    let primary = service
        .attach_primary("primary", true, Size::new(20, 4).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    let mut screen = TerminalScreen::new(Size::new(20, 4).unwrap(), 20).unwrap();
    screen.feed(b"one\ntwo\nthree\nfour\nfive\nsix");
    service.pane_screens.insert(pane_id.clone(), screen);

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::ScrollHistory {
                        lines: -3,
                        position: CopyPosition { line: 1, column: 1 },
                    },
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
    assert!(service.active_copy_modes.contains_key(&pane_id));
    assert!(service.scrollback_copy_mode_panes.contains(&pane_id));
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
            .primary_error_status_overlay
            .as_deref()
            .is_some_and(|message| message.contains("cannot split vertically")),
        "{:?}",
        service.primary_error_status_overlay
    );

    let dismiss = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(dismiss.mux_actions_applied, 0);
    assert!(dismiss.view_refresh_required);
    assert!(dismiss.full_redraw_required);
    assert_eq!(service.session().windows()[0].panes().len(), 1);
    assert!(service.pane_processes().is_empty());
    assert!(service.primary_error_status_overlay.is_none());

    let retried = service
        .apply_attached_terminal_step_plan(&primary, &step)
        .unwrap();

    assert_eq!(retried.mux_actions_applied, 0);
    assert!(service.primary_error_status_overlay.is_some());
    assert_eq!(service.session().windows()[0].panes().len(), 1);
    assert!(service.pane_processes().is_empty());
}

/// Verifies that command display output is owned by runtime state instead of a
/// nested terminal loop. The modal overlay must render through the normal
/// primary client view, consume user input while active, and clear on Escape or
/// `q` without forwarding those bytes into the active pane.
#[test]
fn runtime_primary_display_overlay_renders_and_clears_via_terminal_step() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(40, 6).unwrap(), 120)
        .unwrap();
    let pane_id = service.active_pane_id().unwrap().to_string();
    service
        .apply_pane_output_bytes(pane_id, b"prompt$ ".to_vec())
        .unwrap();
    let base_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(40, 6).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(base_view.cursor_visible);
    service
        .show_primary_display_overlay(vec![
            "first display line".to_string(),
            "second display line".to_string(),
        ])
        .unwrap();

    let overlay_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(40, 6).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(overlay_view.lines[0].trim_end(), "mezzanine command output");
    assert!(
        overlay_view
            .lines
            .iter()
            .any(|line| line.contains("first display line")),
        "{:?}",
        overlay_view.lines
    );
    assert!(!overlay_view.cursor_visible);

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
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);

    let cleared_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(40, 6).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        !cleared_view
            .lines
            .iter()
            .any(|line| line.contains("mezzanine command output")),
        "{:?}",
        cleared_view.lines
    );
    assert!(cleared_view.cursor_visible);
    assert_eq!(cleared_view.cursor_row, base_view.cursor_row);
    assert_eq!(cleared_view.cursor_column, base_view.cursor_column);

    service
        .show_primary_display_overlay(vec!["third display line".to_string()])
        .unwrap();
    let quit = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(b"q".to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(quit.forwarded_bytes, 0);
    assert!(quit.view_refresh_required);
    assert!(service.primary_display_overlay.is_none());
}

/// Verifies keyboard movement inside a primary command-output pager refreshes
/// through the retained-frame diff path.
///
/// Navigating a selectable pager row only changes the active highlight and
/// optional viewport offset. It must not invalidate the whole attached output
/// frame, otherwise remote terminals flicker during routine list navigation.
#[test]
fn runtime_primary_display_overlay_keyboard_navigation_requests_diff_refresh() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work", false, None)
        .unwrap();

    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.active_selection_index),
        Some(0)
    );
    let initial_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let initial_active_row = initial_view
        .lines
        .iter()
        .position(|line| line.starts_with("> "))
        .expect("overlay should show an active selector gutter");
    assert!(
        initial_view
            .lines
            .iter()
            .enumerate()
            .any(|(index, line)| index != initial_active_row && line.starts_with("  ")),
        "{initial_view:?}"
    );

    let report = service
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

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.active_selection_index),
        Some(1)
    );
    let moved_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let moved_active_row = moved_view
        .lines
        .iter()
        .position(|line| line.starts_with("> "))
        .expect("overlay should keep an active selector gutter after navigation");
    assert_ne!(moved_active_row, initial_active_row, "{moved_view:?}");
    assert!(
        moved_view
            .lines
            .iter()
            .enumerate()
            .any(|(index, line)| index != moved_active_row && line.starts_with("  ")),
        "{moved_view:?}"
    );
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies mouse-wheel scrolling inside a primary command-output pager uses a
/// light view refresh instead of a full terminal-frame redraw.
///
/// The overlay renderer already produces a complete next view for the changed
/// rows, so the attach client can keep diffing against the retained frame.
#[test]
fn runtime_primary_display_overlay_mouse_scroll_requests_diff_refresh() {
    let mut service = test_runtime_service_with_size(Size::new(40, 6).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(40, 6).unwrap(), 120)
        .unwrap();
    service
        .show_primary_display_overlay(
            (0..20)
                .map(|index| format!("display line {index:02}"))
                .collect(),
        )
        .unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::ScrollDisplayOverlay { lines: 2 },
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
    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .map(|overlay| overlay.scroll_offset),
        Some(2)
    );
}

/// Verifies forward text search inside a primary command-output pager, including
/// empty-query repeat and wraparound back to the first matching line.
#[test]
fn runtime_primary_display_overlay_search_repeats_and_wraps() {
    let mut service = test_runtime_service_with_size(Size::new(80, 10).unwrap());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 10).unwrap(), 120)
        .unwrap();
    service
        .show_primary_display_overlay(vec![
            "alpha opening".to_string(),
            "needle first".to_string(),
            "middle text".to_string(),
            "needle second".to_string(),
            "closing text".to_string(),
        ])
        .unwrap();

    let initial_search = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"needle".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\r".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(initial_search.forwarded_bytes, 0);
    assert!(initial_search.view_refresh_required);
    assert!(!initial_search.full_redraw_required);
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.search_query.as_deref()),
        Some("needle")
    );
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.search_match.map(|search_match| search_match.line_index)),
        Some(1)
    );

    let next_match = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\r".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(next_match.forwarded_bytes, 0);
    assert!(next_match.view_refresh_required);
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.search_match.map(|search_match| search_match.line_index)),
        Some(3)
    );
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.search_status.as_deref()),
        None
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\r".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.search_match.map(|search_match| search_match.line_index)),
        Some(1)
    );
    assert_eq!(
        service
            .primary_display_overlay
            .as_ref()
            .and_then(|overlay| overlay.search_status.as_deref()),
        None
    );

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"/".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"absent".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\r".to_vec()),
                ],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    let overlay = service.primary_display_overlay.as_ref().unwrap();
    assert_eq!(overlay.search_match.map(|search_match| search_match.line_index), Some(1));
    assert_eq!(
        overlay.search_status.as_deref(),
        Some("pattern not found: absent")
    );
}

/// Verifies that command chooser output rendered in the primary overlay is not
/// inert text. Rows that advertise an `action=` command must retain selectable
/// metadata so a mouse click can execute the command through the normal
/// terminal command path and then close or replace the overlay.
#[test]
fn runtime_primary_display_overlay_executes_selectable_command_rows() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work", false, None)
        .unwrap();

    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    let overlay = service
        .primary_display_overlay
        .as_ref()
        .expect("choose-window should open a command display overlay");
    let work_selection = overlay
        .selections
        .iter()
        .find(|selection| selection.command == "select-window -t @2")
        .expect("work window row should advertise a selectable action");
    let clicked_row = work_selection.line_index.saturating_add(1);
    let clicked_column = work_selection.start_column.saturating_add(2);

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectDisplayOverlay {
                        position: CopyPosition {
                            line: clicked_row,
                            column: clicked_column,
                        },
                    },
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
    assert!(service.primary_display_overlay.is_none());
    assert_eq!(service.session().active_window().unwrap().name, "work");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies selectable command rows exposed by the primary display overlay can
/// be chosen from the keyboard. Mouse clicks and keyboard Enter must execute the
/// same stored command metadata so chooser output does not depend on scraping
/// the rendered text.
#[test]
fn runtime_primary_display_overlay_executes_keyboard_selected_command_rows() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .create_window_with_pane_process(&primary, "work", false, None)
        .unwrap();

    service
        .execute_attached_display_command(&primary, "choose-window")
        .unwrap();
    assert!(service.primary_display_overlay.is_some());
    assert_eq!(service.session().active_window().unwrap().name, "0");

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"\x1b[B".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\r".to_vec()),
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
    assert!(report.view_refresh_required);
    assert!(report.full_redraw_required);
    assert!(service.primary_display_overlay.is_none());
    assert_eq!(service.session().active_window().unwrap().name, "work");
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies command overlays can expose multiple selectable choices on one row.
/// The user should be able to distinguish routine and destructive choices by
/// color, move between them with selector keys, and execute the active choice
/// without scraping command text out of the rendered row.
#[test]
fn runtime_primary_display_overlay_executes_multiple_action_chips() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .paste_buffers
        .set_with_origin("main", "pasted\n", Some("test".to_string()))
        .unwrap();
    service.active_paste_buffer = Some("main".to_string());

    service
        .execute_attached_display_command(&primary, "choose-buffer")
        .unwrap();
    let overlay = service
        .primary_display_overlay
        .as_ref()
        .expect("choose-buffer should open a command display overlay");
    let paste = overlay
        .selections
        .iter()
        .position(|selection| selection.command == "paste-buffer -b main")
        .expect("buffer row should expose a paste choice");
    let delete = overlay
        .selections
        .iter()
        .position(|selection| selection.command == "delete-buffer main")
        .expect("buffer row should expose a delete choice");
    assert_eq!(delete, paste.saturating_add(1));

    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    let row = view
        .lines
        .iter()
        .position(|line| line.contains("[paste]") && line.contains("[delete]"))
        .expect("overlay should render compact action chips");
    assert!(view.lines[row].contains("[paste]"));
    assert!(view.lines[row].contains("[delete]"));
    assert!(
        view.line_style_spans[row].iter().any(|span| {
            span.length == "[paste]".len()
                && !span.rendition.inverse
                && span.rendition.background
                    == Some(service.ui_theme.colors.agent_reasoning.background)
                && span.rendition.foreground
                    == Some(service.ui_theme.colors.agent_reasoning.foreground)
                && span.rendition.bold
                && span.rendition.underline
        }),
        "{view:?}"
    );
    assert!(
        view.line_style_spans[row].iter().any(|span| {
            span.length == "[delete]".len()
                && !span.rendition.inverse
                && span.rendition.background.is_none()
                && span.rendition.foreground
                    == Some(service.ui_theme.colors.agent_status_failed.foreground)
                && span.rendition.bold
                && span.rendition.underline
        }),
        "{view:?}"
    );

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![
                    TerminalClientLoopAction::ForwardToPane(b"\x1b[C".to_vec()),
                    TerminalClientLoopAction::ForwardToPane(b"\r".to_vec()),
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
    assert!(report.view_refresh_required);
    assert!(service.paste_buffers.get("main").is_none());
    assert_eq!(service.active_paste_buffer.as_deref(), None);
    assert!(service.primary_display_overlay.is_none());
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies mouse selection resolves the clicked chip when multiple choices are
/// present on the same display row. This keeps multi-action rows from falling
/// back to ambiguous whole-row execution.
#[test]
fn runtime_primary_display_overlay_mouse_selects_action_chip() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .paste_buffers
        .set_with_origin("main", "pasted\n", Some("test".to_string()))
        .unwrap();
    service.active_paste_buffer = Some("main".to_string());

    service
        .execute_attached_display_command(&primary, "choose-buffer")
        .unwrap();
    let (clicked_line, clicked_column) = service
        .primary_display_overlay
        .as_ref()
        .and_then(|overlay| {
            overlay
                .selections
                .iter()
                .find(|selection| selection.command == "delete-buffer main")
                .map(|selection| {
                    (
                        selection.line_index.saturating_add(1),
                        selection.start_column.saturating_add(2),
                    )
                })
        })
        .expect("delete-buffer choice should be selectable");

    service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::HandleMouse(
                    MouseAction::SelectDisplayOverlay {
                        position: CopyPosition {
                            line: clicked_line,
                            column: clicked_column,
                        },
                    },
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert!(service.paste_buffers.get("main").is_none());
    assert_eq!(service.active_paste_buffer.as_deref(), None);
    service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies observer chooser rows expose the concrete available decisions as
/// compact action chips. The row also contains descriptive `actions=` metadata,
/// but the executable choices must come from the command list so keyboard and
/// mouse selection run real terminal commands.
#[test]
fn runtime_primary_display_overlay_exposes_observer_action_chips() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 24).unwrap(), 120)
        .unwrap();
    let (_observer_client, observer_request) = service
        .session
        .request_observer_with_terminal("observer", None);

    service
        .execute_attached_display_command(&primary, "choose-observer")
        .unwrap();
    let overlay = service
        .primary_display_overlay
        .as_ref()
        .expect("choose-observer should open a command display overlay");
    assert!(
        overlay
            .selections
            .iter()
            .any(|selection| selection.command
                == format!("approve-observer -t {observer_request}")),
        "{overlay:?}"
    );
    assert!(
        overlay
            .selections
            .iter()
            .any(|selection| selection.command == format!("reject-observer -t {observer_request}")),
        "{overlay:?}"
    );
    let view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(100, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        view.lines
            .iter()
            .any(|line| line.contains("[approve]") && line.contains("[reject]")),
        "{view:?}"
    );
}

/// Verifies that the primary command prompt is runtime state rather than a
/// nested prompt loop. Submitted input must be consumed by the actor, clear the
/// prompt immediately, execute the terminal command, and render command output
/// through the primary display overlay without forwarding bytes to the pane.
#[test]
fn runtime_primary_command_prompt_submits_and_clears_through_terminal_step() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service.enter_primary_command_prompt("").unwrap();

    let prompt_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(50, 8).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(prompt_view.primary_prompt_active);
    assert_eq!(
        prompt_view.lines.last().map(|line| line.trim_end()),
        Some("▐ :")
    );

    let report = service
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

    assert_eq!(report.forwarded_bytes, 0);
    assert!(report.view_refresh_required);
    assert!(!report.full_redraw_required);
    let display_view = service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(50, 8).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(!display_view.primary_prompt_active);
    assert_eq!(display_view.lines[0].trim_end(), "mezzanine command output");
    assert!(
        display_view
            .lines
            .iter()
            .any(|line| line.contains("Mezzanine command help")),
        "{:?}",
        display_view.lines
    );
    assert!(
        display_view
            .lines
            .iter()
            .any(|line| line.contains("Category") && line.contains("Command")),
        "{:?}",
        display_view.lines
    );
}

/// Verifies Ctrl+L clears the live viewport while keeping the terminal command
/// prompt open and preserving prior visible content in pane history. Escape
/// exits that prompt without forwarding bytes.
#[test]
fn runtime_primary_command_prompt_ctrl_l_clears_and_escape_exits() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    let mut screen = TerminalScreen::new(Size::new(50, 8).unwrap(), 120).unwrap();
    screen.feed(b"old output");
    service.pane_screens.insert("%1".to_string(), screen);
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("old output")
    );

    service.enter_primary_command_prompt("li").unwrap();
    let clear = service
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

    assert_eq!(clear.forwarded_bytes, 0);
    assert!(service.primary_prompt_input.is_some());
    assert!(
        !service
            .pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("old output")
    );
    assert!(
        service
            .pane_screen("%1")
            .unwrap()
            .normal_content_lines()
            .join("\n")
            .contains("old output")
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
    assert!(service.primary_prompt_input.is_none());
}

/// Verifies that immediate terminal commands submitted through the command
/// prompt take effect without opening a modal display overlay. Commands like
/// `send-prefix` already have an observable pane effect, so users should not
/// have to press Escape after invoking them from the prompt.
#[test]
fn runtime_primary_command_prompt_immediate_command_does_not_open_overlay() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(50, 8).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.enter_primary_command_prompt("").unwrap();

    let report = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"send-prefix\r".to_vec(),
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
    assert!(report.view_refresh_required);
    assert!(service.primary_prompt_input.is_none());
    assert!(service.primary_display_overlay.is_none());
    service.enter_primary_command_prompt("").unwrap();

    let create_buffer = service
        .apply_attached_terminal_step_plan(
            &primary,
            &AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(
                    b"create-buffer ack --content hello\r".to_vec(),
                )],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .unwrap();

    assert_eq!(create_buffer.forwarded_bytes, 0);
    assert!(create_buffer.view_refresh_required);
    assert!(service.primary_prompt_input.is_none());
    assert!(service.primary_display_overlay.is_none());
    assert_eq!(service.paste_buffers.get("ack"), Some("hello"));
    assert!(
        service
            .primary_error_status_overlay
            .as_deref()
            .is_some_and(|message| message.contains("buffer: ack")),
        "{:?}",
        service.primary_error_status_overlay
    );
    let pane_text = service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(!pane_text.contains("mez: buffer: ack"), "{pane_text}");
    assert!(!pane_text.contains("created=true"), "{pane_text}");
    service.pane_processes_mut().terminate_all().unwrap();
}
