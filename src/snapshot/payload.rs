//! Snapshot payload construction, validation, encoding, and resume planning.
//!
//! Payloads capture restorable session topology plus safe terminal history and
//! transcript references. They do not contain raw credentials or live processes.

use crate::error::{MezError, Result};
use mez_agent::messaging::{MessageService, MessageServiceSnapshot};
use mez_mux::layout::LayoutPolicy;
use mez_mux::process::PaneExitStatus;
use mez_mux::session::{Session, SessionState};
use mez_terminal::{
    GraphicRendition, TerminalColor, TerminalCursorState, TerminalModeState,
    TerminalSavedDecPrivateMode, TerminalSavedState, TerminalStyleSpan, tracked_dec_private_mode,
};

use super::encoding::{
    escape_field, non_empty_string, parse_bool, parse_u16, parse_u32, parse_u64, parse_usize,
    split_fields,
};
use super::types::{
    LayoutLoadPlan, PaneSnapshotPayload, SessionSnapshotPayload, SnapshotAgentSession,
    SnapshotApprovalGrantMetadata, SnapshotApprovalRequestMetadata, SnapshotConfigDiagnostic,
    SnapshotConfigLayerMetadata, SnapshotCreationContext, SnapshotFrameSettings,
    SnapshotFrameState, SnapshotLayoutNode, SnapshotMcpExternalCapability, SnapshotMcpServerState,
    SnapshotMcpToolEffects, SnapshotMcpToolState, SnapshotPaneCapture, SnapshotPaneGeometry,
    SnapshotSessionState, SnapshotShellMetadata, WindowGroupSnapshotPayload, WindowSnapshotPayload,
};

