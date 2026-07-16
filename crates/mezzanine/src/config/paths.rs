//! Config Paths implementation.
//!
//! This module owns the config paths boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    ConfigDiagnostic, DEFAULT_CONFIG_TOML, MezError, OpenOptions, PRIMARY_CONFIG_FILENAMES, Path,
    PathBuf, Result, Write, env, fs,
};
use std::io::ErrorKind;
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(test)]
use tokio::io::AsyncWriteExt;

// Config path discovery and private file/directory helpers.

/// Carries Config Paths state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub struct ConfigPaths {
    /// Stores the root value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) root: PathBuf,
}

impl ConfigPaths {
    /// Runs the from process env operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_process_env() -> Result<Self> {
        let home = env::var_os("HOME").ok_or_else(|| {
            MezError::config("HOME is not set; cannot locate ~/.config/mezzanine")
        })?;
        Ok(Self::from_home(PathBuf::from(home)))
    }

    /// Runs the from home operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_home(home: PathBuf) -> Self {
        Self {
            root: home.join(".config").join("mezzanine"),
        }
    }

    /// Runs the from root operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_root(root: PathBuf) -> Self {
        Self { root }
    }

    /// Runs the root operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Runs the default primary file operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn default_primary_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// Runs the supported primary files operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn supported_primary_files(&self) -> Vec<PathBuf> {
        PRIMARY_CONFIG_FILENAMES
            .iter()
            .map(|file_name| self.root.join(file_name))
            .collect()
    }

    /// Runs the select primary file operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn select_primary_file(&self) -> Result<Option<PathBuf>> {
        let existing = self
            .supported_primary_files()
            .into_iter()
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();

        match existing.len() {
            0 => Ok(None),
            1 => Ok(existing.into_iter().next()),
            _ => Err(MezError::config(format!(
                "multiple primary config files found in {}",
                self.root.display()
            ))),
        }
    }

    /// Runs the select primary file async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub async fn select_primary_file_async(&self) -> Result<Option<PathBuf>> {
        let mut existing = Vec::new();
        for path in self.supported_primary_files() {
            if tokio::fs::metadata(&path)
                .await
                .map(|metadata| metadata.is_file())
                .unwrap_or(false)
            {
                existing.push(path);
            }
        }

        match existing.len() {
            0 => Ok(None),
            1 => Ok(existing.into_iter().next()),
            _ => Err(MezError::config(format!(
                "multiple primary config files found in {}",
                self.root.display()
            ))),
        }
    }

    /// Runs the ensure default config operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn ensure_default_config(&self) -> Result<PathBuf> {
        ensure_private_dir(&self.root)?;

        if let Some(path) = self.select_primary_file()? {
            return Ok(path);
        }

        let path = self.default_primary_file();
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                return self.select_primary_file()?.ok_or_else(|| {
                    MezError::config("default config was created concurrently but is unavailable")
                });
            }
            Err(error) => return Err(error.into()),
        };
        file.write_all(DEFAULT_CONFIG_TOML.as_bytes())?;
        set_private_file_permissions(&path)?;
        Ok(path)
    }

    /// Runs the ensure default config async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub async fn ensure_default_config_async(&self) -> Result<PathBuf> {
        ensure_private_dir_async(&self.root).await?;

        if let Some(path) = self.select_primary_file_async().await? {
            return Ok(path);
        }

        let path = self.default_primary_file();
        let mut file = match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                return self.select_primary_file_async().await?.ok_or_else(|| {
                    MezError::config("default config was created concurrently but is unavailable")
                });
            }
            Err(error) => return Err(error.into()),
        };
        file.write_all(DEFAULT_CONFIG_TOML.as_bytes()).await?;
        set_private_file_permissions_async(&path).await?;
        Ok(path)
    }
}

/// Runs the ensure private dir operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn ensure_private_dir(path: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err(MezError::config(format!(
                "config directory {} must not be a symlink",
                path.display()
            )));
        }
        if !metadata.is_dir() {
            return Err(MezError::config(format!(
                "config path {} exists but is not a directory",
                path.display()
            )));
        }
    } else {
        fs::create_dir_all(path)?;
    }

    set_private_dir_permissions(path)?;
    Ok(())
}

