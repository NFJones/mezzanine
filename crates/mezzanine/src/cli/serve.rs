//! Cli Serve implementation.
//!
//! This module owns the cli serve boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    Args, AsRawFd, AsyncAttachedTerminalClientServiceConfig, AsyncAttachedTerminalLoopRequest,
    AsyncAttachedTerminalPresentationGuard, AsyncRuntimeService, AsyncRuntimeServiceExit,
    AttachedTerminalClientLoopConfig, AuxiliarySocketKind, CliEnv, CliOutputFormat, ClientEvent,
    ClientId, ClientViewRole, ConfigFormat, ConfigLayer, ConfigPaths, ConfigScope, IsTerminal,
    MezError, PathBuf, Result, RuntimeEvent, RuntimeEventBatch, RuntimeLifecycleState, Session,
    SessionSnapshotPayload, Size, SnapshotRestoreResult, SocketSelection, TerminalClientLoopConfig,
    Write, attached_terminal_client_service_exit, auxiliary_socket_path_for_control_socket,
    current_unix_seconds, io, json_escape, load_runtime_config_layers, resolve_shell,
    run_async_attached_terminal_client_service, selected_socket_path,
    terminal_size_from_fd_or_environment, write_json_or_plain,
};
use crate::host::session::{
    SessionFactory, SessionFactoryRequest, SessionRuntimeConfig, SessionRuntimeLimits,
    SessionRuntimeStartup, SessionSocketPublication,
};
use crate::{
    control::{CONTROL_CONTENT_TYPE, encode_control_body},
    protocol::framing::ProtocolFrameCodec,
};
use futures_util::StreamExt;
use mez_core::ids::SessionId;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt as _;
use tokio::process::Child;
use tokio_util::codec::Framed;

// Foreground daemon startup and serve options.

/// Defines the LIVE SESSION ID COUNTER static used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
static LIVE_SESSION_ID_COUNTER: AtomicU64 = AtomicU64::new(1);
/// Maximum body length accepted for the background daemon startup probe.
const BACKGROUND_DAEMON_STARTUP_PROBE_MAX_CONTENT_LENGTH: usize = 1024 * 1024;
/// JSON-RPC id used by the background daemon startup probe.
const BACKGROUND_DAEMON_STARTUP_PROBE_ID: &str = "cli-startup-probe";
/// A harmless request that proves the daemon control actor is serving frames.
const BACKGROUND_DAEMON_STARTUP_PROBE_REQUEST: &str =
    r#"{"jsonrpc":"2.0","id":"cli-startup-probe","method":"session/get","params":{}}"#;
/// Suffix appended to a detached daemon's socket file name for diagnostics.
const BACKGROUND_DAEMON_DIAGNOSTIC_SUFFIX: &str = ".diagnostics.log";

/// Runs the run new operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn run_new<W: Write>(
    socket_selection: &SocketSelection,
    parsed: NewCliArgs,
    env: CliEnv,
    interactive: bool,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let dry_run = parsed.dry_run;
    let session_name = parsed.name;
    if !interactive && !dry_run {
        return Err(MezError::forbidden(
            "creating a primary-attached session requires an interactive terminal; use --dry-run for validation",
        ));
    }

    let paths = env.config_paths()?;
    let config_path = if dry_run {
        paths
            .select_primary_file()?
            .unwrap_or_else(|| paths.default_primary_file())
    } else {
        paths.ensure_default_config()?
    };
    let shell = resolve_shell(env.shell.clone())?;
    let size = if interactive && !dry_run {
        let terminal_size_fd = io::stdout().is_terminal().then(|| io::stdout().as_raw_fd());
        let (columns, rows) = terminal_size_from_fd_or_environment(terminal_size_fd);
        Size::new(columns, rows)?
    } else {
        Size::new(80, 24)?
    };
    let launch_directory = std::env::current_dir()?;
    let mut session = Session::new_default(shell, size);
    apply_requested_session_name(&mut session, session_name.as_deref())?;
    if interactive && !dry_run {
        let new_socket_selection = socket_selection_for_new_session(socket_selection)?;
        let socket_path = selected_socket_path(&new_socket_selection).clone();
        let mut daemon = spawn_background_control_daemon(
            socket_path.as_path(),
            &env,
            &launch_directory,
            session_name.as_deref(),
        )?;
        wait_for_background_control_daemon(socket_path.as_path(), &mut daemon).await?;
        return super::run_attach(
            &new_socket_selection,
            &super::ControlTargetSelection::Unix,
            super::attach::AttachCliArgs {
                observer: false,
                default: false,
                session_id: None,
                create: false,
                create_name: None,
            },
            env,
            interactive,
            output_format,
            stdout,
        )
        .await;
    }
    if interactive {
        let _ = session.attach_primary("primary", true)?;
    }

    let output = format!(
        r#"{{"session_id":"{}","config":"{}","window_count":{},"pane_count":{},"shell":"{}","dry_run":{}}}"#,
        json_escape(session.id.as_str()),
        json_escape(&config_path.to_string_lossy()),
        session.windows().len(),
        session.windows()[0].panes().len(),
        json_escape(&session.shell.path().to_string_lossy()),
        dry_run
    );
    write_json_or_plain(stdout, output_format, &output)?;

    Ok(())
}

