//! Unit tests for session lifecycle, client ownership, and window operations.

use super::{
    ClientRole, ClientState, ClientTerminalDescriptor, ObserverDecisionState, Session, SessionState,
};
use crate::layout::{LayoutPolicy, PaneGeometry, PaneNavigationDirection, Size, SplitDirection};
use crate::shell::{ResolvedShell, ShellSource};
use crate::snapshot::{SessionSnapshotPayload, SnapshotPaneGeometry, SnapshotSessionState};
use std::path::PathBuf;

/// Runs the test session operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_session() -> Session {
    Session::new_default(
        ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        Size::new(80, 24).unwrap(),
    )
}

/// Verifies new session has one window and one pane.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn new_session_has_one_window_and_one_pane() {
    let session = test_session();

    assert_eq!(session.windows().len(), 1);
    assert_eq!(session.windows()[0].panes().len(), 1);
    assert!(session.windows()[0].created_at_unix_seconds.is_some());
    assert_eq!(session.state, SessionState::Running);
    assert!(session.created_at_unix_seconds > 0);
    assert_eq!(
        session.updated_at_unix_seconds,
        session.created_at_unix_seconds
    );
    assert_eq!(session.last_attached_at_unix_seconds, None);
}

/// Verifies new sessions start with one hidden-by-default window group.
///
/// Window groups are an organizational layer over windows, so the initial
/// session must still look like the legacy one-window layout while carrying a
/// coherent default group for later group creation, switching, and removal.
#[test]
fn new_session_has_single_default_window_group() {
    let session = test_session();

    assert_eq!(session.window_groups().len(), 1);
    let group = session.active_group().unwrap();
    assert_eq!(group.index, 0);
    assert_eq!(group.name, "0");
    assert_eq!(group.window_ids, vec![session.windows()[0].id.clone()]);
    assert_eq!(
        group.active_window_id,
        Some(session.windows()[0].id.clone())
    );
    assert_eq!(session.active_group_windows().len(), 1);
}

/// Verifies primary requires interactive terminal.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn primary_requires_interactive_terminal() {
    let mut session = test_session();

    let error = session.attach_primary("client", false).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
}

/// Verifies primary attach preserves supplied terminal descriptor.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn primary_attach_preserves_supplied_terminal_descriptor() {
    let mut session = test_session();

    let primary = session
        .attach_primary_with_terminal(
            "client",
            true,
            Some(ClientTerminalDescriptor {
                columns: 132,
                rows: 43,
                term: "xterm-256color".to_string(),
                features: Vec::new(),
            }),
        )
        .unwrap();

    let client = session
        .clients()
        .iter()
        .find(|client| client.id == primary)
        .unwrap();
    assert_eq!(
        client.terminal,
        Some(ClientTerminalDescriptor {
            columns: 132,
            rows: 43,
            term: "xterm-256color".to_string(),
            features: Vec::new(),
        })
    );
}

/// Verifies primary attach rejects invalid terminal descriptor.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn primary_attach_rejects_invalid_terminal_descriptor() {
    let mut session = test_session();

    let error = session
        .attach_primary_with_terminal(
            "client",
            true,
            Some(ClientTerminalDescriptor {
                columns: 0,
                rows: 24,
                term: "xterm-256color".to_string(),
                features: Vec::new(),
            }),
        )
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies primary selection is atomic and requires interactive target.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn primary_selection_is_atomic_and_requires_interactive_target() {
    let mut session = test_session();
    let first = session.attach_primary("first", true).unwrap();
    session.detach_primary(&first).unwrap();
    let second = session.attach_primary("second", true).unwrap();

    let selected = session
        .select_primary_client(Some(&second), first.as_str())
        .unwrap();

    assert_eq!(selected, first);
    assert_eq!(session.primary_client_id(), Some(&first));
    assert_eq!(
        session
            .clients()
            .iter()
            .filter(|client| client.role == ClientRole::Primary)
            .count(),
        1
    );

    let (_observer_client, observer_request) = session.request_observer("observer");
    session
        .reject_observer_target(&first, observer_request.as_str())
        .unwrap();
    let revoked_observer_client = session.clients().last().unwrap().id.to_string();
    let error = session
        .select_primary_client(Some(&first), &revoked_observer_client)
        .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
}

