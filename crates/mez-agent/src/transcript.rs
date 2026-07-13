//! Provider-independent transcript projection and persistence contracts.
//!
//! This module owns the bounded records produced by an agent turn and the
//! narrow persistence port needed to append them. Durable encoding, storage,
//! retention, and recovery remain responsibilities of the product adapter.

use std::error::Error;
use std::fmt;

/// Role associated with one projected agent transcript entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTranscriptRole {
    /// User-authored message.
    User,
    /// Assistant-authored message.
    Assistant,
    /// Tool output or tool call transcript entry.
    Tool,
    /// System or instruction message.
    System,
}

/// One provider-independent agent transcript entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTranscriptEntry {
    /// Conversation identity.
    pub conversation_id: String,
    /// One-based sequence number within the conversation.
    pub sequence: u64,
    /// Creation time as Unix seconds.
    pub created_at_unix_seconds: u64,
    /// Message role.
    pub role: AgentTranscriptRole,
    /// Turn id associated with the entry.
    pub turn_id: String,
    /// Agent id associated with the entry.
    pub agent_id: String,
    /// Pane id associated with the entry.
    pub pane_id: String,
    /// Message content.
    pub content: String,
}

impl AgentTranscriptEntry {
    /// Validates identifiers, sequence metadata, and required text fields.
    ///
    /// Returns a contract error when the entry cannot be persisted safely.
    pub fn validate(&self) -> Result<(), TranscriptContractError> {
        validate_conversation_id(&self.conversation_id)?;
        if self.sequence == 0 || self.created_at_unix_seconds == 0 {
            return Err(TranscriptContractError::new(
                "transcript sequence and creation time must be non-zero",
            ));
        }
        validate_required("turn id", &self.turn_id)?;
        validate_required("agent id", &self.agent_id)?;
        validate_required("pane id", &self.pane_id)?;
        validate_required("transcript content", &self.content)?;
        Ok(())
    }
}

/// Error returned when an agent transcript contract is malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptContractError {
    message: String,
}

impl TranscriptContractError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TranscriptContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for TranscriptContractError {}

/// Persistence boundary required by agent transcript projection.
///
/// Implementations translate these records into their durable representation.
/// A missing conversation is represented by `Ok(None)` so the agent harness
/// can allocate the first one-based sequence without depending on product
/// storage error categories.
pub trait TranscriptPersistence {
    /// Product-owned persistence failure type.
    type Error;

    /// Returns the next sequence for an existing conversation, or `None` when
    /// the conversation has no durable transcript yet.
    fn next_sequence(&self, conversation_id: &str) -> Result<Option<u64>, Self::Error>;

    /// Appends one validated transcript entry durably.
    fn append(&self, entry: &AgentTranscriptEntry) -> Result<(), Self::Error>;
}

fn validate_conversation_id(value: &str) -> Result<(), TranscriptContractError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(TranscriptContractError::new(
            "conversation id must contain only ASCII letters, digits, '-' or '_'",
        ));
    }
    Ok(())
}

fn validate_required(label: &str, value: &str) -> Result<(), TranscriptContractError> {
    if value.trim().is_empty() || value.bytes().any(|byte| byte == 0) {
        return Err(TranscriptContractError::new(format!(
            "{label} must not be empty or contain NUL bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{AgentTranscriptEntry, AgentTranscriptRole};

    fn valid_entry() -> AgentTranscriptEntry {
        AgentTranscriptEntry {
            conversation_id: "conversation-1".to_string(),
            sequence: 1,
            created_at_unix_seconds: 1,
            role: AgentTranscriptRole::Assistant,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            pane_id: "pane-1".to_string(),
            content: "done".to_string(),
        }
    }

    #[test]
    /// Verifies a complete one-based transcript record is accepted before a
    /// product persistence adapter receives it.
    fn transcript_entry_validation_accepts_complete_records() {
        valid_entry().validate().unwrap();
    }

    #[test]
    /// Verifies zero sequence metadata and empty required content are rejected
    /// at the dependency-neutral agent boundary.
    fn transcript_entry_validation_rejects_invalid_records() {
        let mut entry = valid_entry();
        entry.sequence = 0;
        assert!(entry.validate().is_err());

        entry.sequence = 1;
        entry.content.clear();
        assert!(entry.validate().is_err());

        entry.content = "done".to_string();
        entry.conversation_id = "../conversation".to_string();
        assert!(entry.validate().is_err());
    }
}
