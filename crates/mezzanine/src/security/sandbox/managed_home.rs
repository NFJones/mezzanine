//! Managed persistent home directories for trusted-project Bubblewrap runs.
//!
//! Homes live below the private Mezzanine configuration root and are keyed by
//! canonical project identity plus the fixed sandbox profile. The helper never
//! copies host-home content and rejects symlinked storage components.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{BUBBLEWRAP_RUNTIME_PROFILE_VERSION, SandboxCompileError, SandboxCompileErrorKind};

/// Prepared host-side home projected at `/home/mez` inside Bubblewrap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BubblewrapManagedHome {
    /// Private host directory mounted read-write as the synthetic home.
    pub(crate) host_path: PathBuf,
    /// Stable non-secret project/profile key used for isolation and cleanup.
    pub(crate) project_key: String,
}

/// Creates or reuses one private managed home for a canonical trusted project.
pub(crate) fn prepare_bubblewrap_managed_home(
    config_root: &Path,
    project_root: &Path,
) -> Result<BubblewrapManagedHome, SandboxCompileError> {
    if !config_root.is_absolute() || !project_root.is_absolute() {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            "managed Bubblewrap homes require absolute configuration and project roots",
        ));
    }
    let project_key = bubblewrap_managed_home_project_key(project_root);
    let home = config_root
        .join("sandbox")
        .join("cache-homes")
        .join(&project_key)
        .join("home");
    for directory in [
        home.clone(),
        home.join(".cache"),
        home.join(".config"),
        home.join(".local"),
        home.join(".local/share"),
        home.join(".local/state"),
    ] {
        ensure_private_managed_directory(&directory)?;
    }
    Ok(BubblewrapManagedHome {
        host_path: home,
        project_key,
    })
}

/// Removes the managed home associated with a revoked canonical project.
pub(crate) fn remove_bubblewrap_managed_home(
    config_root: &Path,
    project_root: &Path,
) -> Result<bool, SandboxCompileError> {
    let project_key = bubblewrap_managed_home_project_key(project_root);
    let project_directory = config_root
        .join("sandbox")
        .join("cache-homes")
        .join(project_key);
    match fs::symlink_metadata(&project_directory) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                "managed Bubblewrap home cleanup target is not a private directory",
            ))
        }
        Ok(_) => {
            fs::remove_dir_all(project_directory).map_err(|error| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("managed Bubblewrap home cleanup failed: {error}"),
                )
            })?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("managed Bubblewrap home cleanup inspection failed: {error}"),
        )),
    }
}

/// Returns the stable project/profile storage key without disclosing the path.
pub(crate) fn bubblewrap_managed_home_project_key(project_root: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(b"mez-bubblewrap-managed-home-v1\0");
    digest.update(BUBBLEWRAP_RUNTIME_PROFILE_VERSION.as_bytes());
    digest.update(b"\0");
    digest.update(project_root.as_os_str().as_encoded_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Creates one directory with user-only permissions and rejects symlinks.
fn ensure_private_managed_directory(path: &Path) -> Result<(), SandboxCompileError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                "managed Bubblewrap home paths must be ordinary directories",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|error| {
                SandboxCompileError::new(
                    SandboxCompileErrorKind::InvalidInput,
                    format!("managed Bubblewrap home creation failed: {error}"),
                )
            })?;
        }
        Err(error) => {
            return Err(SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                format!("managed Bubblewrap home inspection failed: {error}"),
            ));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            SandboxCompileError::new(
                SandboxCompileErrorKind::InvalidInput,
                format!("managed Bubblewrap home permission update failed: {error}"),
            )
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies one project/profile identity reuses its private managed home
    /// while a different project receives a distinct storage key and path.
    #[test]
    fn managed_home_is_private_persistent_and_project_isolated() {
        let root = std::env::temp_dir().join(format!(
            "mez-managed-bubblewrap-home-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let config_root = root.join("config");
        let first_project = root.join("first-project");
        let second_project = root.join("second-project");
        fs::create_dir_all(&first_project).unwrap();
        fs::create_dir_all(&second_project).unwrap();

        let first = prepare_bubblewrap_managed_home(&config_root, &first_project).unwrap();
        fs::write(first.host_path.join(".cache/persisted"), "cache").unwrap();
        let reused = prepare_bubblewrap_managed_home(&config_root, &first_project).unwrap();
        let second = prepare_bubblewrap_managed_home(&config_root, &second_project).unwrap();

        assert_eq!(first, reused);
        assert_eq!(
            fs::read_to_string(reused.host_path.join(".cache/persisted")).unwrap(),
            "cache"
        );
        assert_ne!(first.project_key, second.project_key);
        assert_ne!(first.host_path, second.host_path);
        for relative in [".cache", ".config", ".local/share", ".local/state"] {
            assert!(first.host_path.join(relative).is_dir());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&first.host_path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    /// Verifies revocation cleanup removes only the matching project home and
    /// reports an already-absent home without affecting sibling projects.
    #[test]
    fn managed_home_cleanup_is_project_scoped_and_idempotent() {
        let root = std::env::temp_dir().join(format!(
            "mez-managed-bubblewrap-cleanup-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let config_root = root.join("config");
        let first_project = root.join("first-project");
        let second_project = root.join("second-project");
        fs::create_dir_all(&first_project).unwrap();
        fs::create_dir_all(&second_project).unwrap();
        let first = prepare_bubblewrap_managed_home(&config_root, &first_project).unwrap();
        let second = prepare_bubblewrap_managed_home(&config_root, &second_project).unwrap();

        assert!(remove_bubblewrap_managed_home(&config_root, &first_project).unwrap());
        assert!(!first.host_path.exists());
        assert!(second.host_path.exists());
        assert!(!remove_bubblewrap_managed_home(&config_root, &first_project).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
}
