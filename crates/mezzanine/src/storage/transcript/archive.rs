//! Crash-safe tar+zstd lifecycle storage for saved agent conversations.
//!
//! Archives live beneath the private transcript root and contain exactly one
//! top-level conversation directory. A compact sidecar carries the bounded
//! catalog projection so healthy discovery and rebuilds never decompress every
//! archive. Extraction validates every entry before installing an active
//! payload and never delegates path creation to the tar implementation.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use mez_agent::AgentConversationKind;
use mez_agent::transcript::{ConversationSummary, validate_conversation_id};
use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{MezError, Result};

use super::catalog::{self, CatalogCandidate, CatalogPayloadLayout};
use super::fs::{set_private_dir_permissions, set_private_file_permissions};
use super::types::AgentTranscriptStore;

const ARCHIVE_DIRECTORY_NAME: &str = "archived";
const ARCHIVE_MANIFEST_FILE_NAME: &str = "archive-manifest.json";
const ARCHIVE_FORMAT_VERSION: u64 = 1;
const ARCHIVE_RECOVERY_DIRECTORY_NAME: &str = ".archive-recovery";
const ARCHIVE_RECOVERY_VERSION: u64 = 1;
const MAX_ARCHIVE_ENTRIES: usize = 100_000;
const MAX_ARCHIVE_UNCOMPRESSED_BYTES: u64 = 1024 * 1024 * 1024;

