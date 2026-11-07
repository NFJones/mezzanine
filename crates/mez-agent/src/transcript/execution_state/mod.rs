//! Execution lifecycle controls have no model occurrence, cached cost or stable
//! instruction slot. Version two of the strict context-record family identifies
//! this separate envelope: older family readers refuse it instead of silently
//! treating unknown System text as display. Decoding remains descriptive; product
//! source custody must be qualified before the canonical replay port is invoked.

use super::TranscriptContractError;
use crate::ReplayExecutionState;
use serde::{Deserialize, Serialize};

/// Control framing deliberately differs from version-one model occurrences.
pub const EXECUTION_STATE_RECORD_MARKER: &str = "[mez-transcript-context-record/v2]\n";
const VERSION: &str = "mez-transcript-context-record/v2";
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// Original producer state without a fabricated model occurrence or expense.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptExecutionStateRecord {
    state: ReplayExecutionState,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: String,
    state: ReplayExecutionState,
}

impl TranscriptExecutionStateRecord {
    /// Captures a validated descriptor. This grants no producer, native worker,
    /// task-retirement or ordinary append authority to an arbitrary caller.
    pub fn new(state: ReplayExecutionState) -> Result<Self, TranscriptContractError> {
        state.validate().map_err(|_| invalid())?;
        Ok(Self { state })
    }

    /// Borrows original group membership and descriptive interval metadata.
    pub fn state(&self) -> &ReplayExecutionState {
        &self.state
    }

    /// Encodes bounded non-model metadata, never an estimated context block.
    pub fn to_transcript_content(&self) -> Result<String, TranscriptContractError> {
        let payload = serde_json::to_string(&Wire {
            version: VERSION.into(),
            state: self.state.clone(),
        })
        .map_err(|_| invalid())?;
        if payload
            .len()
            .saturating_add(EXECUTION_STATE_RECORD_MARKER.len())
            > MAX_BYTES
        {
            return Err(invalid());
        }
        Ok(format!("{EXECUTION_STATE_RECORD_MARKER}{payload}"))
    }

    /// Unrelated records return None. Recognized malformed controls refuse;
    /// the enclosing context-record family decoder rejects unsupported versions.
    /// Caller must require the original System role and qualify actual custody.
    pub fn from_transcript_content(content: &str) -> Result<Option<Self>, TranscriptContractError> {
        if !content.starts_with("[mez-transcript-context-record/v2]") {
            return Ok(None);
        }
        if content.len() > MAX_BYTES {
            return Err(invalid());
        }
        let payload = content
            .strip_prefix(EXECUTION_STATE_RECORD_MARKER)
            .ok_or_else(invalid)?;
        let wire: Wire = serde_json::from_str(payload).map_err(|_| invalid())?;
        if wire.version != VERSION {
            return Err(invalid());
        }
        Self::new(wire.state).map(Some)
    }
}

fn invalid() -> TranscriptContractError {
    TranscriptContractError::new("invalid execution lifecycle control record")
}

#[cfg(test)]
mod tests;
