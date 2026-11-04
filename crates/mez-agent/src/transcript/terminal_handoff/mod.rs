//! Canonical neutral delivery facts attributed to the original native sender.
//!
//! This codec records a RuntimeHint, never peer input, delivery acknowledgement,
//! task completion or permission to repeat effects. Source data cannot construct
//! an original handoff capability. The trusted product port must compare every
//! field against an actual source anchor and observe the original archive before
//! it can seal canonical proof. Historical decoding never registers a sender or
//! revives its task/provider. Live projection and historical matching are distinct.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{TranscriptContractError, validate_conversation_id};
use crate::messaging::{
    DeliveryTerminalReason, MessageDeliveryOutcome, MessageNativeOwner,
    MessageRecipientBindingSnapshot, MessageTerminalHandoffAnchor,
};

/// Bound used by original acceptance for one payload-free delivery outcome.
const MAX_OUTCOME_BYTES: usize = 64 * 1024;

/// Descriptive provenance of exactly one original terminal outcome.
/// The distinct version field also separates this schema from native input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptTerminalHandoffSource {
    /// Terminal handoff framing version, currently one.
    pub terminal_handoff_version: u32,
    /// Actual original private custody allocation.
    pub store_id: [u8; 16],
    /// Fingerprint of the complete original accepted source.
    pub source_sha256: [u8; 32],
    /// Original transport occurrence and frozen recipient slot.
    pub message_sequence: u64,
    pub binding_index: u32,
    /// Original accepted ID; correlation only, never task or replay authority.
    pub message_id: String,
    /// Original native sender and its registration interval.
    pub sender: String,
    pub sender_registered_after_sequence: u64,
    /// Frozen recipient whose terminal outcome is being handed off.
    pub recipient: String,
    pub registered_after_sequence: u64,
    /// Original native SENDER, not the recipient's conversation or incarnation.
    pub native_owner: MessageNativeOwner,
    /// Immutable first terminal reason and observation, never a renewed horizon.
    pub reason: DeliveryTerminalReason,
    pub observed_at_ms: u64,
}

impl TranscriptTerminalHandoffSource {
    /// Projects an actual live original sender source. Lookup anchors may match
    /// historical rows but cannot authorize fresh context materialization here.
    pub fn from_anchor(
        anchor: &MessageTerminalHandoffAnchor,
        store_id: [u8; 16],
    ) -> Result<Self, TranscriptContractError> {
        if !anchor.is_live() {
            return Err(invalid());
        }
        let source = Self {
            terminal_handoff_version: 1,
            store_id,
            source_sha256: anchor.source_sha256(),
            message_sequence: anchor.sequence(),
            binding_index: u32::try_from(anchor.binding_index()).map_err(|_| invalid())?,
            message_id: anchor.outcome().message_id,
            sender: anchor.sender().to_string(),
            sender_registered_after_sequence: anchor.sender_registered_after_sequence(),
            recipient: anchor.recipient().to_string(),
            registered_after_sequence: anchor.registered_after_sequence(),
            native_owner: anchor.native_owner().clone(),
            reason: anchor.terminal().reason,
            observed_at_ms: anchor.terminal().observed_at_ms,
        };
        source.validate()?;
        Ok(source)
    }

    /// Rejects impossible attribution and bounds all retained descriptive data.
    pub fn validate(&self) -> Result<(), TranscriptContractError> {
        if self.terminal_handoff_version != 1
            || self.store_id == [0; 16]
            || self.source_sha256 == [0; 32]
            || self.message_sequence == 0
            || self.binding_index >= 4096
            || self.sender_registered_after_sequence >= self.message_sequence
            || self.registered_after_sequence >= self.message_sequence
            || !valid_agent(&self.sender)
            || !valid_agent(&self.recipient)
            || self.message_id.is_empty()
            || self
                .message_id
                .len()
                .saturating_add(self.recipient.len())
                .saturating_add(128)
                > MAX_OUTCOME_BYTES
            || self.native_owner.conversation_id().len() > 256
            || self.native_owner.incarnation().len() != 64
            || !self
                .native_owner
                .incarnation()
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.native_owner.incarnation().bytes().all(|b| b == b'0')
        {
            return Err(invalid());
        }
        validate_conversation_id(self.native_owner.conversation_id())
    }

    /// Describes the exact neutral fact; these bytes cannot consume an outcome.
    pub fn outcome(&self) -> MessageDeliveryOutcome {
        MessageDeliveryOutcome {
            sequence: self.message_sequence,
            message_id: self.message_id.clone(),
            recipient: MessageRecipientBindingSnapshot {
                recipient: self.recipient.clone(),
                registered_after_sequence: self.registered_after_sequence,
            },
            reason: self.reason,
            observed_at_ms: self.observed_at_ms,
        }
    }

