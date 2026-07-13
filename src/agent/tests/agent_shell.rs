//! Agent tests for agent shell behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

#[test]
/// Verifies agent shell executes builtin slash command effects.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn agent_shell_executes_builtin_slash_command_effects() {
    let mut store = AgentShellStore::default();
    store.enter_or_resume("%1").unwrap();
    store.start_turn("%1", "turn-1").unwrap();
    store.finish_turn("%1", "turn-1").unwrap();
    store.record_transcript_entries("%1", 1).unwrap();

    let status = execute_agent_shell_command(&mut store, "%1", "/status")
        .unwrap()
        .unwrap();
    assert!(matches!(
        status,
        AgentShellCommandOutcome::Display { ref body, .. }
            if body.contains("visibility: visible")
                && body.contains("transcript entries: 1")
                && body.contains("log level: normal")
    ));

    let clear = execute_agent_shell_command(&mut store, "%1", "/clear")
        .unwrap()
        .unwrap();
    assert!(matches!(
        clear,
        AgentShellCommandOutcome::Mutated {
            visibility: AgentShellVisibility::Visible,
            ref body,
            ..
        } if body.contains("transcript_entries=0") && body.contains("new=true")
    ));
    assert_eq!(store.get("%1").unwrap().transcript_entries, 0);

    store.start_turn("%1", "turn-2").unwrap();
    let exit = execute_agent_shell_command(&mut store, "%1", "/quit")
        .unwrap()
        .unwrap();
    assert!(matches!(
        exit,
        AgentShellCommandOutcome::RequiresRuntime { ref command, .. } if command == "exit"
    ));

    let help = execute_agent_shell_command(&mut store, "%1", "/help")
        .unwrap()
        .unwrap();
    let AgentShellCommandOutcome::Display { ref body, .. } = help else {
        panic!("expected /help display outcome");
    };
    assert!(body.contains("# Agent shell commands"), "{body}");
    assert!(
        body.contains("| Category | Command | Description |"),
        "{body}"
    );
    assert!(body.contains("| `/list-sessions` |"), "{body}");
    assert!(
        body.contains("list resumable saved agent conversations."),
        "{body}"
    );
    assert!(body.contains("| `/list-skills` |"), "{body}");
    assert!(
        body.contains("list available skills and their $skill prompt names."),
        "{body}"
    );
    assert!(body.contains("| `/status` |"), "{body}");
    assert!(
        body.contains("show the current agent shell session status."),
        "{body}"
    );
    assert!(body.contains("| `/sync-builtin-skills` |"), "{body}");
    assert!(
        body.contains("synchronize managed built-in skills into the user configuration root."),
        "{body}"
    );
    assert!(body.contains("| `/show-issues` |"), "{body}");
    assert!(
        body.contains("browse project issue records and open issue details."),
        "{body}"
    );
    assert!(!body.contains("run the slash command."), "{body}");
    assert!(body.contains("| Copy and diagnostics |  |  |"), "{body}");
    assert!(body.contains("| Configuration |  |  |"), "{body}");
    assert!(body.contains("| Discovery |  |  |"), "{body}");
    assert!(body.contains("| Work control |  |  |"), "{body}");
    assert!(body.find("/approval").unwrap() < body.find("/approve").unwrap());
    assert!(!body.contains("/agent"), "{body}");
    assert!(body.contains("| `/memory` |"), "{body}");
    assert!(
        body.contains("inspect or change persistent memory enablement."),
        "{body}"
    );
    assert!(!body.contains("/mention"), "{body}");
    assert!(!body.contains("/plan"), "{body}");
    assert!(!body.contains("/ps"), "{body}");
    assert!(!body.contains("/review"), "{body}");
    assert!(!body.contains("effect="), "{body}");

    store.finish_turn("%1", "turn-2").unwrap();
    let old_session = store.get("%1").unwrap().session_id.clone();
    let new = execute_agent_shell_command(&mut store, "%1", "/new")
        .unwrap()
        .unwrap();
    assert!(matches!(
        new,
        AgentShellCommandOutcome::Mutated {
            visibility: AgentShellVisibility::Visible,
            ref body,
            ..
        } if body.contains("new=true") && body.contains("transcript_entries=0")
    ));
    assert_ne!(store.get("%1").unwrap().session_id, old_session);
    assert_eq!(store.get("%1").unwrap().transcript_entries, 0);
    assert_eq!(store.get("%1").unwrap().log_level, AgentLogLevel::Normal);

    let verbose = execute_agent_shell_command(&mut store, "%1", "/log-level verbose")
        .unwrap()
        .unwrap();
    assert!(matches!(
        verbose,
        AgentShellCommandOutcome::Mutated {
            visibility: AgentShellVisibility::Visible,
            ref body,
            ..
        } if body.contains("agent log level for pane %1 is now verbose.")
    ));
    assert_eq!(store.get("%1").unwrap().log_level, AgentLogLevel::Verbose);

    let debug = execute_agent_shell_command(&mut store, "%1", "/log-level debug")
        .unwrap()
        .unwrap();
    assert!(matches!(
        debug,
        AgentShellCommandOutcome::Mutated {
            visibility: AgentShellVisibility::Visible,
            ref body,
            ..
        } if body.contains("agent log level for pane %1 is now debug.")
    ));
    assert_eq!(store.get("%1").unwrap().log_level, AgentLogLevel::Debug);

    let trace = execute_agent_shell_command(&mut store, "%1", "/log-level trace")
        .unwrap()
        .unwrap();
    assert!(matches!(
        trace,
        AgentShellCommandOutcome::Mutated {
            visibility: AgentShellVisibility::Visible,
            ref body,
            ..
        } if body.contains("agent log level for pane %1 is now trace.")
    ));
    assert_eq!(store.get("%1").unwrap().log_level, AgentLogLevel::Trace);

    let current = execute_agent_shell_command(&mut store, "%1", "/log-level")
        .unwrap()
        .unwrap();
    assert!(matches!(
        current,
        AgentShellCommandOutcome::Display { ref body, .. }
            if body.contains("agent log level for pane %1 is trace.")
                && body.contains("normal, verbose, debug, trace")
    ));

    let directive = execute_agent_shell_command(&mut store, "%1", "/directive focus on tests")
        .unwrap()
        .unwrap();
    assert!(matches!(
        directive,
        AgentShellCommandOutcome::Mutated {
            visibility: AgentShellVisibility::Visible,
            ref body,
            ..
        } if body.contains("agent directive for pane %1 is now `focus on tests`.")
    ));
    assert_eq!(
        store.get("%1").unwrap().directive.as_deref(),
        Some("focus on tests")
    );

    let directive_status = execute_agent_shell_command(&mut store, "%1", "/directive")
        .unwrap()
        .unwrap();
    assert!(matches!(
        directive_status,
        AgentShellCommandOutcome::Display { ref body, .. }
            if body.contains("agent directive for pane %1 is `focus on tests`.")
    ));

    let directive_clear = execute_agent_shell_command(&mut store, "%1", "/directive clear")
        .unwrap()
        .unwrap();
    assert!(matches!(
        directive_clear,
        AgentShellCommandOutcome::Mutated {
            visibility: AgentShellVisibility::Visible,
            ref body,
            ..
        } if body.contains("agent directive for pane %1 is now not set.")
    ));
    assert_eq!(store.get("%1").unwrap().directive, None);

    let normal = execute_agent_shell_command(&mut store, "%1", "/log-level normal")
        .unwrap()
        .unwrap();
    assert!(matches!(
        normal,
        AgentShellCommandOutcome::Mutated {
            visibility: AgentShellVisibility::Visible,
            ref body,
            ..
        } if body.contains("agent log level for pane %1 is now normal.")
    ));
    assert_eq!(store.get("%1").unwrap().log_level, AgentLogLevel::Normal);

    store.start_turn("%1", "turn-3").unwrap();
    let running_new = execute_agent_shell_command(&mut store, "%1", "/new")
        .unwrap()
        .unwrap();
    assert!(matches!(
        running_new,
        AgentShellCommandOutcome::Display { ref body, .. }
            if body.contains("/new cannot run while an agent turn is active")
    ));
    store.finish_turn("%1", "turn-3").unwrap();

    let model = execute_agent_shell_command(&mut store, "%1", "/model gpt-test")
        .unwrap()
        .unwrap();
    assert!(matches!(
        model,
        AgentShellCommandOutcome::RequiresRuntime { ref reason, .. }
            if reason.contains("PolicyMutation")
    ));
    assert!(
        execute_agent_shell_command(&mut store, "%1", "ordinary prompt")
            .unwrap()
            .is_none()
    );
}

