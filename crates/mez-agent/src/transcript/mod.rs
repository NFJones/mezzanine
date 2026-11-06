//! Provider-independent transcript records and persistence contracts.
//!
//! Canonical entries, conversation summaries, and durable session checkpoints
//! live here. Product adapters retain filesystem layout, compatibility codecs,
//! retention, compression, prompt history, and terminal presentation replay.

mod checkpoint;
mod context_event;
mod context_record;
mod error;
mod records;
mod replay_projection;
pub use replay_projection::{transcript_display_copy_indices, transcript_zero_projection_indices};
mod steering;
mod summary;
mod terminal_handoff;

pub use checkpoint::{AgentSessionMetadata, PaneModelProfileSelection};
pub use context_event::{TRANSCRIPT_CONTEXT_EVENT_MARKER, TranscriptContextEvent};
pub use context_record::{
    ACCOUNTED_CONTEXT_RECORD_MARKER, TranscriptContextRecord, is_accounted_context_record,
};
pub use error::TranscriptContractError;
pub use records::{TranscriptEntry, TranscriptRole, validate_conversation_id};
pub use steering::{
    STEERING_RECOVERY_BYTES, STEERING_RECOVERY_ENTRIES, SteeringRecoveryReceipt,
    SteeringRecoveryStatus, validate_steering_recovery,
};
pub use summary::{
    ConversationSummary, bounded_summary_text, summarize_conversation,
    transcript_entry_user_content,
};
pub use terminal_handoff::{
    TranscriptTerminalHandoffEvent, TranscriptTerminalHandoffSource, terminal_outcome_reference,
};

/// Persistence boundary required by agent transcript projection.
///
/// Implementations translate canonical entries into their durable format. A
/// missing conversation is represented by `Ok(None)` so the harness allocates
/// its first sequence without depending on product storage error categories.
pub trait TranscriptPersistence {
    /// Product-owned persistence failure type.
    type Error;

    /// Returns the next sequence for an existing conversation, or `None` when
    /// the conversation has no durable transcript yet.
    fn next_sequence(&self, conversation_id: &str) -> Result<Option<u64>, Self::Error>;

    /// Appends one validated transcript entry durably.
    fn append(&self, entry: &TranscriptEntry) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests;

mod native_input;
pub use native_input::{TranscriptNativeInputEvent, TranscriptNativeInputSource};
