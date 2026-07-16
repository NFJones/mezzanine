//! Command session tests.

use super::*;

/// Verifies the product parser adapter preserves lower command diagnostics
/// while projecting them into the product invalid-argument category.
#[test]
fn command_parser_projects_mux_errors_to_product_invalid_args() {
    let error = parse_command_sequence("rename-window \"unterminated").unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(error.message().contains("unterminated quoted"));
}

/// Verifies executes window commands against session state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn executes_window_commands_against_session_state() {
    let (mut session, primary) = test_session();

    execute_command_sequence(
        &mut session,
        &primary,
        "new-window work; rename-window build",
    )
    .unwrap();

    assert_eq!(session.windows().len(), 2);
    assert_eq!(session.active_window().unwrap().name, "build");
}

/// Verifies new window preserves explicit shell command plan.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn new_window_preserves_explicit_shell_command_plan() {
    let (mut session, primary) = test_session();
    let invocation = parse_command_sequence("new-window -n build -c /tmp -- echo 'hello world'")
        .unwrap()
        .remove(0);
    let outcome = execute_command(&mut session, &primary, &invocation).unwrap();

    match outcome {
        CommandOutcome::MutatedWithPaneCommand {
            command,
            shell_command,
            start_directory,
        } => {
            assert_eq!(command, "new-window");
            assert_eq!(shell_command, "echo 'hello world'");
            assert_eq!(start_directory.as_deref(), Some("/tmp"));
        }
        other => panic!("expected pane command plan, got {other:?}"),
    }
    assert_eq!(session.active_window().unwrap().name, "build");
}

/// Verifies new group preserves explicit shell command plan.
///
/// The generic command dispatcher cannot start a live pane process itself, but
/// it must still parse the same command shape as new-window, mutate the group
/// topology, and return the shell plan for the runtime dispatcher.
#[test]
fn new_group_preserves_explicit_shell_command_plan() {
    let (mut session, primary) = test_session();
    let invocation = parse_command_sequence("new-group -n work -c /tmp -- echo 'hello world'")
        .unwrap()
        .remove(0);
    let outcome = execute_command(&mut session, &primary, &invocation).unwrap();

    match outcome {
        CommandOutcome::MutatedWithPaneCommand {
            command,
            shell_command,
            start_directory,
        } => {
            assert_eq!(command, "new-group");
            assert_eq!(shell_command, "echo 'hello world'");
            assert_eq!(start_directory.as_deref(), Some("/tmp"));
        }
        other => panic!("expected pane command plan, got {other:?}"),
    }
    assert_eq!(session.window_groups().len(), 2);
    assert_eq!(session.active_group().unwrap().name, "work");
    assert_eq!(session.active_window().unwrap().name, "work");
}

/// Verifies executes window navigation commands.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn executes_window_navigation_commands() {
    let (mut session, primary) = test_session();

    execute_command_sequence(
        &mut session,
        &primary,
        "new-window one; new-window two; next-window; previous-window; last-window",
    )
    .unwrap();

    assert_eq!(session.active_window().unwrap().index, 0);
}

/// Verifies the command used by the default `Ctrl+A .` prefix prompt. The
/// binding pre-fills `move-window -t `, so submitting an index must reorder the
/// active window while preserving its stable id, panes, and active selection.
#[test]
fn move_window_command_reorders_active_window_by_target_index() {
    let (mut session, primary) = test_session();
    let initial_window_id = session.active_window().unwrap().id.clone();

    execute_command_sequence(
        &mut session,
        &primary,
        "new-window one; new-window two; select-window -t 0; move-window -t 2",
    )
    .unwrap();

    let names = session
        .windows()
        .iter()
        .map(|window| window.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["one", "two", "0"]);
    let active = session.active_window().unwrap();
    assert_eq!(active.id, initial_window_id);
    assert_eq!(active.index, 2);
}

/// Covers the failure side of window reordering. An out-of-range target index
/// must fail before mutation so command-prompt mistakes cannot silently
/// corrupt window order or active-window bookkeeping.
#[test]
fn move_window_command_rejects_out_of_range_target_without_reordering() {
    let (mut session, primary) = test_session();

    execute_command_sequence(&mut session, &primary, "new-window one").unwrap();
    let before = session
        .windows()
        .iter()
        .map(|window| (window.id.to_string(), window.index, window.name.clone()))
        .collect::<Vec<_>>();
    let error = execute_command_sequence(&mut session, &primary, "move-window -t 2").unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    let after = session
        .windows()
        .iter()
        .map(|window| (window.id.to_string(), window.index, window.name.clone()))
        .collect::<Vec<_>>();
    assert_eq!(after, before);
}

