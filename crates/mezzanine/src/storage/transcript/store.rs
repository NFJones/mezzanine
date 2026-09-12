//! Filesystem-backed transcript store operations.
//!
//! Store methods validate conversation ids, enforce private storage
//! permissions, and use append-only TSV records for inspectable persistence.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self as std_fs, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use tokio::fs::{self as tokio_fs, OpenOptions as TokioOpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{MezError, MezErrorKind, Result};
use crate::session_title::{bound_session_title, is_display_format_character};

use super::CompareAndSwapTranscriptEntryResult;
use super::archive::{
    ArchiveRecoveryOperation, archived_catalog_candidate, archived_catalog_candidates,
    archived_payloads_exist,
};
use super::catalog::{self, CatalogCandidate, CatalogPayloadLayout};
use super::encoding::{
    decode_agent_session_metadata, decode_structured_prompt_history_entry, decode_transcript_entry,
    encode_agent_session_metadata, encode_structured_prompt_history_entry, encode_transcript_entry,
};
use super::fs::{
    set_private_dir_permissions, set_private_dir_permissions_async, set_private_file_permissions,
    set_private_file_permissions_async,
};
use super::types::{
    AgentPresentationEntry, AgentTranscriptStore, NamedAgentSession, SavedAgentSession,
    SavedSessionCatalogStatus, SavedSessionPage, SavedSessionQuery, SavedSessionRetentionFailure,
    SavedSessionRetentionPolicy, SavedSessionRetentionReport, SessionObjectiveMirror,
    SessionObjectiveMirrorHandleState, SessionObjectiveMirrorStatus,
    SessionObjectiveMirrorWriteRead, SessionTitleGenerationProbe, SessionTitleMirror,
    SessionTitleMirrorHandleState, SessionTitleMirrorStatus, SessionTitleMirrorWriteRead,
};
use mez_agent::transcript::{
    AgentSessionMetadata, ConversationSummary, TranscriptEntry, TranscriptRole,
    bounded_summary_text, summarize_conversation, validate_conversation_id,
};
use mez_agent::{AgentConversationKind, AllowedActionSet};
use mez_mux::readline::ReadlineHistoryEntry;

/// Defines the SESSION TRANSCRIPT FILE NAME const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const SESSION_TRANSCRIPT_FILE_NAME: &str = "history.tsv";
/// Defines the bounded conversation summary sidecar file name for this subsystem.
///
/// The file stores one JSON object with list/resume metadata so saved-session
/// pickers do not need to decode full transcript histories.
const SESSION_SUMMARY_FILE_NAME: &str = "summary.json";
/// Defines the versioned durable conversation classification sidecar.
const SESSION_METADATA_FILE_NAME: &str = "metadata.json";
/// Current per-conversation metadata schema version.
const SESSION_METADATA_VERSION: u64 = 3;

/// Versioned authoritative metadata for one durable conversation.
///
/// The sidecar is the only durable source for user-selected objectives. It is
/// updated under the conversation lock so independent kind and objective
/// mutations cannot overwrite one another.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct ConversationMetadata {
    version: u64,
    conversation_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_objective: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_objective: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_conversation_id: Option<String>,
    #[serde(default)]
    subagent_lifetime: mez_agent::SubagentLifetime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subagent_scope: Option<mez_agent::SubagentScopeDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    allowed_actions: Option<AllowedActionSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subagent_lineage: Option<mez_agent::SubagentSessionLineage>,
}

/// Rejects incomplete child sidecars while preserving true legacy root
/// conversations, which predate every child-contract field.
fn validate_conversation_metadata_contract(metadata: &ConversationMetadata) -> Result<()> {
    match metadata.conversation_kind.as_str() {
        "root" => {
            if metadata.subagent_lineage.is_some()
                || metadata.parent_objective.is_some()
                || metadata.parent_conversation_id.is_some()
                || metadata.subagent_lifetime != mez_agent::SubagentLifetime::Task
                || metadata.subagent_scope.is_some()
            {
                return Err(MezError::invalid_args(
                    "root conversation metadata cannot contain subagent lifecycle metadata",
                ));
            }
            if let Some(allowed_actions) = metadata.allowed_actions.as_ref() {
                allowed_actions
                    .validate_persisted()
                    .map_err(MezError::invalid_args)?;
            }
        }
        "subagent" => {
            if metadata.version == 1
                && (metadata.subagent_lineage.is_none() || metadata.allowed_actions.is_none())
            {
                return Ok(());
            }
            let lineage = metadata.subagent_lineage.as_ref().ok_or_else(|| {
                MezError::invalid_state(
                    "subagent conversation cannot restore without durable lineage",
                )
            })?;
            lineage
                .validate_persisted()
                .map_err(MezError::invalid_args)?;
            let allowed_actions = metadata.allowed_actions.as_ref().ok_or_else(|| {
                MezError::invalid_state(
                    "subagent conversation cannot restore without durable action catalog",
                )
            })?;
            allowed_actions
                .validate_persisted()
                .map_err(MezError::invalid_args)?;
            if lineage.terminal && allowed_actions.contains(mez_agent::AllowedAction::SpawnAgent) {
                return Err(MezError::invalid_args(
                    "terminal subagent conversation metadata cannot contain spawn_agent",
                ));
            }
            if metadata.subagent_lifetime == mez_agent::SubagentLifetime::Persistent {
                let objective = metadata.parent_objective.as_deref().ok_or_else(|| {
                    MezError::invalid_state(
                        "persistent subagent conversation requires a parent objective",
                    )
                })?;
                mez_agent::messaging::normalize_objective(objective)?;
                let parent_conversation_id =
                    metadata.parent_conversation_id.as_deref().ok_or_else(|| {
                        MezError::invalid_state(
                            "persistent subagent conversation requires a parent conversation",
                        )
                    })?;
                validate_conversation_id(parent_conversation_id)?;
                if metadata.subagent_scope.is_none() {
                    return Err(MezError::invalid_state(
                        "persistent subagent conversation requires a durable scope declaration",
                    ));
                }
            } else if metadata.subagent_scope.is_some() {
                return Err(MezError::invalid_state(
                    "task subagent conversation cannot contain persistent scope metadata",
                ));
            }
        }
        _ => return Err(MezError::invalid_args("invalid conversation metadata kind")),
    }
    Ok(())
}

/// Records a root TSV move that can be rolled back before metadata commits.
///
/// The state is created immediately after the rename. This keeps the legacy
/// payload recoverable when a subsequent permission update fails.
struct LegacyTranscriptPromotion {
    legacy_path: PathBuf,
    transcript_path: PathBuf,
}
/// Defines the SESSION PRESENTATION FILE NAME const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const SESSION_PRESENTATION_FILE_NAME: &str = "presentation.tsv";
/// Defines the presentation sequence index file name for this subsystem.
///
/// The file stores the latest durable presentation sequence so new appends can
/// allocate the next sequence without replaying compressed presentation history.
const SESSION_PRESENTATION_INDEX_FILE_NAME: &str = "presentation-index.tsv";
/// Defines the compressed presentation history file name for this subsystem.
///
/// The file is append-only and may contain any number of concatenated zstd
/// frames. The active cleartext tail remains in `presentation.tsv`.
const SESSION_PRESENTATION_COMPRESSED_FILE_NAME: &str = "presentation.tsv.zst";
/// Defines the shared agent prompt-history file name used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const SHARED_PROMPT_HISTORY_FILE_NAME: &str = "prompt-history.tsv";
/// Advisory lock serializing shared prompt-history migration and mutation.
const SHARED_PROMPT_HISTORY_LOCK_FILE_NAME: &str = ".prompt-history.tsv.lock";
/// Marker recording completion of the per-conversation history import.
const SHARED_PROMPT_HISTORY_MIGRATION_FILE_NAME: &str = ".prompt-history-shared-v1";
/// Defines the SHARED COMMAND PROMPT HISTORY FILE NAME const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const SHARED_COMMAND_PROMPT_HISTORY_FILE_NAME: &str = "command-prompt-history.tsv";
/// Defines the ACTIVE AGENT SESSION METADATA FILE NAME const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const ACTIVE_AGENT_SESSION_METADATA_FILE_NAME: &str = "active-agent-sessions.tsv";
/// Root-owned TSV files that are not legacy conversation transcripts.
///
/// Their stems satisfy the conversation-id grammar, so migration and exact
/// lookup must reserve them explicitly instead of classifying by extension.
const ROOT_CONTROL_TSV_FILE_NAMES: [&str; 3] = [
    SHARED_PROMPT_HISTORY_FILE_NAME,
    SHARED_COMMAND_PROMPT_HISTORY_FILE_NAME,
    ACTIVE_AGENT_SESSION_METADATA_FILE_NAME,
];
/// Versioned root-level index containing durable user-assigned session names.
const NAMED_AGENT_SESSIONS_FILE_NAME: &str = "named-sessions.json";
/// Advisory lock serializing named-session index updates.
const NAMED_AGENT_SESSIONS_LOCK_FILE_NAME: &str = ".named-sessions.json.lock";
/// Current durable named-session index schema version.
const NAMED_AGENT_SESSIONS_VERSION: u64 = 1;
/// Versioned root-level index containing bounded objective title mirrors.
const SESSION_OBJECTIVE_MIRRORS_FILE_NAME: &str = "session-objectives.json";
/// Advisory lock serializing objective title mirror index updates.
const SESSION_OBJECTIVE_MIRRORS_LOCK_FILE_NAME: &str = ".session-objectives.json.lock";
/// Atomic-replacement temporary file for the objective title mirror index.
const SESSION_OBJECTIVE_MIRRORS_TEMP_FILE_NAME: &str = ".session-objectives.json.tmp";
/// Current durable objective title mirror index schema version.
const SESSION_OBJECTIVE_MIRRORS_VERSION: u64 = 1;
/// Maximum accepted objective title mirror index size in bytes.
pub(super) const SESSION_OBJECTIVE_MIRRORS_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Bounded quarantine name for one unreadable objective title mirror index.
const SESSION_OBJECTIVE_MIRRORS_QUARANTINE_FILE_NAME: &str = ".session-objectives.json.unreadable";
/// Maximum number of objective title mirrors retained in the persisted index.
///
/// The index is a display cache: at this bound each new mirror write drops the
/// oldest retained mirrors, so growth from conversations that never enter the
/// catalog stays bounded. The bound comfortably covers a typical retained
/// session set while keeping the encoded index far below the byte bound (about
/// 600 KiB at this count for bounded objectives), so the byte bound stays a
/// guard rather than the trigger that disables mirroring.
pub(super) const SESSION_OBJECTIVE_MIRRORS_MAX_ENTRIES: usize = 4_096;
/// Maximum accepted length of one bounded mirror recovery diagnostic.
const SESSION_OBJECTIVE_MIRROR_RECOVERY_REASON_MAX_CHARS: usize = 160;
/// Versioned root-level index containing bounded generated session titles.
///
/// Generated titles live in their own sidecar so the objective mirror keeps its
/// single-writer invariant of being written only from the published objective.
const SESSION_TITLE_MIRRORS_FILE_NAME: &str = "session-titles.json";
/// Advisory lock serializing generated-title mirror index updates.
const SESSION_TITLE_MIRRORS_LOCK_FILE_NAME: &str = ".session-titles.json.lock";
/// Atomic-replacement temporary file for the generated-title mirror index.
const SESSION_TITLE_MIRRORS_TEMP_FILE_NAME: &str = ".session-titles.json.tmp";
/// Current durable generated-title mirror index schema version.
const SESSION_TITLE_MIRRORS_VERSION: u64 = 1;
/// Maximum accepted generated-title mirror index size in bytes.
pub(super) const SESSION_TITLE_MIRRORS_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Bounded quarantine name for one unreadable generated-title mirror index.
const SESSION_TITLE_MIRRORS_QUARANTINE_FILE_NAME: &str = ".session-titles.json.unreadable";
/// Maximum number of generated-title mirrors retained in the persisted index.
///
/// The index is a display cache: at this bound each new title write drops the
/// oldest retained titles, so growth from conversations that never enter the
/// catalog stays bounded. The byte bound stays a read-side guard.
pub(super) const SESSION_TITLE_MIRRORS_MAX_ENTRIES: usize = 4_096;
/// Maximum accepted length of one bounded generated-title recovery diagnostic.
const SESSION_TITLE_MIRROR_RECOVERY_REASON_MAX_CHARS: usize = 160;
/// Maximum conversations one handle remembers as deleted.
///
/// The tombstones only protect a late settle from re-inserting a title for a
/// conversation deleted in this process, so a bounded newest-last list is
/// enough and keeps the per-handle state small.
const SESSION_TITLE_MIRROR_DELETED_MAX_ENTRIES: usize = 1_024;
/// Maximum accepted session-name length in Unicode scalar values.
const MAX_AGENT_SESSION_NAME_CHARS: usize = crate::session_title::MAX_SESSION_TITLE_CHARS;
/// Defines the DEFAULT AGENT PROMPT HISTORY LIMIT const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const DEFAULT_AGENT_PROMPT_HISTORY_LIMIT: usize = 1000;
/// Maximum on-disk bytes allowed before prompt history is compacted.
pub(super) const PROMPT_HISTORY_COMPACTION_BYTES: u64 =
    (mez_mux::readline::MAX_READLINE_HISTORY_BYTES * 2 + DEFAULT_AGENT_PROMPT_HISTORY_LIMIT * 64)
        as u64;
/// Maximum encoded tail needed to recover one accepted prompt-history row.
const PROMPT_HISTORY_TAIL_READ_BYTES: u64 =
    (mez_mux::readline::MAX_READLINE_HISTORY_ENTRY_BYTES * 2 + 128) as u64;
/// Defines the DEFAULT TRANSCRIPT TAIL READ BYTES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const DEFAULT_TRANSCRIPT_TAIL_READ_BYTES: u64 = 2 * 1024 * 1024;
/// Defines the default presentation tail read used for bounded replay/index fallback.
///
/// This caps resume presentation replay and legacy index recovery to a recent
/// cleartext tail rather than decoding compressed historical presentation rows.
const DEFAULT_PRESENTATION_TAIL_READ_BYTES: u64 = 2 * 1024 * 1024;
/// Defines the cleartext presentation tail size that triggers compression.
///
/// Keeping recent rows cleartext makes ordinary appends simple, while moving
/// larger historical tails into concatenated zstd frames bounds disk usage.
pub(super) const PRESENTATION_CLEAR_TAIL_COMPACT_BYTES: u64 = 256 * 1024;

/// Maximum saved agent conversations retained by default for `/resume`.
pub const DEFAULT_SAVED_AGENT_SESSION_LIMIT: usize = 10_000;
/// Maximum age of an active saved conversation since its latest durable activity.
pub const DEFAULT_SAVED_AGENT_SESSION_RETENTION_DAYS: u64 = 90;

/// Returns the built-in active saved-session retention policy.
const fn default_saved_session_retention_policy() -> SavedSessionRetentionPolicy {
    SavedSessionRetentionPolicy {
        max_active_sessions: DEFAULT_SAVED_AGENT_SESSION_LIMIT,
        retention_days: DEFAULT_SAVED_AGENT_SESSION_RETENTION_DAYS,
    }
}

impl AgentTranscriptStore {
    /// Creates a store under the standard config-root agent-session directory.
    pub fn under_config_root(config_root: impl Into<PathBuf>) -> Self {
        Self {
            root: config_root.into().join("agent-sessions"),
            saved_session_retention: default_saved_session_retention_policy(),
            presentation_compaction_threshold: PRESENTATION_CLEAR_TAIL_COMPACT_BYTES,
            session_objective_mirrors: Arc::new(Mutex::new(
                SessionObjectiveMirrorHandleState::default(),
            )),
            session_title_mirrors: Arc::new(Mutex::new(SessionTitleMirrorHandleState::default())),
            session_title_mirror_max_entries: SESSION_TITLE_MIRRORS_MAX_ENTRIES,
            #[cfg(test)]
            fail_metadata_write_after_promotion: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            fail_legacy_promotion_permissions_after_rename: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            fail_archive_recovery_journal_removal: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            fail_agent_session_metadata_write: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            fail_subagent_contract_catalog_upsert: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            fail_user_objective_read_countdown: Arc::new(AtomicU8::new(0)),
        }
    }