/// Defines the SNAPSHOT PAYLOAD FORMAT VERSION const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const SNAPSHOT_PAYLOAD_FORMAT_VERSION: u32 = 4;
/// Defines the MIN SUPPORTED SNAPSHOT PAYLOAD FORMAT VERSION const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const MIN_SUPPORTED_SNAPSHOT_PAYLOAD_FORMAT_VERSION: u32 = 2;

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

    /// Runs the contains terminal history operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn contains_terminal_history(&self) -> bool {
        self.windows.iter().any(|window| {
            window.panes.iter().any(|pane| {
                !pane.terminal_history.is_empty()
                    || (!pane.alternate_screen_active && !pane.visible_lines.is_empty())
            })
        })
    }

    /// Runs the contains agent transcripts operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn contains_agent_transcripts(&self) -> bool {
        self.windows.iter().any(|window| {
            window
                .panes
                .iter()
                .any(|pane| !pane.transcript_refs.is_empty())
        })
    }

    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn validate(&self) -> Result<()> {
        if self.session_id.is_empty() || self.name.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot payload session identity fields must not be empty",
            ));
        }
        if self.authoritative_columns == 0 || self.authoritative_rows == 0 {
            return Err(MezError::invalid_args(
                "snapshot payload authoritative size must be non-zero",
            ));
        }
        self.shell.validate()?;
        for layer in &self.active_config_layers {
            layer.validate()?;
        }
        self.frame_state.validate()?;
        for agent_session in &self.agent_sessions {
            agent_session.validate()?;
        }
        for grant in &self.approval_grants {
            grant.validate()?;
        }
        for request in &self.approval_requests {
            request.validate()?;
        }
        if let Some(message_state) = &self.message_state {
            validate_message_snapshot_state(message_state)?;
        }
        for server in &self.mcp_servers {
            server.validate()?;
        }
        for group in &self.window_groups {
            group.validate()?;
        }
        for window in &self.windows {
            window.validate()?;
        }
        validate_snapshot_window_groups(self)?;
        Ok(())
    }

    /// Runs the resume plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn resume_plan(&self) -> LayoutLoadPlan {
        let restart_required_panes = self
            .windows
            .iter()
            .flat_map(|window| window.panes.iter())
            .filter(|pane| pane.live_at_snapshot)
            .map(|pane| pane.pane_id.clone())
            .collect::<Vec<_>>();
        let pane_count = self
            .windows
            .iter()
            .map(|window| window.panes.len())
            .sum::<usize>();
        let running_agent_sessions = self
            .agent_sessions
            .iter()
            .filter(|session| session.running_turn_id.is_some())
            .map(|session| session.pane_id.clone())
            .collect::<Vec<_>>();
        let mut limitations = if restart_required_panes.is_empty() {
            Vec::new()
        } else {
            vec![
                "pane primary processes cannot be restored from snapshot and must be restarted"
                    .to_string(),
            ]
        };
        if !running_agent_sessions.is_empty() {
            limitations.push(
                "running agent turns are restored as interrupted and require explicit user confirmation before retrying non-idempotent actions"
                    .to_string(),
            );
        }
        if !self.mcp_servers.is_empty() {
            limitations.push(
                "MCP runtime transports are not restored from snapshot metadata and must be rediscovered"
                    .to_string(),
            );
        }

        LayoutLoadPlan {
            session_id: self.session_id.clone(),
            window_count: self.windows.len(),
            pane_count,
            restart_required_panes,
            limitations,
        }
    }

    /// Runs the encode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn encode(&self) -> Result<String> {
        let mut output = String::new();
        output.push_str(&format!(
            "payload_version\t{SNAPSHOT_PAYLOAD_FORMAT_VERSION}\n"
        ));
        output.push_str(&format!(
            "session\t{}\t{}\t{}\t{}\t{}\t{}\n",
            escape_field(&self.session_id),
            escape_field(&self.name),
            self.state.as_str(),
            self.authoritative_columns,
            self.authoritative_rows,
            escape_field(self.active_window_id.as_deref().unwrap_or(""))
        ));
        output.push_str(&format!(
            "shell\t{}\t{}\t{}\n",
            escape_field(&self.shell.path),
            escape_field(&self.shell.source),
            self.shell.used_fallback
        ));
        for layer in &self.active_config_layers {
            output.push_str(&format!(
                "config_layer\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                escape_field(&layer.id),
                escape_field(&layer.layer_type),
                layer.precedence,
                escape_field(layer.path.as_deref().unwrap_or("")),
                layer.trusted,
                layer.applied,
                layer.schema_version
            ));
            for diagnostic in &layer.diagnostics {
                output.push_str(&format!(
                    "config_diagnostic\t{}\t{}\t{}\n",
                    escape_field(&layer.id),
                    escape_field(&diagnostic.path),
                    escape_field(&diagnostic.message)
                ));
            }
        }
        encode_frame_settings("window", &self.frame_state.window, &mut output);
        encode_frame_settings("pane", &self.frame_state.pane, &mut output);
        for agent_session in &self.agent_sessions {
            output.push_str(&format!(
                "agent_session\t{}\t{}\t{}\t{}\t{}\n",
                escape_field(&agent_session.pane_id),
                escape_field(&agent_session.conversation_id),
                escape_field(&agent_session.visibility),
                escape_field(agent_session.running_turn_id.as_deref().unwrap_or("")),
                agent_session.transcript_entries
            ));
        }
        for grant in &self.approval_grants {
            output.push_str(&format!(
                "approval_grant\t{}\t{}\t{}\n",
                escape_field(&grant.id),
                escape_field(&grant.scope),
                escape_field(&grant.decision)
            ));
            for token in &grant.command_prefix {
                output.push_str(&format!(
                    "approval_grant_prefix\t{}\t{}\n",
                    escape_field(&grant.id),
                    escape_field(token)
                ));
            }
        }
        for request in &self.approval_requests {
            output.push_str(&format!(
                "approval_request\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                escape_field(&request.id),
                escape_field(&request.requesting_agent_id),
                escape_field(&request.pane_id),
                escape_field(&request.action_kind),
                escape_field(&request.action_summary),
                escape_field(&request.state),
                escape_field(request.decision.as_deref().unwrap_or("")),
                request
                    .created_at_unix_seconds
                    .map(|seconds| seconds.to_string())
                    .unwrap_or_default(),
                request
                    .decided_at_unix_seconds
                    .map(|seconds| seconds.to_string())
                    .unwrap_or_default(),
                escape_field(request.decided_by_client_id.as_deref().unwrap_or("")),
                escape_field(request.redirect_instruction.as_deref().unwrap_or(""))
            ));
            for agent_id in &request.parent_agent_chain {
                output.push_str(&format!(
                    "approval_request_parent\t{}\t{}\n",
                    escape_field(&request.id),
                    escape_field(agent_id)
                ));
            }
            for effect in &request.declared_effects {
                output.push_str(&format!(
                    "approval_request_effect\t{}\t{}\n",
                    escape_field(&request.id),
                    escape_field(effect)
                ));
            }
            for rule in &request.matched_rules {
                output.push_str(&format!(
                    "approval_request_rule\t{}\t{}\n",
                    escape_field(&request.id),
                    escape_field(rule)
                ));
            }
            for scope in &request.read_scopes {
                output.push_str(&format!(
                    "approval_request_read_scope\t{}\t{}\n",
                    escape_field(&request.id),
                    escape_field(scope)
                ));
            }
            for scope in &request.write_scopes {
                output.push_str(&format!(
                    "approval_request_write_scope\t{}\t{}\n",
                    escape_field(&request.id),
                    escape_field(scope)
                ));
            }
        }
        if let Some(message_state) = &self.message_state {
            let encoded = serde_json::to_string(message_state)
                .map_err(|_| MezError::invalid_state("snapshot MMP state could not be encoded"))?;
            output.push_str(&format!("message_state\t{}\n", escape_field(&encoded)));
        }
        if !self.mcp_servers.is_empty() {
            let encoded = serde_json::to_string(&self.mcp_servers)
                .map_err(|_| MezError::invalid_state("snapshot MCP state could not be encoded"))?;
            output.push_str(&format!("mcp_state\t{}\n", escape_field(&encoded)));
        }
        for group in &self.window_groups {
            output.push_str(&format!(
                "window_group\t{}\t{}\t{}\t{}\t{}\t{}\n",
                escape_field(&group.group_id),
                group.index,
                escape_field(&group.name),
                group.active,
                escape_field(group.active_window_id.as_deref().unwrap_or("")),
                escape_field(group.last_active_window_id.as_deref().unwrap_or(""))
            ));
            for window_id in &group.window_ids {
                output.push_str(&format!(
                    "window_group_window\t{}\t{}\n",
                    escape_field(&group.group_id),
                    escape_field(window_id)
                ));
            }
        }
        for window in &self.windows {
            output.push_str(&format!(
                "window\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                escape_field(&window.window_id),
                window.index,
                escape_field(&window.name),
                window.active,
                window.columns,
                window.rows,
                escape_field(&window.layout_policy)
            ));
            if let Some(layout_root) = &window.layout_root {
                let encoded = serde_json::to_string(layout_root).map_err(|_| {
                    MezError::invalid_state("snapshot layout tree could not be encoded")
                })?;
                output.push_str(&format!(
                    "window_layout\t{}\t{}\n",
                    escape_field(&window.window_id),
                    escape_field(&encoded)
                ));
            }
            for pane in &window.panes {
                output.push_str(&format!(
                    "pane\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                    escape_field(&pane.pane_id),
                    pane.index,
                    escape_field(&pane.title),
                    pane.active,
                    pane.live_at_snapshot,
                    pane.columns,
                    pane.rows
                ));
                output.push_str(&format!(
                    "pane_shell\t{}\t{}\t{}\t{}\t{}\n",
                    escape_field(&pane.pane_id),
                    optional_u32_field(pane.primary_pid),
                    escape_field(&pane.process_state),
                    escape_field(pane.current_working_directory.as_deref().unwrap_or("")),
                    escape_field(&pane.readiness_state)
                ));
                if let Some(exit_status) = pane.exit_status {
                    output.push_str(&format!(
                        "pane_exit_status\t{}\t{}\t{}\t{}\n",
                        escape_field(&pane.pane_id),
                        optional_i32_field(exit_status.code),
                        optional_i32_field(exit_status.signal),
                        exit_status.success
                    ));
                }
                if pane.alternate_screen_active {
                    output.push_str(&format!(
                        "pane_alternate_screen\t{}\ttrue\n",
                        escape_field(&pane.pane_id)
                    ));
                }
                if let Some(geometry) = &pane.geometry {
                    output.push_str(&format!(
                        "pane_geometry\t{}\t{}\t{}\t{}\t{}\n",
                        escape_field(&pane.pane_id),
                        geometry.column,
                        geometry.row,
                        geometry.columns,
                        geometry.rows
                    ));
                }
                encode_terminal_modes(&pane.pane_id, &pane.terminal_modes, &mut output);
                encode_terminal_saved_state(&pane.pane_id, &pane.terminal_saved_state, &mut output);
                for line in &pane.terminal_history {
                    output.push_str(&format!(
                        "pane_history\t{}\t{}\n",
                        escape_field(&pane.pane_id),
                        escape_field(line)
                    ));
                }
                for (line_index, style_spans) in
                    pane.terminal_history_line_style_spans.iter().enumerate()
                {
                    for span in style_spans {
                        output.push_str(&format!(
                            "pane_history_style\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                            escape_field(&pane.pane_id),
                            line_index,
                            span.start,
                            span.length,
                            span.rendition.bold,
                            span.rendition.dim,
                            span.rendition.italic,
                            span.rendition.underline,
                            span.rendition.double_underline,
                            span.rendition.strikethrough,
                            span.rendition.inverse,
                            span.rendition.hidden,
                            escape_field(&snapshot_terminal_color_name(span.rendition.foreground)),
                            escape_field(&snapshot_terminal_color_name(span.rendition.background))
                        ));
                    }
                }
                for line in &pane.visible_lines {
                    output.push_str(&format!(
                        "pane_visible\t{}\t{}\n",
                        escape_field(&pane.pane_id),
                        escape_field(line)
                    ));
                }
                for (line_index, style_spans) in pane.visible_line_style_spans.iter().enumerate() {
                    for span in style_spans {
                        output.push_str(&format!(
                            "pane_visible_style\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                            escape_field(&pane.pane_id),
                            line_index,
                            span.start,
                            span.length,
                            span.rendition.bold,
                            span.rendition.dim,
                            span.rendition.italic,
                            span.rendition.underline,
                            span.rendition.double_underline,
                            span.rendition.strikethrough,
                            span.rendition.inverse,
                            span.rendition.hidden,
                            escape_field(&snapshot_terminal_color_name(span.rendition.foreground)),
                            escape_field(&snapshot_terminal_color_name(span.rendition.background))
                        ));
                    }
                }
                for transcript_ref in &pane.transcript_refs {
                    output.push_str(&format!(
                        "pane_transcript\t{}\t{}\n",
                        escape_field(&pane.pane_id),
                        escape_field(transcript_ref)
                    ));
                }
            }
        }
        Ok(output)
    }

    /// Runs the decode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn decode(data: &str) -> Result<Self> {
        let mut lines = data.lines();
        let first_line = lines
            .next()
            .ok_or_else(|| MezError::invalid_args("snapshot payload is empty"))?;
        let first_fields = split_fields(first_line)?;
        let session_fields = if first_fields.first().map(String::as_str) == Some("payload_version")
        {
            if first_fields.len() != 2 {
                return Err(MezError::invalid_args(
                    "invalid snapshot payload version field count",
                ));
            }
            let version = parse_u32(&first_fields[1])?;
            if !(MIN_SUPPORTED_SNAPSHOT_PAYLOAD_FORMAT_VERSION..=SNAPSHOT_PAYLOAD_FORMAT_VERSION)
                .contains(&version)
            {
                return Err(MezError::invalid_args(
                    "unsupported snapshot payload format version",
                ));
            }
            let session_line = lines
                .next()
                .ok_or_else(|| MezError::invalid_args("snapshot payload session header missing"))?;
            split_fields(session_line)?
        } else {
            first_fields
        };
        if session_fields.len() != 7 || session_fields[0] != "session" {
            return Err(MezError::invalid_args(
                "invalid snapshot session payload header",
            ));
        }

        let mut payload = Self {
            session_id: session_fields[1].clone(),
            name: session_fields[2].clone(),
            state: SnapshotSessionState::parse(&session_fields[3])?,
            authoritative_columns: parse_u16(&session_fields[4])?,
            authoritative_rows: parse_u16(&session_fields[5])?,
            active_window_id: non_empty_string(&session_fields[6]),
            shell: SnapshotShellMetadata::default(),
            active_config_layers: Vec::new(),
            frame_state: SnapshotFrameState::default(),
            agent_sessions: Vec::new(),
            approval_grants: Vec::new(),
            approval_requests: Vec::new(),
            message_state: None,
            mcp_servers: Vec::new(),
            window_groups: Vec::new(),
            windows: Vec::new(),
        };

        for line in lines {
            let fields = split_fields(line)?;
            match fields.first().map(String::as_str) {
                Some("window_group") => {
                    if fields.len() != 7 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot window group field count",
                        ));
                    }
                    payload.window_groups.push(WindowGroupSnapshotPayload {
                        group_id: fields[1].clone(),
                        index: parse_usize(&fields[2])?,
                        name: fields[3].clone(),
                        window_ids: Vec::new(),
                        active: parse_bool(&fields[4])?,
                        active_window_id: non_empty_string(&fields[5]),
                        last_active_window_id: non_empty_string(&fields[6]),
                    });
                }
                Some("window_group_window") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot window group window field count",
                        ));
                    }
                    let group = payload_window_group_mut(&mut payload, &fields[1])?;
                    group.window_ids.push(fields[2].clone());
                }
                Some("window") => {
                    if fields.len() != 8 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot window payload field count",
                        ));
                    }
                    payload.windows.push(WindowSnapshotPayload {
                        window_id: fields[1].clone(),
                        index: parse_usize(&fields[2])?,
                        name: fields[3].clone(),
                        active: parse_bool(&fields[4])?,
                        columns: parse_u16(&fields[5])?,
                        rows: parse_u16(&fields[6])?,
                        layout_policy: fields[7].clone(),
                        layout_root: None,
                        panes: Vec::new(),
                    });
                }
                Some("window_layout") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot window layout field count",
                        ));
                    }
                    let window = payload_window_mut(&mut payload, &fields[1])?;
                    if window.layout_root.is_some() {
                        return Err(MezError::invalid_args(
                            "snapshot window layout appeared more than once",
                        ));
                    }
                    window.layout_root = Some(serde_json::from_str(&fields[2]).map_err(|_| {
                        MezError::invalid_args("invalid snapshot window layout JSON")
                    })?);
                }
                Some("shell") => {
                    if fields.len() != 4 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot shell payload field count",
                        ));
                    }
                    payload.shell = SnapshotShellMetadata {
                        path: fields[1].clone(),
                        source: fields[2].clone(),
                        used_fallback: parse_bool(&fields[3])?,
                    };
                }
                Some("config_layer") => {
                    if fields.len() != 8 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot config layer field count",
                        ));
                    }
                    payload
                        .active_config_layers
                        .push(SnapshotConfigLayerMetadata {
                            id: fields[1].clone(),
                            layer_type: fields[2].clone(),
                            precedence: parse_usize(&fields[3])?,
                            path: non_empty_string(&fields[4]),
                            trusted: parse_bool(&fields[5])?,
                            applied: parse_bool(&fields[6])?,
                            schema_version: parse_u32(&fields[7])?,
                            diagnostics: Vec::new(),
                        });
                }
                Some("config_diagnostic") => {
                    if fields.len() != 4 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot config diagnostic field count",
                        ));
                    }
                    payload_config_layer_mut(&mut payload, &fields[1])?
                        .diagnostics
                        .push(SnapshotConfigDiagnostic {
                            path: fields[2].clone(),
                            message: fields[3].clone(),
                        });
                }
                Some("frame") => {
                    if fields.len() != 6 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot frame payload field count",
                        ));
                    }
                    *payload_frame_settings_mut(&mut payload, &fields[1])? =
                        SnapshotFrameSettings {
                            enabled: parse_bool(&fields[2])?,
                            position: fields[3].clone(),
                            style: fields[4].clone(),
                            template: fields[5].clone(),
                            visible_fields: Vec::new(),
                        };
                }
                Some("frame_visible") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot frame visible-field count",
                        ));
                    }
                    payload_frame_settings_mut(&mut payload, &fields[1])?
                        .visible_fields
                        .push(fields[2].clone());
                }
                Some("agent_session") => {
                    if fields.len() != 6 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot agent session field count",
                        ));
                    }
                    payload.agent_sessions.push(SnapshotAgentSession {
                        pane_id: fields[1].clone(),
                        conversation_id: fields[2].clone(),
                        visibility: fields[3].clone(),
                        running_turn_id: non_empty_string(&fields[4]),
                        transcript_entries: parse_u64(&fields[5])?,
                    });
                }
                Some("approval_grant") => {
                    if fields.len() != 4 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot approval grant field count",
                        ));
                    }
                    payload.approval_grants.push(SnapshotApprovalGrantMetadata {
                        id: fields[1].clone(),
                        command_prefix: Vec::new(),
                        scope: fields[2].clone(),
                        decision: fields[3].clone(),
                    });
                }
                Some("approval_grant_prefix") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot approval grant prefix field count",
                        ));
                    }
                    payload_approval_grant_mut(&mut payload, &fields[1])?
                        .command_prefix
                        .push(fields[2].clone());
                }
                Some("approval_request") => {
                    if fields.len() != 12 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot approval request field count",
                        ));
                    }
                    payload
                        .approval_requests
                        .push(SnapshotApprovalRequestMetadata {
                            id: fields[1].clone(),
                            requesting_agent_id: fields[2].clone(),
                            pane_id: fields[3].clone(),
                            parent_agent_chain: Vec::new(),
                            action_kind: fields[4].clone(),
                            action_summary: fields[5].clone(),
                            declared_effects: Vec::new(),
                            matched_rules: Vec::new(),
                            read_scopes: Vec::new(),
                            write_scopes: Vec::new(),
                            created_at_unix_seconds: parse_optional_u64(&fields[8])?,
                            decided_at_unix_seconds: parse_optional_u64(&fields[9])?,
                            decided_by_client_id: non_empty_string(&fields[10]),
                            state: fields[6].clone(),
                            decision: non_empty_string(&fields[7]),
                            redirect_instruction: non_empty_string(&fields[11]),
                        });
                }
                Some("approval_request_parent") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot approval request parent field count",
                        ));
                    }
                    payload_approval_request_mut(&mut payload, &fields[1])?
                        .parent_agent_chain
                        .push(fields[2].clone());
                }
                Some("approval_request_effect") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot approval request effect field count",
                        ));
                    }
                    payload_approval_request_mut(&mut payload, &fields[1])?
                        .declared_effects
                        .push(fields[2].clone());
                }
                Some("approval_request_rule") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot approval request rule field count",
                        ));
                    }
                    payload_approval_request_mut(&mut payload, &fields[1])?
                        .matched_rules
                        .push(fields[2].clone());
                }
                Some("approval_request_read_scope") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot approval request read-scope field count",
                        ));
                    }
                    payload_approval_request_mut(&mut payload, &fields[1])?
                        .read_scopes
                        .push(fields[2].clone());
                }
                Some("approval_request_write_scope") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot approval request write-scope field count",
                        ));
                    }
                    payload_approval_request_mut(&mut payload, &fields[1])?
                        .write_scopes
                        .push(fields[2].clone());
                }
                Some("message_state") => {
                    if fields.len() != 2 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot message state field count",
                        ));
                    }
                    if payload.message_state.is_some() {
                        return Err(MezError::invalid_args(
                            "snapshot message state appeared more than once",
                        ));
                    }
                    payload.message_state =
                        Some(serde_json::from_str(&fields[1]).map_err(|_| {
                            MezError::invalid_args("invalid snapshot message state JSON")
                        })?);
                }
                Some("mcp_state") => {
                    if fields.len() != 2 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot MCP state field count",
                        ));
                    }
                    if !payload.mcp_servers.is_empty() {
                        return Err(MezError::invalid_args(
                            "snapshot MCP state appeared more than once",
                        ));
                    }
                    payload.mcp_servers = serde_json::from_str(&fields[1])
                        .map_err(|_| MezError::invalid_args("invalid snapshot MCP state JSON"))?;
                }
                Some("pane") => {
                    if fields.len() != 8 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane payload field count",
                        ));
                    }
                    let Some(window) = payload.windows.last_mut() else {
                        return Err(MezError::invalid_args(
                            "snapshot pane payload appeared before any window",
                        ));
                    };
                    let live_at_snapshot = parse_bool(&fields[5])?;
                    window.panes.push(PaneSnapshotPayload {
                        pane_id: fields[1].clone(),
                        index: parse_usize(&fields[2])?,
                        title: fields[3].clone(),
                        active: parse_bool(&fields[4])?,
                        live_at_snapshot,
                        columns: parse_u16(&fields[6])?,
                        rows: parse_u16(&fields[7])?,
                        primary_pid: None,
                        process_state: default_pane_process_state(live_at_snapshot).to_string(),
                        current_working_directory: None,
                        readiness_state: "unknown".to_string(),
                        exit_status: None,
                        geometry: None,
                        terminal_modes: TerminalModeState::default(),
                        terminal_saved_state: TerminalSavedState::default(),
                        terminal_history: Vec::new(),
                        terminal_history_line_style_spans: Vec::new(),
                        visible_lines: Vec::new(),
                        visible_line_style_spans: Vec::new(),
                        alternate_screen_active: false,
                        transcript_refs: Vec::new(),
                    });
                }
                Some("pane_shell") => {
                    if fields.len() != 6 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane shell metadata field count",
                        ));
                    }
                    let pane = payload_pane_mut(&mut payload, &fields[1])?;
                    pane.primary_pid = parse_optional_u32_field(&fields[2])?;
                    pane.process_state = fields[3].clone();
                    pane.current_working_directory = non_empty_string(&fields[4]);
                    pane.readiness_state = fields[5].clone();
                }
                Some("pane_alternate_screen") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane alternate-screen field count",
                        ));
                    }
                    payload_pane_mut(&mut payload, &fields[1])?.alternate_screen_active =
                        parse_bool(&fields[2])?;
                }
                Some("pane_exit_status") => {
                    if fields.len() != 5 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane exit-status field count",
                        ));
                    }
                    let pane = payload_pane_mut(&mut payload, &fields[1])?;
                    if pane.exit_status.is_some() {
                        return Err(MezError::invalid_args(
                            "snapshot pane exit status appeared more than once",
                        ));
                    }
                    pane.exit_status = Some(PaneExitStatus {
                        code: parse_optional_i32_field(&fields[2])?,
                        signal: parse_optional_i32_field(&fields[3])?,
                        success: parse_bool(&fields[4])?,
                    });
                }
                Some("pane_geometry") => {
                    if fields.len() != 6 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane geometry field count",
                        ));
                    }
                    let pane = payload_pane_mut(&mut payload, &fields[1])?;
                    if pane.geometry.is_some() {
                        return Err(MezError::invalid_args(
                            "snapshot pane geometry appeared more than once",
                        ));
                    }
                    pane.geometry = Some(SnapshotPaneGeometry {
                        column: parse_u16(&fields[2])?,
                        row: parse_u16(&fields[3])?,
                        columns: parse_u16(&fields[4])?,
                        rows: parse_u16(&fields[5])?,
                    });
                }
                Some("pane_terminal_modes") => {
                    if fields.len() != 9
                        && fields.len() != 10
                        && fields.len() != 11
                        && fields.len() != 12
                        && fields.len() != 13
                    {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane terminal modes field count",
                        ));
                    }
                    let (has_cursor_visible, has_independent_mouse_modes, has_autowrap_mode) =
                        match fields.len() {
                            9 => (false, false, false),
                            10 => (true, false, false),
                            11 => (false, true, false),
                            12 => (true, true, false),
                            13 => (true, true, true),
                            _ => unreachable!("validated pane terminal mode field count"),
                        };
                    let mode_offset = usize::from(has_cursor_visible);
                    let pane = payload_pane_mut(&mut payload, &fields[1])?;
                    let (
                        normal_mouse_tracking_enabled,
                        button_event_mouse_tracking_enabled,
                        any_event_mouse_tracking_enabled,
                        sgr_mode_index,
                    ) = if has_independent_mouse_modes {
                        (
                            parse_bool(&fields[3 + mode_offset])?,
                            parse_bool(&fields[4 + mode_offset])?,
                            parse_bool(&fields[5 + mode_offset])?,
                            6 + mode_offset,
                        )
                    } else {
                        let mouse_tracking_enabled = parse_bool(&fields[3 + mode_offset])?;
                        (
                            mouse_tracking_enabled,
                            mouse_tracking_enabled,
                            mouse_tracking_enabled,
                            4 + mode_offset,
                        )
                    };
                    pane.terminal_modes = TerminalModeState {
                        cursor_visible: if has_cursor_visible {
                            parse_bool(&fields[2])?
                        } else {
                            true
                        },
                        bracketed_paste_enabled: parse_bool(&fields[2 + mode_offset])?,
                        normal_mouse_tracking_enabled,
                        button_event_mouse_tracking_enabled,
                        any_event_mouse_tracking_enabled,
                        sgr_mouse_enabled: parse_bool(&fields[sgr_mode_index])?,
                        application_cursor_enabled: parse_bool(&fields[sgr_mode_index + 1])?,
                        origin_mode_enabled: false,
                        autowrap_enabled: if has_autowrap_mode {
                            parse_bool(&fields[sgr_mode_index + 2])?
                        } else {
                            true
                        },
                        application_keypad_enabled: parse_bool(
                            &fields[sgr_mode_index + 2 + usize::from(has_autowrap_mode)],
                        )?,
                        focus_events_enabled: parse_bool(
                            &fields[sgr_mode_index + 3 + usize::from(has_autowrap_mode)],
                        )?,
                        title: non_empty_string(
                            &fields[sgr_mode_index + 4 + usize::from(has_autowrap_mode)],
                        ),
                    };
                }
                Some("pane_terminal_saved_cursor") => {
                    if fields.len() != 4 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane saved cursor field count",
                        ));
                    }
                    let pane = payload_pane_mut(&mut payload, &fields[1])?;
                    if pane.terminal_saved_state.saved_cursor.is_some() {
                        return Err(MezError::invalid_args(
                            "snapshot pane saved cursor appeared more than once",
                        ));
                    }
                    pane.terminal_saved_state.saved_cursor = Some(TerminalCursorState {
                        row: parse_usize(&fields[2])?,
                        column: parse_usize(&fields[3])?,
                    });
                }
                Some("pane_terminal_saved_dec_private_mode") => {
                    if fields.len() != 4 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane saved DEC private mode field count",
                        ));
                    }
                    let pane = payload_pane_mut(&mut payload, &fields[1])?;
                    pane.terminal_saved_state.saved_dec_private_modes.push(
                        TerminalSavedDecPrivateMode {
                            mode: parse_u16(&fields[2])?,
                            enabled: parse_bool(&fields[3])?,
                        },
                    );
                }
                Some("pane_history") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane history field count",
                        ));
                    }
                    payload_pane_mut(&mut payload, &fields[1])?
                        .terminal_history
                        .push(fields[2].clone());
                }
                Some("pane_history_style") => {
                    if fields.len() != 15 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane history style field count",
                        ));
                    }
                    let line_index = parse_usize(&fields[2])?;
                    let pane = payload_pane_mut(&mut payload, &fields[1])?;
                    if line_index >= pane.terminal_history.len() {
                        return Err(MezError::invalid_args(
                            "snapshot pane history style references unknown history line",
                        ));
                    }
                    while pane.terminal_history_line_style_spans.len() <= line_index {
                        pane.terminal_history_line_style_spans.push(Vec::new());
                    }
                    pane.terminal_history_line_style_spans[line_index].push(TerminalStyleSpan {
                        start: parse_usize(&fields[3])?,
                        length: parse_usize(&fields[4])?,
                        rendition: GraphicRendition {
                            bold: parse_bool(&fields[5])?,
                            dim: parse_bool(&fields[6])?,
                            italic: parse_bool(&fields[7])?,
                            underline: parse_bool(&fields[8])?,
                            double_underline: parse_bool(&fields[9])?,
                            strikethrough: parse_bool(&fields[10])?,
                            inverse: parse_bool(&fields[11])?,
                            hidden: parse_bool(&fields[12])?,
                            foreground: parse_snapshot_terminal_color(&fields[13])?,
                            background: parse_snapshot_terminal_color(&fields[14])?,
                        },
                    });
                }
                Some("pane_visible") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane visible field count",
                        ));
                    }
                    payload_pane_mut(&mut payload, &fields[1])?
                        .visible_lines
                        .push(fields[2].clone());
                }
                Some("pane_visible_style") => {
                    if fields.len() != 15 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane visible style field count",
                        ));
                    }
                    let line_index = parse_usize(&fields[2])?;
                    let pane = payload_pane_mut(&mut payload, &fields[1])?;
                    if line_index >= pane.visible_lines.len() {
                        return Err(MezError::invalid_args(
                            "snapshot pane visible style references unknown visible line",
                        ));
                    }
                    while pane.visible_line_style_spans.len() <= line_index {
                        pane.visible_line_style_spans.push(Vec::new());
                    }
                    pane.visible_line_style_spans[line_index].push(TerminalStyleSpan {
                        start: parse_usize(&fields[3])?,
                        length: parse_usize(&fields[4])?,
                        rendition: GraphicRendition {
                            bold: parse_bool(&fields[5])?,
                            dim: parse_bool(&fields[6])?,
                            italic: parse_bool(&fields[7])?,
                            underline: parse_bool(&fields[8])?,
                            double_underline: parse_bool(&fields[9])?,
                            strikethrough: parse_bool(&fields[10])?,
                            inverse: parse_bool(&fields[11])?,
                            hidden: parse_bool(&fields[12])?,
                            foreground: parse_snapshot_terminal_color(&fields[13])?,
                            background: parse_snapshot_terminal_color(&fields[14])?,
                        },
                    });
                }
                Some("pane_transcript") => {
                    if fields.len() != 3 {
                        return Err(MezError::invalid_args(
                            "invalid snapshot pane transcript field count",
                        ));
                    }
                    payload_pane_mut(&mut payload, &fields[1])?
                        .transcript_refs
                        .push(fields[2].clone());
                }
                _ => return Err(MezError::invalid_args("unknown snapshot payload record")),
            }
        }

        normalize_payload_visible_line_style_spans(&mut payload);
        payload.validate()?;
        Ok(payload)
    }
}