/// Verifies enforces single primary.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn enforces_single_primary() {
    let mut session = test_session();
    session.attach_primary("first", true).unwrap();

    let error = session.attach_primary("second", true).unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Conflict);
}

/// Verifies observer starts pending and approval sets visibility marker.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn observer_starts_pending_and_approval_sets_visibility_marker() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    assert!(session.last_attached_at_unix_seconds.is_some());
    let (_client, observer) = session.request_observer("observer");
    assert!(session.clients()[0].attached_at_unix_seconds.is_some());
    assert!(session.clients()[0].last_seen_at_unix_seconds.is_some());
    assert_eq!(session.clients()[1].attached_at_unix_seconds, None);
    assert_eq!(session.clients()[1].last_seen_at_unix_seconds, None);
    assert_eq!(session.observers()[0].descriptor_name, "observer");
    assert!(!session.observers()[0].descriptor_interactive);
    assert_eq!(session.observers()[0].descriptor_terminal, None);
    assert!(session.observers()[0].requested_at_unix_seconds.is_some());
    assert_eq!(session.observers()[0].decided_at_unix_seconds, None);
    assert_eq!(session.observers()[0].decided_by_client_id, None);
    assert_eq!(session.observers()[0].visible_from_unix_seconds, None);
    assert_eq!(session.observers()[0].reason, None);

    session.approve_observer(&primary, &observer).unwrap();

    let observer = &session.observers()[0];
    assert_eq!(observer.state, ObserverDecisionState::Approved);
    assert!(observer.decided_at_unix_seconds.is_some());
    assert_eq!(
        observer.decided_by_client_id.as_deref(),
        Some(primary.as_str())
    );
    assert!(observer.visible_from_event_id.is_some());
    assert_eq!(
        observer.visible_from_unix_seconds,
        observer.decided_at_unix_seconds
    );
    assert!(session.clients()[1].attached_at_unix_seconds.is_some());
    assert!(session.clients()[1].last_seen_at_unix_seconds.is_some());
}

/// Verifies observer request preserves supplied terminal descriptor.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn observer_request_preserves_supplied_terminal_descriptor() {
    let mut session = test_session();

    let (client, observer) = session.request_observer_with_terminal(
        "observer",
        Some(ClientTerminalDescriptor {
            columns: 132,
            rows: 43,
            term: "xterm-256color".to_string(),
            features: Vec::new(),
        }),
    );

    let request = session
        .observers()
        .iter()
        .find(|request| request.id == observer)
        .unwrap();
    assert_eq!(request.client_id, client);
    assert_eq!(
        request.descriptor_terminal,
        Some(ClientTerminalDescriptor {
            columns: 132,
            rows: 43,
            term: "xterm-256color".to_string(),
            features: Vec::new(),
        })
    );
    assert_eq!(
        session
            .clients()
            .iter()
            .find(|candidate| candidate.id == client)
            .unwrap()
            .terminal,
        None
    );
}

/// Verifies primary can reject revoke and detach observers.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn primary_can_reject_revoke_and_detach_observers() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    let (_rejected_client, rejected) = session.request_observer("rejected");
    session
        .reject_observer_target_with_reason(&primary, rejected.as_str(), Some("not today".into()))
        .unwrap();
    assert_eq!(
        session.observers()[0].state,
        ObserverDecisionState::Rejected
    );
    assert!(session.observers()[0].decided_at_unix_seconds.is_some());
    assert_eq!(
        session.observers()[0].decided_by_client_id.as_deref(),
        Some(primary.as_str())
    );
    assert_eq!(session.observers()[0].reason.as_deref(), Some("not today"));

    let (client, approved) = session.request_observer("approved");
    session
        .approve_observer_target(&primary, approved.as_str())
        .unwrap();
    session
        .revoke_observer_client(&primary, client.as_str())
        .unwrap();
    assert_eq!(session.observers()[1].state, ObserverDecisionState::Revoked);
    assert!(session.observers()[1].decided_at_unix_seconds.is_some());
    assert_eq!(
        session.observers()[1].decided_by_client_id.as_deref(),
        Some(primary.as_str())
    );

    let (client, _pending) = session.request_observer("detached");
    session
        .detach_client_target(&primary, client.as_str())
        .unwrap();
    assert_eq!(
        session.clients().last().unwrap().state,
        ClientState::Detached
    );
}