    /// Creates a store rooted at a specific directory.
    #[cfg(test)]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            saved_session_retention: default_saved_session_retention_policy(),
            presentation_compaction_threshold: PRESENTATION_CLEAR_TAIL_COMPACT_BYTES,
            session_objective_mirrors: Arc::new(Mutex::new(
                SessionObjectiveMirrorHandleState::default(),
            )),
            session_title_mirrors: Arc::new(Mutex::new(SessionTitleMirrorHandleState::default())),
            session_title_mirror_max_entries: SESSION_TITLE_MIRRORS_MAX_ENTRIES,
            fail_metadata_write_after_promotion: Arc::new(AtomicBool::new(false)),
            fail_legacy_promotion_permissions_after_rename: Arc::new(AtomicBool::new(false)),
            fail_archive_recovery_journal_removal: Arc::new(AtomicBool::new(false)),
            fail_agent_session_metadata_write: Arc::new(AtomicBool::new(false)),
            fail_subagent_contract_catalog_upsert: Arc::new(AtomicBool::new(false)),
            fail_user_objective_read_countdown: Arc::new(AtomicU8::new(0)),
        }
    }

    /// Returns this test store with a smaller presentation compaction threshold.
    #[cfg(test)]
    pub fn with_presentation_compaction_threshold(mut self, threshold: u64) -> Result<Self> {
        if threshold == 0 {
            return Err(MezError::invalid_args(
                "presentation compaction threshold must be greater than zero",
            ));
        }
        self.presentation_compaction_threshold = threshold;
        Ok(self)
    }

    /// Returns this test store with a smaller generated-title mirror cap.
    ///
    /// The production cap is thousands of entries; focused tests lower it so
    /// bounded compaction is observable without thousands of writes.
    #[cfg(test)]
    pub fn with_session_title_mirror_max_entries(mut self, max_entries: usize) -> Result<Self> {
        if max_entries == 0 {
            return Err(MezError::invalid_args(
                "session title mirror cap must be greater than zero",
            ));
        }
        self.session_title_mirror_max_entries = max_entries;
        Ok(self)
    }

    /// Causes the next metadata write after legacy promotion to fail in focused tests.
    #[cfg(test)]
    pub fn fail_next_metadata_write_after_promotion(&self) {
        self.fail_metadata_write_after_promotion
            .store(true, Ordering::SeqCst);
    }

    /// Causes the next promoted transcript permission update to fail after rename.
    #[cfg(test)]
    pub fn fail_next_legacy_promotion_permissions_after_rename(&self) {
        self.fail_legacy_promotion_permissions_after_rename
            .store(true, Ordering::SeqCst);
    }

    /// Causes the next archive recovery-journal cleanup to fail in focused tests.
    #[cfg(test)]
    pub fn fail_next_archive_recovery_journal_removal(&self) {
        self.fail_archive_recovery_journal_removal
            .store(true, Ordering::SeqCst);
    }

    /// Causes the next agent-session metadata replacement to fail in focused tests.
    #[cfg(test)]
    pub fn fail_next_agent_session_metadata_write(&self) {
        self.fail_agent_session_metadata_write
            .store(true, Ordering::SeqCst);
    }

    /// Causes the next child-contract catalog upsert to fail after metadata commits.
    #[cfg(test)]
    pub fn fail_next_subagent_contract_catalog_upsert(&self) {
        self.fail_subagent_contract_catalog_upsert
            .store(true, Ordering::SeqCst);
    }

    /// Causes the second subsequent objective metadata read to fail in focused tests.
    #[cfg(test)]
    pub fn fail_second_subsequent_user_objective_read(&self) {
        self.fail_user_objective_read_countdown
            .store(2, Ordering::SeqCst);
    }

    /// Reports the remaining objective-read failure countdown in focused tests.
    #[cfg(test)]
    pub fn user_objective_read_failure_countdown(&self) -> u8 {
        self.fail_user_objective_read_countdown
            .load(Ordering::SeqCst)
    }

    /// Atomically updates the active saved-conversation retention policy.
    pub fn set_saved_session_retention_policy(
        &mut self,
        policy: SavedSessionRetentionPolicy,
    ) -> Result<()> {
        if policy.max_active_sessions == 0 {
            return Err(MezError::invalid_args(
                "saved agent session limit must be greater than zero",
            ));
        }
        if policy.retention_days == 0 {
            return Err(MezError::invalid_args(
                "saved agent session retention days must be greater than zero",
            ));
        }
        self.saved_session_retention = policy;
        Ok(())
    }

    /// Returns the configured active saved-session retention policy.
    pub fn saved_session_retention_policy(&self) -> SavedSessionRetentionPolicy {
        self.saved_session_retention
    }

    /// Returns the root directory used by this store.
    #[cfg(test)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Initializes and validates the rebuildable saved-conversation catalog.
    ///
    /// The first initialization imports existing filesystem metadata while
    /// retaining every transcript, presentation file, and sidecar in place.
    pub fn initialize(&self, now_unix_seconds: u64) -> Result<()> {
        catalog::initialize(self, now_unix_seconds)?;
        self.recover_archive_transactions()
    }

    /// Reconstructs the saved-conversation catalog from retained session files.
    pub fn rebuild_catalog(&self, now_unix_seconds: u64) -> Result<()> {
        catalog::rebuild(self, now_unix_seconds)?;
        self.recover_archive_transactions()
    }

    /// Returns bounded, read-only saved-session catalog health information.
    pub fn catalog_status(&self) -> SavedSessionCatalogStatus {
        catalog::status(self)
    }

    /// Returns the catalog database path for focused tests.
    #[cfg(test)]
    pub fn catalog_path(&self) -> PathBuf {
        catalog::catalog_path(self)
    }

    /// Loads one exact saved session, repairing catalog divergence from its files.
    pub fn saved_session(&self, conversation_id: &str) -> Result<Option<SavedAgentSession>> {
        validate_conversation_id(conversation_id)?;
        if self.is_unrestorable_legacy_subagent(conversation_id)? {
            catalog::delete(self, conversation_id)?;
            return Ok(None);
        }
        let mut session = match catalog::record(self, conversation_id)? {
            Some(record) if self.catalog_record_payloads_exist(conversation_id, &record)? => {
                record.session
            }
            _ => {
                catalog::note_exact_repair();
                self.upsert_catalog_from_files(conversation_id, None)?;
                let Some(record) = catalog::record(self, conversation_id)? else {
                    return Ok(None);
                };
                record.session
            }
        };
        self.attach_session_objective_titles(std::slice::from_mut(&mut session));
        Ok(Some(session))
    }

    /// Loads the most recently active root conversation through the catalog.
    pub fn latest_root_session(&self) -> Result<Option<SavedAgentSession>> {
        while let Some(record) = catalog::latest_root_record(self)? {
            let conversation_id = record.session.summary.conversation_id.clone();
            match self.saved_session(&conversation_id)? {
                Some(session) if session.conversation_kind == AgentConversationKind::Root => {
                    return Ok(Some(session));
                }
                Some(_) | None => {}
            }
        }
        Ok(None)
    }

    /// Returns bounded root-session completion rows for one UUID prefix.
    pub fn root_session_completions(
        &self,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<SavedAgentSession>> {
        let mut sessions = catalog::root_session_completions(self, prefix, limit)?;
        self.attach_session_objective_titles(&mut sessions);
        Ok(sessions)
    }

    /// Returns one bounded keyset page for saved-session browsing.
    ///
    /// Persisted objective title mirrors are attached after the indexed page is
    /// read: they are display metadata only, so ordering, filtering, and
    /// pagination stay entirely catalog-defined.
    pub fn query_saved_sessions(&self, query: &SavedSessionQuery) -> Result<SavedSessionPage> {
        let mut page = catalog::query_saved_sessions(self, query)?;
        self.attach_session_objective_titles(&mut page.sessions);
        Ok(page)
    }

    /// Loads one exact saved-session record from the catalog for focused tests.
    #[cfg(test)]
    pub fn catalog_saved_session(
        &self,
        conversation_id: &str,
    ) -> Result<Option<SavedAgentSession>> {
        Ok(catalog::record(self, conversation_id)?.map(|record| record.session))
    }

    /// Verifies the payload files promised by one catalog row still exist.
    fn catalog_record_payloads_exist(
        &self,
        conversation_id: &str,
        record: &super::catalog::CatalogRecord,
    ) -> Result<bool> {
        if record.session.archived_at_unix_seconds.is_some() {
            return archived_payloads_exist(self, conversation_id);
        }
        let transcript_exists = if !record.has_transcript {
            true
        } else {
            match record.payload_layout {
                CatalogPayloadLayout::Directory => {
                    self.transcript_path_for(conversation_id)?.is_file()
                }
                CatalogPayloadLayout::LegacyTsv => {
                    self.legacy_transcript_path_for(conversation_id)?.is_file()
                }
            }
        };
        let presentation_exists = !record.has_presentation
            || self.presentation_path_for(conversation_id)?.is_file()
            || self
                .presentation_compressed_path_for(conversation_id)?
                .is_file();
        Ok(transcript_exists && presentation_exists)
    }

    /// Reconstructs and upserts one exact catalog row from retained files.
    ///
    /// This is the payload-first synchronization boundary used after ordinary
    /// session mutations and for lazy exact-lookup repair. It never enumerates
    /// the session root.
    pub(super) fn upsert_catalog_from_files(
        &self,
        conversation_id: &str,
        kind_override: Option<AgentConversationKind>,
    ) -> Result<bool> {
        let Some(mut candidate) = self.catalog_candidate_for_conversation(conversation_id)? else {
            catalog::delete(self, conversation_id)?;
            return Ok(false);
        };
        if let Some(kind) = kind_override {
            candidate.conversation_kind = kind;
        }
        let catalog_updated_at = candidate
            .named_at_unix_seconds
            .unwrap_or(candidate.summary.last_created_at_unix_seconds)
            .max(candidate.summary.last_created_at_unix_seconds);
        catalog::upsert(self, &candidate, catalog_updated_at)?;
        Ok(true)
    }

    /// Reconstructs one exact candidate without scanning sibling sessions.
    fn catalog_candidate_for_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<CatalogCandidate>> {
        validate_conversation_id(conversation_id)?;
        let existing = catalog::record(self, conversation_id)?;
        let named = self.read_named_sessions_index()?.remove(conversation_id);
        let session_dir = self.session_dir_for(conversation_id)?;
        let directory_transcript = session_dir.join(SESSION_TRANSCRIPT_FILE_NAME).is_file();
        let legacy_transcript = self.legacy_transcript_path_for(conversation_id)?.is_file();
        let has_transcript = directory_transcript || legacy_transcript;
        let has_presentation = session_dir.join(SESSION_PRESENTATION_FILE_NAME).is_file()
            || session_dir
                .join(SESSION_PRESENTATION_COMPRESSED_FILE_NAME)
                .is_file();
        if !has_transcript && !has_presentation {
            if let Some(candidate) = archived_catalog_candidate(self, conversation_id)? {
                return Ok(Some(candidate));
            }
            if self.user_objective(conversation_id)?.is_some() {
                return self.objective_only_catalog_candidate(conversation_id, named.as_ref());
            }
            return named
                .as_ref()
                .map(|named| self.named_only_catalog_candidate(named))
                .transpose();
        }
        let candidate = self.catalog_candidate_for_payload(
            conversation_id,
            has_transcript,
            has_presentation,
            if directory_transcript || !has_transcript {
                CatalogPayloadLayout::Directory
            } else {
                CatalogPayloadLayout::LegacyTsv
            },
            named.as_ref(),
        )?;
        if candidate.is_some() {
            return Ok(candidate);
        }
        let Some(existing) = existing.filter(|record| record.session.name.is_some()) else {
            return Ok(None);
        };
        let mut summary = existing.session.summary;
        summary.entries = 0;
        Ok(Some(CatalogCandidate {
            summary,
            name: None,
            named_at_unix_seconds: None,
            name_preferred: true,
            conversation_kind: existing.session.conversation_kind,
            has_transcript,
            has_presentation,
            payload_layout: if directory_transcript || !has_transcript {
                CatalogPayloadLayout::Directory
            } else {
                CatalogPayloadLayout::LegacyTsv
            },
            archived_at_unix_seconds: None,
            archive_compressed_bytes: None,
            archive_sha256: None,
        }))
    }

    /// Appends one validated transcript entry to its conversation file.
    ///
    /// Creates the store root when needed, updates private permissions, and
    /// syncs the file before returning.
    pub fn append(&self, entry: &TranscriptEntry) -> Result<()> {
        entry.validate()?;
        let _conversation_lock = self.acquire_conversation_lock(&entry.conversation_id)?;
        self.append_one_locked(entry)?;
        Ok(())
    }

    /// Appends multiple validated transcript entries and returns bytes written.
    ///
    /// This preserves the same per-entry durability behavior as `append` while
    /// giving async persistence workers a single call that can report a useful
    /// byte count after executing off the runtime actor.
    pub fn append_many(&self, entries: &[TranscriptEntry]) -> Result<usize> {
        let mut grouped = BTreeMap::<String, Vec<&TranscriptEntry>>::new();
        for entry in entries {
            entry.validate()?;
            grouped
                .entry(entry.conversation_id.clone())
                .or_default()
                .push(entry);
        }
        let mut bytes = 0usize;
        for (conversation_id, entries) in grouped {
            let _conversation_lock = self.acquire_conversation_lock(&conversation_id)?;
            for entry in entries {
                bytes = bytes.saturating_add(self.append_one_locked(entry)?);
            }
        }
        Ok(bytes)
    }

    /// Atomically captures the immutable durable contract for one child conversation.
    pub fn save_subagent_conversation_contract(
        &self,
        conversation_id: &str,
        lineage: mez_agent::SubagentSessionLineage,
        allowed_actions: AllowedActionSet,
    ) -> Result<()> {
        self.save_subagent_conversation_contract_with_lifecycle(
            conversation_id,
            lineage,
            allowed_actions,
            None,
        )
    }

    /// Atomically captures a reusable child contract and its parent objective.
    pub fn save_persistent_subagent_conversation_contract(
        &self,
        conversation_id: &str,
        lineage: mez_agent::SubagentSessionLineage,
        allowed_actions: AllowedActionSet,
        parent_conversation_id: &str,
        objective: &str,
        scope: mez_agent::SubagentScopeDeclaration,
    ) -> Result<()> {
        validate_conversation_id(parent_conversation_id)?;
        let objective = mez_agent::messaging::normalize_objective(objective)?;
        self.save_subagent_conversation_contract_with_lifecycle(
            conversation_id,
            lineage,
            allowed_actions,
            Some((parent_conversation_id, objective.as_str(), scope)),
        )
    }

    /// Owns the single-lock child metadata transaction for both lifetimes.
    fn save_subagent_conversation_contract_with_lifecycle(
        &self,
        conversation_id: &str,
        lineage: mez_agent::SubagentSessionLineage,
        allowed_actions: AllowedActionSet,
        persistent: Option<(&str, &str, mez_agent::SubagentScopeDeclaration)>,
    ) -> Result<()> {
        lineage
            .validate_persisted()
            .map_err(MezError::invalid_args)?;
        allowed_actions
            .validate_persisted()
            .map_err(MezError::invalid_args)?;
        if lineage.terminal && allowed_actions.contains(mez_agent::AllowedAction::SpawnAgent) {
            return Err(MezError::invalid_args(
                "terminal subagent conversation metadata cannot contain spawn_agent",
            ));
        }
        let _conversation_lock = self.acquire_conversation_lock(conversation_id)?;
        let metadata_path = self
            .session_dir_for(conversation_id)?
            .join(SESSION_METADATA_FILE_NAME);
        let previous_metadata = metadata_path
            .exists()
            .then(|| std_fs::read(&metadata_path))
            .transpose()?;
        let mut metadata = self.read_conversation_metadata_locked(conversation_id)?;
        let (lifetime, parent_conversation_id, parent_objective, subagent_scope) = persistent
            .map_or(
                (mez_agent::SubagentLifetime::Task, None, None, None),
                |(parent_conversation_id, objective, scope)| {
                    (
                        mez_agent::SubagentLifetime::Persistent,
                        Some(parent_conversation_id.to_string()),
                        Some(objective.to_string()),
                        Some(scope),
                    )
                },
            );
        match (
            metadata.conversation_kind.as_str(),
            metadata.subagent_lineage.as_ref(),
            metadata.allowed_actions.as_ref(),
        ) {
            ("root", None, None) => {
                metadata.conversation_kind = "subagent".to_string();
                metadata.subagent_lineage = Some(lineage);
                metadata.allowed_actions = Some(allowed_actions);
                metadata.subagent_lifetime = lifetime;
                metadata.parent_conversation_id = parent_conversation_id;
                metadata.parent_objective = parent_objective;
                metadata.subagent_scope = subagent_scope;
            }
            ("subagent", Some(current_lineage), Some(current_actions))
                if current_lineage == &lineage
                    && current_actions == &allowed_actions
                    && metadata.subagent_lifetime == lifetime
                    && metadata.parent_conversation_id == parent_conversation_id
                    && metadata.parent_objective == parent_objective
                    && metadata.subagent_scope == subagent_scope =>
            {
                return Ok(());
            }
            ("subagent", _, _) => {
                return Err(MezError::invalid_state(
                    "subagent conversation contract is incomplete or cannot be overwritten",
                ));
            }
            ("root", _, _) => {
                return Err(MezError::invalid_state(
                    "root conversation contract cannot be converted into a subagent contract",
                ));
            }
            _ => return Err(MezError::invalid_args("invalid conversation metadata kind")),
        }
        self.write_conversation_metadata_locked(conversation_id, &metadata)?;
        #[cfg(test)]
        let catalog_result = if self
            .fail_subagent_contract_catalog_upsert
            .swap(false, Ordering::SeqCst)
        {
            Err(MezError::invalid_state(
                "injected subagent contract catalog upsert failure",
            ))
        } else {
            self.upsert_catalog_from_files(conversation_id, Some(AgentConversationKind::Subagent))
                .map(|_| ())
        };
        #[cfg(not(test))]
        let catalog_result = self
            .upsert_catalog_from_files(conversation_id, Some(AgentConversationKind::Subagent))
            .map(|_| ());
        if let Err(error) = catalog_result {
            self.restore_conversation_metadata_snapshot_locked(
                conversation_id,
                &metadata_path,
                previous_metadata.as_deref(),
            )?;
            return Err(error);
        }
        self.remove_archive_recovery_journal(conversation_id)?;
        Ok(())
    }

    /// Loads the durable lifetime of one child conversation.
    pub fn subagent_lifetime(&self, conversation_id: &str) -> Result<mez_agent::SubagentLifetime> {
        Ok(self
            .read_conversation_metadata(conversation_id)?
            .subagent_lifetime)
    }

    /// Loads the durable owner conversation of a persistent child.
    pub fn persistent_subagent_parent_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .read_conversation_metadata(conversation_id)?
            .parent_conversation_id)
    }

    /// Loads the parent-assigned persistent objective for a child.
    pub fn parent_objective(&self, conversation_id: &str) -> Result<Option<String>> {
        Ok(self
            .read_conversation_metadata(conversation_id)?
            .parent_objective)
    }

    /// Loads the durable narrowed scope of a persistent child.
    pub fn persistent_subagent_scope(
        &self,
        conversation_id: &str,
    ) -> Result<Option<mez_agent::SubagentScopeDeclaration>> {
        Ok(self
            .read_conversation_metadata(conversation_id)?
            .subagent_scope)
    }

    /// Resolves the durable objective precedence for one conversation.
    pub fn effective_persisted_objective(&self, conversation_id: &str) -> Result<Option<String>> {
        let metadata = self.read_conversation_metadata(conversation_id)?;
        Ok(metadata.user_objective.or(metadata.parent_objective))
    }

    /// Loads one conversation's durable origin, defaulting legacy sessions to root.
    pub fn conversation_kind(&self, conversation_id: &str) -> Result<AgentConversationKind> {
        let metadata = self.read_conversation_metadata(conversation_id)?;
        match metadata.conversation_kind.as_str() {
            "root" => Ok(AgentConversationKind::Root),
            "subagent" => Ok(AgentConversationKind::Subagent),
            _ => Err(MezError::invalid_args("invalid conversation metadata kind")),
        }
    }

    /// Loads the immutable action catalog owned by one durable conversation.
    ///
    /// Legacy conversation metadata has no catalog and returns `None`, letting
    /// the runtime capture the configured catalog at its next session boundary.
    pub fn conversation_allowed_actions(
        &self,
        conversation_id: &str,
    ) -> Result<Option<AllowedActionSet>> {
        Ok(self
            .read_conversation_metadata(conversation_id)?
            .allowed_actions)
    }

    /// Saves the immutable action catalog owned by one conversation.
    ///
    /// This sidecar is keyed by durable conversation identity rather than a
    /// replaceable pane binding, so inactive conversations retain their own
    /// catalog through later checkpoints and resumes. A catalog may be written
    /// exactly once; subsequent writes must be byte-for-byte equivalent.
    pub fn save_conversation_allowed_actions(
        &self,
        conversation_id: &str,
        allowed_actions: Option<AllowedActionSet>,
    ) -> Result<()> {
        let _conversation_lock = self.acquire_conversation_lock(conversation_id)?;
        let mut metadata = self.read_conversation_metadata_locked(conversation_id)?;
        match (&metadata.allowed_actions, allowed_actions) {
            (None, Some(allowed_actions)) => {
                allowed_actions
                    .validate_persisted()
                    .map_err(MezError::invalid_args)?;
                metadata.allowed_actions = Some(allowed_actions);
            }
            (Some(current), Some(candidate)) if current == &candidate => return Ok(()),
            (None, None) => {
                return Err(MezError::invalid_args(
                    "conversation action catalog must be written with a nonempty catalog",
                ));
            }
            (Some(_), None) => {
                return Err(MezError::invalid_state(
                    "conversation action catalog cannot be cleared after capture",
                ));
            }
            (Some(_), Some(_)) => {
                return Err(MezError::invalid_state(
                    "conversation action catalog cannot be overwritten after capture",
                ));
            }
        }
        self.write_conversation_metadata_locked(conversation_id, &metadata)
    }

    /// Restores a catalog value captured before a failed compound metadata write.
    ///
    /// This narrowly supports rollback of a newly-created root catalog when
    /// pane-binding metadata persistence fails after the catalog sidecar write.
    pub(crate) fn restore_conversation_allowed_actions(
        &self,
        conversation_id: &str,
        allowed_actions: Option<AllowedActionSet>,
    ) -> Result<()> {
        let _conversation_lock = self.acquire_conversation_lock(conversation_id)?;
        let mut metadata = self.read_conversation_metadata_locked(conversation_id)?;
        if metadata.conversation_kind == "subagent" && allowed_actions.is_none() {
            return Err(MezError::invalid_state(
                "subagent conversation catalog cannot be removed",
            ));
        }
        metadata.allowed_actions = allowed_actions;
        self.write_conversation_metadata_locked(conversation_id, &metadata)
    }

    /// Loads durable spawned-session structural identity for one conversation.
    pub fn conversation_subagent_lineage(
        &self,
        conversation_id: &str,
    ) -> Result<Option<mez_agent::SubagentSessionLineage>> {
        let lineage = self
            .read_conversation_metadata(conversation_id)?
            .subagent_lineage;
        if let Some(lineage) = lineage.as_ref() {
            lineage
                .validate_persisted()
                .map_err(MezError::invalid_args)?;
        }
        Ok(lineage)
    }

    /// Returns the durable user-selected objective for one conversation.
    ///
    /// Missing metadata is a legacy root conversation with no override. The
    /// value is normalized before it reaches runtime precedence resolution, so
    /// corrupt objective metadata fails closed rather than being published.
    pub fn user_objective(&self, conversation_id: &str) -> Result<Option<String>> {
        #[cfg(test)]
        if self
            .fail_user_objective_read_countdown
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .unwrap_or(0)
            == 1
        {
            return Err(MezError::invalid_state(
                "injected second user objective metadata read failure",
            ));
        }
        let metadata = self.read_conversation_metadata(conversation_id)?;
        Ok(metadata
            .user_objective
            .as_deref()
            .map(mez_agent::messaging::normalize_objective)
            .transpose()?)
    }

    /// Atomically sets or clears one conversation's authoritative user objective.
    ///
    /// The same per-conversation lock and atomic sidecar replacement used for
    /// conversation kind writes preserve unrelated metadata fields when a kind
    /// update races with an objective update. `None` is the explicit clear API;
    /// protocol objective absence is handled above this persistence boundary.
    pub fn save_user_objective(
        &self,
        conversation_id: &str,
        objective: Option<&str>,
    ) -> Result<bool> {
        let objective = objective
            .map(mez_agent::messaging::normalize_objective)
            .transpose()?;
        let _conversation_lock = self.acquire_conversation_lock(conversation_id)?;
        let mut metadata = self.read_conversation_metadata_locked(conversation_id)?;
        if metadata.user_objective == objective {
            return Ok(false);
        }
        metadata.user_objective = objective;
        self.write_conversation_metadata_locked(conversation_id, &metadata)?;
        // The sidecar is authoritative and has already been atomically
        // committed. The discovery catalog is rebuildable display metadata, so
        // refresh or recovery-journal cleanup failure must not make a completed
        // set or clear appear to have failed to its live MMP synchronization
        // caller. Startup replay idempotently removes any retained journal.
        let _ = self.upsert_catalog_from_files(conversation_id, None);
        let _ = self.remove_archive_recovery_journal(conversation_id);
        Ok(true)
    }

    /// Appends multiple validated transcript entries through Tokio filesystem
    /// I/O and returns bytes written.
    ///
    /// This is used by the async runtime persistence worker so transcript
    /// durability does not require a blocking worker task.
    pub async fn append_many_async(&self, entries: &[TranscriptEntry]) -> Result<usize> {
        let store = self.clone();
        let entries = entries.to_vec();
        tokio::task::spawn_blocking(move || store.append_many(&entries))
            .await
            .map_err(|error| {
                MezError::invalid_state(format!(
                    "transcript persistence worker join failed: {error}"
                ))
            })?
    }

    /// Appends one validated presentation entry to its conversation file.
    ///
    /// Presentation rows are user-interface replay state. They intentionally
    /// live beside, not inside, model-facing transcript history.
    pub fn append_presentation(&self, entry: &AgentPresentationEntry) -> Result<()> {
        let entry = entry.normalized_for_agent_log_wrap();
        entry.validate()?;
        let _conversation_lock = self.acquire_conversation_lock(&entry.conversation_id)?;
        self.ensure_session_dir(&entry.conversation_id)?;
        let path = self.presentation_path_for(&entry.conversation_id)?;
        let encoded = entry.encode()?;
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        file.write_all(encoded.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        set_private_file_permissions(&path)?;
        self.write_presentation_index(&entry)?;
        self.compact_presentation_tail_if_needed(&entry.conversation_id)?;
        self.upsert_catalog_after_presentation(&entry)?;
        Ok(())
    }

    /// Assigns contiguous sequences and appends one immutable presentation batch.
    ///
    /// The existing per-conversation lock covers sequence discovery, one file
    /// append and sync, one index update, at most one compaction, and one catalog
    /// update. This keeps actor-enqueued presentation effects ordered with
    /// transcript and archive effects while removing filesystem work from the
    /// serialized runtime owner.
    pub fn append_presentation_many(&self, entries: &[AgentPresentationEntry]) -> Result<usize> {
        let Some(first) = entries.first() else {
            return Ok(0);
        };
        validate_conversation_id(&first.conversation_id)?;
        if entries
            .iter()
            .any(|entry| entry.conversation_id != first.conversation_id)
        {
            return Err(MezError::invalid_args(
                "presentation batch entries must share one conversation",
            ));
        }
        let _conversation_lock = self.acquire_conversation_lock(&first.conversation_id)?;
        let mut sequence = self.next_presentation_sequence(&first.conversation_id)?;
        let mut normalized = Vec::with_capacity(entries.len());
        let mut encoded = String::new();
        for entry in entries {
            let mut entry = entry.normalized_for_agent_log_wrap();
            entry.sequence = sequence;
            entry.validate()?;
            encoded.push_str(&entry.encode()?);
            encoded.push('\n');
            normalized.push(entry);
            sequence = sequence.saturating_add(1);
        }
        self.ensure_session_dir(&first.conversation_id)?;
        let path = self.presentation_path_for(&first.conversation_id)?;
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        file.write_all(encoded.as_bytes())?;
        file.sync_all()?;
        set_private_file_permissions(&path)?;
        let latest = normalized
            .last()
            .ok_or_else(|| MezError::invalid_state("presentation batch disappeared"))?;
        self.write_presentation_index(latest)?;
        self.compact_presentation_tail_if_needed(&first.conversation_id)?;
        self.upsert_catalog_after_presentation(latest)?;
        Ok(encoded.len())
    }

    /// Appends a presentation batch on Tokio's blocking pool.
    pub async fn append_presentation_many_async(
        &self,
        entries: &[AgentPresentationEntry],
    ) -> Result<usize> {
        let store = self.clone();
        let entries = entries.to_vec();
        tokio::task::spawn_blocking(move || store.append_presentation_many(&entries))
            .await
            .map_err(|error| {
                MezError::invalid_state(format!(
                    "presentation persistence worker join failed: {error}"
                ))
            })?
    }

    /// Updates one catalog row after a presentation append without replaying history.
    fn upsert_catalog_after_presentation(&self, entry: &AgentPresentationEntry) -> Result<()> {
        let existing = catalog::record(self, &entry.conversation_id)?;
        let named = if existing.is_none() {
            self.read_named_sessions_index()?
                .remove(&entry.conversation_id)
        } else {
            None
        };
        let summary = if let Some(summary) = self.summary(&entry.conversation_id)? {
            summary
        } else if let Some(record) = existing.as_ref() {
            let mut summary = record.session.summary.clone();
            if summary.first_created_at_unix_seconds == 0 {
                summary.first_created_at_unix_seconds = entry.created_at_unix_seconds;
            }
            summary.last_created_at_unix_seconds = entry.created_at_unix_seconds;
            summary.last_turn_id = entry.turn_id.clone().unwrap_or_default();
            summary.pane_id = entry.pane_id.clone();
            summary
        } else {
            ConversationSummary {
                conversation_id: entry.conversation_id.clone(),
                entries: 0,
                first_created_at_unix_seconds: entry.created_at_unix_seconds,
                last_created_at_unix_seconds: entry.created_at_unix_seconds,
                last_turn_id: entry.turn_id.clone().unwrap_or_default(),
                agent_id: String::new(),
                pane_id: entry.pane_id.clone(),
                directory: named.as_ref().and_then(|session| session.directory.clone()),
                initial_prompt: None,
                latest_user_prompt: None,
            }
        };
        let candidate = CatalogCandidate {
            summary,
            name: named
                .as_ref()
                .map(|session| session.name.clone())
                .or_else(|| {
                    existing
                        .as_ref()
                        .and_then(|record| record.session.name.clone())
                }),
            named_at_unix_seconds: named.as_ref().map(|session| session.named_at_unix_seconds),
            name_preferred: named
                .as_ref()
                .map(|session| !session.ephemeral)
                .unwrap_or(true),
            conversation_kind: self.conversation_kind(&entry.conversation_id)?,
            has_transcript: self.transcript_path_for(&entry.conversation_id)?.is_file()
                || self
                    .legacy_transcript_path_for(&entry.conversation_id)?
                    .is_file(),
            has_presentation: true,
            payload_layout: CatalogPayloadLayout::Directory,
            archived_at_unix_seconds: None,
            archive_compressed_bytes: None,
            archive_sha256: None,
        };
        catalog::upsert(self, &candidate, entry.created_at_unix_seconds)
    }

    /// Reads all presentation entries for one conversation.
    ///
    /// Missing presentation logs are treated as empty so older transcript
    /// directories can still use synthesized resume display.
    pub fn inspect_presentation(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<AgentPresentationEntry>> {
        let mut data = String::new();
        let compressed_path = self.presentation_compressed_path_for(conversation_id)?;
        if compressed_path.exists() {
            let compressed = std_fs::read(&compressed_path)?;
            let decoded = zstd::stream::decode_all(&compressed[..]).map_err(|error| {
                MezError::invalid_args(format!(
                    "presentation compressed history decode failed: {error}"
                ))
            })?;
            data.push_str(&String::from_utf8(decoded).map_err(|error| {
                MezError::invalid_args(format!(
                    "presentation compressed history is not UTF-8: {error}"
                ))
            })?);
        }
        let path = self.presentation_path_for(conversation_id)?;
        if path.exists() {
            std_fs::File::open(path)?.read_to_string(&mut data)?;
        }
        if data.is_empty() {
            return Ok(Vec::new());
        }
        data.lines()
            .filter(|line| !line.trim().is_empty())
            .map(AgentPresentationEntry::decode)
            .collect()
    }

    /// Returns the next append sequence for one presentation log.
    pub fn next_presentation_sequence(&self, conversation_id: &str) -> Result<u64> {
        if let Some(sequence) = self.read_presentation_index(conversation_id)? {
            return Ok(sequence.saturating_add(1));
        }
        let entries = self.inspect_recent_presentation(
            conversation_id,
            1,
            DEFAULT_PRESENTATION_TAIL_READ_BYTES,
        )?;
        Ok(entries
            .last()
            .map(|entry| entry.sequence.saturating_add(1))
            .unwrap_or(1))
    }

    /// Appends one validated transcript entry while its conversation lock is held.
    fn append_one_locked(&self, entry: &TranscriptEntry) -> Result<usize> {
        self.retire_stale_active_delete_journal(&entry.conversation_id)?;
        self.ensure_session_dir(&entry.conversation_id)?;
        let path = self.transcript_path_for(&entry.conversation_id)?;
        let encoded = encode_transcript_entry(entry)?;
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        file.write_all(encoded.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        set_private_file_permissions(&path)?;
        self.update_summary_after_append(entry)?;
        self.upsert_catalog_from_files(&entry.conversation_id, None)?;
        Ok(encoded.len().saturating_add(1))
    }

    /// Reads and decodes the latest cleartext presentation entries without
    /// loading compressed historical presentation frames.
    ///
    /// Resume replay only needs a bounded visible tail. When the cleartext tail
    /// is empty because all historical rows were compacted, callers receive an
    /// empty vector and can fall back to transcript metadata or recent text.
    pub fn inspect_recent_presentation(
        &self,
        conversation_id: &str,
        max_entries: usize,
        max_bytes: u64,
    ) -> Result<Vec<AgentPresentationEntry>> {
        if max_entries == 0 {
            return Ok(Vec::new());
        }
        if max_bytes == 0 {
            return Err(MezError::invalid_args(
                "recent presentation byte limit must be non-zero",
            ));
        }
        let path = self.presentation_path_for(conversation_id)?;
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut file = std_fs::File::open(path)?;
        let length = file.metadata()?.len();
        let start = length.saturating_sub(max_bytes);
        let seek_start = if start > 0 {
            start.saturating_sub(1)
        } else {
            0
        };
        if seek_start > 0 {
            file.seek(SeekFrom::Start(seek_start))?;
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let tail_bytes = if start == 0 {
            bytes.as_slice()
        } else if bytes.first().is_some_and(|byte| *byte == b'\n') {
            &bytes[1..]
        } else if let Some(newline_index) = bytes.iter().position(|byte| *byte == b'\n') {
            &bytes[newline_index.saturating_add(1)..]
        } else {
            &[]
        };
        let text = String::from_utf8_lossy(tail_bytes);
        let lines = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        let decoded = lines
            .into_iter()
            .map(AgentPresentationEntry::decode)
            .collect::<Result<Vec<_>>>()?;
        let first = decoded.len().saturating_sub(max_entries);
        Ok(decoded[first..].to_vec())
    }

    /// Moves an oversized cleartext presentation tail into compressed history.
    ///
    /// The compressed history is an append-only zstd stream made from
    /// concatenated frames, so replay can decode the full historical prefix and
    /// then append the active cleartext tail.
    fn compact_presentation_tail_if_needed(&self, conversation_id: &str) -> Result<()> {
        let path = self.presentation_path_for(conversation_id)?;
        if !path.exists() {
            return Ok(());
        }
        let metadata = std_fs::metadata(&path)?;
        if metadata.len() < self.presentation_compaction_threshold {
            return Ok(());
        }
        let data = std_fs::read(&path)?;
        if data.is_empty() {
            return Ok(());
        }
        let compressed = zstd::stream::encode_all(&data[..], 0).map_err(|error| {
            MezError::invalid_args(format!("presentation compression failed: {error}"))
        })?;
        let compressed_path = self.presentation_compressed_path_for(conversation_id)?;
        let mut compressed_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&compressed_path)?;
        compressed_file.write_all(&compressed)?;
        compressed_file.sync_all()?;
        set_private_file_permissions(&compressed_path)?;

        let tail = OpenOptions::new().write(true).truncate(true).open(&path)?;
        tail.sync_all()?;
        set_private_file_permissions(&path)?;
        Ok(())
    }

    /// Reads and decodes all entries for one conversation.
    ///
    /// Returns a not-found error when the conversation file does not exist.
    pub fn inspect(&self, conversation_id: &str) -> Result<Vec<TranscriptEntry>> {
        let path = self.existing_transcript_path_for(conversation_id)?;
        if !path.exists() {
            return Err(MezError::new(
                MezErrorKind::NotFound,
                "conversation transcript not found",
            ));
        }
        let mut data = String::new();
        std_fs::File::open(path)?.read_to_string(&mut data)?;
        data.lines()
            .filter(|line| !line.trim().is_empty())
            .map(decode_transcript_entry)
            .collect()
    }

    /// Returns a canonical SHA-256 revision covering every persisted entry field.
    pub(crate) fn transcript_entry_revision(&self, entry: &TranscriptEntry) -> Result<String> {
        let encoded = encode_transcript_entry(entry)?;
        Ok(Sha256::digest(encoded.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    /// Replaces only one transcript entry's content when its full revision and
    /// pane identity still match, then atomically rebuilds derived metadata.
    pub(crate) fn compare_and_swap_entry_content(
        &self,
        conversation_id: &str,
        sequence: u64,
        pane_id: &str,
        expected_revision: &str,
        content: String,
    ) -> Result<CompareAndSwapTranscriptEntryResult> {
        validate_conversation_id(conversation_id)?;
        if sequence == 0 {
            return Err(MezError::invalid_args(
                "transcript entry sequence must be non-zero",
            ));
        }
        if expected_revision.len() != 64
            || !expected_revision
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(MezError::invalid_args(
                "transcript entry revision must be a lowercase SHA-256 digest",
            ));
        }
        let _conversation_lock = self.acquire_conversation_lock(conversation_id)?;
        let existing_path = self.existing_transcript_path_for(conversation_id)?;
        if !existing_path.exists() {
            return Ok(CompareAndSwapTranscriptEntryResult::Deleted);
        }
        let mut entries = self.inspect(conversation_id)?;
        let Some(entry) = entries.iter_mut().find(|entry| entry.sequence == sequence) else {
            return Ok(CompareAndSwapTranscriptEntryResult::Deleted);
        };
        if entry.pane_id != pane_id {
            return Ok(CompareAndSwapTranscriptEntryResult::WrongPane);
        }
        let current_revision = self.transcript_entry_revision(entry)?;
        if current_revision != expected_revision {
            return Ok(CompareAndSwapTranscriptEntryResult::Stale { current_revision });
        }
        entry.content = content;
        entry.validate()?;
        let updated = entry.clone();
        self.rewrite_transcript_locked(conversation_id, &existing_path, &entries)?;
        Ok(CompareAndSwapTranscriptEntryResult::Updated(updated))
    }

    /// Deletes one transcript entry identified by its current sequence number.
    ///
    /// Surviving entries retain transcript order and are renumbered contiguously
    /// before an atomic file replacement. The saved-session summary is rebuilt
    /// from the rewritten transcript so later appends and session listings stay
    /// consistent. Returns `false` without mutation when the sequence is absent.
    pub fn delete_entry(&self, conversation_id: &str, sequence: u64) -> Result<bool> {
        validate_conversation_id(conversation_id)?;
        if sequence == 0 {
            return Err(MezError::invalid_args(
                "transcript entry sequence must be non-zero",
            ));
        }
        let _conversation_lock = self.acquire_conversation_lock(conversation_id)?;
        let existing_path = self.existing_transcript_path_for(conversation_id)?;
        let mut entries = self.inspect(conversation_id)?;
        let Some(index) = entries.iter().position(|entry| entry.sequence == sequence) else {
            return Ok(false);
        };
        entries.remove(index);
        for (index, entry) in entries.iter_mut().enumerate() {
            entry.sequence = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
            entry.validate()?;
        }

        self.rewrite_transcript_locked(conversation_id, &existing_path, &entries)?;
        Ok(true)
    }

    /// Atomically replaces one transcript while its conversation lock is held,
    /// then rebuilds the summary sidecar and saved-session catalog row.
    fn rewrite_transcript_locked(
        &self,
        conversation_id: &str,
        existing_path: &Path,
        entries: &[TranscriptEntry],
    ) -> Result<()> {
        let session_dir = self.ensure_session_dir(conversation_id)?;
        let path = self.transcript_path_for(conversation_id)?;
        let temp_path = session_dir.join("history.tsv.tmp");
        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temp_path)?;
            for entry in entries {
                file.write_all(encode_transcript_entry(entry)?.as_bytes())?;
                file.write_all(b"\n")?;
            }
            file.sync_all()?;
        }
        set_private_file_permissions(&temp_path)?;
        std_fs::rename(&temp_path, &path)?;
        set_private_file_permissions(&path)?;
        if existing_path != path && existing_path.exists() {
            std_fs::remove_file(existing_path)?;
        }

        if let Some(summary) = summarize_conversation(entries.to_vec()) {
            self.write_summary_sidecar(&summary)?;
        } else {
            let summary_path = session_dir.join(SESSION_SUMMARY_FILE_NAME);
            if summary_path.exists() {
                std_fs::remove_file(summary_path)?;
            }
        }
        self.upsert_catalog_from_files(conversation_id, None)?;
        Ok(())
    }

    /// Reads and decodes the latest entries for one conversation without
    /// loading the entire transcript file.
    ///
    /// The reader seeks from the end of the append-only TSV file, discards a
    /// partial first line when the read starts in the middle of the file, and
    /// returns at most `max_entries` decoded records. This keeps model-context
    /// assembly bounded even when an older transcript grew unexpectedly.
    pub fn inspect_recent(
        &self,
        conversation_id: &str,
        max_entries: usize,
        max_bytes: u64,
    ) -> Result<Vec<TranscriptEntry>> {
        if max_entries == 0 {
            return Ok(Vec::new());
        }
        if max_bytes == 0 {
            return Err(MezError::invalid_args(
                "recent transcript byte limit must be non-zero",
            ));
        }
        let path = self.existing_transcript_path_for(conversation_id)?;
        if !path.exists() {
            return Err(MezError::new(
                MezErrorKind::NotFound,
                "conversation transcript not found",
            ));
        }
        let mut file = std_fs::File::open(path)?;
        let length = file.metadata()?.len();
        let start = length.saturating_sub(max_bytes);
        let seek_start = if start > 0 {
            start.saturating_sub(1)
        } else {
            0
        };
        if seek_start > 0 {
            file.seek(SeekFrom::Start(seek_start))?;
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let tail_bytes = if start == 0 {
            bytes.as_slice()
        } else if bytes.first().is_some_and(|byte| *byte == b'\n') {
            &bytes[1..]
        } else if let Some(newline_index) = bytes.iter().position(|byte| *byte == b'\n') {
            &bytes[newline_index.saturating_add(1)..]
        } else {
            &[]
        };
        let text = String::from_utf8_lossy(tail_bytes);
        let lines = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        let decoded = lines
            .into_iter()
            .map(decode_transcript_entry)
            .collect::<Result<Vec<_>>>()?;
        let first = decoded.len().saturating_sub(max_entries);
        Ok(decoded[first..].to_vec())
    }

    /// Returns the next append sequence for one conversation without scanning
    /// the full transcript file.
    ///
    /// The method reads only the bounded tail needed to decode the latest
    /// complete entry. If the transcript exists but the bounded tail contains
    /// no complete entry, the file is treated as oversized or corrupt rather
    /// than risking a whole-file read.
    pub fn next_sequence(&self, conversation_id: &str) -> Result<u64> {
        let path = self.existing_transcript_path_for(conversation_id)?;
        if !path.exists() {
            return Err(MezError::new(
                MezErrorKind::NotFound,
                "conversation transcript not found",
            ));
        }
        let entries =
            self.inspect_recent(conversation_id, 1, DEFAULT_TRANSCRIPT_TAIL_READ_BYTES)?;
        if let Some(entry) = entries.last() {
            return Ok(entry.sequence.saturating_add(1));
        }
        if path.metadata()?.len() == 0 {
            return Ok(1);
        }
        Err(MezError::invalid_state(
            "conversation transcript tail contains no complete entry",
        ))
    }

    /// Lists transcript-backed summaries for focused compatibility tests.
    #[cfg(test)]
    pub fn list(&self) -> Result<Vec<ConversationSummary>> {
        catalog::transcript_summaries(self)
    }

    /// Lists transcript-backed and named zero-entry sessions as one durable view.
    #[cfg(test)]
    pub fn saved_sessions(&self) -> Result<Vec<SavedAgentSession>> {
        let mut sessions = catalog::saved_sessions(self)?;
        self.attach_session_objective_titles(&mut sessions);
        Ok(sessions)
    }

    /// Builds one deterministic migration snapshot from retained session files.
    ///
    /// The root is enumerated once. Directory payloads take precedence over a
    /// duplicate legacy TSV, names are read once and overlaid last, and
    /// unrelated root entries are ignored. Presentation-only metadata is read
    /// as a stream so compressed history is never inflated into one allocation.
    pub(super) fn catalog_migration_candidates(&self) -> Result<Vec<CatalogCandidate>> {
        catalog::note_full_scan();
        let names = self.read_named_sessions_index()?;
        if !self.root.exists() {
            return names
                .values()
                .map(|named| self.named_only_catalog_candidate(named))
                .collect::<Result<Vec<_>>>();
        }

        let mut paths = std_fs::read_dir(&self.root)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        let mut candidates = BTreeMap::<String, CatalogCandidate>::new();

        for path in &paths {
            if !path.is_dir() {
                continue;
            }
            let Some(conversation_id) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if validate_conversation_id(conversation_id).is_err() {
                continue;
            }
            let has_transcript = path.join(SESSION_TRANSCRIPT_FILE_NAME).is_file();
            let has_presentation = path.join(SESSION_PRESENTATION_FILE_NAME).is_file()
                || path
                    .join(SESSION_PRESENTATION_COMPRESSED_FILE_NAME)
                    .is_file();
            let has_objective = self.user_objective(conversation_id)?.is_some();
            if !has_transcript && !has_presentation && !has_objective {
                continue;
            }
            let candidate = if !has_transcript && !has_presentation {
                self.objective_only_catalog_candidate(conversation_id, names.get(conversation_id))?
            } else {
                self.catalog_candidate_for_payload(
                    conversation_id,
                    has_transcript,
                    has_presentation,
                    CatalogPayloadLayout::Directory,
                    names.get(conversation_id),
                )?
            };
            if let Some(candidate) = candidate {
                candidates.insert(conversation_id.to_string(), candidate);
            }
        }

        for path in &paths {
            if !path.is_file()
                || is_root_control_tsv_path(path)
                || path.extension().and_then(|extension| extension.to_str()) != Some("tsv")
            {
                continue;
            }
            let Some(conversation_id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if candidates.contains_key(conversation_id)
                || validate_conversation_id(conversation_id).is_err()
            {
                continue;
            }
            let session_dir = self.session_dir_for(conversation_id)?;
            let has_presentation = session_dir.join(SESSION_PRESENTATION_FILE_NAME).is_file()
                || session_dir
                    .join(SESSION_PRESENTATION_COMPRESSED_FILE_NAME)
                    .is_file();
            if let Some(candidate) = self.catalog_candidate_for_payload(
                conversation_id,
                true,
                has_presentation,
                CatalogPayloadLayout::LegacyTsv,
                names.get(conversation_id),
            )? {
                candidates.insert(conversation_id.to_string(), candidate);
            }
        }

        for path in &paths {
            if !path.is_dir() {
                continue;
            }
            let Some(conversation_id) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if candidates.contains_key(conversation_id)
                || validate_conversation_id(conversation_id).is_err()
            {
                continue;
            }
            let has_presentation = path.join(SESSION_PRESENTATION_FILE_NAME).is_file()
                || path
                    .join(SESSION_PRESENTATION_COMPRESSED_FILE_NAME)
                    .is_file();
            if !has_presentation {
                continue;
            }
            if let Some(candidate) = self.catalog_candidate_for_payload(
                conversation_id,
                false,
                true,
                CatalogPayloadLayout::Directory,
                names.get(conversation_id),
            )? {
                candidates.insert(conversation_id.to_string(), candidate);
            }
        }

        for candidate in archived_catalog_candidates(self)? {
            candidates
                .entry(candidate.summary.conversation_id.clone())
                .or_insert(candidate);
        }

        for named in names.values() {
            if let Some(candidate) = candidates.get_mut(&named.conversation_id) {
                candidate.name = Some(named.name.clone());
                candidate.named_at_unix_seconds = Some(named.named_at_unix_seconds);
                if candidate.summary.directory.is_none() {
                    candidate.summary.directory = named.directory.clone();
                }
            } else {
                candidates.insert(
                    named.conversation_id.clone(),
                    self.named_only_catalog_candidate(named)?,
                );
            }
        }
        candidates
            .into_values()
            .map(|candidate| {
                self.is_unrestorable_legacy_subagent(&candidate.summary.conversation_id)
                    .map(|unrestorable| (!unrestorable).then_some(candidate))
            })
            .collect::<Result<Vec<_>>>()
            .map(|candidates| candidates.into_iter().flatten().collect())
    }

    /// Builds one candidate for a transcript-backed or presentation-only payload.
    fn catalog_candidate_for_payload(
        &self,
        conversation_id: &str,
        has_transcript: bool,
        has_presentation: bool,
        payload_layout: CatalogPayloadLayout,
        named: Option<&NamedAgentSession>,
    ) -> Result<Option<CatalogCandidate>> {
        if self.is_unrestorable_legacy_subagent(conversation_id)? {
            return Ok(None);
        }
        let summary = if has_transcript {
            self.summary(conversation_id)?
        } else {
            self.presentation_migration_summary(conversation_id, named)?
        };
        let Some(mut summary) = summary else {
            return named
                .map(|named| self.named_only_catalog_candidate(named))
                .transpose();
        };
        if summary.directory.is_none() {
            summary.directory = named.and_then(|session| session.directory.clone());
        }
        Ok(Some(CatalogCandidate {
            summary,
            name: named.map(|session| session.name.clone()),
            named_at_unix_seconds: named.map(|session| session.named_at_unix_seconds),
            name_preferred: named.map(|session| !session.ephemeral).unwrap_or(true),
            conversation_kind: self.conversation_kind(conversation_id)?,
            has_transcript,
            has_presentation,
            payload_layout,
            archived_at_unix_seconds: None,
            archive_compressed_bytes: None,
            archive_sha256: None,
        }))
    }

    /// Synthesizes one zero-entry catalog record from durable naming metadata.
    fn named_only_catalog_candidate(&self, named: &NamedAgentSession) -> Result<CatalogCandidate> {
        Ok(CatalogCandidate {
            summary: ConversationSummary {
                conversation_id: named.conversation_id.clone(),
                entries: 0,
                first_created_at_unix_seconds: named.named_at_unix_seconds,
                last_created_at_unix_seconds: named.named_at_unix_seconds,
                last_turn_id: String::new(),
                agent_id: String::new(),
                pane_id: String::new(),
                directory: named.directory.clone(),
                initial_prompt: None,
                latest_user_prompt: None,
            },
            name: Some(named.name.clone()),
            named_at_unix_seconds: Some(named.named_at_unix_seconds),
            name_preferred: !named.ephemeral,
            conversation_kind: self.conversation_kind(&named.conversation_id)?,
            has_transcript: false,
            has_presentation: false,
            payload_layout: CatalogPayloadLayout::Directory,
            archived_at_unix_seconds: None,
            archive_compressed_bytes: None,
            archive_sha256: None,
        })
    }

    /// Synthesizes a zero-entry catalog record from durable objective metadata.
    ///
    /// The objective remains exclusively in the metadata sidecar; the catalog
    /// records only the conversation identity and metadata modification time
    /// needed for lifecycle lookup and ordering.
    fn objective_only_catalog_candidate(
        &self,
        conversation_id: &str,
        named: Option<&NamedAgentSession>,
    ) -> Result<Option<CatalogCandidate>> {
        let metadata_path = self.conversation_metadata_path_for(conversation_id)?;
        let modified_at = metadata_path
            .metadata()?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| {
                MezError::invalid_state(
                    "conversation metadata modification time predates Unix epoch",
                )
            })?
            .as_secs();
        Ok(Some(CatalogCandidate {
            summary: ConversationSummary {
                conversation_id: conversation_id.to_string(),
                entries: 0,
                first_created_at_unix_seconds: modified_at,
                last_created_at_unix_seconds: modified_at,
                last_turn_id: String::new(),
                agent_id: String::new(),
                pane_id: String::new(),
                directory: named.and_then(|session| session.directory.clone()),
                initial_prompt: None,
                latest_user_prompt: None,
            },
            name: named.map(|session| session.name.clone()),
            named_at_unix_seconds: named.map(|session| session.named_at_unix_seconds),
            name_preferred: named.map(|session| !session.ephemeral).unwrap_or(true),
            conversation_kind: self.conversation_kind(conversation_id)?,
            has_transcript: false,
            has_presentation: false,
            payload_layout: CatalogPayloadLayout::Directory,
            archived_at_unix_seconds: None,
            archive_compressed_bytes: None,
            archive_sha256: None,
        }))
    }

    /// Assigns or replaces the user-assigned display name for one conversation.
    ///
    /// An ephemeral name is still a real name for display, lookup, and title
    /// precedence; the flag only removes the row from the named-first picker
    /// ranking, and a later plain assignment promotes it back to durable.
    pub fn name_session(
        &self,
        conversation_id: &str,
        name: &str,
        named_at_unix_seconds: u64,
        directory: Option<String>,
        ephemeral: bool,
    ) -> Result<NamedAgentSession> {
        validate_conversation_id(conversation_id)?;
        let _conversation_lock = self.acquire_conversation_lock(conversation_id)?;
        let name = validate_agent_session_name(name)?;
        let directory = directory
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let session = NamedAgentSession {
            conversation_id: conversation_id.to_string(),
            name,
            named_at_unix_seconds,
            directory,
            ephemeral,
        };
        let _lock = self.acquire_named_sessions_lock()?;
        let mut sessions = self.read_named_sessions_index()?;
        sessions.insert(conversation_id.to_string(), session.clone());
        self.write_named_sessions_index(&sessions)?;
        self.update_archived_session_name(
            conversation_id,
            Some((&session.name, named_at_unix_seconds)),
        )?;
        self.upsert_catalog_from_files(conversation_id, None)?;
        catalog::set_name(
            self,
            conversation_id,
            &session.name,
            named_at_unix_seconds,
            !session.ephemeral,
        )?;
        Ok(session)
    }

    /// Removes one durable conversation name without deleting conversation data.
    ///
    /// Returns true when a name existed and false when the conversation was
    /// already unnamed.
    pub fn clear_session_name(&self, conversation_id: &str) -> Result<bool> {
        validate_conversation_id(conversation_id)?;
        let _conversation_lock = self.acquire_conversation_lock(conversation_id)?;
        let removed = self.remove_named_session(conversation_id)?;
        if removed {
            self.update_archived_session_name(conversation_id, None)?;
        }
        if removed && self.upsert_catalog_from_files(conversation_id, None)? {
            catalog::clear_name(self, conversation_id)?;
        }
        Ok(removed)
    }

    /// Loads one durable name record when the conversation has been named.
    #[cfg(test)]
    pub fn named_session(&self, conversation_id: &str) -> Result<Option<NamedAgentSession>> {
        validate_conversation_id(conversation_id)?;
        Ok(self.read_named_sessions_index()?.remove(conversation_id))
    }

    /// Lists durable name records in conversation-id order.
    #[cfg(test)]
    pub fn named_sessions(&self) -> Result<Vec<NamedAgentSession>> {
        Ok(self.read_named_sessions_index()?.into_values().collect())
    }

    /// Loads bounded summary metadata for one saved conversation.
    ///
    /// New transcript appends maintain a sidecar summary. Legacy sessions fall
    /// back to decoding the first complete transcript row and a bounded tail so
    /// list/latest paths avoid whole-transcript decoding.
    pub fn summary(&self, conversation_id: &str) -> Result<Option<ConversationSummary>> {
        validate_conversation_id(conversation_id)?;
        if let Some(summary) = self.read_summary_sidecar(conversation_id)? {
            return Ok(Some(summary));
        }
        self.legacy_bounded_summary(conversation_id)
    }

    /// Returns the durable active agent-session metadata file for tests.
    ///
    /// Restored-snapshot degradation tests seed malformed durable state through
    /// this accessor instead of duplicating the on-disk layout.
    #[cfg(test)]
    pub(crate) fn agent_session_metadata_path_for_tests(&self) -> PathBuf {
        self.agent_session_metadata_path()
    }

    /// Loads active agent-session metadata for one Mezzanine session id.
    pub fn load_agent_session_metadata(
        &self,
        mezzanine_session_id: &str,
    ) -> Result<Vec<AgentSessionMetadata>> {
        if mezzanine_session_id.trim().is_empty() {
            return Err(MezError::invalid_args(
                "mezzanine session id must not be empty",
            ));
        }
        let path = self.agent_session_metadata_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut data = String::new();
        std_fs::File::open(path)?.read_to_string(&mut data)?;
        data.lines()
            .filter(|line| !line.trim().is_empty())
            .map(decode_agent_session_metadata)
            .filter_map(|decoded| match decoded {
                Ok(metadata) if metadata.mezzanine_session_id == mezzanine_session_id => {
                    Some(Ok(metadata))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }

    /// Replaces active agent-session metadata for one Mezzanine session id.
    ///
    /// Records for other live or saved Mezzanine sessions are preserved. This
    /// makes each checkpoint idempotent while avoiding cross-session
    /// contamination when a new daemon owns a different session identity.
    pub fn save_agent_session_metadata(
        &self,
        mezzanine_session_id: &str,
        records: &[AgentSessionMetadata],
    ) -> Result<usize> {
        if mezzanine_session_id.trim().is_empty() {
            return Err(MezError::invalid_args(
                "mezzanine session id must not be empty",
            ));
        }
        for record in records {
            record.validate()?;
            if record.mezzanine_session_id != mezzanine_session_id {
                return Err(MezError::invalid_args(
                    "agent session metadata belongs to a different Mezzanine session",
                ));
            }
        }
        #[cfg(test)]
        if self
            .fail_agent_session_metadata_write
            .swap(false, Ordering::SeqCst)
        {
            return Err(MezError::invalid_state(
                "injected agent session metadata write failure",
            ));
        }
        self.ensure_store_dir()?;
        let path = self.agent_session_metadata_path();
        let mut merged = Vec::new();
        if path.exists() {
            let mut data = String::new();
            std_fs::File::open(&path)?.read_to_string(&mut data)?;
            for line in data.lines().filter(|line| !line.trim().is_empty()) {
                let metadata = decode_agent_session_metadata(line)?;
                if metadata.mezzanine_session_id != mezzanine_session_id {
                    merged.push(metadata);
                }
            }
        }
        merged.extend(records.iter().cloned());
        let temp_path = path.with_extension("tmp");
        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temp_path)?;
            for metadata in &merged {
                file.write_all(encode_agent_session_metadata(metadata)?.as_bytes())?;
                file.write_all(b"\n")?;
            }
            file.sync_all()?;
        }
        set_private_file_permissions(&temp_path)?;
        std_fs::rename(&temp_path, &path)?;
        set_private_file_permissions(&path)?;
        Ok(records.len())
    }

    /// Deletes a conversation transcript.
    ///
    /// Returns true when a file was removed and false when the conversation was
    /// already absent.
    pub fn delete(&self, conversation_id: &str) -> Result<bool> {
        validate_conversation_id(conversation_id)?;
        let _conversation_lock = self.acquire_conversation_lock(conversation_id)?;
        self.delete_locked(conversation_id)
    }

    /// Deletes one conversation while its per-conversation lock is held.
    fn delete_locked(&self, conversation_id: &str) -> Result<bool> {
        self.write_archive_recovery_journal(
            conversation_id,
            ArchiveRecoveryOperation::DeleteActive,
            CatalogPayloadLayout::Directory,
        )?;
        let session_dir = self.session_dir_for(conversation_id)?;
        let removed_directory = if session_dir.exists() {
            std_fs::remove_dir_all(session_dir)?;
            true
        } else {
            false
        };
        let legacy_path = self.legacy_transcript_path_for(conversation_id)?;
        let removed_legacy = if legacy_path.exists() {
            std_fs::remove_file(legacy_path)?;
            true
        } else {
            false
        };
        let removed_name = self.remove_named_session(conversation_id)?;
        // The objective title mirror is a display cache: a missing or unreadable
        // index must never fail a conversation delete.
        let _ = self.remove_session_objective_mirror(conversation_id);
        // The generated-title sidecar is the same kind of cache, pruned for the
        // same reason and with the same best-effort failure rule.
        let _ = self.remove_session_title_mirror(conversation_id);
        // A title worker that started before this delete can still settle
        // afterwards, so the deletion is remembered until this handle is dropped.
        self.note_session_title_mirror_deleted(conversation_id);
        catalog::delete(self, conversation_id)?;
        self.mark_active_delete_journal_committed(conversation_id)?;
        // Payload and catalog deletion have committed. Startup replay can
        // safely finish a retained journal, so cleanup must not fail delete.
        let _ = self.remove_archive_recovery_journal(conversation_id);
        Ok(removed_directory || removed_legacy || removed_name)
    }

    /// Forks an existing conversation into a new conversation id.
    ///
    /// Returns a conflict error when the target already exists and an invalid
    /// state error when the source conversation has no entries.
    pub fn fork(
        &self,
        source_conversation_id: &str,
        target_conversation_id: &str,
        created_at_unix_seconds: u64,
    ) -> Result<ConversationSummary> {
        validate_conversation_id(target_conversation_id)?;
        if self.conversation_exists(target_conversation_id)? {
            return Err(MezError::conflict("target conversation already exists"));
        }
        let entries = self.inspect(source_conversation_id)?;
        if entries.is_empty() {
            return Err(MezError::invalid_state(
                "source conversation has no entries",
            ));
        }
        let fork_result = (|| {
            for entry in entries {
                let forked = TranscriptEntry {
                    conversation_id: target_conversation_id.to_string(),
                    created_at_unix_seconds,
                    ..entry
                };
                self.append(&forked)?;
            }
            for presentation in self.inspect_presentation(source_conversation_id)? {
                let forked = AgentPresentationEntry {
                    conversation_id: target_conversation_id.to_string(),
                    created_at_unix_seconds,
                    ..presentation
                };
                self.append_presentation(&forked)?;
            }
            self.saved_session(target_conversation_id)?
                .map(|session| session.summary)
                .ok_or_else(|| MezError::invalid_state("forked conversation summary missing"))
        })();
        if fork_result.is_err()
            && let Err(cleanup_error) = self.delete(target_conversation_id)
        {
            return Err(MezError::invalid_state(format!(
                "conversation fork failed and target cleanup failed: {cleanup_error}"
            )));
        }
        fork_result
    }

    /// Appends one submitted agent prompt to the bounded shared history file.
    #[cfg(test)]
    pub fn append_prompt_history(&self, conversation_id: &str, prompt: &str) -> Result<bool> {
        self.append_structured_prompt_history(
            conversation_id,
            &ReadlineHistoryEntry::literal(prompt),
        )
    }

    /// Appends one submitted agent prompt together with collapsed-paste provenance.
    pub fn append_structured_prompt_history(
        &self,
        conversation_id: &str,
        prompt: &ReadlineHistoryEntry,
    ) -> Result<bool> {
        validate_conversation_id(conversation_id)?;
        if prompt.text.trim().is_empty()
            || prompt.text.len() > mez_mux::readline::MAX_READLINE_HISTORY_ENTRY_BYTES
            || !prompt.is_valid()
        {
            return Ok(false);
        }
        let _lock = self.acquire_prompt_history_lock()?;
        self.migrate_prompt_history_locked()?;
        let path = self.prompt_history_path();
        if Self::latest_structured_prompt_history_entry(&path)?.as_ref() == Some(prompt) {
            self.compact_prompt_history_if_needed()?;
            return Ok(false);
        }
        if Self::latest_structured_prompt_history_entry(&path)?
            .as_ref()
            .is_some_and(|latest| latest.text == prompt.text)
        {
            let mut prompts = self.read_structured_prompt_history_file()?;
            if let Some(latest) = prompts.last_mut() {
                *latest = prompt.clone();
            }
            self.write_structured_prompt_history(prompts)?;
            return Ok(true);
        }
        let encoded = encode_structured_prompt_history_entry(prompt)?;
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        file.write_all(encoded.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        set_private_file_permissions(&path)?;
        self.compact_prompt_history_if_needed()?;
        Ok(true)
    }

    /// Appends one submitted primary command prompt to its bounded shared
    /// history file.
    #[cfg(test)]
    pub fn append_command_prompt_history(&self, command: &str) -> Result<bool> {
        self.append_structured_command_prompt_history(&ReadlineHistoryEntry::literal(command))
    }

    /// Appends one command prompt together with collapsed-paste provenance.
    pub fn append_structured_command_prompt_history(
        &self,
        command: &ReadlineHistoryEntry,
    ) -> Result<bool> {
        if command.text.trim().is_empty() || !command.is_valid() {
            return Ok(false);
        }
        let mut commands = self.structured_command_prompt_history()?;
        if !append_structured_history_entry(&mut commands, command.clone()) {
            return Ok(false);
        }
        self.write_structured_command_prompt_history(commands)?;
        Ok(true)
    }

    /// Appends one submitted agent prompt through Tokio filesystem I/O.
    #[cfg(test)]
    pub async fn append_prompt_history_async(
        &self,
        conversation_id: &str,
        prompt: &str,
    ) -> Result<bool> {
        self.append_structured_prompt_history_async(
            conversation_id,
            ReadlineHistoryEntry::literal(prompt),
        )
        .await
    }

    /// Appends one structured agent prompt through Tokio filesystem I/O.
    pub async fn append_structured_prompt_history_async(
        &self,
        conversation_id: &str,
        prompt: ReadlineHistoryEntry,
    ) -> Result<bool> {
        validate_conversation_id(conversation_id)?;
        if prompt.text.trim().is_empty() {
            return Ok(false);
        }
        let store = self.clone();
        let conversation_id = conversation_id.to_string();
        tokio::task::spawn_blocking(move || {
            store.append_structured_prompt_history(&conversation_id, &prompt)
        })
        .await
        .map_err(|error| {
            MezError::invalid_state(format!("prompt-history persistence task failed: {error}"))
        })?
    }

    /// Appends one submitted primary command prompt through Tokio filesystem
    /// I/O.
    #[cfg(test)]
    pub async fn append_command_prompt_history_async(&self, command: &str) -> Result<bool> {
        self.append_structured_command_prompt_history_async(ReadlineHistoryEntry::literal(command))
            .await
    }

    /// Appends one structured command prompt through Tokio filesystem I/O.
    pub async fn append_structured_command_prompt_history_async(
        &self,
        command: ReadlineHistoryEntry,
    ) -> Result<bool> {
        if command.text.trim().is_empty() || !command.is_valid() {
            return Ok(false);
        }
        let mut commands = self.structured_command_prompt_history_async().await?;
        if !append_structured_history_entry(&mut commands, command) {
            return Ok(false);
        }
        self.write_structured_command_prompt_history_async(commands)
            .await?;
        Ok(true)
    }

    /// Reads bounded submitted prompt history shared by all conversations.
    pub fn prompt_history(&self, conversation_id: &str) -> Result<Vec<String>> {
        Ok(self
            .structured_prompt_history(conversation_id)?
            .into_iter()
            .map(|entry| entry.text)
            .collect())
    }

    /// Reads bounded prompt history with collapsed-paste provenance.
    pub fn structured_prompt_history(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<ReadlineHistoryEntry>> {
        validate_conversation_id(conversation_id)?;
        let _lock = self.acquire_prompt_history_lock()?;
        self.migrate_prompt_history_locked()?;
        self.read_structured_prompt_history_file()
    }

    /// Reads bounded submitted primary command prompt history.
    #[cfg(test)]
    pub fn command_prompt_history(&self) -> Result<Vec<String>> {
        Ok(self
            .structured_command_prompt_history()?
            .into_iter()
            .map(|entry| entry.text)
            .collect())
    }

    /// Reads command prompt history with collapsed-paste provenance.
    pub fn structured_command_prompt_history(&self) -> Result<Vec<ReadlineHistoryEntry>> {
        let path = self.command_prompt_history_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut data = String::new();
        std_fs::File::open(path)?.read_to_string(&mut data)?;
        let commands = data
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(decode_structured_prompt_history_entry)
            .collect::<Result<Vec<_>>>()?;
        Ok(canonicalize_structured_history(commands))
    }

    /// Reads bounded submitted primary command prompt history through Tokio
    /// filesystem I/O.
    #[cfg(test)]
    pub async fn command_prompt_history_async(&self) -> Result<Vec<String>> {
        Ok(self
            .structured_command_prompt_history_async()
            .await?
            .into_iter()
            .map(|entry| entry.text)
            .collect())
    }

    /// Reads structured command prompt history through Tokio filesystem I/O.
    pub async fn structured_command_prompt_history_async(
        &self,
    ) -> Result<Vec<ReadlineHistoryEntry>> {
        let path = self.command_prompt_history_path();
        let mut data = String::new();
        match tokio_fs::File::open(path).await {
            Ok(mut file) => {
                file.read_to_string(&mut data).await?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error.into()),
        }
        let commands = data
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(decode_structured_prompt_history_entry)
            .collect::<Result<Vec<_>>>()?;
        Ok(canonicalize_structured_history(commands))
    }

    /// Returns the shared prompt-history file path after validating the caller.
    pub fn prompt_history_file(&self, conversation_id: &str) -> Result<PathBuf> {
        validate_conversation_id(conversation_id)?;
        Ok(self.prompt_history_path())
    }

    /// Returns the shared primary command prompt history file path.
    pub fn command_prompt_history_file(&self) -> PathBuf {
        self.command_prompt_history_path()
    }

    /// Returns the durable active agent-session metadata file path.
    #[cfg(test)]
    pub fn agent_session_metadata_file(&self) -> PathBuf {
        self.agent_session_metadata_path()
    }

    /// Returns the directory for one persisted agent session.
    #[cfg(test)]
    pub fn session_dir(&self, conversation_id: &str) -> Result<PathBuf> {
        self.session_dir_for(conversation_id)
    }

    /// Returns the transcript path for one persisted agent session.
    pub fn transcript_path(&self, conversation_id: &str) -> Result<PathBuf> {
        self.transcript_path_for(conversation_id)
    }

    /// Returns the presentation path for one persisted agent session.
    pub fn presentation_path(&self, conversation_id: &str) -> Result<PathBuf> {
        self.presentation_path_for(conversation_id)
    }

    /// Returns the compressed presentation-history path for one persisted agent session.
    #[cfg(test)]
    pub fn presentation_compressed_path(&self, conversation_id: &str) -> Result<PathBuf> {
        self.presentation_compressed_path_for(conversation_id)
    }

    /// Rewrites shared prompt history while preserving paste provenance.
    fn write_structured_prompt_history(
        &self,
        prompts: impl IntoIterator<Item = ReadlineHistoryEntry>,
    ) -> Result<()> {
        self.ensure_store_dir()?;
        let path = self.prompt_history_path();
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        for prompt in prompts {
            if prompt.text.is_empty() {
                continue;
            }
            file.write_all(encode_structured_prompt_history_entry(&prompt)?.as_bytes())?;
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
        set_private_file_permissions(&path)?;
        Ok(())
    }

    /// Rewrites command prompt history while preserving paste provenance.
    fn write_structured_command_prompt_history(
        &self,
        commands: impl IntoIterator<Item = ReadlineHistoryEntry>,
    ) -> Result<()> {
        self.ensure_store_dir()?;
        let path = self.command_prompt_history_path();
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        for command in commands {
            if command.text.is_empty() {
                continue;
            }
            file.write_all(encode_structured_prompt_history_entry(&command)?.as_bytes())?;
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
        set_private_file_permissions(&path)?;
        Ok(())
    }

    /// Updates the per-conversation summary sidecar after one transcript append.
    fn update_summary_after_append(&self, entry: &TranscriptEntry) -> Result<()> {
        let mut summary = match self.read_summary_sidecar(&entry.conversation_id)? {
            Some(summary) => summary,
            None => self
                .legacy_bounded_summary(&entry.conversation_id)?
                .unwrap_or_else(|| {
                    summarize_conversation(vec![entry.clone()])
                        .expect("single valid transcript entry summarizes")
                }),
        };
        summary.conversation_id = entry.conversation_id.clone();
        summary.entries = usize::try_from(entry.sequence).unwrap_or(usize::MAX);
        if summary.first_created_at_unix_seconds == 0 {
            summary.first_created_at_unix_seconds = entry.created_at_unix_seconds;
        }
        summary.last_created_at_unix_seconds = entry.created_at_unix_seconds;
        summary.last_turn_id = entry.turn_id.clone();
        summary.agent_id = entry.agent_id.clone();
        summary.pane_id = entry.pane_id.clone();
        if summary.directory.is_none() {
            summary.directory = transcript_entry_directory(entry);
        } else if let Some(directory) = transcript_entry_project_root(entry) {
            summary.directory = Some(directory);
        }
        if entry.role == TranscriptRole::User {
            let preview = bounded_summary_text(&entry.content, 120);
            if summary.initial_prompt.is_none() {
                summary.initial_prompt = Some(preview.clone());
            }
            summary.latest_user_prompt = Some(preview);
        }
        self.write_summary_sidecar(&summary)
    }

    /// Writes one summary sidecar for saved-session listing and latest lookup.
    fn write_summary_sidecar(&self, summary: &ConversationSummary) -> Result<()> {
        let session_dir = self.ensure_session_dir(&summary.conversation_id)?;
        let path = session_dir.join(SESSION_SUMMARY_FILE_NAME);
        let encoded = encode_conversation_summary(summary);
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        file.write_all(encoded.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        set_private_file_permissions(&path)?;
        Ok(())
    }

    /// Reads one summary sidecar when present.
    fn read_summary_sidecar(&self, conversation_id: &str) -> Result<Option<ConversationSummary>> {
        let path = self
            .session_dir_for(conversation_id)?
            .join(SESSION_SUMMARY_FILE_NAME);
        if !path.exists() {
            return Ok(None);
        }
        let mut data = String::new();
        std_fs::File::open(path)?.read_to_string(&mut data)?;
        let Some(line) = data.lines().find(|line| !line.trim().is_empty()) else {
            return Ok(None);
        };
        decode_conversation_summary(line).map(Some)
    }

    /// Builds a summary for older conversations without decoding the whole file.
    fn legacy_bounded_summary(&self, conversation_id: &str) -> Result<Option<ConversationSummary>> {
        let path = self.existing_transcript_path_for(conversation_id)?;
        if !path.exists() {
            return Ok(None);
        }
        let first = self.first_transcript_entry(conversation_id)?;
        let tail = self.inspect_recent(conversation_id, 64, DEFAULT_TRANSCRIPT_TAIL_READ_BYTES)?;
        let Some(last) = tail.last().or(first.as_ref()) else {
            return Ok(None);
        };
        let first_entry = first.as_ref().unwrap_or(last);
        let mut directory = first.as_ref().and_then(transcript_entry_directory);
        for entry in &tail {
            if let Some(project_root) = transcript_entry_project_root(entry) {
                directory = Some(project_root);
            } else if directory.is_none() {
                directory = transcript_entry_directory(entry);
            }
        }
        let initial_prompt = first
            .as_ref()
            .filter(|entry| entry.role == TranscriptRole::User)
            .map(|entry| bounded_summary_text(&entry.content, 120));
        let latest_user_prompt = tail
            .iter()
            .rev()
            .find(|entry| entry.role == TranscriptRole::User)
            .map(|entry| bounded_summary_text(&entry.content, 120))
            .or_else(|| initial_prompt.clone());
        Ok(Some(ConversationSummary {
            conversation_id: conversation_id.to_string(),
            entries: usize::try_from(last.sequence).unwrap_or(usize::MAX),
            first_created_at_unix_seconds: first_entry.created_at_unix_seconds,
            last_created_at_unix_seconds: last.created_at_unix_seconds,
            last_turn_id: last.turn_id.clone(),
            agent_id: last.agent_id.clone(),
            pane_id: last.pane_id.clone(),
            directory,
            initial_prompt,
            latest_user_prompt,
        }))
    }

    /// Reads the first complete transcript entry for legacy summary fallback.
    fn first_transcript_entry(&self, conversation_id: &str) -> Result<Option<TranscriptEntry>> {
        let path = self.existing_transcript_path_for(conversation_id)?;
        if !path.exists() {
            return Ok(None);
        }
        let file = std_fs::File::open(path)?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                return Ok(None);
            }
            if !line.trim().is_empty() {
                return decode_transcript_entry(line.trim_end_matches(['\r', '\n'])).map(Some);
            }
        }
    }

    /// Writes the latest presentation sequence index.
    fn write_presentation_index(&self, entry: &AgentPresentationEntry) -> Result<()> {
        let session_dir = self.ensure_session_dir(&entry.conversation_id)?;
        let path = session_dir.join(SESSION_PRESENTATION_INDEX_FILE_NAME);
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        file.write_all(entry.sequence.to_string().as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        set_private_file_permissions(&path)?;
        Ok(())
    }

    /// Reads the latest presentation sequence index when present.
    fn read_presentation_index(&self, conversation_id: &str) -> Result<Option<u64>> {
        validate_conversation_id(conversation_id)?;
        let path = self
            .session_dir_for(conversation_id)?
            .join(SESSION_PRESENTATION_INDEX_FILE_NAME);
        if !path.exists() {
            return Ok(None);
        }
        let mut data = String::new();
        std_fs::File::open(path)?.read_to_string(&mut data)?;
        let value = data.trim();
        if value.is_empty() {
            return Ok(None);
        }
        value.parse::<u64>().map(Some).map_err(|error| {
            MezError::invalid_args(format!("presentation index is invalid: {error}"))
        })
    }

    /// Streams presentation rows to recover only the first and latest entries.
    fn presentation_migration_summary(
        &self,
        conversation_id: &str,
        named: Option<&NamedAgentSession>,
    ) -> Result<Option<ConversationSummary>> {
        let mut first = None;
        let mut last = None;
        let compressed_path = self.presentation_compressed_path_for(conversation_id)?;
        if compressed_path.is_file() {
            let file = std_fs::File::open(&compressed_path)?;
            let decoder = zstd::stream::read::Decoder::new(file).map_err(|error| {
                MezError::invalid_args(format!(
                    "presentation compressed history decode failed: {error}"
                ))
            })?;
            Self::read_presentation_bounds(BufReader::new(decoder), &mut first, &mut last)?;
        }
        let cleartext_path = self.presentation_path_for(conversation_id)?;
        if cleartext_path.is_file() {
            Self::read_presentation_bounds(
                BufReader::new(std_fs::File::open(cleartext_path)?),
                &mut first,
                &mut last,
            )?;
        }
        let (Some(first), Some(last)) = (first, last) else {
            return Ok(None);
        };
        Ok(Some(ConversationSummary {
            conversation_id: conversation_id.to_string(),
            entries: 0,
            first_created_at_unix_seconds: first.created_at_unix_seconds,
            last_created_at_unix_seconds: last.created_at_unix_seconds,
            last_turn_id: last.turn_id.unwrap_or_default(),
            agent_id: String::new(),
            pane_id: last.pane_id,
            directory: named.and_then(|session| session.directory.clone()),
            initial_prompt: None,
            latest_user_prompt: None,
        }))
    }

    /// Updates bounded first/latest presentation state from one line reader.
    fn read_presentation_bounds(
        mut reader: impl BufRead,
        first: &mut Option<AgentPresentationEntry>,
        last: &mut Option<AgentPresentationEntry>,
    ) -> Result<()> {
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.trim().is_empty() {
                continue;
            }
            let entry = AgentPresentationEntry::decode(line)?;
            if first.is_none() {
                *first = Some(entry.clone());
            }
            *last = Some(entry);
        }
    }

    /// Enforces active saved-session age and count retention in bounded batches.
    ///
    /// Age expiry is inclusive at the cutoff and completes before count
    /// enforcement. Protected durable conversations and archived rows are never
    /// deleted. Independent deletion failures are reported while later
    /// candidates continue.
    pub fn enforce_saved_session_retention(
        &self,
        now_unix_seconds: u64,
        protected_conversation_ids: &BTreeSet<String>,
    ) -> Result<SavedSessionRetentionReport> {
        const RETENTION_BATCH_LIMIT: usize = 256;
        const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

        let retention_seconds = self
            .saved_session_retention
            .retention_days
            .saturating_mul(SECONDS_PER_DAY);
        let cutoff_unix_seconds = now_unix_seconds.saturating_sub(retention_seconds);
        let mut excluded = protected_conversation_ids.clone();
        let mut report = SavedSessionRetentionReport::default();

        loop {
            let candidates = catalog::age_retention_candidates(
                self,
                cutoff_unix_seconds,
                &excluded,
                RETENTION_BATCH_LIMIT,
            )?;
            if candidates.is_empty() {
                break;
            }
            self.delete_retention_candidates(
                candidates,
                protected_conversation_ids,
                &mut excluded,
                &mut report,
            )?;
        }

        loop {
            let active_count = catalog::active_payload_session_count(self)?;
            let excess =
                active_count.saturating_sub(self.saved_session_retention.max_active_sessions);
            if excess == 0 {
                break;
            }
            let candidates = catalog::count_retention_candidates(
                self,
                &excluded,
                excess.min(RETENTION_BATCH_LIMIT),
            )?;
            if candidates.is_empty() {
                break;
            }
            self.delete_retention_candidates(
                candidates,
                protected_conversation_ids,
                &mut excluded,
                &mut report,
            )?;
        }
        Ok(report)
    }

    /// Rechecks and deletes one bounded retention candidate batch.
    fn delete_retention_candidates(
        &self,
        candidates: Vec<String>,
        protected_conversation_ids: &BTreeSet<String>,
        excluded: &mut BTreeSet<String>,
        report: &mut SavedSessionRetentionReport,
    ) -> Result<()> {
        for conversation_id in candidates {
            excluded.insert(conversation_id.clone());
            let deletion = (|| {
                let _conversation_lock = self.acquire_conversation_lock(&conversation_id)?;
                if protected_conversation_ids.contains(&conversation_id) {
                    return Ok(false);
                }
                let Some(record) = catalog::record(self, &conversation_id)? else {
                    return Ok(false);
                };
                if record.session.archived_at_unix_seconds.is_some()
                    || (!record.has_transcript && !record.has_presentation)
                {
                    return Ok(false);
                }
                self.delete_locked(&conversation_id)
            })();
            match deletion {
                Ok(true) => report.deleted_conversation_ids.push(conversation_id),
                Ok(false) => {}
                Err(error) => report.failures.push(SavedSessionRetentionFailure {
                    conversation_id,
                    error: error.message().to_string(),
                }),
            }
        }
        Ok(())
    }

    /// Rewrites structured command prompt history through Tokio filesystem I/O.
    async fn write_structured_command_prompt_history_async(
        &self,
        commands: impl IntoIterator<Item = ReadlineHistoryEntry>,
    ) -> Result<()> {
        self.ensure_store_dir_async().await?;
        let path = self.command_prompt_history_path();
        let mut file = TokioOpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .await?;
        for command in commands {
            if command.text.is_empty() {
                continue;
            }
            file.write_all(encode_structured_prompt_history_entry(&command)?.as_bytes())
                .await?;
            file.write_all(b"\n").await?;
        }
        file.sync_all().await?;
        set_private_file_permissions_async(&path).await?;
        Ok(())
    }

    /// Runs the ensure store dir operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn ensure_store_dir(&self) -> Result<()> {
        std_fs::create_dir_all(&self.root)?;
        set_private_dir_permissions(&self.root)?;
        Ok(())
    }

    /// Runs the ensure store dir async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn ensure_store_dir_async(&self) -> Result<()> {
        tokio_fs::create_dir_all(&self.root).await?;
        set_private_dir_permissions_async(&self.root).await?;
        Ok(())
    }

    /// Runs the ensure session dir operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn ensure_session_dir(&self, conversation_id: &str) -> Result<PathBuf> {
        self.ensure_store_dir()?;
        let session_dir = self.session_dir_for(conversation_id)?;
        std_fs::create_dir_all(&session_dir)?;
        set_private_dir_permissions(&session_dir)?;
        Ok(session_dir)
    }

    /// Runs the conversation exists operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn conversation_exists(&self, conversation_id: &str) -> Result<bool> {
        Ok(self.transcript_path_for(conversation_id)?.exists()
            || self.legacy_transcript_path_for(conversation_id)?.exists()
            || self.session_dir_for(conversation_id)?.exists())
    }

    /// Runs the existing transcript path for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn existing_transcript_path_for(&self, conversation_id: &str) -> Result<PathBuf> {
        let path = self.transcript_path_for(conversation_id)?;
        if path.exists() {
            return Ok(path);
        }
        let legacy_path = self.legacy_transcript_path_for(conversation_id)?;
        if legacy_path.exists() {
            return Ok(legacy_path);
        }
        Ok(path)
    }

    /// Runs the session dir for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn session_dir_for(&self, conversation_id: &str) -> Result<PathBuf> {
        validate_conversation_id(conversation_id)?;
        Ok(self.root.join(conversation_id))
    }

    /// Runs the transcript path for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn transcript_path_for(&self, conversation_id: &str) -> Result<PathBuf> {
        Ok(self
            .session_dir_for(conversation_id)?
            .join(SESSION_TRANSCRIPT_FILE_NAME))
    }

    /// Returns the durable conversation metadata sidecar path.
    fn conversation_metadata_path_for(&self, conversation_id: &str) -> Result<PathBuf> {
        Ok(self
            .session_dir_for(conversation_id)?
            .join(SESSION_METADATA_FILE_NAME))
    }

    /// Runs the presentation path for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    /// Reads one versioned conversation metadata record without taking a lock.
    fn read_conversation_metadata(&self, conversation_id: &str) -> Result<ConversationMetadata> {
        let path = self.conversation_metadata_path_for(conversation_id)?;
        if !path.exists() {
            return Ok(ConversationMetadata {
                version: SESSION_METADATA_VERSION,
                conversation_kind: "root".to_string(),
                user_objective: None,
                parent_objective: None,
                parent_conversation_id: None,
                subagent_lifetime: mez_agent::SubagentLifetime::Task,
                subagent_scope: None,
                allowed_actions: None,
                subagent_lineage: None,
            });
        }
        let data = std_fs::read(&path)?;
        let metadata = serde_json::from_slice::<ConversationMetadata>(&data).map_err(|error| {
            MezError::invalid_args(format!("conversation metadata decode failed: {error}"))
        })?;
        match metadata.version {
            1 | 2 | SESSION_METADATA_VERSION => {}
            _ => {
                return Err(MezError::invalid_args(
                    "unsupported conversation metadata version",
                ));
            }
        }
        if let Some(objective) = metadata.user_objective.as_deref() {
            mez_agent::messaging::normalize_objective(objective)?;
        }
        if let Some(objective) = metadata.parent_objective.as_deref() {
            mez_agent::messaging::normalize_objective(objective)?;
        }
        validate_conversation_metadata_contract(&metadata)?;
        Ok(metadata)
    }

    /// Reports whether a legacy child sidecar lacks the durable contract needed
    /// to resume it without granting authority it cannot prove.
    pub(super) fn is_unrestorable_legacy_subagent(&self, conversation_id: &str) -> Result<bool> {
        let metadata = self.read_conversation_metadata(conversation_id)?;
        Ok(metadata.version == 1
            && metadata.conversation_kind == "subagent"
            && (metadata.subagent_lineage.is_none() || metadata.allowed_actions.is_none()))
    }

    /// Reads metadata while the caller owns the conversation mutation lock.
    fn read_conversation_metadata_locked(
        &self,
        conversation_id: &str,
    ) -> Result<ConversationMetadata> {
        self.read_conversation_metadata(conversation_id)
    }

    /// Atomically replaces metadata while the caller owns the conversation lock.
    fn write_conversation_metadata_locked(
        &self,
        conversation_id: &str,
        metadata: &ConversationMetadata,
    ) -> Result<()> {
        self.retire_stale_active_delete_journal(conversation_id)?;
        let promotion = self.promote_legacy_transcript_locked(conversation_id)?;
        let session_dir = self.session_dir_for(conversation_id)?;
        let path = session_dir.join(SESSION_METADATA_FILE_NAME);
        let temp_path = session_dir.join(".metadata.json.tmp");
        let mut metadata = metadata.clone();
        metadata.version = SESSION_METADATA_VERSION;
        let write_result = (|| {
            self.ensure_session_dir(conversation_id)?;
            if let Some(promotion) = promotion.as_ref() {
                #[cfg(test)]
                if self
                    .fail_legacy_promotion_permissions_after_rename
                    .swap(false, Ordering::SeqCst)
                {
                    return Err(MezError::invalid_state(
                        "injected legacy promotion permission failure after rename",
                    ));
                }
                set_private_file_permissions(&promotion.transcript_path)?;
            }
            #[cfg(test)]
            if self
                .fail_metadata_write_after_promotion
                .swap(false, Ordering::SeqCst)
            {
                return Err(MezError::invalid_state(
                    "injected metadata write failure after legacy promotion",
                ));
            }
            let encoded = serde_json::to_vec(&metadata).map_err(|error| {
                MezError::invalid_args(format!("conversation metadata encode failed: {error}"))
            })?;
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temp_path)?;
            file.write_all(&encoded)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            set_private_file_permissions(&temp_path)?;
            std_fs::rename(&temp_path, &path)?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = std_fs::remove_file(&temp_path);
            if let Some(promotion) = promotion {
                self.rollback_legacy_transcript_promotion_locked(&promotion)?;
                self.upsert_catalog_from_files(conversation_id, None)?;
                self.remove_archive_recovery_journal(conversation_id)?;
            }
            return Err(error);
        }
        Ok(())
    }

    /// Restores the exact metadata sidecar state from before a compound write.
    fn restore_conversation_metadata_snapshot_locked(
        &self,
        conversation_id: &str,
        metadata_path: &Path,
        previous_metadata: Option<&[u8]>,
    ) -> Result<()> {
        match previous_metadata {
            Some(previous_metadata) => {
                let temporary_path = self
                    .session_dir_for(conversation_id)?
                    .join(".metadata.json.rollback");
                let mut file = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&temporary_path)?;
                file.write_all(previous_metadata)?;
                file.sync_all()?;
                set_private_file_permissions(&temporary_path)?;
                std_fs::rename(temporary_path, metadata_path)?;
                set_private_file_permissions(metadata_path)?;
            }
            None if metadata_path.exists() => std_fs::remove_file(metadata_path)?,
            None => {}
        }
        Ok(())
    }

    /// Promotes a legacy root TSV into the directory payload before metadata is written.
    ///
    /// Metadata sidecars require a session directory. Moving the legacy payload
    /// into that directory as one locked transition ensures archive, restore,
    /// and deletion observe one complete payload rather than split transcript
    /// and metadata state.
    fn promote_legacy_transcript_locked(
        &self,
        conversation_id: &str,
    ) -> Result<Option<LegacyTranscriptPromotion>> {
        let legacy_path = self.legacy_transcript_path_for(conversation_id)?;
        if !legacy_path.is_file() {
            return Ok(None);
        }
        let session_dir = self.ensure_session_dir(conversation_id)?;
        let transcript_path = session_dir.join(SESSION_TRANSCRIPT_FILE_NAME);
        if transcript_path.exists() {
            return Err(MezError::conflict(
                "legacy and directory transcript payloads both exist during metadata promotion",
            ));
        }
        self.write_archive_recovery_journal(
            conversation_id,
            ArchiveRecoveryOperation::PromoteLegacy,
            CatalogPayloadLayout::Directory,
        )?;
        std_fs::rename(&legacy_path, &transcript_path)?;
        Ok(Some(LegacyTranscriptPromotion {
            legacy_path,
            transcript_path,
        }))
    }

    /// Restores a promoted legacy transcript after its metadata transaction fails.
    fn rollback_legacy_transcript_promotion_locked(
        &self,
        promotion: &LegacyTranscriptPromotion,
    ) -> Result<()> {
        if promotion.transcript_path.is_file() && !promotion.legacy_path.exists() {
            std_fs::rename(&promotion.transcript_path, &promotion.legacy_path)?;
            set_private_file_permissions(&promotion.legacy_path)?;
        }
        let Some(session_dir) = promotion.transcript_path.parent() else {
            return Err(MezError::invalid_state(
                "promoted transcript path has no session directory",
            ));
        };
        if session_dir.is_dir() && std_fs::read_dir(session_dir)?.next().is_none() {
            std_fs::remove_dir(session_dir)?;
        }
        Ok(())
    }

    fn presentation_path_for(&self, conversation_id: &str) -> Result<PathBuf> {
        Ok(self
            .session_dir_for(conversation_id)?
            .join(SESSION_PRESENTATION_FILE_NAME))
    }

    /// Runs the compressed presentation path operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn presentation_compressed_path_for(&self, conversation_id: &str) -> Result<PathBuf> {
        Ok(self
            .session_dir_for(conversation_id)?
            .join(SESSION_PRESENTATION_COMPRESSED_FILE_NAME))
    }

    /// Runs the prompt history path operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn prompt_history_path(&self) -> PathBuf {
        self.root.join(SHARED_PROMPT_HISTORY_FILE_NAME)
    }

    /// Acquires the process-wide advisory lock for shared prompt history.
    fn acquire_prompt_history_lock(&self) -> Result<std_fs::File> {
        self.ensure_store_dir()?;
        let path = self.root.join(SHARED_PROMPT_HISTORY_LOCK_FILE_NAME);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        set_private_file_permissions(&path)?;
        flock(&file, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
        Ok(file)
    }

    /// Imports histories written by the temporary conversation-scoped layout.
    fn migrate_prompt_history_locked(&self) -> Result<()> {
        let marker = self.root.join(SHARED_PROMPT_HISTORY_MIGRATION_FILE_NAME);
        if marker.exists() {
            return Ok(());
        }
        let mut prompts = self.read_structured_prompt_history_file()?;
        let mut legacy_paths = std_fs::read_dir(&self.root)?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.is_dir())
            .map(|path| path.join(SHARED_PROMPT_HISTORY_FILE_NAME))
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        legacy_paths.sort();
        for path in legacy_paths {
            prompts.extend(Self::read_structured_prompt_history_path(&path)?);
        }
        prompts = canonicalize_structured_history(prompts);
        if !prompts.is_empty() {
            self.write_structured_prompt_history(prompts)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&marker)?;
        file.write_all(b"mez-agent-prompt-history-shared/1\n")?;
        file.sync_all()?;
        set_private_file_permissions(&marker)?;
        Ok(())
    }

    /// Reads the shared history while retaining collapsed-paste provenance.
    fn read_structured_prompt_history_file(&self) -> Result<Vec<ReadlineHistoryEntry>> {
        Self::read_structured_prompt_history_path(&self.prompt_history_path())
    }

    /// Reads only the bounded tail needed for adjacent duplicate suppression.
    fn latest_structured_prompt_history_entry(
        path: &std::path::Path,
    ) -> Result<Option<ReadlineHistoryEntry>> {
        let mut file = match std_fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let len = file.metadata()?.len();
        let start = len.saturating_sub(PROMPT_HISTORY_TAIL_READ_BYTES);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let bytes = if start == 0 {
            bytes.as_slice()
        } else if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
            &bytes[newline.saturating_add(1)..]
        } else {
            &[]
        };
        let text = std::str::from_utf8(bytes)
            .map_err(|_| MezError::invalid_args("prompt history tail is not valid UTF-8"))?;
        text.lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .map(decode_structured_prompt_history_entry)
            .transpose()
    }

    /// Rewrites prompt history only after its append-only file crosses a bound.
    fn compact_prompt_history_if_needed(&self) -> Result<()> {
        let path = self.prompt_history_path();
        if !path.exists() || path.metadata()?.len() <= PROMPT_HISTORY_COMPACTION_BYTES {
            return Ok(());
        }
        self.write_structured_prompt_history(self.read_structured_prompt_history_file()?)
    }

    /// Reads one prompt-history path while retaining collapsed-paste provenance.
    fn read_structured_prompt_history_path(
        path: &std::path::Path,
    ) -> Result<Vec<ReadlineHistoryEntry>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut data = String::new();
        std_fs::File::open(path)?.read_to_string(&mut data)?;
        let prompts = data
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(decode_structured_prompt_history_entry)
            .collect::<Result<Vec<_>>>()?;
        Ok(canonicalize_structured_history(prompts))
    }

    /// Runs the command prompt history path operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn command_prompt_history_path(&self) -> PathBuf {
        self.root.join(SHARED_COMMAND_PROMPT_HISTORY_FILE_NAME)
    }

    /// Runs the agent session metadata path operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn agent_session_metadata_path(&self) -> PathBuf {
        self.root.join(ACTIVE_AGENT_SESSION_METADATA_FILE_NAME)
    }

    /// Acquires the exclusive advisory lock for named-session index mutation.
    fn acquire_named_sessions_lock(&self) -> Result<std_fs::File> {
        self.ensure_store_dir()?;
        let path = self.root.join(NAMED_AGENT_SESSIONS_LOCK_FILE_NAME);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        set_private_file_permissions(&path)?;
        flock(&file, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
        Ok(file)
    }

    /// Reads and validates the complete named-session index.
    fn read_named_sessions_index(&self) -> Result<BTreeMap<String, NamedAgentSession>> {
        let path = self.root.join(NAMED_AGENT_SESSIONS_FILE_NAME);
        if !path.exists() {
            return Ok(BTreeMap::new());
        }
        let mut data = String::new();
        std_fs::File::open(path)?.read_to_string(&mut data)?;
        let value: serde_json::Value = serde_json::from_str(&data).map_err(|error| {
            MezError::invalid_args(format!("named-session index decode failed: {error}"))
        })?;
        if value.get("version").and_then(serde_json::Value::as_u64)
            != Some(NAMED_AGENT_SESSIONS_VERSION)
        {
            return Err(MezError::invalid_args(
                "named-session index version is unsupported",
            ));
        }
        let sessions: Vec<NamedAgentSession> =
            serde_json::from_value(value.get("sessions").cloned().ok_or_else(|| {
                MezError::invalid_args("named-session index sessions are missing")
            })?)
            .map_err(|error| {
                MezError::invalid_args(format!("named-session index records are invalid: {error}"))
            })?;
        let mut indexed = BTreeMap::new();
        for mut session in sessions {
            validate_conversation_id(&session.conversation_id)?;
            session.name = validate_agent_session_name(&session.name)?;
            if indexed
                .insert(session.conversation_id.clone(), session)
                .is_some()
            {
                return Err(MezError::invalid_args(
                    "named-session index contains duplicate conversations",
                ));
            }
        }
        Ok(indexed)
    }

    /// Atomically replaces the durable named-session index.
    fn write_named_sessions_index(
        &self,
        sessions: &BTreeMap<String, NamedAgentSession>,
    ) -> Result<()> {
        self.ensure_store_dir()?;
        let path = self.root.join(NAMED_AGENT_SESSIONS_FILE_NAME);
        let temp_path = self.root.join(".named-sessions.json.tmp");
        let encoded = serde_json::to_vec(&serde_json::json!({
            "version": NAMED_AGENT_SESSIONS_VERSION,
            "sessions": sessions.values().collect::<Vec<_>>(),
        }))
        .map_err(|error| {
            MezError::invalid_args(format!("named-session index encode failed: {error}"))
        })?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temp_path)?;
        file.write_all(&encoded)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        set_private_file_permissions(&temp_path)?;
        std_fs::rename(&temp_path, &path)?;
        set_private_file_permissions(&path)?;
        Ok(())
    }

    /// Removes one durable name record while preserving all other names.
    pub(super) fn remove_named_session(&self, conversation_id: &str) -> Result<bool> {
        let _lock = self.acquire_named_sessions_lock()?;
        let mut sessions = self.read_named_sessions_index()?;
        let removed = sessions.remove(conversation_id).is_some();
        if removed {
            self.write_named_sessions_index(&sessions)?;
        }
        Ok(removed)
    }

    /// Persists one bounded mirror of the published objective for a conversation.
    ///
    /// Returns true only when the stored mirror changed. The value is bounded by
    /// the shared title rules before it is written, and the mirror is written
    /// only from the published objective value: the published discovery
    /// objective stays the source of truth. The mirror exists so archived and
    /// offline conversations can still resolve a policy-derived title.
    ///
    /// An objective that bounds to nothing retires any retained mirror so
    /// resolution falls through to the first prompt, an unchanged refresh is
    /// answered from this handle's last persisted value without reading the
    /// bounded index, and an unreadable index is treated as empty and rebuilt
    /// from this value.
    pub fn mirror_session_objective(
        &self,
        conversation_id: &str,
        objective: &str,
        updated_at_unix_seconds: u64,
    ) -> Result<bool> {
        validate_conversation_id(conversation_id)?;
        let Some(objective) = bound_session_title(objective) else {
            return self.remove_session_objective_mirror(conversation_id);
        };
        // Unchanged refreshes dominate on the serialized runtime thread, so they
        // are answered from the last value this handle persisted and never read
        // or write the bounded index.
        if self.session_objective_mirror_is_unchanged(conversation_id, &objective) {
            return Ok(false);
        }
        let _lock = self.acquire_session_objective_mirrors_lock()?;
        let mut mirrors = self
            .read_session_objective_mirror_records_for_write()?
            .records;
        mirrors.insert(
            conversation_id.to_string(),
            SessionObjectiveMirror {
                conversation_id: conversation_id.to_string(),
                objective: objective.clone(),
                updated_at_unix_seconds,
            },
        );
        self.compact_session_objective_mirror_records(&mut mirrors, conversation_id);
        self.write_session_objective_mirror_records(&mirrors)?;
        self.note_session_objective_mirror_written(conversation_id, &objective);
        Ok(true)
    }

    /// Removes one conversation's mirror while holding the index lock.
    ///
    /// Returns true when a retained mirror was pruned. An unreadable index is
    /// treated as empty and replaced with a valid empty index, so a delete can no
    /// longer be blocked by a corrupt cache file.
    pub(super) fn remove_session_objective_mirror(&self, conversation_id: &str) -> Result<bool> {
        validate_conversation_id(conversation_id)?;
        self.forget_session_objective_mirror(conversation_id);
        if !self.root.join(SESSION_OBJECTIVE_MIRRORS_FILE_NAME).exists() {
            return Ok(false);
        }
        let _lock = self.acquire_session_objective_mirrors_lock()?;
        let write_read = self.read_session_objective_mirror_records_for_write()?;
        let mut mirrors = write_read.records;
        let removed = mirrors.remove(conversation_id).is_some();
        if removed || write_read.recovered {
            self.write_session_objective_mirror_records(&mirrors)?;
        }
        Ok(removed)
    }

    /// Loads one validated objective title mirror for focused tests.
    #[cfg(test)]
    pub fn session_objective_mirror(
        &self,
        conversation_id: &str,
    ) -> Result<Option<SessionObjectiveMirror>> {
        validate_conversation_id(conversation_id)?;
        Ok(self
            .read_session_objective_mirror_records()?
            .remove(conversation_id))
    }

    /// Attaches persisted objective title mirrors to one bounded row set.
    ///
    /// The index is a cache, so an unreadable or invalid file degrades to no
    /// mirror and every row still renders from its summary and conversation id.
    fn attach_session_objective_titles(&self, sessions: &mut [SavedAgentSession]) {
        if sessions.is_empty() {
            return;
        }
        let mirrors = self
            .read_session_objective_mirror_records()
            .unwrap_or_default();
        let generated = self.read_session_title_mirror_records().unwrap_or_default();
        for session in sessions {
            session.objective_title = mirrors
                .get(&session.summary.conversation_id)
                .map(|record| record.objective.clone());
            session.generated_title = generated
                .get(&session.summary.conversation_id)
                .map(|record| record.title.clone());
        }
    }

    /// Persists one bounded generated display title for one conversation.
    ///
    /// Generated titles live in their own bounded sidecar so the objective
    /// mirror keeps its single-writer invariant of being written only from the
    /// published objective. The sidecar follows the same durability rules as
    /// that mirror: bounded entries, atomic temp+fsync+rename replacement under
    /// an advisory lock, validation on read, and quarantine-and-rebuild recovery
    /// on the next write. A title that bounds to nothing retires any retained
    /// value so the row falls back to the objective-derived title.
    pub fn mirror_session_generated_title(
        &self,
        conversation_id: &str,
        title: &str,
        updated_at_unix_seconds: u64,
    ) -> Result<bool> {
        validate_conversation_id(conversation_id)?;
        // A worker that started before its conversation was deleted must not
        // re-insert a row for a conversation that no longer exists.
        if self.session_title_mirror_was_deleted(conversation_id) {
            self.forget_session_title_mirror(conversation_id);
            return Ok(false);
        }
        let Some(title) = bound_session_title(title) else {
            return self.remove_session_title_mirror(conversation_id);
        };
        // Unchanged refreshes dominate on the serialized runtime thread, so they
        // are answered from the last value this handle persisted and never read
        // or write the bounded index.
        if self.session_title_mirror_is_unchanged(conversation_id, &title) {
            return Ok(false);
        }
        let _lock = self.acquire_session_title_mirrors_lock()?;
        let mut mirrors = self.read_session_title_mirror_records_for_write()?.records;
        mirrors.insert(
            conversation_id.to_string(),
            SessionTitleMirror {
                conversation_id: conversation_id.to_string(),
                title: title.clone(),
                updated_at_unix_seconds,
            },
        );
        self.compact_session_title_mirror_records(&mut mirrors, conversation_id);
        self.write_session_title_mirror_records(&mirrors)?;
        self.note_session_title_mirror_written(conversation_id, &title);
        Ok(true)
    }

    /// Removes one conversation's title mirror while holding the index lock.
    ///
    /// Returns true when a retained title was pruned. An unreadable index is
    /// treated as empty and replaced with a valid empty index, so a delete can
    /// never be blocked by a corrupt cache file.
    pub(super) fn remove_session_title_mirror(&self, conversation_id: &str) -> Result<bool> {
        validate_conversation_id(conversation_id)?;
        self.forget_session_title_mirror(conversation_id);
        if !self.root.join(SESSION_TITLE_MIRRORS_FILE_NAME).exists() {
            return Ok(false);
        }
        let _lock = self.acquire_session_title_mirrors_lock()?;
        let write_read = self.read_session_title_mirror_records_for_write()?;
        let mut mirrors = write_read.records;
        let removed = mirrors.remove(conversation_id).is_some();
        if removed || write_read.recovered {
            self.write_session_title_mirror_records(&mirrors)?;
        }
        Ok(removed)
    }

    /// Loads one validated generated-title mirror for one conversation.
    ///
    /// The read is bounded to one index load and is only used when title
    /// generation is being considered, never on the per-turn path.
    #[cfg(test)]
    pub fn session_generated_title(
        &self,
        conversation_id: &str,
    ) -> Result<Option<SessionTitleMirror>> {
        validate_conversation_id(conversation_id)?;
        Ok(self
            .read_session_title_mirror_records()?
            .remove(conversation_id))
    }

    /// Reports whether one conversation still needs a generated title.
    ///
    /// A conversation with a durable manual name, or one that already has a
    /// stored generated title, must never spend a provider call on title
    /// generation. Both answers come from bounded indices that are never touched
    /// on the ordinary per-turn path, and a failed read is returned as an error
    /// instead of being folded into either answer: an unreadable index never
    /// implies a manual name. The unreadable generated-title index is quarantined
    /// so the next admission reads an empty index and the next write rebuilds it.
    pub fn session_title_generation_probe(
        &self,
        conversation_id: &str,
    ) -> Result<SessionTitleGenerationProbe> {
        validate_conversation_id(conversation_id)?;
        if self
            .read_named_sessions_index()?
            .contains_key(conversation_id)
        {
            return Ok(SessionTitleGenerationProbe {
                has_manual_name: true,
                has_stored_generated_title: false,
            });
        }
        match self.read_session_title_mirror_records() {
            Ok(records) => Ok(SessionTitleGenerationProbe {
                has_manual_name: false,
                has_stored_generated_title: records.contains_key(conversation_id),
            }),
            Err(error) => {
                self.quarantine_session_title_mirror_index();
                self.note_session_title_mirror_recovery(error.message());
                Err(error)
            }
        }
    }

    /// Acquires the exclusive advisory lock for title mirror index mutation.
    fn acquire_session_title_mirrors_lock(&self) -> Result<std_fs::File> {
        self.ensure_store_dir()?;
        let path = self.root.join(SESSION_TITLE_MIRRORS_LOCK_FILE_NAME);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        set_private_file_permissions(&path)?;
        flock(&file, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
        Ok(file)
    }

    /// Reads and validates the complete generated-title mirror index.
    fn read_session_title_mirror_records(&self) -> Result<BTreeMap<String, SessionTitleMirror>> {
        self.note_session_title_mirror_index_read();
        let path = self.root.join(SESSION_TITLE_MIRRORS_FILE_NAME);
        if !path.exists() {
            return Ok(BTreeMap::new());
        }
        if std_fs::metadata(&path)?.len() > SESSION_TITLE_MIRRORS_MAX_BYTES {
            return Err(MezError::invalid_args(
                "generated session title index exceeds the bounded size",
            ));
        }
        let mut data = String::new();
        std_fs::File::open(path)?.read_to_string(&mut data)?;
        let value: serde_json::Value = serde_json::from_str(&data).map_err(|error| {
            MezError::invalid_args(format!("generated session title decode failed: {error}"))
        })?;
        if value.get("version").and_then(serde_json::Value::as_u64)
            != Some(SESSION_TITLE_MIRRORS_VERSION)
        {
            return Err(MezError::invalid_args(
                "generated session title index version is unsupported",
            ));
        }
        let records: Vec<SessionTitleMirror> =
            serde_json::from_value(value.get("titles").cloned().ok_or_else(|| {
                MezError::invalid_args("generated session title records are missing")
            })?)
            .map_err(|error| {
                MezError::invalid_args(format!(
                    "generated session title records are invalid: {error}"
                ))
            })?;
        let mut indexed: BTreeMap<String, SessionTitleMirror> = BTreeMap::new();
        for mut record in records {
            validate_conversation_id(&record.conversation_id)?;
            record.title = bound_session_title(&record.title)
                .ok_or_else(|| MezError::invalid_args("generated session title is invalid"))?;
            if indexed
                .insert(record.conversation_id.clone(), record)
                .is_some()
            {
                return Err(MezError::invalid_args(
                    "generated session title index contains duplicate conversations",
                ));
            }
        }
        Ok(indexed)
    }

    /// Atomically replaces the durable generated-title mirror index.
    fn write_session_title_mirror_records(
        &self,
        mirrors: &BTreeMap<String, SessionTitleMirror>,
    ) -> Result<()> {
        self.ensure_store_dir()?;
        self.note_session_title_mirror_index_write();
        let path = self.root.join(SESSION_TITLE_MIRRORS_FILE_NAME);
        let temp_path = self.root.join(SESSION_TITLE_MIRRORS_TEMP_FILE_NAME);
        let encoded = encode_session_title_mirror_records(mirrors)?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temp_path)?;
        file.write_all(&encoded)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        set_private_file_permissions(&temp_path)?;
        std_fs::rename(&temp_path, &path)?;
        set_private_file_permissions(&path)?;
        Ok(())
    }

    /// Reads the title index for a write, treating an unreadable index as empty.
    ///
    /// The index is a display cache, never the source of truth, so a corrupt,
    /// version-mismatched, or over-budget file must not permanently disable
    /// title mirroring. The unreadable file is moved aside under one bounded
    /// quarantine name, one bounded diagnostic is recorded, and the caller
    /// rebuilds the index from the value it is writing.
    fn read_session_title_mirror_records_for_write(&self) -> Result<SessionTitleMirrorWriteRead> {
        match self.read_session_title_mirror_records() {
            Ok(records) => Ok(SessionTitleMirrorWriteRead {
                records,
                recovered: false,
            }),
            Err(error) => {
                self.quarantine_session_title_mirror_index();
                self.note_session_title_mirror_recovery(error.message());
                Ok(SessionTitleMirrorWriteRead {
                    records: BTreeMap::new(),
                    recovered: true,
                })
            }
        }
    }

    /// Moves one unreadable title index aside under a bounded quarantine name.
    ///
    /// Quarantine is best effort: a failed move never fails the write, because
    /// the atomic rebuild replaces the index anyway, and only one bounded
    /// quarantine copy is ever retained.
    fn quarantine_session_title_mirror_index(&self) {
        let path = self.root.join(SESSION_TITLE_MIRRORS_FILE_NAME);
        let quarantine = self.root.join(SESSION_TITLE_MIRRORS_QUARANTINE_FILE_NAME);
        let _ = std_fs::remove_file(&quarantine);
        let _ = std_fs::rename(&path, &quarantine);
    }

    /// Drops the oldest titles so the persisted index stays within its bounds.
    ///
    /// The incoming conversation is always retained, so a title write can never
    /// be refused: oldest-`updated_at` titles are dropped until the index holds
    /// at most the configured maximum. The byte bound stays a read-side guard,
    /// where an over-budget index is quarantined and rebuilt from the incoming
    /// value instead of permanently disabling title mirroring.
    fn compact_session_title_mirror_records(
        &self,
        mirrors: &mut BTreeMap<String, SessionTitleMirror>,
        keep_conversation_id: &str,
    ) {
        let excess = mirrors
            .len()
            .saturating_sub(self.session_title_mirror_max_entries);
        if excess == 0 {
            return;
        }
        let mut evictable = mirrors
            .iter()
            .filter(|(conversation_id, _)| conversation_id.as_str() != keep_conversation_id)
            .map(|(conversation_id, record)| {
                (record.updated_at_unix_seconds, conversation_id.clone())
            })
            .collect::<Vec<_>>();
        evictable.sort();
        for (_, conversation_id) in evictable.into_iter().take(excess) {
            mirrors.remove(&conversation_id);
        }
    }

    /// Records one bounded generated-title mirror recovery diagnostic.
    fn note_session_title_mirror_recovery(&self, reason: &str) {
        let bounded = reason
            .chars()
            .filter(|character| !is_display_format_character(*character))
            .take(SESSION_TITLE_MIRROR_RECOVERY_REASON_MAX_CHARS)
            .collect::<String>();
        if let Ok(mut state) = self.session_title_mirror_handle_state().lock() {
            state.recoveries = state.recoveries.saturating_add(1);
            state.last_recovery_reason = Some(bounded);
        }
    }

    /// Returns the shared throttle and diagnostic state for this handle.
    fn session_title_mirror_handle_state(&self) -> &Mutex<SessionTitleMirrorHandleState> {
        &self.session_title_mirrors
    }

    /// Returns bounded diagnostics for this handle's generated-title mirrors.
    ///
    /// The report follows the objective mirror pattern: counts plus one bounded
    /// reason, and never mirror content or conversation identifiers.
    pub fn session_title_mirror_status(&self) -> SessionTitleMirrorStatus {
        let quarantined_index = self
            .root
            .join(SESSION_TITLE_MIRRORS_QUARANTINE_FILE_NAME)
            .is_file();
        self.session_title_mirror_handle_state()
            .lock()
            .map(|state| SessionTitleMirrorStatus {
                index_reads: state.index_reads,
                index_writes: state.index_writes,
                recoveries: state.recoveries,
                last_recovery_reason: state.last_recovery_reason.clone(),
                quarantined_index,
            })
            .unwrap_or(SessionTitleMirrorStatus {
                quarantined_index,
                ..SessionTitleMirrorStatus::default()
            })
    }

    /// Counts one generated-title mirror index read attempt.
    fn note_session_title_mirror_index_read(&self) {
        if let Ok(mut state) = self.session_title_mirror_handle_state().lock() {
            state.index_reads = state.index_reads.saturating_add(1);
        }
    }

    /// Counts one generated-title mirror index write.
    fn note_session_title_mirror_index_write(&self) {
        if let Ok(mut state) = self.session_title_mirror_handle_state().lock() {
            state.index_writes = state.index_writes.saturating_add(1);
        }
    }

    /// Reports whether one conversation was deleted through this handle.
    fn session_title_mirror_was_deleted(&self, conversation_id: &str) -> bool {
        self.session_title_mirror_handle_state()
            .lock()
            .map(|state| {
                state
                    .deleted_conversations
                    .iter()
                    .any(|deleted| deleted == conversation_id)
            })
            .unwrap_or(false)
    }

    /// Remembers one deleted conversation until this handle is dropped.
    pub(super) fn note_session_title_mirror_deleted(&self, conversation_id: &str) {
        let Ok(mut state) = self.session_title_mirror_handle_state().lock() else {
            return;
        };
        if state
            .deleted_conversations
            .iter()
            .any(|deleted| deleted == conversation_id)
        {
            return;
        }
        state
            .deleted_conversations
            .push(conversation_id.to_string());
        let excess = state
            .deleted_conversations
            .len()
            .saturating_sub(SESSION_TITLE_MIRROR_DELETED_MAX_ENTRIES);
        if excess > 0 {
            state.deleted_conversations.drain(..excess);
        }
    }

    /// Reports whether this handle already persisted the same bounded title.
    ///
    /// The throttle is per handle, so a value written by another process can be
    /// skipped until the title changes. That is acceptable for a display cache
    /// and is what keeps an unchanged refresh off the index.
    fn session_title_mirror_is_unchanged(&self, conversation_id: &str, title: &str) -> bool {
        self.session_title_mirror_handle_state()
            .lock()
            .map(|state| {
                state.last_mirrored.as_ref()
                    == Some(&(conversation_id.to_string(), title.to_string()))
            })
            .unwrap_or(false)
    }

    /// Records the title this handle most recently persisted.
    fn note_session_title_mirror_written(&self, conversation_id: &str, title: &str) {
        if let Ok(mut state) = self.session_title_mirror_handle_state().lock() {
            state.last_mirrored = Some((conversation_id.to_string(), title.to_string()));
        }
    }

    /// Clears this handle's throttle entry for one pruned conversation.
    fn forget_session_title_mirror(&self, conversation_id: &str) {
        let Ok(mut state) = self.session_title_mirror_handle_state().lock() else {
            return;
        };
        if state
            .last_mirrored
            .as_ref()
            .is_some_and(|(id, _)| id == conversation_id)
        {
            state.last_mirrored = None;
        }
    }

    /// Acquires the exclusive advisory lock for objective mirror index mutation.
    fn acquire_session_objective_mirrors_lock(&self) -> Result<std_fs::File> {
        self.ensure_store_dir()?;
        let path = self.root.join(SESSION_OBJECTIVE_MIRRORS_LOCK_FILE_NAME);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        set_private_file_permissions(&path)?;
        flock(&file, FlockOperation::LockExclusive).map_err(std::io::Error::from)?;
        Ok(file)
    }

    /// Reads and validates the complete objective title mirror index.
    fn read_session_objective_mirror_records(
        &self,
    ) -> Result<BTreeMap<String, SessionObjectiveMirror>> {
        self.note_session_objective_mirror_index_read();
        let path = self.root.join(SESSION_OBJECTIVE_MIRRORS_FILE_NAME);
        if !path.exists() {
            return Ok(BTreeMap::new());
        }
        if std_fs::metadata(&path)?.len() > SESSION_OBJECTIVE_MIRRORS_MAX_BYTES {
            return Err(MezError::invalid_args(
                "objective title mirror index exceeds the bounded size",
            ));
        }
        let mut data = String::new();
        std_fs::File::open(path)?.read_to_string(&mut data)?;
        let value: serde_json::Value = serde_json::from_str(&data).map_err(|error| {
            MezError::invalid_args(format!("objective title mirror decode failed: {error}"))
        })?;
        if value.get("version").and_then(serde_json::Value::as_u64)
            != Some(SESSION_OBJECTIVE_MIRRORS_VERSION)
        {
            return Err(MezError::invalid_args(
                "objective title mirror index version is unsupported",
            ));
        }
        let records: Vec<SessionObjectiveMirror> =
            serde_json::from_value(value.get("objectives").cloned().ok_or_else(|| {
                MezError::invalid_args("objective title mirror records are missing")
            })?)
            .map_err(|error| {
                MezError::invalid_args(format!(
                    "objective title mirror records are invalid: {error}"
                ))
            })?;
        let mut indexed = BTreeMap::new();
        for mut record in records {
            validate_conversation_id(&record.conversation_id)?;
            record.objective = bound_session_title(&record.objective).ok_or_else(|| {
                MezError::invalid_args("objective title mirror objective is invalid")
            })?;
            if indexed
                .insert(record.conversation_id.clone(), record)
                .is_some()
            {
                return Err(MezError::invalid_args(
                    "objective title mirror index contains duplicate conversations",
                ));
            }
        }
        Ok(indexed)
    }

    /// Atomically replaces the durable objective title mirror index.
    fn write_session_objective_mirror_records(
        &self,
        mirrors: &BTreeMap<String, SessionObjectiveMirror>,
    ) -> Result<()> {
        self.ensure_store_dir()?;
        let path = self.root.join(SESSION_OBJECTIVE_MIRRORS_FILE_NAME);
        let temp_path = self.root.join(SESSION_OBJECTIVE_MIRRORS_TEMP_FILE_NAME);
        let encoded = encode_session_objective_mirror_records(mirrors)?;
        self.note_session_objective_mirror_index_write();
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temp_path)?;
        file.write_all(&encoded)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        set_private_file_permissions(&temp_path)?;
        std_fs::rename(&temp_path, &path)?;
        set_private_file_permissions(&path)?;
        Ok(())
    }

    /// Returns bounded diagnostics for this handle's objective title mirrors.
    ///
    /// The report follows the catalog status pattern: counts plus one bounded
    /// reason, and never mirror content or conversation identifiers.
    ///
    /// The counters are per store handle, while `quarantined_index` reports the
    /// durable quarantined file an operator can inspect on disk.
    pub fn session_objective_mirror_status(&self) -> SessionObjectiveMirrorStatus {
        let quarantined_index = self
            .root
            .join(SESSION_OBJECTIVE_MIRRORS_QUARANTINE_FILE_NAME)
            .is_file();
        self.session_objective_mirror_handle_state()
            .lock()
            .map(|state| SessionObjectiveMirrorStatus {
                index_reads: state.index_reads,
                index_writes: state.index_writes,
                recoveries: state.recoveries,
                last_recovery_reason: state.last_recovery_reason.clone(),
                quarantined_index,
            })
            .unwrap_or(SessionObjectiveMirrorStatus {
                quarantined_index,
                ..SessionObjectiveMirrorStatus::default()
            })
    }

    /// Returns the shared throttle and diagnostic state for this handle.
    fn session_objective_mirror_handle_state(&self) -> &Mutex<SessionObjectiveMirrorHandleState> {
        &self.session_objective_mirrors
    }

    /// Reports whether this handle already persisted the same bounded objective.
    ///
    /// The throttle is per handle, so a value written by another process can be
    /// skipped until the published objective changes. That is acceptable for a
    /// display cache and is what keeps an unchanged refresh off the index.
    fn session_objective_mirror_is_unchanged(
        &self,
        conversation_id: &str,
        objective: &str,
    ) -> bool {
        self.session_objective_mirror_handle_state()
            .lock()
            .map(|state| {
                state.last_mirrored.as_ref()
                    == Some(&(conversation_id.to_string(), objective.to_string()))
            })
            .unwrap_or(false)
    }

    /// Records the mirror value this handle most recently persisted.
    fn note_session_objective_mirror_written(&self, conversation_id: &str, objective: &str) {
        if let Ok(mut state) = self.session_objective_mirror_handle_state().lock() {
            state.last_mirrored = Some((conversation_id.to_string(), objective.to_string()));
        }
    }

    /// Clears this handle's throttle entry for one pruned conversation.
    fn forget_session_objective_mirror(&self, conversation_id: &str) {
        let Ok(mut state) = self.session_objective_mirror_handle_state().lock() else {
            return;
        };
        if state
            .last_mirrored
            .as_ref()
            .is_some_and(|(id, _)| id == conversation_id)
        {
            state.last_mirrored = None;
        }
    }

    /// Counts one objective title mirror index read attempt.
    fn note_session_objective_mirror_index_read(&self) {
        if let Ok(mut state) = self.session_objective_mirror_handle_state().lock() {
            state.index_reads = state.index_reads.saturating_add(1);
        }
    }

    /// Counts one objective title mirror index write.
    fn note_session_objective_mirror_index_write(&self) {
        if let Ok(mut state) = self.session_objective_mirror_handle_state().lock() {
            state.index_writes = state.index_writes.saturating_add(1);
        }
    }

    /// Records one bounded objective title mirror recovery diagnostic.
    fn note_session_objective_mirror_recovery(&self, reason: &str) {
        let bounded = reason
            .chars()
            .filter(|character| !is_display_format_character(*character))
            .take(SESSION_OBJECTIVE_MIRROR_RECOVERY_REASON_MAX_CHARS)
            .collect::<String>();
        if let Ok(mut state) = self.session_objective_mirror_handle_state().lock() {
            state.recoveries = state.recoveries.saturating_add(1);
            state.last_recovery_reason = Some(bounded);
        }
    }

    /// Reads the mirror index for a write, treating an unreadable index as empty.
    ///
    /// The index is a display cache, never the source of truth, so a corrupt,
    /// version-mismatched, or over-budget file must not permanently disable
    /// mirroring. The unreadable file is moved aside under one bounded quarantine
    /// name, one bounded diagnostic is recorded, and the caller rebuilds the
    /// index from the value it is writing. Row rendering still degrades to no
    /// mirror for an unreadable index.
    fn read_session_objective_mirror_records_for_write(
        &self,
    ) -> Result<SessionObjectiveMirrorWriteRead> {
        match self.read_session_objective_mirror_records() {
            Ok(records) => Ok(SessionObjectiveMirrorWriteRead {
                records,
                recovered: false,
            }),
            Err(error) => {
                self.quarantine_session_objective_mirror_index();
                self.note_session_objective_mirror_recovery(error.message());
                Ok(SessionObjectiveMirrorWriteRead {
                    records: BTreeMap::new(),
                    recovered: true,
                })
            }
        }
    }

    /// Moves one unreadable mirror index aside under a bounded quarantine name.
    ///
    /// Quarantine is best effort: a failed move never fails the write, because
    /// the atomic rebuild replaces the index anyway, and only one bounded
    /// quarantine copy is ever retained.
    fn quarantine_session_objective_mirror_index(&self) {
        let path = self.root.join(SESSION_OBJECTIVE_MIRRORS_FILE_NAME);
        let quarantine = self
            .root
            .join(SESSION_OBJECTIVE_MIRRORS_QUARANTINE_FILE_NAME);
        let _ = std_fs::remove_file(&quarantine);
        let _ = std_fs::rename(&path, &quarantine);
    }

    /// Drops the oldest mirrors so the persisted index stays within its bounds.
    ///
    /// The incoming conversation is always retained, so a mirror write can never
    /// be refused: oldest-`updated_at` mirrors are dropped until the index holds
    /// at most `SESSION_OBJECTIVE_MIRRORS_MAX_ENTRIES` entries. The 4 MiB byte
    /// bound stays a guard on the read side, where an over-budget index is
    /// quarantined and rebuilt from the incoming value instead of permanently
    /// disabling mirroring.
    fn compact_session_objective_mirror_records(
        &self,
        mirrors: &mut BTreeMap<String, SessionObjectiveMirror>,
        keep_conversation_id: &str,
    ) {
        let excess = mirrors
            .len()
            .saturating_sub(SESSION_OBJECTIVE_MIRRORS_MAX_ENTRIES);
        if excess == 0 {
            return;
        }
        let mut evictable = mirrors
            .iter()
            .filter(|(conversation_id, _)| conversation_id.as_str() != keep_conversation_id)
            .map(|(conversation_id, record)| {
                (record.updated_at_unix_seconds, conversation_id.clone())
            })
            .collect::<Vec<_>>();
        evictable.sort();
        for (_, conversation_id) in evictable.into_iter().take(excess) {
            mirrors.remove(&conversation_id);
        }
    }

    /// Returns the durable naming timestamp retained for archive rebuild metadata.
    pub(super) fn archive_named_at_unix_seconds(
        &self,
        conversation_id: &str,
    ) -> Result<Option<u64>> {
        validate_conversation_id(conversation_id)?;
        let _lock = self.acquire_named_sessions_lock()?;
        Ok(self
            .read_named_sessions_index()?
            .remove(conversation_id)
            .map(|session| session.named_at_unix_seconds))
    }

    /// Reports whether one conversation's user-assigned name is picker-preferred.
    ///
    /// Retained archive sidecars mirror only the name and its timestamp, so the
    /// bounded naming index stays the authority for the preferred-name
    /// partition. A conversation with no index record keeps the durable
    /// preferred default that records written before the flag existed decode to.
    pub(super) fn archive_name_preferred(&self, conversation_id: &str) -> Result<bool> {
        validate_conversation_id(conversation_id)?;
        Ok(self
            .read_named_sessions_index()?
            .get(conversation_id)
            .map(|session| !session.ephemeral)
            .unwrap_or(true))
    }

    /// Runs the legacy transcript path for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn legacy_transcript_path_for(&self, conversation_id: &str) -> Result<PathBuf> {
        validate_conversation_id(conversation_id)?;
        if is_root_control_conversation_id(conversation_id) {
            return self.transcript_path_for(conversation_id);
        }
        Ok(self.root.join(format!("{conversation_id}.tsv")))
    }
}

