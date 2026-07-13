//! Product integration tests for session restoration from persisted snapshots.

use super::{Session, SessionState};
use crate::shell::{ResolvedShell, ShellSource};
use crate::snapshot::{SessionSnapshotPayload, SnapshotPaneGeometry, SnapshotSessionState};
use mez_mux::layout::{LayoutPolicy, PaneGeometry};
use std::path::PathBuf;

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

    let restore_input = crate::snapshot::session_restore_input(&payload).unwrap();
    let mut session = Session::from_restore_input(shell, restore_input).unwrap();

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