#[test]
/// Verifies agent shell MCP command lists injected registry state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn agent_shell_mcp_command_lists_injected_registry_state() {
    let mut registry = McpRegistry::default();
    registry
        .add_server(crate::mcp::McpServerConfig::stdio(
            "fs",
            "filesystem",
            "mcp-fs",
            Vec::new(),
        ))
        .unwrap();
    registry
        .mark_available("fs", vec![agent_shell_test_mcp_tool("read_file")])
        .unwrap();

    let body = agent_shell_test_mcp_body(&registry);

    assert!(body.contains("## MCP Servers"), "{body}");
    assert!(body.contains("Servers: 1"), "{body}");
    assert!(body.contains("Tools: 1"), "{body}");
    assert!(body.contains("Source: runtime-mcp"), "{body}");
    assert!(body.contains("### `fs` - filesystem"), "{body}");
    assert!(body.contains("- State: available"), "{body}");
    assert!(body.contains("- Status: available"), "{body}");
    assert!(body.contains("- Retryable: false"), "{body}");
    assert!(
        body.contains("| Tool | State | Approval | Permission | Effects | Description |"),
        "{body}"
    );
    assert!(
        body.contains("| `read_file` | available | inherit | true | read-fs |"),
        "{body}"
    );
}

#[test]
/// Verifies that `/list-mcp` exposes servers disabled by configuration as disabled
/// and non-retryable. The spec requires disabled MCP integrations to remain
/// visible to the agent shell rather than disappearing from the listing.
fn agent_shell_mcp_command_reports_disabled_server() {
    let mut registry = McpRegistry::default();
    let mut disabled =
        crate::mcp::McpServerConfig::stdio("disabled", "Disabled MCP", "mcp-disabled", Vec::new());
    disabled.enabled = false;
    registry.add_server(disabled).unwrap();

    let body = agent_shell_test_mcp_body(&registry);

    assert!(body.contains("### `disabled` - Disabled MCP"), "{body}");
    assert!(body.contains("- State: disabled"), "{body}");
    assert!(body.contains("- Enabled: false"), "{body}");
    assert!(body.contains("- Status: configured"), "{body}");
    assert!(body.contains("- Retryable: false"), "{body}");
    assert!(body.contains("- Reason: disabled"), "{body}");
}

