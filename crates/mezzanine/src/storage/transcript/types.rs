//! Transcript store data types.
//!
//! Presentation replay remains product-owned because validation depends on
//! terminal wrapping policy. The store handle owns configured filesystem state;
//! canonical transcript and session records live in `mez_agent::transcript`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
    /// Whether this user-assigned name is ephemeral in the picker ranking.
    ///
    /// An ephemeral name is still a real name: it renders, resolves, matches
    /// lookups, and wins over a generated title exactly like a durable name.
    /// The flag only removes the row from the named-first partition of the
    /// saved-session picker. Records written before this field existed decode
    /// with the default, so every stored name stays durable and preferred.
    #[serde(default)]
    pub ephemeral: bool,
}

/// Bounded persisted mirror of one conversation's published agent objective.
///
/// The mirror is a display cache written only from the published objective so
/// archived and offline conversations can still resolve a policy-derived title.
/// The published discovery objective remains the source of truth, and a missing
/// mirror degrades to prompt-based rendering instead of failing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionObjectiveMirror {
    /// Durable conversation identity.
    pub conversation_id: String,
    /// Bounded single-line objective title.
    pub objective: String,
    /// Time at which the mirror was most recently refreshed.
    pub updated_at_unix_seconds: u64,
}

/// One write-path objective title mirror index read and its recovery outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SessionObjectiveMirrorWriteRead {
    /// Mirrors read from the index, empty when the index was unreadable.
    pub(super) records: BTreeMap<String, SessionObjectiveMirror>,
    /// Whether the unreadable index was quarantined and must be rewritten.
    pub(super) recovered: bool,
}

/// Bounded persisted mirror of one conversation's generated display title.
///
/// The mirror is stored in its own bounded sidecar rather than in the objective
/// mirror, so the objective mirror keeps its single-writer invariant of being
/// written only from the published objective. It is a display cache: a missing,
/// unreadable, or over-cap index degrades to the prompt-based rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTitleMirror {
    /// Durable conversation identity.
    pub conversation_id: String,
    /// Bounded single-line generated display title.
    pub title: String,
    /// Time at which the title was most recently refreshed.
    pub updated_at_unix_seconds: u64,
}

/// One write-path generated-title mirror index read and its recovery outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SessionTitleMirrorWriteRead {
    /// Mirrors read from the index, empty when the index was unreadable.
    pub(super) records: BTreeMap<String, SessionTitleMirror>,
    /// Whether the unreadable index was quarantined and must be rewritten.
    pub(super) recovered: bool,
}

/// Saved-session record merged from transcript summary and name metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedAgentSession {
    /// Bounded transcript metadata, synthesized for named zero-entry sessions.
    pub summary: ConversationSummary,
    /// User-assigned display name, when present.
    pub name: Option<String>,
    /// Whether the user-assigned name ranks in the picker's preferred partition.
    ///
    /// Catalog readers that do not project the preferred-name rank report the
    /// durable default, and the picker page overwrites this from its own ranked
    /// column so a keyset anchor built from a returned row matches the ordering
    /// expression exactly.
    pub name_preferred: bool,
    /// Bounded persisted mirror of the published agent objective, when cached.
    ///
    /// This is display-only state used to resolve a policy-derived title for
    /// archived and offline conversations. A missing mirror degrades to the
    /// prompt-based rendering rather than failing.
    pub objective_title: Option<String>,
    /// Bounded persisted model-generated display title, when one exists.
    ///
    /// This is display-only state and is used only while the configured title
    /// policy is `generated`. A missing value degrades to the objective-derived
    /// title and then the first prompt, so the row always renders something.
    pub generated_title: Option<String>,
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
    /// Preferred-name rank for the named-first picker partition.
    ///
    /// This is the preferred-name rank rather than the presence of a name: a
    /// user-assigned ephemeral name stays a real name but is ranked here with
    /// unnamed rows. The field name and wire shape are unchanged.
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
            named: session.name_preferred,
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
#[derive(Debug, Clone)]
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
    /// Shared objective title mirror throttle and diagnostics for this handle.
    ///
    /// Store clones share this state so the unchanged-value throttle and its
    /// bounded diagnostics follow one logical store. It is deliberately not part
    /// of store equality: it holds only transient mirror index state.
    pub(super) session_objective_mirrors: Arc<Mutex<SessionObjectiveMirrorHandleState>>,
    /// Shared generated-title mirror throttle and diagnostics for this handle.
    ///
    /// This mirrors the objective mirror state shape, but it tracks the separate
    /// generated-title sidecar and additionally retains the bounded tombstones
    /// that keep a late settle from re-inserting a deleted conversation. Like the
    /// objective mirror it is deliberately not part of store equality.
    pub(super) session_title_mirrors: Arc<Mutex<SessionTitleMirrorHandleState>>,
    /// Maximum generated-title mirror entries retained before compaction.
    ///
    /// Production stores use `SESSION_TITLE_MIRRORS_MAX_ENTRIES`; focused tests
    /// lower it so bounded compaction is observable without thousands of writes.
    pub(super) session_title_mirror_max_entries: usize,
}

