//! Regression coverage for the async runtime tests subsystem.
//!
//! These tests describe the behavior protected by the repository
//! specification and workflow guidance. Keeping the scenarios documented
//! makes failures easier to map back to the user-visible contract.

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
    AsyncRuntimeServiceReport, AsyncRuntimeServiceSupervisor, AsyncRuntimeSessionActor,
    AsyncRuntimeSideEffectServiceConfig, AsyncTerminalOutputWriteReport, ClientEvent,
    DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES, Duration, PaneEvent, PersistenceTarget,
    PersistenceWriteMode, ProcessEvent, RenderInvalidationReason, Result, RuntimeEvent,
    RuntimeEventBatch, RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind, ShutdownEvent,
    SyncAttachedTerminalIoAdapter, TimerEvent, build_async_attached_terminal_client_service,
    build_async_runtime_daemon_services, flush_async_runtime_event_wakeups_to_stream,
    plan_and_apply_async_attached_terminal_client_step, plan_async_attached_terminal_client_step,
    run_async_agent_provider_service, run_async_attached_terminal_client_loop,
    run_async_attached_terminal_client_loop_deferred_pane_io,
    run_async_attached_terminal_client_service,
    run_async_attached_terminal_client_service_deferred_pane_io,
    run_async_client_output_flush_service, run_async_hook_side_effect_service,
    run_async_pane_io_side_effect_service, run_async_pane_process_driver_service,
    run_async_pane_process_service, run_async_pane_process_supervisor_service,
    run_async_persistence_side_effect_service, run_async_render_side_effect_service,
    run_async_runtime_daemon, run_async_runtime_side_effect_service,
    run_async_runtime_timer_side_effect_service, serve_async_runtime_control_connection,
    serve_async_runtime_control_connection_loop, serve_async_runtime_control_listener,
    serve_async_runtime_event_connection, serve_async_runtime_event_listener,
    serve_async_runtime_message_connection, serve_async_runtime_message_connection_loop,
    serve_async_runtime_message_listener, serve_async_runtime_message_listener_concurrent,
    supervise_async_runtime_services,
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
use crate::transcript::AgentTranscriptStore;

/// Runs the test service operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_service() -> RuntimeSessionService {
    let shell = resolve_shell(Some(OsString::from("/bin/sh"))).unwrap();
    let size = Size::new(80, 24).unwrap();
    let session = Session::new_default(shell, size);
    RuntimeSessionService::new(
        session,
        PathBuf::from("/tmp/mez-async-runtime-test.sock"),
        1,
    )
    .unwrap()
}

/// Returns the Unix permission mode for a test path without file type bits.
#[cfg(unix)]
fn unix_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Runs the test pane environment operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_pane_environment() -> crate::runtime::PaneEnvironment {
    let mut ids = IdFactory::default();
    pane_environment(
        Path::new("/tmp/mez-async-runtime-test.sock"),
        &ids.session(),
        &ids.window(),
        &ids.pane(),
    )
    .unwrap()
}

/// Runs the test service with event log operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_service_with_event_log() -> RuntimeSessionService {
    let shell = resolve_shell(Some(OsString::from("/bin/sh"))).unwrap();
    let size = Size::new(80, 24).unwrap();
    let session = Session::new_default(shell, size);
    RuntimeSessionService::with_event_log(
        session,
        PathBuf::from("/tmp/mez-async-runtime-test.sock"),
        1,
        16,
        4096,
    )
    .unwrap()
}

/// Carries Fake Attached Terminal Loop Io state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Default)]
struct FakeAttachedTerminalLoopIo {
    /// Stores the readiness batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    readiness_batches: Vec<Vec<AttachedTerminalFdReadiness>>,
    /// Stores the input batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    input_batches: Vec<Vec<u8>>,
    /// Stores the written batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    written_batches: Vec<Vec<String>>,
    /// Stores the write error kinds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    write_error_kinds: Vec<std::io::ErrorKind>,
}

impl AttachedTerminalClientLoopIo for FakeAttachedTerminalLoopIo {
    /// Runs the poll readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_readiness(&mut self) -> Result<Vec<AttachedTerminalFdReadiness>> {
        if self.readiness_batches.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self.readiness_batches.remove(0))
    }

    /// Runs the read input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_input(&mut self, max_bytes: usize) -> Result<Vec<u8>> {
        if self.input_batches.is_empty() {
            return Ok(Vec::new());
        }
        let mut input = self.input_batches.remove(0);
        if input.len() > max_bytes {
            let remainder = input.split_off(max_bytes);
            self.input_batches.insert(0, remainder);
        }
        input.truncate(max_bytes);
        Ok(input)
    }

    /// Runs the write output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_output(&mut self, lines: &[String]) -> Result<usize> {
        if !self.write_error_kinds.is_empty() {
            let kind = self.write_error_kinds.remove(0);
            return Err(std::io::Error::new(kind, "simulated terminal write failure").into());
        }
        self.written_batches.push(lines.to_vec());
        Ok(lines.iter().map(String::len).sum())
    }
}

impl AsyncAttachedTerminalIo for FakeAttachedTerminalLoopIo {
    /// Runs the poll readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_readiness<'a>(
        &'a mut self,
    ) -> super::AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        Box::pin(async move { <Self as AttachedTerminalClientLoopIo>::poll_readiness(self) })
    }

    /// Runs the poll input readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_input_readiness<'a>(
        &'a mut self,
    ) -> super::AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        Box::pin(async move {
            Ok(
                <Self as AttachedTerminalClientLoopIo>::poll_readiness(self)?
                    .into_iter()
                    .filter(|ready| {
                        ready.role != AttachedTerminalFdRole::Output || ready.hangup || ready.error
                    })
                    .collect(),
            )
        })
    }

    /// Runs the read input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_input<'a>(&'a mut self, max_bytes: usize) -> super::AsyncTerminalIoFuture<'a, Vec<u8>> {
        Box::pin(async move { <Self as AttachedTerminalClientLoopIo>::read_input(self, max_bytes) })
    }

    /// Runs the write styled output with modes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_styled_output_with_modes<'a>(
        &'a mut self,
        lines: &'a [String],
        line_style_spans: &'a [Vec<crate::terminal::TerminalStyleSpan>],
        modes: AttachedTerminalOutputModes,
    ) -> super::AsyncTerminalIoFuture<'a, usize> {
        Box::pin(async move {
            <Self as AttachedTerminalClientLoopIo>::write_styled_output_with_modes(
                self,
                lines,
                line_style_spans,
                modes,
            )
        })
    }
}

/// Carries Fake Resizing Attached Terminal Loop Io state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Default)]
struct FakeResizingAttachedTerminalLoopIo {
    /// Stores the inner value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    inner: FakeAttachedTerminalLoopIo,
    /// Stores the terminal size batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    terminal_size_batches: Vec<Option<Size>>,
    /// Stores the invalidated output frames value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    invalidated_output_frames: usize,
}

impl AttachedTerminalClientLoopIo for FakeResizingAttachedTerminalLoopIo {
    /// Runs the poll readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_readiness(&mut self) -> Result<Vec<AttachedTerminalFdReadiness>> {
        <FakeAttachedTerminalLoopIo as AttachedTerminalClientLoopIo>::poll_readiness(
            &mut self.inner,
        )
    }

    /// Runs the read input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_input(&mut self, max_bytes: usize) -> Result<Vec<u8>> {
        <FakeAttachedTerminalLoopIo as AttachedTerminalClientLoopIo>::read_input(
            &mut self.inner,
            max_bytes,
        )
    }

    /// Runs the write output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_output(&mut self, lines: &[String]) -> Result<usize> {
        self.inner.write_output(lines)
    }

    /// Runs the terminal size operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn terminal_size(&mut self) -> Result<Option<Size>> {
        if self.terminal_size_batches.is_empty() {
            return Ok(None);
        }
        Ok(self.terminal_size_batches.remove(0))
    }

    /// Runs the invalidate output frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn invalidate_output_frame(&mut self) -> Result<()> {
        self.invalidated_output_frames = self.invalidated_output_frames.saturating_add(1);
        Ok(())
    }
}

impl AsyncAttachedTerminalIo for FakeResizingAttachedTerminalLoopIo {
    /// Runs the poll readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_readiness<'a>(
        &'a mut self,
    ) -> super::AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        Box::pin(async move { <Self as AttachedTerminalClientLoopIo>::poll_readiness(self) })
    }

    /// Runs the poll input readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_input_readiness<'a>(
        &'a mut self,
    ) -> super::AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        Box::pin(async move {
            Ok(
                <Self as AttachedTerminalClientLoopIo>::poll_readiness(self)?
                    .into_iter()
                    .filter(|ready| {
                        ready.role != AttachedTerminalFdRole::Output || ready.hangup || ready.error
                    })
                    .collect(),
            )
        })
    }

    /// Runs the read input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_input<'a>(&'a mut self, max_bytes: usize) -> super::AsyncTerminalIoFuture<'a, Vec<u8>> {
        Box::pin(async move { <Self as AttachedTerminalClientLoopIo>::read_input(self, max_bytes) })
    }

    /// Runs the write styled output with modes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_styled_output_with_modes<'a>(
        &'a mut self,
        lines: &'a [String],
        line_style_spans: &'a [Vec<crate::terminal::TerminalStyleSpan>],
        modes: AttachedTerminalOutputModes,
    ) -> super::AsyncTerminalIoFuture<'a, usize> {
        Box::pin(async move {
            <Self as AttachedTerminalClientLoopIo>::write_styled_output_with_modes(
                self,
                lines,
                line_style_spans,
                modes,
            )
        })
    }

    /// Runs the terminal size operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn terminal_size<'a>(&'a mut self) -> super::AsyncTerminalIoFuture<'a, Option<Size>> {
        Box::pin(async move { <Self as AttachedTerminalClientLoopIo>::terminal_size(self) })
    }

    /// Runs the invalidate output frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn invalidate_output_frame<'a>(&'a mut self) -> super::AsyncTerminalIoFuture<'a, ()> {
        Box::pin(
            async move { <Self as AttachedTerminalClientLoopIo>::invalidate_output_frame(self) },
        )
    }
}

/// Carries Idle Async Attached Terminal Loop Io state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
struct IdleAsyncAttachedTerminalLoopIo {
    /// Stores the write count value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    write_count: StdArc<AtomicUsize>,
    /// Stores the write notify value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    write_notify: StdArc<tokio::sync::Notify>,
}

impl IdleAsyncAttachedTerminalLoopIo {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn new(write_count: StdArc<AtomicUsize>, write_notify: StdArc<tokio::sync::Notify>) -> Self {
        Self {
            write_count,
            write_notify,
        }
    }
}

impl AsyncAttachedTerminalIo for IdleAsyncAttachedTerminalLoopIo {
    /// Runs the poll readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_readiness<'a>(
        &'a mut self,
    ) -> super::AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        Box::pin(std::future::pending())
    }

    /// Runs the poll input readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_input_readiness<'a>(
        &'a mut self,
    ) -> super::AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        Box::pin(std::future::pending())
    }

    /// Runs the read input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_input<'a>(
        &'a mut self,
        _max_bytes: usize,
    ) -> super::AsyncTerminalIoFuture<'a, Vec<u8>> {
        Box::pin(std::future::pending())
    }

    /// Runs the write styled output with modes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_styled_output_with_modes<'a>(
        &'a mut self,
        lines: &'a [String],
        _line_style_spans: &'a [Vec<crate::terminal::TerminalStyleSpan>],
        _modes: AttachedTerminalOutputModes,
    ) -> super::AsyncTerminalIoFuture<'a, usize> {
        Box::pin(async move {
            self.write_count.fetch_add(1, Ordering::SeqCst);
            self.write_notify.notify_waiters();
            Ok(lines.iter().map(String::len).sum())
        })
    }
}

/// Attached-terminal fake that idles for input while counting output-frame
/// invalidations.
#[derive(Debug)]
struct InvalidatingIdleAsyncAttachedTerminalLoopIo {
    /// Number of completed foreground frame writes.
    write_count: StdArc<AtomicUsize>,
    /// Notification emitted after each completed foreground frame write.
    write_notify: StdArc<tokio::sync::Notify>,
    /// Number of times retained differential output state was discarded.
    invalidate_count: StdArc<AtomicUsize>,
    /// Terminal size responses returned by foreground size polling.
    terminal_size_batches: Vec<Option<Size>>,
}

impl InvalidatingIdleAsyncAttachedTerminalLoopIo {
    /// Creates an idle output-counting fake for attached-terminal service
    /// tests.
    fn new(
        write_count: StdArc<AtomicUsize>,
        write_notify: StdArc<tokio::sync::Notify>,
        invalidate_count: StdArc<AtomicUsize>,
    ) -> Self {
        Self {
            write_count,
            write_notify,
            invalidate_count,
            terminal_size_batches: Vec::new(),
        }
    }

    /// Replaces the terminal size responses returned by foreground polling.
    fn with_terminal_size_batches(mut self, terminal_size_batches: Vec<Option<Size>>) -> Self {
        self.terminal_size_batches = terminal_size_batches;
        self
    }
}

impl AsyncAttachedTerminalIo for InvalidatingIdleAsyncAttachedTerminalLoopIo {
    /// Parks forever while waiting for ordinary readiness, forcing the service
    /// to rely on runtime-side-effect wakeups.
    fn poll_readiness<'a>(
        &'a mut self,
    ) -> super::AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        Box::pin(std::future::pending())
    }

    /// Parks forever while waiting for input readiness, forcing the service to
    /// rely on runtime-side-effect wakeups.
    fn poll_input_readiness<'a>(
        &'a mut self,
    ) -> super::AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        Box::pin(std::future::pending())
    }

    /// Parks forever when asked to read input because this fake never reports
    /// readable input.
    fn read_input<'a>(
        &'a mut self,
        _max_bytes: usize,
    ) -> super::AsyncTerminalIoFuture<'a, Vec<u8>> {
        Box::pin(std::future::pending())
    }

    /// Counts a completed frame write and wakes the test driver.
    fn write_styled_output_with_modes<'a>(
        &'a mut self,
        lines: &'a [String],
        _line_style_spans: &'a [Vec<crate::terminal::TerminalStyleSpan>],
        _modes: AttachedTerminalOutputModes,
    ) -> super::AsyncTerminalIoFuture<'a, usize> {
        Box::pin(async move {
            self.write_count.fetch_add(1, Ordering::SeqCst);
            self.write_notify.notify_waiters();
            Ok(lines.iter().map(String::len).sum())
        })
    }

    /// Counts a retained-frame invalidation before the next full repaint.
    fn invalidate_output_frame<'a>(&'a mut self) -> super::AsyncTerminalIoFuture<'a, ()> {
        Box::pin(async move {
            self.invalidate_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    /// Returns the next configured terminal size response for resize polling.
    fn terminal_size<'a>(&'a mut self) -> super::AsyncTerminalIoFuture<'a, Option<Size>> {
        Box::pin(async move {
            if self.terminal_size_batches.is_empty() {
                return Ok(None);
            }
            Ok(self.terminal_size_batches.remove(0))
        })
    }
}

/// Attached-terminal fake with caller-controlled stale pending output.
#[derive(Debug)]
struct SupersedablePendingOutputIo {
    /// Completed latest-frame writes.
    write_count: StdArc<AtomicUsize>,
    /// Notification emitted after a latest-frame write.
    write_notify: StdArc<tokio::sync::Notify>,
    /// Simulated stale pending bytes from a partially written older frame.
    pending_output_bytes: StdArc<AtomicUsize>,
    /// Count of stale pending-output flush attempts.
    stale_flushes: StdArc<AtomicUsize>,
}

impl SupersedablePendingOutputIo {
    /// Builds a fake output endpoint with externally controlled pending bytes.
    fn new(
        write_count: StdArc<AtomicUsize>,
        write_notify: StdArc<tokio::sync::Notify>,
        pending_output_bytes: StdArc<AtomicUsize>,
        stale_flushes: StdArc<AtomicUsize>,
    ) -> Self {
        Self {
            write_count,
            write_notify,
            pending_output_bytes,
            stale_flushes,
        }
    }
}

impl AsyncAttachedTerminalIo for SupersedablePendingOutputIo {
    /// Returns output writability when a caller asks to flush pending bytes.
    fn poll_readiness<'a>(
        &'a mut self,
    ) -> super::AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        Box::pin(async move {
            Ok(vec![AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Output,
                fd: 1,
                interest: TerminalFdInterest::write(),
                readable: false,
                writable: true,
                hangup: false,
                error: false,
            }])
        })
    }

    /// Leaves input idle so render timing is the only wake source.
    fn poll_input_readiness<'a>(
        &'a mut self,
    ) -> super::AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        Box::pin(std::future::pending())
    }

    /// Leaves input idle so render timing is the only wake source.
    fn read_input<'a>(
        &'a mut self,
        _max_bytes: usize,
    ) -> super::AsyncTerminalIoFuture<'a, Vec<u8>> {
        Box::pin(std::future::pending())
    }

    /// Records that the latest frame replaced any stale pending output.
    fn write_styled_output_with_modes<'a>(
        &'a mut self,
        lines: &'a [String],
        _line_style_spans: &'a [Vec<crate::terminal::TerminalStyleSpan>],
        _modes: AttachedTerminalOutputModes,
    ) -> super::AsyncTerminalIoFuture<'a, usize> {
        Box::pin(async move {
            self.pending_output_bytes.store(0, Ordering::SeqCst);
            self.write_count.fetch_add(1, Ordering::SeqCst);
            self.write_notify.notify_waiters();
            Ok(lines.iter().map(String::len).sum())
        })
    }

    /// Returns the simulated stale pending byte count.
    fn pending_output_bytes(&self) -> usize {
        self.pending_output_bytes.load(Ordering::SeqCst)
    }

    /// Records an obsolete pending-frame flush attempt.
    fn flush_pending_output<'a>(
        &'a mut self,
        _max_bytes: usize,
    ) -> super::AsyncTerminalIoFuture<'a, AsyncTerminalOutputWriteReport> {
        Box::pin(async move {
            self.stale_flushes.fetch_add(1, Ordering::SeqCst);
            let pending = self.pending_output_bytes.load(Ordering::SeqCst);
            Ok(AsyncTerminalOutputWriteReport {
                bytes_written: pending.min(1),
                completed: false,
                pending_bytes: pending,
            })
        })
    }
}

/// Attached-terminal fake that writes output in bounded chunks while still
/// allowing input readiness to be observed between incomplete frame flushes.
#[derive(Debug)]
struct SlowOutputAttachedTerminalLoopIo {
    /// Readiness batches returned by the foreground wait path.
    readiness_batches: Vec<Vec<AttachedTerminalFdReadiness>>,
    /// Input payloads returned when input readiness is observed.
    input_batches: Vec<Vec<u8>>,
    /// Maximum bytes accepted by one fake output write attempt.
    write_limit: usize,
    /// Bytes retained from a started but incomplete output frame.
    pending_output_bytes: usize,
    /// Number of fully completed output frames.
    completed_frames: usize,
    /// Number of partial write attempts.
    partial_writes: usize,
    /// Total bytes written by this fake.
    bytes_written: usize,
}

impl SlowOutputAttachedTerminalLoopIo {
    /// Creates a slow output fake with no started output frame.
    fn new(
        readiness_batches: Vec<Vec<AttachedTerminalFdReadiness>>,
        input_batches: Vec<Vec<u8>>,
        write_limit: usize,
    ) -> Self {
        Self {
            readiness_batches,
            input_batches,
            write_limit,
            pending_output_bytes: 0,
            completed_frames: 0,
            partial_writes: 0,
            bytes_written: 0,
        }
    }

    /// Returns the approximate encoded frame size used by this test fake.
    fn frame_bytes(lines: &[String]) -> usize {
        lines.iter().map(|line| line.len()).sum::<usize>().max(1)
    }

    /// Writes from the retained fake output frame using the supplied bound.
    fn write_pending_output(&mut self, max_bytes: usize) -> Result<AsyncTerminalOutputWriteReport> {
        if max_bytes == 0 {
            return Err(MezError::invalid_args(
                "test output write limit must be greater than zero",
            ));
        }
        if self.pending_output_bytes == 0 {
            return Ok(AsyncTerminalOutputWriteReport::completed(0));
        }
        let accepted = self
            .pending_output_bytes
            .min(max_bytes)
            .min(self.write_limit);
        self.pending_output_bytes = self.pending_output_bytes.saturating_sub(accepted);
        self.bytes_written = self.bytes_written.saturating_add(accepted);
        if self.pending_output_bytes == 0 {
            self.completed_frames = self.completed_frames.saturating_add(1);
            Ok(AsyncTerminalOutputWriteReport::completed(accepted))
        } else {
            self.partial_writes = self.partial_writes.saturating_add(1);
            Ok(AsyncTerminalOutputWriteReport {
                bytes_written: accepted,
                completed: false,
                pending_bytes: self.pending_output_bytes,
            })
        }
    }
}

impl AsyncAttachedTerminalIo for SlowOutputAttachedTerminalLoopIo {
    /// Returns the next prepared readiness batch.
    fn poll_readiness<'a>(
        &'a mut self,
    ) -> super::AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        Box::pin(async move {
            if self.readiness_batches.is_empty() {
                return Ok(Vec::new());
            }
            Ok(self.readiness_batches.remove(0))
        })
    }

    /// Returns input-oriented readiness without synthetic output readiness.
    fn poll_input_readiness<'a>(
        &'a mut self,
    ) -> super::AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        Box::pin(async move {
            if self.readiness_batches.is_empty() {
                return Ok(Vec::new());
            }
            Ok(self
                .readiness_batches
                .remove(0)
                .into_iter()
                .filter(|ready| {
                    ready.role != AttachedTerminalFdRole::Output || ready.hangup || ready.error
                })
                .collect())
        })
    }

    /// Returns the next prepared input payload.
    fn read_input<'a>(&'a mut self, max_bytes: usize) -> super::AsyncTerminalIoFuture<'a, Vec<u8>> {
        Box::pin(async move {
            if self.input_batches.is_empty() {
                return Ok(Vec::new());
            }
            let mut input = self.input_batches.remove(0);
            input.truncate(max_bytes);
            Ok(input)
        })
    }

    /// Writes an entire fake output frame, looping internally across chunks.
    fn write_styled_output_with_modes<'a>(
        &'a mut self,
        lines: &'a [String],
        _line_style_spans: &'a [Vec<TerminalStyleSpan>],
        _modes: AttachedTerminalOutputModes,
    ) -> super::AsyncTerminalIoFuture<'a, usize> {
        Box::pin(async move {
            if self.pending_output_bytes == 0 {
                self.pending_output_bytes = Self::frame_bytes(lines);
            }
            let starting_bytes = self.pending_output_bytes;
            while self.pending_output_bytes > 0 {
                self.write_pending_output(usize::MAX)?;
            }
            Ok(starting_bytes)
        })
    }

    /// Returns retained fake output bytes.
    fn pending_output_bytes(&self) -> usize {
        self.pending_output_bytes
    }

    /// Flushes retained fake output bytes using a bounded chunk size.
    fn flush_pending_output<'a>(
        &'a mut self,
        max_bytes: usize,
    ) -> super::AsyncTerminalIoFuture<'a, AsyncTerminalOutputWriteReport> {
        Box::pin(async move { self.write_pending_output(max_bytes) })
    }

    /// Starts or continues a bounded fake output frame write.
    fn write_styled_output_with_modes_bounded<'a>(
        &'a mut self,
        lines: &'a [String],
        _line_style_spans: &'a [Vec<TerminalStyleSpan>],
        _modes: AttachedTerminalOutputModes,
        max_bytes: usize,
    ) -> super::AsyncTerminalIoFuture<'a, AsyncTerminalOutputWriteReport> {
        Box::pin(async move {
            if self.pending_output_bytes == 0 {
                self.pending_output_bytes = Self::frame_bytes(lines);
            }
            self.write_pending_output(max_bytes)
        })
    }
}

/// Verifies that the async runtime event model preserves the actor-facing
/// delivery order and exposes stable event-family names. The Tokio refactor will
/// eventually route client, pane, provider, process, hook, timer, and shutdown
/// stimuli through this model, so tests need a simple invariant that catches
/// accidental reordering or ad hoc string changes before production I/O starts
/// using the channel.
#[test]
fn async_runtime_event_batch_preserves_delivery_order() {
    let client_id = ClientId::parse('c', "c1").unwrap();
    let mut batch = RuntimeEventBatch::new();
    batch.push(RuntimeEvent::Client(ClientEvent::Input {
        client_id: client_id.clone(),
        bytes: b"abc".to_vec(),
    }));
    batch.push(RuntimeEvent::Pane(PaneEvent::Output {
        pane_id: "%1".to_string(),
        bytes: b"pane-output".to_vec(),
    }));
    batch.push(RuntimeEvent::Timer(TimerEvent {
        key: RuntimeTimerKey::new(RuntimeTimerKind::ShellTransaction, "turn-1", 7),
        now_ms: 42,
    }));

    assert_eq!(batch.families(), vec!["client", "pane", "timer"]);
    assert_eq!(batch.events[0].family(), "client");
    assert_eq!(batch.events[1].family(), "pane");
    assert_eq!(batch.events[2].family(), "timer");

    let effect = RuntimeSideEffect::RenderClient {
        client_id,
        reason: RenderInvalidationReason::FullRedraw,
    };
    assert!(matches!(
        effect,
        RuntimeSideEffect::RenderClient {
            reason: RenderInvalidationReason::FullRedraw,
            ..
        }
    ));
}

/// Verifies that typed runtime events can cross the async actor boundary through
/// the same serialized request channel used by legacy compatibility requests.
/// Non-mutating event families are accepted without side effects, while later
/// tests cover mutating pane output. Keeping both paths explicit prevents the
/// event channel from silently accepting events that should have state effects.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_accepts_runtime_event_batches_in_order() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let client_id = ClientId::parse('c', "c1").unwrap();
    let mut batch = RuntimeEventBatch::new();
    batch.push(RuntimeEvent::Client(ClientEvent::Resize {
        client_id,
        size: Size::new(100, 30).unwrap(),
    }));
    batch.push(RuntimeEvent::Pane(PaneEvent::InputWritten {
        pane_id: "%1".to_string(),
        bytes: 12,
    }));

    let client = async {
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.applied, 0);
        assert_eq!(report.families, vec!["client", "pane"]);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 2);
}

/// Verifies that a primary-client resize delivered through typed async runtime
/// events mutates authoritative terminal geometry through the actor instead of
/// the compatibility resize request. Resize events are high-frequency terminal
/// stimuli, so this guards the migration invariant that stale/non-primary
/// events are harmless while active primary events use the established pane
/// geometry and render-invalidation path.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_primary_client_resize_events() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::Resize {
            client_id: primary,
            size: Size::new(100, 30).unwrap(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.session().authoritative_size,
        Size::new(100, 30).unwrap()
    );
    assert_eq!(exit.commands_processed, 2);
}

/// Verifies that a foreground resize signal can wake the render path without
/// directly mutating geometry in the actor event. The attached terminal service
/// owns the actual terminal-size read, so the signal event should only enqueue
/// a resize render invalidation for the target client.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_client_resize_signal_as_render_invalidation() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::ResizeSignal {
            client_id: primary.clone(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        let effects = handle
            .drain_render_side_effects_for_client(primary.clone(), 8)
            .await
            .unwrap();
        assert_eq!(
            effects,
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::Resize,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.session().authoritative_size,
        Size::new(80, 24).unwrap()
    );
    assert_eq!(exit.commands_processed, 3);
}

/// Verifies that primary-client input delivered as a typed runtime event uses
/// the normal terminal planner and applies the resulting client step through
/// the serialized actor. This protects mux key handling during the migration
/// away from compatibility client-loop requests: the input bytes are external
/// stimuli, while split-pane state mutation remains actor-owned.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_primary_client_input_events() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::Input {
            client_id: primary,
            bytes: b"\x01%".to_vec(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let pane_count = exit
        .service
        .session()
        .active_window()
        .map(|window| window.panes().len())
        .unwrap_or_default();
    assert_eq!(pane_count, 2);
    assert_eq!(exit.commands_processed, 2);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that client output-readiness events are applied only for attached
/// clients and enqueue a render side effect without composing or writing the
/// frame inside the actor. This keeps slow or backpressured frame delivery on
/// the side-effect boundary while still waking the eventual render worker as
/// soon as stdout becomes writable again.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_client_output_ready_events_as_render_side_effects() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::OutputReady {
            client_id: primary.clone(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.drain_runtime_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::FullRedraw,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 3);
}

/// Verifies that primary-client disconnects can be applied from async runtime
/// event ingress without a nested terminal loop owning detach behavior. This
/// covers fd hangup and attached-client task shutdown paths where the actor
/// must record a normal primary detach plus a diagnostic reason while leaving
/// stale observer or non-primary events non-mutating.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_primary_client_disconnect_events() {
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::Disconnected {
            client_id: primary,
            reason: "terminal input hangup".to_string(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 0);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Detached
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.lifecycle_state(),
        RuntimeLifecycleState::Detached
    );
    assert!(
        exit.service
            .session()
            .clients()
            .iter()
            .all(|client| client.state != ClientState::Attached)
    );
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .iter()
        .map(|event| event.payload.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        events.contains(r#""client_disconnect":"primary""#),
        "{events}"
    );
    assert!(
        events.contains(r#""reason":"terminal input hangup""#),
        "{events}"
    );
    assert_eq!(exit.commands_processed, 2);
}

/// Verifies that render-only runtime timers become actor-owned side-effect
/// producers instead of accepted-only bookkeeping. Resize debounce and cursor
/// blink timers should not mutate session state, but they must wake frame
/// delivery so attached clients repaint at the correct moments without a blind
/// compatibility tick.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_render_timer_events_as_side_effects() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let resize_key =
            RuntimeTimerKey::new(RuntimeTimerKind::ResizeDebounce, primary.as_str(), 1);
        let cursor_key = RuntimeTimerKey::new(RuntimeTimerKind::CursorBlink, primary.as_str(), 2);
        let status_key = RuntimeTimerKey::new(RuntimeTimerKind::StatusRefresh, primary.as_str(), 3);
        handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::ScheduleTimer {
                    key: resize_key.clone(),
                    delay_ms: 1,
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: cursor_key.clone(),
                    delay_ms: 1,
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: status_key.clone(),
                    delay_ms: 1,
                },
            ])
            .await
            .unwrap();
        let scheduled = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(scheduled.len(), 3);

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: resize_key,
            now_ms: 100,
        }));
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: cursor_key,
            now_ms: 200,
        }));
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: status_key,
            now_ms: 300,
        }));
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: RuntimeTimerKey::new(RuntimeTimerKind::IdleCleanup, "session", 4),
            now_ms: 400,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 4);
        assert_eq!(report.applied, 3);
        assert_eq!(report.side_effects, 4);
        let side_effects = handle.drain_runtime_side_effects(8).await.unwrap();
        assert_eq!(
            side_effects,
            vec![
                RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::Resize,
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::StatusRefresh,
                        primary.as_str(),
                        1300,
                    ),
                    delay_ms: 1000,
                },
            ]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 5);
    assert_eq!(exit.metrics.runtime_timer_events_ignored, 1);
}

/// Verifies that resize debounce timer events are generation checked by the
/// actor before producing a render invalidation. Rapid resize activity cancels
/// the old debounce key and schedules a new one, so a late firing for the stale
/// key must not force another full-size repaint.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_ignores_stale_resize_debounce_timer_events() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let stale_key = RuntimeTimerKey::new(RuntimeTimerKind::ResizeDebounce, primary.as_str(), 1);
        let active_key =
            RuntimeTimerKey::new(RuntimeTimerKind::ResizeDebounce, primary.as_str(), 2);
        handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::ScheduleTimer {
                    key: stale_key.clone(),
                    delay_ms: 50,
                },
                RuntimeSideEffect::CancelTimer {
                    key: stale_key.clone(),
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: active_key.clone(),
                    delay_ms: 50,
                },
            ])
            .await
            .unwrap();
        let scheduled = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(scheduled.len(), 3);

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: stale_key,
            now_ms: 100,
        }));
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: active_key,
            now_ms: 100,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.drain_runtime_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::Resize,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 5);
}

/// Verifies that status-line refresh timers are owned per attached client and
/// generation checked before repainting. Runtime status fields such as uptime
/// and local datetime update periodically, but a cancelled or superseded timer
/// must not create an extra frame when an older deadline fires late.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_ignores_stale_status_refresh_timer_events() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let stale_key = RuntimeTimerKey::new(RuntimeTimerKind::StatusRefresh, primary.as_str(), 1);
        let active_key = RuntimeTimerKey::new(RuntimeTimerKind::StatusRefresh, primary.as_str(), 2);
        handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::ScheduleTimer {
                    key: stale_key.clone(),
                    delay_ms: 1000,
                },
                RuntimeSideEffect::CancelTimer {
                    key: stale_key.clone(),
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: active_key.clone(),
                    delay_ms: 1000,
                },
            ])
            .await
            .unwrap();
        let scheduled = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(scheduled.len(), 3);

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: stale_key,
            now_ms: 1000,
        }));
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: active_key,
            now_ms: 1000,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 2);
        let side_effects = handle.drain_runtime_side_effects(8).await.unwrap();
        assert_eq!(side_effects.len(), 2);
        assert_eq!(
            side_effects[0],
            RuntimeSideEffect::RenderClient {
                client_id: primary.clone(),
                reason: RenderInvalidationReason::StatusLine,
            }
        );
        let RuntimeSideEffect::ScheduleTimer { key, delay_ms } = &side_effects[1] else {
            panic!("expected status refresh reschedule: {side_effects:?}");
        };
        assert_eq!(key.kind, RuntimeTimerKind::StatusRefresh);
        assert_eq!(key.owner_id, primary.to_string());
        assert_eq!(*delay_ms, 1000);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 5);
}

