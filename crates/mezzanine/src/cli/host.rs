//! Persistent local host lifecycle commands and client routing helpers.
//!
//! The CLI communicates with one owner-private host management socket. Normal
//! local commands auto-start the host in production, request a supervised
//! session, and then attach through that session's compatibility control
//! socket. Explicit session sockets and `mez serve` remain direct compatibility
//! paths.

use std::fs;
use std::io::{self, IsTerminal, Write};
use std::os::fd::AsRawFd;
#[cfg(not(test))]
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
#[cfg(not(test))]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
#[cfg(not(test))]
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use rustix::fs::{FlockOperation, Mode, OFlags, flock, open};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{
    CliEnv, CliOutputFormat, MezError, Result, default_socket_directory,
    load_runtime_config_layers, resolve_shell, serialize_json,
    terminal_size_from_fd_or_environment, write_json_or_plain,
};
use crate::host::iroh::HostIrohRuntime;
use crate::host::ownership::HostOwnershipGuard;
use crate::host::server::{HostServer, HostServerConfig, host_socket_path};

const HOST_RESPONSE_LIMIT: usize = 1024 * 1024;
const HOST_STARTUP_LOCK_FILE_NAME: &str = "host.startup.lock";

/// Typed `mez host` lifecycle arguments.
#[derive(Debug, Clone, Args)]
pub(super) struct HostCliArgs {
    /// Persistent host lifecycle operation.
    #[command(subcommand)]
    command: HostCliCommand,
}

/// Persistent local host lifecycle commands.
#[derive(Debug, Clone, Subcommand)]
enum HostCliCommand {
    /// Runs the persistent host in the foreground.
    Serve {
        /// Maximum durable session records accepted by the host.
        #[arg(long, value_name = "N")]
        max_sessions: Option<usize>,
        /// Maximum concurrently live session runtimes.
        #[arg(long, value_name = "N")]
        max_live_sessions: Option<usize>,
    },
    /// Shows readiness and supervised-session counts.
    Status,
    /// Requests graceful host shutdown.
    Stop {
        /// Maximum seconds to wait for the host socket to disappear.
        #[arg(long, default_value_t = 10)]
        timeout: u64,
    },
    /// Prunes stale compatibility discovery records.
    Reconcile,
}

/// Runs one explicit `mez host` command.
pub(super) async fn run_host<W: Write>(
    args: HostCliArgs,
    env: CliEnv,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    match args.command {
        HostCliCommand::Serve {
            max_sessions,
            max_live_sessions,
        } => run_host_serve(env, max_sessions, max_live_sessions, output_format, stdout).await,
        HostCliCommand::Status => {
            let result = request_host(&env, "host/get", serde_json::json!({})).await?;
            write_json_or_plain(stdout, output_format, &serialize_json(&result)?)
        }
        HostCliCommand::Stop { timeout } => {
            let result =
                request_host(&env, "host/shutdown", serde_json::json!({"force": false})).await?;
            write_json_or_plain(stdout, output_format, &serialize_json(&result)?)?;
            wait_for_host_stop(&env, Duration::from_secs(timeout)).await
        }
        HostCliCommand::Reconcile => {
            let result = request_host(&env, "host/reconcile", serde_json::json!({})).await?;
            write_json_or_plain(stdout, output_format, &serialize_json(&result)?)
        }
    }
}

