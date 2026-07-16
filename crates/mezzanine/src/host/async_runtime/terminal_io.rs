//! Async attached-terminal I/O traits and deterministic fakes.
//!
//! This module is the terminal I/O boundary for the Tokio terminal refactor.
//! Live attached-terminal paths should depend on `AsyncAttachedTerminalIo` so
//! terminal readiness, input, output, resize, and presentation cleanup can be
//! driven by Tokio tasks without blocking runtime worker threads. The
//! tests can still wrap deterministic synchronous fakes without exposing that
//! bridge to production callers.

use super::{
    AttachedTerminalFdReadiness, AttachedTerminalFdRole, AttachedTerminalOutputModes, MezError,
    Result, Size,
};
#[cfg(test)]
use crate::host::terminal::AttachedTerminalClientLoopIo;
use crate::host::terminal::{
    TerminalFdInterest, TerminalRawModeGuard, attached_terminal_output_disconnected,
    read_attached_terminal_size,
};
use mez_mux::attached_client::{
    AttachedTerminalOutputFrameState, attached_terminal_enter_presentation_frame,
    attached_terminal_restore_presentation_frame,
    encode_attached_terminal_output_update_frame_with_styles,
};
use mez_terminal::TerminalStyleSpan;
#[cfg(test)]
use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::pin::Pin;
use std::time::{Duration, Instant};
use tokio::io::unix::AsyncFd;

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::{Errno, read as rustix_read, write as rustix_write};

/// Default maximum attached-terminal output bytes written in one flush pass.
pub const DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES: usize = 64 * 1024;

/// Minimum attached-terminal output write budget after backpressure is observed.
const MIN_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES: usize = 8 * 1024;
/// Completed flushes required before a reduced write budget grows again.
const ATTACHED_TERMINAL_OUTPUT_WRITE_RECOVERY_FLUSHES: u8 = 3;
/// Flush duration that indicates the current output budget is too large.
const ATTACHED_TERMINAL_OUTPUT_SLOW_FLUSH_THRESHOLD: Duration = Duration::from_millis(25);

/// Boxed future returned by async terminal I/O trait methods.
pub type AsyncTerminalIoFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Result of one bounded attached-terminal output write attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncTerminalOutputWriteReport {
    /// Bytes written during this attempt.
    pub bytes_written: usize,
    /// Whether the output frame was fully written.
    pub completed: bool,
    /// Bytes still retained for later output flush attempts.
    pub pending_bytes: usize,
}

impl AsyncTerminalOutputWriteReport {
    /// Returns a completed write report.
    pub const fn completed(bytes_written: usize) -> Self {
        Self {
            bytes_written,
            completed: true,
            pending_bytes: 0,
        }
    }

    /// Returns whether this write left bytes pending.
    pub const fn is_partial(self) -> bool {
        !self.completed || self.pending_bytes > 0
    }
}

/// Async terminal I/O boundary used by the Tokio-native attached client path.
pub trait AsyncAttachedTerminalIo: Send {
    /// Waits until one or more terminal file descriptors have observable
    /// readiness.
    fn poll_readiness<'a>(
        &'a mut self,
    ) -> AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>>;

    /// Waits for terminal readiness that can originate new user/control input.
    ///
    /// Long-lived attached clients use this input-focused wait between render
    /// invalidations so an always-writable stdout fd does not become an idle
    /// redraw clock. Implementations that cannot distinguish input readiness
    /// may conservatively delegate to `poll_readiness`.
    fn poll_input_readiness<'a>(
        &'a mut self,
    ) -> AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        self.poll_readiness()
    }

    /// Reads at most `max_bytes` bytes of client input.
    fn read_input<'a>(&'a mut self, max_bytes: usize) -> AsyncTerminalIoFuture<'a, Vec<u8>>;

    /// Writes one styled terminal frame with presentation modes.
    fn write_styled_output_with_modes<'a>(
        &'a mut self,
        lines: &'a [String],
        line_style_spans: &'a [Vec<TerminalStyleSpan>],
        modes: AttachedTerminalOutputModes,
    ) -> AsyncTerminalIoFuture<'a, usize>;

    /// Returns the number of already-started output bytes retained for later
    /// flush attempts.
    fn pending_output_bytes(&self) -> usize {
        0
    }

    /// Flushes retained output bytes without accepting a new frame.
    fn flush_pending_output<'a>(
        &'a mut self,
        _max_bytes: usize,
    ) -> AsyncTerminalIoFuture<'a, AsyncTerminalOutputWriteReport> {
        Box::pin(async { Ok(AsyncTerminalOutputWriteReport::completed(0)) })
    }

    /// Writes at most `max_bytes` of one styled terminal frame.
    ///
    /// Implementations that cannot retain partial output may use the default
    /// whole-frame behavior. Production foreground terminal endpoints override
    /// this so slow clients cannot monopolize an attached-terminal loop pass.
    fn write_styled_output_with_modes_bounded<'a>(
        &'a mut self,
        lines: &'a [String],
        line_style_spans: &'a [Vec<TerminalStyleSpan>],
        modes: AttachedTerminalOutputModes,
        _max_bytes: usize,
    ) -> AsyncTerminalIoFuture<'a, AsyncTerminalOutputWriteReport> {
        Box::pin(async move {
            let bytes_written = self
                .write_styled_output_with_modes(lines, line_style_spans, modes)
                .await?;
            Ok(AsyncTerminalOutputWriteReport::completed(bytes_written))
        })
    }

    /// Reads the current terminal size when available.
    fn terminal_size<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, Option<Size>> {
        Box::pin(async { Ok(None) })
    }

    /// Invalidates retained differential output state.
    fn invalidate_output_frame<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    /// Enters Mezzanine's presentation mode for this terminal.
    fn enter_presentation<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    /// Restores host terminal presentation state.
    fn restore_presentation<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Test adapter that exposes deterministic synchronous attached-terminal fakes
