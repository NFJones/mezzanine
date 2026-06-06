//! Async Runtime Actor Types implementation.
//!
//! This module owns the async runtime actor types boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::AsyncRuntimeActorMetrics;
use super::{
    AgentId, AsRawFd, AsyncControlInputResult, AsyncMessageFanout, AsyncMessageInputResult,
    AsyncRuntimeControlConnectionConfig, AsyncRuntimeMessageConnectionConfig,
    AsyncRuntimeSessionHandle, AsyncWriteExt, AttachedClientStepApplication,
    AttachedTerminalClientStepPlan, AttachedTerminalFdReadiness, AttachedTerminalOutputModes,
    ClientEvent, ClientId, ClientStatusLine, ClientViewRole, ControlConnectionState,
    DeliveryCursor, FanoutBatch, Framed, JoinSet, MessageConnection, MezError, PaneProcess,
    ProtocolFrameCodec, RenderedClientView, Result, RuntimeAgentCompactionDispatch,
    RuntimeAgentProviderDispatch, RuntimeAgentProviderTask, RuntimeAgentRememberDispatch,
    RuntimeEvent, RuntimeEventBatch, RuntimeEventConnectionTable, RuntimeEventIngressReport,
    RuntimeEventWakeup, RuntimeLifecycleState, RuntimeSideEffect,
    RuntimeSnapshotControlAsyncOutcome, RuntimeSnapshotControlAsyncWork, Size, StreamExt,
    TerminalClientLoopConfig, TerminalStyleSpan, UnixListener, UnixStream,
    authorize_unix_peer_raw_fd, encode_frame, oneshot, plan_attached_terminal_client_step,
};
use crate::runtime::PaneResizeUpdate;
use crate::snapshot::SnapshotRepository;

// Async runtime actor request and report types.

/// Carries Async Rendered Client Frame state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct AsyncRenderedClientFrame {
    /// Stores the config value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub config: TerminalClientLoopConfig,
    /// Stores the view value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub view: Option<RenderedClientView>,
}

/// Carries Async Rendered Client Flush state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncRenderedClientFlush {
    /// Stores the client id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub client_id: ClientId,
    /// Stores the lines value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub lines: Vec<String>,
    /// Stores the line style spans value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Stores the modes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub modes: AttachedTerminalOutputModes,
}