/// Verifies that render workers can drain only render invalidations while
/// leaving provider dispatches queued for provider workers. This protects the
/// side-effect queue from family-specific workers stealing unrelated work as
/// the async runtime grows more concrete worker services.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_drains_render_side_effects_without_stealing_provider_dispatches() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let expected_turn = pending[0].turn_id.clone();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let provider_key =
            RuntimeTimerKey::new(RuntimeTimerKind::ProviderPoll, "agent-provider", 1);
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer {
                key: provider_key.clone(),
                delay_ms: 1,
            }])
            .await
            .unwrap();
        assert_eq!(handle.drain_timer_side_effects(8).await.unwrap().len(), 1);

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: provider_key,
            now_ms: 1,
        }));
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"render-worker-preserve\n".to_vec(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.applied, 2);
        assert_eq!(report.side_effects, 3);

        let render_effects = handle.drain_render_side_effects(8).await.unwrap();
        assert_eq!(
            render_effects,
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::PaneOutput,
            }]
        );
        assert_eq!(
            handle
                .drain_agent_provider_dispatch_side_effects(8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: expected_agent,
                turn_id: expected_turn,
            }]
        );
        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(timer_effects.len(), 1);
        assert!(
            matches!(
                &timer_effects[0],
                RuntimeSideEffect::ScheduleTimer { key, .. }
                    if key.kind == RuntimeTimerKind::IdleCleanup
            ),
            "{timer_effects:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_queued, 4);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 4);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies manual `/compact` publishes visible compaction state and queues a
/// provider-side compaction dispatch instead of blocking the actor while the
/// model request runs. This protects the terminal UI from invisible synchronous
/// compaction work.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_compact_command_queues_compaction_dispatch() {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "async-compact-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "async-compact-test"
[providers.openai]
kind = "openai"
models = ["gpt-compact-test"]
default_model = "gpt-compact-test"
[model_profiles.async-compact-test]
provider = "openai"
model = "gpt-compact-test"
context_window_tokens = 128000
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-async-compact-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&transcript_root).unwrap();
    let transcript_store = AgentTranscriptStore::new(transcript_root);
    for sequence in 1..=3 {
        transcript_store
            .append(&crate::transcript::TranscriptEntry {
                conversation_id: "async-compact".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: crate::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("compact source entry {sequence}"),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "async-compact", 3)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let response = handle
            .execute_agent_shell_command(primary, "/compact".to_string())
            .await
            .unwrap();
        assert!(response.contains("state=queued"), "{response}");
        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert!(dispatches.iter().any(|effect| matches!(
            effect,
            RuntimeSideEffect::DispatchAgentCompaction { pane_id } if pane_id == "%1"
        )));
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let config = exit
        .service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert_eq!(
        config
            .frame_context
            .panes
            .get("%1")
            .and_then(|pane| pane.agent_status.as_deref()),
        Some("compacting")
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies `/compact` submitted through the pane-local prompt queues async
/// compaction dispatch side effects. This covers the attached-terminal path
/// used by interactive agent mode, where `/compact` must not leave only a
/// pending task and a visible `compacting` status.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_compact_submission_queues_compaction_dispatch() {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "async-attached-compact-context-window".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "async-attached-compact-test"
[providers.openai]
kind = "openai"
models = ["gpt-compact-test"]
default_model = "gpt-compact-test"
[model_profiles.async-attached-compact-test]
provider = "openai"
model = "gpt-compact-test"
context_window_tokens = 128000
"#
            .to_string(),
        }])
        .unwrap();
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-async-attached-compact-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&transcript_root).unwrap();
    let transcript_store = AgentTranscriptStore::new(transcript_root);
    for sequence in 1..=3 {
        transcript_store
            .append(&crate::transcript::TranscriptEntry {
                conversation_id: "async-attached-compact".to_string(),
                sequence,
                created_at_unix_seconds: sequence,
                role: crate::transcript::TranscriptRole::Assistant,
                turn_id: format!("turn-{sequence}"),
                agent_id: "agent-%1".to_string(),
                pane_id: "%1".to_string(),
                content: format!("compact source entry {sequence}"),
            })
            .unwrap();
    }
    service.set_agent_transcript_store(transcript_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "async-attached-compact", 3)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            },
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Output,
                fd: 1,
                interest: TerminalFdInterest::write(),
                readable: false,
                writable: true,
                hangup: false,
                error: false,
            },
        ]],
        input_batches: vec![b"/compact\r".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();
        assert_eq!(
            report.actions,
            vec![TerminalClientLoopAction::ForwardToPane(
                b"/compact\r".to_vec()
            )]
        );
        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert!(
            dispatches.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::DispatchAgentCompaction { pane_id } if pane_id == "%1"
            )),
            "{dispatches:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let config = exit
        .service
        .terminal_client_loop_config(TerminalClientLoopConfig::default())
        .unwrap();
    assert_eq!(
        config
            .frame_context
            .panes
            .get("%1")
            .and_then(|pane| pane.agent_status.as_deref()),
        Some("compacting")
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that render drains coalesce redundant invalidations for the same
/// client before a render worker composes frames. The async render queue can
/// receive several causes while a client is not writable; the worker should
/// render that client once, keep the strongest redraw reason, and preserve
/// unrelated side-effect families in queue order.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_coalesces_render_side_effects_by_client() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let other = ClientId::new('c', 9006);
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::PaneOutput,
                },
                RuntimeSideEffect::DispatchAgentProvider {
                    agent_id: AgentId::opaque("agent-%1").unwrap(),
                    turn_id: "turn-1".to_string(),
                },
                RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::FullRedraw,
                },
                RuntimeSideEffect::RenderClient {
                    client_id: other.clone(),
                    reason: RenderInvalidationReason::CursorBlink,
                },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 4);

        let render_effects = handle.drain_render_side_effects(8).await.unwrap();
        assert_eq!(
            render_effects,
            vec![
                RuntimeSideEffect::RenderClient {
                    client_id: primary,
                    reason: RenderInvalidationReason::FullRedraw,
                },
                RuntimeSideEffect::RenderClient {
                    client_id: other,
                    reason: RenderInvalidationReason::CursorBlink,
                },
            ]
        );
        assert_eq!(
            handle.drain_runtime_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: AgentId::opaque("agent-%1").unwrap(),
                turn_id: "turn-1".to_string(),
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_queued, 3);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 3);
    assert_eq!(exit.metrics.render_invalidations_coalesced, 1);
}

/// Verifies that render invalidations are coalesced before the actor applies
/// its bounded queue capacity check. A pane can emit output faster than the
/// attached client redraw path drains invalidations, and redundant redraw
/// requests for the same client must not be able to exhaust the shared
/// side-effect queue.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_coalesces_render_side_effects_before_capacity_check() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeSessionActor::new(
        service,
        AsyncRuntimeActorConfig {
            side_effect_buffer: 2,
            ..AsyncRuntimeActorConfig::default()
        },
    )
    .unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::PaneOutput,
                },
                RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::CursorBlink,
                },
                RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::FullRedraw,
                },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 3);
        assert_eq!(
            handle.drain_render_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::FullRedraw,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_queued, 1);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 1);
    assert_eq!(exit.metrics.render_invalidations_coalesced, 2);
    assert_eq!(exit.metrics.side_effect_queue_high_water, 1);
}

/// Verifies that full client-output flushes are coalesced before bounded queue
/// capacity is checked. Pane output bursts can produce new full-frame flushes
/// faster than a terminal can write them; only the latest pending frame for a
/// client needs to survive because the frame is a complete presentation state.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_coalesces_client_flush_side_effects_before_capacity_check() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) = AsyncRuntimeSessionActor::new(
        service,
        AsyncRuntimeActorConfig {
            side_effect_buffer: 2,
            ..AsyncRuntimeActorConfig::default()
        },
    )
    .unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::FlushClientOutput {
                    client_id: primary.clone(),
                    lines: vec!["stale-one".to_string()],
                    line_style_spans: vec![Vec::new()],
                    modes: AttachedTerminalOutputModes {
                        cursor_column: 1,
                        ..AttachedTerminalOutputModes::default()
                    },
                },
                RuntimeSideEffect::FlushClientOutput {
                    client_id: primary.clone(),
                    lines: vec!["stale-two".to_string()],
                    line_style_spans: vec![Vec::new()],
                    modes: AttachedTerminalOutputModes {
                        cursor_column: 2,
                        ..AttachedTerminalOutputModes::default()
                    },
                },
                RuntimeSideEffect::FlushClientOutput {
                    client_id: primary.clone(),
                    lines: vec!["latest".to_string()],
                    line_style_spans: vec![Vec::new()],
                    modes: AttachedTerminalOutputModes {
                        cursor_column: 3,
                        ..AttachedTerminalOutputModes::default()
                    },
                },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 3);

        let effects = handle
            .drain_client_output_flush_side_effects(Some(primary), 8)
            .await
            .unwrap();
        assert_eq!(effects.len(), 1);
        let RuntimeSideEffect::FlushClientOutput { lines, modes, .. } = &effects[0] else {
            panic!("expected retained client output flush, got {effects:?}");
        };
        assert_eq!(lines, &vec!["latest".to_string()]);
        assert_eq!(modes.cursor_column, 3);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_queued, 1);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 1);
    assert_eq!(exit.metrics.render_invalidations_coalesced, 2);
    assert_eq!(exit.metrics.side_effect_queue_high_water, 1);
}

/// Verifies that registry persistence side effects are coalesced before queue
/// capacity is checked. Registry writes describe the latest discoverable
/// session state, so a burst only needs the newest pending update for that
/// session rather than a queue entry per intermediate state.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_coalesces_registry_persistence_before_capacity_check() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-registry-coalesce-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), current_effective_uid());
    let mut service = test_service();
    service.set_session_registry(registry.clone());
    let update = service.registry_update_plan();
    let (handle, actor) = AsyncRuntimeSessionActor::new(
        service,
        AsyncRuntimeActorConfig {
            side_effect_buffer: 2,
            ..AsyncRuntimeActorConfig::default()
        },
    )
    .unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::PersistRegistry {
                    registry: registry.clone(),
                    update: update.clone(),
                },
                RuntimeSideEffect::PersistRegistry {
                    registry: registry.clone(),
                    update: update.clone(),
                },
                RuntimeSideEffect::PersistRegistry { registry, update },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 3);

        let effects = handle.drain_persistence_side_effects(8).await.unwrap();
        assert_eq!(effects.len(), 1);
        assert!(
            matches!(effects[0], RuntimeSideEffect::PersistRegistry { .. }),
            "{effects:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_queued, 1);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 1);
    assert_eq!(exit.metrics.render_invalidations_coalesced, 2);
    assert_eq!(exit.metrics.side_effect_queue_high_water, 1);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that raw pane-output bursts do not enqueue registry persistence.
/// Pane output changes the terminal screen and event stream, but it does not
/// change the discoverable session registry record; persisting after every PTY
/// read can overflow the bounded side-effect queue during high-volume output.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_does_not_persist_registry_for_pane_output_bursts() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-registry-output-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), current_effective_uid());
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service.set_session_registry(registry);
    let (handle, actor) = AsyncRuntimeSessionActor::new(
        service,
        AsyncRuntimeActorConfig {
            side_effect_buffer: 2,
            ..AsyncRuntimeActorConfig::default()
        },
    )
    .unwrap();

    let client = async {
        for index in 0..128 {
            let mut batch = RuntimeEventBatch::new();
            batch.push(RuntimeEvent::Pane(PaneEvent::Output {
                pane_id: "%1".to_string(),
                bytes: format!("burst-output-{index}\n").into_bytes(),
            }));
            let report = handle.submit_runtime_events(batch).await.unwrap();
            assert_eq!(report.accepted, 1);
            assert_eq!(report.applied, 1);
            assert_eq!(report.side_effects, 1);
        }

        let persistence = handle.drain_persistence_side_effects(8).await.unwrap();
        assert!(
            persistence.is_empty(),
            "pane output should not queue registry persistence: {persistence:?}"
        );
        let render = handle.drain_render_side_effects(8).await.unwrap();
        assert_eq!(
            render,
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::PaneOutput,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.side_effect_queue_high_water, 1);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that a foreground attached client can drain only its own render
/// invalidations while preserving other clients and side-effect families. This
/// lets the live foreground service wake on side-effect notifications without
/// treating unrelated timer, provider, or observer work as a redraw request.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_drains_render_side_effects_for_one_client() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let other = ClientId::new('c', 9016);
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::RenderClient {
                    client_id: other.clone(),
                    reason: RenderInvalidationReason::CursorBlink,
                },
                RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::PaneOutput,
                },
                RuntimeSideEffect::DispatchAgentProvider {
                    agent_id: AgentId::opaque("agent-%1").unwrap(),
                    turn_id: "turn-1".to_string(),
                },
                RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::FullRedraw,
                },
            ])
            .await
            .unwrap();

        assert_eq!(
            handle
                .drain_render_side_effects_for_client(primary.clone(), 8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::FullRedraw,
            }]
        );
        assert_eq!(
            handle.drain_render_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: other,
                reason: RenderInvalidationReason::CursorBlink,
            }]
        );
        assert_eq!(
            handle.drain_runtime_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: AgentId::opaque("agent-%1").unwrap(),
                turn_id: "turn-1".to_string(),
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 5);
}

/// Verifies that the concrete render side-effect service converts actor-owned
/// render invalidations into styled client-output flush effects. The service
/// composes frames through the actor and hands the resulting flush to a worker
/// callback without draining unrelated side-effect families.
#[tokio::test(flavor = "current_thread")]
async fn async_render_side_effect_service_composes_flush_effects() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let flushed = StdArc::new(Mutex::new(Vec::new()));
    let flushed_for_service = flushed.clone();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"render-service-output\n".to_vec(),
        }));
        let ingress = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(ingress.side_effects, 1);

        let report = run_async_render_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            TerminalClientLoopConfig::default(),
            |_, _| Ok(None),
            |effect| {
                flushed_for_service.lock().unwrap().push(effect);
                Ok(())
            },
            |_, _| false,
        )
        .await
        .unwrap();
        assert_eq!(report.polls, 1);
        assert_eq!(report.drained, 1);
        assert_eq!(report.applied, 1);
        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(timers.len(), 1);
        let RuntimeSideEffect::ScheduleTimer { key, delay_ms } = &timers[0] else {
            panic!("expected status refresh timer: {timers:?}");
        };
        assert_eq!(key.kind, RuntimeTimerKind::StatusRefresh);
        assert_eq!(key.owner_id, primary.to_string());
        assert_eq!(*delay_ms, 1000);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_drained, 2);
    let flushed = flushed.lock().unwrap();
    assert_eq!(flushed.len(), 1);
    let RuntimeSideEffect::FlushClientOutput {
        client_id,
        lines,
        line_style_spans,
        modes,
    } = &flushed[0]
    else {
        panic!("render service should emit a flush side effect");
    };
    assert_eq!(client_id, &primary);
    assert_eq!(lines.len(), line_style_spans.len());
    assert!(
        lines
            .iter()
            .any(|line| line.contains("render-service-output"))
    );
    assert!(modes.cursor_visible);
}

/// Verifies that active agent pane status can drive status refresh timers even
/// when the window status line is disabled. The running status pill has an
/// animated scan background, so pane-frame-only configurations need the
/// animation refresh cadence while agent work is active.
#[tokio::test(flavor = "current_thread")]
async fn async_render_side_effect_service_refreshes_active_agent_status_without_window_status() {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[frames.window]\nenabled = false\n[frames.pane]\nenabled = true\n".to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                client_id: primary.clone(),
                reason: RenderInvalidationReason::PaneOutput,
            }])
            .await
            .unwrap();
        let report = run_async_render_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            TerminalClientLoopConfig::default(),
            |_, _| Ok(None),
            |_| Ok(()),
            |_, _| false,
        )
        .await
        .unwrap();
        assert_eq!(report.applied, 1);

        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert!(
            timers.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                    if key.kind == RuntimeTimerKind::StatusRefresh
                        && key.owner_id == primary.to_string()
                        && *delay_ms == 180
            )),
            "{timers:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed > 0);
}

/// Verifies that an existing slow status refresh timer is replaced by the
/// animation cadence when an agent starts running. A window status timer may
/// already exist before the prompt is submitted, but active agent indicators
/// should not wait for that slower deadline before animating.
#[tokio::test(flavor = "current_thread")]
async fn async_render_side_effect_service_retargets_status_refresh_for_agent_animation() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                client_id: primary.clone(),
                reason: RenderInvalidationReason::PaneOutput,
            }])
            .await
            .unwrap();
        run_async_render_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            TerminalClientLoopConfig::default(),
            |_, _| Ok(None),
            |_| Ok(()),
            |_, _| false,
        )
        .await
        .unwrap();
        let initial_timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert!(
            initial_timers.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                    if key.kind == RuntimeTimerKind::StatusRefresh
                        && key.owner_id == primary.to_string()
                        && *delay_ms == 1000
            )),
            "{initial_timers:?}"
        );

        let start = handle
            .execute_agent_shell_command(primary.clone(), "summarize the pane".to_string())
            .await
            .unwrap();
        assert!(start.contains(r#""state":"running""#), "{start}");
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                client_id: primary.clone(),
                reason: RenderInvalidationReason::AgentPrompt,
            }])
            .await
            .unwrap();
        run_async_render_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            TerminalClientLoopConfig::default(),
            |_, _| Ok(None),
            |_| Ok(()),
            |_, _| false,
        )
        .await
        .unwrap();
        let animation_timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert!(
            animation_timers.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::CancelTimer { key }
                    if key.kind == RuntimeTimerKind::StatusRefresh
                        && key.owner_id == primary.to_string()
            )),
            "{animation_timers:?}"
        );
        assert!(
            animation_timers.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                    if key.kind == RuntimeTimerKind::StatusRefresh
                        && key.owner_id == primary.to_string()
                        && *delay_ms == 180
            )),
            "{animation_timers:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed > 0);
}

/// Verifies that flush side effects can be queued through the actor and drained
/// independently by the client output worker. This is the output half of the
/// render pipeline: render workers can enqueue styled flushes without sharing
/// a mutable queue, and output workers can write them without stealing render
/// or provider side effects.
#[tokio::test(flavor = "current_thread")]
async fn async_client_output_flush_service_writes_styled_flush_effects() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let other_client = ClientId::new('c', 9005);
        let queued = handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::FlushClientOutput {
                    client_id: primary.clone(),
                    lines: vec!["flush-one".to_string(), "flush-two".to_string()],
                    line_style_spans: vec![Vec::new(), Vec::new()],
                    modes: AttachedTerminalOutputModes {
                        cursor_visible: true,
                        cursor_row: 1,
                        cursor_column: 2,
                        ..AttachedTerminalOutputModes::default()
                    },
                },
                RuntimeSideEffect::FlushClientOutput {
                    client_id: other_client.clone(),
                    lines: vec!["other-client".to_string()],
                    line_style_spans: vec![Vec::new()],
                    modes: AttachedTerminalOutputModes::default(),
                },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 2);

        let mut io = AsyncFakeAttachedTerminalIo::default();
        let report = run_async_client_output_flush_service(
            &handle,
            primary.clone(),
            &mut io,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        assert_eq!(report.polls, 1);
        assert_eq!(report.drained, 1);
        assert_eq!(report.flushed, 1);
        assert_eq!(report.output_hangups, 0);
        assert_eq!(io.written_frames.len(), 1);
        assert_eq!(
            io.written_frames[0].lines,
            vec!["flush-one".to_string(), "flush-two".to_string()]
        );
        assert_eq!(io.written_frames[0].line_style_spans.len(), 2);
        assert_eq!(io.written_frames[0].modes.cursor_column, 2);
        let retained = handle
            .drain_client_output_flush_side_effects(Some(other_client), 8)
            .await
            .unwrap();
        assert_eq!(retained.len(), 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_queued, 2);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 2);
}

/// Verifies that the client output worker prefers a newer frame over stale
/// pending output.
///
/// Slow terminal transports can return from a bounded write with bytes still
/// pending. If a newer frame is already queued, the worker should materialize
/// the latest state instead of spending bandwidth on obsolete frame bytes.
#[tokio::test(flavor = "current_thread")]
async fn async_client_output_flush_service_prefers_new_frame_over_stale_pending_output() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let pending_output_bytes = StdArc::new(AtomicUsize::new(256));
    let stale_flushes = StdArc::new(AtomicUsize::new(0));

    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::FlushClientOutput {
                client_id: primary.clone(),
                lines: vec!["newer frame".to_string()],
                line_style_spans: vec![Vec::new()],
                modes: AttachedTerminalOutputModes::default(),
            }])
            .await
            .unwrap();
        let mut io = SupersedablePendingOutputIo::new(
            write_count.clone(),
            write_notify,
            pending_output_bytes.clone(),
            stale_flushes.clone(),
        );

        let report = run_async_client_output_flush_service(
            &handle,
            primary.clone(),
            &mut io,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |_, _| false,
        )
        .await
        .unwrap();

        assert_eq!(report.polls, 1);
        assert_eq!(report.drained, 1);
        assert_eq!(report.flushed, 1);
        assert_eq!(report.partial_writes, 0);
        assert!(report.bytes_written > 0);
        assert_eq!(report.pending_output_bytes, 0);
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        assert_eq!(stale_flushes.load(Ordering::SeqCst), 0);
        assert_eq!(pending_output_bytes.load(Ordering::SeqCst), 0);
        assert_eq!(
            handle
                .drain_client_output_flush_side_effects(Some(primary), 8)
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_side_effects_queued, 1);
}

/// Verifies that async hook worker results are no longer accepted-only events.
/// The worker event model currently carries diagnostics rather than full hook
/// pipeline continuation state, so the actor applies completion and failure
/// events as replayable session diagnostics while preserving event ordering for
/// subscribers.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_hook_completion_events_to_event_log() {
    let (handle, actor) = AsyncRuntimeSessionActor::new(
        test_service_with_event_log(),
        AsyncRuntimeActorConfig::default(),
    )
    .unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Hook(AsyncHookEvent::Completed {
            hook_id: "hook-1".to_string(),
            exit_code: Some(0),
            output_preview: "ok".to_string(),
        }));
        batch.push(RuntimeEvent::Hook(AsyncHookEvent::Failed {
            hook_id: "hook-2".to_string(),
            error: "worker channel closed".to_string(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.applied, 2);
        assert_eq!(report.side_effects, 0);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .iter()
        .map(|event| event.payload.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(events.contains(r#""async_hook":"completed""#), "{events}");
    assert!(events.contains(r#""hook_id":"hook-1""#), "{events}");
    assert!(events.contains(r#""exit_code":0"#), "{events}");
    assert!(events.contains(r#""output_preview":"ok""#), "{events}");
    assert!(events.contains(r#""async_hook":"failed""#), "{events}");
    assert!(events.contains(r#""hook_id":"hook-2""#), "{events}");
    assert!(
        events.contains(r#""error":"worker channel closed""#),
        "{events}"
    );
    assert_eq!(exit.commands_processed, 2);
}

/// Verifies that a typed pane-output runtime event is applied by the actor using
/// the same terminal-screen, OSC, shell-transaction, event-log, and title-update
/// machinery as the legacy polling path. This closes the first production gap
/// between the async pane driver boundary and visible runtime state.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_pane_output_events_to_rendered_view() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"event-output\n".to_vec(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.families, vec!["pane"]);

        let view = handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(
            view.lines.iter().any(|line| line.contains("event-output")),
            "{:?}",
            view.lines
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 3);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that foreground process metadata from an async pane worker updates
/// pane title state through the actor. This protects automatic title refresh
/// after live PTY ownership has moved out of the synchronous manager.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_foreground_process_metadata_to_pane_title() {
    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 1)
        .unwrap();
    service
        .start_initial_pane_process(Some("sleep 30"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::ForegroundProcess {
            pane_id: "%1".to_string(),
            process_name: "vim".to_string(),
            process_group_id: 4242,
            current_working_directory: Some("/tmp/mez-async-title".to_string()),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.families, vec!["pane"]);
        assert!(
            report.side_effects > 0,
            "title changes should invalidate rendered pane frames"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let window = exit.service.session().active_window().unwrap();
    assert_eq!(window.active_pane().title, "vim");
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .iter()
        .map(|event| event.payload.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(events.contains(r#""title":"vim""#), "{events}");
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that state-mutating runtime events can enqueue bounded actor-owned
/// side effects without executing those effects immediately. This is the
/// migration point for render invalidation, pane I/O writes, and other external
/// work that must eventually leave the actor through supervised async workers
/// instead of direct synchronous service calls.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_queues_render_side_effects_for_applied_events() {
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"side-effect-output\n".to_vec(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);

        let effects = handle.drain_runtime_side_effects(8).await.unwrap();
        assert_eq!(
            effects,
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::PaneOutput,
            }]
        );
        assert!(
            handle
                .drain_runtime_side_effects(8)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 4);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that provider-poll timer events convert pending provider work into
/// bounded dispatch side effects without executing provider I/O inside the
/// actor. The second poll before a drain proves the actor does not enqueue
/// duplicate dispatch requests for a turn that already has a dispatch side
/// effect waiting for a supervised provider worker. A queued render effect is
/// left behind for the render worker, proving provider dispatch drains do not
/// steal work from other side-effect families.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_queues_provider_dispatch_side_effects_for_provider_poll_timer() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let task = pending[0].clone();
    let expected_agent = AgentId::opaque(task.agent_id.clone()).unwrap();
    let expected_turn = task.turn_id.clone();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let key = RuntimeTimerKey::new(RuntimeTimerKind::ProviderPoll, "agent-provider", 1);
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer {
                key: key.clone(),
                delay_ms: 1,
            }])
            .await
            .unwrap();
        assert_eq!(handle.drain_timer_side_effects(8).await.unwrap().len(), 1);

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: key.clone(),
            now_ms: 1,
        }));
        let first = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(first.accepted, 1);
        assert_eq!(first.applied, 1);
        assert_eq!(first.side_effects, 1);

        let mut duplicate = RuntimeEventBatch::new();
        duplicate.push(RuntimeEvent::Timer(TimerEvent { key, now_ms: 2 }));
        let second = handle.submit_runtime_events(duplicate).await.unwrap();
        assert_eq!(second.accepted, 1);
        assert_eq!(second.applied, 0);
        assert_eq!(second.side_effects, 0);

        let mut render_batch = RuntimeEventBatch::new();
        render_batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"provider-side-effect-preserve\n".to_vec(),
        }));
        let render_report = handle.submit_runtime_events(render_batch).await.unwrap();
        assert_eq!(render_report.accepted, 1);
        assert_eq!(render_report.applied, 1);
        assert_eq!(render_report.side_effects, 2);

        let effects = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert_eq!(
            effects,
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: expected_agent,
                turn_id: expected_turn,
            }]
        );
        assert_eq!(
            handle.drain_render_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::PaneOutput,
            }]
        );
        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(timer_effects.len(), 1);
        assert!(
            matches!(
                &timer_effects[0],
                RuntimeSideEffect::ScheduleTimer { key, .. }
                    if key.kind == RuntimeTimerKind::IdleCleanup
            ),
            "{timer_effects:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.runtime_event_batches, 3);
    assert_eq!(exit.metrics.runtime_side_effects_queued, 4);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 4);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that provider-poll timer events are generation checked before they
/// dispatch pending model work. Provider polling is a runtime-owned timer
/// family, so a cancelled stale deadline must not wake the provider worker or
/// enqueue duplicate provider dispatch side effects after a newer poll timer is
/// scheduled.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_ignores_stale_provider_poll_timer_events() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let expected_turn = pending[0].turn_id.clone();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let stale_key = RuntimeTimerKey::new(RuntimeTimerKind::ProviderPoll, "agent-provider", 1);
        let active_key = RuntimeTimerKey::new(RuntimeTimerKind::ProviderPoll, "agent-provider", 2);
        handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::ScheduleTimer {
                    key: stale_key.clone(),
                    delay_ms: 1,
                },
                RuntimeSideEffect::CancelTimer {
                    key: stale_key.clone(),
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: active_key.clone(),
                    delay_ms: 1,
                },
            ])
            .await
            .unwrap();
        assert_eq!(handle.drain_timer_side_effects(8).await.unwrap().len(), 3);

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: stale_key,
            now_ms: 1,
        }));
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: active_key,
            now_ms: 1,
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);

        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert_eq!(
            dispatches,
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: expected_agent,
                turn_id: expected_turn,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.metrics.runtime_side_effects_queued >= 4);
    assert!(exit.metrics.runtime_side_effects_drained >= 4);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that retryable provider failures schedule a per-turn retry timer
/// instead of immediately requeueing provider work. This keeps retry/backoff as
/// an explicit actor-owned timer transition rather than another provider-poll
/// fallback that can wake too early.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_schedules_provider_retry_timer_for_retryable_failure() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let expected_turn = pending[0].turn_id.clone();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut failure = RuntimeEventBatch::new();
        failure.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
            agent_id: expected_agent.clone(),
            turn_id: expected_turn.clone(),
            kind: "invalid_state".to_string(),
            message: "provider HTTP request failed: rate limited".to_string(),
            provider_failure_json: Some(r#"{"status_code":429}"#.to_string()),
            provider_raw_text: None,
        }));
        let failure_report = handle.submit_runtime_events(failure).await.unwrap();
        assert_eq!(failure_report.accepted, 1);
        assert_eq!(failure_report.applied, 1);
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty(),
            "retryable failure should wait for the retry timer before requeueing provider work"
        );
        assert!(
            handle
                .drain_agent_provider_dispatch_side_effects(8)
                .await
                .unwrap()
                .is_empty(),
            "retryable failure must not dispatch provider work before the retry timer fires"
        );

        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        let (retry_key, retry_delay_ms) = timer_effects
            .iter()
            .find_map(|effect| match effect {
                RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                    if key.kind == RuntimeTimerKind::ProviderRetry =>
                {
                    Some((key.clone(), *delay_ms))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected provider retry timer, got {timer_effects:?}"));
        assert_eq!(retry_key.owner_id, expected_turn);
        assert_eq!(retry_key.generation, 1);
        assert_eq!(retry_delay_ms, 1_000);

        let mut retry = RuntimeEventBatch::new();
        retry.push(RuntimeEvent::Timer(TimerEvent {
            key: retry_key,
            now_ms: 1_000,
        }));
        let retry_report = handle.submit_runtime_events(retry).await.unwrap();
        assert_eq!(retry_report.accepted, 1);
        assert_eq!(retry_report.applied, 1);

        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert_eq!(
            dispatches,
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: expected_agent,
                turn_id: expected_turn,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.metrics.runtime_side_effects_queued >= 2);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies provider output-limit failures use the retry timer path after
/// mutating only active-turn retry guidance and the model profile output cap.
///
/// This protects OpenAI `response.incomplete/max_output_tokens` events from
/// becoming terminal turn failures or context-compaction triggers.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_recovers_output_limit_failure_before_provider_retry() {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "async-output-limit-recovery".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "openai"
default_model_profile = "default"
[providers.openai]
kind = "openai"
models = ["gpt-test"]
default_model = "gpt-test"
[model_profiles.default]
provider = "openai"
model = "gpt-test"
max_output_tokens = 4096
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "continue the implementation")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let expected_turn = pending[0].turn_id.clone();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut failure = RuntimeEventBatch::new();
        failure.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
            agent_id: expected_agent.clone(),
            turn_id: expected_turn.clone(),
            kind: "invalid_state".to_string(),
            message: "OpenAI stream returned an incomplete response: max_output_tokens".to_string(),
            provider_failure_json: Some(
                r#"{"incomplete_details":{"reason":"max_output_tokens"}}"#.to_string(),
            ),
            provider_raw_text: None,
        }));
        let failure_report = handle.submit_runtime_events(failure).await.unwrap();
        assert_eq!(failure_report.accepted, 1);
        assert_eq!(failure_report.applied, 1);
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty()
        );

        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        let retry_key = timer_effects
            .iter()
            .find_map(|effect| match effect {
                RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                    if key.kind == RuntimeTimerKind::ProviderRetry =>
                {
                    assert_eq!(*delay_ms, 1_000);
                    Some(key.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected provider retry timer, got {timer_effects:?}"));
        assert_eq!(retry_key.owner_id, expected_turn);

        let mut retry = RuntimeEventBatch::new();
        retry.push(RuntimeEvent::Timer(TimerEvent {
            key: retry_key,
            now_ms: 1_000,
        }));
        let retry_report = handle.submit_runtime_events(retry).await.unwrap();
        assert_eq!(retry_report.accepted, 1);
        assert_eq!(retry_report.applied, 1);

        let pending = handle.pending_agent_provider_tasks().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].model_profile.max_output_tokens(), Some(16_384));
        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert_eq!(
            dispatches,
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: expected_agent,
                turn_id: expected_turn,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("provider response hit output limit; retrying compactly"),
        "{pane_text}"
    );
    assert!(
        !pane_text.contains("provider rejected context as too large"),
        "{pane_text}"
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies idle cleanup does not fail turns whose only progress path is an
/// actor-owned provider retry timer.
///
/// Retry timers are tracked by the async actor instead of
/// `RuntimeSessionService`. If an idle-cleanup timer fires while the turn is
/// waiting for retry backoff, service-level progress reconciliation must treat
/// that actor-owned timer as valid progress rather than failing the running
/// turn as unreachable.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_idle_cleanup_preserves_turn_waiting_for_provider_retry_timer() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let expected_turn = pending[0].turn_id.clone();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut failure = RuntimeEventBatch::new();
        failure.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
            agent_id: expected_agent.clone(),
            turn_id: expected_turn.clone(),
            kind: "invalid_state".to_string(),
            message: "provider HTTP request failed: rate limited".to_string(),
            provider_failure_json: Some(r#"{"status_code":429}"#.to_string()),
            provider_raw_text: None,
        }));
        let failure_report = handle.submit_runtime_events(failure).await.unwrap();
        assert_eq!(failure_report.accepted, 1);
        assert_eq!(failure_report.applied, 1);

        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        let retry_key = timer_effects
            .iter()
            .find_map(|effect| match effect {
                RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                    if key.kind == RuntimeTimerKind::ProviderRetry =>
                {
                    assert_eq!(*delay_ms, 1_000);
                    Some(key.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected provider retry timer, got {timer_effects:?}"));
        assert_eq!(retry_key.owner_id, expected_turn);

        let idle_key = RuntimeTimerKey::new(RuntimeTimerKind::IdleCleanup, "session", 42);
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer {
                key: idle_key.clone(),
                delay_ms: 1,
            }])
            .await
            .unwrap();
        let idle_effects = handle.drain_timer_side_effects(8).await.unwrap();
        assert!(
            idle_effects.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::ScheduleTimer { key, .. } if key == &idle_key
            )),
            "{idle_effects:?}"
        );

        let mut idle = RuntimeEventBatch::new();
        idle.push(RuntimeEvent::Timer(TimerEvent {
            key: idle_key,
            now_ms: 500,
        }));
        let idle_report = handle.submit_runtime_events(idle).await.unwrap();
        assert_eq!(idle_report.accepted, 1);
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty(),
            "idle cleanup must not requeue provider work before retry backoff"
        );
        assert!(
            handle
                .drain_agent_provider_dispatch_side_effects(8)
                .await
                .unwrap()
                .is_empty(),
            "idle cleanup must not dispatch provider work before retry backoff"
        );

        let mut retry = RuntimeEventBatch::new();
        retry.push(RuntimeEvent::Timer(TimerEvent {
            key: retry_key,
            now_ms: 1_000,
        }));
        let retry_report = handle.submit_runtime_events(retry).await.unwrap();
        assert_eq!(retry_report.accepted, 1);
        assert_eq!(retry_report.applied, 1);

        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert_eq!(
            dispatches,
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: expected_agent,
                turn_id: expected_turn,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        !pane_text.contains("runtime found no remaining progress path"),
        "{pane_text}"
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies OpenAI-style provider/controller failures that explicitly invite
/// retry follow the same bounded retry timer path as transport and rate-limit
/// failures.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_schedules_provider_retry_timer_for_controller_retry_hint() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let expected_turn = pending[0].turn_id.clone();
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let retry_message = "An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID b331baf5-b254-46d7-8d3f-58b563ce7ee8 in your message.";
        let mut failure = RuntimeEventBatch::new();
        failure.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
            agent_id: expected_agent,
            turn_id: expected_turn.clone(),
            kind: "invalid_state".to_string(),
            message: retry_message.to_string(),
            provider_failure_json: Some(
                serde_json::json!({
                    "error": {
                        "message": retry_message,
                        "type": "server_error"
                    }
                })
                .to_string(),
            ),
            provider_raw_text: None,
        }));
        let failure_report = handle.submit_runtime_events(failure).await.unwrap();
        assert_eq!(failure_report.accepted, 1);
        assert_eq!(failure_report.applied, 1);

        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        let retry_key = timer_effects
            .iter()
            .find_map(|effect| match effect {
                RuntimeSideEffect::ScheduleTimer { key, delay_ms }
                    if key.kind == RuntimeTimerKind::ProviderRetry =>
                {
                    assert_eq!(*delay_ms, 1_000);
                    Some(key.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected provider retry timer, got {timer_effects:?}"));
        assert_eq!(retry_key.owner_id, expected_turn);
        assert_eq!(retry_key.generation, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.metrics.runtime_side_effects_queued >= 1);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that non-retryable provider failures still fail the active turn and
/// do not schedule delayed provider retry timers. Authentication and validation
/// failures must be visible immediately instead of being hidden behind backoff.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_fails_non_retryable_provider_failures_without_retry_timer() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let expected_turn = pending[0].turn_id.clone();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut failure = RuntimeEventBatch::new();
        failure.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
            agent_id: expected_agent,
            turn_id: expected_turn,
            kind: "invalid_state".to_string(),
            message: "OpenAI provider returned 401 Unauthorized: invalid token".to_string(),
            provider_failure_json: Some(r#"{"status_code":401}"#.to_string()),
            provider_raw_text: None,
        }));
        let failure_report = handle.submit_runtime_events(failure).await.unwrap();
        assert_eq!(failure_report.accepted, 1);
        assert_eq!(failure_report.applied, 1);
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty()
        );
        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        assert!(
            !timer_effects.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::ScheduleTimer { key, .. }
                    if key.kind == RuntimeTimerKind::ProviderRetry
            )),
            "{timer_effects:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(
        exit.service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref())
            .is_none()
    );
    assert_eq!(exit.service.agent_scheduler().snapshot().running, 0);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that actor metrics track typed event ingress, side-effect queue
