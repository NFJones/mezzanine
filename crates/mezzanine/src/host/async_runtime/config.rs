//! Async Runtime Config implementation.
//!
//! This module owns the async runtime config boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AgentId, Arc, AsyncRuntimeRequestEnvelope, ClientId, ControlConnectionState,
    DEFAULT_ASYNC_CONTROL_MAX_CONTENT_LENGTH, DEFAULT_ASYNC_EVENT_LIMIT_PER_CONNECTION,
    DEFAULT_ASYNC_RUNTIME_COMMAND_BUFFER, Duration, FanoutBatch, HashMap, HashSet,
    MessageConnection, MezError, Notify, PaneProcessInstance, Result, RuntimeLifecycleState,
    RuntimeSessionService, RuntimeSideEffect, RuntimeTimerKey, UnixListener, VecDeque,
    current_effective_uid, mpsc, watch,
};
use crate::storage::snapshot::SnapshotRepository;

// Async runtime, daemon, connection, and client configuration.

/// Generation-fenced clipboard-route cleanup emitted synchronously when an
/// event-stream owner is dropped or aborted.
#[derive(Debug)]
pub(super) struct ClientClipboardRouteCleanup {
    pub(super) client_id: ClientId,
    pub(super) generation: u64,
}

/// Exact clipboard-route ownership for one live Iroh event stream.
#[derive(Debug)]
pub(crate) struct ClientClipboardRouteLease {
    pub(super) handle: AsyncRuntimeSessionHandle,
    pub(super) client_id: ClientId,
    pub(super) generation: u64,
    pub(super) armed: bool,
}

/// Carries Async Runtime Actor Config state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncRuntimeActorConfig {
    /// Stores the command buffer value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub command_buffer: usize,
    /// Stores the side effect buffer value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub side_effect_buffer: usize,
}

impl Default for AsyncRuntimeActorConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            command_buffer: DEFAULT_ASYNC_RUNTIME_COMMAND_BUFFER,
            side_effect_buffer: DEFAULT_ASYNC_RUNTIME_COMMAND_BUFFER,
        }
    }
}

/// Snapshot of async runtime actor counters used for migration diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeHistogramBucket {
    /// Inclusive upper bound represented by this bucket.
    pub upper_bound: u64,
    /// Number of observations recorded in this bucket.
    pub count: u64,
}
/// Bounded histogram used by async runtime metrics snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeHistogram {
    /// Number of recorded observations.
    pub observations: u64,
    /// Sum of all recorded values.
    pub sum: u64,
    /// Minimum observed value.
    pub min: Option<u64>,
    /// Maximum observed value.
    pub max: Option<u64>,
    /// Fixed-width buckets that accumulate observations by upper bound.
    pub buckets: Vec<RuntimeHistogramBucket>,
}
impl Default for RuntimeHistogram {
    /// Builds the default bounded histogram buckets used by runtime metrics.
    fn default() -> Self {
        Self {
            observations: 0,
            sum: 0,
            min: None,
            max: None,
            buckets: [
                0,
                1,
                2,
                4,
                8,
                16,
                32,
                64,
                128,
                256,
                512,
                1024,
                4096,
                16384,
                u64::MAX,
            ]
            .into_iter()
            .map(|upper_bound| RuntimeHistogramBucket {
                upper_bound,
                count: 0,
            })
            .collect(),
        }
    }
}
impl RuntimeHistogram {
    /// Records one observation in the histogram using saturating arithmetic.
    pub fn record(&mut self, value: u64) {
        self.observations = self.observations.saturating_add(1);
        self.sum = self.sum.saturating_add(value);
        self.min = Some(self.min.map_or(value, |current| current.min(value)));
        self.max = Some(self.max.map_or(value, |current| current.max(value)));
        if let Some(bucket) = self
            .buckets
            .iter_mut()
            .find(|bucket| value <= bucket.upper_bound)
        {
            bucket.count = bucket.count.saturating_add(1);
        }
    }
}
/// Fixed, allocation-free actor request families used for latency attribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum AsyncRuntimeRequestFamily {
    /// Lifecycle probes, metrics snapshots, and shutdown.
    Lifecycle,
    /// Client view, frame, and render-timer work.
    Render,
    /// Framed control dispatch and asynchronous control settlement.
    Control,
    /// Message ingress, fanout, acknowledgements, and event wakeups.
    Message,
    /// Attached-terminal mutation and terminal command work.
    Terminal,
    /// Provider, approval, compaction, and durable-memory work.
    Provider,
    /// Typed runtime event batch application.
    Event,
    /// Runtime side-effect queue and drain work.
    SideEffect,
}

