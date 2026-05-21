//! Event data types and retained log storage.
//!
//! These types define the stable event stream contract. The log owns retention
//! state while visibility and notification encoding live in sibling modules.

use std::collections::VecDeque;

/// Kinds of state changes emitted by the Mezzanine runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// A client attached to the session.
    ClientAttached,
    /// A client detached from the session.
    ClientDetached,
    /// Observer access was requested.
    ObserverRequested,
    /// Observer access was approved or denied.
    ObserverDecided,
    /// Window state changed.
    WindowChanged,
    /// Pane state changed.
    PaneChanged,
    /// Agent status changed.
    AgentStatus,
    /// A message was created or changed.
    Message,
    /// Configuration changed.
    ConfigChanged,
    /// Snapshot state changed.
    SnapshotChanged,
    /// Approval state changed.
    ApprovalChanged,
    /// MCP server state changed.
    McpServerChanged,
    /// A hook failed.
    HookFailed,
    /// Diagnostic information was emitted.
    Diagnostic,
}

/// Visibility policy attached to an event at append time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventVisibility {
    /// Visible only to the primary client.
    PrimaryOnly,
    /// Visible to the session view after observer approval.
    SessionView,
    /// Visible only to one pending observer request.
    PendingObserverRequest(String),
    /// Visible only to one agent.
    Agent(String),
    /// Visible to automation clients.
    Automation,
}

/// Audience requesting retained event replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventAudience {
    /// Primary client with complete session visibility.
    Primary,
    /// Approved observer, visible only from its approval marker onward.
    ApprovedObserver {
        /// Earliest event id the observer may see.
        visible_from_event_id: u64,
    },
    /// Pending observer, visible only to request-local status.
    PendingObserver {
        /// Observer request id.
        observer_request_id: String,
    },
    /// Agent-local event stream.
    Agent {
        /// Agent id requesting replay.
        agent_id: String,
    },
    /// Automation client stream.
    Automation,
}

/// Retained event with full visibility metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MezzanineEvent {
    /// Monotonic event id.
    pub id: u64,
    /// RFC 3339 timestamp for when the event was appended.
    pub time: String,
    /// Event kind.
    pub kind: EventKind,
    /// Optional session id associated with the event.
    pub session_id: Option<String>,
    /// Visibility policy for replay.
    pub visibility: EventVisibility,
    /// JSON object string or plain payload to encode in notifications.
    pub payload: String,
}

/// Event projected for a specific audience.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleEvent {
    /// Monotonic event id.
    pub id: u64,
    /// RFC 3339 timestamp for when the event was appended.
    pub time: String,
    /// Event kind.
    pub kind: EventKind,
    /// Optional session id visible to the audience.
    pub session_id: Option<String>,
    /// Audience-visible payload.
    pub payload: String,
}

/// Bounded retained event log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLog {
    /// Stores the max events value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) max_events: usize,
    /// Stores the max payload bytes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) max_payload_bytes: usize,
    /// Stores the next id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) next_id: u64,
    /// Stores the events value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) events: VecDeque<MezzanineEvent>,
}
