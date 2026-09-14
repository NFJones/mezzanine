//! Pane command planning and PTY-backed process spawning.
//!
//! Spawning resolves the shell invocation, prepares the pane environment, opens
//! the PTY pair, and hands the live process handle back to the manager.

use std::path::Path;

#[cfg(target_os = "macos")]
use std::collections::BTreeMap;
#[cfg(target_os = "macos")]
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use portable_pty::{CommandBuilder, native_pty_system};

use crate::{MuxError as MezError, Result};
use mez_terminal::TerminalSize;

use super::pane::{PaneProcess, configure_pty_master_nonblocking};
#[cfg(target_os = "macos")]
use super::process_metadata::RawEnvironmentEntry;
use super::pty::pty_size;
use super::types::{PaneCommandPlan, PaneProcessEnvironment, PaneProcessLaunch};

/// Opens a PTY and starts one argv-addressed program directly.
///
/// This path does not invoke a shell. The child receives the server process
/// environment plus the supplied overrides, starts in `start_directory` when
/// present, and owns a fresh controlling terminal sized to `size`.
pub fn spawn_argv_pty_process(
    executable: &Path,
    arguments: &[String],
    environment: &[(String, String)],
    size: TerminalSize,
    start_directory: Option<&Path>,
) -> Result<PaneProcess> {
    if executable.as_os_str().is_empty() {
        return Err(MezError::invalid_args(
            "PTY process executable must not be empty",
        ));
    }
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(pty_size(size))
        .map_err(|error| MezError::invalid_state(format!("failed to open process PTY: {error}")))?;

    let mut command = CommandBuilder::new(executable);
    command.args(arguments);
    let initial_working_directory = initial_working_directory(start_directory);
    if let Some(start_directory) = start_directory {
        validate_start_directory(start_directory)?;
        command.cwd(start_directory);
    }
    for (key, value) in environment {
        command.env(key, value);
    }

    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| MezError::io(format!("failed to spawn PTY process: {error}")))?;
    drop(pair.slave);

    let primary_pid = child
        .process_id()
        .ok_or_else(|| MezError::invalid_state("spawned PTY process did not expose a pid"))?;
    let process_group_leader = pair.master.process_group_leader();
    configure_pty_master_nonblocking(pair.master.as_ref())?;

    Ok(PaneProcess {
        child,
        master: pair.master,
        output_backlog: std::collections::VecDeque::new(),
        output_read_buffer: vec![0; super::pty::PTY_IO_CHUNK_BYTES],
        output_backlog_limit_bytes: super::pane::DEFAULT_OUTPUT_BACKLOG_LIMIT_BYTES,
        output_activity_sequence: 0,
        output_closed: false,
        #[cfg(target_os = "macos")]
        shell_input_acknowledgements_supported: false,
        #[cfg(target_os = "macos")]
        shell_input_acknowledgements_seen: 0,
        #[cfg(target_os = "macos")]
        launch_environment: Vec::new(),
        primary_pid,
        process_group_leader,
        initial_working_directory,
        exit_status: None,
    })
}

/// Runs the pane command plan operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn pane_command_plan(
    launch: &PaneProcessLaunch,
    explicit_command: Option<&str>,
) -> Result<PaneCommandPlan> {
    let program = launch.program().to_string_lossy().to_string();
    let args = match explicit_command {
        Some(command) => {
            let command = command.trim();
            if command.is_empty() {
                return Err(MezError::invalid_args(
                    "explicit pane command must not be empty",
                ));
            }
            vec!["-c".to_string(), format!("exec {command}")]
        }
        None => launch
            .interactive_arguments()
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect(),
    };
    Ok(PaneCommandPlan { program, args })
}

/// Converts an argv-style pane creation command to shell input that can be
/// executed with the same `exec` replacement semantics as string commands.
pub fn shell_command_from_argv(argv: &[String]) -> Result<String> {
    if argv.is_empty() {
        return Err(MezError::invalid_args(
            "shell_command array must contain at least one entry",
        ));
    }
    if argv[0].is_empty() {
        return Err(MezError::invalid_args(
            "shell_command array must start with a program name",
        ));
    }
    argv.iter()
        .map(|argument| {
            shlex::try_quote(argument)
                .map(|quoted| quoted.into_owned())
                .map_err(|error| {
                    MezError::invalid_args(format!(
                        "shell_command array contains an unquotable argument: {error}"
                    ))
                })
        })
        .collect::<Result<Vec<_>>>()
        .map(|arguments| arguments.join(" "))
}

/// Runs the spawn pane process operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn spawn_pane_process(
    launch: &PaneProcessLaunch,
    explicit_command: Option<&str>,
    environment: &PaneProcessEnvironment,
    size: TerminalSize,
) -> Result<PaneProcess> {
    spawn_pane_process_with_start_directory(launch, explicit_command, environment, size, None)
}