impl AsyncRuntimeRequestFamily {
    /// Every request family in stable diagnostics order.
    pub const ALL: [Self; 8] = [
        Self::Lifecycle,
        Self::Render,
        Self::Control,
        Self::Message,
        Self::Terminal,
        Self::Provider,
        Self::Event,
        Self::SideEffect,
    ];

    /// Returns this fixed family index without allocating a label.
    const fn index(self) -> usize {
        self as usize
    }

    /// Returns the stable diagnostics label for this family.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Lifecycle => "lifecycle",
            Self::Render => "render",
            Self::Control => "control",
            Self::Message => "message",
            Self::Terminal => "terminal",
            Self::Provider => "provider",
            Self::Event => "event",
            Self::SideEffect => "side_effect",
        }
    }
}

/// Queue-wait and handler-duration histograms for one actor request family.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AsyncRuntimeRequestLatency {
    /// Milliseconds from enqueue until the actor starts handling the request.
    pub queue_wait_ms: RuntimeHistogram,
    /// Milliseconds spent handling the request on the serialized actor.
    pub handler_duration_ms: RuntimeHistogram,
}

/// Fixed runtime phases used for bounded latency attribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum AsyncRuntimeLatencyPhase {
    /// Applying one prioritized runtime event batch.
    EventBatchApply,
    /// Once-per-batch runtime reconciliation.
    EventReconciliation,
    /// Composing a rendered client view.
    RenderComposition,
    /// Encoding a composed view into styled presentation lines.
    RenderEncoding,
    /// Time until the first decoded provider progress chunk.
    ProviderTtfb,
    /// Time between decoded provider progress chunks.
    ProviderChunkInterval,
    /// Total provider worker execution time.
    ProviderTotal,
    /// One persistence operation or compatible audit batch.
    PersistenceOperation,
    /// One drained persistence worker batch.
    PersistenceBatch,
    /// One bounded attached-terminal output flush attempt.
    OutputFlush,
    /// Age of the current continuously non-empty side-effect queue generation.
    SideEffectQueueAge,
}

impl AsyncRuntimeLatencyPhase {
    /// Every phase in stable diagnostics order.
    pub const ALL: [Self; 11] = [
        Self::EventBatchApply,
        Self::EventReconciliation,
        Self::RenderComposition,
        Self::RenderEncoding,
        Self::ProviderTtfb,
        Self::ProviderChunkInterval,
        Self::ProviderTotal,
        Self::PersistenceOperation,
        Self::PersistenceBatch,
        Self::OutputFlush,
        Self::SideEffectQueueAge,
    ];

    /// Returns this fixed phase index without allocating a label.
    const fn index(self) -> usize {
        self as usize
    }