/// Runs the payload config layer mut operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn payload_config_layer_mut<'a>(
    payload: &'a mut SessionSnapshotPayload,
    layer_id: &str,
) -> Result<&'a mut SnapshotConfigLayerMetadata> {
    payload
        .active_config_layers
        .iter_mut()
        .find(|layer| layer.id == layer_id)
        .ok_or_else(|| {
            MezError::invalid_args("snapshot config diagnostic references unknown layer")
        })
}

/// Runs the payload window mut operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn payload_window_mut<'a>(
    payload: &'a mut SessionSnapshotPayload,
    window_id: &str,
) -> Result<&'a mut WindowSnapshotPayload> {
    payload
        .windows
        .iter_mut()
        .find(|window| window.window_id == window_id)
        .ok_or_else(|| MezError::invalid_args("snapshot window layout references unknown window"))
}

/// Returns the mutable snapshot window group with the requested stable id.
fn payload_window_group_mut<'a>(
    payload: &'a mut SessionSnapshotPayload,
    group_id: &str,
) -> Result<&'a mut WindowGroupSnapshotPayload> {
    payload
        .window_groups
        .iter_mut()
        .find(|group| group.group_id == group_id)
        .ok_or_else(|| MezError::invalid_args("snapshot window group references unknown group"))
}

