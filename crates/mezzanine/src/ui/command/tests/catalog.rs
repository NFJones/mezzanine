//! Command catalog tests.

use super::*;

/// Verifies that `list-commands` reports support status granularly enough to
/// distinguish generic in-memory command behavior from commands whose complete
/// behavior requires runtime, persistent store, or control/repository context.
#[test]
fn list_commands_reports_baseline_command_statuses() {
    let (mut session, primary) = test_session();

    let outcome = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("list-commands").unwrap()[0],
    )
    .unwrap();
    let body = display_body(outcome);

    assert!(body.contains("help:status=implemented"));
    assert!(body.contains("new-window:status=implemented"));
    assert!(body.contains("new-group:status=implemented"));
    assert!(body.contains("kill-group:status=implemented"));
    assert!(body.contains("select-group:status=implemented"));
    assert!(body.contains("swap-pane:status=implemented"));
    assert!(body.contains("break-pane:status=implemented"));
    assert!(body.contains("join-pane:status=implemented"));
    assert!(body.contains("rebalance-window:status=implemented"));
    assert!(body.contains("synchronize-panes:status=implemented"));
    assert!(body.contains("attach-session:status=control-required"));
    assert!(body.contains("list-sessions:status=control-required"));
    assert!(body.contains("copy-mode:status=runtime-required"));
    assert!(body.contains("show-messages:status=implemented"));
    assert!(body.contains("show-metrics:status=runtime-required"));
    assert!(body.contains("list-keys:status=implemented"));
    assert!(body.contains("list-themes:status=implemented"));
    assert!(body.contains("set-theme:status=store-required"));
    assert!(body.contains("show-options:status=implemented"));
    assert!(body.contains("bind-key:status=store-required"));
    assert!(body.contains("unbind-key:status=store-required"));
    assert!(body.contains("set-option:status=store-required"));
    assert!(body.contains("source-file:status=store-required"));
    assert!(body.contains("refresh-client:status=runtime-required"));
    assert!(body.contains("agent-shell:status=runtime-required"));
    assert!(body.contains("mcp-status:status=store-required"));
    assert!(body.contains("mark-pane-ready:status=store-required"));
    assert!(body.contains("copy-selection:status=runtime-required"));
    assert!(body.contains("paste-clipboard:status=runtime-required"));
    assert!(body.contains("paste-buffer:status=runtime-required"));
    assert!(body.contains("create-buffer:status=runtime-required"));
    assert!(body.contains("list-buffers:status=runtime-required"));
    assert!(body.contains("capture-pane:status=runtime-required"));
    assert!(body.contains("save-buffer:status=runtime-required"));
    assert!(body.contains("clear-history:status=runtime-required"));
    assert!(body.contains("search-history:status=runtime-required"));
    assert!(body.contains("export-history:status=runtime-required"));
    assert!(body.contains("pipe-pane:status=runtime-required"));
    assert!(body.contains("save-layout:status=control-required"));
    assert!(body.contains("load-layout:status=control-required"));
    assert!(body.contains("list-observers:status=implemented"));
    assert!(body.contains("choose-observer:status=implemented"));
    assert!(body.contains("approve-observer:status=runtime-required"));
    assert!(body.contains("reject-observer:status=runtime-required"));
    assert!(body.contains("revoke-observer:status=runtime-required"));
    assert!(!body.contains("auth-status:"), "{body}");
    assert!(!body.contains("refresh-provider-info:"), "{body}");
}