/// Opens a PTY and starts the resolved shell for a pane from an optional directory.
///
/// If `explicit_command` is present, the shell receives `exec <command>` so the
/// pane's primary PID is replaced by the requested program. If
/// `start_directory` is present, it must be an accessible directory and becomes
/// the process working directory before the shell starts.
pub fn spawn_pane_process_with_start_directory(
    launch: &PaneProcessLaunch,
    explicit_command: Option<&str>,
    environment: &PaneProcessEnvironment,
    size: TerminalSize,
    start_directory: Option<&Path>,
) -> Result<PaneProcess> {
    let plan = pane_command_plan(launch, explicit_command)?;
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(pty_size(size))
        .map_err(|error| MezError::invalid_state(format!("failed to open pane PTY: {error}")))?;

    let mut command = CommandBuilder::new(&plan.program);
    command.args(&plan.args);
    if launch.clears_environment() {
        command.env_clear();
    }
    let initial_working_directory = initial_working_directory(start_directory);
    if let Some(start_directory) = start_directory {
        validate_start_directory(start_directory)?;
        command.cwd(start_directory);
    }
    command.env("MEZ", &environment.mez);
    command.env("MEZ_SESSION", &environment.session);
    command.env("MEZ_WINDOW", &environment.window);
    command.env("MEZ_PANE", &environment.pane);
    command.env("TERM", &environment.term);
    command.env("GIT_OPTIONAL_LOCKS", "0");
    for (key, value) in launch.environment() {
        command.env(key, value);
    }
    #[cfg(target_os = "macos")]
    let launch_environment = pane_launch_environment(launch, environment, &command.get_shell());

    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| MezError::io(format!("failed to spawn pane process: {error}")))?;
    drop(pair.slave);

    let primary_pid = child
        .process_id()
        .ok_or_else(|| MezError::invalid_state("spawned pane process did not expose a pid"))?;
    let process_group_leader = pair.master.process_group_leader();
    configure_pty_master_nonblocking(pair.master.as_ref())?;

    Ok(PaneProcess {
        child,
        master: pair.master,
        output_backlog: std::collections::VecDeque::new(),
        output_read_buffer: vec![0; super::pty::PTY_IO_CHUNK_BYTES],
        output_backlog_limit_bytes: super::pane::DEFAULT_OUTPUT_BACKLOG_LIMIT_BYTES,
        output_activity_sequence: 0,
        output_closed: false,
        #[cfg(target_os = "macos")]
        shell_input_acknowledgements_supported: explicit_command.is_none(),
        #[cfg(target_os = "macos")]
        shell_input_acknowledgements_seen: 0,
        #[cfg(target_os = "macos")]
        launch_environment,
        primary_pid,
        process_group_leader,
        initial_working_directory,
        exit_status: None,
    })
}

/// Runs the initial working directory operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn initial_working_directory(start_directory: Option<&Path>) -> Option<std::path::PathBuf> {
    match start_directory {
        Some(start_directory) => std::fs::canonicalize(start_directory)
            .ok()
            .or_else(|| Some(start_directory.to_path_buf())),
        None => std::env::current_dir().ok(),
    }
}

/// Captures the effective environment requested for one macOS pane launch.
///
/// Darwin can omit another process's environment from `KERN_PROCARGS2`, so a
/// pane retains this code-owned launch contract as metadata fallback. Explicit
/// harness and launch values replace inherited values exactly as the command
/// builder does.
#[cfg(target_os = "macos")]
fn pane_launch_environment(
    launch: &PaneProcessLaunch,
    environment: &PaneProcessEnvironment,
    shell: &str,
) -> Vec<RawEnvironmentEntry> {
    let mut entries = if launch.clears_environment() {
        BTreeMap::new()
    } else {
        std::env::vars_os()
            .map(|(key, value)| (key.into_vec(), value.into_vec()))
            .collect::<BTreeMap<_, _>>()
    };
    for (key, value) in [
        (b"MEZ".as_slice(), environment.mez.as_bytes()),
        (b"MEZ_SESSION".as_slice(), environment.session.as_bytes()),
        (b"MEZ_WINDOW".as_slice(), environment.window.as_bytes()),
        (b"MEZ_PANE".as_slice(), environment.pane.as_bytes()),
        (b"TERM".as_slice(), environment.term.as_bytes()),
        (b"GIT_OPTIONAL_LOCKS".as_slice(), b"0".as_slice()),
    ] {
        entries.insert(key.to_vec(), value.to_vec());
    }
    for (key, value) in launch.environment() {
        entries.insert(key.as_bytes().to_vec(), value.as_bytes().to_vec());
    }
    entries
        .entry(b"SHELL".to_vec())
        .or_insert_with(|| shell.as_bytes().to_vec());
    entries
        .into_iter()
        .map(|(key, value)| RawEnvironmentEntry { key, value })
        .collect()
}

/// Runs the validate start directory operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_start_directory(start_directory: &Path) -> Result<()> {
    if start_directory.as_os_str().is_empty() {
        return Err(MezError::invalid_args("start_directory must not be empty"));
    }
    let metadata = std::fs::metadata(start_directory).map_err(|error| {
        MezError::invalid_args(format!(
            "start_directory `{}` is not accessible: {error}",
            start_directory.display()
        ))
    })?;
    if !metadata.is_dir() {
        return Err(MezError::invalid_args(format!(
            "start_directory `{}` is not a directory",
            start_directory.display()
        )));
    }
    Ok(())
}
