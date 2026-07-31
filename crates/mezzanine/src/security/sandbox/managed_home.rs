//! Managed persistent home directories for trusted-project Bubblewrap runs.
//!
//! Homes live below the private Mezzanine configuration root and are keyed by
//! canonical project identity plus the fixed sandbox profile. A shared activity
//! lock excludes maintenance for each mounted workload, while a separate
//! exclusive preparation lock serializes directory and metadata updates without
//! upgrading the activity lock. The helper never copies host-home content and
//! rejects symlinked storage components.

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{BUBBLEWRAP_RUNTIME_PROFILE_VERSION, SandboxCompileError, SandboxCompileErrorKind};

const MANAGED_HOME_METADATA_FILE: &str = "metadata.json";
const MANAGED_HOME_LOCK_FILE: &str = ".active.lock";
const MANAGED_HOME_PREPARATION_LOCK_FILE: &str = ".prepare.lock";
const MANAGED_HOME_PASSWD_FILE: &str = "passwd";
const MANAGED_HOME_GROUP_FILE: &str = "group";

/// Private lifecycle metadata retained beside one managed home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BubblewrapManagedHomeMetadata {
    /// Metadata contract version.
    pub(crate) version: u32,
    /// Bubblewrap runtime profile that owns this home.
    pub(crate) profile_version: String,
    /// Stable project/profile storage identity.
    pub(crate) project_key: String,
    /// Initial creation time in Unix seconds.
    pub(crate) created_at_unix_seconds: u64,
    /// Most recent preparation time in Unix seconds.
    pub(crate) last_used_at_unix_seconds: u64,
}

/// Side-effect-free inspection result for one managed home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct BubblewrapManagedHomeInspection {
    /// Stable project/profile storage identity.
    pub(crate) project_key: String,
    /// Whether the managed home exists.
    pub(crate) exists: bool,
    /// Total regular-file bytes without following symlinks.
    pub(crate) bytes: u64,
    /// Whether a live workload currently holds the shared activity lock.
    pub(crate) active: bool,
    /// Valid lifecycle metadata when present.
    pub(crate) metadata: Option<BubblewrapManagedHomeMetadata>,
}

/// Shared activity lock retained while one managed home is mounted.
#[derive(Debug)]
pub(crate) struct BubblewrapManagedHomeActivityLock {
    _file: fs::File,
}

/// Result of one scoped clear or prune candidate operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct BubblewrapManagedHomeMaintenance {
    /// Stable project/profile storage identity.
    pub(crate) project_key: String,
    /// Whether the scoped managed home existed when maintenance began.
    pub(crate) exists: bool,
    /// Measured regular-file bytes before deletion.
    pub(crate) bytes: u64,
    /// Whether an active workload caused the operation to be skipped.
    pub(crate) active: bool,
    /// Whether the home is an inactive deletion candidate.
    pub(crate) candidate: bool,
    /// Whether the managed home was removed.
    pub(crate) removed: bool,
}

/// Prepared host-side home projected at `/home/mez` inside Bubblewrap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BubblewrapManagedHome {
    /// Private host directory mounted read-write as the synthetic home.
    pub(crate) host_path: PathBuf,
    /// Private synthetic passwd record mounted read-only inside Bubblewrap.
    pub(crate) passwd_path: PathBuf,
    /// Private synthetic group record mounted read-only inside Bubblewrap.
    pub(crate) group_path: PathBuf,
    /// Stable non-secret project/profile key used for isolation and cleanup.
    pub(crate) project_key: String,
}

/// Creates or reuses one private managed home for a canonical trusted project.
pub(crate) fn prepare_bubblewrap_managed_home(
    config_root: &Path,
    project_root: &Path,
) -> Result<BubblewrapManagedHome, SandboxCompileError> {
    let (home, _activity) =
        prepare_bubblewrap_managed_home_for_workload(config_root, project_root)?;
    Ok(home)
}

