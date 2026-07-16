//! Runtime pane I/O state and pipe workers.
//!
//! This module owns the data records and helper workers that connect live pane
//! processes to runtime state updates. It intentionally stays focused on typed
//! pane process, resize, input, output, exit, and pipe records so the central
//! runtime service can coordinate those records without also owning the pipe
//! implementation details.

use super::service_state::RuntimeRegistryUpdatePlan;
use super::{
    File, MezError, OpenOptions, PaneExitStatus, Path, PathBuf, Result, Size, Stdio, Write,
};
use tokio::io::AsyncWriteExt;

/// Bounded number of pending output chunks accepted by command-backed pane pipes.
const COMMAND_PANE_PIPE_QUEUE_CAPACITY: usize = 256;
/// Maximum time to wait for a pipe command to exit after its stdin closes.
const COMMAND_PANE_PIPE_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

/// Carries Pane Process Start state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneProcessStart {
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_id: String,
    /// Stores the window id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_id: String,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the primary pid value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub primary_pid: u32,
    /// Stores the size value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub size: Size,
    /// Stores the registry update value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub registry_update: RuntimeRegistryUpdatePlan,
}

/// Carries Pane Resize Update state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneResizeUpdate {
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_id: String,
    /// Stores the window id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_id: String,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the primary pid value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub primary_pid: u32,
    /// Stores the size value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub size: Size,
    /// Stores the registry update value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub registry_update: RuntimeRegistryUpdatePlan,
}

/// Carries Pane Input Dispatch state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneInputDispatch {
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_id: String,
    /// Stores the window id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_id: String,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the primary pid value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub primary_pid: u32,
    /// Stores the bytes written value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub bytes_written: usize,
}

/// Carries Pane Output Update state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneOutputUpdate {
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_id: String,
    /// Stores the window id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_id: String,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the primary pid value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub primary_pid: u32,
    /// Stores the bytes read value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub bytes_read: usize,
    /// Stores the activity events value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub activity_events: u64,
    /// Stores the bell events value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub bell_events: u64,
    /// Stores the background value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub background: bool,
    /// Stores whether this output requires retained attached-terminal frame state
    /// to be discarded before the next redraw.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub invalidate_output_frame: bool,
}

/// Carries Active Pane Pipe state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
pub(super) struct ActivePanePipe {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_id: String,
    /// Stores the target value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) target: ActivePanePipeTarget,
    /// Stores the bytes written value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) bytes_written: usize,
}

/// Carries Active Pane Pipe Target state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
pub(super) enum ActivePanePipeTarget {
    /// Represents the File case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    File {
        /// Stores the path value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        path: PathBuf,
        /// Stores the file value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        file: Option<File>,
    },
    /// Represents the Command case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Command {
        /// Stores the command value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        command: String,
        /// Stores the writer value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        writer: CommandPanePipeWriter,
    },
}

/// Bounded Tokio-backed writer for command-backed pane pipes.
///
/// The runtime state keeps only the bounded sender and worker status. The pipe
/// command process and stdin are owned by a small Tokio runtime so pane-output
/// application does not block on the pipe command reading from stdin.
#[derive(Debug)]
pub(super) struct CommandPanePipeWriter {
    /// Stores the sender value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    sender: tokio::sync::mpsc::Sender<Vec<u8>>,
    /// Stores the status value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    status: CommandPanePipeWorkerState,
}

/// Carries Command Pane Pipe Worker Status state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Default)]
struct CommandPanePipeWorkerStatus {
    /// Stores the completed value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    completed: bool,
    /// Stores the failure value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    failure: Option<String>,
}

/// Defines the Command Pane Pipe Worker State type used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
type CommandPanePipeWorkerState = std::sync::Arc<std::sync::Mutex<CommandPanePipeWorkerStatus>>;

/// Carries Command Pane Pipe Status Snapshot state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommandPanePipeStatusSnapshot {
    /// Stores the completed value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) completed: bool,
    /// Stores the failure value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) failure: Option<String>,
}

/// Carries Stopped Pane Pipe state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoppedPanePipe {
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) pane_id: String,
    /// Stores the mode value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) mode: &'static str,
    /// Stores the target value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) target: String,
    /// Stores the bytes written value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) bytes_written: usize,
    /// Stores the failure value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) failure: Option<String>,
}