/// Verifies primary can create and select windows.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn primary_can_create_and_select_windows() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    let window_id = session
        .new_window(&primary, "work", true)
        .unwrap()
        .to_string();

    assert_eq!(session.windows().len(), 2);
    assert_eq!(session.active_window().unwrap().id.to_string(), window_id);
    assert!(session.windows()[1].created_at_unix_seconds.is_some());
    assert_eq!(session.window_groups().len(), 1);
    assert_eq!(session.active_group_windows().len(), 2);

    session.select_window(&primary, "0").unwrap();
    assert_eq!(session.active_window().unwrap().index, 0);
}

/// Verifies primary can create and switch between window groups.
///
/// Creating a group must create a landing window, select the group by default,
/// and preserve the previous group so next, previous, and last-group navigation
/// can round-trip between independent window collections.
#[test]
fn primary_can_create_and_switch_window_groups() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    let default_window = session.windows()[0].id.clone();

    let (group_id, window_id) = session.new_group(&primary, "work", true).unwrap();

    assert_eq!(session.window_groups().len(), 2);
    assert_eq!(session.active_group().unwrap().id, group_id);
    assert_eq!(session.active_window().unwrap().id, window_id);
    assert_eq!(session.active_group_windows().len(), 1);
    assert_eq!(session.active_group_windows()[0].id, window_id);

    let selected_effects = session.select_group_transition(&primary, "0").unwrap();
    assert_eq!(selected_effects.len(), 2);
    assert_eq!(session.active_window().unwrap().id, default_window);

    let next_effects = session.next_group_transition(&primary).unwrap();
    assert_eq!(next_effects.len(), 2);
    assert_eq!(session.active_group().unwrap().id, group_id);
    let previous_effects = session.previous_group_transition(&primary).unwrap();
    assert_eq!(previous_effects.len(), 2);
    assert_eq!(session.active_window().unwrap().id, default_window);
    let last_effects = session.last_group_transition(&primary).unwrap();
    assert_eq!(last_effects.len(), 2);
    assert_eq!(session.active_group().unwrap().id, group_id);
}

/// Verifies window cycling is scoped to the active window group.
///
/// Once more than one group exists, next-window and previous-window should
/// follow the visible window bar for the active group instead of accidentally
/// traversing windows owned by hidden groups.
#[test]
fn window_navigation_is_scoped_to_active_group() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    let first = session.windows()[0].id.clone();
    let second = session.new_window(&primary, "second", true).unwrap();
    let (_group_id, hidden_window) = session.new_group(&primary, "hidden", true).unwrap();

    session.select_group(&primary, "0").unwrap();
    session.next_window(&primary).unwrap();
    assert_eq!(session.active_window().unwrap().id, first);
    session.previous_window(&primary).unwrap();
    assert_eq!(session.active_window().unwrap().id, second);

    session.select_group(&primary, "1").unwrap();
    session.next_window(&primary).unwrap();
    assert_eq!(session.active_window().unwrap().id, hidden_window);
}

/// Verifies killing a group's final window removes the group.
///
/// Closing the last window in a group should close that group as a side effect,
/// while the final remaining group stays available and must be removed through
/// session shutdown rather than kill-group.
#[test]
fn closing_last_window_in_group_closes_group() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    let default_window = session.windows()[0].id.clone();
    let (_group_id, window_id) = session.new_group(&primary, "work", true).unwrap();

    let removed = session
        .kill_window(&primary, Some(window_id.as_str()), true)
        .unwrap();

    assert_eq!(removed.id, window_id);
    assert_eq!(session.window_groups().len(), 1);
    assert_eq!(session.active_window().unwrap().id, default_window);
    assert_eq!(
        session.active_group().unwrap().window_ids,
        vec![default_window]
    );

    let error = session.kill_group(&primary, None, true).unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
}

/// Verifies kill-group removes all windows in the target group.
///
/// The group command is intentionally broader than kill-window, so it must
/// remove every owned window, reject live panes without force, and leave the
/// remaining group active with coherent active-window state.
#[test]
fn kill_group_removes_owned_windows_and_preserves_remaining_group() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    let default_window = session.windows()[0].id.clone();
    let (group_id, first_group_window) = session.new_group(&primary, "work", true).unwrap();
    let second_group_window = session.new_window(&primary, "logs", true).unwrap();

    let error = session
        .kill_group(&primary, Some(group_id.as_str()), false)
        .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);

    let removed = session
        .kill_group(&primary, Some(group_id.as_str()), true)
        .unwrap();

    let removed_ids = removed
        .into_iter()
        .map(|window| window.id)
        .collect::<Vec<_>>();
    assert_eq!(removed_ids, vec![first_group_window, second_group_window]);
    assert_eq!(session.window_groups().len(), 1);
    assert_eq!(session.active_window().unwrap().id, default_window);
}

