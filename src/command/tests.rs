//! Regression coverage for the command tests subsystem.
//!
//! These tests describe the behavior protected by the repository
//! specification and workflow guidance. Keeping the scenarios documented
//! makes failures easier to map back to the user-visible contract.

// Command module tests.

use super::{
    AuditLog, AuthStore, CommandOutcome, PaneReadinessOverrideStore, PaneReadinessState,
    baseline_commands, execute_auth_command, execute_command, execute_command_sequence,
    execute_config_store_command, execute_mark_pane_ready_command, execute_mcp_config_command,
    parse_command_sequence,
};
use crate::auth::AuthPaths;
use crate::config::ConfigPaths;
use crate::ids::ClientId;
use crate::layout::Size;
use crate::session::{ClientState, Session};
use crate::shell::{ResolvedShell, ShellSource};
use std::fs;
use std::path::PathBuf;

/// Runs the test session operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_session() -> (Session, ClientId) {
    let mut session = Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    );
    let primary = session.attach_primary("primary", true).unwrap();
    (session, primary)
}

/// Verifies parses command with quotes and target flag.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn parses_command_with_quotes_and_target_flag() {
    let commands = parse_command_sequence("rename-window -t @1 \"work tree\"").unwrap();

    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].name, "rename-window");
    assert_eq!(commands[0].target_arg(), Some("@1"));
    assert_eq!(commands[0].args[2], "work tree");
}

/// Verifies splits semicolon sequence outside quotes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn splits_semicolon_sequence_outside_quotes() {
    let commands = parse_command_sequence("select-window -t @1; rename-window 'a;b'").unwrap();

    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].name, "select-window");
    assert_eq!(commands[1].args[0], "a;b");
}