/// depth, high-water marks, drains, and worker notifications at the serialized
/// runtime boundary. These counters are the Phase 0 instrumentation surface for
/// replacing tick polling with event producers without losing visibility into
/// wakeups and retained work.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_metrics_track_event_and_side_effect_activity() {
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"metrics-output\n".to_vec(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);

        let queued = handle.metrics().await.unwrap();
        assert_eq!(queued.commands_processed, 2);
        assert_eq!(queued.runtime_event_batches, 1);
        assert_eq!(queued.runtime_events_accepted, 1);
        assert_eq!(queued.runtime_events_applied, 1);
        assert_eq!(queued.runtime_side_effects_queued, 1);
        assert_eq!(queued.runtime_side_effects_drained, 0);
        assert_eq!(queued.pane_output_chunks, 1);
        assert_eq!(
            queued.pane_output_bytes,
            u64::try_from(b"metrics-output\n".len()).unwrap()
        );
        assert_eq!(queued.side_effect_queue_depth, 1);
        assert_eq!(queued.side_effect_queue_high_water, 1);
        assert_eq!(queued.side_effect_delivery_notifications, 1);

        assert_eq!(
            handle.drain_runtime_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::PaneOutput,
            }]
        );

        let drained = handle.metrics().await.unwrap();
        assert_eq!(drained.commands_processed, 4);
        assert_eq!(drained.runtime_side_effects_drained, 1);
        assert_eq!(drained.side_effect_queue_depth, 0);
        assert_eq!(drained.side_effect_queue_high_water, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.commands_processed, 5);
    assert_eq!(exit.metrics.runtime_event_batches, 1);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 1);
    assert_eq!(exit.metrics.pane_output_chunks, 1);
    assert_eq!(
        exit.metrics.pane_output_bytes,
        u64::try_from(b"metrics-output\n".len()).unwrap()
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
}
/// Verifies that actor metrics expose rendered-view and terminal control
/// request counts that can be used for idle attach benchmarking. The counters
/// distinguish direct actor render calls from control-socket `terminal/view`
/// and `terminal/step` traffic so regressions toward periodic redraws remain
/// measurable.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_metrics_track_render_and_terminal_control_requests() {
    use crate::control::encode_control_body;

    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let client = async {
        handle
            .render_client_frame(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
                true,
            )
            .await
            .unwrap();
        handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap();
        let terminal_step = encode_control_body(
            r#"{"jsonrpc":"2.0","id":"step","method":"terminal/step","params":{"idempotency_key":"metrics-step","client_size":{"columns":80,"rows":24},"render":false,"input_bytes":[]}}"#,
        );
        let terminal_view = encode_control_body(
            r#"{"jsonrpc":"2.0","id":"view","method":"terminal/view","params":{"client_size":{"columns":80,"rows":24}}}"#,
        );
        handle
            .handle_control_input_for_connection(
                [terminal_step, terminal_view].concat(),
                1024 * 1024,
                ControlConnectionState::trusted_existing_client(primary),
            )
            .await
            .unwrap();
        let metrics = handle.metrics().await.unwrap();
        assert_eq!(metrics.render_client_frame_requests, 1);
        assert_eq!(metrics.render_client_view_requests, 1);
        assert_eq!(metrics.terminal_step_control_requests, 1);
        assert_eq!(metrics.terminal_view_control_requests, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.render_client_frame_requests, 1);
    assert_eq!(exit.metrics.render_client_view_requests, 1);
    assert_eq!(exit.metrics.terminal_step_control_requests, 1);
    assert_eq!(exit.metrics.terminal_view_control_requests, 1);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that queued runtime side effects can be consumed by a supervised
/// async service without taking mutable access to the runtime service itself.
/// This is the worker-side half of the actor side-effect boundary and prevents
/// later render, pane I/O, provider, hook, and persistence workers from growing
/// bespoke drain loops.
#[tokio::test(flavor = "current_thread")]
async fn async_side_effect_service_drains_actor_queue() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let applied = StdArc::new(Mutex::new(Vec::new()));

    let client = {
        let applied = applied.clone();
        async move {
            let mut batch = RuntimeEventBatch::new();
            batch.push(RuntimeEvent::Pane(PaneEvent::Output {
                pane_id: "%1".to_string(),
                bytes: b"worker-side-effect-output\n".to_vec(),
            }));
            let report = handle.submit_runtime_events(batch).await.unwrap();
            assert_eq!(report.side_effects, 1);

            let side_effect_report = run_async_runtime_side_effect_service(
                &handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: 1,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                },
                |effect| {
                    applied.lock().unwrap().push(effect);
                    Ok(())
                },
                |_, _| false,
            )
            .await
            .unwrap();
            assert_eq!(side_effect_report.polls, 1);
            assert_eq!(side_effect_report.drained, 1);
            assert_eq!(side_effect_report.applied, 1);
            assert_eq!(
                handle.shutdown().await.unwrap(),
                RuntimeLifecycleState::Running
            );
        }
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(
        *applied.lock().unwrap(),
        vec![RuntimeSideEffect::RenderClient {
            client_id: primary,
            reason: RenderInvalidationReason::PaneOutput,
        }]
    );
    assert_eq!(exit.commands_processed, 3);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that a bounded side-effect worker exits immediately after its
/// final empty poll instead of sleeping for the idle fallback interval. This
/// protects tests, supervised short runs, and shutdown paths from an avoidable
/// extra delay when no side effects are queued.
#[tokio::test(flavor = "current_thread")]
async fn async_side_effect_service_exits_after_final_empty_poll_without_sleep() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let report = tokio::time::timeout(
            Duration::from_millis(250),
            run_async_runtime_side_effect_service(
                &handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: 1,
                    drain_limit: 8,
                    idle_interval: Duration::from_secs(60),
                },
                |_| Ok(()),
                |_, _| false,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(report.polls, 1);
        assert_eq!(report.drained, 0);
        assert_eq!(report.applied, 0);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 2);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that an unbounded side-effect worker still performs a bounded idle
/// actor-state probe. Runtime side-effect notifications are the fast path, but
/// this probe prevents a missed retained notification permit from stranding
/// queued side effects in a long-lived daemon.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_side_effect_service_uses_bounded_idle_probe_when_unbounded() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();

    let worker_handle = handle.clone();
    let shutdown_handle = handle.clone();
    let client = async move {
        let worker = tokio::spawn(async move {
            run_async_runtime_side_effect_service(
                &worker_handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: u64::MAX,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(10),
                },
                |_| Ok(()),
                |polls, _| polls >= 2,
            )
            .await
            .unwrap()
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(9)).await;
        tokio::task::yield_now().await;
        assert!(
            !worker.is_finished(),
            "side-effect service should wait until its configured idle probe interval"
        );
        tokio::time::advance(Duration::from_millis(11)).await;
        tokio::task::yield_now().await;

        let report = worker.await.unwrap();
        assert_eq!(
            shutdown_handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        report
    };

    let (report, mut exit) = tokio::join!(client, actor.run());
    assert_eq!(report.polls, 2);
    assert_eq!(report.drained, 0);
    assert_eq!(report.applied, 0);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that a filtered side-effect drain re-notifies retained work for
/// another worker. Prompt submission can queue timer/persistence/render work
/// beside provider dispatch work; if one worker consumes the original
/// notification and retains the provider dispatch, the next worker must be
/// woken immediately instead of waiting for its idle probe.
#[tokio::test(flavor = "current_thread")]
async fn async_filtered_side_effect_drain_renotifies_retained_work() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let agent_id = AgentId::opaque("agent-%1").unwrap();

    let client = async {
        handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(RuntimeTimerKind::ProviderPoll, "agent-provider", 1),
                    delay_ms: 1,
                },
                RuntimeSideEffect::DispatchAgentProvider {
                    agent_id,
                    turn_id: "turn-retained".to_string(),
                },
            ])
            .await
            .unwrap();
        handle.wait_for_runtime_side_effects().await;

        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(timers.len(), 1);
        tokio::time::timeout(
            Duration::from_millis(50),
            handle.wait_for_runtime_side_effects(),
        )
        .await
        .expect("retained side-effect work should be re-notified immediately");

        let retained = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert_eq!(retained.len(), 1);
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.metrics.side_effect_delivery_notifications >= 2);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that side-effect delivery revisions wake every worker watching the
/// queue instead of acting like a single consumable permit. Side-effect
/// families run as independent workers, so a provider dispatch must not wait
/// for an idle probe merely because another worker observed the same enqueue.
#[tokio::test(flavor = "current_thread")]
async fn async_side_effect_delivery_watcher_broadcasts_to_all_workers() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut first_worker = handle.side_effect_delivery_watcher();
    let mut second_worker = handle.side_effect_delivery_watcher();

    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer {
                key: RuntimeTimerKey::new(RuntimeTimerKind::ProviderPoll, "agent-provider", 1),
                delay_ms: 1,
            }])
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_millis(50), first_worker.changed())
            .await
            .expect("first side-effect worker should observe the delivery revision")
            .unwrap();
        tokio::time::timeout(Duration::from_millis(50), second_worker.changed())
            .await
            .expect("second side-effect worker should observe the same delivery revision")
            .unwrap();

        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(timers.len(), 1);
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.metrics.side_effect_delivery_notifications, 1);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the side-effect worker wakes from actor notifications instead
/// of relying on its bounded idle probe. This keeps queued render or pane I/O
/// work responsive on the normal notification path while the probe remains only
/// a liveness backstop.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_side_effect_service_wakes_when_actor_queues_effects() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let applied = StdArc::new(Mutex::new(Vec::new()));

    let worker_handle = handle.clone();
    let worker_applied = applied.clone();
    let worker_stop_applied = applied.clone();
    let worker = async move {
        tokio::time::timeout(
            Duration::from_millis(250),
            run_async_runtime_side_effect_service(
                &worker_handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: u64::MAX,
                    drain_limit: 8,
                    idle_interval: Duration::from_secs(60),
                },
                |effect| {
                    worker_applied.lock().unwrap().push(effect);
                    Ok(())
                },
                |_, _| !worker_stop_applied.lock().unwrap().is_empty(),
            ),
        )
        .await
        .unwrap()
        .unwrap()
    };
    let producer_handle = handle.clone();
    let producer = async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"notified-side-effect-output\n".to_vec(),
        }));
        let report = producer_handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.side_effects, 1);
    };
    let shutdown = async {
        let (side_effect_report, ()) = tokio::join!(worker, producer);
        assert_eq!(side_effect_report.polls, 2);
        assert_eq!(side_effect_report.drained, 1);
        assert_eq!(side_effect_report.applied, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(shutdown, actor.run());
    assert_eq!(
        *applied.lock().unwrap(),
        vec![RuntimeSideEffect::RenderClient {
            client_id: primary,
            reason: RenderInvalidationReason::PaneOutput,
        }]
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that typed process-exit events close the pane through the actor
/// using the same session, registry, event-log, pane-pipe, and agent-turn
/// cleanup path as the legacy process polling loop. Async process watchers need
/// this path before pane lifecycle polling can be removed from daemon ticks.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_process_exit_events_to_session_state() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Process(ProcessEvent::Exited {
            pane_id: "%1".to_string(),
            exit_code: Some(0),
            signal: None,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.families, vec!["process"]);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Killed
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.commands_processed, 2);
    assert!(exit.service.session().windows().is_empty());
}

/// Verifies that typed process-spawn events initialize pane lifecycle state
/// through the actor instead of remaining accepted-only bookkeeping. Live async
/// pane ownership will emit this event after a process handle is created, so
/// clients need the normal pane-start lifecycle event and redraw invalidation.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_process_spawn_events_to_event_log() {
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Process(ProcessEvent::Spawned {
            pane_id: "%1".to_string(),
            pid: Some(42),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.drain_runtime_side_effects(8).await.unwrap(),
            vec![RuntimeSideEffect::RenderClient {
                client_id: primary,
                reason: RenderInvalidationReason::FullRedraw,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""process_state":"running""#)
            && event.payload.contains(r#""primary_pid":42"#)
    }));
    assert_eq!(exit.commands_processed, 3);
}

/// Verifies that actor-applied runtime events refresh the session registry
/// by queuing a persistence-worker side effect rather than writing from inside
/// the actor. Daemon discovery must see sessions whose state changes through
/// typed events, and persistence-completion diagnostics must not recursively
/// enqueue another registry write.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_persists_registry_after_applied_runtime_events() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-registry-event-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), current_effective_uid());
    let mut service = test_service();
    service.set_session_registry(registry.clone());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Process(ProcessEvent::Spawned {
            pane_id: "%1".to_string(),
            pid: Some(42),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 3,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert_eq!(persistence.submitted_events, 1);
        assert_eq!(persistence.applied_events, 1);
        assert_eq!(registry.list().unwrap().len(), 1);
        assert!(
            handle
                .drain_persistence_side_effects(8)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 3);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that compatibility-style service methods called through the async
/// actor defer registry persistence into a single worker side effect. Primary
/// disconnect paths can request registry persistence internally, and the actor
/// must not add a duplicate registry write for the same event batch.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_compatibility_registry_updates_once() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-registry-compat-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let registry = SessionRegistry::new(root.clone(), current_effective_uid());
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service.set_session_registry(registry.clone());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::Disconnected {
            client_id: primary,
            reason: "test disconnect".to_string(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);

        let effects = handle.drain_persistence_side_effects(8).await.unwrap();
        let registry_effects = effects
            .iter()
            .filter(|effect| matches!(effect, RuntimeSideEffect::PersistRegistry { .. }))
            .count();
        assert_eq!(registry_effects, 1);
        handle.queue_runtime_side_effects(effects).await.unwrap();

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 3,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert_eq!(registry.list().unwrap().len(), 1);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 6);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that process watcher failures are applied as diagnostic runtime
/// events instead of being silently accepted and dropped. Async pane wait,
/// resize, write, or termination tasks need this path so failures can be
/// replayed to clients and inspected after the worker that observed them exits.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_process_failure_events_to_event_log() {
    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Process(ProcessEvent::Failed {
            pane_id: "%1".to_string(),
            error: "wait task failed".to_string(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        let _ = handle.drain_runtime_side_effects(8).await.unwrap();
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""process_state":"failed""#)
            && event.payload.contains("wait task failed")
    }));
    assert_eq!(exit.commands_processed, 3);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that pane I/O completion events from the async driver are applied
/// through the actor instead of being treated as accepted-only bookkeeping.
/// Write failures must become replayable diagnostics, and resize completions
/// must update retained terminal state and lifecycle output for attached
/// clients.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_pane_io_completion_events_to_event_log() {
    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::WriteFailed {
            pane_id: "%1".to_string(),
            error: "broken pipe".to_string(),
        }));
        batch.push(RuntimeEvent::Pane(PaneEvent::Resized {
            pane_id: "%1".to_string(),
            size: Size::new(100, 30).unwrap(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 2);
        assert_eq!(report.applied, 2);
        assert_eq!(report.side_effects, 2);
        assert_eq!(handle.drain_runtime_side_effects(8).await.unwrap().len(), 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""pane_io":"write_failed""#)
            && event.payload.contains("broken pipe")
    }));
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""pty_resize":"applied""#)
            && event.payload.contains(r#""columns":100"#)
            && event.payload.contains(r#""rows":30"#)
    }));
    assert_eq!(exit.commands_processed, 3);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that supervisor shutdown events are applied through typed runtime
/// ingress even when there is no active primary client. This is the async
/// supervisor path for forced daemon shutdown, failed critical services, and
/// signal handling where the multiplexer must terminate live panes and remove the
/// registry record without routing through a primary-owned control command.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_forced_shutdown_events_without_primary() {
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .detach_primary(&primary, Size::new(80, 24).unwrap())
        .unwrap();
    assert_eq!(service.lifecycle_state(), RuntimeLifecycleState::Detached);

    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: "test supervisor shutdown".to_string(),
            force: true,
            failed: false,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 0);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Killed
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.lifecycle_state(),
        RuntimeLifecycleState::Killed
    );
    assert!(exit.service.session().windows().is_empty());
    assert!(
        exit.service
            .session()
            .clients()
            .iter()
            .all(|client| client.state != ClientState::Attached)
    );
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""lifecycle":"shutdown""#)
            && event.payload.contains("test supervisor shutdown")
    }));
    assert_eq!(exit.commands_processed, 2);
}

/// Verifies that primary-client detach only changes attachment lifecycle when
/// pane processes have moved to async workers. This protects detachable daemon
/// sessions from treating the loss of the foreground client as a pane shutdown
/// request after process ownership leaves the synchronous manager.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_primary_detach_retains_worker_owned_panes() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_processes_for_async_owner(8)
            .await
            .unwrap();
        assert_eq!(processes.len(), 1);
        let (pane_id, mut process) = processes.pop().unwrap();

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::Disconnected {
            client_id: primary.clone(),
            reason: "primary fd closed".to_string(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(
            handle
                .drain_pane_io_side_effects(pane_id.as_str(), 8)
                .await
                .unwrap(),
            Vec::<RuntimeSideEffect>::new()
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Detached
        );
        let _ = process.terminate(Duration::from_millis(10));
        pane_id
    };

    let (pane_id, exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.lifecycle_state(),
        RuntimeLifecycleState::Detached
    );
    assert!(
        exit.service.pane_process_is_async_owned(&pane_id),
        "detaching the primary client must not release async worker ownership"
    );
}

/// Verifies that a primary control attach resizes pane geometry even when the
/// initial pane process has already moved to an async worker. Default `mez`
/// launch starts a background daemon, the pane supervisor can claim the shell
/// before the foreground attach initializes, and the first agent prompt must
/// still render at the live terminal bottom rather than the daemon's bootstrap
/// 80x24 size.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_control_initialize_resizes_worker_owned_initial_pane() {
    use crate::control::{decode_control_frame, encode_control_body};

    let mut service = test_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_processes_for_async_owner(8)
            .await
            .unwrap();
        assert_eq!(processes.len(), 1);
        let (_pane_id, mut process) = processes.pop().unwrap();
        let mut connection = ControlConnectionState::new(true, true);
        let initialize = encode_control_body(
            r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"requested_role":"primary","requested_version":1,"client_name":"mez-cli","client":{"name":"mez-cli","interactive":true,"terminal":{"columns":100,"rows":40,"term":"xterm-256color"}}}}"#,
        );

        let result = handle
            .handle_control_input_for_connection(initialize, 1024 * 1024, connection.clone())
            .await
            .unwrap();
        connection = result.connection;
        let (body, _) = decode_control_frame(&result.output, 1024 * 1024).unwrap();
        assert!(body.contains(r#""granted_role":"primary""#), "{body}");
        let show = encode_control_body(
            r#"{"jsonrpc":"2.0","id":"show","method":"agent/shell/show","params":{"target":{"pane_id":"%1"},"idempotency_key":"show-agent"}}"#,
        );
        let show_result = handle
            .handle_control_input_for_connection(show, 1024 * 1024, connection.clone())
            .await
            .unwrap();
        connection = show_result.connection;
        let (show_body, _) = decode_control_frame(&show_result.output, 1024 * 1024).unwrap();
        assert!(show_body.contains(r#""visible":true"#), "{show_body}");
        let frame = handle
            .render_client_frame(
                ClientViewRole::Primary,
                Size::new(100, 40).unwrap(),
                TerminalClientLoopConfig::default(),
                true,
            )
            .await
            .unwrap();
        let view = frame.view.unwrap();
        assert_eq!(view.authoritative_size, Size::new(100, 40).unwrap());
        let region = view.agent_prompt_region.unwrap();
        assert_eq!(region.rows, 38);
        let prompt_row = view
            .lines
            .iter()
            .rposition(|line| line.contains("agent>"))
            .unwrap();
        assert!(
            prompt_row >= 38,
            "agent prompt text should render at attached terminal bottom: {view:?}"
        );
        assert!(
            view.cursor_row >= 38,
            "agent prompt cursor should render at attached terminal bottom: {view:?}"
        );
        let effects = handle.drain_pane_io_side_effects("%1", 8).await.unwrap();
        assert!(
            effects.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::ResizePane {
                    pane_id,
                    size,
                } if pane_id == "%1" && *size == Size::new(100, 38).unwrap()
            )),
            "{effects:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        assert!(connection.caller_client_id().is_some());
        let _ = process.terminate(Duration::from_millis(10));
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.session().authoritative_size,
        Size::new(100, 40).unwrap()
    );
}

/// Verifies that forced supervisor shutdown crosses the async pane ownership
/// boundary. When a pane process is worker-owned, the runtime actor must queue
/// a termination side effect for the worker instead of assuming the
/// compatibility process manager can terminate the PTY directly.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_forced_shutdown_terminates_worker_owned_panes() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .detach_primary(&primary, Size::new(80, 24).unwrap())
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_processes_for_async_owner(8)
            .await
            .unwrap();
        assert_eq!(processes.len(), 1);
        let (pane_id, mut process) = processes.pop().unwrap();

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: "forced async shutdown".to_string(),
            force: true,
            failed: false,
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(
            handle
                .drain_pane_io_side_effects(pane_id.as_str(), 8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::TerminatePane {
                pane_id: pane_id.clone(),
                force: true,
            }]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Killed
        );
        let _ = process.terminate(Duration::from_millis(10));
        pane_id
    };

    let (pane_id, exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.lifecycle_state(),
        RuntimeLifecycleState::Killed
    );
    assert!(
        !exit.service.pane_process_is_async_owned(&pane_id),
        "forced shutdown must release async ownership after queuing worker termination"
    );
    assert!(exit.service.session().windows().is_empty());
}

/// Verifies that non-forced supervisor shutdown requests apply through the
/// same typed runtime event ingress as forced shutdown without detaching the
/// primary client or killing panes. This covers graceful supervisor paths where
/// peer services should observe the stopping lifecycle before any later forced
/// cleanup decision.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_graceful_shutdown_events() {
    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: "test graceful shutdown".to_string(),
            force: false,
            failed: false,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Stopping
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.lifecycle_state(),
        RuntimeLifecycleState::Stopping
    );
    assert!(
        exit.service
            .session()
            .clients()
            .iter()
            .any(|client| client.state == ClientState::Attached)
    );
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .iter()
        .map(|event| event.payload.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(events.contains(r#""lifecycle":"stopping""#), "{events}");
    assert!(
        events.contains(r#""shutdown_reason":"test graceful shutdown""#),
        "{events}"
    );
    assert!(events.contains(r#""force":false"#), "{events}");
    assert_eq!(exit.commands_processed, 2);
}

/// Verifies that supervisor failure shutdown events are represented as failed
/// runtime state rather than being collapsed into graceful stopping or forced
/// kill semantics. This gives the Tokio supervisor a typed failure path for
/// critical-service failures while still recording a replayable lifecycle
/// diagnostic and notifying attached clients through render side effects.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_failed_shutdown_events() {
    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: "critical service failed".to_string(),
            force: false,
            failed: true,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Failed
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service.lifecycle_state(),
        RuntimeLifecycleState::Failed
    );
    assert_eq!(
        exit.service.session().state,
        crate::session::SessionState::Failed
    );
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary)
        .iter()
        .map(|event| event.payload.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(events.contains(r#""lifecycle":"failed""#), "{events}");
    assert!(
        events.contains(r#""shutdown_reason":"critical service failed""#),
        "{events}"
    );
    assert!(events.contains(r#""force":false"#), "{events}");
    assert_eq!(exit.commands_processed, 2);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the deterministic async attached-terminal fake behaves like an
/// ordered terminal endpoint: readiness, input truncation, size responses,
/// presentation guards, invalidation, and styled-frame writes are all visible
/// without using wall-clock sleeps. This gives later Tokio client-loop tests a
/// stable fake before production file descriptors are migrated to `AsyncFd`.
#[tokio::test]
async fn async_fake_attached_terminal_io_records_ordered_operations() {
    let mut io = AsyncFakeAttachedTerminalIo::default();
    let readiness = AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Input,
        fd: 0,
        interest: TerminalFdInterest::read(),
        readable: true,
        writable: false,
        hangup: false,
        error: false,
    };
    io.push_readiness(vec![readiness]);
    io.push_input(b"abcdef".to_vec());
    io.push_terminal_size(Some(Size::new(100, 30).unwrap()));

    assert_eq!(io.poll_readiness().await.unwrap(), vec![readiness]);
    assert_eq!(io.read_input(3).await.unwrap(), b"abc");
    assert_eq!(
        io.terminal_size().await.unwrap(),
        Some(Size::new(100, 30).unwrap())
    );

    io.enter_presentation().await.unwrap();
    io.invalidate_output_frame().await.unwrap();
    let modes = AttachedTerminalOutputModes {
        cursor_visible: true,
        cursor_row: 2,
        cursor_column: 3,
        ..AttachedTerminalOutputModes::default()
    };
    let lines = vec!["hello".to_string(), "world".to_string()];
    let bytes = io
        .write_styled_output_with_modes(&lines, &[], modes)
        .await
        .unwrap();
    io.restore_presentation().await.unwrap();

    assert_eq!(bytes, "helloworld".len());
    assert_eq!(io.presentation_entries, 1);
    assert_eq!(io.presentation_restores, 1);
    assert_eq!(io.invalidated_output_frames, 1);
    assert_eq!(io.written_frames.len(), 1);
    assert_eq!(io.written_frames[0].lines, lines);
    assert_eq!(io.written_frames[0].modes.cursor_row, 2);
    assert_eq!(io.written_frames[0].modes.cursor_column, 3);
}

/// Verifies that the shared attached-terminal presentation guard validates the
/// raw-mode descriptor before entering the foreground terminal path. This keeps
/// daemon and control-socket attach clients on one setup boundary and prevents
/// invalid descriptors from partially constructing async fd state that would
/// later be difficult to clean up.
#[test]
fn async_attached_terminal_presentation_guard_rejects_invalid_raw_fd() {
    let error = AsyncAttachedTerminalPresentationGuard::new(-1, -1, None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("terminal raw mode file descriptor is invalid"),
        "{error}"
    );
}

/// Verifies that the transitional sync-to-async terminal adapter preserves the
/// existing `AttachedTerminalClientLoopIo` behavior while exposing the new async
/// trait. The adapter is a migration bridge only, so this test protects current
/// behavior while making its replacement with a native Tokio implementation
/// mechanically straightforward.
#[tokio::test]
async fn sync_attached_terminal_io_adapter_preserves_existing_fake_behavior() {
    let mut sync = FakeAttachedTerminalLoopIo::default();
    let readiness = AttachedTerminalFdReadiness {
        role: AttachedTerminalFdRole::Output,
        fd: 1,
        interest: TerminalFdInterest::write(),
        readable: false,
        writable: true,
        hangup: false,
        error: false,
    };
    sync.readiness_batches.push(vec![readiness]);
    sync.input_batches.push(b"input".to_vec());
    let mut adapter = SyncAttachedTerminalIoAdapter::new(sync);

    assert_eq!(adapter.poll_readiness().await.unwrap(), vec![readiness]);
    assert_eq!(adapter.read_input(2).await.unwrap(), b"in");
    let lines = vec!["frame".to_string()];
    let bytes = adapter
        .write_styled_output_with_modes(&lines, &[], AttachedTerminalOutputModes::default())
        .await
        .unwrap();

    let sync = adapter.into_inner();
    assert_eq!(bytes, "frame".len());
    assert_eq!(sync.written_batches, vec![lines]);
}

/// Verifies that the Tokio `AsyncFd` attached-terminal endpoint can read and
/// write through Unix file descriptors without the synchronous terminal polling
/// trait. The test uses a Unix socket pair as a deterministic fd source, which
/// exercises nonblocking flag setup, async input readiness, async output
/// flushing, and terminal-frame encoding without requiring a real foreground
/// TTY.
#[tokio::test]
async fn async_fd_attached_terminal_io_reads_and_writes_socket_pair() {
    let (driver, mut peer) = StdUnixStream::pair().unwrap();
    let driver_output = driver.try_clone().unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    peer.write_all(b"input").unwrap();

    let mut io =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();
    let input = io.read_input(5).await.unwrap();
    assert_eq!(input, b"input");

    let lines = vec!["async-frame".to_string()];
    let bytes = io
        .write_styled_output_with_modes(&lines, &[], AttachedTerminalOutputModes::default())
        .await
        .unwrap();
    assert!(bytes > "async-frame".len());

    let mut output = vec![0u8; 4096];
    let read = peer.read(&mut output).unwrap();
    let output = String::from_utf8_lossy(&output[..read]);
    assert!(output.contains("async-frame"), "{output:?}");
}

/// Verifies that the native async terminal endpoint's normal frame-write API
/// completes frames larger than the adaptive bounded-write chunk. Control-socket
/// attach rendering uses this API directly; returning after the first chunk
/// leaves the rest of a scroll or copy-mode repaint retained but never flushed,
/// which appears as large unrendered regions on the attached terminal.
#[tokio::test]
async fn async_fd_attached_terminal_io_unbounded_write_completes_large_frame() {
    let (driver, mut peer) = StdUnixStream::pair().unwrap();
    let driver_output = driver.try_clone().unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();

    let mut io =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();
    let large_line = format!(
        "{}tail-marker",
        "x".repeat(DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES + 1024)
    );
    let lines = vec![large_line.clone()];
    let bytes = io
        .write_styled_output_with_modes(&lines, &[], AttachedTerminalOutputModes::default())
        .await
        .unwrap();

    assert!(bytes > DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES);
    assert_eq!(io.pending_output_bytes(), 0);

    let mut output = Vec::new();
    drop(io);
    drop(driver_output);
    drop(driver);
    peer.read_to_end(&mut output).unwrap();
    let output = String::from_utf8_lossy(&output);
    assert!(output.contains("tail-marker"), "{output:?}");
}

/// Verifies that the native async terminal endpoint reports pending input
/// before the always-writable output side of an interactive PTY-like fd pair.
/// This protects foreground attach loops from starving user keystrokes while
/// redraws remain possible on every iteration.
#[tokio::test]
async fn async_fd_attached_terminal_io_prioritizes_input_over_writable_output() {
    let (driver, mut peer) = StdUnixStream::pair().unwrap();
    let driver_output = driver.try_clone().unwrap();
    peer.write_all(b"x").unwrap();

    let mut io =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();
    let readiness = io.poll_readiness().await.unwrap();

    assert!(
        readiness
            .iter()
            .any(|ready| ready.role == AttachedTerminalFdRole::Input && ready.readable),
        "{readiness:?}"
    );
}

/// Verifies that the native async terminal endpoint's input-focused readiness
/// wait does not wake merely because stdout is writable. This is the attach
/// service idle-CPU guard: redraws should come from actor render notifications
/// or explicit fallback timers, while user input still wakes the service
/// promptly.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_fd_attached_terminal_input_readiness_ignores_writable_output() {
    let (driver, mut peer) = StdUnixStream::pair().unwrap();
    let driver_output = driver.try_clone().unwrap();
    let mut io =
        AsyncAttachedTerminalFdLoopIo::new(driver.as_raw_fd(), driver_output.as_raw_fd(), None)
            .unwrap();

    let idle = tokio::time::timeout(Duration::from_millis(1), io.poll_input_readiness()).await;
    assert!(idle.is_err(), "writable output should not wake input wait");

    peer.write_all(b"x").unwrap();
    let readiness = tokio::time::timeout(Duration::from_millis(1), io.poll_input_readiness())
        .await
        .unwrap()
        .unwrap();
    assert!(
        readiness
            .iter()
            .any(|ready| ready.role == AttachedTerminalFdRole::Input && ready.readable),
        "{readiness:?}"
    );
}

/// Verifies that the per-pane async driver converts PTY output from its backend
/// into an ordered runtime event without mutating shared session state. This is
/// the first step toward replacing global pane-output polling with one
/// independently scheduled pane task per live process.
#[tokio::test]
async fn async_pane_process_driver_converts_output_to_runtime_event() {
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_output(b"hello from pty".to_vec());
    let mut driver = AsyncPaneProcessDriver::new(
        "%1",
        backend,
        AsyncPaneProcessDriverConfig {
            max_output_bytes_per_event: 5,
        },
    )
    .unwrap();

    assert_eq!(driver.pane_id(), "%1");
    let event = driver.poll_output_event().await.unwrap();

    assert_eq!(
        event,
        Some(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"hello".to_vec(),
        }))
    );
}

/// Verifies that foreground process metadata observed by a pane worker becomes
/// a typed pane event. Automatic pane titles need this event once live pane
/// ownership has moved out of the synchronous process manager.
#[tokio::test]
async fn async_pane_process_driver_reports_foreground_process_metadata() {
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
        process_name: "vim".to_string(),
        process_group_id: 42,
        current_working_directory: Some(std::path::PathBuf::from("/tmp/project")),
    })));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let event = driver.poll_foreground_process_event().await.unwrap();

    assert_eq!(
        event,
        Some(RuntimeEvent::Pane(PaneEvent::ForegroundProcess {
            pane_id: "%1".to_string(),
            process_name: "vim".to_string(),
            process_group_id: 42,
            current_working_directory: Some("/tmp/project".to_string()),
        }))
    );
}

/// Verifies that pane write, resize, and termination completions become typed
/// runtime events instead of panics or global driver failures. The async pane
/// migration needs this behavior so one pane's I/O failure can be rendered and
/// audited without blocking unrelated panes or attached clients.
#[tokio::test]
async fn async_pane_process_driver_reports_io_and_lifecycle_results() {
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_write_result(Err(MezError::invalid_state("write failed")));
    backend.push_resize_result(Ok(()));
    backend.push_terminate_result(Ok(ProcessEvent::Exited {
        pane_id: "%1".to_string(),
        exit_code: Some(0),
        signal: None,
    }));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let write = driver.write_input_event(b"input").await;
    let resize = driver.resize_event(Size::new(100, 30).unwrap()).await;
    let terminated = driver.terminate_event(false).await;
    let backend = driver.into_backend();

    assert_eq!(
        write,
        RuntimeEvent::Pane(PaneEvent::WriteFailed {
            pane_id: "%1".to_string(),
            error: "InvalidState: write failed".to_string(),
        })
    );
    assert_eq!(
        resize,
        RuntimeEvent::Pane(PaneEvent::Resized {
            pane_id: "%1".to_string(),
            size: Size::new(100, 30).unwrap(),
        })
    );
    assert_eq!(
        terminated,
        RuntimeEvent::Process(ProcessEvent::Exited {
            pane_id: "%1".to_string(),
            exit_code: Some(0),
            signal: None,
        })
    );
    assert_eq!(backend.writes, vec![b"input".to_vec()]);
    assert_eq!(backend.resizes, vec![Size::new(100, 30).unwrap()]);
    assert_eq!(backend.terminations, vec![false]);
}

/// Verifies that natural pane exits are polled as typed process events and
/// reported only once. A per-pane owner must not keep re-submitting the same
/// recorded process exit after the backend has reached a terminal state.
#[tokio::test]
async fn async_pane_process_driver_reports_polled_exit_once() {
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_exit_result(Ok(Some(ProcessEvent::Exited {
        pane_id: "%1".to_string(),
        exit_code: Some(7),
        signal: None,
    })));
    backend.push_exit_result(Ok(Some(ProcessEvent::Exited {
        pane_id: "%1".to_string(),
        exit_code: Some(7),
        signal: None,
    })));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let first = driver.poll_exit_event().await.unwrap();
    let second = driver.poll_exit_event().await.unwrap();

    assert_eq!(
        first,
        Some(RuntimeEvent::Process(ProcessEvent::Exited {
            pane_id: "%1".to_string(),
            exit_code: Some(7),
            signal: None,
        }))
    );
    assert_eq!(second, None);
}

/// Verifies that the Tokio PTY backend can drive a real portable-pty pane
/// process without blocking the async test task. This keeps the live pane path
/// honest: the async driver boundary is not only a fake-test facade, and it can
/// read live PTY bytes and report process termination as a typed lifecycle
/// event.
#[tokio::test]
async fn async_pty_pane_process_io_bridges_live_portable_pty() {
    let shell = resolve_shell(Some(OsString::from("/bin/sh"))).unwrap();
    let process = spawn_pane_process(
        &shell,
        Some("/bin/sh -c 'printf async-bridge-output; sleep 5'"),
        &test_pane_environment(),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let mut backend = AsyncPtyPaneProcessIo::new("%bridge", process).unwrap();

    let mut output = Vec::new();
    for _ in 0..50 {
        if let Some(bytes) = backend.read_output(4096).await.unwrap() {
            output.extend(bytes);
        }
        if String::from_utf8_lossy(&output).contains("async-bridge-output") {
            break;
        }
        if let Some(activity) = backend.output_activity()
            && let Ok(result) = tokio::time::timeout(Duration::from_millis(500), activity).await
        {
            result.unwrap();
        }
    }

    assert!(
        String::from_utf8_lossy(&output).contains("async-bridge-output"),
        "{}",
        String::from_utf8_lossy(&output)
    );
    let event = backend.terminate(true).await.unwrap();

    let ProcessEvent::Exited {
        pane_id,
        exit_code,
        signal,
    } = event
    else {
        panic!("expected process exit event, got {event:?}");
    };
    assert_eq!(pane_id, "%bridge");
    assert!(
        exit_code.is_some() || signal.is_some(),
        "terminated process should expose an exit code or signal"
    );
}

/// Verifies that the pane driver service loop submits output events to the
/// runtime actor and reports both submitted and applied event counts. This is
/// the reusable bridge that live per-pane PTY tasks use after actor handoff.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_driver_service_submits_output_to_actor() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_output(b"driver-service-output\n".to_vec());
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let report = run_async_pane_process_driver_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessDriverServiceConfig {
                max_polls: 1,
                idle_interval: Duration::from_millis(1),
            },
            |_| false,
        )
        .await
        .unwrap();
        let view = service_handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(
            view.lines
                .iter()
                .any(|line| line.contains("driver-service-output")),
            "{:?}",
            view.lines
        );
        let _ = service_handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(service, actor.run());

    assert_eq!(report.polls, 1);
    assert_eq!(report.submitted_events, 1);
    assert_eq!(report.applied_events, 1);
    assert_eq!(exit.commands_processed, 3);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that an idle pane driver service wakes between empty output polls
/// when the actor queues side effects. Bounded runs still keep a fallback
/// interval so finite tests can complete, but actor-side work must wake the
/// service before that full interval elapses.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_pane_process_driver_service_wakes_between_empty_polls_on_side_effects() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_no_output();
    backend.push_no_output();
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let notify_handle = handle.clone();
    let service = async move {
        let driver_service = run_async_pane_process_driver_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessDriverServiceConfig {
                max_polls: 2,
                idle_interval: Duration::from_secs(60),
            },
            |_| false,
        );
        let notifier = async {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(10)).await;
            notify_handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::ResizePane {
                    pane_id: "%1".to_string(),
                    size: Size::new(90, 30).unwrap(),
                }])
                .await
                .unwrap();
        };
        let (report, ()) = tokio::time::timeout(Duration::from_millis(250), async {
            tokio::join!(driver_service, notifier)
        })
        .await
        .unwrap();
        let report = report.unwrap();
        assert_eq!(report.polls, 2);
        assert_eq!(report.submitted_events, 0);
        assert_eq!(report.applied_events, 0);
        let _ = service_handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(service, actor.run());
    assert!(exit.commands_processed >= 2);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that an unbounded pane driver service waits only on output, actor