    /// Returns the stable diagnostics label for this phase.
    pub const fn name(self) -> &'static str {
        match self {
            Self::EventBatchApply => "event_batch_apply_ms",
            Self::EventReconciliation => "event_reconciliation_ms",
            Self::RenderComposition => "render_composition_ms",
            Self::RenderEncoding => "render_encoding_ms",
            Self::ProviderTtfb => "provider_ttfb_ms",
            Self::ProviderChunkInterval => "provider_chunk_interval_ms",
            Self::ProviderTotal => "provider_total_ms",
            Self::PersistenceOperation => "persistence_operation_ms",
            Self::PersistenceBatch => "persistence_batch_ms",
            Self::OutputFlush => "output_flush_ms",
            Self::SideEffectQueueAge => "side_effect_queue_age_ms",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AsyncRuntimeActorMetrics {
    /// Number of actor requests processed in serialized order.
    pub commands_processed: u64,
    /// Number of direct actor requests for a rendered client view.
    pub render_client_view_requests: u64,
    /// Number of direct actor frame requests that included a rendered view.
    pub render_client_frame_requests: u64,
    /// Number of `terminal/step` control requests observed by the actor.
    pub terminal_step_control_requests: u64,
    /// Number of `terminal/view` control requests observed by the actor.
    pub terminal_view_control_requests: u64,
    /// Number of typed runtime event batches accepted by the actor.
    pub runtime_event_batches: u64,
    /// Number of typed runtime events accepted by the actor.
    pub runtime_events_accepted: u64,
    /// Number of typed runtime events applied to mutable runtime state.
    pub runtime_events_applied: u64,
    /// Number of once-per-batch global reconciliation passes performed.
    pub runtime_event_reconciliation_passes: u64,
    /// Histogram of accepted event counts per runtime event batch.
    pub runtime_event_batch_sizes: RuntimeHistogram,
    /// Number of runtime side effects queued by event application.
    pub runtime_side_effects_queued: u64,
    /// Number of runtime side effects drained by supervised workers.
    pub runtime_side_effects_drained: u64,
    /// Histogram of queued side-effect counts per enqueue pass.
    pub runtime_side_effect_enqueue_sizes: RuntimeHistogram,
    /// Histogram of drained side-effect counts per drain pass.
    pub runtime_side_effect_drain_sizes: RuntimeHistogram,
    /// Number of pane output chunks applied through typed runtime events.
    pub pane_output_chunks: u64,
    /// Number of pane output bytes applied through typed runtime events.
    pub pane_output_bytes: u64,
    /// Histogram of pane output chunk sizes in bytes.
    pub pane_output_chunk_bytes: RuntimeHistogram,
    /// Number of redundant render invalidations merged by render side-effect drains.
    pub render_invalidations_coalesced: u64,
    /// Number of runtime timer schedule side effects queued through the actor.
    pub runtime_timer_schedules_queued: u64,
    /// Number of runtime timer cancellation side effects queued through the actor.
    pub runtime_timer_cancellations_queued: u64,
    /// Number of generation-checked runtime timer events ignored as stale.
    pub runtime_timer_events_ignored: u64,
    /// Current side-effect queue depth.
    pub side_effect_queue_depth: usize,
    /// Maximum side-effect queue depth observed since actor startup.
    pub side_effect_queue_high_water: usize,
    /// Histogram of sampled side-effect queue depths.
    pub side_effect_queue_depth_samples: RuntimeHistogram,
    /// Message-delivery notifications emitted by actor mutations.
    pub message_delivery_notifications: u64,
    /// Event-delivery notifications emitted by actor mutations.
    pub event_delivery_notifications: u64,
    /// Side-effect-delivery notifications emitted by actor mutations.
    pub side_effect_delivery_notifications: u64,
    /// Lifecycle-state notifications emitted by actor mutations.
    pub lifecycle_state_notifications: u64,
    /// Heap-owned fixed request-family latency histograms in stable enum order.
    ///
    /// Keeping this cold diagnostics payload behind one pointer prevents actor
    /// and daemon construction futures from inheriting the full histogram
    /// storage in their stack frames.
    request_latencies: Box<[AsyncRuntimeRequestLatency; 8]>,
    /// Heap-owned fixed runtime-phase latency histograms in stable enum order.
    ///
    /// Phase storage follows the same layout rule as request-family storage so
    /// cloning or moving actor metrics does not inflate async worker frames.
    phase_latencies: Box<[RuntimeHistogram; 11]>,
}

impl AsyncRuntimeActorMetrics {
    /// Returns latency histograms for one fixed request family.
    pub fn request_latency(
        &self,
        family: AsyncRuntimeRequestFamily,
    ) -> &AsyncRuntimeRequestLatency {
        &self.request_latencies[family.index()]
    }

    /// Records one actor queue-wait and handler-duration observation.
    pub(crate) fn record_request_latency(
        &mut self,
        family: AsyncRuntimeRequestFamily,
        queue_wait_ms: u64,
        handler_duration_ms: u64,
    ) {
        let latency = &mut self.request_latencies[family.index()];
        latency.queue_wait_ms.record(queue_wait_ms);
        latency.handler_duration_ms.record(handler_duration_ms);
    }

    /// Returns the latency histogram for one fixed runtime phase.
    pub fn phase_latency(&self, phase: AsyncRuntimeLatencyPhase) -> &RuntimeHistogram {
        &self.phase_latencies[phase.index()]
    }

    /// Records one fixed runtime-phase latency observation.
    pub(crate) fn record_phase_latency(
        &mut self,
        phase: AsyncRuntimeLatencyPhase,
        elapsed_ms: u64,
    ) {
        self.phase_latencies[phase.index()].record(elapsed_ms);
    }

    /// Clears latency histograms while preserving counters and volume metrics.
    #[cfg(test)]
    pub fn reset_latency_histograms(&mut self) {
        *self.request_latencies = Default::default();
        *self.phase_latencies = Default::default();
    }
}

/// Adapter-owned desired and active runtime timer state.
///
/// The tracker centralizes timer generations without moving domain transition
/// decisions out of `RuntimeSessionService`. Tokio workers still execute the
/// emitted schedule and cancellation effects.
#[derive(Debug, Default)]
pub(super) struct RuntimeTimerTracker {
    pub(super) shell_transactions: HashSet<RuntimeTimerKey>,
    pub(super) resize_debounce: HashSet<RuntimeTimerKey>,
    pub(super) cursor_blink: HashMap<String, RuntimeTimerKey>,
    pub(super) status_refresh: HashMap<String, RuntimeTimerKey>,
    pub(super) provider_poll: Option<RuntimeTimerKey>,
    pub(super) provider_retry: HashMap<String, RuntimeTimerKey>,
    pub(super) provider_claim: HashMap<String, RuntimeTimerKey>,
    pub(super) next_provider_claim_generation: u64,
    pub(super) pane_pipe_health: HashMap<String, RuntimeTimerKey>,
    pub(super) synchronized_output: HashMap<String, RuntimeTimerKey>,
    pub(super) next_pane_pipe_health_generation: u64,
    pub(super) idle_cleanup: Option<RuntimeTimerKey>,
    pub(super) saved_session_retention: Option<RuntimeTimerKey>,
}

/// Carries Async Runtime Session Actor state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
pub struct AsyncRuntimeSessionActor {
    /// Stores the service value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) service: RuntimeSessionService,
    /// Stores the sender value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) sender: mpsc::Sender<AsyncRuntimeRequestEnvelope>,
    /// Stores the receiver value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) receiver: mpsc::Receiver<AsyncRuntimeRequestEnvelope>,
    /// Pending transient clipboard write keyed by the exact live Iroh primary.
    pub(super) client_clipboard_routes:
        HashMap<ClientId, Option<crate::runtime::ClientClipboardWrite>>,
    /// Current event-stream generation owning each exact clipboard route.
    pub(super) client_clipboard_route_generations: HashMap<ClientId, u64>,
    /// Next generation assigned when an event stream replaces a route.
    pub(super) next_client_clipboard_route_generation: u64,
    /// Last route-local clipboard sequence assigned to each live Iroh primary.
    pub(super) client_clipboard_sequences: HashMap<ClientId, u64>,
    /// Cancellation-safe route cleanup sent synchronously by event-task Drop.
    pub(super) client_clipboard_route_cleanup_rx:
        mpsc::UnboundedReceiver<ClientClipboardRouteCleanup>,
    /// Stores the message delivery notify value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) message_delivery_notify: Arc<Notify>,
    /// Stores the event delivery notify value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) event_delivery_notify: Arc<Notify>,
    /// Publishes a durable revision for every event-delivery notification.
    pub(super) event_delivery_revision_tx: watch::Sender<u64>,
    /// Stores the side effect delivery notify value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) side_effect_delivery_notify: Arc<Notify>,
    /// Stores the side effect delivery revision tx value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) side_effect_delivery_tx: watch::Sender<u64>,
    /// Stores the lifecycle state tx value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) lifecycle_state_tx: watch::Sender<RuntimeLifecycleState>,
    /// Current generation of actor-resolved terminal configuration snapshots.
    pub(super) terminal_config_generation: u64,
    /// Publishes terminal configuration generation changes to attached clients.
    pub(super) terminal_config_generation_tx: watch::Sender<u64>,
    /// Stores the side effects value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) side_effects: VecDeque<RuntimeSideEffect>,
    /// Start of the current continuously non-empty side-effect queue generation.
    pub(super) side_effect_queue_nonempty_since: Option<std::time::Instant>,
    /// Active transaction input leases keyed by exact pane process generation.
    pub(super) pane_input_leases: HashMap<PaneProcessInstance, String>,
    /// Adapter-owned timer scheduling and stale-generation state.
    pub(super) timers: RuntimeTimerTracker,
    /// Stores the side effect buffer value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) side_effect_buffer: usize,
    /// Stores the commands processed value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) commands_processed: u64,
    /// Stores the metrics value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) metrics: AsyncRuntimeActorMetrics,
}

/// Carries Async Runtime Session Handle state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct AsyncRuntimeSessionHandle {
    /// Stores the sender value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) sender: mpsc::Sender<AsyncRuntimeRequestEnvelope>,
    /// Nonblocking generation-fenced clipboard cleanup for aborted event tasks.
    pub(super) client_clipboard_route_cleanup_tx:
        mpsc::UnboundedSender<ClientClipboardRouteCleanup>,
    /// Stores the message delivery notify value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) message_delivery_notify: Arc<Notify>,
    /// Stores the event delivery notify value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) event_delivery_notify: Arc<Notify>,
    /// Receives durable event-delivery revisions independently per handle.
    pub(super) event_delivery_revision_rx: watch::Receiver<u64>,
    /// Stores the side effect delivery notify value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    #[allow(
        dead_code,
        reason = "handle notification port is retained for typed host services"
    )]
    pub(super) side_effect_delivery_notify: Arc<Notify>,
    /// Stores the side effect delivery revision rx value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) side_effect_delivery_rx: watch::Receiver<u64>,
    /// Stores the lifecycle state rx value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) lifecycle_state_rx: watch::Receiver<RuntimeLifecycleState>,
    /// Observes terminal configuration generation changes without actor calls.
    pub(super) terminal_config_generation_rx: watch::Receiver<u64>,
}