    /// Uses a separate domain from recipient input, including the immutable
    /// first reason/clock. Chronology and archive positions do not renew it.
    pub fn occurrence(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"mez-terminal-canonical-handoff-v1\0");
        hash.update(self.store_id);
        hash.update(self.source_sha256);
        hash.update(self.binding_index.to_le_bytes());
        hash.update(self.observed_at_ms.to_le_bytes());
        hash.update([match self.reason {
            DeliveryTerminalReason::Expired => 1,
            DeliveryTerminalReason::RecipientRetired => 2,
        }]);
        hash.finalize().into()
    }

    /// Compares the whole original source/before-image, including sender and
    /// recipient roles. Lookup matching grants no fresh materialization rights.
    pub fn matches_anchor(
        &self,
        anchor: &MessageTerminalHandoffAnchor,
        store_id: &[u8; 16],
    ) -> bool {
        self.validate().is_ok()
            && self.store_id == *store_id
            && self.source_sha256 == anchor.source_sha256()
            && self.message_sequence == anchor.sequence()
            && self.binding_index as usize == anchor.binding_index()
            && self.sender == anchor.sender().as_str()
            && self.sender_registered_after_sequence == anchor.sender_registered_after_sequence()
            && self.recipient == anchor.recipient().as_str()
            && self.registered_after_sequence == anchor.registered_after_sequence()
            && &self.native_owner == anchor.native_owner()
            && self.reason == anchor.terminal().reason
            && self.observed_at_ms == anchor.terminal().observed_at_ms
            && self.message_id == anchor.outcome().message_id
    }
}

/// Canonical RuntimeHint for an accepted send's local terminal delivery fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptTerminalHandoffEvent {
    /// Descriptive original provenance, qualified separately at storage.
    pub source: TranscriptTerminalHandoffSource,
    /// Actual nonzero chronology identity, distinct from transport sequence.
    pub event_sequence: u64,
    /// Deterministic label and neutral fact, never peer/user instructions.
    pub label: String,
    pub content: String,
    /// Complete source/chronology/projection fingerprint.
    pub projection_sha256: [u8; 32],
}

impl TranscriptTerminalHandoffEvent {
    /// Builds the fixed neutral projection; callers cannot substitute success,
    /// approval or retry text even by recomputing a projection digest.
    pub fn new(
        source: TranscriptTerminalHandoffSource,
        event_sequence: u64,
    ) -> Result<Self, TranscriptContractError> {
        source.validate()?;
        let (label, content) = terminal_outcome_reference(&source.outcome())?;
        let mut event = Self {
            source,
            event_sequence,
            label,
            content,
            projection_sha256: [0; 32],
        };
        event.projection_sha256 = event.digest()?;
        event.validate()?;
        Ok(event)
    }

    /// Checks complete framing, exact neutral semantics and immutable chronology.
    pub fn validate(&self) -> Result<(), TranscriptContractError> {
        self.source.validate()?;
        let (label, content) = terminal_outcome_reference(&self.source.outcome())?;
        if self.event_sequence == 0
            || self.label != label
            || self.content != content
            || self.projection_sha256 != self.digest()?
        {
            return Err(invalid());
        }
        Ok(())
    }

    /// Hashes the complete typed attribution and model-visible exact projection.
    fn digest(&self) -> Result<[u8; 32], TranscriptContractError> {
        let mut hash = Sha256::new();
        hash.update(b"mez-terminal-canonical-event-v1\0");
        hash.update(serde_json::to_vec(&self.source).map_err(|_| invalid())?);
        hash.update(self.event_sequence.to_le_bytes());
        hash.update((self.label.len() as u64).to_le_bytes());
        hash.update(self.label.as_bytes());
        hash.update((self.content.len() as u64).to_le_bytes());
        hash.update(self.content.as_bytes());
        Ok(hash.finalize().into())
    }
}

/// Produces the single neutral model-visible form shared by live context and
/// canonical projection. This pure formatter grants no ownership or consume cap.
pub fn terminal_outcome_reference(
    outcome: &MessageDeliveryOutcome,
) -> Result<(String, String), TranscriptContractError> {
    if outcome
        .message_id
        .len()
        .saturating_add(outcome.recipient.recipient.len())
        .saturating_add(128)
        > MAX_OUTCOME_BYTES
    {
        return Err(invalid());
    }
    let recipient_hash = Sha256::digest(outcome.recipient.recipient.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let label = format!(
        "mmp-delivery-outcome sequence={} recipient={recipient_hash} floor={}",
        outcome.sequence, outcome.recipient.registered_after_sequence
    );
    let content = format!(
        "Accepted MMP recipient delivery ended without recipient commit. This is a local delivery fact, not a peer answer, approval, task success or permission to retry effects. The original send action remains accepted.\n{}",
        serde_json::to_string(outcome).map_err(|_| invalid())?
    );
    Ok((label, content))
}

/// Validates bounded identity text without granting registration authority.
fn valid_agent(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

/// Refuses without exposing original IDs, attribution or reference content.
fn invalid() -> TranscriptContractError {
    TranscriptContractError::new("invalid canonical terminal handoff event")
}

#[cfg(test)]
mod tests;
