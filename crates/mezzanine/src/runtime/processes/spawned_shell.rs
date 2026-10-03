//! Spawned shell executor for native shell mode.
//!
//! Native shell mode executes agent actions in a fresh shell process derived
//! from pane root-process metadata. This module owns that spawn: it
//! materializes the transaction command to a temporary file, runs the shell
//! (or an explicit typed child launch) with the inferred environment and
//! working directory, captures stdout and stderr through pipes within a
//! bounded budget, and kills the whole child process group on timeout or
//! interruption. Nothing is written to or read from the pane PTY.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mez_agent::SpawnedShellExecutor as SpawnedShellExecutorPort;
use mez_agent::semantic_patch_planning::{
    ApplyPatchProgressDecoder, ApplyPatchTransactionPhase, apply_patch_transaction_phase,
};
use mez_agent::{
    DEFAULT_AGENT_TURN_TIMEOUT_MS, ShellChildArgument, ShellExecutionOutput, ShellExecutionRequest,
    ShellTransaction, ShellTransportDiagnostics,
};

use crate::error::{MezError, Result};
use crate::runtime::status_pills::{
    RuntimePaneStatusProviderEvent, RuntimePaneStatusProviderLaunch,
    RuntimePaneStatusProviderOutcome, RuntimePaneStatusProviderRefreshPlan,
    STATUS_PILL_OUTPUT_LIMIT_BYTES, runtime_status_pill_normalize_output,
};

use super::launch_accounting::{NativeLaunchLedger, NativeLaunchReason};
use super::native_shell_inference::NativeShellContext;

/// Minimum captured bytes retained per stream even when the transaction
/// output budget is small.
const MIN_STREAM_CAPTURE_BUDGET: usize = 4096;
/// Maximum captured bytes retained per stream regardless of transaction
/// budget; reads continue past the cap but bytes are dropped and counted.
const SPAWNED_CHILD_CAPTURE_HARD_CAP: usize = 16 * 1024 * 1024;
/// Maximum trusted lifecycle-status bytes accepted from a typed child.
const SPAWNED_CHILD_STATUS_CAPTURE_LIMIT: usize = 64 * 1024;
/// Poll interval while waiting for the spawned child to exit.
const SPAWNED_CHILD_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Grace period for draining bytes already buffered when the direct child exits.
const SPAWNED_CHILD_READER_SHUTDOWN_GRACE: Duration = Duration::from_millis(100);
/// Maximum cumulative output retained for transient native-shell progress.
const SPAWNED_CHILD_PROGRESS_PREVIEW_LIMIT: usize = 256 * 1024;

/// Creates a status pipe whose original descriptors never leak through exec.
///
/// Darwin does not implement `pipe2`, so Rustix intentionally does not expose
/// `pipe_with` there. Set `FD_CLOEXEC` on both ends after creating the POSIX
/// pipe; the child pre-exec hook duplicates the writer to its requested status
/// descriptor and explicitly clears that duplicate's close-on-exec flag.
fn status_pipe() -> Result<(OwnedFd, OwnedFd)> {
    let (reader, writer) = rustix::pipe::pipe().map_err(|error| {
        MezError::invalid_state(format!(
            "spawned shell execution could not create its status pipe: {error}"
        ))
    })?;
    for descriptor in [&reader, &writer] {
        rustix::io::fcntl_setfd(descriptor, rustix::io::FdFlags::CLOEXEC).map_err(|error| {
            MezError::invalid_state(format!(
                "spawned shell execution could not set close-on-exec on its status pipe: {error}"
            ))
        })?;
    }
    Ok((reader, writer))
}

/// Mutable cumulative preview and revision shared by output reader threads.
struct SpawnedChildProgressState {
    /// Bounded cumulative output retained for the next publication.
    preview: Vec<u8>,
    /// Strictly increasing revision assigned under the same output lock.
    revision: u64,
    /// Decoder retained only for one native semantic-patch write transaction.
    patch_decoder: Option<ApplyPatchProgressDecoder>,
    /// Confirmed write sections retained across coalesced watch publications.
    confirmed_patch_sections: Vec<mez_agent::semantic_patch_planning::ApplyPatchConfirmedSection>,
}

/// Shared latest-value relay used by stdout and stderr reader threads.
#[derive(Clone)]
struct SpawnedChildProgressReporter {
    state: Arc<Mutex<SpawnedChildProgressState>>,
    sender: tokio::sync::watch::Sender<Option<crate::runtime::RuntimeNativeShellWorkerProgress>>,
}

impl SpawnedChildProgressReporter {
    /// Appends one observed chunk and publishes the newest revisioned preview.
    fn report(&self, stream: SpawnedChildPipe, bytes: &[u8]) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.preview.extend_from_slice(bytes);
        if state.preview.len() > SPAWNED_CHILD_PROGRESS_PREVIEW_LIMIT {
            let excess = state.preview.len() - SPAWNED_CHILD_PROGRESS_PREVIEW_LIMIT;
            state.preview.drain(..excess);
        }
        state.revision = state.revision.saturating_add(1);
        if stream == SpawnedChildPipe::Stdout
            && let Some(decoder) = state.patch_decoder.as_mut()
            && let Ok(progress) = decoder.push(bytes)
        {
            state
                .confirmed_patch_sections
                .extend(progress.confirmed_sections);
        }
        let snapshot = crate::runtime::RuntimeNativeShellWorkerProgress {
            output: Some((
                state.revision,
                String::from_utf8_lossy(&state.preview).into_owned(),
            )),
            confirmed_patch_sections: state.confirmed_patch_sections.clone(),
        };
        self.sender.send_replace(Some(snapshot));
    }
}

/// Identifies one pipe drained from the spawned child process tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpawnedChildPipe {
    /// Captured standard output.
    Stdout,
    /// Captured standard error.
    Stderr,
    /// Trusted typed-child lifecycle status.
    Status,
}

impl SpawnedChildPipe {
    /// Returns the stable stream label used in failure diagnostics.
    const fn label(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Status => "status",
        }
    }
}

/// Bounded bytes captured from one child pipe.
struct CapturedPipeOutput {
    /// Bytes retained up to the configured stream budget.
    bytes: Vec<u8>,
    /// Bytes discarded after the retained budget was exhausted.
    dropped: usize,
}

/// Cancelable reader and its completion notification state.
struct SpawnedChildPipeReader {
    /// Pipe identity used for diagnostics and completion tracking.
    stream: SpawnedChildPipe,
    /// Requests that a nonblocking reader stop waiting for inherited writers.
    cancel: mpsc::Sender<()>,
    /// Reader thread joined only after EOF or cancellation.
    handle: JoinHandle<Result<CapturedPipeOutput>>,
    /// Whether the reader reported completion through the shared channel.
    completed: bool,
}

/// Spawned process plus its optional runtime-owned lifecycle-status reader.
struct SpawnedChild {
    /// Running direct child process.
    child: Child,
    /// Read end of the descriptor selected by `ShellChildLaunch::status_fd`.
    status_reader: Option<OwnedFd>,
}

/// Owner-only command and artifact files prepared for one native launch.
struct MaterializedShellLaunch {
    /// Canonical private directory removed after every completion path.
    directory: PathBuf,
    /// Canonical command-file path substituted into typed argv.
    command_file: PathBuf,
    /// Canonical artifact paths keyed by validated launch ids.
    artifacts: BTreeMap<mez_agent::ShellLaunchArtifactId, PathBuf>,
}

impl MaterializedShellLaunch {
    /// Removes every launch file through its private owner directory.
    fn cleanup(&self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// Creates one new owner-only launch file and writes its inert bytes.
fn write_owner_only_file(path: &Path, content: &[u8], mode: u32) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    file.write_all(content)?;
    file.sync_all()?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

/// Returns one validated materialized artifact path from the native launch set.
fn materialized_artifact_path<'a>(
    materialized: &'a MaterializedShellLaunch,
    artifact: &mez_agent::ShellLaunchArtifactId,
) -> Result<&'a Path> {
    materialized
        .artifacts
        .get(artifact)
        .map(PathBuf::as_path)
        .ok_or_else(|| MezError::invalid_state("typed child launch artifact was not materialized"))
}

/// Executes one shell transaction in a freshly spawned shell process.
pub(crate) struct SpawnedShellExecutor {
    /// Inferred shell path, environment, and working directory.
    context: NativeShellContext,
    /// Interruption flag shared with the interrupt handle.
    interrupted: Arc<AtomicBool>,
    /// Explicit per-workload direct-launch evidence and admission policy.
    launches: NativeLaunchLedger,
    /// Semantic purpose selected by the actor, not command-string heuristics.
    launch_reason: NativeLaunchReason,
}

impl SpawnedShellExecutor {
    /// Builds an executor around one inferred native shell context.
    #[cfg(test)]
    pub(crate) fn new(context: NativeShellContext) -> Self {
        Self {
            context,
            interrupted: Arc::new(AtomicBool::new(false)),
            launches: NativeLaunchLedger::new(false),
            launch_reason: NativeLaunchReason::ShellCommand,
        }
    }

    /// Builds an executor with a caller-owned lifecycle cancellation fence.
    fn with_interrupted(context: NativeShellContext, interrupted: Arc<AtomicBool>) -> Self {
        Self {
            context,
            interrupted,
            launches: NativeLaunchLedger::new(false),
            launch_reason: NativeLaunchReason::StatusProvider,
        }
    }

    /// Returns a cancellation handle for the next execution.
    #[cfg(test)]
    pub(crate) fn interrupt_handle(&self) -> SpawnedShellInterrupt {
        SpawnedShellInterrupt {
            flag: Arc::clone(&self.interrupted),
        }
    }

    /// Line prefix appended to materialized apply-patch sidecar records.
    ///
    /// The generated write-phase script extracts final-content records with
    /// `sed -n 's/^# __MEZ_INPUT_SIDECAR_V1__ <index> //p'` from `$0`, so the
    /// spawned command file must carry exactly the same prefixed lines as the
    /// pane transport's sidecar-frame appender.
    const INPUT_SIDECAR_LINE_PREFIX: &str = "# __MEZ_INPUT_SIDECAR_V1__ ";

