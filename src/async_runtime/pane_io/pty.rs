//! Tokio PTY readiness backend and process lifecycle adaptation.

use super::*;

/// Tokio readiness backend for a live portable-pty pane process.
///
/// The backend owns the `PaneProcess` after handoff from the actor and registers
/// a duplicated PTY master fd with Tokio. Reads are opportunistic nonblocking
/// polls, while output-activity waits and writes use reactor readiness. Process
/// termination is driven by nonblocking signal and `try_wait` polling.
#[derive(Debug)]
pub struct AsyncPtyPaneProcessIo {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pane_id: String,
    /// Stores the process value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    process: super::PaneProcess,
    /// Stores the pty value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pty: AsyncFd<AsyncPanePtyFd>,
}

impl AsyncPtyPaneProcessIo {
    /// Creates an async PTY backend for one live pane process.
    pub fn new(pane_id: impl Into<String>, process: super::PaneProcess) -> Result<Self> {
        let pane_id = pane_id.into();
        if pane_id.trim().is_empty() {
            return Err(MezError::invalid_args(
                "async pane PTY process backend pane id must not be empty",
            ));
        }
        let fd = process.duplicate_master_fd()?;
        let pty = AsyncFd::new(AsyncPanePtyFd { fd })?;
        Ok(Self {
            pane_id,
            process,
            pty,
        })
    }
}

impl Drop for AsyncPtyPaneProcessIo {
    /// Runs the drop operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn drop(&mut self) {
        if self.process.recorded_exit_status().is_none() {
            let _ = self
                .process
                .send_signal_to_process_group(rustix::process::Signal::KILL);
        }
    }
}

impl AsyncPaneProcessIo for AsyncPtyPaneProcessIo {
    /// Runs the read output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_output<'a>(&'a mut self, max_bytes: usize) -> AsyncPaneIoFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            let bytes = self.process.read_available_output(max_bytes)?;
            if bytes.is_empty() {
                Ok(None)
            } else {
                Ok(Some(bytes))
            }
        })
    }

    /// Runs the output activity operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn output_activity<'a>(&'a mut self) -> Option<AsyncPaneIoFuture<'a, ()>> {
        Some(Box::pin(async move {
            let mut guard = self.pty.readable().await?;
            guard.clear_ready();
            Ok(())
        }))
    }

    /// Runs the write input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_input<'a>(&'a mut self, bytes: &'a [u8]) -> AsyncPaneIoFuture<'a, usize> {
        let bytes = bytes.to_vec();
        Box::pin(async move {
            let mut written = 0usize;
            while written < bytes.len() {
                let mut guard =
                    match timeout(PANE_INPUT_WRITE_READY_TIMEOUT, self.pty.writable()).await {
                        Ok(Ok(guard)) => guard,
                        Ok(Err(error)) => {
                            if written > 0 {
                                return Ok(written);
                            }
                            return Err(error.into());
                        }
                        Err(_) => {
                            if written > 0 {
                                return Ok(written);
                            }
                            return Err(MezError::invalid_state(format!(
                                "pane PTY input write timed out after {} ms",
                                PANE_INPUT_WRITE_READY_TIMEOUT.as_millis()
                            )));
                        }
                    };
                match guard.try_io(|inner| {
                    mez_mux::process::write_pty_fd_nonblocking_io(
                        inner.get_ref().fd.as_raw_fd(),
                        &bytes[written..],
                    )
                }) {
                    Ok(Ok(count)) if count > 0 => {
                        written = written.saturating_add(count);
                    }
                    Ok(Ok(_)) => {
                        if written > 0 {
                            return Ok(written);
                        }
                        return Err(MezError::invalid_state(
                            "async pane PTY write accepted zero bytes",
                        ));
                    }
                    Ok(Err(error)) => {
                        if written > 0 {
                            return Ok(written);
                        }
                        return Err(error.into());
                    }
                    Err(_would_block) => continue,
                }
            }
            Ok(written)
        })
    }

    /// Runs the poll exit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_exit<'a>(&'a mut self) -> AsyncPaneIoFuture<'a, Option<ProcessEvent>> {
        let pane_id = self.pane_id.clone();
        Box::pin(async move {
            let Some(status) = self.process.poll_exit()? else {
                return Ok(None);
            };
            if self.process.has_pending_output() || !self.process.output_reader_closed() {
                return Ok(None);
            }
            Ok(Some(ProcessEvent::Exited {
                pane_id,
                primary_pid: Some(self.process.primary_pid()),
                exit_code: status.code,
                signal: status.signal.map(|signal| signal.to_string()),
            }))
        })
    }

    /// Runs the foreground process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn foreground_process<'a>(
        &'a mut self,
    ) -> AsyncPaneIoFuture<'a, Option<AsyncPaneForegroundProcess>> {
        Box::pin(async move {
            let Some(process_name) = self.process.foreground_process_name() else {
                return Ok(None);
            };
            let Some(process_group_id) = self.process.foreground_process_group_id() else {
                return Ok(None);
            };
            Ok(Some(AsyncPaneForegroundProcess {
                process_name,
                process_group_id,
                current_working_directory: self.process.current_working_directory(),
            }))
        })
    }

    /// Runs the resize operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn resize<'a>(&'a mut self, size: Size) -> AsyncPaneIoFuture<'a, ()> {
        Box::pin(async move { self.process.resize(size).map_err(Into::into) })
    }

    /// Runs the terminate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn terminate<'a>(&'a mut self, force: bool) -> AsyncPaneIoFuture<'a, ProcessEvent> {
        let pane_id = self.pane_id.clone();
        Box::pin(async move {
            let status = terminate_pane_process_async(&mut self.process, force).await?;
            Ok(ProcessEvent::Exited {
                pane_id,
                primary_pid: Some(self.process.primary_pid()),
                exit_code: status.code,
                signal: status.signal.map(|signal| signal.to_string()),
            })
        })
    }
}