/// event, and side-effect notifications between empty output polls. This keeps
/// the production-shaped legacy driver path from falling back to a periodic
/// idle poll.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_pane_process_driver_service_unbounded_waits_for_notifications() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_no_output();
    backend.push_no_output();
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let notify_handle = handle.clone();
    let service = async move {
        let driver_service = run_async_pane_process_driver_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessDriverServiceConfig {
                max_polls: u64::MAX,
                idle_interval: Duration::from_millis(1),
            },
            |polls| polls >= 2,
        );
        let notifier = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            notify_handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::ResizePane {
                    pane_id: "%1".to_string(),
                    size: Size::new(90, 30).unwrap(),
                }])
                .await
                .unwrap();
        };
        let (report, ()) = tokio::join!(driver_service, notifier);
        let report = report.unwrap();
        assert_eq!(report.polls, 2);
        assert_eq!(report.submitted_events, 0);
        assert_eq!(report.applied_events, 0);
        let _ = service_handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(service, actor.run());
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the live PTY backend can wake a pane driver from Tokio
/// readiness instead of waiting for a compatibility service interval. This
/// keeps idle panes asleep until the PTY master becomes readable.
#[tokio::test]
async fn async_pane_process_driver_service_wakes_on_live_output_activity() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let shell = resolve_shell(Some(OsString::from("/bin/sh"))).unwrap();
    let process = spawn_pane_process(
        &shell,
        Some("/bin/sh -c 'sleep 0.05; printf live-activity-output'"),
        &test_pane_environment(),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let backend = AsyncPtyPaneProcessIo::new("%1", process).unwrap();
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let report = tokio::time::timeout(
            Duration::from_secs(2),
            run_async_pane_process_driver_service(
                &service_handle,
                &mut driver,
                AsyncPaneProcessDriverServiceConfig {
                    max_polls: 2,
                    idle_interval: Duration::from_secs(60),
                },
                |_| false,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(report.submitted_events, 1);
        assert_eq!(report.applied_events, 1);
        let view = service_handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(
            view.lines
                .iter()
                .any(|line| line.contains("live-activity-output")),
            "{:?}",
            view.lines
        );
        let _ = service_handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(service, actor.run());
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that pane write, resize, and termination side effects can be
/// drained and executed by a per-pane async worker. The worker must leave
/// unrelated side-effect families in the actor queue, submit typed completion
/// events back through the actor, and keep backend I/O details outside the
/// serialized runtime actor.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_io_side_effect_service_executes_pane_effects() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_write_result(Ok(5));
    backend.push_resize_result(Ok(()));
    backend.push_terminate_result(Ok(ProcessEvent::Exited {
        pane_id: "%1".to_string(),
        exit_code: Some(0),
        signal: None,
    }));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let provider_agent = AgentId::opaque("agent-%1").unwrap();
        let queued = service_handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::WritePaneInput {
                    pane_id: "%1".to_string(),
                    bytes: b"input".to_vec(),
                },
                RuntimeSideEffect::DispatchAgentProvider {
                    agent_id: provider_agent.clone(),
                    turn_id: "turn-1".to_string(),
                },
                RuntimeSideEffect::ResizePane {
                    pane_id: "%1".to_string(),
                    size: Size::new(100, 30).unwrap(),
                },
                RuntimeSideEffect::TerminatePane {
                    pane_id: "%1".to_string(),
                    force: true,
                },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 4);

        let report = run_async_pane_io_side_effect_service(
            &service_handle,
            &mut driver,
            AsyncPaneIoSideEffectServiceConfig {
                max_polls: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        let backend = driver.into_backend();
        assert_eq!(backend.writes, vec![b"input".to_vec()]);
        assert_eq!(backend.resizes, vec![Size::new(100, 30).unwrap()]);
        assert_eq!(backend.terminations, vec![true]);
        assert_eq!(
            service_handle
                .drain_agent_provider_dispatch_side_effects(8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: provider_agent,
                turn_id: "turn-1".to_string(),
            }]
        );
        let _ = service_handle.shutdown().await.unwrap();
        report
    };

    let (report, exit) = tokio::join!(service, actor.run());

    assert_eq!(report.polls, 1);
    assert_eq!(report.drained, 3);
    assert_eq!(report.submitted_events, 3);
    assert_eq!(exit.metrics.runtime_side_effects_queued, 4);
    assert_eq!(exit.metrics.runtime_side_effects_drained, 4);
}

/// Verifies default pane I/O drain limits interleave input chunks with output.
///
/// Large clipboard pastes into full-screen editors can fill the PTY output
/// side while the editor is still consuming input. Draining one pane input
/// effect per service poll gives the combined pane worker an opportunity to
/// read redraw output before accepting the next paste chunk.
#[test]
fn async_pane_io_default_drain_limits_interleave_paste_with_output() {
    assert_eq!(AsyncPaneIoSideEffectServiceConfig::default().drain_limit, 1);
    assert_eq!(
        AsyncPaneProcessServiceConfig::default().output_drain_limit,
        4
    );
    assert_eq!(AsyncPaneProcessServiceConfig::default().drain_limit, 1);
}

/// Verifies that the combined pane process service defers large input
/// remainders after one bounded write.
///
/// This keeps a paste-sized pane input side effect from monopolizing the PTY
/// write path. The next service poll can read full-screen application redraw
/// output before accepting the following input chunk while preserving input
/// ordering ahead of later actor-queued pane input.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_defers_large_input_remainders() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let backend = AsyncFakePaneProcessIo::default();
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let large_input = vec![b'x'; 468_586];
        service_handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::WritePaneInput {
                    pane_id: "%1".to_string(),
                    bytes: large_input.clone(),
                },
                RuntimeSideEffect::WritePaneInput {
                    pane_id: "%1".to_string(),
                    bytes: b"after".to_vec(),
                },
            ])
            .await
            .unwrap();

        let report = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 2,
                output_drain_limit: 1,
                drain_limit: 1,
                idle_interval: Duration::from_millis(1),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        let backend = driver.into_backend();
        assert_eq!(report.drained, 2);
        assert_eq!(report.submitted_events, 2);
        assert_eq!(
            backend.writes,
            vec![
                large_input[..crate::process::PTY_INPUT_WRITE_CHUNK_BYTES].to_vec(),
                large_input[crate::process::PTY_INPUT_WRITE_CHUNK_BYTES
                    ..crate::process::PTY_INPUT_WRITE_CHUNK_BYTES * 2]
                    .to_vec()
            ]
        );
        assert_eq!(
            service_handle
                .drain_pane_io_side_effects("%1", 8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"after".to_vec(),
            }]
        );
        let _ = service_handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(service, actor.run());
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that partial PTY write progress remains observable and ordered.
///
/// A backend can accept only part of a pane input chunk before applying
/// backpressure. The worker must surface that accepted byte count, keep the
/// unsent remainder ahead of later queued input, and retry the remainder on the
/// next poll instead of treating the whole write as failed or re-sending bytes
/// already accepted by the PTY.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_retries_partial_input_remainders() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_write_result(Ok(2));
    backend.push_write_result(Ok(4));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        service_handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::WritePaneInput {
                    pane_id: "%1".to_string(),
                    bytes: b"abcdef".to_vec(),
                },
                RuntimeSideEffect::WritePaneInput {
                    pane_id: "%1".to_string(),
                    bytes: b"after".to_vec(),
                },
            ])
            .await
            .unwrap();

        let report = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 2,
                output_drain_limit: 1,
                drain_limit: 1,
                idle_interval: Duration::from_millis(1),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        let backend = driver.into_backend();
        assert_eq!(report.drained, 2);
        assert_eq!(report.submitted_events, 2);
        assert_eq!(backend.writes, vec![b"abcdef".to_vec(), b"cdef".to_vec()]);
        assert_eq!(
            service_handle
                .drain_pane_io_side_effects("%1", 8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"after".to_vec(),
            }]
        );
        let _ = service_handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(service, actor.run());
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies zero-byte PTY writes are reported as bounded failures.
///
/// A zero-byte write makes no transport progress. Treating it as success would
/// drop the pending input remainder and leave higher-level shell transactions
/// waiting for markers that can never arrive.
#[tokio::test]
async fn async_pane_process_driver_rejects_zero_byte_input_progress() {
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_write_result(Ok(0));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let event = driver.write_input_event(b"input").await;

    assert_eq!(
        event,
        RuntimeEvent::Pane(PaneEvent::WriteFailed {
            pane_id: "%1".to_string(),
            error: "InvalidState: pane PTY write accepted zero bytes".to_string(),
        })
    );
}

/// Verifies that an unbounded pane I/O side-effect worker parks on actor
/// notifications instead of polling at its idle interval while waiting for
/// pane-specific work. This protects the production worker path from idle CPU
/// churn while retaining prompt wakeups for queued input, resize, and terminate
/// side effects.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_pane_io_side_effect_service_unbounded_waits_for_notifications() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_write_result(Ok(4));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let worker_handle = handle.clone();
    let worker = async move {
        let report = run_async_pane_io_side_effect_service(
            &worker_handle,
            &mut driver,
            AsyncPaneIoSideEffectServiceConfig {
                max_polls: u64::MAX,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        (report, driver.into_backend())
    };
    let producer_handle = handle.clone();
    let producer = async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        producer_handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"wake".to_vec(),
            }])
            .await
            .unwrap();
    };
    let shutdown = async {
        let ((report, backend), ()) = tokio::join!(worker, producer);
        assert_eq!(report.polls, 2);
        assert_eq!(report.drained, 1);
        assert_eq!(report.submitted_events, 1);
        assert_eq!(backend.writes, vec![b"wake".to_vec()]);
        handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(shutdown, actor.run());
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that an idle pane I/O side-effect worker wakes from lifecycle
/// notifications without relying on its idle interval. This keeps worker-owned
/// pane input, resize, and terminate drains responsive to daemon shutdown even
/// when no pane-specific side effects are queued.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_pane_io_side_effect_service_wakes_on_lifecycle_change_without_idle_poll() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let backend = AsyncFakePaneProcessIo::default();
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let worker_handle = handle.clone();
    let shutdown_handle = handle.clone();
    let worker = async move {
        let pane_worker = tokio::spawn(async move {
            run_async_pane_io_side_effect_service(
                &worker_handle,
                &mut driver,
                AsyncPaneIoSideEffectServiceConfig {
                    max_polls: u64::MAX,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                },
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
            .unwrap()
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(
            !pane_worker.is_finished(),
            "idle pane I/O worker should not wake from elapsed time alone"
        );

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: "pane side-effect lifecycle wake test".to_string(),
            force: true,
            failed: false,
        }));
        shutdown_handle.submit_runtime_events(batch).await.unwrap();
        let report = tokio::time::timeout(Duration::from_millis(250), pane_worker)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.polls, 1);
        assert_eq!(report.drained, 0);
        assert_eq!(report.terminal_state, RuntimeLifecycleState::Killed);
        shutdown_handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(worker, actor.run());
    assert!(exit.commands_processed >= 3);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the combined pane process service serializes PTY output and
/// pane I/O side effects through one driver. This is the ownership shape needed
/// before production live pane processes can move out of global manager
/// polling without introducing cross-task write/output/exit ordering races.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_serializes_output_and_side_effects() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_output(b"combined-service-output\n".to_vec());
    backend.push_write_result(Ok(5));
    backend.push_resize_result(Ok(()));
    backend.push_terminate_result(Ok(ProcessEvent::Exited {
        pane_id: "%1".to_string(),
        exit_code: Some(0),
        signal: None,
    }));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let provider_agent = AgentId::opaque("agent-%1").unwrap();
        let queued = service_handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::WritePaneInput {
                    pane_id: "%1".to_string(),
                    bytes: b"input".to_vec(),
                },
                RuntimeSideEffect::DispatchAgentProvider {
                    agent_id: provider_agent.clone(),
                    turn_id: "turn-1".to_string(),
                },
                RuntimeSideEffect::ResizePane {
                    pane_id: "%1".to_string(),
                    size: Size::new(100, 30).unwrap(),
                },
                RuntimeSideEffect::TerminatePane {
                    pane_id: "%1".to_string(),
                    force: false,
                },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 4);

        let report = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 1,
                output_drain_limit: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        assert_eq!(
            service_handle
                .drain_agent_provider_dispatch_side_effects(8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: provider_agent,
                turn_id: "turn-1".to_string(),
            }]
        );
        let _ = service_handle.shutdown().await.unwrap();
        (report, driver.into_backend())
    };

    let ((report, backend), mut exit) = tokio::join!(service, actor.run());

    assert_eq!(report.polls, 1);
    assert_eq!(report.output_events, 1);
    assert_eq!(report.drained, 3);
    assert_eq!(report.submitted_events, 4);
    assert!(
        report.applied_events >= 1,
        "output should be applied before later pane lifecycle events: {report:?}"
    );
    assert_eq!(backend.writes, vec![b"input".to_vec()]);
    assert_eq!(backend.resizes, vec![Size::new(100, 30).unwrap()]);
    assert_eq!(backend.terminations, vec![false]);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies bursty pane output is submitted to the actor as one event batch.
///
/// SSH sessions are sensitive to event-loop and render invalidation churn. A
/// bounded output burst should therefore cross the actor boundary as one
/// ordered pane-output event with coalesced bytes.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_batches_bursty_output_events() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_output(b"one".to_vec());
    backend.push_output(b"two".to_vec());
    backend.push_output(b"three".to_vec());
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let report = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 1,
                output_drain_limit: 8,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        service_handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(service, actor.run());

    assert_eq!(report.output_events, 3);
    assert_eq!(report.submitted_events, 1);
    assert_eq!(exit.metrics.runtime_event_batches, 1);
    assert_eq!(exit.metrics.pane_output_chunks, 1);
    assert_eq!(exit.metrics.pane_output_bytes, 11);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies foreground process metadata is not polled again for every output
/// chunk before its refresh interval elapses. Pane output should remain cheap
/// during bursty redraws, while process-title metadata still refreshes on its
/// own cadence.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_throttles_metadata_during_output_bursts() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_output(b"first".to_vec());
    backend.push_output(b"second".to_vec());
    backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
        process_name: "vim".to_string(),
        process_group_id: 42,
        current_working_directory: Some(std::path::PathBuf::from("/tmp/project")),
    })));
    backend.push_foreground_process_result(Ok(Some(AsyncPaneForegroundProcess {
        process_name: "sh".to_string(),
        process_group_id: 43,
        current_working_directory: Some(std::path::PathBuf::from("/tmp/other")),
    })));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let report = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 2,
                output_drain_limit: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        service_handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(service, actor.run());

    assert_eq!(report.output_events, 2);
    assert_eq!(report.submitted_events, 3);
    assert_eq!(exit.metrics.pane_output_chunks, 2);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the combined pane process service wakes for queued pane I/O
/// side effects even when no PTY output is available. A live pane task must not
/// wait for its fallback interval before delivering user input, resize, or
/// termination requests.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_wakes_for_pane_side_effects() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_no_output();
    backend.push_no_output();
    backend.push_write_result(Ok(4));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let notify_handle = handle.clone();
    let service = async move {
        let pane_service = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 2,
                output_drain_limit: 1,
                drain_limit: 8,
                idle_interval: Duration::from_secs(60),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        );
        let notifier = async {
            tokio::task::yield_now().await;
            notify_handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::WritePaneInput {
                    pane_id: "%1".to_string(),
                    bytes: b"wake".to_vec(),
                }])
                .await
                .unwrap();
        };
        let (report, ()) = tokio::time::timeout(Duration::from_millis(250), async {
            tokio::join!(pane_service, notifier)
        })
        .await
        .unwrap();
        let report = report.unwrap();
        let _ = service_handle.shutdown().await.unwrap();
        (report, driver.into_backend())
    };

    let ((report, backend), mut exit) = tokio::join!(service, actor.run());

    assert_eq!(report.polls, 2);
    assert_eq!(report.output_events, 0);
    assert_eq!(report.drained, 1);
    assert_eq!(report.submitted_events, 1);
    assert_eq!(backend.writes, vec![b"wake".to_vec()]);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that a quiet combined pane worker sleeps until the next foreground
/// metadata deadline instead of waking at the short compatibility idle
/// interval. This keeps idle pane workers from consuming CPU while preserving
/// periodic metadata refreshes and notification-driven side-effect wakeups.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_pane_process_service_uses_metadata_deadline_for_quiet_panes() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_no_output();
    backend.push_no_output();
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let report = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 2,
                output_drain_limit: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        let _ = service_handle.shutdown().await.unwrap();
        (report, driver.into_backend())
    };
    let joined = async { tokio::join!(service, actor.run()) };
    tokio::pin!(joined);

    tokio::select! {
        _ = &mut joined => panic!("quiet pane worker woke before foreground metadata was due"),
        _ = tokio::time::sleep(Duration::from_millis(59_999)) => {}
    }
    tokio::time::advance(Duration::from_millis(1)).await;

    let ((report, _backend), mut exit) = joined.await;

    assert_eq!(report.polls, 2);
    assert_eq!(report.output_events, 0);
    assert_eq!(report.drained, 0);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that an idle combined pane worker wakes from the actor lifecycle
/// watch channel and terminates its backend when the daemon enters a terminal
/// state. This prevents shutdown from relying on synchronous `Drop` cleanup for
/// worker-owned PTYs when no pane output, side effect, or short idle timer is
/// available to wake the task.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_wakes_on_terminal_lifecycle_and_terminates_backend() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_terminate_result(Ok(ProcessEvent::Exited {
        pane_id: "%1".to_string(),
        exit_code: None,
        signal: Some("killed".to_string()),
    }));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let shutdown_handle = handle.clone();
    let service = async move {
        let pane_service = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: u64::MAX,
                output_drain_limit: 1,
                drain_limit: 8,
                idle_interval: Duration::from_secs(60),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, state| {
                matches!(
                    state,
                    RuntimeLifecycleState::Stopping
                        | RuntimeLifecycleState::Killed
                        | RuntimeLifecycleState::Failed
                )
            },
        );
        let shutdown = async {
            tokio::task::yield_now().await;
            let mut batch = RuntimeEventBatch::new();
            batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
                reason: "terminal lifecycle pane worker test".to_string(),
                force: true,
                failed: false,
            }));
            shutdown_handle.submit_runtime_events(batch).await.unwrap();
        };
        let (report, ()) = tokio::time::timeout(Duration::from_millis(250), async {
            tokio::join!(pane_service, shutdown)
        })
        .await
        .unwrap();
        let report = report.unwrap();
        let _ = service_handle.shutdown().await.unwrap();
        (report, driver.into_backend())
    };

    let ((report, backend), mut exit) = tokio::join!(service, actor.run());

    assert_eq!(report.terminal_state, RuntimeLifecycleState::Killed);
    assert_eq!(report.exit_events, 1);
    assert_eq!(backend.terminations, vec![true]);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the combined pane process service submits a natural process
/// exit only after a preceding PTY output poll has been given its own service
/// turn. This protects the migration's output-before-exit ordering contract
/// before live pane process ownership moves into the service.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_process_service_reports_exit_after_output_turn() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut backend = AsyncFakePaneProcessIo::default();
    backend.push_output(b"final output before exit\n".to_vec());
    backend.push_exit_result(Ok(Some(ProcessEvent::Exited {
        pane_id: "%1".to_string(),
        exit_code: Some(0),
        signal: None,
    })));
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let report = run_async_pane_process_service(
            &service_handle,
            &mut driver,
            AsyncPaneProcessServiceConfig {
                max_polls: 2,
                output_drain_limit: 1,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
                foreground_metadata_interval: Duration::from_secs(60),
            },
            |_, _| false,
        )
        .await
        .unwrap();
        let _ = service_handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(service, actor.run());

    assert_eq!(report.polls, 2);
    assert_eq!(report.output_events, 1);
    assert_eq!(report.exit_events, 1);
    assert_eq!(report.submitted_events, 2);
    assert!(
        report.applied_events >= 1,
        "output should apply before exit event teardown: {report:?}"
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the live PTY backend does not report process exit before
/// preceding output bytes have been drained. The backend may observe child exit
/// before the PTY master reports closure, so exit reporting must be held until
/// no output remains pending.
#[tokio::test]
async fn async_pane_process_service_waits_for_live_output_before_exit() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let shell = resolve_shell(Some(OsString::from("/bin/sh"))).unwrap();
    let process = spawn_pane_process(
        &shell,
        Some("/bin/sh -c 'printf live-output-before-exit'"),
        &test_pane_environment(),
        Size::new(80, 24).unwrap(),
    )
    .unwrap();
    let backend = AsyncPtyPaneProcessIo::new("%1", process).unwrap();
    let mut driver =
        AsyncPaneProcessDriver::new("%1", backend, AsyncPaneProcessDriverConfig::default())
            .unwrap();

    let service_handle = handle.clone();
    let service = async move {
        let report = tokio::time::timeout(
            Duration::from_secs(2),
            run_async_pane_process_service(
                &service_handle,
                &mut driver,
                AsyncPaneProcessServiceConfig {
                    max_polls: 20,
                    output_drain_limit: 1,
                    drain_limit: 8,
                    idle_interval: Duration::from_secs(60),
                    foreground_metadata_interval: Duration::from_secs(60),
                },
                |_, _| false,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let _ = service_handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(service, actor.run());

    assert!(
        report.output_events >= 1,
        "live output should be observed before exit: {report:?}"
    );
    assert_eq!(report.exit_events, 1);
    assert!(
        report.submitted_events >= 2,
        "output and exit should both be submitted: {report:?}"
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the dynamic pane-process supervisor can claim a live
/// manager-owned pane through the actor and start a per-pane worker without a
/// startup-only handoff list. This is the daemon path needed for panes created
/// after the initial session boot.
#[tokio::test]
async fn async_pane_process_supervisor_claims_live_manager_panes() {
    let mut service = test_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let supervisor_handle = handle.clone();
    let supervisor = async move {
        let report = run_async_pane_process_supervisor_service(
            supervisor_handle,
            AsyncPaneProcessSupervisorServiceConfig {
                max_polls: 2,
                take_limit: 8,
                idle_interval: Duration::from_millis(1),
                pane_service: AsyncPaneProcessServiceConfig {
                    max_polls: u64::MAX,
                    output_drain_limit: 1,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                    foreground_metadata_interval: Duration::from_secs(60),
                },
            },
            |_, _| false,
        )
        .await
        .unwrap();
        assert_eq!(report.spawned_workers, 1);
        assert_eq!(
            handle
                .take_running_pane_processes_for_async_owner(8)
                .await
                .unwrap()
                .len(),
            0
        );
        let _ = handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(supervisor, actor.run());

    assert_eq!(report.polls, 2);
    assert_eq!(report.spawned_workers, 1);
    assert!(exit.service.pane_processes_mut().terminate_all().is_ok());
}

/// Verifies that the dynamic pane-process supervisor observes child worker
/// completion directly instead of waking on its fallback idle interval. This
/// keeps production supervision responsive to short-lived panes without adding
/// an idle poll while no new handoffs are available.
#[tokio::test]
async fn async_pane_process_supervisor_wakes_on_worker_completion() {
    let mut service = test_service();
    service.start_initial_pane_process(Some("true")).unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let supervisor_handle = handle.clone();
    let supervisor = async move {
        let report = tokio::time::timeout(
            Duration::from_secs(2),
            run_async_pane_process_supervisor_service(
                supervisor_handle,
                AsyncPaneProcessSupervisorServiceConfig {
                    max_polls: u64::MAX,
                    take_limit: 8,
                    idle_interval: Duration::from_secs(60),
                    pane_service: AsyncPaneProcessServiceConfig {
                        max_polls: u64::MAX,
                        output_drain_limit: 1,
                        drain_limit: 8,
                        idle_interval: Duration::from_secs(60),
                        foreground_metadata_interval: Duration::from_secs(60),
                    },
                },
                |polls, _| polls >= 3,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(supervisor, actor.run());

    assert_eq!(report.spawned_workers, 1);
    assert_eq!(report.completed_workers, 1);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that timer side effects are consumed by the timer worker rather
/// than remaining as inert actor queue entries. The scheduled provider-poll
/// timer must re-enter the actor as a typed `TimerEvent`, which then produces a
/// provider-dispatch side effect through the same path used by direct timer
/// ingress.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_timer_side_effect_service_fires_scheduled_timers() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let expected_agent = AgentId::opaque(pending[0].agent_id.clone()).unwrap();
    let expected_turn = pending[0].turn_id.clone();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let key = RuntimeTimerKey::new(RuntimeTimerKind::ProviderPoll, "agent-provider", 1);
        let queued = handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer { key, delay_ms: 1 }])
            .await
            .unwrap();
        assert_eq!(queued, 1);

        let timer = run_async_runtime_timer_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 4,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            100,
            |polls, _| polls >= 4,
        );
        let clock = async {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(1)).await;
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(1)).await;
        };
        let (report, ()) = tokio::join!(timer, clock);
        let report = report.unwrap();
        assert_eq!(report.drained, 1);
        assert_eq!(report.scheduled, 1);
        assert_eq!(report.fired, 1);
        assert_eq!(report.submitted_events, 1);
        assert_eq!(report.applied_events, 1);

        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert_eq!(
            dispatches,
            vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: expected_agent,
                turn_id: expected_turn,
            }]
        );
        handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 3);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that cancelled runtime timers are removed before they can emit
/// stale events. This prevents old readiness, shell transaction, or resize
/// generations from racing later actor state after a newer timer supersedes
/// them.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_timer_side_effect_service_cancels_scheduled_timers() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let key = RuntimeTimerKey::new(RuntimeTimerKind::CursorBlink, "primary", 9);
        let queued = handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::ScheduleTimer {
                    key: key.clone(),
                    delay_ms: 1,
                },
                RuntimeSideEffect::CancelTimer { key },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 2);

        let timer = run_async_runtime_timer_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            0,
            |polls, _| polls >= 2,
        );
        let clock = async {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(1)).await;
        };
        let (report, ()) = tokio::join!(timer, clock);
        let report = report.unwrap();
        assert_eq!(report.drained, 2);
        assert_eq!(report.scheduled, 1);
        assert_eq!(report.cancelled, 1);
        assert_eq!(report.fired, 0);
        assert_eq!(report.submitted_events, 0);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
    assert_eq!(exit.metrics.runtime_timer_schedules_queued, 1);
    assert_eq!(exit.metrics.runtime_timer_cancellations_queued, 1);
}

/// Verifies that program hook side effects are executed by the async hook
/// worker and reported back through typed actor events. This keeps lifecycle
/// hook process latency out of the actor while preserving ordered runtime
/// application of hook results.
#[tokio::test(flavor = "current_thread")]
async fn async_hook_side_effect_service_executes_program_hooks() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-hook-complete-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let payload_path = root.join("payload.json");
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RunProgramHook {
                plan: Box::new(HookExecutionPlan {
                    hook_id: "async-hook".to_string(),
                    event: HookEvent::ClientDetach,
                    run_in_focused_shell: false,
                    target_pane_id: None,
                    blocks_on_shell_availability: false,
                    program: Some("/bin/sh".to_string()),
                    args: vec![
                        "-c".to_string(),
                        "cat > \"$1\"".to_string(),
                        "hook".to_string(),
                        payload_path.display().to_string(),
                    ],
                    shell_command: None,
                    event_payload_json: r#"{"client_id":"primary"}"#.to_string(),
                    timeout_ms: 1_000,
                    on_failure: HookOnFailure::Warn,
                }),
                triggering_event_completed: true,
            }])
            .await
            .unwrap();
        assert_eq!(queued, 1);

        let report = run_async_hook_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(report.drained, 1);
        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(report.submitted_events, 1);
        assert_eq!(report.applied_events, 1);
        assert_eq!(
            std::fs::read_to_string(&payload_path).unwrap(),
            r#"{"client_id":"primary"}"#
        );
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that actor-applied lifecycle events defer non-blocking configured
/// program hooks as hook-worker side effects. Blocking pre-action hooks remain
/// synchronous for now, but completed lifecycle hooks should no longer spawn
/// hook processes inside the actor.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_completed_program_hooks_to_hook_worker() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-hook-deferral-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let payload_path = root.join("detach.json");
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: format!(
                "[hooks.detach]\nevent = \"client_detach\"\nprogram = \"/bin/sh\"\nargs = [\"-c\", \"cat > \\\"$1\\\"\", \"hook\", \"{}\"]\non_failure = \"warn\"\n",
                payload_path.display()
            ),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Client(ClientEvent::Disconnected {
            client_id: primary,
            reason: "test disconnect".to_string(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 1);

        let effects = handle.drain_hook_side_effects(8).await.unwrap();
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            &effects[0],
            RuntimeSideEffect::RunProgramHook { plan, triggering_event_completed: true }
                if plan.hook_id == "detach"
        ));
        assert!(!payload_path.exists());
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 3);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that persistence side effects are owned by a concrete Tokio worker
/// instead of the actor. The worker writes the bytes and reports completion back
/// through typed event ingress so later audit, transcript, snapshot, and config
/// migrations can share the same boundary.
#[tokio::test(flavor = "current_thread")]
async fn async_persistence_side_effect_service_writes_bytes_and_reports_completion() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-persistence-complete-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("audit.jsonl");
    let (handle, actor) = AsyncRuntimeSessionActor::new(
        test_service_with_event_log(),
        AsyncRuntimeActorConfig::default(),
    )
    .unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::Persist {
                target: PersistenceTarget::AuditLog,
                path: path.clone(),
                bytes: b"{\"event\":\"worker\"}\n".to_vec(),
                mode: PersistenceWriteMode::Append,
            }])
            .await
            .unwrap();
        assert_eq!(queued, 1);

        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(report.drained, 1);
        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(report.bytes_written, 19);
        assert_eq!(report.submitted_events, 1);
        assert_eq!(report.applied_events, 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"event\":\"worker\"}\n"
        );
        #[cfg(unix)]
        {
            assert_eq!(unix_mode(&root), 0o700);
            assert_eq!(unix_mode(&path), 0o600);
        }
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"audit_log""#)
            && event.payload.contains(r#""state":"completed""#)
    }));
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that an idle persistence worker wakes on actor lifecycle
/// notifications before its bounded idle probe interval elapses. This covers
/// the shared side-effect worker wait primitive used by persistence, hooks,
/// render, client-output flushing, and generic side-effect drains.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_persistence_side_effect_service_wakes_on_lifecycle_change_without_idle_poll() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();

    let worker_handle = handle.clone();
    let shutdown_handle = handle.clone();
    let client = async move {
        let worker = tokio::spawn(async move {
            run_async_persistence_side_effect_service(
                &worker_handle,
                AsyncRuntimeSideEffectServiceConfig {
                    max_polls: u64::MAX,
                    drain_limit: 8,
                    idle_interval: Duration::from_secs(60),
                },
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
            .unwrap()
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(
            !worker.is_finished(),
            "idle persistence worker should not wake before its idle probe interval"
        );

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: "persistence lifecycle wake test".to_string(),
            force: true,
            failed: false,
        }));
        shutdown_handle.submit_runtime_events(batch).await.unwrap();
        let report = tokio::time::timeout(Duration::from_millis(250), worker)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.polls, 1);
        assert_eq!(report.drained, 0);
        assert_eq!(report.terminal_state, RuntimeLifecycleState::Killed);
        shutdown_handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 3);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that persistence write modes can preserve create-new semantics for
