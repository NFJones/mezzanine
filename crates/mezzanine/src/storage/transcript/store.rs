//! Filesystem-backed transcript store operations.
//!
//! Store methods validate conversation ids, enforce private storage
//! permissions, and use append-only TSV records for inspectable persistence.

use std::collections::BTreeSet;
use std::fs::{self as std_fs, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

use tokio::fs::{self as tokio_fs, OpenOptions as TokioOpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::{MezError, MezErrorKind, Result};

use super::encoding::{
    decode_agent_session_metadata, decode_prompt_history_entry, decode_transcript_entry,
    encode_agent_session_metadata, encode_prompt_history_entry, encode_transcript_entry,
};
use super::fs::{
    set_private_dir_permissions, set_private_dir_permissions_async, set_private_file_permissions,
    set_private_file_permissions_async,
};
use super::types::{AgentPresentationEntry, AgentTranscriptStore};
use mez_agent::transcript::{
    AgentSessionMetadata, ConversationSummary, TranscriptEntry, TranscriptRole,
    bounded_summary_text, summarize_conversation, validate_conversation_id,
};

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
/// Defines the SHARED PROMPT HISTORY FILE NAME const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const SHARED_PROMPT_HISTORY_FILE_NAME: &str = "prompt-history.tsv";
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
const PRESENTATION_CLEAR_TAIL_COMPACT_BYTES: u64 = 256 * 1024;

/// Maximum saved agent conversations retained by default for `/resume`.
pub const DEFAULT_SAVED_AGENT_SESSION_LIMIT: usize = 100;

impl AgentTranscriptStore {
    /// Creates a store under the standard config-root agent-session directory.
    pub fn under_config_root(config_root: impl Into<PathBuf>) -> Self {
        Self {
            root: config_root.into().join("agent-sessions"),
            saved_sessions_limit: DEFAULT_SAVED_AGENT_SESSION_LIMIT,
        }
    }

    /// Creates a store rooted at a specific directory.
    #[cfg(test)]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            saved_sessions_limit: DEFAULT_SAVED_AGENT_SESSION_LIMIT,
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
        Ok(())
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
        if metadata.len() < PRESENTATION_CLEAR_TAIL_COMPACT_BYTES {
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

    /// Lists summaries for all transcript files in this store.
    pub fn list(&self) -> Result<Vec<ConversationSummary>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut summaries = Vec::new();
        let mut seen = BTreeSet::new();
        for entry in std_fs::read_dir(&self.root)? {
            let path = entry?.path();
            let file_name = path.file_name().and_then(|name| name.to_str());
            let conversation_id = if path.is_dir() {
                if !path.join(SESSION_TRANSCRIPT_FILE_NAME).exists() {
                    continue;
                }
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(ToOwned::to_owned)
            } else if matches!(
                file_name,
                Some(
                    SHARED_PROMPT_HISTORY_FILE_NAME
                        | SHARED_COMMAND_PROMPT_HISTORY_FILE_NAME
                        | ACTIVE_AGENT_SESSION_METADATA_FILE_NAME
                )
            ) {
                continue;
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("tsv") {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(ToOwned::to_owned)
            } else {
                None
            };
            let Some(conversation_id) = conversation_id else {
                continue;
            };
            if !seen.insert(conversation_id.clone()) {
                continue;
            };
            if let Some(summary) = self.summary(&conversation_id)? {
                summaries.push(summary);
            }
        }
        summaries.sort_by(|left, right| left.conversation_id.cmp(&right.conversation_id));
        Ok(summaries)
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
        if session_dir.exists() {
            std_fs::remove_dir_all(session_dir)?;
            return Ok(true);
        }
        let legacy_path = self.legacy_transcript_path_for(conversation_id)?;
        if legacy_path.exists() {
            std_fs::remove_file(legacy_path)?;
            return Ok(true);
        }
        Ok(false)
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
    }

    /// Appends one submitted agent prompt to the bounded shared history file.
    pub fn append_prompt_history(&self, conversation_id: &str, prompt: &str) -> Result<bool> {
        validate_conversation_id(conversation_id)?;
        if prompt.trim().is_empty() {
            return Ok(false);
        }
        let mut prompts = self.prompt_history(conversation_id)?;
        prompts.push(prompt.to_string());
        while prompts.len() > DEFAULT_AGENT_PROMPT_HISTORY_LIMIT {
            prompts.remove(0);
        }
        self.write_prompt_history(prompts)?;
        Ok(true)
    }

    /// Appends one submitted primary command prompt to its bounded shared
    /// history file.
    pub fn append_command_prompt_history(&self, command: &str) -> Result<bool> {
        if command.trim().is_empty() {
            return Ok(false);
        }
        let mut commands = self.command_prompt_history()?;
        commands.push(command.to_string());
        while commands.len() > DEFAULT_AGENT_PROMPT_HISTORY_LIMIT {
            commands.remove(0);
        }
        self.write_command_prompt_history(commands)?;
        Ok(true)
    }

    /// Appends one submitted agent prompt through Tokio filesystem I/O.
    pub async fn append_prompt_history_async(
        &self,
        conversation_id: &str,
        prompt: &str,
    ) -> Result<bool> {
        validate_conversation_id(conversation_id)?;
        if prompt.trim().is_empty() {
            return Ok(false);
        }
        let mut prompts = self.prompt_history_async(conversation_id).await?;
        prompts.push(prompt.to_string());
        while prompts.len() > DEFAULT_AGENT_PROMPT_HISTORY_LIMIT {
            prompts.remove(0);
        }
        self.write_prompt_history_async(prompts).await?;
        Ok(true)
    }

    /// Appends one submitted primary command prompt through Tokio filesystem
    /// I/O.
    pub async fn append_command_prompt_history_async(&self, command: &str) -> Result<bool> {
        if command.trim().is_empty() {
            return Ok(false);
        }
        let mut commands = self.command_prompt_history_async().await?;
        commands.push(command.to_string());
        while commands.len() > DEFAULT_AGENT_PROMPT_HISTORY_LIMIT {
            commands.remove(0);
        }
        self.write_command_prompt_history_async(commands).await?;
        Ok(true)
    }

    /// Reads bounded submitted prompt history shared by all agent sessions.
    pub fn prompt_history(&self, conversation_id: &str) -> Result<Vec<String>> {
        validate_conversation_id(conversation_id)?;
        let path = self.prompt_history_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut data = String::new();
        std_fs::File::open(path)?.read_to_string(&mut data)?;
        let mut prompts = data
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(decode_prompt_history_entry)
            .collect::<Result<Vec<_>>>()?;
        if prompts.len() > DEFAULT_AGENT_PROMPT_HISTORY_LIMIT {
            prompts = prompts[prompts.len() - DEFAULT_AGENT_PROMPT_HISTORY_LIMIT..].to_vec();
        }
        Ok(prompts)
    }

    /// Reads bounded submitted primary command prompt history.
    pub fn command_prompt_history(&self) -> Result<Vec<String>> {
        let path = self.command_prompt_history_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut data = String::new();
        std_fs::File::open(path)?.read_to_string(&mut data)?;
        let mut commands = data
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(decode_prompt_history_entry)
            .collect::<Result<Vec<_>>>()?;
        if commands.len() > DEFAULT_AGENT_PROMPT_HISTORY_LIMIT {
            commands = commands[commands.len() - DEFAULT_AGENT_PROMPT_HISTORY_LIMIT..].to_vec();
        }
        Ok(commands)
    }

    /// Reads bounded shared prompt history through Tokio filesystem I/O.
    pub async fn prompt_history_async(&self, conversation_id: &str) -> Result<Vec<String>> {
        validate_conversation_id(conversation_id)?;
        let path = self.prompt_history_path();
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
        let mut prompts = data
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(decode_prompt_history_entry)
            .collect::<Result<Vec<_>>>()?;
        if prompts.len() > DEFAULT_AGENT_PROMPT_HISTORY_LIMIT {
            prompts = prompts[prompts.len() - DEFAULT_AGENT_PROMPT_HISTORY_LIMIT..].to_vec();
        }
        Ok(prompts)
    }

    /// Reads bounded submitted primary command prompt history through Tokio
    /// filesystem I/O.
    pub async fn command_prompt_history_async(&self) -> Result<Vec<String>> {
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
        let mut commands = data
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(decode_prompt_history_entry)
            .collect::<Result<Vec<_>>>()?;
        if commands.len() > DEFAULT_AGENT_PROMPT_HISTORY_LIMIT {
            commands = commands[commands.len() - DEFAULT_AGENT_PROMPT_HISTORY_LIMIT..].to_vec();
        }
        Ok(commands)
    }

    /// Returns the shared prompt-history file path.
    pub fn prompt_history_file(&self) -> PathBuf {
        self.prompt_history_path()
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

    /// Runs the write prompt history operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_prompt_history(&self, prompts: impl IntoIterator<Item = String>) -> Result<()> {
        self.ensure_store_dir()?;
        let path = self.prompt_history_path();
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        for prompt in prompts {
            if prompt.is_empty() {
                continue;
            }
            file.write_all(encode_prompt_history_entry(&prompt)?.as_bytes())?;
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
        set_private_file_permissions(&path)?;
        Ok(())
    }

    /// Runs the write command prompt history operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn write_command_prompt_history(
        &self,
        commands: impl IntoIterator<Item = String>,
    ) -> Result<()> {
        self.ensure_store_dir()?;
        let path = self.command_prompt_history_path();
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        for command in commands {
            if command.is_empty() {
                continue;
            }
            file.write_all(encode_prompt_history_entry(&command)?.as_bytes())?;
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

    /// Deletes oldest saved conversations until the configured resume cap holds.
    fn prune_saved_sessions_over_limit(&self) -> Result<()> {
        let mut summaries = self.list()?;
        if summaries.len() <= self.saved_sessions_limit {
            return Ok(());
        }
        summaries.sort_by(|left, right| {
            left.last_created_at_unix_seconds
                .cmp(&right.last_created_at_unix_seconds)
                .then_with(|| {
                    left.first_created_at_unix_seconds
                        .cmp(&right.first_created_at_unix_seconds)
                })
                .then_with(|| left.conversation_id.cmp(&right.conversation_id))
        });
        let delete_count = summaries.len().saturating_sub(self.saved_sessions_limit);
        for summary in summaries.into_iter().take(delete_count) {
            self.delete(&summary.conversation_id)?;
        }
        Ok(())
    }

    /// Runs the write prompt history async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn write_prompt_history_async(
        &self,
        prompts: impl IntoIterator<Item = String>,
    ) -> Result<()> {
        self.ensure_store_dir_async().await?;
        let path = self.prompt_history_path();
        let mut file = TokioOpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .await?;
        for prompt in prompts {
            if prompt.is_empty() {
                continue;
            }
            file.write_all(encode_prompt_history_entry(&prompt)?.as_bytes())
                .await?;
            file.write_all(b"\n").await?;
        }
        file.sync_all().await?;
        set_private_file_permissions_async(&path).await?;
        Ok(())
    }

    /// Runs the write command prompt history async operation for this
    /// subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn write_command_prompt_history_async(
        &self,
        commands: impl IntoIterator<Item = String>,
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
            if command.is_empty() {
                continue;
            }
            file.write_all(encode_prompt_history_entry(&command)?.as_bytes())
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