/// Prepares one managed home and retains its shared lock for a workload.
pub(crate) fn prepare_bubblewrap_managed_home_for_workload(
    config_root: &Path,
    project_root: &Path,
) -> Result<(BubblewrapManagedHome, BubblewrapManagedHomeActivityLock), SandboxCompileError> {
    if !config_root.is_absolute() || !project_root.is_absolute() {
        return Err(SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            "managed Bubblewrap homes require absolute configuration and project roots",
        ));
    }
    let project_key = bubblewrap_managed_home_project_key(project_root);
    ensure_managed_homes_root(config_root)?;
    let project_directory = managed_home_project_directory(config_root, &project_key);
    ensure_private_managed_directory(&project_directory)?;
    let activity = lock_bubblewrap_managed_home(config_root, &project_key)?;
    let preparation = open_managed_home_preparation_lock(&project_directory)?;
    flock(&preparation, FlockOperation::LockExclusive).map_err(|error| {
        managed_home_error(format!(
            "managed Bubblewrap home preparation lock failed: {error}"
        ))
    })?;
    let home = project_directory.join("home");
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
    let passwd_path = project_directory.join(MANAGED_HOME_PASSWD_FILE);
    let group_path = project_directory.join(MANAGED_HOME_GROUP_FILE);
    write_private_managed_file(
        &passwd_path,
        b"mez:x:1000:1000:Mezzanine sandbox user:/home/mez:/bin/sh\n",
    )?;
    write_private_managed_file(&group_path, b"mez:x:1000:\n")?;
    write_managed_home_metadata(&project_directory, &project_key)?;
    Ok((
        BubblewrapManagedHome {
            host_path: home,
            passwd_path,
            group_path,
            project_key,
        },
        activity,
    ))
}

/// Removes the managed home associated with a revoked canonical project.
pub(crate) fn remove_bubblewrap_managed_home(
    config_root: &Path,
    project_root: &Path,
) -> Result<bool, SandboxCompileError> {
    let project_key = bubblewrap_managed_home_project_key(project_root);
    Ok(maintain_managed_home(config_root, &project_key, false)?.removed)
}

/// Acquires the shared activity lock held for one mounted managed home.
pub(crate) fn lock_bubblewrap_managed_home(
    config_root: &Path,
    project_key: &str,
) -> Result<BubblewrapManagedHomeActivityLock, SandboxCompileError> {
    let project_directory = managed_home_project_directory(config_root, project_key);
    loop {
        let file = open_managed_home_lock(&project_directory)?;
        flock(&file, FlockOperation::LockShared).map_err(|error| {
            managed_home_error(format!(
                "managed Bubblewrap home activity lock failed: {error}"
            ))
        })?;
        if managed_home_lock_is_current(&project_directory, &file)? {
            return Ok(BubblewrapManagedHomeActivityLock { _file: file });
        }
    }
}

/// Inspects one project-scoped managed home without creating any state.
pub(crate) fn inspect_bubblewrap_managed_home(
    config_root: &Path,
    project_root: &Path,
) -> Result<BubblewrapManagedHomeInspection, SandboxCompileError> {
    inspect_managed_home_key(
        config_root,
        &bubblewrap_managed_home_project_key(project_root),
    )
}

/// Lists every valid managed home without creating or modifying storage.
pub(crate) fn list_bubblewrap_managed_homes(
    config_root: &Path,
) -> Result<Vec<BubblewrapManagedHomeInspection>, SandboxCompileError> {
    let Some(root) = validate_existing_managed_homes_root(config_root)? else {
        return Ok(Vec::new());
    };
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) => {
            return Err(managed_home_error(format!(
                "managed home listing failed: {error}"
            )));
        }
    };
    let mut inspections = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|error| managed_home_error(format!("managed home listing failed: {error}")))?;
        let key = entry
            .file_name()
            .into_string()
            .map_err(|_| managed_home_error("managed home key is not UTF-8"))?;
        inspections.push(inspect_managed_home_key(config_root, &key)?);
    }
    inspections.sort_by(|left, right| left.project_key.cmp(&right.project_key));
    Ok(inspections)
}

/// Clears one project-scoped managed home or reports a dry-run candidate.
pub(crate) fn clear_bubblewrap_managed_home(
    config_root: &Path,
    project_root: &Path,
    dry_run: bool,
) -> Result<BubblewrapManagedHomeMaintenance, SandboxCompileError> {
    maintain_managed_home(
        config_root,
        &bubblewrap_managed_home_project_key(project_root),
        dry_run,
    )
}

