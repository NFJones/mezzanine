//! Async pane process I/O driver boundary.
//!
//! Tokio-native pane process I/O workers.
//!
//! The runtime actor owns pane state, while one Tokio task owns each live pane
//! process and PTY master. The backend reads and writes the PTY through
//! nonblocking file descriptors, converts output and side-effect completions
//! into typed runtime events, and keeps I/O failure handling local to the pane
//! instead of letting one pane block the whole runtime.

use super::{
    AsyncRuntimeService, AsyncRuntimeServiceExit, AsyncRuntimeSessionHandle, Duration, MezError,
    PaneEvent, ProcessEvent, Result, RuntimeEvent, RuntimeEventBatch, RuntimeLifecycleState,
    RuntimeSideEffect, Size, is_terminal_runtime_lifecycle_state,
};
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::pin::Pin;
use tokio::io::unix::AsyncFd;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep, timeout};

/// Maximum time to wait for one PTY input chunk to become writable.
///
/// Agent shell transactions must never be able to wedge the async pane worker
/// while delivering generated input. A stalled PTY write is surfaced as a pane
/// write failure so runtime transaction timeouts and diagnostics can proceed.
const PANE_INPUT_WRITE_READY_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies async pane-input writes allow ten seconds for PTY writability
    /// before reporting a bounded failure.
    ///
    /// The async runtime drives live agent shell transactions, so this timeout
    /// must be long enough for slower pane transports while still preventing
    /// indefinite runtime worker stalls.
    #[test]
    fn async_pane_input_write_timeout_is_ten_seconds() {
        assert_eq!(PANE_INPUT_WRITE_READY_TIMEOUT, Duration::from_secs(10));
    }
}

/// Boxed future returned by async pane I/O trait methods.
pub type AsyncPaneIoFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Foreground process metadata observed from a pane backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncPaneForegroundProcess {
    /// Foreground process display name.
    pub process_name: String,
    /// Foreground process group id.
    pub process_group_id: u32,
    /// Foreground process current working directory when known.
    pub current_working_directory: Option<PathBuf>,
}

/// Async backend for one pane process and its PTY.
pub trait AsyncPaneProcessIo {
    /// Reads the next available PTY output chunk.
    fn read_output<'a>(&'a mut self, max_bytes: usize) -> AsyncPaneIoFuture<'a, Option<Vec<u8>>>;

    /// Waits for backend output activity when the backend can signal it.
    ///
    /// Backends that cannot expose activity notifications may return `None`;
    /// the service loop will keep its compatibility interval for those cases.
    fn output_activity<'a>(&'a mut self) -> Option<AsyncPaneIoFuture<'a, ()>> {
        None
    }

    /// Polls the pane process for a natural exit.
    fn poll_exit<'a>(&'a mut self) -> AsyncPaneIoFuture<'a, Option<ProcessEvent>> {
        Box::pin(async { Ok(None) })
    }

    /// Polls foreground process metadata when the backend can observe it.
    fn foreground_process<'a>(
        &'a mut self,
    ) -> AsyncPaneIoFuture<'a, Option<AsyncPaneForegroundProcess>> {
        Box::pin(async { Ok(None) })
    }

    /// Writes input bytes to the pane PTY and returns bytes accepted.
    fn write_input<'a>(&'a mut self, bytes: &'a [u8]) -> AsyncPaneIoFuture<'a, usize>;

    /// Resizes the pane PTY.
    fn resize<'a>(&'a mut self, size: Size) -> AsyncPaneIoFuture<'a, ()>;

    /// Terminates the pane process and returns a process lifecycle event.
    fn terminate<'a>(&'a mut self, force: bool) -> AsyncPaneIoFuture<'a, ProcessEvent>;
}

/// Configuration for one async pane process driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncPaneProcessDriverConfig {
    /// Maximum PTY output bytes emitted in one runtime event.
    pub max_output_bytes_per_event: usize,
}

impl Default for AsyncPaneProcessDriverConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            max_output_bytes_per_event: 64 * 1024,
        }
    }
}

impl AsyncPaneProcessDriverConfig {
    /// Validates driver limits before polling a backend.
    pub fn validate(self) -> Result<()> {
        if self.max_output_bytes_per_event == 0 {
            return Err(MezError::invalid_args(
                "async pane output event byte limit must be greater than zero",
            ));
        }
        Ok(())
    }
}

/// Per-pane async process driver.
#[derive(Debug)]
pub struct AsyncPaneProcessDriver<B> {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pane_id: String,
    /// Stores the backend value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    backend: B,
    /// Stores the config value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    config: AsyncPaneProcessDriverConfig,
    /// Stores the exit reported value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    exit_reported: bool,
}

impl<B> AsyncPaneProcessDriver<B> {
    /// Creates a driver for one pane backend.
    pub fn new(
        pane_id: impl Into<String>,
        backend: B,
        config: AsyncPaneProcessDriverConfig,
    ) -> Result<Self> {
        config.validate()?;
        let pane_id = pane_id.into();
        if pane_id.is_empty() {
            return Err(MezError::invalid_args(
                "async pane process driver pane id must not be empty",
            ));
        }
        Ok(Self {
            pane_id,
            backend,
            config,
            exit_reported: false,
        })
    }

    /// Returns the pane identity owned by this driver.
    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }

    /// Waits for backend output activity when the backend can signal it.
    pub fn output_activity(&mut self) -> Option<AsyncPaneIoFuture<'_, ()>>
    where
        B: AsyncPaneProcessIo,
    {
        self.backend.output_activity()
    }

    /// Returns the wrapped backend after test or migration use.
    pub fn into_backend(self) -> B {
        self.backend
    }
}