    /// Materializes one owner-only command file and bounded artifact set.
    fn materialize_launch(
        &self,
        transaction: &ShellTransaction,
    ) -> Result<MaterializedShellLaunch> {
        static NEXT_LAUNCH_DIRECTORY_ID: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let sequence = NEXT_LAUNCH_DIRECTORY_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "mez-spawned-{}-{unique}-{sequence}",
            std::process::id()
        ));
        let mut directory_builder = fs::DirBuilder::new();
        directory_builder.mode(0o700);
        directory_builder.create(&directory).map_err(|error| {
            MezError::invalid_state(format!(
                "spawned shell execution could not create its private launch directory: {error}"
            ))
        })?;
        let result = self.materialize_launch_in_directory(transaction, &directory);
        if result.is_err() {
            let _ = fs::remove_dir_all(&directory);
        }
        result
    }

    /// Writes launch files below one newly created private directory.
    fn materialize_launch_in_directory(
        &self,
        transaction: &ShellTransaction,
        directory: &Path,
    ) -> Result<MaterializedShellLaunch> {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(|error| {
            MezError::invalid_state(format!(
                "spawned shell execution could not protect its launch directory: {error}"
            ))
        })?;
        let directory = fs::canonicalize(directory).map_err(|error| {
            MezError::invalid_state(format!(
                "spawned shell execution could not canonicalize its launch directory: {error}"
            ))
        })?;
        let command_file = directory.join("command");
        let mut content = transaction.command.clone();
        if !content.ends_with('\n') {
            content.push('\n');
        }
        if let Some(sidecar) = transaction.input_sidecar() {
            for line in sidecar.lines() {
                content.push_str(Self::INPUT_SIDECAR_LINE_PREFIX);
                content.push_str(line);
                content.push('\n');
            }
        }
        write_owner_only_file(&command_file, content.as_bytes(), 0o600).map_err(|error| {
            MezError::invalid_state(format!(
                "spawned shell execution could not materialize its command file: {error}"
            ))
        })?;
        let command_file = fs::canonicalize(&command_file).map_err(|error| {
            MezError::invalid_state(format!(
                "spawned shell execution could not canonicalize its command file: {error}"
            ))
        })?;
        let mut artifacts = BTreeMap::new();
        if let Some(launch) = &transaction.child_launch {
            for (index, artifact) in launch.artifacts.iter().enumerate() {
                let path = directory.join(index.to_string());
                write_owner_only_file(&path, &artifact.content, artifact.mode).map_err(|error| {
                    MezError::invalid_state(format!(
                        "spawned shell execution could not materialize a launch artifact: {error}"
                    ))
                })?;
                let path = fs::canonicalize(&path).map_err(|error| {
                    MezError::invalid_state(format!(
                        "spawned shell execution could not canonicalize a launch artifact: {error}"
                    ))
                })?;
                artifacts.insert(artifact.id.clone(), path);
            }
        }
        Ok(MaterializedShellLaunch {
            directory,
            command_file,
            artifacts,
        })
    }

    /// Spawns the shell (or typed child launch) with the inferred context.
    fn spawn_child(
        &self,
        transaction: &ShellTransaction,
        materialized: &MaterializedShellLaunch,
    ) -> Result<SpawnedChild> {
        let mut command = if let Some(launch) = &transaction.child_launch {
            let mut command = Command::new(&launch.executable);
            for argument in &launch.arguments {
                match argument {
                    ShellChildArgument::Literal(value) => {
                        command.arg(value);
                    }
                    ShellChildArgument::MaterializedCommandFile => {
                        command.arg(&materialized.command_file);
                    }
                    ShellChildArgument::MaterializedArtifact(artifact) => {
                        command.arg(materialized_artifact_path(materialized, artifact)?);
                    }
                    ShellChildArgument::MaterializedPathBinding { name, artifact } => {
                        let path = materialized_artifact_path(materialized, artifact)?;
                        command.arg(format!("{name}={}", path.to_string_lossy()));
                    }
                }
            }
            command
        } else {
            let mut command = Command::new(self.context.shell_path());
            command.arg(&materialized.command_file);
            command
        };
        command
            .current_dir(self.context.working_directory())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        // Native launches never inherit the ambient `mez` environment. The
        // composed native workload environment is the only source of child
        // entries: validated pane-root evidence plus the narrowly enumerated
        // runtime requirements for a workload, or the launcher/control bucket
        // for a code-owned sandbox launcher.
        command.env_clear();
        for entry in self.context.launch_environment() {
            command.env(
                OsStr::from_bytes(&entry.key),
                OsStr::from_bytes(&entry.value),
            );
        }
        let (status_reader, status_writer) = if let Some(status_fd) = transaction
            .child_launch
            .as_ref()
            .and_then(|launch| launch.status_fd)
        {
            let (reader, writer) = status_pipe()?;
            let writer_fd = writer.as_raw_fd();
            let target_fd = i32::from(status_fd);
            // SAFETY: the closure performs only async-signal-safe descriptor
            // operations between fork and exec. `writer` remains alive until
            // `spawn` returns, and the duplicated target has CLOEXEC cleared.
            unsafe {
                command.pre_exec(move || {
                    if libc::dup2(writer_fd, target_fd) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::fcntl(target_fd, libc::F_SETFD, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            (Some(reader), Some(writer))
        } else {
            (None, None)
        };
        let child = self.launches.launch(self.launch_reason, || {
            if self.interrupted.load(Ordering::SeqCst) {
                return Err(MezError::conflict(
                    "native shell dispatch was cancelled before launch",
                ));
            }
            command.spawn().map_err(|error| {
                MezError::invalid_state(format!("spawned shell execution failed to start: {error}"))
            })
        })?;
        drop(status_writer);
        Ok(SpawnedChild {
            child,
            status_reader,
        })
    }

    /// Waits for the child, enforcing the deadline and interruption flag,
    /// then joins the capture readers into normalized shell output.
    fn collect(
        &self,
        spawned: SpawnedChild,
        timeout_ms: Option<u64>,
        output_budget: usize,
        progress: Option<SpawnedChildProgressReporter>,
        sandbox_backend: Option<crate::runtime::SandboxBackend>,
    ) -> Result<ShellExecutionOutput> {
        let SpawnedChild {
            mut child,
            status_reader,
        } = spawned;
        let stdout = child.stdout.take().ok_or_else(|| {
            MezError::invalid_state("spawned shell execution lost its stdout pipe")
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            MezError::invalid_state("spawned shell execution lost its stderr pipe")
        })?;
        let stream_budget =
            (output_budget / 2).clamp(MIN_STREAM_CAPTURE_BUDGET, SPAWNED_CHILD_CAPTURE_HARD_CAP);
        let (done_tx, done_rx) = mpsc::channel();
        let mut stdout_reader = spawn_output_reader(
            SpawnedChildPipe::Stdout,
            stdout,
            stream_budget,
            done_tx.clone(),
            progress.clone(),
        );
        let mut stderr_reader = spawn_output_reader(
            SpawnedChildPipe::Stderr,
            stderr,
            stream_budget,
            done_tx.clone(),
            progress,
        );
        let mut status_reader = status_reader.map(|reader| {
            spawn_output_reader(
                SpawnedChildPipe::Status,
                std::fs::File::from(reader),
                SPAWNED_CHILD_STATUS_CAPTURE_LIMIT,
                done_tx,
                None,
            )
        });
        let pid = child.id() as i32;
        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_AGENT_TURN_TIMEOUT_MS).max(1);
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut exit_status = None;
        let mut timed_out = false;
        let mut interrupted = false;
        loop {
            if self.interrupted.load(Ordering::SeqCst) {
                interrupted = true;
                kill_process_group(pid);
                break;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                kill_process_group(pid);
                break;
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    exit_status = Some(status);
                    break;
                }
                Ok(None) => std::thread::sleep(SPAWNED_CHILD_POLL_INTERVAL),
                Err(error) => {
                    return Err(MezError::invalid_state(format!(
                        "spawned shell execution wait failed: {error}"
                    )));
                }
            }
        }
        if exit_status.is_none() {
            exit_status = Some(child.wait().map_err(|error| {
                MezError::invalid_state(format!("spawned shell execution reap failed: {error}"))
            })?);
        }
        let status = exit_status.ok_or_else(|| {
            MezError::invalid_state("spawned shell execution observed no exit status")
        })?;
        drain_reader_completions(
            &done_rx,
            &mut stdout_reader,
            &mut stderr_reader,
            status_reader.as_mut(),
        );
        let drain_deadline = Instant::now() + SPAWNED_CHILD_READER_SHUTDOWN_GRACE;
        while readers_are_pending(&stdout_reader, &stderr_reader, status_reader.as_ref()) {
            let now = Instant::now();
            if now >= drain_deadline {
                break;
            }
            let wait =
                SPAWNED_CHILD_POLL_INTERVAL.min(drain_deadline.saturating_duration_since(now));
            wait_for_reader_completion(
                &done_rx,
                wait,
                &mut stdout_reader,
                &mut stderr_reader,
                status_reader.as_mut(),
            );
        }
        cancel_pending_reader(&stdout_reader);
        cancel_pending_reader(&stderr_reader);
        if let Some(reader) = status_reader.as_ref() {
            cancel_pending_reader(reader);
        }
        let stdout_capture = join_output_reader(stdout_reader)?;
        let stderr_capture = join_output_reader(stderr_reader)?;
        let output = ShellExecutionOutput {
            exit_code: if timed_out || interrupted {
                None
            } else {
                status.code()
            },
            signal: status.signal(),
            stdout: String::from_utf8_lossy(&stdout_capture.bytes).into_owned(),
            stderr: String::from_utf8_lossy(&stderr_capture.bytes).into_owned(),
            timed_out,
            interrupted,
            transport_diagnostics: ShellTransportDiagnostics {
                output_bytes_dropped: stdout_capture
                    .dropped
                    .saturating_add(stderr_capture.dropped),
                ..ShellTransportDiagnostics::default()
            },
        };
        if let Some(status_reader) = status_reader {
            let backend = sandbox_backend.unwrap_or(crate::runtime::SandboxBackend::Bubblewrap);
            let status_complete = status_reader.completed;
            let status_capture = join_output_reader(status_reader).map_err(|_| {
                spawned_lifecycle_error(
                    backend,
                    crate::security::sandbox::SandboxLifecycleFailureClass::Transport,
                    None,
                    &output,
                    stderr_capture.dropped,
                )
            })?;
            validate_spawned_child_status(
                backend,
                &status_capture.bytes,
                status_capture.dropped,
                status_complete,
                &output,
                stderr_capture.dropped,
            )?;
        }
        Ok(output)
    }
}

impl SpawnedShellExecutorPort for SpawnedShellExecutor {
    type Error = MezError;

    /// Executes one shell transaction in a spawned shell process and returns
    /// normalized output.
    fn execute_shell(&mut self, request: &ShellExecutionRequest) -> Result<ShellExecutionOutput> {
        if request.interactive || request.stateful {
            return Err(MezError::invalid_args(
                "native transport does not serve stateful or interactive execution",
            ));
        }
        self.interrupted.store(false, Ordering::SeqCst);
        let materialized = self.materialize_launch(&request.transaction)?;
        let output = self
            .spawn_child(&request.transaction, &materialized)
            .and_then(|child| {
                self.collect(
                    child,
                    request.timeout_ms,
                    request.transaction.output_max_raw_bytes,
                    None,
                    None,
                )
            });
        materialized.cleanup();
        output
    }
}

/// Executes one actor-authorized native shell dispatch on an external worker.
///
/// The returned outcome retains the exact turn, action, and marker fence so
/// serialized runtime state can reject stale completion after cancellation or
/// turn replacement without trusting worker timing.
#[cfg(test)]
pub(crate) fn execute_native_shell_dispatch(
    dispatch: crate::runtime::RuntimeNativeShellDispatch,
) -> crate::runtime::RuntimeNativeShellOutcome {
    execute_native_shell_dispatch_inner(dispatch, None)
}

/// Executes one actor-admitted pane provider through its compiled sandbox launch.
pub(crate) fn execute_pane_status_provider_launch(
    plan: RuntimePaneStatusProviderRefreshPlan,
) -> Option<RuntimePaneStatusProviderEvent> {
    let RuntimePaneStatusProviderRefreshPlan {
        key,
        generation,
        definition,
        launch,
        cancellation,
    } = plan;
    if cancellation.is_cancelled() {
        return None;
    }
    let RuntimePaneStatusProviderLaunch {
        context,
        capability_probe,
        sandbox_backend,
        child_launch,
        bubblewrap_activity_lease: _bubblewrap_activity_lease,
        seatbelt_workload_lease: _seatbelt_workload_lease,
    } = launch;
    let cancellation_flag = cancellation.flag();
    let outcome = capability_probe
        .map_or(Ok(()), |probe| {
            probe.run_with_cancellation(&cancellation_flag).map(|_| ())
        })
        .and_then(|()| {
            if cancellation.is_cancelled() {
                return Err(MezError::conflict("pane status provider was cancelled"));
            }
            let marker = mez_agent::MarkerToken::new("00000000000000000000000000000000")
                .map_err(|error| MezError::invalid_state(error.message()))?;
            let transaction = mez_agent::ShellTransaction::new(
                marker,
                "pane-status-provider",
                "pane-status-provider",
                &key.pane_id,
                context.shell_path(),
                &definition.command,
            )
            .map_err(|error| MezError::invalid_state(error.message()))?
            .with_child_launch(child_launch)
            .with_output_max_raw_bytes(STATUS_PILL_OUTPUT_LIMIT_BYTES);
            let request = mez_agent::ShellExecutionRequest {
                action_id: format!("pane-status:{}:{}", key.pane_id, key.name),
                transaction,
                timeout_ms: Some(definition.timeout_ms),
                interactive: false,
                stateful: false,
            };
            let executor =
                SpawnedShellExecutor::with_interrupted(context, Arc::clone(&cancellation_flag));
            let materialized = executor.materialize_launch(&request.transaction)?;
            let result = executor
                .spawn_child(&request.transaction, &materialized)
                .and_then(|child| {
                    executor.collect(
                        child,
                        request.timeout_ms,
                        request.transaction.output_max_raw_bytes,
                        None,
                        Some(sandbox_backend),
                    )
                });
            materialized.cleanup();
            result
        });
    let outcome = if cancellation.is_cancelled() {
        RuntimePaneStatusProviderOutcome::Cancelled
    } else {
        match outcome {
            Ok(output)
                if output.exit_code == Some(0)
                    && !output.timed_out
                    && !output.interrupted
                    && !output.stdout.contains('\u{fffd}') =>
            {
                RuntimePaneStatusProviderOutcome::Succeeded(runtime_status_pill_normalize_output(
                    &output.stdout,
                    definition.max_output_chars,
                ))
            }
            _ => RuntimePaneStatusProviderOutcome::Failed,
        }
    };
    Some(RuntimePaneStatusProviderEvent {
        key,
        generation,
        definition,
        outcome,
    })
}

/// Executes a native shell dispatch while publishing bounded output previews.
pub(crate) fn execute_native_shell_dispatch_with_progress(
    dispatch: crate::runtime::RuntimeNativeShellDispatch,
    progress_sender: tokio::sync::watch::Sender<
        Option<crate::runtime::RuntimeNativeShellWorkerProgress>,
    >,
) -> crate::runtime::RuntimeNativeShellOutcome {
    let patch_write = apply_patch_transaction_phase(&dispatch.request.transaction.command)
        == Some(ApplyPatchTransactionPhase::Write);
    let progress = SpawnedChildProgressReporter {
        state: Arc::new(Mutex::new(SpawnedChildProgressState {
            preview: Vec::new(),
            revision: 0,
            patch_decoder: patch_write.then(ApplyPatchProgressDecoder::new),
            confirmed_patch_sections: Vec::new(),
        })),
        sender: progress_sender,
    };
    execute_native_shell_dispatch_inner(dispatch, Some(progress))
}

/// Executes one dispatch with an optional transient output reporter.
fn execute_native_shell_dispatch_inner(
    dispatch: crate::runtime::RuntimeNativeShellDispatch,
    progress: Option<SpawnedChildProgressReporter>,
) -> crate::runtime::RuntimeNativeShellOutcome {
    let crate::runtime::RuntimeNativeShellDispatch {
        turn_id,
        action_id,
        marker,
        cancellation,
        context,
        capability_probe,
        capability_probe_only,
        sandbox_backend,
        bubblewrap_activity_lease: _bubblewrap_activity_lease,
        seatbelt_workload_lease: _seatbelt_workload_lease,
        request,
        launch_reason,
        started_at_unix_ms,
    } = dispatch;
    let command = request.transaction.command.clone();
    let mut executor = SpawnedShellExecutor::with_interrupted(context, cancellation.flag());
    executor.launch_reason = launch_reason;
    let mut sandbox_capability = None;
    let result = if executor.interrupted.load(Ordering::SeqCst) {
        Err(MezError::conflict(
            "native shell dispatch was cancelled before startup",
        ))
    } else if request.interactive || request.stateful {
        Err(MezError::invalid_args(
            "native transport does not serve stateful or interactive execution",
        ))
    } else {
        capability_probe
            .map_or(Ok(None), |probe| {
                probe
                    .run_accounted_with_cancellation(
                        &executor.launches,
                        Some(&executor.interrupted),
                    )
                    .map(Some)
            })
            .and_then(|capability| {
                sandbox_capability = capability;
                if executor.interrupted.load(Ordering::SeqCst) {
                    return Err(MezError::conflict(
                        "native shell dispatch was cancelled after capability probe",
                    ));
                }
                if capability_probe_only {
                    return Ok(mez_agent::ShellExecutionOutput::new(
                        Some(0),
                        String::new(),
                        String::new(),
                        false,
                        false,
                    ));
                }
                executor
                    .materialize_launch(&request.transaction)
                    .and_then(|materialized| {
                        let result = executor
                            .spawn_child(&request.transaction, &materialized)
                            .and_then(|child| {
                                executor.collect(
                                    child,
                                    request.timeout_ms,
                                    request.transaction.output_max_raw_bytes,
                                    progress,
                                    sandbox_backend,
                                )
                            });
                        materialized.cleanup();
                        result
                    })
            })
    }
    .map_err(|error| crate::runtime::RuntimeNativeShellFailure {
        kind: format!("{:?}", error.kind()).to_ascii_lowercase(),
        message: error.message().to_string(),
        lifecycle: error.sandbox_lifecycle_failure().cloned(),
    });
    crate::runtime::RuntimeNativeShellOutcome {
        turn_id,
        action_id,
        marker,
        command,
        started_at_unix_ms,
        sandbox_capability: sandbox_capability.map(Box::new),
        capability_probe_only,
        launch_counts: executor.launches.snapshot(),
        result,
    }
}

/// Cancellation handle for one spawned shell execution.
#[cfg(test)]
pub(crate) struct SpawnedShellInterrupt {
    /// Shared flag polled by the executing wait loop.
    flag: Arc<AtomicBool>,
}

#[cfg(test)]
impl SpawnedShellInterrupt {
    /// Requests interruption of the currently executing child; the executor
    /// kills the child process group and reports `interrupted`.
    pub(crate) fn interrupt(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }
}

/// Spawns one cancelable nonblocking pipe reader.
fn spawn_output_reader<R>(
    stream: SpawnedChildPipe,
    reader: R,
    budget: usize,
    done: mpsc::Sender<SpawnedChildPipe>,
    progress: Option<SpawnedChildProgressReporter>,
) -> SpawnedChildPipeReader
where
    R: Read + AsFd + Send + 'static,
{
    let (cancel, cancellation) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = drain_output_stream(stream, reader, budget, cancellation, progress);
        let _ = done.send(stream);
        result
    });
    SpawnedChildPipeReader {
        stream,
        cancel,
        handle,
        completed: false,
    }
}