/// Typed process CLI arguments for `mez new`.
#[derive(Debug, Clone, Default, Args)]
pub(super) struct NewCliArgs {
    /// Validate session construction without starting or attaching to a daemon.
    #[arg(long)]
    pub(super) dry_run: bool,
    /// Optional name assigned to a newly created supervised session.
    #[arg(long, value_name = "NAME")]
    pub(super) name: Option<String>,
}

/// Runs the socket selection for new session operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn socket_selection_for_new_session(
    socket_selection: &SocketSelection,
) -> Result<SocketSelection> {
    let socket_path = selected_socket_path(socket_selection);
    match socket_selection {
        SocketSelection::Default(_) | SocketSelection::InPane(_) => {
            fresh_default_new_session_socket(socket_path).map(SocketSelection::Explicit)
        }
        SocketSelection::Explicit(path) | SocketSelection::Named(path) => {
            reject_active_new_session_socket(path)?;
            Ok(socket_selection.clone())
        }
    }
}

/// Runs the fresh default new session socket operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn fresh_default_new_session_socket(base_socket: &std::path::Path) -> Result<PathBuf> {
    let directory = base_socket.parent().ok_or_else(|| {
        MezError::invalid_args("control socket path must have a parent directory")
    })?;
    let stem = base_socket
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".sock").or(Some(name)))
        .filter(|name| !name.is_empty())
        .unwrap_or("default");
    for attempt in 0..1000u16 {
        let name = format!("{stem}.new.{}.{}.sock", std::process::id(), attempt);
        let candidate = super::socket_path_for_name(directory, &name)?;
        if candidate.exists() {
            continue;
        }
        return Ok(candidate);
    }
    Err(MezError::conflict(
        "could not allocate a fresh control socket for a new session",
    ))
}

/// Runs the reject active new session socket operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn reject_active_new_session_socket(path: &std::path::Path) -> Result<()> {
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(MezError::conflict(format!(
            "control socket already has a running session: {}",
            path.display()
        ))),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

/// Runs the spawn background control daemon operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn spawn_background_control_daemon(
    socket_path: &std::path::Path,
    env: &CliEnv,
    launch_directory: &std::path::Path,
    session_name: Option<&str>,
) -> Result<BackgroundControlDaemon> {
    let executable = std::env::current_exe()?;
    let diagnostic_path = background_daemon_diagnostic_path(socket_path)?;
    let mut command = Command::new(executable);
    command
        .arg("-S")
        .arg(socket_path)
        .arg("serve")
        .current_dir(launch_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    if let Some(session_name) = session_name {
        command.arg("--session-name").arg(session_name);
    }
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        command.env("RUST_BACKTRACE", "1");
    }
    if let Some(home) = env.home.as_ref() {
        command.env("HOME", home);
    }
    if let Some(shell) = env.shell.as_ref() {
        command.env("SHELL", shell);
    }
    if let Some(mez_tmpdir) = env.runtime.mez_tmpdir.as_ref() {
        command.env("MEZ_TMPDIR", mez_tmpdir);
    }
    if let Some(xdg_runtime_dir) = env.runtime.xdg_runtime_dir.as_ref() {
        command.env("XDG_RUNTIME_DIR", xdg_runtime_dir);
    }
    if let Some(mez) = env.mez.as_ref() {
        command.env("MEZ", mez);
    }
    // SAFETY: `pre_exec` runs in the child immediately before exec. Calling
    // `setsid` does not touch shared Rust state and detaches the daemon from
    // the invoking terminal/session before it starts serving sockets.
    unsafe {
        command.pre_exec(|| rustix::process::setsid().map(|_| ()).map_err(Into::into));
    }
    BackgroundControlDaemon::spawn(command, diagnostic_path)
}