/// Clears every inactive managed home or reports dry-run candidates.
pub(crate) fn prune_bubblewrap_managed_homes(
    config_root: &Path,
    dry_run: bool,
) -> Result<Vec<BubblewrapManagedHomeMaintenance>, SandboxCompileError> {
    list_bubblewrap_managed_homes(config_root)?
        .into_iter()
        .map(|inspection| maintain_managed_home(config_root, &inspection.project_key, dry_run))
        .collect()
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

fn managed_homes_root(config_root: &Path) -> PathBuf {
    config_root.join("sandbox").join("cache-homes")
}

fn ensure_managed_homes_root(config_root: &Path) -> Result<(), SandboxCompileError> {
    let sandbox_root = config_root.join("sandbox");
    ensure_private_managed_directory(&sandbox_root)?;
    ensure_private_managed_directory(&sandbox_root.join("cache-homes"))
}

fn validate_existing_managed_homes_root(
    config_root: &Path,
) -> Result<Option<PathBuf>, SandboxCompileError> {
    let sandbox_root = config_root.join("sandbox");
    for path in [&sandbox_root, &sandbox_root.join("cache-homes")] {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(managed_home_error(
                    "managed Bubblewrap home storage roots must be ordinary directories",
                ));
            }
            Ok(metadata) if metadata.permissions().mode() & 0o077 != 0 => {
                return Err(managed_home_error(
                    "managed Bubblewrap home storage roots must be user-private",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(managed_home_error(format!(
                    "managed home storage inspection failed: {error}"
                )));
            }
        }
    }
    Ok(Some(managed_homes_root(config_root)))
}

fn managed_home_project_directory(config_root: &Path, project_key: &str) -> PathBuf {
    managed_homes_root(config_root).join(project_key)
}

fn managed_home_error(message: impl Into<String>) -> SandboxCompileError {
    SandboxCompileError::new(SandboxCompileErrorKind::InvalidInput, message)
}

/// Replaces one private regular file without accepting a symlink target.
fn write_private_managed_file(path: &Path, contents: &[u8]) -> Result<(), SandboxCompileError> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(managed_home_error(
            "managed Bubblewrap identity record must be a regular file",
        ));
    }
    fs::write(path, contents).map_err(|error| {
        managed_home_error(format!(
            "managed Bubblewrap identity record write failed: {error}"
        ))
    })?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
        managed_home_error(format!(
            "managed Bubblewrap identity record permission update failed: {error}"
        ))
    })
}

fn validate_project_key(project_key: &str) -> Result<(), SandboxCompileError> {
    if project_key.len() != 64
        || !project_key
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(managed_home_error(
            "managed Bubblewrap home key must be a lowercase SHA-256 identity",
        ));
    }
    Ok(())
}

fn unix_now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn write_managed_home_metadata(
    project_directory: &Path,
    project_key: &str,
) -> Result<(), SandboxCompileError> {
    validate_project_key(project_key)?;
    ensure_private_managed_directory(project_directory)?;
    let path = project_directory.join(MANAGED_HOME_METADATA_FILE);
    let now = unix_now_seconds();
    let created_at_unix_seconds = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(managed_home_error(
                "managed home metadata must be an ordinary file",
            ));
        }
        Ok(metadata) if metadata.permissions().mode() & 0o077 != 0 => {
            return Err(managed_home_error(
                "managed home metadata must be user-private",
            ));
        }
        Ok(_) => {
            let text = fs::read_to_string(&path).map_err(|error| {
                managed_home_error(format!("managed home metadata read failed: {error}"))
            })?;
            let metadata =
                serde_json::from_str::<BubblewrapManagedHomeMetadata>(&text).map_err(|error| {
                    managed_home_error(format!("managed home metadata is invalid: {error}"))
                })?;
            if metadata.version != 1
                || metadata.profile_version != BUBBLEWRAP_RUNTIME_PROFILE_VERSION
                || metadata.project_key != project_key
            {
                return Err(managed_home_error(
                    "managed home metadata does not match its storage identity",
                ));
            }
            metadata.created_at_unix_seconds
        }
        Err(error) if error.kind() == ErrorKind::NotFound => now,
        Err(error) => {
            return Err(managed_home_error(format!(
                "managed home metadata read failed: {error}"
            )));
        }
    };
    let metadata = BubblewrapManagedHomeMetadata {
        version: 1,
        profile_version: BUBBLEWRAP_RUNTIME_PROFILE_VERSION.to_string(),
        project_key: project_key.to_string(),
        created_at_unix_seconds,
        last_used_at_unix_seconds: now,
    };
    let mut rendered = serde_json::to_string_pretty(&metadata)
        .map_err(|error| managed_home_error(format!("managed home metadata failed: {error}")))?;
    rendered.push('\n');
    let temporary = project_directory.join(format!(
        ".{MANAGED_HOME_METADATA_FILE}.{}.tmp",
        std::process::id()
    ));
    fs::write(&temporary, rendered).map_err(|error| {
        managed_home_error(format!("managed home metadata write failed: {error}"))
    })?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).map_err(|error| {
        managed_home_error(format!(
            "managed home metadata permission update failed: {error}"
        ))
    })?;
    fs::rename(&temporary, &path).map_err(|error| {
        managed_home_error(format!("managed home metadata replace failed: {error}"))
    })?;
    Ok(())
}

