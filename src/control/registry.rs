//! Control method registry.
//!
//! The control endpoint has two responsibilities that must stay aligned:
//! dispatching each JSON-RPC method to its owner and rejecting unknown
//! parameters before handler-specific parsing. This module is the shared source
//! of method metadata so new methods do not require matching edits in unrelated
//! string switches.

/// Describes how a registered control method validates its params object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ControlParamsSchema {
    /// Handler-specific validation owns the params shape.
    Unchecked,
    /// The params object may contain only the listed top-level keys.
    Allowed(&'static [&'static str]),
    /// The config subsystem owns specialized validation for this method.
    Config,
}

/// Identifies the parsed dispatch path for a registered control method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ControlDispatchKind {
    /// Initialize a control connection.
    ControlInitialize,
    /// Shut down a control connection.
    ControlShutdown,
    /// Cancel a pending request.
    ControlCancel,
    /// Attach to a session.
    SessionAttach,
    /// List sessions.
    SessionList,
    /// Inspect the active session.
    SessionGet,
    /// Rename the active session.
    SessionRename,
    /// Kill the active session.
    SessionKill,
    /// List windows.
    WindowList,
    /// Create a window.
    WindowCreate,
    /// Rename a window.
    WindowRename,
    /// Select a window.
    WindowSelect,
    /// Close a window.
    WindowClose,
    /// List panes.
    PaneList,
    /// Create a pane.
    PaneCreate,
    /// Select a pane.
    PaneSelect,
    /// Resize a pane.
    PaneResize,
    /// Swap panes.
    PaneSwap,
    /// Break a pane into a new window.
    PaneBreak,
    /// Join or move a pane.
    PaneJoinMove,
    /// Close a pane.
    PaneClose,
    /// Capture pane output.
    PaneCapture,
    /// Read a rendered frame.
    FrameRead,
    /// Render a terminal view.
    TerminalView,
    /// Step terminal input.
    TerminalStep,
    /// Execute a terminal command.
    TerminalCommand,
    /// List clients.
    ClientList,
    /// Detach a client.
    ClientDetach,
    /// Select the primary client.
    ClientSelectPrimary,
    /// List observer requests.
    ObserverList,
    /// Inspect an observer request.
    ObserverInspect,
    /// Approve an observer request.
    ObserverApprove,
    /// Reject an observer request.
    ObserverReject,
    /// Revoke observer status.
    ObserverRevoke,
    /// List agents.
    AgentList,
    /// List agent tasks.
    AgentTaskList,
    /// Spawn an agent.
    AgentSpawn,
    /// Show or hide the pane agent shell.
    AgentShellVisibility { visible: bool },
    /// Submit text to the pane agent shell.
    AgentShellCommand,
    /// List events.
    EventList,
    /// List approvals.
    ApprovalList,
    /// Decide an approval.
    ApprovalDecide,
    /// List snapshots.
    SnapshotList,
    /// Create a snapshot.
    SnapshotCreate,
    /// Resume a snapshot.
    SnapshotResume,
    /// Delete a snapshot.
    SnapshotDelete,
    /// List project trust entries.
    ProjectTrustList,
    /// Inspect project trust state.
    ProjectTrustInspect,
    /// Decide project trust state.
    ProjectTrustDecide,
    /// Revoke project trust state.
    ProjectTrustRevoke,
    /// List MCP servers and tools.
    McpList,
    /// Retry an MCP server.
    McpRetry,
    /// Delegate to the configuration dispatcher.
    Config,
}

/// Carries the registry entry for one control method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ControlMethodSpec {
    /// JSON-RPC method name.
    pub(super) method: &'static str,
    /// Parsed dispatch path for this method.
    pub(super) dispatch: ControlDispatchKind,
    /// Top-level params schema for early unknown-field rejection.
    pub(super) params_schema: ControlParamsSchema,
}

const NO_PARAMS: &[&str] = &[];
const TARGET_PARAMS: &[&str] = &["target"];
const CLIENT_IDEMPOTENCY_PARAMS: &[&str] = &["client_id", "idempotency_key"];
const WINDOW_TARGET_MUTATION_PARAMS: &[&str] = &[
    "target",
    "window_id",
    "window_name",
    "window_index",
    "name",
    "force",
    "idempotency_key",
];
const PANE_TARGET_MUTATION_PARAMS: &[&str] = &[
    "target",
    "pane_id",
    "pane_title",
    "pane_index",
    "size",
    "name",
    "force",
    "range",
    "include_history",
    "idempotency_key",
];
const PANE_TRANSFER_PARAMS: &[&str] = &[
    "source",
    "source_pane_id",
    "target",
    "pane_id",
    "pane_title",
    "pane_index",
    "destination",
    "destination_pane_id",
    "destination_window_id",
    "position",
    "idempotency_key",
];
const FRAME_TARGET_PARAMS: &[&str] = &[
    "target",
    "window_id",
    "window_name",
    "window_index",
    "pane_id",
    "pane_title",
    "pane_index",
];

