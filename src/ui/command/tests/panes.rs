//! Command panes tests.

use super::*;

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