impl<B> AsyncPaneProcessDriver<B>
where
    B: AsyncPaneProcessIo,
{
    /// Polls one output event from the pane PTY.
    pub async fn poll_output_event(&mut self) -> Result<Option<RuntimeEvent>> {
        self.config.validate()?;
        let Some(bytes) = self
            .backend
            .read_output(self.config.max_output_bytes_per_event)
            .await?
        else {
            return Ok(None);
        };
        if bytes.is_empty() {
            return Ok(None);
        }
        Ok(Some(RuntimeEvent::Pane(PaneEvent::Output {
            pane_id: self.pane_id.clone(),
            bytes,
        })))
    }

    /// Polls one process-exit event from the pane backend.
    pub async fn poll_exit_event(&mut self) -> Result<Option<RuntimeEvent>> {
        if self.exit_reported {
            return Ok(None);
        }
        let Some(event) = self.backend.poll_exit().await? else {
            return Ok(None);
        };
        if matches!(event, ProcessEvent::Exited { .. }) {
            self.exit_reported = true;
        }
        Ok(Some(RuntimeEvent::Process(event)))
    }

    /// Polls one foreground-process metadata event from the pane backend.
    pub async fn poll_foreground_process_event(&mut self) -> Result<Option<RuntimeEvent>> {
        let Some(metadata) = self.backend.foreground_process().await? else {
            return Ok(None);
        };
        Ok(Some(RuntimeEvent::Pane(PaneEvent::ForegroundProcess {
            pane_id: self.pane_id.clone(),
            process_name: metadata.process_name,
            process_group_id: metadata.process_group_id,
            current_working_directory: metadata
                .current_working_directory
                .map(|path| path.to_string_lossy().to_string()),
        })))
    }

    /// Writes input and returns the resulting pane I/O event.
    pub async fn write_input_event(&mut self, bytes: &[u8]) -> RuntimeEvent {
        match self.backend.write_input(bytes).await {
            Ok(0) => RuntimeEvent::Pane(PaneEvent::WriteFailed {
                pane_id: self.pane_id.clone(),
                error: "InvalidState: pane PTY write accepted zero bytes".to_string(),
            }),
            Ok(written) => RuntimeEvent::Pane(PaneEvent::InputWritten {
                pane_id: self.pane_id.clone(),
                bytes: written,
            }),
            Err(error) => RuntimeEvent::Pane(PaneEvent::WriteFailed {
                pane_id: self.pane_id.clone(),
                error: error.to_string(),
            }),
        }
    }

    /// Resizes the pane PTY and returns the resulting pane I/O event.
    pub async fn resize_event(&mut self, size: Size) -> RuntimeEvent {
        match self.backend.resize(size).await {
            Ok(()) => RuntimeEvent::Pane(PaneEvent::Resized {
                pane_id: self.pane_id.clone(),
                size,
            }),
            Err(error) => RuntimeEvent::Process(ProcessEvent::Failed {
                pane_id: self.pane_id.clone(),
                error: error.to_string(),
            }),
        }
    }

    /// Terminates the pane process and returns the lifecycle event.
    pub async fn terminate_event(&mut self, force: bool) -> RuntimeEvent {
        let event = match self.backend.terminate(force).await {
            Ok(event) => RuntimeEvent::Process(event),
            Err(error) => RuntimeEvent::Process(ProcessEvent::Failed {
                pane_id: self.pane_id.clone(),
                error: error.to_string(),
            }),
        };
        if matches!(event, RuntimeEvent::Process(ProcessEvent::Exited { .. })) {
            self.exit_reported = true;
        }
        event
    }
}

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
                    crate::process::write_pty_fd_nonblocking_io(
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
        Box::pin(async move { self.process.resize(size) })
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
async fn terminate_pane_process_async(
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
async fn wait_for_pane_process_exit_async(
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
async fn wait_for_optional_pane_process_exit_async(
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

/// Configuration for a pane process driver service loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncPaneProcessDriverServiceConfig {
    /// Maximum output polls before the service returns.
    pub max_polls: u64,
    /// Sleep interval used after an empty output poll.
    pub idle_interval: Duration,
}

impl Default for AsyncPaneProcessDriverServiceConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            max_polls: u64::MAX,
            idle_interval: Duration::from_millis(16),
        }
    }
}

impl AsyncPaneProcessDriverServiceConfig {
    /// Validates service loop bounds.
    pub fn validate(self) -> Result<()> {
        if self.max_polls == 0 {
            return Err(MezError::invalid_args(
                "async pane driver service max_polls must be greater than zero",
            ));
        }
        if self.idle_interval.is_zero() {
            return Err(MezError::invalid_args(
                "async pane driver service idle interval must be greater than zero",
            ));
        }
        Ok(())
    }
}

/// Report returned by one pane process driver service loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncPaneProcessDriverServiceReport {
    /// Number of output polls attempted.
    pub polls: u64,
    /// Number of runtime events submitted to the actor.
    pub submitted_events: usize,
    /// Number of submitted events applied to runtime state.
    pub applied_events: usize,
}

/// Runs one pane driver until stopped, submitting output events to the actor.
pub async fn run_async_pane_process_driver_service<B, F>(
    handle: &AsyncRuntimeSessionHandle,
    driver: &mut AsyncPaneProcessDriver<B>,
    config: AsyncPaneProcessDriverServiceConfig,
    mut should_stop: F,
) -> Result<AsyncPaneProcessDriverServiceReport>
where
    B: AsyncPaneProcessIo,
    F: FnMut(u64) -> bool,
{
    config.validate()?;
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let mut report = AsyncPaneProcessDriverServiceReport {
        polls: 0,
        submitted_events: 0,
        applied_events: 0,
    };

    while report.polls < config.max_polls {
        if should_stop(report.polls) {
            return Ok(report);
        }
        report.polls = report.polls.saturating_add(1);
        let Some(event) = driver.poll_output_event().await? else {
            if report.polls >= config.max_polls {
                return Ok(report);
            }
            if should_stop(report.polls) {
                return Ok(report);
            }
            let bounded_idle = (config.max_polls != u64::MAX).then_some(config.idle_interval);
            match (driver.output_activity(), bounded_idle) {
                (Some(output_activity), Some(idle_interval)) => {
                    tokio::select! {
                        result = output_activity => result?,
                        _ = handle.wait_for_event_delivery() => {}
                        result = side_effect_watcher.changed() => {
                            let _ = result;
                        }
                        _ = sleep(idle_interval) => {}
                    }
                }
                (Some(output_activity), None) => {
                    tokio::select! {
                        result = output_activity => result?,
                        _ = handle.wait_for_event_delivery() => {}
                        result = side_effect_watcher.changed() => {
                            let _ = result;
                        }
                    }
                }
                (None, Some(idle_interval)) => {
                    tokio::select! {
                        _ = handle.wait_for_event_delivery() => {}
                        result = side_effect_watcher.changed() => {
                            let _ = result;
                        }
                        _ = sleep(idle_interval) => {}
                    }
                }
                (None, None) => {
                    tokio::select! {
                        _ = handle.wait_for_event_delivery() => {}
                        result = side_effect_watcher.changed() => {
                            let _ = result;
                        }
                    }
                }
            }
            continue;
        };
        let mut batch = RuntimeEventBatch::new();
        batch.push(event);
        let ingress = handle.submit_runtime_events(batch).await?;
        report.submitted_events = report.submitted_events.saturating_add(ingress.accepted);
        report.applied_events = report.applied_events.saturating_add(ingress.applied);
    }

    Ok(report)
}