/// Carries Async Pane Pty Fd state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
struct AsyncPanePtyFd {
    /// Stores the fd value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    fd: OwnedFd,
}

impl AsRawFd for AsyncPanePtyFd {
    /// Runs the as raw fd operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.fd.as_raw_fd()
    }
}

/// Runs the terminate pane process async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn terminate_pane_process_async(
    process: &mut super::PaneProcess,
    force: bool,
) -> Result<super::PaneExitStatus> {
    if let Some(status) = process.recorded_exit_status() {
        return Ok(status);
    }
    if force {
        process.send_signal_to_process_group(rustix::process::Signal::KILL)?;
        return wait_for_pane_process_exit_async(process, Duration::from_secs(1)).await;
    }
    process.send_signal_to_process_group(rustix::process::Signal::HUP)?;
    if let Some(status) =
        wait_for_optional_pane_process_exit_async(process, Duration::from_millis(500)).await?
    {
        return Ok(status);
    }
    process.send_signal_to_process_group(rustix::process::Signal::TERM)?;
    if let Some(status) =
        wait_for_optional_pane_process_exit_async(process, Duration::from_millis(500)).await?
    {
        return Ok(status);
    }
    process.send_signal_to_process_group(rustix::process::Signal::KILL)?;
    wait_for_pane_process_exit_async(process, Duration::from_secs(1)).await
}

/// Runs the wait for pane process exit async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn wait_for_pane_process_exit_async(
    process: &mut super::PaneProcess,
    timeout: Duration,
) -> Result<super::PaneExitStatus> {
    wait_for_optional_pane_process_exit_async(process, timeout)
        .await?
        .ok_or_else(|| MezError::invalid_state("pane process did not exit after kill signal"))
}

/// Runs the wait for optional pane process exit async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn wait_for_optional_pane_process_exit_async(
    process: &mut super::PaneProcess,
    timeout: Duration,
) -> Result<Option<super::PaneExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = process.poll_exit()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        sleep(Duration::from_millis(10)).await;
    }
}
