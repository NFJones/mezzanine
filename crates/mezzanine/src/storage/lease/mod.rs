//! Durable remote-session lease persistence.
//!
//! Leases are server-side assignments between an authenticated host principal
//! and one stable session identity. They are deliberately separate from Iroh
//! profiles, trust credentials, endpoint locks, snapshots, and the ephemeral
//! live-session registry. The repository serializes every read-modify-write
//! mutation under a private advisory lock and atomically replaces one bounded,
//! versioned JSON database.

use std::path::{Path, PathBuf};

use crate::error::{MezError, Result};

mod repository;
mod types;

#[allow(
    unused_imports,
    reason = "host routing, administration, and recovery consume these lease contracts in subsequent architecture phases"
)]
pub(crate) use repository::RemoteSessionLeaseRepository;
#[allow(
    unused_imports,
    reason = "host routing, administration, and recovery consume these lease contracts in subsequent architecture phases"
)]
pub(crate) use types::{
    LeaseCheckpointReference, LeaseGarbageCollectionPolicy, LeaseGarbageCollectionPreview,
    LeaseReservation, LeaseReservationRequest, RemoteSessionLease, RemoteSessionLeaseState,
};

/// Returns the protected durable host-state directory below one config root.
pub(crate) fn default_host_state_directory(config_root: impl AsRef<Path>) -> PathBuf {
    config_root.as_ref().join("host")
}

/// Returns the protected durable-lease directory below one host-state root.
pub(crate) fn default_remote_session_lease_directory(config_root: impl AsRef<Path>) -> PathBuf {
    default_host_state_directory(config_root).join("leases")
}

fn validate_nonempty_identifier(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty()
        || value.len() > 256
        || value.chars().any(char::is_control)
        || value.contains('/')
        || value.contains('\\')
        || matches!(value, "." | "..")
    {
        return Err(MezError::invalid_args(format!(
            "remote session lease {field} must be non-empty printable identifier text"
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
            "remote session lease {field} must be printable text up to {max_bytes} bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
