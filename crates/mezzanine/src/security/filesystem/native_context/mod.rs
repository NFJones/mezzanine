//! Kernel context readers for native filesystem roots, without helper launches.
//!
//! Both supported systems supply actual target credentials and kernel cwd
//! identity. Missing or inconsistent evidence fails closed; neither shell
//! inference nor daemon-group substitution can authorize a foreign process.

#[cfg(not(target_os = "macos"))]
use crate::error::MezError;
use crate::error::Result;
use mez_mux::process::ProcessCredentials;
use std::fs::File;
#[cfg(target_os = "macos")]
mod darwin;

/// Reads and normalizes actual target filesystem credentials, never substitutes.
pub(super) fn credentials(pid: u32) -> Result<ProcessCredentials> {
    #[cfg(target_os = "macos")]
    let mut credentials = darwin::credentials(pid)?;
    #[cfg(not(target_os = "macos"))]
    let mut credentials = mez_mux::process::filesystem_credentials_for_pid(pid)
        .ok_or_else(|| MezError::invalid_state("native filesystem credentials unavailable"))?;
    credentials.supplementary_group_ids.sort_unstable();
    credentials.supplementary_group_ids.dedup();
    Ok(credentials)
}

/// Opens a cwd object matched to kernel identity, not an unqualified path reopen.
pub(super) fn open_process_cwd(pid: u32) -> Result<File> {
    #[cfg(target_os = "linux")]
    {
        rustix::fs::openat(
            rustix::fs::CWD,
            format!("/proc/{pid}/cwd"),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map(File::from)
        .map_err(|error| {
            MezError::invalid_state(format!("native kernel cwd witness unavailable: {error}"))
        })
    }
    #[cfg(target_os = "macos")]
    {
        darwin::open_process_cwd(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        Err(MezError::forbidden(
            "native kernel cwd witness unsupported on this platform",
        ))
    }
}
