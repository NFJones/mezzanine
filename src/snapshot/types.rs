//! Snapshot state, manifest, payload, and restore-plan data types.
//!
//! These types model persisted snapshot metadata and payload contents without
//! performing filesystem I/O or serialization.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{MezError, Result};
use crate::layout::{LayoutNode, Pane, SplitDirection};
use crate::message::MessageServiceSnapshot;
use crate::process::PaneExitStatus;
use crate::session::Session;
use crate::terminal::{
    DEFAULT_PANE_FRAME_TEMPLATE, DEFAULT_WINDOW_FRAME_TEMPLATE, TerminalModeState,
    TerminalSavedState, TerminalStyleSpan,
};

/// Carries Snapshot Kind state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotKind {
    /// Represents the Live case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Live,
    /// Represents the Manual case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Manual,
    /// Represents the Automatic case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Automatic,
    /// Represents the Crash Recovery case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    CrashRecovery,
}

/// Carries Snapshot State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotState {
    /// Stores the id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: String,
    /// Stores the version value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub version: u32,
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_id: String,
    /// Stores the name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub name: Option<String>,
    /// Stores the created at value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub created_at: String,
    /// Stores the kind value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub kind: SnapshotKind,
    /// Stores the restorable value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub restorable: bool,
    /// Stores the window count value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_count: usize,
    /// Stores the pane count value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_count: usize,
    /// Stores the limitations value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub limitations: Vec<String>,
    /// Stores the storage ref value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub storage_ref: String,
}

/// Carries Snapshot Manifest state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotManifest {
    /// Stores the state value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub state: SnapshotState,
    /// Stores the contains terminal history value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub contains_terminal_history: bool,
    /// Stores the contains agent transcripts value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub contains_agent_transcripts: bool,
    /// Stores the contains raw credentials value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub contains_raw_credentials: bool,
    /// Stores the active approvals restored value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub active_approvals_restored: bool,
    /// Stores the restart required panes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub restart_required_panes: Vec<String>,
}

/// Carries Snapshot Repository state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRepository {
    /// Stores the root value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) root: PathBuf,
}

/// Carries Session Snapshot Payload state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSnapshotPayload {
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_id: String,
    /// Stores the name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub name: String,
    /// Stores the state value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub state: SnapshotSessionState,
    /// Stores the authoritative columns value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub authoritative_columns: u16,
    /// Stores the authoritative rows value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub authoritative_rows: u16,
    /// Stores the active window id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub active_window_id: Option<String>,
    /// Stores the shell value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub shell: SnapshotShellMetadata,
    /// Stores the active config layers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub active_config_layers: Vec<SnapshotConfigLayerMetadata>,
    /// Stores the frame state value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub frame_state: SnapshotFrameState,
    /// Stores the agent sessions value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_sessions: Vec<SnapshotAgentSession>,
    /// Stores the approval grants value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub approval_grants: Vec<SnapshotApprovalGrantMetadata>,
    /// Stores the approval requests value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub approval_requests: Vec<SnapshotApprovalRequestMetadata>,
    /// Stores the message state value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub message_state: Option<MessageServiceSnapshot>,
    /// Stores the mcp servers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mcp_servers: Vec<SnapshotMcpServerState>,
    /// Stores the windows value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub windows: Vec<WindowSnapshotPayload>,
}

/// Session approval grant metadata captured for snapshot audit history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotApprovalGrantMetadata {
    /// Stable approval grant identifier.
    pub id: String,
    /// Tokenized command prefix covered by the grant.
    pub command_prefix: Vec<String>,
    /// Approval scope name, such as session or global.
    pub scope: String,
    /// Grant decision name, such as approve, disapprove, or redirect.
    pub decision: String,
}