/// Enables nonblocking reads so cancellation never depends on pipe EOF.
fn configure_output_reader_nonblocking(stream: SpawnedChildPipe, reader: &impl AsFd) -> Result<()> {
    let flags = rustix::fs::fcntl_getfl(reader.as_fd()).map_err(|error| {
        MezError::invalid_state(format!(
            "spawned shell execution could not inspect its {} pipe: {error}",
            stream.label()
        ))
    })?;
    if !flags.contains(rustix::fs::OFlags::NONBLOCK) {
        rustix::fs::fcntl_setfl(reader.as_fd(), flags | rustix::fs::OFlags::NONBLOCK).map_err(
            |error| {
                MezError::invalid_state(format!(
                    "spawned shell execution could not make its {} pipe nonblocking: {error}",
                    stream.label()
                ))
            },
        )?;
    }
    Ok(())
}

/// Drains one child output stream while permitting bounded cancellation.
fn drain_output_stream<R>(
    stream: SpawnedChildPipe,
    mut reader: R,
    budget: usize,
    cancellation: mpsc::Receiver<()>,
    progress: Option<SpawnedChildProgressReporter>,
) -> Result<CapturedPipeOutput>
where
    R: Read + AsFd,
{
    configure_output_reader_nonblocking(stream, &reader)?;
    let mut retained = Vec::new();
    let mut dropped = 0_usize;
    let mut buffer = [0_u8; 8192];
    loop {
        match cancellation.try_recv() {
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                if let Some(progress) = progress.as_ref() {
                    progress.report(stream, &buffer[..count]);
                }
                let remaining = budget.saturating_sub(retained.len());
                let keep = count.min(remaining);
                retained.extend_from_slice(&buffer[..keep]);
                dropped = dropped.saturating_add(count.saturating_sub(keep));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                match cancellation.recv_timeout(SPAWNED_CHILD_POLL_INTERVAL) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
            Err(error) => {
                return Err(MezError::invalid_state(format!(
                    "spawned shell execution could not read its {} pipe: {error}",
                    stream.label()
                )));
            }
        }
    }
    Ok(CapturedPipeOutput {
        bytes: retained,
        dropped,
    })
}