/// Verifies rejects unterminated quotes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_unterminated_quotes() {
    let error = parse_command_sequence("rename-window \"unterminated").unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
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
        Some(crate::session::ClientTerminalDescriptor {
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

/// Verifies executes split and pane selection commands.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn executes_split_and_pane_selection_commands() {
    let (mut session, primary) = test_session();

    execute_command_sequence(
        &mut session,
        &primary,
        "split-window --select; select-pane 0",
    )
    .unwrap();

    let window = session.active_window().unwrap();
    assert_eq!(window.panes().len(), 2);
    assert_eq!(window.active_pane().index, 0);
}

/// Verifies the established mux-compatible default that `split-window` moves focus to the
/// new pane unless the user passes the explicit detached/no-select flag.
#[test]
fn split_window_selects_new_pane_unless_detached() {
    let (mut session, primary) = test_session();

    execute_command_sequence(&mut session, &primary, "split-window").unwrap();
    assert_eq!(session.active_window().unwrap().active_pane().index, 1);

    execute_command_sequence(&mut session, &primary, "split-window -d").unwrap();
    assert_eq!(session.active_window().unwrap().active_pane().index, 1);
}

/// Verifies that the direction commands advertised by the default key-binding
/// display are executable command-language inputs. Without this coverage,
/// `list-keys` can report `select-pane -L` and `select-pane -R` commands that
/// fail at prompt submission time because they do not provide a target id.
#[test]
fn select_pane_direction_flags_focus_adjacent_panes() {
    let (mut session, primary) = test_session();

    execute_command_sequence(
        &mut session,
        &primary,
        "split-window --select; select-pane -L; select-pane -R",
    )
    .unwrap();

    let window = session.active_window().unwrap();
    assert_eq!(window.panes().len(), 2);
    assert_eq!(window.active_pane().index, 1);
}

/// Verifies that the target aliases used by advertised pane-cycling commands
/// are executable. This protects the default `Ctrl+A o` expansion
/// `select-pane -t next` and the corresponding last/previous command forms.
#[test]
fn select_pane_target_aliases_focus_relative_panes() {
    let (mut session, primary) = test_session();

    execute_command_sequence(
        &mut session,
        &primary,
        "split-window; select-pane -t next; select-pane -t last; select-pane -t next; select-pane -t previous",
    )
    .unwrap();

    let window = session.active_window().unwrap();
    assert_eq!(window.panes().len(), 2);
    assert_eq!(window.active_pane().index, 1);
}

/// Covers ambiguous directional pane selection. Multiple direction flags should
/// fail before focus changes so mistyped command-prompt input cannot select an
/// arbitrary adjacent pane.
#[test]
fn select_pane_rejects_multiple_direction_flags_without_focus_change() {
    let (mut session, primary) = test_session();

    execute_command_sequence(&mut session, &primary, "split-window --select").unwrap();
    let before = session.active_window().unwrap().active_pane().index;
    let error = execute_command_sequence(&mut session, &primary, "select-pane -L -R").unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert_eq!(session.active_window().unwrap().active_pane().index, before);
}

/// Verifies that the swap commands advertised by the default prefix table are
/// executable command-language inputs. This keeps `Ctrl+A {` and `Ctrl+A }`
/// command expansions aligned with the prompt/runtime command dispatcher.
#[test]
fn swap_pane_direction_flags_exchange_neighbor_panes() {
    let (mut session, primary) = test_session();

    execute_command_sequence(&mut session, &primary, "split-window").unwrap();
    let before = session
        .active_window()
        .unwrap()
        .panes()
        .iter()
        .map(|pane| pane.id.to_string())
        .collect::<Vec<_>>();
    execute_command_sequence(&mut session, &primary, "swap-pane -D").unwrap();

    let after = session
        .active_window()
        .unwrap()
        .panes()
        .iter()
        .map(|pane| pane.id.to_string())
        .collect::<Vec<_>>();
    assert_eq!(after, vec![before[1].clone(), before[0].clone()]);
}

/// Covers ambiguous directional pane swaps. Multiple direction flags should
/// fail before pane order changes because a command prompt typo must not select
/// an arbitrary swap target.
#[test]
fn swap_pane_rejects_multiple_direction_flags_without_reordering() {
    let (mut session, primary) = test_session();

    execute_command_sequence(&mut session, &primary, "split-window").unwrap();
    let before = session
        .active_window()
        .unwrap()
        .panes()
        .iter()
        .map(|pane| pane.id.to_string())
        .collect::<Vec<_>>();
    let error = execute_command_sequence(&mut session, &primary, "swap-pane -U -D").unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
    let after = session
        .active_window()
        .unwrap()
        .panes()
        .iter()
        .map(|pane| pane.id.to_string())
        .collect::<Vec<_>>();
    assert_eq!(after, before);
}

/// Verifies split window accepts shell command argument.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn split_window_accepts_shell_command_argument() {
    let (mut session, primary) = test_session();
    let invocation = parse_command_sequence("split-window --select -- printf 'a b'")
        .unwrap()
        .remove(0);
    let outcome = execute_command(&mut session, &primary, &invocation).unwrap();

    match outcome {
        CommandOutcome::MutatedWithPaneCommand {
            command,
            shell_command,
            start_directory,
        } => {
            assert_eq!(command, "split-window");
            assert_eq!(shell_command, "printf 'a b'");
            assert_eq!(start_directory, None);
        }
        other => panic!("expected pane command plan, got {other:?}"),
    }
    assert_eq!(session.active_window().unwrap().panes().len(), 2);
}

/// Verifies executes resize pane command.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn executes_resize_pane_command() {
    let (mut session, primary) = test_session();

    execute_command_sequence(
        &mut session,
        &primary,
        "split-window --select; resize-pane -t 1 -x 30 -y 12",
    )
    .unwrap();

    let window = session.active_window().unwrap();
    let pane = window.panes().iter().find(|pane| pane.index == 1).unwrap();
    assert_eq!(pane.size, Size::new(30, 12).unwrap());
}

