//! Descriptive original-source provenance on a canonical native peer event.
//!
//! This typed archive row remains untrusted peer reference data on hydration.
//! Its metadata is not native delivery, ACK, scheduling or reconnect authority.
//! Only an original-root product port comparing the actual source and canonical
//! storage may qualify it into a live native commit proof. Ordinary message
//! labels, presentation rows and imported DTOs never acquire that capability.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{TranscriptContractError, validate_conversation_id};
use crate::messaging::{MessageNativeInputAnchor, MessageNativeOwner};

/// Immutable descriptive attribution, validated again against the real original.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptNativeInputSource {
    /// Private provenance framing version; currently one.
    pub version: u32,
    /// Original custody allocation, never reconstructed from a public session ID.
    pub store_id: [u8; 16],
    /// Digest of the complete immutable accepted source.
    pub source_sha256: [u8; 32],
    /// Original transport occurrence; independent of transcript row numbering.
    pub message_sequence: u64,
    /// Frozen recipient slot in the original accepted audience.
    pub binding_index: u32,
    /// Original recipient identity, without registration authority.
    pub recipient: String,
    /// Original recipient registration interval floor.
    pub registered_after_sequence: u64,
    /// Frozen conversation and shell incarnation; descriptive bytes only.
    pub native_owner: MessageNativeOwner,
    /// Genuine first selected delivery clock; never refreshed on recovery.
    pub offered_at_ms: u64,
}
impl TranscriptNativeInputSource {
    /// Describes one actual source; original root identity is supplied by its
    /// trusted port. Returned bytes grant no native authority by themselves.
    pub fn from_anchor(
        anchor: &MessageNativeInputAnchor,
        store_id: [u8; 16],
    ) -> Result<Self, TranscriptContractError> {
        let source = Self {
            version: 1,
            store_id,
            source_sha256: anchor.source_sha256(),
            message_sequence: anchor.sequence(),
            binding_index: u32::try_from(anchor.binding_index()).map_err(|_| invalid())?,
            recipient: anchor.recipient().to_string(),
            registered_after_sequence: anchor.registered_after_sequence(),
            native_owner: anchor.native_owner().clone(),
            offered_at_ms: anchor.offered_at_ms().ok_or_else(invalid)?,
        };
        source.validate()?;
        Ok(source)
    }
    /// Rejects impossible or unbounded attribution before archive use.
    pub fn validate(&self) -> Result<(), TranscriptContractError> {
        if self.version != 1
            || self.store_id.iter().all(|b| *b == 0)
            || self.source_sha256.iter().all(|b| *b == 0)
            || self.message_sequence == 0
            || self.binding_index >= 4096
            || self.registered_after_sequence >= self.message_sequence
            || self.recipient.is_empty()
            || self.recipient.len() > 256
            || self.recipient.chars().any(char::is_control)
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
    /// Domain-separated occurrence key: original source includes its immutable
    /// native audience, and the slot chooses exactly one accepted recipient.
    pub fn occurrence(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"mez-native-canonical-input-v1\0");
        digest.update(self.store_id);
        digest.update(self.source_sha256);
        digest.update(self.binding_index.to_le_bytes());
        digest.finalize().into()
    }
    /// Compares all actual source attribution; equal occurrence text alone is
    /// insufficient. A lookup needs no new offer; its stored clock stays original.
    pub fn matches_anchor(&self, anchor: &MessageNativeInputAnchor, store_id: &[u8; 16]) -> bool {
        self.store_id == *store_id
            && self.source_sha256 == anchor.source_sha256()
            && self.message_sequence == anchor.sequence()
            && self.binding_index as usize == anchor.binding_index()
            && self.recipient == anchor.recipient().as_str()
            && self.registered_after_sequence == anchor.registered_after_sequence()
            && &self.native_owner == anchor.native_owner()
    }
}

/// Canonical peer reference plus original transport provenance, never a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptNativeInputEvent {
    /// Exact original transport attribution, qualified separately by storage.
    pub source: TranscriptNativeInputSource,
    /// Actual nonzero canonical context chronology event sequence.
    pub event_sequence: u64,
    /// Exact untrusted peer reference label.
    pub label: String,
    /// Exact canonical peer reference content, never a user instruction.
    pub content: String,
    /// Digest binding provenance, chronology identity, label and content.
    pub projection_sha256: [u8; 32],
}
impl TranscriptNativeInputEvent {
    /// Constructs a checked archive projection from the actual original native
    /// context event. No stored fact becomes a live receipt here.
    pub fn new(
        source: TranscriptNativeInputSource,
        event_sequence: u64,
        label: String,
        content: String,
    ) -> Result<Self, TranscriptContractError> {
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
    /// Validates bounded framing and complete immutable projection consistency.
    pub fn validate(&self) -> Result<(), TranscriptContractError> {
        self.source.validate()?;
        if self.event_sequence == 0
            || self.label.trim().is_empty()
            || self.label.len() > 4096
            || self.label.bytes().any(|b| b == 0)
            || self.content.trim().is_empty()
            || self.content.len() > 2 * 1024 * 1024
            || self.digest()? != self.projection_sha256
        {
            return Err(invalid());
        }
        Ok(())
    }
    /// Hashes borrowed content and the small typed attribution without copying
    /// the canonical payload a second time.
    fn digest(&self) -> Result<[u8; 32], TranscriptContractError> {
        let mut digest = Sha256::new();
        digest.update(b"mez-native-canonical-peer-event-v1\0");
        digest.update(serde_json::to_vec(&self.source).map_err(|_| invalid())?);
        digest.update(self.event_sequence.to_le_bytes());
        digest.update((self.label.len() as u64).to_le_bytes());
        digest.update(self.label.as_bytes());
        digest.update((self.content.len() as u64).to_le_bytes());
        digest.update(self.content.as_bytes());
        Ok(digest.finalize().into())
    }
}

/// Content-free rejection never includes private original attribution or input.
fn invalid() -> TranscriptContractError {
    TranscriptContractError::new("invalid canonical native input event")
}

#[cfg(test)]
mod tests;