/// Configuration for one pane I/O side-effect worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncPaneIoSideEffectServiceConfig {
    /// Maximum side-effect polls before the service returns.
    pub max_polls: u64,
    /// Maximum pane I/O side effects drained per poll.
    pub drain_limit: usize,
    /// Sleep interval used after an empty drain.
    pub idle_interval: Duration,
}

impl Default for AsyncPaneIoSideEffectServiceConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            max_polls: u64::MAX,
            drain_limit: 1,
            idle_interval: Duration::from_millis(16),
        }
    }
}

impl AsyncPaneIoSideEffectServiceConfig {
    /// Validates pane side-effect worker bounds.
    pub fn validate(self) -> Result<()> {
        if self.max_polls == 0 {
            return Err(MezError::invalid_args(
                "async pane I/O side-effect service max_polls must be greater than zero",
            ));
        }
        if self.drain_limit == 0 {
            return Err(MezError::invalid_args(
                "async pane I/O side-effect service drain_limit must be greater than zero",
            ));
        }
        if self.idle_interval.is_zero() {
            return Err(MezError::invalid_args(
                "async pane I/O side-effect service idle interval must be greater than zero",
            ));
        }
        Ok(())
    }
}

/// Report returned by one pane I/O side-effect worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncPaneIoSideEffectServiceReport {
    /// Number of side-effect polls attempted.
    pub polls: u64,
    /// Number of pane side effects drained.
    pub drained: u64,
    /// Number of runtime events submitted after pane I/O execution.
    pub submitted_events: usize,
    /// Number of submitted events applied to runtime state.
    pub applied_events: usize,
    /// Last observed runtime lifecycle state.
    pub terminal_state: RuntimeLifecycleState,
}

/// Configuration for one combined pane process worker.
///
/// This service shape is the migration target for Phase 4: one task owns one
/// pane backend, drains output, executes pane I/O side effects, and reports all
/// resulting runtime events in the same per-pane order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncPaneProcessServiceConfig {
    /// Maximum service polls before returning.
    pub max_polls: u64,
    /// Maximum pane I/O side effects drained per poll.
    pub drain_limit: usize,
    /// Sleep interval used when neither output nor side effects are ready.
    pub idle_interval: Duration,
    /// Minimum interval between foreground process metadata polls.
    pub foreground_metadata_interval: Duration,
}

impl Default for AsyncPaneProcessServiceConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            max_polls: u64::MAX,
            drain_limit: 1,
            idle_interval: Duration::from_millis(16),
            foreground_metadata_interval: Duration::from_secs(1),
        }
    }
}

impl AsyncPaneProcessServiceConfig {
    /// Validates service loop bounds.
    pub fn validate(self) -> Result<()> {
        if self.max_polls == 0 {
            return Err(MezError::invalid_args(
                "async pane process service max_polls must be greater than zero",
            ));
        }
        if self.drain_limit == 0 {
            return Err(MezError::invalid_args(
                "async pane process service drain_limit must be greater than zero",
            ));
        }
        if self.idle_interval.is_zero() {
            return Err(MezError::invalid_args(
                "async pane process service idle interval must be greater than zero",
            ));
        }
        if self.foreground_metadata_interval.is_zero() {
            return Err(MezError::invalid_args(
                "async pane process service foreground metadata interval must be greater than zero",
            ));
        }
        Ok(())
    }
}

/// Report returned by one combined pane process worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncPaneProcessServiceReport {
    /// Number of service polls attempted.
    pub polls: u64,
    /// Number of pane output events observed.
    pub output_events: u64,
    /// Number of natural process exit events observed.
    pub exit_events: u64,
    /// Number of pane I/O side effects drained.
    pub drained: u64,
    /// Number of runtime events submitted after pane I/O execution.
    pub submitted_events: usize,
    /// Number of submitted events applied to runtime state.
    pub applied_events: usize,
    /// Last observed runtime lifecycle state.
    pub terminal_state: RuntimeLifecycleState,
}

impl AsyncPaneProcessServiceReport {
    /// Creates a report initialized with the actor lifecycle state observed at
    /// service startup.
    fn new(initial_state: RuntimeLifecycleState) -> Self {
        Self {
            polls: 0,
            output_events: 0,
            exit_events: 0,
            drained: 0,
            submitted_events: 0,
            applied_events: 0,
            terminal_state: initial_state,
        }
    }
}

/// Configuration for the daemon pane-process supervisor.
///
/// The supervisor is responsible for dynamic ownership transfer: it asks the
/// actor for running pane processes that are still manager-owned, starts one
/// combined async pane worker per process, and continues watching for panes
/// created after daemon startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsyncPaneProcessSupervisorServiceConfig {
    /// Maximum supervisor polling iterations before returning.
    pub max_polls: u64,
    /// Maximum pane processes to claim from the actor per poll.
    pub take_limit: usize,
    /// Sleep interval used when no handoff, side effect, event, or worker
    /// completion is ready.
    pub idle_interval: Duration,
    /// Configuration passed to each per-pane process worker.
    pub pane_service: AsyncPaneProcessServiceConfig,
}

