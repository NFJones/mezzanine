//! Filesystem-backed transcript store operations.
//!
//! Store methods validate conversation ids, enforce private storage
//! permissions, and use append-only TSV records for inspectable persistence.

use std::collections::BTreeMap;
use std::fs::{self as std_fs, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

use tokio::fs::{self as tokio_fs, OpenOptions as TokioOpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use rustix::fs::{FlockOperation, flock};

use crate::error::{MezError, MezErrorKind, Result};

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
};
use mez_agent::AgentConversationKind;
use mez_agent::transcript::{
    AgentSessionMetadata, ConversationSummary, TranscriptEntry, TranscriptRole,
    bounded_summary_text, summarize_conversation, validate_conversation_id,
};
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
const SESSION_METADATA_VERSION: u64 = 1;
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
/// Versioned root-level index containing durable user-assigned session names.
const NAMED_AGENT_SESSIONS_FILE_NAME: &str = "named-sessions.json";
/// Advisory lock serializing named-session index updates.
const NAMED_AGENT_SESSIONS_LOCK_FILE_NAME: &str = ".named-sessions.json.lock";
/// Current durable named-session index schema version.
const NAMED_AGENT_SESSIONS_VERSION: u64 = 1;
/// Maximum accepted session-name length in Unicode scalar values.
const MAX_AGENT_SESSION_NAME_CHARS: usize = 80;
/// Defines the SHARED PROMPT HISTORY CONVERSATION ID const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const SHARED_PROMPT_HISTORY_CONVERSATION_ID: &str = "prompt-history";
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
pub const DEFAULT_SAVED_AGENT_SESSION_LIMIT: usize = 100;

impl AgentTranscriptStore {
    /// Creates a store under the standard config-root agent-session directory.
    pub fn under_config_root(config_root: impl Into<PathBuf>) -> Self {
        Self {
            root: config_root.into().join("agent-sessions"),
            saved_sessions_limit: DEFAULT_SAVED_AGENT_SESSION_LIMIT,
            presentation_compaction_threshold: PRESENTATION_CLEAR_TAIL_COMPACT_BYTES,
        }
    }

    /// Creates a store rooted at a specific directory.
    #[cfg(test)]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            saved_sessions_limit: DEFAULT_SAVED_AGENT_SESSION_LIMIT,
            presentation_compaction_threshold: PRESENTATION_CLEAR_TAIL_COMPACT_BYTES,
        }
    }

    /// Returns this store with a configured saved-conversation retention limit.
    #[cfg(test)]
    pub fn with_saved_sessions_limit(mut self, limit: usize) -> Result<Self> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "saved agent session limit must be greater than zero",
            ));
        }
        self.saved_sessions_limit = limit;
        Ok(self)
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

    /// Updates the configured saved-conversation retention limit.
    pub fn set_saved_sessions_limit(&mut self, limit: usize) -> Result<()> {
        if limit == 0 {
            return Err(MezError::invalid_args(
                "saved agent session limit must be greater than zero",
            ));
        }
        self.saved_sessions_limit = limit;
        Ok(())
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
        catalog::initialize(self, now_unix_seconds)
    }

    /// Reconstructs the saved-conversation catalog from retained session files.
    #[cfg(test)]
    pub fn rebuild_catalog(&self, now_unix_seconds: u64) -> Result<()> {
        catalog::rebuild(self, now_unix_seconds)
    }

    /// Returns the catalog database path for focused tests.
    #[cfg(test)]
    pub fn catalog_path(&self) -> PathBuf {
        catalog::catalog_path(self)
    }

    /// Loads one exact saved session, repairing catalog divergence from its files.
    pub fn saved_session(&self, conversation_id: &str) -> Result<Option<SavedAgentSession>> {
        validate_conversation_id(conversation_id)?;
        if let Some(record) = catalog::record(self, conversation_id)?
            && self.catalog_record_payloads_exist(conversation_id, &record)?
        {
            return Ok(Some(record.session));
        }
        self.upsert_catalog_from_files(conversation_id, None)?;
        Ok(catalog::record(self, conversation_id)?.map(|record| record.session))
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
    fn upsert_catalog_from_files(
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
        let named = if existing.is_none() {
            self.read_named_sessions_index()?.remove(conversation_id)
        } else {
            None
        };
        let session_dir = self.session_dir_for(conversation_id)?;
        let directory_transcript = session_dir.join(SESSION_TRANSCRIPT_FILE_NAME).is_file();
        let legacy_transcript = self.legacy_transcript_path_for(conversation_id)?.is_file();
        let has_transcript = directory_transcript || legacy_transcript;
        let has_presentation = session_dir.join(SESSION_PRESENTATION_FILE_NAME).is_file()
            || session_dir
                .join(SESSION_PRESENTATION_COMPRESSED_FILE_NAME)
                .is_file();
        if !has_transcript && !has_presentation {
            return named
                .as_ref()
                .map(|named| self.named_only_catalog_candidate(named))
                .transpose();
        }
        let candidate = self.catalog_candidate_for_payload(
            conversation_id,
            has_transcript,
            has_presentation,
            if directory_transcript || has_presentation {
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
            conversation_kind: existing.session.conversation_kind,
            has_transcript,
            has_presentation,
            payload_layout: if directory_transcript || has_presentation {
                CatalogPayloadLayout::Directory
            } else {
                CatalogPayloadLayout::LegacyTsv
            },
        }))
    }

    /// Appends one validated transcript entry to its conversation file.
    ///
    /// Creates the store root when needed, updates private permissions, and
    /// syncs the file before returning.
    pub fn append(&self, entry: &TranscriptEntry) -> Result<()> {
        self.append_one(entry)?;
        Ok(())
    }

    /// Appends multiple validated transcript entries and returns bytes written.
    ///
    /// This preserves the same per-entry durability behavior as `append` while
    /// giving async persistence workers a single call that can report a useful
    /// byte count after executing off the runtime actor.
    pub fn append_many(&self, entries: &[TranscriptEntry]) -> Result<usize> {
        let mut bytes = 0usize;
        for entry in entries {
            bytes = bytes.saturating_add(self.append_one(entry)?);
        }
        Ok(bytes)
    }

    /// Persists the durable origin classification for one conversation.
    pub fn save_conversation_kind(
        &self,
        conversation_id: &str,
        kind: AgentConversationKind,
    ) -> Result<()> {
        let session_dir = self.ensure_session_dir(conversation_id)?;
        let path = session_dir.join(SESSION_METADATA_FILE_NAME);
        let temp_path = session_dir.join(".metadata.json.tmp");
        let kind_name = match kind {
            AgentConversationKind::Root => "root",
            AgentConversationKind::Subagent => "subagent",
        };
        let encoded = serde_json::to_vec(&serde_json::json!({
            "version": SESSION_METADATA_VERSION,
            "conversation_kind": kind_name,
        }))
        .map_err(|error| {
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
        set_private_file_permissions(&path)?;
        self.upsert_catalog_from_files(conversation_id, Some(kind))?;
        Ok(())
    }

    /// Loads one conversation's durable origin, defaulting legacy sessions to root.
    pub fn conversation_kind(&self, conversation_id: &str) -> Result<AgentConversationKind> {
        let path = self.conversation_metadata_path_for(conversation_id)?;
        if !path.exists() {
            return Ok(AgentConversationKind::Root);
        }
        let data = std_fs::read(&path)?;
        let value = serde_json::from_slice::<serde_json::Value>(&data).map_err(|error| {
            MezError::invalid_args(format!("conversation metadata decode failed: {error}"))
        })?;
        let object = value
            .as_object()
            .ok_or_else(|| MezError::invalid_args("conversation metadata must be a JSON object"))?;
        if object.get("version").and_then(serde_json::Value::as_u64)
            != Some(SESSION_METADATA_VERSION)
        {
            return Err(MezError::invalid_args(
                "unsupported conversation metadata version",
            ));
        }
        match object
            .get("conversation_kind")
            .and_then(serde_json::Value::as_str)
        {
            Some("root") => Ok(AgentConversationKind::Root),
            Some("subagent") => Ok(AgentConversationKind::Subagent),
            _ => Err(MezError::invalid_args("invalid conversation metadata kind")),
        }
    }

    /// Appends multiple validated transcript entries through Tokio filesystem
    /// I/O and returns bytes written.
    ///
    /// This is used by the async runtime persistence worker so transcript
    /// durability does not require a blocking worker task.
    pub async fn append_many_async(&self, entries: &[TranscriptEntry]) -> Result<usize> {
        let mut bytes = 0usize;
        for entry in entries {
            bytes = bytes.saturating_add(self.append_one_async(entry).await?);
        }
        Ok(bytes)
    }

    /// Appends one validated presentation entry to its conversation file.
    ///
    /// Presentation rows are user-interface replay state. They intentionally
    /// live beside, not inside, model-facing transcript history.
    pub fn append_presentation(&self, entry: &AgentPresentationEntry) -> Result<()> {
        let entry = entry.normalized_for_agent_log_wrap();
        entry.validate()?;
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
            conversation_kind: self.conversation_kind(&entry.conversation_id)?,
            has_transcript: self.transcript_path_for(&entry.conversation_id)?.is_file()
                || self
                    .legacy_transcript_path_for(&entry.conversation_id)?
                    .is_file(),
            has_presentation: true,
            payload_layout: CatalogPayloadLayout::Directory,
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

    /// Appends one transcript entry and returns the encoded byte count.
    fn append_one(&self, entry: &TranscriptEntry) -> Result<usize> {
        entry.validate()?;
        let new_conversation = !self.conversation_exists(&entry.conversation_id)?;
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
        if new_conversation {
            self.prune_saved_sessions_over_limit()?;
        }
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

    /// Appends one transcript entry through Tokio filesystem I/O.
    async fn append_one_async(&self, entry: &TranscriptEntry) -> Result<usize> {
        entry.validate()?;
        let new_conversation = !self.conversation_exists(&entry.conversation_id)?;
        self.ensure_session_dir_async(&entry.conversation_id)
            .await?;
        let path = self.transcript_path_for(&entry.conversation_id)?;
        let encoded = encode_transcript_entry(entry)?;
        let mut file = TokioOpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        file.write_all(encoded.as_bytes()).await?;
        file.write_all(b"\n").await?;
        file.sync_all().await?;
        set_private_file_permissions_async(&path).await?;
        self.update_summary_after_append(entry)?;
        self.upsert_catalog_from_files(&entry.conversation_id, None)?;
        if new_conversation {
            self.prune_saved_sessions_over_limit()?;
        }
        Ok(encoded.len().saturating_add(1))
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

        let session_dir = self.ensure_session_dir(conversation_id)?;
        let path = self.transcript_path_for(conversation_id)?;
        let temp_path = session_dir.join("history.tsv.tmp");
        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temp_path)?;
            for entry in &entries {
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

        if let Some(summary) = summarize_conversation(entries) {
            self.write_summary_sidecar(&summary)?;
        } else {
            let summary_path = session_dir.join(SESSION_SUMMARY_FILE_NAME);
            if summary_path.exists() {
                std_fs::remove_file(summary_path)?;
            }
        }
        self.upsert_catalog_from_files(conversation_id, None)?;
        Ok(true)
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

    /// Lists transcript-backed summaries from the saved-session catalog.
    pub fn list(&self) -> Result<Vec<ConversationSummary>> {
        catalog::transcript_summaries(self)
    }

    /// Lists transcript-backed and named zero-entry sessions as one durable view.
    pub fn saved_sessions(&self) -> Result<Vec<SavedAgentSession>> {
        catalog::saved_sessions(self)
    }

    /// Builds one deterministic migration snapshot from retained session files.
    ///
    /// The root is enumerated once. Directory payloads take precedence over a
    /// duplicate legacy TSV, names are read once and overlaid last, and
    /// unrelated root entries are ignored. Presentation-only metadata is read
    /// as a stream so compressed history is never inflated into one allocation.
    pub(super) fn catalog_migration_candidates(&self) -> Result<Vec<CatalogCandidate>> {
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
            if !has_transcript && !has_presentation {
                continue;
            }
            if let Some(candidate) = self.catalog_candidate_for_payload(
                conversation_id,
                has_transcript,
                has_presentation,
                CatalogPayloadLayout::Directory,
                names.get(conversation_id),
            )? {
                candidates.insert(conversation_id.to_string(), candidate);
            }
        }

        for path in &paths {
            if !path.is_file()
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
            if let Some(candidate) = self.catalog_candidate_for_payload(
                conversation_id,
                true,
                false,
                CatalogPayloadLayout::LegacyTsv,
                names.get(conversation_id),
            )? {
                candidates.insert(conversation_id.to_string(), candidate);
            }
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
        Ok(candidates.into_values().collect())
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
            conversation_kind: self.conversation_kind(conversation_id)?,
            has_transcript,
            has_presentation,
            payload_layout,
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
            conversation_kind: self.conversation_kind(&named.conversation_id)?,
            has_transcript: false,
            has_presentation: false,
            payload_layout: CatalogPayloadLayout::Directory,
        })
    }

    /// Assigns or replaces the durable display name for one conversation.
    pub fn name_session(
        &self,
        conversation_id: &str,
        name: &str,
        named_at_unix_seconds: u64,
        directory: Option<String>,
    ) -> Result<NamedAgentSession> {
        validate_conversation_id(conversation_id)?;
        let name = validate_agent_session_name(name)?;
        let directory = directory
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let session = NamedAgentSession {
            conversation_id: conversation_id.to_string(),
            name,
            named_at_unix_seconds,
            directory,
        };
        let _lock = self.acquire_named_sessions_lock()?;
        let mut sessions = self.read_named_sessions_index()?;
        sessions.insert(conversation_id.to_string(), session.clone());
        self.write_named_sessions_index(&sessions)?;
        self.upsert_catalog_from_files(conversation_id, None)?;
        catalog::set_name(self, conversation_id, &session.name, named_at_unix_seconds)?;
        Ok(session)
    }

    /// Removes one durable conversation name without deleting conversation data.
    ///
    /// Returns true when a name existed and false when the conversation was
    /// already unnamed.
    pub fn clear_session_name(&self, conversation_id: &str) -> Result<bool> {
        validate_conversation_id(conversation_id)?;
        let removed = self.remove_named_session(conversation_id)?;
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
        let session_dir = self.session_dir_for(conversation_id)?;
        let removed_payload = if session_dir.exists() {
            std_fs::remove_dir_all(session_dir)?;
            true
        } else {
            let legacy_path = self.legacy_transcript_path_for(conversation_id)?;
            if legacy_path.exists() {
                std_fs::remove_file(legacy_path)?;
                true
            } else {
                false
            }
        };
        let removed_name = self.remove_named_session(conversation_id)?;
        catalog::delete(self, conversation_id)?;
        Ok(removed_payload || removed_name)
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
            self.list()?
                .into_iter()
                .find(|summary| summary.conversation_id == target_conversation_id)
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
    #[cfg(test)]
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

    /// Deletes oldest saved conversations until the configured resume cap holds.
    fn prune_saved_sessions_over_limit(&self) -> Result<()> {
        for conversation_id in catalog::unnamed_prune_candidates(self, self.saved_sessions_limit)? {
            if catalog::is_named(self, &conversation_id)? {
                continue;
            }
            self.delete(&conversation_id)?;
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

    /// Runs the ensure session dir async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn ensure_session_dir_async(&self, conversation_id: &str) -> Result<PathBuf> {
        self.ensure_store_dir_async().await?;
        let session_dir = self.session_dir_for(conversation_id)?;
        tokio_fs::create_dir_all(&session_dir).await?;
        set_private_dir_permissions_async(&session_dir).await?;
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
    fn remove_named_session(&self, conversation_id: &str) -> Result<bool> {
        let _lock = self.acquire_named_sessions_lock()?;
        let mut sessions = self.read_named_sessions_index()?;
        let removed = sessions.remove(conversation_id).is_some();
        if removed {
            self.write_named_sessions_index(&sessions)?;
        }
        Ok(removed)
    }

    /// Runs the legacy transcript path for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn legacy_transcript_path_for(&self, conversation_id: &str) -> Result<PathBuf> {
        validate_conversation_id(conversation_id)?;
        if conversation_id == SHARED_PROMPT_HISTORY_CONVERSATION_ID {
            return Ok(self.root.join(SHARED_PROMPT_HISTORY_CONVERSATION_ID));
        }
        Ok(self.root.join(format!("{conversation_id}.tsv")))
    }
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
    if name.chars().any(char::is_control) {
        return Err(MezError::invalid_args(
            "agent session name must not contain control characters",
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
