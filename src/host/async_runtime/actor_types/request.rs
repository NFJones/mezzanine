//! The complete command envelope accepted by the async runtime actor.

use super::{
    AgentId, AsyncControlInputResult, AsyncMessageFanout, AsyncMessageInputResult,
    AsyncRenderedClientFlush, AsyncRenderedClientFrame, AsyncRuntimeActorMetrics,
    AttachedClientStepApplication, AttachedTerminalClientStepPlan, ClientId, ClientStatusLine,
    ClientViewRole, ControlConnectionState, DeliveryCursor, FanoutBatch, MessageConnection,
    PaneProcess, PaneResizeUpdate, RenderedClientView, Result, RuntimeAgentCompactionDispatch,
    RuntimeAgentProviderDispatch, RuntimeAgentProviderTask, RuntimeAgentRememberDispatch,
    RuntimeEventBatch, RuntimeEventConnectionTable, RuntimeEventIngressReport, RuntimeEventWakeup,
    RuntimeLifecycleState, RuntimeSideEffect, RuntimeSnapshotControlAsyncOutcome,
    RuntimeSnapshotControlAsyncWork, Size, SnapshotRepository, TerminalClientLoopConfig, oneshot,
};

/// Carries Async Runtime Request state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(in crate::host::async_runtime) enum AsyncRuntimeRequest {
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
    TakeRunningPaneProcessesForAdapter {
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