/// Carries Async Runtime Actor Exit state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
pub struct AsyncRuntimeActorExit {
    /// Stores the service value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub service: RuntimeSessionService,
    /// Stores the commands processed value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    #[allow(
        dead_code,
        reason = "actor exit report is consumed by service owners and tests"
    )]
    pub commands_processed: u64,
    /// Stores the metrics value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    #[allow(
        dead_code,
        reason = "actor exit report is consumed by service owners and tests"
    )]
    pub metrics: AsyncRuntimeActorMetrics,
}

/// Drop-safe publication fence for a terminal lifecycle transition.
///
/// The actor installs this fence when a control request makes the session
/// terminal. The connection acknowledges it only after the response flushes;
/// dropping an unacknowledged fence still publishes shutdown so failed or
/// cancelled transports cannot leave the runtime services alive indefinitely.
#[derive(Debug)]
pub(super) struct AsyncTerminalLifecycleFlushGuard {
    lifecycle_state_tx: watch::Sender<RuntimeLifecycleState>,
    state: RuntimeLifecycleState,
    armed: bool,
}

impl AsyncTerminalLifecycleFlushGuard {
    /// Arms one exact terminal lifecycle publication.
    pub(super) fn new(
        lifecycle_state_tx: watch::Sender<RuntimeLifecycleState>,
        state: RuntimeLifecycleState,
    ) -> Self {
        Self {
            lifecycle_state_tx,
            state,
            armed: true,
        }
    }