/// Encodes one complete objective title mirror index.
fn encode_session_objective_mirror_records(
    mirrors: &BTreeMap<String, SessionObjectiveMirror>,
) -> Result<Vec<u8>> {
    serde_json::to_vec(&serde_json::json!({
        "version": SESSION_OBJECTIVE_MIRRORS_VERSION,
        "objectives": mirrors.values().collect::<Vec<_>>(),
    }))
    .map_err(|error| {
        MezError::invalid_args(format!("objective title mirror encode failed: {error}"))
    })
}

/// Encodes one complete generated-title mirror index.
fn encode_session_title_mirror_records(
    mirrors: &BTreeMap<String, SessionTitleMirror>,
) -> Result<Vec<u8>> {
    serde_json::to_vec(&serde_json::json!({
        "version": SESSION_TITLE_MIRRORS_VERSION,
        "titles": mirrors.values().collect::<Vec<_>>(),
    }))
    .map_err(|error| {
        MezError::invalid_args(format!("generated session title encode failed: {error}"))
    })
}

/// Returns whether one root entry belongs to a shared non-transcript TSV store.
fn is_root_control_tsv_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| ROOT_CONTROL_TSV_FILE_NAMES.contains(&name))
}

/// Returns whether an identifier is reserved by a root-owned control TSV.
fn is_root_control_conversation_id(conversation_id: &str) -> bool {
    ROOT_CONTROL_TSV_FILE_NAMES.iter().any(|file_name| {
        file_name
            .strip_suffix(".tsv")
            .is_some_and(|stem| stem == conversation_id)
    })
}