#[test]
/// Verifies that configured disabled tools take precedence in `/list-mcp` display
/// classification. A disabled tool should be reported as disabled even when
/// discovery found it, matching the registry's action-planning behavior.
fn agent_shell_mcp_command_reports_disabled_tool_precedence() {
    let mut registry = McpRegistry::default();
    let mut config = crate::mcp::McpServerConfig::stdio("fs", "filesystem", "mcp-fs", Vec::new());
    config.disabled_tools.push("read_file".to_string());
    registry.add_server(config).unwrap();
    registry
        .mark_available("fs", vec![agent_shell_test_mcp_tool("read_file")])
        .unwrap();

    let body = agent_shell_test_mcp_body(&registry);

    assert!(body.contains("### `fs` - filesystem"), "{body}");
    assert!(body.contains("- State: available"), "{body}");
    assert!(body.contains("| `read_file` | disabled |"), "{body}");
}

#[test]
/// Verifies that `/list-mcp` reports an empty live registry as a concrete runtime
/// state instead of omitting the command body. This covers the zero-server case
/// in the agent-shell MCP visibility requirement.
fn agent_shell_mcp_command_reports_empty_registry() {
    let registry = McpRegistry::default();

    let body = agent_shell_test_mcp_body(&registry);

    assert_eq!(
        body,
        "## MCP Servers\n\nServers: 0\nTools: 0\nSource: runtime-mcp\n\nNo MCP servers are configured."
    );
}

