#![cfg(unix)]

//! Regression coverage for the foreground cli subsystem.
//!
//! These tests describe the behavior protected by the repository
//! specification and workflow guidance. Keeping the scenarios documented
//! makes failures easier to map back to the user-visible contract.

use mezzanine::control::{decode_control_frame, encode_control_body};
use portable_pty::{Child as PtyChild, CommandBuilder, PtySize, native_pty_system};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child as ProcessChild, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Carries Foreground Process state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
struct ForegroundProcess {
    /// Stores the child value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    child: Box<dyn PtyChild + Send + Sync>,
    /// Stores the writer value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    writer: Option<Box<dyn Write + Send>>,
    /// Stores the output rx value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    output_rx: Receiver<Vec<u8>>,
    /// Stores the reader thread value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    reader_thread: Option<JoinHandle<()>>,
    /// Stores the root value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    root: PathBuf,
}

impl ForegroundProcess {
    /// Runs the read until operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_until<F>(
        &mut self,
        output: &mut Vec<u8>,
        timeout: Duration,
        predicate: F,
    ) -> Result<(), String>
    where
        F: Fn(&str) -> bool,
    {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(status) = self
                .child
                .try_wait()
                .map_err(|error| format!("failed to poll foreground child: {error}"))?
            {
                return Err(format!(
                    "foreground child exited before expected output: status={status:?} output={}",
                    output_excerpt(output)
                ));
            }

            match self.output_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(chunk) => {
                    output.extend_from_slice(&chunk);
                    let text = String::from_utf8_lossy(output);
                    if predicate(&text) {
                        return Ok(());
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!(
                        "foreground output reader closed before expected output: output={}",
                        output_excerpt(output)
                    ));
                }
            }
        }

        Err(format!(
            "timed out waiting for foreground output: output={}",
            output_excerpt(output)
        ))
    }

    /// Runs the send interrupt operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_interrupt(&mut self) -> Result<(), String> {
        let process_id = self
            .child
            .process_id()
            .ok_or_else(|| "foreground child has no process id".to_string())?;
        let raw_pid = i32::try_from(process_id)
            .map_err(|_| format!("foreground child process id {process_id} is too large"))?;
        let pid = rustix::process::Pid::from_raw(raw_pid)
            .ok_or_else(|| format!("foreground child process id {process_id} is invalid"))?;
        rustix::process::kill_process(pid, rustix::process::Signal::INT)
            .map_err(|error| format!("failed to send SIGINT to foreground child: {error}"))
    }

    /// Runs the write input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_input(&mut self, input: &[u8]) -> Result<(), String> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| "foreground input writer is closed".to_string())?;
        writer
            .write_all(input)
            .map_err(|error| format!("failed to write foreground input: {error}"))?;
        writer
            .flush()
            .map_err(|error| format!("failed to flush foreground input: {error}"))
    }

    /// Runs the wait for exit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn wait_for_exit(&mut self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self
                .child
                .try_wait()
                .map_err(|error| format!("failed to poll foreground child: {error}"))?
                .is_some()
            {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
        Err("foreground child did not exit after SIGINT".to_string())
    }

    /// Runs the read until exit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn read_until_exit(&mut self, output: &mut Vec<u8>, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            while let Ok(chunk) = self.output_rx.try_recv() {
                output.extend_from_slice(&chunk);
            }
            if self
                .child
                .try_wait()
                .map_err(|error| format!("failed to poll foreground child: {error}"))?
                .is_some()
            {
                while let Ok(chunk) = self.output_rx.recv_timeout(Duration::from_millis(20)) {
                    output.extend_from_slice(&chunk);
                }
                return Ok(());
            }
            match self.output_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(chunk) => output.extend_from_slice(&chunk),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    if self
                        .child
                        .try_wait()
                        .map_err(|error| format!("failed to poll foreground child: {error}"))?
                        .is_some()
                    {
                        return Ok(());
                    }
                    return Err(format!(
                        "foreground output reader closed before process exit: output={}",
                        output_excerpt(output)
                    ));
                }
            }
        }
        Err(format!(
            "foreground child did not exit before timeout: output={}",
            output_excerpt(output)
        ))
    }
}