/// through the async trait.
#[cfg(test)]
pub struct SyncAttachedTerminalIoAdapter<I> {
    /// Stores the inner value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    inner: I,
}

#[cfg(test)]
impl<I> SyncAttachedTerminalIoAdapter<I> {
    /// Wraps an existing synchronous terminal I/O implementation.
    pub fn new(inner: I) -> Self {
        Self { inner }
    }

    /// Returns the wrapped synchronous implementation.
    pub fn into_inner(self) -> I {
        self.inner
    }
}

#[cfg(test)]
impl<I> AsyncAttachedTerminalIo for SyncAttachedTerminalIoAdapter<I>
where
    I: AttachedTerminalClientLoopIo + Send,
{
    /// Runs the poll readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_readiness<'a>(
        &'a mut self,
    ) -> AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        Box::pin(async move { self.inner.poll_readiness() })
    }

    /// Runs the poll input readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_input_readiness<'a>(
        &'a mut self,
    ) -> AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        Box::pin(async move {
            Ok(self
                .inner
                .poll_readiness()?
                .into_iter()
                .filter(|ready| {
                    ready.role != AttachedTerminalFdRole::Output || ready.hangup || ready.error
                })
                .collect())
        })
    }

    /// Runs the read input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_input<'a>(&'a mut self, max_bytes: usize) -> AsyncTerminalIoFuture<'a, Vec<u8>> {
        Box::pin(async move { self.inner.read_input(max_bytes) })
    }

    /// Runs the write styled output with modes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_styled_output_with_modes<'a>(
        &'a mut self,
        lines: &'a [String],
        line_style_spans: &'a [Vec<TerminalStyleSpan>],
        modes: AttachedTerminalOutputModes,
    ) -> AsyncTerminalIoFuture<'a, usize> {
        Box::pin(async move {
            self.inner
                .write_styled_output_with_modes(lines, line_style_spans, modes)
        })
    }

    /// Runs the terminal size operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn terminal_size<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, Option<Size>> {
        Box::pin(async move { self.inner.terminal_size() })
    }

    /// Runs the invalidate output frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn invalidate_output_frame<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, ()> {
        Box::pin(async move { self.inner.invalidate_output_frame() })
    }
}

/// Tokio `AsyncFd` backed attached-terminal I/O endpoint.
///
/// The endpoint borrows process-owned terminal file descriptors; it never owns
/// or closes them. It temporarily enables `O_NONBLOCK` so Tokio readiness can
/// drive reads and writes, then restores the original file status flags on drop.
/// Raw-mode ownership remains separate and should stay guarded by terminal
/// raw-mode setup code at the attach boundary.
#[derive(Debug)]
pub struct AsyncAttachedTerminalFdLoopIo {
    /// Stores the input value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    input: AsyncFd<AsyncTerminalRawFd>,
    /// Stores the output value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    output: AsyncFd<AsyncTerminalRawFd>,
    /// Stores the control value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    control: Option<AsyncFd<AsyncTerminalRawFd>>,
    /// Stores the original flags value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    original_flags: Vec<(RawFd, OFlags)>,
    /// Stores the application keypad mode value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    application_keypad_mode: bool,
    /// Stores the previous output frame value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    previous_output_frame: Option<AttachedTerminalOutputFrameState>,
    /// Started output frame retained across bounded flush attempts.
    pending_output_frame: Option<PendingAttachedTerminalOutputFrame>,
    /// Whether a full redraw was requested while a pending frame was still
    /// being written.
    pending_output_invalidates_next_frame: bool,
    /// Current adaptive upper bound for one terminal output flush pass.
    output_write_limit_bytes: usize,
    /// Consecutive completed flushes since the last output short write.
    completed_output_flushes_since_short_write: u8,
    /// Stores the presentation active value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    presentation_active: bool,
}

