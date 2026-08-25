//! The complete command envelope accepted by the async runtime actor.

use super::{
    AgentId, AsyncClientRenderToken, AsyncControlInputResult, AsyncMessageFanout,
    AsyncMessageInputResult, AsyncRenderedClientFlush, AsyncRenderedClientFrame,
    AsyncRuntimeActorMetrics, AsyncTerminalClientConfigInput, AsyncTerminalClientConfigSnapshot,
    AttachedClientStepApplication, AttachedTerminalClientStepPlan, ClientId, ClientStatusLine,
    ClientViewRole, ControlConnectionState, DeliveryCursor, FanoutBatch, MessageConnection,
    PaneProcess, PaneResizeUpdate, RenderedClientView, Result, RuntimeAgentCompactionDispatch,
    RuntimeAgentProviderDispatch, RuntimeAgentProviderTask, RuntimeAgentRememberDispatch,
    RuntimeApprovedExternalActionDispatch, RuntimeApprovedExternalActionOutcome, RuntimeEventBatch,
    RuntimeEventIngressReport, RuntimeEventWakeup, RuntimeLifecycleState,
    RuntimeProviderInfoRefreshOutcome, RuntimeSideEffect, RuntimeSnapshotControlAsyncOutcome,
    RuntimeSnapshotControlAsyncWork, Size, SnapshotRepository, TerminalClientLoopConfig, oneshot,
};
use crate::runtime::PaneProcessInstance;
use crate::runtime::RuntimeAgentPromptProviderInfoRefresh;
use crate::runtime::RuntimeNativeShellDispatch;
#[cfg(test)]
use crate::runtime::{PaneInputDispatch, RuntimeEventConnectionTable};
use crate::runtime::{RuntimeAgentProviderPreparationOutcome, RuntimeAgentProviderPreparationWork};
use std::time::Instant;

/// Timestamped command envelope accepted by the serialized runtime actor.
pub(in crate::host::async_runtime) struct AsyncRuntimeRequestEnvelope {
    /// Fixed request family captured without allocating a dynamic label.
    pub(in crate::host::async_runtime) family:
        crate::host::async_runtime::AsyncRuntimeRequestFamily,
    /// Whether this command contributes actor queue and handler observations.
    pub(in crate::host::async_runtime) record_actor_latency: bool,
    /// Monotonic enqueue timestamp used to measure dequeue latency.
    pub(in crate::host::async_runtime) enqueued_at: Instant,
    /// Typed actor command payload.
    pub(in crate::host::async_runtime) request: AsyncRuntimeRequest,
}

impl AsyncRuntimeRequestEnvelope {
    /// Captures one request's fixed family and monotonic enqueue timestamp.
    pub(in crate::host::async_runtime) fn new(request: AsyncRuntimeRequest) -> Self {
        Self {
            family: request.family(),
            record_actor_latency: request.records_actor_latency(),
            enqueued_at: Instant::now(),
            request,
        }
    }
}