impl Default for AsyncPaneProcessSupervisorServiceConfig {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            max_polls: u64::MAX,
            take_limit: 16,
            idle_interval: Duration::from_millis(100),
            pane_service: AsyncPaneProcessServiceConfig::default(),
        }
    }
}

impl AsyncPaneProcessSupervisorServiceConfig {
    /// Validates supervisor and per-pane worker bounds.
    pub fn validate(self) -> Result<()> {
        if self.max_polls == 0 {
            return Err(MezError::invalid_args(
                "async pane process supervisor max_polls must be greater than zero",
            ));
        }
        if self.take_limit == 0 {
            return Err(MezError::invalid_args(
                "async pane process supervisor take_limit must be greater than zero",
            ));
        }
        if self.idle_interval.is_zero() {
            return Err(MezError::invalid_args(
                "async pane process supervisor idle interval must be greater than zero",
            ));
        }
        self.pane_service.validate()
    }
}

/// Report returned by the dynamic pane-process supervisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncPaneProcessSupervisorServiceReport {
    /// Number of supervisor polling iterations.
    pub polls: u64,
    /// Number of pane workers spawned from actor-owned handoffs.
    pub spawned_workers: u64,
    /// Number of pane workers that completed successfully.
    pub completed_workers: u64,
    /// Last observed runtime lifecycle state.
    pub terminal_state: RuntimeLifecycleState,
}

impl AsyncPaneProcessSupervisorServiceReport {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn new(initial_state: RuntimeLifecycleState) -> Self {
        Self {
            polls: 0,
            spawned_workers: 0,
            completed_workers: 0,
            terminal_state: initial_state,
        }
    }
}

/// Runs one combined pane process worker until stopped.
///
/// The worker first drains at most one PTY output chunk, then drains pending
/// pane I/O side effects for the same pane. This keeps the future live
/// ownership path from racing write, resize, terminate, and output handling
/// across independent tasks.
pub async fn run_async_pane_process_service<B, F>(
    handle: &AsyncRuntimeSessionHandle,
    driver: &mut AsyncPaneProcessDriver<B>,
    config: AsyncPaneProcessServiceConfig,
    mut should_stop: F,
) -> Result<AsyncPaneProcessServiceReport>
where
    B: AsyncPaneProcessIo,
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    config.validate()?;
    let mut lifecycle_watcher = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let mut report = AsyncPaneProcessServiceReport::new(*lifecycle_watcher.borrow());
    let mut last_foreground_metadata_poll: Option<Instant> = None;
    let mut pending_pane_io_side_effects = VecDeque::new();

    while report.polls < config.max_polls {
        let state = *lifecycle_watcher.borrow_and_update();
        report.terminal_state = state;
        if is_terminal_runtime_lifecycle_state(state) {
            terminate_pane_process_for_terminal_state(handle, driver, config, state, &mut report)
                .await?;
            return Ok(report);
        }
        if should_stop(report.polls, state) {
            return Ok(report);
        }

        report.polls = report.polls.saturating_add(1);
        let mut made_progress = false;
        let mut observed_output = false;
        let mut pane_exited = false;

        if let Some(event) = driver.poll_output_event().await? {
            report.output_events = report.output_events.saturating_add(1);
            submit_pane_runtime_event(
                handle,
                event,
                &mut report.submitted_events,
                &mut report.applied_events,
            )
            .await?;
            made_progress = true;
            observed_output = true;
        }

        let foreground_metadata_due = observed_output
            || last_foreground_metadata_poll
                .is_none_or(|last_poll| last_poll.elapsed() >= config.foreground_metadata_interval);
        if foreground_metadata_due {
            last_foreground_metadata_poll = Some(Instant::now());
            if let Some(event) = driver.poll_foreground_process_event().await? {
                submit_pane_runtime_event(
                    handle,
                    event,
                    &mut report.submitted_events,
                    &mut report.applied_events,
                )
                .await?;
                made_progress = true;
            }
        }

        let effects = if pending_pane_io_side_effects.is_empty() {
            handle
                .drain_pane_io_side_effects(driver.pane_id().to_string(), config.drain_limit)
                .await?
        } else {
            drain_pending_pane_io_side_effects(
                &mut pending_pane_io_side_effects,
                config.drain_limit,
            )
        };
        if !effects.is_empty() {
            made_progress = true;
            report.drained = report
                .drained
                .saturating_add(u64::try_from(effects.len()).unwrap_or(u64::MAX));
            for event in
                pane_io_events_for_side_effects(driver, effects, &mut pending_pane_io_side_effects)
                    .await
            {
                pane_exited |= is_process_exit_event(&event);
                submit_pane_runtime_event(
                    handle,
                    event,
                    &mut report.submitted_events,
                    &mut report.applied_events,
                )
                .await?;
            }
        }

        if !observed_output && let Some(event) = driver.poll_exit_event().await? {
            report.exit_events = report.exit_events.saturating_add(1);
            pane_exited = is_process_exit_event(&event);
            submit_pane_runtime_event(
                handle,
                event,
                &mut report.submitted_events,
                &mut report.applied_events,
            )
            .await?;
            made_progress = true;
        }

        if pane_exited {
            report.terminal_state = *lifecycle_watcher.borrow();
            return Ok(report);
        }

        if !made_progress && report.polls < config.max_polls {
            let idle_delay = pane_process_quiet_delay(last_foreground_metadata_poll, config);
            if let Some(output_activity) = driver.output_activity() {
                tokio::select! {
                    result = output_activity => result?,
                    _ = handle.wait_for_event_delivery() => {}
                    result = side_effect_watcher.changed() => {
                        let _ = result;
                    }
                    result = lifecycle_watcher.changed() => {
                        let _ = result;
                    }
                    _ = sleep(idle_delay) => {}
                }
            } else {
                tokio::select! {
                    _ = handle.wait_for_event_delivery() => {}
                    result = side_effect_watcher.changed() => {
                        let _ = result;
                    }
                    result = lifecycle_watcher.changed() => {
                        let _ = result;
                    }
                    _ = sleep(idle_delay) => {}
                }
            }
        }
    }

    report.terminal_state = *lifecycle_watcher.borrow();
    Ok(report)
}