/// Verifies that the command-language `help` command returns a human-readable
/// command guide instead of requiring users to infer behavior from the
/// script-oriented `list-commands` status inventory.
#[test]
fn help_command_describes_mezzanine_command_set() {
    let (mut session, primary) = test_session();

    let help = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("help").unwrap()[0],
        )
        .unwrap(),
    );

    assert!(help.contains("# Mezzanine command help"), "{help}");
    assert!(
        help.contains("| Category | Command | Description |"),
        "{help}"
    );
    assert!(help.contains("| Agent and integrations |  |  |"), "{help}");
    assert!(help.contains("| Configuration |  |  |"), "{help}");
    assert!(
        help.contains("| Copy, buffers, and history |  |  |"),
        "{help}"
    );
    assert!(help.contains("| Diagnostics and help |  |  |"), "{help}");
    assert!(help.contains("| Sessions and clients |  |  |"), "{help}");
    assert!(
        help.contains("| Windows, groups, and panes |  |  |"),
        "{help}"
    );
    assert!(help.contains("| `list-commands` |"), "{help}");
    assert!(help.contains("| `list-keys` |"), "{help}");
    assert!(help.contains("show-metrics"), "{help}");
    assert!(help.contains("rebalance-window"), "{help}");
    assert!(help.contains("synchronize-panes"), "{help}");
    assert!(help.contains("set-theme"), "{help}");
    assert!(help.contains("agent-shell"), "{help}");
    assert!(help.contains("save-layout"), "{help}");
    assert!(help.contains("\n## Key bindings\n"), "{help}");
    assert!(help.contains("key"), "{help}");
    assert!(help.contains("source"), "{help}");
    assert!(help.contains("command"), "{help}");
    assert!(!help.contains("A-\\"), "{help}");
    assert!(help.contains("C-a ?"), "{help}");
    assert!(help.contains("list-keys"), "{help}");
    assert!(help.contains("C-a ["), "{help}");
    assert!(help.contains("copy-mode"), "{help}");
    assert!(!help.contains("auth-logout"), "{help}");
    assert!(!help.contains("mcp-list"), "{help}");
    assert!(!help.contains("trust-project"), "{help}");
    assert!(!help.contains("list-command-rules"), "{help}");
    assert!(!help.contains("allow-command"), "{help}");
    assert!(!help.contains("bypass-approvals"), "{help}");
    assert!(!help.contains("permissions"), "{help}");
    assert!(!help.contains("approval              "), "{help}");
    assert!(
        help.find("agent-shell").unwrap() < help.find("approve-observer").unwrap(),
        "{help}"
    );
    assert!(
        help.find("approve-observer").unwrap() < help.find("attach-session").unwrap(),
        "{help}"
    );
    assert!(
        help.find("set-option").unwrap() < help.find("select-window").unwrap(),
        "{help}"
    );
    assert!(
        help.find("| Windows, groups, and panes |  |  |").unwrap()
            < help.find("\n## Key bindings\n").unwrap(),
        "{help}"
    );
    let mut trailing_lines = help.lines().rev();
    assert_eq!(trailing_lines.next(), Some("```"), "{help}");
    let last_binding = trailing_lines.next().unwrap_or_default();
    assert!(last_binding.contains("C-a ~"), "{help}");
    assert!(last_binding.contains("show-messages"), "{help}");
}

/// Verifies that help rendering can substitute a caller-provided key binding
/// table so runtime `help` output stays aligned with the effective configured
/// bindings instead of falling back to the static defaults.
#[test]
fn command_help_display_uses_supplied_key_bindings() {
    let help = super::super::display::command_help_display_with_key_bindings(
        "key   source   command\nF5    project  list-keys\nF6    runtime  split-window -h",
    );

    assert!(help.contains("\n## Key bindings\n"), "{help}");
    assert!(help.contains("F5    project  list-keys"), "{help}");
    assert!(help.contains("F6    runtime  split-window -h"), "{help}");
    assert!(!help.contains("C-a ?"), "{help}");
}

/// Verifies that `synchronize-panes` accepts all documented modes and stores
/// active-window state without affecting the command parser's normal sequence
/// execution rules. The command is intentionally window-scoped, so status must
/// follow the active window rather than a global toggle.
#[test]
fn synchronize_panes_controls_active_window_state() {
    let (mut session, primary) = test_session();

    let outcomes = execute_command_sequence(
        &mut session,
        &primary,
        "synchronize-panes status; synchronize-panes on; synchronize-panes status; synchronize-panes toggle; synchronize-panes off",
    )
    .unwrap();
    let bodies = outcomes.into_iter().map(display_body).collect::<Vec<_>>();

    assert_eq!(
        bodies,
        vec![
            "synchronize-panes=off",
            "synchronize-panes=on",
            "synchronize-panes=on",
            "synchronize-panes=off",
            "synchronize-panes=off",
        ]
    );
    let error =
        execute_command_sequence(&mut session, &primary, "synchronize-panes maybe").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("synchronize-panes accepts on, off, toggle, or status"),
        "{error}"
    );
}