    /// Publishes the terminal state after the owning response was flushed.
    pub(super) fn acknowledge(mut self) {
        let _ = self.lifecycle_state_tx.send(self.state);
        self.armed = false;
    }
}

impl Drop for AsyncTerminalLifecycleFlushGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.lifecycle_state_tx.send(self.state);
        }
    }
}

/// Carries Async Control Input Result state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
pub struct AsyncControlInputResult {
    /// Stores the output value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub output: Vec<u8>,
    /// Stores the consumed value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub consumed: usize,
    /// Stores the connection value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub connection: ControlConnectionState,
    /// Terminal lifecycle publication deferred until this response flushes.
    pub(super) terminal_lifecycle_flush: Option<AsyncTerminalLifecycleFlushGuard>,
}

impl AsyncControlInputResult {
    /// Moves response bytes, connection state, and any flush fence to the adapter.
    pub(super) fn into_parts(
        self,
    ) -> (
        Vec<u8>,
        ControlConnectionState,
        Option<AsyncTerminalLifecycleFlushGuard>,
    ) {
        (self.output, self.connection, self.terminal_lifecycle_flush)
    }
}

/// Carries Async Message Input Result state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncMessageInputResult {
    /// Stores the output value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub output: Vec<u8>,
    /// Stores the consumed value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub consumed: usize,
    /// Stores the connection value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub connection: MessageConnection,
}

/// Carries Async Message Fanout state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncMessageFanout {
    /// Stores the recipient value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub recipient: AgentId,
    /// Stores the frame value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub frame: Vec<u8>,
    /// Stores the messages value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub messages: usize,
    /// Stores the batch value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub batch: FanoutBatch,
}

/// Carries Async Agent Provider Poll Report state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncAgentProviderPollReport {
    /// Stores the polls value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub polls: u64,
    /// Stores the executions value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub executions: u64,
    /// Stores the idle polls value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub idle_polls: u64,
    /// Stores the terminal state value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub terminal_state: RuntimeLifecycleState,
}