/// Runs the terminate pane process for terminal state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn terminate_pane_process_for_terminal_state<B>(
    handle: &AsyncRuntimeSessionHandle,
    driver: &mut AsyncPaneProcessDriver<B>,
    config: AsyncPaneProcessServiceConfig,
    state: RuntimeLifecycleState,
    report: &mut AsyncPaneProcessServiceReport,
) -> Result<()>
where
    B: AsyncPaneProcessIo,
{
    let pane_id = driver.pane_id().to_string();
    let mut force = matches!(
        state,
        RuntimeLifecycleState::Killed | RuntimeLifecycleState::Failed
    );
    let effects = handle
        .drain_pane_io_side_effects(pane_id, config.drain_limit)
        .await?;
    report.drained = report
        .drained
        .saturating_add(u64::try_from(effects.len()).unwrap_or(u64::MAX));
    for effect in effects {
        if let RuntimeSideEffect::TerminatePane {
            force: requested_force,
            ..
        } = effect
        {
            force |= requested_force;
        }
    }
    let event = driver.terminate_event(force).await;
    if is_process_exit_event(&event) {
        report.exit_events = report.exit_events.saturating_add(1);
    }
    Ok(())
}

/// Runs the pane process quiet delay operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn pane_process_quiet_delay(
    last_foreground_metadata_poll: Option<Instant>,
    config: AsyncPaneProcessServiceConfig,
) -> Duration {
    let Some(last_foreground_metadata_poll) = last_foreground_metadata_poll else {
        return config.idle_interval;
    };
    let remaining = config
        .foreground_metadata_interval
        .saturating_sub(last_foreground_metadata_poll.elapsed());
    if remaining.is_zero() {
        config.idle_interval
    } else {
        remaining
    }
}

/// Builds an auxiliary service for the combined async pane process path.
pub fn build_async_pane_process_service<B>(
    name: impl Into<String>,
    handle: AsyncRuntimeSessionHandle,
    mut driver: AsyncPaneProcessDriver<B>,
    config: AsyncPaneProcessServiceConfig,
) -> Result<AsyncRuntimeService>
where
    B: AsyncPaneProcessIo + Send + 'static,
{
    config.validate()?;
    Ok(AsyncRuntimeService::new_auxiliary(name, async move {
        let report = run_async_pane_process_service(&handle, &mut driver, config, |_, state| {
            is_terminal_runtime_lifecycle_state(state)
        })
        .await?;
        let work_units = report.drained.saturating_add(report.exit_events);
        if is_terminal_runtime_lifecycle_state(report.terminal_state) {
            Ok(AsyncRuntimeServiceExit::shutdown(work_units))
        } else {
            Ok(AsyncRuntimeServiceExit::completed(work_units))
        }
    }))
}

/// Runs the daemon pane-process supervisor until stopped.
///
/// The supervisor claims any manager-owned running pane processes through the
/// actor and immediately moves each claimed process into a combined async pane
/// worker. Each worker owns its pane backend until the pane exits or the daemon
/// enters a terminal lifecycle state.
pub async fn run_async_pane_process_supervisor_service<F>(
    handle: AsyncRuntimeSessionHandle,
    config: AsyncPaneProcessSupervisorServiceConfig,
    mut should_stop: F,
) -> Result<AsyncPaneProcessSupervisorServiceReport>
where
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    config.validate()?;
    let mut lifecycle_watcher = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let mut report = AsyncPaneProcessSupervisorServiceReport::new(*lifecycle_watcher.borrow());
    let mut active_panes = HashSet::<String>::new();
    let mut workers = JoinSet::new();

    while report.polls < config.max_polls {
        let state = *lifecycle_watcher.borrow_and_update();
        report.terminal_state = state;
        drain_completed_pane_process_workers(&mut workers, &mut active_panes, &mut report)?;
        if is_terminal_runtime_lifecycle_state(report.terminal_state) {
            drain_completed_pane_process_workers_after_yields(
                &mut workers,
                &mut active_panes,
                &mut report,
            )
            .await?;
            abort_pane_process_workers(&mut workers).await;
            return Ok(report);
        }
        if should_stop(report.polls, state) {
            abort_pane_process_workers(&mut workers).await;
            return Ok(report);
        }

        report.polls = report.polls.saturating_add(1);
        let mut made_progress = false;
        let processes = match handle
            .take_running_pane_processes_for_async_owner(config.take_limit)
            .await
        {
            Ok(processes) => processes,
            Err(error) if is_terminal_pane_supervisor_error(&error) => {
                abort_pane_process_workers(&mut workers).await;
                report.terminal_state = *lifecycle_watcher.borrow();
                return Ok(report);
            }
            Err(error) => return Err(error),
        };
        for (pane_id, process) in processes {
            active_panes.insert(pane_id.clone());
            spawn_owned_pane_process_worker(
                &mut workers,
                handle.clone(),
                pane_id,
                process,
                config.pane_service,
            )?;
            report.spawned_workers = report.spawned_workers.saturating_add(1);
            made_progress = true;
        }

        drain_completed_pane_process_workers(&mut workers, &mut active_panes, &mut report)?;
        if is_terminal_runtime_lifecycle_state(report.terminal_state) {
            drain_completed_pane_process_workers_after_yields(
                &mut workers,
                &mut active_panes,
                &mut report,
            )
            .await?;
            abort_pane_process_workers(&mut workers).await;
            return Ok(report);
        }

        if !made_progress && report.polls < config.max_polls {
            let bounded_idle = (config.max_polls != u64::MAX).then_some(config.idle_interval);
            if let Some(joined) = wait_for_pane_process_supervisor_wakeup(
                &handle,
                &mut workers,
                &mut lifecycle_watcher,
                &mut side_effect_watcher,
                bounded_idle,
            )
            .await
            {
                record_joined_pane_process_worker(joined, &mut active_panes, &mut report)?;
            }
            drain_completed_pane_process_workers(&mut workers, &mut active_panes, &mut report)?;
            if is_terminal_runtime_lifecycle_state(report.terminal_state) {
                abort_pane_process_workers(&mut workers).await;
                return Ok(report);
            }
        }
    }

    report.terminal_state = *lifecycle_watcher.borrow();
    abort_pane_process_workers(&mut workers).await;
    Ok(report)
}