#[test]
/// Verifies that `/list-mcp` exposes session-blacklisted server state, failure
/// reason, retryability, and blacklisted tools. Session blacklisting is a
/// required safety signal for agents choosing external tool actions.
fn agent_shell_mcp_command_reports_session_blacklisted_server_and_tools() {
    let mut registry = McpRegistry::default();
    registry
        .add_server(crate::mcp::McpServerConfig::stdio(
            "fs",
            "filesystem",
            "mcp-fs",
            Vec::new(),
        ))
        .unwrap();
    registry
        .mark_available("fs", vec![agent_shell_test_mcp_tool("read_file")])
        .unwrap();
    registry
        .blacklist_for_session("fs", "failed handshake")
        .unwrap();

    let body = agent_shell_test_mcp_body(&registry);

    assert!(body.contains("### `fs` - filesystem"), "{body}");
    assert!(body.contains("- State: blacklisted"), "{body}");
    assert!(body.contains("- Status: blacklisted"), "{body}");
    assert!(body.contains("- Blacklisted: true"), "{body}");
    assert!(body.contains("- Session blacklisted: true"), "{body}");
    assert!(body.contains("- Retryable: true"), "{body}");
    assert!(body.contains("- Reason: failed handshake"), "{body}");
    assert!(body.contains("| `read_file` | blacklisted |"), "{body}");
}

#[test]
/// Verifies that `/list-mcp` exposes unavailable server diagnostics and retryability
/// from the live registry. This keeps agent-shell MCP visibility aligned with
/// control state and the live MCP registry.
fn agent_shell_mcp_command_reports_unavailable_server_reason() {
    let mut registry = McpRegistry::default();
    registry
        .add_server(crate::mcp::McpServerConfig::stdio(
            "fs",
            "filesystem",
            "mcp-fs",
            Vec::new(),
        ))
        .unwrap();
    registry
        .mark_available("fs", vec![agent_shell_test_mcp_tool("read_file")])
        .unwrap();
    registry.mark_unavailable("fs", "process exited").unwrap();

    let body = agent_shell_test_mcp_body(&registry);

    assert!(body.contains("### `fs` - filesystem"), "{body}");
    assert!(body.contains("- State: unavailable"), "{body}");
    assert!(body.contains("- Status: unavailable"), "{body}");
    assert!(body.contains("- Blacklisted: true"), "{body}");
    assert!(body.contains("- Session blacklisted: false"), "{body}");
    assert!(body.contains("- Retryable: true"), "{body}");
    assert!(body.contains("- Reason: process exited"), "{body}");
    assert!(body.contains("| `read_file` | unavailable |"), "{body}");
}

#[test]
/// Verifies agent shell permissions command lists injected policy.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn agent_shell_permissions_command_lists_injected_policy() {
    let mut store = AgentShellStore::default();
    store.enter_or_resume("%1").unwrap();
    let mut policy = PermissionPolicy::default()
        .with_approval_policy(crate::permissions::ApprovalPolicy::FullAccess);
    policy.set_approval_bypass(true);
    let summary = policy.agent_shell_summary();

    let display =
        execute_agent_shell_command_with_permissions(&mut store, "%1", "/approvals", &summary)
            .unwrap()
            .unwrap();

    assert!(matches!(
        display,
        AgentShellCommandOutcome::Display { ref command, ref body }
            if command == "permissions"
                && body.contains("preset=read-only")
                && body.contains("approval_policy=full-access")
                && body.contains("bypass=true")
                && body.contains("source=runtime-policy")
    ));

    let mutation = execute_agent_shell_command_with_permissions(
        &mut store,
        "%1",
        "/permissions approval-policy ask",
        &summary,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(
        mutation,
        AgentShellCommandOutcome::RequiresRuntime { ref reason, .. }
            if reason.contains("primary-client approval")
    ));

    let missing_runtime = execute_agent_shell_command(&mut store, "%1", "/permissions")
        .unwrap()
        .unwrap();
    assert!(matches!(
        missing_runtime,
        AgentShellCommandOutcome::RequiresRuntime { ref reason, .. }
            if reason.contains("live permission policy")
    ));
}

