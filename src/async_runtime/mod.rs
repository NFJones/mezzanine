//! Tokio orchestration for the runtime session service.
//!
//! The runtime session service is intentionally synchronous: it owns session
//! state, terminal screens, process metadata, message state, and control
//! idempotency in one place. This module adds an asynchronous command boundary
//! around that owner so future socket tasks, timer tasks, and terminal fd tasks
//! can run concurrently while all session mutations remain serialized through a
//! single actor.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::os::fd::AsRawFd;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::task::{Id as TokioTaskId, JoinError, JoinSet};
use tokio::time::sleep;
use tokio_util::codec::Framed;

use crate::agent::{AgentTurnExecution, AgentTurnLedger, AgentTurnRunner};
use crate::control::{ControlConnectionState, encode_control_body};
use crate::error::{MezError, Result};
use crate::event::{EventAudience, encode_event_notification};
use crate::framing::{ProtocolFrameCodec, encode_frame};
use crate::ids::{AgentId, ClientId};
use crate::message::{
    DeliveryCursor, FanoutBatch, MessageConnection, delivery_batch_json, encode_mmp_body,
};
use crate::runtime::{
    AttachedClientStepApplication, RuntimeAgentCompactionDispatch, RuntimeAgentProviderDispatch,
    RuntimeAgentProviderDispatchProvider, RuntimeAgentProviderTask, RuntimeAgentRememberDispatch,
    RuntimeEventConnectionTable, RuntimeEventWakeup, RuntimeLifecycleState, RuntimeSessionService,
    RuntimeSnapshotControlAsyncOutcome, RuntimeSnapshotControlAsyncWork,
    RuntimeSnapshotControlAsyncWorkKind, authorize_unix_peer_raw_fd, current_effective_uid,
};
use crate::terminal::{
    AttachedTerminalClientLoopConfig, AttachedTerminalClientLoopReport,
    AttachedTerminalClientStepPlan, AttachedTerminalFdReadiness, AttachedTerminalFdRole,
    AttachedTerminalOutputModes, ClientStatusLine, ClientViewRole, MouseAction, RenderedClientView,
    TerminalClientLoopAction, TerminalClientLoopConfig, TerminalStyleSpan,
    compose_client_presentation_with_styles, plan_attached_terminal_client_step,
    plan_attached_terminal_client_step_with_host_paste_buffer,
};
use mez_mux::layout::Size;
use mez_mux::process::{PaneExitStatus, PaneProcess};
use mez_mux::session::ClientState;

/// Exposes the actor module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod actor;
/// Exposes the actor types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod actor_types;
/// Exposes the client module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod client;
/// Exposes the config module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod config;
/// Exposes the daemon module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod daemon;
/// Exposes the events module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod events;
/// Exposes the pane io module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod pane_io;
/// Exposes the provider module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod provider;
/// Exposes the side effects module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod side_effects;
/// Exposes the supervisor module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod supervisor;
/// Exposes the terminal module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod terminal;
/// Exposes the terminal io module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod terminal_io;