/// Carries Async Runtime Request state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(super) enum AsyncRuntimeRequest {
    /// Represents the Lifecycle State case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    LifecycleState {
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<RuntimeLifecycleState>,
    },
    /// Represents the Metrics case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Metrics {
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<AsyncRuntimeActorMetrics>,
    },
    /// Represents the Render Client View case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    RenderClientView {
        /// Stores the role value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        role: ClientViewRole,
        /// Stores the client size value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        client_size: Size,
        /// Stores the config value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        config: TerminalClientLoopConfig,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Option<RenderedClientView>>>,
    },
    /// Represents the Render Client Frame case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    RenderClientFrame {
        /// Stores the role value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        role: ClientViewRole,
        /// Stores the client size value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        client_size: Size,
        /// Stores the config value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        config: TerminalClientLoopConfig,
        /// Stores the render value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        render: bool,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<AsyncRenderedClientFrame>>,
    },
    /// Represents the Render Client Side Effect case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    RenderClientSideEffect {
        /// Stores the client id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        client_id: ClientId,
        /// Stores the config value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        config: TerminalClientLoopConfig,
        /// Stores the status value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        status: Option<ClientStatusLine>,
        /// Stores the cursor blink elapsed ms value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        cursor_blink_elapsed_ms: u64,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Option<AsyncRenderedClientFlush>>>,
    },
    /// Represents the Ensure Client Render Timers case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    EnsureClientRenderTimers {
        /// Stores the client id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        client_id: ClientId,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<usize>>,
    },
    /// Represents the Terminal Client Loop Config case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    TerminalClientLoopConfig {
        /// Stores the config value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        config: TerminalClientLoopConfig,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<TerminalClientLoopConfig>>,
    },
    /// Represents the Handle Control Input case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    HandleControlInput {
        /// Stores the input value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        input: Vec<u8>,
        /// Stores the max content length value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        max_content_length: usize,
        /// Stores the connection value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        connection: ControlConnectionState,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<AsyncControlInputResult>>,
    },
    /// Represents the Handle Control Input With Snapshots case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    HandleControlInputWithSnapshots {
        /// Stores the input value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        input: Vec<u8>,
        /// Stores the max content length value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        max_content_length: usize,
        /// Stores the connection value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        connection: ControlConnectionState,
        /// Stores the snapshots value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        snapshots: SnapshotRepository,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<AsyncControlInputResult>>,
    },
    /// Represents the Complete Snapshot Control Input case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    CompleteSnapshotControlInput {
        /// Stores the consumed value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        consumed: usize,
        /// Stores the connection value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        connection: ControlConnectionState,
        /// Stores the work value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        work: RuntimeSnapshotControlAsyncWork,
        /// Stores the outcome value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        outcome: Box<RuntimeSnapshotControlAsyncOutcome>,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<AsyncControlInputResult>>,
    },
    /// Represents the Handle Message Input case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    HandleMessageInput {
        /// Stores the input value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        input: Vec<u8>,
        /// Stores the max content length value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        max_content_length: usize,
        /// Stores the connection value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        connection: MessageConnection,
        /// Stores the now ms value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        now_ms: u64,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<AsyncMessageInputResult>>,
    },
    /// Represents the Message Fanout Ready For case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    MessageFanoutReadyFor {
        /// Stores the recipient value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        recipient: AgentId,
        /// Stores the now ms value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        now_ms: u64,
        /// Stores the limit value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        limit: usize,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Option<AsyncMessageFanout>>>,
    },
    /// Represents the Acknowledge Message Fanout case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    AcknowledgeMessageFanout {
        /// Stores the batch value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        batch: FanoutBatch,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<DeliveryCursor>>,
    },
    /// Represents the Event Wakeups case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    EventWakeups {
        /// Stores the connections value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        connections: RuntimeEventConnectionTable,
        /// Stores the limit per connection value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        limit_per_connection: usize,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Vec<RuntimeEventWakeup>>>,
    },
    /// Represents the Apply Attached Terminal Step case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ApplyAttachedTerminalStep {
        /// Stores the primary client id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        primary_client_id: ClientId,
        /// Stores the step value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        step: AttachedTerminalClientStepPlan,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<AttachedClientStepApplication>>,
    },
    /// Represents the Apply Attached Terminal Step Inline Pane Io case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ApplyAttachedTerminalStepInlinePaneIo {
        /// Stores the primary client id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        primary_client_id: ClientId,
        /// Stores the step value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        step: AttachedTerminalClientStepPlan,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<AttachedClientStepApplication>>,
    },
    /// Represents the Resize Attached Primary Terminal case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ResizeAttachedPrimaryTerminal {
        /// Stores the primary client id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        primary_client_id: ClientId,
        /// Stores the size value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        size: Size,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Vec<PaneResizeUpdate>>>,
    },
    /// Represents the Execute Terminal Command case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ExecuteTerminalCommand {
        /// Stores the primary client id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        primary_client_id: ClientId,
        /// Stores the input value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        input: String,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<String>>,
    },
    /// Represents the Refresh Provider Info case for this enumeration.
    ///
    /// Callers use this variant to refresh cached provider metadata without
    /// routing through a terminal-command string.
    RefreshProviderInfo {
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<String>>,
    },
    /// Represents the Show Primary Display Overlay case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ShowPrimaryDisplayOverlay {
        /// Stores the lines value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        lines: Vec<String>,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<()>>,
    },
    /// Represents the Show Primary Error Overlay case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ShowPrimaryErrorOverlay {
        /// Stores the lines value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        lines: Vec<String>,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<()>>,
    },
    /// Represents the Execute Agent Shell Command case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ExecuteAgentShellCommand {
        /// Stores the primary client id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        primary_client_id: ClientId,
        /// Stores the input value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        input: String,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<String>>,
    },
    /// Represents the Pending Agent Provider Tasks case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    PendingAgentProviderTasks {
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Vec<RuntimeAgentProviderTask>>>,
    },
    /// Checks whether a turn is still live before a provider worker keeps
    /// allocating provider response state for it.
    AgentTurnIsRunning {
        /// Stores the turn id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        turn_id: String,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<bool>>,
    },
    /// Represents the Queue Provider Poll Timer If Needed case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    QueueProviderPollTimerIfNeeded {
        /// Stores the generation value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        generation: u64,
        /// Stores the delay ms value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        delay_ms: u64,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<bool>>,
    },
    /// Represents the Claim Configured Agent Provider Task case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    ClaimConfiguredAgentProviderTask {
        /// Stores the agent id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        agent_id: AgentId,
        /// Stores the turn id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        turn_id: String,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Option<RuntimeAgentProviderDispatch>>>,
    },
    /// Claims a queued model-backed conversation compaction task.
    ClaimAgentCompactionTask {
        /// Pane whose queued compaction should be claimed.
        pane_id: String,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Option<RuntimeAgentCompactionDispatch>>>,
    },
    /// Claims a queued model-backed durable memory task.
    ClaimAgentRememberTask {
        /// Pane whose queued memory generation should be claimed.
        pane_id: String,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Option<RuntimeAgentRememberDispatch>>>,
    },
    /// Represents the Submit Runtime Events case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    SubmitRuntimeEvents {
        /// Stores the batch value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        batch: RuntimeEventBatch,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<RuntimeEventIngressReport>>,
    },
    /// Represents the Drain Runtime Side Effects case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DrainRuntimeSideEffects {
        /// Stores the limit value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        limit: usize,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Vec<RuntimeSideEffect>>>,
    },
    /// Represents the Queue Runtime Side Effects case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    QueueRuntimeSideEffects {
        /// Stores the side effects value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        side_effects: Vec<RuntimeSideEffect>,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<usize>>,
    },
    /// Represents the Drain Agent Provider Dispatch Side Effects case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DrainAgentProviderDispatchSideEffects {
        /// Stores the limit value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        limit: usize,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Vec<RuntimeSideEffect>>>,
    },
    /// Represents the Drain Render Side Effects case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DrainRenderSideEffects {
        /// Stores the limit value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        limit: usize,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Vec<RuntimeSideEffect>>>,
    },
    /// Represents the Drain Render Side Effects For Client case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DrainRenderSideEffectsForClient {
        /// Stores the client id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        client_id: ClientId,
        /// Stores the limit value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        limit: usize,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Vec<RuntimeSideEffect>>>,
    },
    /// Represents the Drain Client Output Flush Side Effects case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DrainClientOutputFlushSideEffects {
        /// Stores the client id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        client_id: Option<ClientId>,
        /// Stores the limit value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        limit: usize,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Vec<RuntimeSideEffect>>>,
    },
    /// Represents the Drain Timer Side Effects case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DrainTimerSideEffects {
        /// Stores the limit value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        limit: usize,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Vec<RuntimeSideEffect>>>,
    },
    /// Represents the Drain Persistence Side Effects case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DrainPersistenceSideEffects {
        /// Stores the limit value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        limit: usize,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Vec<RuntimeSideEffect>>>,
    },
    /// Represents the Drain Hook Side Effects case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DrainHookSideEffects {
        /// Stores the limit value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        limit: usize,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Vec<RuntimeSideEffect>>>,
    },
    /// Represents the Drain Pane Io Side Effects case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    DrainPaneIoSideEffects {
        /// Stores the pane id value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        pane_id: String,
        /// Stores the limit value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        limit: usize,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Vec<RuntimeSideEffect>>>,
    },
    /// Represents the Take Running Pane Processes For Async Owner case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    TakeRunningPaneProcessesForAsyncOwner {
        /// Stores the limit value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        limit: usize,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Vec<(String, PaneProcess)>>>,
    },
    /// Represents the Shutdown case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Shutdown {
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<RuntimeLifecycleState>,
    },
}