#[test]
/// Verifies agent shell rejects mismatched turn completion.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn agent_shell_rejects_mismatched_turn_completion() {
    let mut store = AgentShellStore::default();
    store.enter_or_resume("%1").unwrap();
    store.start_turn("%1", "turn-1").unwrap();

    let error = store.finish_turn("%1", "turn-2").unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

#[test]
/// Verifies that invalid agent slash commands become readable display output
/// instead of escaping as command errors that can tear down the prompt loop.
fn agent_shell_reports_invalid_slash_command_as_display_output() {
    let mut store = AgentShellStore::default();
    store.enter_or_resume("%1").unwrap();

    let unknown = execute_agent_shell_command(&mut store, "%1", "/not-a-command")
        .unwrap()
        .unwrap();
    let invalid_arg = execute_agent_shell_command(&mut store, "%1", "/log-level maybe")
        .unwrap()
        .unwrap();

    assert!(matches!(
        unknown,
        AgentShellCommandOutcome::Display { ref body, .. }
            if body.contains("agent command error: unknown slash command")
    ));
    assert!(matches!(
        invalid_arg,
        AgentShellCommandOutcome::Display { ref body, .. }
            if body.contains("log-level expects one of: normal, verbose, debug, trace")
    ));
}

#[test]
/// Verifies that hiding an agent shell immediately returns pane input focus to
/// the user even when a turn continues in the background. Finishing the turn
/// keeps the same session while transcript state remains tied to durable
/// transcript writes.
fn agent_shell_resumes_per_pane_and_hides_immediately_during_running_turn() {
    let mut store = AgentShellStore::default();
    let first_session_id = store.enter_or_resume("%1").unwrap().session_id.to_string();
    assert!(looks_like_uuid_v4(&first_session_id));

    store.start_turn("%1", "turn-1").unwrap();
    let pending = store.request_exit("%1").unwrap();
    assert_eq!(pending.visibility, AgentShellVisibility::Hidden);

    let hidden = store.finish_turn("%1", "turn-1").unwrap();
    assert_eq!(hidden.visibility, AgentShellVisibility::Hidden);
    assert_eq!(hidden.transcript_entries, 0);
    let recorded = store.record_transcript_entries("%1", 3).unwrap();
    assert_eq!(recorded.transcript_entries, 3);

    let resumed = store.enter_or_resume("%1").unwrap();
    assert_eq!(resumed.session_id, first_session_id);
    assert_eq!(resumed.visibility, AgentShellVisibility::Visible);
    assert_eq!(resumed.transcript_entries, 3);

    let other = store.enter_or_resume("%2").unwrap();
    assert!(looks_like_uuid_v4(&other.session_id));
    assert_ne!(other.session_id, first_session_id);
}

