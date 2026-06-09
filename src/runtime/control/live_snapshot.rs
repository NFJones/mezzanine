//! Runtime control live snapshot capture helpers.
//!
//! This module owns the conversion from live runtime session state into the
//! durable snapshot payload and metadata records consumed by the snapshot
//! repository. Keeping live capture assembly separate from request dispatch
//! keeps the main control adapter focused on protocol method routing.

use super::super::{
    BlockedApprovalState, ConfigScope, MessageServiceSnapshot, Result, RuntimeSessionService,
    SnapshotAgentSession, SnapshotApprovalGrantMetadata, SnapshotApprovalRequestMetadata,
    SnapshotConfigDiagnostic, SnapshotConfigLayerMetadata, SnapshotCreationContext,
    SnapshotFrameSettings, SnapshotFrameState, SnapshotMcpServerState, SnapshotPaneCapture,
    SnapshotRepository, SnapshotState, validate_config_text,
};
use super::snapshot::{
    runtime_snapshot_agent_visibility_name, runtime_snapshot_approval_grant,
    runtime_snapshot_approval_request, runtime_snapshot_config_scope_name,
    runtime_snapshot_frame_position_name, runtime_snapshot_frame_style_name,
    runtime_snapshot_mcp_server_state,
};

impl RuntimeSessionService {
    /// Captures live pane terminal state and process metadata for snapshots.
    pub fn live_snapshot_pane_captures(&self) -> Vec<SnapshotPaneCapture> {
        self.session
            .windows()
            .iter()
            .flat_map(|window| window.panes().iter())
            .map(|pane| {
                let pane_id = pane.id.as_str();
                let screen = self.pane_screens.get(pane_id);
                let history_styled_lines = screen
                    .map(|screen| screen.history().styled_lines().collect::<Vec<_>>())
                    .unwrap_or_default();
                let visible_styled_lines = screen
                    .map(|screen| {
                        if screen.alternate_screen_active() {
                            Vec::new()
                        } else {
                            screen.visible_styled_lines()
                        }
                    })
                    .unwrap_or_default();
                let primary_pid = self.primary_pid_for_live_pane_process(pane_id);
                let process_state = if primary_pid.is_some() {
                    "running"
                } else if pane.live {
                    "starting"
                } else {
                    "exited"
                };
                SnapshotPaneCapture {
                    pane_id: pane_id.to_string(),
                    primary_pid,
                    process_state: Some(process_state.to_string()),
                    current_working_directory: self
                        .pane_current_working_directory(pane_id)
                        .map(|path| path.to_string_lossy().to_string()),
                    readiness_state: Some(
                        super::super::runtime_pane_readiness_state_name(
                            self.pane_readiness_state(pane_id),
                        )
                        .to_string(),
                    ),
                    terminal_history: history_styled_lines
                        .iter()
                        .map(|line| line.text.clone())
                        .collect(),
                    terminal_history_line_style_spans: history_styled_lines
                        .into_iter()
                        .map(|line| line.style_spans)
                        .collect(),
                    visible_lines: visible_styled_lines
                        .iter()
                        .map(|line| line.text.clone())
                        .collect(),
                    visible_line_style_spans: visible_styled_lines
                        .into_iter()
                        .map(|line| line.style_spans)
                        .collect(),
                    terminal_modes: screen.map(|screen| screen.mode_state()).unwrap_or_default(),
                    terminal_saved_state: screen
                        .map(|screen| screen.saved_state())
                        .unwrap_or_default(),
                    exit_status: self
                        .pane_exit_records
                        .get(pane_id)
                        .map(|record| record.exit_status),
                    alternate_screen_active: screen
                        .is_some_and(|screen| screen.alternate_screen_active()),
                    transcript_refs: self.snapshot_transcript_refs_for_pane(pane_id),
                }
            })
            .collect()
    }

    /// Returns durable transcript references for one live pane snapshot.
    fn snapshot_transcript_refs_for_pane(&self, pane_id: &str) -> Vec<String> {
        let mut refs = self
            .pane_transcript_refs
            .get(pane_id)
            .cloned()
            .unwrap_or_default();
        if let Some(session) = self.agent_shell_store.get(pane_id) {
            let transcript_ref = format!("transcript:{pane_id}:{}", session.session_id);
            if !refs.iter().any(|existing| existing == &transcript_ref) {
                refs.push(transcript_ref);
            }
        }
        refs
    }

