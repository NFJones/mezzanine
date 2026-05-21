//! Async Runtime Config implementation.
//!
//! This module owns the async runtime config boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AgentId, Arc, AsyncRuntimeRequest, ControlConnectionState,
    DEFAULT_ASYNC_CONTROL_MAX_CONTENT_LENGTH, DEFAULT_ASYNC_EVENT_LIMIT_PER_CONNECTION,
    DEFAULT_ASYNC_RUNTIME_COMMAND_BUFFER, Duration, EventAudience, FanoutBatch, HashMap, HashSet,
    MessageConnection, MezError, Notify, Result, RuntimeLifecycleState, RuntimeSessionService,
    RuntimeSideEffect, RuntimeTimerKey, UnixListener, VecDeque, current_effective_uid, mpsc, watch,
};
use crate::snapshot::SnapshotRepository;

// Async runtime, daemon, connection, and client configuration.

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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AsyncRuntimeActorMetrics {
    /// Number of actor requests processed in serialized order.
    pub commands_processed: u64,
    /// Number of typed runtime event batches accepted by the actor.
    pub runtime_event_batches: u64,
    /// Number of typed runtime events accepted by the actor.
    pub runtime_events_accepted: u64,
    /// Number of typed runtime events applied to mutable runtime state.
    pub runtime_events_applied: u64,
    /// Number of runtime side effects queued by event application.
    pub runtime_side_effects_queued: u64,
    /// Number of runtime side effects drained by supervised workers.
    pub runtime_side_effects_drained: u64,
    /// Number of pane output chunks applied through typed runtime events.
    pub pane_output_chunks: u64,
    /// Number of pane output bytes applied through typed runtime events.
    pub pane_output_bytes: u64,
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
    /// Message-delivery notifications emitted by actor mutations.
    pub message_delivery_notifications: u64,
    /// Event-delivery notifications emitted by actor mutations.
    pub event_delivery_notifications: u64,
    /// Side-effect-delivery notifications emitted by actor mutations.
    pub side_effect_delivery_notifications: u64,
    /// Lifecycle-state notifications emitted by actor mutations.
    pub lifecycle_state_notifications: u64,
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
    pub(super) sender: mpsc::Sender<AsyncRuntimeRequest>,
    /// Stores the receiver value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) receiver: mpsc::Receiver<AsyncRuntimeRequest>,
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
    /// Stores the side effect delivery notify value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) side_effect_delivery_notify: Arc<Notify>,
    /// Stores the lifecycle state tx value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) lifecycle_state_tx: watch::Sender<RuntimeLifecycleState>,
    /// Stores the side effects value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) side_effects: VecDeque<RuntimeSideEffect>,
    /// Stores the scheduled shell transaction timers value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) scheduled_shell_transaction_timers: HashSet<RuntimeTimerKey>,
    /// Stores the scheduled resize debounce timers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) scheduled_resize_debounce_timers: HashSet<RuntimeTimerKey>,
    /// Stores the scheduled cursor blink timers value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) scheduled_cursor_blink_timers: HashMap<String, RuntimeTimerKey>,
    /// Stores the scheduled status refresh timers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) scheduled_status_refresh_timers: HashMap<String, RuntimeTimerKey>,
    /// Stores the scheduled provider poll timer value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) scheduled_provider_poll_timer: Option<RuntimeTimerKey>,
    /// Stores the scheduled provider retry timers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) scheduled_provider_retry_timers: HashMap<String, RuntimeTimerKey>,
    /// Stores timeout timers for provider tasks claimed by async workers.
    ///
    /// These timers ensure a worker that never reports completion or failure
    /// cannot leave a turn running indefinitely.
    pub(super) scheduled_provider_claim_timers: HashMap<String, RuntimeTimerKey>,
    /// Stores the next provider claim timer generation value.
    ///
    /// The generation lets the actor ignore stale claim timeout events from
    /// earlier workers for the same turn.
    pub(super) next_provider_claim_timer_generation: u64,
    /// Stores the provider retry attempts value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) provider_retry_attempts: HashMap<String, u32>,
    /// Stores the scheduled pane pipe health timers value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) scheduled_pane_pipe_health_timers: HashMap<String, RuntimeTimerKey>,
    /// Stores the next pane pipe health timer generation value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) next_pane_pipe_health_timer_generation: u64,
    /// Stores the scheduled idle cleanup timer value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) scheduled_idle_cleanup_timer: Option<RuntimeTimerKey>,
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
    pub(super) sender: mpsc::Sender<AsyncRuntimeRequest>,
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
    /// Stores the side effect delivery notify value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) side_effect_delivery_notify: Arc<Notify>,
    /// Stores the lifecycle state rx value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) lifecycle_state_rx: watch::Receiver<RuntimeLifecycleState>,
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
    pub commands_processed: u64,
    /// Stores the metrics value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub metrics: AsyncRuntimeActorMetrics,
}

/// Carries Async Control Input Result state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub fn with_idle_interval(mut self, idle_interval: Duration) -> Result<Self> {
        self.idle_interval = idle_interval;
        self.validate()?;
        Ok(self)
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
        })
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
    /// Stores the event audience value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub event_audience: EventAudience,
    /// Stores the timer base now ms value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
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
            event_audience: EventAudience::Primary,
            timer_base_now_ms: 0,
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
        if self.max_control_connections == 0
            && self.max_message_connections == 0
            && self.max_event_connections == 0
        {
            return Err(MezError::invalid_args(
                "async daemon requires at least one permitted listener connection",
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