/// Runs the serve async runtime control connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn serve_async_runtime_control_connection(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut ControlConnectionState,
    config: AsyncRuntimeControlConnectionConfig,
) -> Result<usize> {
    serve_async_runtime_control_connection_with_snapshots(stream, handle, connection, config, None)
        .await
}

/// Runs the serve async runtime control connection with snapshots operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn serve_async_runtime_control_connection_with_snapshots(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut ControlConnectionState,
    config: AsyncRuntimeControlConnectionConfig,
    snapshots: Option<&SnapshotRepository>,
) -> Result<usize> {
    authorize_unix_peer_raw_fd(stream.as_raw_fd(), config.owner_uid)?;
    let mut framed = Framed::new(stream, ProtocolFrameCodec::new(config.max_content_length)?);
    let Some(frame) = framed.next().await else {
        return Ok(0);
    };
    let input = encode_frame(&frame?);
    let result = handle_control_input_with_optional_snapshots(
        handle,
        input,
        config.max_content_length,
        connection,
        snapshots,
    )
    .await?;
    *connection = result.connection;
    framed.get_mut().write_all(&result.output).await?;
    framed.get_mut().flush().await?;
    Ok(result.consumed)
}

/// Runs the serve async runtime control connection loop operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn serve_async_runtime_control_connection_loop<F>(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut ControlConnectionState,
    config: AsyncRuntimeControlConnectionConfig,
    should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    serve_async_runtime_control_connection_loop_with_snapshots(
        stream,
        handle,
        connection,
        config,
        None,
        should_stop,
    )
    .await
}