impl AsyncAttachedTerminalFdLoopIo {
    /// Creates a Tokio-backed attached-terminal endpoint from raw file
    /// descriptors. The descriptors must remain valid for the lifetime of this
    /// value.
    pub fn new(input_fd: RawFd, output_fd: RawFd, control_fd: Option<RawFd>) -> Result<Self> {
        let mut original_flags = Vec::new();
        remember_original_flags(&mut original_flags, input_fd)?;
        remember_original_flags(&mut original_flags, output_fd)?;
        if let Some(control_fd) = control_fd {
            remember_original_flags(&mut original_flags, control_fd)?;
        }

        Ok(Self {
            input: AsyncFd::new(AsyncTerminalRawFd { fd: input_fd })?,
            output: AsyncFd::new(AsyncTerminalRawFd { fd: output_fd })?,
            control: control_fd
                .map(|fd| AsyncFd::new(AsyncTerminalRawFd { fd }))
                .transpose()?,
            original_flags,
            application_keypad_mode: false,
            previous_output_frame: None,
            pending_output_frame: None,
            pending_output_invalidates_next_frame: false,
            output_write_limit_bytes: DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES,
            completed_output_flushes_since_short_write: 0,
            presentation_active: false,
        })
    }

    /// Runs the readiness for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn readiness_for(
        role: AttachedTerminalFdRole,
        fd: RawFd,
        readable: bool,
        writable: bool,
    ) -> AttachedTerminalFdReadiness {
        AttachedTerminalFdReadiness {
            role,
            fd,
            interest: match role {
                AttachedTerminalFdRole::Input | AttachedTerminalFdRole::Control => {
                    TerminalFdInterest::read()
                }
                AttachedTerminalFdRole::Output => TerminalFdInterest::write(),
            },
            readable,
            writable,
            hangup: false,
            error: false,
        }
    }

    /// Queues a freshly encoded output frame for bounded flushing.
    fn queue_pending_output_frame(
        &mut self,
        lines: &[String],
        line_style_spans: &[Vec<TerminalStyleSpan>],
        modes: AttachedTerminalOutputModes,
    ) {
        let keypad_transition = if modes.application_keypad != self.application_keypad_mode {
            self.application_keypad_mode = modes.application_keypad;
            Some(modes.application_keypad)
        } else {
            None
        };
        let bytes = encode_attached_terminal_output_update_frame_with_styles(
            lines,
            line_style_spans,
            keypad_transition,
            modes,
            self.previous_output_frame.as_ref(),
        );
        let next_state =
            AttachedTerminalOutputFrameState::new_with_modes(lines, line_style_spans, modes);
        self.pending_output_frame = Some(PendingAttachedTerminalOutputFrame {
            bytes,
            written: 0,
            next_state,
        });
    }

    /// Flushes retained output bytes up to `max_bytes`.
    async fn flush_pending_output_bounded_attempt(
        &mut self,
        max_bytes: usize,
    ) -> Result<AttachedTerminalOutputFlushAttempt> {
        if max_bytes == 0 {
            return Err(MezError::invalid_args(
                "async attached terminal output write limit must be greater than zero",
            ));
        }
        let mut bytes_written = 0usize;
        let mut short_write_observed = false;
        while bytes_written < max_bytes {
            let Some(pending) = self.pending_output_frame.as_ref() else {
                return Ok(AttachedTerminalOutputFlushAttempt {
                    report: AsyncTerminalOutputWriteReport::completed(bytes_written),
                    short_write_observed,
                });
            };
            if pending.remaining_bytes() == 0 {
                self.commit_completed_pending_output_frame();
                continue;
            }
            let remaining_budget = max_bytes.saturating_sub(bytes_written);
            let write_end = pending
                .written
                .saturating_add(pending.remaining_bytes().min(remaining_budget));
            let write_start = pending.written;
            let bytes = pending.bytes[write_start..write_end].to_vec();
            let attempted_bytes = bytes.len();
            let count = write_some_async_fd(&self.output, &bytes).await?;
            if count < attempted_bytes {
                short_write_observed = true;
            }
            let Some(pending) = self.pending_output_frame.as_mut() else {
                return Err(MezError::invalid_state(
                    "pending attached terminal output disappeared during write",
                ));
            };
            pending.written = pending.written.saturating_add(count);
            bytes_written = bytes_written.saturating_add(count);
            if pending.remaining_bytes() == 0 {
                self.commit_completed_pending_output_frame();
                return Ok(AttachedTerminalOutputFlushAttempt {
                    report: AsyncTerminalOutputWriteReport::completed(bytes_written),
                    short_write_observed,
                });
            }
        }
        Ok(AttachedTerminalOutputFlushAttempt {
            report: AsyncTerminalOutputWriteReport {
                bytes_written,
                completed: false,
                pending_bytes: self.pending_output_bytes(),
            },
            short_write_observed,
        })
    }

    /// Flushes retained output bytes using the adaptive write budget.
    async fn flush_pending_output_bounded(
        &mut self,
        max_bytes: usize,
    ) -> Result<AsyncTerminalOutputWriteReport> {
        let effective_limit = max_bytes.min(self.output_write_limit_bytes);
        let started_at = Instant::now();
        let attempt = self
            .flush_pending_output_bounded_attempt(effective_limit)
            .await?;
        let backpressure_observed = attempt.short_write_observed
            || started_at.elapsed() >= ATTACHED_TERMINAL_OUTPUT_SLOW_FLUSH_THRESHOLD;
        let budget = adapt_attached_terminal_output_write_budget(
            AttachedTerminalOutputWriteBudget {
                limit_bytes: self.output_write_limit_bytes,
                completed_flushes_since_short_write: self
                    .completed_output_flushes_since_short_write,
            },
            attempt.report,
            backpressure_observed,
        );
        self.output_write_limit_bytes = budget.limit_bytes;
        self.completed_output_flushes_since_short_write =
            budget.completed_flushes_since_short_write;
        Ok(attempt.report)
    }

    /// Commits and clears a fully written pending output frame.
    fn commit_completed_pending_output_frame(&mut self) {
        let Some(pending) = self.pending_output_frame.take() else {
            return;
        };
        if self.pending_output_invalidates_next_frame {
            self.previous_output_frame = None;
            self.pending_output_invalidates_next_frame = false;
        } else {
            self.previous_output_frame = Some(pending.next_state);
        }
    }
}