/// Returns the private persistent diagnostic path for one detached daemon.
fn background_daemon_diagnostic_path(socket_path: &std::path::Path) -> Result<PathBuf> {
    let file_name = socket_path
        .file_name()
        .ok_or_else(|| MezError::invalid_args("control socket path must name a file"))?;
    Ok(socket_path.with_file_name(format!(
        "{}{}",
        file_name.to_string_lossy(),
        BACKGROUND_DAEMON_DIAGNOSTIC_SUFFIX
    )))
}

/// Tokio-owned handle for a background daemon while startup is being observed.
///
/// Tokio child processes must be spawned and waited from a live runtime. This
/// wrapper keeps that runtime alive only until the foreground `mez new` path has
/// either connected to the daemon socket or observed a child startup failure.
pub(super) struct BackgroundControlDaemon {
    /// Stores the child value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    child: Child,
    /// Private persistent stderr and panic diagnostic path for this daemon.
    diagnostic_path: PathBuf,
}

impl BackgroundControlDaemon {
    /// Spawns a daemon command under the caller's Tokio runtime.
    pub(super) fn spawn(mut command: Command, diagnostic_path: PathBuf) -> Result<Self> {
        let diagnostic_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&diagnostic_path)?;
        #[cfg(unix)]
        std::fs::set_permissions(&diagnostic_path, std::fs::Permissions::from_mode(0o600))?;
        command.stderr(Stdio::from(diagnostic_file));
        let child = tokio::process::Command::from(command).spawn()?;
        Ok(Self {
            child,
            diagnostic_path,
        })
    }

    /// Stops the child process owned by a startup-observation test.
    #[cfg(test)]
    pub(super) async fn terminate_for_test(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

/// Runs the wait for background control daemon operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn wait_for_background_control_daemon(
    socket_path: &std::path::Path,
    daemon: &mut BackgroundControlDaemon,
) -> Result<()> {
    let deadline = Instant::now() + StdDuration::from_secs(5);
    while Instant::now() < deadline {
        match probe_background_control_daemon(socket_path).await {
            Ok(()) => return Ok(()),
            Err(error) if retryable_background_daemon_probe_error(&error) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let retry_delay = remaining.min(StdDuration::from_millis(20));
                tokio::select! {
                    status = daemon.child.wait() => {
                        return Err(background_daemon_exit_error(daemon, status?).await);
                    }
                    _ = tokio::time::sleep(retry_delay) => {}
                }
            }
            Err(error) => {
                return Err(MezError::invalid_state(format!(
                    "background daemon startup probe failed: {error}"
                )));
            }
        }
    }
    Err(MezError::invalid_state(
        "background daemon did not accept connections before timeout",
    ))
}

/// Sends one framed startup probe through the control socket.
///
/// A connect-only probe can succeed as soon as the socket is bound, before the
/// session has started its initial pane process or the async control actor is
/// serving requests. Waiting for a framed response proves the background
/// daemon has reached the user-visible control boundary.
async fn probe_background_control_daemon(socket_path: &std::path::Path) -> Result<()> {
    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let codec = ProtocolFrameCodec::new(BACKGROUND_DAEMON_STARTUP_PROBE_MAX_CONTENT_LENGTH)?;
    let mut framed = Framed::new(stream, codec);
    framed
        .get_mut()
        .write_all(&encode_control_body(
            BACKGROUND_DAEMON_STARTUP_PROBE_REQUEST,
        ))
        .await?;
    framed.get_mut().flush().await?;

    let frame = framed.next().await.ok_or_else(|| {
        MezError::invalid_state("background daemon closed startup probe before responding")
    })??;
    if frame.content_type != CONTROL_CONTENT_TYPE {
        return Err(MezError::invalid_state(
            "background daemon startup probe returned unexpected content type",
        ));
    }
    validate_background_daemon_startup_probe_response(&frame.body)?;
    Ok(())
}

