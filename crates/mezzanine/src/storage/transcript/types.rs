//! Transcript store data types.
//!
//! Presentation replay remains product-owned because validation depends on
//! terminal wrapping policy. The store handle owns configured filesystem state;
//! canonical transcript and session records live in `mez_agent::transcript`.

use std::path::PathBuf;

use mez_agent::AgentConversationKind;
use mez_agent::transcript::ConversationSummary;
use serde::{Deserialize, Serialize};

/// Read-only health report for the saved-session discovery catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SavedSessionCatalogStatus {
    /// Whether the SQLite database currently exists.
    pub database_exists: bool,
    /// Whether the durable schema-v1 migration marker exists.
    pub migration_complete: bool,
    /// Whether a previous database backup is retained.
    pub backup_exists: bool,
    /// Whether an interrupted rebuild temporary database exists.
    pub rebuild_temporary_exists: bool,
    /// SQLite schema version when the database could be read.
    pub schema_version: Option<i64>,
    /// Number of indexed saved conversations when readable.
    pub indexed_conversations: Option<u64>,
    /// Whether SQLite's bounded integrity check succeeded.
    pub integrity_ok: bool,
    /// Whether the migration/rebuild lock was immediately available.
    pub lock_available: bool,
    /// Number of indexed catalog queries observed by this process.
    pub indexed_queries: u64,
    /// Number of exact UUID repair attempts observed by this process.
    pub exact_repairs: u64,
    /// Number of full catalog rebuilds observed by this process.
    pub rebuilds: u64,
    /// Number of recovery-only full session-root scans observed by this process.
    pub full_scans: u64,
    /// Secret-safe actionable diagnostic for an unreadable catalog.
    pub diagnostic: Option<String>,
}

/// Durable user-assigned metadata for one agent conversation.
///
/// Names are independent of transcript-derived summaries so summary rebuilds
/// cannot discard them and named conversations can exist before their first
/// transcript entry is persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedAgentSession {
    /// Durable conversation identity.
    pub conversation_id: String,
    /// User-assigned display name.
    pub name: String,
    /// Time at which the name was most recently assigned.
    pub named_at_unix_seconds: u64,
    /// Best known working directory when the name was assigned.
    pub directory: Option<String>,
}

/// Saved-session record merged from transcript summary and name metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedAgentSession {
    /// Bounded transcript metadata, synthesized for named zero-entry sessions.
    pub summary: ConversationSummary,
    /// User-assigned display name, when present.
    pub name: Option<String>,
    /// Durable origin classification used by resume discovery filters.
    pub conversation_kind: AgentConversationKind,
    /// Time at which the active payload was archived, when archived.
    pub archived_at_unix_seconds: Option<u64>,
    /// Compressed archive size recorded by the lifecycle transaction.
    pub archive_compressed_bytes: Option<u64>,
    /// Lowercase SHA-256 digest of the installed archive.
    pub archive_sha256: Option<String>,
}

/// Lifecycle partition selected by one saved-session discovery query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "archived discovery is consumed by the dependent archive browser work"
)]
pub enum SavedSessionLifecycleFilter {
    /// Active payload-backed sessions only.
    Active,
    /// Archived sessions only.
    Archived,
}

/// Stable keyset cursor for saved-session catalog ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSessionCursor {
    /// Whether the row belongs to the named-first picker partition.
    pub named: bool,
    /// Most recent durable activity timestamp.
    pub last_created_at_unix_seconds: u64,
    /// First durable activity timestamp used as a deterministic tie-breaker.
    pub first_created_at_unix_seconds: u64,
    /// Durable conversation identity used as the final ordering key.
    pub conversation_id: String,
}

impl SavedSessionCursor {
    /// Builds the cursor corresponding to one saved-session row.
    pub fn from_session(session: &SavedAgentSession) -> Self {
        Self {
            named: session.name.is_some(),
            last_created_at_unix_seconds: session.summary.last_created_at_unix_seconds,
            first_created_at_unix_seconds: session.summary.first_created_at_unix_seconds,
            conversation_id: session.summary.conversation_id.clone(),
        }
    }
}

/// Directional keyset anchor for one bounded saved-session page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SavedSessionPageAnchor {
    /// Return rows ordered after this cursor.
    After(SavedSessionCursor),
    /// Return rows ordered before this cursor.
    Before(SavedSessionCursor),
    /// Return a page ending with this cursor when the row still matches.
    At(SavedSessionCursor),
    /// Return the final bounded page in picker order.
    Last,
}

/// Indexed filters and bounds for one saved-session page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSessionQuery {
    /// Active or archived lifecycle partition.
    pub lifecycle: SavedSessionLifecycleFilter,
    /// Optional exact directory scope.
    pub directory: Option<String>,
    /// Whether delegated child conversations are included.
    pub include_subagents: bool,
    /// Whether rows must contain a latest user prompt.
    pub require_latest_user_prompt: bool,
    /// Optional case-insensitive search across identity and bounded metadata.
    pub search: Option<String>,
    /// Optional forward or backward keyset anchor.
    pub anchor: Option<SavedSessionPageAnchor>,
    /// Maximum rows returned by this query.
    pub limit: usize,
}

/// One bounded page of catalog-backed saved sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSessionPage {
    /// Rows in named-first picker order.
    pub sessions: Vec<SavedAgentSession>,
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
    /// Time-and-count policy applied to active saved conversations.
    pub(super) saved_session_retention: SavedSessionRetentionPolicy,
    /// Cleartext presentation bytes retained before compaction.
    pub(super) presentation_compaction_threshold: u64,
}

/// Time-and-count retention policy for active saved conversations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedSessionRetentionPolicy {
    /// Maximum active payload-backed conversations retained on disk.
    pub max_active_sessions: usize,
    /// Maximum age in days since the latest durable activity.
    pub retention_days: u64,
}

/// One failed deletion observed while enforcing saved-session retention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSessionRetentionFailure {
    /// Durable conversation identity whose deletion failed.
    pub conversation_id: String,
    /// Secret-safe storage failure diagnostic.
    pub error: String,
}

/// Outcome of one age-before-count active saved-session retention pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedSessionRetentionReport {
    /// Conversations deleted in deterministic oldest-first order.
    pub deleted_conversation_ids: Vec<String>,
    /// Candidate deletions that failed while other independent work continued.
    pub failures: Vec<SavedSessionRetentionFailure>,
}