/// Collapses adjacent equal raw prompts while retaining the newest paste
/// representation and enforcing the shared history bounds.
fn canonicalize_structured_history(
    history: Vec<ReadlineHistoryEntry>,
) -> Vec<ReadlineHistoryEntry> {
    let mut canonical = Vec::<ReadlineHistoryEntry>::with_capacity(history.len());
    let mut retained_bytes = 0usize;
    for entry in history {
        if entry.text.is_empty()
            || entry.text.len() > mez_mux::readline::MAX_READLINE_HISTORY_ENTRY_BYTES
            || !entry.is_valid()
        {
            continue;
        }
        if canonical
            .last()
            .is_some_and(|previous| previous.text == entry.text)
        {
            if let Some(previous) = canonical.last_mut() {
                *previous = entry;
            }
            continue;
        }
        retained_bytes = retained_bytes.saturating_add(entry.text.len());
        canonical.push(entry);
        while canonical.len() > DEFAULT_AGENT_PROMPT_HISTORY_LIMIT
            || retained_bytes > mez_mux::readline::MAX_READLINE_HISTORY_BYTES
        {
            let removed = canonical.remove(0);
            retained_bytes = retained_bytes.saturating_sub(removed.text.len());
        }
    }
    canonical
}

/// Appends a structured entry, replacing only the representation metadata
/// when its raw prompt matches the current history tail.
fn append_structured_history_entry(
    history: &mut Vec<ReadlineHistoryEntry>,
    entry: ReadlineHistoryEntry,
) -> bool {
    if let Some(previous) = history.last_mut()
        && previous.text == entry.text
    {
        if previous == &entry {
            return false;
        }
        *previous = entry;
        return true;
    }
    history.push(entry);
    if history.len() > DEFAULT_AGENT_PROMPT_HISTORY_LIMIT {
        history.remove(0);
    }
    true
}