/// Returns whether a startup probe failure may clear during daemon startup.
fn retryable_background_daemon_probe_error(error: &MezError) -> bool {
    matches!(
        error.io_kind(),
        Some(
            std::io::ErrorKind::NotFound
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::UnexpectedEof
        )
    ) || (error.kind() == crate::error::MezErrorKind::InvalidState
        && error
            .message()
            .contains("closed startup probe before responding"))
}

/// Validates that the daemon answered the startup probe request.
fn validate_background_daemon_startup_probe_response(body: &str) -> Result<()> {
    let value = serde_json::from_str::<serde_json::Value>(body).map_err(|error| {
        MezError::invalid_state(format!(
            "background daemon startup probe returned invalid JSON: {error}"
        ))
    })?;
    if value.get("id").and_then(serde_json::Value::as_str)
        != Some(BACKGROUND_DAEMON_STARTUP_PROBE_ID)
    {
        return Err(MezError::invalid_state(
            "background daemon startup probe returned mismatched response id",
        ));
    }
    if value.get("result").is_none() && value.get("error").is_none() {
        return Err(MezError::invalid_state(
            "background daemon startup probe returned no result or error",
        ));
    }
    Ok(())
}

/// Runs the background daemon exit error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn background_daemon_exit_error(
    daemon: &mut BackgroundControlDaemon,
    status: ExitStatus,
) -> MezError {
    let detail = background_daemon_stderr(&daemon.diagnostic_path).await;
    let detail = detail.trim();
    let detail = if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    };
    MezError::invalid_state(format!(
        "background daemon exited before accepting connections: {status}{detail}"
    ))
}

/// Runs the background daemon stderr operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn background_daemon_stderr(path: &std::path::Path) -> String {
    tokio::fs::read_to_string(path).await.unwrap_or_default()
}

/// Carries Serve Options state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ServeOptions {
    /// Stores the max control connections value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) max_control_connections: u64,
    /// Stores the max message connections value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) max_message_connections: u64,
    /// Stores the max event connections value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) max_event_connections: u64,
    /// Stores the max event batches per connection value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) max_event_batches_per_connection: u64,
}

impl Default for ServeOptions {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self {
            max_control_connections: u64::MAX,
            max_message_connections: 0,
            max_event_connections: 0,
            max_event_batches_per_connection: u64::MAX,
        }
    }
}

/// Carries Parsed Serve Options state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct ParsedServeOptions {
    /// Stores the message socket value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) message_socket: Option<PathBuf>,
    /// Stores the event socket value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) event_socket: Option<PathBuf>,
    /// Stores the no aux sockets value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) no_aux_sockets: bool,
    /// Stores the attach primary value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) attach_primary: bool,
    /// Stores the attached primary client id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) attached_primary_client_id: Option<ClientId>,
    /// Stores the limits value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) limits: ServeOptions,
}

/// Typed process CLI options shared by `mez serve` and snapshot resume serving.
#[derive(Debug, Clone, Default, Args)]
pub(super) struct ServeCliArgs {
    /// Internal session name forwarded by the `mez new` compatibility launcher.
    #[arg(long, hide = true, value_name = "NAME")]
    session_name: Option<String>,
    /// Enables an auxiliary message socket at an explicit absolute path.
    #[arg(long, value_name = "PATH")]
    message_socket: Option<PathBuf>,
    /// Enables an auxiliary event socket at an explicit absolute path.
    #[arg(long, value_name = "PATH")]
    event_socket: Option<PathBuf>,
    /// Disables default auxiliary message and event sockets.
    #[arg(long)]
    no_aux_sockets: bool,
    /// Attaches the invoking terminal as the primary client.
    #[arg(long)]
    attach_primary: bool,
    /// Maximum concurrent control connections.
    #[arg(long, value_name = "N")]
    max_control_connections: Option<u64>,
    /// Maximum concurrent message connections.
    #[arg(long, value_name = "N")]
    max_message_connections: Option<u64>,
    /// Maximum concurrent event connections.
    #[arg(long, value_name = "N")]
    max_event_connections: Option<u64>,
    /// Maximum event batches per event connection.
    #[arg(long, value_name = "N")]
    max_event_batches_per_connection: Option<u64>,
}