impl PartialEq for AgentTranscriptStore {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root
            && self.saved_session_retention == other.saved_session_retention
            && self.presentation_compaction_threshold == other.presentation_compaction_threshold
    }
}

impl Eq for AgentTranscriptStore {}

/// Transient mirror throttle and bounded diagnostics for one store handle.
///
/// The last persisted `(conversation_id, objective)` pair lets an unchanged
/// refresh skip reading the bounded index, and the counters make mirror index
/// reads, writes, and recoveries observable without exposing mirror content.
#[derive(Debug, Default)]
pub(super) struct SessionObjectiveMirrorHandleState {
    /// Last `(conversation_id, bounded objective)` this handle persisted.
    pub(super) last_mirrored: Option<(String, String)>,
    /// Count of mirror index reads performed by this handle.
    pub(super) index_reads: u64,
    /// Count of mirror index writes performed by this handle.
    pub(super) index_writes: u64,
    /// Count of unreadable mirror indices quarantined and rebuilt.
    pub(super) recoveries: u64,
    /// Bounded reason recorded for the most recent quarantine.
    pub(super) last_recovery_reason: Option<String>,
}

/// Bounded diagnostics for one handle's persisted objective title mirror index.
///
/// The report carries counts and one bounded reason only: it never contains
/// mirror content or conversation identifiers. The counters are per-process,
/// while `quarantined_index` reports the durable artifact an operator can find.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SessionObjectiveMirrorStatus {
    /// Mirror index reads performed by this store handle.
    pub index_reads: u64,
    /// Mirror index writes performed by this store handle.
    pub index_writes: u64,
    /// Unreadable mirror indices quarantined and rebuilt by this handle.
    pub recoveries: u64,
    /// Bounded reason recorded for the most recent quarantine.
    pub last_recovery_reason: Option<String>,
    /// Whether one quarantined unreadable index file is retained on disk.
    pub quarantined_index: bool,
}

/// Transient mirror throttle, delete tombstones, and diagnostics for one store handle.
///
/// The last persisted `(conversation_id, title)` pair lets an unchanged refresh
/// skip reading the bounded index, `deleted_conversations` keeps a late settle
/// from re-inserting a conversation that was deleted, and the counters make index
/// reads, writes, and recoveries observable without exposing mirror content.
#[derive(Debug, Default)]
pub(super) struct SessionTitleMirrorHandleState {
    /// Last `(conversation_id, bounded title)` this handle persisted.
    pub(super) last_mirrored: Option<(String, String)>,
    /// Count of title index reads performed by this handle.
    pub(super) index_reads: u64,
    /// Count of title index writes performed by this handle.
    pub(super) index_writes: u64,
    /// Count of unreadable title indices quarantined and rebuilt.
    pub(super) recoveries: u64,
    /// Bounded reason recorded for the most recent quarantine.
    pub(super) last_recovery_reason: Option<String>,
    /// Conversations deleted through this handle, newest last and bounded.
    ///
    /// A worker that started before a conversation was deleted can still settle
    /// afterwards. Remembering the deletion keeps that late settle from writing a
    /// title row for a conversation that no longer exists.
    pub(super) deleted_conversations: Vec<String>,
}

/// Bounded diagnostics for one handle's persisted generated-title mirror index.
///
/// The report carries counts and one bounded reason only: it never contains
/// mirror content or conversation identifiers. The counters are per-process,
/// while `quarantined_index` reports the durable artifact an operator can find.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SessionTitleMirrorStatus {
    /// Title index reads performed by this store handle.
    pub index_reads: u64,
    /// Title index writes performed by this store handle.
    pub index_writes: u64,
    /// Unreadable title indices quarantined and rebuilt by this handle.
    pub recoveries: u64,
    /// Bounded reason recorded for the most recent quarantine.
    pub last_recovery_reason: Option<String>,
    /// Whether one quarantined unreadable index file is retained on disk.
    pub quarantined_index: bool,
}

/// Bounded answer to whether one conversation still needs a generated title.
///
/// The probe reports only the two stored reasons a request is pointless. A failed
/// probe is an error instead, so a read failure can never be mistaken for a manual
/// name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionTitleGenerationProbe {
    /// Whether a durable user-assigned name already wins over any title.
    pub has_manual_name: bool,
    /// Whether a generated title is already stored for this conversation.
    pub has_stored_generated_title: bool,
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