fn open_managed_home_lock(project_directory: &Path) -> Result<fs::File, SandboxCompileError> {
    open_managed_home_lock_file(project_directory, MANAGED_HOME_LOCK_FILE)
}

/// Opens the mutex used only for short-lived directory and metadata updates.
fn open_managed_home_preparation_lock(
    project_directory: &Path,
) -> Result<fs::File, SandboxCompileError> {
    open_managed_home_lock_file(project_directory, MANAGED_HOME_PREPARATION_LOCK_FILE)
}

/// Opens or creates one private lock file beneath a validated project directory.
fn open_managed_home_lock_file(
    project_directory: &Path,
    file_name: &str,
) -> Result<fs::File, SandboxCompileError> {
    ensure_private_managed_directory(project_directory)?;
    let lock_path = project_directory.join(file_name);
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| managed_home_error(format!("managed home lock open failed: {error}")))?;
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600)).map_err(|error| {
        managed_home_error(format!(
            "managed home lock permission update failed: {error}"
        ))
    })?;
    Ok(file)
}

/// Confirms a locked descriptor still names the lock in the current directory.
///
/// Maintenance may unlink the project directory after a caller opens the lock
/// but before its shared acquisition completes. Such a descriptor protects the
/// removed inode rather than a recreated home, so callers must retry it.
fn managed_home_lock_is_current(
    project_directory: &Path,
    file: &fs::File,
) -> Result<bool, SandboxCompileError> {
    let open_metadata = file.metadata().map_err(|error| {
        managed_home_error(format!("managed home lock inspection failed: {error}"))
    })?;
    let lock_path = project_directory.join(MANAGED_HOME_LOCK_FILE);
    let current_metadata = match fs::metadata(&lock_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(managed_home_error(format!(
                "managed home lock inspection failed: {error}"
            )));
        }
    };
    Ok(open_metadata.dev() == current_metadata.dev()
        && open_metadata.ino() == current_metadata.ino())
}

fn inspect_managed_home_key(
    config_root: &Path,
    project_key: &str,
) -> Result<BubblewrapManagedHomeInspection, SandboxCompileError> {
    validate_project_key(project_key)?;
    if validate_existing_managed_homes_root(config_root)?.is_none() {
        return Ok(BubblewrapManagedHomeInspection {
            project_key: project_key.to_string(),
            exists: false,
            bytes: 0,
            active: false,
            metadata: None,
        });
    }
    let project_directory = managed_home_project_directory(config_root, project_key);
    let metadata = match fs::symlink_metadata(&project_directory) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(managed_home_error(
                "managed Bubblewrap home inspection target is not a private directory",
            ));
        }
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(BubblewrapManagedHomeInspection {
                project_key: project_key.to_string(),
                exists: false,
                bytes: 0,
                active: false,
                metadata: None,
            });
        }
        Err(error) => {
            return Err(managed_home_error(format!(
                "managed home inspection failed: {error}"
            )));
        }
    };
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(managed_home_error(
            "managed Bubblewrap home directory must be user-private",
        ));
    }
    let bytes = measure_managed_tree(&project_directory)?;
    let metadata_path = project_directory.join(MANAGED_HOME_METADATA_FILE);
    let lifecycle = match fs::read_to_string(&metadata_path) {
        Ok(text) => {
            let metadata =
                serde_json::from_str::<BubblewrapManagedHomeMetadata>(&text).map_err(|error| {
                    managed_home_error(format!("managed home metadata is invalid: {error}"))
                })?;
            if metadata.version != 1
                || metadata.profile_version != BUBBLEWRAP_RUNTIME_PROFILE_VERSION
                || metadata.project_key != project_key
            {
                return Err(managed_home_error(
                    "managed home metadata does not match its storage identity",
                ));
            }
            Some(metadata)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => {
            return Err(managed_home_error(format!(
                "managed home metadata inspection failed: {error}"
            )));
        }
    };
    let active = managed_home_is_active(&project_directory)?;
    Ok(BubblewrapManagedHomeInspection {
        project_key: project_key.to_string(),
        exists: true,
        bytes,
        active,
        metadata: lifecycle,
    })
}

