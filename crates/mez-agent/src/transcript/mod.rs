//! Provider-independent transcript records and persistence contracts.
//!
//! Canonical entries, conversation summaries, and durable session checkpoints
//! live here. Product adapters retain filesystem layout, compatibility codecs,
//! retention, compression, prompt history, and terminal presentation replay.

mod checkpoint;
mod error;
mod records;
mod summary;

pub use checkpoint::AgentSessionMetadata;
pub use error::TranscriptContractError;
pub use records::{TranscriptEntry, TranscriptRole, validate_conversation_id};
pub use summary::{ConversationSummary, summarize_conversation};

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
