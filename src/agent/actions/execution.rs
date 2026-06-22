//! Shell and MCP action execution helpers.
//!
//! This module owns the boundary between planned agent actions and the
//! executor interfaces supplied by the runtime. It converts shell/MCP executor
//! outputs back into durable `ActionResult` values while keeping pane and MCP
//! I/O details out of turn negotiation.

use super::super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentTurnRecord,
    DEFAULT_TOOL_DISCOVERY_TIMEOUT_MS, EnvironmentSignature, LocalActionKind, LocalActionPlan,
    MarkerToken, McpToolCallPlan, McpToolCallResponse, MezError, Path, Result, ShellTransaction,
    ShellTransactionOutputTransport, ToolDiscoveryCache, ToolInventory,
    action_content_blocks_from_json_or_text, action_text_content_blocks, apply_patch_natively,
    json_escape, local_action_plan, tool_discovery_script,
};
use super::{
    ShellTransportDiagnostics, decode_shell_output_transport_with_diagnostics,
    shell_command_structured_content_json,
};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
use std::io::Read;
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use wait_timeout::ChildExt;

/// Maximum bytes retained from one native stdout or stderr stream.
const NATIVE_SHELL_OUTPUT_LIMIT_BYTES: usize = 256 * 1024;
/// Poll cadence used while monitoring native children and pipe readers.
const NATIVE_SHELL_POLL_INTERVAL_MS: u64 = 10;
/// Grace period between TERM and KILL when timing out a native command.
const NATIVE_SHELL_TIMEOUT_KILL_GRACE_MS: u64 = 100;
/// Grace period that lets native pipe readers drain buffered output after exit.
const NATIVE_SHELL_READER_SHUTDOWN_GRACE_MS: u64 = 100;
/// Default turn-wide shell action timeout used by transport-neutral execution.
const LOCAL_EXECUTION_DEFAULT_TIMEOUT_MS: u64 = 30 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Carries shell execution request state for this subsystem.
///
/// The fields are kept explicit so callers can inspect and move structured
/// runtime data without parsing display text.
pub struct ShellExecutionRequest {
    /// Structured `action_id` value carried by this API type.
    pub action_id: String,
    /// Structured `transaction` value carried by this API type.
    pub transaction: ShellTransaction,
    /// Structured `timeout_ms` value carried by this API type.
    pub timeout_ms: Option<u64>,
    /// Structured `interactive` value carried by this API type.
    pub interactive: bool,
    /// Structured `stateful` value carried by this API type.
    pub stateful: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Carries shell execution output state for this subsystem.
///
/// The fields are kept explicit so callers can inspect and move structured
/// runtime data without parsing display text.
pub struct ShellExecutionOutput {
    /// Structured `exit_code` value carried by this API type.
    pub exit_code: Option<i32>,
    /// Structured `stdout` value carried by this API type.
    pub stdout: String,
    /// Structured `stderr` value carried by this API type.
    pub stderr: String,
    /// Structured `timed_out` value carried by this API type.
    pub timed_out: bool,
    /// Structured `interrupted` value carried by this API type.
    pub interrupted: bool,
    /// Structured transport diagnostics kept separate from command output.
    pub transport_diagnostics: ShellTransportDiagnostics,
}

impl ShellExecutionOutput {
    /// Builds shell execution output with no transport diagnostics.
    ///
    /// Callers that already provide decoded or non-transported output can use
    /// this constructor without fabricating empty diagnostic state at each
    /// call site.
    pub fn new(
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
        timed_out: bool,
        interrupted: bool,
    ) -> Self {
        Self {
            exit_code,
            stdout,
            stderr,
            timed_out,
            interrupted,
            transport_diagnostics: ShellTransportDiagnostics::default(),
        }
    }
}

/// Defines the `PaneShellExecutor` behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary used by
/// higher-level runtime code.
pub trait PaneShellExecutor {
    /// Runs the execute shell operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in the
    /// owning module so callers receive typed results instead of relying on
    /// duplicated control-flow logic.
    fn execute_shell(&mut self, request: &ShellExecutionRequest) -> Result<ShellExecutionOutput>;
}

/// Identifies the runtime transport used to execute a local action plan.
///
/// Local actions keep their model-facing MAAP action names regardless of this
/// value. The transport is selected by runtime code after planning and policy
/// checks have accepted an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalExecutionTransport {
    /// Execute the local action by dispatching a transaction to the pane shell.
    PaneShell,
    /// Execute the local action through a native runtime executor.
    Native,
}

impl LocalExecutionTransport {
    /// Returns the stable string recorded in structured action-result metadata.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PaneShell => "pane_shell",
            Self::Native => "native",
        }
    }
}

/// Reports whether a pane shell environment is equivalent to native runtime execution.
///
/// Native local execution uses this state for launch-time diagnostics so users
/// can see when local actions may target a different host or filesystem view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentEquivalence {
    /// Required identity fields match and no uncertainty remains.
    Equivalent,
    /// Some evidence matches, but required proof is incomplete.
    ProbablyEquivalent,
    /// Required identity fields differ.
    Different,
    /// The runtime lacks enough evidence to compare the environments.
    Unknown,
}

impl EnvironmentEquivalence {
    /// Returns the stable string used in diagnostics and structured results.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Equivalent => "equivalent",
            Self::ProbablyEquivalent => "probably_equivalent",
            Self::Different => "different",
            Self::Unknown => "unknown",
        }
    }
}