/// Verifies resize pane command accepts delta percent and edge specs.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn resize_pane_command_accepts_delta_percent_and_edge_specs() {
    let (mut session, primary) = test_session();

    execute_command_sequence(
        &mut session,
        &primary,
        "resize-pane -R 5; resize-pane --percent 50 --axis rows; resize-pane --edge bottom --amount 3",
    )
    .unwrap();

    let pane = session.active_window().unwrap().active_pane();
    assert_eq!(pane.size, Size::new(85, 15).unwrap());
}

/// Verifies executes pane cycle zoom rotate and layout commands.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn executes_pane_cycle_zoom_rotate_and_layout_commands() {
    let (mut session, primary) = test_session();

    execute_command_sequence(&mut session, &primary, "split-window; next-pane; last-pane").unwrap();
    assert_eq!(session.active_window().unwrap().active_pane().index, 1);

    let zoom = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("resize-pane -Z").unwrap()[0],
    )
    .unwrap();
    assert!(display_body(zoom).contains("zoomed=%"));

    let before = session.active_window().unwrap().panes()[0].id.clone();
    execute_command_sequence(&mut session, &primary, "rotate-pane").unwrap();
    assert_ne!(session.active_window().unwrap().panes()[0].id, before);

    let layout = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("next-layout").unwrap()[0],
    )
    .unwrap();
    assert_eq!(display_body(layout), "layout=even-vertical");

    let selected_layout = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("select-layout even-horizontal").unwrap()[0],
    )
    .unwrap();
    assert_eq!(display_body(selected_layout), "layout=even-horizontal");
    assert_eq!(
        session.active_window().unwrap().layout_policy().name(),
        "even-horizontal"
    );

    let grid_layout = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("select-layout even-grid").unwrap()[0],
    )
    .unwrap();
    assert_eq!(display_body(grid_layout), "layout=even-grid");
    assert_eq!(
        session.active_window().unwrap().layout_policy().name(),
        "even-grid"
    );

    let invalid = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("select-layout unknown").unwrap()[0],
    )
    .unwrap_err();
    assert_eq!(invalid.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies rebalance window reapplies the selected layout policy.
///
/// This regression scenario protects the user-visible `rebalance-window`
/// command: direct pane resizing may temporarily disturb balanced geometry,
/// but rebalance must keep the active policy selected and recompute pane sizes
/// through the normal layout engine.
#[test]
fn rebalance_window_reapplies_active_layout_policy() {
    let (mut session, primary) = test_session();

    execute_command_sequence(&mut session, &primary, "split-window").unwrap();
    execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("select-layout even-horizontal").unwrap()[0],
    )
    .unwrap();
    assert_eq!(
        session
            .active_window()
            .unwrap()
            .panes()
            .iter()
            .map(|pane| pane.size.rows)
            .collect::<Vec<_>>(),
        vec![12, 12]
    );

    execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("resize-pane --percent 25 --axis rows").unwrap()[0],
    )
    .unwrap();
    assert_ne!(
        session
            .active_window()
            .unwrap()
            .panes()
            .iter()
            .map(|pane| pane.size.rows)
            .collect::<Vec<_>>(),
        vec![12, 12]
    );

    let rebalanced = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("rebalance-window").unwrap()[0],
    )
    .unwrap();
    assert_eq!(display_body(rebalanced), "layout=even-horizontal");
    assert_eq!(
        session
            .active_window()
            .unwrap()
            .panes()
            .iter()
            .map(|pane| pane.size.rows)
            .collect::<Vec<_>>(),
        vec![12, 12]
    );
}

