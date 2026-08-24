use serde::{Deserialize, Serialize};

use super::{Result, validate_identifier, validate_optional_text};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LocalSessionAssignmentState {
    Pending,
    Active,
    Recoverable,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LocalAssignmentCheckpoint {
    pub(crate) snapshot_id: String,
    pub(crate) snapshot_version: u32,
    pub(crate) session_id: String,
    pub(crate) recorded_at_unix_seconds: u64,
}

impl LocalAssignmentCheckpoint {
    pub(crate) fn validate(&self, expected_session_id: &str) -> Result<()> {
        validate_identifier(&self.snapshot_id, "checkpoint snapshot id")?;
        validate_identifier(&self.session_id, "checkpoint session id")?;
        if self.snapshot_version == 0 {
            return Err(crate::error::MezError::invalid_args(
                "local session checkpoint version must be positive",
            ));
        }
        if self.session_id != expected_session_id {
            return Err(crate::error::MezError::conflict(
                "local session checkpoint belongs to a different session",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LocalSessionAssignment {
    pub(crate) session_id: String,
    pub(crate) name: String,
    pub(crate) default_for_host: bool,
    pub(crate) state: LocalSessionAssignmentState,
    pub(crate) created_at_unix_seconds: u64,
    pub(crate) updated_at_unix_seconds: u64,
    pub(crate) checkpoint: Option<LocalAssignmentCheckpoint>,
    pub(crate) failure: Option<String>,
    pub(crate) boot_generation: u64,
    pub(crate) assignment_generation: u64,
}

impl LocalSessionAssignment {
    pub(crate) fn validate(&self) -> Result<()> {
        validate_identifier(&self.session_id, "session id")?;
        validate_optional_text(Some(&self.name), "name", 256)?;
        validate_optional_text(self.failure.as_deref(), "failure", 1024)?;
        if self.assignment_generation == 0 {
            return Err(crate::error::MezError::invalid_args(
                "local session assignment generation must be positive",
            ));
        }
        if self.updated_at_unix_seconds < self.created_at_unix_seconds {
            return Err(crate::error::MezError::invalid_state(
                "local session assignment timestamps are not monotonic",
            ));
        }
        if let Some(checkpoint) = &self.checkpoint {
            checkpoint.validate(&self.session_id)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LocalAssignmentReservationRequest {
    pub(crate) session_id: String,
    pub(crate) name: String,
    pub(crate) default_for_host: bool,
    pub(crate) now_unix_seconds: u64,
}
