//! Runtime tests for config reload behavior.

use super::*;

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
    service.set_audit_log(AuditLog::new(crate::security::audit::AuditConfig {
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

/// Verifies zen mode follows normal layer precedence and live reload semantics.
///
/// A trusted project layer must override the primary value, and reloading that
/// same layer must atomically replace the stored presentation boolean without
/// changing the independently configured frame toggles.
#[test]
fn runtime_config_reload_applies_layered_zen_mode() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-zen-mode-reload");
    let primary_path = root.join("config.toml");
    let project_path = root.join("project.toml");
    fs::write(
        &primary_path,
        "[terminal]\nzen_mode = false\n[frames.window]\nenabled = true\n[frames.pane]\nenabled = true\n",
    )
    .unwrap();
    fs::write(
        &project_path,
        "version = 85\n[terminal]\nzen_mode = false\n",
    )
    .unwrap();
    service
        .replace_config_layers(vec![
            ConfigLayer {
                name: "primary".to_string(),
                path: Some(primary_path.clone()),
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: fs::read_to_string(&primary_path).unwrap(),
            },
            ConfigLayer {
                name: "project".to_string(),
                path: Some(project_path.clone()),
                format: ConfigFormat::Toml,
                scope: ConfigScope::ProjectOverlay,
                trusted: true,
                text: fs::read_to_string(&project_path).unwrap(),
            },
        ])
        .unwrap();

    service.start_initial_pane_process(Some("cat")).unwrap();
    assert!(!service.terminal_zen_mode());
    assert!(service.window_frames_enabled());
    assert!(service.pane_frames_enabled());
    assert_eq!(
        service.process_pane_screen("%1").unwrap().size(),
        Size::new(100, 38).unwrap()
    );

    fs::write(&project_path, "version = 85\n[terminal]\nzen_mode = true\n").unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload","method":"config/reload","params":{"idempotency_key":"reload-zen-mode"}}"#,
        &primary,
    );

    assert!(response.contains(r#""operation":"reload""#), "{response}");
    assert!(service.terminal_zen_mode());
    assert!(service.window_frames_enabled());
    assert!(service.pane_frames_enabled());
    assert_eq!(
        service.process_pane_screen("%1").unwrap().size(),
        Size::new(100, 40).unwrap()
    );

    fs::write(
        &project_path,
        "version = 85\n[terminal]\nzen_mode = false\n",
    )
    .unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"restore","method":"config/reload","params":{"idempotency_key":"restore-zen-mode"}}"#,
        &primary,
    );

    assert!(response.contains(r#""operation":"reload""#), "{response}");
    assert!(!service.terminal_zen_mode());
    assert_eq!(
        service.process_pane_screen("%1").unwrap().size(),
        Size::new(100, 38).unwrap()
    );

    fs::write(
        &project_path,
        "version = 85\n[terminal]\nzen_mode = \"sometimes\"\n",
    )
    .unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload-invalid","method":"config/reload","params":{"idempotency_key":"reload-invalid-zen-mode"}}"#,
        &primary,
    );

    assert!(response.contains(r#""error""#), "{response}");
    assert!(!service.terminal_zen_mode());
    assert!(service.window_frames_enabled());
    assert!(service.pane_frames_enabled());
    assert_eq!(
        service.process_pane_screen("%1").unwrap().size(),
        Size::new(100, 38).unwrap()
    );
    service.terminate_all_pane_processes().unwrap();
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
    screen.restore_normal_styled_content(
        &["one".to_string(), "two".to_string(), "three".to_string()],
        &[],
    );
    service.set_pane_screen("%1".to_string(), screen);

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

/// Verifies a live history reload applies saved-session age and count as one
/// typed policy rather than leaving either half at a stale prior value.
#[test]
fn runtime_config_reload_applies_saved_session_retention_policy_atomically() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-saved-session-retention-reload");
    let path = root.join("config.toml");
    let transcript_store = AgentTranscriptStore::new(root.join("agent-sessions"));
    service.set_agent_transcript_store(transcript_store);
    fs::write(
        &path,
        "[history]\nsaved_sessions_limit = 40\nsaved_sessions_retention_days = 30\n",
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
    service.persistence.enable_transcript_adapter();

    fs::write(
        &path,
        "[history]\nsaved_sessions_limit = 25\nsaved_sessions_retention_days = 15\n",
    )
    .unwrap();
    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload","method":"config/reload","params":{"idempotency_key":"reload-saved-session-retention"}}"#,
        &primary,
    );

    assert!(response.contains(r#""operation":"reload""#), "{response}");
    let policy = service
        .agent_transcript_store()
        .unwrap()
        .saved_session_retention_policy();
    assert_eq!(policy.max_active_sessions, 25);
    assert_eq!(policy.retention_days, 15);
    let persistence = service
        .drain_transcript_persistence_transition()
        .side_effects;
    assert!(matches!(
        persistence.as_slice(),
        [RuntimeSideEffect::PersistSavedSessionRetention {
            schedule_next: false,
            ..
        }]
    ));
    let _ = fs::remove_dir_all(root);
}

/// Verifies an unrelated live reload does not rescan persistent memory.
///
/// Persistent records are hydrated during daemon startup. A later history
/// setting change must not pull records written by another process into the
/// running session, because doing so adds unrelated filesystem work to the
/// serialized runtime control path.
#[test]
fn runtime_config_reload_does_not_reload_persistent_memory() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();
    let root = temp_root("runtime-config-reload-memory");
    let config_root = root.join("config");
    fs::create_dir_all(&config_root).unwrap();
    service.set_config_root(config_root.clone());
    let path = config_root.join("config.toml");
    fs::write(&path, "[history]\nlines = 4\n").unwrap();
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

    let store = crate::storage::memory::PersistentMemoryStore::under_config_root(&config_root);
    store
        .upsert(mez_agent::memory::MemoryRecord::new_with_defaults(
            "external-memory",
            mez_agent::memory::MemoryScope::Global,
            120,
            120,
            mez_agent::memory::MemorySource::User,
            20,
            "written after runtime startup",
        ))
        .unwrap();
    fs::write(&path, "[history]\nlines = 2\n").unwrap();

    let response = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"reload","method":"config/reload","params":{"idempotency_key":"reload-without-memory"}}"#,
        &primary,
    );

    assert!(response.contains(r#""operation":"reload""#), "{response}");
    assert_eq!(service.terminal_history_limit(), 2);
    assert!(
        service
            .memory_records()
            .iter()
            .all(|record| record.content != "written after runtime startup")
    );
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
    assert_eq!(service.agent_action_failure_retry_limit(), 5);
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

    assert_eq!(service.agent_action_failure_retry_limit(), 2);
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime config reload updates provider retry count and unlimited
/// mode independently without replacing the scheduler or its delay bounds.
#[test]
fn runtime_config_reload_applies_provider_error_retry_policy() {
    let mut service = test_runtime_service();
    assert_eq!(
        service.provider_retry_scheduler_mut().policy(),
        mez_agent::DEFAULT_PROVIDER_RETRY_POLICY
    );
    let root = temp_root("runtime-provider-error-retry-policy");
    let path = root.join("config.toml");
    fs::write(
        &path,
        "[agents]\nprovider_error_retry_limit = 2\nprovider_error_retry_unlimited = true\n",
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

    let policy = service.provider_retry_scheduler_mut().policy();
    assert_eq!(policy.max_attempts, 2);
    assert!(policy.unlimited);
    assert_eq!(policy.initial_delay_ms, 1_000);
    assert_eq!(policy.max_delay_ms, 15 * 60 * 1_000);
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies runtime config reload changes the deadline used by future turns.
///
/// Existing turns retain their snapshotted absolute deadline in their turn
/// record, while this live value supplies the duration for later turn creation.
#[test]
fn runtime_config_reload_applies_agent_turn_timeout() {
    let mut service = test_runtime_service();
    assert_eq!(
        service.agent_turn_timeout_ms(),
        mez_agent::DEFAULT_AGENT_TURN_TIMEOUT_MS
    );
    let root = temp_root("runtime-agent-turn-timeout");
    let path = root.join("config.toml");
    fs::write(&path, "[agents]\nturn_timeout_ms = 900000\n").unwrap();

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

    assert_eq!(service.agent_turn_timeout_ms(), 900_000);
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(root);
}

/// Verifies native shell timeout reloads apply to future turns while an
/// existing turn preserves the deadline captured at its creation boundary.
#[test]
fn runtime_config_reload_snapshots_native_shell_timeout_per_turn() {
    let mut service = test_runtime_service();
    assert_eq!(
        service.agent_native_shell_timeout_ms(),
        mez_agent::DEFAULT_NATIVE_SHELL_TIMEOUT_MS
    );
    service.set_agent_native_shell_timeout_ms(4_321);
    service.snapshot_agent_native_shell_timeout_for_turn("turn-existing");
    service.set_agent_native_shell_timeout_ms(1_234);

    assert_eq!(
        service.agent_native_shell_timeout_ms_for_turn("turn-existing"),
        4_321
    );
    assert_eq!(service.agent_native_shell_timeout_ms(), 1_234);
    assert_eq!(
        service.agent_native_shell_timeout_ms_for_turn("turn-created-after-reload"),
        1_234
    );
}

/// Verifies always-exposed MCP server selection is replaced on config reload.
///
/// The setting is live request policy, so removing a server from configuration
/// must affect later model turns without retaining a stale runtime snapshot.
#[test]
fn runtime_config_reload_replaces_always_exposed_mcp_servers() {
    let mut service = test_runtime_service();

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nalways_exposed_mcp_servers = [\"GitHub\", \"state\"]\n".to_string(),
        }])
        .unwrap();
    assert_eq!(
        service.integration.always_exposed_mcp_servers(),
        &["GitHub".to_string(), "state".to_string()]
    );

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\nalways_exposed_mcp_servers = [\"state\"]\n".to_string(),
        }])
        .unwrap();
    assert_eq!(
        service.integration.always_exposed_mcp_servers(),
        &["state".to_string()]
    );
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
    assert_eq!(service.subagent_wait_policy(), SubagentWaitPolicy::Join);

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
    assert_eq!(service.subagent_wait_policy(), SubagentWaitPolicy::Detach);

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

    assert_eq!(service.max_root_subagents(), 4);
    assert_eq!(service.max_subagents_per_subagent(), 2);
    assert_eq!(service.max_subagent_depth(), 2);

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

    assert_eq!(service.max_root_subagents(), 6);
    assert_eq!(service.max_subagents_per_subagent(), 3);
    assert_eq!(service.max_subagent_depth(), 4);

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

/// Verifies applying a new emoji-width policy rebuilds existing pane cells
/// before later output uses the updated width. Without this rebuild, a wide
/// warning-sign continuation cell would survive the narrow policy and make
/// subsequent writes wrap at an obsolete column.
#[test]
fn runtime_config_reload_rebuilds_live_emoji_cell_footprints() {
    let mut service = test_runtime_service();
    let mut screen = TerminalScreen::new(Size::new(5, 2).unwrap(), 10).unwrap();
    screen.feed("ab⚠️c".as_bytes());
    service.set_pane_screen("%1".to_string(), screen);

    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nemoji_width = \"narrow\"\n".to_string(),
        }])
        .unwrap();
    let screen = service.pane_screen_mut("%1").unwrap();
    screen.feed(b"d");

    assert_eq!(screen.visible_lines()[0], "ab⚠️cd");
    assert_eq!(screen.cursor_state().row, 0);
    assert_eq!(screen.cursor_state().column, 4);
}

/// Verifies a pane-selected generated profile retains its explicit policy but
/// rebases inherited provider-model metadata when configuration is replaced.
///
/// Reload must preserve the override definition rather than copying a stale
/// effective option map from the old provider-model base.
#[test]
fn runtime_config_reload_rebases_generated_model_profile_definitions() {
    let mut service = test_runtime_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let config = |context_window_tokens| ConfigLayer {
        name: "primary".to_string(),
        path: None,
        format: ConfigFormat::Toml,
        scope: ConfigScope::Primary,
        trusted: true,
        text: format!(
            "[agents]\ndefault_provider = \"custom\"\ndefault_model_profile = \"work\"\n[providers.custom]\nkind = \"openai-compatible\"\ndefault_model = \"model-a\"\n[providers.custom.models.primary]\nid = \"model-a\"\ncontext_window_tokens = {context_window_tokens}\n[model_profiles.work]\nprovider = \"custom\"\nmodel = \"model-a\"\n"
        ),
    };
    service
        .replace_config_layers(vec![config(100_000)])
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let latency = service.dispatch_runtime_control_body(
        r#"{"jsonrpc":"2.0","id":"latency","method":"agent/shell/command","params":{"idempotency_key":"latency-rebase","input":"/latency fast"}}"#,
        &primary,
    );
    assert!(latency.contains(r#""kind":"mutated""#), "{latency}");
    let (generated_name, before) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(before.known_context_window_tokens(), Some(100_000));
    assert_eq!(before.latency_preference.as_deref(), Some("fast"));

    service
        .replace_config_layers(vec![config(200_000)])
        .unwrap();

    let (reloaded_name, after) = service
        .active_model_profile_for_pane("%1", "agent-%1", None)
        .unwrap();
    assert_eq!(reloaded_name, generated_name);
    assert_eq!(after.known_context_window_tokens(), Some(200_000));
    assert_eq!(after.latency_preference.as_deref(), Some("fast"));
    assert_eq!(before.known_context_window_tokens(), Some(100_000));
}