/// Decided blocked approval request metadata captured for snapshot audit history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotApprovalRequestMetadata {
    /// Stable blocked approval request identifier.
    pub id: String,
    /// Agent that requested approval.
    pub requesting_agent_id: String,
    /// Pane associated with the requested action.
    pub pane_id: String,
    /// Parent agent chain active when approval was requested.
    pub parent_agent_chain: Vec<String>,
    /// Action category, such as shell_command.
    pub action_kind: String,
    /// Human-readable action summary.
    pub action_summary: String,
    /// Declared effect labels for the requested action.
    pub declared_effects: Vec<String>,
    /// Permission rules matched by the request.
    pub matched_rules: Vec<String>,
    /// Read scopes active for the requested action.
    pub read_scopes: Vec<String>,
    /// Write scopes active for the requested action.
    pub write_scopes: Vec<String>,
    /// Request creation time in Unix seconds, when known.
    pub created_at_unix_seconds: Option<u64>,
    /// Decision time in Unix seconds, when known.
    pub decided_at_unix_seconds: Option<u64>,
    /// Client that decided the request, when known.
    pub decided_by_client_id: Option<String>,
    /// Decided request state.
    pub state: String,
    /// Decision name, when the request was decided.
    pub decision: Option<String>,
    /// Redirect instruction for redirect decisions.
    pub redirect_instruction: Option<String>,
}

/// Sanitized MCP server state captured with a session snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotMcpServerState {
    /// Stable MCP server identifier.
    pub id: String,
    /// User-visible MCP server name.
    pub name: String,
    /// Transport category without transport endpoint details.
    pub kind: String,
    /// Whether this server was enabled in effective configuration.
    pub enabled: bool,
    /// Runtime availability state at snapshot time.
    pub status: String,
    /// Last status-check time in Unix seconds, when known.
    pub last_checked_at_unix_seconds: Option<u64>,
    /// Blacklist or unavailable reason, when known.
    pub blacklist_reason: Option<String>,
    /// Declared external capability metadata.
    pub external_capability: SnapshotMcpExternalCapability,
    /// Tool states known for this server.
    pub tools: Vec<SnapshotMcpToolState>,
}

/// Sanitized MCP external capability metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotMcpExternalCapability {
    /// Whether the server can mutate files outside the pane shell.
    pub mutates_filesystem_outside_shell: bool,
    /// Whether the server can execute processes outside the pane shell.
    pub executes_processes_outside_shell: bool,
    /// Whether the server can access credentials outside the pane shell.
    pub accesses_credentials_outside_shell: bool,
    /// Human-readable purpose for external capabilities.
    pub purpose: String,
}

/// Sanitized MCP tool state captured with a session snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotMcpToolState {
    /// MCP server that owns this tool.
    pub server_id: String,
    /// Tool name advertised by the MCP server.
    pub name: String,
    /// Whether the tool was available to agents at snapshot time.
    pub available: bool,
    /// Whether the tool was session-blacklisted.
    pub blacklisted: bool,
    /// Whether policy requires approval for this tool.
    pub permission_required: bool,
    /// Declared tool effects.
    pub effects: SnapshotMcpToolEffects,
    /// Effective approval policy for this tool.
    pub approval: String,
    /// Tool description from discovery.
    pub description: String,
    /// Tool input schema JSON.
    pub input_schema_json: String,
}

/// Sanitized MCP tool effect flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotMcpToolEffects {
    /// Stores the reads filesystem value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub reads_filesystem: bool,
    /// Stores the mutates filesystem value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mutates_filesystem: bool,
    /// Stores the executes processes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub executes_processes: bool,
    /// Stores the accesses credentials value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub accesses_credentials: bool,
    /// Stores the uses network value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub uses_network: bool,
    /// Stores the has side effects value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub has_side_effects: bool,
}

/// Agent shell session metadata captured with a session snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotAgentSession {
    /// Pane that owns this agent shell session.
    pub pane_id: String,
    /// Durable conversation or agent-shell session identifier.
    pub conversation_id: String,
    /// Agent shell visibility at snapshot time.
    pub visibility: String,
    /// Running turn identifier if an agent turn was active at snapshot time.
    pub running_turn_id: Option<String>,
    /// Number of transcript entries known to the session metadata.
    pub transcript_entries: u64,
}

/// Resolved shell metadata captured with a session snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotShellMetadata {
    /// Absolute shell path used by the session runtime.
    pub path: String,
    /// Shell resolution source, such as shell-env or fallback-bin-sh.
    pub source: String,
    /// Whether shell resolution fell back to `/bin/sh`.
    pub used_fallback: bool,
}

impl Default for SnapshotShellMetadata {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            path: "/bin/sh".to_string(),
            source: "fallback-bin-sh".to_string(),
            used_fallback: true,
        }
    }
}

