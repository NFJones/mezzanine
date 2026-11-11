//! Inert active-checkpoint provenance. A saved reference never grants live
//! binding custody, policy mutation, provider admission or cleanup authority.

use super::{BudgetReceiptId, BudgetRootId};
use crate::transcript::{TranscriptContractError, validate_conversation_id};

/// Exact durable accounting subject and captured binding owner. Product restore
/// must qualify this against its original stores before installing the binding.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetCheckpointReference {
    /// Original accounting-store namespace, not a caller-selected store path.
    pub namespace: BudgetReceiptId,
    /// Immutable root whose expense and overrides remain authoritative.
    pub root: BudgetRootId,
    /// Exact conversation membership, never inferred from pane or lineage labels.
    pub conversation: String,
    /// Original immutable membership-publication operation.
    pub operation: BudgetReceiptId,
    /// Captured native binding owner; persisted text is not retirement proof.
    pub owner: BudgetReceiptId,
}

impl BudgetCheckpointReference {
    /// Checks bounded subject framing only, not store availability or authority.
    pub fn validate(&self) -> Result<(), TranscriptContractError> {
        validate_conversation_id(&self.conversation)?;
        if self.conversation.len() > 256 {
            return Err(TranscriptContractError::new(
                "budget checkpoint conversation too long",
            ));
        }
        Ok(())
    }
}