/// Carries Async Agent Provider Service Config state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncAgentProviderServiceConfig {
    /// Stores the max tasks per poll value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub max_tasks_per_poll: usize,
    /// Bounded idle interval before the service probes actor state again.
    ///
    /// The provider worker normally wakes from side-effect notifications. This
    /// interval is a liveness backstop for missed retained notification permits
    /// on slower systems and should stay large enough to avoid idle churn.
    pub idle_interval: Duration,
}

impl AsyncAgentProviderServiceConfig {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn new(max_tasks_per_poll: usize) -> Result<Self> {
        let config = Self {
            max_tasks_per_poll,
            ..Self::default()
        };
        config.validate()?;
        Ok(config)
    }

    /// Returns this config with a caller-selected idle probe interval.
    ///
    /// # Parameters
    /// - `idle_interval`: The bounded delay before the provider worker probes
    ///   actor state again while otherwise idle.
    ///
    /// # Errors
    /// Returns an error when the interval is zero or another config invariant
    /// no longer holds after the update.
    #[cfg(test)]
    pub fn with_idle_interval(mut self, idle_interval: Duration) -> Result<Self> {
        self.idle_interval = idle_interval;
        self.validate()?;
        Ok(self)
    }
    /// Returns the actor-owned provider-poll fallback delay in milliseconds.
    ///
    /// The fallback reuses the validated idle interval so the async provider
    /// worker keeps a bounded liveness backstop without collapsing into a
    /// high-frequency wakeup loop.
    pub fn provider_poll_fallback_delay_ms(&self) -> u64 {
        let delay_ms = self.idle_interval.as_millis();
        u64::try_from(delay_ms).unwrap_or(u64::MAX).max(1)
    }

    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn validate(&self) -> Result<()> {
        if self.max_tasks_per_poll == 0 {
            return Err(MezError::invalid_args(
                "async agent provider max tasks per poll must be greater than zero",
            ));
        }
        if self.idle_interval.is_zero() {
            return Err(MezError::invalid_args(
                "async agent provider idle interval must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl Default for AsyncAgentProviderServiceConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            max_tasks_per_poll: 1,
            idle_interval: Duration::from_millis(100),
        }
    }
}

/// Carries Async Runtime Control Connection Config state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncRuntimeControlConnectionConfig {
    /// Stores the max content length value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub max_content_length: usize,
    /// Stores the owner uid value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub owner_uid: u32,
    /// Optional application-idle deadline for one complete request/response
    /// cycle. Unix control leaves this unset; remote transports opt in.
    pub application_idle_timeout: Option<Duration>,
}

impl AsyncRuntimeControlConnectionConfig {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(max_content_length: usize, owner_uid: u32) -> Result<Self> {
        if max_content_length == 0 {
            return Err(MezError::invalid_args(
                "async control max content length must be greater than zero",
            ));
        }
        Ok(Self {
            max_content_length,
            owner_uid,
            application_idle_timeout: None,
        })
    }

    /// Applies a finite application-idle deadline in focused connection tests.
    #[cfg(test)]
    pub fn with_application_idle_timeout(mut self, timeout: Duration) -> Self {
        self.application_idle_timeout = Some(timeout);
        self
    }
}

impl Default for AsyncRuntimeControlConnectionConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            max_content_length: DEFAULT_ASYNC_CONTROL_MAX_CONTENT_LENGTH,
            owner_uid: current_effective_uid(),
            application_idle_timeout: None,
        }
    }
}

/// Carries Async Runtime Message Connection Config state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncRuntimeMessageConnectionConfig {
    /// Stores the max content length value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub max_content_length: usize,
    /// Stores the fanout limit value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub fanout_limit: usize,
}

impl AsyncRuntimeMessageConnectionConfig {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(max_content_length: usize, fanout_limit: usize) -> Result<Self> {
        if max_content_length == 0 {
            return Err(MezError::invalid_args(
                "async message max content length must be greater than zero",
            ));
        }
        if fanout_limit == 0 {
            return Err(MezError::invalid_args(
                "async message fanout limit must be greater than zero",
            ));
        }
        Ok(Self {
            max_content_length,
            fanout_limit,
        })
    }
}

