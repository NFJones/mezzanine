//! Runtime Types implementation.
//!
//! This module owns the runtime types boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::RuntimeAutoSizingConfig;
use super::agent_state::{
    RuntimeAgentCompactionTask, RuntimeAgentLoopState, RuntimeAgentLoopTurn,
    RuntimeAgentProviderClaim, RuntimeAgentRememberTask,
};
use super::pane_io::{ActivePanePipe, PaneExitRecord};
use super::{
    ActionStatus, AgentAction, AgentActionPayload, AgentContext, AgentScheduler, AgentShellStore,
    AgentShellVisibility, AgentTranscriptStore, AgentTurnExecution, AgentTurnLedger,
    AgentTurnState, AuditLog, AuthStore, BTreeMap, BTreeSet, BlockedApprovalQueue, ConfigLayer,
    ControlIdempotencyCache, CopyMode, EnvironmentSignature, EventLog, FocusedShellHookQueue,
    HookDefinition, HookEvent, HookExecutionPlan, HookExecutionResult, HookFailureKind,
    HostClipboard, KeyBindings, KeyChord, McpRegistry, McpServerStatus, McpStartupPlan,
    McpStdioConnection, McpToolCallPlan, McpToolCallResponse, MessageService, MezError,
    ModelProfile, ModelRequest, ModelResponse, ModelTokenUsage, ModelTokenUsageKey, PaneGeometry,
    PaneId, PaneProcessManager, PaneReadinessOverrideStore, PaneReadinessState, PasteBuffers,
    PathBuf, PermissionPolicy, ProjectTrustStore, ProviderQuotaUsage, Result, RuntimeSideEffect,
    RuntimeStatusPillCache, RuntimeStatusPillDefinition, ScopeRegistry, Session,
    SessionApprovalStore, SessionMemoryStore, SessionRecord, SessionRegistry, Size,
    SnapshotRepository, SplitDirection, SubagentProfile, SubagentScopeDeclaration,
    TerminalCursorStyle, TerminalScreen, ToolDiscoveryCache, WindowFrameAction, WindowId,
    execute_streamable_http_exchange, mcp_tools_call_operation,
};
use super::{RuntimePresetRegistry, RuntimeProviderRegistry};
use crate::error::MezErrorKind;
use crate::readline::{ReadlineInputDecoder, ReadlinePrompt};
use crate::terminal::PaneAgentStatusField;
use mez_agent::instructions::DiscoveredInstructionFile;
use mez_agent::{MacroManagedSubagent, MacroRunState};
use mez_mux::copy::CopyPosition;
use mez_mux::layout::PaneTitleSource;
use mez_mux::presentation::{TerminalFramePosition, TerminalFrameStyle};
use mez_mux::record_browser::RecordBrowser;
use mez_mux::theme::UiTheme;
use mez_terminal::TerminalEmojiWidth;
use secrecy::ExposeSecret;

// Runtime data types, connection tables, and provider/MCP registries.

mod agent_state;
mod interaction;
mod lifecycle;
mod mcp_transport;
mod metrics;
mod session;
pub(in crate::runtime) use agent_state::{
    BlockedAgentApprovalRef, JoinedSubagentDependency, PaneDescriptor,
    PendingFocusedShellHookContinuation, PendingFocusedShellHookTransaction,
    RunningShellTransactionKind, RunningShellTransactionRef, RuntimeAgentPersonalityProfile,
    RuntimeAgentPreShellHookCompletion, RuntimeApplyPatchBatchState, RuntimeDisplayOverlay,
    RuntimeHookPipelineDecision, RuntimeModelProfileOverrideScope,
    RuntimeModelProfileOverrideStore, RuntimePaneAgentStatusSelector,
    RuntimeRecordBrowserOverlayFrame, RuntimeRecordBrowserOverlaySource,
    RuntimeRecordBrowserOverlayState, RuntimeShellTransactionActionFailure, RuntimeSubagentLineage,
};
pub use agent_state::{
    RuntimeShellTransactionTimerKind, RuntimeShellTransactionTimerRef, SubagentWaitPolicy,
};
pub(in crate::runtime) use interaction::{
    MouseResizeDragState, MouseSelectionDragState, RuntimeAgentCopyOutput,
    RuntimeAgentModifiedFileSummary, RuntimeAgentNetworkActionHistory, RuntimeAgentPromptInput,
    RuntimeAgentShellDispatchHistory, RuntimeAgentTurnSteering, RuntimeCommandBinding,
    RuntimeMouseClickState, RuntimePrimaryPromptInput, RuntimeSubagentPlacement,
};
pub use interaction::{
    RuntimeAgentPromptTurnStart, RuntimeAgentTurnStop, RuntimeConfigApplyReport,
};
pub(in crate::runtime) use lifecycle::RuntimeAgentPatchRecord;
pub use lifecycle::{RuntimeLifecycleState, RuntimeRegistryUpdatePlan};
pub(crate) use lifecycle::{
    RuntimeSnapshotControlAsyncOutcome, RuntimeSnapshotControlAsyncWork,
    RuntimeSnapshotControlAsyncWorkKind, RuntimeSnapshotOwnedCreationContext,
};
pub(in crate::runtime) use mcp_transport::{
    RuntimeHookPipelineBlock, RuntimeHttpMcpTransportState, RuntimeMcpRetryReport,
    RuntimeMcpTransportSet,
};
pub use metrics::{
    DEFAULT_AGENT_ACTION_FAILURE_RETRY_LIMIT, DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT,
    DEFAULT_AGENT_IMPLEMENTATION_PRESSURE_AFTER_SHELL_ACTIONS, DEFAULT_AGENT_LOOP_LIMIT,
    DEFAULT_AGENT_ROUTING, DEFAULT_MAX_ROOT_SUBAGENTS, DEFAULT_MAX_SUBAGENT_DEPTH,
    DEFAULT_MAX_SUBAGENT_PANES_PER_WINDOW, DEFAULT_MAX_SUBAGENTS_PER_SUBAGENT,
    DEFAULT_PTY_READ_LIMIT_BYTES, DEFAULT_SUBAGENT_WAIT_POLICY,
};
pub(in crate::runtime) use metrics::{ProgramOwnedPaneTitle, RuntimeMetricsSnapshot};
pub use session::RuntimeSessionService;
