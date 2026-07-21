//! Runtime Types implementation.
//!
//! This module owns the runtime types boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    ActionStatus, AgentShellVisibility, AgentTurnState, AuthStore, HookEvent, HookExecutionPlan,
    HookFailureKind, McpServerStatus, McpStartupPlan, McpStdioConnection, McpToolCallPlan,
    McpToolCallResponse, MezError, ModelRequest, ModelResponse, ModelTokenUsage,
    ModelTokenUsageKey, PaneGeometry, PaneId, Result, SessionRecord, Size, SplitDirection,
    WindowId, execute_streamable_http_exchange, mcp_tools_call_operation,
};

// Runtime data types, connection tables, and provider/MCP registries.

mod agent_state;
mod interaction;
mod lifecycle;
mod mcp_transport;
mod metrics;
pub(crate) use agent_state::{
    BlockedAgentApprovalRef, JoinedSubagentDependency, PaneDescriptor,
    PendingFocusedShellHookContinuation, PendingFocusedShellHookTransaction,
    RunningShellTransactionKind, RunningShellTransactionRef, RuntimeAgentPersonalityProfile,
    RuntimeAgentPreShellHookCompletion, RuntimeApplyPatchBatchState, RuntimeDisplayOverlay,
    RuntimeHookPipelineDecision, RuntimeModelProfileOverrideScope,
    RuntimeModelProfileOverrideStore, RuntimePaneAgentStatusSelector,
    RuntimePathResolutionCacheKey, RuntimeRecordBrowserOverlayFrame,
    RuntimeRecordBrowserOverlaySource, RuntimeRecordBrowserOverlayState,
    RuntimeSandboxFailureAssessment, RuntimeSandboxFallbackAudit,
    RuntimeShellTransactionActionFailure, RuntimeSubagentLineage,
};
pub use agent_state::{
    RuntimeShellTransactionTimerKind, RuntimeShellTransactionTimerRef, SubagentWaitPolicy,
};
pub(crate) use interaction::{
    MouseResizeDragState, MouseSelectionDragState, RuntimeAgentCopyOutput,
    RuntimeAgentModifiedFileSummary, RuntimeAgentPromptInput, RuntimeCommandBinding,
    RuntimeMouseClickState, RuntimePrimaryPromptInput, RuntimeSubagentPlacement,
};
pub use interaction::{
    RuntimeAgentPromptTurnStart, RuntimeAgentTurnStop, RuntimeConfigApplyReport,
};
pub(crate) use lifecycle::RuntimeAgentPatchRecord;
pub use lifecycle::{RuntimeLifecycleState, RuntimeRegistryUpdatePlan};
pub(crate) use lifecycle::{
    RuntimeSnapshotControlAsyncOutcome, RuntimeSnapshotControlAsyncWork,
    RuntimeSnapshotControlAsyncWorkKind, RuntimeSnapshotOwnedCreationContext,
};
pub(crate) use mcp_transport::{
    RuntimeHookPipelineBlock, RuntimeHttpMcpTransportState, RuntimeMcpRetryReport,
    RuntimeMcpTransportSet,
};
pub use metrics::{
    DEFAULT_AGENT_ACTION_FAILURE_RETRY_LIMIT, DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT,
    DEFAULT_AGENT_LOOP_LIMIT, DEFAULT_AGENT_ROUTING, DEFAULT_MAX_ROOT_SUBAGENTS,
    DEFAULT_MAX_SUBAGENT_DEPTH, DEFAULT_MAX_SUBAGENT_PANES_PER_WINDOW,
    DEFAULT_MAX_SUBAGENTS_PER_SUBAGENT, DEFAULT_PTY_READ_LIMIT_BYTES, DEFAULT_SUBAGENT_WAIT_POLICY,
};
pub(crate) use metrics::{ProgramOwnedPaneTitle, RuntimeMetricsSnapshot};
