//! Control Types implementation.
//!
//! This module owns the control types boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{current_rfc3339_seconds, effective_uid};
use mez_mux::process::PaneExitStatus;
use mez_terminal::TerminalStyleSpan;

// Control protocol data types and capabilities.

/// Defines the CONTROL CONTENT TYPE const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const CONTROL_CONTENT_TYPE: &str = "application/vnd.mezzanine.control+json; version=1";
/// Defines the MAX EVENT REPLAY RETENTION const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const MAX_EVENT_REPLAY_RETENTION: usize = 1_000;

/// Carries Requested Role state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestedRole {
    /// Represents the Primary case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Primary,
    /// Represents the Observer case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Observer,
    /// Represents the Agent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Agent,
    /// Represents the Automation case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Automation,
}

/// Carries Granted Role state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantedRole {
    /// Represents the Primary case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Primary,
    /// Represents the Pending Observer case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PendingObserver,
    /// Represents the Observer case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Observer,
    /// Represents the Agent case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Agent,
    /// Represents the Automation case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Automation,
}

/// Carries Authentication Mechanism state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthenticationMechanism {
    /// Represents the Peer Credentials case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PeerCredentials,
    /// Represents the Bearer Token case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    BearerToken,
    /// Represents the None case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    None,
    /// Represents the Extension case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Extension(String),
}

/// Carries Authentication Material state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticationMaterial {
    /// Stores the mechanism value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mechanism: AuthenticationMechanism,
    /// Stores the token value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub token: Option<String>,
}

impl AuthenticationMaterial {
    /// Runs the none operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn none() -> Self {
        Self {
            mechanism: AuthenticationMechanism::None,
            token: None,
        }
    }

    /// Runs the bearer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn bearer(token: impl Into<String>) -> Self {
        Self {
            mechanism: AuthenticationMechanism::BearerToken,
            token: Some(token.into()),
        }
    }

    /// Runs the peer credentials operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn peer_credentials() -> Self {
        Self {
            mechanism: AuthenticationMechanism::PeerCredentials,
            token: None,
        }
    }

    /// Runs the is payload authenticated operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn is_payload_authenticated(&self) -> bool {
        match self.mechanism {
            AuthenticationMechanism::PeerCredentials => true,
            AuthenticationMechanism::BearerToken => false,
            AuthenticationMechanism::None => false,
            AuthenticationMechanism::Extension(_) => false,
        }
    }
}

/// Carries Terminal Descriptor state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalDescriptor {
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
    /// Stores the term value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub term: String,
    /// Stores the features value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub features: Vec<String>,
}

/// Carries Client Stdio Descriptor state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientStdioDescriptor {
    /// Stores the stdin is tty value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub stdin_is_tty: Option<bool>,
    /// Stores the stdout is tty value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub stdout_is_tty: Option<bool>,
    /// Stores the stderr is tty value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub stderr_is_tty: Option<bool>,
    /// Stores the controlling tty value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub controlling_tty: Option<String>,
    /// Stores the tty device value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub tty_device: Option<String>,
}

/// Carries Client Descriptor state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientDescriptor {
    /// Stores the name value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub name: String,
    /// Stores the version value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub version: Option<String>,
    /// Stores the pid value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pid: Option<u32>,
    /// Stores the host value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub host: Option<String>,
    /// Stores the user value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub user: Option<String>,
    /// Stores the purpose value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub purpose: Option<String>,
    /// Stores the requested role value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub requested_role: Option<RequestedRole>,
    /// Stores the interactive value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub interactive: bool,
    /// Stores the stdio value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub stdio: Option<ClientStdioDescriptor>,
    /// Stores the metadata json value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub metadata_json: Option<String>,
    /// Stores the terminal value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub terminal: Option<TerminalDescriptor>,
}

impl ClientDescriptor {
    /// Runs the identifies interactive terminal operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn identifies_interactive_terminal(&self, trusted_assertion: bool) -> bool {
        self.interactive && self.terminal.is_some() && trusted_assertion
    }
}

/// Carries Initialize Params state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitializeParams {
    /// Stores the client name value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub client_name: String,
    /// Stores the requested version value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub requested_version: u32,
    /// Stores the requested role value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub requested_role: RequestedRole,
    /// Stores the client version value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub client_version: Option<String>,
    /// Stores the session target json value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_target_json: Option<String>,
    /// Stores whether a primary connection should detach its client on disconnect.
    ///
    /// Foreground attach clients set this for the long-lived terminal control
    /// socket. Short-lived administrative control clients leave it disabled so
    /// closing a request connection does not clear primary ownership.
    pub detach_primary_on_disconnect: bool,
    /// Stores the client value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub client: Option<ClientDescriptor>,
    /// Stores the authentication value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub authentication: Option<AuthenticationMaterial>,
}

/// Carries Capabilities state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// Stores the protocol version value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub protocol_version: u32,
    /// Stores the methods value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub methods: Vec<&'static str>,
    /// Stores the event types value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub event_types: Vec<&'static str>,
    /// Stores the roles value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub roles: Vec<&'static str>,
    /// Stores the transports value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub transports: Vec<&'static str>,
    /// Stores the limits value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub limits: CapabilityLimits,
    /// Stores the features value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub features: CapabilityFeatures,
}