/// future snapshot and default-config migrations. The first create-new write
/// succeeds, the second conflicting write is reported as a typed persistence
/// failure, and the original private file contents remain intact.
#[tokio::test(flavor = "current_thread")]
async fn async_persistence_side_effect_service_honors_create_new_mode() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-persistence-create-new-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let path = root.join("config.toml");
    let (handle, actor) = AsyncRuntimeSessionActor::new(
        test_service_with_event_log(),
        AsyncRuntimeActorConfig::default(),
    )
    .unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![
                RuntimeSideEffect::Persist {
                    target: PersistenceTarget::Config,
                    path: path.clone(),
                    bytes: b"first\n".to_vec(),
                    mode: PersistenceWriteMode::CreateNew,
                },
                RuntimeSideEffect::Persist {
                    target: PersistenceTarget::Config,
                    path: path.clone(),
                    bytes: b"second\n".to_vec(),
                    mode: PersistenceWriteMode::CreateNew,
                },
            ])
            .await
            .unwrap();
        assert_eq!(queued, 2);

        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(report.drained, 2);
        assert_eq!(report.completed, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(report.bytes_written, 6);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\n");
        #[cfg(unix)]
        {
            assert_eq!(unix_mode(&root), 0o700);
            assert_eq!(unix_mode(&path), 0o600);
        }
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"config""#)
            && event.payload.contains(r#""state":"completed""#)
    }));
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"config""#)
            && event.payload.contains(r#""state":"failed""#)
    }));
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that audit records created through actor-owned runtime commands are
/// queued for the persistence worker instead of written from inside the actor.
/// The command still mutates policy state immediately, while the audit JSONL
/// append is drained through the target-specific persistence path.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_audit_writes_to_persistence_worker() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-audit-defer-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let audit_path = root.join("audit.jsonl");
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service.set_audit_log(crate::audit::AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: true,
        required: true,
    }));
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let response = handle
            .execute_agent_shell_command(primary, "/approval full-access".to_string())
            .await
            .unwrap();
        assert!(response.contains("changed=true"), "{response}");
        assert!(!audit_path.exists());

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert_eq!(persistence.submitted_events, 1);
        assert_eq!(persistence.applied_events, 1);

        let audit = std::fs::read_to_string(&audit_path).unwrap();
        assert!(audit.contains(r#""event_type":"permission""#), "{audit}");
        assert!(
            audit.contains(r#""permission_id":"permissions.approval_policy""#),
            "{audit}"
        );
        assert!(audit.contains(r#""hash":"#), "{audit}");
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"audit_log""#)
            && event.payload.contains(r#""state":"completed""#)
    }));
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that file-backed pane pipes start and append output through the
/// persistence worker in the async actor path. This keeps both `pipe-pane -o`
/// setup and later pane-output application from blocking on file I/O while
/// preserving the existing user behavior.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_file_pane_pipe_writes_to_persistence_worker() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-pane-pipe-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("pane.log");
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let started = handle
            .execute_terminal_command(primary, format!("pipe-pane -o {}", path.display()))
            .await
            .unwrap();
        assert!(started.contains("pipe=started"), "{started}");
        assert!(
            !path.exists(),
            "async actor should not create file-backed pipe output before persistence worker drains"
        );

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"pipe-async\n".to_vec(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 1);

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert_eq!(persistence.submitted_events, 1);
        assert_eq!(persistence.applied_events, 1);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("pipe-async")
        );
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"pane_pipe""#)
            && event.payload.contains(r#""state":"completed""#)
    }));
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that file-backed pane pipe persistence failures stop the active
/// pipe through the actor. A failed async append otherwise leaves runtime state
/// believing that pane output is still being captured even though subsequent
/// writes will continue to fail.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_stops_file_pane_pipe_after_persistence_failure() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-pane-pipe-failed-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("pane.log");
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let started = service
        .execute_terminal_command(&primary, &format!("pipe-pane -o {}", path.display()))
        .unwrap();
    assert!(started.contains("pipe=started"), "{started}");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"pipe-fail\n".to_vec(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 1);

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 0);
        assert_eq!(persistence.failed, 1);
        assert_eq!(persistence.submitted_events, 1);
        assert_eq!(persistence.applied_events, 1);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"pane_pipe""#)
            && event.payload.contains(r#""state":"failed""#)
    }));
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""pipe":"stopped""#)
            && event.payload.contains(r#""reason":"persistence-failed""#)
    }));
    assert_eq!(exit.service.active_pane_pipe_display(), "active_pipes=0");
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that command-backed pane pipes are checked by actor-owned timers
/// after accepted pane output. The command writer can fail after `write_output`
/// has already accepted bytes into its bounded queue; the timer makes that
/// asynchronous failure visible and stops the active pipe without requiring a
/// later pane-output write or an explicit `pipe-pane --stop`.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_stops_command_pane_pipe_after_health_timer() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-command-pane-pipe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let script = root.join("pipe-command.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nhead -c 1 >/dev/null\nsleep 0.02\nexit 7\n",
    )
    .unwrap();
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let started = service
        .execute_terminal_command(&primary, &format!("pipe-pane /bin/sh {}", script.display()))
        .unwrap();
    assert!(started.contains("pipe=started"), "{started}");
    assert!(started.contains("mode=command"), "{started}");
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"pipe-command-health\n".to_vec(),
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 1);
        tokio::time::sleep(Duration::from_millis(80)).await;

        let timers = run_async_runtime_timer_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 4,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            1_000,
            |polls, _| polls >= 4,
        )
        .await
        .unwrap();
        assert_eq!(timers.drained, 1);
        assert_eq!(timers.fired, 1);
        assert_eq!(timers.submitted_events, 1);
        assert_eq!(timers.applied_events, 1);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""pipe":"stopped""#)
            && event.payload.contains(r#""mode":"command""#)
            && event.payload.contains(r#""reason":"command-failed""#)
    }));
    assert_eq!(exit.service.active_pane_pipe_display(), "active_pipes=0");
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that actor-owned command-backed pane pipes receive a health timer
/// as soon as the terminal command starts the pipe and reschedule that health
/// check while the pipe command is still active. This protects command pipe
/// lifecycle cleanup from depending on unrelated pane output and keeps quick
/// exits or deferred startup failures discoverable through timer ingress.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_actor_schedules_command_pane_pipe_health_after_start() {
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let started = handle
            .execute_terminal_command(primary.clone(), "pipe-pane cat >/dev/null".to_string())
            .await
            .unwrap();
        assert!(started.contains("pipe=started"), "{started}");
        assert!(started.contains("mode=command"), "{started}");

        let effects = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(effects.len(), 1, "{effects:?}");
        let first_key = match &effects[0] {
            RuntimeSideEffect::ScheduleTimer { key, delay_ms } => {
                assert_eq!(key.kind, RuntimeTimerKind::PanePipeHealth);
                assert_eq!(key.owner_id, "%1");
                assert_eq!(*delay_ms, 50);
                key.clone()
            }
            other => panic!("expected pane-pipe health timer, got {other:?}"),
        };

        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Timer(TimerEvent {
            key: first_key.clone(),
            now_ms: 1_060,
        }));
        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 0);
        assert_eq!(report.side_effects, 1);

        let effects = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(effects.len(), 1, "{effects:?}");
        let second_key = match &effects[0] {
            RuntimeSideEffect::ScheduleTimer { key, delay_ms } => {
                assert_eq!(key.kind, RuntimeTimerKind::PanePipeHealth);
                assert_eq!(key.owner_id, "%1");
                assert_eq!(*delay_ms, 50);
                key.clone()
            }
            other => panic!("expected rescheduled pane-pipe health timer, got {other:?}"),
        };
        assert!(second_key.generation > first_key.generation);

        let stopped = handle
            .execute_terminal_command(primary, "pipe-pane --stop".to_string())
            .await
            .unwrap();
        assert!(stopped.contains("pipe=stopped"), "{stopped}");
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.service.active_pane_pipe_display(), "active_pipes=0");
    assert!(exit.commands_processed >= 4);
}

/// Verifies that persistence worker write failures become diagnostic runtime
/// events instead of crashing the worker or daemon supervisor. This keeps
/// latency-sensitive persistence paths debuggable while preserving actor
/// ownership of visible error state.
#[tokio::test(flavor = "current_thread")]
async fn async_persistence_side_effect_service_reports_failures_without_crashing() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-persistence-failed-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let (handle, actor) = AsyncRuntimeSessionActor::new(
        test_service_with_event_log(),
        AsyncRuntimeActorConfig::default(),
    )
    .unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::Persist {
                target: PersistenceTarget::Config,
                path: root.clone(),
                bytes: b"will fail".to_vec(),
                mode: PersistenceWriteMode::Replace,
            }])
            .await
            .unwrap();
        assert_eq!(queued, 1);

        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(report.drained, 1);
        assert_eq!(report.completed, 0);
        assert_eq!(report.failed, 1);
        assert_eq!(report.bytes_written, 0);
        assert_eq!(report.submitted_events, 1);
        assert_eq!(report.applied_events, 1);
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"config""#)
            && event.payload.contains(r#""state":"failed""#)
    }));
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that config-target persistence rejects symlink destinations before
/// opening the file. Config writes carry user secrets and must preserve the
/// synchronous config writer's direct-private-file expectation when they move
/// onto the async persistence worker.
#[cfg(unix)]
/// Verifies async persistence side effect service rejects config symlink destinations.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_persistence_side_effect_service_rejects_config_symlink_destinations() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-persistence-symlink-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let link_path = root.join("config.toml");
    let linked_target = root.join("linked-target.toml");
    std::os::unix::fs::symlink(&linked_target, &link_path).unwrap();
    let (handle, actor) = AsyncRuntimeSessionActor::new(
        test_service_with_event_log(),
        AsyncRuntimeActorConfig::default(),
    )
    .unwrap();

    let client = async {
        let queued = handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::Persist {
                target: PersistenceTarget::Config,
                path: link_path.clone(),
                bytes: b"secret = true\n".to_vec(),
                mode: PersistenceWriteMode::Replace,
            }])
            .await
            .unwrap();
        assert_eq!(queued, 1);

        let report = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(report.drained, 1);
        assert_eq!(report.completed, 0);
        assert_eq!(report.failed, 1);
        assert!(!linked_target.exists());
        handle.shutdown().await.unwrap();
    };

    let ((), exit) = tokio::join!(client, actor.run());
    let events = exit
        .service
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::Primary);
    assert!(events.iter().any(|event| {
        event.payload.contains(r#""worker":"async-persistence""#)
            && event.payload.contains(r#""target":"config""#)
            && event.payload.contains(r#""state":"failed""#)
    }));
    assert!(exit.commands_processed >= 4);
    let _ = std::fs::remove_dir_all(root);
}

/// Runs the test supervised service operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_supervised_service(
    name: &'static str,
    exit: AsyncRuntimeServiceExit,
) -> AsyncRuntimeService {
    AsyncRuntimeService::new(name, async move { Ok(exit) })
}

/// Verifies that daemon supervision rejects invalid service sets before
/// spawning tasks. This matters because duplicate or missing listener names
/// would make later failure and shutdown reports ambiguous.
#[test]
fn async_runtime_service_supervisor_validates_service_set() {
    let empty_error = AsyncRuntimeServiceSupervisor::new(Vec::new()).unwrap_err();
    assert_eq!(empty_error.kind(), crate::error::MezErrorKind::InvalidArgs);

    let unnamed_error = AsyncRuntimeServiceSupervisor::new(vec![test_supervised_service(
        " ",
        AsyncRuntimeServiceExit::completed(0),
    )])
    .unwrap_err();
    assert_eq!(
        unnamed_error.kind(),
        crate::error::MezErrorKind::InvalidArgs
    );

    let duplicate_error = AsyncRuntimeServiceSupervisor::new(vec![
        test_supervised_service("control", AsyncRuntimeServiceExit::completed(0)),
        test_supervised_service("control", AsyncRuntimeServiceExit::completed(1)),
    ])
    .unwrap_err();
    assert_eq!(
        duplicate_error.kind(),
        crate::error::MezErrorKind::InvalidArgs
    );
    assert!(duplicate_error.message().contains("control"));
}

/// Exercises the successful path for multiple supervised services. The
/// assertion sorts by name so the test verifies task scheduling without
/// relying on Tokio's completion order for ready futures.
#[tokio::test(flavor = "current_thread")]
async fn async_runtime_service_supervisor_reports_named_completions() {
    let report = supervise_async_runtime_services(
        vec![
            test_supervised_service("control", AsyncRuntimeServiceExit::completed(1)),
            test_supervised_service("message", AsyncRuntimeServiceExit::completed(2)),
        ],
        std::future::pending(),
    )
    .await
    .unwrap();

    let mut services = report.services;
    services.sort_by(|left, right| left.name.cmp(&right.name));

    assert!(!report.shutdown_requested);
    assert_eq!(
        services,
        vec![
            AsyncRuntimeServiceReport {
                name: "control".to_string(),
                exit: AsyncRuntimeServiceExit::completed(1),
            },
            AsyncRuntimeServiceReport {
                name: "message".to_string(),
                exit: AsyncRuntimeServiceExit::completed(2),
            },
        ]
    );
}

/// Verifies that an auxiliary maintenance task does not keep supervision alive
/// after all primary services have completed. This protects daemon tests and
/// bounded listener runs from hanging behind the long-lived tick service while
/// still reporting that the tick task stopped without requesting shutdown.
#[tokio::test(flavor = "current_thread")]
async fn async_runtime_service_supervisor_stops_auxiliary_after_primary_completion() {
    let report = supervise_async_runtime_services(
        vec![
            test_supervised_service("control", AsyncRuntimeServiceExit::completed(1)),
            AsyncRuntimeService::new_auxiliary("tick", async {
                std::future::pending::<Result<AsyncRuntimeServiceExit>>().await
            }),
        ],
        std::future::pending(),
    )
    .await
    .unwrap();

    let mut services = report.services;
    services.sort_by(|left, right| left.name.cmp(&right.name));

    assert!(!report.shutdown_requested);
    assert_eq!(
        services,
        vec![
            AsyncRuntimeServiceReport {
                name: "control".to_string(),
                exit: AsyncRuntimeServiceExit::completed(1),
            },
            AsyncRuntimeServiceReport {
                name: "tick".to_string(),
                exit: AsyncRuntimeServiceExit::completed(0),
            },
        ]
    );
}

/// Ensures service task errors are propagated rather than hidden in a
/// nominal completion report. The service name is part of the diagnostic so
/// daemon startup can identify which listener failed.
#[tokio::test(flavor = "current_thread")]
async fn async_runtime_service_supervisor_propagates_named_failures() {
    let error = supervise_async_runtime_services(
        vec![AsyncRuntimeService::new("events", async {
            Err(MezError::invalid_state("listener exited unexpectedly"))
        })],
        std::future::pending(),
    )
    .await
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(error.message().contains("events"));
    assert!(error.message().contains("listener exited unexpectedly"));
}

/// Covers external cancellation of a long-lived listener task. The task
/// never completes on its own, so the cancellation future is the only route
/// to a bounded shutdown report.
#[tokio::test(flavor = "current_thread")]
async fn async_runtime_service_supervisor_reports_cancelled_services_as_shutdown() {
    use tokio::sync::oneshot;

    let (started_sender, started_receiver) = oneshot::channel();
    let (cancel_sender, cancel_receiver) = oneshot::channel();
    let pending_control = AsyncRuntimeService::new("control", async move {
        let _ = started_sender.send(());
        std::future::pending::<Result<AsyncRuntimeServiceExit>>().await
    });

    let supervision = supervise_async_runtime_services(vec![pending_control], async {
        let _ = cancel_receiver.await;
    });
    let canceller = async {
        started_receiver.await.unwrap();
        cancel_sender.send(()).unwrap();
    };

    let (report, ()) = tokio::join!(supervision, canceller);
    let report = report.unwrap();

    assert!(report.shutdown_requested);
    assert_eq!(
        report.services,
        vec![AsyncRuntimeServiceReport {
            name: "control".to_string(),
            exit: AsyncRuntimeServiceExit::shutdown(0),
        }]
    );
}

/// Verifies async agent provider service polls runtime queue.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_agent_provider_service_polls_runtime_queue() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let service_handle = handle.clone();
    let service = async move {
        let report = run_async_agent_provider_service(
            &service_handle,
            AsyncAgentProviderServiceConfig::new(1).unwrap(),
            |polls, _| polls >= 1,
        )
        .await
        .unwrap();
        let _ = service_handle.shutdown().await.unwrap();
        report
    };

    let (report, exit) = tokio::join!(service, actor.run());

    assert_eq!(report.polls, 1);
    assert_eq!(report.idle_polls, 1);
    assert_eq!(report.executions, 0);
    assert!(exit.commands_processed >= 3);
}

/// Verifies that an idle provider service performs a bounded actor-state probe
/// even when no notification arrives. This protects prompt submission on slow
/// systems from a missed side-effect notification permit: ordinary prompt work
/// still wakes the service immediately, while the bounded probe keeps queued
/// turns from staying stranded forever.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_agent_provider_service_uses_bounded_idle_probe() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let config = AsyncAgentProviderServiceConfig::new(1)
            .unwrap()
            .with_idle_interval(Duration::from_millis(10))
            .unwrap();
        let report = tokio::time::timeout(
            Duration::from_millis(50),
            run_async_agent_provider_service(&handle, config, |polls, _| polls >= 2),
        )
        .await
        .unwrap()
        .unwrap();
        let _ = handle.shutdown().await.unwrap();
        report
    };

    let (report, exit) = tokio::join!(client, actor.run());
    assert_eq!(report.polls, 2);
    assert_eq!(report.idle_polls, 2);
    assert_eq!(report.executions, 0);
    assert!(exit.commands_processed >= 1);
}

/// Verifies that the provider service delegates provider-poll timer ownership
/// to the actor instead of retaining a local duplicate guard. With pending
/// provider work and no timer worker draining the queue, multiple idle provider
/// polls should leave exactly one scheduled provider-poll timer side effect.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_agent_provider_service_uses_actor_owned_provider_poll_guard() {
    let idle_interval = Duration::from_millis(25);
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let service_handle = handle.clone();
    let service = async move {
        run_async_agent_provider_service(
            &service_handle,
            AsyncAgentProviderServiceConfig::new(1)
                .unwrap()
                .with_idle_interval(idle_interval)
                .unwrap(),
            |polls, _| polls >= 2,
        )
        .await
        .unwrap()
    };
    let clock = async {
        tokio::task::yield_now().await;
        tokio::time::advance(idle_interval).await;
        tokio::task::yield_now().await;
        tokio::time::advance(idle_interval).await;
    };
    let client = async {
        let (report, ()) = tokio::join!(service, clock);
        assert_eq!(report.polls, 2);
        assert_eq!(report.idle_polls, 2);
        assert_eq!(report.executions, 0);

        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(timers.len(), 1);
        let RuntimeSideEffect::ScheduleTimer { key, delay_ms } = &timers[0] else {
            panic!("expected provider poll timer side effect, got {timers:?}");
        };
        assert_eq!(key.kind, RuntimeTimerKind::ProviderPoll);
        assert_eq!(key.owner_id, "agent-provider");
        assert_eq!(*delay_ms, idle_interval.as_millis() as u64);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.metrics.runtime_side_effects_queued >= 1);
    assert!(exit.metrics.runtime_side_effects_drained >= 1);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that async provider worker failures can finish the active agent
/// turn through typed runtime event ingress. The failed event has enough
/// identity and error information to reuse the configured provider failure
/// path, including audit, prompt display, scheduler cleanup, and pending-task
/// removal, without returning an error to the daemon supervisor.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_agent_provider_failure_events() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let task = pending[0].clone();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Failed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id,
            kind: "invalid_state".to_string(),
            message: "provider worker failed before response".to_string(),
            provider_failure_json: None,
            provider_raw_text: None,
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        exit.service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("provider_error"), "{pane_text}");
    assert!(
        pane_text.contains("provider worker failed before response"),
        "{pane_text}"
    );
    assert_eq!(exit.commands_processed, 2);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies model-correctable file-action failures enqueue the next provider
/// dispatch immediately through the async actor.
///
/// The runtime service stores the retry context, but the async actor owns
/// side-effect dispatch. A failed provider completion that queues action
/// failure feedback must therefore emit a fresh provider-dispatch side effect
/// instead of waiting for an unrelated timer path before the model can
/// self-correct.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_dispatches_provider_retry_after_file_action_failure_feedback() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "write then inspect")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service
        .pending_agent_provider_tasks()
        .into_iter()
        .next()
        .expect("prompt should queue a provider task");
    let turn = crate::agent::AgentTurnRecord {
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        pane_id: task.pane_id.clone(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 2,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: crate::agent::AgentTurnState::Running,
        cooperation_mode: None,
    };
    let write_action = crate::agent::AgentAction {
        id: "patch-fail".to_string(),
        rationale: "write a source file".to_string(),
        payload: crate::agent::AgentActionPayload::ApplyPatch {
            patch: "*** Begin Patch\n*** Add File: src/generated.rs\n+content\n*** End Patch"
                .to_string(),
            strip: None,
        },
    };
    let read_action = crate::agent::AgentAction {
        id: "read-unsent".to_string(),
        rationale: "read the source file".to_string(),
        payload: crate::agent::AgentActionPayload::ShellCommand {
            summary: "Read the source file".to_string(),
            command: "sed -n '1,120p' src/generated.rs".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
        },
    };
    let mut failed = crate::agent::ActionResult::failed(
        &turn,
        &write_action,
        crate::agent::ActionStatus::Failed,
        "pane_input_write_failed",
        "pane input write failed while sending shell action",
    )
    .unwrap();
    failed.structured_content_json = Some(
        serde_json::json!({
            "kind": "apply_patch",
            "terminal_observation": {
                "state": "pane-input-write-failed"
            }
        })
        .to_string(),
    );
    let pending = crate::agent::ActionResult::running(
        &turn,
        &read_action,
        vec!["local action accepted for pane execution".to_string()],
        Some(r#"{"state":"pending_dispatch"}"#.to_string()),
    );
    let batch = crate::agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        actions: vec![write_action, read_action],
        final_turn: false,
    };
    let execution = crate::agent::AgentTurnExecution {
        request: crate::agent::ModelRequest {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            reasoning_effort: task
                .model_profile
                .provider_options
                .get("reasoning_effort")
                .cloned()
                .or_else(|| task.model_profile.reasoning_profile.clone()),
            latency_preference: task.model_profile.latency_preference.clone(),
            prompt_cache_retention: task
                .model_profile
                .provider_options
                .get("prompt_cache_retention")
                .cloned(),
            max_output_tokens: task.model_profile.max_output_tokens(),
            prompt_cache_session_id: None,
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            available_mcp_tools: Vec::new(),
            interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
            allowed_actions: crate::agent::AllowedActionSet::for_capability(
                crate::agent::AgentCapability::Shell,
            ),
            messages: vec![crate::agent::ModelMessage {
                role: crate::agent::ModelMessageRole::User,
                source: crate::agent::ContextSourceKind::UserInstruction,
                content: "write then inspect".to_string(),
            }],
        },
        response: crate::agent::ModelResponse {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            raw_text: "failed file action response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(batch),
        },
        latest_response_usage: Default::default(),
        action_results: vec![failed, pending],
        final_turn: false,
        terminal_state: crate::agent::AgentTurnState::Failed,
    };
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut provider_batch = RuntimeEventBatch::new();
        provider_batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id.clone()).unwrap(),
            turn_id: task.turn_id.clone(),
            execution: Box::new(execution),
        }));
        let report = handle.submit_runtime_events(provider_batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        let dispatches = handle
            .drain_agent_provider_dispatch_side_effects(8)
            .await
            .unwrap();
        assert!(dispatches.iter().any(|effect| matches!(
            effect,
            RuntimeSideEffect::DispatchAgentProvider { turn_id, .. }
                if turn_id == &task.turn_id
        )));
        let pending = handle.pending_agent_provider_tasks().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].turn_id, task.turn_id);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("agent: action failed; asking model to recover"),
        "{pane_text}"
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that async provider worker completions can apply a model-produced
/// execution through typed runtime ingress. This keeps successful provider
/// output on the same actor-owned transcript, audit, scheduler, prompt display,
/// and pane rendering path as the compatibility provider poller while allowing
/// future workers to perform network I/O outside the actor.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_applies_agent_provider_completion_events() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let task = pending[0].clone();
    let turn = crate::agent::AgentTurnRecord {
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        pane_id: task.pane_id.clone(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: crate::agent::AgentTurnState::Running,
        cooperation_mode: None,
    };
    let action = crate::agent::AgentAction {
        id: "say-1".to_string(),
        rationale: "complete with a visible summary".to_string(),
        payload: crate::agent::AgentActionPayload::Say {
            status: crate::agent::SayStatus::Final,
            text: "Typed completion applied.".to_string(),
            content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let response_batch = crate::agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        actions: vec![action.clone()],
        final_turn: true,
    };
    let execution = crate::agent::AgentTurnExecution {
        request: crate::agent::ModelRequest {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            reasoning_effort: task
                .model_profile
                .provider_options
                .get("reasoning_effort")
                .cloned()
                .or_else(|| task.model_profile.reasoning_profile.clone()),
            latency_preference: task.model_profile.latency_preference.clone(),
            prompt_cache_retention: task
                .model_profile
                .provider_options
                .get("prompt_cache_retention")
                .cloned(),
            max_output_tokens: task.model_profile.max_output_tokens(),
            prompt_cache_session_id: None,
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            available_mcp_tools: Vec::new(),
            interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
            allowed_actions: crate::agent::AllowedActionSet::for_capability(
                crate::agent::AgentCapability::RespondOnly,
            ),
            messages: vec![crate::agent::ModelMessage {
                role: crate::agent::ModelMessageRole::User,
                source: crate::agent::ContextSourceKind::UserInstruction,
                content: "summarize the pane".to_string(),
            }],
        },
        response: crate::agent::ModelResponse {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            raw_text: "Typed completion applied.".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(response_batch),
        },
        latest_response_usage: Default::default(),
        action_results: vec![crate::agent::ActionResult::succeeded(
            &turn,
            &action,
            vec!["Typed completion applied.".to_string()],
            Some(
                r#"{"kind":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"Typed completion applied."}"#
                    .to_string(),
            ),
        )],
        final_turn: true,
        terminal_state: crate::agent::AgentTurnState::Completed,
    };
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id,
            execution: Box::new(execution),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        exit.service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("Typed completion applied."),
        "{pane_text}"
    );
    assert_eq!(exit.commands_processed, 2);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that provider completions queue durable transcript entries for the
/// persistence worker when a transcript store is configured. The actor assigns
/// transcript sequence numbers and records the pane reference immediately, while
/// filesystem writes happen only after the persistence worker drains the typed
/// transcript side effect.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_agent_transcript_entries_to_persistence_worker() {
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-async-transcript-defer-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&transcript_root);
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    let mut service = test_service();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let transcript_path = transcript_store.transcript_path(&conversation_id).unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let task = pending[0].clone();
    let turn = crate::agent::AgentTurnRecord {
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        pane_id: task.pane_id.clone(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: crate::agent::AgentTurnState::Running,
        cooperation_mode: None,
    };
    let action = crate::agent::AgentAction {
        id: "say-1".to_string(),
        rationale: "complete with a visible summary".to_string(),
        payload: crate::agent::AgentActionPayload::Say {
            status: crate::agent::SayStatus::Final,
            text: "Typed transcript completion.".to_string(),
            content_type: crate::agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.to_string(),
        },
    };
    let response_batch = crate::agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        actions: vec![action.clone()],
        final_turn: true,
    };
    let execution = crate::agent::AgentTurnExecution {
        request: crate::agent::ModelRequest {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            reasoning_effort: task
                .model_profile
                .provider_options
                .get("reasoning_effort")
                .cloned()
                .or_else(|| task.model_profile.reasoning_profile.clone()),
            latency_preference: task.model_profile.latency_preference.clone(),
            prompt_cache_retention: task
                .model_profile
                .provider_options
                .get("prompt_cache_retention")
                .cloned(),
            max_output_tokens: task.model_profile.max_output_tokens(),
            prompt_cache_session_id: None,
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            available_mcp_tools: Vec::new(),
            interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
            allowed_actions: crate::agent::AllowedActionSet::for_capability(
                crate::agent::AgentCapability::RespondOnly,
            ),
            messages: vec![crate::agent::ModelMessage {
                role: crate::agent::ModelMessageRole::User,
                source: crate::agent::ContextSourceKind::UserInstruction,
                content: "summarize the pane".to_string(),
            }],
        },
        response: crate::agent::ModelResponse {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            raw_text: "Typed transcript completion.".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(response_batch),
        },
        latest_response_usage: Default::default(),
        action_results: vec![crate::agent::ActionResult::succeeded(
            &turn,
            &action,
            vec!["Typed transcript completion.".to_string()],
            Some(
                r#"{"kind":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"Typed transcript completion."}"#
                    .to_string(),
            ),
        )],
        final_turn: true,
        terminal_state: crate::agent::AgentTurnState::Completed,
    };
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id,
            execution: Box::new(execution),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert!(report.side_effects >= 2);
        assert!(!transcript_path.exists());

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert!(persistence.bytes_written > 0);

        let entries = transcript_store.inspect(&conversation_id).unwrap();
        assert!(!entries.is_empty());
        assert!(
            entries
                .iter()
                .any(|entry| { entry.content.contains("Typed transcript completion.") })
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert!(exit.commands_processed >= 4);
    exit.service.pane_processes_mut().terminate_all().unwrap();
    let _ = std::fs::remove_dir_all(transcript_root);
}