/// Runs the serve async runtime control connection loop with snapshots operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn serve_async_runtime_control_connection_loop_with_snapshots<F>(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut ControlConnectionState,
    config: AsyncRuntimeControlConnectionConfig,
    snapshots: Option<&SnapshotRepository>,
    mut should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    authorize_unix_peer_raw_fd(stream.as_raw_fd(), config.owner_uid)?;
    let mut framed = Framed::new(stream, ProtocolFrameCodec::new(config.max_content_length)?);
    let mut served = 0u64;
    let mut lifecycle = handle.lifecycle_state_watcher();
    loop {
        let state = *lifecycle.borrow();
        if should_stop(served, state) {
            return Ok(served);
        }

        tokio::select! {
            frame = framed.next() => {
                let Some(frame) = frame else {
                    submit_control_connection_disconnect_event(handle, connection).await?;
                    return Ok(served);
                };
                let input = encode_frame(&frame?);
                let result = handle_control_input_with_optional_snapshots(
                    handle,
                    input,
                    config.max_content_length,
                    connection,
                    snapshots,
                )
                .await?;
                *connection = result.connection;
                framed.get_mut().write_all(&result.output).await?;
                framed.get_mut().flush().await?;
                served = served.saturating_add(1);
            }
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    return Ok(served);
                }
            }
        }
    }
}

/// Submits a best-effort client disconnect event when a control connection EOFs.
///
/// The async control socket owns the live connection state, so it is the only
/// layer that can reliably convert a foreground attach fd hangup into the
/// runtime event that clears stale attached-primary session state. Request-local
/// control clients do not opt into this behavior because their EOF is just the
/// end of one RPC exchange.
async fn submit_control_connection_disconnect_event(
    handle: &AsyncRuntimeSessionHandle,
    connection: &ControlConnectionState,
) -> Result<()> {
    if !connection.detach_primary_on_disconnect() {
        return Ok(());
    }
    let Some(client_id) = connection.caller_client_id().cloned() else {
        return Ok(());
    };
    let mut batch = RuntimeEventBatch::new();
    batch.push(RuntimeEvent::Client(ClientEvent::Disconnected {
        client_id,
        reason: "control socket EOF".to_string(),
    }));
    handle.submit_runtime_events(batch).await?;
    Ok(())
}