/// Carries Capability Limits state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityLimits {
    /// Stores the max frame size value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub max_frame_size: usize,
    /// Stores the max request size value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub max_request_size: usize,
    /// Stores the max event replay retention value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub max_event_replay_retention: usize,
    /// Stores the max capture payload size value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub max_capture_payload_size: usize,
}

/// Carries Capability Features state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityFeatures {
    /// Stores the tcp value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub tcp: bool,
    /// Stores the event replay value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub event_replay: bool,
    /// Stores the observers value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub observers: bool,
    /// Stores the mcp value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub mcp: bool,
    /// Stores the snapshots value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub snapshots: bool,
    /// Stores the audit value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub audit: bool,
    /// Stores the approval bypass value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub approval_bypass: bool,
}

/// Defines the PENDING OBSERVER CONTROL METHODS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(crate) const PENDING_OBSERVER_CONTROL_METHODS: &[&str] = &[
    "control/initialize",
    "session/attach",
    "observer/inspect",
    "control/cancel",
    "control/shutdown",
];

/// Defines the OBSERVER CONTROL METHODS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(crate) const OBSERVER_CONTROL_METHODS: &[&str] = &[
    "control/initialize",
    "control/shutdown",
    "control/cancel",
    "terminal/view",
    "event/list",
    "session/attach",
    "observer/inspect",
];

/// Defines the AGENT CONTROL METHODS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(crate) const AGENT_CONTROL_METHODS: &[&str] = &[
    "control/initialize",
    "control/shutdown",
    "control/cancel",
    "mcp/list",
    "agent/spawn",
    "event/list",
];

/// Defines the AUTOMATION CONTROL METHODS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(crate) const AUTOMATION_CONTROL_METHODS: &[&str] = &[
    "control/initialize",
    "control/shutdown",
    "control/cancel",
    "session/list",
    "event/list",
];

/// Defines the UNAUTHENTICATED CONTROL METHODS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(crate) const UNAUTHENTICATED_CONTROL_METHODS: &[&str] =
    &["control/initialize", "control/shutdown"];

/// Defines the PRIMARY CONTROL METHODS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub(crate) const PRIMARY_CONTROL_METHODS: &[&str] = &[
    "control/initialize",
    "control/cancel",
    "control/shutdown",
    "session/list",
    "session/get",
    "session/rename",
    "session/kill",
    "window/list",
    "window/create",
    "window/rename",
    "window/select",
    "window/close",
    "pane/list",
    "pane/create",
    "pane/select",
    "pane/resize",
    "pane/move",
    "pane/swap",
    "pane/break",
    "pane/join",
    "pane/close",
    "pane/capture",
    "terminal/step",
    "terminal/view",
    "terminal/command",
    "frame/read",
    "agent/shell/show",
    "agent/shell/hide",
    "agent/shell/command",
    "agent/list",
    "agent/task/list",
    "agent/spawn",
    "client/list",
    "client/detach",
    "client/select_primary",
    "observer/list",
    "observer/inspect",
    "observer/approve",
    "observer/reject",
    "observer/revoke",
    "event/list",
    "config/validate",
    "config/get",
    "config/set",
    "config/unset",
    "config/reload",
    "project/trust/list",
    "project/trust/inspect",
    "project/trust/decide",
    "project/trust/revoke",
    "mcp/list",
    "mcp/retry",
    "approval/list",
    "approval/decide",
    "snapshot/list",
    "snapshot/create",
    "snapshot/resume",
    "snapshot/delete",
];

/// Carries Pane Capture Source state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneCaptureSource {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the visible lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub visible_lines: Vec<String>,
    /// Non-default SGR style spans aligned to `visible_lines`.
    pub visible_line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Stores the history lines value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub history_lines: Vec<String>,
    /// Non-default SGR style spans aligned to `history_lines`.
    pub history_line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Stores the alternate screen active value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub alternate_screen_active: bool,
    /// Stores the truncated value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub truncated: bool,
    /// Stores the primary pid value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub primary_pid: Option<u32>,
    /// Stores the process state value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub process_state: Option<String>,
    /// Stores the readiness state value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub readiness_state: Option<String>,
    /// Stores the exit status value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub exit_status: Option<PaneExitStatus>,
}

impl PaneCaptureSource {
    /// Runs the visible operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn visible(pane_id: impl Into<String>, lines: Vec<String>) -> Self {
        Self {
            pane_id: pane_id.into(),
            visible_line_style_spans: vec![Vec::new(); lines.len()],
            visible_lines: lines,
            history_lines: Vec::new(),
            history_line_style_spans: Vec::new(),
            alternate_screen_active: false,
            truncated: false,
            primary_pid: None,
            process_state: None,
            readiness_state: None,
            exit_status: None,
        }
    }
}

/// Carries Capture Origin state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CaptureOrigin {
    /// Represents the Visible case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Visible,
    /// Represents the History case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    History,
    /// Represents the Combined case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Combined,
}