/// Records the evidence used by native environment-equivalence diagnostics.
///
/// The diagnostic intentionally names the compared fields instead of reducing
/// failures to a boolean so pane-visible warnings can explain why native
/// execution may not match the pane shell environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentEquivalenceProbe {
    /// Final equivalence state derived from the required comparisons.
    pub equivalence: EnvironmentEquivalence,
    /// Human-readable comparison details safe for model-facing diagnostics.
    pub diagnostics: Vec<String>,
}

impl EnvironmentEquivalenceProbe {
    /// Compares the pane bootstrap signature with the native runtime context.
    pub fn compare(
        pane_signature: Option<&EnvironmentSignature>,
        native_working_directory: &std::path::Path,
    ) -> Self {
        let Some(pane_signature) = pane_signature else {
            return Self::new(
                EnvironmentEquivalence::Unknown,
                vec!["pane environment signature is unavailable".to_string()],
            );
        };
        if pane_signature.is_unknown() {
            return Self::new(
                EnvironmentEquivalence::Unknown,
                vec!["pane environment signature is unknown".to_string()],
            );
        }

        let native_cwd = match native_working_directory.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                return Self::new(
                    EnvironmentEquivalence::Unknown,
                    vec![format!(
                        "native working directory cannot be resolved: {error}"
                    )],
                );
            }
        };
        let pane_cwd = match std::path::Path::new(&pane_signature.working_directory).canonicalize()
        {
            Ok(path) => path,
            Err(error) => {
                return Self::new(
                    EnvironmentEquivalence::Unknown,
                    vec![format!(
                        "pane working directory cannot be resolved: {error}"
                    )],
                );
            }
        };

        if native_cwd != pane_cwd {
            return Self::new(
                EnvironmentEquivalence::Different,
                vec![format!(
                    "working_directory mismatch: pane={} native={}",
                    pane_cwd.display(),
                    native_cwd.display()
                )],
            );
        }

        let native_os = std::env::consts::OS;
        let native_arch = std::env::consts::ARCH;
        let mut diagnostics = vec![format!("working_directory={}", native_cwd.display())];
        let mut different = Vec::new();
        if !pane_signature.os.eq_ignore_ascii_case(native_os) {
            different.push(format!(
                "os mismatch: pane={} native={native_os}",
                pane_signature.os
            ));
        }
        if !pane_signature.arch.eq_ignore_ascii_case(native_arch) {
            different.push(format!(
                "arch mismatch: pane={} native={native_arch}",
                pane_signature.arch
            ));
        }
        if !different.is_empty() {
            return Self::new(EnvironmentEquivalence::Different, different);
        }
        diagnostics.push(format!("os={native_os}"));
        diagnostics.push(format!("arch={native_arch}"));
        Self::new(EnvironmentEquivalence::Equivalent, diagnostics)
    }

    fn new(equivalence: EnvironmentEquivalence, diagnostics: Vec<String>) -> Self {
        Self {
            equivalence,
            diagnostics,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Carries transport-neutral local action execution request state.
///
/// The planned command remains available to every transport, while
/// pane-specific wrapper state is supplied only by the pane-shell adapter.
pub struct LocalExecutionRequest {
    /// Structured `action_id` value carried by this API type.
    pub action_id: String,
    /// Original MAAP action whose model-facing shape must stay transport-neutral.
    pub action: AgentAction,
    /// Structured `turn_id` value needed by transports that render wrappers.
    pub turn_id: String,
    /// Structured `agent_id` value needed by transports that render wrappers.
    pub agent_id: String,
    /// Structured `pane_id` value needed by transports that render wrappers.
    pub pane_id: String,
    /// Planned local action semantics selected before transport dispatch.
    pub plan: LocalActionPlan,
    /// Effective finite timeout after applying the enclosing turn budget.
    pub effective_timeout_ms: u64,
    /// Runtime transport selected for this local action.
    pub transport: LocalExecutionTransport,
    /// Marker token used by transports that need a command-output boundary.
    pub marker: MarkerToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Carries transport-neutral local action execution output state.
///
/// The output mirrors shell execution data so existing action-result recovery
/// and display logic can be reused while the transport choice stays explicit.
pub struct LocalExecutionOutput {
    /// Runtime transport that produced this output.
    pub transport: LocalExecutionTransport,
    /// Whether this execution sent input to the pane shell.
    pub sent_to_pane: bool,
    /// Shell-shaped output returned by the selected local action transport.
    pub shell_output: ShellExecutionOutput,
}

impl LocalExecutionOutput {
    /// Builds output for a pane-shell local action execution.
    pub fn pane_shell(shell_output: ShellExecutionOutput) -> Self {
        Self {
            transport: LocalExecutionTransport::PaneShell,
            sent_to_pane: true,
            shell_output,
        }
    }

    /// Builds output for a native local action execution.
    pub fn native(shell_output: ShellExecutionOutput) -> Self {
        Self {
            transport: LocalExecutionTransport::Native,
            sent_to_pane: false,
            shell_output,
        }
    }
}

/// Defines the local action executor behavior contract for this subsystem.
///
/// Implementors select the concrete transport for an already-planned local
/// action without changing the model-facing action interface.
pub trait LocalActionExecutor {
    /// Runs one planned local action through the selected runtime transport.
    fn execute_local_action(
        &mut self,
        request: &LocalExecutionRequest,
    ) -> Result<LocalExecutionOutput>;
}

/// Adapts the existing pane shell executor to the transport-neutral executor
/// contract.
pub struct PaneShellLocalExecutor<'a, E> {
    shell_path: &'a Path,
    pane_executor: &'a mut E,
}

impl<'a, E> PaneShellLocalExecutor<'a, E> {
    /// Builds an adapter around a pane shell executor and shell path.
    pub fn new(shell_path: &'a Path, pane_executor: &'a mut E) -> Self {
        Self {
            shell_path,
            pane_executor,
        }
    }
}

impl<E> LocalActionExecutor for PaneShellLocalExecutor<'_, E>
where
    E: PaneShellExecutor,
{
    fn execute_local_action(
        &mut self,
        request: &LocalExecutionRequest,
    ) -> Result<LocalExecutionOutput> {
        let transaction = ShellTransaction::new(
            request.marker.clone(),
            &request.turn_id,
            &request.agent_id,
            &request.pane_id,
            self.shell_path,
            &request.plan.command,
        )?
        .with_output_transport(ShellTransactionOutputTransport::Base64);
        let shell_request = ShellExecutionRequest {
            action_id: request.action_id.clone(),
            transaction,
            timeout_ms: Some(request.effective_timeout_ms),
            interactive: request.plan.interactive,
            stateful: request.plan.stateful,
        };
        self.pane_executor
            .execute_shell(&shell_request)
            .map(LocalExecutionOutput::pane_shell)
    }
}

/// Receives a bounded cumulative preview of native shell output while a
/// command is still running.
type NativeShellOutputProgressCallback<'a> = dyn FnMut(&str) -> Result<()> + 'a;

/// Executes native-eligible local actions without dispatching through the pane
/// shell.
pub struct NativeShellLocalExecutor<'a> {
    shell_path: std::path::PathBuf,
    working_directory: std::path::PathBuf,
    output_progress: Option<Box<NativeShellOutputProgressCallback<'a>>>,
}

impl<'a> NativeShellLocalExecutor<'a> {
    /// Builds a native shell-command executor rooted at one working directory.
    pub fn new(
        shell_path: impl Into<std::path::PathBuf>,
        working_directory: impl AsRef<std::path::Path>,
    ) -> Self {
        Self {
            shell_path: shell_path.into(),
            working_directory: working_directory.as_ref().to_path_buf(),
            output_progress: None,
        }
    }

    /// Installs a progress callback that receives cumulative native shell
    /// output previews while shell commands are still running.
    pub fn with_output_progress(
        mut self,
        output_progress: impl FnMut(&str) -> Result<()> + 'a,
    ) -> Self {
        self.output_progress = Some(Box::new(output_progress));
        self
    }
}

impl LocalActionExecutor for NativeShellLocalExecutor<'_> {
    fn execute_local_action(
        &mut self,
        request: &LocalExecutionRequest,
    ) -> Result<LocalExecutionOutput> {
        match request.plan.kind {
            LocalActionKind::ShellCommand => {
                if request.plan.interactive {
                    return Err(MezError::invalid_args(
                        "native shell_command execution does not support interactive actions",
                    ));
                }
                if request.plan.stateful {
                    return Err(MezError::invalid_args(
                        "native shell_command execution does not support stateful actions",
                    ));
                }
                execute_native_shell_command(
                    &self.shell_path,
                    &self.working_directory,
                    request,
                    self.output_progress.as_deref_mut(),
                )
                .map(LocalExecutionOutput::native)
            }
            LocalActionKind::ApplyPatch => execute_native_apply_patch(
                &request.action,
                &self.working_directory,
                request.effective_timeout_ms,
            )
            .map(LocalExecutionOutput::native),
        }
    }
}

