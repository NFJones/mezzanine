// Regression coverage for the async runtime tests subsystem.
//
// These tests describe the behavior protected by the repository
// specification and workflow guidance. Keeping the scenarios documented
// makes failures easier to map back to the user-visible contract.

// Async runtime module tests.

use super::{
    AgentProviderEvent, AsyncAgentProviderServiceConfig, AsyncAttachedTerminalClientServiceConfig,
    AsyncAttachedTerminalFdLoopIo, AsyncAttachedTerminalIo, AsyncAttachedTerminalLoopRequest,
    AsyncAttachedTerminalPresentationGuard, AsyncAttachedTerminalStepRequest,
    AsyncFakeAttachedTerminalIo, AsyncFakePaneProcessIo, AsyncHookEvent,
    AsyncPaneForegroundProcess, AsyncPaneIoSideEffectServiceConfig, AsyncPaneProcessDriver,
    AsyncPaneProcessDriverConfig, AsyncPaneProcessDriverServiceConfig, AsyncPaneProcessIo,
    AsyncPaneProcessServiceConfig, AsyncPaneProcessSupervisorServiceConfig, AsyncPtyPaneProcessIo,
    AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeDaemonConfig,
    AsyncRuntimeDaemonListeners, AsyncRuntimeEventConnectionConfig,
    AsyncRuntimeMessageConnectionConfig, AsyncRuntimeService, AsyncRuntimeServiceExit,
    AsyncRuntimeServiceReport, AsyncRuntimeServiceSupervisor, AsyncRuntimeSessionHandle,
    AsyncRuntimeSideEffectServiceConfig, AsyncTerminalIoFuture, AsyncTerminalOutputWriteReport,
    ClientEvent, DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES, Duration, PaneEvent,
    PersistenceTarget, PersistenceWriteMode, ProcessEvent, RenderInvalidationReason, Result,
    RuntimeEvent, RuntimeEventBatch, RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind,
    ShutdownEvent, SyncAttachedTerminalIoAdapter, TimerEvent,
    build_async_attached_terminal_client_service, build_async_runtime_daemon_services,
    flush_async_runtime_event_wakeups_to_stream,
    plan_and_apply_async_attached_terminal_client_step, plan_async_attached_terminal_client_step,
    run_async_agent_provider_service, run_async_attached_terminal_client_loop,
    run_async_attached_terminal_client_service, run_async_client_output_flush_service,
    run_async_hook_side_effect_service, run_async_pane_io_side_effect_service,
    run_async_pane_process_driver_service, run_async_pane_process_service,
    run_async_pane_process_supervisor_service, run_async_persistence_side_effect_service,
    run_async_render_side_effect_service, run_async_runtime_daemon,
    run_async_runtime_side_effect_service, run_async_runtime_timer_side_effect_service,
    serve_async_runtime_control_connection, serve_async_runtime_control_connection_loop,
    serve_async_runtime_control_listener, serve_async_runtime_event_connection,
    serve_async_runtime_event_listener, serve_async_runtime_message_connection,
    serve_async_runtime_message_connection_loop, serve_async_runtime_message_listener,
    serve_async_runtime_message_listener_concurrent, supervise_async_runtime_services,
};
use crate::MezError;
use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
use crate::control::ControlConnectionState;
use crate::event::EventAudience;
use crate::hooks::{HookEvent, HookExecutionPlan, HookOnFailure};
use crate::ids::{AgentId, ClientId, IdFactory};
use crate::message::MessageConnection;
use crate::process::spawn_pane_process;
use crate::registry::SessionRegistry;
use crate::runtime::{
    RuntimeLifecycleState, RuntimeSessionService, current_effective_uid, pane_environment,
};
use crate::terminal::{AttachedTerminalClientStepPlan, AttachedTerminalOutputModes};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc as StdArc, Mutex};
use std::time::Instant;

use crate::layout::Size;
use crate::runtime::RuntimeEventConnectionTable;
use crate::session::{ClientState, Session};
use crate::shell::resolve_shell;
use crate::terminal::{
    AttachedTerminalClientLoopConfig, AttachedTerminalClientLoopIo, AttachedTerminalFdReadiness,
    AttachedTerminalFdRole, ClientStatusKind, ClientStatusLine, ClientViewRole, MuxAction,
    TerminalClientLoopAction, TerminalClientLoopConfig, TerminalFdInterest, TerminalStyleSpan,
};
use crate::test_support::async_runtime::AsyncRuntimeActorFixture;
use crate::transcript::AgentTranscriptStore;

/// Runs the test service operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
mod fixtures;

use fixtures::*;

mod actor;
mod connections;
mod daemon;
mod services;
