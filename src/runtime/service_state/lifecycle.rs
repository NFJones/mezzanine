//! Runtime lifecycle, registry-update, and snapshot work contracts.

use super::{Result, SessionRecord};

/// One retained `apply_patch` attempt emitted by the current pane agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::runtime) struct RuntimeAgentPatchRecord {
    /// Turn id that contained the patch action.
    pub turn_id: String,
    /// Action id assigned by the model to the patch action.
    pub action_id: String,
    /// Lowercase action status observed by the runtime.
    pub status: String,
    /// Patch body exactly as emitted in the MAAP action payload.
    pub patch: String,
    /// Optional `strip` value supplied with the patch payload.
    pub strip: Option<u64>,
    /// Optional structured error code recorded for a failed patch.
    pub error_code: Option<String>,
    /// Optional human-readable error or patch diagnostic for a failed patch.
    pub error_message: Option<String>,
}

/// Carries Runtime Lifecycle State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeLifecycleState {
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
    /// Represents the Stopping case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Stopping,
    /// Represents the Killed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Killed,
    /// Represents the Failed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Failed,
}

/// Carries Runtime Registry Update Plan state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeRegistryUpdatePlan {
    /// Represents the Upsert case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Upsert(SessionRecord),
    /// Represents the Remove case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Remove {
        /// Stores the session id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        session_id: String,
    },
}

/// Owned snapshot creation context captured by the actor before repository I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeSnapshotOwnedCreationContext {
    /// Live pane terminal/process captures.
    pub pane_captures: Vec<crate::snapshot::SnapshotPaneCapture>,
    /// Active config layers at capture time.
    pub active_config_layers: Vec<crate::snapshot::SnapshotConfigLayerMetadata>,
    /// Live terminal frame state at capture time.
    pub frame_state: crate::snapshot::SnapshotFrameState,
    /// Agent sessions to include in the snapshot payload.
    pub agent_sessions: Vec<crate::snapshot::SnapshotAgentSession>,
    /// Approval grants to include in the snapshot payload.
    pub approval_grants: Vec<crate::snapshot::SnapshotApprovalGrantMetadata>,
    /// Approval requests to include in the snapshot payload.
    pub approval_requests: Vec<crate::snapshot::SnapshotApprovalRequestMetadata>,
    /// Message-service state to include in the snapshot payload.
    pub message_state: mez_agent::messaging::MessageServiceSnapshot,
    /// MCP server state to include in the snapshot payload.
    pub mcp_servers: Vec<crate::snapshot::SnapshotMcpServerState>,
}

impl RuntimeSnapshotOwnedCreationContext {
    /// Borrows the owned context as the snapshot repository creation context.
    pub(crate) fn as_creation_context(&self) -> crate::snapshot::SnapshotCreationContext<'_> {
        crate::snapshot::SnapshotCreationContext::new(
            &self.pane_captures,
            &self.active_config_layers,
            &self.frame_state,
            &self.agent_sessions,
        )
        .with_approvals(&self.approval_grants, &self.approval_requests)
        .with_message_state(&self.message_state)
        .with_mcp_servers(&self.mcp_servers)
    }
}

/// Snapshot control operation that can perform repository I/O off the actor.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeSnapshotControlAsyncWork {
    /// Parsed JSON-RPC request.
    pub request: crate::control::JsonRpcRequest,
    /// Client authorized to make the request.
    pub caller_client_id: mez_core::ids::ClientId,
    /// Operation-specific repository work.
    pub kind: RuntimeSnapshotControlAsyncWorkKind,
}

/// Repository work shape for actor-deferred snapshot control operations.
#[derive(Debug, Clone)]
pub(crate) enum RuntimeSnapshotControlAsyncWorkKind {
    /// Snapshot list/create/delete or plan-only resume dispatch.
    Dispatch {
        /// Session snapshot captured before the repository operation.
        session: Box<mez_mux::session::Session>,
        /// Owned snapshot context captured before the repository operation.
        context: Box<RuntimeSnapshotOwnedCreationContext>,
    },
    /// Live snapshot resume that must return payload metadata for actor apply.
    Resume {
        /// Shell to seed restored panes with.
        shell: crate::shell::ResolvedShell,
    },
}

/// Repository result returned to the actor after async snapshot control work.
#[derive(Debug)]
pub(crate) enum RuntimeSnapshotControlAsyncOutcome {
    /// JSON result body produced by the snapshot dispatcher.
    Dispatch(Result<String>),
    /// Live resume payload plus restored session state.
    Resume(
        Box<
            Result<(
                crate::snapshot::SessionSnapshotPayload,
                crate::snapshot::SnapshotRestoreResult,
            )>,
        >,
    ),
}