/// Bounded metadata describing one installed archived session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArchivedSessionInfo {
    /// Durable conversation identity.
    pub conversation_id: String,
    /// Time at which archival completed.
    pub archived_at_unix_seconds: u64,
    /// Installed compressed archive size.
    pub compressed_bytes: u64,
    /// Lowercase SHA-256 digest of the compressed archive.
    pub sha256: String,
    /// Bounded discovery summary retained without archive decompression.
    pub summary: ConversationSummary,
    /// Durable user-assigned display name, when present.
    pub name: Option<String>,
    /// Durable root/subagent classification.
    pub conversation_kind: AgentConversationKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ArchiveManifest {
    version: u64,
    conversation_id: String,
    archived_at_unix_seconds: u64,
    entries: usize,
    first_created_at_unix_seconds: u64,
    last_created_at_unix_seconds: u64,
    last_turn_id: String,
    agent_id: String,
    pane_id: String,
    directory: Option<String>,
    initial_prompt: Option<String>,
    latest_user_prompt: Option<String>,
    name: Option<String>,
    named_at_unix_seconds: Option<u64>,
    conversation_kind: String,
    has_transcript: bool,
    has_presentation: bool,
    payload_layout: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ArchiveSidecar {
    version: u64,
    archive_compressed_bytes: u64,
    archive_sha256: String,
    manifest: ArchiveManifest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ArchiveRecoveryOperation {
    Archive,
    Restore,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ArchiveRecoveryJournal {
    version: u64,
    conversation_id: String,
    operation: ArchiveRecoveryOperation,
    payload_layout: String,
}

enum StagedActivePayload {
    Directory { staged: PathBuf, active: PathBuf },
    Legacy { staged: PathBuf, active: PathBuf },
}

impl AgentTranscriptStore {
    /// Acquires the exclusive advisory lock shared by one conversation's mutations.
    pub(super) fn acquire_conversation_lock(&self, conversation_id: &str) -> Result<File> {
        validate_conversation_id(conversation_id)?;
        fs::create_dir_all(&self.root)?;
        set_private_dir_permissions(&self.root)?;
        let lock_directory = self.root.join(".conversation-locks");
        fs::create_dir_all(&lock_directory)?;
        set_private_dir_permissions(&lock_directory)?;
        let path = lock_directory.join(format!("{conversation_id}.lock"));
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

    /// Creates the private archive directory when a lifecycle mutation needs it.
    fn ensure_archive_directory(&self) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        set_private_dir_permissions(&self.root)?;
        let directory = archive_directory(self);
        fs::create_dir_all(&directory)?;
        set_private_dir_permissions(&directory)
    }

    /// Returns a directory-shaped source, normalizing a legacy root TSV when needed.
    fn prepare_archive_source(
        &self,
        conversation_id: &str,
        layout: CatalogPayloadLayout,
    ) -> Result<PathBuf> {
        match layout {
            CatalogPayloadLayout::Directory => {
                let source = self.session_dir_path(conversation_id)?;
                if !source.is_dir() {
                    return Err(MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "active saved-conversation directory is missing",
                    ));
                }
                Ok(source)
            }
            CatalogPayloadLayout::LegacyTsv => {
                let legacy = self.legacy_transcript_path(conversation_id)?;
                if !legacy.is_file() {
                    return Err(MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "legacy saved-conversation transcript is missing",
                    ));
                }
                let source = self.root.join(format!(".archive-source-{conversation_id}"));
                if source.exists() {
                    fs::remove_dir_all(&source)?;
                }
                fs::create_dir_all(&source)?;
                set_private_dir_permissions(&source)?;
                let target = source.join("history.tsv");
                fs::copy(legacy, &target)?;
                set_private_file_permissions(&target)?;
                Ok(source)
            }
        }
    }

    /// Atomically moves the active payload aside before installing archive metadata.
    fn stage_active_payload(
        &self,
        conversation_id: &str,
        layout: CatalogPayloadLayout,
    ) -> Result<StagedActivePayload> {
        match layout {
            CatalogPayloadLayout::Directory => {
                let active = self.session_dir_path(conversation_id)?;
                let staged = self.root.join(format!(".archive-stage-{conversation_id}"));
                remove_if_exists(&staged)?;
                fs::rename(&active, &staged)?;
                Ok(StagedActivePayload::Directory { staged, active })
            }
            CatalogPayloadLayout::LegacyTsv => {
                let active = self.legacy_transcript_path(conversation_id)?;
                let staged = self
                    .root
                    .join(format!(".archive-stage-{conversation_id}.tsv"));
                remove_if_exists(&staged)?;
                fs::rename(&active, &staged)?;
                Ok(StagedActivePayload::Legacy { staged, active })
            }
        }
    }

    /// Returns the active directory path without exposing store internals publicly.
    fn session_dir_path(&self, conversation_id: &str) -> Result<PathBuf> {
        validate_conversation_id(conversation_id)?;
        Ok(self.root.join(conversation_id))
    }

    /// Returns the legacy root-level transcript path used by old payload layouts.
    fn legacy_transcript_path(&self, conversation_id: &str) -> Result<PathBuf> {
        self.legacy_transcript_path_for(conversation_id)
    }

    /// Rebuilds the active catalog row after a verified restore is installed.
    fn upsert_catalog_from_active_files(&self, conversation_id: &str) -> Result<()> {
        if self.upsert_catalog_from_files(conversation_id, None)? {
            Ok(())
        } else {
            Err(MezError::invalid_state(
                "restored saved conversation did not produce catalog metadata",
            ))
        }
    }

    /// Removes retained naming metadata during archived-session deletion.
    fn remove_archived_session_name(&self, conversation_id: &str) -> Result<()> {
        let _ = self.remove_named_session(conversation_id)?;
        Ok(())
    }

    /// Compresses one active session and moves it into the archived lifecycle.
    pub(crate) fn archive_session(
        &self,
        conversation_id: &str,
        archived_at_unix_seconds: u64,
    ) -> Result<ArchivedSessionInfo> {
        validate_conversation_id(conversation_id)?;
        if archived_at_unix_seconds == 0 {
            return Err(MezError::invalid_args(
                "archive timestamp must be greater than zero",
            ));
        }
        let _lock = self.acquire_conversation_lock(conversation_id)?;
        self.ensure_archive_directory()?;
        if archive_path(self, conversation_id).is_file()
            || archive_sidecar_path(self, conversation_id).is_file()
        {
            return Err(MezError::conflict("saved conversation is already archived"));
        }

        let record = catalog::record(self, conversation_id)?.ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "saved conversation not found",
            )
        })?;
        if record.session.archived_at_unix_seconds.is_some() {
            return Err(MezError::conflict("saved conversation is already archived"));
        }
        self.write_archive_recovery_journal(
            conversation_id,
            ArchiveRecoveryOperation::Archive,
            record.payload_layout,
        )?;
        let source = self.prepare_archive_source(conversation_id, record.payload_layout)?;
        let named_at_unix_seconds = self.archive_named_at_unix_seconds(conversation_id)?;
        if record.session.name.is_some() && named_at_unix_seconds.is_none() {
            return Err(MezError::invalid_state(
                "named saved conversation is missing durable naming metadata",
            ));
        }
        let manifest =
            manifest_from_record(&record, archived_at_unix_seconds, named_at_unix_seconds);
        let temporary_archive = temporary_archive_path(self, conversation_id);
        remove_if_exists(&temporary_archive)?;
        build_archive(&source, conversation_id, &manifest, &temporary_archive)?;
        let (compressed_bytes, sha256) = archive_digest(&temporary_archive)?;
        validate_archive(
            &temporary_archive,
            conversation_id,
            Some(&manifest),
            false,
            None,
        )?;
        let sidecar = ArchiveSidecar {
            version: ARCHIVE_FORMAT_VERSION,
            archive_compressed_bytes: compressed_bytes,
            archive_sha256: sha256.clone(),
            manifest: manifest.clone(),
        };
        let temporary_sidecar = temporary_sidecar_path(self, conversation_id);
        write_sidecar(&temporary_sidecar, &sidecar)?;

        let staged = self.stage_active_payload(conversation_id, record.payload_layout)?;
        let install_result = (|| {
            fs::rename(&temporary_archive, archive_path(self, conversation_id))?;
            set_private_file_permissions(&archive_path(self, conversation_id))?;
            fs::rename(
                &temporary_sidecar,
                archive_sidecar_path(self, conversation_id),
            )?;
            set_private_file_permissions(&archive_sidecar_path(self, conversation_id))?;
            catalog::mark_archived(
                self,
                conversation_id,
                archived_at_unix_seconds,
                compressed_bytes,
                &sha256,
            )?;
            Ok(())
        })();
        if let Err(error) = install_result {
            let _ = remove_if_exists(&archive_path(self, conversation_id));
            let _ = remove_if_exists(&archive_sidecar_path(self, conversation_id));
            let _ = restore_staged_payload(&staged);
            return Err(error);
        }
        remove_staged_payload(&staged)?;
        if source != self.session_dir_path(conversation_id)? {
            let _ = fs::remove_dir_all(source);
        }
        self.remove_archive_recovery_journal(conversation_id)?;
        Ok(info_from_sidecar(sidecar))
    }

    /// Restores one verified archive into the active session directory.
    pub(crate) fn restore_archived_session(
        &self,
        conversation_id: &str,
    ) -> Result<ArchivedSessionInfo> {
        validate_conversation_id(conversation_id)?;
        let _lock = self.acquire_conversation_lock(conversation_id)?;
        let sidecar = read_sidecar(self, conversation_id)?;
        verify_installed_archive(self, conversation_id, &sidecar)?;
        let active = self.session_dir_path(conversation_id)?;
        let legacy = self.legacy_transcript_path(conversation_id)?;
        if active.exists() || legacy.exists() {
            return Err(MezError::conflict(
                "active saved conversation already exists; archived duplicate retained",
            ));
        }
        self.write_archive_recovery_journal(
            conversation_id,
            ArchiveRecoveryOperation::Restore,
            CatalogPayloadLayout::Directory,
        )?;
        let extraction_root = restore_temporary_path(self, conversation_id);
        if extraction_root.exists() {
            fs::remove_dir_all(&extraction_root)?;
        }
        fs::create_dir_all(&extraction_root)?;
        set_private_dir_permissions(&extraction_root)?;
        validate_archive(
            &archive_path(self, conversation_id),
            conversation_id,
            Some(&sidecar.manifest),
            true,
            Some(&extraction_root),
        )?;
        let extracted = extraction_root.join(conversation_id);
        if !extracted.is_dir() {
            return Err(MezError::invalid_args(
                "session archive did not contain its conversation directory",
            ));
        }
        fs::rename(&extracted, &active)?;
        set_private_dir_permissions(&active)?;
        let _ = fs::remove_dir_all(&extraction_root);
        self.upsert_catalog_from_active_files(conversation_id)?;
        remove_if_exists(&archive_path(self, conversation_id))?;
        remove_if_exists(&archive_sidecar_path(self, conversation_id))?;
        self.remove_archive_recovery_journal(conversation_id)?;
        Ok(info_from_sidecar(sidecar))
    }

    /// Deletes one archived payload, sidecar, name record, and catalog row.
    pub(crate) fn delete_archived_session(&self, conversation_id: &str) -> Result<bool> {
        validate_conversation_id(conversation_id)?;
        let _lock = self.acquire_conversation_lock(conversation_id)?;
        let archive = archive_path(self, conversation_id);
        let sidecar = archive_sidecar_path(self, conversation_id);
        let existed = archive.exists() || sidecar.exists();
        if !existed {
            return Ok(false);
        }
        self.write_archive_recovery_journal(
            conversation_id,
            ArchiveRecoveryOperation::Delete,
            CatalogPayloadLayout::Directory,
        )?;
        remove_if_exists(&archive)?;
        remove_if_exists(&sidecar)?;
        if self.session_dir_path(conversation_id)?.is_dir()
            || self.legacy_transcript_path(conversation_id)?.is_file()
        {
            self.upsert_catalog_from_active_files(conversation_id)?;
        } else {
            self.remove_archived_session_name(conversation_id)?;
            catalog::delete(self, conversation_id)?;
        }
        self.remove_archive_recovery_journal(conversation_id)?;
        Ok(existed)
    }

    /// Reads bounded archived-session metadata without decompressing the archive.
    #[allow(
        dead_code,
        reason = "bounded archive details are consumed by the dependent resume browser work"
    )]
    pub(crate) fn inspect_archived_session(
        &self,
        conversation_id: &str,
    ) -> Result<Option<ArchivedSessionInfo>> {
        validate_conversation_id(conversation_id)?;
        let sidecar_path = archive_sidecar_path(self, conversation_id);
        if !sidecar_path.is_file() {
            return Ok(None);
        }
        read_sidecar(self, conversation_id)
            .map(info_from_sidecar)
            .map(Some)
    }

    /// Updates mutable naming metadata in one installed archive sidecar.
    pub(super) fn update_archived_session_name(
        &self,
        conversation_id: &str,
        name: Option<(&str, u64)>,
    ) -> Result<()> {
        validate_conversation_id(conversation_id)?;
        let sidecar_path = archive_sidecar_path(self, conversation_id);
        if !sidecar_path.is_file() {
            return Ok(());
        }
        let mut sidecar = read_sidecar(self, conversation_id)?;
        let (name, named_at_unix_seconds) = name
            .map(|(name, named_at)| (Some(name.to_string()), Some(named_at)))
            .unwrap_or((None, None));
        sidecar.manifest.name = name;
        sidecar.manifest.named_at_unix_seconds = named_at_unix_seconds;
        let temporary = temporary_sidecar_path(self, conversation_id);
        write_sidecar(&temporary, &sidecar)?;
        fs::rename(&temporary, &sidecar_path)?;
        set_private_file_permissions(&sidecar_path)
    }

    /// Repairs only operations named by the bounded recovery-journal directory.
    pub(super) fn recover_archive_transactions(&self) -> Result<()> {
        let recovery_directory = archive_recovery_directory(self);
        if !recovery_directory.is_dir() {
            return Ok(());
        }
        let mut journals = fs::read_dir(&recovery_directory)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        journals.sort();
        for path in journals {
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let journal = read_archive_recovery_journal(self, &path)?;
            let _lock = self.acquire_conversation_lock(&journal.conversation_id)?;
            self.recover_archive_transaction(&journal)?;
            remove_if_exists(&path)?;
        }
        Ok(())
    }

    fn recover_archive_transaction(&self, journal: &ArchiveRecoveryJournal) -> Result<()> {
        let conversation_id = journal.conversation_id.as_str();
        match journal.operation {
            ArchiveRecoveryOperation::Archive => {
                let layout = payload_layout_from_name(&journal.payload_layout)?;
                let staged = staged_active_payload(self, conversation_id, layout)?;
                if let Ok(sidecar) = validate_installed_archive_payload(self, conversation_id) {
                    remove_staged_payload(&staged)?;
                    let candidate = candidate_from_sidecar(sidecar);
                    catalog::upsert(
                        self,
                        &candidate,
                        candidate.archived_at_unix_seconds.unwrap_or_default(),
                    )?;
                } else {
                    remove_if_exists(&archive_path(self, conversation_id))?;
                    remove_if_exists(&archive_sidecar_path(self, conversation_id))?;
                    restore_staged_payload(&staged)?;
                    self.upsert_catalog_from_active_files(conversation_id)?;
                }
                remove_if_exists(&temporary_archive_path(self, conversation_id))?;
                remove_if_exists(&temporary_sidecar_path(self, conversation_id))?;
                remove_if_exists(&self.root.join(format!(".archive-source-{conversation_id}")))?;
            }
            ArchiveRecoveryOperation::Restore => {
                let active_exists = self.session_dir_path(conversation_id)?.is_dir()
                    || self.legacy_transcript_path(conversation_id)?.is_file();
                if active_exists {
                    self.upsert_catalog_from_active_files(conversation_id)?;
                    let archive = archive_path(self, conversation_id);
                    let sidecar = archive_sidecar_path(self, conversation_id);
                    if archive.is_file() != sidecar.is_file() {
                        remove_if_exists(&archive)?;
                        remove_if_exists(&sidecar)?;
                    }
                }
                remove_if_exists(&restore_temporary_path(self, conversation_id))?;
            }
            ArchiveRecoveryOperation::Delete => {
                remove_if_exists(&archive_path(self, conversation_id))?;
                remove_if_exists(&archive_sidecar_path(self, conversation_id))?;
                if self.session_dir_path(conversation_id)?.is_dir()
                    || self.legacy_transcript_path(conversation_id)?.is_file()
                {
                    self.upsert_catalog_from_active_files(conversation_id)?;
                } else {
                    self.remove_archived_session_name(conversation_id)?;
                    catalog::delete(self, conversation_id)?;
                }
            }
        }
        Ok(())
    }

    fn write_archive_recovery_journal(
        &self,
        conversation_id: &str,
        operation: ArchiveRecoveryOperation,
        payload_layout: CatalogPayloadLayout,
    ) -> Result<()> {
        let directory = archive_recovery_directory(self);
        fs::create_dir_all(&directory)?;
        set_private_dir_permissions(&directory)?;
        let journal = ArchiveRecoveryJournal {
            version: ARCHIVE_RECOVERY_VERSION,
            conversation_id: conversation_id.to_string(),
            operation,
            payload_layout: payload_layout.as_str().to_string(),
        };
        let path = archive_recovery_journal_path(self, conversation_id);
        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(&journal).map_err(|error| {
            MezError::invalid_args(format!("archive recovery journal encode failed: {error}"))
        })?;
        remove_if_exists(&temporary)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        set_private_file_permissions(&temporary)?;
        fs::rename(&temporary, &path)?;
        set_private_file_permissions(&path)
    }

    fn remove_archive_recovery_journal(&self, conversation_id: &str) -> Result<()> {
        remove_if_exists(&archive_recovery_journal_path(self, conversation_id))
    }
}