impl ServeCliArgs {
    /// Returns true when the user supplied at least one serve-related option.
    pub(super) fn any_present(&self) -> bool {
        self.message_socket.is_some()
            || self.event_socket.is_some()
            || self.no_aux_sockets
            || self.attach_primary
            || self.max_control_connections.is_some()
            || self.max_message_connections.is_some()
            || self.max_event_connections.is_some()
            || self.max_event_batches_per_connection.is_some()
    }

    /// Converts typed CLI options into runtime daemon options.
    pub(super) fn into_parsed(self) -> Result<ParsedServeOptions> {
        let message_socket = absolute_optional_path(
            self.message_socket,
            "--message-socket requires an absolute path",
        )?;
        let event_socket = absolute_optional_path(
            self.event_socket,
            "--event-socket requires an absolute path",
        )?;
        let mut limits = ServeOptions::default();
        if message_socket.is_some() {
            limits.max_message_connections = u64::MAX;
        }
        if event_socket.is_some() {
            limits.max_event_connections = u64::MAX;
        }
        if let Some(value) = self.max_control_connections {
            limits.max_control_connections =
                nonzero_limit(value, "--max-control-connections must be greater than zero")?;
        }
        if let Some(value) = self.max_message_connections {
            limits.max_message_connections =
                nonzero_limit(value, "--max-message-connections must be greater than zero")?;
        }
        if let Some(value) = self.max_event_connections {
            limits.max_event_connections =
                nonzero_limit(value, "--max-event-connections must be greater than zero")?;
        }
        if let Some(value) = self.max_event_batches_per_connection {
            limits.max_event_batches_per_connection = nonzero_limit(
                value,
                "--max-event-batches-per-connection must be greater than zero",
            )?;
        }
        Ok(ParsedServeOptions {
            message_socket,
            event_socket,
            no_aux_sockets: self.no_aux_sockets,
            attach_primary: self.attach_primary,
            attached_primary_client_id: None,
            limits,
        })
    }
}

/// Carries Runtime Daemon Startup state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RuntimeDaemonStartup {
    /// Represents the Initial case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Initial {
        /// Stores the explicit command value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        explicit_command: Option<String>,
    },
    /// Represents the Restored Snapshot case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    RestoredSnapshot {
        /// Stores the payload value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        payload: Box<SessionSnapshotPayload>,
        /// Stores the restart command value for this data structure.
        ///
        /// The field is part of structured state exchanged across this module
        /// boundary and should remain aligned with the owning type invariant.
        restart_command: Option<String>,
    },
}

/// Carries Loaded Runtime Config state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub(super) struct LoadedRuntimeConfig {
    /// Stores the layers value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) layers: Vec<ConfigLayer>,
    /// Stores the root value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) root: PathBuf,
}

/// Carries Restored Snapshot Daemon Request state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(super) struct RestoredSnapshotDaemonRequest<'a> {
    /// Stores the restored value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) restored: SnapshotRestoreResult,
    /// Stores the payload value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) payload: SessionSnapshotPayload,
    /// Stores the restart command value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) restart_command: Option<String>,
    /// Stores the paths value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) paths: &'a ConfigPaths,
    /// Stores the socket selection value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) socket_selection: &'a SocketSelection,
    /// Stores the owner uid value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) owner_uid: u32,
    /// Stores the options value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) options: ParsedServeOptions,
}

