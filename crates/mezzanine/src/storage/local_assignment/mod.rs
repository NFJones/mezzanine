//! Durable hosted-local session assignments.
//!
//! Assignments retain stable local session identity and checkpoint ownership
//! across host restarts. They deliberately exclude remote trust, endpoint, and
//! lease authority and remain separate from runtime-root live discovery.

use std::path::{Path, PathBuf};

use crate::error::{MezError, Result};

mod repository;
mod types;

pub(crate) use repository::LocalSessionAssignmentRepository;
pub(crate) use types::{
    LocalAssignmentCheckpoint, LocalAssignmentReservationRequest, LocalSessionAssignment,
    LocalSessionAssignmentState,
};

pub(crate) fn default_local_assignment_directory(config_root: impl AsRef<Path>) -> PathBuf {
    crate::storage::lease::default_host_state_directory(config_root).join("local-sessions")
}

fn validate_identifier(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty()
        || value.len() > 256
        || value.chars().any(char::is_control)
        || value.contains('/')
        || value.contains('\\')
        || matches!(value, "." | "..")
    {
        return Err(MezError::invalid_args(format!(
            "local session assignment {field} must be non-empty printable identifier text"
        )));
    }
    Ok(())
}

fn validate_optional_text(value: Option<&str>, field: &str, max_bytes: usize) -> Result<()> {
    if let Some(value) = value
        && (value.trim().is_empty()
            || value.len() > max_bytes
            || value.chars().any(char::is_control))
    {
        return Err(MezError::invalid_args(format!(
            "local session assignment {field} must be printable text up to {max_bytes} bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