/// Verifies moving a window keeps group display order in sync.
///
/// The normal window bar displays windows from the active group, so move-window
/// must update the group's stable id order after it reorders the flat session
/// window vector.
#[test]
fn move_window_updates_active_group_display_order() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    let first = session.windows()[0].id.clone();
    let second = session.new_window(&primary, "second", true).unwrap();
    let third = session.new_window(&primary, "third", true).unwrap();

    session
        .move_window(&primary, Some(third.as_str()), 0)
        .unwrap();

    let ordered = session
        .active_group_windows()
        .into_iter()
        .map(|window| window.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(ordered, vec![third, first, second]);
}

/// Verifies select window targets id before colliding name.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn select_window_targets_id_before_colliding_name() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    let first = session.windows()[0].id.to_string();
    let second = session
        .new_window(&primary, "second", true)
        .unwrap()
        .to_string();

    session
        .rename_window(&primary, Some(&first), second.clone())
        .unwrap();
    session.select_window(&primary, &second).unwrap();

    assert_eq!(session.active_window().unwrap().id.as_str(), second);
}

/// Verifies rename window targets id before colliding name.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rename_window_targets_id_before_colliding_name() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    let first = session.windows()[0].id.to_string();
    let second = session
        .new_window(&primary, "second", true)
        .unwrap()
        .to_string();

    session
        .rename_window(&primary, Some(&first), second.clone())
        .unwrap();
    session
        .rename_window(&primary, Some(&second), "renamed")
        .unwrap();

    assert_eq!(session.windows()[0].name, second);
    assert_eq!(session.windows()[1].id.as_str(), second);
    assert_eq!(session.windows()[1].name, "renamed");
}

/// Verifies kill window targets id before colliding name.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn kill_window_targets_id_before_colliding_name() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    let first = session.windows()[0].id.to_string();
    let second = session
        .new_window(&primary, "second", true)
        .unwrap()
        .to_string();

    session
        .rename_window(&primary, Some(&first), second.clone())
        .unwrap();
    let removed = session.kill_window(&primary, Some(&second), true).unwrap();

    assert_eq!(removed.id.as_str(), second);
    assert_eq!(session.windows().len(), 1);
    assert_eq!(session.windows()[0].id.as_str(), first);
    assert_eq!(session.windows()[0].name, second);
}

/// Verifies join pane window destination targets id before colliding name.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn join_pane_window_destination_targets_id_before_colliding_name() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    let first = session.windows()[0].id.to_string();
    let source_pane_id = session.windows()[0].panes()[0].id.clone();
    let destination = session.new_window(&primary, "dest", true).unwrap();

    session
        .rename_window(&primary, Some(&first), destination.to_string())
        .unwrap();
    let joined = session
        .join_pane(
            &primary,
            Some(source_pane_id.as_str()),
            destination.as_str(),
            SplitDirection::Vertical,
            true,
        )
        .unwrap();

    assert_eq!(joined, source_pane_id);
    assert_eq!(session.windows().len(), 1);
    assert_eq!(session.windows()[0].id, destination);
    assert_eq!(session.windows()[0].panes().len(), 2);
}

/// Verifies primary can cycle and return to last window.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn primary_can_cycle_and_return_to_last_window() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    session.new_window(&primary, "one", true).unwrap();
    session.new_window(&primary, "two", true).unwrap();

    session.next_window(&primary).unwrap();
    assert_eq!(session.active_window().unwrap().index, 0);

    session.previous_window(&primary).unwrap();
    assert_eq!(session.active_window().unwrap().index, 2);

    session.last_window(&primary).unwrap();
    assert_eq!(session.active_window().unwrap().index, 0);
}