/// Carries Capture Endpoint state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CaptureEndpoint {
    /// Represents the Start case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Start,
    /// Represents the End case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    End,
    /// Represents the Offset case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Offset(usize),
}

/// Carries Capture Range state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CaptureRange {
    /// Stores the origin value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) origin: CaptureOrigin,
    /// Stores the start value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) start: CaptureEndpoint,
    /// Stores the end value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) end: CaptureEndpoint,
}

impl Capabilities {
    /// Runs the with methods operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn with_methods(methods: Vec<&'static str>) -> Self {
        Self {
            protocol_version: 1,
            methods,
            event_types: vec![
                "client_attached",
                "client_detached",
                "observer_requested",
                "observer_decided",
                "window_changed",
                "pane_changed",
                "agent_status",
                "message",
                "config_changed",
                "snapshot_changed",
                "approval_changed",
                "mcp_server_changed",
                "hook_failed",
                "diagnostic",
            ],
            roles: vec![
                "primary",
                "pending_observer",
                "observer",
                "agent",
                "automation",
            ],
            transports: vec!["unix"],
            limits: CapabilityLimits {
                max_frame_size: 1_048_576,
                max_request_size: 1_048_576,
                max_event_replay_retention: MAX_EVENT_REPLAY_RETENTION,
                max_capture_payload_size: 1_048_576,
            },
            features: CapabilityFeatures {
                tcp: false,
                event_replay: true,
                observers: true,
                mcp: true,
                snapshots: true,
                audit: true,
                approval_bypass: true,
            },
        }
    }

    /// Runs the pending observer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn pending_observer() -> Self {
        let mut capabilities = Self::with_methods(PENDING_OBSERVER_CONTROL_METHODS.to_vec());
        capabilities.features.event_replay = false;
        capabilities.features.mcp = false;
        capabilities.features.snapshots = false;
        capabilities.features.audit = false;
        capabilities.features.approval_bypass = false;
        capabilities
    }

    /// Runs the primary operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn primary() -> Self {
        Self::with_methods(PRIMARY_CONTROL_METHODS.to_vec())
    }

    /// Runs the unauthenticated operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn unauthenticated() -> Self {
        let mut capabilities = Self::with_methods(UNAUTHENTICATED_CONTROL_METHODS.to_vec());
        capabilities.features.event_replay = false;
        capabilities.features.observers = false;
        capabilities.features.mcp = false;
        capabilities.features.snapshots = false;
        capabilities.features.audit = false;
        capabilities.features.approval_bypass = false;
        capabilities
    }

    /// Runs the observer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn observer() -> Self {
        let mut capabilities = Self::with_methods(OBSERVER_CONTROL_METHODS.to_vec());
        capabilities.features.mcp = false;
        capabilities.features.snapshots = false;
        capabilities.features.audit = false;
        capabilities.features.approval_bypass = false;
        capabilities
    }

    /// Runs the agent operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn agent() -> Self {
        Self::with_methods(AGENT_CONTROL_METHODS.to_vec())
    }

    /// Runs the automation operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn automation() -> Self {
        Self::with_methods(AUTOMATION_CONTROL_METHODS.to_vec())
    }
}

/// Carries Observer Request Summary state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObserverRequestSummary {
    /// Stores the request id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub request_id: String,
    /// Stores the state value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub state: &'static str,
    /// Stores the state json value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub state_json: Option<String>,
}

/// Carries Server Identity state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerIdentity {
    /// Stores the id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: String,
    /// Stores the implementation name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub implementation_name: &'static str,
    /// Stores the version value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub version: &'static str,
    /// Stores the protocol versions value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub protocol_versions: Vec<u32>,
    /// Stores the started at value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub started_at: String,
    /// Stores the user id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub user_id: Option<u32>,
    /// Stores the host value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub host: Option<String>,
    /// Stores the pid value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pid: Option<u32>,
}

impl ServerIdentity {
    /// Runs the current operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn current() -> Self {
        let pid = std::process::id();
        Self {
            id: format!("mez-{pid}"),
            implementation_name: "mezzanine",
            version: env!("CARGO_PKG_VERSION"),
            protocol_versions: vec![1],
            started_at: current_rfc3339_seconds(),
            user_id: Some(effective_uid()),
            host: std::env::var("HOSTNAME")
                .ok()
                .filter(|host| !host.trim().is_empty()),
            pid: Some(pid),
        }
    }
}

/// Carries Initialize Result state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitializeResult {
    /// Stores the selected version value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub selected_version: u32,
    /// Stores the server value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub server: ServerIdentity,
    /// Stores the session value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session: Option<String>,
    /// Stores the granted role value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub granted_role: GrantedRole,
    /// Stores the capabilities value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub capabilities: Capabilities,
    /// Stores the approval pending value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub approval_pending: bool,
    /// Stores the observer request value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub observer_request: Option<ObserverRequestSummary>,
}

/// Carries Initialize Context state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitializeContext {
    /// Stores the outer authenticated value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub outer_authenticated: bool,
    /// Stores the trusted interactive assertion value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub trusted_interactive_assertion: bool,
}
