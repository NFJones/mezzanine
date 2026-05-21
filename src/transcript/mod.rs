//! Durable agent conversation sessions.
//!
//! Each agent session is stored in a private directory under the configured
//! session root. The directory contains an append-only transcript, while the
//! session root contains bounded shared prompt-history metadata for agent and
//! primary command prompts so readline navigation can span prompt openings
//! without requiring a database or provider credentials.

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
/// Exposes the summary module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod summary;
/// Exposes the types module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
mod types;

pub use types::{
    AgentPresentationEntry, AgentSessionMetadata, AgentTranscriptStore, ConversationSummary,
    TranscriptEntry, TranscriptRole,
};

/// Exposes the tests module boundary.
///
/// The nested module keeps its implementation details isolated while this
/// declaration makes the boundary available to the crate.
#[cfg(test)]
mod tests;