/// Verifies list commands return session state.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn list_commands_return_session_state() {
    let (mut session, primary) = test_session();
    let (observer_client, observer_request) = session.request_observer_with_terminal(
        "observer",
        Some(mez_mux::session::ClientTerminalDescriptor {
            columns: 100,
            rows: 30,
            term: "xterm-256color".to_string(),
            features: vec!["rgb".to_string()],
        }),
    );
    execute_command_sequence(&mut session, &primary, "split-window --select").unwrap();

    let windows = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("list-windows").unwrap()[0],
    )
    .unwrap();
    let panes = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("list-panes").unwrap()[0],
    )
    .unwrap();
    let clients = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("list-clients").unwrap()[0],
    )
    .unwrap();
    let observers = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("list-observers").unwrap()[0],
    )
    .unwrap();
    let choose_observer = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("choose-observer").unwrap()[0],
    )
    .unwrap();
    let sessions = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("list-sessions").unwrap()[0],
    )
    .unwrap();
    let pane_selector = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("display-panes").unwrap()[0],
    )
    .unwrap();
    let window_chooser = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("choose-window").unwrap()[0],
    )
    .unwrap();
    let client_chooser = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("choose-client").unwrap()[0],
    )
    .unwrap();
    let send_prefix = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("send-prefix").unwrap()[0],
    )
    .unwrap();

    assert!(display_body(windows).contains("panes=2"));
    let panes = display_body(panes);
    assert!(panes.contains("active=true"), "{panes}");
    assert!(panes.contains("primary_pid=none"), "{panes}");
    assert!(panes.contains("agent_id=none"), "{panes}");
    assert!(panes.contains("size="), "{panes}");
    let clients = display_body(clients);
    assert!(clients.contains("role=primary"), "{clients}");
    assert!(clients.contains("attached_at="), "{clients}");
    assert!(clients.contains("last_seen_at="), "{clients}");
    assert!(
        clients.contains("terminal=80x24:term=screen-256color"),
        "{clients}"
    );
    assert!(
        clients.contains(&format!("approval={observer_request}:pending")),
        "{clients}"
    );
    assert!(
        clients.contains(&format!("{observer_client}:observer:role=pending_observer")),
        "{clients}"
    );
    assert!(
        clients.contains("terminal=100x30:term=xterm-256color"),
        "{clients}"
    );
    let observers = display_body(observers);
    assert!(observers.contains("state=pending"), "{observers}");
    assert!(observers.contains("requested_at="), "{observers}");
    assert!(observers.contains("decided_at=none"), "{observers}");
    assert!(observers.contains("decided_by=none"), "{observers}");
    assert!(observers.contains("visible_from_time=none"), "{observers}");
    assert!(
        observers.contains("terminal=100x30:term=xterm-256color"),
        "{observers}"
    );
    let choose_observer = display_body(choose_observer);
    assert!(
        choose_observer.contains("actions=inspect,approve,reject"),
        "{choose_observer}"
    );
    assert!(
        choose_observer.contains(&format!(
            "commands=approve-observer -t {observer_request}|reject-observer -t {observer_request}"
        )),
        "{choose_observer}"
    );
    assert!(
        choose_observer.contains(&format!("{observer_request}:client={observer_client}")),
        "{choose_observer}"
    );
    let sessions = display_body(sessions);
    assert!(sessions.contains("created_at="), "{sessions}");
    assert!(sessions.contains("last_attached_at="), "{sessions}");
    assert!(sessions.contains("attached_clients=1"), "{sessions}");
    assert!(sessions.contains("primary_available=false"), "{sessions}");
    let pane_selector = display_body(pane_selector);
    assert!(
        pane_selector.contains("action=select-pane -t 0"),
        "{pane_selector}"
    );
    assert!(
        pane_selector.contains("action=select-pane -t 1"),
        "{pane_selector}"
    );
    let window_chooser = display_body(window_chooser);
    assert!(
        window_chooser.contains("chooser=select-window"),
        "{window_chooser}"
    );
    assert!(
        window_chooser.contains("action=select-window -t @1"),
        "{window_chooser}"
    );
    let client_chooser = display_body(client_chooser);
    assert!(
        client_chooser.contains("chooser=detach-client"),
        "{client_chooser}"
    );
    assert!(
        client_chooser.contains(&format!("action=detach-client -t {observer_client}")),
        "{client_chooser}"
    );
    assert_eq!(
        display_body(send_prefix),
        "sent=false:reason=live-terminal-state-unavailable"
    );
}

/// Verifies rename and kill session commands execute.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rename_and_kill_session_commands_execute() {
    let (mut session, primary) = test_session();

    execute_command_sequence(&mut session, &primary, "rename-session work").unwrap();
    assert_eq!(session.name, "work");

    let error = execute_command_sequence(&mut session, &primary, "kill-session").unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);

    execute_command_sequence(&mut session, &primary, "kill-session --force").unwrap();
    assert!(session.windows().is_empty());
}

/// Verifies command sequence stops on first failed command.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn command_sequence_stops_on_first_failed_command() {
    let (mut session, primary) = test_session();

    let error = execute_command_sequence(
        &mut session,
        &primary,
        "select-window missing; new-window should-not-run",
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::NotFound);
    assert_eq!(session.windows().len(), 1);
}