/// Carries Async Runtime Request state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(in crate::host::async_runtime) enum AsyncRuntimeRequest {
    /// Installs the shared diagnostics registry for one host-routed Iroh connection.
    SetHostRoutedIrohDiagnostics {
        /// Privacy-safe registry populated by the host-owned path sampler.
        diagnostics: crate::runtime::RuntimeIrohDiagnostics,
        /// Confirms that the serialized runtime mutation completed.
        reply: oneshot::Sender<()>,
    },
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
    /// Captures and persists one actor-consistent host checkpoint.
    CreateHostCheckpoint {
        /// Repository receiving the immutable checkpoint payload and manifest.
        snapshots: SnapshotRepository,
        /// Stable checkpoint identity selected by the host coordinator.
        snapshot_id: String,
        /// Optional operator-facing checkpoint label.
        name: Option<String>,
        /// Receives the committed snapshot metadata after asynchronous I/O.
        reply: oneshot::Sender<Result<crate::storage::snapshot::SnapshotState>>,
    },
    /// Represents the Metrics case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    #[allow(
        dead_code,
        reason = "typed actor request retained for complete host service API"
    )]
    Metrics {
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<AsyncRuntimeActorMetrics>,
    },
    /// Records one fixed worker-phase latency without a dynamic label.
    RecordLatencyPhase {
        /// Fixed phase receiving this observation.
        phase: crate::host::async_runtime::AsyncRuntimeLatencyPhase,
        /// Saturated elapsed milliseconds for the phase.
        elapsed_ms: u64,
    },
    /// Sends bytes directly to one process-owned pane for boundary tests.
    #[cfg(test)]
    WriteInputToPane {
        /// Primary client authorizing the pane input.
        primary_client_id: ClientId,
        /// Pane whose process terminal receives the bytes.
        pane_id: String,
        /// Exact bytes to write to the pane process.
        input: Vec<u8>,
        /// Receives the queued pane-input dispatch metadata.
        reply: oneshot::Sender<Result<PaneInputDispatch>>,
    },
    /// Reads the managed-shell child and restoration lifecycle for boundary tests.
    #[cfg(test)]
    ManagedShellLifecycleState {
        /// Pane whose managed-shell handoff is observed.
        pane_id: String,
        /// Receives child-active, bootstrap-pending, and restoration-pending flags.
        reply: oneshot::Sender<(bool, bool, bool)>,
    },
    /// Reads semantic bootstrap-certification state for boundary tests.
    #[cfg(test)]
    PaneCertificationSnapshot {
        /// Pane whose certification state is observed.
        pane_id: String,
        /// Receives one internally consistent actor-owned state snapshot.
        reply: oneshot::Sender<crate::host::async_runtime::AsyncPaneCertificationSnapshot>,
    },
    /// Reads the retained process presentation during a managed-shell handoff.
    #[cfg(test)]
    ManagedShellProcessScreenText {
        /// Pane whose retained process screen is observed.
        pane_id: String,
        /// Receives the current normal-screen content joined by newlines.
        reply: oneshot::Sender<String>,
    },
    /// Reads whether the managed Zsh adapter completed startup admission.
    #[cfg(test)]
    ManagedZshAdmissionReady {
        /// Pane whose managed Zsh adapter is observed.
        pane_id: String,
        /// Receives whether the current process installed a usable trigger.
        reply: oneshot::Sender<bool>,
    },
    /// Represents the Render Client View case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    #[allow(
        dead_code,
        reason = "typed actor request retained for complete host service API"
    )]
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
        /// Exact attached client whose presentation is rendered.
        client_id: ClientId,
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
        config: AsyncTerminalClientConfigInput,
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
    #[allow(
        dead_code,
        reason = "typed actor request retained for complete host service API"
    )]
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
    /// Resolves or refreshes a shared terminal configuration snapshot.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    TerminalClientLoopConfigSnapshot {
        /// Stores the config value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        config: AsyncTerminalClientConfigInput,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<AsyncTerminalClientConfigSnapshot>>,
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
        /// Encoded responses accumulated from earlier frames in this batch.
        output_prefix: Vec<u8>,
        /// Input bytes consumed by earlier frames in this batch.
        consumed_prefix: usize,
        /// Whether this continuation should record metrics for the full batch.
        record_metrics: bool,
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
        /// Total input bytes consumed through the completed frame.
        consumed_prefix: usize,
        /// Encoded responses accumulated through earlier frames.
        output_prefix: Vec<u8>,
        /// Unprocessed framed input that follows the completed snapshot frame.
        remaining_input: Vec<u8>,
        /// Maximum accepted content length for subsequent frames.
        max_content_length: usize,
        /// Snapshot repository retained for subsequent frame continuations.
        snapshots: SnapshotRepository,
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
    #[cfg(test)]
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
    /// Resolves one live control client current event audience and bounded wakeups.
    EventWakeupsForClient {
        /// Initialized session client bound to the owning control connection.
        caller_client_id: ClientId,
        /// Stable event-stream identity within the owning QUIC connection.
        connection_id: String,
        /// Last event id acknowledged by the stream writer.
        last_delivered_event_id: u64,
        /// Maximum projected events returned in one actor response.
        limit_per_connection: usize,
        /// Authorized visible events or a current-role authorization failure.
        reply: oneshot::Sender<Result<Vec<RuntimeEventWakeup>>>,
    },
    /// Consumes one short-lived Unix event binding credential.
    ConsumeUnixEventBinding {
        /// Raw bearer credential received only from the event initialization frame.
        token: String,
        /// Authenticated Unix peer uid for the event socket.
        peer_uid: u32,
        /// Exact initialized client bound to the consumed credential.
        reply: oneshot::Sender<Result<ClientId>>,
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
        /// Render identity used to fence coordinate-derived actions.
        render_token: Option<AsyncClientRenderToken>,
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
    #[allow(
        dead_code,
        reason = "typed actor request retained for complete host service API"
    )]
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
    /// Applies provider catalog results after worker-owned HTTP completes.
    CompleteProviderInfoRefresh {
        /// Provider catalog worker outcome to install in actor-owned cache state.
        outcome: RuntimeProviderInfoRefreshOutcome,
        /// Original caller waiting for the rendered refresh report.
        reply: oneshot::Sender<Result<String>>,
    },
    /// Represents the Show Primary Display Overlay case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    #[allow(
        dead_code,
        reason = "typed actor request retained for complete host service API"
    )]
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
    #[allow(
        dead_code,
        reason = "typed actor request retained for complete host service API"
    )]
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
    /// Applies MCP discovery before completing an actor-owned `/list-mcp` command.
    CompleteAgentShellMcpDiscovery {
        /// Primary client that submitted the command.
        primary_client_id: ClientId,
        /// Original shell command input.
        input: String,
        /// MCP-only discovery result returned by the worker.
        preparation: RuntimeAgentProviderPreparationOutcome,
        /// Original caller waiting for the command response.
        reply: oneshot::Sender<Result<String>>,
    },
    /// Applies provider catalog results before completing a shell refresh command.
    CompleteAgentShellProviderInfoRefresh {
        /// Primary client that submitted the command.
        primary_client_id: ClientId,
        /// Original shell command input.
        input: String,
        /// Provider catalog worker outcome.
        outcome: RuntimeProviderInfoRefreshOutcome,
        /// Original caller waiting for the command response.
        reply: oneshot::Sender<Result<String>>,
    },
    /// Applies a provider refresh submitted through an interactive prompt.
    CompleteAgentPromptProviderInfoRefresh {
        /// Prompt submission captured before worker-owned provider I/O.
        refresh: RuntimeAgentPromptProviderInfoRefresh,
        /// Provider catalog worker outcome.
        outcome: RuntimeProviderInfoRefreshOutcome,
    },
    /// Represents the Pending Agent Provider Tasks case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    #[allow(
        dead_code,
        reason = "typed actor request retained for complete host service API"
    )]
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
    /// Returns immutable external preparation work before a provider task is claimed.
    ///
    /// Credential refresh and MCP discovery run outside the serialized actor.
    /// The later claim request applies the outcome and revalidates turn state.
    PrepareConfiguredAgentProviderTask {
        /// Returns the immutable preparation inputs extracted from actor state.
        reply: oneshot::Sender<Result<RuntimeAgentProviderPreparationWork>>,
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
        /// External preparation outcome to apply before claiming the turn.
        preparation: RuntimeAgentProviderPreparationOutcome,
        /// Stores the reply value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        reply: oneshot::Sender<Result<Option<RuntimeAgentProviderDispatch>>>,
    },
    /// Claims one approved network or MCP action for execution outside the actor.
    ClaimApprovedExternalAction {
        /// Turn that owns the approved action.
        turn_id: String,
        /// Stable action identity within the turn.
        action_id: String,
        /// Returns immutable worker inputs when the queued action is claimable.
        reply: oneshot::Sender<Result<Option<RuntimeApprovedExternalActionDispatch>>>,
    },
    /// Claims one authorized native shell action for execution outside the actor.
    ClaimNativeShellAction {
        /// Turn that owns the action.
        turn_id: String,
        /// Stable action identity within the turn.
        action_id: String,
        /// Returns immutable worker input when the queued action is claimable.
        reply: oneshot::Sender<Result<Option<RuntimeNativeShellDispatch>>>,
    },
    /// Applies one approved external-action worker result inside the actor.
    CompleteApprovedExternalAction {
        /// Worker result, including any MCP transport returning to actor ownership.
        outcome: RuntimeApprovedExternalActionOutcome,
        /// Reports whether the active turn accepted the result.
        reply: oneshot::Sender<Result<bool>>,
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
    /// Captures immutable cumulative streaming-say projection work.
    TakeStreamingSayProjectionWork {
        /// Pane whose newest cumulative source should be projected.
        pane_id: String,
        /// Provider turn that owns the source-backed presentation.
        turn_id: String,
        /// Returns generation-stamped worker input when projection is ready.
        reply: oneshot::Sender<Result<Option<crate::runtime::RuntimeStreamingSayProjectionWork>>>,
    },
    /// Atomically installs one worker-rendered streaming generation.
    ApplyStreamingSayProjection {
        /// Generation-stamped atomic candidate returned by the worker.
        result: crate::runtime::RuntimeStreamingSayProjectionResult,
        /// Reports whether current actor-owned state accepted the candidate.
        reply: oneshot::Sender<Result<bool>>,
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
    #[allow(
        dead_code,
        reason = "typed actor request retained for complete host service API"
    )]
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
    #[allow(
        dead_code,
        reason = "typed actor request retained for complete host service API"
    )]
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
    /// Drains bounded host-clipboard read requests for the external worker.
    DrainHostClipboardSideEffects {
        /// Maximum effects returned in one request.
        limit: usize,
        /// Completion channel for the drained effects.
        reply: oneshot::Sender<Result<Vec<RuntimeSideEffect>>>,
    },
    /// Drains command-backed status-pill refreshes for the external worker.
    DrainStatusPillSideEffects {
        /// Maximum effects returned in one request.
        limit: usize,
        /// Completion channel for the drained effects.
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
    /// Drains pane I/O effects for one exact adapter-owned process instance.
    DrainPaneProcessIoSideEffects {
        /// Exact process ownership lifetime whose effects may be drained.
        instance: PaneProcessInstance,
        /// Maximum effects returned in one request.
        limit: usize,
        /// Completion channel for the drained effects.
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
        reply: oneshot::Sender<Result<Vec<(PaneProcessInstance, PaneProcess)>>>,
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

impl AsyncRuntimeRequest {
    /// Returns whether this command should observe its own actor latency.
    const fn records_actor_latency(&self) -> bool {
        !matches!(self, Self::Metrics { .. } | Self::RecordLatencyPhase { .. })
    }

    /// Maps every command to one fixed, allocation-free diagnostics family.
    pub(super) const fn family(&self) -> crate::host::async_runtime::AsyncRuntimeRequestFamily {
        use crate::host::async_runtime::AsyncRuntimeRequestFamily as Family;

        match self {
            Self::LifecycleState { .. } | Self::Metrics { .. } | Self::Shutdown { .. } => {
                Family::Lifecycle
            }
            Self::RecordLatencyPhase { phase, .. } => match phase {
                crate::host::async_runtime::AsyncRuntimeLatencyPhase::EventBatchApply
                | crate::host::async_runtime::AsyncRuntimeLatencyPhase::EventReconciliation => {
                    Family::Event
                }
                crate::host::async_runtime::AsyncRuntimeLatencyPhase::RenderComposition
                | crate::host::async_runtime::AsyncRuntimeLatencyPhase::RenderEncoding
                | crate::host::async_runtime::AsyncRuntimeLatencyPhase::OutputFlush => {
                    Family::Render
                }
                crate::host::async_runtime::AsyncRuntimeLatencyPhase::ProviderTtfb
                | crate::host::async_runtime::AsyncRuntimeLatencyPhase::ProviderChunkInterval
                | crate::host::async_runtime::AsyncRuntimeLatencyPhase::ProviderTotal => {
                    Family::Provider
                }
                crate::host::async_runtime::AsyncRuntimeLatencyPhase::PersistenceOperation
                | crate::host::async_runtime::AsyncRuntimeLatencyPhase::PersistenceBatch
                | crate::host::async_runtime::AsyncRuntimeLatencyPhase::SideEffectQueueAge => {
                    Family::SideEffect
                }
            },
            Self::RenderClientView { .. }
            | Self::RenderClientFrame { .. }
            | Self::RenderClientSideEffect { .. }
            | Self::EnsureClientRenderTimers { .. }
            | Self::TerminalClientLoopConfigSnapshot { .. } => Family::Render,
            Self::SetHostRoutedIrohDiagnostics { .. }
            | Self::HandleControlInput { .. }
            | Self::HandleControlInputWithSnapshots { .. }
            | Self::CompleteSnapshotControlInput { .. }
            | Self::CreateHostCheckpoint { .. } => Family::Control,
            #[cfg(test)]
            Self::EventWakeups { .. } => Family::Message,
            Self::HandleMessageInput { .. }
            | Self::MessageFanoutReadyFor { .. }
            | Self::AcknowledgeMessageFanout { .. }
            | Self::EventWakeupsForClient { .. }
            | Self::ConsumeUnixEventBinding { .. } => Family::Message,
            #[cfg(test)]
            Self::WriteInputToPane { .. }
            | Self::ManagedShellLifecycleState { .. }
            | Self::PaneCertificationSnapshot { .. }
            | Self::ManagedShellProcessScreenText { .. }
            | Self::ManagedZshAdmissionReady { .. } => Family::Terminal,
            Self::ApplyAttachedTerminalStep { .. }
            | Self::ResizeAttachedPrimaryTerminal { .. }
            | Self::ExecuteTerminalCommand { .. }
            | Self::ShowPrimaryDisplayOverlay { .. }
            | Self::ShowPrimaryErrorOverlay { .. }
            | Self::TakeRunningPaneProcessesForAdapter { .. } => Family::Terminal,
            Self::RefreshProviderInfo { .. }
            | Self::CompleteProviderInfoRefresh { .. }
            | Self::ExecuteAgentShellCommand { .. }
            | Self::CompleteAgentShellMcpDiscovery { .. }
            | Self::CompleteAgentShellProviderInfoRefresh { .. }
            | Self::CompleteAgentPromptProviderInfoRefresh { .. }
            | Self::PendingAgentProviderTasks { .. }
            | Self::AgentTurnIsRunning { .. }
            | Self::QueueProviderPollTimerIfNeeded { .. }
            | Self::PrepareConfiguredAgentProviderTask { .. }
            | Self::ClaimConfiguredAgentProviderTask { .. }
            | Self::ClaimApprovedExternalAction { .. }
            | Self::ClaimNativeShellAction { .. }
            | Self::CompleteApprovedExternalAction { .. }
            | Self::ClaimAgentCompactionTask { .. }
            | Self::ClaimAgentRememberTask { .. }
            | Self::TakeStreamingSayProjectionWork { .. }
            | Self::ApplyStreamingSayProjection { .. } => Family::Provider,
            Self::SubmitRuntimeEvents { .. } => Family::Event,
            Self::DrainRuntimeSideEffects { .. }
            | Self::QueueRuntimeSideEffects { .. }
            | Self::DrainAgentProviderDispatchSideEffects { .. }
            | Self::DrainRenderSideEffects { .. }
            | Self::DrainRenderSideEffectsForClient { .. }
            | Self::DrainClientOutputFlushSideEffects { .. }
            | Self::DrainTimerSideEffects { .. }
            | Self::DrainPersistenceSideEffects { .. }
            | Self::DrainHookSideEffects { .. }
            | Self::DrainHostClipboardSideEffects { .. }
            | Self::DrainStatusPillSideEffects { .. }
            | Self::DrainPaneIoSideEffects { .. }
            | Self::DrainPaneProcessIoSideEffects { .. } => Family::SideEffect,
        }
    }
}