pub use crate::runtime::{
    AgentCompactionEvent, AgentProviderEvent, AgentRememberEvent, AsyncHookEvent, ClientEvent,
    PaneEvent, PersistenceEvent, PersistenceTarget, PersistenceWriteMode, ProcessEvent,
    RenderInvalidationReason, RuntimeEvent, RuntimeEventBatch, RuntimeEventIngressReport,
    RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind, RuntimeTransition, ShutdownEvent,
    TimerEvent,
};
pub use actor_types::{
    AsyncAttachedTerminalStepRequest, AsyncRenderedClientFlush, AsyncRenderedClientFrame,
    plan_and_apply_async_attached_terminal_client_step, plan_async_attached_terminal_client_step,
    serve_async_runtime_control_connection, serve_async_runtime_control_connection_loop,
    serve_async_runtime_control_connection_loop_with_snapshots,
    serve_async_runtime_control_connection_with_snapshots, serve_async_runtime_control_listener,
    serve_async_runtime_control_listener_with_snapshots, serve_async_runtime_message_connection,
    serve_async_runtime_message_connection_loop, serve_async_runtime_message_listener,
    serve_async_runtime_message_listener_concurrent,
};
pub use client::{
    AsyncAttachedTerminalClientServiceConfig, AsyncAttachedTerminalClientServiceReport,
    build_async_attached_terminal_client_service, run_async_agent_provider_service,
    run_async_attached_terminal_client_service,
};
pub use config::{
    AsyncAgentProviderPollReport, AsyncAgentProviderServiceConfig, AsyncControlInputResult,
    AsyncMessageFanout, AsyncMessageInputResult, AsyncRuntimeActorConfig, AsyncRuntimeActorExit,
    AsyncRuntimeActorMetrics, AsyncRuntimeControlConnectionConfig, AsyncRuntimeDaemonConfig,
    AsyncRuntimeDaemonListeners, AsyncRuntimeEventConnectionConfig,
    AsyncRuntimeMessageConnectionConfig, AsyncRuntimeSessionActor, AsyncRuntimeSessionHandle,
    RuntimeHistogram, RuntimeHistogramBucket,
};
pub use daemon::{build_async_runtime_daemon_services, run_async_runtime_daemon};
pub use events::{
    flush_async_runtime_event_wakeups_to_stream, serve_async_runtime_event_connection,
    serve_async_runtime_event_listener,
};
#[cfg(test)]
pub use pane_io::AsyncFakePaneProcessIo;
pub use pane_io::{
    AsyncPaneForegroundProcess, AsyncPaneIoFuture, AsyncPaneIoSideEffectServiceConfig,
    AsyncPaneIoSideEffectServiceReport, AsyncPaneProcessDriver, AsyncPaneProcessDriverConfig,
    AsyncPaneProcessDriverServiceConfig, AsyncPaneProcessDriverServiceReport, AsyncPaneProcessIo,
    AsyncPaneProcessServiceConfig, AsyncPaneProcessServiceReport,
    AsyncPaneProcessSupervisorServiceConfig, AsyncPaneProcessSupervisorServiceReport,
    AsyncPtyPaneProcessIo, build_async_pane_io_side_effect_service,
    build_async_pane_process_service, build_async_pane_process_supervisor_service,
    run_async_pane_io_side_effect_service, run_async_pane_process_driver_service,
    run_async_pane_process_service, run_async_pane_process_supervisor_service,
};
pub use provider::build_async_agent_provider_service;
pub use side_effects::{
    AsyncClientOutputFlushServiceReport, AsyncHookSideEffectServiceReport,
    AsyncPersistenceSideEffectServiceReport, AsyncRuntimeSideEffectServiceConfig,
    AsyncRuntimeSideEffectServiceReport, AsyncRuntimeTimerServiceReport,
    build_async_client_output_flush_service, build_async_hook_side_effect_service,
    build_async_persistence_side_effect_service, build_async_render_side_effect_service,
    build_async_runtime_side_effect_service, build_async_runtime_timer_side_effect_service,
    run_async_client_output_flush_service, run_async_hook_side_effect_service,
    run_async_persistence_side_effect_service, run_async_render_side_effect_service,
    run_async_runtime_side_effect_service, run_async_runtime_timer_side_effect_service,
};
pub use supervisor::{
    AsyncRuntimeService, AsyncRuntimeServiceExit, AsyncRuntimeServiceExitKind,
    AsyncRuntimeServiceReport, AsyncRuntimeServiceSupervisor, AsyncRuntimeSupervisionReport,
    DEFAULT_ASYNC_ATTACHED_TERMINAL_POLL_TIMEOUT, DEFAULT_ASYNC_CONTROL_MAX_CONTENT_LENGTH,
    DEFAULT_ASYNC_EVENT_LIMIT_PER_CONNECTION, DEFAULT_ASYNC_IDLE_CLEANUP_INTERVAL,
    DEFAULT_ASYNC_RUNTIME_COMMAND_BUFFER, supervise_async_runtime_services,
};
pub use terminal::{AsyncAttachedTerminalLoopRequest, run_async_attached_terminal_client_loop};
#[cfg(test)]
pub use terminal_io::SyncAttachedTerminalIoAdapter;
pub use terminal_io::{
    AsyncAttachedTerminalFdLoopIo, AsyncAttachedTerminalIo, AsyncAttachedTerminalPresentationGuard,
    AsyncTerminalIoFuture, AsyncTerminalOutputWriteReport,
    DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES,
};
#[cfg(test)]
pub use terminal_io::{AsyncFakeAttachedTerminalIo, AsyncFakeTerminalFrame};

use actor_types::AsyncRuntimeRequest;
use provider::{
    empty_attached_terminal_loop_report, is_terminal_runtime_lifecycle_state,
    merge_attached_terminal_loop_report,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
