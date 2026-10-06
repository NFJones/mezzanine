//! Explicit broker process launch and child ownership, not CLI activation.
//!
//! The caller supplies an absolute executable and HOME matching its elected
//! configuration root. Fixed argv and a cleared environment carry no remote
//! credentials. Diagnostics are opened relative to the held root, without
//! following links or blocking on special nodes. The returned child stays
//! caller-owned for startup observation and reaping; dropping a frontend or
//! launch handle must not terminate a broker shared by sibling frontends.
//! Failed readiness does not prove this child owns a published listener.

use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use super::election::StartupElection;
use crate::cli::CliEnv;
use crate::error::{MezError, Result};

const DIAGNOSTIC_NAME: &str = "outbound.diagnostics.log";

/// Connects through protected election/readiness with an explicitly selected
/// executable. The caller retains any spawned child even when readiness fails
/// or this future is cancelled; no socket observation authorizes killing it.
/// This is not ordinary attach/new activation or a background reaper.
pub(in crate::cli) async fn connect_owned(
    executable: &Path,
    env: &CliEnv,
    budget: std::time::Duration,
    child: &mut Option<LaunchedBroker>,
) -> Result<crate::host::outbound_frontend::client::OutboundFrontendClient> {
    if !(std::time::Duration::from_millis(100)..=std::time::Duration::from_secs(120))
        .contains(&budget)
    {
        return Err(MezError::invalid_args(
            "outbound startup deadline unavailable",
        ));
    }
    let paths = env.config_paths()?;
    let layers = crate::cli::load_runtime_config_layers(&paths)?;
    let structured = crate::runtime::runtime_effective_config_value(&layers)?;
    let policy = crate::runtime::runtime_iroh_transport_policy_from_config(&structured)?;
    if !policy.outbound_enabled {
        return Err(MezError::forbidden("Iroh outbound transport is disabled"));
    }
    paths.ensure_default_config()?;
    super::startup::connect_with_launcher(paths.root(), budget, |election| {
        if child.is_some() {
            return Err(MezError::conflict(
                "outbound startup child already retained; inspect it before retrying",
            ));
        }
        *child = Some(LaunchedBroker::spawn(executable, env, election)?);
        Ok(())
    })
    .await
}

/// One exact spawned child. Drop does not kill it; Tokio's best-effort reaper
/// is not a guarantee of bounded reaping. Deliberate callers should wait for exit.
pub(in crate::cli) struct LaunchedBroker {
    child: tokio::process::Child,
}

impl LaunchedBroker {
    /// Spawns only the supplied absolute executable under validated election.
    /// The returned process, not a discovered socket owner, is the caller's child.
    pub(super) fn spawn(
        executable: &Path,
        env: &CliEnv,
        election: &StartupElection,
    ) -> Result<Self> {
        let command = launch_command(executable, env, election)?;
        election.validate()?;
        let child = tokio::process::Command::from(command)
            .kill_on_drop(false)
            .spawn()?;
        Ok(Self { child })
    }

    /// Observes/reaps an exited child without reading or exposing diagnostic contents.
    pub(in crate::cli) fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        self.child.try_wait().map_err(Into::into)
    }

    /// Waits for this exact child's exit. Cancellation retains the child in this
    /// handle; no automatic kill, relaunch or application retry occurs.
    pub(in crate::cli) async fn wait(&mut self) -> Result<ExitStatus> {
        self.child.wait().await.map_err(Into::into)
    }

    /// Stops this caller-owned child only when explicitly requested. This is
    /// not a frontend-disconnect action and must not be inferred from readiness.
    pub(super) async fn terminate(&mut self) -> Result<ExitStatus> {
        self.child.start_kill()?;
        self.wait().await
    }
}

/// Constructs fixed launch inputs and private diagnostic ownership. It performs
/// no PATH lookup, credential export or shell wrapping. Same-user executable
/// replacement remains the caller's provenance boundary, not an fd-exec guarantee.
fn launch_command(executable: &Path, env: &CliEnv, election: &StartupElection) -> Result<Command> {
    if !executable.is_absolute() {
        return Err(MezError::invalid_args(
            "outbound broker executable must be absolute",
        ));
    }
    let home = env
        .home
        .as_ref()
        .ok_or_else(|| MezError::invalid_args("outbound broker HOME must be explicit"))?;
    if !home.is_absolute() {
        return Err(MezError::invalid_args(
            "outbound broker HOME must be absolute",
        ));
    }
    let (root_path, root) = election.launch_root()?;
    if std::fs::canonicalize(crate::config::ConfigPaths::from_home(home.clone()).root())?
        != root_path
    {
        return Err(MezError::conflict(
            "outbound broker HOME does not match elected root",
        ));
    }
    let descriptor = rustix::fs::openat(
        root,
        DIAGNOSTIC_NAME,
        rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::APPEND
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .map_err(std::io::Error::from)?;
    let diagnostic = std::fs::File::from(descriptor);
    let metadata = diagnostic.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != crate::runtime::current_effective_uid()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(MezError::forbidden(
            "outbound diagnostics must be a private regular file",
        ));
    }
    election.validate()?;
    let mut command = Command::new(executable);
    command
        .args(["remote", "outbound-serve"])
        .env_clear()
        .env("HOME", home)
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(diagnostic));
    // SAFETY: setsid is the only child pre-exec operation and accesses no Rust
    // shared state; it detaches this process from the launcher's terminal session.
    unsafe {
        command.pre_exec(|| rustix::process::setsid().map(|_| ()).map_err(Into::into));
    }
    Ok(command)
}

#[cfg(test)]
mod tests;
