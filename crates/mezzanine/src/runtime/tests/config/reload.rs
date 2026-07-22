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