/// Verifies primary can rename and kill session with force.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn primary_can_rename_and_kill_session_with_force() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();

    session.rename_session(&primary, "work").unwrap();
    assert_eq!(session.name, "work");

    let error = session.kill_session(&primary, false).unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);

    session.kill_session(&primary, true).unwrap();
    assert_eq!(session.state, SessionState::Empty);
    assert!(session.windows().is_empty());
}

/// Verifies primary can split and select panes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn primary_can_split_and_select_panes() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    let original_pane_id = session
        .active_window()
        .unwrap()
        .active_pane()
        .id
        .to_string();
    let pane_id = session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();

    assert_eq!(session.active_window().unwrap().panes().len(), 2);
    assert_ne!(pane_id.to_string(), original_pane_id);
    assert_eq!(
        session
            .active_window()
            .unwrap()
            .active_pane()
            .id
            .to_string(),
        pane_id.to_string()
    );

    session.select_pane(&primary, "0").unwrap();
    assert_eq!(session.active_window().unwrap().active_pane().index, 0);

    session
        .split_active_pane_select(&primary, SplitDirection::Horizontal, true)
        .unwrap();
    assert_eq!(session.active_window().unwrap().active_pane().index, 1);
}

/// Verifies that snapshot restore rebuilds the saved session topology, seeds
/// future ids past restored ids, and carries complete pane rectangle metadata
/// into the restored layout model. The geometry assertion keeps snapshot resume
/// aligned with the stored pane-rectangle conformance work instead of silently
/// falling back to inferred rectangles when metadata is present.
#[test]
fn session_restores_layout_from_snapshot_payload_and_seeds_ids() {
    let shell = ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh);
    let payload = SessionSnapshotPayload {
        session_id: "$4".to_string(),
        name: "restored".to_string(),
        state: SnapshotSessionState::Detached,
        authoritative_columns: 100,
        authoritative_rows: 40,
        active_window_id: Some("@8".to_string()),
        shell: crate::snapshot::SnapshotShellMetadata::default(),
        active_config_layers: Vec::new(),
        frame_state: crate::snapshot::SnapshotFrameState::default(),
        agent_sessions: Vec::new(),
        approval_grants: Vec::new(),
        approval_requests: Vec::new(),
        message_state: None,
        mcp_servers: Vec::new(),
        window_groups: Vec::new(),
        windows: vec![crate::snapshot::WindowSnapshotPayload {
            window_id: "@8".to_string(),
            index: 0,
            name: "work".to_string(),
            active: true,
            columns: 100,
            rows: 40,
            layout_policy: LayoutPolicy::EvenHorizontal.name().to_string(),
            layout_root: None,
            panes: vec![crate::snapshot::PaneSnapshotPayload {
                pane_id: "%12".to_string(),
                index: 0,
                title: "shell".to_string(),
                active: true,
                live_at_snapshot: true,
                columns: 100,
                rows: 40,
                primary_pid: Some(4242),
                process_state: "running".to_string(),
                current_working_directory: Some("/workspace/project".to_string()),
                readiness_state: "ready".to_string(),
                exit_status: None,
                geometry: Some(SnapshotPaneGeometry {
                    column: 0,
                    row: 0,
                    columns: 100,
                    rows: 40,
                }),
                terminal_modes: mez_terminal::TerminalModeState::default(),
                terminal_saved_state: mez_terminal::TerminalSavedState::default(),
                terminal_history: Vec::new(),
                terminal_history_line_style_spans: Vec::new(),
                visible_lines: Vec::new(),
                visible_line_style_spans: Vec::new(),
                alternate_screen_active: false,
                transcript_refs: Vec::new(),
            }],
        }],
    };

    let mut session = Session::from_snapshot_payload(shell, &payload).unwrap();

    assert_eq!(session.id.as_str(), "$4");
    assert_eq!(session.name, "restored");
    assert_eq!(session.state, SessionState::Detached);
    assert_eq!(session.active_window().unwrap().id.as_str(), "@8");
    assert_eq!(
        session.active_window().unwrap().active_pane().id.as_str(),
        "%12"
    );
    assert!(!session.active_window().unwrap().active_pane().live);
    assert_eq!(
        session.active_window().unwrap().pane_geometries(),
        vec![PaneGeometry {
            index: 0,
            column: 0,
            row: 0,
            columns: 100,
            rows: 40,
        }],
    );
    assert_eq!(
        session.active_window().unwrap().layout_policy(),
        LayoutPolicy::EvenHorizontal
    );

    let primary = session.attach_primary("primary", true).unwrap();
    let window_id = session.new_window(&primary, "next", true).unwrap();
    let pane_id = session
        .active_window()
        .and_then(|window| window.panes().first())
        .map(|pane| pane.id.clone())
        .unwrap();
    assert_eq!(window_id.as_str(), "@9");
    assert_eq!(pane_id.as_str(), "%13");
}