/// Registered control method metadata.
pub(super) const CONTROL_METHOD_REGISTRY: &[ControlMethodSpec] = &[
    ControlMethodSpec {
        method: "control/initialize",
        dispatch: ControlDispatchKind::ControlInitialize,
        params_schema: ControlParamsSchema::Unchecked,
    },
    ControlMethodSpec {
        method: "control/shutdown",
        dispatch: ControlDispatchKind::ControlShutdown,
        params_schema: ControlParamsSchema::Allowed(NO_PARAMS),
    },
    ControlMethodSpec {
        method: "control/cancel",
        dispatch: ControlDispatchKind::ControlCancel,
        params_schema: ControlParamsSchema::Allowed(&["request_id"]),
    },
    ControlMethodSpec {
        method: "session/attach",
        dispatch: ControlDispatchKind::SessionAttach,
        params_schema: ControlParamsSchema::Allowed(&[
            "target",
            "role",
            "client",
            "idempotency_key",
        ]),
    },
    ControlMethodSpec {
        method: "session/list",
        dispatch: ControlDispatchKind::SessionList,
        params_schema: ControlParamsSchema::Allowed(NO_PARAMS),
    },
    ControlMethodSpec {
        method: "session/get",
        dispatch: ControlDispatchKind::SessionGet,
        params_schema: ControlParamsSchema::Allowed(TARGET_PARAMS),
    },
    ControlMethodSpec {
        method: "session/rename",
        dispatch: ControlDispatchKind::SessionRename,
        params_schema: ControlParamsSchema::Allowed(&["name", "idempotency_key"]),
    },
    ControlMethodSpec {
        method: "session/kill",
        dispatch: ControlDispatchKind::SessionKill,
        params_schema: ControlParamsSchema::Allowed(&["force", "idempotency_key"]),
    },
    ControlMethodSpec {
        method: "client/list",
        dispatch: ControlDispatchKind::ClientList,
        params_schema: ControlParamsSchema::Allowed(TARGET_PARAMS),
    },
    ControlMethodSpec {
        method: "client/detach",
        dispatch: ControlDispatchKind::ClientDetach,
        params_schema: ControlParamsSchema::Allowed(CLIENT_IDEMPOTENCY_PARAMS),
    },
    ControlMethodSpec {
        method: "client/select_primary",
        dispatch: ControlDispatchKind::ClientSelectPrimary,
        params_schema: ControlParamsSchema::Allowed(CLIENT_IDEMPOTENCY_PARAMS),
    },
    ControlMethodSpec {
        method: "observer/list",
        dispatch: ControlDispatchKind::ObserverList,
        params_schema: ControlParamsSchema::Allowed(&["target", "state"]),
    },
    ControlMethodSpec {
        method: "observer/inspect",
        dispatch: ControlDispatchKind::ObserverInspect,
        params_schema: ControlParamsSchema::Allowed(&["observer_request_id"]),
    },
    ControlMethodSpec {
        method: "observer/approve",
        dispatch: ControlDispatchKind::ObserverApprove,
        params_schema: ControlParamsSchema::Allowed(&["observer_request_id", "idempotency_key"]),
    },
    ControlMethodSpec {
        method: "observer/reject",
        dispatch: ControlDispatchKind::ObserverReject,
        params_schema: ControlParamsSchema::Allowed(&[
            "observer_request_id",
            "reason",
            "idempotency_key",
        ]),
    },
    ControlMethodSpec {
        method: "observer/revoke",
        dispatch: ControlDispatchKind::ObserverRevoke,
        params_schema: ControlParamsSchema::Allowed(&["client_id", "reason", "idempotency_key"]),
    },
    ControlMethodSpec {
        method: "window/list",
        dispatch: ControlDispatchKind::WindowList,
        params_schema: ControlParamsSchema::Allowed(TARGET_PARAMS),
    },
    ControlMethodSpec {
        method: "window/create",
        dispatch: ControlDispatchKind::WindowCreate,
        params_schema: ControlParamsSchema::Allowed(&[
            "target",
            "name",
            "start_directory",
            "shell_command",
            "select",
            "idempotency_key",
        ]),
    },
    ControlMethodSpec {
        method: "window/rename",
        dispatch: ControlDispatchKind::WindowRename,
        params_schema: ControlParamsSchema::Allowed(WINDOW_TARGET_MUTATION_PARAMS),
    },
    ControlMethodSpec {
        method: "window/select",
        dispatch: ControlDispatchKind::WindowSelect,
        params_schema: ControlParamsSchema::Allowed(WINDOW_TARGET_MUTATION_PARAMS),
    },
    ControlMethodSpec {
        method: "window/close",
        dispatch: ControlDispatchKind::WindowClose,
        params_schema: ControlParamsSchema::Allowed(WINDOW_TARGET_MUTATION_PARAMS),
    },
    ControlMethodSpec {
        method: "pane/list",
        dispatch: ControlDispatchKind::PaneList,
        params_schema: ControlParamsSchema::Allowed(TARGET_PARAMS),
    },
    ControlMethodSpec {
        method: "pane/create",
        dispatch: ControlDispatchKind::PaneCreate,
        params_schema: ControlParamsSchema::Allowed(&[
            "target",
            "pane_id",
            "pane_title",
            "pane_index",
            "split",
            "start_directory",
            "shell_command",
            "size",
            "select",
            "idempotency_key",
        ]),
    },
    ControlMethodSpec {
        method: "pane/select",
        dispatch: ControlDispatchKind::PaneSelect,
        params_schema: ControlParamsSchema::Allowed(PANE_TARGET_MUTATION_PARAMS),
    },
    ControlMethodSpec {
        method: "pane/resize",
        dispatch: ControlDispatchKind::PaneResize,
        params_schema: ControlParamsSchema::Allowed(PANE_TARGET_MUTATION_PARAMS),
    },
    ControlMethodSpec {
        method: "pane/break",
        dispatch: ControlDispatchKind::PaneBreak,
        params_schema: ControlParamsSchema::Allowed(PANE_TARGET_MUTATION_PARAMS),
    },
    ControlMethodSpec {
        method: "pane/close",
        dispatch: ControlDispatchKind::PaneClose,
        params_schema: ControlParamsSchema::Allowed(PANE_TARGET_MUTATION_PARAMS),
    },
    ControlMethodSpec {
        method: "pane/capture",
        dispatch: ControlDispatchKind::PaneCapture,
        params_schema: ControlParamsSchema::Allowed(PANE_TARGET_MUTATION_PARAMS),
    },
    ControlMethodSpec {
        method: "pane/swap",
        dispatch: ControlDispatchKind::PaneSwap,
        params_schema: ControlParamsSchema::Allowed(PANE_TRANSFER_PARAMS),
    },
    ControlMethodSpec {
        method: "pane/join",
        dispatch: ControlDispatchKind::PaneJoinMove,
        params_schema: ControlParamsSchema::Allowed(PANE_TRANSFER_PARAMS),
    },
    ControlMethodSpec {
        method: "pane/move",
        dispatch: ControlDispatchKind::PaneJoinMove,
        params_schema: ControlParamsSchema::Allowed(PANE_TRANSFER_PARAMS),
    },
    ControlMethodSpec {
        method: "frame/read",
        dispatch: ControlDispatchKind::FrameRead,
        params_schema: ControlParamsSchema::Allowed(FRAME_TARGET_PARAMS),
    },
    ControlMethodSpec {
        method: "terminal/view",
        dispatch: ControlDispatchKind::TerminalView,
        params_schema: ControlParamsSchema::Allowed(&["client_size", "view_offset", "viewport"]),
    },
    ControlMethodSpec {
        method: "terminal/step",
        dispatch: ControlDispatchKind::TerminalStep,
        params_schema: ControlParamsSchema::Allowed(&[
            "idempotency_key",
            "client_size",
            "render",
            "input_bytes",
        ]),
    },
    ControlMethodSpec {
        method: "terminal/command",
        dispatch: ControlDispatchKind::TerminalCommand,
        params_schema: ControlParamsSchema::Allowed(&["idempotency_key", "input"]),
    },
    ControlMethodSpec {
        method: "agent/list",
        dispatch: ControlDispatchKind::AgentList,
        params_schema: ControlParamsSchema::Allowed(TARGET_PARAMS),
    },
    ControlMethodSpec {
        method: "agent/task/list",
        dispatch: ControlDispatchKind::AgentTaskList,
        params_schema: ControlParamsSchema::Allowed(&["target", "agent_id", "pane_id"]),
    },
    ControlMethodSpec {
        method: "agent/spawn",
        dispatch: ControlDispatchKind::AgentSpawn,
        params_schema: ControlParamsSchema::Allowed(&[
            "parent_agent",
            "placement",
            "role",
            "cooperation_mode",
            "read_scopes",
            "write_scopes",
            "prompt",
            "idempotency_key",
        ]),
    },
    ControlMethodSpec {
        method: "agent/shell/show",
        dispatch: ControlDispatchKind::AgentShellVisibility { visible: true },
        params_schema: ControlParamsSchema::Allowed(&[
            "target",
            "pane_id",
            "pane_title",
            "pane_index",
            "idempotency_key",
        ]),
    },
    ControlMethodSpec {
        method: "agent/shell/hide",
        dispatch: ControlDispatchKind::AgentShellVisibility { visible: false },
        params_schema: ControlParamsSchema::Allowed(&[
            "target",
            "pane_id",
            "pane_title",
            "pane_index",
            "idempotency_key",
        ]),
    },
    ControlMethodSpec {
        method: "agent/shell/command",
        dispatch: ControlDispatchKind::AgentShellCommand,
        params_schema: ControlParamsSchema::Allowed(&["input", "idempotency_key"]),
    },
    ControlMethodSpec {
        method: "event/list",
        dispatch: ControlDispatchKind::EventList,
        params_schema: ControlParamsSchema::Allowed(&["after_event_id", "limit", "extensions"]),
    },
    ControlMethodSpec {
        method: "approval/list",
        dispatch: ControlDispatchKind::ApprovalList,
        params_schema: ControlParamsSchema::Allowed(&["target", "state"]),
    },
    ControlMethodSpec {
        method: "approval/decide",
        dispatch: ControlDispatchKind::ApprovalDecide,
        params_schema: ControlParamsSchema::Allowed(&[
            "approval_id",
            "decision",
            "scope",
            "instruction",
            "idempotency_key",
        ]),
    },
    ControlMethodSpec {
        method: "snapshot/list",
        dispatch: ControlDispatchKind::SnapshotList,
        params_schema: ControlParamsSchema::Allowed(TARGET_PARAMS),
    },
    ControlMethodSpec {
        method: "snapshot/create",
        dispatch: ControlDispatchKind::SnapshotCreate,
        params_schema: ControlParamsSchema::Allowed(&["target", "name", "idempotency_key"]),
    },
    ControlMethodSpec {
        method: "snapshot/resume",
        dispatch: ControlDispatchKind::SnapshotResume,
        params_schema: ControlParamsSchema::Allowed(&["snapshot_id", "idempotency_key"]),
    },
    ControlMethodSpec {
        method: "snapshot/delete",
        dispatch: ControlDispatchKind::SnapshotDelete,
        params_schema: ControlParamsSchema::Allowed(&["snapshot_id", "idempotency_key"]),
    },
    ControlMethodSpec {
        method: "project/trust/list",
        dispatch: ControlDispatchKind::ProjectTrustList,
        params_schema: ControlParamsSchema::Allowed(&["state"]),
    },
    ControlMethodSpec {
        method: "project/trust/inspect",
        dispatch: ControlDispatchKind::ProjectTrustInspect,
        params_schema: ControlParamsSchema::Allowed(&["project_root"]),
    },
    ControlMethodSpec {
        method: "project/trust/decide",
        dispatch: ControlDispatchKind::ProjectTrustDecide,
        params_schema: ControlParamsSchema::Allowed(&[
            "project_root",
            "decision",
            "reason",
            "idempotency_key",
        ]),
    },
    ControlMethodSpec {
        method: "project/trust/revoke",
        dispatch: ControlDispatchKind::ProjectTrustRevoke,
        params_schema: ControlParamsSchema::Allowed(&["project_root", "reason", "idempotency_key"]),
    },
    ControlMethodSpec {
        method: "mcp/list",
        dispatch: ControlDispatchKind::McpList,
        params_schema: ControlParamsSchema::Allowed(TARGET_PARAMS),
    },
    ControlMethodSpec {
        method: "mcp/retry",
        dispatch: ControlDispatchKind::McpRetry,
        params_schema: ControlParamsSchema::Allowed(&["server_id", "id", "idempotency_key"]),
    },
    ControlMethodSpec {
        method: "config/validate",
        dispatch: ControlDispatchKind::Config,
        params_schema: ControlParamsSchema::Config,
    },
    ControlMethodSpec {
        method: "config/get",
        dispatch: ControlDispatchKind::Config,
        params_schema: ControlParamsSchema::Config,
    },
    ControlMethodSpec {
        method: "config/set",
        dispatch: ControlDispatchKind::Config,
        params_schema: ControlParamsSchema::Config,
    },
    ControlMethodSpec {
        method: "config/unset",
        dispatch: ControlDispatchKind::Config,
        params_schema: ControlParamsSchema::Config,
    },
    ControlMethodSpec {
        method: "config/reload",
        dispatch: ControlDispatchKind::Config,
        params_schema: ControlParamsSchema::Config,
    },
];

/// Finds the registry entry for one JSON-RPC method name.
pub(super) fn control_method_spec(method: &str) -> Option<&'static ControlMethodSpec> {
    CONTROL_METHOD_REGISTRY
        .iter()
        .find(|spec| spec.method == method)
}
