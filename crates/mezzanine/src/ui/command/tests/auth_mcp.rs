//! Command auth mcp tests.

use super::*;

/// Verifies refresh and agent shell are noops without live client state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn refresh_and_agent_shell_are_noops_without_live_client_state() {
    let (mut session, primary) = test_session();

    let refresh = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("refresh-client").unwrap()[0],
    )
    .unwrap();
    let agent_shell = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("agent-shell").unwrap()[0],
    )
    .unwrap();

    assert_noop(refresh, "refresh-client");
    assert_noop(agent_shell, "agent-shell");
}

/// Verifies removed terminal auth commands are rejected rather than retained as aliases.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn removed_terminal_auth_status_command_is_rejected() {
    let (mut session, primary) = test_session();

    let status = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("auth-status").unwrap()[0],
    )
    .unwrap_err();
    assert_eq!(status.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(status.message().contains("unknown command"));

    let logout = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("auth-login").unwrap()[0],
    )
    .unwrap_err();
    assert_eq!(logout.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(logout.message().contains("unknown command"));
}

/// Verifies the removed MCP terminal commands are rejected rather than
/// remaining available through dispatch or a command alias.
///
/// This regression scenario protects the command surface while preserving MCP
/// support in the dedicated CLI and agent integrations.
#[test]
fn removed_mcp_terminal_commands_are_rejected() {
    let (mut session, primary) = test_session();

    for input in ["mcp", "mcp-status atlassian_rovo"] {
        let error = execute_command(
            &mut session,
            &primary,
            &parse_command_sequence(input).unwrap()[0],
        )
        .unwrap_err();
        assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
        assert!(error.message().contains("unknown command"), "{error}");
    }
}

/// Verifies kill commands require force for live targets.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn kill_commands_require_force_for_live_targets() {
    let (mut session, primary) = test_session();

    let error = execute_command_sequence(&mut session, &primary, "kill-window").unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);

    execute_command_sequence(&mut session, &primary, "kill-window --force").unwrap();
    assert!(session.windows().is_empty());
}

/// Verifies that `detach-client -t` detaches the named client instead of
/// always detaching the invoking primary. The default target remains the
/// primary for the established mux-compatible short form, but chooser output depends on
/// target-aware command semantics.
#[test]
fn detach_client_command_honors_target_client() {
    let (mut session, primary) = test_session();
    let (observer_client, _observer_request) = session.request_observer("observer");

    execute_command_sequence(
        &mut session,
        &primary,
        &format!("detach-client -t {observer_client}"),
    )
    .unwrap();

    let primary_client = session
        .clients()
        .iter()
        .find(|client| client.id == primary)
        .unwrap();
    let detached_client = session
        .clients()
        .iter()
        .find(|client| client.id == observer_client)
        .unwrap();
    assert_eq!(primary_client.state, ClientState::Attached);
    assert_eq!(detached_client.state, ClientState::Detached);
}
