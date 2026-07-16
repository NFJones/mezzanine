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

/// Verifies auth commands report planning placeholders.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn auth_commands_report_planning_placeholders() {
    let (mut session, primary) = test_session();

    let status = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("auth-status").unwrap()[0],
        )
        .unwrap(),
    );
    assert!(status.contains("authenticated=unknown"));
    assert!(status.contains("source=not-connected"));

    let logout = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("auth-login").unwrap()[0],
    )
    .unwrap_err();
    assert_eq!(logout.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(logout.message().contains("unknown command"));
}

/// Verifies auth commands can execute against auth store without printing secret.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn auth_commands_can_execute_against_auth_store_without_printing_secret() {
    let root = std::env::temp_dir().join(format!("mez-command-auth-store-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let key_file = root.join("openai.key");
    fs::write(&key_file, "sk-command-secret\n").unwrap();
    let auth_store = AuthStore::new(AuthPaths::under_config_root(&root));

    auth_store
        .login_provider_api_key_with_selected_store(
            "openai",
            "work",
            "sk-command-secret",
            Some("file"),
        )
        .unwrap();

    let status_invocation = parse_command_sequence("auth-status").unwrap().remove(0);
    let status = display_body(execute_auth_command(&auth_store, &status_invocation).unwrap());

    assert!(status.contains("authenticated=true"));
    assert!(status.contains("provider=openai"));
    assert!(status.contains("profile=work"));
    assert!(status.contains("credential_store=file"));
    assert!(!status.contains("sk-command-secret"));

    let _ = fs::remove_dir_all(root);
}

/// Verifies MCP status executes against the auth store from the terminal
/// command path instead of falling back to display-only placeholder text.
#[test]
fn mcp_status_executes_against_auth_store_without_placeholder_status() {
    let root =
        std::env::temp_dir().join(format!("mez-command-mcp-auth-store-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let auth_store = AuthStore::new(AuthPaths::under_config_root(&root));

    let status_invocation = parse_command_sequence("mcp-status atlassian_rovo")
        .unwrap()
        .remove(0);
    let status = display_body(execute_auth_command(&auth_store, &status_invocation).unwrap());
    assert!(status.contains("server=atlassian_rovo"), "{status}");
    assert!(status.contains("state=logged-out"), "{status}");
    assert!(status.contains("source=auth-store"), "{status}");
    assert!(
        !status.contains("reason=auth-store-unavailable"),
        "{status}"
    );

    let _ = fs::remove_dir_all(root);
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
