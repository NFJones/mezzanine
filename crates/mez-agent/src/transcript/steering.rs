//! Execution-inert restart evidence for accepted steering occurrences.
//!
//! These records are presentation recovery, never an input queue or authority.
//! Pending local evidence becomes admission-unknown after restart: a checkpoint
//! can predate an admitted request, so recovery must not claim it was not sent.
//! Exact occurrence identity and independent display source remain unchanged.

use serde::{Deserialize, Serialize};

use super::TranscriptContractError;

/// Finite recovery source budget per pane/conversation checkpoint.
pub const STEERING_RECOVERY_BYTES: usize = 1024 * 1024;
/// Finite recovery occurrence count per pane/conversation checkpoint.
pub const STEERING_RECOVERY_ENTRIES: usize = 128;

/// Local admission evidence; no state asserts remote model receipt or cognition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SteeringRecoveryStatus {
    /// Accepted input whose local admission was not yet recorded.
    Pending,
    /// Positively recorded ordinary local request generation.
    Admitted(u64),
    /// Ownership ended before any local ordinary request admission.
    NotSent,
    /// Restart lost the live admission owner; no automatic resend is permitted.
    AdmissionUnknown,
}

/// One occurrence retained independently of canonical transcript and log sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteeringRecoveryReceipt {
    /// Stable occurrence identity, not content matching or time correlation.
    pub id: String,
    /// Exact original turn when bound; absent during pre-turn compaction work.
    pub turn_id: Option<String>,
    /// Exact canonical event identity when bound, never a snapshot high-water.
    pub event_sequence: Option<u64>,
    /// Independent exact user-facing source; no execution input is recovered.
    pub display: String,
    /// Local admission or explicit terminal uncertainty evidence.
    pub status: SteeringRecoveryStatus,
}

impl SteeringRecoveryReceipt {
    /// Validates bounded inert identity and coherent local admission evidence.
    pub fn validate(&self) -> Result<(), TranscriptContractError> {
        let identity = |value: &str| {
            !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
        };
        if !identity(&self.id)
            || self
                .turn_id
                .as_deref()
                .is_some_and(|value| !identity(value))
            || self.turn_id.is_some() != self.event_sequence.is_some()
            || self.event_sequence == Some(0)
            || self.display.len() > STEERING_RECOVERY_BYTES
            || matches!(self.status, SteeringRecoveryStatus::Admitted(0))
            || (matches!(self.status, SteeringRecoveryStatus::Admitted(_))
                && self.turn_id.is_none())
        {
            return Err(TranscriptContractError::new(
                "invalid steering recovery occurrence",
            ));
        }
        Ok(())
    }

    /// Converts abandoned pending ownership to uncertainty without replay.
    /// Positive admission and explicit not-sent evidence remain unchanged.
    pub fn after_restart(mut self) -> Self {
        if self.status == SteeringRecoveryStatus::Pending {
            self.status = SteeringRecoveryStatus::AdmissionUnknown;
        }
        self
    }
}

/// Rejects over-budget or duplicate occurrences instead of truncating pending input.
pub fn validate_steering_recovery(
    receipts: &[SteeringRecoveryReceipt],
) -> Result<(), TranscriptContractError> {
    let mut ids = std::collections::BTreeSet::new();
    let mut bytes = 0usize;
    if receipts.len() > STEERING_RECOVERY_ENTRIES {
        return Err(TranscriptContractError::new(
            "steering recovery count exhausted",
        ));
    }
    for receipt in receipts {
        receipt.validate()?;
        bytes = bytes.saturating_add(receipt.display.len());
        if !ids.insert(&receipt.id) || bytes > STEERING_RECOVERY_BYTES {
            return Err(TranscriptContractError::new(
                "steering recovery identity or source budget exhausted",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Restart uncertainty is idempotent and never upgrades pending evidence to
    /// admitted or unsent; equal text remains distinct by accepted occurrence ID.
    #[test]
    fn steering_recovery_restart_preserves_occurrences_and_uncertainty() {
        let first = SteeringRecoveryReceipt {
            id: "first".into(),
            turn_id: Some("turn".into()),
            event_sequence: Some(1),
            display: "same\r\n雪".into(),
            status: SteeringRecoveryStatus::Pending,
        };
        let mut second = first.clone();
        second.id = "second".into();
        validate_steering_recovery(&[first.clone(), second]).unwrap();
        let recovered = first.after_restart();
        assert_eq!(recovered.status, SteeringRecoveryStatus::AdmissionUnknown);
        assert_eq!(recovered.clone().after_restart(), recovered);
        for status in [
            SteeringRecoveryStatus::Admitted(7),
            SteeringRecoveryStatus::NotSent,
        ] {
            let mut receipt = recovered.clone();
            receipt.status = status;
            assert_eq!(receipt.clone().after_restart(), receipt);
        }
    }

    /// Invalid ownership, zero admission, duplicates and source/count pressure
    /// reject before durable use; the validator never repairs or drops records.
    #[test]
    fn steering_recovery_rejects_invalid_evidence_and_budgets() {
        let mut receipt = SteeringRecoveryReceipt {
            id: "receipt".into(),
            turn_id: None,
            event_sequence: None,
            display: String::new(),
            status: SteeringRecoveryStatus::Pending,
        };
        receipt.validate().unwrap();
        assert!(validate_steering_recovery(&[receipt.clone(), receipt.clone()]).is_err());
        assert!(
            validate_steering_recovery(&vec![receipt.clone(); STEERING_RECOVERY_ENTRIES + 1])
                .is_err()
        );
        receipt.status = SteeringRecoveryStatus::Admitted(1);
        assert!(receipt.validate().is_err());
        receipt.turn_id = Some("turn".into());
        receipt.event_sequence = Some(1);
        receipt.status = SteeringRecoveryStatus::Admitted(0);
        assert!(receipt.validate().is_err());
        receipt.status = SteeringRecoveryStatus::Pending;
        receipt.display = "x".repeat(STEERING_RECOVERY_BYTES + 1);
        assert!(receipt.validate().is_err());
    }
}
