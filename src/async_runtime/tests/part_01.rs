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
    AsyncRuntimeServiceReport, AsyncRuntimeServiceSupervisor,
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
use crate::test_support::async_runtime::AsyncRuntimeActorFixture;
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
        AsyncRuntimeActorFixture::from_service(test_service()).build().unwrap();
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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();
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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .config(AsyncRuntimeActorConfig {
            side_effect_buffer: 2,
            ..AsyncRuntimeActorConfig::default()
        })
        .build()
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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .config(AsyncRuntimeActorConfig {
            side_effect_buffer: 2,
            ..AsyncRuntimeActorConfig::default()
        })
        .build()
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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .config(AsyncRuntimeActorConfig {
            side_effect_buffer: 2,
            ..AsyncRuntimeActorConfig::default()
        })
        .build()
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
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .config(AsyncRuntimeActorConfig {
            side_effect_buffer: 2,
            ..AsyncRuntimeActorConfig::default()
        })
        .build()
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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();
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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();
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
    let (handle, actor) =
        AsyncRuntimeActorFixture::from_service(test_service_with_event_log())
            .build()
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
        AsyncRuntimeActorFixture::from_service(test_service()).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        assert_eq!(queued.runtime_event_batch_sizes.observations, 1);
        assert_eq!(queued.runtime_event_batch_sizes.min, Some(1));
        assert_eq!(queued.runtime_event_batch_sizes.max, Some(1));
        assert_eq!(queued.runtime_side_effects_queued, 1);
        assert_eq!(queued.runtime_side_effects_drained, 0);
        assert_eq!(queued.runtime_side_effect_enqueue_sizes.observations, 1);
        assert_eq!(queued.runtime_side_effect_enqueue_sizes.max, Some(1));
        assert_eq!(queued.pane_output_chunks, 1);
        assert_eq!(
            queued.pane_output_bytes,
            u64::try_from(b"metrics-output\n".len()).unwrap()
        );
        assert_eq!(queued.pane_output_chunk_bytes.observations, 1);
        assert_eq!(
            queued.pane_output_chunk_bytes.max,
            Some(u64::try_from(b"metrics-output\n".len()).unwrap())
        );
        assert_eq!(queued.side_effect_queue_depth, 1);
        assert_eq!(queued.side_effect_queue_high_water, 1);
        assert_eq!(queued.side_effect_queue_depth_samples.max, Some(1));
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
        assert_eq!(drained.runtime_side_effect_drain_sizes.observations, 1);
        assert_eq!(drained.runtime_side_effect_drain_sizes.max, Some(1));
        assert_eq!(drained.side_effect_queue_depth, 0);
        assert_eq!(drained.side_effect_queue_high_water, 1);
        assert!(drained.side_effect_queue_depth_samples.observations >= 2);
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
    assert_eq!(exit.metrics.runtime_event_batch_sizes.max, Some(1));
    assert_eq!(exit.metrics.runtime_side_effect_enqueue_sizes.max, Some(1));
    assert_eq!(exit.metrics.runtime_side_effect_drain_sizes.max, Some(1));
    assert_eq!(
        exit.metrics.pane_output_chunk_bytes.max,
        Some(u64::try_from(b"metrics-output\n".len()).unwrap())
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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();
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
/// Verifies that the async runtime terminal command path exposes the current
/// actor counters and histograms through `show-metrics` for pager rendering.
#[tokio::test(flavor = "current_thread")]
async fn async_terminal_show_metrics_command_renders_actor_metrics() {
    let mut service = test_service_with_event_log();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    let (handle, actor) =
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();
    let client = async {
        let mut batch = RuntimeEventBatch::new();
        batch.push(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: "%1".to_string(),
            bytes: b"show-metrics\n".to_vec(),
        }));
        handle.submit_runtime_events(batch).await.unwrap();
        let output = handle
            .execute_terminal_command(primary, "show-metrics".to_string())
            .await
            .unwrap();
        assert!(output.contains(r#""command":"show-metrics""#), "{output}");
        assert!(
            output.contains("metrics source=async-runtime status=available"),
            "{output}"
        );
        assert!(
            output.contains("metrics source=runtime-service status=available"),
            "{output}"
        );
        assert!(output.contains("[runtime counts]"), "{output}");
        assert!(output.contains("provider_requests_started ="), "{output}");
        assert!(output.contains("[runtime histograms]"), "{output}");
        assert!(
            output.contains("provider_prompt_cacheable_prefix_bytes"),
            "{output}"
        );
        assert!(output.contains("[async runtime counts]"), "{output}");
        assert!(output.contains("commands_processed ="), "{output}");
        assert!(output.contains("[async runtime histograms]"), "{output}");
        assert!(output.contains("runtime_event_batch_sizes"), "{output}");
        assert!(output.contains("pane_output_chunk_bytes"), "{output}");
        assert!(
            output.contains("side_effect_queue_depth_samples"),
            "{output}"
        );
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();
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
        AsyncRuntimeActorFixture::from_service(test_service()).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(test_service()).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(test_service()).build().unwrap();
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
        AsyncRuntimeActorFixture::from_service(test_service()).build().unwrap();
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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();
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
        AsyncRuntimeActorFixture::from_service(test_service()).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
        AsyncRuntimeActorFixture::from_service(service).build().unwrap();

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