fn managed_home_is_active(project_directory: &Path) -> Result<bool, SandboxCompileError> {
    let lock_path = project_directory.join(MANAGED_HOME_LOCK_FILE);
    let metadata = match fs::symlink_metadata(&lock_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(managed_home_error(
                "managed home lock must be an ordinary file",
            ));
        }
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(managed_home_error(format!(
                "managed home lock inspection failed: {error}"
            )));
        }
    };
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(managed_home_error("managed home lock must be user-private"));
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| managed_home_error(format!("managed home lock open failed: {error}")))?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(false),
        Err(error) if error == rustix::io::Errno::WOULDBLOCK => Ok(true),
        Err(error) => Err(managed_home_error(format!(
            "managed home lock inspection failed: {error}"
        ))),
    }
}

fn measure_managed_tree(path: &Path) -> Result<u64, SandboxCompileError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| managed_home_error(format!("managed home measurement failed: {error}")))?;
    if metadata.file_type().is_symlink() {
        return Err(managed_home_error(
            "managed home measurement refuses symbolic links",
        ));
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Err(managed_home_error(
            "managed home contains an unsupported filesystem entry",
        ));
    }
    let mut bytes = 0_u64;
    for entry in fs::read_dir(path)
        .map_err(|error| managed_home_error(format!("managed home measurement failed: {error}")))?
    {
        let entry = entry.map_err(|error| {
            managed_home_error(format!("managed home measurement failed: {error}"))
        })?;
        bytes = bytes
            .checked_add(measure_managed_tree(&entry.path())?)
            .ok_or_else(|| managed_home_error("managed home byte count overflowed"))?;
    }
    Ok(bytes)
}

fn maintain_managed_home(
    config_root: &Path,
    project_key: &str,
    dry_run: bool,
) -> Result<BubblewrapManagedHomeMaintenance, SandboxCompileError> {
    let inspection = inspect_managed_home_key(config_root, project_key)?;
    if !inspection.exists || inspection.active {
        return Ok(BubblewrapManagedHomeMaintenance {
            project_key: project_key.to_string(),
            exists: inspection.exists,
            bytes: inspection.bytes,
            active: inspection.active,
            candidate: inspection.exists && !inspection.active,
            removed: false,
        });
    }
    let project_directory = managed_home_project_directory(config_root, project_key);
    let lock = open_managed_home_lock(&project_directory)?;
    match flock(&lock, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(error) if error == rustix::io::Errno::WOULDBLOCK => {
            return Ok(BubblewrapManagedHomeMaintenance {
                project_key: project_key.to_string(),
                exists: true,
                bytes: inspection.bytes,
                active: true,
                candidate: false,
                removed: false,
            });
        }
        Err(error) => {
            return Err(managed_home_error(format!(
                "managed home maintenance lock failed: {error}"
            )));
        }
    }
    if !dry_run {
        remove_managed_tree(&project_directory)?;
    }
    Ok(BubblewrapManagedHomeMaintenance {
        project_key: project_key.to_string(),
        exists: true,
        bytes: inspection.bytes,
        active: false,
        candidate: true,
        removed: !dry_run,
    })
}