/// Verifies that agent-scoped commands with slash-command equivalents are no
/// longer part of the Mezzanine terminal command language. The terminal command
/// prompt should stay focused on multiplexer and terminal/session operations,
/// while these behaviors are reachable through `/logout`, `/list-mcp`, `/trust`,
/// `/permissions`, and `/approval` inside the pane-local agent shell.
#[test]
fn agent_scoped_slash_duplicates_are_not_terminal_commands() {
    let (mut session, primary) = test_session();
    let removed = [
        "auth-logout",
        "mcp-list",
        "list-project-trust",
        "trust-project /tmp/project",
        "reject-project /tmp/project",
        "revoke-project-trust /tmp/project",
        "permissions",
        "approval",
        "list-command-rules",
        "allow-command cargo test",
        "deny-command rm",
        "prompt-command git commit",
        "remove-command-rule rule1",
        "bypass-approvals status",
    ];

    for input in removed {
        let error = execute_command(
            &mut session,
            &primary,
            &parse_command_sequence(input).unwrap()[0],
        )
        .unwrap_err();
        assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
        assert!(
            error.message().contains("unknown command"),
            "{input}: {error}"
        );
    }
}

/// Verifies that `list-themes` has a useful offline fallback before a live
/// runtime config is attached: it should show the built-in theme registry and
/// mark the generated default `kanagawa` theme as active.
#[test]
fn list_themes_reports_builtin_defaults_without_runtime_config() {
    let (mut session, primary) = test_session();

    let outcome = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("list-themes").unwrap()[0],
    )
    .unwrap();
    let body = display_body(outcome);
    assert!(
        body.starts_with(
            "| active | theme | preview | source | preview colors | action |\n| --- | --- | --- | --- | --- | --- |"
        )
    );
    assert!(body.contains("| ★ active | kanagawa | █████ | builtin |"));
    assert!(body.contains("| — | deepforest | █████ | builtin |"));
    assert!(body.contains("| — | gruvbox_dark | █████ | builtin |"));
    assert!(body.contains("| — | catppuccin_latte | █████ | builtin |"));
    assert!(body.contains("| — | high_contrast_dark | █████ | builtin |"));
    assert!(body.contains("| — | dracula | █████ | builtin |"));
    assert!(body.contains("[`set-theme kanagawa`](mez-agent:set-theme%20kanagawa)"));
}

/// Verifies that the baseline command registry reports a known support level
/// for every command instead of using a binary implemented/pending flag that
/// would hide runtime-, store-, or control-backed fallback behavior.
#[test]
fn every_baseline_command_reports_an_authoritative_support_status() {
    for command in baseline_commands() {
        assert!(
            matches!(
                command.status.as_str(),
                "implemented" | "runtime-required" | "store-required" | "control-required"
            ),
            "baseline command registry contains an unsupported status for {}",
            command.name
        );
    }
}

/// Verifies display safe pending command defaults.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn display_safe_pending_command_defaults() {
    let (mut session, primary) = test_session();

    let keys = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("list-keys").unwrap()[0],
        )
        .unwrap(),
    );
    assert!(!keys.contains("A-\\"));
    assert!(keys.contains("C-a ?"));
    assert!(keys.contains("list-keys"));
    assert!(keys.contains("C-a ["));
    assert!(keys.contains("copy-mode"));

    let options = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("show-options").unwrap()[0],
        )
        .unwrap(),
    );
    assert!(options.contains("source=default live_mutation=not-connected"));
    assert!(options.contains("[history]\nlines = 10000"));

    let copy_mode = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("copy-mode -t 0").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        copy_mode,
        "target=0:copy_mode=not-entered:reason=live-terminal-state-unavailable"
    );

    let messages = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("show-messages").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(messages, "messages=0 source=in-memory-log status=empty");
    let metrics = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("show-metrics").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(metrics, "metrics source=async-runtime status=unavailable");
}