#[test]
/// Verifies slash command parser normalizes aliases and classifies effects.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn slash_command_parser_normalizes_aliases_and_classifies_effects() {
    let invocation = parse_slash_command("/approvals add git status")
        .unwrap()
        .unwrap();

    assert_eq!(invocation.name, "permissions");
    assert_eq!(invocation.args, "add git status");
    assert_eq!(invocation.effect, SlashCommandEffect::PolicyMutation);
    assert!(invocation.queueable_while_running);
    let dump_context = parse_slash_command("/dump-context buffer diag")
        .unwrap()
        .unwrap();
    assert_eq!(dump_context.name, "copy-context");
    assert_eq!(dump_context.args, "buffer diag");
    assert_eq!(dump_context.effect, SlashCommandEffect::SessionMutation);
    let trace_log = parse_slash_command("/copy-trace-log buffer diag")
        .unwrap()
        .unwrap();
    assert_eq!(trace_log.name, "copy-trace-log");
    assert_eq!(trace_log.args, "buffer diag");
    assert_eq!(trace_log.effect, SlashCommandEffect::SessionMutation);
    let copy_patches = parse_slash_command("/copy-patches clipboard")
        .unwrap()
        .unwrap();
    assert_eq!(copy_patches.name, "copy-patches");
    assert_eq!(copy_patches.args, "clipboard");
    assert_eq!(copy_patches.effect, SlashCommandEffect::SessionMutation);
    let copy = parse_slash_command("/copy buffer latest-answer")
        .unwrap()
        .unwrap();
    assert_eq!(copy.name, "copy");
    assert_eq!(copy.args, "buffer latest-answer");
    assert_eq!(copy.effect, SlashCommandEffect::SessionMutation);
    let sessions = parse_slash_command("/list-sessions").unwrap().unwrap();
    assert_eq!(sessions.name, "list-sessions");
    assert_eq!(sessions.effect, SlashCommandEffect::ReadOnly);
    let macros = parse_slash_command("/list-macros").unwrap().unwrap();
    assert_eq!(macros.name, "list-macros");
    assert_eq!(macros.effect, SlashCommandEffect::ReadOnly);
    let skills = parse_slash_command("/list-skills").unwrap().unwrap();
    assert_eq!(skills.name, "list-skills");
    assert_eq!(skills.effect, SlashCommandEffect::ReadOnly);
    let directive = parse_slash_command("/directive focus on regressions")
        .unwrap()
        .unwrap();
    assert_eq!(directive.name, "directive");
    assert_eq!(directive.args, "focus on regressions");
    assert_eq!(directive.effect, SlashCommandEffect::SessionMutation);
    let loop_command = parse_slash_command("/loop review the docs")
        .unwrap()
        .unwrap();
    assert_eq!(loop_command.name, "loop");
    assert_eq!(loop_command.args, "review the docs");
    assert_eq!(loop_command.effect, SlashCommandEffect::SessionMutation);
    assert!(!loop_command.queueable_while_running);
    let memory = parse_slash_command("/memory toggle").unwrap().unwrap();
    assert_eq!(memory.name, "memory");
    assert_eq!(memory.args, "toggle");
    assert_eq!(memory.effect, SlashCommandEffect::PolicyMutation);
    assert!(memory.queueable_while_running);
    assert_eq!(
        parse_slash_command("/sessions").unwrap_err().kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
    assert_eq!(
        parse_slash_command("/steer use the smaller patch")
            .unwrap_err()
            .kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
    assert!(parse_slash_command("ordinary prompt").unwrap().is_none());
    assert_eq!(
        parse_slash_command("/fast").unwrap_err().kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
    assert_eq!(
        parse_slash_command("/apps").unwrap_err().kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
    for removed in [
        "/agent",
        "/mention",
        "/plan",
        "/ps",
        "/review",
        "/trace",
        "/trace-log",
        "/copy-patch",
    ] {
        assert_eq!(
            parse_slash_command(removed).unwrap_err().kind(),
            crate::error::MezErrorKind::InvalidArgs,
            "{removed} must stay removed"
        );
    }
    assert_eq!(
        parse_slash_command("/does-not-exist").unwrap_err().kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
}

#[test]
/// Verifies slash command registry contains required baseline commands.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn slash_command_registry_contains_required_baseline_commands() {
    let commands = baseline_slash_commands()
        .into_iter()
        .map(|command| command.name)
        .collect::<BTreeSet<_>>();

    for required in [
        "help",
        "permissions",
        "approval",
        "approve",
        "trust",
        "directive",
        "list-sessions",
        "list-skills",
        "copy-context",
        "copy-trace-log",
        "copy-patches",
        "clear",
        "compact",
        "copy",
        "diff",
        "exit",
        "init",
        "thinking",
        "logout",
        "list-mcp",
        "memory",
        "model",
        "loop",
        "stop",
        "fork",
        "resume",
        "new",
        "status",
        "debug-config",
        "statusline",
        "title",
        "log-level",
    ] {
        assert!(commands.contains(required), "missing {required}");
    }

    assert!(
        !commands.contains("fast"),
        "removed command must stay absent"
    );
    for removed in ["agent", "mention", "plan", "ps", "review"] {
        assert!(
            !commands.contains(removed),
            "removed command must stay absent: {removed}"
        );
    }
    assert!(
        !commands.contains("apps"),
        "removed command must stay absent"
    );
}
