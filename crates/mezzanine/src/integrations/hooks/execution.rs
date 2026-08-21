//! Program and focused-shell hook execution.
//!
//! Execution owns process spawning, timeout handling, shell-executor adaptation,
//! and conversion of runner output into uniform hook execution results.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinSet;

use crate::error::{MezError, Result};
use crate::host::process::wait_for_child_with_timeout;

use super::types::{
    FocusedShellExecutor, HookExecutionPlan, HookExecutionResult, HookExecutionStatus, HookFailure,
    HookFailureKind,
};

/// Maximum bytes retained independently from each program-hook output stream.
///
/// Readers continue draining after this bound so a child cannot block on a
/// full pipe. Detailed truncation metadata and configurable limits remain an
/// integration policy concern; this executor invariant prevents hook output
/// from making process completion depend on unbounded memory.
const PROGRAM_HOOK_OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;

/// Best-effort process-group guard for standalone hook subprocesses.
///
/// Linux and macOS hooks start in a private process group. Dropping an
/// in-flight executor therefore terminates descendants as well as relying on
/// Tokio's direct-child `kill_on_drop` behavior.
struct ProgramHookProcessGroupGuard {
    #[cfg(unix)]
    process_group_id: Option<i32>,
    armed: bool,
}

impl ProgramHookProcessGroupGuard {
    /// Arms a guard for the spawned child process group.
    fn new(process_id: Option<u32>) -> Self {
        Self {
            #[cfg(unix)]
            process_group_id: process_id.and_then(|id| i32::try_from(id).ok()),
            armed: true,
        }
    }

    /// Prevents termination after the child has been reaped normally.
    fn disarm(&mut self) {
        self.armed = false;
    }

    /// Terminates the complete private process group when supported.
    fn terminate(&self) {
        if !self.armed {
            return;
        }
        #[cfg(unix)]
        if let Some(process_group_id) = self.process_group_id {
            // SAFETY: `process_group_id` comes from the successfully spawned
            // child. A negative pid targets only that private process group.
            unsafe {
                libc::kill(-process_group_id, libc::SIGKILL);
            }
        }
    }
}

impl Drop for ProgramHookProcessGroupGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// Runs the execute program hook operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn execute_program_hook(plan: &HookExecutionPlan) -> Result<HookExecutionResult> {
    if plan.run_in_focused_shell {
        return Err(MezError::invalid_args(
            "focused-shell hooks must be executed through the pane shell",
        ));
    }
    let program = plan
        .program
        .as_deref()
        .ok_or_else(|| MezError::invalid_args("program hook plan is missing program"))?;
    let mut command = Command::new(program);
    command
        .args(&plan.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|error| {
        MezError::new(
            crate::error::MezErrorKind::Io,
            format!("failed to spawn hook `{}`: {error}", plan.hook_id),
        )
    })?;
    let mut process_group = ProgramHookProcessGroupGuard::new(Some(child.id()));
    if let Some(mut stdin) = child.stdin.take() {
        match stdin.write_all(plan.event_payload_json.as_bytes()) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(error) => return Err(error.into()),
        }
    }

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || read_child_pipe(stdout));
    let stderr_reader = std::thread::spawn(move || read_child_pipe(stderr));
    let status = wait_for_child_with_timeout(&mut child, Duration::from_millis(plan.timeout_ms))?;
    if status.is_none() {
        process_group.terminate();
        let _ = child.kill();
        let _ = child.wait();
    } else {
        process_group.disarm();
    }
    let stdout = join_child_pipe_reader(stdout_reader)?;
    let stderr = join_child_pipe_reader(stderr_reader)?;
    let Some(status) = status else {
        return Ok(HookExecutionResult {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            status: HookExecutionStatus::TimedOut,
            exit_code: None,
            stdout: stdout.text,
            stderr: stderr.text,
            stdout_bytes: stdout.observed_bytes,
            stderr_bytes: stderr.observed_bytes,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
            failure: Some(HookFailure {
                hook_id: plan.hook_id.clone(),
                event: plan.event,
                kind: HookFailureKind::Timeout,
                message: "hook timed out".to_string(),
                retryable: true,
            }),
        });
    };

    if status.success() {
        Ok(HookExecutionResult {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            status: HookExecutionStatus::Succeeded,
            exit_code: status.code(),
            stdout: stdout.text,
            stderr: stderr.text,
            stdout_bytes: stdout.observed_bytes,
            stderr_bytes: stderr.observed_bytes,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
            failure: None,
        })
    } else {
        Ok(HookExecutionResult {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            status: HookExecutionStatus::Failed,
            exit_code: status.code(),
            stdout: stdout.text,
            stderr: stderr.text,
            stdout_bytes: stdout.observed_bytes,
            stderr_bytes: stderr.observed_bytes,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
            failure: Some(HookFailure {
                hook_id: plan.hook_id.clone(),
                event: plan.event,
                kind: HookFailureKind::ExitNonZero,
                message: "hook exited with non-zero status".to_string(),
                retryable: false,
            }),
        })
    }
}

