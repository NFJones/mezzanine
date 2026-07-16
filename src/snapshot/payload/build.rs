//! Snapshot payload construction from live session state and captures.

use super::helpers::{default_pane_process_state, shell_metadata_from_session};
use super::{
    PaneSnapshotPayload, Session, SessionSnapshotPayload, SnapshotAgentSession,
    SnapshotConfigLayerMetadata, SnapshotCreationContext, SnapshotFrameState, SnapshotLayoutNode,
    SnapshotPaneCapture, SnapshotPaneGeometry, SnapshotSessionState, TerminalModeState,
    TerminalSavedState, WindowGroupSnapshotPayload, WindowSnapshotPayload,
};
impl SessionSnapshotPayload {
    /// Runs the from session operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_session(session: &Session) -> Self {
        Self::from_session_with_captures(session, &[])
    }

    /// Runs the from session with captures operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_session_with_captures(
        session: &Session,
        pane_captures: &[SnapshotPaneCapture],
    ) -> Self {
        Self::from_session_with_captures_and_config_layers(session, pane_captures, &[])
    }

    /// Runs the from session with captures and config layers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_session_with_captures_and_config_layers(
        session: &Session,
        pane_captures: &[SnapshotPaneCapture],
        active_config_layers: &[SnapshotConfigLayerMetadata],
    ) -> Self {
        Self::from_session_with_captures_and_config_layers_and_frame_state(
            session,
            pane_captures,
            active_config_layers,
            &SnapshotFrameState::default(),
        )
    }

    /// Runs the from session with captures and config layers and frame state operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_session_with_captures_and_config_layers_and_frame_state(
        session: &Session,
        pane_captures: &[SnapshotPaneCapture],
        active_config_layers: &[SnapshotConfigLayerMetadata],
        frame_state: &SnapshotFrameState,
    ) -> Self {
        Self::from_session_with_captures_config_layers_frame_state_and_agent_sessions(
            session,
            pane_captures,
            active_config_layers,
            frame_state,
            &[],
        )
    }

    /// Runs the from session with captures config layers frame state and agent sessions operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_session_with_captures_config_layers_frame_state_and_agent_sessions(
        session: &Session,
        pane_captures: &[SnapshotPaneCapture],
        active_config_layers: &[SnapshotConfigLayerMetadata],
        frame_state: &SnapshotFrameState,
        agent_sessions: &[SnapshotAgentSession],
    ) -> Self {
        Self::from_session_with_context(
            session,
            SnapshotCreationContext::new(
                pane_captures,
                active_config_layers,
                frame_state,
                agent_sessions,
            ),
        )
    }

    /// Runs the from session with context operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_session_with_context(
        session: &Session,
        context: SnapshotCreationContext<'_>,
    ) -> Self {
        let active_window_id = session.active_window().map(|window| window.id.to_string());
        let windows = session
            .windows()
            .iter()
            .map(|window| {
                let pane_geometries = window.pane_geometries();
                WindowSnapshotPayload {
                    window_id: window.id.to_string(),
                    index: window.index,
                    name: window.name.clone(),
                    active: Some(window.id.to_string()) == active_window_id,
                    columns: window.size.columns,
                    rows: window.size.rows,
                    layout_policy: window.layout_policy().name().to_string(),
                    layout_root: SnapshotLayoutNode::from_layout_node(
                        window.layout_root(),
                        window.panes(),
                    )
                    .ok(),
                    panes: window
                        .panes()
                        .iter()
                        .map(|pane| {
                            let pane_id = pane.id.to_string();
                            let current_working_directory = context
                                .pane_captures
                                .iter()
                                .find(|capture| capture.pane_id == pane_id)
                                .and_then(|capture| capture.current_working_directory.clone());
                            PaneSnapshotPayload {
                                pane_id,
                                index: pane.index,
                                title: pane.title.clone(),
                                active: pane.active,
                                live_at_snapshot: false,
                                columns: pane.size.columns,
                                rows: pane.size.rows,
                                primary_pid: None,
                                process_state: default_pane_process_state(false).to_string(),
                                current_working_directory,
                                readiness_state: "unknown".to_string(),
                                exit_status: None,
                                geometry: pane_geometries
                                    .iter()
                                    .find(|geometry| geometry.index == pane.index)
                                    .map(|geometry| SnapshotPaneGeometry {
                                        column: geometry.column,
                                        row: geometry.row,
                                        columns: geometry.columns,
                                        rows: geometry.rows,
                                    }),
                                terminal_modes: TerminalModeState::default(),
                                terminal_saved_state: TerminalSavedState::default(),
                                terminal_history: Vec::new(),
                                terminal_history_line_style_spans: Vec::new(),
                                visible_lines: Vec::new(),
                                visible_line_style_spans: Vec::new(),
                                alternate_screen_active: false,
                                transcript_refs: Vec::new(),
                            }
                        })
                        .collect(),
                }
            })
            .collect();
        let active_group_id = session.active_group().map(|group| group.id.to_string());
        let window_groups = session
            .window_groups()
            .iter()
            .map(|group| WindowGroupSnapshotPayload {
                group_id: group.id.to_string(),
                index: group.index,
                name: group.name.clone(),
                window_ids: group.window_ids.iter().map(ToString::to_string).collect(),
                active_window_id: group.active_window_id.as_ref().map(ToString::to_string),
                last_active_window_id: group
                    .last_active_window_id
                    .as_ref()
                    .map(ToString::to_string),
                active: Some(group.id.to_string()) == active_group_id,
            })
            .collect();

        Self {
            session_id: session.id.to_string(),
            name: session.name.clone(),
            state: SnapshotSessionState::from_session_state(session.state),
            authoritative_columns: session.authoritative_size.columns,
            authoritative_rows: session.authoritative_size.rows,
            active_window_id,
            shell: shell_metadata_from_session(session),
            active_config_layers: Vec::new(),
            frame_state: SnapshotFrameState::default(),
            agent_sessions: Vec::new(),
            approval_grants: Vec::new(),
            approval_requests: Vec::new(),
            message_state: None,
            mcp_servers: Vec::new(),
            window_groups,
            windows,
        }
    }
}