/// Encodes one conversation summary sidecar as compact JSON.
fn encode_conversation_summary(summary: &ConversationSummary) -> String {
    serde_json::json!({
        "version": 1,
        "conversation_id": summary.conversation_id,
        "entries": summary.entries,
        "first_created_at_unix_seconds": summary.first_created_at_unix_seconds,
        "last_created_at_unix_seconds": summary.last_created_at_unix_seconds,
        "last_turn_id": summary.last_turn_id,
        "agent_id": summary.agent_id,
        "pane_id": summary.pane_id,
        "directory": summary.directory,
        "initial_prompt": summary.initial_prompt,
        "latest_user_prompt": summary.latest_user_prompt,
    })
    .to_string()
}

/// Normalizes and validates one user-assigned agent-session name.
fn validate_agent_session_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(MezError::invalid_args(
            "agent session name must not be empty",
        ));
    }
    if name.chars().any(is_display_format_character) {
        return Err(MezError::invalid_args(
            "agent session name must not contain control or format characters",
        ));
    }
    if name.chars().count() > MAX_AGENT_SESSION_NAME_CHARS {
        return Err(MezError::invalid_args(format!(
            "agent session name must not exceed {MAX_AGENT_SESSION_NAME_CHARS} characters"
        )));
    }
    Ok(name.to_string())
}