/// Builds the production dynamic pane-process supervisor service.
pub fn build_async_pane_process_supervisor_service(
    name: impl Into<String>,
    handle: AsyncRuntimeSessionHandle,
    config: AsyncPaneProcessSupervisorServiceConfig,
) -> Result<AsyncRuntimeService> {
    config.validate()?;
    Ok(AsyncRuntimeService::new_auxiliary(name, async move {
        let report = run_async_pane_process_supervisor_service(handle, config, |_, state| {
            is_terminal_runtime_lifecycle_state(state)
        })
        .await?;
        let work_units = report
            .spawned_workers
            .saturating_add(report.completed_workers);
        if is_terminal_runtime_lifecycle_state(report.terminal_state) {
            Ok(AsyncRuntimeServiceExit::shutdown(work_units))
        } else {
            Ok(AsyncRuntimeServiceExit::completed(work_units))
        }
    }))
}

/// Drains pane I/O side effects for one pane and executes them through that
/// pane's async driver.
pub async fn run_async_pane_io_side_effect_service<B, F>(
    handle: &AsyncRuntimeSessionHandle,
    driver: &mut AsyncPaneProcessDriver<B>,
    config: AsyncPaneIoSideEffectServiceConfig,
    mut should_stop: F,
) -> Result<AsyncPaneIoSideEffectServiceReport>
where
    B: AsyncPaneProcessIo,
    F: FnMut(u64, RuntimeLifecycleState) -> bool,
{
    config.validate()?;
    let mut lifecycle_watcher = handle.lifecycle_state_watcher();
    let mut side_effect_watcher = handle.side_effect_delivery_watcher();
    let mut pending_pane_io_side_effects = VecDeque::new();
    let mut report = AsyncPaneIoSideEffectServiceReport {
        polls: 0,
        drained: 0,
        submitted_events: 0,
        applied_events: 0,
        terminal_state: *lifecycle_watcher.borrow(),
    };

    while report.polls < config.max_polls {
        let state = *lifecycle_watcher.borrow_and_update();
        report.terminal_state = state;
        if should_stop(report.polls, state) {
            return Ok(report);
        }

        report.polls = report.polls.saturating_add(1);
        let effects = if pending_pane_io_side_effects.is_empty() {
            handle
                .drain_pane_io_side_effects(driver.pane_id().to_string(), config.drain_limit)
                .await?
        } else {
            drain_pending_pane_io_side_effects(
                &mut pending_pane_io_side_effects,
                config.drain_limit,
            )
        };
        if effects.is_empty() {
            if report.polls >= config.max_polls {
                return Ok(report);
            }
            if should_stop(report.polls, state) {
                return Ok(report);
            }
            wait_for_pane_side_effects_or_bounded_idle(
                &mut lifecycle_watcher,
                &mut side_effect_watcher,
                config,
            )
            .await;
            continue;
        }

        report.drained = report
            .drained
            .saturating_add(u64::try_from(effects.len()).unwrap_or(u64::MAX));
        for event in
            pane_io_events_for_side_effects(driver, effects, &mut pending_pane_io_side_effects)
                .await
        {
            let mut batch = RuntimeEventBatch::new();
            batch.push(event);
            let ingress = handle.submit_runtime_events(batch).await?;
            report.submitted_events = report.submitted_events.saturating_add(ingress.accepted);
            report.applied_events = report.applied_events.saturating_add(ingress.applied);
        }
    }

    report.terminal_state = *lifecycle_watcher.borrow();
    Ok(report)
}

/// Runs the wait for pane side effects or bounded idle operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn wait_for_pane_side_effects_or_bounded_idle(
    lifecycle_watcher: &mut watch::Receiver<RuntimeLifecycleState>,
    side_effect_watcher: &mut watch::Receiver<u64>,
    config: AsyncPaneIoSideEffectServiceConfig,
) {
    if config.max_polls == u64::MAX {
        tokio::select! {
            result = side_effect_watcher.changed() => {
                let _ = result;
            }
            result = lifecycle_watcher.changed() => {
                let _ = result;
            }
        }
    } else {
        tokio::select! {
            result = side_effect_watcher.changed() => {
                let _ = result;
            }
            result = lifecycle_watcher.changed() => {
                let _ = result;
            }
            _ = sleep(config.idle_interval) => {}
        }
    }
}

/// Builds an auxiliary service for one pane's side-effect-driven I/O path.
pub fn build_async_pane_io_side_effect_service<B>(
    name: impl Into<String>,
    handle: AsyncRuntimeSessionHandle,
    mut driver: AsyncPaneProcessDriver<B>,
    config: AsyncPaneIoSideEffectServiceConfig,
) -> Result<AsyncRuntimeService>
where
    B: AsyncPaneProcessIo + Send + 'static,
{
    config.validate()?;
    Ok(AsyncRuntimeService::new_auxiliary(name, async move {
        let report =
            run_async_pane_io_side_effect_service(&handle, &mut driver, config, |_, state| {
                is_terminal_runtime_lifecycle_state(state)
            })
            .await?;
        Ok(AsyncRuntimeServiceExit::completed(report.drained))
    }))
}

/// Submits one pane-produced runtime event and accumulates ingress counters.
async fn submit_pane_runtime_event(
    handle: &AsyncRuntimeSessionHandle,
    event: RuntimeEvent,
    submitted_events: &mut usize,
    applied_events: &mut usize,
) -> Result<()> {
    let mut batch = RuntimeEventBatch::new();
    batch.push(event);
    let ingress = handle.submit_runtime_events(batch).await?;
    *submitted_events = submitted_events.saturating_add(ingress.accepted);
    *applied_events = applied_events.saturating_add(ingress.applied);
    Ok(())
}