/// Window and pane frame settings captured with a session snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotFrameState {
    /// Window frame settings active at snapshot time.
    pub window: SnapshotFrameSettings,
    /// Pane frame settings active at snapshot time.
    pub pane: SnapshotFrameSettings,
}

impl Default for SnapshotFrameState {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            window: SnapshotFrameSettings {
                enabled: false,
                position: "top".to_string(),
                style: "default".to_string(),
                template: DEFAULT_WINDOW_FRAME_TEMPLATE.to_string(),
                visible_fields: crate::terminal::DEFAULT_WINDOW_FRAME_VISIBLE_FIELDS
                    .iter()
                    .map(|field| (*field).to_string())
                    .collect(),
            },
            pane: SnapshotFrameSettings {
                enabled: false,
                position: "top".to_string(),
                style: "default".to_string(),
                template: DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
                visible_fields: crate::terminal::DEFAULT_PANE_FRAME_VISIBLE_FIELDS
                    .iter()
                    .map(|field| (*field).to_string())
                    .collect(),
            },
        }
    }
}

/// Frame renderer settings for one frame target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotFrameSettings {
    /// Whether this frame target was enabled.
    pub enabled: bool,
    /// Frame placement, such as top, bottom, or border.
    pub position: String,
    /// Frame visual style, such as default, bold, underline, or inverse.
    pub style: String,
    /// Named-field frame template.
    pub template: String,
    /// Field names used when constructing a template from visible fields.
    pub visible_fields: Vec<String>,
}

/// Configuration provenance captured with a session snapshot.
///
/// This metadata records which configuration layers were known to the runtime
/// at snapshot time without persisting raw configuration text or secret values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotConfigLayerMetadata {
    /// Stable layer identifier from the runtime configuration stack.
    pub id: String,
    /// Layer category, such as built-in, primary, project overlay, or runtime.
    pub layer_type: String,
    /// Zero-based precedence order in the active configuration stack.
    pub precedence: usize,
    /// Source file path when the layer came from disk.
    pub path: Option<String>,
    /// Whether the layer passed its applicable trust gate.
    pub trusted: bool,
    /// Whether the layer contributed to the effective configuration.
    pub applied: bool,
    /// Configuration schema version understood for this layer.
    pub schema_version: u32,
    /// Validation diagnostics observed for the layer at snapshot time.
    pub diagnostics: Vec<SnapshotConfigDiagnostic>,
}

/// One validation diagnostic attached to snapshot configuration metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotConfigDiagnostic {
    /// Configuration path or source location associated with the diagnostic.
    pub path: String,
    /// Human-readable diagnostic message.
    pub message: String,
}

/// Carries Window Snapshot Payload state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSnapshotPayload {
    /// Stores the window id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_id: String,
    /// Stores the index value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub index: usize,
    /// Stores the name value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub name: String,
    /// Stores the active value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub active: bool,
    /// Stores the columns value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub columns: u16,
    /// Stores the rows value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub rows: u16,
    /// Active layout policy captured for this window.
    pub layout_policy: String,
    /// Recursive split tree captured for this window, when available.
    pub layout_root: Option<SnapshotLayoutNode>,
    /// Stores the panes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub panes: Vec<PaneSnapshotPayload>,
}

/// Recursive layout tree captured by a snapshot payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SnapshotLayoutNode {
    /// A pane leaf identified by stable pane id.
    #[serde(rename = "pane")]
    Pane {
        /// Pane id occupying this leaf at snapshot time.
        pane_id: String,
    },
    /// A split node containing child layout nodes in visual order.
    #[serde(rename = "split")]
    Split {
        /// Split direction, either `vertical` or `horizontal`.
        direction: String,
        /// Child layout nodes.
        children: Vec<SnapshotLayoutNode>,
        /// Allocation sizes for children along the split axis.
        sizes: Vec<u16>,
    },
}

impl SnapshotLayoutNode {
    /// Builds a snapshot layout tree from an in-memory layout tree.
    pub fn from_layout_node(node: &LayoutNode, panes: &[Pane]) -> Result<Self> {
        match node {
            LayoutNode::Pane { index } => {
                let pane = panes.get(*index).ok_or_else(|| {
                    MezError::invalid_state("layout tree references an unknown pane")
                })?;
                Ok(Self::Pane {
                    pane_id: pane.id.to_string(),
                })
            }
            LayoutNode::Split {
                direction,
                children,
            } => Ok(Self::Split {
                direction: direction.name().to_string(),
                children: children
                    .iter()
                    .map(|child| Self::from_layout_node(child, panes))
                    .collect::<Result<Vec<_>>>()?,
                sizes: children
                    .iter()
                    .map(|child| child.allocation_on_axis(panes, *direction))
                    .collect(),
            }),
        }
    }

