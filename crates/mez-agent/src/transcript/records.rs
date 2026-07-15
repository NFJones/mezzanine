//! Canonical provider-independent transcript entries.

use super::TranscriptContractError;

/// Role associated with one transcript entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptRole {
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
pub struct TranscriptEntry {
    /// Conversation identity.
    pub conversation_id: String,
    /// One-based sequence number within the conversation.
    pub sequence: u64,
    /// Creation time as Unix seconds.
    pub created_at_unix_seconds: u64,
    /// Message role.
    pub role: TranscriptRole,
    /// Turn id associated with the entry.
    pub turn_id: String,
    /// Agent id associated with the entry.
    pub agent_id: String,
    /// Pane id associated with the entry.
    pub pane_id: String,
    /// Message content.
    pub content: String,
}

impl TranscriptEntry {
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

pub(super) fn validate_conversation_id(value: &str) -> Result<(), TranscriptContractError> {
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

pub(super) fn validate_required(label: &str, value: &str) -> Result<(), TranscriptContractError> {
    if value.trim().is_empty() || value.bytes().any(|byte| byte == 0) {
        return Err(TranscriptContractError::new(format!(
            "{label} must not be empty or contain NUL bytes"
        )));
    }
    Ok(())
}