pub(super) fn archived_catalog_candidates(
    store: &AgentTranscriptStore,
) -> Result<Vec<CatalogCandidate>> {
    let directory = archive_directory(store);
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    let mut candidates = Vec::new();
    for path in paths
        .into_iter()
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
    {
        let Some(conversation_id) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if validate_conversation_id(conversation_id).is_err()
            || store.root.join(conversation_id).is_dir()
            || store.legacy_transcript_path_for(conversation_id)?.is_file()
        {
            continue;
        }
        let sidecar = read_sidecar_path(store, &path)?;
        let archive = archive_path(store, &sidecar.manifest.conversation_id);
        if !archive.is_file() {
            continue;
        }
        if archive.metadata()?.len() != sidecar.archive_compressed_bytes {
            return Err(MezError::invalid_state(
                "session archive compressed size does not match its sidecar",
            ));
        }
        candidates.push(candidate_from_sidecar(sidecar));
    }
    Ok(candidates)
}

pub(super) fn archived_catalog_candidate(
    store: &AgentTranscriptStore,
    conversation_id: &str,
) -> Result<Option<CatalogCandidate>> {
    if !archived_payloads_exist(store, conversation_id)? {
        return Ok(None);
    }
    validate_installed_archive_payload(store, conversation_id)
        .map(candidate_from_sidecar)
        .map(Some)
}