/// Verifies executes baseline pane movement commands.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn executes_baseline_pane_movement_commands() {
    let (mut session, primary) = test_session();

    execute_command_sequence(&mut session, &primary, "split-window --select").unwrap();
    let first_id = session.active_window().unwrap().panes()[0].id.clone();
    let second_id = session.active_window().unwrap().panes()[1].id.clone();

    execute_command_sequence(&mut session, &primary, "swap-pane -t 0").unwrap();
    assert_eq!(session.active_window().unwrap().panes()[0].id, second_id);
    assert_eq!(session.active_window().unwrap().panes()[1].id, first_id);

    execute_command_sequence(&mut session, &primary, "break-pane -n moved").unwrap();
    assert_eq!(session.windows().len(), 2);
    assert_eq!(session.active_window().unwrap().name, "moved");

    execute_command_sequence(&mut session, &primary, "join-pane -t 0 --select").unwrap();
    assert_eq!(session.windows().len(), 1);
    assert_eq!(session.active_window().unwrap().panes().len(), 2);
}

/// Verifies attach session reports current in memory session.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn attach_session_reports_current_in_memory_session() {
    let (mut session, primary) = test_session();

    let outcome = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("attach-session").unwrap()[0],
    )
    .unwrap();

    let body = display_body(outcome);
    assert!(body.contains("attach=already-attached"));
    assert!(body.contains("role=primary"));
}

/// Verifies executes observer decision commands.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn executes_observer_decision_commands() {
    let (mut session, primary) = test_session();
    let (_approved_client, approved_request) = session.request_observer("approved");
    let (_rejected_client, rejected_request) = session.request_observer("rejected");
    let (revoked_client, revoked_request) = session.request_observer("revoked");

    execute_command_sequence(
        &mut session,
        &primary,
        &format!(
            "approve-observer {}; reject-observer -t {}; approve-observer {}",
            approved_request, rejected_request, revoked_request
        ),
    )
    .unwrap();
    execute_command_sequence(
        &mut session,
        &primary,
        &format!("revoke-observer {}", revoked_client),
    )
    .unwrap();

    let observers = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("list-observers").unwrap()[0],
        )
        .unwrap(),
    );
    assert!(observers.contains(&format!("{}:client=", approved_request)));
    assert!(observers.contains("state=approved"));
    assert!(observers.contains("state=rejected"));
    assert!(observers.contains("state=revoked"));
}

/// Verifies mcp config commands report config control plans.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mcp_config_commands_report_config_control_plans() {
    let (mut session, primary) = test_session();

    let add = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("mcp-add fs --command mcp-fs --arg --root --arg .").unwrap()[0],
    )
    .unwrap();
    let remove = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("mcp-remove fs").unwrap()[0],
    )
    .unwrap();
    let retry = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("mcp-retry fs").unwrap()[0],
    )
    .unwrap();

    assert!(display_body(add).contains("server=fs:transport=stdio:target=mcp-fs:args=2"));
    assert!(display_body(remove).contains("server=fs:removed=false"));
    assert!(display_body(retry).contains("server=fs:retried=false"));
}

/// Verifies mcp config commands can mutate primary config store.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mcp_config_commands_can_mutate_primary_config_store() {
    let root = std::env::temp_dir().join(format!("mez-command-mcp-store-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let paths = ConfigPaths::from_root(root.clone());
    let add_invocation = parse_command_sequence("mcp-add fs --command mcp-fs --arg --root --arg .")
        .unwrap()
        .remove(0);

    let add = display_body(execute_mcp_config_command(&paths, &add_invocation).unwrap());

    assert!(add.contains("server=fs:transport=stdio:target=mcp-fs"));
    assert!(add.contains("changed=true"));
    assert!(add.contains("reload_required=true"));
    assert!(add.contains("source=config-store"));
    let text = fs::read_to_string(root.join("config.toml")).unwrap();
    assert!(text.contains("[mcp_servers.fs]"));
    assert!(text.contains("command = \"mcp-fs\""));
    assert!(text.contains("args = [\"--root\", \".\"]"));
    assert!(text.contains("enabled = true"));

    let remove_invocation = parse_command_sequence("mcp-remove fs").unwrap().remove(0);
    let remove = display_body(execute_mcp_config_command(&paths, &remove_invocation).unwrap());

    assert!(remove.contains("server=fs:removed=true"));
    assert!(remove.contains("changed=true"));
    assert!(remove.contains("reload_required=true"));
    let text = fs::read_to_string(root.join("config.toml")).unwrap();
    assert!(!text.contains("command = \"mcp-fs\""));
    assert!(!text.contains("args = [\"--root\", \".\"]"));

    let _ = fs::remove_dir_all(root);
}

/// Verifies config commands report live config requirements without store.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_commands_report_live_config_requirements_without_store() {
    let (mut session, primary) = test_session();

    let set = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("set-option history.lines 2048").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        set,
        "path=history.lines:value=2048:changed=false:reason=live-config-control-unavailable"
    );

    let theme = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("set-theme nord").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        theme,
        "theme=nord:changed=false:reason=live-config-control-unavailable"
    );

    let bind = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("bind-key C-a split-window -h").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        bind,
        "key=C-a:command=split-window -h:changed=false:reason=live-config-control-unavailable"
    );

    let unbind = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("unbind-key C-a").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        unbind,
        "key=C-a:removed=false:reason=live-config-control-unavailable"
    );
}

