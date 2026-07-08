//! SQLite-backed project issue tracking.
//!
//! The issue subsystem owns Mezzanine-local defect and task records. It keeps
//! validation, project-key normalization, SQLite persistence, and result
//! formatting independent from CLI, slash-command, and MAAP action surfaces so
//! every user entry point shares the same behavior.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{MezError, Result};

mod store;
mod types;

pub use store::IssueStore;
pub use types::{
    DeleteIssueResult, IssueBrowserQuery, IssueKind, IssueQuery, IssueRecord, IssueState,
    IssueUpdate, NewIssueRecord, UpdateIssueResult, validate_issue_dependency_ids,
};

/// Default maximum issue records returned by one query.
pub const DEFAULT_ISSUE_QUERY_LIMIT: usize = 50;
/// Hard upper bound for one issue query result set.
pub const MAX_ISSUE_QUERY_LIMIT: usize = 200;

/// Returns the canonical SQLite database path under a Mezzanine config root.
pub fn default_issue_database_path(config_root: impl AsRef<Path>) -> PathBuf {
    config_root.as_ref().join("issues.sqlite")
}

/// Resolved issue database location and whether Mezzanine owns its parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueDatabasePath {
    path: PathBuf,
    manage_private_parent: bool,
}

impl IssueDatabasePath {
    /// Builds a resolved issue database path with its parent-management policy.
    fn new(path: PathBuf, manage_private_parent: bool) -> Self {
        Self {
            path,
            manage_private_parent,
        }
    }

    /// Returns the SQLite database path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns whether Mezzanine should create/chmod the database parent.
    pub fn manages_private_parent(&self) -> bool {
        self.manage_private_parent
    }

    /// Consumes this value and returns the SQLite database path.
    pub fn into_path(self) -> PathBuf {
        self.path
    }
}

/// Resolves an optional configured database path and parent ownership policy.
///
/// Empty and relative paths are stored below the Mezzanine config root, so the
/// issue store owns and privatizes their parent directories. Absolute
/// configured paths are caller-owned locations; Mezzanine opens the database
/// file there but does not create or chmod the surrounding directory.
pub fn issue_database_location(
    config_root: impl AsRef<Path>,
    configured: Option<&str>,
) -> IssueDatabasePath {
    let Some(configured) = configured.map(str::trim).filter(|value| !value.is_empty()) else {
        return IssueDatabasePath::new(default_issue_database_path(config_root), true);
    };
    let path = PathBuf::from(configured);
    if path.is_absolute() {
        IssueDatabasePath::new(path, false)
    } else {
        IssueDatabasePath::new(config_root.as_ref().join(path), true)
    }
}

/// Resolves an optional configured database path against the config root.
///
/// Empty paths use the standard `issues.sqlite` sibling under the config root.
/// Relative configured paths are resolved under the same root so local state
/// stays inside Mezzanine's private configuration directory by default.
pub fn issue_database_path(config_root: impl AsRef<Path>, configured: Option<&str>) -> PathBuf {
    issue_database_location(config_root, configured).into_path()
}

/// Returns a stable project key for a current working directory.
///
/// Git repositories are keyed by their repository root. Non-git directories are
/// keyed by the working directory itself. Callers that already have a user
/// supplied project string should pass it through `validate_project_key` instead
/// of using filesystem discovery.
pub fn project_key_for_working_directory(working_directory: impl AsRef<Path>) -> String {
    crate::project::discover_project_root(working_directory.as_ref())
        .to_string_lossy()
        .into_owned()
}

/// Validates a user or runtime resolved project key.
pub fn validate_project_key(project: &str) -> Result<()> {
    validate_non_empty_single_line("issue project", project)
}

/// Validates an issue title.
pub fn validate_issue_title(title: &str) -> Result<()> {
    validate_non_empty_single_line("issue title", title)
}

/// Validates optional issue body text.
pub fn validate_issue_body(body: Option<&str>) -> Result<()> {
    if let Some(body) = body
        && body.bytes().any(|byte| byte == 0)
    {
        return Err(MezError::invalid_args(
            "issue body must not contain NUL bytes",
        ));
    }
    Ok(())
}

/// Validates optional mutable issue notes text.
pub fn validate_issue_notes(notes: Option<&str>) -> Result<()> {
    if let Some(notes) = notes
        && notes.bytes().any(|byte| byte == 0)
    {
        return Err(MezError::invalid_args(
            "issue notes must not contain NUL bytes",
        ));
    }
    Ok(())
}

/// Creates a best-effort globally unique issue id.
pub fn generate_issue_id() -> String {
    let mut bytes = [0u8; 16];
    let mut rng = rand::rng();
    use rand::Rng;
    rng.fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15],
    )
}

/// Ensures the parent directory for a private issue database exists.
pub(crate) fn ensure_private_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        set_private_dir_permissions(parent)?;
    }
    Ok(())
}

/// Applies private file permissions to an issue database.
pub(crate) fn set_private_issue_file_permissions(path: &Path) -> Result<()> {
    set_private_file_permissions(path)
}

fn set_private_dir_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn set_private_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn validate_non_empty_single_line(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(MezError::invalid_args(format!("{label} must not be empty")));
    }
    if value
        .bytes()
        .any(|byte| byte == 0 || byte == b'\n' || byte == b'\r')
    {
        return Err(MezError::invalid_args(format!(
            "{label} must be a single line without NUL bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