/// Verifies primary can cycle rotate zoom and cycle layouts.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn primary_can_cycle_rotate_zoom_and_cycle_layouts() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane_select(&primary, SplitDirection::Vertical, true)
        .unwrap();
    session
        .split_active_pane_select(&primary, SplitDirection::Horizontal, true)
        .unwrap();
    let ids = session.active_window().unwrap().panes().to_vec();

    let selected = session
        .select_adjacent_pane(&primary, PaneNavigationDirection::Down)
        .unwrap();
    assert_eq!(selected, ids[1].id);

    let last = session.select_last_pane(&primary).unwrap();
    assert_eq!(last, ids[2].id);

    let (zoomed, zoom_effects) = session
        .toggle_active_pane_zoom_transition(&primary)
        .unwrap();
    assert_eq!(zoomed, Some(ids[2].id.clone()));
    assert_eq!(zoom_effects.len(), 3);
    let (unzoomed, unzoom_effects) = session
        .toggle_active_pane_zoom_transition(&primary)
        .unwrap();
    assert_eq!(unzoomed, None);
    assert_eq!(unzoom_effects.len(), 3);

    let rotate_effects = session.rotate_panes_transition(&primary, false).unwrap();
    assert_eq!(session.active_window().unwrap().panes()[0].id, ids[1].id);
    assert_eq!(rotate_effects.len(), 3);
    assert_eq!(
        rotate_effects
            .iter()
            .map(|effect| effect.pane_id.clone())
            .collect::<Vec<_>>(),
        session
            .active_window()
            .unwrap()
            .panes()
            .iter()
            .map(|pane| pane.id.clone())
            .collect::<Vec<_>>()
    );

    let (policy, layout_effects) = session.cycle_layout_transition(&primary).unwrap();
    assert_eq!(policy, LayoutPolicy::EvenVertical);
    assert_eq!(layout_effects.len(), 3);
    assert_eq!(
        session.active_window().unwrap().panes()[0].size,
        Size::new(27, 24).unwrap()
    );
}

/// Verifies primary can resize target pane.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn primary_can_resize_target_pane() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    let pane_id = session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();

    let pane = session
        .resize_pane(&primary, Some(pane_id.as_str()), Size::new(20, 10).unwrap())
        .unwrap();

    assert_eq!(pane.id, pane_id);
    assert_eq!(pane.size, Size::new(20, 10).unwrap());
}

/// Verifies pane resize transitions describe every resulting pane size.
///
/// Runtime adapters synchronize PTYs and terminal surfaces from these effects,
/// so the transition must expose the complete post-layout pane-size set while
/// preserving the pane selected by the resize request.
#[test]
fn pane_resize_transition_describes_resulting_pane_sizes() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    let pane_id = session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();

    let transition = session
        .resize_pane_transition(&primary, Some(pane_id.as_str()), Size::new(20, 10).unwrap())
        .unwrap();

    assert_eq!(transition.pane.id, pane_id);
    assert_eq!(transition.pane.size, Size::new(20, 10).unwrap());
    let resulting_sizes = session
        .active_window()
        .unwrap()
        .panes()
        .iter()
        .map(|pane| (pane.id.clone(), pane.size))
        .collect::<Vec<_>>();
    let effect_sizes = transition
        .effects
        .into_iter()
        .map(|effect| (effect.pane_id, effect.size))
        .collect::<Vec<_>>();
    assert_eq!(effect_sizes, resulting_sizes);
}