/// Returns whether an event reports a terminal pane process exit.
fn is_process_exit_event(event: &RuntimeEvent) -> bool {
    matches!(event, RuntimeEvent::Process(ProcessEvent::Exited { .. }))
}

/// Runs the spawn owned pane process worker operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn spawn_owned_pane_process_worker(
    workers: &mut JoinSet<Result<(String, AsyncPaneProcessServiceReport)>>,
    handle: AsyncRuntimeSessionHandle,
    pane_id: String,
    process: super::PaneProcess,
    config: AsyncPaneProcessServiceConfig,
) -> Result<()> {
    let backend = AsyncPtyPaneProcessIo::new(pane_id.clone(), process)?;
    let driver =
        AsyncPaneProcessDriver::new(&pane_id, backend, AsyncPaneProcessDriverConfig::default())?;
    workers.spawn(async move {
        let mut driver = driver;
        let report = run_async_pane_process_service(&handle, &mut driver, config, |_, state| {
            is_terminal_runtime_lifecycle_state(state)
        })
        .await?;
        Ok((pane_id, report))
    });
    Ok(())
}

/// Runs the drain completed pane process workers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn drain_completed_pane_process_workers(
    workers: &mut JoinSet<Result<(String, AsyncPaneProcessServiceReport)>>,
    active_panes: &mut HashSet<String>,
    report: &mut AsyncPaneProcessSupervisorServiceReport,
) -> Result<()> {
    while let Some(joined) = workers.try_join_next() {
        record_joined_pane_process_worker(joined, active_panes, report)?;
    }
    Ok(())
}

/// Runs the drain completed pane process workers after yields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn drain_completed_pane_process_workers_after_yields(
    workers: &mut JoinSet<Result<(String, AsyncPaneProcessServiceReport)>>,
    active_panes: &mut HashSet<String>,
    report: &mut AsyncPaneProcessSupervisorServiceReport,
) -> Result<()> {
    for _ in 0..16 {
        drain_completed_pane_process_workers(workers, active_panes, report)?;
        if workers.is_empty() {
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
    drain_completed_pane_process_workers(workers, active_panes, report)
}

/// Runs the record joined pane process worker operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn record_joined_pane_process_worker(
    joined: std::result::Result<
        Result<(String, AsyncPaneProcessServiceReport)>,
        tokio::task::JoinError,
    >,
    active_panes: &mut HashSet<String>,
    report: &mut AsyncPaneProcessSupervisorServiceReport,
) -> Result<()> {
    match joined {
        Ok(Ok((pane_id, worker_report))) => {
            active_panes.remove(&pane_id);
            report.terminal_state = worker_report.terminal_state;
            report.completed_workers = report.completed_workers.saturating_add(1);
            Ok(())
        }
        Ok(Err(error)) => Err(error),
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(MezError::invalid_state(format!(
            "async pane process worker task failed: {error}"
        ))),
    }
}

/// Runs the wait for pane process supervisor wakeup operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn wait_for_pane_process_supervisor_wakeup(
    handle: &AsyncRuntimeSessionHandle,
    workers: &mut JoinSet<Result<(String, AsyncPaneProcessServiceReport)>>,
    lifecycle_watcher: &mut watch::Receiver<RuntimeLifecycleState>,
    side_effect_watcher: &mut watch::Receiver<u64>,
    bounded_idle: Option<Duration>,
) -> Option<
    std::result::Result<Result<(String, AsyncPaneProcessServiceReport)>, tokio::task::JoinError>,
> {
    match (workers.is_empty(), bounded_idle) {
        (true, Some(idle_interval)) => {
            tokio::select! {
                _ = handle.wait_for_event_delivery() => None,
                result = side_effect_watcher.changed() => {
                    let _ = result;
                    None
                },
                result = lifecycle_watcher.changed() => {
                    let _ = result;
                    None
                },
                _ = sleep(idle_interval) => None,
            }
        }
        (true, None) => {
            tokio::select! {
                _ = handle.wait_for_event_delivery() => None,
                result = side_effect_watcher.changed() => {
                    let _ = result;
                    None
                },
                result = lifecycle_watcher.changed() => {
                    let _ = result;
                    None
                },
            }
        }
        (false, Some(idle_interval)) => {
            tokio::select! {
                biased;
                joined = workers.join_next() => joined,
                _ = handle.wait_for_event_delivery() => None,
                result = side_effect_watcher.changed() => {
                    let _ = result;
                    None
                },
                result = lifecycle_watcher.changed() => {
                    let _ = result;
                    None
                },
                _ = sleep(idle_interval) => None,
            }
        }
        (false, None) => {
            tokio::select! {
                biased;
                joined = workers.join_next() => joined,
                _ = handle.wait_for_event_delivery() => None,
                result = side_effect_watcher.changed() => {
                    let _ = result;
                    None
                },
                result = lifecycle_watcher.changed() => {
                    let _ = result;
                    None
                },
            }
        }
    }
}

/// Runs the abort pane process workers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn abort_pane_process_workers(
    workers: &mut JoinSet<Result<(String, AsyncPaneProcessServiceReport)>>,
) {
    workers.abort_all();
    while workers.join_next().await.is_some() {}
}

/// Runs the is terminal pane supervisor error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn is_terminal_pane_supervisor_error(error: &MezError) -> bool {
    error.kind() == crate::error::MezErrorKind::InvalidState
        && matches!(
            error.message(),
            "runtime service is stopping"
                | "runtime service has already been killed"
                | "runtime service is in a failed lifecycle state"
        )
}

/// Drains locally deferred pane I/O side effects before actor-queued work.
///
/// Locally deferred effects preserve byte order for large input writes that
/// were split across service polls. They must run before newly drained actor
/// effects so a later keystroke cannot overtake a remaining paste chunk.
fn drain_pending_pane_io_side_effects(
    pending: &mut VecDeque<RuntimeSideEffect>,
    limit: usize,
) -> Vec<RuntimeSideEffect> {
    let mut effects = Vec::new();
    while effects.len() < limit {
        let Some(effect) = pending.pop_front() else {
            break;
        };
        effects.push(effect);
    }
    effects
}