/// Executes a program hook with Tokio process I/O.
///
/// The hook payload is written to child stdin, stdout and stderr are drained
/// concurrently, and `plan.timeout_ms` is enforced with Tokio time. Focused
/// shell hooks return `InvalidArgs` because they must be dispatched through the
/// pane shell executor instead of spawned as standalone programs.
#[cfg(test)]
pub async fn execute_program_hook_async(plan: &HookExecutionPlan) -> Result<HookExecutionResult> {
    execute_program_hook_async_with_cancellation(plan, std::future::pending())
        .await?
        .ok_or_else(|| {
            MezError::invalid_state("program hook cancelled without a cancellation source")
        })
}

/// Executes a program hook until it completes, times out, or is cancelled.
///
/// Cancellation terminates and reaps the private process group before this
/// function returns. `None` distinguishes cancellation from a hook timeout,
/// which remains an ordinary [`HookExecutionResult`] governed by hook policy.
pub async fn execute_program_hook_async_with_cancellation<C>(
    plan: &HookExecutionPlan,
    cancellation: C,
) -> Result<Option<HookExecutionResult>>
where
    C: std::future::Future<Output = ()>,
{
    if plan.run_in_focused_shell {
        return Err(MezError::invalid_args(
            "focused-shell hooks must be executed through the pane shell",
        ));
    }
    let program = plan
        .program
        .as_deref()
        .ok_or_else(|| MezError::invalid_args("program hook plan is missing program"))?;
    let mut command = tokio::process::Command::new(program);
    command
        .args(&plan.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    let mut child = command.spawn().map_err(|error| {
        MezError::new(
            crate::error::MezErrorKind::Io,
            format!("failed to spawn hook `{}`: {error}", plan.hook_id),
        )
    })?;
    let mut process_group = ProgramHookProcessGroupGuard::new(child.id());

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdin = child.stdin.take();
    let payload = plan.event_payload_json.clone();
    let mut io_tasks = JoinSet::new();
    io_tasks.spawn(async move {
        ProgramHookIoOutput::Stdin(write_async_child_stdin(stdin, &payload).await)
    });
    io_tasks.spawn(async move { ProgramHookIoOutput::Stdout(read_async_child_pipe(stdout).await) });
    io_tasks.spawn(async move { ProgramHookIoOutput::Stderr(read_async_child_pipe(stderr).await) });

    let deadline = tokio::time::Instant::now() + Duration::from_millis(plan.timeout_ms);
    tokio::pin!(cancellation);
    let status = tokio::select! {
        status = child.wait() => status?,
        _ = tokio::time::sleep_until(deadline) => {
            terminate_program_hook_process(&mut child, &process_group, &mut io_tasks).await;
            process_group.disarm();
            return Ok(Some(program_hook_timeout_result(plan)));
        }
        _ = &mut cancellation => {
            terminate_program_hook_process(&mut child, &process_group, &mut io_tasks).await;
            process_group.disarm();
            return Ok(None);
        }
    };

    let (stdout, stderr) = tokio::select! {
        output = collect_program_hook_io(&mut io_tasks) => output?,
        _ = tokio::time::sleep_until(deadline) => {
            terminate_program_hook_process(&mut child, &process_group, &mut io_tasks).await;
            process_group.disarm();
            return Ok(Some(program_hook_timeout_result(plan)));
        }
        _ = &mut cancellation => {
            terminate_program_hook_process(&mut child, &process_group, &mut io_tasks).await;
            process_group.disarm();
            return Ok(None);
        }
    };
    process_group.disarm();

    if status.success() {
        Ok(Some(HookExecutionResult {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            status: HookExecutionStatus::Succeeded,
            exit_code: status.code(),
            stdout: stdout.text,
            stderr: stderr.text,
            stdout_bytes: stdout.observed_bytes,
            stderr_bytes: stderr.observed_bytes,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
            failure: None,
        }))
    } else {
        Ok(Some(HookExecutionResult {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            status: HookExecutionStatus::Failed,
            exit_code: status.code(),
            stdout: stdout.text,
            stderr: stderr.text,
            stdout_bytes: stdout.observed_bytes,
            stderr_bytes: stderr.observed_bytes,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
            failure: Some(HookFailure {
                hook_id: plan.hook_id.clone(),
                event: plan.event,
                kind: HookFailureKind::ExitNonZero,
                message: "hook exited with non-zero status".to_string(),
                retryable: false,
            }),
        }))
    }
}

/// One typed completion from the hook's owned asynchronous pipe tasks.
enum ProgramHookIoOutput {
    /// Standard-input payload delivery and shutdown.
    Stdin(Result<()>),
    /// Bounded standard-output retention.
    Stdout(Result<BoundedHookOutput>),
    /// Bounded standard-error retention.
    Stderr(Result<BoundedHookOutput>),
}

/// Collects all owned pipe tasks after the direct child exits.
async fn collect_program_hook_io(
    tasks: &mut JoinSet<ProgramHookIoOutput>,
) -> Result<(BoundedHookOutput, BoundedHookOutput)> {
    let mut stdin_complete = false;
    let mut stdout = None;
    let mut stderr = None;
    while let Some(joined) = tasks.join_next().await {
        let output = joined
            .map_err(|error| MezError::invalid_state(format!("hook I/O task failed: {error}")))?;
        match output {
            ProgramHookIoOutput::Stdin(result) => {
                result?;
                stdin_complete = true;
            }
            ProgramHookIoOutput::Stdout(result) => stdout = Some(result?),
            ProgramHookIoOutput::Stderr(result) => stderr = Some(result?),
        }
    }
    if !stdin_complete || stdout.is_none() || stderr.is_none() {
        return Err(MezError::invalid_state(
            "hook I/O tasks completed without every stream result",
        ));
    }
    Ok((stdout.unwrap_or_default(), stderr.unwrap_or_default()))
}

/// Terminates descendants, reaps the direct child, and joins owned I/O tasks.
async fn terminate_program_hook_process(
    child: &mut tokio::process::Child,
    process_group: &ProgramHookProcessGroupGuard,
    io_tasks: &mut JoinSet<ProgramHookIoOutput>,
) {
    process_group.terminate();
    let _ = child.start_kill();
    let _ = child.wait().await;
    io_tasks.shutdown().await;
}

/// Builds the ordinary policy-visible result for a total hook timeout.
fn program_hook_timeout_result(plan: &HookExecutionPlan) -> HookExecutionResult {
    HookExecutionResult {
        hook_id: plan.hook_id.clone(),
        event: plan.event,
        status: HookExecutionStatus::TimedOut,
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        stdout_bytes: 0,
        stderr_bytes: 0,
        stdout_truncated: false,
        stderr_truncated: false,
        failure: Some(HookFailure {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            kind: HookFailureKind::Timeout,
            message: "hook timed out".to_string(),
            retryable: true,
        }),
    }
}

/// Runs the execute focused shell hook operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn execute_focused_shell_hook(
    plan: &HookExecutionPlan,
    executor: &mut impl FocusedShellExecutor,
) -> Result<HookExecutionResult> {
    if !plan.run_in_focused_shell {
        return Err(MezError::invalid_args(
            "program hooks must be executed through the program hook runner",
        ));
    }
    plan.shell_command
        .as_deref()
        .ok_or_else(|| MezError::invalid_args("focused-shell hook plan is missing command"))?;
    let output = executor.run_hook_command(plan)?;
    let stdout_bytes = output.stdout_bytes;
    let stderr_bytes = output.stderr_bytes;
    let stdout_truncated = output.stdout_truncated;
    let stderr_truncated = output.stderr_truncated;

    if output.shell_unavailable {
        return Ok(HookExecutionResult {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            status: HookExecutionStatus::Failed,
            exit_code: None,
            stdout: output.stdout,
            stderr: output.stderr,
            stdout_bytes,
            stderr_bytes,
            stdout_truncated,
            stderr_truncated,
            failure: Some(HookFailure {
                hook_id: plan.hook_id.clone(),
                event: plan.event,
                kind: HookFailureKind::ShellUnavailable,
                message: "focused shell is unavailable".to_string(),
                retryable: true,
            }),
        });
    }

    if output.policy_denied {
        return Ok(HookExecutionResult {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            status: HookExecutionStatus::Failed,
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr.clone(),
            stdout_bytes,
            stderr_bytes,
            stdout_truncated,
            stderr_truncated,
            failure: Some(HookFailure {
                hook_id: plan.hook_id.clone(),
                event: plan.event,
                kind: HookFailureKind::PolicyDenied,
                message: output.stderr,
                retryable: false,
            }),
        });
    }

    if output.timed_out {
        return Ok(HookExecutionResult {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            status: HookExecutionStatus::TimedOut,
            exit_code: output.exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
            stdout_bytes,
            stderr_bytes,
            stdout_truncated,
            stderr_truncated,
            failure: Some(HookFailure {
                hook_id: plan.hook_id.clone(),
                event: plan.event,
                kind: HookFailureKind::Timeout,
                message: "focused-shell hook timed out".to_string(),
                retryable: true,
            }),
        });
    }

    if output.exit_code.is_none() {
        return Ok(HookExecutionResult {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            status: HookExecutionStatus::Queued,
            exit_code: None,
            stdout: output.stdout,
            stderr: output.stderr,
            stdout_bytes,
            stderr_bytes,
            stdout_truncated,
            stderr_truncated,
            failure: None,
        });
    }

    let success = output.exit_code == Some(0);
    Ok(HookExecutionResult {
        hook_id: plan.hook_id.clone(),
        event: plan.event,
        status: if success {
            HookExecutionStatus::Succeeded
        } else {
            HookExecutionStatus::Failed
        },
        exit_code: output.exit_code,
        stdout: output.stdout,
        stderr: output.stderr,
        stdout_bytes,
        stderr_bytes,
        stdout_truncated,
        stderr_truncated,
        failure: if success {
            None
        } else {
            Some(HookFailure {
                hook_id: plan.hook_id.clone(),
                event: plan.event,
                kind: HookFailureKind::ExitNonZero,
                message: "focused-shell hook exited with non-zero status".to_string(),
                retryable: false,
            })
        },
    })
}