    /// Converts a snapshot layout tree back into in-memory pane-slot indices.
    pub fn to_layout_node(&self, panes: &[PaneSnapshotPayload]) -> Result<LayoutNode> {
        match self {
            Self::Pane { pane_id } => {
                let index = panes
                    .iter()
                    .position(|pane| pane.pane_id == *pane_id)
                    .ok_or_else(|| {
                        MezError::invalid_args("snapshot layout references an unknown pane")
                    })?;
                Ok(LayoutNode::Pane { index })
            }
            Self::Split {
                direction,
                children,
                sizes,
            } => {
                let direction = SplitDirection::from_name(direction).ok_or_else(|| {
                    MezError::invalid_args("snapshot layout split direction is invalid")
                })?;
                if children.len() < 2 || children.len() != sizes.len() {
                    return Err(MezError::invalid_args(
                        "snapshot layout split children and sizes must align",
                    ));
                }
                if sizes.contains(&0) {
                    return Err(MezError::invalid_args(
                        "snapshot layout split sizes must be non-zero",
                    ));
                }
                Ok(LayoutNode::Split {
                    direction,
                    children: children
                        .iter()
                        .map(|child| child.to_layout_node(panes))
                        .collect::<Result<Vec<_>>>()?,
                })
            }
        }
    }
}

/// Carries Pane Snapshot Payload state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSnapshotPayload {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the index value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub index: usize,
    /// Stores the title value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub title: String,
    /// Stores the active value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub active: bool,
    /// Stores the live at snapshot value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub live_at_snapshot: bool,
    /// Stores the columns value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub columns: u16,
    /// Stores the rows value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub rows: u16,
    /// Primary PID observed at snapshot time, retained as audit metadata only.
    pub primary_pid: Option<u32>,
    /// Pane primary process state observed at snapshot time.
    pub process_state: String,
    /// Pane shell current working directory observed at snapshot time.
    pub current_working_directory: Option<String>,
    /// Agent-harness readiness state for this pane at snapshot time.
    pub readiness_state: String,
    /// Primary process exit status known at snapshot time.
    pub exit_status: Option<PaneExitStatus>,
    /// Stored pane rectangle at snapshot time, when available.
    pub geometry: Option<SnapshotPaneGeometry>,
    /// Terminal title and mode flags tracked at snapshot time.
    pub terminal_modes: TerminalModeState,
    /// Saved terminal parser state used by later save/restore sequences.
    pub terminal_saved_state: TerminalSavedState,
    /// Stores the terminal history value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub terminal_history: Vec<String>,
    /// Non-default SGR style spans aligned to `terminal_history`.
    pub terminal_history_line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Stores the visible lines value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub visible_lines: Vec<String>,
    /// Non-default SGR style spans aligned to `visible_lines`.
    pub visible_line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Stores the alternate screen active value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub alternate_screen_active: bool,
    /// Stores the transcript refs value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub transcript_refs: Vec<String>,
}

/// Pane rectangle metadata captured with a snapshot payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotPaneGeometry {
    /// Zero-based column where the pane starts.
    pub column: u16,
    /// Zero-based row where the pane starts.
    pub row: u16,
    /// Pane width in terminal cells.
    pub columns: u16,
    /// Pane height in terminal cells.
    pub rows: u16,
}