/// Runs the payload approval grant mut operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn payload_approval_grant_mut<'a>(
    payload: &'a mut SessionSnapshotPayload,
    grant_id: &str,
) -> Result<&'a mut SnapshotApprovalGrantMetadata> {
    payload
        .approval_grants
        .iter_mut()
        .find(|grant| grant.id == grant_id)
        .ok_or_else(|| {
            MezError::invalid_args("snapshot approval grant prefix references unknown grant")
        })
}

/// Runs the payload approval request mut operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn payload_approval_request_mut<'a>(
    payload: &'a mut SessionSnapshotPayload,
    request_id: &str,
) -> Result<&'a mut SnapshotApprovalRequestMetadata> {
    payload
        .approval_requests
        .iter_mut()
        .find(|request| request.id == request_id)
        .ok_or_else(|| {
            MezError::invalid_args("snapshot approval request extension references unknown request")
        })
}

/// Runs the shell metadata from session operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn shell_metadata_from_session(session: &Session) -> SnapshotShellMetadata {
    SnapshotShellMetadata {
        path: session.shell.path().display().to_string(),
        source: session.shell.source_name().to_string(),
        used_fallback: session.shell.used_fallback(),
    }
}

/// Runs the default pane process state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn default_pane_process_state(live_at_snapshot: bool) -> &'static str {
    if live_at_snapshot {
        "starting"
    } else {
        "exited"
    }
}

