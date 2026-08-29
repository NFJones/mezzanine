//! Durable agent conversation sessions.
//!
//! Each agent session is stored in a private directory under the configured
//! session root. The directory contains an append-only transcript, while the
//! session root contains bounded shared prompt-history metadata for agent and
//! primary command prompts. A rebuildable SQLite catalog indexes saved-session
//! discovery metadata without moving transcript or presentation payloads out
//! of their inspectable per-session files.

/// Exposes the catalog module boundary.
///
/// The catalog owns only rebuildable discovery metadata; transcript and
/// presentation payloads remain filesystem-backed and authoritative.
mod catalog;

/// Exposes the encoding module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod encoding;
/// Exposes the fs module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod fs;
/// Exposes the store module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod store;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;

pub use types::{
    AgentPresentationEntry, AgentTranscriptStore, SavedAgentSession, SavedSessionCursor,
    SavedSessionPageAnchor, SavedSessionQuery,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;

pub use store::DEFAULT_SAVED_AGENT_SESSION_LIMIT;

impl mez_agent::TranscriptPersistence for AgentTranscriptStore {
    type Error = crate::error::MezError;

    fn next_sequence(&self, conversation_id: &str) -> Result<Option<u64>, Self::Error> {
        match AgentTranscriptStore::next_sequence(self, conversation_id) {
            Ok(sequence) => Ok(Some(sequence)),
            Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn append(&self, entry: &mez_agent::transcript::TranscriptEntry) -> Result<(), Self::Error> {
        AgentTranscriptStore::append(self, entry)
    }
}