/// Runs the read child pipe operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn read_child_pipe<T: Read>(pipe: Option<T>) -> Result<BoundedHookOutput> {
    let Some(mut pipe) = pipe else {
        return Ok(BoundedHookOutput::default());
    };
    let mut retained = Vec::new();
    let mut observed_bytes = 0usize;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = pipe.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        observed_bytes = observed_bytes.saturating_add(read);
        let remaining = PROGRAM_HOOK_OUTPUT_LIMIT_BYTES.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    bounded_hook_output(retained, observed_bytes)
}

/// Joins one synchronous pipe reader without allowing a panic to escape.
fn join_child_pipe_reader(
    reader: std::thread::JoinHandle<Result<BoundedHookOutput>>,
) -> Result<BoundedHookOutput> {
    reader
        .join()
        .map_err(|_| MezError::invalid_state("hook pipe reader thread panicked"))?
}

/// Bounded retained hook output plus complete drained byte accounting.
#[derive(Debug, Default)]
struct BoundedHookOutput {
    text: String,
    observed_bytes: usize,
    truncated: bool,
}

/// Converts a retained hook-output prefix while tolerating a split final scalar.
fn bounded_hook_output(mut retained: Vec<u8>, observed_bytes: usize) -> Result<BoundedHookOutput> {
    let truncated = observed_bytes > retained.len();
    match String::from_utf8(retained) {
        Ok(text) => Ok(BoundedHookOutput {
            text,
            observed_bytes,
            truncated,
        }),
        Err(error) if error.utf8_error().error_len().is_none() => {
            let valid_up_to = error.utf8_error().valid_up_to();
            retained = error.into_bytes();
            retained.truncate(valid_up_to);
            let text = String::from_utf8(retained).map_err(|error| {
                MezError::new(
                    crate::error::MezErrorKind::Io,
                    format!("hook output is not UTF-8: {error}"),
                )
            })?;
            Ok(BoundedHookOutput {
                text,
                observed_bytes,
                truncated: true,
            })
        }
        Err(error) => Err(MezError::new(
            crate::error::MezErrorKind::Io,
            format!("hook output is not UTF-8: {error}"),
        )),
    }
}

/// Runs the read async child pipe operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn read_async_child_pipe<T>(pipe: Option<T>) -> Result<BoundedHookOutput>
where
    T: AsyncRead + Unpin,
{
    let Some(mut pipe) = pipe else {
        return Ok(BoundedHookOutput::default());
    };
    let mut retained = Vec::new();
    let mut observed_bytes = 0usize;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = pipe.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        observed_bytes = observed_bytes.saturating_add(read);
        let remaining = PROGRAM_HOOK_OUTPUT_LIMIT_BYTES.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    bounded_hook_output(retained, observed_bytes)
}

/// Writes the complete hook payload and closes stdin on completion.
async fn write_async_child_stdin<T>(stdin: Option<T>, payload: &str) -> Result<()>
where
    T: tokio::io::AsyncWrite + Unpin,
{
    let Some(mut stdin) = stdin else {
        return Ok(());
    };
    match stdin.write_all(payload.as_bytes()).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(error) => return Err(error.into()),
    }
    let _ = stdin.shutdown().await;
    Ok(())
}