impl Drop for ForegroundProcess {
    /// Runs the drop operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn drop(&mut self) {
        let _ = self.writer.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(reader_thread) = self.reader_thread.take() {
            let _ = reader_thread.join();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Launches the real `mez serve --attach-primary` binary inside a PTY so the
/// foreground path sees interactive stdin/stdout instead of the unit-test
/// harness. The fixture verifies the default foreground draw includes the
/// visible window and pane state rows required by the spec, and that SIGINT
/// cancellation clears Mezzanine's drawn viewport and emits the presentation
/// restore sequence rather than leaving the host cursor hidden after the
/// supervised async service is aborted.
#[test]
fn foreground_serve_attach_primary_renders_default_frames_and_restores_presentation() {
    let root = test_root("foreground-serve-attach");
    let home = root.join("home");
    let runtime = root.join("runtime");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = runtime.join("foreground.sock");

    let mut process = spawn_foreground_serve(&root, &home, &runtime, &socket);
    let mut output = Vec::new();
    process
        .read_until(&mut output, Duration::from_secs(10), |text| {
            text.contains("serving: true")
                && text.contains("\x1b[?25l")
                && text.contains("\x1b[?1000;1002;1006h")
                && text.contains("\x1b[2J\x1b[H")
                && text.contains("0 shell")
        })
        .unwrap();

    process.send_interrupt().unwrap();
    process
        .read_until(&mut output, Duration::from_secs(5), |text| {
            text.contains("\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b>\x1b[0m\x1b[?6l\x1b[?69l\x1b[r\x1b[?7h\x1b[2J\x1b[H\x1b[?25h\x1b[0 q")
        })
        .unwrap();
    process.wait_for_exit(Duration::from_secs(5)).unwrap();
}

/// Launches a real foreground primary session and exits the pane shell normally.
/// This covers the clean primary-exit path where the terminal endpoint may
/// disappear while Mezzanine is restoring presentation state; that condition
/// must be treated as normal teardown rather than printing a `Broken pipe`
/// diagnostic to the user's terminal.
#[test]
fn foreground_serve_attach_primary_exits_cleanly_without_broken_pipe_error() {
    let root = test_root("foreground-serve-clean-exit");
    let home = root.join("home");
    let runtime = root.join("runtime");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = runtime.join("foreground.sock");

    let mut process = spawn_foreground_serve(&root, &home, &runtime, &socket);
    let mut output = Vec::new();
    process
        .read_until(&mut output, Duration::from_secs(10), |text| {
            text.contains("serving: true") && text.contains("0 shell")
        })
        .unwrap();

    process.write_input(b"exit\n").unwrap();
    process
        .read_until_exit(&mut output, Duration::from_secs(10))
        .unwrap();

    let text = String::from_utf8_lossy(&output);
    assert!(!text.contains("Broken pipe"), "{text}");
    assert!(
        !text.contains("async runtime service attached-terminal-primary failed"),
        "{text}"
    );
}

/// Launches a detached foreground service and attaches with a separate
/// interactive `mez attach` process. This covers the direct attach teardown path
/// used by normal `mez new` reattachment; closing the primary shell must not
/// surface output or control-socket EPIPE as a user-visible `Broken pipe`
/// diagnostic.
#[test]
fn foreground_attach_exits_cleanly_without_broken_pipe_error() {
    let root = test_root("foreground-attach-clean-exit");
    let home = root.join("home");
    let runtime = root.join("runtime");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = runtime.join("attach.sock");

    let mut daemon = spawn_detached_serve(&home, &runtime, &socket);
    wait_for_path(&socket, Duration::from_secs(10)).unwrap();
    let mut process = spawn_foreground_attach(&root, &home, &runtime, &socket);
    let mut output = Vec::new();
    process
        .read_until(&mut output, Duration::from_secs(10), |text| {
            text.contains("0 shell") && text.contains("$")
        })
        .unwrap();

    process.write_input(b"exit\n").unwrap();
    process
        .read_until_exit(&mut output, Duration::from_secs(10))
        .unwrap();
    wait_for_process_exit(&mut daemon, Duration::from_secs(10)).unwrap();

    let text = String::from_utf8_lossy(&output);
    assert!(!text.contains("Broken pipe"), "{text}");
    assert!(!text.contains("mez: Io"), "{text}");
}

/// Launches a detached foreground service and runs a deterministic full-screen
/// shell script through a real `mez attach` PTY.
///
/// This regression exercises the end-to-end foreground attach path with a
/// minimal TUI-style script that explicitly enters alternate screen, enables
/// focus events, clears the viewport, draws sentinel text, and restores the
/// normal screen. The attach client must surface the full-screen host mode
/// bytes and return to the shell prompt after the script exits.
#[test]
fn foreground_attach_runs_minimal_full_screen_script() {
    let root = test_root("fg-attach-tui");
    let home = root.join("home");
    let runtime = root.join("runtime");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = runtime.join("tui.sock");

    let mut process = spawn_foreground_serve(&root, &home, &runtime, &socket);
    let mut output = Vec::new();
    process
        .read_until(&mut output, Duration::from_secs(10), |text| {
            text.contains("serving: true")
                && text.contains("0 shell")
                && text.contains("\r\n\x1b[0m$")
                && text.contains("\x1b[?25h")
        })
        .unwrap();

    process
        .write_input(
            b"printf '\\033[?1049h\\033[?1004h\\033[H\\033[2Jmini-tui'; printf '\\nready'; sleep 1\n",
        )
        .unwrap();
    thread::sleep(Duration::from_secs(2));

    process
        .write_input(b"printf '\\033[?1004l\\033[?1049l'; echo shell-resumed; exit\n")
        .unwrap();
    process
        .read_until_exit(&mut output, Duration::from_secs(10))
        .unwrap();

    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("\x1b[?1049h"), "{text}");
    assert!(text.contains("\x1b[?1004h"), "{text}");
    assert!(text.contains("mini-tui"), "{text}");
    assert!(text.contains("ready"), "{text}");
    assert!(text.contains("\x1b[?1004l"), "{text}");
    assert!(text.contains("\x1b[?1049l"), "{text}");
}

/// Launches `mez attach` inside a PTY whose real size intentionally differs
/// from stale `COLUMNS`/`LINES` environment values. The first control initialize
/// frame must report the PTY size, because default `mez` startup uses this frame
/// to resize the initial daemon pane before its first user-facing render.
#[test]
fn foreground_attach_initialize_prefers_tty_size_over_stale_environment() {
    let root = test_root("foreground-attach-tty-size");
    let home = root.join("home");
    let runtime = root.join("runtime");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = runtime.join("attach-size.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (request_tx, request_rx) = mpsc::channel();

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut input = Vec::new();
        loop {
            let mut chunk = [0u8; 4096];
            let read = stream.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            input.extend_from_slice(&chunk[..read]);
            if let Ok((body, _)) = decode_control_frame(&input, 1024 * 1024) {
                request_tx.send(body).unwrap();
                let response = r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"primary","session":{"primary_client_id":"c1"}}}"#;
                let _ = stream.write_all(&encode_control_body(response));
                let _ = stream.flush();
                break;
            }
        }
    });
    let mut process = spawn_foreground_attach_with_terminal_size(
        &root,
        &home,
        &runtime,
        &socket,
        PtySize {
            rows: 31,
            cols: 101,
            pixel_width: 0,
            pixel_height: 0,
        },
        ("80", "24"),
    );

    let request = request_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("attach did not send control initialize");

    assert!(
        request.contains(r#""method":"control/initialize""#),
        "{request}"
    );
    assert!(request.contains(r#""columns":101"#), "{request}");
    assert!(request.contains(r#""rows":31"#), "{request}");
    assert!(!request.contains(r#""columns":80"#), "{request}");
    assert!(!request.contains(r#""rows":24"#), "{request}");
    let _ = process.child.kill();
    let _ = process.child.wait();
    server.join().unwrap();
}

/// Runs the spawn foreground serve operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn spawn_foreground_serve(
    root: &Path,
    home: &Path,
    runtime: &Path,
    socket: &Path,
) -> ForegroundProcess {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_mez"));
    command.env_clear();
    command.env("HOME", home.as_os_str());
    command.env("SHELL", "/bin/sh");
    command.env("MEZ_TMPDIR", runtime.as_os_str());
    command.env("XDG_RUNTIME_DIR", runtime.as_os_str());
    command.env("TERM", "xterm-256color");
    command.env("COLUMNS", "80");
    command.env("LINES", "24");
    command.arg("-S");
    command.arg(socket.as_os_str());
    command.arg("serve");
    command.arg("--attach-primary");
    command.arg("--no-aux-sockets");

    let child = pair.slave.spawn_command(command).unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let writer = pair.master.take_writer().unwrap();
    drop(pair.slave);
    let (output_tx, output_rx) = mpsc::channel();
    let reader_thread = thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if output_tx.send(buffer[..read].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    ForegroundProcess {
        child,
        writer: Some(writer),
        output_rx,
        reader_thread: Some(reader_thread),
        root: root.to_path_buf(),
    }
}

/// Runs the spawn foreground attach operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn spawn_foreground_attach(
    root: &Path,
    home: &Path,
    runtime: &Path,
    socket: &Path,
) -> ForegroundProcess {
    spawn_foreground_attach_with_terminal_size(
        root,
        home,
        runtime,
        socket,
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
        ("80", "24"),
    )
}

/// Runs the spawn foreground attach with terminal size operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn spawn_foreground_attach_with_terminal_size(
    root: &Path,
    home: &Path,
    runtime: &Path,
    socket: &Path,
    pty_size: PtySize,
    env_size: (&str, &str),
) -> ForegroundProcess {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(pty_size).unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_mez"));
    command.env_clear();
    command.env("HOME", home.as_os_str());
    command.env("SHELL", "/bin/sh");
    command.env("MEZ_TMPDIR", runtime.as_os_str());
    command.env("XDG_RUNTIME_DIR", runtime.as_os_str());
    command.env("TERM", "xterm-256color");
    command.env("COLUMNS", env_size.0);
    command.env("LINES", env_size.1);
    command.arg("-S");
    command.arg(socket.as_os_str());
    command.arg("attach");

    let child = pair.slave.spawn_command(command).unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let writer = pair.master.take_writer().unwrap();
    drop(pair.slave);
    let (output_tx, output_rx) = mpsc::channel();
    let reader_thread = thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if output_tx.send(buffer[..read].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    ForegroundProcess {
        child,
        writer: Some(writer),
        output_rx,
        reader_thread: Some(reader_thread),
        root: root.to_path_buf(),
    }
}

/// Runs the spawn detached serve operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn spawn_detached_serve(home: &Path, runtime: &Path, socket: &Path) -> ProcessChild {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mez"));
    command.env_clear();
    command.env("HOME", home.as_os_str());
    command.env("SHELL", "/bin/sh");
    command.env("MEZ_TMPDIR", runtime.as_os_str());
    command.env("XDG_RUNTIME_DIR", runtime.as_os_str());
    command.env("TERM", "xterm-256color");
    command
        .arg("-S")
        .arg(socket.as_os_str())
        .arg("serve")
        .arg("--no-aux-sockets")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.spawn().unwrap()
}

/// Runs the wait for path operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn wait_for_path(path: &Path, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(format!("timed out waiting for {}", path.display()))
}

/// Runs the wait for process exit operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn wait_for_process_exit(child: &mut ProcessChild, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child
            .try_wait()
            .map_err(|error| format!("failed to poll detached serve child: {error}"))?
            .is_some()
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    Err("detached serve child did not exit before timeout".to_string())
}

/// Runs the test root operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn test_root(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("mez-{name}-{}-{unique}", std::process::id()))
}

/// Runs the output excerpt operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn output_excerpt(output: &[u8]) -> String {
    let escaped = String::from_utf8_lossy(output).escape_debug().to_string();
    if escaped.len() <= 2000 {
        escaped
    } else {
        format!("{}...", &escaped[..2000])
    }
}