/// Runs the serve async runtime control listener operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn serve_async_runtime_control_listener<F>(
    listener: &UnixListener,
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncRuntimeControlConnectionConfig,
    should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    serve_async_runtime_control_listener_with_snapshots(listener, handle, config, None, should_stop)
        .await
}

/// Runs the serve async runtime control listener with snapshots operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn serve_async_runtime_control_listener_with_snapshots<F>(
    listener: &UnixListener,
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncRuntimeControlConnectionConfig,
    snapshots: Option<SnapshotRepository>,
    mut should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    let mut accepted = 0u64;
    let mut tasks = JoinSet::new();
    let mut lifecycle = handle.lifecycle_state_watcher();
    loop {
        let state = *lifecycle.borrow();
        if should_stop(accepted, state) {
            break;
        }

        let (mut stream, _addr) = tokio::select! {
            accepted = listener.accept() => accepted?,
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    break;
                }
                continue;
            }
        };
        let connection_handle = handle.clone();
        let connection_snapshots = snapshots.clone();
        tasks.spawn(async move {
            let mut connection = ControlConnectionState::new(true, true);
            serve_async_runtime_control_connection_loop_with_snapshots(
                &mut stream,
                &connection_handle,
                &mut connection,
                config,
                connection_snapshots.as_ref(),
                |_, state| {
                    matches!(
                        state,
                        RuntimeLifecycleState::Stopping
                            | RuntimeLifecycleState::Killed
                            | RuntimeLifecycleState::Failed
                    )
                },
            )
            .await
        });
        accepted = accepted.saturating_add(1);
    }

    while let Some(joined) = tasks.join_next().await {
        joined.map_err(|error| {
            MezError::invalid_state(format!("async control connection task failed: {error}"))
        })??;
    }

    Ok(accepted)
}

/// Runs the handle control input with optional snapshots operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn handle_control_input_with_optional_snapshots(
    handle: &AsyncRuntimeSessionHandle,
    input: Vec<u8>,
    max_content_length: usize,
    connection: &ControlConnectionState,
    snapshots: Option<&SnapshotRepository>,
) -> Result<AsyncControlInputResult> {
    match snapshots {
        Some(snapshots) => {
            handle
                .handle_control_input_for_connection_with_snapshots(
                    input,
                    max_content_length,
                    connection.clone(),
                    snapshots.clone(),
                )
                .await
        }
        None => {
            handle
                .handle_control_input_for_connection(input, max_content_length, connection.clone())
                .await
        }
    }
}

/// Runs the serve async runtime message connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn serve_async_runtime_message_connection(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut MessageConnection,
    max_content_length: usize,
    now_ms: u64,
    fanout_limit: usize,
) -> Result<usize> {
    if max_content_length == 0 {
        return Err(MezError::invalid_args(
            "async message max content length must be greater than zero",
        ));
    }

    let mut framed = Framed::new(stream, ProtocolFrameCodec::new(max_content_length)?);
    let Some(frame) = framed.next().await else {
        return Ok(0);
    };
    let input = encode_frame(&frame?);
    let result = handle
        .handle_message_input(input, max_content_length, connection.clone(), now_ms)
        .await?;
    *connection = result.connection;
    framed.get_mut().write_all(&result.output).await?;

    if let Some(agent_id) = connection.agent_id.clone()
        && let Some(fanout) = handle
            .message_fanout_ready_for(agent_id, now_ms, fanout_limit)
            .await?
    {
        framed.get_mut().write_all(&fanout.frame).await?;
        handle.acknowledge_message_fanout(fanout.batch).await?;
    }

    framed.get_mut().flush().await?;
    Ok(result.consumed)
}

/// Runs the flush async message fanout for connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn flush_async_message_fanout_for_connection(
    framed: &mut Framed<&mut UnixStream, ProtocolFrameCodec>,
    handle: &AsyncRuntimeSessionHandle,
    connection: &MessageConnection,
    now_ms: u64,
    fanout_limit: usize,
) -> Result<usize> {
    let Some(agent_id) = connection.agent_id.clone() else {
        return Ok(0);
    };
    let Some(fanout) = handle
        .message_fanout_ready_for(agent_id, now_ms, fanout_limit)
        .await?
    else {
        return Ok(0);
    };

    framed.get_mut().write_all(&fanout.frame).await?;
    handle.acknowledge_message_fanout(fanout.batch).await?;
    Ok(fanout.messages)
}

