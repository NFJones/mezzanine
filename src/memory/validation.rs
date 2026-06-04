//! Validation and permission helpers for memory records.
//!
//! The validation layer centralizes sensitive-content checks, scope ownership,
//! non-empty constraints, and private filesystem permissions.

use super::{MemoryScope, MezError, Path, Result, fs};

/// Runs the validate scope operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_scope(scope: &MemoryScope) -> Result<()> {
    match scope {
        MemoryScope::Global => Ok(()),
        MemoryScope::Project { root } => validate_non_empty("project memory root", root),
        MemoryScope::Session { session_id } => validate_non_empty("session memory id", session_id),
        MemoryScope::Window {
            session_id,
            window_id,
        } => {
            validate_non_empty("window memory session id", session_id)?;
            validate_non_empty("window memory id", window_id)
        }
        MemoryScope::Pane {
            session_id,
            pane_id,
        } => {
            validate_non_empty("pane memory session id", session_id)?;
            validate_non_empty("pane memory id", pane_id)
        }
        MemoryScope::Agent {
            session_id,
            agent_id,
        } => {
            validate_non_empty("agent memory session id", session_id)?;
            validate_non_empty("agent memory id", agent_id)
        }
    }
}

/// Runs the scope belongs to session operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn scope_belongs_to_session(scope: &MemoryScope, session_id: &str) -> bool {
    match scope {
        MemoryScope::Session { session_id: id }
        | MemoryScope::Window { session_id: id, .. }
        | MemoryScope::Pane { session_id: id, .. }
        | MemoryScope::Agent { session_id: id, .. } => id == session_id,
        MemoryScope::Global | MemoryScope::Project { .. } => false,
    }
}

/// Runs the validate non empty operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_non_empty(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        Err(MezError::invalid_args(format!("{label} must not be empty")))
    } else {
        Ok(())
    }
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
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
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
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