/// Runs the parse optional u64 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_optional_u64(value: &str) -> Result<Option<u64>> {
    if value.is_empty() {
        Ok(None)
    } else {
        parse_u64(value).map(Some)
    }
}

/// Runs the optional u32 field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_u32_field(value: Option<u32>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

/// Runs the parse optional u32 field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_optional_u32_field(value: &str) -> Result<Option<u32>> {
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse::<u32>()
            .map(Some)
            .map_err(|_| MezError::invalid_args("invalid integer in snapshot pane shell metadata"))
    }
}

/// Runs the optional i32 field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_i32_field(value: Option<i32>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

/// Runs the parse optional i32 field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_optional_i32_field(value: &str) -> Result<Option<i32>> {
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse::<i32>()
            .map(Some)
            .map_err(|_| MezError::invalid_args("invalid integer in snapshot pane exit status"))
    }
}

/// Runs the validate non empty collection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_non_empty_collection(values: &[String], message: &'static str) -> Result<()> {
    if values.iter().any(|value| value.trim().is_empty()) {
        return Err(MezError::invalid_args(message));
    }
    Ok(())
}

/// Runs the validate approval decision name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_approval_decision_name(value: &str) -> Result<()> {
    match value {
        "approve" | "disapprove" | "redirect" => Ok(()),
        _ => Err(MezError::invalid_args(
            "snapshot approval decision must be approve, disapprove, or redirect",
        )),
    }
}

/// Runs the validate message snapshot state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_message_snapshot_state(state: &MessageServiceSnapshot) -> Result<()> {
    MessageService::from_snapshot_state(state)
        .map(|_| ())
        .map_err(Into::into)
}

/// Runs the validate mcp name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_mcp_name(value: &str, message: &'static str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(MezError::invalid_args(message));
    }
    Ok(())
}

/// Runs the validate mcp json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_mcp_json(value: &str) -> Result<()> {
    serde_json::from_str::<serde_json::Value>(value)
        .map(|_| ())
        .map_err(|_| MezError::invalid_args("snapshot MCP input schema must be valid JSON"))
}

/// Runs the normalized line style spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn normalized_line_style_spans(
    source: &[Vec<TerminalStyleSpan>],
    line_count: usize,
) -> Vec<Vec<TerminalStyleSpan>> {
    let mut spans = source.iter().take(line_count).cloned().collect::<Vec<_>>();
    while spans.len() < line_count {
        spans.push(Vec::new());
    }
    spans
}