/// Runs the serve async runtime message connection loop operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn serve_async_runtime_message_connection_loop<F>(
    stream: &mut UnixStream,
    handle: &AsyncRuntimeSessionHandle,
    connection: &mut MessageConnection,
    config: AsyncRuntimeMessageConnectionConfig,
    mut now_ms: impl FnMut(u64) -> u64,
    mut should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    let mut served = 0u64;
    let mut framed = Framed::new(stream, ProtocolFrameCodec::new(config.max_content_length)?);
    let mut lifecycle = handle.lifecycle_state_watcher();
    loop {
        let state = *lifecycle.borrow();
        if should_stop(served, state) {
            return Ok(served);
        }

        if connection.agent_id.is_some() {
            let now = now_ms(served);
            if flush_async_message_fanout_for_connection(
                &mut framed,
                handle,
                connection,
                now,
                config.fanout_limit,
            )
            .await?
                > 0
            {
                framed.get_mut().flush().await?;
            }
            tokio::select! {
                frame = framed.next() => {
                    let Some(frame) = frame else {
                        return Ok(served);
                    };
                    let input = encode_frame(&frame?);
                    let now = now_ms(served);
                    let result = handle
                        .handle_message_input(input, config.max_content_length, connection.clone(), now)
                        .await?;
                    *connection = result.connection;
                    framed.get_mut().write_all(&result.output).await?;
                    let _ = flush_async_message_fanout_for_connection(
                        &mut framed,
                        handle,
                        connection,
                        now,
                        config.fanout_limit,
                    )
                        .await?;
                    framed.get_mut().flush().await?;
                    served = served.saturating_add(1);
                }
                _ = handle.wait_for_message_delivery() => {
                    let now = now_ms(served);
                    if flush_async_message_fanout_for_connection(
                        &mut framed,
                        handle,
                        connection,
                        now,
                        config.fanout_limit,
                    )
                    .await? > 0
                    {
                        framed.get_mut().flush().await?;
                    }
                }
                changed = lifecycle.changed() => {
                    if changed.is_err() {
                        return Ok(served);
                    }
                }
            }
        } else {
            tokio::select! {
                frame = framed.next() => {
                    let Some(frame) = frame else {
                        return Ok(served);
                    };
                    let input = encode_frame(&frame?);
                    let now = now_ms(served);
                    let result = handle
                        .handle_message_input(input, config.max_content_length, connection.clone(), now)
                        .await?;
                    *connection = result.connection;
                    framed.get_mut().write_all(&result.output).await?;
                    let _ = flush_async_message_fanout_for_connection(
                        &mut framed,
                        handle,
                        connection,
                        now,
                        config.fanout_limit,
                    )
                    .await?;
                    framed.get_mut().flush().await?;
                    served = served.saturating_add(1);
                }
                changed = lifecycle.changed() => {
                    if changed.is_err() {
                        return Ok(served);
                    }
                }
            }
        }
    }
}

/// Runs the serve async runtime message listener operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn serve_async_runtime_message_listener<F>(
    listener: &UnixListener,
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncRuntimeMessageConnectionConfig,
    base_now_ms: u64,
    mut should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    let mut served = 0u64;
    let mut lifecycle = handle.lifecycle_state_watcher();
    loop {
        let state = *lifecycle.borrow();
        if should_stop(served, state) {
            return Ok(served);
        }

        let (mut stream, _addr) = tokio::select! {
            accepted = listener.accept() => accepted?,
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    return Ok(served);
                }
                continue;
            }
        };
        let mut connection = MessageConnection::default();
        serve_async_runtime_message_connection_loop(
            &mut stream,
            handle,
            &mut connection,
            config,
            |request_index| {
                base_now_ms
                    .saturating_add(served)
                    .saturating_add(request_index)
            },
            |_, state| should_stop(served, state),
        )
        .await?;
        served = served.saturating_add(1);
    }
}