impl ActivePanePipe {
    /// Runs the file operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn file(pane_id: String, path: PathBuf) -> Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            pane_id,
            target: ActivePanePipeTarget::File {
                path,
                file: Some(file),
            },
            bytes_written: 0,
        })
    }

    /// Runs the deferred file operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn deferred_file(pane_id: String, path: PathBuf) -> Self {
        Self {
            pane_id,
            target: ActivePanePipeTarget::File { path, file: None },
            bytes_written: 0,
        }
    }

    /// Runs the command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn command(pane_id: String, shell_path: &Path, command: String) -> Result<Self> {
        Ok(Self {
            pane_id,
            target: ActivePanePipeTarget::Command {
                writer: CommandPanePipeWriter::spawn(shell_path, command.clone())?,
                command,
            },
            bytes_written: 0,
        })
    }

    /// Runs the deferred command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn deferred_command(
        pane_id: String,
        shell_path: &Path,
        command: String,
    ) -> Result<Self> {
        Ok(Self {
            pane_id,
            target: ActivePanePipeTarget::Command {
                writer: CommandPanePipeWriter::spawn_without_startup_wait(
                    shell_path,
                    command.clone(),
                )?,
                command,
            },
            bytes_written: 0,
        })
    }

    /// Runs the write output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn write_output(&mut self, bytes: &[u8]) -> Result<()> {
        match &mut self.target {
            ActivePanePipeTarget::File {
                file: Some(file), ..
            } => file.write_all(bytes)?,
            ActivePanePipeTarget::File { file: None, .. } => {
                return Err(MezError::invalid_state(
                    "deferred file pane pipe cannot write inline",
                ));
            }
            ActivePanePipeTarget::Command { writer, .. } => writer.write(bytes)?,
        }
        self.bytes_written = self.bytes_written.saturating_add(bytes.len());
        Ok(())
    }

    /// Runs the file target path operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn file_target_path(&self) -> Option<PathBuf> {
        match &self.target {
            ActivePanePipeTarget::File { path, .. } => Some(path.clone()),
            ActivePanePipeTarget::Command { .. } => None,
        }
    }

    /// Runs the record deferred output operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn record_deferred_output(&mut self, bytes: usize) {
        self.bytes_written = self.bytes_written.saturating_add(bytes);
    }

    /// Runs the mode operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn mode(&self) -> &'static str {
        match &self.target {
            ActivePanePipeTarget::File { .. } => "file",
            ActivePanePipeTarget::Command { .. } => "command",
        }
    }

    /// Runs the target label operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn target_label(&self) -> String {
        match &self.target {
            ActivePanePipeTarget::File { path, .. } => path.display().to_string(),
            ActivePanePipeTarget::Command { command, .. } => command.clone(),
        }
    }

    /// Runs the stop operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn stop(self) -> StoppedPanePipe {
        let mode = self.mode();
        let target = self.target_label();
        let failure = match self.target {
            ActivePanePipeTarget::Command { writer, .. } => writer.close(),
            ActivePanePipeTarget::File { .. } => None,
        };
        StoppedPanePipe {
            pane_id: self.pane_id,
            mode,
            target,
            bytes_written: self.bytes_written,
            failure,
        }
    }

    /// Runs the command status operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn command_status(&self) -> Result<Option<CommandPanePipeStatusSnapshot>> {
        match &self.target {
            ActivePanePipeTarget::Command { writer, .. } => writer.status().map(Some),
            ActivePanePipeTarget::File { .. } => Ok(None),
        }
    }
}

impl CommandPanePipeWriter {
    /// Runs the spawn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn spawn(shell_path: &Path, command: String) -> Result<Self> {
        Self::spawn_async(shell_path, command)
    }

    /// Runs the spawn without startup wait operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn spawn_without_startup_wait(shell_path: &Path, command: String) -> Result<Self> {
        Self::spawn_async(shell_path, command)
    }

    /// Runs the spawn async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn spawn_async(shell_path: &Path, command: String) -> Result<Self> {
        let (sender, receiver) = tokio::sync::mpsc::channel(COMMAND_PANE_PIPE_QUEUE_CAPACITY);
        let status =
            std::sync::Arc::new(std::sync::Mutex::new(CommandPanePipeWorkerStatus::default()));
        let worker_status = status.clone();
        let shell_path = shell_path.to_path_buf();
        let worker_command = command.clone();
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            MezError::invalid_state("command pane pipe requires an active Tokio runtime")
        })?;
        handle.spawn(run_command_pane_pipe_writer_async(
            shell_path,
            worker_command,
            receiver,
            worker_status,
        ));
        Ok(Self { sender, status })
    }

    /// Runs the write operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write(&self, bytes: &[u8]) -> Result<()> {
        let status = self.status()?;
        if let Some(failure) = status.failure {
            return Err(MezError::invalid_state(format!(
                "pipe command writer failed: {failure}"
            )));
        }
        if status.completed {
            return Err(MezError::invalid_state(
                "pipe command writer completed before accepting output",
            ));
        }
        self.sender.try_send(bytes.to_vec()).map_err(|error| {
            let message = match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    "pipe command writer queue is full".to_string()
                }
                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                    "pipe command writer is closed".to_string()
                }
            };
            MezError::invalid_state(message)
        })?;
        let status = self.status()?;
        if let Some(failure) = status.failure {
            return Err(MezError::invalid_state(format!(
                "pipe command writer failed: {failure}"
            )));
        }
        Ok(())
    }

    /// Runs the close operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn close(self) -> Option<String> {
        let failure = self.status().ok().and_then(|status| status.failure);
        drop(self.sender);
        failure
    }

    /// Runs the status operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn status(&self) -> Result<CommandPanePipeStatusSnapshot> {
        self.status
            .lock()
            .map(|status| CommandPanePipeStatusSnapshot {
                completed: status.completed,
                failure: status.failure.clone(),
            })
            .map_err(|_| MezError::invalid_state("pipe command writer status lock is poisoned"))
    }
}

