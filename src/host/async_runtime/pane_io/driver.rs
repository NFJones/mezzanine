//! Generic async pane backend contract and per-pane event driver.

use super::{MezError, PaneEvent, ProcessEvent, Result, RuntimeEvent, Size};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

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
    #[cfg(test)]
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