/// Runs the serve async runtime message listener concurrent operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn serve_async_runtime_message_listener_concurrent<F>(
    listener: &UnixListener,
    handle: &AsyncRuntimeSessionHandle,
    config: AsyncRuntimeMessageConnectionConfig,
    base_now_ms: u64,
    max_connections: u64,
    mut should_stop: F,
) -> Result<u64>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    if max_connections == 0 {
        return Err(MezError::invalid_args(
            "async message listener max connections must be greater than zero",
        ));
    }

    let mut accepted = 0u64;
    let mut tasks = JoinSet::new();
    let mut lifecycle = handle.lifecycle_state_watcher();
    loop {
        let state = *lifecycle.borrow();
        if accepted >= max_connections || should_stop(accepted, state) {
            break;
        }

        let (mut stream, _addr) = tokio::select! {
            accepted = listener.accept() => accepted?,
            changed = lifecycle.changed() => {
                if changed.is_err() {
                    break;
                }
                continue;
            }
        };
        let connection_handle = handle.clone();
        let connection_now_base = base_now_ms.saturating_add(accepted);
        tasks.spawn(async move {
            let mut connection = MessageConnection::default();
            serve_async_runtime_message_connection_loop(
                &mut stream,
                &connection_handle,
                &mut connection,
                config,
                |request_index| connection_now_base.saturating_add(request_index),
                |_, state| {
                    matches!(
                        state,
                        RuntimeLifecycleState::Stopping
                            | RuntimeLifecycleState::Killed
                            | RuntimeLifecycleState::Failed
                    )
                },
            )
            .await
        });
        accepted = accepted.saturating_add(1);
    }

    while let Some(joined) = tasks.join_next().await {
        joined.map_err(|error| {
            MezError::invalid_state(format!("async message connection task failed: {error}"))
        })??;
    }

    Ok(accepted)
}

/// Runs the plan async attached terminal client step operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn plan_async_attached_terminal_client_step(
    handle: &AsyncRuntimeSessionHandle,
    role: ClientViewRole,
    client_size: Size,
    config: TerminalClientLoopConfig,
    readiness: &[AttachedTerminalFdReadiness],
    input: Option<&[u8]>,
    status: Option<&ClientStatusLine>,
) -> Result<AttachedTerminalClientStepPlan> {
    let frame = handle
        .render_client_frame(role, client_size, config, true)
        .await?;
    plan_attached_terminal_client_step(readiness, input, frame.view.as_ref(), status, &frame.config)
}

/// Carries Async Attached Terminal Step Request state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct AsyncAttachedTerminalStepRequest<'a> {
    /// Stores the primary client id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub primary_client_id: ClientId,
    /// Stores the role value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub role: ClientViewRole,
    /// Stores the client size value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub client_size: Size,
    /// Stores the config value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub config: TerminalClientLoopConfig,
    /// Stores the readiness value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub readiness: &'a [AttachedTerminalFdReadiness],
    /// Stores the input value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub input: Option<&'a [u8]>,
    /// Stores the status value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub status: Option<&'a ClientStatusLine>,
}

/// Runs the plan and apply async attached terminal client step operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub async fn plan_and_apply_async_attached_terminal_client_step(
    handle: &AsyncRuntimeSessionHandle,
    request: AsyncAttachedTerminalStepRequest<'_>,
) -> Result<(
    AttachedTerminalClientStepPlan,
    AttachedClientStepApplication,
)> {
    let plan = plan_async_attached_terminal_client_step(
        handle,
        request.role,
        request.client_size,
        request.config,
        request.readiness,
        request.input,
        request.status,
    )
    .await?;
    let application = handle
        .apply_attached_terminal_step_plan(request.primary_client_id, plan.clone())
        .await?;
    Ok((plan, application))
}