/// Decodes one conversation summary sidecar and validates required fields.
fn decode_conversation_summary(line: &str) -> Result<ConversationSummary> {
    let value: serde_json::Value = serde_json::from_str(line).map_err(|error| {
        MezError::invalid_args(format!("conversation summary decode failed: {error}"))
    })?;
    if value.get("version").and_then(|field| field.as_u64()) != Some(1) {
        return Err(MezError::invalid_args(
            "conversation summary version is invalid",
        ));
    }
    let conversation_id = required_summary_string(&value, "conversation_id")?;
    validate_conversation_id(&conversation_id)?;
    let summary = ConversationSummary {
        conversation_id,
        entries: required_summary_u64(&value, "entries")?
            .try_into()
            .unwrap_or(usize::MAX),
        first_created_at_unix_seconds: required_summary_u64(
            &value,
            "first_created_at_unix_seconds",
        )?,
        last_created_at_unix_seconds: required_summary_u64(&value, "last_created_at_unix_seconds")?,
        last_turn_id: required_summary_string(&value, "last_turn_id")?,
        agent_id: required_summary_string(&value, "agent_id")?,
        pane_id: required_summary_string(&value, "pane_id")?,
        directory: optional_summary_string(&value, "directory"),
        initial_prompt: optional_summary_string(&value, "initial_prompt"),
        latest_user_prompt: optional_summary_string(&value, "latest_user_prompt"),
    };
    Ok(summary)
}