/// Records all currently available reader completion notifications.
fn drain_reader_completions(
    done: &mpsc::Receiver<SpawnedChildPipe>,
    stdout: &mut SpawnedChildPipeReader,
    stderr: &mut SpawnedChildPipeReader,
    status: Option<&mut SpawnedChildPipeReader>,
) {
    let mut status = status;
    while let Ok(stream) = done.try_recv() {
        record_reader_completion(stream, stdout, stderr, status.as_deref_mut());
    }
}

/// Waits briefly for one reader completion and drains any concurrent notices.
fn wait_for_reader_completion(
    done: &mpsc::Receiver<SpawnedChildPipe>,
    wait: Duration,
    stdout: &mut SpawnedChildPipeReader,
    stderr: &mut SpawnedChildPipeReader,
    status: Option<&mut SpawnedChildPipeReader>,
) {
    let mut status = status;
    if let Ok(stream) = done.recv_timeout(wait) {
        record_reader_completion(stream, stdout, stderr, status.as_deref_mut());
    }
    drain_reader_completions(done, stdout, stderr, status);
}

/// Marks the matching reader as complete.
fn record_reader_completion(
    completed: SpawnedChildPipe,
    stdout: &mut SpawnedChildPipeReader,
    stderr: &mut SpawnedChildPipeReader,
    status: Option<&mut SpawnedChildPipeReader>,
) {
    match completed {
        SpawnedChildPipe::Stdout => stdout.completed = true,
        SpawnedChildPipe::Stderr => stderr.completed = true,
        SpawnedChildPipe::Status => {
            if let Some(status) = status {
                status.completed = true;
            }
        }
    }
}

/// Reports whether any reader still waits for EOF from inherited writers.
fn readers_are_pending(
    stdout: &SpawnedChildPipeReader,
    stderr: &SpawnedChildPipeReader,
    status: Option<&SpawnedChildPipeReader>,
) -> bool {
    !stdout.completed || !stderr.completed || status.is_some_and(|reader| !reader.completed)
}

/// Requests shutdown only when EOF has not already completed the reader.
fn cancel_pending_reader(reader: &SpawnedChildPipeReader) {
    if !reader.completed {
        let _ = reader.cancel.send(());
    }
}

/// Joins one canceled or completed reader and preserves typed diagnostics.
fn join_output_reader(reader: SpawnedChildPipeReader) -> Result<CapturedPipeOutput> {
    reader.handle.join().map_err(|_| {
        MezError::invalid_state(format!(
            "spawned shell execution {} reader failed",
            reader.stream.label()
        ))
    })?
}

/// Validates trusted sandbox lifecycle evidence captured outside stdout and
/// stderr for one completed typed child launch.
fn validate_spawned_child_status(
    backend: crate::runtime::SandboxBackend,
    status_bytes: &[u8],
    status_dropped: usize,
    status_complete: bool,
    output: &ShellExecutionOutput,
    stderr_dropped: usize,
) -> Result<()> {
    use crate::security::sandbox::SandboxLifecycleFailureClass as Class;
    if output.timed_out || output.interrupted {
        return Ok(());
    }
    let failure =
        |class, status| spawned_lifecycle_error(backend, class, status, output, stderr_dropped);
    if status_dropped > 0 {
        return Err(failure(Class::Truncated, None));
    }
    if !status_complete {
        return Err(failure(Class::Transport, None));
    }
    let status_text =
        std::str::from_utf8(status_bytes).map_err(|_| failure(Class::InvalidUtf8, None))?;
    let status = crate::security::sandbox::parse_sandbox_lifecycle_status(backend, status_text)
        .map_err(|_| failure(Class::Malformed, None))?;
    let reported_exit_code = status
        .exit_code()
        .ok_or_else(|| failure(Class::MissingExit, Some(status)))?;
    if output.exit_code != Some(reported_exit_code) {
        return Err(failure(Class::ContradictoryExit, Some(status)));
    }
    Ok(())
}

/// Retains bounded status facts without promoting stdout or stderr to trusted evidence.
/// Invalid status leaves record presence unknown, and no failure authorizes replay.
fn spawned_lifecycle_error(
    backend: crate::runtime::SandboxBackend,
    class: crate::security::sandbox::SandboxLifecycleFailureClass,
    status: Option<crate::security::sandbox::SandboxLifecycleStatus>,
    output: &ShellExecutionOutput,
    stderr_dropped: usize,
) -> MezError {
    use crate::security::sandbox::{
        SandboxLifecycleFailure, SandboxLifecycleFailureClass as Class,
    };
    const DIAGNOSTIC_LIMIT: usize = 4096;
    let sanitized = sanitize_spawned_lifecycle_diagnostic(&output.stderr);
    let mut end = sanitized.len().min(DIAGNOSTIC_LIMIT);
    while !sanitized.is_char_boundary(end) {
        end -= 1;
    }
    let message = if class == Class::ContradictoryExit {
        format!(
            "{} status exit code contradicts the spawned process; payload completion was not proven",
            backend.as_str()
        )
    } else {
        format!(
            "{} lifecycle completion was not proven ({class:?}); payload execution or effects may be uncertain; do not automatically replay",
            backend.as_str()
        )
    };
    MezError::invalid_state(message).with_sandbox_lifecycle_failure(SandboxLifecycleFailure {
        backend: backend.as_str().to_string(),
        class,
        outer_exit_code: output.exit_code,
        outer_signal: output.signal,
        child_record_present: status.map(|status| status.child_pid().is_some()),
        exit_record_present: status.map(|status| status.exit_code().is_some()),
        stderr: sanitized[..end].to_string(),
        stderr_truncated: stderr_dropped > 0
            || output.stderr.len() > DIAGNOSTIC_LIMIT
            || sanitized.len() > end,
    })
}

/// Redacts structured credentials before retaining launcher diagnostics.
/// Whole JSON and JSON-lines use the shared recursive sanitizer. Mixed or
/// incomplete structured fragments are withheld rather than guessed at; this
/// also prevents pretty-printed fragments from leaking secrets on later lines.
fn sanitize_spawned_lifecycle_diagnostic(text: &str) -> String {
    if serde_json::from_str::<serde_json::Value>(text).is_ok() {
        return mez_agent::sanitize_provider_diagnostic_text(text);
    }
    let mut lines = Vec::new();
    for line in text.lines() {
        if serde_json::from_str::<serde_json::Value>(line).is_ok() {
            lines.push(mez_agent::sanitize_provider_diagnostic_text(line));
        } else if line.contains(['{', '[', '"']) {
            return "[redacted: mixed or incomplete structured diagnostic]".to_string();
        } else {
            lines.push(mez_agent::sanitize_provider_primary_error_text(line));
        }
    }
    lines.join("\n")
}