/// Runs the pane io events for side effects operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn pane_io_events_for_side_effects<B>(
    driver: &mut AsyncPaneProcessDriver<B>,
    effects: Vec<RuntimeSideEffect>,
    pending: &mut VecDeque<RuntimeSideEffect>,
) -> Vec<RuntimeEvent>
where
    B: AsyncPaneProcessIo,
{
    let mut events = Vec::new();
    let mut effects: VecDeque<_> = effects.into();
    while let Some(effect) = effects.pop_front() {
        let event = match effect {
            RuntimeSideEffect::WritePaneInput { pane_id, bytes }
            | RuntimeSideEffect::WritePaneInputPriority { pane_id, bytes } => {
                if bytes.is_empty() {
                    continue;
                }
                let chunk_len = bytes.len().min(crate::process::PTY_INPUT_WRITE_CHUNK_BYTES);
                let event = driver.write_input_event(&bytes[..chunk_len]).await;
                if let RuntimeEvent::Pane(PaneEvent::InputWritten { bytes: written, .. }) = &event
                    && *written > 0
                    && *written < bytes.len()
                {
                    let existing_pending = std::mem::take(pending);
                    pending.push_back(RuntimeSideEffect::WritePaneInput {
                        pane_id,
                        bytes: bytes[*written..].to_vec(),
                    });
                    pending.extend(effects);
                    pending.extend(existing_pending);
                    events.push(event);
                    break;
                }
                event
            }
            RuntimeSideEffect::ResizePane { size, .. } => driver.resize_event(size).await,
            RuntimeSideEffect::TerminatePane { force, .. } => driver.terminate_event(force).await,
            _ => continue,
        };
        events.push(event);
    }
    events
}

/// Deterministic fake backend for async pane driver tests.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct AsyncFakePaneProcessIo {
    /// Stores the output batches value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    output_batches: VecDeque<Result<Option<Vec<u8>>>>,
    /// Stores the exit results value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    exit_results: VecDeque<Result<Option<ProcessEvent>>>,
    /// Stores the foreground results value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    foreground_results: VecDeque<Result<Option<AsyncPaneForegroundProcess>>>,
    /// Stores the write results value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    write_results: VecDeque<Result<usize>>,
    /// Stores the resize results value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    resize_results: VecDeque<Result<()>>,
    /// Stores the terminate results value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    terminate_results: VecDeque<Result<ProcessEvent>>,
    /// Writes requested by the driver.
    pub writes: Vec<Vec<u8>>,
    /// Resizes requested by the driver.
    pub resizes: Vec<Size>,
    /// Termination requests made by the driver.
    pub terminations: Vec<bool>,
}

#[cfg(test)]
impl AsyncFakePaneProcessIo {
    /// Queues one output chunk.
    pub fn push_output(&mut self, bytes: impl Into<Vec<u8>>) {
        self.output_batches.push_back(Ok(Some(bytes.into())));
    }

    /// Queues an empty output poll.
    pub fn push_no_output(&mut self) {
        self.output_batches.push_back(Ok(None));
    }

    /// Queues one process-exit poll result.
    pub fn push_exit_result(&mut self, result: Result<Option<ProcessEvent>>) {
        self.exit_results.push_back(result);
    }

    /// Queues one foreground process metadata poll result.
    pub fn push_foreground_process_result(
        &mut self,
        result: Result<Option<AsyncPaneForegroundProcess>>,
    ) {
        self.foreground_results.push_back(result);
    }

    /// Queues one output read failure.
    pub fn push_output_error(&mut self, message: impl Into<String>) {
        self.output_batches
            .push_back(Err(MezError::invalid_state(message.into())));
    }

    /// Queues a write result.
    pub fn push_write_result(&mut self, result: Result<usize>) {
        self.write_results.push_back(result);
    }

    /// Queues a resize result.
    pub fn push_resize_result(&mut self, result: Result<()>) {
        self.resize_results.push_back(result);
    }

    /// Queues a termination result.
    pub fn push_terminate_result(&mut self, result: Result<ProcessEvent>) {
        self.terminate_results.push_back(result);
    }
}

#[cfg(test)]
impl AsyncPaneProcessIo for AsyncFakePaneProcessIo {
    /// Runs the read output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_output<'a>(&'a mut self, max_bytes: usize) -> AsyncPaneIoFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            let mut output = self.output_batches.pop_front().unwrap_or(Ok(None))?;
            if let Some(bytes) = output.as_mut() {
                bytes.truncate(max_bytes);
            }
            Ok(output)
        })
    }

    /// Runs the write input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_input<'a>(&'a mut self, bytes: &'a [u8]) -> AsyncPaneIoFuture<'a, usize> {
        Box::pin(async move {
            self.writes.push(bytes.to_vec());
            self.write_results.pop_front().unwrap_or(Ok(bytes.len()))
        })
    }

    /// Runs the poll exit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn poll_exit<'a>(&'a mut self) -> AsyncPaneIoFuture<'a, Option<ProcessEvent>> {
        Box::pin(async move { self.exit_results.pop_front().unwrap_or(Ok(None)) })
    }

    /// Runs the foreground process operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn foreground_process<'a>(
        &'a mut self,
    ) -> AsyncPaneIoFuture<'a, Option<AsyncPaneForegroundProcess>> {
        Box::pin(async move { self.foreground_results.pop_front().unwrap_or(Ok(None)) })
    }

    /// Runs the resize operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn resize<'a>(&'a mut self, size: Size) -> AsyncPaneIoFuture<'a, ()> {
        Box::pin(async move {
            self.resizes.push(size);
            self.resize_results.pop_front().unwrap_or(Ok(()))
        })
    }

    /// Runs the terminate operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn terminate<'a>(&'a mut self, force: bool) -> AsyncPaneIoFuture<'a, ProcessEvent> {
        Box::pin(async move {
            self.terminations.push(force);
            self.terminate_results.pop_front().unwrap_or_else(|| {
                Ok(ProcessEvent::Exited {
                    pane_id: String::new(),
                    exit_code: None,
                    signal: Some(if force { "killed" } else { "terminated" }.to_string()),
                })
            })
        })
    }
}