/// Returns whether one archived catalog row still has both authoritative files.
pub(super) fn archived_payloads_exist(
    store: &AgentTranscriptStore,
    conversation_id: &str,
) -> Result<bool> {
    validate_conversation_id(conversation_id)?;
    let archive = archive_path(store, conversation_id);
    let sidecar_path = archive_sidecar_path(store, conversation_id);
    if !archive.is_file() || !sidecar_path.is_file() {
        return Ok(false);
    }
    let sidecar = read_sidecar_path(store, &sidecar_path)?;
    Ok(archive.metadata()?.len() == sidecar.archive_compressed_bytes)
}

/// Fully verifies one installed archive when repair or crash recovery requires it.
fn validate_installed_archive_payload(
    store: &AgentTranscriptStore,
    conversation_id: &str,
) -> Result<ArchiveSidecar> {
    let sidecar = read_sidecar(store, conversation_id)?;
    verify_installed_archive(store, conversation_id, &sidecar)?;
    validate_archive(
        &archive_path(store, conversation_id),
        conversation_id,
        Some(&sidecar.manifest),
        false,
        None,
    )?;
    Ok(sidecar)
}

fn archive_directory(store: &AgentTranscriptStore) -> PathBuf {
    store.root.join(ARCHIVE_DIRECTORY_NAME)
}

