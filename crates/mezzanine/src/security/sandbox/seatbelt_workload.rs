//! Canonical private artifacts retained for one Seatbelt workload.
//!
//! Pane and native transports share this owner so policy compilation receives
//! concrete canonical host paths before launch. Each workload gets an
//! owner-only action directory, command and environment files, and temporary
//! directory. Trusted projects may reuse their backend-tagged managed HOME;
//! other workloads receive an ephemeral HOME below the action directory. A
//! cloneable lease retains the managed-home activity lock and removes only the
//! private action tree after every transport owner releases it.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::managed_home::{
    SandboxManagedHomeActivityLock, SeatbeltEphemeralHome, prepare_seatbelt_ephemeral_home,
    prepare_seatbelt_managed_home_for_workload,
};
use super::{SandboxCompileError, SandboxCompileErrorKind};

const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const ENVIRONMENT_DOCUMENT_MODE: u32 = 0o400;
const ENVIRONMENT_DOCUMENT_MAX_BYTES: usize = 64 * 1024;
const INPUT_SIDECAR_LINE_PREFIX: &str = "# __MEZ_INPUT_SIDECAR_V1__ ";

/// Canonical paths and lease retained for one compiled Seatbelt workload.
#[derive(Debug)]
pub(crate) struct SeatbeltWorkloadArtifacts {
    /// Canonical private action directory containing transient workload state.
    pub(crate) action_directory: PathBuf,
    /// Canonical owner-only command file read by the payload shell.
    pub(crate) command_file_path: PathBuf,
    /// Canonical owner-only environment document read by the child launcher.
    pub(crate) environment_file_path: PathBuf,
    /// Canonical private HOME projected into the payload environment.
    pub(crate) home_directory: PathBuf,
    /// Canonical private temporary directory projected as `TMPDIR`.
    pub(crate) temporary_directory: PathBuf,
    /// Cloneable cleanup and managed-home activity lease.
    pub(crate) lease: SeatbeltWorkloadLease,
}

impl SeatbeltWorkloadArtifacts {
    /// Writes the compiler-produced environment document exactly once before
    /// launch and makes it owner-read-only for the sandboxed child.
    pub(crate) fn write_environment_document(
        &self,
        document: &[u8],
    ) -> Result<(), SandboxCompileError> {
        if document.len() > ENVIRONMENT_DOCUMENT_MAX_BYTES {
            return Err(workload_error(
                "Seatbelt environment document exceeds its bounded size",
            ));
        }
        fs::write(&self.environment_file_path, document).map_err(|error| {
            workload_error(format!(
                "Seatbelt environment document write failed: {error}"
            ))
        })?;
        fs::set_permissions(
            &self.environment_file_path,
            fs::Permissions::from_mode(ENVIRONMENT_DOCUMENT_MODE),
        )
        .map_err(|error| {
            workload_error(format!(
                "Seatbelt environment document permission update failed: {error}"
            ))
        })
    }
}

/// Cloneable owner retaining transient cleanup and persistent-home exclusion.
#[derive(Debug, Clone)]
pub(crate) struct SeatbeltWorkloadLease {
    inner: Arc<SeatbeltWorkloadLeaseInner>,
}

impl PartialEq for SeatbeltWorkloadLease {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for SeatbeltWorkloadLease {}

impl SeatbeltWorkloadLease {
    /// Returns the private action directory for cleanup regressions.
    #[cfg(test)]
    pub(crate) fn action_directory_for_tests(&self) -> &Path {
        &self.inner.action_directory
    }
}

#[derive(Debug)]
struct SeatbeltWorkloadLeaseInner {
    action_directory: PathBuf,
    _activity_lock: Option<SandboxManagedHomeActivityLock>,
    _ephemeral_home: Option<SeatbeltEphemeralHome>,
}

impl Drop for SeatbeltWorkloadLeaseInner {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.action_directory);
    }
}

/// Materializes canonical owner-only state for one pane or native Seatbelt
/// workload without exposing the user's real HOME.
pub(crate) fn prepare_seatbelt_workload_artifacts(
    config_root: Option<&Path>,
    trusted_project_root: Option<&Path>,
    command: &str,
    input_sidecar: Option<&str>,
) -> Result<SeatbeltWorkloadArtifacts, SandboxCompileError> {
    let action_directory = create_private_action_directory()?;
    let temporary_directory = action_directory.join("tmp");
    create_private_directory(&temporary_directory)?;
    let temporary_directory = canonicalize(&temporary_directory, "temporary directory")?;

    let (home_directory, activity_lock, ephemeral_home) = match (config_root, trusted_project_root)
    {
        (Some(config_root), Some(project_root)) => {
            let (home, activity_lock) =
                prepare_seatbelt_managed_home_for_workload(config_root, project_root)?;
            (home.host_path, Some(activity_lock), None)
        }
        _ => {
            let home = prepare_seatbelt_ephemeral_home(&action_directory)?;
            (home.host_path.clone(), None, Some(home))
        }
    };

    let command_file_path = action_directory.join("command");
    write_command_file(&command_file_path, command, input_sidecar)?;
    let command_file_path = canonicalize(&command_file_path, "command file")?;
    let environment_file_path = action_directory.join("environment.json");
    write_private_file(&environment_file_path, &[])?;
    let environment_file_path = canonicalize(&environment_file_path, "environment document")?;
    let lease = SeatbeltWorkloadLease {
        inner: Arc::new(SeatbeltWorkloadLeaseInner {
            action_directory: action_directory.clone(),
            _activity_lock: activity_lock,
            _ephemeral_home: ephemeral_home,
        }),
    };

    Ok(SeatbeltWorkloadArtifacts {
        action_directory,
        command_file_path,
        environment_file_path,
        home_directory,
        temporary_directory,
        lease,
    })
}