/// Verifies config store commands mutate primary config and validate source file.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_store_commands_mutate_primary_config_and_validate_source_file() {
    let root =
        std::env::temp_dir().join(format!("mez-command-config-store-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let paths = ConfigPaths::from_root(root.clone());
    let config_path = paths.ensure_default_config().unwrap();

    let set = display_body(
        execute_config_store_command(
            &paths,
            &parse_command_sequence("set-option history.lines 2048")
                .unwrap()
                .remove(0),
        )
        .unwrap(),
    );
    assert_eq!(
        set,
        "path=history.lines:changed=true:reload_required=true:source=config-store"
    );
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("lines = 2048"));

    let set_theme = display_body(
        execute_config_store_command(
            &paths,
            &parse_command_sequence("set-theme nord").unwrap().remove(0),
        )
        .unwrap(),
    );
    assert!(set_theme.contains("theme=nord"), "{set_theme}");
    assert!(set_theme.contains("changed=true"), "{set_theme}");
    assert!(set_theme.contains("reload_required=true"), "{set_theme}");
    assert!(set_theme.contains("source=config-store"), "{set_theme}");
    assert!(set_theme.contains("aliases=8"), "{set_theme}");
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("active = \"nord\""));
    assert!(text.contains("primary = \"#88c0d0\""));
    assert!(text.contains("window_active_bg = \"primary\""));
    assert!(!text.contains("#7e9cd8"));

    let bind = display_body(
        execute_config_store_command(
            &paths,
            &parse_command_sequence("bind-key C-a split-window -h")
                .unwrap()
                .remove(0),
        )
        .unwrap(),
    );
    assert!(bind.contains("key=C-a:config_key=key_43_2d_61"));
    assert!(bind.contains("command=split-window -h"));
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(text.contains("[keys.command_bindings]"));
    assert!(text.contains("key_43_2d_61 = \"split-window -h\""));

    let unbind = display_body(
        execute_config_store_command(
            &paths,
            &parse_command_sequence("unbind-key C-a").unwrap().remove(0),
        )
        .unwrap(),
    );
    assert!(unbind.contains("removed=true"));
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(!text.contains("key_43_2d_61 = \"split-window -h\""));

    let source = display_body(
        execute_config_store_command(
            &paths,
            &parse_command_sequence(&format!("source-file {}", config_path.display()))
                .unwrap()
                .remove(0),
        )
        .unwrap(),
    );
    assert!(source.contains("valid=true"));
    assert!(source.contains("diagnostics=0"));
    assert!(source.contains("source=config-store"));

    let _ = fs::remove_dir_all(root);
}

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
    assert!(body.contains("attach-session:status=implemented"));
    assert!(body.contains("copy-mode:status=runtime-required"));
    assert!(body.contains("show-messages:status=implemented"));
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
    assert!(body.contains("auth-login:status=store-required"));
    assert!(body.contains("auth-status:status=store-required"));
    assert!(body.contains("mcp-add:status=store-required"));
    assert!(body.contains("mcp-remove:status=store-required"));
    assert!(body.contains("mcp-retry:status=runtime-required"));
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
    assert!(body.contains("snapshot-session:status=control-required"));
    assert!(body.contains("resume-session:status=control-required"));
    assert!(body.contains("list-observers:status=implemented"));
    assert!(body.contains("choose-observer:status=implemented"));
    assert!(body.contains("approve-observer:status=runtime-required"));
    assert!(body.contains("reject-observer:status=runtime-required"));
    assert!(body.contains("revoke-observer:status=runtime-required"));
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

    assert!(help.contains("mezzanine command help"), "{help}");
    assert!(help.contains("agent and integrations"), "{help}");
    assert!(help.contains("configuration"), "{help}");
    assert!(help.contains("copy, buffers, and history"), "{help}");
    assert!(help.contains("diagnostics and help"), "{help}");
    assert!(help.contains("sessions and clients"), "{help}");
    assert!(help.contains("windows, groups, and panes"), "{help}");
    assert!(help.contains("list-commands"), "{help}");
    assert!(help.contains("list-keys"), "{help}");
    assert!(help.contains("rebalance-window"), "{help}");
    assert!(help.contains("set-theme"), "{help}");
    assert!(help.contains("agent-shell"), "{help}");
    assert!(help.contains("snapshot-session"), "{help}");
    assert!(help.contains("\nkey bindings\n"), "{help}");
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
        help.find("windows, groups, and panes").unwrap() < help.find("\nkey bindings\n").unwrap(),
        "{help}"
    );
    let last_line = help.lines().last().unwrap_or_default();
    assert!(last_line.contains("C-a ~"), "{help}");
    assert!(last_line.contains("show-messages"), "{help}");
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
    assert!(body.contains("theme=kanagawa:source=builtin:active=true:action=set-theme kanagawa"));
    assert!(body.contains("theme=deepforest:source=builtin:active=false"));
    assert!(body.contains("theme=gruvbox_dark:source=builtin:active=false"));
    assert!(body.contains("theme=catppuccin_latte:source=builtin:active=false"));
    assert!(body.contains("theme=high_contrast_dark:source=builtin:active=false"));
    assert!(body.contains("theme=dracula:source=builtin:active=false"));
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
}