fn archive_recovery_directory(store: &AgentTranscriptStore) -> PathBuf {
    store.root.join(ARCHIVE_RECOVERY_DIRECTORY_NAME)
}

fn archive_recovery_journal_path(store: &AgentTranscriptStore, conversation_id: &str) -> PathBuf {
    archive_recovery_directory(store).join(format!("{conversation_id}.json"))
}

fn read_archive_recovery_journal(
    store: &AgentTranscriptStore,
    path: &Path,
) -> Result<ArchiveRecoveryJournal> {
    let journal: ArchiveRecoveryJournal =
        serde_json::from_slice(&fs::read(path)?).map_err(|error| {
            MezError::invalid_args(format!("archive recovery journal decode failed: {error}"))
        })?;
    if journal.version != ARCHIVE_RECOVERY_VERSION {
        return Err(MezError::invalid_args(
            "unsupported archive recovery journal version",
        ));
    }
    validate_conversation_id(&journal.conversation_id)?;
    payload_layout_from_name(&journal.payload_layout)?;
    if path != archive_recovery_journal_path(store, &journal.conversation_id) {
        return Err(MezError::invalid_args(
            "archive recovery journal filename does not match conversation id",
        ));
    }
    Ok(journal)
}

fn archive_path(store: &AgentTranscriptStore, conversation_id: &str) -> PathBuf {
    archive_directory(store).join(format!("{conversation_id}.tar.zst"))
}

fn archive_sidecar_path(store: &AgentTranscriptStore, conversation_id: &str) -> PathBuf {
    archive_directory(store).join(format!("{conversation_id}.json"))
}

fn temporary_archive_path(store: &AgentTranscriptStore, conversation_id: &str) -> PathBuf {
    archive_directory(store).join(format!(".{conversation_id}.tar.zst.tmp"))
}

fn temporary_sidecar_path(store: &AgentTranscriptStore, conversation_id: &str) -> PathBuf {
    archive_directory(store).join(format!(".{conversation_id}.json.tmp"))
}

fn restore_temporary_path(store: &AgentTranscriptStore, conversation_id: &str) -> PathBuf {
    store
        .root
        .join(format!(".archive-restore-{conversation_id}"))
}

fn staged_active_payload(
    store: &AgentTranscriptStore,
    conversation_id: &str,
    layout: CatalogPayloadLayout,
) -> Result<StagedActivePayload> {
    Ok(match layout {
        CatalogPayloadLayout::Directory => StagedActivePayload::Directory {
            staged: store.root.join(format!(".archive-stage-{conversation_id}")),
            active: store.root.join(conversation_id),
        },
        CatalogPayloadLayout::LegacyTsv => StagedActivePayload::Legacy {
            staged: store
                .root
                .join(format!(".archive-stage-{conversation_id}.tsv")),
            active: store.legacy_transcript_path_for(conversation_id)?,
        },
    })
}

fn manifest_from_record(
    record: &catalog::CatalogRecord,
    archived_at_unix_seconds: u64,
    named_at_unix_seconds: Option<u64>,
) -> ArchiveManifest {
    let session = &record.session;
    let summary = &session.summary;
    ArchiveManifest {
        version: ARCHIVE_FORMAT_VERSION,
        conversation_id: summary.conversation_id.clone(),
        archived_at_unix_seconds,
        entries: summary.entries,
        first_created_at_unix_seconds: summary.first_created_at_unix_seconds,
        last_created_at_unix_seconds: summary.last_created_at_unix_seconds,
        last_turn_id: summary.last_turn_id.clone(),
        agent_id: summary.agent_id.clone(),
        pane_id: summary.pane_id.clone(),
        directory: summary.directory.clone(),
        initial_prompt: summary.initial_prompt.clone(),
        latest_user_prompt: summary.latest_user_prompt.clone(),
        name: session.name.clone(),
        named_at_unix_seconds,
        conversation_kind: conversation_kind_name(session.conversation_kind).to_string(),
        has_transcript: record.has_transcript,
        has_presentation: record.has_presentation,
        payload_layout: record.payload_layout.as_str().to_string(),
    }
}

