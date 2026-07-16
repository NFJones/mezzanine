//! Program and focused-shell hook execution.
//!
//! Execution owns process spawning, timeout handling, shell-executor adaptation,
//! and conversion of runner output into uniform hook execution results.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use wait_timeout::ChildExt;

use crate::error::{MezError, Result};

use super::types::{
    FocusedShellExecutor, HookExecutionPlan, HookExecutionResult, HookExecutionStatus, HookFailure,
    HookFailureKind,
};

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
    let mut child = Command::new(program)
        .args(&plan.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            MezError::new(
                crate::error::MezErrorKind::Io,
                format!("failed to spawn hook `{}`: {error}", plan.hook_id),
            )
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        match stdin.write_all(plan.event_payload_json.as_bytes()) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(error) => return Err(error.into()),
        }
    }

    let Some(status) = child.wait_timeout(Duration::from_millis(plan.timeout_ms))? else {
        let _ = child.kill();
        let _ = child.wait();
        return Ok(HookExecutionResult {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            status: HookExecutionStatus::TimedOut,
            exit_code: None,
            stdout: read_child_pipe(child.stdout.take())?,
            stderr: read_child_pipe(child.stderr.take())?,
            failure: Some(HookFailure {
                hook_id: plan.hook_id.clone(),
                event: plan.event,
                kind: HookFailureKind::Timeout,
                message: "hook timed out".to_string(),
                retryable: true,
            }),
        });
    };

    let stdout = read_child_pipe(child.stdout.take())?;
    let stderr = read_child_pipe(child.stderr.take())?;
    if status.success() {
        Ok(HookExecutionResult {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            status: HookExecutionStatus::Succeeded,
            exit_code: status.code(),
            stdout,
            stderr,
            failure: None,
        })
    } else {
        Ok(HookExecutionResult {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            status: HookExecutionStatus::Failed,
            exit_code: status.code(),
            stdout,
            stderr,
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
pub async fn execute_program_hook_async(plan: &HookExecutionPlan) -> Result<HookExecutionResult> {
    if plan.run_in_focused_shell {
        return Err(MezError::invalid_args(
            "focused-shell hooks must be executed through the pane shell",
        ));
    }
    let program = plan
        .program
        .as_deref()
        .ok_or_else(|| MezError::invalid_args("program hook plan is missing program"))?;
    let mut child = tokio::process::Command::new(program)
        .args(&plan.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            MezError::new(
                crate::error::MezErrorKind::Io,
                format!("failed to spawn hook `{}`: {error}", plan.hook_id),
            )
        })?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_task = tokio::spawn(async move { read_async_child_pipe(stdout).await });
    let stderr_task = tokio::spawn(async move { read_async_child_pipe(stderr).await });

    if let Some(mut stdin) = child.stdin.take() {
        match stdin.write_all(plan.event_payload_json.as_bytes()).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(error) => return Err(error.into()),
        }
        let _ = stdin.shutdown().await;
    }

    let timeout = Duration::from_millis(plan.timeout_ms);
    let wait_result = tokio::time::timeout(timeout, child.wait()).await;
    let status = match wait_result {
        Ok(status) => Some(status?),
        Err(_) => {
            let _ = child.kill().await;
            None
        }
    };
    let stdout = join_async_child_pipe(stdout_task, "stdout").await?;
    let stderr = join_async_child_pipe(stderr_task, "stderr").await?;

    let Some(status) = status else {
        return Ok(HookExecutionResult {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            status: HookExecutionStatus::TimedOut,
            exit_code: None,
            stdout,
            stderr,
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
            stdout,
            stderr,
            failure: None,
        })
    } else {
        Ok(HookExecutionResult {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            status: HookExecutionStatus::Failed,
            exit_code: status.code(),
            stdout,
            stderr,
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

    if output.shell_unavailable {
        return Ok(HookExecutionResult {
            hook_id: plan.hook_id.clone(),
            event: plan.event,
            status: HookExecutionStatus::Failed,
            exit_code: None,
            stdout: output.stdout,
            stderr: output.stderr,
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
fn read_child_pipe<T: Read>(pipe: Option<T>) -> Result<String> {
    let Some(mut pipe) = pipe else {
        return Ok(String::new());
    };
    let mut output = String::new();
    pipe.read_to_string(&mut output)?;
    Ok(output)
}

/// Runs the read async child pipe operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn read_async_child_pipe<T>(pipe: Option<T>) -> Result<String>
where
    T: AsyncRead + Unpin,
{
    let Some(mut pipe) = pipe else {
        return Ok(String::new());
    };
    let mut output = String::new();
    pipe.read_to_string(&mut output).await?;
    Ok(output)
}

/// Runs the join async child pipe operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn join_async_child_pipe(
    task: tokio::task::JoinHandle<Result<String>>,
    stream_name: &str,
) -> Result<String> {
    task.await.map_err(|error| {
        MezError::new(
            crate::error::MezErrorKind::InvalidState,
            format!("hook {stream_name} reader task failed: {error}"),
        )
    })?
}
