//! Runtime tests for config persistence behavior.

use super::*;

/// Verifies that command-prompt MCP mutation commands update the live runtime
/// config layer and immediately refresh the in-memory MCP registry. This keeps
/// terminal MCP management aligned with the persisted CLI/config-store path
/// without using the removed agent-scoped `mcp-list` command.
#[test]
fn runtime_terminal_mcp_command_mutates_persisted_config_and_registry() {
    let mut service = test_runtime_service();
    let config_root = temp_root("runtime-terminal-mcp-command");
    service.set_config_root(config_root.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();

    let output = service
        .execute_terminal_command(
            &primary,
            "mcp add fs --command mcp-fs --arg --root --arg . --disabled",
        )
        .unwrap();
    assert!(output.contains(r#""command":"mcp""#), "{output}");
    assert!(output.contains("server=fs:action=add"), "{output}");
    assert!(output.contains("changed=true"), "{output}");
    assert_eq!(service.mcp_registry().list_servers().len(), 1);
    assert!(!service.mcp_registry().list_servers()[0].configured.enabled);

    let output = service
        .execute_terminal_command(&primary, "mcp tools enable fs read_file")
        .unwrap();
    assert!(output.contains("action=tools-enable"), "{output}");
    assert_eq!(
        service.mcp_registry().list_servers()[0]
            .configured
            .enabled_tools,
        vec!["read_file".to_string()]
    );

    let config_text = fs::read_to_string(config_root.join("config.toml")).unwrap();
    assert!(config_text.contains("[mcp_servers.fs]"));
    assert!(config_text.contains("enabled_tools"));

    let _ = fs::remove_dir_all(config_root);
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
        mez_mux::presentation::TerminalFramePosition::Bottom
    );
    assert_eq!(
        config.window_frame_style,
        mez_mux::presentation::TerminalFrameStyle::Inverse
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
        service.window_frame_right_status_template(),
        "#{datetime.local}"
    );
    assert!(config.pane_frames_enabled);
    assert_eq!(
        config.pane_frame_position,
        mez_mux::presentation::TerminalFramePosition::Bottom
    );
    assert_eq!(
        config.pane_frame_style,
        mez_mux::presentation::TerminalFrameStyle::Bold
    );
    assert_eq!(config.pane_frame_template, "#{pane.index} #{agent.status}");
    assert_eq!(
        config.pane_frame_visible_fields,
        vec!["pane.index".to_string(), "agent.status".to_string()]
    );
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
    fs::write(&project_path, "version = 19\n[history]\nlines = 10\n").unwrap();
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