fn execute_native_apply_patch(
    action: &AgentAction,
    working_directory: &std::path::Path,
    timeout_ms: u64,
) -> Result<ShellExecutionOutput> {
    let AgentActionPayload::ApplyPatch { patch, strip } = &action.payload else {
        return Err(MezError::invalid_args(
            "native apply_patch execution requires an apply_patch action",
        ));
    };
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    match apply_patch_natively(patch, *strip, working_directory, Some(deadline)) {
        Ok(()) => Ok(ShellExecutionOutput::new(
            Some(0),
            String::new(),
            String::new(),
            false,
            false,
        )),
        Err(error) if error.message().contains("apply_patch timed out") => {
            Ok(ShellExecutionOutput::new(
                None,
                String::new(),
                format!("{}\n", error.message()),
                true,
                false,
            ))
        }
        Err(error) => Ok(ShellExecutionOutput::new(
            Some(1),
            String::new(),
            format!("{}\n", error.message()),
            false,
            false,
        )),
    }
}

fn execute_native_shell_command(
    shell_path: &std::path::Path,
    working_directory: &std::path::Path,
    request: &LocalExecutionRequest,
    output_progress: Option<&mut NativeShellOutputProgressCallback<'_>>,
) -> Result<ShellExecutionOutput> {
    let mut command = Command::new(shell_path);
    command
        .arg("-lc")
        .arg(&request.plan.command)
        .current_dir(working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: `pre_exec` runs in the child immediately before `exec`. Calling
    // `setsid` only detaches the native command into its own session so timeout
    // cleanup can signal the full descendant process group without touching
    // shared Rust state in the parent.
    unsafe {
        command.pre_exec(|| rustix::process::setsid().map(|_| ()).map_err(Into::into));
    }
    let mut child = command.spawn().map_err(|error| {
        MezError::new(
            crate::error::MezErrorKind::Io,
            format!(
                "failed to spawn native shell_command `{}`: {error}",
                request.action_id
            ),
        )
    })?;
    let (event_tx, event_rx) = mpsc::channel();
    let (stdout_cancel_tx, stdout_reader) = spawn_child_pipe_reader(
        NativePipeStream::Stdout,
        child.stdout.take(),
        NATIVE_SHELL_OUTPUT_LIMIT_BYTES,
        event_tx.clone(),
    );
    let (stderr_cancel_tx, stderr_reader) = spawn_child_pipe_reader(
        NativePipeStream::Stderr,
        child.stderr.take(),
        NATIVE_SHELL_OUTPUT_LIMIT_BYTES,
        event_tx,
    );
    let deadline = Instant::now() + Duration::from_millis(request.effective_timeout_ms.max(1));
    let poll_interval = Duration::from_millis(NATIVE_SHELL_POLL_INTERVAL_MS);
    let reader_shutdown_grace = Duration::from_millis(NATIVE_SHELL_READER_SHUTDOWN_GRACE_MS);
    let process_group_leader = i32::try_from(child.id()).ok().and_then(Pid::from_raw);
    let mut exit_status = None;
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut timed_out = false;
    let mut progress = NativeShellOutputProgress::new(output_progress);
    loop {
        drain_child_pipe_events(&event_rx, &mut stdout_done, &mut stderr_done, &mut progress)?;
        if exit_status.is_some()
            && (stdout_done && stderr_done
                || native_child_process_group_exited(process_group_leader)?)
        {
            break;
        }
        let now = Instant::now();
        if now >= deadline {
            timed_out = true;
            break;
        }
        let wait = std::cmp::min(deadline.saturating_duration_since(now), poll_interval);
        if exit_status.is_none() {
            if let Some(status) = child.wait_timeout(wait)? {
                exit_status = Some(status);
            }
        } else {
            wait_for_child_pipe_event(
                &event_rx,
                wait,
                &mut stdout_done,
                &mut stderr_done,
                &mut progress,
            )?;
        }
        if Instant::now() >= deadline && !(exit_status.is_some() && stdout_done && stderr_done) {
            timed_out = true;
            break;
        }
    }
    if timed_out {
        terminate_child_process_group(&mut child)?;
        if exit_status.is_none() {
            exit_status = Some(child.wait()?);
        }
    }
    let shutdown_deadline = Instant::now() + reader_shutdown_grace;
    while !(stdout_done && stderr_done) {
        let now = Instant::now();
        if now >= shutdown_deadline {
            break;
        }
        let wait = std::cmp::min(
            shutdown_deadline.saturating_duration_since(now),
            poll_interval,
        );
        wait_for_child_pipe_event(
            &event_rx,
            wait,
            &mut stdout_done,
            &mut stderr_done,
            &mut progress,
        )?;
    }
    if !stdout_done {
        request_child_pipe_reader_shutdown(&stdout_cancel_tx);
    }
    if !stderr_done {
        request_child_pipe_reader_shutdown(&stderr_cancel_tx);
    }
    let stdout = join_child_pipe_reader(NativePipeStream::Stdout, stdout_reader)?;
    let stderr = join_child_pipe_reader(NativePipeStream::Stderr, stderr_reader)?;
    let mut output = ShellExecutionOutput::new(
        exit_status.and_then(|status| status.code()),
        stdout.text,
        stderr.text,
        timed_out,
        false,
    );
    output.transport_diagnostics.output_bytes_dropped =
        stdout.bytes_dropped.saturating_add(stderr.bytes_dropped);
    Ok(output)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Identifies one native stdio stream drained by a background reader.
enum NativePipeStream {
    /// Standard output from the native child process tree.
    Stdout,
    /// Standard error from the native child process tree.
    Stderr,
}

impl NativePipeStream {
    /// Returns the stable display label used in diagnostics.
    fn label(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

#[derive(Debug)]
/// Bounded text captured from one native child pipe plus dropped-byte metadata.
struct CapturedPipeOutput {
    /// UTF-8 text retained for the model-facing action result.
    text: String,
    /// Bytes discarded after the retained output reached its configured cap.
    bytes_dropped: usize,
}

/// Reports native child pipe reader events to the parent monitor loop.
enum NativePipeEvent {
    /// Retained output bytes read from one pipe.
    Output(Vec<u8>),
    /// One pipe reader reached EOF or stopped after cancellation.
    Done(NativePipeStream),
}

/// Maintains and reports the cumulative native shell output preview.
struct NativeShellOutputProgress<'callback, 'env> {
    output_progress: Option<&'callback mut NativeShellOutputProgressCallback<'env>>,
    preview: Vec<u8>,
}

impl<'callback, 'env> NativeShellOutputProgress<'callback, 'env> {
    /// Builds progress state around an optional runtime callback.
    fn new(
        output_progress: Option<&'callback mut NativeShellOutputProgressCallback<'env>>,
    ) -> Self {
        Self {
            output_progress,
            preview: Vec::new(),
        }
    }

    /// Appends retained bytes and reports the decoded cumulative preview.
    fn record_output(&mut self, bytes: Vec<u8>) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.preview.extend_from_slice(&bytes);
        if let Some(output_progress) = self.output_progress.as_deref_mut() {
            let text = String::from_utf8_lossy(&self.preview);
            output_progress(&text)?;
        }
        Ok(())
    }
}

/// Spawns a background reader that drains one native child pipe to EOF.
fn spawn_child_pipe_reader<R>(
    stream: NativePipeStream,
    pipe: Option<R>,
    max_bytes: usize,
    event_tx: mpsc::Sender<NativePipeEvent>,
) -> (
    mpsc::Sender<()>,
    thread::JoinHandle<Result<CapturedPipeOutput>>,
)
where
    R: Read + AsFd + Send + 'static,
{
    let (cancel_tx, cancel_rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let result = read_child_pipe(stream, pipe, max_bytes, cancel_rx, &event_tx);
        let _ = event_tx.send(NativePipeEvent::Done(stream));
        result
    });
    (cancel_tx, reader)
}

/// Drains any available native pipe reader events without blocking.
fn drain_child_pipe_events(
    event_rx: &mpsc::Receiver<NativePipeEvent>,
    stdout_done: &mut bool,
    stderr_done: &mut bool,
    progress: &mut NativeShellOutputProgress<'_, '_>,
) -> Result<()> {
    while let Ok(event) = event_rx.try_recv() {
        record_child_pipe_event(event, stdout_done, stderr_done, progress)?;
    }
    Ok(())
}

/// Waits briefly for one reader event and records any immediately queued events.
fn wait_for_child_pipe_event(
    event_rx: &mpsc::Receiver<NativePipeEvent>,
    wait: Duration,
    stdout_done: &mut bool,
    stderr_done: &mut bool,
    progress: &mut NativeShellOutputProgress<'_, '_>,
) -> Result<()> {
    match event_rx.recv_timeout(wait) {
        Ok(event) => record_child_pipe_event(event, stdout_done, stderr_done, progress)?,
        Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
    }
    drain_child_pipe_events(event_rx, stdout_done, stderr_done, progress)
}

/// Records one native pipe reader event.
fn record_child_pipe_event(
    event: NativePipeEvent,
    stdout_done: &mut bool,
    stderr_done: &mut bool,
    progress: &mut NativeShellOutputProgress<'_, '_>,
) -> Result<()> {
    match event {
        NativePipeEvent::Output(bytes) => progress.record_output(bytes)?,
        NativePipeEvent::Done(NativePipeStream::Stdout) => *stdout_done = true,
        NativePipeEvent::Done(NativePipeStream::Stderr) => *stderr_done = true,
    }
    Ok(())
}

/// Requests that one native pipe reader stop waiting for additional bytes.
fn request_child_pipe_reader_shutdown(cancel_tx: &mpsc::Sender<()>) {
    let _ = cancel_tx.send(());
}

/// Joins one native reader thread and converts panics into typed errors.
fn join_child_pipe_reader(
    stream: NativePipeStream,
    reader: thread::JoinHandle<Result<CapturedPipeOutput>>,
) -> Result<CapturedPipeOutput> {
    match reader.join() {
        Ok(result) => result,
        Err(_) => Err(MezError::new(
            crate::error::MezErrorKind::Io,
            format!(
                "native shell_command {} reader thread panicked",
                stream.label()
            ),
        )),
    }
}

/// Reports whether the native child process group has fully exited.
fn native_child_process_group_exited(process_group_leader: Option<Pid>) -> Result<bool> {
    let Some(process_group_leader) = process_group_leader else {
        return Ok(false);
    };
    match test_kill_process_group(process_group_leader) {
        Ok(()) => Ok(false),
        Err(Errno::SRCH) => Ok(true),
        Err(error) => Err(MezError::new(
            crate::error::MezErrorKind::Io,
            format!(
                "failed to probe native child process group {}: {error}",
                process_group_leader.as_raw_nonzero().get(),
            ),
        )),
    }
}

/// Enables nonblocking reads on one native child pipe.
fn configure_child_pipe_nonblocking<R: AsFd>(stream: NativePipeStream, pipe: &R) -> Result<()> {
    let borrowed = pipe.as_fd();
    let flags = fcntl_getfl(borrowed).map_err(|error| {
        MezError::new(
            crate::error::MezErrorKind::Io,
            format!(
                "failed to inspect native shell_command {} pipe flags: {error}",
                stream.label()
            ),
        )
    })?;
    if !flags.contains(OFlags::NONBLOCK) {
        fcntl_setfl(borrowed, flags | OFlags::NONBLOCK).map_err(|error| {
            MezError::new(
                crate::error::MezErrorKind::Io,
                format!(
                    "failed to enable nonblocking mode for native shell_command {} pipe: {error}",
                    stream.label()
                ),
            )
        })?;
    }
    Ok(())
}

/// Signals the timed-out native child session and its descendants.
fn terminate_child_process_group(child: &mut std::process::Child) -> Result<()> {
    if let Some(process_group_leader) = i32::try_from(child.id()).ok().and_then(Pid::from_raw) {
        send_signal_to_process_group(process_group_leader, Signal::TERM)?;
        thread::sleep(Duration::from_millis(NATIVE_SHELL_TIMEOUT_KILL_GRACE_MS));
        let _ = send_signal_to_process_group(process_group_leader, Signal::KILL);
    }
    let _ = child.kill();
    Ok(())
}

/// Sends one signal to the native child process group, tolerating races.
fn send_signal_to_process_group(process_group_leader: Pid, signal: Signal) -> Result<()> {
    match kill_process_group(process_group_leader, signal) {
        Ok(()) | Err(Errno::SRCH) => Ok(()),
        Err(error) => Err(MezError::new(
            crate::error::MezErrorKind::Io,
            format!(
                "failed to send signal {} to native child process group {}: {error}",
                signal.as_raw(),
                process_group_leader.as_raw_nonzero().get(),
            ),
        )),
    }
}

/// Drains one native child pipe while bounding retained bytes.
fn read_child_pipe<R: Read + AsFd>(
    stream: NativePipeStream,
    mut pipe: Option<R>,
    max_bytes: usize,
    cancel_rx: mpsc::Receiver<()>,
    event_tx: &mpsc::Sender<NativePipeEvent>,
) -> Result<CapturedPipeOutput> {
    let mut bytes = Vec::new();
    let mut bytes_dropped = 0usize;
    let mut buffer = [0u8; 8192];
    if let Some(pipe) = pipe.as_mut() {
        configure_child_pipe_nonblocking(stream, pipe)?;
        loop {
            match cancel_rx.try_recv() {
                Ok(()) | Err(mpsc::TryRecvError::Disconnected) => break,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let retained = std::cmp::min(max_bytes.saturating_sub(bytes.len()), read);
                    if retained > 0 {
                        let retained_bytes = buffer[..retained].to_vec();
                        bytes.extend_from_slice(&retained_bytes);
                        let _ = event_tx.send(NativePipeEvent::Output(retained_bytes));
                    }
                    bytes_dropped = bytes_dropped.saturating_add(read.saturating_sub(retained));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    match cancel_rx
                        .recv_timeout(Duration::from_millis(NATIVE_SHELL_POLL_INTERVAL_MS))
                    {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(CapturedPipeOutput {
        text: String::from_utf8_lossy(&bytes).into_owned(),
        bytes_dropped,
    })
}

/// Defines the `McpActionExecutor` behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary used by
/// higher-level runtime code.
pub trait McpActionExecutor {
    /// Runs the execute mcp call operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in the
    /// owning module so callers receive typed results instead of relying on
    /// duplicated control-flow logic.
    fn execute_mcp_call(&mut self, plan: &McpToolCallPlan) -> Result<McpToolCallResponse>;
}

#[allow(async_fn_in_trait)]
/// Defines the `AsyncMcpActionExecutor` behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary used by
/// higher-level runtime code.
pub trait AsyncMcpActionExecutor {
    /// Runs the execute mcp call async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in the
    /// owning module so callers receive typed results instead of relying on
    /// duplicated control-flow logic.
    async fn execute_mcp_call_async(
        &mut self,
        plan: &McpToolCallPlan,
    ) -> Result<McpToolCallResponse>;
}

/// Executes the `execute_shell_action_through_pane` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn execute_shell_action_through_pane(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    marker: MarkerToken,
    shell_path: &Path,
    executor: &mut impl PaneShellExecutor,
) -> Result<ActionResult> {
    let mut local_executor = PaneShellLocalExecutor::new(shell_path, executor);
    execute_local_action(turn, action, marker, &mut local_executor)
}

/// Executes a local action through the supplied transport-neutral executor.
///
/// Callers receive the same `ActionResult` shape regardless of the transport
/// that ran the planned local action.
pub fn execute_local_action(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    marker: MarkerToken,
    executor: &mut impl LocalActionExecutor,
) -> Result<ActionResult> {
    let Some(plan) = local_action_plan(action)? else {
        return Err(MezError::invalid_args(
            "local execution requires a local action",
        ));
    };
    let effective_timeout_ms = local_execution_shell_timeout_ms(turn, plan.timeout_ms);
    let request = LocalExecutionRequest {
        action_id: action.id.clone(),
        action: action.clone(),
        turn_id: turn.turn_id.clone(),
        agent_id: turn.agent_id.clone(),
        pane_id: turn.pane_id.clone(),
        plan,
        effective_timeout_ms,
        transport: LocalExecutionTransport::PaneShell,
        marker: marker.clone(),
    };
    let mut output = executor.execute_local_action(&request)?;
    output.shell_output = postprocess_semantic_shell_output(action, output.shell_output)?;
    local_output_to_action_result(turn, action, output, marker)
}

/// Returns the remaining turn-wide timeout budget for transport-neutral local execution.
fn local_execution_turn_remaining_timeout_ms(turn: &AgentTurnRecord) -> u64 {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0);
    if turn.started_at_unix_seconds < 946_684_800 {
        return LOCAL_EXECUTION_DEFAULT_TIMEOUT_MS;
    }
    let started_at_ms = turn.started_at_unix_seconds.saturating_mul(1000);
    let elapsed_ms = now_ms.saturating_sub(started_at_ms);
    LOCAL_EXECUTION_DEFAULT_TIMEOUT_MS
        .saturating_sub(elapsed_ms)
        .max(1)
}

/// Returns the finite shell timeout for one local execution request.
fn local_execution_shell_timeout_ms(turn: &AgentTurnRecord, timeout_ms: Option<u64>) -> u64 {
    let remaining = local_execution_turn_remaining_timeout_ms(turn);
    timeout_ms
        .map(|timeout_ms| timeout_ms.min(remaining))
        .unwrap_or(remaining)
        .max(1)
}

/// Applies native success-output shaping for shell-backed semantic actions.
///
/// Pane-side semantic commands stay limited to small shell primitives. Line
/// slicing, truncation notices, and generated change previews are applied here
/// after the pane shell returns its bounded output.
pub fn postprocess_shell_action_success_output(
    action: &AgentAction,
    stdout: String,
) -> Result<String> {
    let output = ShellExecutionOutput::new(Some(0), stdout, String::new(), false, false);
    postprocess_semantic_shell_output(action, output).map(|output| output.stdout)
}

/// Builds compact action-result content for a plain model-authored shell command.
///
/// # Parameters
/// - `output`: The command stdout/stderr already decoded for model context.
/// - `exit_code`: The observed process exit code, when one was observed.
/// - `timed_out`: Whether the command timed out before a process exit.
/// - `interrupted`: Whether the command was interrupted by the runtime.
pub fn shell_command_result_content(
    output: &str,
    exit_code: Option<i32>,
    timed_out: bool,
    interrupted: bool,
) -> Vec<String> {
    if !output.trim().is_empty() {
        return vec![output.to_string()];
    }
    let status = if timed_out {
        "shell command timed out".to_string()
    } else if interrupted {
        "shell command was interrupted".to_string()
    } else if let Some(exit_code) = exit_code {
        format!("shell command exited with status {exit_code}")
    } else {
        "shell command finished without an exit status".to_string()
    };
    vec![status]
}

fn postprocess_semantic_shell_output(
    action: &AgentAction,
    mut output: ShellExecutionOutput,
) -> Result<ShellExecutionOutput> {
    let decoded = decode_shell_output_transport_with_diagnostics(&output.stdout);
    if decoded.diagnostics.saw_begin_marker {
        output.stdout = decoded.output;
        output.transport_diagnostics = decoded.diagnostics;
    }
    if output.exit_code != Some(0) || output.timed_out || output.interrupted {
        return Ok(output);
    }
    if let AgentActionPayload::ApplyPatch { patch, .. } = &action.payload {
        ensure_success_preview(&mut output, patch_change_preview(patch));
    }
    Ok(output)
}

fn ensure_success_preview(output: &mut ShellExecutionOutput, preview: String) {
    if output.stdout.trim().is_empty() {
        output.stdout = preview;
    }
}

fn patch_change_preview(patch: &str) -> String {
    const MAX_PREVIEW_LINES: usize = 160;
    let mut lines = vec!["diff -- apply patch".to_string()];
    for line in patch.lines().take(MAX_PREVIEW_LINES) {
        lines.push(line.to_string());
    }
    let total_lines = patch.lines().count();
    if total_lines > MAX_PREVIEW_LINES {
        lines.push(format!(
            "[mez: diff truncated; {} lines omitted]",
            total_lines - MAX_PREVIEW_LINES
        ));
    }
    lines.join("\n") + "\n"
}

/// Executes the `execute_mcp_action_through_runtime` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn execute_mcp_action_through_runtime(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    plan: &McpToolCallPlan,
    executor: &mut impl McpActionExecutor,
) -> Result<ActionResult> {
    let AgentActionPayload::McpCall {
        server,
        tool,
        arguments_json,
    } = &action.payload
    else {
        return Err(MezError::invalid_args(
            "MCP execution requires an mcp_call action",
        ));
    };
    if plan.server_id != *server
        || plan.tool_name != *tool
        || plan.arguments_json.trim() != arguments_json.trim()
    {
        return Err(MezError::invalid_args(
            "MCP execution plan does not match the action payload",
        ));
    }

    let response = executor.execute_mcp_call(plan)?;
    mcp_response_to_action_result(turn, action, plan, response)
}

/// Executes the `execute_mcp_action_through_runtime_async` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub async fn execute_mcp_action_through_runtime_async(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    plan: &McpToolCallPlan,
    executor: &mut impl AsyncMcpActionExecutor,
) -> Result<ActionResult> {
    let AgentActionPayload::McpCall {
        server,
        tool,
        arguments_json,
    } = &action.payload
    else {
        return Err(MezError::invalid_args(
            "MCP execution requires an mcp_call action",
        ));
    };
    if plan.server_id != *server
        || plan.tool_name != *tool
        || plan.arguments_json.trim() != arguments_json.trim()
    {
        return Err(MezError::invalid_args(
            "MCP execution plan does not match the action payload",
        ));
    }

    let response = executor.execute_mcp_call_async(plan).await?;
    mcp_response_to_action_result(turn, action, plan, response)
}

/// Executes the `discover_tools_through_pane_shell` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub fn discover_tools_through_pane_shell(
    cache: &mut ToolDiscoveryCache,
    signature: EnvironmentSignature,
    turn: &AgentTurnRecord,
    marker: MarkerToken,
    shell_path: &Path,
    executor: &mut impl PaneShellExecutor,
) -> Result<ToolInventory> {
    if let Some(inventory) = cache.get(&signature) {
        return Ok(inventory.clone());
    }

    let transaction = ShellTransaction::new(
        marker,
        &turn.turn_id,
        &turn.agent_id,
        &turn.pane_id,
        shell_path,
        tool_discovery_script(),
    )?;
    let request = ShellExecutionRequest {
        action_id: format!("tool-discovery:{}", turn.turn_id),
        transaction,
        timeout_ms: Some(DEFAULT_TOOL_DISCOVERY_TIMEOUT_MS),
        interactive: false,
        stateful: false,
    };
    let output = executor.execute_shell(&request)?;
    if output.timed_out {
        return Err(MezError::invalid_state("tool discovery timed out"));
    }
    if output.interrupted {
        return Err(MezError::invalid_state("tool discovery was interrupted"));
    }
    if output.exit_code != Some(0) {
        return Err(MezError::invalid_state(format!(
            "tool discovery failed: {}",
            output.stderr.trim()
        )));
    }

    let inventory = ToolInventory::parse_bootstrap_output(&output.stdout);
    cache.record(signature, inventory.clone());
    Ok(inventory)
}

/// Executes the `shell_output_to_action_result` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(super) fn local_output_to_action_result(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    output: LocalExecutionOutput,
    marker: MarkerToken,
) -> Result<ActionResult> {
    local_output_to_action_result_with_transport(
        turn,
        action,
        output.transport,
        output.sent_to_pane,
        output.shell_output,
        marker,
    )
}