/// Verifies paste and history commands report live terminal requirements.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn paste_and_history_commands_report_live_terminal_requirements() {
    let (mut session, primary) = test_session();

    let buffers = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("list-buffers").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(buffers, "buffers=0 source=not-connected status=empty");

    let paste = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("paste-buffer -b build").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        paste,
        "buffer=build:paste=not-sent:reason=live-terminal-state-unavailable"
    );

    let create = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("create-buffer -b build --content seed").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        create,
        "buffer=build:created=false:reason=live-paste-buffer-unavailable"
    );

    let paste_clipboard = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("paste-clipboard -t 0").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        paste_clipboard,
        "target=0:paste=not-sent:source=clipboard-or-buffer:reason=live-terminal-state-unavailable"
    );

    let copy_selection = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("copy-selection -t 0").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        copy_selection,
        "target=0:copy=not-copied:reason=live-terminal-state-unavailable"
    );

    let capture = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("capture-pane -p -t 0").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        capture,
        "target=0:capture=not-read:output=stdout:reason=live-terminal-state-unavailable"
    );

    let save = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("save-buffer -b build --output /tmp/buf.txt").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        save,
        "buffer=build:save=not-written:output=/tmp/buf.txt:reason=live-paste-buffer-unavailable"
    );

    let clear = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("clear-history -t 0").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        clear,
        "target=0:cleared=false:reason=live-terminal-state-unavailable"
    );

    let search = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("search-history error").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        search,
        "target=active-pane:matches=0:query=error:source=not-connected"
    );

    let export = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("export-history --output /tmp/history.txt").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        export,
        "target=active-pane:export=not-written:output=/tmp/history.txt:reason=live-terminal-state-unavailable"
    );

    let pipe = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("pipe-pane -t 0 cat >/tmp/pane.log").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        pipe,
        "target=0:pipe=not-started:command=cat >/tmp/pane.log:reason=live-terminal-state-unavailable"
    );

    let snapshot = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("snapshot-session --name checkpoint").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        snapshot,
        "name=checkpoint:snapshot=not-created:reason=live-control-unavailable"
    );

    let resume = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("resume-session snap-1").unwrap()[0],
        )
        .unwrap(),
    );
    assert_eq!(
        resume,
        "snapshot=snap-1:resume=not-started:reason=live-control-unavailable"
    );

    let error = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("delete-buffer missing").unwrap()[0],
    )
    .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::NotFound);
}