/// Runs the ensure private dir async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(super) async fn ensure_private_dir_async(path: &Path) -> Result<()> {
    if let Ok(metadata) = tokio::fs::symlink_metadata(path).await {
        if metadata.file_type().is_symlink() {
            return Err(MezError::config(format!(
                "config directory {} must not be a symlink",
                path.display()
            )));
        }
        if !metadata.is_dir() {
            return Err(MezError::config(format!(
                "config path {} exists but is not a directory",
                path.display()
            )));
        }
    } else {
        tokio::fs::create_dir_all(path).await?;
    }

    set_private_dir_permissions_async(path).await?;
    Ok(())
}

/// Runs the set private dir permissions operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn set_private_dir_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::Permissions::from_mode(0o700);
        fs::set_permissions(path, permissions)?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

/// Runs the set private dir permissions async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(super) async fn set_private_dir_permissions_async(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(0o700);
        tokio::fs::set_permissions(path, permissions).await?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

/// Runs the set private file permissions operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn set_private_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, permissions)?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

/// Runs the set private file permissions async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(super) async fn set_private_file_permissions_async(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(0o600);
        tokio::fs::set_permissions(path, permissions).await?;
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
}

/// Runs the write private config file operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn write_private_config_file(path: &Path, text: &str) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        return Err(MezError::config(format!(
            "config file {} must not be a symlink",
            path.display()
        )));
    }

    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }

    write_private_config_file_atomic(path, text)
}

/// Atomically replaces a private config file with validated text.
///
/// # Parameters
/// - `path`: Destination config file.
/// - `text`: Complete file contents to persist.
fn write_private_config_file_atomic(path: &Path, text: &str) -> Result<()> {
    let temp_path = private_config_temp_path(path);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)?;
    let write_result = (|| -> Result<()> {
        file.write_all(text.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        set_private_file_permissions(&temp_path)?;
        fs::rename(&temp_path, path)?;
        set_private_file_permissions(path)?;
        sync_parent_directory(path);
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    write_result
}

/// Runs the write private config file async operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub(super) async fn write_private_config_file_async(path: &Path, text: &str) -> Result<()> {
    if let Ok(metadata) = tokio::fs::symlink_metadata(path).await
        && metadata.file_type().is_symlink()
    {
        return Err(MezError::config(format!(
            "config file {} must not be a symlink",
            path.display()
        )));
    }

    if let Some(parent) = path.parent() {
        ensure_private_dir_async(parent).await?;
    }

    write_private_config_file_atomic_async(path, text).await
}

/// Atomically replaces a private config file with validated text asynchronously.
///
/// # Parameters
/// - `path`: Destination config file.
/// - `text`: Complete file contents to persist.
#[cfg(test)]
async fn write_private_config_file_atomic_async(path: &Path, text: &str) -> Result<()> {
    let temp_path = private_config_temp_path(path);
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .await?;
    let write_result = async {
        file.write_all(text.as_bytes()).await?;
        file.flush().await?;
        file.sync_all().await?;
        set_private_file_permissions_async(&temp_path).await?;
        tokio::fs::rename(&temp_path, path).await?;
        set_private_file_permissions_async(path).await?;
        sync_parent_directory_async(path).await;
        Ok(())
    }
    .await;
    if write_result.is_err() {
        let _ = tokio::fs::remove_file(&temp_path).await;
    }
    write_result
}

/// Builds a sibling temporary path used for atomic config replacement.
///
/// # Parameters
/// - `path`: Destination config file.
fn private_config_temp_path(path: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    path.with_file_name(format!(
        ".{file_name}.mez-tmp-{}-{nonce}",
        std::process::id()
    ))
}

/// Best-effort directory fsync for atomic rename durability.
///
/// # Parameters
/// - `path`: File whose parent directory should be synchronized.
fn sync_parent_directory(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(directory) = fs::File::open(parent)
    {
        let _ = directory.sync_all();
    }
}

/// Best-effort async directory fsync for atomic rename durability.
///
/// # Parameters
/// - `path`: File whose parent directory should be synchronized.
#[cfg(test)]
async fn sync_parent_directory_async(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(directory) = tokio::fs::File::open(parent).await
    {
        let _ = directory.sync_all().await;
    }
}

/// Runs the format diagnostics operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn format_diagnostics(diagnostics: &[ConfigDiagnostic]) -> String {
    diagnostics
        .iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.path, diagnostic.message))
        .collect::<Vec<_>>()
        .join("; ")
}
