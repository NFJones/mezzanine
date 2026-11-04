//! Typed source roles shared by the physical canonical receipt owner.
//!
//! Recipient input and sender terminal handoff use separate schemas, occurrence
//! domains, filenames, versions and final row kinds. Only physical ownership is
//! shared. The untagged native variant preserves existing v3 source bytes; strict
//! source structs prevent one role from decoding as the other. No descriptive
//! source or physical receipt grants a messaging capability.

use super::*;
use mez_agent::messaging::{MessageNativeInputAnchor, MessageTerminalHandoffAnchor};
use mez_agent::transcript::{
    TranscriptContextEvent, TranscriptNativeInputSource, TranscriptTerminalHandoffSource,
};

/// Role-specific journal expectations, in addition to actual-anchor matching.
/// Native p1 freezes its first offer clock; terminal source matching already
/// freezes every first terminal field, so its journal need not duplicate IDs.
#[derive(Clone, Copy)]
pub(crate) enum CustodyExpectation<'a> {
    NativeInput(&'a TranscriptNativeInputSource, &'a CustodyTranscriptPlan),
    TerminalHandoff(&'a CustodyTranscriptPlan),
}
impl CustodyExpectation<'_> {
    /// Checks exact first placement and the matching role's immutable metadata.
    pub(super) fn matches(self, plan: &CustodyAppendPlan) -> bool {
        match self {
            Self::NativeInput(source, input) => {
                plan.source().native_input() == Some(source) && plan.input() == input
            }
            Self::TerminalHandoff(input) => {
                plan.source().kind() == CustodyReceiptKind::TerminalHandoff && plan.input() == input
            }
        }
    }
}

/// Distinct physical namespaces owned by the same checked publication machinery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CustodyReceiptKind {
    NativeInput,
    TerminalHandoff,
}
impl CustodyReceiptKind {
    /// Returns the canonical namespace word; conversation IDs cannot select it.
    pub(crate) fn namespace(self) -> &'static str {
        match self {
            Self::NativeInput => "native",
            Self::TerminalHandoff => "handoff",
        }
    }
    /// Returns the framing version whose source role must match this kind.
    pub(crate) fn receipt_version(self) -> u64 {
        match self {
            Self::NativeInput => 3,
            Self::TerminalHandoff => 4,
        }
    }
}

/// Descriptive role, preserving the original native v3 serialized shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum CustodySource {
    NativeInput(TranscriptNativeInputSource),
    TerminalHandoff(TranscriptTerminalHandoffSource),
}
impl CustodySource {
    /// Validates bounded data only; the actual root checks its source anchor.
    pub(crate) fn validate(&self) -> Result<()> {
        match self {
            Self::NativeInput(s) => s.validate(),
            Self::TerminalHandoff(s) => s.validate(),
        }
        .map_err(|_| unavailable())
    }
    /// Determines namespace/version without permitting caller-selected routing.
    pub(crate) fn kind(&self) -> CustodyReceiptKind {
        match self {
            Self::NativeInput(_) => CustodyReceiptKind::NativeInput,
            Self::TerminalHandoff(_) => CustodyReceiptKind::TerminalHandoff,
        }
    }
    /// Returns the original transport occurrence, independently of archive rows.
    pub(crate) fn message_sequence(&self) -> u64 {
        match self {
            Self::NativeInput(s) => s.message_sequence,
            Self::TerminalHandoff(s) => s.message_sequence,
        }
    }
    /// Returns the distinct source-role occurrence digest.
    pub(crate) fn occurrence(&self) -> [u8; 32] {
        match self {
            Self::NativeInput(s) => s.occurrence(),
            Self::TerminalHandoff(s) => s.occurrence(),
        }
    }
    /// Selects original recipient conversation for input, sender for handoff.
    pub(crate) fn conversation_id(&self) -> &str {
        match self {
            Self::NativeInput(s) => s.native_owner.conversation_id(),
            Self::TerminalHandoff(s) => s.native_owner.conversation_id(),
        }
    }
    /// Extracts only the native recipient schema for existing p1/c1 journals.
    pub(crate) fn native_input(&self) -> Option<&TranscriptNativeInputSource> {
        match self {
            Self::NativeInput(s) => Some(s),
            Self::TerminalHandoff(_) => None,
        }
    }
    /// Checks exact typed final row, actor and target, without inventing input
    /// rights for a sender handoff or trusting equal display labels.
    pub(super) fn matches_row(&self, row: &TranscriptEntry) -> bool {
        if row.conversation_id != self.conversation_id()
            || row.role != mez_agent::transcript::TranscriptRole::System
        {
            return false;
        }
        match (
            self,
            TranscriptContextEvent::from_transcript_content(&row.content),
        ) {
            (
                Self::NativeInput(source),
                Some(TranscriptContextEvent::NativePeerMessage { event }),
            ) => row.agent_id == source.recipient && event.source == *source,
            (
                Self::TerminalHandoff(source),
                Some(TranscriptContextEvent::TerminalHandoff { event }),
            ) => row.agent_id == source.sender && event.source == *source,
            _ => false,
        }
    }
    /// Reads a validated typed final row into descriptive physical-plan data.
    pub(super) fn from_row(row: &TranscriptEntry) -> Result<Self> {
        let source = match TranscriptContextEvent::from_transcript_content(&row.content) {
            Some(TranscriptContextEvent::NativePeerMessage { event }) => {
                Self::NativeInput(event.source)
            }
            Some(TranscriptContextEvent::TerminalHandoff { event }) => {
                Self::TerminalHandoff(event.source)
            }
            _ => return Err(unavailable()),
        };
        if !source.matches_row(row) {
            return Err(unavailable());
        }
        Ok(source)
    }
}

/// Original-source interface implemented only by the two actual lower caps.
/// A descriptive receipt source cannot satisfy this interface by decoding data.
pub(crate) trait CustodyAnchor {
    /// Selects the physical namespace from the actual source role.
    fn kind(&self) -> CustodyReceiptKind;
    /// Borrows the immutable original native destination, never a new task.
    fn conversation_id(&self) -> &str;
    /// Requires complete original attribution and matching source role.
    fn matches(&self, source: &CustodySource, store_id: &[u8; 16]) -> bool;
}
impl CustodyAnchor for MessageNativeInputAnchor {
    fn kind(&self) -> CustodyReceiptKind {
        CustodyReceiptKind::NativeInput
    }
    fn conversation_id(&self) -> &str {
        self.native_owner().conversation_id()
    }
    fn matches(&self, source: &CustodySource, store_id: &[u8; 16]) -> bool {
        matches!(source, CustodySource::NativeInput(s) if s.matches_anchor(self, store_id))
    }
}
impl CustodyAnchor for MessageTerminalHandoffAnchor {
    fn kind(&self) -> CustodyReceiptKind {
        CustodyReceiptKind::TerminalHandoff
    }
    fn conversation_id(&self) -> &str {
        self.native_owner().conversation_id()
    }
    fn matches(&self, source: &CustodySource, store_id: &[u8; 16]) -> bool {
        matches!(source, CustodySource::TerminalHandoff(s) if s.matches_anchor(self, store_id))
    }
}