/// Result of one internal output flush plus backpressure observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AttachedTerminalOutputFlushAttempt {
    /// Public write report for the flush pass.
    report: AsyncTerminalOutputWriteReport,
    /// Whether any nonblocking write accepted less than the requested chunk.
    short_write_observed: bool,
}

/// Adaptive attached-terminal output write budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AttachedTerminalOutputWriteBudget {
    /// Maximum bytes to attempt in one output flush pass.
    limit_bytes: usize,
    /// Consecutive completed flushes since the last short write.
    completed_flushes_since_short_write: u8,
}

/// Adapts the terminal output budget from the last flush observation.
fn adapt_attached_terminal_output_write_budget(
    budget: AttachedTerminalOutputWriteBudget,
    report: AsyncTerminalOutputWriteReport,
    backpressure_observed: bool,
) -> AttachedTerminalOutputWriteBudget {
    if backpressure_observed {
        return AttachedTerminalOutputWriteBudget {
            limit_bytes: (budget.limit_bytes / 2)
                .max(MIN_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES),
            completed_flushes_since_short_write: 0,
        };
    }
    if report.is_partial() {
        return AttachedTerminalOutputWriteBudget {
            limit_bytes: budget.limit_bytes,
            completed_flushes_since_short_write: 0,
        };
    }
    let completed_flushes_since_short_write =
        budget.completed_flushes_since_short_write.saturating_add(1);
    if completed_flushes_since_short_write < ATTACHED_TERMINAL_OUTPUT_WRITE_RECOVERY_FLUSHES {
        return AttachedTerminalOutputWriteBudget {
            limit_bytes: budget.limit_bytes,
            completed_flushes_since_short_write,
        };
    }
    AttachedTerminalOutputWriteBudget {
        limit_bytes: budget
            .limit_bytes
            .saturating_mul(2)
            .min(DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES),
        completed_flushes_since_short_write: 0,
    }
}

/// Encoded attached-terminal frame that has started writing but is not yet
/// complete.
#[derive(Debug)]
struct PendingAttachedTerminalOutputFrame {
    /// Encoded terminal bytes.
    bytes: Vec<u8>,
    /// Number of bytes already written to the attached terminal.
    written: usize,
    /// Differential frame state to commit only after all bytes are written.
    next_state: AttachedTerminalOutputFrameState,
}

impl PendingAttachedTerminalOutputFrame {
    /// Returns remaining bytes for this pending frame.
    fn remaining_bytes(&self) -> usize {
        self.bytes.len().saturating_sub(self.written)
    }
}

/// Owns foreground raw-mode and async presentation cleanup for one attached TTY.
///
/// The guard keeps the async terminal endpoint before the raw-mode guard so
/// drop-time best-effort presentation cleanup runs before termios restoration.
/// Callers should still prefer explicit `restore` so presentation errors can be
/// reported while raw mode is restored on every path.
#[derive(Debug)]
pub struct AsyncAttachedTerminalPresentationGuard {
    /// Stores the io value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    io: AsyncAttachedTerminalFdLoopIo,
    /// Stores the raw mode value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    raw_mode: TerminalRawModeGuard,
}

impl AsyncAttachedTerminalPresentationGuard {
    /// Enables raw mode and creates an async attached-terminal endpoint for the
    /// supplied foreground terminal file descriptors.
    pub fn new(input_fd: RawFd, output_fd: RawFd, control_fd: Option<RawFd>) -> Result<Self> {
        let raw_mode = TerminalRawModeGuard::enable(input_fd)?;
        let io = AsyncAttachedTerminalFdLoopIo::new(input_fd, output_fd, control_fd)?;
        Ok(Self { io, raw_mode })
    }