async fn run_host_serve<W: Write>(
    env: CliEnv,
    max_sessions_override: Option<usize>,
    max_live_sessions_override: Option<usize>,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let paths = env.config_paths()?;
    let config_path = paths.ensure_default_config()?;
    let layers = load_runtime_config_layers(&paths)?;
    let structured = crate::runtime::runtime_effective_config_value(&layers)?;
    let host = structured
        .get("host")
        .and_then(serde_json::Value::as_object);
    let max_sessions = max_sessions_override
        .or_else(|| host_usize(host, "max_sessions"))
        .unwrap_or(64);
    let max_live_sessions = max_live_sessions_override
        .or_else(|| host_usize(host, "max_live_sessions"))
        .unwrap_or(16);
    let max_remote_leases = host
        .and_then(|host| host.get("leases"))
        .and_then(serde_json::Value::as_object)
        .and_then(|leases| leases.get("max_per_remote_client"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(8);
    let shutdown_timeout = Duration::from_millis(
        host.and_then(|host| host.get("shutdown_timeout_ms"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(10_000),
    );
    let runtime_root = default_socket_directory(&env.runtime)?.path;
    let ownership = HostOwnershipGuard::acquire(paths.root(), env.runtime.uid)?;
    let iroh_policy = crate::runtime::runtime_iroh_transport_policy_from_config(&structured)?;
    let iroh = HostIrohRuntime::bind(paths.root(), iroh_policy).await?;
    let audit_log = crate::runtime::runtime_audit_log_from_config(&structured, Some(paths.root()))?;
    let server = HostServer::bind_with_ownership(
        HostServerConfig {
            runtime_root,
            owner_uid: env.runtime.uid,
            config_root: paths.root().to_path_buf(),
            config_layers: layers,
            shell: resolve_shell(env.shell)?,
            max_sessions,
            max_live_sessions,
            shutdown_timeout,
            iroh_invitation_issuer: iroh.as_ref().map(HostIrohRuntime::invitation_issuer),
            max_remote_leases,
            audit_log,
        },
        ownership,
    )?;
    server.prepare_startup().await?;
    let started = serde_json::json!({
        "serving": true,
        "host": true,
        "socket": server.socket_path(),
        "config": config_path,
        "iroh_enabled": iroh.is_some(),
        "iroh_endpoint_id": iroh.as_ref().map(HostIrohRuntime::endpoint_id),
    });
    write_json_or_plain(stdout, output_format, &serialize_json(&started)?)?;
    stdout.flush()?;
    let Some(iroh) = iroh else {
        return server.serve(host_shutdown_signal()).await;
    };
    let (shutdown, _) = tokio::sync::watch::channel(false);
    let mut local_shutdown = shutdown.subscribe();
    let mut remote_shutdown = shutdown.subscribe();
    let local = server.serve(async move {
        if *local_shutdown.borrow() {
            return;
        }
        let _ = local_shutdown.changed().await;
    });
    let remote = iroh.serve_routed(server.router(), async move {
        if *remote_shutdown.borrow() {
            return;
        }
        let _ = remote_shutdown.changed().await;
    });
    tokio::pin!(local);
    tokio::pin!(remote);
    tokio::select! {
        result = &mut local => {
            let _ = shutdown.send(true);
            result?;
            remote.await.map(|_| ())
        }
        result = &mut remote => {
            let _ = shutdown.send(true);
            result?;
            local.await
        }
        () = host_shutdown_signal() => {
            let _ = shutdown.send(true);
            let (local_result, remote_result) = tokio::join!(local, remote);
            local_result?;
            remote_result.map(|_| ())
        }
    }
}

fn host_usize(
    host: Option<&serde_json::Map<String, serde_json::Value>>,
    key: &str,
) -> Option<usize> {
    host?
        .get(key)?
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
}

/// Returns true when the host is ready, auto-starting it in production.
pub(super) async fn ensure_host_available(env: &CliEnv) -> Result<bool> {
    if request_host(env, "host/get", serde_json::json!({}))
        .await
        .is_ok()
    {
        return Ok(true);
    }
    if !host_auto_start_enabled(env)? {
        return Ok(false);
    }
    #[cfg(test)]
    {
        Ok(false)
    }
    #[cfg(not(test))]
    {
        let runtime_root = default_socket_directory(&env.runtime)?.path;
        crate::runtime::ensure_private_socket_directory(&runtime_root, env.runtime.uid)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if request_host(env, "host/get", serde_json::json!({}))
                .await
                .is_ok()
            {
                return Ok(true);
            }
            if let Some(_election) = acquire_host_startup_election(&runtime_root, env.runtime.uid)?
            {
                if request_host(env, "host/get", serde_json::json!({}))
                    .await
                    .is_ok()
                {
                    return Ok(true);
                }
                spawn_background_host(env)?;
                while Instant::now() < deadline {
                    if request_host(env, "host/get", serde_json::json!({}))
                        .await
                        .is_ok()
                    {
                        return Ok(true);
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Err(MezError::invalid_state(
            "persistent host did not become ready before timeout",
        ))
    }
}

/// Returns whether primary-user policy opts ordinary local commands into host startup.
fn host_auto_start_enabled(env: &CliEnv) -> Result<bool> {
    let paths = env.config_paths()?;
    let layers = load_runtime_config_layers(&paths)?;
    let structured = crate::runtime::runtime_effective_config_value(&layers)?;
    let host = structured
        .get("host")
        .and_then(serde_json::Value::as_object);
    let enabled = host
        .and_then(|host| host.get("enabled"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let auto_start = host
        .and_then(|host| host.get("auto_start_local"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    Ok(enabled && auto_start)
}

/// Elects at most one concurrent process to launch the persistent host.
fn acquire_host_startup_election(
    runtime_root: &std::path::Path,
    owner_uid: u32,
) -> Result<Option<fs::File>> {
    crate::runtime::ensure_private_socket_directory(runtime_root, owner_uid)?;
    let path = runtime_root.join(HOST_STARTUP_LOCK_FILE_NAME);
    let descriptor = open(
        &path,
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(std::io::Error::from)?;
    let file = fs::File::from(descriptor);
    let metadata = file.metadata()?;
    if metadata.uid() != owner_uid || metadata.permissions().mode() & 0o077 != 0 {
        return Err(MezError::forbidden(
            "host startup lock must be private and owned by the current user",
        ));
    }
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(file)),
        Err(error) if error == rustix::io::Errno::WOULDBLOCK => Ok(None),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}

/// Creates a fresh supervised local session and returns its compatibility socket.
pub(super) async fn host_create_session(env: &CliEnv, name: Option<&str>) -> Result<PathBuf> {
    let mut params = local_host_launch_context(env)?;
    params.insert(
        "name".to_string(),
        name.map_or(serde_json::Value::Null, |name| name.into()),
    );
    let result = request_host(
        env,
        "host/session/create",
        serde_json::Value::Object(params),
    )
    .await?;
    host_result_socket(&result)
}

/// Atomically resolves the primary-attachable hosted session or creates one.
pub(super) async fn host_resolve_or_create_session(env: &CliEnv) -> Result<PathBuf> {
    let result = request_host(
        env,
        "host/session/resolve-or-create",
        serde_json::Value::Object(local_host_launch_context(env)?),
    )
    .await?;
    host_result_socket(&result)
}

fn local_host_launch_context(env: &CliEnv) -> Result<serde_json::Map<String, serde_json::Value>> {
    let current_directory = std::env::current_dir()?.canonicalize()?;
    let shell = resolve_shell(env.shell.clone())?;
    let terminal_fd = io::stdout().is_terminal().then(|| io::stdout().as_raw_fd());
    let (columns, rows) = terminal_size_from_fd_or_environment(terminal_fd);
    let mut environment = serde_json::Map::new();
    if let Some(home) = &env.home {
        environment.insert(
            "HOME".to_string(),
            home.to_string_lossy().into_owned().into(),
        );
    }
    environment.insert(
        "SHELL".to_string(),
        shell.path().to_string_lossy().into_owned().into(),
    );
    environment.insert("COLUMNS".to_string(), columns.to_string().into());
    environment.insert("LINES".to_string(), rows.to_string().into());
    for key in [
        "PATH",
        "USER",
        "LOGNAME",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "COLORTERM",
        "TERM_PROGRAM",
        "TERM_PROGRAM_VERSION",
        "TERM_FEATURES",
        "NO_COLOR",
    ] {
        if let Ok(value) = std::env::var(key) {
            environment.insert(key.to_string(), value.into());
        }
    }
    Ok(serde_json::Map::from_iter([
        (
            "cwd".to_string(),
            current_directory.to_string_lossy().into_owned().into(),
        ),
        (
            "shell".to_string(),
            shell.path().to_string_lossy().into_owned().into(),
        ),
        ("columns".to_string(), columns.into()),
        ("rows".to_string(), rows.into()),
        ("environment".to_string(), environment.into()),
    ]))
}

/// Resolves an existing supervised local session without creating one.
pub(super) async fn host_resolve_session(
    env: &CliEnv,
    target: Option<&str>,
    role: &str,
) -> Result<PathBuf> {
    let result = request_host(
        env,
        "host/session/resolve",
        serde_json::json!({"target": target, "role": role}),
    )
    .await?;
    host_result_socket(&result)
}

/// Lists host-supervised compatibility session records.
pub(super) async fn host_list_sessions(env: &CliEnv) -> Result<serde_json::Value> {
    let result = request_host(env, "host/session/list", serde_json::json!({})).await?;
    Ok(result
        .get("sessions")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new())))
}

fn host_result_socket(result: &serde_json::Value) -> Result<PathBuf> {
    result
        .get("socket")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| MezError::invalid_state("host response omitted session socket"))
}

pub(super) async fn request_host(
    env: &CliEnv,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let runtime_root = default_socket_directory(&env.runtime)?.path;
    let socket = host_socket_path(&runtime_root)?;
    let mut stream = tokio::net::UnixStream::connect(&socket).await?;
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "host-cli",
        "method": method,
        "params": params,
    })
    .to_string();
    stream
        .write_all(&crate::control::encode_control_body(&request))
        .await?;
    stream.flush().await?;
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            return Err(MezError::invalid_state(
                "host control socket closed before a complete response",
            ));
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > HOST_RESPONSE_LIMIT + 8192 {
            return Err(MezError::invalid_state("host response exceeds limit"));
        }
        if let Ok((body, _)) = crate::control::decode_control_frame(&bytes, HOST_RESPONSE_LIMIT) {
            let value: serde_json::Value = serde_json::from_str(&body).map_err(|error| {
                MezError::invalid_state(format!("invalid host response JSON: {error}"))
            })?;
            if let Some(error) = value.get("error") {
                return Err(host_response_error(error));
            }
            return value
                .get("result")
                .cloned()
                .ok_or_else(|| MezError::invalid_state("host response omitted result"));
        }
    }
}

fn host_response_error(error: &serde_json::Value) -> MezError {
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("host request failed");
    match error
        .pointer("/data/mezzanine_code")
        .and_then(serde_json::Value::as_str)
    {
        Some("notfound" | "not_found") => {
            MezError::new(crate::error::MezErrorKind::NotFound, message)
        }
        Some("conflict") => MezError::conflict(message),
        Some("forbidden") => MezError::forbidden(message),
        Some("invalidargs" | "invalid_args" | "invalid_params") => MezError::invalid_args(message),
        _ => MezError::invalid_state(message),
    }
}

#[cfg(not(test))]
fn spawn_background_host(env: &CliEnv) -> Result<()> {
    let executable = std::env::current_exe()?;
    let runtime_root = default_socket_directory(&env.runtime)?.path;
    crate::runtime::ensure_private_socket_directory(&runtime_root, env.runtime.uid)?;
    let diagnostic_path = runtime_root.join("host.diagnostics.log");
    let diagnostic = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&diagnostic_path)?;
    std::fs::set_permissions(&diagnostic_path, std::fs::Permissions::from_mode(0o600))?;
    let mut command = Command::new(executable);
    command
        .arg("host")
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(diagnostic));
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
    unsafe {
        command.pre_exec(|| rustix::process::setsid().map(|_| ()).map_err(Into::into));
    }
    let _child = command.spawn()?;
    Ok(())
}

async fn wait_for_host_stop(env: &CliEnv, timeout: Duration) -> Result<()> {
    let runtime_root = default_socket_directory(&env.runtime)?.path;
    let socket = host_socket_path(&runtime_root)?;
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !socket.exists() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err(MezError::invalid_state(
        "persistent host did not stop before timeout",
    ))
}

async fn host_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let (Ok(mut interrupt), Ok(mut terminate), Ok(mut hangup)) = (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
            signal(SignalKind::hangup()),
        ) {
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

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use crate::config::{ConfigFormat, ConfigLayer, ConfigScope, DEFAULT_CONFIG_TOML};
    use crate::host::shell::{ResolvedShell, ShellSource};
    use crate::runtime::RuntimeEnv;

    use super::*;

    /// Only one concurrent cold-start caller owns the launcher election, and
    /// releasing that owner permits a waiter to recover a failed startup.
    #[test]
    fn host_startup_election_has_one_recoverable_owner() {
        let root = test_root("startup-election");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let owner_uid = crate::runtime::effective_uid_for_tests();

        let elected = acquire_host_startup_election(&root, owner_uid)
            .unwrap()
            .expect("first caller must be elected");
        assert!(
            acquire_host_startup_election(&root, owner_uid)
                .unwrap()
                .is_none()
        );
        drop(elected);
        assert!(
            acquire_host_startup_election(&root, owner_uid)
                .unwrap()
                .is_some()
        );

        let _ = fs::remove_dir_all(root);
    }

    /// The CLI host client must exercise the protected management socket for
    /// status, fresh creation, default resolution, listing, reconciliation,
    /// and graceful shutdown without bypassing the session supervisor.
    #[tokio::test(flavor = "current_thread")]
    async fn host_management_socket_routes_lifecycle_and_session_requests() {
        let root = test_root("management");
        let env = test_env(&root);
        let runtime_root = default_socket_directory(&env.runtime).unwrap().path;
        let config_root = env.config_paths().unwrap().root().to_path_buf();
        let server = HostServer::bind(HostServerConfig {
            runtime_root,
            owner_uid: env.runtime.uid,
            config_root,
            config_layers: vec![ConfigLayer {
                name: "host-cli-test".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: DEFAULT_CONFIG_TOML.to_string(),
            }],
            shell: ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
            max_sessions: 8,
            max_live_sessions: 4,
            shutdown_timeout: Duration::from_secs(2),
            iroh_invitation_issuer: None,
            max_remote_leases: 8,
            audit_log: None,
        })
        .unwrap();

        let server_future = server.serve(std::future::pending());
        let client_future = async {
            let status = request_host(&env, "host/get", serde_json::json!({}))
                .await
                .unwrap();
            assert_eq!(status["ready"], true);
            assert_eq!(status["running_sessions"], 0);

            let first = host_resolve_or_create_session(&env).await.unwrap();
            let selected_again = host_resolve_or_create_session(&env).await.unwrap();
            assert_eq!(selected_again, first);
            let resolved = host_resolve_session(&env, None, "primary").await.unwrap();
            assert_eq!(resolved, first);
            let second = host_create_session(&env, Some("second")).await.unwrap();
            assert_ne!(second, first);

            let sessions = host_list_sessions(&env).await.unwrap();
            assert_eq!(sessions.as_array().unwrap().len(), 2);
            let reconciled = request_host(&env, "host/reconcile", serde_json::json!({}))
                .await
                .unwrap();
            assert_eq!(reconciled["reconciled"], true);

            let shutdown = request_host(&env, "host/shutdown", serde_json::json!({"force": true}))
                .await
                .unwrap();
            assert_eq!(shutdown["shutting_down"], true);
        };

        let (server_result, ()) = tokio::join!(server_future, client_future);
        server_result.unwrap();
        drop(server);
        assert!(
            !host_socket_path(&default_socket_directory(&env.runtime).unwrap().path)
                .unwrap()
                .exists()
        );
        let _ = fs::remove_dir_all(root);
    }

    /// A noninteractive bare invocation fails before the hosted
    /// resolve-or-create request can allocate a session.
    #[tokio::test(flavor = "current_thread")]
    async fn noninteractive_bare_cli_does_not_create_hosted_session() {
        let root = test_root("noninteractive-bare");
        let env = test_env(&root);
        let runtime_root = default_socket_directory(&env.runtime).unwrap().path;
        let config_root = env.config_paths().unwrap().root().to_path_buf();
        let server = HostServer::bind(HostServerConfig {
            runtime_root,
            owner_uid: env.runtime.uid,
            config_root,
            config_layers: vec![ConfigLayer {
                name: "host-cli-test".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: DEFAULT_CONFIG_TOML.to_string(),
            }],
            shell: ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
            max_sessions: 8,
            max_live_sessions: 4,
            shutdown_timeout: Duration::from_secs(2),
            iroh_invitation_issuer: None,
            max_remote_leases: 8,
            audit_log: None,
        })
        .unwrap();

        let server_future = server.serve(std::future::pending());
        let client_future = async {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let error = crate::cli::run_with(
                vec!["mez".to_string()],
                env.clone(),
                false,
                &mut stdout,
                &mut stderr,
            )
            .await
            .unwrap_err();
            assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
            assert!(error.message().contains("interactive terminal"));
            assert!(
                host_list_sessions(&env)
                    .await
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .is_empty()
            );

            request_host(&env, "host/shutdown", serde_json::json!({"force": true}))
                .await
                .unwrap();
        };

        let (server_result, ()) = tokio::join!(server_future, client_future);
        server_result.unwrap();
        drop(server);
        let _ = fs::remove_dir_all(root);
    }

    fn test_env(root: &Path) -> CliEnv {
        let runtime_tmp = root.join("runtime");
        fs::create_dir_all(&runtime_tmp).unwrap();
        fs::set_permissions(&runtime_tmp, fs::Permissions::from_mode(0o700)).unwrap();
        CliEnv {
            home: Some(root.to_path_buf()),
            shell: Some(OsString::from("/bin/sh")),
            mez: None,
            runtime: RuntimeEnv {
                mez_tmpdir: Some(runtime_tmp.into_os_string()),
                xdg_runtime_dir: None,
                tmpdir: None,
                uid: crate::runtime::effective_uid_for_tests(),
            },
        }
    }

    fn test_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mez-cli-host-{label}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }
}
