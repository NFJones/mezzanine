//! Durable lease records and repository operation contracts.

use serde::{Deserialize, Serialize};

use super::{Result, validate_nonempty_identifier, validate_optional_text};

/// Explicit lifecycle of one durable remote-session assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RemoteSessionLeaseState {
    /// Identity and authority are reserved while runtime construction proceeds.
    Pending,
    /// A ready runtime currently backs the lease.
    Active,
    /// Durable authority remains, but a fresh runtime must be reconstructed.
    Recoverable,
    /// The durable reservation was intentionally released.
    Released,
    /// Future attachment and recovery are denied.
    Revoked,
    /// Construction or recovery ended in an administratively visible failure.
    Failed,
}

impl RemoteSessionLeaseState {
    /// Whether retention policy may garbage-collect this terminal record.
    pub(crate) const fn is_garbage_collectable(self) -> bool {
        matches!(self, Self::Released | Self::Revoked | Self::Failed)
    }
}

/// Versioned reference to snapshot data used for fresh-process reconstruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LeaseCheckpointReference {
    /// Existing snapshot manifest identity.
    pub(crate) snapshot_id: String,
    /// Snapshot manifest format version expected by the lease.
    pub(crate) snapshot_version: u32,
    /// Session identity encoded by the referenced snapshot payload.
    pub(crate) session_id: String,
    /// Time at which this reference became authoritative.
    pub(crate) recorded_at_unix_seconds: u64,
}

impl LeaseCheckpointReference {
    pub(super) fn validate(&self, expected_session_id: &str) -> Result<()> {
        validate_nonempty_identifier(&self.snapshot_id, "checkpoint snapshot id")?;
        validate_nonempty_identifier(&self.session_id, "checkpoint session id")?;
        if self.snapshot_version == 0 {
            return Err(crate::error::MezError::invalid_args(
                "remote session lease checkpoint version must be positive",
            ));
        }
        if self.session_id != expected_session_id {
            return Err(crate::error::MezError::conflict(
                "remote session lease checkpoint belongs to a different session",
            ));
        }
        Ok(())
    }
}

/// Durable server-side assignment and lifecycle metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RemoteSessionLease {
    pub(crate) lease_id: String,
    pub(crate) session_id: String,
    pub(crate) owner_principal_id: String,
    #[serde(default = "default_owner_live_session_limit")]
    pub(crate) owner_live_session_limit: usize,
    pub(crate) name: Option<String>,
    pub(crate) default_for_owner: bool,
    pub(crate) state: RemoteSessionLeaseState,
    pub(crate) created_at_unix_seconds: u64,
    pub(crate) updated_at_unix_seconds: u64,
    pub(crate) activated_at_unix_seconds: Option<u64>,
    pub(crate) terminal_at_unix_seconds: Option<u64>,
    #[serde(default)]
    pub(crate) expires_at_unix_seconds: Option<u64>,
    pub(crate) idempotency_key: String,
    pub(crate) creation_fingerprint: String,
    pub(crate) checkpoint: Option<LeaseCheckpointReference>,
    pub(crate) failure: Option<String>,
    pub(crate) boot_generation: u64,
    pub(crate) lease_generation: u64,
}

impl RemoteSessionLease {
    pub(super) fn validate(&self) -> Result<()> {
        validate_nonempty_identifier(&self.lease_id, "id")?;
        validate_nonempty_identifier(&self.session_id, "session id")?;
        validate_nonempty_identifier(&self.owner_principal_id, "owner principal id")?;
        if self.owner_live_session_limit == 0 {
            return Err(crate::error::MezError::invalid_args(
                "remote session lease owner live-session limit must be positive",
            ));
        }
        validate_nonempty_identifier(&self.idempotency_key, "idempotency key")?;
        validate_nonempty_identifier(&self.creation_fingerprint, "creation fingerprint")?;
        validate_optional_text(self.name.as_deref(), "name", 256)?;
        validate_optional_text(self.failure.as_deref(), "failure", 1024)?;
        if self.lease_generation == 0 {
            return Err(crate::error::MezError::invalid_args(
                "remote session lease generation must be positive",
            ));
        }
        if self.updated_at_unix_seconds < self.created_at_unix_seconds
            || self
                .activated_at_unix_seconds
                .is_some_and(|value| value < self.created_at_unix_seconds)
            || self
                .terminal_at_unix_seconds
                .is_some_and(|value| value < self.created_at_unix_seconds)
            || self
                .expires_at_unix_seconds
                .is_some_and(|value| value <= self.created_at_unix_seconds)
        {
            return Err(crate::error::MezError::invalid_state(
                "remote session lease timestamps are not monotonic",
            ));
        }
        if let Some(checkpoint) = &self.checkpoint {
            checkpoint.validate(&self.session_id)?;
        }
        if self.state.is_garbage_collectable() != self.terminal_at_unix_seconds.is_some() {
            return Err(crate::error::MezError::invalid_state(
                "remote session lease terminal timestamp does not match lifecycle state",
            ));
        }
        Ok(())
    }
}

/// Inputs reserved transactionally before runtime allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeaseReservationRequest {
    pub(crate) lease_id: String,
    pub(crate) session_id: String,
    pub(crate) owner_principal_id: String,
    pub(crate) owner_live_session_limit: usize,
    pub(crate) name: Option<String>,
    pub(crate) default_for_owner: bool,
    pub(crate) expires_at_unix_seconds: Option<u64>,
    pub(crate) idempotency_key: String,
    pub(crate) creation_fingerprint: String,
    pub(crate) now_unix_seconds: u64,
}

fn default_owner_live_session_limit() -> usize {
    usize::MAX
}

/// Result of principal-scoped idempotent reservation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LeaseReservation {
    /// A new pending lease was durably created.
    Created(RemoteSessionLease),
    /// The exact prior reservation/result was replayed.
    Replay(RemoteSessionLease),
}

impl LeaseReservation {
    pub(crate) fn lease(&self) -> &RemoteSessionLease {
        match self {
            Self::Created(lease) | Self::Replay(lease) => lease,
        }
    }
}

/// Retention cutoffs used for safe terminal-record garbage collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LeaseGarbageCollectionPolicy {
    pub(crate) released_before_unix_seconds: u64,
    pub(crate) revoked_before_unix_seconds: u64,
    pub(crate) failed_before_unix_seconds: u64,
}

/// Stable preview/result for one bounded garbage-collection operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LeaseGarbageCollectionPreview {
    pub(crate) lease_ids: Vec<String>,
    pub(crate) checkpoint_snapshot_ids: Vec<String>,
}