    /// Returns the guarded async terminal endpoint.
    pub fn io_mut(&mut self) -> &mut AsyncAttachedTerminalFdLoopIo {
        &mut self.io
    }

    /// Enters Mezzanine presentation mode for the guarded terminal.
    pub async fn enter_presentation(&mut self) -> Result<()> {
        self.io.enter_presentation().await
    }

    /// Restores presentation state and raw mode, always attempting both.
    pub async fn restore(&mut self) -> Result<()> {
        let presentation_result = self.io.restore_presentation().await;
        let raw_result = self.raw_mode.restore();
        match (presentation_result, raw_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error),
        }
    }
}

impl Drop for AsyncAttachedTerminalFdLoopIo {
    /// Runs the drop operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn drop(&mut self) {
        if self.presentation_active {
            let output_fd = self.output.get_ref().fd;
            if let Some((_, flags)) = self
                .original_flags
                .iter()
                .find(|(fd, _)| *fd == output_fd)
                .copied()
            {
                let _ = fcntl_setfl(borrow_async_raw_fd(output_fd), flags);
            }
            let _ = write_all_raw_fd_best_effort(
                output_fd,
                attached_terminal_restore_presentation_frame(),
            );
            self.presentation_active = false;
        }
        for (fd, flags) in &self.original_flags {
            let _ = fcntl_setfl(borrow_async_raw_fd(*fd), *flags);
        }
    }
}

impl AsyncAttachedTerminalIo for AsyncAttachedTerminalFdLoopIo {
    /// Runs the poll readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_readiness<'a>(
        &'a mut self,
    ) -> AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        Box::pin(async move {
            let input = &self.input;
            let output = &self.output;
            let control = self.control.as_ref();
            tokio::select! {
                biased;
                result = input.readable() => {
                    let _guard = result?;
                    Ok(vec![Self::readiness_for(
                        AttachedTerminalFdRole::Input,
                        input.get_ref().fd,
                        true,
                        false,
                    )])
                }
                result = async {
                    match control {
                        Some(control) => control.readable().await.map(Some),
                        None => std::future::pending().await,
                    }
                } => {
                    let Some(guard) = result? else {
                        return Ok(Vec::new());
                    };
                    Ok(vec![Self::readiness_for(
                        AttachedTerminalFdRole::Control,
                        guard.get_ref().get_ref().fd,
                        true,
                        false,
                    )])
                }
                result = output.writable() => {
                    let _guard = result?;
                    Ok(vec![Self::readiness_for(
                        AttachedTerminalFdRole::Output,
                        output.get_ref().fd,
                        false,
                        true,
                    )])
                }
            }
        })
    }

    /// Runs the poll input readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_input_readiness<'a>(
        &'a mut self,
    ) -> AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        Box::pin(async move {
            let input = &self.input;
            let control = self.control.as_ref();
            tokio::select! {
                biased;
                result = input.readable() => {
                    let _guard = result?;
                    Ok(vec![Self::readiness_for(
                        AttachedTerminalFdRole::Input,
                        input.get_ref().fd,
                        true,
                        false,
                    )])
                }
                result = async {
                    match control {
                        Some(control) => control.readable().await.map(Some),
                        None => std::future::pending().await,
                    }
                } => {
                    let Some(guard) = result? else {
                        return Ok(Vec::new());
                    };
                    Ok(vec![Self::readiness_for(
                        AttachedTerminalFdRole::Control,
                        guard.get_ref().get_ref().fd,
                        true,
                        false,
                    )])
                }
            }
        })
    }

    /// Runs the read input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_input<'a>(&'a mut self, max_bytes: usize) -> AsyncTerminalIoFuture<'a, Vec<u8>> {
        Box::pin(async move {
            if max_bytes == 0 {
                return Err(MezError::invalid_args(
                    "async attached terminal input read limit must be greater than zero",
                ));
            }
            loop {
                let mut guard = self.input.readable().await?;
                match guard.try_io(|inner| read_nonblocking_fd(inner.get_ref().fd, max_bytes)) {
                    Ok(result) => return Ok(result?),
                    Err(_would_block) => continue,
                }
            }
        })
    }

    /// Runs the write styled output with modes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_styled_output_with_modes<'a>(
        &'a mut self,
        lines: &'a [String],
        line_style_spans: &'a [Vec<TerminalStyleSpan>],
        modes: AttachedTerminalOutputModes,
    ) -> AsyncTerminalIoFuture<'a, usize> {
        Box::pin(async move {
            if self.pending_output_frame.is_some() {
                // A complete frame write supersedes any older retained partial
                // output. The next encoded frame must stand on its own.
                self.pending_output_frame = None;
                self.previous_output_frame = None;
                self.pending_output_invalidates_next_frame = false;
            }
            self.queue_pending_output_frame(lines, line_style_spans, modes);
            let mut bytes_written = 0usize;
            while self.pending_output_frame.is_some() {
                let report = self.flush_pending_output_bounded(usize::MAX).await?;
                bytes_written = bytes_written.saturating_add(report.bytes_written);
                if report.is_partial() && report.bytes_written == 0 {
                    return Err(MezError::invalid_state(
                        "async attached terminal output write made no progress",
                    ));
                }
            }
            Ok(bytes_written)
        })
    }

    fn pending_output_bytes(&self) -> usize {
        self.pending_output_frame
            .as_ref()
            .map(PendingAttachedTerminalOutputFrame::remaining_bytes)
            .unwrap_or(0)
    }

    fn flush_pending_output<'a>(
        &'a mut self,
        max_bytes: usize,
    ) -> AsyncTerminalIoFuture<'a, AsyncTerminalOutputWriteReport> {
        Box::pin(async move { self.flush_pending_output_bounded(max_bytes).await })
    }

    fn write_styled_output_with_modes_bounded<'a>(
        &'a mut self,
        lines: &'a [String],
        line_style_spans: &'a [Vec<TerminalStyleSpan>],
        modes: AttachedTerminalOutputModes,
        max_bytes: usize,
    ) -> AsyncTerminalIoFuture<'a, AsyncTerminalOutputWriteReport> {
        Box::pin(async move {
            if self.pending_output_frame.is_some() {
                // A newer render frame supersedes any old frame that could not
                // be fully written to a slow foreground terminal.
                self.pending_output_frame = None;
                self.previous_output_frame = None;
                self.pending_output_invalidates_next_frame = false;
            }
            self.queue_pending_output_frame(lines, line_style_spans, modes);
            self.flush_pending_output_bounded(max_bytes).await
        })
    }

    /// Runs the terminal size operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn terminal_size<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, Option<Size>> {
        Box::pin(async move { read_attached_terminal_size(self.output.get_ref().fd) })
    }

    /// Runs the invalidate output frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn invalidate_output_frame<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, ()> {
        Box::pin(async move {
            if self.pending_output_frame.is_some() {
                self.pending_output_frame = None;
                self.pending_output_invalidates_next_frame = false;
                self.previous_output_frame = None;
            } else {
                self.previous_output_frame = None;
            }
            Ok(())
        })
    }

    /// Runs the enter presentation operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn enter_presentation<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, ()> {
        Box::pin(async move {
            if self.presentation_active {
                return Ok(());
            }
            write_all_async_fd(&self.output, attached_terminal_enter_presentation_frame()).await?;
            self.presentation_active = true;
            Ok(())
        })
    }

    /// Runs the restore presentation operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn restore_presentation<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, ()> {
        Box::pin(async move {
            if !self.presentation_active {
                return Ok(());
            }
            match write_all_async_fd(&self.output, attached_terminal_restore_presentation_frame())
                .await
            {
                Ok(()) => {
                    self.presentation_active = false;
                    Ok(())
                }
                Err(error) if attached_terminal_output_disconnected(&error) => {
                    self.presentation_active = false;
                    Ok(())
                }
                Err(error) => Err(error),
            }
        })
    }
}