/// Verifies that submitted agent prompts are appended to the shared prompt
/// history through the persistence worker instead of being written while actor
/// state is applying the prompt. This keeps the hot input path non-blocking
/// while preserving the global prompt-history UX across sessions.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_agent_prompt_history_to_persistence_worker() {
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-async-prompt-history-defer-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&transcript_root);
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    let mut service = test_service();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let conversation_id = service
        .agent_shell_store()
        .get("%1")
        .unwrap()
        .session_id
        .clone();
    let prompt_history_path = transcript_store.prompt_history_file();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let response = handle
            .execute_agent_shell_command(primary, "remember this prompt".to_string())
            .await
            .unwrap();
        assert!(response.contains(r#""state":"running""#), "{response}");
        assert!(!prompt_history_path.exists());
        assert!(
            transcript_store
                .prompt_history(&conversation_id)
                .unwrap()
                .is_empty()
        );

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert!(persistence.bytes_written > 0);

        assert_eq!(
            transcript_store.prompt_history(&conversation_id).unwrap(),
            vec![String::from("remember this prompt")]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
    exit.service.pane_processes_mut().terminate_all().unwrap();
    let _ = std::fs::remove_dir_all(transcript_root);
}

/// Verifies that the async actor defers `/init` scaffold creation to the
/// persistence worker. The command path records the user-visible mutation
/// immediately, but the project instruction file is created by the async
/// persistence worker so the actor does not perform the potentially slow file
/// write inline.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_agent_init_scaffold_to_persistence_worker() {
    let root = std::env::temp_dir().join(format!(
        "mez-async-agent-init-defer-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let scaffold = root.join("AGENTS.md");
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let started = service
        .start_initial_pane_process(Some("sleep 30"))
        .unwrap();
    service
        .apply_pane_foreground_process_event(
            started.pane_id.clone(),
            "sleep",
            started.primary_pid,
            Some(root.to_string_lossy().to_string()),
        )
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let response = handle
            .execute_agent_shell_command(primary, "/init".to_string())
            .await
            .unwrap();
        assert!(response.contains(r#""kind":"mutated""#), "{response}");
        assert!(response.contains(r#""command":"init""#), "{response}");
        assert!(response.contains("created=true"), "{response}");
        assert!(
            !scaffold.exists(),
            "async actor should not create AGENTS.md before persistence worker drains"
        );

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert!(persistence.bytes_written > 0);

        let text = std::fs::read_to_string(&scaffold).unwrap();
        assert!(text.contains("# Repository Guidelines"), "{text}");
        assert!(
            text.contains("## Build, Test, and Development Commands"),
            "{text}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
    exit.service.pane_processes_mut().terminate_all().unwrap();
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that project-scoped approval persistence updates runtime policy
/// immediately while deferring the project config file write to the persistence
/// worker. This covers the approval workflow's config-producing path and
/// prevents actor-owned control requests from writing `.mezzanine/config.toml`
/// inline.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_project_approval_config_to_persistence_worker() {
    use crate::control::{decode_control_frame, encode_control_body};

    let root = std::env::temp_dir().join(format!(
        "mez-async-project-approval-config-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let project_config = root.join(".mezzanine/config.toml");
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 10)
        .unwrap();
    let started = service
        .start_initial_pane_process(Some("sleep 30"))
        .unwrap();
    service
        .apply_pane_foreground_process_event(
            started.pane_id.clone(),
            "sleep",
            started.primary_pid,
            Some(root.to_string_lossy().to_string()),
        )
        .unwrap();
    let approval_id = service
        .queue_blocked_approval(crate::permissions::BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            parent_agent_chain: vec!["agent-%1".to_string()],
            action_kind: "shell_command".to_string(),
            action_summary: "mez-test-command --flag".to_string(),
            declared_effects: vec!["unknown command effects".to_string()],
            matched_rules: vec!["default.prompt".to_string()],
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: crate::permissions::BlockedApprovalState::Pending,
            decision: None,
            redirect_instruction: None,
        })
        .unwrap();
    let deny_id = service
        .queue_blocked_approval(crate::permissions::BlockedApprovalRequest {
            id: String::new(),
            requesting_agent_id: "agent-%1".to_string(),
            pane_id: "%1".to_string(),
            parent_agent_chain: vec!["agent-%1".to_string()],
            action_kind: "shell_command".to_string(),
            action_summary: "mez-test-command --delete".to_string(),
            declared_effects: vec!["unknown command effects".to_string()],
            matched_rules: vec!["default.prompt".to_string()],
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            cooperation_mode: None,
            created_at_unix_seconds: None,
            decided_at_unix_seconds: None,
            decided_by_client_id: None,
            state: crate::permissions::BlockedApprovalState::Pending,
            decision: None,
            redirect_instruction: None,
        })
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let input = encode_control_body(&format!(
            r#"{{"jsonrpc":"2.0","id":"allow-project","method":"approval/decide","params":{{"approval_id":"{}","decision":"approve","scope":{{"persistence":"project"}},"idempotency_key":"allow-project"}}}}"#,
            approval_id
        ));
        let result = handle
            .handle_control_input_for_connection(
                input,
                4096,
                ControlConnectionState::trusted_existing_client(primary.clone()),
            )
            .await
            .unwrap();
        let (body, _) = decode_control_frame(&result.output, 4096).unwrap();
        assert!(body.contains(r#""state":"approved""#), "{body}");
        let input = encode_control_body(&format!(
            r#"{{"jsonrpc":"2.0","id":"deny-project","method":"approval/decide","params":{{"approval_id":"{}","decision":"disapprove","scope":{{"persistence":"project"}},"idempotency_key":"deny-project"}}}}"#,
            deny_id
        ));
        let result = handle
            .handle_control_input_for_connection(
                input,
                4096,
                ControlConnectionState::trusted_existing_client(primary),
            )
            .await
            .unwrap();
        let (body, _) = decode_control_frame(&result.output, 4096).unwrap();
        assert!(body.contains(r#""state":"disapproved""#), "{body}");
        assert!(
            !project_config.exists(),
            "async actor should not write project config before persistence worker drains"
        );

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 2);
        assert_eq!(persistence.completed, 2);
        assert_eq!(persistence.failed, 0);
        assert!(persistence.bytes_written > 0);

        let config_text = std::fs::read_to_string(&project_config).unwrap();
        assert!(config_text.contains(r#"approval_policy = "ask""#));
        assert!(config_text.contains(r#"match = "exact_sha256""#));
        assert!(config_text.contains(r#"decision = "allow""#));
        assert!(config_text.contains(r#"decision = "deny""#));
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(
        exit.service
            .permission_policy()
            .evaluate_shell_command("mez-test-command --flag"),
        crate::permissions::RuleDecision::Allow
    );
    assert_eq!(
        exit.service
            .permission_policy()
            .evaluate_shell_command("mez-test-command --delete"),
        crate::permissions::RuleDecision::Forbid
    );
    assert!(exit.commands_processed >= 4);
    exit.service.pane_processes_mut().terminate_all().unwrap();
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that actor-owned control dispatch can route snapshot requests
/// through a configured repository. The async daemon control service uses this
/// path when serving live `mez snapshot` requests, so `snapshot/list` must not
/// fail with the repository-missing error that applies to generic control
/// dispatch.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_handles_control_requests_with_snapshot_repository() {
    use crate::control::{decode_control_frame, encode_control_body};
    use crate::snapshot::SnapshotRepository;

    let root = std::env::temp_dir().join(format!(
        "mez-async-control-snapshots-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let snapshots = SnapshotRepository::new(root.join("snapshots"));
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 10)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut connection = ControlConnectionState::trusted_existing_client(primary.clone());
        let input = encode_control_body(
            r#"{"jsonrpc":"2.0","id":"snapshot-list","method":"snapshot/list","params":{}}"#,
        );
        let result = handle
            .handle_control_input_for_connection_with_snapshots(
                input,
                4096,
                connection,
                snapshots.clone(),
            )
            .await
            .unwrap();
        connection = result.connection;
        let (body, consumed) = decode_control_frame(&result.output, 4096).unwrap();
        assert_eq!(consumed, result.output.len());
        assert!(body.contains(r#""id":"snapshot-list""#), "{body}");
        assert!(body.contains(r#""snapshots":[]"#), "{body}");
        assert!(!body.contains("runtime snapshot repository is not configured"));

        let input = encode_control_body(
            r#"{"jsonrpc":"2.0","id":"snapshot-create","method":"snapshot/create","params":{"target":{"default":true},"name":"manual","idempotency_key":"async-snapshot-create"}}"#,
        );
        let result = handle
            .handle_control_input_for_connection_with_snapshots(
                input,
                4096,
                connection,
                snapshots.clone(),
            )
            .await
            .unwrap();
        connection = result.connection;
        let (body, _) = decode_control_frame(&result.output, 4096).unwrap();
        assert!(body.contains(r#""id":"snapshot-create""#), "{body}");
        assert!(body.contains(r#""name":"manual""#), "{body}");
        let response: serde_json::Value = serde_json::from_str(&body).unwrap();
        let snapshot_id = response["result"]["snapshot"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        let input = encode_control_body(&format!(
            r#"{{"jsonrpc":"2.0","id":"snapshot-resume","method":"snapshot/resume","params":{{"snapshot_id":"{}","idempotency_key":"async-snapshot-resume"}}}}"#,
            snapshot_id
        ));
        let result = handle
            .handle_control_input_for_connection_with_snapshots(
                input,
                4096,
                connection,
                snapshots.clone(),
            )
            .await
            .unwrap();
        connection = result.connection;
        let (body, _) = decode_control_frame(&result.output, 4096).unwrap();
        assert!(body.contains(r#""id":"snapshot-resume""#), "{body}");
        assert!(body.contains(r#""resumed":true"#), "{body}");
        assert!(body.contains(r#""primary_client_id""#), "{body}");

        let input = encode_control_body(&format!(
            r#"{{"jsonrpc":"2.0","id":"snapshot-delete","method":"snapshot/delete","params":{{"snapshot_id":"{}","idempotency_key":"async-snapshot-delete"}}}}"#,
            snapshot_id
        ));
        let result = handle
            .handle_control_input_for_connection_with_snapshots(input, 4096, connection, snapshots)
            .await
            .unwrap();
        let (body, _) = decode_control_frame(&result.output, 4096).unwrap();
        assert!(body.contains(r#""id":"snapshot-delete""#), "{body}");
        assert!(body.contains(r#""deleted":true"#), "{body}");
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(
        exit.commands_processed >= 8,
        "snapshot control should use a request plus completion actor command per operation: {:?}",
        exit.commands_processed
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that runtime `config/set` requests which target the user-private
/// config file update actor-owned runtime configuration immediately while
/// deferring the actual file replacement to the async persistence worker. This
/// prevents actor-owned control requests from performing inline config writes
/// while preserving the user-visible live reload semantics of the command.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_defers_user_config_mutation_to_persistence_worker() {
    use crate::control::{decode_control_frame, encode_control_body};

    let root = std::env::temp_dir().join(format!(
        "mez-async-user-config-mutation-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let config_root = root.join("config");
    let config_path = config_root.join("config.toml");
    std::fs::create_dir_all(&config_root).unwrap();
    std::fs::write(&config_path, "[history]\nlines = 10\n").unwrap();

    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 10)
        .unwrap();
    service.set_config_root(config_root);
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: Some(config_path.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: std::fs::read_to_string(&config_path).unwrap(),
        }])
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let config_path_json = serde_json::to_string(&config_path.to_string_lossy()).unwrap();
        let input = encode_control_body(&format!(
            r#"{{"jsonrpc":"2.0","id":"user-config-set","method":"config/set","params":{{"path":"history.lines","value":7,"persist":{{"scope":"user","path":{config_path_json}}},"idempotency_key":"user-config-set"}}}}"#
        ));
        let result = handle
            .handle_control_input_for_connection(
                input,
                4096,
                ControlConnectionState::trusted_existing_client(primary),
            )
            .await
            .unwrap();
        let (body, _) = decode_control_frame(&result.output, 4096).unwrap();
        assert!(body.contains(r#""applied":true"#), "{body}");
        assert!(body.contains(r#""persisted":true"#), "{body}");
        assert!(
            std::fs::read_to_string(&config_path)
                .unwrap()
                .contains("lines = 10"),
            "async actor should not replace the user config file before the persistence worker drains"
        );

        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert!(persistence.bytes_written > 0);
        assert!(
            std::fs::read_to_string(&config_path)
                .unwrap()
                .contains("lines = 7")
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.service.terminal_history_limit(), 7);
    assert!(exit.commands_processed >= 3);
    let _ = std::fs::remove_dir_all(root);
}

/// Verifies that an async provider completion which dispatches a shell command
/// also queues a runtime-owned shell transaction timer side effect. Without
/// this timer handoff, shell action timeouts still depend on the compatibility
/// tick loop instead of the dedicated Tokio timer worker. The first timer is
/// the short payload-receiver start deadline because the command body is still
/// waiting for the shell wrapper start marker.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_queues_shell_transaction_timer_after_provider_completion() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let ready = service
        .execute_terminal_command(&primary, "mark-pane-ready --acknowledge-risk --reason test")
        .unwrap();
    assert!(ready.contains("override=applied"), "{ready}");
    let start = service
        .execute_agent_shell_command(&primary, "print a marker")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let pending = service.pending_agent_provider_tasks();
    assert_eq!(pending.len(), 1);
    let task = pending[0].clone();
    let turn = crate::agent::AgentTurnRecord {
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        pane_id: task.pane_id.clone(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: crate::agent::AgentTurnState::Running,
        cooperation_mode: None,
    };
    let action = crate::agent::AgentAction {
        id: "shell-1".to_string(),
        rationale: "run a short shell command for the user".to_string(),
        payload: crate::agent::AgentActionPayload::ShellCommand {
            summary: "Print a shell marker.".to_string(),
            command: "printf 'async timer shell\\n'".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: Some(60_000),
        },
    };
    let response_batch = crate::agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        actions: vec![action.clone()],
        final_turn: false,
    };
    let execution = crate::agent::AgentTurnExecution {
        request: crate::agent::ModelRequest {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            reasoning_effort: task
                .model_profile
                .provider_options
                .get("reasoning_effort")
                .cloned()
                .or_else(|| task.model_profile.reasoning_profile.clone()),
            latency_preference: task.model_profile.latency_preference.clone(),
            prompt_cache_retention: task
                .model_profile
                .provider_options
                .get("prompt_cache_retention")
                .cloned(),
            max_output_tokens: task.model_profile.max_output_tokens(),
            prompt_cache_session_id: None,
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            available_mcp_tools: Vec::new(),
            interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
            allowed_actions: crate::agent::AllowedActionSet::for_capability(
                crate::agent::AgentCapability::Shell,
            ),
            messages: vec![crate::agent::ModelMessage {
                role: crate::agent::ModelMessageRole::User,
                source: crate::agent::ContextSourceKind::UserInstruction,
                content: "print a marker".to_string(),
            }],
        },
        response: crate::agent::ModelResponse {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            raw_text: "shell command response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(response_batch),
        },
        latest_response_usage: Default::default(),
        action_results: vec![crate::agent::ActionResult::running(
            &turn,
            &action,
            vec!["shell command accepted for pane execution".to_string()],
            Some(r#"{"state":"pending_dispatch"}"#.to_string()),
        )],
        final_turn: false,
        terminal_state: crate::agent::AgentTurnState::Running,
    };
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id.clone(),
            execution: Box::new(execution),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.side_effects, 2);
        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(timers.len(), 1);
        let RuntimeSideEffect::ScheduleTimer { key, delay_ms } = &timers[0] else {
            panic!("expected shell transaction timer side effect, got {timers:?}");
        };
        assert_eq!(key.kind, RuntimeTimerKind::ShellTransaction);
        assert!((0..=30 * 1000).contains(delay_ms), "{delay_ms}");
        assert!(!key.owner_id.is_empty());
        let scheduled_key = key.clone();
        let mut output_batch = RuntimeEventBatch::new();
        output_batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"shell output before marker\n".to_vec(),
        }));
        let output_report = handle.submit_runtime_events(output_batch).await.unwrap();
        assert_eq!(output_report.accepted, 1);
        assert_eq!(output_report.applied, 1);
        assert_eq!(output_report.side_effects, 2);
        let idle_timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(idle_timers.len(), 1);
        let RuntimeSideEffect::ScheduleTimer { key, delay_ms } = &idle_timers[0] else {
            panic!("expected idle cleanup timer side effect, got {idle_timers:?}");
        };
        assert_eq!(key.kind, RuntimeTimerKind::IdleCleanup);
        assert!(*delay_ms > 0, "{delay_ms}");

        let marker_output = format!(
            "\x1b]133;D;0;mez_marker={};mez_turn={};mez_agent=agent-%1;mez_pane=%1\x1b\\",
            scheduled_key.owner_id.as_str(),
            task.turn_id
        );
        let mut marker_batch = RuntimeEventBatch::new();
        marker_batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: marker_output.into_bytes(),
        }));
        let marker_report = handle.submit_runtime_events(marker_batch).await.unwrap();
        assert_eq!(marker_report.accepted, 1);
        assert_eq!(marker_report.applied, 1);
        assert!(marker_report.side_effects >= 2, "{marker_report:?}");
        let timer_cancellations = handle.drain_timer_side_effects(8).await.unwrap();
        assert!(
            timer_cancellations
                .iter()
                .any(|effect| matches!(effect, RuntimeSideEffect::CancelTimer { key } if key == &scheduled_key)),
            "shell transaction timer should be cancelled after marker: {timer_cancellations:?}"
        );
        assert!(
            timer_cancellations.iter().all(|effect| match effect {
                RuntimeSideEffect::CancelTimer { .. } => true,
                RuntimeSideEffect::ScheduleTimer { key, .. } => {
                    key.kind == RuntimeTimerKind::Bootstrap
                }
                _ => false,
            }),
            "only cancellation and bootstrap timer effects should be queued: {timer_cancellations:?}"
        );
        assert!(
            handle.shutdown().await.unwrap() == RuntimeLifecycleState::Running,
            "actor should shut down from running state"
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(
        exit.service.running_shell_transaction_timers().iter().all(
            |timer| timer.kind != crate::runtime::RuntimeShellTransactionTimerKind::AgentAction
        ),
        "agent shell transaction timer should be settled"
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the async-owned pane path keeps the pane shell alive after the
/// first agent shell command dispatch. This covers the production daemon shape:
/// a real PTY shell is claimed by the Tokio pane worker, a provider completion
/// queues a shell action, and a later pane input still reaches the same shell
/// instead of observing a process exit or supervisor shutdown.
#[tokio::test(flavor = "current_thread")]
async fn async_pane_worker_keeps_shell_alive_after_first_agent_command() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", crate::agent::AgentLogLevel::Verbose)
        .unwrap();
    service.permission_policy_mut().set_approval_bypass(true);

    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let pane_worker_handle = handle.clone();
    let client_handle = handle.clone();
    let pane_worker_done = StdArc::new(AtomicBool::new(false));
    let pane_worker_stop = StdArc::clone(&pane_worker_done);
    let (pane_worker_stopped_tx, pane_worker_stopped_rx) = tokio::sync::oneshot::channel();

    let pane_worker = async move {
        let report = run_async_pane_process_supervisor_service(
            pane_worker_handle,
            AsyncPaneProcessSupervisorServiceConfig {
                max_polls: u64::MAX,
                take_limit: 8,
                idle_interval: Duration::from_millis(1),
                pane_service: AsyncPaneProcessServiceConfig {
                    max_polls: u64::MAX,
                    output_drain_limit: 1,
                    drain_limit: 8,
                    idle_interval: Duration::from_millis(1),
                    foreground_metadata_interval: Duration::from_secs(60),
                },
            },
            move |_, state| {
                pane_worker_stop.load(Ordering::SeqCst)
                    || matches!(state, RuntimeLifecycleState::Stopping)
            },
        )
        .await
        .unwrap();
        let _ = pane_worker_stopped_tx.send(());
        report
    };

    let client = async move {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        let ready = client_handle
            .execute_terminal_command(
                primary.clone(),
                "mark-pane-ready --acknowledge-risk --reason async-agent-test".to_string(),
            )
            .await
            .unwrap();
        assert!(ready.contains("override=applied"), "{ready}");

        let start = client_handle
            .execute_agent_shell_command(primary, "print a marker".to_string())
            .await
            .unwrap();
        assert!(start.contains(r#""state":"running""#), "{start}");
        let task = client_handle
            .pending_agent_provider_tasks()
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.turn_id == "turn-1")
            .expect("agent prompt should queue turn-1 provider task");
        let turn = crate::agent::AgentTurnRecord {
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            pane_id: task.pane_id.clone(),
            trigger: crate::agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            state: crate::agent::AgentTurnState::Running,
            cooperation_mode: None,
        };
        let action = crate::agent::AgentAction {
            id: "shell-1".to_string(),
            rationale: "print a marker".to_string(),
            payload: crate::agent::AgentActionPayload::ShellCommand {
                summary: "Print a marker".to_string(),
                command: "printf 'AGENT_ASYNC_FIRST_COMMAND\\n'".to_string(),
                interactive: false,
                stateful: false,
                timeout_ms: Some(60_000),
            },
        };
        let batch = crate::agent::MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            actions: vec![action.clone()],
            final_turn: false,
        };
        let execution = crate::agent::AgentTurnExecution {
            request: crate::agent::ModelRequest {
                provider: task.model_profile.provider.clone(),
                model: task.model_profile.model.clone(),
                reasoning_effort: task
                    .model_profile
                    .provider_options
                    .get("reasoning_effort")
                    .cloned()
                    .or_else(|| task.model_profile.reasoning_profile.clone()),
                latency_preference: task.model_profile.latency_preference.clone(),
                prompt_cache_retention: task
                    .model_profile
                    .provider_options
                    .get("prompt_cache_retention")
                    .cloned(),
                max_output_tokens: task.model_profile.max_output_tokens(),
                prompt_cache_session_id: None,
                turn_id: task.turn_id.clone(),
                agent_id: task.agent_id.clone(),
                available_mcp_tools: Vec::new(),
                interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
                allowed_actions: crate::agent::AllowedActionSet::for_capability(
                    crate::agent::AgentCapability::Shell,
                ),
                messages: vec![crate::agent::ModelMessage {
                    role: crate::agent::ModelMessageRole::User,
                    source: crate::agent::ContextSourceKind::UserInstruction,
                    content: "print a marker".to_string(),
                }],
            },
            response: crate::agent::ModelResponse {
                provider: task.model_profile.provider.clone(),
                model: task.model_profile.model.clone(),
                raw_text: "shell command response".to_string(),
                usage: Default::default(),
                quota_usage: Default::default(),
                action_batch: Some(batch),
            },
            latest_response_usage: Default::default(),
            action_results: vec![crate::agent::ActionResult::running(
                &turn,
                &action,
                vec!["shell command accepted for pane execution".to_string()],
                Some(r#"{"state":"pending_dispatch"}"#.to_string()),
            )],
            final_turn: false,
            terminal_state: crate::agent::AgentTurnState::Running,
        };
        let mut provider_batch = RuntimeEventBatch::new();
        provider_batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id.clone(),
            execution: Box::new(execution),
        }));
        let provider_report = client_handle
            .submit_runtime_events(provider_batch)
            .await
            .unwrap();
        assert_eq!(provider_report.accepted, 1);
        assert_eq!(provider_report.applied, 1);

        let first_seen = wait_for_rendered_text(
            &client_handle,
            ClientViewRole::Primary,
            "AGENT_ASYNC_FIRST_COMMAND",
        )
        .await
        .unwrap();
        assert!(
            first_seen.contains("AGENT_ASYNC_FIRST_COMMAND"),
            "{first_seen}"
        );
        let mut first_shell_transaction_settled = false;
        for _ in 0..200 {
            let timer_effects = client_handle.drain_timer_side_effects(16).await.unwrap();
            if timer_effects.iter().any(|effect| {
                matches!(
                    effect,
                    RuntimeSideEffect::CancelTimer { key }
                        if key.kind == RuntimeTimerKind::ShellTransaction
                )
            }) {
                first_shell_transaction_settled = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            first_shell_transaction_settled,
            "first shell transaction should settle before submitting continuation"
        );

        let mut next_task = None;
        for _ in 0..200 {
            if let Some(task) = client_handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .into_iter()
                .find(|pending| pending.turn_id == "turn-1")
            {
                next_task = Some(task);
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let next_task =
            next_task.expect("first shell transaction should queue provider continuation");
        let second_turn = crate::agent::AgentTurnRecord {
            turn_id: next_task.turn_id.clone(),
            agent_id: next_task.agent_id.clone(),
            pane_id: next_task.pane_id.clone(),
            trigger: crate::agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 2,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            state: crate::agent::AgentTurnState::Running,
            cooperation_mode: None,
        };
        let second_action = crate::agent::AgentAction {
            id: "shell-2".to_string(),
            rationale: "verify the pane shell still accepts input".to_string(),
            payload: crate::agent::AgentActionPayload::ShellCommand {
                summary: "Print a second marker".to_string(),
                command: "printf 'ASYNC_PANE_STILL_ALIVE\\n'".to_string(),
                interactive: false,
                stateful: false,
                timeout_ms: Some(60_000),
            },
        };
        let second_batch = crate::agent::MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "test action batch rationale".to_string(),
            thought: None,
            turn_id: next_task.turn_id.clone(),
            agent_id: next_task.agent_id.clone(),
            actions: vec![second_action.clone()],
            final_turn: false,
        };
        let second_execution = crate::agent::AgentTurnExecution {
            request: crate::agent::ModelRequest {
                provider: next_task.model_profile.provider.clone(),
                model: next_task.model_profile.model.clone(),
                reasoning_effort: next_task
                    .model_profile
                    .provider_options
                    .get("reasoning_effort")
                    .cloned()
                    .or_else(|| next_task.model_profile.reasoning_profile.clone()),
                latency_preference: next_task.model_profile.latency_preference.clone(),
                prompt_cache_retention: next_task
                    .model_profile
                    .provider_options
                    .get("prompt_cache_retention")
                    .cloned(),
                max_output_tokens: next_task.model_profile.max_output_tokens(),
                prompt_cache_session_id: None,
                turn_id: next_task.turn_id.clone(),
                agent_id: next_task.agent_id.clone(),
                available_mcp_tools: Vec::new(),
                interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
                allowed_actions: crate::agent::AllowedActionSet::for_capability(
                    crate::agent::AgentCapability::Shell,
                ),
                messages: vec![crate::agent::ModelMessage {
                    role: crate::agent::ModelMessageRole::User,
                    source: crate::agent::ContextSourceKind::UserInstruction,
                    content: "print a second marker".to_string(),
                }],
            },
            response: crate::agent::ModelResponse {
                provider: next_task.model_profile.provider.clone(),
                model: next_task.model_profile.model.clone(),
                raw_text: "second shell command response".to_string(),
                usage: Default::default(),
                quota_usage: Default::default(),
                action_batch: Some(second_batch),
            },
            latest_response_usage: Default::default(),
            action_results: vec![crate::agent::ActionResult::running(
                &second_turn,
                &second_action,
                vec!["second shell command accepted for pane execution".to_string()],
                Some(r#"{"state":"pending_dispatch"}"#.to_string()),
            )],
            final_turn: false,
            terminal_state: crate::agent::AgentTurnState::Running,
        };
        let mut second_provider_batch = RuntimeEventBatch::new();
        second_provider_batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(next_task.agent_id).unwrap(),
            turn_id: next_task.turn_id,
            execution: Box::new(second_execution),
        }));
        let second_provider_report = client_handle
            .submit_runtime_events(second_provider_batch)
            .await
            .unwrap();
        assert_eq!(second_provider_report.accepted, 1);
        assert_eq!(second_provider_report.applied, 1);
        let mut second_shell_transaction_settled = false;
        for _ in 0..200 {
            let timer_effects = client_handle.drain_timer_side_effects(16).await.unwrap();
            if timer_effects.iter().any(|effect| {
                matches!(
                    effect,
                    RuntimeSideEffect::CancelTimer { key }
                        if key.kind == RuntimeTimerKind::ShellTransaction
                )
            }) {
                second_shell_transaction_settled = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            second_shell_transaction_settled,
            "second shell transaction should settle before ending the test client"
        );
        let alive_seen = wait_for_rendered_text(
            &client_handle,
            ClientViewRole::Primary,
            "ASYNC_PANE_STILL_ALIVE",
        )
        .await
        .unwrap();
        assert!(
            alive_seen.contains("ASYNC_PANE_STILL_ALIVE"),
            "{alive_seen}"
        );
        assert_eq!(
            client_handle.lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        pane_worker_done.store(true, Ordering::SeqCst);
        pane_worker_stopped_rx
            .await
            .expect("pane worker should stop before actor shutdown");
        assert_eq!(
            client_handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), supervisor_report, mut actor_exit) = tokio::join!(client, pane_worker, actor.run());
    assert_eq!(
        actor_exit.service.lifecycle_state(),
        RuntimeLifecycleState::Running
    );
    assert!(supervisor_report.spawned_workers >= 1);
    assert_eq!(
        supervisor_report.terminal_state,
        RuntimeLifecycleState::Running
    );
    actor_exit
        .service
        .pane_processes_mut()
        .terminate_all()
        .unwrap();
}

/// Verifies that a provider-completed shell action whose pane cannot accept
/// shell input fails the agent turn instead of failing the async runtime actor
/// request. This reproduces the daemon-exit class where the model's first shell
/// command reaches runtime dispatch before a pane process is available.
#[tokio::test(flavor = "current_thread")]
async fn async_provider_completed_shell_dispatch_error_fails_turn_without_exiting_runtime() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .set_log_level("%1", crate::agent::AgentLogLevel::Trace)
        .unwrap();
    service.permission_policy_mut().set_approval_bypass(true);
    let start = service
        .execute_agent_shell_command(&primary, "list files")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service
        .pending_agent_provider_tasks()
        .into_iter()
        .find(|task| task.turn_id == "turn-1")
        .expect("agent prompt should queue turn-1 provider task");
    let turn = crate::agent::AgentTurnRecord {
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        pane_id: task.pane_id.clone(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: crate::agent::AgentTurnState::Running,
        cooperation_mode: None,
    };
    let action = crate::agent::AgentAction {
        id: "shell-1".to_string(),
        rationale: "list files".to_string(),
        payload: crate::agent::AgentActionPayload::ShellCommand {
            summary: "List files in the current directory".to_string(),
            command: "ls".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: Some(60_000),
        },
    };
    let response_batch = crate::agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        actions: vec![action.clone()],
        final_turn: false,
    };
    let execution = crate::agent::AgentTurnExecution {
        request: crate::agent::ModelRequest {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            reasoning_effort: task
                .model_profile
                .provider_options
                .get("reasoning_effort")
                .cloned()
                .or_else(|| task.model_profile.reasoning_profile.clone()),
            latency_preference: task.model_profile.latency_preference.clone(),
            prompt_cache_retention: task
                .model_profile
                .provider_options
                .get("prompt_cache_retention")
                .cloned(),
            max_output_tokens: task.model_profile.max_output_tokens(),
            prompt_cache_session_id: None,
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            available_mcp_tools: Vec::new(),
            interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
            allowed_actions: crate::agent::AllowedActionSet::for_capability(
                crate::agent::AgentCapability::Shell,
            ),
            messages: vec![crate::agent::ModelMessage {
                role: crate::agent::ModelMessageRole::User,
                source: crate::agent::ContextSourceKind::UserInstruction,
                content: "list files".to_string(),
            }],
        },
        response: crate::agent::ModelResponse {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            raw_text: "shell command response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(response_batch),
        },
        latest_response_usage: Default::default(),
        action_results: vec![crate::agent::ActionResult::running(
            &turn,
            &action,
            vec!["shell command accepted for pane execution".to_string()],
            Some(r#"{"state":"pending_dispatch"}"#.to_string()),
        )],
        final_turn: false,
        terminal_state: crate::agent::AgentTurnState::Running,
    };
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let client = async move {
        let mut provider_batch = RuntimeEventBatch::new();
        provider_batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id.clone(),
            execution: Box::new(execution),
        }));
        let report = handle.submit_runtime_events(provider_batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(
            handle.lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), actor_exit) = tokio::join!(client, actor.run());
    assert_eq!(
        actor_exit.service.lifecycle_state(),
        RuntimeLifecycleState::Running
    );
    assert!(actor_exit.service.pending_agent_provider_tasks().is_empty());
    assert!(!actor_exit.service.agent_turn_is_running("turn-1"));
    assert_eq!(
        actor_exit
            .service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    let pane_text = actor_exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(
        pane_text.contains("shell command failed before execution"),
        "{pane_text}"
    );
    assert!(pane_text.contains("pane process not found"), "{pane_text}");
}

/// Verifies malformed provider-completion action state fails only the affected
/// agent turn instead of failing the async runtime actor request.
///
/// Provider completions arrive after the provider claim has already been
/// cleared. If applying the completion discovers an impossible internal action
/// state, such as a running network result whose action is missing from the
/// returned batch, the runtime must settle the turn as failed and keep the
/// daemon usable for other panes.
#[tokio::test(flavor = "current_thread")]
async fn async_provider_completion_application_error_fails_turn_without_exiting_runtime() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "research patch behavior")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    let task = service
        .pending_agent_provider_tasks()
        .into_iter()
        .find(|task| task.turn_id == "turn-1")
        .expect("agent prompt should queue turn-1 provider task");
    let turn = crate::agent::AgentTurnRecord {
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        pane_id: task.pane_id.clone(),
        trigger: crate::agent::AgentTurnTrigger::UserPrompt,
        started_at_unix_seconds: 1,
        policy_profile: "default".to_string(),
        model_profile: "default".to_string(),
        parent_turn_id: None,
        state: crate::agent::AgentTurnState::Running,
        cooperation_mode: None,
    };
    let batch_action = crate::agent::AgentAction {
        id: "fetch-listed".to_string(),
        rationale: "fetch the listed source".to_string(),
        payload: crate::agent::AgentActionPayload::FetchUrl {
            url: "https://example.com/listed".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let missing_action = crate::agent::AgentAction {
        id: "fetch-missing-result".to_string(),
        rationale: "this result no longer has a matching batch action".to_string(),
        payload: crate::agent::AgentActionPayload::FetchUrl {
            url: "https://example.com/missing".to_string(),
            format: None,
            max_bytes: None,
        },
    };
    let response_batch = crate::agent::MaapBatch {
        protocol: "maap/1".to_string(),
        rationale: "test action batch rationale".to_string(),
        thought: None,
        turn_id: task.turn_id.clone(),
        agent_id: task.agent_id.clone(),
        actions: vec![batch_action],
        final_turn: false,
    };
    let execution = crate::agent::AgentTurnExecution {
        request: crate::agent::ModelRequest {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            reasoning_effort: task
                .model_profile
                .provider_options
                .get("reasoning_effort")
                .cloned()
                .or_else(|| task.model_profile.reasoning_profile.clone()),
            latency_preference: task.model_profile.latency_preference.clone(),
            prompt_cache_retention: task
                .model_profile
                .provider_options
                .get("prompt_cache_retention")
                .cloned(),
            max_output_tokens: task.model_profile.max_output_tokens(),
            prompt_cache_session_id: None,
            turn_id: task.turn_id.clone(),
            agent_id: task.agent_id.clone(),
            available_mcp_tools: Vec::new(),
            interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
            allowed_actions: crate::agent::AllowedActionSet::for_capability(
                crate::agent::AgentCapability::NetworkFetch,
            ),
            messages: vec![crate::agent::ModelMessage {
                role: crate::agent::ModelMessageRole::User,
                source: crate::agent::ContextSourceKind::UserInstruction,
                content: "research patch behavior".to_string(),
            }],
        },
        response: crate::agent::ModelResponse {
            provider: task.model_profile.provider.clone(),
            model: task.model_profile.model.clone(),
            raw_text: "network completion response".to_string(),
            usage: Default::default(),
            quota_usage: Default::default(),
            action_batch: Some(response_batch),
        },
        latest_response_usage: Default::default(),
        action_results: vec![crate::agent::ActionResult::running(
            &turn,
            &missing_action,
            vec!["network action accepted".to_string()],
            Some(r#"{"state":"pending_network"}"#.to_string()),
        )],
        final_turn: false,
        terminal_state: crate::agent::AgentTurnState::Running,
    };
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let client = async move {
        let mut provider_batch = RuntimeEventBatch::new();
        provider_batch.push(RuntimeEvent::AgentProvider(AgentProviderEvent::Completed {
            agent_id: AgentId::opaque(task.agent_id).unwrap(),
            turn_id: task.turn_id.clone(),
            execution: Box::new(execution),
        }));
        let report = handle.submit_runtime_events(provider_batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(
            handle.lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut actor_exit) = tokio::join!(client, actor.run());
    assert_eq!(
        actor_exit.service.lifecycle_state(),
        RuntimeLifecycleState::Running
    );
    assert!(actor_exit.service.pending_agent_provider_tasks().is_empty());
    assert!(!actor_exit.service.agent_turn_is_running("turn-1"));
    assert_eq!(
        actor_exit
            .service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    let pane_text = actor_exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("Failed after"), "{pane_text}");
    actor_exit
        .service
        .pane_processes_mut()
        .terminate_all()
        .unwrap();
}

/// Waits for rendered primary-client text to contain a target string and
/// returns the most recent rendered text for assertions.
async fn wait_for_rendered_text(
    handle: &super::AsyncRuntimeSessionHandle,
    role: ClientViewRole,
    needle: &str,
) -> Result<String> {
    let mut last_text = String::new();
    for _ in 0..1000 {
        if let Some(view) = handle
            .render_client_view(
                role,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await?
        {
            last_text = view.lines.join("\n");
            if last_text.contains(needle) {
                return Ok(last_text);
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    Err(MezError::invalid_state(format!(
        "timed out waiting for rendered text {needle:?}; last render: {last_text}"
    )))
}

/// Verifies that configured provider task failures stay scoped to the pending
/// agent turn when the async provider service polls the runtime queue. This
/// protects the interactive prompt UX from crashing the daemon when the default
/// provider cannot run, such as a fresh OpenAI setup with no attached auth
/// store.
#[tokio::test(flavor = "current_thread")]
async fn async_agent_provider_service_keeps_running_after_prompt_provider_failure() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let start = service
        .execute_agent_shell_command(&primary, "summarize the pane")
        .unwrap();
    assert!(start.contains(r#""state":"running""#), "{start}");
    assert_eq!(service.pending_agent_provider_tasks().len(), 1);
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let service_handle = handle.clone();
    let timer_handle = handle.clone();
    let shutdown_handle = handle.clone();
    let service = async move {
        run_async_agent_provider_service(
            &service_handle,
            AsyncAgentProviderServiceConfig::new(1).unwrap(),
            |polls, _| polls >= 4,
        )
        .await
        .unwrap()
    };
    let timer = async move {
        run_async_runtime_timer_side_effect_service(
            &timer_handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 8,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            0,
            |polls, _| polls >= 8,
        )
        .await
        .unwrap()
    };
    let client = async move {
        let (report, timer_report) = tokio::join!(service, timer);
        let _ = shutdown_handle.shutdown().await.unwrap();
        (report, timer_report)
    };

    let ((report, timer_report), mut exit) = tokio::join!(client, actor.run());

    assert!(report.polls >= 2, "{report:?}");
    assert!(report.idle_polls >= 1, "{report:?}");
    assert_eq!(report.executions, 0);
    assert!(timer_report.fired >= 1, "{timer_report:?}");
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        exit.service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    assert!(exit.commands_processed >= 4);
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the async provider service wakes from runtime notifications
/// when a prompt queues provider work after an initially idle poll. This ties
/// agent prompt latency to notification delivery once the actor observes a user
/// prompt.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_agent_provider_service_wakes_when_prompt_queues_work() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let provider_handle = handle.clone();
    let provider = async move {
        run_async_agent_provider_service(
            &provider_handle,
            AsyncAgentProviderServiceConfig::new(1).unwrap(),
            |polls, _| polls >= 2,
        )
        .await
        .unwrap()
    };
    let (prompt_done_tx, prompt_done_rx) = tokio::sync::oneshot::channel();
    let prompt_handle = handle.clone();
    let prompt = async move {
        tokio::task::yield_now().await;
        let start = prompt_handle
            .execute_agent_shell_command(primary, "summarize the pane".to_string())
            .await
            .unwrap();
        assert!(start.contains(r#""state":"running""#), "{start}");
        let _ = prompt_done_tx.send(());
    };
    let watchdog = async move {
        let _ = prompt_done_rx.await;
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        panic!("provider service did not process prompt work after actor notification");
    };
    let provider_or_watchdog = async move {
        tokio::select! {
            report = provider => report,
            _ = watchdog => unreachable!("provider wakeup watchdog panicked"),
        }
    };
    let client = async {
        let (report, ()) = tokio::join!(provider_or_watchdog, prompt);
        assert!(report.polls >= 2, "{report:?}");
        assert!(report.idle_polls >= 1, "{report:?}");
        assert_eq!(report.executions, 0);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        report
    };

    let (report, mut exit) = tokio::join!(client, actor.run());
    assert_eq!(report.terminal_state, RuntimeLifecycleState::Running);
    assert!(exit.service.pending_agent_provider_tasks().is_empty());
    assert_eq!(
        exit.service
            .agent_shell_store()
            .get("%1")
            .and_then(|session| session.running_turn_id.as_deref()),
        None
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies async actor serializes lifecycle render and shutdown.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_serializes_lifecycle_render_and_shutdown() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        assert_eq!(
            handle.lifecycle_state().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        let view = handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(80, 24).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(view.authoritative_size, Size::new(80, 24).unwrap());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert_eq!(exit.commands_processed, 3);
}

/// Verifies that bootstrap dispatch is driven by typed pane-output events
/// instead of a later compatibility tick. A prompt marker makes the pending
/// pane bootstrap-ready; the actor should immediately enqueue the hidden
/// bootstrap wrapper for the async pane worker and schedule its timeout timer.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_dispatches_bootstrap_after_prompt_ready_output_event() {
    let mut service = test_service();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_processes_for_async_owner(8)
            .await
            .unwrap();
        assert_eq!(processes.len(), 1);
        let (pane_id, mut process) = processes.pop().unwrap();
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: pane_id.clone(),
            bytes: b"\x1b]133;A\x1b\\".to_vec(),
        }));

        let report = handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.applied, 1);

        let pane_effects = handle
            .drain_pane_io_side_effects(pane_id.as_str(), 8)
            .await
            .unwrap();
        let bootstrap_bytes = match pane_effects.as_slice() {
            [
                RuntimeSideEffect::WritePaneInput {
                    pane_id: effect_pane,
                    bytes,
                },
            ] if effect_pane == &pane_id => bytes,
            effects => panic!("expected one bootstrap pane write, got {effects:?}"),
        };
        let bootstrap_wrapper = std::str::from_utf8(bootstrap_bytes).unwrap();
        assert!(
            bootstrap_wrapper.contains("MEZ_COMMAND_B64") && bootstrap_wrapper.contains("base64"),
            "bootstrap wrapper should be queued for the pane worker"
        );

        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        assert!(
            timer_effects.iter().any(|effect| matches!(
                effect,
                RuntimeSideEffect::ScheduleTimer { key, .. }
                    if key.kind == RuntimeTimerKind::Bootstrap
            )),
            "bootstrap timeout should be scheduled by actor timer side effects: {timer_effects:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        let _ = process.terminate(Duration::from_millis(10));
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pane_processes_mut().terminate_all().is_ok());
}

/// Verifies async attached terminal step uses runtime rendered view.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_step_uses_runtime_rendered_view() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let readiness = vec![
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            },
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Output,
                fd: 1,
                interest: TerminalFdInterest::write(),
                readable: false,
                writable: true,
                hangup: false,
                error: false,
            },
        ];
        let status = ClientStatusLine {
            kind: ClientStatusKind::Plain,
            text: "attached".to_string(),
        };
        let plan = plan_async_attached_terminal_client_step(
            &handle,
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            TerminalClientLoopConfig::default(),
            &readiness,
            Some(b"\x01\""),
            Some(&status),
        )
        .await
        .unwrap();

        assert_eq!(
            plan.actions,
            vec![crate::terminal::TerminalClientLoopAction::ExecuteMux(
                MuxAction::SplitPaneHorizontal
            )]
        );
        assert_eq!(plan.output_lines.len(), 24);
        assert_eq!(plan.output_lines[23].trim_end(), "attached");
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert_eq!(exit.commands_processed, 2);
}

/// Verifies async attached terminal step can be applied through actor.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_step_can_be_applied_through_actor() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let readiness = vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: true,
            writable: false,
            hangup: false,
            error: false,
        }];
        let (_plan, application) = plan_and_apply_async_attached_terminal_client_step(
            &handle,
            AsyncAttachedTerminalStepRequest {
                primary_client_id: primary.clone(),
                role: ClientViewRole::Primary,
                client_size: Size::new(80, 24).unwrap(),
                config: TerminalClientLoopConfig::default(),
                readiness: &readiness,
                input: Some(b"hello\n"),
                status: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(application.forwarded_bytes, 6);
        assert_eq!(
            handle.drain_pane_io_side_effects("%1", 8).await.unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"hello\n".to_vec(),
            }]
        );
        let large_input = vec![b'x'; 468_586];
        let (_plan, application) = plan_and_apply_async_attached_terminal_client_step(
            &handle,
            AsyncAttachedTerminalStepRequest {
                primary_client_id: primary.clone(),
                role: ClientViewRole::Primary,
                client_size: Size::new(80, 24).unwrap(),
                config: TerminalClientLoopConfig::default(),
                readiness: &readiness,
                input: Some(&large_input),
                status: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(application.forwarded_bytes, large_input.len());
        assert_eq!(
            handle
                .drain_pane_io_side_effects("%1", usize::MAX)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: large_input,
            }]
        );
        let split = AttachedTerminalClientStepPlan {
            actions: vec![TerminalClientLoopAction::ExecuteMux(
                MuxAction::SplitPaneVertical,
            )],
            output_lines: Vec::new(),
            input_hangup: false,
            output_hangup: false,
            error_roles: Vec::new(),
        };
        let split_application = handle
            .apply_attached_terminal_step_plan(primary, split)
            .await
            .unwrap();
        assert_eq!(split_application.mux_actions_applied, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());

    assert!(
        exit.commands_processed >= 5,
        "actor should process client-step, drain, split, and shutdown requests"
    );
    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that pane input produced by the compatibility terminal-step path is
/// converted into actor side effects after a pane has been handed to an async
/// process owner. This protects older control-socket attach calls while the
/// production foreground path moves to fully deferred pane I/O.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_drains_service_deferred_input_after_pane_handoff() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_processes_for_async_owner(8)
            .await
            .unwrap();
        assert_eq!(processes.len(), 1);
        let (pane_id, mut process) = processes.pop().unwrap();
        let step = AttachedTerminalClientStepPlan {
            actions: vec![TerminalClientLoopAction::ForwardToPane(b"hello\n".to_vec())],
            output_lines: Vec::new(),
            input_hangup: false,
            output_hangup: false,
            error_roles: Vec::new(),
        };
        let application = handle
            .apply_attached_terminal_step_plan_inline_pane_io(primary, step)
            .await
            .unwrap();
        assert_eq!(application.forwarded_bytes, 6);
        assert_eq!(
            handle
                .drain_pane_io_side_effects(pane_id.as_str(), 8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id,
                bytes: b"hello\n".to_vec(),
            }]
        );
        let _ = process.terminate(Duration::from_millis(10));
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pane_processes_mut().terminate_all().is_ok());
}

/// Verifies that pane close commands produce async termination side effects
/// after pane process ownership has moved out of the manager. Without this
/// bridge, `kill-pane` would close runtime layout state while leaving the
/// worker-owned PTY process alive.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_drains_service_deferred_termination_after_pane_handoff() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut processes = handle
            .take_running_pane_processes_for_async_owner(8)
            .await
            .unwrap();
        assert_eq!(processes.len(), 1);
        let (pane_id, mut process) = processes.pop().unwrap();
        let output = handle
            .execute_terminal_command(primary, "kill-pane --force".to_string())
            .await
            .unwrap();
        assert!(output.contains("closed=true"));
        assert_eq!(
            handle
                .drain_pane_io_side_effects(pane_id.as_str(), 8)
                .await
                .unwrap(),
            vec![RuntimeSideEffect::TerminatePane {
                pane_id,
                force: true,
            }]
        );
        let _ = process.terminate(Duration::from_millis(10));
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());
    assert!(exit.service.pane_processes_mut().terminate_all().is_ok());
}

/// Verifies that the attached terminal loop can run in deferred pane I/O mode,
/// where forwarded primary input becomes a pane side effect instead of a direct
/// synchronous manager write. This is the mode required once live pane
/// processes are owned by supervised async workers.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_can_defer_pane_input_to_worker() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: true,
            writable: false,
            hangup: false,
            error: false,
        }]],
        input_batches: vec![b"hello\n".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop_deferred_pane_io(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 1);
        assert_eq!(
            report.actions,
            vec![TerminalClientLoopAction::ForwardToPane(b"hello\n".to_vec())]
        );
        assert_eq!(report.output_frames, 0);
        assert_eq!(io.written_batches.len(), 0);
        assert_eq!(
            handle.drain_pane_io_side_effects("%1", 8).await.unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"hello\n".to_vec(),
            }]
        );
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());

    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies large foreground input is drained across bounded client reads.
///
/// Host paste payloads can be larger than one attached-terminal read. The
/// client loop must keep reading subsequent chunks and queue every accepted
/// byte as ordered pane-input side effects instead of treating the first
/// viewport-sized read as the whole paste.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_preserves_large_deferred_paste_across_reads() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let paste = b"large-paste-".repeat(16);
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![
            vec![AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            }],
            vec![AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            }],
            vec![AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            }],
        ],
        input_batches: vec![paste.clone()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop_deferred_pane_io(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 3,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 3);
        let queued = handle.drain_pane_io_side_effects("%1", 8).await.unwrap();
        let forwarded = queued
            .into_iter()
            .filter_map(|effect| match effect {
                RuntimeSideEffect::WritePaneInput { bytes, .. } => Some(bytes),
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(forwarded, paste);
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());

    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that the higher-level attached-terminal client service can use the
/// deferred pane I/O mode across its prepolled batch boundary. Foreground
/// daemon attach uses this service wrapper, so the production handoff needs the
/// service-level path as well as the single-loop path.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_can_defer_pane_input_to_worker() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: true,
            writable: false,
            hangup: false,
            error: false,
        }]],
        input_batches: vec![b"service-input\n".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_service_deferred_pane_io(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.batches, 2);
        assert!(
            report
                .loop_report
                .actions
                .contains(&TerminalClientLoopAction::ForwardToPane(
                    b"service-input\n".to_vec()
                ))
        );
        assert_eq!(
            handle.drain_pane_io_side_effects("%1", 8).await.unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"service-input\n".to_vec(),
            }]
        );
        let _ = handle.shutdown().await.unwrap();
    };

    let ((), mut exit) = tokio::join!(client, actor.run());

    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies that slow client-output flushing does not block foreground input
/// routing. The first service batch starts a large frame and leaves bytes
/// pending; the second batch observes user input before that frame has been
/// fully written and still forwards the payload to the primary pane worker.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_routes_input_while_output_is_pending() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .start_initial_pane_process(Some("cat >/dev/null"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = SlowOutputAttachedTerminalLoopIo::new(
        vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: true,
            writable: false,
            hangup: false,
            error: false,
        }]],
        vec![b"hello\n".to_vec()],
        64,
    );

    let client = async {
        let report = run_async_attached_terminal_client_service_deferred_pane_io(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.batches, 2);
        assert!(report.loop_report.partial_writes > 0);
        assert!(report.loop_report.pending_output_bytes > 0);
        assert!(
            report
                .loop_report
                .actions
                .contains(&TerminalClientLoopAction::ForwardToPane(
                    b"hello\n".to_vec()
                ))
        );
        assert_eq!(
            handle.drain_pane_io_side_effects("%1", 8).await.unwrap(),
            vec![RuntimeSideEffect::WritePaneInput {
                pane_id: "%1".to_string(),
                bytes: b"hello\n".to_vec(),
            }]
        );
        assert_eq!(io.completed_frames, 0);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), mut exit) = tokio::join!(client, actor.run());

    exit.service.pane_processes_mut().terminate_all().unwrap();
}

/// Verifies async attached terminal loop renders and applies primary actions.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_renders_and_applies_primary_actions() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            },
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Output,
                fd: 1,
                interest: TerminalFdInterest::write(),
                readable: false,
                writable: true,
                hangup: false,
                error: false,
            },
        ]],
        input_batches: vec![b"\x01\x1b[C".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| {
                Ok(Some(ClientStatusLine {
                    kind: ClientStatusKind::Plain,
                    text: "attached".to_string(),
                }))
            },
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 1);
        assert_eq!(
            report.actions,
            vec![TerminalClientLoopAction::ExecuteMux(MuxAction::FocusPane(
                crate::terminal::PaneFocusDirection::Right
            ))]
        );
        assert_eq!(report.output_frames, 2);
        assert_eq!(io.written_batches.len(), 2);
        assert_eq!(
            io.written_batches.last().unwrap()[23].trim_end(),
            "attached"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed > 0);
}

/// Verifies that foreground client-step errors are shown through actor-owned
/// overlay state instead of a private prompt-error acknowledgement loop. This
/// keeps the async loop non-blocking even when no acknowledgement input is
/// available in the current batch.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_routes_runtime_errors_to_actor_overlay() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let wrong_primary = ClientId::new('c', 4242);
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            },
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Output,
                fd: 1,
                interest: TerminalFdInterest::write(),
                readable: false,
                writable: true,
                hangup: false,
                error: false,
            },
        ]],
        input_batches: vec![b"hello\n".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = tokio::time::timeout(
            Duration::from_millis(250),
            run_async_attached_terminal_client_loop(
                &handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: primary.clone(),
                    primary_client_id: Some(wrong_primary),
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                |_| Ok(None),
            ),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(report.output_frames, 2);
        assert_eq!(io.written_batches.len(), 2);
        let error_frame = io.written_batches.last().unwrap();
        assert!(
            error_frame
                .iter()
                .any(|line| line.contains("operation requires the primary client")),
            "{:?}",
            error_frame
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    let overlay_view = exit
        .service
        .render_client_view(
            ClientViewRole::Primary,
            Size::new(80, 24).unwrap(),
            &TerminalClientLoopConfig::default(),
        )
        .unwrap()
        .unwrap();
    assert!(
        overlay_view
            .lines
            .iter()
            .any(|line| line.contains("operation requires the primary client")),
        "{:?}",
        overlay_view.lines
    );
    assert!(exit.commands_processed >= 4);
}

/// Verifies async attached terminal loop runs actor owned command prompt.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_runs_actor_owned_command_prompt() {
    let transcript_root = std::env::temp_dir().join(format!(
        "mez-async-command-prompt-history-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&transcript_root);
    let transcript_store = AgentTranscriptStore::new(transcript_root.clone());
    let mut service = test_service();
    service.set_agent_transcript_store(transcript_store.clone());
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let command_history_path = transcript_store.command_prompt_history_file();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![
            vec![
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: TerminalFdInterest::read(),
                    readable: true,
                    writable: false,
                    hangup: false,
                    error: false,
                },
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Output,
                    fd: 1,
                    interest: TerminalFdInterest::write(),
                    readable: false,
                    writable: true,
                    hangup: false,
                    error: false,
                },
            ],
            vec![
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: TerminalFdInterest::read(),
                    readable: true,
                    writable: false,
                    hangup: false,
                    error: false,
                },
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Output,
                    fd: 1,
                    interest: TerminalFdInterest::write(),
                    readable: false,
                    writable: true,
                    hangup: false,
                    error: false,
                },
            ],
            vec![
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: TerminalFdInterest::read(),
                    readable: true,
                    writable: false,
                    hangup: false,
                    error: false,
                },
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Output,
                    fd: 1,
                    interest: TerminalFdInterest::write(),
                    readable: false,
                    writable: true,
                    hangup: false,
                    error: false,
                },
            ],
        ],
        input_batches: vec![
            b"\x01:".to_vec(),
            b"list-buffers\r".to_vec(),
            b"\x1b".to_vec(),
        ],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 2,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 2);
        assert_eq!(report.output_frames, 4);
        assert_eq!(
            report.actions,
            vec![
                TerminalClientLoopAction::ExecuteMux(MuxAction::EnterCommandPrompt),
                TerminalClientLoopAction::ForwardToPane(b"list-buffers\r".to_vec())
            ]
        );
        assert_eq!(io.written_batches.len(), 4);
        assert_eq!(io.written_batches[1][23].trim_end(), "▐ :");
        assert!(
            io.written_batches[3]
                .iter()
                .any(|line| line.contains("buffers: 0"))
        );
        assert!(
            io.written_batches[3]
                .iter()
                .any(|line| line.contains("source: runtime"))
        );
        assert!(
            io.written_batches[3]
                .iter()
                .any(|line| line.contains("status: empty"))
        );
        assert!(!command_history_path.exists());
        let persistence = run_async_persistence_side_effect_service(
            &handle,
            AsyncRuntimeSideEffectServiceConfig {
                max_polls: 2,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
            },
            |polls, _| polls >= 2,
        )
        .await
        .unwrap();
        assert_eq!(persistence.drained, 1);
        assert_eq!(persistence.completed, 1);
        assert_eq!(persistence.failed, 0);
        assert_eq!(
            transcript_store.command_prompt_history().unwrap(),
            vec![String::from("list-buffers")]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed > 0);
    let _ = std::fs::remove_dir_all(transcript_root);
}

/// Verifies async attached terminal loop routes agent shell input non modally.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_routes_agent_shell_input_non_modally() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![
            vec![
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: TerminalFdInterest::read(),
                    readable: true,
                    writable: false,
                    hangup: false,
                    error: false,
                },
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Output,
                    fd: 1,
                    interest: TerminalFdInterest::write(),
                    readable: false,
                    writable: true,
                    hangup: false,
                    error: false,
                },
            ],
            vec![
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: TerminalFdInterest::read(),
                    readable: true,
                    writable: false,
                    hangup: false,
                    error: false,
                },
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Output,
                    fd: 1,
                    interest: TerminalFdInterest::write(),
                    readable: false,
                    writable: true,
                    hangup: false,
                    error: false,
                },
            ],
            vec![
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: TerminalFdInterest::read(),
                    readable: false,
                    writable: false,
                    hangup: false,
                    error: false,
                },
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Output,
                    fd: 1,
                    interest: TerminalFdInterest::write(),
                    readable: false,
                    writable: true,
                    hangup: false,
                    error: false,
                },
            ],
        ],
        input_batches: vec![b"\x01a".to_vec(), b"/status\r".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 3,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 3);
        assert_eq!(
            report.actions,
            vec![
                TerminalClientLoopAction::ExecuteMux(MuxAction::ToggleAgentShell),
                TerminalClientLoopAction::ForwardToPane(b"/status\r".to_vec()),
            ]
        );
        assert_eq!(report.output_frames, 4);
        assert_eq!(io.written_batches.len(), 4);
        assert!(
            io.written_batches[1]
                .iter()
                .any(|line| line.trim_end() == "▐ agent>")
        );
        let status_output = io.written_batches[2].join("\n");
        assert!(
            status_output.contains("│ Permissions")
                && status_output.contains("preset read-only")
                && !status_output.contains("Quota Usage"),
            "{status_output}"
        );
        assert!(
            !io.written_batches[2]
                .iter()
                .any(|line| line.contains("agent-shell:"))
        );
        assert!(
            !io.written_batches[2]
                .iter()
                .any(|line| line.trim_end() == "▐ agent>"),
            "status display should use the pager overlay instead of pane prompt rows"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 6);
}

