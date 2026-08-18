//! Spawned shell executor for native shell mode.
//!
//! Native shell mode executes agent actions in a fresh shell process derived
//! from pane root-process metadata. This module owns that spawn: it
//! materializes the transaction command to a temporary file, runs the shell
//! (or an explicit typed child launch) with the inferred environment and
//! working directory, captures stdout and stderr through pipes within a
//! bounded budget, and kills the whole child process group on timeout or
//! interruption. Nothing is written to or read from the pane PTY.

use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mez_agent::SpawnedShellExecutor as SpawnedShellExecutorPort;
use mez_agent::{
    DEFAULT_AGENT_TURN_TIMEOUT_MS, ShellChildArgument, ShellExecutionOutput, ShellExecutionRequest,
    ShellTransaction, ShellTransportDiagnostics,
};

use crate::error::{MezError, Result};

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

/// Spawned process plus its optional runtime-owned lifecycle-status reader.
struct SpawnedChild {
    /// Running direct child process.
    child: Child,
    /// Read end of the descriptor selected by `ShellChildLaunch::status_fd`.
    status_reader: Option<OwnedFd>,
}

/// Executes one shell transaction in a freshly spawned shell process.
pub(crate) struct SpawnedShellExecutor {
    /// Inferred shell path, environment, and working directory.
    context: NativeShellContext,
    /// Interruption flag shared with the interrupt handle.
    interrupted: Arc<AtomicBool>,
}