/// Runs the run serve operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn run_serve<W: Write>(
    socket_selection: &SocketSelection,
    args: ServeCliArgs,
    env: CliEnv,
    interactive: bool,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let session_name = args.session_name.clone();
    let mut options = args.into_parsed()?;
    let paths = env.config_paths()?;
    let config_path = paths.ensure_default_config()?;
    let shell = resolve_shell(env.shell)?;
    let terminal_size_fd = if interactive {
        io::stdout().is_terminal().then(|| io::stdout().as_raw_fd())
    } else {
        None
    };
    let (columns, rows) = terminal_size_from_fd_or_environment(terminal_size_fd);
    let mut session = Session::new_default(shell, Size::new(columns, rows)?);
    apply_requested_session_name(&mut session, session_name.as_deref())?;
    assign_unique_live_session_id(&mut session)?;
    let socket_path = selected_socket_path(socket_selection).clone();
    if options.attach_primary && !interactive {
        return Err(MezError::forbidden(
            "starting an attached primary client requires an interactive terminal",
        ));
    }
    if options.attach_primary {
        let primary_client_id = session.attach_primary("primary", true)?;
        options.attached_primary_client_id = Some(primary_client_id);
    }
    apply_default_serve_auxiliary_sockets(&mut options, &socket_path)?;
    validate_serve_options(&options)?;
    let session_id = session.id.to_string();
    let message_socket_json = options
        .message_socket
        .as_ref()
        .map(|path| format!(r#""{}""#, json_escape(&path.to_string_lossy())))
        .unwrap_or_else(|| "null".to_string());
    let event_socket_json = options
        .event_socket
        .as_ref()
        .map(|path| format!(r#""{}""#, json_escape(&path.to_string_lossy())))
        .unwrap_or_else(|| "null".to_string());

    let startup = format!(
        r#"{{"serving":true,"session_id":"{}","socket":"{}","message_socket":{},"event_socket":{},"config":"{}","control":true,"message":{},"event":{}}}"#,
        json_escape(&session_id),
        json_escape(&socket_path.to_string_lossy()),
        message_socket_json,
        event_socket_json,
        json_escape(&config_path.to_string_lossy()),
        options.message_socket.is_some(),
        options.event_socket.is_some(),
    );
    write_json_or_plain(stdout, output_format, &startup)?;
    stdout.flush()?;

    run_foreground_control_daemon(
        session,
        socket_path,
        env.runtime.uid,
        current_unix_seconds()?,
        LoadedRuntimeConfig {
            layers: load_runtime_config_layers(&paths)?,
            root: paths.root().to_path_buf(),
        },
        options,
        RuntimeDaemonStartup::Initial {
            explicit_command: None,
        },
    )
    .await
}

fn apply_requested_session_name(session: &mut Session, name: Option<&str>) -> Result<()> {
    let Some(name) = name else {
        return Ok(());
    };
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        return Err(MezError::invalid_args(
            "session name must be non-empty printable text",
        ));
    }
    session.name = name.to_string();
    Ok(())
}

/// Runs the assign unique live session id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn assign_unique_live_session_id(session: &mut Session) -> Result<()> {
    session.id = unique_live_session_id()?;
    Ok(())
}

/// Runs the unique live session id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn unique_live_session_id() -> Result<SessionId> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| MezError::invalid_state("system clock is before the Unix epoch"))?;
    let timestamp = now
        .as_secs()
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::from(now.subsec_nanos()));
    let process_component = u64::from(std::process::id()) << 32;
    let counter = LIVE_SESSION_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let value = timestamp ^ process_component ^ counter;
    Ok(SessionId::new('$', value.max(1)))
}

/// Returns an absolute optional path or a CLI validation error.
///
/// # Parameters
/// - `path`: The optional path parsed by `clap`.
/// - `message`: The validation message for relative paths.
fn absolute_optional_path(path: Option<PathBuf>, message: &'static str) -> Result<Option<PathBuf>> {
    match path {
        Some(path) if !path.is_absolute() => Err(MezError::invalid_args(message)),
        other => Ok(other),
    }
}

/// Validates a positive numeric serve limit.
///
/// # Parameters
/// - `value`: The parsed limit.
/// - `message`: The validation message for zero.
fn nonzero_limit(value: u64, message: &'static str) -> Result<u64> {
    if value == 0 {
        Err(MezError::invalid_args(message))
    } else {
        Ok(value)
    }
}