fn candidate_from_sidecar(sidecar: ArchiveSidecar) -> CatalogCandidate {
    let manifest = sidecar.manifest;
    CatalogCandidate {
        summary: summary_from_manifest(&manifest),
        name: manifest.name,
        named_at_unix_seconds: manifest.named_at_unix_seconds,
        conversation_kind: conversation_kind_from_name(&manifest.conversation_kind)
            .unwrap_or(AgentConversationKind::Root),
        has_transcript: manifest.has_transcript,
        has_presentation: manifest.has_presentation,
        payload_layout: payload_layout_from_name(&manifest.payload_layout)
            .unwrap_or(CatalogPayloadLayout::Directory),
        archived_at_unix_seconds: Some(manifest.archived_at_unix_seconds),
        archive_compressed_bytes: Some(sidecar.archive_compressed_bytes),
        archive_sha256: Some(sidecar.archive_sha256),
    }
}

fn info_from_sidecar(sidecar: ArchiveSidecar) -> ArchivedSessionInfo {
    let manifest = sidecar.manifest;
    ArchivedSessionInfo {
        conversation_id: manifest.conversation_id.clone(),
        archived_at_unix_seconds: manifest.archived_at_unix_seconds,
        compressed_bytes: sidecar.archive_compressed_bytes,
        sha256: sidecar.archive_sha256,
        summary: summary_from_manifest(&manifest),
        name: manifest.name,
        conversation_kind: conversation_kind_from_name(&manifest.conversation_kind)
            .unwrap_or(AgentConversationKind::Root),
    }
}

fn summary_from_manifest(manifest: &ArchiveManifest) -> ConversationSummary {
    ConversationSummary {
        conversation_id: manifest.conversation_id.clone(),
        entries: manifest.entries,
        first_created_at_unix_seconds: manifest.first_created_at_unix_seconds,
        last_created_at_unix_seconds: manifest.last_created_at_unix_seconds,
        last_turn_id: manifest.last_turn_id.clone(),
        agent_id: manifest.agent_id.clone(),
        pane_id: manifest.pane_id.clone(),
        directory: manifest.directory.clone(),
        initial_prompt: manifest.initial_prompt.clone(),
        latest_user_prompt: manifest.latest_user_prompt.clone(),
    }
}

fn conversation_kind_name(kind: AgentConversationKind) -> &'static str {
    match kind {
        AgentConversationKind::Root => "root",
        AgentConversationKind::Subagent => "subagent",
    }
}

fn conversation_kind_from_name(value: &str) -> Result<AgentConversationKind> {
    match value {
        "root" => Ok(AgentConversationKind::Root),
        "subagent" => Ok(AgentConversationKind::Subagent),
        _ => Err(MezError::invalid_args(
            "archive sidecar has invalid conversation kind",
        )),
    }
}

fn payload_layout_from_name(value: &str) -> Result<CatalogPayloadLayout> {
    match value {
        "directory" => Ok(CatalogPayloadLayout::Directory),
        "legacy-tsv" => Ok(CatalogPayloadLayout::LegacyTsv),
        _ => Err(MezError::invalid_args(
            "archive sidecar has invalid payload layout",
        )),
    }
}

fn read_sidecar(store: &AgentTranscriptStore, conversation_id: &str) -> Result<ArchiveSidecar> {
    read_sidecar_path(store, &archive_sidecar_path(store, conversation_id))
}

fn read_sidecar_path(store: &AgentTranscriptStore, path: &Path) -> Result<ArchiveSidecar> {
    let bytes = fs::read(path)?;
    let sidecar: ArchiveSidecar = serde_json::from_slice(&bytes).map_err(|error| {
        MezError::invalid_args(format!("archive sidecar decode failed: {error}"))
    })?;
    validate_sidecar(store, path, &sidecar)?;
    Ok(sidecar)
}

fn validate_sidecar(
    store: &AgentTranscriptStore,
    path: &Path,
    sidecar: &ArchiveSidecar,
) -> Result<()> {
    if sidecar.version != ARCHIVE_FORMAT_VERSION
        || sidecar.manifest.version != ARCHIVE_FORMAT_VERSION
    {
        return Err(MezError::invalid_args(
            "unsupported session archive version",
        ));
    }
    validate_conversation_id(&sidecar.manifest.conversation_id)?;
    if sidecar.manifest.archived_at_unix_seconds == 0
        || sidecar.archive_compressed_bytes == 0
        || !valid_sha256(&sidecar.archive_sha256)
    {
        return Err(MezError::invalid_args(
            "archive sidecar lifecycle metadata is invalid",
        ));
    }
    conversation_kind_from_name(&sidecar.manifest.conversation_kind)?;
    payload_layout_from_name(&sidecar.manifest.payload_layout)?;
    let expected = archive_sidecar_path(store, &sidecar.manifest.conversation_id);
    if path != expected {
        return Err(MezError::invalid_args(
            "archive sidecar filename does not match conversation id",
        ));
    }
    Ok(())
}