/// Carries Async Terminal Raw Fd state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
struct AsyncTerminalRawFd {
    /// Stores the fd value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    fd: RawFd,
}

impl AsRawFd for AsyncTerminalRawFd {
    /// Runs the as raw fd operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

/// Runs the remember original flags operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn remember_original_flags(flags: &mut Vec<(RawFd, OFlags)>, fd: RawFd) -> Result<()> {
    if fd < 0 {
        return Err(MezError::invalid_args(
            "async attached terminal file descriptor is invalid",
        ));
    }
    if flags.iter().any(|(known_fd, _)| *known_fd == fd) {
        return Ok(());
    }
    let original = fcntl_getfl(borrow_async_raw_fd(fd)).map_err(io::Error::from)?;
    if !original.contains(OFlags::NONBLOCK) {
        fcntl_setfl(borrow_async_raw_fd(fd), original | OFlags::NONBLOCK)
            .map_err(io::Error::from)?;
    }
    flags.push((fd, original));
    Ok(())
}

/// Runs the borrow async raw fd operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn borrow_async_raw_fd(fd: RawFd) -> BorrowedFd<'static> {
    // SAFETY: The async terminal adapter validates descriptors before
    // registering them and borrows each fd only for one immediate syscall.
    unsafe { BorrowedFd::borrow_raw(fd) }
}

/// Runs the read nonblocking fd operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn read_nonblocking_fd(fd: RawFd, max_bytes: usize) -> io::Result<Vec<u8>> {
    let mut buffer = vec![0u8; max_bytes];
    loop {
        match rustix_read(borrow_async_raw_fd(fd), buffer.as_mut_slice()) {
            Ok(count) => {
                buffer.truncate(count);
                return Ok(buffer);
            }
            Err(Errno::INTR) => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            Err(error) => return Err(io::Error::from(error)),
        }
    }
}