fn remove_managed_tree(path: &Path) -> Result<(), SandboxCompileError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| managed_home_error(format!("managed home cleanup failed: {error}")))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(managed_home_error(
            "managed home cleanup refuses non-directory or symbolic-link roots",
        ));
    }
    for entry in fs::read_dir(path)
        .map_err(|error| managed_home_error(format!("managed home cleanup failed: {error}")))?
    {
        let entry = entry
            .map_err(|error| managed_home_error(format!("managed home cleanup failed: {error}")))?;
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path)
            .map_err(|error| managed_home_error(format!("managed home cleanup failed: {error}")))?;
        if metadata.file_type().is_symlink() {
            return Err(managed_home_error(
                "managed home cleanup refuses symbolic links",
            ));
        }
        if metadata.is_dir() {
            remove_managed_tree(&entry_path)?;
        } else if metadata.is_file() {
            fs::remove_file(&entry_path).map_err(|error| {
                managed_home_error(format!("managed home cleanup failed: {error}"))
            })?;
        } else {
            return Err(managed_home_error(
                "managed home cleanup found an unsupported filesystem entry",
            ));
        }
    }
    fs::remove_dir(path)
        .map_err(|error| managed_home_error(format!("managed home cleanup failed: {error}")))
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
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        SandboxCompileError::new(
            SandboxCompileErrorKind::InvalidInput,
            format!("managed Bubblewrap home permission update failed: {error}"),
        )
    })?;
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
        assert_eq!(
            fs::read_to_string(&first.passwd_path).unwrap(),
            "mez:x:1000:1000:Mezzanine sandbox user:/home/mez:/bin/sh\n"
        );
        assert_eq!(
            fs::read_to_string(&first.group_path).unwrap(),
            "mez:x:1000:\n"
        );
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

    /// Verifies preparing an overlapping workload for one project completes
    /// while the first workload retains its shared activity lock. This guards
    /// against upgrading a separately opened activity lock and self-deadlocking
    /// the single-threaded server runtime.
    #[test]
    fn managed_home_preparation_allows_overlapping_workloads() {
        let root = std::env::temp_dir().join(format!(
            "mez-managed-bubblewrap-overlap-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let config_root = root.join("config");
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();

        let (first, first_activity) =
            prepare_bubblewrap_managed_home_for_workload(&config_root, &project).unwrap();
        let second_config_root = config_root.clone();
        let second_project = project.clone();
        let (completed_tx, completed_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result =
                prepare_bubblewrap_managed_home_for_workload(&second_config_root, &second_project);
            completed_tx.send(()).unwrap();
            result
        });

        completed_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("overlapping managed-home preparation must not wait for the first workload");
        let (second, second_activity) = worker.join().unwrap().unwrap();
        assert_eq!(second, first);

        drop(second_activity);
        drop(first_activity);
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

    /// Verifies inspection reports lifecycle metadata and bytes without
    /// mutation, dry runs retain storage, and active homes are skipped.
    #[test]
    fn managed_home_inspection_and_maintenance_respect_activity_lock() {
        let root = std::env::temp_dir().join(format!(
            "mez-managed-bubblewrap-maintenance-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let config_root = root.join("config");
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        let managed = prepare_bubblewrap_managed_home(&config_root, &project).unwrap();
        fs::write(managed.host_path.join(".cache/payload"), b"payload").unwrap();

        let inspection = inspect_bubblewrap_managed_home(&config_root, &project).unwrap();
        assert!(inspection.exists);
        assert!(!inspection.active);
        assert!(inspection.bytes >= 7);
        let metadata = inspection.metadata.unwrap();
        assert_eq!(metadata.version, 1);
        assert_eq!(metadata.project_key, managed.project_key);
        assert_eq!(metadata.profile_version, BUBBLEWRAP_RUNTIME_PROFILE_VERSION);

        let preview = clear_bubblewrap_managed_home(&config_root, &project, true).unwrap();
        assert!(!preview.removed);
        assert!(managed.host_path.exists());

        let activity = lock_bubblewrap_managed_home(&config_root, &managed.project_key).unwrap();
        let active = clear_bubblewrap_managed_home(&config_root, &project, false).unwrap();
        assert!(active.active);
        assert!(!active.removed);
        drop(activity);

        let removed = clear_bubblewrap_managed_home(&config_root, &project, false).unwrap();
        assert!(removed.removed);
        assert!(!managed.host_path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    /// Verifies inspection and cleanup reject a symbolic-link descendant
    /// without reading or deleting the external target it references.
    #[test]
    fn managed_home_maintenance_rejects_symbolic_link_descendants() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "mez-managed-bubblewrap-symlink-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let config_root = root.join("config");
        let project = root.join("project");
        let external = root.join("external-secret");
        fs::create_dir_all(&project).unwrap();
        fs::write(&external, b"retained").unwrap();
        let managed = prepare_bubblewrap_managed_home(&config_root, &project).unwrap();
        symlink(&external, managed.host_path.join("escaped-link")).unwrap();

        let inspect_error = inspect_bubblewrap_managed_home(&config_root, &project).unwrap_err();
        assert!(inspect_error.message().contains("refuses symbolic links"));
        let clear_error = clear_bubblewrap_managed_home(&config_root, &project, false).unwrap_err();
        assert!(clear_error.message().contains("refuses symbolic links"));
        assert_eq!(fs::read(&external).unwrap(), b"retained");
        assert!(managed.host_path.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