/// Reads one required string from a summary JSON object.
fn required_summary_string(value: &serde_json::Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(|field| field.as_str())
        .filter(|field| !field.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| MezError::invalid_args(format!("conversation summary {field} is invalid")))
}

/// Reads one required u64 from a summary JSON object.
fn required_summary_u64(value: &serde_json::Value, field: &str) -> Result<u64> {
    value
        .get(field)
        .and_then(|field| field.as_u64())
        .ok_or_else(|| MezError::invalid_args(format!("conversation summary {field} is invalid")))
}

/// Reads one optional string from a summary JSON object.
fn optional_summary_string(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(|field| field.as_str())
        .filter(|field| !field.trim().is_empty())
        .map(ToOwned::to_owned)
}

/// Returns the best directory hint in one transcript entry.
fn transcript_entry_directory(entry: &TranscriptEntry) -> Option<String> {
    transcript_entry_project_root(entry).or_else(|| {
        entry.content.lines().find_map(|line| {
            line.strip_prefix("cwd=")
                .or_else(|| line.strip_prefix("working_directory="))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
    })
}

/// Returns a project-root hint from one transcript entry.
fn transcript_entry_project_root(entry: &TranscriptEntry) -> Option<String> {
    entry.content.lines().find_map(|line| {
        line.strip_prefix("project_root=")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}