/// Runs the write nonblocking fd operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn write_nonblocking_fd(fd: RawFd, bytes: &[u8]) -> io::Result<usize> {
    match rustix_write(borrow_async_raw_fd(fd), bytes) {
        Ok(count) => Ok(count),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        }
        Err(error) => Err(io::Error::from(error)),
    }
}

/// Runs the write all raw fd best effort operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn write_all_raw_fd_best_effort(fd: RawFd, bytes: &[u8]) -> io::Result<()> {
    let mut written = 0usize;
    while written < bytes.len() {
        match rustix_write(borrow_async_raw_fd(fd), &bytes[written..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "attached terminal drop restore made no progress",
                ));
            }
            Ok(count) => written = written.saturating_add(count),
            Err(Errno::INTR) => continue,
            Err(error) => return Err(io::Error::from(error)),
        }
    }
    Ok(())
}

/// Runs the write all async fd operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn write_all_async_fd(fd: &AsyncFd<AsyncTerminalRawFd>, bytes: &[u8]) -> Result<()> {
    let mut written = 0usize;
    while written < bytes.len() {
        let count = write_some_async_fd(fd, &bytes[written..]).await?;
        written = written.saturating_add(count);
    }
    Ok(())
}

/// Writes one non-empty output chunk after Tokio reports output readiness.
async fn write_some_async_fd(fd: &AsyncFd<AsyncTerminalRawFd>, bytes: &[u8]) -> Result<usize> {
    if bytes.is_empty() {
        return Ok(0);
    }
    loop {
        let mut guard = fd.writable().await?;
        match guard.try_io(|inner| write_nonblocking_fd(inner.get_ref().fd, bytes)) {
            Ok(Ok(count)) if count > 0 => return Ok(count),
            Ok(Ok(_)) => {
                return Err(MezError::invalid_state(
                    "async attached terminal output write made no progress",
                ));
            }
            Ok(Err(error)) => return Err(error.into()),
            Err(_would_block) => continue,
        }
    }
}

/// Styled frame captured by the deterministic async fake.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncFakeTerminalFrame {
    /// Rendered lines.
    pub lines: Vec<String>,
    /// Rendered line style spans.
    pub line_style_spans: Vec<Vec<TerminalStyleSpan>>,
    /// Presentation modes used for this frame.
    pub modes: AttachedTerminalOutputModes,
}

/// Deterministic async attached-terminal fake for event-order and prompt tests.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct AsyncFakeAttachedTerminalIo {
    /// Stores the readiness batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    readiness_batches: VecDeque<Vec<AttachedTerminalFdReadiness>>,
    /// Stores the input batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    input_batches: VecDeque<Vec<u8>>,
    /// Number of input reads that should remain pending until caller timeout.
    pending_input_reads: usize,
    /// Stores the terminal size batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    terminal_size_batches: VecDeque<Option<Size>>,
    /// Frames written by code under test.
    pub written_frames: Vec<AsyncFakeTerminalFrame>,
    /// Number of output-frame invalidations.
    pub invalidated_output_frames: usize,
    /// Number of presentation entries.
    pub presentation_entries: usize,
    /// Number of presentation restores.
    pub presentation_restores: usize,
}

#[cfg(test)]
impl AsyncFakeAttachedTerminalIo {
    /// Adds a readiness batch to be returned by the next readiness poll.
    pub fn push_readiness(&mut self, readiness: Vec<AttachedTerminalFdReadiness>) {
        self.readiness_batches.push_back(readiness);
    }

    /// Adds an input batch to be returned by the next input read.
    pub fn push_input(&mut self, input: impl Into<Vec<u8>>) {
        self.input_batches.push_back(input.into());
    }
    /// Adds one input read that remains pending until the caller times out.
    pub fn push_pending_input_read(&mut self) {
        self.pending_input_reads = self.pending_input_reads.saturating_add(1);
    }

    /// Adds a terminal size response to be returned by the next size query.
    pub fn push_terminal_size(&mut self, size: Option<Size>) {
        self.terminal_size_batches.push_back(size);
    }
}