/// Runs the normalize payload visible line style spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn normalize_payload_visible_line_style_spans(payload: &mut SessionSnapshotPayload) {
    for pane in payload
        .windows
        .iter_mut()
        .flat_map(|window| window.panes.iter_mut())
    {
        pane.terminal_history_line_style_spans = normalized_line_style_spans(
            &pane.terminal_history_line_style_spans,
            pane.terminal_history.len(),
        );
        pane.visible_line_style_spans =
            normalized_line_style_spans(&pane.visible_line_style_spans, pane.visible_lines.len());
    }
}

/// Runs the snapshot terminal color name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn snapshot_terminal_color_name(color: Option<TerminalColor>) -> String {
    match color {
        Some(TerminalColor::Indexed(index)) => format!("indexed:{index}"),
        Some(TerminalColor::Rgb(red, green, blue)) => format!("rgb:{red},{green},{blue}"),
        None => String::new(),
    }
}

/// Runs the parse snapshot terminal color operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_snapshot_terminal_color(value: &str) -> Result<Option<TerminalColor>> {
    if value.is_empty() {
        return Ok(None);
    }
    if let Some(index) = value.strip_prefix("indexed:") {
        let index = parse_u8_component(index, "snapshot terminal indexed color is invalid")?;
        return Ok(Some(TerminalColor::Indexed(index)));
    }
    if let Some(rgb) = value.strip_prefix("rgb:") {
        let parts = rgb.split(',').collect::<Vec<_>>();
        if parts.len() != 3 {
            return Err(MezError::invalid_args(
                "snapshot terminal RGB color is invalid",
            ));
        }
        return Ok(Some(TerminalColor::Rgb(
            parse_u8_component(parts[0], "snapshot terminal RGB color is invalid")?,
            parse_u8_component(parts[1], "snapshot terminal RGB color is invalid")?,
            parse_u8_component(parts[2], "snapshot terminal RGB color is invalid")?,
        )));
    }
    Err(MezError::invalid_args(
        "snapshot terminal color must be empty, indexed, or rgb",
    ))
}

/// Runs the parse u8 component operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_u8_component(value: &str, message: &'static str) -> Result<u8> {
    let parsed = value
        .parse::<u16>()
        .map_err(|_| MezError::invalid_args(message))?;
    u8::try_from(parsed).map_err(|_| MezError::invalid_args(message))
}

/// Runs the payload frame settings mut operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn payload_frame_settings_mut<'a>(
    payload: &'a mut SessionSnapshotPayload,
    target: &str,
) -> Result<&'a mut SnapshotFrameSettings> {
    match target {
        "window" => Ok(&mut payload.frame_state.window),
        "pane" => Ok(&mut payload.frame_state.pane),
        _ => Err(MezError::invalid_args(
            "snapshot frame payload references unknown target",
        )),
    }
}

/// Runs the payload pane mut operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn payload_pane_mut<'a>(
    payload: &'a mut SessionSnapshotPayload,
    pane_id: &str,
) -> Result<&'a mut PaneSnapshotPayload> {
    payload
        .windows
        .iter_mut()
        .flat_map(|window| window.panes.iter_mut())
        .find(|pane| pane.pane_id == pane_id)
        .ok_or_else(|| MezError::invalid_args("snapshot pane extension references unknown pane"))
}

/// Runs the encode frame settings operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn encode_frame_settings(target: &str, settings: &SnapshotFrameSettings, output: &mut String) {
    output.push_str(&format!(
        "frame\t{}\t{}\t{}\t{}\t{}\n",
        target,
        settings.enabled,
        escape_field(&settings.position),
        escape_field(&settings.style),
        escape_field(&settings.template)
    ));
    for field in &settings.visible_fields {
        output.push_str(&format!(
            "frame_visible\t{}\t{}\n",
            target,
            escape_field(field)
        ));
    }
}

/// Runs the encode terminal modes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn encode_terminal_modes(pane_id: &str, modes: &TerminalModeState, output: &mut String) {
    output.push_str(&format!(
        "pane_terminal_modes\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
        escape_field(pane_id),
        modes.cursor_visible,
        modes.bracketed_paste_enabled,
        modes.normal_mouse_tracking_enabled,
        modes.button_event_mouse_tracking_enabled,
        modes.any_event_mouse_tracking_enabled,
        modes.sgr_mouse_enabled,
        modes.application_cursor_enabled,
        modes.autowrap_enabled,
        modes.application_keypad_enabled,
        modes.focus_events_enabled,
        escape_field(modes.title.as_deref().unwrap_or(""))
    ));
}

/// Runs the encode terminal saved state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn encode_terminal_saved_state(
    pane_id: &str,
    saved_state: &TerminalSavedState,
    output: &mut String,
) {
    if let Some(cursor) = saved_state.saved_cursor {
        output.push_str(&format!(
            "pane_terminal_saved_cursor\t{}\t{}\t{}\n",
            escape_field(pane_id),
            cursor.row,
            cursor.column
        ));
    }
    for saved_mode in &saved_state.saved_dec_private_modes {
        output.push_str(&format!(
            "pane_terminal_saved_dec_private_mode\t{}\t{}\t{}\n",
            escape_field(pane_id),
            saved_mode.mode,
            saved_mode.enabled
        ));
    }
}

impl SnapshotShellMetadata {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate(&self) -> Result<()> {
        if self.path.trim().is_empty() {
            return Err(MezError::invalid_args(
                "snapshot shell path must not be empty",
            ));
        }
        match self.source.as_str() {
            "shell-env" | "fallback-bin-sh" => {}
            _ => {
                return Err(MezError::invalid_args(
                    "snapshot shell source must be shell-env or fallback-bin-sh",
                ));
            }
        }
        if self.used_fallback != (self.source == "fallback-bin-sh") {
            return Err(MezError::invalid_args(
                "snapshot shell fallback flag must match shell source",
            ));
        }
        Ok(())
    }
}

impl SnapshotAgentSession {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate(&self) -> Result<()> {
        if self.pane_id.trim().is_empty() || self.conversation_id.trim().is_empty() {
            return Err(MezError::invalid_args(
                "snapshot agent session identity fields must not be empty",
            ));
        }
        match self.visibility.as_str() {
            "hidden" | "visible" | "hide-pending-task-completion" => {}
            _ => {
                return Err(MezError::invalid_args(
                    "snapshot agent session visibility is invalid",
                ));
            }
        }
        if self
            .running_turn_id
            .as_ref()
            .is_some_and(|turn_id| turn_id.trim().is_empty())
        {
            return Err(MezError::invalid_args(
                "snapshot agent session running turn id must not be empty",
            ));
        }
        if self.visibility == "hide-pending-task-completion" && self.running_turn_id.is_none() {
            return Err(MezError::invalid_args(
                "snapshot pending-hide agent sessions must include a running turn",
            ));
        }
        Ok(())
    }
}

impl SnapshotApprovalGrantMetadata {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            return Err(MezError::invalid_args(
                "snapshot approval grant id must not be empty",
            ));
        }
        if self.command_prefix.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot approval grant command prefix must not be empty",
            ));
        }
        validate_non_empty_collection(
            &self.command_prefix,
            "snapshot approval grant command prefix tokens must not be empty",
        )?;
        match self.scope.as_str() {
            "session" | "global" => {}
            _ => {
                return Err(MezError::invalid_args(
                    "snapshot approval grant scope must be session or global",
                ));
            }
        }
        validate_approval_decision_name(&self.decision)
    }
}