/// Runs the run command pane pipe writer async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn run_command_pane_pipe_writer_async(
    shell_path: PathBuf,
    command: String,
    mut receiver: tokio::sync::mpsc::Receiver<Vec<u8>>,
    status: CommandPanePipeWorkerState,
) {
    let mut child = match tokio::process::Command::new(&shell_path)
        .arg("-c")
        .arg(command.as_str())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let message = format!("failed to spawn pipe command `{command}`: {error}");
            record_command_pane_pipe_failure(&status, message.clone());
            mark_command_pane_pipe_completed(&status);
            return;
        }
    };
    let mut stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            let message = "pipe command stdin was not captured".to_string();
            record_command_pane_pipe_failure(&status, message.clone());
            mark_command_pane_pipe_completed(&status);
            return;
        }
    };

    loop {
        tokio::select! {
            child_status = child.wait() => {
                record_command_pane_pipe_child_status(&status, child_status);
                mark_command_pane_pipe_completed(&status);
                return;
            }
            maybe_bytes = receiver.recv() => {
                let Some(bytes) = maybe_bytes else {
                    drop(stdin);
                    wait_for_command_pane_pipe_child(&mut child, &status).await;
                    mark_command_pane_pipe_completed(&status);
                    return;
                };
                if let Err(error) = stdin.write_all(&bytes).await {
                    record_command_pane_pipe_failure(&status, format!("stdin write failed: {error}"));
                    drop(stdin);
                    wait_for_command_pane_pipe_child(&mut child, &status).await;
                    mark_command_pane_pipe_completed(&status);
                    return;
                }
            }
        }
    }
}

/// Runs the wait for command pane pipe child operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn wait_for_command_pane_pipe_child(
    child: &mut tokio::process::Child,
    status: &CommandPanePipeWorkerState,
) {
    match tokio::time::timeout(COMMAND_PANE_PIPE_CLOSE_TIMEOUT, child.wait()).await {
        Ok(child_status) => record_command_pane_pipe_child_status(status, child_status),
        Err(_) => {
            record_command_pane_pipe_failure(status, command_pane_pipe_close_timeout_message());
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

/// Runs the record command pane pipe child status operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn record_command_pane_pipe_child_status(
    status: &CommandPanePipeWorkerState,
    child_status: std::io::Result<std::process::ExitStatus>,
) {
    match child_status {
        Ok(exit_status) if exit_status.success() => {}
        Ok(exit_status) => {
            record_command_pane_pipe_failure(status, format!("child exited with {exit_status}"));
        }
        Err(error) => {
            record_command_pane_pipe_failure(status, format!("child status check failed: {error}"));
        }
    }
}

/// Runs the mark command pane pipe completed operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mark_command_pane_pipe_completed(status: &CommandPanePipeWorkerState) {
    if let Ok(mut status) = status.lock() {
        status.completed = true;
    }
}

/// Runs the record command pane pipe failure operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn record_command_pane_pipe_failure(status: &CommandPanePipeWorkerState, message: String) {
    if let Ok(mut status) = status.lock() {
        status.failure.get_or_insert(message);
    }
}

/// Runs the command pane pipe close timeout message operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn command_pane_pipe_close_timeout_message() -> String {
    format!(
        "child did not exit within {}ms after pipe close",
        COMMAND_PANE_PIPE_CLOSE_TIMEOUT.as_millis()
    )
}

/// Carries Pane Exit Update state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneExitUpdate {
    /// Stores the session id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_id: String,
    /// Stores the window id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub window_id: String,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the primary pid value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub primary_pid: u32,
    /// Stores the exit status value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub exit_status: PaneExitStatus,
    /// Stores the closed window value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub closed_window: bool,
    /// Stores the session empty value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub session_empty: bool,
    /// Stores the registry update value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub registry_update: RuntimeRegistryUpdatePlan,
}

/// Carries Pane Exit Record state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PaneExitRecord {
    /// Stores the exit status value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub exit_status: PaneExitStatus,
}