/// Carries Snapshot Pane Capture state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotPaneCapture {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Primary PID observed at snapshot time, retained as audit metadata only.
    pub primary_pid: Option<u32>,
    /// Pane primary process state observed at snapshot time.
    pub process_state: Option<String>,
    /// Pane shell current working directory observed at snapshot time.
    pub current_working_directory: Option<String>,
    /// Agent-harness readiness state for this pane at snapshot time.
    pub readiness_state: Option<String>,
    /// Stores the terminal history value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub terminal_history: Vec<String>,
    /// Non-default SGR style spans aligned to `terminal_history`.
    pub terminal_history_line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Stores the visible lines value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub visible_lines: Vec<String>,
    /// Non-default SGR style spans aligned to `visible_lines`.
    pub visible_line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Terminal title and mode flags tracked at snapshot time.
    pub terminal_modes: TerminalModeState,
    /// Saved terminal parser state used by later save/restore sequences.
    pub terminal_saved_state: TerminalSavedState,
    /// Primary process exit status known at snapshot time.
    pub exit_status: Option<PaneExitStatus>,
    /// Stores the alternate screen active value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub alternate_screen_active: bool,
    /// Stores the transcript refs value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub transcript_refs: Vec<String>,
}

/// Extra live runtime state supplied when creating a snapshot payload.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotCreationContext<'a> {
    /// Terminal and transcript capture state by pane.
    pub pane_captures: &'a [SnapshotPaneCapture],
    /// Active configuration layer metadata.
    pub active_config_layers: &'a [SnapshotConfigLayerMetadata],
    /// Window and pane frame state.
    pub frame_state: &'a SnapshotFrameState,
    /// Agent shell session metadata.
    pub agent_sessions: &'a [SnapshotAgentSession],
    /// Approval grant metadata retained for audit history.
    pub approval_grants: &'a [SnapshotApprovalGrantMetadata],
    /// Decided approval request metadata retained for audit history.
    pub approval_requests: &'a [SnapshotApprovalRequestMetadata],
    /// Local message protocol state.
    pub message_state: Option<&'a MessageServiceSnapshot>,
    /// Sanitized MCP server state.
    pub mcp_servers: &'a [SnapshotMcpServerState],
}

impl<'a> SnapshotCreationContext<'a> {
    /// Creates a snapshot creation context from already-borrowed live state.
    pub fn new(
        pane_captures: &'a [SnapshotPaneCapture],
        active_config_layers: &'a [SnapshotConfigLayerMetadata],
        frame_state: &'a SnapshotFrameState,
        agent_sessions: &'a [SnapshotAgentSession],
    ) -> Self {
        Self {
            pane_captures,
            active_config_layers,
            frame_state,
            agent_sessions,
            approval_grants: &[],
            approval_requests: &[],
            message_state: None,
            mcp_servers: &[],
        }
    }

    /// Adds approval audit metadata to the snapshot creation context.
    pub fn with_approvals(
        mut self,
        approval_grants: &'a [SnapshotApprovalGrantMetadata],
        approval_requests: &'a [SnapshotApprovalRequestMetadata],
    ) -> Self {
        self.approval_grants = approval_grants;
        self.approval_requests = approval_requests;
        self
    }

    /// Adds local message protocol state to the snapshot creation context.
    pub fn with_message_state(mut self, message_state: &'a MessageServiceSnapshot) -> Self {
        self.message_state = Some(message_state);
        self
    }

    /// Adds sanitized MCP server state to the snapshot creation context.
    pub fn with_mcp_servers(mut self, mcp_servers: &'a [SnapshotMcpServerState]) -> Self {
        self.mcp_servers = mcp_servers;
        self
    }
}

/// Carries Snapshot Session State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotSessionState {
    /// Represents the Running case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Running,
    /// Represents the Detached case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Detached,
    /// Represents the Empty case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Empty,
    /// Represents the Stopping case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Stopping,
    /// Represents the Failed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Failed,
}

/// Carries Snapshot Resume Plan state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotResumePlan {
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_id: String,
    /// Stores the window count value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_count: usize,
    /// Stores the pane count value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_count: usize,
    /// Stores the restart required panes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub restart_required_panes: Vec<String>,
    /// Stores the limitations value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub limitations: Vec<String>,
}

/// Carries Snapshot Rollback Plan state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRollbackPlan {
    /// Stores the snapshot id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub snapshot_id: String,
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_id: String,
    /// Stores the available value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub available: bool,
    /// Stores the restore command value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub restore_command: Option<String>,
    /// Stores the restart required panes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub restart_required_panes: Vec<String>,
    /// Stores the limitations value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub limitations: Vec<String>,
}

/// Carries Snapshot Restore Result state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct SnapshotRestoreResult {
    /// Stores the session value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session: Session,
    /// Stores the resume plan value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub resume_plan: SnapshotResumePlan,
}