impl SnapshotApprovalRequestMetadata {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty()
            || self.requesting_agent_id.trim().is_empty()
            || self.pane_id.trim().is_empty()
            || self.action_kind.trim().is_empty()
            || self.action_summary.trim().is_empty()
        {
            return Err(MezError::invalid_args(
                "snapshot approval request identity fields must not be empty",
            ));
        }
        validate_non_empty_collection(
            &self.parent_agent_chain,
            "snapshot approval request parent chain entries must not be empty",
        )?;
        validate_non_empty_collection(
            &self.declared_effects,
            "snapshot approval request declared effects must not be empty",
        )?;
        validate_non_empty_collection(
            &self.matched_rules,
            "snapshot approval request matched rules must not be empty",
        )?;
        validate_non_empty_collection(
            &self.read_scopes,
            "snapshot approval request read scopes must not be empty",
        )?;
        validate_non_empty_collection(
            &self.write_scopes,
            "snapshot approval request write scopes must not be empty",
        )?;
        let expected_decision = match self.state.as_str() {
            "approved" => "approve",
            "disapproved" => "disapprove",
            "redirected" => "redirect",
            _ => {
                return Err(MezError::invalid_args(
                    "snapshot approval request state must be approved, disapproved, or redirected",
                ));
            }
        };
        let Some(decision) = self.decision.as_deref() else {
            return Err(MezError::invalid_args(
                "snapshot decided approval request must include a decision",
            ));
        };
        validate_approval_decision_name(decision)?;
        if decision != expected_decision {
            return Err(MezError::invalid_args(
                "snapshot approval request decision must match its state",
            ));
        }
        if self
            .decided_by_client_id
            .as_ref()
            .is_some_and(|client_id| client_id.trim().is_empty())
        {
            return Err(MezError::invalid_args(
                "snapshot approval request deciding client id must not be empty",
            ));
        }
        match self.redirect_instruction.as_deref() {
            Some(instruction) if instruction.trim().is_empty() => Err(MezError::invalid_args(
                "snapshot approval request redirect instruction must not be empty",
            )),
            Some(_) if decision != "redirect" => Err(MezError::invalid_args(
                "snapshot approval request redirect instruction requires redirect decision",
            )),
            None if decision == "redirect" => Err(MezError::invalid_args(
                "snapshot redirected approval request must include redirect instruction",
            )),
            _ => Ok(()),
        }
    }
}

impl SnapshotMcpServerState {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate(&self) -> Result<()> {
        validate_mcp_name(&self.id, "snapshot MCP server id must not be empty")?;
        validate_mcp_name(&self.name, "snapshot MCP server name must not be empty")?;
        match self.kind.as_str() {
            "stdio" | "streamable_http" => {}
            _ => {
                return Err(MezError::invalid_args(
                    "snapshot MCP server kind must be stdio or streamable_http",
                ));
            }
        }
        match self.status.as_str() {
            "configured" | "starting" | "available" | "unavailable" | "blacklisted" | "failed" => {}
            _ => {
                return Err(MezError::invalid_args(
                    "snapshot MCP server status is invalid",
                ));
            }
        }
        if self
            .blacklist_reason
            .as_ref()
            .is_some_and(|reason| reason.trim().is_empty())
        {
            return Err(MezError::invalid_args(
                "snapshot MCP blacklist reason must not be empty",
            ));
        }
        self.external_capability.validate()?;
        for tool in &self.tools {
            tool.validate(&self.id)?;
        }
        Ok(())
    }
}

impl SnapshotMcpExternalCapability {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate(&self) -> Result<()> {
        let requires_purpose = self.mutates_filesystem_outside_shell
            || self.executes_processes_outside_shell
            || self.accesses_credentials_outside_shell;
        if requires_purpose && self.purpose.trim().is_empty() {
            return Err(MezError::invalid_args(
                "snapshot MCP external capability purpose must not be empty",
            ));
        }
        Ok(())
    }
}

impl SnapshotMcpToolState {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate(&self, server_id: &str) -> Result<()> {
        validate_mcp_name(
            &self.server_id,
            "snapshot MCP tool server id must not be empty",
        )?;
        validate_mcp_name(&self.name, "snapshot MCP tool name must not be empty")?;
        if self.server_id != server_id {
            return Err(MezError::invalid_args(
                "snapshot MCP tool server id must match containing server",
            ));
        }
        match self.approval.as_str() {
            "inherit" | "prompt" | "allow" | "deny" => {}
            _ => {
                return Err(MezError::invalid_args(
                    "snapshot MCP tool approval is invalid",
                ));
            }
        }
        self.effects.validate()?;
        validate_mcp_json(&self.input_schema_json)
    }
}

impl SnapshotMcpToolEffects {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate(&self) -> Result<()> {
        let _ = self;
        Ok(())
    }
}

impl SnapshotFrameState {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate(&self) -> Result<()> {
        self.window.validate("window")?;
        self.pane.validate("pane")
    }
}

impl SnapshotFrameSettings {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate(&self, target: &str) -> Result<()> {
        match self.position.as_str() {
            "top" | "bottom" | "border" => {}
            _ => {
                return Err(MezError::invalid_args(format!(
                    "snapshot {target} frame position must be top, bottom, or border"
                )));
            }
        }
        match self.style.as_str() {
            "default" | "bold" | "underline" | "inverse" | "reverse" => {}
            _ => {
                return Err(MezError::invalid_args(format!(
                    "snapshot {target} frame style must be default, bold, underline, inverse, or reverse"
                )));
            }
        }
        if self.template.trim().is_empty() {
            return Err(MezError::invalid_args(format!(
                "snapshot {target} frame template must not be empty"
            )));
        }
        if self.visible_fields.is_empty()
            || self
                .visible_fields
                .iter()
                .any(|field| field.trim().is_empty())
        {
            return Err(MezError::invalid_args(format!(
                "snapshot {target} frame visible fields must not be empty"
            )));
        }
        Ok(())
    }
}

impl WindowGroupSnapshotPayload {
    /// Validates saved window-group topology before session reconstruction.
    fn validate(&self) -> Result<()> {
        if self.group_id.is_empty() || self.name.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot window group identity fields must not be empty",
            ));
        }
        if self.window_ids.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot window group must contain at least one window",
            ));
        }
        if self.window_ids.iter().any(|window_id| window_id.is_empty()) {
            return Err(MezError::invalid_args(
                "snapshot window group window ids must not be empty",
            ));
        }
        Ok(())
    }
}

/// Validates that saved groups reference saved windows and identify one active group.
fn validate_snapshot_window_groups(payload: &SessionSnapshotPayload) -> Result<()> {
    if payload.window_groups.is_empty() {
        return Ok(());
    }
    let active_count = payload
        .window_groups
        .iter()
        .filter(|group| group.active)
        .count();
    if active_count != 1 {
        return Err(MezError::invalid_args(
            "snapshot payload must contain exactly one active window group",
        ));
    }
    for group in &payload.window_groups {
        for window_id in &group.window_ids {
            if !payload
                .windows
                .iter()
                .any(|window| window.window_id == *window_id)
            {
                return Err(MezError::invalid_args(
                    "snapshot window group references an unknown window",
                ));
            }
        }
        for active_window_id in [&group.active_window_id, &group.last_active_window_id]
            .into_iter()
            .flatten()
        {
            if !group
                .window_ids
                .iter()
                .any(|window_id| window_id == active_window_id)
            {
                return Err(MezError::invalid_args(
                    "snapshot window group active window references an unknown group window",
                ));
            }
        }
    }
    Ok(())
}

impl WindowSnapshotPayload {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate(&self) -> Result<()> {
        if self.window_id.is_empty() || self.name.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot window identity fields must not be empty",
            ));
        }
        if self.columns == 0 || self.rows == 0 {
            return Err(MezError::invalid_args(
                "snapshot window size must be non-zero",
            ));
        }
        if LayoutPolicy::from_name(&self.layout_policy).is_none() {
            return Err(MezError::invalid_args(
                "snapshot window layout policy is invalid",
            ));
        }
        if self.panes.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot window must contain at least one pane",
            ));
        }
        for pane in &self.panes {
            pane.validate()?;
        }
        if let Some(layout_root) = &self.layout_root {
            let layout_root = layout_root.to_layout_node(&self.panes)?;
            layout_root.validate_pane_indices(self.panes.len())?;
        }
        validate_window_pane_geometries(self)?;
        Ok(())
    }
}

