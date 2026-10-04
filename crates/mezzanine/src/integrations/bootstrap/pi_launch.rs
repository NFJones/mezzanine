//! Explicit local-child launch with a private, observation-only Unix stream.
//!
//! The caller owns launch authorization, executable provenance, vendor session
//! binding and all child lifecycle decisions. This module performs no discovery,
//! capability issuance, registration, installation or vendor execution policy.
//! Only caller-supplied arguments/environment reach the child; daemon authority
//! remains in the separately owned parent transport. Descriptor 3 is the sole
//! observer endpoint. Telemetry completion/failure must not terminate the child.
//! Dropping the returned child does not kill it; its owner must wait/reap it.

use std::ffi::OsString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;

use crate::error::{MezError, Result};

/// Exact launch inputs from an independently authorized caller, never Debug.
/// No inherited environment, PATH lookup or shell expansion is performed here.
pub(crate) struct LaunchSpec {
    pub(crate) executable: PathBuf,
    pub(crate) directory: PathBuf,
    pub(crate) arguments: Vec<OsString>,
    pub(crate) environment: Vec<(OsString, OsString)>,
    pub(crate) stdin: Stdio,
    pub(crate) stdout: Stdio,
    pub(crate) stderr: Stdio,
}

/// Separate process and telemetry ownership: the caller must reap the child,
/// and may drive/drop the observer without changing vendor behavior.
pub(crate) struct Launched {
    pub(crate) child: tokio::process::Child,
    pub(crate) observer: tokio::net::UnixStream,
}

/// Generic diagnostics deliberately omit executable, arguments and environment.
fn unavailable() -> MezError {
    MezError::invalid_state("Pi observer launch unavailable")
}

/// Starts one explicitly selected child with an inherited observer endpoint.
/// All fallible stream setup occurs before spawn. A spawn error leaves no child;
/// successful return transfers reaping ownership without starting any tasks.
pub(crate) fn spawn(spec: LaunchSpec) -> Result<Launched> {
    if !spec.executable.is_absolute() || !spec.directory.is_absolute() {
        return Err(unavailable());
    }
    let (parent, endpoint) = std::os::unix::net::UnixStream::pair().map_err(|_| unavailable())?;
    parent.set_nonblocking(true).map_err(|_| unavailable())?;
    let observer = tokio::net::UnixStream::from_std(parent).map_err(|_| unavailable())?;
    // SAFETY: duplicates a live endpoint into a distinct owned CLOEXEC fd above
    // stdio and destination 3, so dup2 cannot accidentally retain CLOEXEC.
    let raw = unsafe { libc::fcntl(endpoint.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    if raw < 0 {
        return Err(unavailable());
    }
    // SAFETY: successful fcntl returned a newly owned descriptor.
    let source = unsafe { OwnedFd::from_raw_fd(raw) };
    let raw = source.as_raw_fd();
    let mut command = tokio::process::Command::new(spec.executable);
    command
        .args(spec.arguments)
        .env_clear()
        .envs(spec.environment)
        .current_dir(spec.directory)
        .stdin(spec.stdin)
        .stdout(spec.stdout)
        .stderr(spec.stderr);
    // SAFETY: the post-fork closure calls only async-signal-safe dup2 using
    // captured integers. Source lives through spawn; all parent copies remain
    // CLOEXEC. Destination 3 alone survives exec, without daemon credentials.
    unsafe {
        command.as_std_mut().pre_exec(move || {
            if libc::dup2(raw, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().map_err(|_| unavailable())?;
    Ok(Launched { child, observer })
}

#[cfg(test)]
mod tests;