/// Verifies mark pane ready requires acknowledgement before store mutation.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mark_pane_ready_requires_acknowledgement_before_store_mutation() {
    let (session, primary) = test_session();
    let pane_id = session
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut store = PaneReadinessOverrideStore::default();
    store.record_pending_probe(&pane_id).unwrap();

    let warning = display_body(
        execute_mark_pane_ready_command(
            &session,
            &primary,
            &mut store,
            &parse_command_sequence("mark-pane-ready").unwrap()[0],
            PaneReadinessState::Unknown,
            3,
            None,
        )
        .unwrap(),
    );

    assert!(warning.contains(&format!("pane={pane_id}")));
    assert!(warning.contains("acknowledgement_required=true"));
    assert!(warning.contains("override=not-applied"));
    assert!(store.has_pending_probe(&pane_id));

    let applied = display_body(
        execute_mark_pane_ready_command(
            &session,
            &primary,
            &mut store,
            &parse_command_sequence("mark-pane-ready --acknowledge-risk").unwrap()[0],
            PaneReadinessState::PromptCandidate,
            3,
            None,
        )
        .unwrap(),
    );

    assert_eq!(
        applied,
        format!(
            "pane={pane_id}:readiness_state=prompt-candidate:override=applied:epoch=3:pending_probe_cleared=true:audit=not-configured:source=readiness-store"
        )
    );
    assert!(store.allows_epoch(&pane_id, 3));
    assert!(!store.has_pending_probe(&pane_id));
}