fn write_sidecar(path: &Path, sidecar: &ArchiveSidecar) -> Result<()> {
    remove_if_exists(path)?;
    let bytes = serde_json::to_vec(sidecar).map_err(|error| {
        MezError::invalid_args(format!("archive sidecar encode failed: {error}"))
    })?;
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    set_private_file_permissions(path)
}

fn build_archive(
    source: &Path,
    conversation_id: &str,
    manifest: &ArchiveManifest,
    output: &Path,
) -> Result<()> {
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)?;
    set_private_file_permissions(output)?;
    let encoder = zstd::stream::Encoder::new(file, 3)
        .map_err(|error| MezError::invalid_state(format!("archive compression failed: {error}")))?;
    let mut builder = tar::Builder::new(encoder);
    append_directory_header(&mut builder, Path::new(conversation_id))?;
    let mut entries = 1usize;
    let mut bytes = 0u64;
    append_source_tree(
        &mut builder,
        source,
        source,
        conversation_id,
        &mut entries,
        &mut bytes,
    )?;
    let manifest_bytes = serde_json::to_vec(manifest).map_err(|error| {
        MezError::invalid_args(format!("archive manifest encode failed: {error}"))
    })?;
    entries = entries.saturating_add(1);
    bytes = bytes.saturating_add(u64::try_from(manifest_bytes.len()).unwrap_or(u64::MAX));
    enforce_archive_bounds(entries, bytes)?;
    append_bytes(
        &mut builder,
        &Path::new(conversation_id).join(ARCHIVE_MANIFEST_FILE_NAME),
        &manifest_bytes,
    )?;
    let encoder = builder.into_inner().map_err(|error| {
        MezError::invalid_state(format!("archive tar finalization failed: {error}"))
    })?;
    let file = encoder.finish().map_err(|error| {
        MezError::invalid_state(format!("archive compression finalization failed: {error}"))
    })?;
    file.sync_all()?;
    Ok(())
}

fn append_source_tree<W: Write>(
    builder: &mut tar::Builder<W>,
    root: &Path,
    directory: &Path,
    conversation_id: &str,
    entries: &mut usize,
    bytes: &mut u64,
) -> Result<()> {
    let mut children = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    children.sort();
    for path in children {
        let metadata = fs::symlink_metadata(&path)?;
        let relative = path.strip_prefix(root).map_err(|_| {
            MezError::invalid_state("archive source escaped its conversation directory")
        })?;
        if relative == Path::new(ARCHIVE_MANIFEST_FILE_NAME) {
            return Err(MezError::conflict(
                "active session contains reserved archive manifest filename",
            ));
        }
        let archive_path = Path::new(conversation_id).join(relative);
        *entries = entries.saturating_add(1);
        if metadata.is_dir() {
            enforce_archive_bounds(*entries, *bytes)?;
            append_directory_header(builder, &archive_path)?;
            append_source_tree(builder, root, &path, conversation_id, entries, bytes)?;
        } else if metadata.is_file() {
            *bytes = bytes.saturating_add(metadata.len());
            enforce_archive_bounds(*entries, *bytes)?;
            let mut file = File::open(&path)?;
            append_reader(builder, &archive_path, metadata.len(), &mut file)?;
        } else {
            return Err(MezError::invalid_args(
                "session archive source contains a link or special file",
            ));
        }
    }
    Ok(())
}

fn append_directory_header<W: Write>(builder: &mut tar::Builder<W>, path: &Path) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_size(0);
    header.set_mode(0o700);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    builder.append_data(&mut header, path, std::io::empty())?;
    Ok(())
}

fn append_bytes<W: Write>(builder: &mut tar::Builder<W>, path: &Path, bytes: &[u8]) -> Result<()> {
    let mut reader = bytes;
    append_reader(
        builder,
        path,
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        &mut reader,
    )
}

fn append_reader<W: Write, R: Read>(
    builder: &mut tar::Builder<W>,
    path: &Path,
    size: u64,
    reader: &mut R,
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(size);
    header.set_mode(0o600);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    builder.append_data(&mut header, path, reader)?;
    Ok(())
}