    /// Creates a durable snapshot from the current live runtime state.
    pub fn create_live_snapshot(
        &self,
        snapshots: &SnapshotRepository,
        snapshot_id: &str,
        name: Option<String>,
    ) -> Result<SnapshotState> {
        let active_config_layers = self.live_snapshot_config_layers();
        let pane_captures = self.live_snapshot_pane_captures();
        let frame_state = self.live_snapshot_frame_state();
        let agent_sessions = self.live_snapshot_agent_sessions();
        let approval_grants = self.live_snapshot_approval_grants();
        let approval_requests = self.live_snapshot_approval_requests();
        let message_state = self.live_snapshot_message_state();
        let mcp_servers = self.live_snapshot_mcp_servers();
        snapshots.create_from_session_with_context(
            snapshot_id,
            name,
            &self.session,
            SnapshotCreationContext::new(
                &pane_captures,
                &active_config_layers,
                &frame_state,
                &agent_sessions,
            )
            .with_approvals(&approval_grants, &approval_requests)
            .with_message_state(&message_state)
            .with_mcp_servers(&mcp_servers),
        )
    }

    /// Captures agent-shell conversation metadata for a live snapshot.
    pub(super) fn live_snapshot_agent_sessions(&self) -> Vec<SnapshotAgentSession> {
        self.agent_shell_store
            .sessions()
            .map(|session| SnapshotAgentSession {
                pane_id: session.pane_id.clone(),
                conversation_id: session.session_id.clone(),
                visibility: runtime_snapshot_agent_visibility_name(session.visibility).to_string(),
                running_turn_id: session.running_turn_id.clone(),
                transcript_entries: session.transcript_entries,
            })
            .collect()
    }

    /// Captures non-pending approval grants for a live snapshot.
    pub(super) fn live_snapshot_approval_grants(&self) -> Vec<SnapshotApprovalGrantMetadata> {
        self.session_approvals
            .grants()
            .map(runtime_snapshot_approval_grant)
            .collect()
    }

    /// Captures decided approval requests for a live snapshot.
    pub(super) fn live_snapshot_approval_requests(&self) -> Vec<SnapshotApprovalRequestMetadata> {
        self.blocked_approvals
            .requests()
            .filter(|request| request.state != BlockedApprovalState::Pending)
            .map(runtime_snapshot_approval_request)
            .collect()
    }

    /// Captures runtime message-service state for a live snapshot.
    pub(super) fn live_snapshot_message_state(&self) -> MessageServiceSnapshot {
        self.message_service.snapshot_state()
    }

    /// Captures MCP server state for a live snapshot.
    pub(super) fn live_snapshot_mcp_servers(&self) -> Vec<SnapshotMcpServerState> {
        self.mcp_registry
            .list_servers()
            .iter()
            .map(|server| runtime_snapshot_mcp_server_state(server))
            .collect()
    }

    /// Captures active frame settings for a live snapshot.
    pub(super) fn live_snapshot_frame_state(&self) -> SnapshotFrameState {
        SnapshotFrameState {
            window: SnapshotFrameSettings {
                enabled: self.window_frames_enabled,
                position: runtime_snapshot_frame_position_name(self.window_frame_position)
                    .to_string(),
                style: runtime_snapshot_frame_style_name(self.window_frame_style).to_string(),
                template: self.window_frame_template.clone(),
                visible_fields: self.window_frame_visible_fields.clone(),
            },
            pane: SnapshotFrameSettings {
                enabled: self.pane_frames_enabled,
                position: runtime_snapshot_frame_position_name(self.pane_frame_position)
                    .to_string(),
                style: runtime_snapshot_frame_style_name(self.pane_frame_style).to_string(),
                template: self.pane_frame_template.clone(),
                visible_fields: self.pane_frame_visible_fields.clone(),
            },
        }
    }

    /// Captures active config layer metadata for a live snapshot.
    pub(super) fn live_snapshot_config_layers(&self) -> Vec<SnapshotConfigLayerMetadata> {
        self.config_layers
            .iter()
            .enumerate()
            .map(|(precedence, layer)| {
                let validation = validate_config_text(layer.format, &layer.text, layer.scope);
                let applied = validation.valid
                    && (layer.scope != ConfigScope::ProjectOverlay || layer.trusted);
                SnapshotConfigLayerMetadata {
                    id: layer.name.clone(),
                    layer_type: runtime_snapshot_config_scope_name(layer.scope).to_string(),
                    precedence,
                    path: layer.path.as_ref().map(|path| path.display().to_string()),
                    trusted: layer.trusted,
                    applied,
                    schema_version: 1,
                    diagnostics: validation
                        .diagnostics
                        .into_iter()
                        .map(|diagnostic| SnapshotConfigDiagnostic {
                            path: diagnostic.path,
                            message: diagnostic.message,
                        })
                        .collect(),
                }
            })
            .collect()
    }
}