fn local_output_to_action_result_with_transport(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    transport: LocalExecutionTransport,
    sent_to_pane: bool,
    output: ShellExecutionOutput,
    marker: MarkerToken,
) -> Result<ActionResult> {
    if local_action_plan(action)?.is_none() {
        return Err(MezError::invalid_args(
            "shell output requires a shell-backed action",
        ));
    }
    let combined_output_bytes = output.stdout.len().saturating_add(output.stderr.len());
    let transport_incomplete = output.transport_diagnostics.transport_incomplete();
    let output_truncated = output.transport_diagnostics.output_truncated();
    let signal: Option<i32> = if output.interrupted {
        Some(2) // SIGINT
    } else if let Some(ec) = output.exit_code {
        if ec > 128 && ec < 256 {
            Some(ec - 128)
        } else {
            None
        }
    } else {
        None
    };
    let structured = shell_command_structured_content_json(
        action,
        Some(transport.as_str()),
        sent_to_pane,
        serde_json::Value::Null,
        &[],
        serde_json::json!({
            "source": "executor",
            "stream": if sent_to_pane { "pty_combined" } else { "native_stdio" },
            "marker": marker.as_str(),
            "exit_code": output.exit_code,
            "signal": signal,
            "timed_out": output.timed_out,
            "interrupted": output.interrupted,
            "combined_output_bytes": combined_output_bytes,
            "output_truncated": output_truncated,
            "transport_incomplete": transport_incomplete,
            "transport_diagnostics": output.transport_diagnostics.to_json()
        }),
    )?;
    if output.timed_out {
        let mut result = ActionResult::failed(
            turn,
            action,
            ActionStatus::TimedOut,
            "shell_timeout",
            "shell command timed out",
        )?;
        result.structured_content_json = Some(structured);
        return Ok(result);
    }
    if output.interrupted {
        let mut result = ActionResult::failed(
            turn,
            action,
            ActionStatus::Interrupted,
            "shell_interrupted",
            "shell command was interrupted",
        )?;
        result.structured_content_json = Some(structured);
        return Ok(result);
    }
    let mut combined_output = String::new();
    if !output.stdout.is_empty() {
        combined_output.push_str(&output.stdout);
    }
    if !output.stderr.is_empty() {
        combined_output.push_str(&output.stderr);
    }
    let mut content = Vec::new();
    if !combined_output.is_empty() {
        content.push(combined_output);
    }
    if matches!(action.payload, AgentActionPayload::ShellCommand { .. }) {
        return Ok(ActionResult::succeeded(
            turn,
            action,
            shell_command_result_content(
                content.first().map(String::as_str).unwrap_or_default(),
                output.exit_code,
                output.timed_out,
                output.interrupted,
            ),
            Some(structured),
        ));
    }
    if output.exit_code == Some(0) {
        Ok(ActionResult::succeeded(
            turn,
            action,
            content,
            Some(structured),
        ))
    } else {
        let mut result = ActionResult::failed(
            turn,
            action,
            ActionStatus::Failed,
            "shell_exit_nonzero",
            "shell command exited with non-zero status",
        )?;
        result.content = action_text_content_blocks(content);
        result.structured_content_json = Some(structured);
        Ok(result)
    }
}

/// Executes the `mcp_response_to_action_result` operation for the owning subsystem.
///
/// Callers receive a typed result or error with context from the underlying
/// runtime operation.
pub(super) fn mcp_response_to_action_result(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    plan: &McpToolCallPlan,
    response: McpToolCallResponse,
) -> Result<ActionResult> {
    let content_json = response.content_json.clone();
    let structured_payload = format!(
        r#"{{"server":"{}","tool":"{}","content":{},"structured_content":{},"is_error":{}}}"#,
        json_escape(&plan.server_id),
        json_escape(&plan.tool_name),
        content_json,
        response
            .structured_content_json
            .as_deref()
            .unwrap_or("null"),
        response.is_error
    );
    let content = action_content_blocks_from_json_or_text(&response.content_json);
    if response.is_error {
        let mut result = ActionResult::failed(
            turn,
            action,
            ActionStatus::Failed,
            "mcp_tool_error",
            "MCP tool returned an error",
        )?;
        result.content = content;
        result.structured_content_json = Some(structured_payload);
        Ok(result)
    } else {
        let mut result =
            ActionResult::succeeded(turn, action, Vec::new(), Some(structured_payload));
        result.content = content;
        Ok(result)
    }
}