/// Verifies selecting and rebalancing layouts describe every resulting pane size.
///
/// Runtime adapters consume these effects directly, so both layout mutations
/// must expose the complete post-layout pane-size set without rediscovery.
#[test]
fn layout_policy_transitions_describe_resulting_pane_sizes() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();

    let (selected, select_effects) = session
        .select_layout_transition(&primary, "even-horizontal")
        .unwrap();
    assert_eq!(selected, LayoutPolicy::EvenHorizontal);
    assert_eq!(select_effects.len(), 2);
    assert_eq!(
        select_effects
            .iter()
            .map(|effect| (effect.pane_id.clone(), effect.size))
            .collect::<Vec<_>>(),
        session
            .active_window()
            .unwrap()
            .panes()
            .iter()
            .map(|pane| (pane.id.clone(), pane.size))
            .collect::<Vec<_>>()
    );

    let (rebalanced, rebalance_effects) = session.rebalance_window_transition(&primary).unwrap();
    assert_eq!(rebalanced, LayoutPolicy::EvenHorizontal);
    assert_eq!(rebalance_effects.len(), 2);
    assert_eq!(
        rebalance_effects
            .iter()
            .map(|effect| (effect.pane_id.clone(), effect.size))
            .collect::<Vec<_>>(),
        session
            .active_window()
            .unwrap()
            .panes()
            .iter()
            .map(|pane| (pane.id.clone(), pane.size))
            .collect::<Vec<_>>()
    );
}

/// Verifies geometry replacement returns every resulting pane-size effect.
///
/// Pointer-driven border resizing uses this transition to synchronize product
/// PTYs and terminal surfaces without rediscovering layout output.
#[test]
fn pane_geometry_transition_describes_resulting_pane_sizes() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    let geometries = vec![
        PaneGeometry {
            index: 0,
            column: 0,
            row: 0,
            columns: 30,
            rows: 24,
        },
        PaneGeometry {
            index: 1,
            column: 30,
            row: 0,
            columns: 50,
            rows: 24,
        },
    ];

    let effects = session
        .replace_active_window_pane_geometries_transition(&primary, geometries)
        .unwrap();

    let resulting_sizes = session
        .active_window()
        .unwrap()
        .panes()
        .iter()
        .map(|pane| (pane.id.clone(), pane.size))
        .collect::<Vec<_>>();
    let effect_sizes = effects
        .into_iter()
        .map(|effect| (effect.pane_id, effect.size))
        .collect::<Vec<_>>();
    assert_eq!(effect_sizes, resulting_sizes);
}

/// Verifies authoritative terminal resize reports pane sizes across all windows.
///
/// Product adapters use this complete transition to resize every tracked PTY
/// and terminal surface without re-reading the session layout.
#[test]
fn authoritative_terminal_resize_transition_describes_all_panes() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();
    session.new_window(&primary, "second", true).unwrap();

    let effects = session
        .resize_authoritative_terminal_transition(&primary, Size::new(100, 30).unwrap())
        .unwrap();

    let resulting_sizes = session
        .windows()
        .iter()
        .flat_map(|window| window.panes())
        .map(|pane| (pane.id.clone(), pane.size))
        .collect::<Vec<_>>();
    let effect_sizes = effects
        .into_iter()
        .map(|effect| (effect.pane_id, effect.size))
        .collect::<Vec<_>>();
    assert_eq!(effect_sizes, resulting_sizes);
}

/// Verifies primary can swap panes in active window.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn primary_can_swap_panes_in_active_window() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane_select(&primary, SplitDirection::Vertical, true)
        .unwrap();
    let first_id = session.active_window().unwrap().panes()[0].id.clone();
    let second_id = session.active_window().unwrap().panes()[1].id.clone();

    session.swap_panes(&primary, None, "1").unwrap();

    let panes = session.active_window().unwrap().panes();
    assert_eq!(panes[0].id, first_id);
    assert_eq!(panes[1].id, second_id);
}

/// Verifies pane swaps expose the complete resulting pane-size set.
///
/// Product process and presentation adapters must be able to synchronize the
/// mutation without re-reading session layout or reconstructing affected panes.
#[test]
fn swap_panes_transition_describes_resulting_pane_sizes() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane_select(&primary, SplitDirection::Vertical, true)
        .unwrap();

    let effects = session.swap_panes_transition(&primary, None, "1").unwrap();

    let resulting_sizes = session
        .windows()
        .iter()
        .flat_map(|window| window.panes())
        .map(|pane| (pane.id.clone(), pane.size))
        .collect::<Vec<_>>();
    let effect_sizes = effects
        .into_iter()
        .map(|effect| (effect.pane_id, effect.size))
        .collect::<Vec<_>>();
    assert_eq!(effect_sizes, resulting_sizes);
}