#[cfg(test)]
impl AsyncAttachedTerminalIo for AsyncFakeAttachedTerminalIo {
    /// Runs the poll readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_readiness<'a>(
        &'a mut self,
    ) -> AsyncTerminalIoFuture<'a, Vec<AttachedTerminalFdReadiness>> {
        Box::pin(async move { Ok(self.readiness_batches.pop_front().unwrap_or_default()) })
    }

    /// Runs the read input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_input<'a>(&'a mut self, max_bytes: usize) -> AsyncTerminalIoFuture<'a, Vec<u8>> {
        Box::pin(async move {
            if self.pending_input_reads > 0 {
                self.pending_input_reads -= 1;
                tokio::time::sleep(Duration::from_secs(60 * 60)).await;
                return Ok(Vec::new());
            }
            let mut input = self.input_batches.pop_front().unwrap_or_default();
            input.truncate(max_bytes);
            Ok(input)
        })
    }

    /// Runs the write styled output with modes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_styled_output_with_modes<'a>(
        &'a mut self,
        lines: &'a [String],
        line_style_spans: &'a [Vec<TerminalStyleSpan>],
        modes: AttachedTerminalOutputModes,
    ) -> AsyncTerminalIoFuture<'a, usize> {
        Box::pin(async move {
            self.written_frames.push(AsyncFakeTerminalFrame {
                lines: lines.to_vec(),
                line_style_spans: line_style_spans.to_vec(),
                modes,
            });
            Ok(lines.iter().map(String::len).sum())
        })
    }

    /// Runs the terminal size operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn terminal_size<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, Option<Size>> {
        Box::pin(async move { Ok(self.terminal_size_batches.pop_front().unwrap_or(None)) })
    }

    /// Runs the invalidate output frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn invalidate_output_frame<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, ()> {
        Box::pin(async move {
            self.invalidated_output_frames = self.invalidated_output_frames.saturating_add(1);
            Ok(())
        })
    }

    /// Runs the enter presentation operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn enter_presentation<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, ()> {
        Box::pin(async move {
            self.presentation_entries = self.presentation_entries.saturating_add(1);
            Ok(())
        })
    }

    /// Runs the restore presentation operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn restore_presentation<'a>(&'a mut self) -> AsyncTerminalIoFuture<'a, ()> {
        Box::pin(async move {
            self.presentation_restores = self.presentation_restores.saturating_add(1);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ATTACHED_TERMINAL_OUTPUT_WRITE_RECOVERY_FLUSHES, AsyncTerminalOutputWriteReport,
        AttachedTerminalOutputWriteBudget, DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES,
        MIN_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES,
        adapt_attached_terminal_output_write_budget,
    };

    /// Verifies that the attached-terminal output budget starts above the old
    /// small-frame cap while preserving a lower bound for slow clients.
    ///
    /// SSH sessions pay for each split frame, so the default should be large
    /// enough for common full redraws while still allowing adaptive reduction
    /// when writes start reporting backpressure.
    #[test]
    fn attached_terminal_output_default_budget_is_high_enough_to_reduce_later() {
        let default_limit =
            std::hint::black_box(DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES);
        let minimum_limit = std::hint::black_box(MIN_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES);

        assert_eq!(default_limit, 64 * 1024);
        assert!(default_limit > 16 * 1024);
        assert!(minimum_limit < default_limit);
    }

    /// Verifies that a short nonblocking terminal write reduces the next flush
    /// budget and clears recovery progress.
    ///
    /// A short write is the direct signal that the output side could not accept
    /// the full chunk, so the next pass should attempt less terminal traffic.
    #[test]
    fn attached_terminal_output_budget_reduces_after_short_write() {
        let budget = adapt_attached_terminal_output_write_budget(
            AttachedTerminalOutputWriteBudget {
                limit_bytes: DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES,
                completed_flushes_since_short_write: 2,
            },
            AsyncTerminalOutputWriteReport::completed(1024),
            true,
        );

        assert_eq!(
            budget.limit_bytes,
            DEFAULT_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES / 2
        );
        assert_eq!(budget.completed_flushes_since_short_write, 0);
    }

    /// Verifies that repeated clean output flushes restore a reduced budget
    /// toward the default value.
    ///
    /// This protects slow links from permanent degradation after temporary
    /// congestion, while still requiring several clean flushes before growth.
    #[test]
    fn attached_terminal_output_budget_recovers_after_clean_flushes() {
        let mut budget = AttachedTerminalOutputWriteBudget {
            limit_bytes: MIN_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES,
            completed_flushes_since_short_write: 0,
        };
        for _ in 0..ATTACHED_TERMINAL_OUTPUT_WRITE_RECOVERY_FLUSHES {
            budget = adapt_attached_terminal_output_write_budget(
                budget,
                AsyncTerminalOutputWriteReport::completed(1024),
                false,
            );
        }

        assert_eq!(
            budget.limit_bytes,
            MIN_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES * 2
        );
        assert_eq!(budget.completed_flushes_since_short_write, 0);
    }

    /// Verifies that a budget-limited partial flush does not increase the
    /// output budget before the frame has completed.
    ///
    /// Large frames can exceed the current cap without proving that the
    /// connection has recovered, so only completed flushes count toward growth.
    #[test]
    fn attached_terminal_output_budget_does_not_grow_on_partial_flush() {
        let budget = adapt_attached_terminal_output_write_budget(
            AttachedTerminalOutputWriteBudget {
                limit_bytes: MIN_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES,
                completed_flushes_since_short_write: 2,
            },
            AsyncTerminalOutputWriteReport {
                bytes_written: MIN_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES,
                completed: false,
                pending_bytes: 1024,
            },
            false,
        );

        assert_eq!(
            budget.limit_bytes,
            MIN_ATTACHED_TERMINAL_OUTPUT_WRITE_LIMIT_BYTES
        );
        assert_eq!(budget.completed_flushes_since_short_write, 0);
    }
}