/// Kills the entire process group led by `pid` so command children are
/// reaped with the shell instead of being orphaned.
fn kill_process_group(pid: i32) {
    if pid > 0 {
        // SAFETY: a negative pid targets the process group led by `pid`. The
        // child was spawned with `process_group(0)`, making it the leader.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::native_workload_environment::{
        NativeLaunchEnvironmentRole, compose_native_workload_environment,
    };
    use super::*;
    use mez_agent::{MarkerToken, ShellChildLaunch, ShellLaunchArtifact, ShellLaunchArtifactId};
    use mez_mux::process::RawEnvironmentEntry;

    /// Builds one raw environment entry fixture.
    fn entry(key: &str, value: &str) -> RawEnvironmentEntry {
        RawEnvironmentEntry {
            key: key.as_bytes().to_vec(),
            value: value.as_bytes().to_vec(),
        }
    }

    /// Returns one launched environment value as text.
    fn environment_value<'a>(entries: &'a [RawEnvironmentEntry], key: &str) -> Option<&'a str> {
        entries
            .iter()
            .find(|entry| entry.key.as_slice() == key.as_bytes())
            .and_then(|entry| std::str::from_utf8(&entry.value).ok())
    }

    /// Returns the launched environment keys in composition order.
    fn environment_keys(entries: &[RawEnvironmentEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| String::from_utf8_lossy(&entry.key).into_owned())
            .collect()
    }

    /// Builds the daemon ambient fixture consumed by composition tests.
    ///
    /// The fixture models a daemon environment that carries a harness-only
    /// sentinel credential and a duplicate key. It is supplied per child-process
    /// fixture instead of mutating the test process environment, and it keeps the
    /// real test `PATH` so fixtures can execute ordinary tools.
    fn daemon_ambient_fixture() -> Vec<RawEnvironmentEntry> {
        vec![
            entry("PATH", &std::env::var("PATH").unwrap_or_default()),
            entry("MEZ_DAEMON_ONLY_SENTINEL", "harness-only-credential"),
            entry("MEZ_DUPLICATE_KEY", "daemon"),
        ]
    }

    /// Builds one workload-role context fixture whose environment was composed
    /// from explicit pane-root evidence and the daemon ambient fixture.
    ///
    /// Callers apply the dispatch role switch with
    /// `NativeShellContext::for_launch_role` so fixtures follow the same role
    /// selection production dispatch performs.
    fn fixture_context(evidence: Vec<RawEnvironmentEntry>) -> NativeShellContext {
        let composed = compose_native_workload_environment(
            &evidence,
            &daemon_ambient_fixture(),
            Path::new("/bin/sh"),
        )
        .expect("fixture composition never requires pane identity");
        NativeShellContext::for_test_composed(
            PathBuf::from("/bin/sh"),
            composed,
            std::env::temp_dir(),
        )
    }

    /// Builds one request that runs an environment dump through a typed child
    /// launch, modelling a code-owned sandbox launcher process.
    fn launcher_environment_request() -> ShellExecutionRequest {
        let transaction = ShellTransaction::new(
            MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap(),
            "turn-1",
            "agent-1",
            "%1",
            Path::new("/bin/sh"),
            "printf ignored",
        )
        .unwrap()
        .with_child_launch(ShellChildLaunch::new("/usr/bin/env", Vec::new()).unwrap());
        ShellExecutionRequest {
            action_id: "native-1".to_string(),
            transaction,
            timeout_ms: Some(5_000),
            interactive: false,
            stateful: false,
        }
    }

    /// Builds one test context around the host `/bin/sh`.
    fn test_context() -> NativeShellContext {
        NativeShellContext::for_test(
            PathBuf::from("/bin/sh"),
            vec![RawEnvironmentEntry {
                key: b"MEZ_NATIVE_TEST".to_vec(),
                value: b"visible".to_vec(),
            }],
            std::env::temp_dir(),
        )
    }

    /// Builds one shell execution request for a non-stateful command.
    fn request(command: &str, timeout_ms: Option<u64>) -> ShellExecutionRequest {
        ShellExecutionRequest {
            action_id: "native-1".to_string(),
            transaction: ShellTransaction::new(
                MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap(),
                "turn-1",
                "agent-1",
                "%1",
                Path::new("/bin/sh"),
                command,
            )
            .unwrap(),
            timeout_ms,
            interactive: false,
            stateful: false,
        }
    }

    /// Builds one external-worker dispatch with an optional deferred sandbox
    /// capability probe.
    fn dispatch_with_probe(
        command: &str,
        probe: Option<crate::runtime::processes::NativeSandboxCapabilityProbe>,
    ) -> crate::runtime::RuntimeNativeShellDispatch {
        crate::runtime::RuntimeNativeShellDispatch {
            turn_id: "turn-1".to_string(),
            action_id: "native-1".to_string(),
            marker: "0123456789abcdef0123456789abcdef".to_string(),
            cancellation: super::super::native_cancellation::NativeActionCancellation::new(),
            context: test_context(),
            capability_probe: probe,
            capability_probe_only: false,
            sandbox_backend: None,
            bubblewrap_activity_lease: None,
            seatbelt_workload_lease: None,
            request: request(command, Some(5_000)),
            launch_reason: NativeLaunchReason::ShellCommand,
            started_at_unix_ms: 1,
        }
    }

    /// Cancellation admitted before worker startup must not be reset by the
    /// executor or launch any workload, even when the command is harmless.
    #[test]
    fn native_dispatch_pre_cancelled_does_not_launch() {
        let dispatch = dispatch_with_probe("printf forbidden", None);
        dispatch.cancellation.cancel();
        let outcome = execute_native_shell_dispatch(dispatch);
        assert!(outcome.result.is_err());
        assert!(outcome.launch_counts.unwrap_or_default().is_empty());
    }

    /// A dispatch owner can stop a running child before its delayed write.
    /// The ready file proves the workload started; cancellation must reap the
    /// process group, not merely discard the eventual stale action result.
    #[test]
    fn native_dispatch_cancellation_prevents_delayed_effect() {
        run_native_dispatch_delayed_effect(false);
    }

    /// Dropping the async dispatch owner must signal its blocking worker;
    /// dropping a JoinHandle alone would detach it and allow the delayed write.
    #[test]
    fn native_dispatch_owner_drop_prevents_delayed_effect() {
        run_native_dispatch_delayed_effect(true);
    }

    /// Runs the ready-barrier fixture with explicit or owner-drop cancellation.
    fn run_native_dispatch_delayed_effect(owner_drop: bool) {
        let directory = std::env::temp_dir().join(format!(
            "mez-native-cancel-{}-{}-{}",
            std::process::id(),
            crate::runtime::current_unix_millis(),
            owner_drop
        ));
        fs::create_dir(&directory).unwrap();
        let ready = directory.join("ready");
        let effect = directory.join("effect");
        let command = format!(
            "printf ready > '{}'; sleep 2; printf forbidden > '{}'",
            ready.display(),
            effect.display()
        );
        let dispatch = dispatch_with_probe(&command, None);
        let cancellation = dispatch.cancellation.clone();
        let owner = cancellation.cancel_on_drop();
        let worker = std::thread::spawn(move || execute_native_shell_dispatch(dispatch));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if owner_drop {
            drop(owner);
        } else {
            cancellation.cancel();
        }
        let outcome = worker.join().unwrap();
        let ready_exists = ready.exists();
        let effect_exists = effect.exists();
        fs::remove_dir_all(directory).unwrap();
        assert!(ready_exists, "workload never reached its ready barrier");
        assert!(outcome.result.unwrap().interrupted);
        assert!(!effect_exists, "cancelled worker performed a delayed write");
    }

    /// The legacy native-patch adapter must expose its direct child launch,
    /// and the process-free admission guard must reject that same production
    /// spawn boundary before the command can run. This deliberately does not
    /// assert that migration of the native patch executor is complete.
    #[test]
    fn legacy_native_patch_launch_is_observable_and_rejectable() {
        let mut executor = SpawnedShellExecutor::new(test_context());
        executor.launch_reason = NativeLaunchReason::LegacyPatch;
        let request = request("printf legacy-patch", Some(5_000));
        let output = executor.execute_shell(&request).unwrap();
        assert_eq!(output.stdout, "legacy-patch");
        assert_eq!(
            executor.launches.snapshot().unwrap(),
            vec![super::super::launch_accounting::NativeLaunchCount {
                reason: NativeLaunchReason::LegacyPatch,
                attempted: 1,
                launched: 1,
            }]
        );
        executor.launches = NativeLaunchLedger::new(true);
        assert!(
            executor
                .execute_shell(&request)
                .unwrap_err()
                .message()
                .contains("process-free")
        );
        assert_eq!(executor.launches.snapshot().unwrap()[0].launched, 0);
    }

    /// A configured backend cannot evade the process-free guard using an
    /// absolute executable or a self-reexecuted product helper. The production
    /// probe owner must reject its launch before trying the missing executable.
    #[test]
    fn process_free_probe_rejects_absolute_executable_before_spawn() {
        let ledger = NativeLaunchLedger::new(true);
        let probe = super::super::NativeSandboxCapabilityProbe::Bubblewrap(
            super::super::NativeBubblewrapCapabilityProbe::for_test(
                "/nonexistent/absolute/mez",
                Vec::new(),
                "unused",
            ),
        );
        assert!(
            probe
                .run_accounted(&ledger)
                .unwrap_err()
                .message()
                .contains("process-free")
        );
        assert_eq!(ledger.snapshot().unwrap()[0].attempted, 1);
        assert_eq!(ledger.snapshot().unwrap()[0].launched, 0);
    }

    /// Worker completion carries positive launch evidence for the exact
    /// actor-selected action purpose, not a heuristic from command text.
    #[test]
    fn native_worker_returns_actor_selected_launch_evidence() {
        let mut dispatch = dispatch_with_probe("printf patch", None);
        dispatch.launch_reason = NativeLaunchReason::LegacyPatch;
        let outcome = execute_native_shell_dispatch(dispatch);
        assert!(outcome.result.is_ok());
        assert_eq!(
            outcome.launch_counts.unwrap()[0].reason,
            NativeLaunchReason::LegacyPatch
        );
    }

    /// Verifies native progress publications carry strictly increasing revisions.
    ///
    /// Stdout and stderr readers share one reporter. Each accepted chunk must
    /// advance the revision under the same lock that updates cumulative output
    /// so watch-channel coalescing can never make an older snapshot look newer.
    #[test]
    fn spawned_child_progress_revisions_increase_with_each_snapshot() {
        let (sender, mut receiver) = tokio::sync::watch::channel(None);
        let reporter = SpawnedChildProgressReporter {
            state: Arc::new(Mutex::new(SpawnedChildProgressState {
                preview: Vec::new(),
                revision: 0,
                patch_decoder: None,
                confirmed_patch_sections: Vec::new(),
            })),
            sender,
        };

        reporter.report(SpawnedChildPipe::Stdout, b"first");
        let first = receiver.borrow_and_update().clone().unwrap();
        reporter.report(SpawnedChildPipe::Stderr, b"-second");
        let second = receiver.borrow_and_update().clone().unwrap();

        assert_eq!(first.output, Some((1, "first".to_string())));
        assert!(first.confirmed_patch_sections.is_empty());
        assert_eq!(second.output, Some((2, "first-second".to_string())));
        assert!(second.confirmed_patch_sections.is_empty());
    }

    /// Verifies exit codes and stream separation arrive intact from the
    /// spawned shell without pane framing.
    #[test]
    fn spawned_executor_reports_exit_code_and_stream_separation() {
        let mut executor = SpawnedShellExecutor::new(test_context());
        let output = executor
            .execute_shell(&request("printf out; printf err >&2; exit 3", Some(5_000)))
            .unwrap();

        assert_eq!(output.exit_code, Some(3));
        assert_eq!(output.stdout, "out");
        assert_eq!(output.stderr, "err");
        assert!(!output.timed_out);
        assert!(!output.interrupted);
    }

    /// Verifies the workload keeps the declared `PATH` requirement when the pane
    /// root supplies none.
    ///
    /// `PATH` is the only deliberately ambient-forwarded workload variable and
    /// remains required for ordinary shell command lookup, so the composed
    /// environment still reproduces the ambient `mez` `PATH` for a pane whose
    /// root-process environment is unreadable.
    #[test]
    fn spawned_executor_forwards_declared_workload_path_requirement() {
        let parent_path = std::env::var("PATH").expect("test process has PATH");
        let mut executor = SpawnedShellExecutor::new(test_context());
        let output = executor
            .execute_shell(&request("printf %s \"$PATH\"", Some(5_000)))
            .unwrap();

        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.stdout, parent_path);
    }

    /// Verifies one isolated workload child process never receives a daemon-only
    /// sentinel variable while a distinct pane-provided value is present and
    /// wins on a duplicate key.
    ///
    /// The daemon ambient environment and pane-root evidence are built as a
    /// per-child-process fixture instead of mutating the test process
    /// environment, so the assertion covers composition rather than ambient
    /// state. The payload is the real `/bin/sh` child the executor launches, and
    /// it dumps its own environment for inspection.
    #[test]
    fn spawned_executor_drops_daemon_only_sentinel_while_pane_values_win() {
        let context = fixture_context(vec![
            entry("MEZ_PANE_PROVIDED", "pane-value"),
            entry("MEZ_DUPLICATE_KEY", "pane"),
        ]);
        let mut executor = SpawnedShellExecutor::new(context);
        let output = executor
            .execute_shell(&request("env", Some(5_000)))
            .unwrap();

        assert_eq!(output.exit_code, Some(0));
        assert!(!output.stdout.contains("MEZ_DAEMON_ONLY_SENTINEL"));
        assert!(output.stdout.contains("MEZ_PANE_PROVIDED=pane-value"));
        assert!(output.stdout.contains("MEZ_DUPLICATE_KEY=pane"));
    }

    /// Verifies optional absent values fall back to documented safe defaults
    /// instead of failing the launch.
    #[test]
    fn spawned_executor_tolerates_absent_optional_values() {
        let context = fixture_context(Vec::new());
        let mut executor = SpawnedShellExecutor::new(context);
        let output = executor
            .execute_shell(&request("printf '%s|%s' \"$SHELL\" \"$PATH\"", Some(5_000)))
            .unwrap();

        assert_eq!(output.exit_code, Some(0));
        assert!(output.stdout.starts_with("/bin/sh|"));
        assert!(output.stdout.len() > "/bin/sh|".len());
        assert!(!output.stdout.contains("MEZ_DAEMON_ONLY_SENTINEL"));
    }

    /// Verifies a Bubblewrap dispatch selects the launcher role and hands the
    /// code-owned launcher only the launcher control bucket.
    ///
    /// CI cannot compile or execute a Bubblewrap launch plan, so the test
    /// exercises the real dispatch role switch
    /// (`NativeLaunchEnvironmentRole::for_native_dispatch` and
    /// `NativeShellContext::for_launch_role`, the two calls the native dispatch
    /// makes) on a composed context, then observes the launched environment of a
    /// real child process standing in for the launcher. The sandboxed payload
    /// environment stays owned by the compiled plan (`--clearenv` followed by
    /// `--setenv`), which the security sandbox tests assert on supported
    /// platforms.
    #[test]
    fn spawned_executor_bubblewrap_dispatch_selects_launcher_environment_bucket() {
        let role = NativeLaunchEnvironmentRole::for_native_dispatch(true, false);
        assert_eq!(role, NativeLaunchEnvironmentRole::SandboxLauncher);
        let context = fixture_context(vec![
            entry("PATH", "/pane/bin"),
            entry("MEZ_PANE_CREDENTIAL", "pane-secret"),
        ])
        .for_launch_role(role);
        let launcher_entries = context.launch_environment();

        assert_eq!(environment_keys(launcher_entries), vec!["PATH".to_string()]);
        assert_eq!(
            environment_value(launcher_entries, "PATH"),
            Some("/pane/bin")
        );

        let mut executor = SpawnedShellExecutor::new(context);
        let output = executor
            .execute_shell(&launcher_environment_request())
            .unwrap();

        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.stdout.trim_end(), "PATH=/pane/bin");
    }

    /// Verifies a Seatbelt dispatch selects the launcher role while the same
    /// composed context keeps its pane evidence in the workload bucket only.
    ///
    /// CI cannot compile or execute a Seatbelt profile, so the test exercises the
    /// real dispatch role switch and observes the launched environment of a real
    /// child process standing in for the code-owned child supervisor, which
    /// clears its own environment before entering the profile and projects only
    /// the validated Seatbelt environment document into the sandboxed payload.
    #[test]
    fn spawned_executor_seatbelt_dispatch_selects_launcher_environment_bucket() {
        let role = NativeLaunchEnvironmentRole::for_native_dispatch(false, true);
        assert_eq!(role, NativeLaunchEnvironmentRole::SandboxLauncher);
        let workload_context = fixture_context(vec![
            entry("MEZ_PANE_CREDENTIAL", "pane-secret"),
            entry("MEZ_DUPLICATE_KEY", "pane"),
        ]);
        let context = workload_context.clone().for_launch_role(role);

        assert_eq!(
            environment_value(workload_context.launch_environment(), "MEZ_PANE_CREDENTIAL"),
            Some("pane-secret")
        );
        let launcher_entries = context.launch_environment();

        assert_eq!(environment_keys(launcher_entries), vec!["PATH".to_string()]);
        for key in [
            "MEZ_PANE_CREDENTIAL",
            "MEZ_DUPLICATE_KEY",
            "MEZ_DAEMON_ONLY_SENTINEL",
        ] {
            assert_eq!(
                environment_value(launcher_entries, key),
                None,
                "launcher bucket must not carry {key}"
            );
        }

        let mut executor = SpawnedShellExecutor::new(context);
        let output = executor
            .execute_shell(&launcher_environment_request())
            .unwrap();

        assert_eq!(output.exit_code, Some(0));
        assert!(!output.stdout.contains("MEZ_PANE_CREDENTIAL"));
        assert!(!output.stdout.contains("MEZ_DAEMON_ONLY_SENTINEL"));
        assert_eq!(output.stdout.trim_end().lines().count(), 1);
        assert!(output.stdout.trim_end().starts_with("PATH="));
    }

    /// Verifies a non-sandbox native dispatch keeps the workload bucket, so the
    /// launcher control bucket can never leak into policy-only or host-access
    /// launches.
    #[test]
    fn spawned_executor_non_sandbox_dispatch_keeps_workload_environment_bucket() {
        let role = NativeLaunchEnvironmentRole::for_native_dispatch(false, false);
        assert_eq!(role, NativeLaunchEnvironmentRole::Workload);
        let context = fixture_context(vec![entry("MEZ_PANE_CREDENTIAL", "pane-secret")])
            .for_launch_role(role);
        let workload_entries = context.launch_environment();

        assert_eq!(
            environment_value(workload_entries, "MEZ_PANE_CREDENTIAL"),
            Some("pane-secret")
        );
        assert_eq!(
            environment_value(workload_entries, "MEZ_DAEMON_ONLY_SENTINEL"),
            None
        );
        assert!(environment_keys(workload_entries).contains(&"PATH".to_string()));
    }

    /// Verifies an uncached native Bubblewrap probe executes in the external
    /// worker before the authorized workload and returns capability evidence
    /// for actor-owned caching.
    #[test]
    fn native_worker_runs_deferred_probe_before_workload() {
        let marker =
            std::env::temp_dir().join(format!("mez-native-probe-order-{}", std::process::id()));
        let _ = fs::remove_file(&marker);
        let probe_script = format!(
            "printf ready > '{}'; printf mez-native-probe-ok",
            marker.display()
        );
        let probe = crate::runtime::processes::NativeSandboxCapabilityProbe::Bubblewrap(
            crate::runtime::processes::NativeBubblewrapCapabilityProbe::for_test(
                "/bin/sh",
                vec!["-c".to_string(), probe_script],
                "mez-native-probe-ok",
            ),
        );
        let command = format!(
            "test -f '{}' && printf workload-after-probe",
            marker.display()
        );

        let outcome = execute_native_shell_dispatch(dispatch_with_probe(&command, Some(probe)));
        let output = outcome.result.expect("probe and workload succeed");
        let _ = fs::remove_file(&marker);

        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.stdout, "workload-after-probe");
        assert!(matches!(
            outcome.sandbox_capability.as_deref(),
            Some(crate::security::sandbox::SandboxCapability::Bubblewrap(_))
        ));
    }

    /// Verifies a failed deferred capability probe prevents the workload from
    /// starting and returns no cacheable capability evidence.
    #[test]
    fn native_worker_probe_failure_prevents_workload_launch() {
        let marker =
            std::env::temp_dir().join(format!("mez-native-probe-failure-{}", std::process::id()));
        let _ = fs::remove_file(&marker);
        let probe = crate::runtime::processes::NativeSandboxCapabilityProbe::Bubblewrap(
            crate::runtime::processes::NativeBubblewrapCapabilityProbe::for_test(
                "/bin/sh",
                vec![
                    "-c".to_string(),
                    "printf wrong; printf 'bwrap: namespace denied\\033[31m\\n' >&2; exit 7"
                        .to_string(),
                ],
                "mez-native-probe-ok",
            ),
        );
        let command = format!("printf ran > '{}'", marker.display());

        let outcome = execute_native_shell_dispatch(dispatch_with_probe(&command, Some(probe)));

        let failure = outcome.result.unwrap_err();
        assert!(failure.message.contains("exit code 7"), "{failure:?}");
        assert!(
            failure.message.contains("bwrap: namespace denied"),
            "{failure:?}"
        );
        assert!(failure.message.contains("\\u{1b}"), "{failure:?}");
        assert!(!failure.message.contains('\u{1b}'), "{failure:?}");
        assert!(outcome.sandbox_capability.is_none());
        assert!(!marker.exists(), "workload ran despite failed probe");
    }

    /// Verifies a legacy Bubblewrap implementation that accepts the baseline
    /// probe but rejects the workload-only JSON status descriptor prevents the
    /// workload from starting before capability can be cached.
    #[test]
    fn native_worker_rejects_legacy_bubblewrap_without_json_status_fd() {
        let marker =
            std::env::temp_dir().join(format!("mez-native-probe-status-fd-{}", std::process::id()));
        let _ = fs::remove_file(&marker);
        let probe = crate::runtime::processes::NativeSandboxCapabilityProbe::Bubblewrap(
            crate::runtime::processes::NativeBubblewrapCapabilityProbe::for_test(
                "/bin/sh",
                vec![
                    "-c".to_string(),
                    "if [ \"$1\" = \"--json-status-fd\" ]; then printf '%s' 'bwrap: unknown option --json-status-fd' >&2; exit 64; fi; printf mez-native-probe-ok".to_string(),
                    "legacy-bwrap".to_string(),
                    "--json-status-fd".to_string(),
                    "3".to_string(),
                ],
                "mez-native-probe-ok",
            ),
        );
        let command = format!("printf ran > '{}'", marker.display());

        let outcome = execute_native_shell_dispatch(dispatch_with_probe(&command, Some(probe)));

        let failure = outcome.result.unwrap_err();
        assert!(
            failure.message.contains("unknown option --json-status-fd"),
            "{failure:?}"
        );
        assert!(outcome.sandbox_capability.is_none());
        assert!(
            !marker.exists(),
            "workload ran despite unsupported status descriptor"
        );
    }

    /// Verifies probe pipes are drained concurrently beyond the retained
    /// diagnostic bound so a noisy valid probe cannot deadlock or receive
    /// `SIGPIPE` before publishing its sentinel.
    #[test]
    fn native_worker_drains_noisy_probe_output_without_deadlock() {
        let probe = crate::runtime::processes::NativeSandboxCapabilityProbe::Bubblewrap(
            crate::runtime::processes::NativeBubblewrapCapabilityProbe::for_test(
                "/bin/sh",
                vec![
                    "-c".to_string(),
                    "i=0; while [ \"$i\" -lt 20000 ]; do printf x >&2; i=$((i + 1)); done; printf mez-native-probe-ok"
                        .to_string(),
                ],
                "mez-native-probe-ok",
            ),
        );
        let started = Instant::now();

        let outcome =
            execute_native_shell_dispatch(dispatch_with_probe("printf workload", Some(probe)));
        let output = outcome.result.expect("noisy probe and workload succeed");

        assert_eq!(output.stdout, "workload");
        assert!(matches!(
            outcome.sandbox_capability.as_deref(),
            Some(crate::security::sandbox::SandboxCapability::Bubblewrap(_))
        ));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// Verifies a probe-only native Seatbelt worker returns exact cacheable
    /// capability evidence without executing the user workload. This preserves
    /// fail-closed behavior until the dependent Seatbelt launch integration is
    /// available.
    #[test]
    fn native_seatbelt_probe_only_worker_never_launches_workload() {
        let marker = std::env::temp_dir().join(format!(
            "mez-native-seatbelt-probe-only-{}",
            std::process::id()
        ));
        let _ = fs::remove_file(&marker);
        let probe = crate::runtime::processes::NativeSandboxCapabilityProbe::Seatbelt(
            super::super::native_bubblewrap::NativeSeatbeltCapabilityProbe::for_test(
                "/bin/sh",
                vec![
                    "-c".to_string(),
                    "printf mez-native-seatbelt-ok".to_string(),
                ],
                "mez-native-seatbelt-ok",
            ),
        );
        let mut dispatch = dispatch_with_probe(
            &format!("printf workload-ran > '{}'", marker.display()),
            Some(probe),
        );
        dispatch.capability_probe_only = true;

        let outcome = execute_native_shell_dispatch(dispatch);
        let output = outcome.result.expect("Seatbelt probe succeeds");

        assert_eq!(output.exit_code, Some(0));
        assert!(output.stdout.is_empty());
        assert!(outcome.capability_probe_only);
        assert!(matches!(
            outcome.sandbox_capability.as_deref(),
            Some(crate::security::sandbox::SandboxCapability::Seatbelt(_))
        ));
        assert!(!marker.exists(), "probe-only worker launched the workload");
    }

    /// Verifies a failed probe-only Seatbelt worker returns no cache evidence
    /// and still cannot execute the user workload.
    #[test]
    fn native_seatbelt_probe_failure_is_not_cacheable() {
        let marker = std::env::temp_dir().join(format!(
            "mez-native-seatbelt-probe-failure-{}",
            std::process::id()
        ));
        let _ = fs::remove_file(&marker);
        let probe = crate::runtime::processes::NativeSandboxCapabilityProbe::Seatbelt(
            super::super::native_bubblewrap::NativeSeatbeltCapabilityProbe::for_test(
                "/bin/sh",
                vec!["-c".to_string(), "printf contaminated; exit 1".to_string()],
                "mez-native-seatbelt-ok",
            ),
        );
        let mut dispatch = dispatch_with_probe(
            &format!("printf workload-ran > '{}'", marker.display()),
            Some(probe),
        );
        dispatch.capability_probe_only = true;

        let outcome = execute_native_shell_dispatch(dispatch);

        assert!(outcome.result.is_err());
        assert!(outcome.sandbox_capability.is_none());
        assert!(!marker.exists(), "failed probe launched the workload");
    }

    /// Verifies native worker settlement validates Seatbelt lifecycle records
    /// with the selected backend rather than the legacy Bubblewrap parser.
    #[test]
    fn native_worker_validates_seatbelt_lifecycle_status() {
        let mut dispatch = dispatch_with_probe("printf native-seatbelt-status", None);
        dispatch.sandbox_backend = Some(crate::runtime::SandboxBackend::Seatbelt);
        let status_script = "printf '{\"version\":1,\"event\":\"sandbox-entered\"}\\n{\"version\":1,\"event\":\"child-established\",\"child-pid\":%s}\\n' \"$$\" >&3; /bin/sh \"$1\"; status=$?; printf '{\"version\":1,\"event\":\"exit\",\"exit-code\":%s}\\n' \"$status\" >&3; exit \"$status\"";
        dispatch.request.transaction = dispatch.request.transaction.with_child_launch(
            ShellChildLaunch::new(
                "/bin/sh",
                vec![
                    ShellChildArgument::Literal("-c".to_string()),
                    ShellChildArgument::Literal(status_script.to_string()),
                    ShellChildArgument::Literal("sh".to_string()),
                    ShellChildArgument::MaterializedCommandFile,
                ],
            )
            .unwrap()
            .with_status_fd(crate::security::sandbox::SANDBOX_STATUS_FD)
            .unwrap(),
        );

        let outcome = execute_native_shell_dispatch(dispatch);
        let output = outcome.result.unwrap();

        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.stdout, "native-seatbelt-status");
    }

    /// Verifies the inferred environment and working directory reach the
    /// spawned shell through the composed cleared-base environment captured from
    /// the pane root process.
    #[test]
    fn spawned_executor_forwards_context_environment_and_working_directory() {
        let directory = std::env::temp_dir().join(format!("mez-native-cwd-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let context = NativeShellContext::for_test(
            PathBuf::from("/bin/sh"),
            vec![
                RawEnvironmentEntry {
                    key: b"MEZ_NATIVE_TEST".to_vec(),
                    value: b"visible".to_vec(),
                },
                RawEnvironmentEntry {
                    key: b"PATH".to_vec(),
                    value: b"/pane/root/path".to_vec(),
                },
            ],
            directory.clone(),
        );
        let mut executor = SpawnedShellExecutor::new(context);
        let output = executor
            .execute_shell(&request(
                "printf '%s\\n%s\\n' \"$MEZ_NATIVE_TEST\" \"$PATH\"; pwd",
                Some(5_000),
            ))
            .unwrap();
        let _ = fs::remove_dir(&directory);

        assert_eq!(output.exit_code, Some(0));
        assert!(output.stdout.starts_with("visible\n/pane/root/path\n"));
        assert!(
            output
                .stdout
                .contains(&directory.file_name().unwrap().to_string_lossy().to_string())
        );
    }

    /// Verifies the timeout kills the child process group and reports the
    /// outcome as timed out rather than as a shell exit.
    #[test]
    fn spawned_executor_times_out_and_kills_the_process_group() {
        let mut executor = SpawnedShellExecutor::new(test_context());
        let started = Instant::now();
        let output = executor
            .execute_shell(&request("sleep 30", Some(300)))
            .unwrap();

        assert!(output.timed_out);
        assert!(!output.interrupted);
        assert_eq!(output.exit_code, None);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    /// Verifies a timed-out native worker retains its Seatbelt workload lease
    /// until process-group termination settles, then removes the private
    /// action, HOME, temporary, command, and environment artifact tree.
    #[test]
    fn native_seatbelt_timeout_cleans_workload_lease() {
        let artifacts = crate::security::sandbox::prepare_seatbelt_workload_artifacts(
            Path::new("/tmp"),
            "sleep 30",
            None,
        )
        .unwrap();
        let action_directory = artifacts.action_directory.clone();
        let lease = artifacts.lease.clone();
        drop(artifacts);
        assert!(action_directory.exists());

        let mut dispatch = dispatch_with_probe("sleep 30", None);
        dispatch.request.timeout_ms = Some(300);
        dispatch.seatbelt_workload_lease = Some(lease);
        let outcome = execute_native_shell_dispatch(dispatch);
        let output = outcome.result.unwrap();

        assert!(output.timed_out);
        assert!(!output.interrupted);
        assert_eq!(output.exit_code, None);
        assert!(!action_directory.exists());
    }

    /// Verifies normal completion does not wait indefinitely when a detached
    /// descendant inherits stdout and stderr after the direct shell exits.
    #[test]
    fn spawned_executor_completion_does_not_wait_for_escaped_descendant_pipes() {
        let mut executor = SpawnedShellExecutor::new(test_context());
        let started = Instant::now();
        let output = executor
            .execute_shell(&request(
                "python3 -c 'import subprocess,sys; subprocess.Popen([\"python3\",\"-c\",\"import time; time.sleep(2)\"], stdout=sys.stdout, stderr=sys.stderr, start_new_session=True); print(\"done\")'",
                Some(1_000),
            ))
            .unwrap();

        assert_eq!(output.exit_code, Some(0));
        assert!(output.stdout.contains("done"), "stdout={}", output.stdout);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "completion waited for escaped descendant: {:?}",
            started.elapsed()
        );
    }

    /// Verifies timeout completion remains bounded when a detached descendant
    /// survives process-group cleanup while retaining inherited output pipes.
    /// The wider gap between scheduling slack and descendant lifetime keeps
    /// this integration check distinct from the deterministic open-pipe test.
    #[test]
    fn spawned_executor_timeout_does_not_wait_for_escaped_descendant_pipes() {
        let mut executor = SpawnedShellExecutor::new(test_context());
        let started = Instant::now();
        let output = executor
            .execute_shell(&request(
                "python3 -c 'import subprocess,sys,time; subprocess.Popen([\"python3\",\"-c\",\"import time; time.sleep(10)\"], stdout=sys.stdout, stderr=sys.stderr, start_new_session=True); time.sleep(10)'",
                Some(100),
            ))
            .unwrap();

        assert!(output.timed_out);
        assert_eq!(output.exit_code, None);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timeout waited for escaped descendant: {:?}",
            started.elapsed()
        );
    }

    /// Cancellation must finish while an inherited writer remains open.
    /// Waiting for reader completion before dropping the writer proves that
    /// shutdown does not depend on EOF, independently of process startup time.
    #[test]
    fn spawned_output_reader_cancellation_does_not_require_eof() {
        let (reader, writer) = status_pipe().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let reader = spawn_output_reader(
            SpawnedChildPipe::Stdout,
            std::fs::File::from(reader),
            4096,
            done_tx,
            None,
        );
        cancel_pending_reader(&reader);
        let completion = done_rx.recv_timeout(Duration::from_secs(5));
        // Always release the writer before a failing assertion so a defective
        // blocking reader can unwind rather than leaking an open test pipe.
        drop(writer);
        assert!(
            completion.is_ok(),
            "reader cancellation waited for pipe EOF"
        );
        assert!(join_output_reader(reader).unwrap().bytes.is_empty());
    }

    /// Verifies the interruption handle kills a running child and reports
    /// the outcome as interrupted.
    #[test]
    fn spawned_executor_interrupts_a_running_child() {
        let mut executor = SpawnedShellExecutor::new(test_context());
        let interrupt = executor.interrupt_handle();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            interrupt.interrupt();
        });
        let output = executor.execute_shell(&request("sleep 30", None)).unwrap();

        assert!(output.interrupted);
        assert!(!output.timed_out);
        handle.join().unwrap();
    }

    /// Verifies typed child launches substitute the materialized command
    /// file into their argv, covering interpreter-backed actions.
    #[test]
    fn spawned_executor_honors_typed_child_launches() {
        let mut executor = SpawnedShellExecutor::new(test_context());
        let mut transaction_request = request("printf child-launch-ok", Some(5_000));
        transaction_request.transaction = transaction_request.transaction.with_child_launch(
            ShellChildLaunch::new(
                "/bin/sh",
                vec![
                    ShellChildArgument::Literal("-e".to_string()),
                    ShellChildArgument::MaterializedCommandFile,
                ],
            )
            .unwrap(),
        );
        let output = executor.execute_shell(&transaction_request).unwrap();

        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.stdout, "child-launch-ok");
    }

    /// Verifies native typed launches receive canonical owner-only artifact
    /// paths and validated path bindings, then remove the private launch tree.
    #[test]
    fn spawned_executor_materializes_and_cleans_typed_artifacts() {
        let mut executor = SpawnedShellExecutor::new(test_context());
        let mut transaction_request = request("true", Some(5_000));
        let artifact_id = ShellLaunchArtifactId::new("profile").unwrap();
        transaction_request.transaction = transaction_request.transaction.with_child_launch(
            ShellChildLaunch::new_with_artifacts(
                "/bin/sh",
                vec![
                    ShellChildArgument::Literal("-c".to_string()),
                    ShellChildArgument::Literal(
                        "test -f \"$1\" && test \"$(cat \"$1\")\" = profile && test \"$2\" = \"PROFILE=$1\" && printf '%s' \"$1\""
                            .to_string(),
                    ),
                    ShellChildArgument::Literal("sh".to_string()),
                    ShellChildArgument::MaterializedArtifact(artifact_id.clone()),
                    ShellChildArgument::MaterializedPathBinding {
                        name: "PROFILE".to_string(),
                        artifact: artifact_id.clone(),
                    },
                ],
                vec![
                    ShellLaunchArtifact::new(artifact_id, b"profile".to_vec(), 0o400).unwrap(),
                ],
            )
            .unwrap(),
        );

        let output = executor.execute_shell(&transaction_request).unwrap();
        let artifact_path = PathBuf::from(&output.stdout);

        assert_eq!(output.exit_code, Some(0), "stderr={}", output.stderr);
        assert!(artifact_path.is_absolute(), "stdout={}", output.stdout);
        assert!(!artifact_path.exists(), "artifact was not cleaned up");
        assert!(
            !artifact_path.parent().unwrap().exists(),
            "launch directory was not cleaned up"
        );
    }

    /// An incomplete status after child establishment must not claim that the
    /// payload never ran; stdout cannot supply the missing completion evidence.
    #[test]
    fn native_worker_incomplete_lifecycle_reports_uncertain_completion() {
        let mut dispatch = dispatch_with_probe("true", None);
        dispatch.sandbox_backend = Some(crate::runtime::SandboxBackend::Bubblewrap);
        dispatch.request.transaction = dispatch.request.transaction.with_child_launch(
            ShellChildLaunch::new(
                "/bin/sh",
                vec![
                    ShellChildArgument::Literal("-c".to_string()),
                    ShellChildArgument::Literal("printf '{\"child-pid\":%s}\\n' \"$$\" >&3; printf 'untrusted stdout'; printf 'launcher diagnostic' >&2; exit 7".to_string()),
                ],
            ).unwrap().with_status_fd(3).unwrap(),
        );
        let failure = execute_native_shell_dispatch(dispatch).result.unwrap_err();
        assert!(
            failure.message.contains("completion was not proven"),
            "{failure:?}"
        );
        assert!(
            !failure.message.contains("before payload execution"),
            "{failure:?}"
        );
        let evidence = failure.lifecycle.unwrap();
        assert_eq!(evidence.outer_exit_code, Some(7));
        assert_eq!(evidence.child_record_present, Some(true));
        assert_eq!(evidence.exit_record_present, Some(false));
        assert_eq!(evidence.stderr, "launcher diagnostic");
        assert!(!evidence.stderr.contains("untrusted stdout"));
    }

    /// Invalid, truncated and unclosed status cannot establish trusted record
    /// presence. Diagnostic stderr remains bounded/redacted and stdout is ignored.
    #[test]
    fn native_lifecycle_failures_classify_and_bound_evidence() {
        use crate::security::sandbox::SandboxLifecycleFailureClass as Class;
        let mut output = ShellExecutionOutput::new(
            Some(7),
            "{\"exit-code\":7}".to_string(),
            "diagnostic ".repeat(1000),
            false,
            false,
        );
        for (bytes, dropped, complete, class, child) in [
            (&b""[..], 0, true, Class::MissingExit, Some(false)),
            (
                &b"{\"child-pid\":1}\n"[..],
                0,
                true,
                Class::MissingExit,
                Some(true),
            ),
            (
                &b"{\"child-pid\":1}\n{"[..],
                0,
                true,
                Class::Malformed,
                None,
            ),
            (&b"\xff"[..], 0, true, Class::InvalidUtf8, None),
            (&b"{\"child-pid\":1}\n"[..], 1, true, Class::Truncated, None),
            (
                &b"{\"child-pid\":1}\n"[..],
                0,
                false,
                Class::Transport,
                None,
            ),
        ] {
            let error = validate_spawned_child_status(
                crate::runtime::SandboxBackend::Bubblewrap,
                bytes,
                dropped,
                complete,
                &output,
                0,
            )
            .unwrap_err();
            let evidence = error.sandbox_lifecycle_failure().unwrap();
            assert_eq!(evidence.class, class);
            assert_eq!(evidence.child_record_present, child);
            assert_eq!(evidence.outer_exit_code, Some(7));
            assert!(evidence.stderr.len() <= 4096);
            assert!(evidence.stderr_truncated);
        }
        output.stderr = "password = opaque-secret".to_string();
        for diagnostic in [
            r#"{"nested":{"password":"opaque-secret","access_token":"opaque-credential"}}"#,
            "launcher detail\n{\"nested\":{\"password\":\"opaque-secret\"}}",
            "launcher: {\"access_token\":\"opaque-credential\"}",
            "launcher: {\"password\":\"opaque-secret",
        ] {
            output.stderr = diagnostic.to_string();
            let error = validate_spawned_child_status(
                crate::runtime::SandboxBackend::Bubblewrap,
                b"",
                0,
                true,
                &output,
                0,
            )
            .unwrap_err();
            let evidence = error.sandbox_lifecycle_failure().unwrap();
            assert!(!evidence.stderr.contains("opaque-secret"), "{evidence:?}");
            assert!(
                !evidence.stderr.contains("opaque-credential"),
                "{evidence:?}"
            );
        }
        output.stderr = "password = opaque-secret".to_string();
        let error = validate_spawned_child_status(
            crate::runtime::SandboxBackend::Bubblewrap,
            b"",
            0,
            true,
            &output,
            0,
        )
        .unwrap_err();
        assert!(
            !error
                .sandbox_lifecycle_failure()
                .unwrap()
                .stderr
                .contains("opaque-secret")
        );
    }

    /// Seatbelt launcher failures and outer signals retain backend-specific
    /// trusted record facts without substituting an outer status for completion.
    #[test]
    fn native_lifecycle_failure_preserves_seatbelt_and_signal_facts() {
        let mut output = ShellExecutionOutput::new(
            None,
            String::new(),
            "wait diagnostic".to_string(),
            false,
            false,
        );
        output.signal = Some(libc::SIGTERM);
        let status = b"{\"version\":1,\"event\":\"sandbox-entered\"}\n{\"version\":1,\"event\":\"child-established\",\"child-pid\":42}\n{\"version\":1,\"event\":\"wait-failed\",\"code\":\"payload-wait\"}\n";
        let error = validate_spawned_child_status(
            crate::runtime::SandboxBackend::Seatbelt,
            status,
            0,
            true,
            &output,
            0,
        )
        .unwrap_err();
        let evidence = error.sandbox_lifecycle_failure().unwrap();
        assert_eq!(evidence.backend, "seatbelt");
        assert_eq!(
            evidence.class,
            crate::security::sandbox::SandboxLifecycleFailureClass::MissingExit
        );
        assert_eq!(evidence.outer_exit_code, None);
        assert_eq!(evidence.outer_signal, Some(libc::SIGTERM));
        assert_eq!(evidence.child_record_present, Some(true));
        assert_eq!(evidence.exit_record_present, Some(false));
    }

    /// Verifies direct typed-child execution supplies the selected status
    /// descriptor and accepts Bubblewrap-compatible lifecycle evidence when
    /// its reported payload status matches the spawned process status.
    #[test]
    fn spawned_executor_captures_matching_typed_child_status() {
        let mut executor = SpawnedShellExecutor::new(test_context());
        let mut transaction_request = request("printf status-fd-ok", Some(5_000));
        let status_script = "printf '{\"child-pid\":%s}\\n' \"$$\" >&3; /bin/sh \"$1\"; status=$?; printf '{\"exit-code\":%s}\\n' \"$status\" >&3; exit \"$status\"";
        transaction_request.transaction = transaction_request.transaction.with_child_launch(
            ShellChildLaunch::new(
                "/bin/sh",
                vec![
                    ShellChildArgument::Literal("-c".to_string()),
                    ShellChildArgument::Literal(status_script.to_string()),
                    ShellChildArgument::Literal("mez-status-child".to_string()),
                    ShellChildArgument::MaterializedCommandFile,
                ],
            )
            .unwrap()
            .with_status_fd(3)
            .unwrap(),
        );

        let output = executor.execute_shell(&transaction_request).unwrap();

        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.stdout, "status-fd-ok");
    }

    /// Verifies contradictory lifecycle evidence fails closed rather than
    /// presenting an untrusted spawned-process result as sandboxed output.
    #[test]
    fn spawned_executor_rejects_contradictory_typed_child_status() {
        let mut executor = SpawnedShellExecutor::new(test_context());
        let mut transaction_request = request("true", Some(5_000));
        let status_script =
            "printf '{\"child-pid\":%s}\\n{\"exit-code\":7}\\n' \"$$\" >&3; /bin/sh \"$1\"";
        transaction_request.transaction = transaction_request.transaction.with_child_launch(
            ShellChildLaunch::new(
                "/bin/sh",
                vec![
                    ShellChildArgument::Literal("-c".to_string()),
                    ShellChildArgument::Literal(status_script.to_string()),
                    ShellChildArgument::Literal("mez-status-child".to_string()),
                    ShellChildArgument::MaterializedCommandFile,
                ],
            )
            .unwrap()
            .with_status_fd(3)
            .unwrap(),
        );

        let error = executor.execute_shell(&transaction_request).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("contradicts the spawned process")
        );
    }

    /// Verifies stateful or interactive requests are rejected instead of
    /// silently degrading to non-stateful execution.
    #[test]
    fn spawned_executor_rejects_stateful_or_interactive_requests() {
        let mut executor = SpawnedShellExecutor::new(test_context());
        let mut interactive_request = request("true", Some(5_000));
        interactive_request.interactive = true;
        let error = executor.execute_shell(&interactive_request).unwrap_err();
        assert!(error.to_string().contains("stateful or interactive"));

        let mut stateful_request = request("true", Some(5_000));
        stateful_request.stateful = true;
        let error = executor.execute_shell(&stateful_request).unwrap_err();
        assert!(error.to_string().contains("stateful or interactive"));
    }

    /// Verifies spawned materialization appends input sidecar records with
    /// the apply-patch line prefix so generated write-phase scripts can
    /// extract final-content chunks from their own command file.
    #[test]
    fn spawned_executor_materializes_apply_patch_sidecar_records() {
        let mut executor = SpawnedShellExecutor::new(test_context());
        let mut sidecar_request = request("true", Some(5_000));
        sidecar_request.transaction.command =
            "sed -n 's/^# __MEZ_INPUT_SIDECAR_V1__ 0 //p' \"$0\"; sed -n 's/^# __MEZ_INPUT_SIDECAR_V1__ 1 //p' \"$0\""
                .to_string();
        sidecar_request.transaction = sidecar_request
            .transaction
            .with_input_sidecar(Some("0 YXJjaGl2ZQ==\n1 AQID\n".to_string()));
        let output = executor.execute_shell(&sidecar_request).unwrap();

        assert_eq!(output.exit_code, Some(0), "stderr={}", output.stderr);
        assert!(
            output.stdout.contains("YXJjaGl2ZQ=="),
            "stdout={}",
            output.stdout
        );
        assert!(output.stdout.contains("AQID"), "stdout={}", output.stdout);
    }
}