/// Verifies primary can break pane into new window.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn primary_can_break_pane_into_new_window() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    let pane_id = session
        .split_active_pane_select(&primary, SplitDirection::Vertical, true)
        .unwrap();

    let window_id = session
        .break_pane(&primary, None, Some("moved".to_string()), true)
        .unwrap();

    assert_eq!(session.windows().len(), 2);
    let window = session.active_window().unwrap();
    assert_eq!(window.id, window_id);
    assert_eq!(window.name, "moved");
    assert_eq!(window.panes()[0].id, pane_id);
    assert_eq!(session.window_groups().len(), 1);
    assert!(
        session
            .active_group()
            .unwrap()
            .window_ids
            .contains(&window_id),
        "broken-out window should remain in the source group"
    );
}

/// Verifies pane breaks expose the complete resulting pane-size set.
///
/// Product process and presentation adapters must synchronize both the source
/// and newly created windows without rediscovering session layout.
#[test]
fn break_pane_transition_describes_resulting_pane_sizes() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane_select(&primary, SplitDirection::Vertical, true)
        .unwrap();

    let transition = session
        .break_pane_transition(&primary, None, Some("moved".to_string()), true)
        .unwrap();

    assert_eq!(transition.window_id, session.active_window().unwrap().id);
    let resulting_sizes = session
        .windows()
        .iter()
        .flat_map(|window| window.panes())
        .map(|pane| (pane.id.clone(), pane.size))
        .collect::<Vec<_>>();
    let effect_sizes = transition
        .effects
        .into_iter()
        .map(|effect| (effect.pane_id, effect.size))
        .collect::<Vec<_>>();
    assert_eq!(effect_sizes, resulting_sizes);
}

/// Verifies primary can join pane into destination window.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn primary_can_join_pane_into_destination_window() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    let source_pane_id = session.windows()[0].panes()[0].id.clone();
    let destination_window_id = session.new_window(&primary, "dest", true).unwrap();

    let joined = session
        .join_pane(
            &primary,
            Some(source_pane_id.as_str()),
            destination_window_id.as_str(),
            SplitDirection::Vertical,
            true,
        )
        .unwrap();

    assert_eq!(joined, source_pane_id);
    assert_eq!(session.windows().len(), 1);
    assert_eq!(session.active_window().unwrap().panes().len(), 2);
    assert!(
        session
            .active_window()
            .unwrap()
            .panes()
            .iter()
            .any(|pane| pane.id == source_pane_id)
    );
}

/// Verifies pane joins expose the complete resulting pane-size set.
///
/// Product process and presentation adapters must synchronize the destination
/// layout without reconstructing which panes changed during the move.
#[test]
fn join_pane_transition_describes_resulting_pane_sizes() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    let source_pane_id = session.windows()[0].panes()[0].id.clone();
    let destination_window_id = session.new_window(&primary, "dest", true).unwrap();

    let transition = session
        .join_pane_transition(
            &primary,
            Some(source_pane_id.as_str()),
            destination_window_id.as_str(),
            SplitDirection::Vertical,
            true,
        )
        .unwrap();

    assert_eq!(transition.pane_id, source_pane_id);
    let resulting_sizes = session
        .windows()
        .iter()
        .flat_map(|window| window.panes())
        .map(|pane| (pane.id.clone(), pane.size))
        .collect::<Vec<_>>();
    let effect_sizes = transition
        .effects
        .into_iter()
        .map(|effect| (effect.pane_id, effect.size))
        .collect::<Vec<_>>();
    assert_eq!(effect_sizes, resulting_sizes);
}

/// Verifies killing live pane requires force.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn killing_live_pane_requires_force() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();
    session
        .split_active_pane(&primary, SplitDirection::Vertical)
        .unwrap();

    let error = session.kill_pane(&primary, Some("1"), false).unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);

    let removed = session.kill_pane(&primary, Some("1"), true).unwrap();
    assert_eq!(removed.unwrap().index, 1);
    assert_eq!(session.active_window().unwrap().panes().len(), 1);
}

/// Verifies killing final window marks session empty.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn killing_final_window_marks_session_empty() {
    let mut session = test_session();
    let primary = session.attach_primary("primary", true).unwrap();

    session.kill_window(&primary, None, true).unwrap();

    assert!(session.windows().is_empty());
    assert_eq!(session.state, SessionState::Empty);
}