impl SnapshotConfigLayerMetadata {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate(&self) -> Result<()> {
        if self.id.is_empty() || self.layer_type.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot config layer identity fields must not be empty",
            ));
        }
        if self.schema_version == 0 {
            return Err(MezError::invalid_args(
                "snapshot config layer schema version must be non-zero",
            ));
        }
        for diagnostic in &self.diagnostics {
            diagnostic.validate()?;
        }
        Ok(())
    }
}

impl SnapshotConfigDiagnostic {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate(&self) -> Result<()> {
        if self.path.is_empty() || self.message.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot config diagnostics must not be empty",
            ));
        }
        Ok(())
    }
}

impl PaneSnapshotPayload {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate(&self) -> Result<()> {
        if self.pane_id.is_empty() || self.title.is_empty() {
            return Err(MezError::invalid_args(
                "snapshot pane identity fields must not be empty",
            ));
        }
        if self.columns == 0 || self.rows == 0 {
            return Err(MezError::invalid_args(
                "snapshot pane size must be non-zero",
            ));
        }
        validate_snapshot_pane_process_state(&self.process_state)?;
        validate_snapshot_pane_readiness_state(&self.readiness_state)?;
        if self
            .current_working_directory
            .as_deref()
            .is_some_and(|directory| directory.trim().is_empty())
        {
            return Err(MezError::invalid_args(
                "snapshot pane current working directory must not be empty",
            ));
        }
        if let Some(geometry) = &self.geometry {
            geometry.validate(self.columns, self.rows)?;
        }
        validate_terminal_saved_state(
            &self.terminal_saved_state,
            usize::from(self.rows),
            usize::from(self.columns),
        )?;
        if self.terminal_history_line_style_spans.len() > self.terminal_history.len() {
            return Err(MezError::invalid_args(
                "snapshot pane history style spans must align to history lines",
            ));
        }
        for spans in &self.terminal_history_line_style_spans {
            validate_terminal_style_spans(spans, usize::from(self.columns))?;
        }
        if self.visible_line_style_spans.len() > self.visible_lines.len() {
            return Err(MezError::invalid_args(
                "snapshot pane visible style spans must align to visible lines",
            ));
        }
        for spans in &self.visible_line_style_spans {
            validate_terminal_style_spans(spans, usize::from(self.columns))?;
        }
        if self
            .transcript_refs
            .iter()
            .any(|transcript_ref| transcript_ref.trim().is_empty())
        {
            return Err(MezError::invalid_args(
                "snapshot pane transcript references must not be empty",
            ));
        }
        Ok(())
    }
}

/// Runs the validate snapshot pane process state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_snapshot_pane_process_state(process_state: &str) -> Result<()> {
    match process_state {
        "starting" | "running" | "exited" | "closing" | "failed" => Ok(()),
        _ => Err(MezError::invalid_args(
            "snapshot pane process state is invalid",
        )),
    }
}

/// Runs the validate snapshot pane readiness state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_snapshot_pane_readiness_state(readiness_state: &str) -> Result<()> {
    match readiness_state {
        "unknown"
        | "prompt-candidate"
        | "probing"
        | "ready"
        | "busy"
        | "degraded"
        | "interactive-blocked" => Ok(()),
        _ => Err(MezError::invalid_args(
            "snapshot pane readiness state is invalid",
        )),
    }
}

impl SnapshotPaneGeometry {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn validate(&self, pane_columns: u16, pane_rows: u16) -> Result<()> {
        if self.columns == 0 || self.rows == 0 {
            return Err(MezError::invalid_args(
                "snapshot pane geometry size must be non-zero",
            ));
        }
        if self.columns != pane_columns || self.rows != pane_rows {
            return Err(MezError::invalid_args(
                "snapshot pane geometry size must match pane size",
            ));
        }
        Ok(())
    }
}

/// Runs the validate window pane geometries operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_window_pane_geometries(window: &WindowSnapshotPayload) -> Result<()> {
    let mut geometries = Vec::new();
    for pane in &window.panes {
        let Some(geometry) = pane.geometry.as_ref() else {
            return Ok(());
        };
        if geometry.column.saturating_add(geometry.columns) > window.columns
            || geometry.row.saturating_add(geometry.rows) > window.rows
        {
            return Err(MezError::invalid_args(
                "snapshot pane geometry must fit inside the window",
            ));
        }
        geometries.push(geometry);
    }

    for (left_index, left) in geometries.iter().enumerate() {
        for right in geometries.iter().skip(left_index.saturating_add(1)) {
            if snapshot_pane_geometries_overlap(left, right) {
                return Err(MezError::invalid_args(
                    "snapshot pane geometries must not overlap",
                ));
            }
        }
    }
    Ok(())
}

/// Runs the snapshot pane geometries overlap operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn snapshot_pane_geometries_overlap(
    left: &SnapshotPaneGeometry,
    right: &SnapshotPaneGeometry,
) -> bool {
    let left_right = left.column.saturating_add(left.columns);
    let left_bottom = left.row.saturating_add(left.rows);
    let right_right = right.column.saturating_add(right.columns);
    let right_bottom = right.row.saturating_add(right.rows);
    left.column < right_right
        && right.column < left_right
        && left.row < right_bottom
        && right.row < left_bottom
}

/// Runs the validate terminal style spans operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_terminal_style_spans(spans: &[TerminalStyleSpan], columns: usize) -> Result<()> {
    let mut previous_end = 0usize;
    for span in spans {
        if span.length == 0 {
            return Err(MezError::invalid_args(
                "snapshot terminal style span length must be non-zero",
            ));
        }
        if span.rendition == GraphicRendition::default() {
            return Err(MezError::invalid_args(
                "snapshot terminal style span rendition must be non-default",
            ));
        }
        let end = span
            .start
            .checked_add(span.length)
            .ok_or_else(|| MezError::invalid_args("snapshot terminal style span is too large"))?;
        if span.start < previous_end {
            return Err(MezError::invalid_args(
                "snapshot terminal style spans must be sorted and non-overlapping",
            ));
        }
        if end > columns {
            return Err(MezError::invalid_args(
                "snapshot terminal style span exceeds pane width",
            ));
        }
        previous_end = end;
    }
    Ok(())
}

/// Runs the validate terminal saved state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_terminal_saved_state(
    saved_state: &TerminalSavedState,
    rows: usize,
    columns: usize,
) -> Result<()> {
    if let Some(cursor) = saved_state.saved_cursor
        && (cursor.row >= rows || cursor.column >= columns)
    {
        return Err(MezError::invalid_args(
            "snapshot pane saved cursor must fit pane size",
        ));
    }
    let mut seen_modes = Vec::new();
    for saved_mode in &saved_state.saved_dec_private_modes {
        if !tracked_dec_private_mode(saved_mode.mode) {
            return Err(MezError::invalid_args(
                "snapshot pane saved DEC private mode is not tracked",
            ));
        }
        if seen_modes.contains(&saved_mode.mode) {
            return Err(MezError::invalid_args(
                "snapshot pane saved DEC private modes must not repeat",
            ));
        }
        seen_modes.push(saved_mode.mode);
    }
    Ok(())
}

impl SnapshotSessionState {
    /// Runs the from session state operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn from_session_state(state: SessionState) -> Self {
        match state {
            SessionState::Running => Self::Running,
            SessionState::Detached => Self::Detached,
            SessionState::Empty => Self::Empty,
            SessionState::Stopping => Self::Stopping,
            SessionState::Failed => Self::Failed,
        }
    }

    /// Runs the as str operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Detached => "detached",
            Self::Empty => "empty",
            Self::Stopping => "stopping",
            Self::Failed => "failed",
        }
    }

    /// Runs the parse operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn parse(value: &str) -> Result<Self> {
        match value {
            "running" => Ok(Self::Running),
            "detached" => Ok(Self::Detached),
            "empty" => Ok(Self::Empty),
            "stopping" => Ok(Self::Stopping),
            "failed" => Ok(Self::Failed),
            _ => Err(MezError::invalid_args("unknown snapshot session state")),
        }
    }
}