/// Verifies that submitting pane-local agent prompt input redraws the client
/// frame in the same attached-terminal loop pass. Without this refresh, the
/// submitted prompt text stayed visible until a later agent state change caused
/// the next render, which made queued follow-up prompts feel blocked.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_clears_agent_prompt_on_submit() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            },
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Output,
                fd: 1,
                interest: TerminalFdInterest::write(),
                readable: false,
                writable: true,
                hangup: false,
                error: false,
            },
        ]],
        input_batches: vec![b"list files\r".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 1);
        assert_eq!(
            report.actions,
            vec![TerminalClientLoopAction::ForwardToPane(
                b"list files\r".to_vec()
            )]
        );
        assert_eq!(report.output_frames, 1);
        assert_eq!(io.written_batches.len(), 1);
        let refreshed = io.written_batches.last().unwrap();
        assert!(
            refreshed
                .iter()
                .any(|line| line.trim_end().starts_with("▐ agent> thinking")),
            "{refreshed:?}"
        );
        assert!(
            !refreshed
                .iter()
                .any(|line| line.trim_end() == "▐ agent> list files"),
            "{refreshed:?}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert_eq!(exit.service.pending_agent_provider_tasks().len(), 1);
    let pane_text = exit
        .service
        .pane_screen("%1")
        .unwrap()
        .normal_content_lines()
        .join("\n");
    assert!(pane_text.contains("user> list files"), "{pane_text}");
    assert!(exit.commands_processed >= 4);
}

/// Verifies that leaving pane-local agent mode invalidates the attached
/// terminal's differential frame state before repainting. The agent prompt is a
/// Mezzanine-owned overlay, while the underlying shell prompt is PTY-owned; a
/// full redraw at this boundary keeps cursor placement and stale prompt rows
/// from leaking after the mode switch.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_full_redraws_after_agent_prompt_exit() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeResizingAttachedTerminalLoopIo {
        inner: FakeAttachedTerminalLoopIo {
            readiness_batches: vec![vec![
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Input,
                    fd: 0,
                    interest: TerminalFdInterest::read(),
                    readable: true,
                    writable: false,
                    hangup: false,
                    error: false,
                },
                AttachedTerminalFdReadiness {
                    role: AttachedTerminalFdRole::Output,
                    fd: 1,
                    interest: TerminalFdInterest::write(),
                    readable: false,
                    writable: true,
                    hangup: false,
                    error: false,
                },
            ]],
            input_batches: vec![b"/exit\r".to_vec()],
            written_batches: Vec::new(),
            write_error_kinds: Vec::new(),
        },
        terminal_size_batches: Vec::new(),
        invalidated_output_frames: 0,
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.iterations, 1);
        assert_eq!(report.output_frames, 1);
        assert_eq!(io.invalidated_output_frames, 1);
        assert_eq!(io.inner.written_batches.len(), 1);
        assert!(
            !io.inner.written_batches[0]
                .iter()
                .any(|line| line.contains("▐ agent>")),
            "{:?}",
            io.inner.written_batches[0]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 4);
}

/// Verifies async attached terminal loop renders observer without applying input.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_renders_observer_without_applying_input() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Input,
                fd: 0,
                interest: TerminalFdInterest::read(),
                readable: true,
                writable: false,
                hangup: false,
                error: false,
            },
            AttachedTerminalFdReadiness {
                role: AttachedTerminalFdRole::Output,
                fd: 1,
                interest: TerminalFdInterest::write(),
                readable: false,
                writable: true,
                hangup: false,
                error: false,
            },
        ]],
        input_batches: vec![b"\x1b=".to_vec()],
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Observer,
                client_id: ClientId::new('c', 9001),
                primary_client_id: None,
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| {
                Ok(Some(ClientStatusLine {
                    kind: ClientStatusKind::PendingObserver,
                    text: "observe".to_string(),
                }))
            },
        )
        .await
        .unwrap();

        assert!(report.actions.is_empty());
        assert_eq!(report.output_frames, 1);
        assert_eq!(io.input_batches.len(), 0);
        assert_eq!(io.written_batches[0][23].trim_end(), "observer: observe");
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 4);
}

/// Verifies that direct foreground client-loop rendering schedules the
/// actor-owned cursor and status timers. Foreground attached clients still use a
/// direct render path while the refactor is in progress, and those frames must
/// seed timer-driven invalidations before the blind batch sleep can be removed.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_loop_schedules_render_timers_after_direct_flush() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Output,
            fd: 1,
            interest: TerminalFdInterest::write(),
            readable: false,
            writable: true,
            hangup: false,
            error: false,
        }]],
        input_batches: Vec::new(),
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_loop(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.output_frames, 1);
        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        assert_eq!(timers.len(), 1);
        let RuntimeSideEffect::ScheduleTimer { key, .. } = &timers[0] else {
            panic!("expected status refresh timer: {timers:?}");
        };
        assert_eq!(key.kind, RuntimeTimerKind::StatusRefresh);
        assert_eq!(key.owner_id, primary.to_string());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 5);
}

/// Verifies async attached terminal service runs batches until hangup.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_runs_batches_until_hangup() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: false,
            writable: false,
            hangup: true,
            error: false,
        }]],
        input_batches: Vec::new(),
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Observer,
                client_id: ClientId::new('c', 9002),
                primary_client_id: None,
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 8 },
            |iteration| {
                Ok(Some(ClientStatusLine {
                    kind: ClientStatusKind::Plain,
                    text: format!("service-{iteration}"),
                }))
            },
        )
        .await
        .unwrap();

        assert_eq!(report.batches, 2);
        assert_eq!(report.loop_report.iterations, 2);
        assert_eq!(report.loop_report.output_frames, 1);
        assert_eq!(report.loop_report.input_hangups, 1);
        assert_eq!(io.written_batches.len(), 1);
        assert_eq!(io.written_batches[0][23].trim_end(), "service-0");
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 5);
}

/// Verifies that an attached-terminal service wakes between batches when the
/// actor queues side effects. This keeps render/output work responsive now that
/// quiet periods no longer have a periodic foreground redraw sleep.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_wakes_between_batches_on_side_effects() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![Vec::new(), Vec::new()],
        input_batches: Vec::new(),
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };

    let service_handle = handle.clone();
    let notify_handle = handle.clone();
    let client = async move {
        let service = run_async_attached_terminal_client_service(
            &service_handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Observer,
                client_id: ClientId::new('c', 9022),
                primary_client_id: None,
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
            |_| Ok(None),
        );
        let notifier = async {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(10)).await;
            notify_handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                    client_id: ClientId::new('c', 9022),
                    reason: RenderInvalidationReason::CursorBlink,
                }])
                .await
                .unwrap();
        };
        let (report, ()) = tokio::time::timeout(Duration::from_millis(250), async {
            tokio::join!(service, notifier)
        })
        .await
        .unwrap();
        let report = report.unwrap();
        assert_eq!(report.batches, 2);
        assert_eq!(report.loop_report.iterations, 2);
        assert_eq!(
            service_handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
}

/// Verifies that a quiet attached-terminal service does not advance from an
/// idle timeout after its initial frame. This protects the foreground path from
/// reintroducing a periodic redraw clock that consumes CPU while the terminal is
/// idle.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_has_no_idle_batch_timer() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let mut io = IdleAsyncAttachedTerminalLoopIo::new(write_count.clone(), write_notify.clone());

    let client = async {
        let service = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Observer,
                client_id: ClientId::new('c', 9023),
                primary_client_id: None,
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
            |_| Ok(None),
        );
        tokio::pin!(service);
        tokio::select! {
            _ = write_notify.notified() => {}
            result = &mut service => panic!("attached terminal service completed before idling: {result:?}"),
        }
        let advance = async {
            tokio::time::advance(Duration::from_millis(250)).await;
        };
        let (result, ()) = tokio::join!(
            tokio::time::timeout(Duration::from_millis(200), &mut service),
            advance
        );
        assert!(result.is_err());
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 4);
}

/// Verifies that the attached-terminal service treats queued render work as
/// level-triggered state instead of relying only on a retained notify permit.
///
/// A background service can consume the one stored side-effect notification
/// before the foreground client reaches its idle wait. The render invalidation
/// itself remains queued in the actor, and the client must drain it before
/// awaiting fresh input so a quiet terminal cannot strand a repaint forever.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_drains_stranded_render_effect_before_waiting() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let client_id = ClientId::new('c', 9024);
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let mut io = IdleAsyncAttachedTerminalLoopIo::new(write_count.clone(), write_notify.clone());

    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                client_id: client_id.clone(),
                reason: RenderInvalidationReason::PaneOutput,
            }])
            .await
            .unwrap();
        handle.wait_for_runtime_side_effects().await;

        let report = tokio::time::timeout(
            Duration::from_millis(250),
            run_async_attached_terminal_client_service(
                &handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Observer,
                    client_id,
                    primary_client_id: None,
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
                |_| Ok(None),
            ),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(report.batches, 2);
        assert_eq!(report.loop_report.output_frames, 2);
        assert_eq!(write_count.load(Ordering::SeqCst), 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 7);
}

/// Verifies that bursty render invalidations are coalesced behind the
/// configured foreground render rate and still produce one trailing frame.
///
/// This protects slow remote clients from being flooded by intermediate frames
/// while preserving the final visible state after an output burst settles.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_rate_limits_bursty_render_invalidations() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());

    let client = async {
        let service_handle = handle.clone();
        let service_primary = primary.clone();
        let service_write_notify = write_notify.clone();
        let service_write_count = write_count.clone();
        let service_task = tokio::spawn(async move {
            let mut io =
                IdleAsyncAttachedTerminalLoopIo::new(service_write_count, service_write_notify);
            run_async_attached_terminal_client_service(
                &service_handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: service_primary.clone(),
                    primary_client_id: Some(service_primary),
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
                |_| Ok(None),
            )
            .await
        });

        write_notify.notified().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        for _ in 0..3 {
            handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::PaneOutput,
                }])
                .await
                .unwrap();
        }
        tokio::task::yield_now().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_millis(199)).await;
        tokio::task::yield_now().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        tokio::time::advance(Duration::from_millis(1)).await;
        for _ in 0..8 {
            if write_count.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(write_count.load(Ordering::SeqCst), 2);

        let report = tokio::time::timeout(Duration::from_millis(1), service_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(report.batches, 2);
        assert_eq!(report.loop_report.output_frames, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 7);
}

/// Verifies that the foreground attached-terminal service polls terminal
/// dimensions while otherwise idle.
///
/// Some hosting terminals can change cell dimensions without producing an
/// input or runtime event that wakes the render service. The idle resize poll
/// should notice that size change, invalidate retained diff state, and repaint
/// exactly once instead of waiting for user interaction.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_polls_terminal_size_while_idle() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let invalidate_count = StdArc::new(AtomicUsize::new(0));

    let client = async {
        let service_handle = handle.clone();
        let service_primary = primary.clone();
        let service_write_count = write_count.clone();
        let service_write_notify = write_notify.clone();
        let service_invalidate_count = invalidate_count.clone();
        let service_task = tokio::spawn(async move {
            let mut io = InvalidatingIdleAsyncAttachedTerminalLoopIo::new(
                service_write_count,
                service_write_notify,
                service_invalidate_count,
            )
            .with_terminal_size_batches(vec![None, Some(Size::new(100, 30).unwrap())]);
            run_async_attached_terminal_client_service(
                &service_handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: service_primary.clone(),
                    primary_client_id: Some(service_primary),
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
                |_| Ok(None),
            )
            .await
        });

        write_notify.notified().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        let deadline = Instant::now() + Duration::from_millis(500);
        while write_count.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(write_count.load(Ordering::SeqCst), 2);
        assert_eq!(invalidate_count.load(Ordering::SeqCst), 1);

        let report = tokio::time::timeout(Duration::from_millis(1), service_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(report.terminal_resizes, 1);
        assert_eq!(report.loop_report.output_frames, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 5);
    assert_eq!(
        exit.service.session().authoritative_size,
        Size::new(100, 30).unwrap()
    );
}

/// Verifies that resize render invalidations interrupt an already pending
/// ordinary render-rate wait. Slow remote terminals can leave pane-output
/// refreshes coalesced behind the frame cadence, but a hosting terminal resize
/// changes the visible geometry and must immediately discard retained diff
/// state before repainting instead of waiting for the next pane-output tick.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_resize_bypasses_pending_render_rate_limit() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let invalidate_count = StdArc::new(AtomicUsize::new(0));

    let client = async {
        let service_handle = handle.clone();
        let service_primary = primary.clone();
        let service_write_count = write_count.clone();
        let service_write_notify = write_notify.clone();
        let service_invalidate_count = invalidate_count.clone();
        let service_task = tokio::spawn(async move {
            let mut io = InvalidatingIdleAsyncAttachedTerminalLoopIo::new(
                service_write_count,
                service_write_notify,
                service_invalidate_count,
            );
            run_async_attached_terminal_client_service(
                &service_handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: service_primary.clone(),
                    primary_client_id: Some(service_primary),
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
                |_| Ok(None),
            )
            .await
        });

        write_notify.notified().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                client_id: primary.clone(),
                reason: RenderInvalidationReason::PaneOutput,
            }])
            .await
            .unwrap();
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_millis(50)).await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);

        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                client_id: primary.clone(),
                reason: RenderInvalidationReason::Resize,
            }])
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_millis(1), write_notify.notified())
            .await
            .unwrap();
        assert_eq!(write_count.load(Ordering::SeqCst), 2);
        assert_eq!(invalidate_count.load(Ordering::SeqCst), 1);

        let report = tokio::time::timeout(Duration::from_millis(1), service_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(report.batches, 2);
        assert_eq!(report.loop_report.output_frames, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 7);
}

/// Verifies that a newer rate-limited render supersedes stale pending output.
///
/// Slow clients can leave bytes from an older frame pending. During rapid pane
/// output, the attached client should wait for the next render tick and write
/// the latest frame instead of streaming obsolete pending bytes immediately.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_attached_terminal_service_does_not_flush_stale_pending_output_before_render_tick() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let write_count = StdArc::new(AtomicUsize::new(0));
    let write_notify = StdArc::new(tokio::sync::Notify::new());
    let pending_output_bytes = StdArc::new(AtomicUsize::new(0));
    let stale_flushes = StdArc::new(AtomicUsize::new(0));

    let client = async {
        let service_handle = handle.clone();
        let service_primary = primary.clone();
        let service_write_count = write_count.clone();
        let service_write_notify = write_notify.clone();
        let service_pending_output_bytes = pending_output_bytes.clone();
        let service_stale_flushes = stale_flushes.clone();
        let service_task = tokio::spawn(async move {
            let mut io = SupersedablePendingOutputIo::new(
                service_write_count,
                service_write_notify,
                service_pending_output_bytes,
                service_stale_flushes,
            );
            run_async_attached_terminal_client_service(
                &service_handle,
                &mut io,
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: service_primary.clone(),
                    primary_client_id: Some(service_primary),
                    client_size: Size::new(80, 24).unwrap(),
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        max_input_bytes: 64,
                    },
                },
                AsyncAttachedTerminalClientServiceConfig { max_batches: 2 },
                |_| Ok(None),
            )
            .await
        });

        write_notify.notified().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        pending_output_bytes.store(1024, Ordering::SeqCst);

        for _ in 0..3 {
            handle
                .queue_runtime_side_effects(vec![RuntimeSideEffect::RenderClient {
                    client_id: primary.clone(),
                    reason: RenderInvalidationReason::PaneOutput,
                }])
                .await
                .unwrap();
        }

        tokio::task::yield_now().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        assert_eq!(stale_flushes.load(Ordering::SeqCst), 0);

        tokio::time::advance(Duration::from_millis(199)).await;
        tokio::task::yield_now().await;
        assert_eq!(write_count.load(Ordering::SeqCst), 1);
        assert_eq!(stale_flushes.load(Ordering::SeqCst), 0);

        tokio::time::advance(Duration::from_millis(1)).await;
        for _ in 0..8 {
            if write_count.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(write_count.load(Ordering::SeqCst), 2);
        assert_eq!(stale_flushes.load(Ordering::SeqCst), 0);
        assert_eq!(pending_output_bytes.load(Ordering::SeqCst), 0);

        let report = tokio::time::timeout(Duration::from_millis(1), service_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(report.batches, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());
    assert!(exit.commands_processed >= 7);
}

/// Verifies that a closed foreground terminal output endpoint is treated as a
/// normal hangup instead of bubbling a `BrokenPipe` I/O error to the top-level
/// CLI error handler during clean primary shutdown or terminal teardown.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_treats_broken_pipe_as_output_hangup() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Output,
            fd: 1,
            interest: TerminalFdInterest::write(),
            readable: false,
            writable: true,
            hangup: false,
            error: false,
        }]],
        input_batches: Vec::new(),
        written_batches: Vec::new(),
        write_error_kinds: vec![std::io::ErrorKind::BrokenPipe],
    };

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Observer,
                client_id: ClientId::new('c', 9003),
                primary_client_id: None,
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 4 },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.batches, 1);
        assert_eq!(report.loop_report.output_hangups, 1);
        assert_eq!(report.loop_report.output_frames, 0);
        assert!(io.written_batches.is_empty());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 5);
}

/// Verifies that the long-lived attached-terminal service treats an observed
/// terminal size change as authoritative for the primary client. This covers
/// the runtime path used by foreground sessions after a hosting terminal resize:
/// the client loop observes the new size, updates session geometry through the
/// actor, and subsequent rendering uses the resized authoritative dimensions.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_propagates_primary_terminal_resize() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeResizingAttachedTerminalLoopIo {
        inner: FakeAttachedTerminalLoopIo {
            readiness_batches: vec![Vec::new()],
            input_batches: Vec::new(),
            written_batches: Vec::new(),
            write_error_kinds: Vec::new(),
        },
        terminal_size_batches: vec![Some(Size::new(100, 30).unwrap())],
        invalidated_output_frames: 0,
    };

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 1 },
            |_| Ok(None),
        )
        .await
        .unwrap();
        let view = handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(100, 30).unwrap(),
                TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(report.terminal_resizes, 1);
        assert_eq!(view.authoritative_size, Size::new(100, 30).unwrap());
        assert_eq!(view.client_size, Size::new(100, 30).unwrap());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 5);
}

/// Verifies that a rapid sequence of foreground terminal-size changes
/// reschedules resize debounce work to the newest generation. Slow remote
/// clients can deliver resize signals close together, so the service should
/// cancel older debounce timers instead of letting each intermediate size force
/// a separate delayed full repaint.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_coalesces_resize_storm_timers() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeResizingAttachedTerminalLoopIo {
        inner: FakeAttachedTerminalLoopIo {
            readiness_batches: vec![Vec::new(), Vec::new()],
            input_batches: Vec::new(),
            written_batches: Vec::new(),
            write_error_kinds: Vec::new(),
        },
        terminal_size_batches: vec![
            Some(Size::new(100, 30).unwrap()),
            Some(Size::new(120, 35).unwrap()),
            Some(Size::new(130, 40).unwrap()),
        ],
        invalidated_output_frames: 0,
    };

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig {
                    resize_debounce_ms: 25,
                    ..TerminalClientLoopConfig::default()
                },
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 3 },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.terminal_resizes, 3);
        let timer_effects = handle.drain_timer_side_effects(8).await.unwrap();
        let resize_timer_effects = timer_effects
            .into_iter()
            .filter(|effect| match effect {
                RuntimeSideEffect::ScheduleTimer { key, .. }
                | RuntimeSideEffect::CancelTimer { key } => {
                    key.kind == RuntimeTimerKind::ResizeDebounce
                }
                _ => false,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            resize_timer_effects,
            vec![
                RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::ResizeDebounce,
                        primary.as_str(),
                        1,
                    ),
                    delay_ms: 200,
                },
                RuntimeSideEffect::CancelTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::ResizeDebounce,
                        primary.as_str(),
                        1,
                    ),
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::ResizeDebounce,
                        primary.as_str(),
                        2,
                    ),
                    delay_ms: 200,
                },
                RuntimeSideEffect::CancelTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::ResizeDebounce,
                        primary.as_str(),
                        2,
                    ),
                },
                RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::ResizeDebounce,
                        primary.as_str(),
                        3,
                    ),
                    delay_ms: 200,
                },
            ]
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 13);
}

/// Verifies that resize handling immediately invalidates retained foreground
/// output state and also queues a resize debounce timer. The immediate
/// invalidation gives the resized terminal a full refresh right away, while the
/// actor-owned timer still coalesces follow-up resize work without a blind
/// compatibility client-loop deadline.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_schedules_resize_debounce_timer() {
    let mut service = test_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[terminal]\nresize_debounce_ms = 1\n".to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeResizingAttachedTerminalLoopIo {
        inner: FakeAttachedTerminalLoopIo {
            readiness_batches: vec![Vec::new(), Vec::new()],
            input_batches: Vec::new(),
            written_batches: Vec::new(),
            write_error_kinds: Vec::new(),
        },
        terminal_size_batches: vec![Some(Size::new(100, 30).unwrap()), None],
        invalidated_output_frames: 0,
    };

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 1 },
            |_| Ok(None),
        )
        .await
        .unwrap();

        assert_eq!(report.terminal_resizes, 1);
        assert_eq!(io.invalidated_output_frames, 1);
        let timers = handle.drain_timer_side_effects(8).await.unwrap();
        let resize_timer = timers
            .iter()
            .find(|effect| {
                matches!(
                    effect,
                    RuntimeSideEffect::ScheduleTimer { key, .. }
                        if key.kind == RuntimeTimerKind::ResizeDebounce
                )
            })
            .unwrap_or_else(|| panic!("expected resize debounce timer: {timers:?}"));
        let RuntimeSideEffect::ScheduleTimer { key, delay_ms } = resize_timer else {
            panic!("expected resize debounce timer: {timers:?}");
        };
        assert_eq!(key.kind, RuntimeTimerKind::ResizeDebounce);
        assert_eq!(key.owner_id, primary.to_string());
        assert_eq!(*delay_ms, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed >= 6);
}