impl SpawnedShellExecutor {
    /// Builds an executor around one inferred native shell context.
    pub(crate) fn new(context: NativeShellContext) -> Self {
        Self {
            context,
            interrupted: Arc::new(AtomicBool::new(false)),
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

    /// Materializes the transaction command, plus any input sidecar records,
    /// to one temporary file.
    fn materialize_command_file(&self, transaction: &ShellTransaction) -> Result<PathBuf> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path =
            std::env::temp_dir().join(format!("mez-spawned-{}-{unique}", std::process::id()));
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
        fs::write(&path, content.as_bytes()).map_err(|error| {
            MezError::invalid_state(format!(
                "spawned shell execution could not materialize its command file: {error}"
            ))
        })?;
        Ok(path)
    }

    /// Spawns the shell (or typed child launch) with the inferred context.
    fn spawn_child(
        &self,
        transaction: &ShellTransaction,
        command_file: &Path,
    ) -> Result<SpawnedChild> {
        let mut command = if let Some(launch) = &transaction.child_launch {
            let mut command = Command::new(&launch.executable);
            for argument in &launch.arguments {
                match argument {
                    ShellChildArgument::Literal(value) => {
                        command.arg(value);
                    }
                    ShellChildArgument::MaterializedCommandFile => {
                        command.arg(command_file);
                    }
                }
            }
            command
        } else {
            let mut command = Command::new(self.context.shell_path());
            command.arg(command_file);
            command
        };
        command
            .current_dir(self.context.working_directory())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear()
            .process_group(0);
        for entry in self.context.environment() {
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
            let (reader, writer) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)
                .map_err(|error| {
                    MezError::invalid_state(format!(
                        "spawned shell execution could not create its status pipe: {error}"
                    ))
                })?;
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
        let child = command.spawn().map_err(|error| {
            MezError::invalid_state(format!("spawned shell execution failed to start: {error}"))
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
        let stdout_reader = std::thread::spawn(move || drain_output_stream(stdout, stream_budget));
        let stderr_reader = std::thread::spawn(move || drain_output_stream(stderr, stream_budget));
        let status_reader = status_reader.map(|reader| {
            std::thread::spawn(move || {
                drain_output_stream(
                    std::fs::File::from(reader),
                    SPAWNED_CHILD_STATUS_CAPTURE_LIMIT,
                )
            })
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
        let (stdout_bytes, stdout_dropped) = stdout_reader
            .join()
            .map_err(|_| MezError::invalid_state("spawned shell execution stdout reader failed"))?;
        let (stderr_bytes, stderr_dropped) = stderr_reader
            .join()
            .map_err(|_| MezError::invalid_state("spawned shell execution stderr reader failed"))?;
        let output = ShellExecutionOutput {
            exit_code: if timed_out || interrupted {
                None
            } else {
                status.code()
            },
            signal: status.signal(),
            stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
            stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
            timed_out,
            interrupted,
            transport_diagnostics: ShellTransportDiagnostics {
                output_bytes_dropped: stdout_dropped.saturating_add(stderr_dropped),
                ..ShellTransportDiagnostics::default()
            },
        };
        if let Some(status_reader) = status_reader {
            let (status_bytes, status_dropped) = status_reader.join().map_err(|_| {
                MezError::invalid_state("spawned shell execution status reader failed")
            })?;
            validate_spawned_child_status(&status_bytes, status_dropped, &output)?;
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
        let command_file = self.materialize_command_file(&request.transaction)?;
        let child = self.spawn_child(&request.transaction, &command_file)?;
        let output = self.collect(
            child,
            request.timeout_ms,
            request.transaction.output_max_raw_bytes,
        );
        let _ = fs::remove_file(&command_file);
        output
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

/// Drains one child output stream into memory, retaining at most `budget`
/// bytes and counting the rest as dropped so bounded capture cannot deadlock
/// the child on a full pipe.
fn drain_output_stream(mut stream: impl Read, budget: usize) -> (Vec<u8>, usize) {
    let mut retained = Vec::new();
    let mut dropped = 0_usize;
    let mut buffer = [0_u8; 8192];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                let remaining = budget.saturating_sub(retained.len());
                let keep = count.min(remaining);
                retained.extend_from_slice(&buffer[..keep]);
                dropped = dropped.saturating_add(count.saturating_sub(keep));
            }
        }
    }
    (retained, dropped)
}

/// Validates trusted Bubblewrap lifecycle evidence captured outside stdout
/// and stderr for one completed typed child launch.
fn validate_spawned_child_status(
    status_bytes: &[u8],
    status_dropped: usize,
    output: &ShellExecutionOutput,
) -> Result<()> {
    if output.timed_out || output.interrupted {
        return Ok(());
    }
    if status_dropped > 0 {
        return Err(MezError::invalid_state(
            "spawned shell lifecycle status exceeded its capture limit",
        ));
    }
    let status_text = std::str::from_utf8(status_bytes).map_err(|_| {
        MezError::invalid_state("spawned shell lifecycle status was not valid UTF-8")
    })?;
    let status = crate::security::sandbox::parse_bubblewrap_status(status_text)
        .map_err(|error| MezError::invalid_state(error.message()))?;
    let reported_exit_code = status.exit_code.ok_or_else(|| {
        MezError::invalid_state(crate::security::sandbox::bubblewrap_failure_remediation(
            "Bubblewrap failed before payload execution",
        ))
    })?;
    if output.exit_code != Some(reported_exit_code) {
        return Err(MezError::invalid_state(
            crate::security::sandbox::bubblewrap_failure_remediation(
                "Bubblewrap status exit code contradicts the spawned process",
            ),
        ));
    }
    Ok(())
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
    use super::*;
    use mez_agent::{MarkerToken, ShellChildLaunch};
    use mez_mux::process::RawEnvironmentEntry;

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

    /// Verifies the inferred environment and working directory reach the
    /// spawned shell exactly as captured from the pane root process.
    #[test]
    fn spawned_executor_forwards_context_environment_and_working_directory() {
        let directory = std::env::temp_dir().join(format!("mez-native-cwd-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let context = NativeShellContext::for_test(
            PathBuf::from("/bin/sh"),
            vec![RawEnvironmentEntry {
                key: b"MEZ_NATIVE_TEST".to_vec(),
                value: b"visible".to_vec(),
            }],
            directory.clone(),
        );
        let mut executor = SpawnedShellExecutor::new(context);
        let output = executor
            .execute_shell(&request("printf \"$MEZ_NATIVE_TEST\"; pwd", Some(5_000)))
            .unwrap();
        let _ = fs::remove_dir(&directory);

        assert_eq!(output.exit_code, Some(0));
        assert!(output.stdout.starts_with("visible"));
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