fn create_private_action_directory() -> Result<PathBuf, SandboxCompileError> {
    let root = std::env::temp_dir();
    for attempt in 0..8_u8 {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = root.join(format!(
            "mez-seatbelt-action-{}-{unique}-{attempt}",
            std::process::id()
        ));
        let mut builder = fs::DirBuilder::new();
        builder.mode(PRIVATE_DIRECTORY_MODE);
        match builder.create(&path) {
            Ok(()) => {
                fs::set_permissions(&path, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
                    .map_err(|error| {
                        workload_error(format!(
                            "Seatbelt action-directory permission update failed: {error}"
                        ))
                    })?;
                return canonicalize(&path, "action directory");
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(workload_error(format!(
                    "Seatbelt action-directory creation failed: {error}"
                )));
            }
        }
    }
    Err(workload_error(
        "Seatbelt action-directory allocation exhausted its bounded attempts",
    ))
}

fn create_private_directory(path: &Path) -> Result<(), SandboxCompileError> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(PRIVATE_DIRECTORY_MODE);
    builder.create(path).map_err(|error| {
        workload_error(format!(
            "Seatbelt private directory creation failed: {error}"
        ))
    })?;
    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).map_err(|error| {
        workload_error(format!(
            "Seatbelt private directory permission update failed: {error}"
        ))
    })
}

fn write_command_file(
    path: &Path,
    command: &str,
    input_sidecar: Option<&str>,
) -> Result<(), SandboxCompileError> {
    let mut content = command.to_string();
    if !content.ends_with('\n') {
        content.push('\n');
    }
    if let Some(sidecar) = input_sidecar {
        for line in sidecar.lines() {
            content.push_str(INPUT_SIDECAR_LINE_PREFIX);
            content.push_str(line);
            content.push('\n');
        }
    }
    write_private_file(path, content.as_bytes())
}

fn write_private_file(path: &Path, content: &[u8]) -> Result<(), SandboxCompileError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)
        .map_err(|error| {
            workload_error(format!("Seatbelt private file creation failed: {error}"))
        })?;
    file.write_all(content)
        .map_err(|error| workload_error(format!("Seatbelt private file write failed: {error}")))?;
    file.sync_all()
        .map_err(|error| workload_error(format!("Seatbelt private file sync failed: {error}")))?;
    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_FILE_MODE)).map_err(|error| {
        workload_error(format!(
            "Seatbelt private file permission update failed: {error}"
        ))
    })
}

fn canonicalize(path: &Path, label: &str) -> Result<PathBuf, SandboxCompileError> {
    fs::canonicalize(path).map_err(|error| {
        workload_error(format!("Seatbelt {label} canonicalization failed: {error}"))
    })
}

fn workload_error(message: impl Into<String>) -> SandboxCompileError {
    SandboxCompileError::new(SandboxCompileErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies transient Seatbelt artifacts are canonical and owner-only,
    /// preserve semantic-patch sidecars, and disappear with the final lease.
    #[test]
    fn workload_artifacts_are_private_and_cleanup_with_lease() {
        let artifacts =
            prepare_seatbelt_workload_artifacts(None, None, "printf ok", Some("0 cGF5bG9hZA==\n"))
                .unwrap();
        assert_eq!(
            fs::canonicalize(&artifacts.action_directory).unwrap(),
            artifacts.action_directory
        );
        assert_eq!(
            fs::metadata(&artifacts.action_directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let command = fs::read_to_string(&artifacts.command_file_path).unwrap();
        assert!(command.contains("printf ok\n"));
        assert!(command.contains("# __MEZ_INPUT_SIDECAR_V1__ 0 cGF5bG9hZA==\n"));
        artifacts.write_environment_document(b"{}\n").unwrap();
        assert_eq!(
            fs::metadata(&artifacts.environment_file_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o400
        );
        let root = artifacts.action_directory.clone();
        let lease = artifacts.lease.clone();
        drop(artifacts);
        assert!(root.exists());
        drop(lease);
        assert!(!root.exists());
    }
}