/// Runs the apply default serve auxiliary sockets operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn apply_default_serve_auxiliary_sockets(
    options: &mut ParsedServeOptions,
    control_socket: &std::path::Path,
) -> Result<()> {
    if options.no_aux_sockets {
        return Ok(());
    }
    if options.message_socket.is_none() {
        options.message_socket = Some(auxiliary_socket_path_for_control_socket(
            control_socket,
            AuxiliarySocketKind::Message,
        )?);
    }
    if options.event_socket.is_none() {
        options.event_socket = Some(auxiliary_socket_path_for_control_socket(
            control_socket,
            AuxiliarySocketKind::Event,
        )?);
    }
    if options.limits.max_message_connections == 0 {
        options.limits.max_message_connections = u64::MAX;
    }
    if options.limits.max_event_connections == 0 {
        options.limits.max_event_connections = u64::MAX;
    }
    Ok(())
}

/// Runs the validate serve options operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_serve_options(options: &ParsedServeOptions) -> Result<()> {
    if options.message_socket.is_none() && options.limits.max_message_connections > 0 {
        return Err(MezError::invalid_args(
            "--max-message-connections requires a message socket",
        ));
    }
    if options.event_socket.is_none() && options.limits.max_event_connections > 0 {
        return Err(MezError::invalid_args(
            "--max-event-connections requires an event socket",
        ));
    }
    Ok(())
}

/// Runs the run foreground control daemon operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) async fn run_foreground_control_daemon(
    session: Session,
    socket_path: PathBuf,
    owner_uid: u32,
    created_at_unix_seconds: u64,
    config: LoadedRuntimeConfig,
    options: ParsedServeOptions,
    startup: RuntimeDaemonStartup,
) -> Result<()> {
    let attached_primary_client_id = options.attached_primary_client_id.clone();
    let config_layers = direct_unix_session_config_layers(config.layers);
    let runtime = SessionFactory::create(SessionFactoryRequest {
        session,
        owner_uid,
        created_at_unix_seconds,
        config: SessionRuntimeConfig {
            layers: config_layers,
            root: config.root,
        },
        sockets: SessionSocketPublication {
            control_path: socket_path,
            publish_control: true,
            message_path: options.message_socket,
            event_path: options.event_socket,
            publish_registry: true,
        },
        limits: SessionRuntimeLimits {
            max_control_connections: options.limits.max_control_connections,
            max_message_connections: options.limits.max_message_connections,
            max_event_connections: options.limits.max_event_connections,
            max_event_batches_per_connection: options.limits.max_event_batches_per_connection,
        },
        startup: match startup {
            RuntimeDaemonStartup::Initial { explicit_command } => SessionRuntimeStartup::Initial {
                explicit_command,
                start_directory: None,
                environment: None,
            },
            RuntimeDaemonStartup::RestoredSnapshot {
                payload,
                restart_command,
            } => SessionRuntimeStartup::RestoredSnapshot {
                payload,
                restart_command,
            },
        },
    })
    .await?;
    let handle = runtime.handle();
    let attached_client_size = runtime.attached_client_size();
    let mut services = Vec::new();
    if let Some(primary_client_id) = attached_primary_client_id {
        let resize_client_id = primary_client_id.clone();
        services.push(build_foreground_attached_primary_client_service(
            handle.actor().clone(),
            primary_client_id,
            attached_client_size,
        )?);
        services.push(build_foreground_terminal_resize_signal_service(
            handle.actor().clone(),
            resize_client_id,
        )?);
    }
    let completion = runtime.run(services, foreground_shutdown_signal()).await?;
    let _ = (completion.service, completion.supervision);
    Ok(())
}

/// Disables session-scoped Iroh listeners for a daemon reached through Unix
/// sockets.
///
/// Host-scoped Iroh identity belongs exclusively to `mez host serve`; direct
/// `mez` and `mez serve` sessions retain Unix control even when the primary
/// configuration enables the persistent host endpoint.
pub(super) fn direct_unix_session_config_layers(
    mut config_layers: Vec<ConfigLayer>,
) -> Vec<ConfigLayer> {
    config_layers.push(ConfigLayer {
        name: "direct-unix-session-transport".to_string(),
        path: None,
        format: ConfigFormat::Toml,
        scope: ConfigScope::Primary,
        trusted: true,
        text: "[transport.iroh]\nenabled = false\n".to_string(),
    });
    config_layers
}