impl Default for AsyncRuntimeMessageConnectionConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            max_content_length: DEFAULT_ASYNC_CONTROL_MAX_CONTENT_LENGTH,
            fanout_limit: 100,
        }
    }
}

/// Carries Async Runtime Event Connection Config state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncRuntimeEventConnectionConfig {
    /// Stores the limit per connection value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub limit_per_connection: usize,
    /// Stores the owner uid value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub owner_uid: u32,
}

impl AsyncRuntimeEventConnectionConfig {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn new(limit_per_connection: usize, owner_uid: u32) -> Result<Self> {
        if limit_per_connection == 0 {
            return Err(MezError::invalid_args(
                "async event limit per connection must be greater than zero",
            ));
        }
        Ok(Self {
            limit_per_connection,
            owner_uid,
        })
    }
}

impl Default for AsyncRuntimeEventConnectionConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            limit_per_connection: DEFAULT_ASYNC_EVENT_LIMIT_PER_CONNECTION,
            owner_uid: current_effective_uid(),
        }
    }
}

/// Carries Async Runtime Daemon Listeners state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
pub struct AsyncRuntimeDaemonListeners {
    /// Stores the control value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub control: Option<UnixListener>,
    /// Stores the message value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub message: Option<UnixListener>,
    /// Stores the event value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub event: Option<UnixListener>,
}

impl AsyncRuntimeDaemonListeners {
    /// Runs the control only operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn control_only(listener: UnixListener) -> Self {
        Self {
            control: Some(listener),
            message: None,
            event: None,
        }
    }
}

/// Carries Async Runtime Daemon Config state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct AsyncRuntimeDaemonConfig {
    /// Stores the control value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub control: AsyncRuntimeControlConnectionConfig,
    /// Stores the snapshots value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub snapshots: Option<SnapshotRepository>,
    /// Stores the event value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub event: AsyncRuntimeEventConnectionConfig,
    /// Stores the message max content length value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub message_max_content_length: usize,
    /// Stores the message fanout limit value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub message_fanout_limit: usize,
    /// Stores the message base now ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub message_base_now_ms: u64,
    /// Stores the max control connections value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub max_control_connections: u64,
    /// Stores the max message connections value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub max_message_connections: u64,
    /// Stores the max event connections value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub max_event_connections: u64,
    /// Stores the max event batches per connection value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub max_event_batches_per_connection: u64,
    /// Unix-millisecond clock captured when the daemon timer worker starts.
    ///
    /// Timer events add monotonic elapsed time to this base before comparing
    /// against runtime transaction timestamps, which use the same Unix epoch.
    pub timer_base_now_ms: u64,
}

impl Default for AsyncRuntimeDaemonConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            control: AsyncRuntimeControlConnectionConfig::default(),
            snapshots: None,
            event: AsyncRuntimeEventConnectionConfig::default(),
            message_max_content_length: DEFAULT_ASYNC_CONTROL_MAX_CONTENT_LENGTH,
            message_fanout_limit: 100,
            message_base_now_ms: 0,
            max_control_connections: u64::MAX,
            max_message_connections: u64::MAX,
            max_event_connections: u64::MAX,
            max_event_batches_per_connection: u64::MAX,
            timer_base_now_ms: crate::runtime::current_unix_millis(),
        }
    }
}

impl AsyncRuntimeDaemonConfig {
    /// Runs the validate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn validate(&self) -> Result<()> {
        self.validate_session_services()?;
        if self.max_control_connections == 0
            && self.max_message_connections == 0
            && self.max_event_connections == 0
        {
            return Err(MezError::invalid_args(
                "async daemon requires at least one permitted listener connection",
            ));
        }
        Ok(())
    }

    /// Validates limits used by listener-independent session workers.
    ///
    /// A persistent host may route directly to an actor without publishing a
    /// per-session Unix listener. Such runtimes still require valid message and
    /// event worker bounds, but they do not require a positive listener limit.
    pub fn validate_session_services(&self) -> Result<()> {
        if self.message_max_content_length == 0 {
            return Err(MezError::invalid_args(
                "async daemon message max content length must be greater than zero",
            ));
        }
        if self.message_fanout_limit == 0 {
            return Err(MezError::invalid_args(
                "async daemon message fanout limit must be greater than zero",
            ));
        }
        if self.max_event_batches_per_connection == 0 {
            return Err(MezError::invalid_args(
                "async daemon event batch limit must be greater than zero",
            ));
        }
        Ok(())
    }
}