/// Verifies mark pane ready can emit audit record.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn mark_pane_ready_can_emit_audit_record() {
    let (session, primary) = test_session();
    let pane_id = session
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let mut store = PaneReadinessOverrideStore::default();
    let root =
        std::env::temp_dir().join(format!("mez-mark-pane-ready-audit-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let audit_path = root.join("audit.jsonl");
    let mut audit_log = AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    });

    let applied = display_body(
        execute_mark_pane_ready_command(
            &session,
            &primary,
            &mut store,
            &parse_command_sequence("mark-pane-ready --acknowledge-risk --reason manual").unwrap()
                [0],
            PaneReadinessState::Degraded,
            4,
            Some(&mut audit_log),
        )
        .unwrap(),
    );

    assert!(applied.contains("audit=written"));
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"agent_readiness""#));
    assert!(audit.contains(r#""action":"mark_pane_ready""#));
    assert!(audit.contains(r#""outcome":"applied""#));
    assert!(audit.contains(&format!(r#""pane_id":"{}""#, pane_id)));
    assert!(audit.contains(r#""reason":"manual""#));

    let _ = fs::remove_dir_all(root);
}

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

    let login = display_body(
        execute_command(
            &mut session,
            &primary,
            &parse_command_sequence("auth-login").unwrap()[0],
        )
        .unwrap(),
    );
    assert!(login.contains("method=browser"));
    assert!(login.contains("action=plan-only"));

    let logout = execute_command(
        &mut session,
        &primary,
        &parse_command_sequence("auth-logout").unwrap()[0],
    )
    .unwrap_err();
    assert_eq!(logout.kind(), crate::error::MezErrorKind::InvalidArgs);
    assert!(logout.message().contains("unknown command"));
}

/// Verifies auth login defaults to browser interactive requirement when store is connected.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
/// Verifies that the store-backed terminal auth command mirrors the CLI's
/// browser-first default without prompting for an API-key secret implicitly.
fn auth_login_defaults_to_browser_interactive_requirement_when_store_is_connected() {
    let root = std::env::temp_dir().join(format!(
        "mez-command-auth-default-login-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let auth_store = AuthStore::new(AuthPaths::under_config_root(&root));

    let login_invocation = parse_command_sequence("auth-login").unwrap().remove(0);
    let login = display_body(execute_auth_command(&auth_store, &login_invocation).unwrap());

    assert!(login.contains("provider=openai"));
    assert!(login.contains("method=browser"));
    assert!(login.contains("authenticated=false"));
    assert!(login.contains("action=interactive-required"));
    assert!(login.contains("run-mez-auth-login"));
    assert!(login.contains("source=auth-store"));

    let api_key_invocation = parse_command_sequence("auth-login --api-key")
        .unwrap()
        .remove(0);
    let api_key_login =
        display_body(execute_auth_command(&auth_store, &api_key_invocation).unwrap());

    assert!(api_key_login.contains("provider=openai"));
    assert!(api_key_login.contains("method=api-key"));
    assert!(api_key_login.contains("action=prompt-required"));
    assert!(api_key_login.contains("source=auth-store"));

    let _ = fs::remove_dir_all(root);
}

/// Verifies auth login browser and device code report interactive store flows.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
/// Verifies that explicit browser/device-code terminal auth commands direct the
/// user to the interactive CLI flow instead of reporting a completed login.
fn auth_login_browser_and_device_code_report_interactive_store_flows() {
    let root = std::env::temp_dir().join(format!(
        "mez-command-auth-unsupported-login-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let auth_store = AuthStore::new(AuthPaths::under_config_root(&root));

    for (command, method) in [
        ("auth-login --browser", "browser"),
        ("auth-login --device-code", "device-code"),
        ("auth-login --device-auth", "device-code"),
    ] {
        let invocation = parse_command_sequence(command).unwrap().remove(0);
        let login = display_body(execute_auth_command(&auth_store, &invocation).unwrap());

        assert!(login.contains("provider=openai"));
        assert!(login.contains(&format!("method={method}")));
        assert!(login.contains("authenticated=false"));
        assert!(login.contains("action=interactive-required"));
        assert!(login.contains("run-mez-auth-login"));
    }

    let _ = fs::remove_dir_all(root);
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

    let login_invocation = parse_command_sequence(&format!(
        "auth-login --api-key --credential-store file --api-key-file {} --profile work",
        key_file.display()
    ))
    .unwrap()
    .remove(0);
    let login = display_body(execute_auth_command(&auth_store, &login_invocation).unwrap());

    assert!(login.contains("provider=openai"));
    assert!(login.contains("authenticated=true"));
    assert!(login.contains("selected_model_profile=work"));
    assert!(login.contains("credential_store=file"));
    assert!(!login.contains("sk-command-secret"));

    let status_invocation = parse_command_sequence("auth-status").unwrap().remove(0);
    let status = display_body(execute_auth_command(&auth_store, &status_invocation).unwrap());

    assert!(status.contains("authenticated=true"));
    assert!(status.contains("provider=openai"));
    assert!(status.contains("profile=work"));
    assert!(status.contains("credential_store=file"));
    assert!(!status.contains("sk-command-secret"));

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

/// Runs the display body operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn display_body(outcome: CommandOutcome) -> String {
    match outcome {
        CommandOutcome::Display { body, .. } => body,
        _ => panic!("expected display outcome"),
    }
}

/// Runs the assert noop operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn assert_noop(outcome: CommandOutcome, expected_command: &str) {
    match outcome {
        CommandOutcome::Noop { command } => assert_eq!(command, expected_command),
        _ => panic!("expected noop outcome"),
    }
}