/// Runs the build foreground attached primary client service operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn build_foreground_attached_primary_client_service(
    handle: crate::host::async_runtime::AsyncRuntimeSessionHandle,
    primary_client_id: ClientId,
    client_size: Size,
) -> Result<AsyncRuntimeService> {
    let input_fd = io::stdin().as_raw_fd();
    let output_fd = io::stdout().as_raw_fd();
    Ok(AsyncRuntimeService::new(
        "attached-terminal-primary",
        async move {
            let mut terminal_guard =
                AsyncAttachedTerminalPresentationGuard::new(input_fd, output_fd, None)?;
            terminal_guard.enter_presentation().await?;
            let run_result = run_async_attached_terminal_client_service(
                &handle,
                terminal_guard.io_mut(),
                AsyncAttachedTerminalLoopRequest {
                    role: ClientViewRole::Primary,
                    client_id: primary_client_id.clone(),
                    primary_client_id: Some(primary_client_id),
                    client_size,
                    terminal_config: TerminalClientLoopConfig::default(),
                    loop_config: AttachedTerminalClientLoopConfig {
                        max_iterations: 1,
                        ..AttachedTerminalClientLoopConfig::default()
                    },
                },
                AsyncAttachedTerminalClientServiceConfig::default(),
                |_| Ok(None),
            )
            .await;
            let restore_result = terminal_guard.restore().await;
            let report = match run_result {
                Ok(report) => {
                    restore_result?;
                    report
                }
                Err(error) => {
                    let _ = restore_result;
                    return Err(error);
                }
            };
            if report.loop_report.input_hangups > 0
                || report.loop_report.output_hangups > 0
                || !report.loop_report.error_roles.is_empty()
            {
                Ok(AsyncRuntimeServiceExit::shutdown(
                    report.loop_report.iterations,
                ))
            } else {
                Ok(attached_terminal_client_service_exit(&report))
            }
        },
    ))
}

/// Runs the build foreground terminal resize signal service operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn build_foreground_terminal_resize_signal_service(
    handle: crate::host::async_runtime::AsyncRuntimeSessionHandle,
    primary_client_id: ClientId,
) -> Result<AsyncRuntimeService> {
    Ok(AsyncRuntimeService::new_auxiliary(
        "attached-terminal-resize-signal",
        async move {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};

                let mut resize = signal(SignalKind::window_change())?;
                let mut observed = 0u64;
                loop {
                    if is_foreground_terminal_resize_signal_stop_state(
                        handle.lifecycle_state().await?,
                    ) {
                        return Ok(AsyncRuntimeServiceExit::completed(observed));
                    }
                    tokio::select! {
                        event = resize.recv() => {
                            if event.is_none() {
                                return Ok(AsyncRuntimeServiceExit::completed(observed));
                            }
                            let mut batch = RuntimeEventBatch::new();
                            batch.push(RuntimeEvent::Client(ClientEvent::ResizeSignal {
                                client_id: primary_client_id.clone(),
                            }));
                            let report = handle.submit_runtime_events(batch).await?;
                            observed = observed.saturating_add(u64::try_from(report.applied).unwrap_or(u64::MAX));
                        }
                        _ = handle.wait_for_event_delivery() => {}
                    }
                }
            }
            #[cfg(not(unix))]
            {
                let _ = handle;
                let _ = primary_client_id;
                Ok(AsyncRuntimeServiceExit::completed(0))
            }
        },
    ))
}

/// Runs the is foreground terminal resize signal stop state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn is_foreground_terminal_resize_signal_stop_state(state: RuntimeLifecycleState) -> bool {
    matches!(
        state,
        RuntimeLifecycleState::Detached
            | RuntimeLifecycleState::Stopping
            | RuntimeLifecycleState::Killed
            | RuntimeLifecycleState::Failed
    )
}

/// Runs the foreground shutdown signal operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
async fn foreground_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let interrupt = signal(SignalKind::interrupt());
        let terminate = signal(SignalKind::terminate());
        let hangup = signal(SignalKind::hangup());
        if let (Ok(mut interrupt), Ok(mut terminate), Ok(mut hangup)) =
            (interrupt, terminate, hangup)
        {
            tokio::select! {
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
                _ = hangup.recv() => {}
            }
            return;
        }
    }

    let _ = tokio::signal::ctrl_c().await;
}