/// Verifies async attached terminal service can be supervised by name.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_can_be_supervised_by_name() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let actor_handle = handle.clone();
    let io = FakeAttachedTerminalLoopIo {
        readiness_batches: vec![vec![AttachedTerminalFdReadiness {
            role: AttachedTerminalFdRole::Input,
            fd: 0,
            interest: TerminalFdInterest::read(),
            readable: false,
            writable: false,
            hangup: true,
            error: false,
        }]],
        input_batches: Vec::new(),
        written_batches: Vec::new(),
        write_error_kinds: Vec::new(),
    };
    let service = build_async_attached_terminal_client_service(
        "attached-terminal-primary",
        handle,
        io,
        AsyncAttachedTerminalLoopRequest {
            role: ClientViewRole::Observer,
            client_id: ClientId::new('c', 9004),
            primary_client_id: None,
            client_size: Size::new(80, 24).unwrap(),
            terminal_config: TerminalClientLoopConfig::default(),
            loop_config: AttachedTerminalClientLoopConfig {
                max_iterations: 1,
                max_input_bytes: 64,
            },
        },
        AsyncAttachedTerminalClientServiceConfig { max_batches: 4 },
        |_| Ok(None),
    )
    .unwrap();

    let actor_task = tokio::spawn(actor.run());
    let report = supervise_async_runtime_services(vec![service], std::future::pending())
        .await
        .unwrap();
    assert!(!report.shutdown_requested);
    assert_eq!(
        report.services,
        vec![AsyncRuntimeServiceReport {
            name: "attached-terminal-primary".to_string(),
            exit: AsyncRuntimeServiceExit::completed(2),
        }]
    );
    assert_eq!(
        actor_handle.shutdown().await.unwrap(),
        RuntimeLifecycleState::Running
    );
    let exit = actor_task.await.unwrap();
    assert!(exit.commands_processed >= 4);
}

/// Verifies async attached terminal service exits cleanly after primary detach.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_attached_terminal_service_exits_cleanly_after_primary_detach() {
    let mut service = test_service();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .detach_primary(&primary, Size::new(80, 24).unwrap())
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let mut io = FakeAttachedTerminalLoopIo::default();

    let client = async {
        let report = run_async_attached_terminal_client_service(
            &handle,
            &mut io,
            AsyncAttachedTerminalLoopRequest {
                role: ClientViewRole::Primary,
                client_id: primary.clone(),
                primary_client_id: Some(primary.clone()),
                client_size: Size::new(80, 24).unwrap(),
                terminal_config: TerminalClientLoopConfig::default(),
                loop_config: AttachedTerminalClientLoopConfig {
                    max_iterations: 1,
                    max_input_bytes: 64,
                },
            },
            AsyncAttachedTerminalClientServiceConfig { max_batches: 4 },
            |_| Ok(None),
        )
        .await
        .unwrap();
        assert_eq!(report.batches, 0);
        assert!(report.stopped_by_lifecycle);
        assert_eq!(report.terminal_state, RuntimeLifecycleState::Detached);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Detached
        );
    };

    let ((), exit) = tokio::join!(client, actor.run());

    assert!(exit.commands_processed > 0);
}

/// Verifies async actor rejects requests after shutdown.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_actor_rejects_requests_after_shutdown() {
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let retained_handle = handle.clone();

    let client = async {
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), _exit) = tokio::join!(client, actor.run());
    let error = retained_handle.lifecycle_state().await.unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
}

/// Verifies async control connection authorizes and round trips control frame.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_control_connection_authorizes_and_round_trips_control_frame() {
    use crate::control::{decode_control_frame, encode_control_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let input = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );

    let client = async {
        client_stream.write_all(&input).await.unwrap();
        let mut output = vec![0; 4096];
        let read = client_stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, consumed) = decode_control_frame(&output, 4096).unwrap();
        assert_eq!(consumed, output.len());
        assert!(body.contains(r#""control/initialize""#));
    };
    let server = async {
        let mut connection = ControlConnectionState::new(true, true);
        let served = serve_async_runtime_control_connection(
            &mut server_stream,
            &handle,
            &mut connection,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid()).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(served, input.len());
        assert!(connection.initialized());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());

    assert_eq!(exit.commands_processed, 2);
}

/// Verifies async control connection loop preserves initialized caller.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_control_connection_loop_preserves_initialized_caller() {
    use crate::control::{decode_control_frame, encode_control_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let get_session =
        encode_control_body(r#"{"jsonrpc":"2.0","id":"get","method":"session/get","params":{}}"#);

    let client = async {
        client_stream.write_all(&initialize).await.unwrap();
        let mut first = vec![0; 4096];
        let read = client_stream.read(&mut first).await.unwrap();
        first.truncate(read);
        let (body, _) = decode_control_frame(&first, 4096).unwrap();
        assert!(body.contains(r#""control/initialize""#));

        client_stream.write_all(&get_session).await.unwrap();
        let mut second = vec![0; 4096];
        let read = client_stream.read(&mut second).await.unwrap();
        second.truncate(read);
        let (body, _) = decode_control_frame(&second, 4096).unwrap();
        assert!(body.contains(r#""session_id""#));
        assert!(body.contains(r#""windows""#));
    };
    let server = async {
        let mut connection = ControlConnectionState::new(true, true);
        let served = serve_async_runtime_control_connection_loop(
            &mut server_stream,
            &handle,
            &mut connection,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid()).unwrap(),
            |served, _state| served >= 2,
        )
        .await
        .unwrap();
        assert_eq!(served, 2);
        assert!(connection.initialized());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());

    assert!(exit.commands_processed >= 3);
}

/// Verifies async control listener serves stateful connection until client closes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_control_listener_serves_stateful_connection_until_client_closes() {
    use crate::control::{decode_control_frame, encode_control_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    let path = std::env::temp_dir().join(format!(
        "mez-async-control-listener-{}-{}.sock",
        std::process::id(),
        "stateful"
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let get_session =
        encode_control_body(r#"{"jsonrpc":"2.0","id":"get","method":"session/get","params":{}}"#);

    let client = async {
        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream.write_all(&initialize).await.unwrap();
        let mut first = vec![0; 4096];
        let read = stream.read(&mut first).await.unwrap();
        first.truncate(read);
        let (body, _) = decode_control_frame(&first, 4096).unwrap();
        assert!(body.contains(r#""control/initialize""#));

        stream.write_all(&get_session).await.unwrap();
        let mut second = vec![0; 4096];
        let read = stream.read(&mut second).await.unwrap();
        second.truncate(read);
        let (body, _) = decode_control_frame(&second, 4096).unwrap();
        assert!(body.contains(r#""session_id""#));
    };
    let server = async {
        let served = serve_async_runtime_control_listener(
            &listener,
            &handle,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid()).unwrap(),
            |served, _state| served >= 1,
        )
        .await
        .unwrap();
        assert_eq!(served, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), _exit) = tokio::join!(client, server, actor.run());
    let _ = std::fs::remove_file(&path);
}

/// Verifies the control listener can accept an observer while another control
/// connection remains open. Observer attachment uses a long-lived control
/// socket, so the accept loop must dispatch each connection independently or a
/// pending observer request can never be registered for the primary to review.
#[tokio::test(flavor = "current_thread")]
async fn async_control_listener_registers_observer_while_primary_connection_remains_open() {
    use crate::control::{decode_control_frame, encode_control_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::sync::oneshot;

    async fn read_control_body(stream: &mut UnixStream) -> String {
        let mut output = vec![0; 4096];
        let read = stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, _) = decode_control_frame(&output, 4096).unwrap();
        body
    }

    let path = std::env::temp_dir().join(format!(
        "mez-async-control-listener-{}-{}.sock",
        std::process::id(),
        "observer-concurrent"
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let primary_initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"primary-init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let observer_initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"observer-init","method":"control/initialize","params":{"client_name":"observer-cli","requested_version":1,"requested_role":"observer","client":{"name":"observer-cli","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let list_observers = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"list","method":"observer/list","params":{}}"#,
    );
    let (primary_ready_tx, primary_ready_rx) = oneshot::channel();
    let (observer_ready_tx, observer_ready_rx) = oneshot::channel();

    let primary_client = async {
        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream.write_all(&primary_initialize).await.unwrap();
        let body = read_control_body(&mut stream).await;
        assert!(body.contains(r#""granted_role":"primary""#), "{body}");
        primary_ready_tx.send(()).unwrap();
        observer_ready_rx.await.unwrap();

        stream.write_all(&list_observers).await.unwrap();
        let body = read_control_body(&mut stream).await;
        assert!(body.contains(r#""observers""#), "{body}");
        assert!(body.contains(r#""state":"pending""#), "{body}");
        assert!(body.contains("observer-cli"), "{body}");
    };
    let observer_client = async {
        primary_ready_rx.await.unwrap();
        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream.write_all(&observer_initialize).await.unwrap();
        let body = read_control_body(&mut stream).await;
        assert!(
            body.contains(r#""granted_role":"pending_observer""#),
            "{body}"
        );
        observer_ready_tx.send(()).unwrap();
    };
    let server = async {
        let served = serve_async_runtime_control_listener(
            &listener,
            &handle,
            AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid()).unwrap(),
            |served, _state| served >= 2,
        )
        .await
        .unwrap();
        assert_eq!(served, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), (), _exit) = tokio::join!(primary_client, observer_client, server, actor.run());
    let _ = std::fs::remove_file(&path);
}

/// Verifies async runtime daemon supervises named control and message listeners.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_runtime_daemon_supervises_named_control_and_message_listeners() {
    use crate::control::{decode_control_frame, encode_control_body};
    use crate::message::{decode_mmp_frame, encode_mmp_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    let control_path = std::env::temp_dir().join(format!(
        "mez-async-daemon-control-{}.sock",
        std::process::id()
    ));
    let message_path = std::env::temp_dir().join(format!(
        "mez-async-daemon-message-{}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&control_path);
    let _ = std::fs::remove_file(&message_path);
    let control_listener = UnixListener::bind(&control_path).unwrap();
    let message_listener = UnixListener::bind(&message_path).unwrap();

    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let initialize = encode_control_body(
        r#"{"jsonrpc":"2.0","id":"init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}},"authentication":{"mechanism":"peer_credentials"}}}"#,
    );
    let hello = encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#);

    let control_client = async {
        let mut stream = UnixStream::connect(&control_path).await.unwrap();
        stream.write_all(&initialize).await.unwrap();
        let mut output = vec![0; 4096];
        let read = stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, _) = decode_control_frame(&output, 4096).unwrap();
        assert!(body.contains(r#""control/initialize""#));
    };
    let message_client = async {
        let mut stream = UnixStream::connect(&message_path).await.unwrap();
        stream.write_all(&hello).await.unwrap();
        let mut output = vec![0; 4096];
        let read = stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, _) = decode_mmp_frame(&output, 4096).unwrap();
        assert!(body.contains(r#""type":"welcome""#));
    };
    let daemon_handle = handle.clone();
    let daemon = async move {
        let report = run_async_runtime_daemon(
            daemon_handle.clone(),
            AsyncRuntimeDaemonListeners {
                control: Some(control_listener),
                message: Some(message_listener),
                event: None,
            },
            AsyncRuntimeDaemonConfig {
                control: AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid())
                    .unwrap(),
                message_max_content_length: 4096,
                max_control_connections: 1,
                max_message_connections: 1,
                ..AsyncRuntimeDaemonConfig::default()
            },
            std::future::pending(),
        )
        .await
        .unwrap();
        assert_eq!(
            daemon_handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
        report
    };

    let ((), (), report, _exit) = tokio::join!(control_client, message_client, daemon, actor.run());
    let mut services = report.services;
    services.sort_by(|left, right| left.name.cmp(&right.name));

    assert!(!report.shutdown_requested);
    assert_eq!(services.len(), 7);
    assert_eq!(services[0].name, "agent-provider");
    assert_eq!(services[0].exit.work_units, 0);
    assert_eq!(services[1].name, "control");
    assert_eq!(services[1].exit.work_units, 1);
    assert_eq!(services[2].name, "hook");
    assert_eq!(services[2].exit.work_units, 0);
    assert_eq!(services[3].name, "message");
    assert_eq!(services[3].exit.work_units, 1);
    assert_eq!(services[4].name, "pane-process-supervisor");
    assert_eq!(services[4].exit.work_units, 0);
    assert_eq!(services[5].name, "persistence");
    assert_eq!(services[5].exit.work_units, 0);
    assert_eq!(services[6].name, "timer");
    assert_eq!(services[6].exit.work_units, 0);

    let _ = std::fs::remove_file(&control_path);
    let _ = std::fs::remove_file(&message_path);
}

/// Verifies that supervised async pane workers feed PTY output into runtime
/// terminal screens even when the daemon has no compatibility tick service.
/// Attached-client rendering depends on pane-driver events in the Tokio daemon
/// path.
#[tokio::test(flavor = "current_thread")]
async fn async_runtime_daemon_pane_worker_feeds_pty_output_into_rendered_view() {
    use tokio::net::UnixListener;
    use tokio::time::timeout;

    let path =
        std::env::temp_dir().join(format!("mez-async-daemon-tick-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let mut service = test_service();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .start_initial_pane_process(Some("sh -c 'printf async-daemon-tick; sleep 1'"))
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let services = build_async_runtime_daemon_services(
        handle.clone(),
        AsyncRuntimeDaemonListeners::control_only(listener),
        AsyncRuntimeDaemonConfig {
            control: AsyncRuntimeControlConnectionConfig::new(4096, current_effective_uid())
                .unwrap(),
            ..AsyncRuntimeDaemonConfig::default()
        },
    )
    .unwrap();
    let poll_handle = handle.clone();
    let cancellation = async move {
        timeout(Duration::from_secs(1), async {
            loop {
                let view = poll_handle
                    .render_client_view(
                        ClientViewRole::Primary,
                        Size::new(80, 24).unwrap(),
                        TerminalClientLoopConfig::default(),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                if view.lines.join("\n").contains("async-daemon-tick") {
                    break;
                }
                poll_handle.wait_for_event_delivery().await;
            }
        })
        .await
        .unwrap();
    };
    let shutdown_handle = handle.clone();
    let daemon = async move {
        let report = supervise_async_runtime_services(services, cancellation)
            .await
            .unwrap();
        let _ = shutdown_handle.shutdown().await.unwrap();
        report
    };

    let (report, mut exit) = tokio::join!(daemon, actor.run());

    assert!(report.shutdown_requested);
    assert!(
        report
            .services
            .iter()
            .any(|service| service.name == "pane-process-supervisor")
    );
    assert!(!report.services.iter().any(|service| service.name == "tick"));
    exit.service.pane_processes_mut().terminate_all().unwrap();
    let _ = std::fs::remove_file(&path);
}

/// Verifies async message connection dispatches hello.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_message_connection_dispatches_hello() {
    use crate::message::{decode_mmp_frame, encode_mmp_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let input = encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#);

    let client = async {
        client_stream.write_all(&input).await.unwrap();
        let mut output = vec![0; 4096];
        let read = client_stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, consumed) = decode_mmp_frame(&output, 4096).unwrap();
        assert_eq!(consumed, output.len());
        assert!(body.contains(r#""type":"welcome""#));
    };
    let server = async {
        let mut connection = MessageConnection::default();
        let served = serve_async_runtime_message_connection(
            &mut server_stream,
            &handle,
            &mut connection,
            4096,
            10,
            100,
        )
        .await
        .unwrap();
        assert_eq!(served, input.len());
        assert!(connection.agent_id.is_some());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());

    assert_eq!(exit.commands_processed, 3);
}

/// Verifies async message connection loop preserves agent connection.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_message_connection_loop_preserves_agent_connection() {
    use crate::message::{decode_mmp_frame, encode_mmp_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let hello = encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#);
    let discover = encode_mmp_body(r#"{"protocol":"mmp/1","type":"discover"}"#);

    let client = async {
        client_stream.write_all(&hello).await.unwrap();
        let mut first = vec![0; 4096];
        let read = client_stream.read(&mut first).await.unwrap();
        first.truncate(read);
        let (body, _) = decode_mmp_frame(&first, 4096).unwrap();
        assert!(body.contains(r#""type":"welcome""#));

        client_stream.write_all(&discover).await.unwrap();
        let mut second = vec![0; 4096];
        let read = client_stream.read(&mut second).await.unwrap();
        second.truncate(read);
        let (body, _) = decode_mmp_frame(&second, 4096).unwrap();
        assert!(body.contains(r#""type":"discover_result""#));
        assert!(body.contains(r#""role":"default""#));
    };
    let server = async {
        let mut connection = MessageConnection::default();
        let served = serve_async_runtime_message_connection_loop(
            &mut server_stream,
            &handle,
            &mut connection,
            AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
            |served| 10 + served,
            |served, _state| served >= 2,
        )
        .await
        .unwrap();
        assert_eq!(served, 2);
        assert!(connection.agent_id.is_some());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());

    assert!(exit.commands_processed >= 4);
}

/// Verifies async message listener serves stateful connection until client closes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_message_listener_serves_stateful_connection_until_client_closes() {
    use crate::message::{decode_mmp_frame, encode_mmp_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    let path = std::env::temp_dir().join(format!(
        "mez-async-message-listener-{}-{}.sock",
        std::process::id(),
        "stateful"
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let hello = encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#);
    let discover = encode_mmp_body(r#"{"protocol":"mmp/1","type":"discover"}"#);

    let client = async {
        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream.write_all(&hello).await.unwrap();
        let mut first = vec![0; 4096];
        let read = stream.read(&mut first).await.unwrap();
        first.truncate(read);
        let (body, _) = decode_mmp_frame(&first, 4096).unwrap();
        assert!(body.contains(r#""type":"welcome""#));

        stream.write_all(&discover).await.unwrap();
        let mut second = vec![0; 4096];
        let read = stream.read(&mut second).await.unwrap();
        second.truncate(read);
        let (body, _) = decode_mmp_frame(&second, 4096).unwrap();
        assert!(body.contains(r#""type":"discover_result""#));
    };
    let server = async {
        let served = serve_async_runtime_message_listener(
            &listener,
            &handle,
            AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
            10,
            |served, _| served >= 1,
        )
        .await
        .unwrap();
        assert_eq!(served, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), _exit) = tokio::join!(client, server, actor.run());
    let _ = std::fs::remove_file(&path);
}

/// Verifies async message listener can schedule multiple connections.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_message_listener_can_schedule_multiple_connections() {
    use crate::message::{decode_mmp_frame, encode_mmp_body};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{UnixListener, UnixStream};

    let path = std::env::temp_dir().join(format!(
        "mez-async-message-listener-{}-{}.sock",
        std::process::id(),
        "concurrent"
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(test_service(), AsyncRuntimeActorConfig::default()).unwrap();
    let hello = encode_mmp_body(r#"{"protocol":"mmp/1","type":"hello","role":"default"}"#);

    let client_one_path = path.clone();
    let client_one_hello = hello.clone();
    let client_one = async move {
        let mut stream = UnixStream::connect(&client_one_path).await.unwrap();
        stream.write_all(&client_one_hello).await.unwrap();
        let mut first = vec![0; 4096];
        let read = stream.read(&mut first).await.unwrap();
        first.truncate(read);
        let (body, _) = decode_mmp_frame(&first, 4096).unwrap();
        assert!(body.contains(r#""type":"welcome""#));
    };
    let client_two_path = path.clone();
    let client_two = async move {
        let mut stream = UnixStream::connect(&client_two_path).await.unwrap();
        stream.write_all(&hello).await.unwrap();
        let mut first = vec![0; 4096];
        let read = stream.read(&mut first).await.unwrap();
        first.truncate(read);
        let (body, _) = decode_mmp_frame(&first, 4096).unwrap();
        assert!(body.contains(r#""type":"welcome""#));
    };
    let server = async {
        let served = serve_async_runtime_message_listener_concurrent(
            &listener,
            &handle,
            AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
            10,
            2,
            |served, _| served >= 2,
        )
        .await
        .unwrap();
        assert_eq!(served, 2);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), (), exit) = tokio::join!(client_one, client_two, server, actor.run());

    assert!(exit.commands_processed >= 5);
    let _ = std::fs::remove_file(&path);
}

/// Verifies async message connection flushes fanout after response write.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_message_connection_flushes_fanout_after_response_write() {
    use crate::message::{Envelope, Recipient, decode_mmp_frame, encode_mmp_body};
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;
    use tokio::time::timeout;

    let mut service = test_service();
    let sender = service
        .message_service_mut()
        .register_agent(None, None, "sender", Vec::new());
    let target = service
        .message_service_mut()
        .register_agent(None, None, "target", Vec::new());
    service
        .message_service_mut()
        .subscribe(&target.agent_id)
        .unwrap();
    let message = Envelope {
        protocol: "mmp/1",
        id: "m1".to_string(),
        message_type: "send".to_string(),
        time: "message:test".to_string(),
        sender: sender.clone(),
        recipient: Recipient::Agent(target.agent_id.clone()),
        correlation_id: None,
        ttl_ms: None,
        content_type: "text/plain".to_string(),
        payload: "hello".to_string(),
        extension_fields: Vec::new(),
    };
    service
        .message_service_mut()
        .accept_at(&sender.agent_id, message, 10)
        .unwrap();
    let mut connection = MessageConnection {
        agent_id: Some(target.agent_id.clone()),
        delivery_cursor: service
            .message_service()
            .subscription(&target.agent_id)
            .cloned(),
    };
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let input = encode_mmp_body(r#"{"protocol":"mmp/1","type":"heartbeat","id":"hb1"}"#);

    let client = async {
        client_stream.write_all(&input).await.unwrap();
        let mut output = Vec::new();
        timeout(Duration::from_secs(1), async {
            loop {
                let mut chunk = [0u8; 1024];
                let read = client_stream.read(&mut chunk).await.unwrap();
                assert!(read > 0, "message stream closed before fanout delivery");
                output.extend_from_slice(&chunk[..read]);
                let Ok((_, first_len)) = decode_mmp_frame(&output, 4096) else {
                    continue;
                };
                if decode_mmp_frame(&output[first_len..], 4096).is_ok() {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let (ack, first_len) = decode_mmp_frame(&output, 4096).unwrap();
        let (deliver, _) = decode_mmp_frame(&output[first_len..], 4096).unwrap();
        assert!(ack.contains(r#""type":"ack""#));
        assert!(deliver.contains(r#""type":"deliver""#));
        assert!(deliver.contains(r#""payload":"hello""#));
    };
    let server = async {
        let served = serve_async_runtime_message_connection(
            &mut server_stream,
            &handle,
            &mut connection,
            4096,
            11,
            100,
        )
        .await
        .unwrap();
        assert_eq!(served, input.len());
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());
    let remaining = exit
        .service
        .message_service()
        .receive_subscribed(&target.agent_id, 12, usize::MAX)
        .unwrap();

    assert!(remaining.messages.is_empty());
}

/// Verifies async message connection notification flushes later fanout.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_message_connection_notification_flushes_later_fanout() {
    use crate::message::{decode_mmp_frame, encode_mmp_body};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;
    use tokio::time::{advance, timeout};

    let mut service = test_service();
    let sender = service
        .message_service_mut()
        .register_agent(None, None, "sender", Vec::new());
    let target = service
        .message_service_mut()
        .register_agent(None, None, "target", Vec::new());
    let target_cursor = service
        .message_service_mut()
        .subscribe(&target.agent_id)
        .unwrap()
        .clone();
    let mut target_connection = MessageConnection {
        agent_id: Some(target.agent_id.clone()),
        delivery_cursor: Some(target_cursor),
    };
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let producer_handle = handle.clone();
    let target_id = target.agent_id.clone();
    let sender_id = sender.agent_id.clone();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let delivered = Arc::new(AtomicBool::new(false));
    let client_delivered = delivered.clone();
    let server_delivered = delivered.clone();

    let client = async {
        let mut output = Vec::new();
        timeout(Duration::from_secs(1), async {
            loop {
                let mut chunk = [0u8; 1024];
                let read = client_stream.read(&mut chunk).await.unwrap();
                assert!(read > 0, "message stream closed before fanout delivery");
                output.extend_from_slice(&chunk[..read]);
                if decode_mmp_frame(&output, 4096).is_ok() {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let (deliver, consumed) = decode_mmp_frame(&output, 4096).unwrap();
        assert_eq!(consumed, output.len());
        assert!(deliver.contains(r#""type":"deliver""#));
        assert!(deliver.contains(r#""payload":"idle hello""#));
        client_delivered.store(true, Ordering::SeqCst);
    };
    let producer = async move {
        advance(Duration::from_millis(10)).await;
        let mut sender_connection = MessageConnection {
            agent_id: Some(sender_id),
            delivery_cursor: None,
        };
        let recipient_json = format!(r#"{{"agent_id":"{}"}}"#, target_id);
        let send = format!(
            r#"{{"protocol":"mmp/1","type":"send","id":"m-idle","time":"message:client-idle","sender":{{"agent_id":"{}","role":"sender"}},"recipient":{},"correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"idle hello"}}"#,
            sender_connection.agent_id.as_ref().unwrap(),
            recipient_json
        );
        let result = producer_handle
            .handle_message_input(encode_mmp_body(&send), 4096, sender_connection.clone(), 20)
            .await
            .unwrap();
        sender_connection = result.connection;
        assert!(sender_connection.agent_id.is_some());
        let (ack, _) = decode_mmp_frame(&result.output, 4096).unwrap();
        assert!(ack.contains(r#""status":"accepted""#));
    };
    let server = async {
        let served = serve_async_runtime_message_connection_loop(
            &mut server_stream,
            &handle,
            &mut target_connection,
            AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
            |served| 20 + served,
            |_served, _state| server_delivered.load(Ordering::SeqCst),
        )
        .await
        .unwrap();
        assert_eq!(served, 0);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), (), exit) = tokio::join!(client, producer, server, actor.run());
    let remaining = exit
        .service
        .message_service()
        .receive_subscribed(&target.agent_id, 21, usize::MAX)
        .unwrap();

    assert!(remaining.messages.is_empty());
}

/// Verifies that an idle subscribed message connection wakes from the actor
/// lifecycle channel instead of relying on its long fallback poll interval.
/// This protects shutdown responsiveness for agent message sockets when no
/// message fanout is pending and the peer keeps the Unix stream open.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_message_connection_exits_on_lifecycle_change_without_idle_poll() {
    use tokio::net::UnixStream;
    use tokio::sync::oneshot;

    let mut service = test_service();
    let target = service
        .message_service_mut()
        .register_agent(None, None, "target", Vec::new());
    let target_cursor = service
        .message_service_mut()
        .subscribe(&target.agent_id)
        .unwrap()
        .clone();
    let mut target_connection = MessageConnection {
        agent_id: Some(target.agent_id.clone()),
        delivery_cursor: Some(target_cursor),
    };
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let trigger_handle = handle.clone();
    let (client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let (release_client, hold_client) = oneshot::channel::<()>();

    let client_guard = async move {
        let _client_stream = client_stream;
        let _ = hold_client.await;
    };
    let trigger_shutdown = async move {
        tokio::task::yield_now().await;
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Shutdown(ShutdownEvent {
            reason: "test lifecycle wake".to_string(),
            force: true,
            failed: false,
        }));
        let report = trigger_handle.submit_runtime_events(batch).await.unwrap();
        assert_eq!(report.applied, 1);
        let metrics = trigger_handle.metrics().await.unwrap();
        assert_eq!(metrics.lifecycle_state_notifications, 1);
        assert_eq!(
            trigger_handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Killed
        );
    };
    let server = async {
        let served = serve_async_runtime_message_connection_loop(
            &mut server_stream,
            &handle,
            &mut target_connection,
            AsyncRuntimeMessageConnectionConfig::new(4096, 100).unwrap(),
            |served| 20 + served,
            |_served, state| {
                matches!(
                    state,
                    RuntimeLifecycleState::Stopping
                        | RuntimeLifecycleState::Killed
                        | RuntimeLifecycleState::Failed
                )
            },
        )
        .await
        .unwrap();
        assert_eq!(served, 0);
        let _ = release_client.send(());
    };

    let ((), (), (), exit) = tokio::join!(client_guard, trigger_shutdown, server, actor.run());
    assert_eq!(
        exit.service.lifecycle_state(),
        RuntimeLifecycleState::Killed
    );
}

/// Verifies async event flush writes notifications and advances cursor.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_event_flush_writes_notifications_and_advances_cursor() {
    use crate::control::decode_control_frame;
    use crate::event::EventAudience;
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;

    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut connections = RuntimeEventConnectionTable::default();
    connections
        .attach("events-primary", EventAudience::Primary, true, 0)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();

    let client = async {
        let mut output = vec![0; 4096];
        let read = client_stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, consumed) = decode_control_frame(&output, 4096).unwrap();
        assert_eq!(consumed, output.len());
        assert!(body.contains(r#""method":"event/client_attached""#));
        assert!(body.contains(r#""event_id":1"#));
    };
    let server = async {
        let delivered = flush_async_runtime_event_wakeups_to_stream(
            &mut server_stream,
            &handle,
            &mut connections,
            10,
        )
        .await
        .unwrap();
        assert_eq!(delivered, 1);
        assert_eq!(
            flush_async_runtime_event_wakeups_to_stream(
                &mut server_stream,
                &handle,
                &mut connections,
                10,
            )
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());

    assert_eq!(exit.commands_processed, 3);
}

/// Verifies async event connection serves until shutdown predicate.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_event_connection_serves_until_shutdown_predicate() {
    use crate::control::decode_control_frame;
    use crate::event::EventAudience;
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;

    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let mut connections = RuntimeEventConnectionTable::default();
    connections
        .attach("events-primary", EventAudience::Primary, true, 0)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();

    let client = async {
        let mut output = vec![0; 4096];
        let read = client_stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, _) = decode_control_frame(&output, 4096).unwrap();
        assert!(body.contains(r#""method":"event/client_attached""#));
    };
    let server = async {
        let served = serve_async_runtime_event_connection(
            &mut server_stream,
            &handle,
            &mut connections,
            AsyncRuntimeEventConnectionConfig::new(10, current_effective_uid()).unwrap(),
            |delivered, _state| delivered >= 1,
        )
        .await
        .unwrap();
        assert_eq!(served, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), exit) = tokio::join!(client, server, actor.run());

    assert!(exit.commands_processed >= 2);
}

/// Verifies async event connection notification flushes later events.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn async_event_connection_notification_flushes_later_events() {
    use crate::control::{ControlConnectionState, decode_control_frame, encode_control_body};
    use crate::event::EventAudience;
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;
    use tokio::time::{advance, timeout};

    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let last_event_id = service.event_log().unwrap().latest_event_id();
    let mut connections = RuntimeEventConnectionTable::default();
    connections
        .attach(
            "events-primary",
            EventAudience::Primary,
            true,
            last_event_id,
        )
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let producer_handle = handle.clone();
    let (mut client_stream, mut server_stream) = UnixStream::pair().unwrap();

    let client = async {
        let mut output = Vec::new();
        timeout(Duration::from_secs(1), async {
            loop {
                let mut chunk = [0u8; 1024];
                let read = client_stream.read(&mut chunk).await.unwrap();
                assert!(read > 0, "event stream closed before later event delivery");
                output.extend_from_slice(&chunk[..read]);
                if decode_control_frame(&output, 4096).is_ok() {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let mut decoded = Vec::new();
        let mut offset = 0usize;
        while offset < output.len() {
            match decode_control_frame(&output[offset..], 4096) {
                Ok((body, consumed)) => {
                    decoded.push(body);
                    offset = offset.saturating_add(consumed);
                }
                Err(error) if !decoded.is_empty() => {
                    assert!(
                        matches!(error.kind(), crate::error::MezErrorKind::InvalidArgs),
                        "unexpected trailing event stream decode error: {error}"
                    );
                    break;
                }
                Err(error) => panic!("event stream did not contain a complete frame: {error}"),
            }
        }
        assert!(
            decoded
                .iter()
                .any(|body| body.contains(r#""method":"event/"#)),
            "{decoded:?}"
        );
        assert!(
            decoded.iter().all(|body| !body.contains(r#""event_id":1"#)),
            "{decoded:?}"
        );
    };
    let producer = async move {
        advance(Duration::from_millis(10)).await;
        let input = encode_control_body(
            r#"{"jsonrpc":"2.0","id":1,"method":"window/create","params":{"name":"events","shell_command":"true","idempotency_key":"event-window"}}"#,
        );
        let control_connection = ControlConnectionState::trusted_existing_client(primary);
        let result = producer_handle
            .handle_control_input_for_connection(input, 4096, control_connection)
            .await
            .unwrap();
        let (body, _) = decode_control_frame(&result.output, 4096).unwrap();
        assert!(body.contains(r#""window":{"#), "{body}");
    };
    let server = async {
        let delivered = serve_async_runtime_event_connection(
            &mut server_stream,
            &handle,
            &mut connections,
            AsyncRuntimeEventConnectionConfig::new(10, current_effective_uid()).unwrap(),
            |delivered, _state| delivered >= 1,
        )
        .await
        .unwrap();
        assert!(delivered >= 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), (), exit) = tokio::join!(client, producer, server, actor.run());

    assert!(exit.commands_processed >= 4);
}

/// Verifies async event connection rejects wrong unix peer owner.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_event_connection_rejects_wrong_unix_peer_owner() {
    use crate::event::EventAudience;
    use tokio::net::UnixStream;

    let (handle, _actor) = AsyncRuntimeSessionActor::new(
        test_service_with_event_log(),
        AsyncRuntimeActorConfig::default(),
    )
    .unwrap();
    let (_client_stream, mut server_stream) = UnixStream::pair().unwrap();
    let mut connections = RuntimeEventConnectionTable::default();
    connections
        .attach("events-primary", EventAudience::Primary, true, 0)
        .unwrap();

    let error = serve_async_runtime_event_connection(
        &mut server_stream,
        &handle,
        &mut connections,
        AsyncRuntimeEventConnectionConfig::new(10, current_effective_uid().saturating_add(1))
            .unwrap(),
        |_delivered, _state| true,
    )
    .await
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
}

/// Verifies async event listener accepts and streams visible events.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[tokio::test(flavor = "current_thread")]
async fn async_event_listener_accepts_and_streams_visible_events() {
    use crate::control::decode_control_frame;
    use crate::event::EventAudience;
    use tokio::io::AsyncReadExt;
    use tokio::net::{UnixListener, UnixStream};

    let path = std::env::temp_dir().join(format!(
        "mez-async-event-listener-{}-{}.sock",
        std::process::id(),
        "primary"
    ));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).unwrap();
    let mut service = test_service_with_event_log();
    service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();

    let client = async {
        let mut stream = UnixStream::connect(&path).await.unwrap();
        let mut output = vec![0; 4096];
        let read = stream.read(&mut output).await.unwrap();
        output.truncate(read);
        let (body, _) = decode_control_frame(&output, 4096).unwrap();
        assert!(body.contains(r#""method":"event/client_attached""#));
        assert!(body.contains(r#""event_id":1"#));
    };
    let server = async {
        let served = serve_async_runtime_event_listener(
            &listener,
            &handle,
            AsyncRuntimeEventConnectionConfig::new(10, current_effective_uid()).unwrap(),
            |index| Ok((format!("events-{index}"), EventAudience::Primary, 0)),
            |accepted, delivered, _state| accepted >= 1 || delivered >= 1,
        )
        .await
        .unwrap();
        assert_eq!(served, 1);
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };

    let ((), (), _exit) = tokio::join!(client, server, actor.run());
    let _ = std::fs::remove_file(&path);
}
