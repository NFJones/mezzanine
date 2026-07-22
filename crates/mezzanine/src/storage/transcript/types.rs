//! Transcript store data types.
//!
//! Presentation replay remains product-owned because validation depends on
//! terminal wrapping policy. The store handle owns configured filesystem state;
//! canonical transcript and session records live in `mez_agent::transcript`.

use std::path::PathBuf;

/// One durable user-visible agent transcript presentation entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPresentationEntry {
    /// Conversation identity.
    pub conversation_id: String,
    /// One-based presentation sequence number within the conversation.
    pub sequence: u64,
    /// Creation time as Unix seconds.
    pub created_at_unix_seconds: u64,
    /// Pane id that rendered the presentation entry.
    pub pane_id: String,
    /// Turn id associated with the rendered entry, if known.
    pub turn_id: Option<String>,
    /// Terminal width used when the entry was originally rendered.
    pub terminal_width: u16,
    /// One presentation style name per display line.
    pub style_names: Vec<String>,
    /// Lines injected into the pane buffer before ANSI styling.
    pub display_lines: Vec<String>,
    /// Copy-mode replacement lines for this presentation entry.
    pub copy_lines: Vec<String>,
    /// Exact ANSI terminal bytes encoded as UTF-8 text for replay, if captured.
    pub ansi_text: Option<String>,
    /// Original assistant payload used to reproduce this entry at another geometry.
    pub source_text: Option<String>,
    /// Media type that selects the assistant renderer for `source_text`.
    pub source_content_type: Option<String>,
}

/// Filesystem-backed transcript store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTranscriptStore {
    /// Stores the root value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) root: PathBuf,
    /// Maximum saved conversations retained for resume listing and loading.
    pub(super) saved_sessions_limit: usize,
}
