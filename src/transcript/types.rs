//! Transcript store data types.
//!
//! These types represent saved conversation entries, summaries, and the
//! filesystem-backed store handle. Encoding and I/O behavior live in sibling
//! modules.

use crate::agent::{AgentContextUsageSnapshot, ModelTokenUsage, ModelTokenUsageKey};
use std::collections::BTreeMap;
use std::path::PathBuf;

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

/// One saved conversation transcript entry.
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
}

/// Summary of one saved conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationSummary {
    /// Conversation identity.
    pub conversation_id: String,
    /// Number of entries in the conversation.
    pub entries: usize,
    /// Creation time of the first entry.
    pub first_created_at_unix_seconds: u64,
    /// Creation time of the last entry.
    pub last_created_at_unix_seconds: u64,
    /// Last turn id in the conversation.
    pub last_turn_id: String,
    /// Agent id from the last entry.
    pub agent_id: String,
    /// Pane id from the last entry.
    pub pane_id: String,
    /// Best-known project root or working directory for the conversation.
    pub directory: Option<String>,
    /// Bounded text from the first user-authored prompt in the conversation.
    pub initial_prompt: Option<String>,
}

/// Durable pane binding and preference metadata for an active agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionMetadata {
    /// Owning Mezzanine session identity.
    pub mezzanine_session_id: String,
    /// Pane that owns the agent shell session.
    pub pane_id: String,
    /// Durable conversation identity bound to the pane.
    pub conversation_id: String,
    /// Agent shell visibility name.
    pub visibility: String,
    /// Running turn at the time of the checkpoint, if any.
    pub running_turn_id: Option<String>,
    /// Number of transcript entries known to the pane metadata.
    pub transcript_entries: u64,
    /// Pane-local agent log level.
    pub log_level: String,
    /// Pane-scoped model profile override, if one is active.
    pub pane_model_profile: Option<String>,
    /// Whether pane-local planning mode is active.
    pub planning_enabled: bool,
    /// Pane-local response style, if one is active.
    pub response_style: Option<String>,
    /// Pane-local routing override, if one is active.
    pub routing_enabled: Option<bool>,
    /// Saved approval policy name for the session, if known.
    pub approval_policy: Option<String>,
    /// Best-known pane working directory for the agent session.
    pub working_directory: Option<String>,
    /// Best-known project root for the agent session.
    pub project_root: Option<String>,
    /// Provider-reported token usage accumulated for this conversation.
    pub token_usage: ModelTokenUsage,
    /// Provider-reported token usage accumulated per provider/model.
    pub token_usage_by_model: BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    /// Last provider-reported context usage label shown in pane status.
    pub context_usage: Option<String>,
    /// Last provider request-context snapshot shown in pane status.
    pub context_usage_snapshot: Option<AgentContextUsageSnapshot>,
}

/// Filesystem-backed transcript store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTranscriptStore {
    /// Stores the root value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) root: PathBuf,
}