fn validate_archive(
    path: &Path,
    conversation_id: &str,
    expected_manifest: Option<&ArchiveManifest>,
    extract: bool,
    extraction_root: Option<&Path>,
) -> Result<()> {
    let file = File::open(path)?;
    let decoder = zstd::stream::read::Decoder::new(file).map_err(|error| {
        MezError::invalid_args(format!("archive decompression failed: {error}"))
    })?;
    let mut archive = tar::Archive::new(decoder);
    let mut entry_count = 0usize;
    let mut total_bytes = 0u64;
    let mut manifest = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        entry_count = entry_count.saturating_add(1);
        let size = entry.header().size()?;
        total_bytes = total_bytes.saturating_add(size);
        enforce_archive_bounds(entry_count, total_bytes)?;
        let entry_path = entry.path()?.into_owned();
        validate_archive_path(&entry_path, conversation_id)?;
        let entry_type = entry.header().entry_type();
        if !entry_type.is_dir() && !entry_type.is_file() {
            return Err(MezError::invalid_args(
                "session archive contains a link or special entry",
            ));
        }
        let manifest_path = Path::new(conversation_id).join(ARCHIVE_MANIFEST_FILE_NAME);
        if entry_path == manifest_path {
            if manifest.is_some() || !entry_type.is_file() {
                return Err(MezError::invalid_args(
                    "session archive manifest is invalid",
                ));
            }
            let mut bytes = Vec::new();
            entry.take(1024 * 1024).read_to_end(&mut bytes)?;
            manifest = Some(serde_json::from_slice::<ArchiveManifest>(&bytes).map_err(
                |error| MezError::invalid_args(format!("archive manifest decode failed: {error}")),
            )?);
            continue;
        }
        if extract {
            let root = extraction_root
                .ok_or_else(|| MezError::invalid_state("archive extraction root is missing"))?;
            install_archive_entry(root, &entry_path, entry_type, &mut entry)?;
        }
    }
    let manifest =
        manifest.ok_or_else(|| MezError::invalid_args("session archive manifest is missing"))?;
    if manifest.version != ARCHIVE_FORMAT_VERSION
        || manifest.conversation_id != conversation_id
        || expected_manifest
            .is_some_and(|expected| !archive_payload_manifests_match(expected, &manifest))
    {
        return Err(MezError::invalid_args(
            "session archive manifest does not match sidecar",
        ));
    }
    Ok(())
}

/// Compares immutable payload metadata while allowing names to change after archival.
fn archive_payload_manifests_match(left: &ArchiveManifest, right: &ArchiveManifest) -> bool {
    let mut left = left.clone();
    let mut right = right.clone();
    left.name = None;
    left.named_at_unix_seconds = None;
    right.name = None;
    right.named_at_unix_seconds = None;
    left == right
}

fn install_archive_entry<R: Read>(
    root: &Path,
    entry_path: &Path,
    entry_type: tar::EntryType,
    reader: &mut R,
) -> Result<()> {
    let destination = root.join(entry_path);
    if entry_type.is_dir() {
        fs::create_dir_all(&destination)?;
        set_private_dir_permissions(&destination)?;
        return Ok(());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| MezError::invalid_args("archive entry has no parent"))?;
    fs::create_dir_all(parent)?;
    set_private_dir_permissions(parent)?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&destination)?;
    std::io::copy(reader, &mut output)?;
    output.sync_all()?;
    set_private_file_permissions(&destination)
}

pub(super) fn validate_archive_path(path: &Path, conversation_id: &str) -> Result<()> {
    if path.is_absolute() {
        return Err(MezError::invalid_args(
            "session archive contains an absolute path",
        ));
    }
    let mut components = path.components();
    match components.next() {
        Some(Component::Normal(value)) if value == conversation_id => {}
        _ => {
            return Err(MezError::invalid_args(
                "session archive has an unexpected top-level directory",
            ));
        }
    }
    if components.any(|component| !matches!(component, Component::Normal(_))) {
        return Err(MezError::invalid_args(
            "session archive contains path traversal",
        ));
    }
    Ok(())
}

fn enforce_archive_bounds(entries: usize, bytes: u64) -> Result<()> {
    if entries > MAX_ARCHIVE_ENTRIES {
        return Err(MezError::invalid_args(
            "session archive has too many entries",
        ));
    }
    if bytes > MAX_ARCHIVE_UNCOMPRESSED_BYTES {
        return Err(MezError::invalid_args(
            "session archive exceeds the uncompressed size limit",
        ));
    }
    Ok(())
}

fn archive_digest(path: &Path) -> Result<(u64, String)> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut bytes = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        digest.update(&buffer[..read]);
    }
    let sha256 = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok((bytes, sha256))
}

fn verify_installed_archive(
    store: &AgentTranscriptStore,
    conversation_id: &str,
    sidecar: &ArchiveSidecar,
) -> Result<()> {
    let path = archive_path(store, conversation_id);
    let (bytes, digest) = archive_digest(&path)?;
    if bytes != sidecar.archive_compressed_bytes || digest != sidecar.archive_sha256 {
        return Err(MezError::invalid_args(
            "session archive digest verification failed",
        ));
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn restore_staged_payload(staged: &StagedActivePayload) -> Result<()> {
    match staged {
        StagedActivePayload::Directory { staged, active }
        | StagedActivePayload::Legacy { staged, active } => {
            if staged.exists() && !active.exists() {
                fs::rename(staged, active)?;
            }
        }
    }
    Ok(())
}

fn remove_staged_payload(staged: &StagedActivePayload) -> Result<()> {
    match staged {
        StagedActivePayload::Directory { staged, .. } => {
            if staged.exists() {
                fs::remove_dir_all(staged)?;
            }
        }
        StagedActivePayload::Legacy { staged, .. } => remove_if_exists(staged)?,
    }
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path).map_err(Into::into),
        Ok(_) => fs::remove_file(path).map_err(Into::into),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
