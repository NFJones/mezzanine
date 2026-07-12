//! Shared async-runtime test fixtures with no production ownership.

use super::*;

pub(super) fn test_service() -> RuntimeSessionService {
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
pub(super) fn unix_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Runs the test pane environment operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn test_pane_environment() -> crate::runtime::PaneEnvironment {
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
pub(super) fn test_service_with_event_log() -> RuntimeSessionService {
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
pub(super) struct FakeAttachedTerminalLoopIo {
    /// Stores the readiness batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) readiness_batches: Vec<Vec<AttachedTerminalFdReadiness>>,
    /// Stores the input batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) input_batches: Vec<Vec<u8>>,
    /// Stores the written batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) written_batches: Vec<Vec<String>>,
    /// Stores the write error kinds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) write_error_kinds: Vec<std::io::ErrorKind>,
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
        line_style_spans: &'a [Vec<mez_terminal::TerminalStyleSpan>],
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
pub(super) struct FakeResizingAttachedTerminalLoopIo {
    /// Stores the inner value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) inner: FakeAttachedTerminalLoopIo,
    /// Stores the terminal size batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) terminal_size_batches: Vec<Option<Size>>,
    /// Stores the invalidated output frames value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) invalidated_output_frames: usize,
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
        line_style_spans: &'a [Vec<mez_terminal::TerminalStyleSpan>],
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
pub(super) struct IdleAsyncAttachedTerminalLoopIo {
    /// Stores the write count value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) write_count: StdArc<AtomicUsize>,
    /// Stores the write notify value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) write_notify: StdArc<tokio::sync::Notify>,
}

impl IdleAsyncAttachedTerminalLoopIo {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn new(
        write_count: StdArc<AtomicUsize>,
        write_notify: StdArc<tokio::sync::Notify>,
    ) -> Self {
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
        _line_style_spans: &'a [Vec<mez_terminal::TerminalStyleSpan>],
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
pub(super) struct InvalidatingIdleAsyncAttachedTerminalLoopIo {
    /// Number of completed foreground frame writes.
    pub(super) write_count: StdArc<AtomicUsize>,
    /// Notification emitted after each completed foreground frame write.
    pub(super) write_notify: StdArc<tokio::sync::Notify>,
    /// Number of times retained differential output state was discarded.
    pub(super) invalidate_count: StdArc<AtomicUsize>,
    /// Terminal size responses returned by foreground size polling.
    pub(super) terminal_size_batches: Vec<Option<Size>>,
}

impl InvalidatingIdleAsyncAttachedTerminalLoopIo {
    /// Creates an idle output-counting fake for attached-terminal service
    /// tests.
    pub(super) fn new(
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
    pub(super) fn with_terminal_size_batches(
        mut self,
        terminal_size_batches: Vec<Option<Size>>,
    ) -> Self {
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
        _line_style_spans: &'a [Vec<mez_terminal::TerminalStyleSpan>],
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
pub(super) struct SupersedablePendingOutputIo {
    /// Completed latest-frame writes.
    pub(super) write_count: StdArc<AtomicUsize>,
    /// Notification emitted after a latest-frame write.
    pub(super) write_notify: StdArc<tokio::sync::Notify>,
    /// Simulated stale pending bytes from a partially written older frame.
    pub(super) pending_output_bytes: StdArc<AtomicUsize>,
    /// Count of stale pending-output flush attempts.
    pub(super) stale_flushes: StdArc<AtomicUsize>,
}

impl SupersedablePendingOutputIo {
    /// Builds a fake output endpoint with externally controlled pending bytes.
    pub(super) fn new(
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
        _line_style_spans: &'a [Vec<mez_terminal::TerminalStyleSpan>],
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
pub(super) struct SlowOutputAttachedTerminalLoopIo {
    /// Readiness batches returned by the foreground wait path.
    pub(super) readiness_batches: Vec<Vec<AttachedTerminalFdReadiness>>,
    /// Input payloads returned when input readiness is observed.
    pub(super) input_batches: Vec<Vec<u8>>,
    /// Maximum bytes accepted by one fake output write attempt.
    pub(super) write_limit: usize,
    /// Bytes retained from a started but incomplete output frame.
    pub(super) pending_output_bytes: usize,
    /// Number of fully completed output frames.
    pub(super) completed_frames: usize,
    /// Number of partial write attempts.
    pub(super) partial_writes: usize,
    /// Total bytes written by this fake.
    pub(super) bytes_written: usize,
}

impl SlowOutputAttachedTerminalLoopIo {
    /// Creates a slow output fake with no started output frame.
    pub(super) fn new(
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
/// Verifies that config-target persistence rejects symlink destinations before
/// opening the file. Config writes carry user secrets and must preserve the
/// synchronous config writer's direct-private-file expectation when they move
/// onto the async persistence worker.
#[cfg(unix)]
/// Runs the test supervised service operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn test_supervised_service(
    name: &'static str,
    exit: AsyncRuntimeServiceExit,
) -> AsyncRuntimeService {
    AsyncRuntimeService::new(name, async move { Ok(exit) })
}
/// Waits for rendered primary-client text to contain a target string and
/// returns the most recent rendered text for assertions.
pub(super) async fn wait_for_rendered_text(
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

/// Waits until one agent shell transaction timer has been both scheduled and
/// cancelled, proving the matching runtime transaction settled.
pub(super) async fn wait_for_shell_transaction_timer_settlement(
    handle: &super::AsyncRuntimeSessionHandle,
    label: &str,
) -> Result<()> {
    let mut scheduled_key = None;
    let mut cancelled_keys = Vec::new();
    for _ in 0..3000 {
        let timer_effects = handle.drain_timer_side_effects(16).await?;
        for effect in timer_effects {
            match effect {
                RuntimeSideEffect::ScheduleTimer { key, .. }
                    if key.kind == RuntimeTimerKind::ShellTransaction
                        && scheduled_key.is_none() =>
                {
                    if cancelled_keys.iter().any(|cancelled| cancelled == &key) {
                        return Ok(());
                    }
                    scheduled_key = Some(key);
                }
                RuntimeSideEffect::CancelTimer { key }
                    if key.kind == RuntimeTimerKind::ShellTransaction =>
                {
                    if scheduled_key.is_none() {
                        return Ok(());
                    }
                    if scheduled_key
                        .as_ref()
                        .is_some_and(|scheduled| scheduled == &key)
                    {
                        return Ok(());
                    }
                    cancelled_keys.push(key);
                }
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    Err(MezError::invalid_state(format!(
        "{label} shell transaction timer should settle before the test continues"
    )))
}
/// Reads one HTTP request from the local provider concurrency fixture.
///
/// The helper waits for the full body described by `Content-Length` so the
/// fixture can distinguish the intentionally slow and fast prompt requests by
/// their serialized model context.
pub(super) async fn async_provider_concurrency_read_http_request(
    stream: &mut tokio::net::TcpStream,
) -> String {
    use tokio::io::AsyncReadExt;

    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer).await.unwrap();
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if let Some(header_end) = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
        {
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find_map(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if request.len() >= header_end.saturating_add(content_length) {
                break;
            }
        }
    }
    String::from_utf8_lossy(&request).to_string()
}

/// Writes one OpenAI-compatible Chat Completions response for the provider
/// concurrency fixture.
///
/// The response uses structured MAAP JSON content so the runtime can complete
/// the turn without invoking privileged local actions.
pub(super) async fn async_provider_concurrency_write_chat_response(
    stream: &mut tokio::net::TcpStream,
    text: &str,
) {
    use tokio::io::AsyncWriteExt;

    let content = serde_json::json!({
        "rationale": "provider concurrency fixture completed the turn",
        "thought": null,
        "actions": [
            {
                "type": "say",
                "status": "final",
                "content_type": "text/plain; charset=utf-8",
                "text": text
            }
        ]
    })
    .to_string();
    let body = serde_json::json!({
        "model": "local-chat-model",
        "choices": [
            {
                "message": {
                    "role": "assistant",
                    "content": content,
                    "tool_calls": []
                }
            }
        ],
        "usage": {
            "prompt_tokens": 3,
            "completion_tokens": 2
        }
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
}
