//! Session-isolated active binding checkpoints and conservative legacy import.
//!
//! Older daemons rewrite a shared TSV through a fixed temporary inode without
//! an index lock. New daemons never write that file or its temporary pathname:
//! each Mezzanine session owns a versioned checkpoint under a private namespace.
//! A bounded advisory lock serializes cooperating writers of that checkpoint;
//! exclusive staging prevents inode sharing even across different sessions.
//! Conversation checkpoint locks are acquired before this lock, never inside it.
//!
//! Legacy import validates a stable observed snapshot and leaves the source
//! untouched. Only one terminal standalone bracket after fully valid records is
//! recoverable, after an exact private backup is durable. All other corruption
//! remains an explicit error. Once a scoped checkpoint exists (even empty), it
//! is authoritative and later old-daemon writes cannot replace its bindings.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mez_agent::transcript::AgentSessionMetadata;
use rustix::fs::{FlockOperation, OFlags, flock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::AgentTranscriptStore;
use super::encoding::{decode_agent_session_metadata, encode_agent_session_metadata};
use crate::error::{MezError, MezErrorKind, Result};

const DIRECTORY: &str = ".active-agent-session-metadata-v1";
const VERSION: u64 = 1;
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const LOCK_TIMEOUT: Duration = Duration::from_secs(2);
static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);

/// Acquires an owned private file lock with a finite wait. The global ordering
/// is sorted conversation locks, session checkpoint lock, then backup lock.
fn lock_active_metadata_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC).bits() as i32)
        .open(path)?;
    let owned = file.metadata()?;
    if !owned.is_file()
        || owned.nlink() != 1
        || owned.uid() != rustix::process::geteuid().as_raw()
        || owned.mode() & 0o077 != 0
    {
        return Err(MezError::invalid_state(
            "active metadata lock must be an owned private single-link file",
        ));
    }
    let start = Instant::now();
    loop {
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => break,
            Err(error)
                if error == rustix::io::Errno::WOULDBLOCK && start.elapsed() < LOCK_TIMEOUT =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => {
                return Err(MezError::invalid_state(
                    "active metadata lock timed out; another writer still owns the checkpoint or backup",
                ));
            }
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
    }
    let current = fs::symlink_metadata(path)?;
    if owned.dev() != current.dev() || owned.ino() != current.ino() || !current.is_file() {
        return Err(MezError::conflict(
            "active metadata lock pathname changed before ownership",
        ));
    }
    Ok(file)
}

/// Durable provenance for a narrowly recovered legacy fragment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyRecovery {
    digest: String,
    line: usize,
}

/// Versioned session checkpoint containing existing validated TSV row contracts.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u64,
    mezzanine_session_id: String,
    rows: Vec<String>,
    legacy_recovery: Option<LegacyRecovery>,
}

/// Whether a failed publication has crossed the authoritative rename boundary.
/// Compound sidecar checkpoints must not roll back captures after publication.
pub(super) struct PublicationFailure {
    pub(super) error: MezError,
    pub(super) published: bool,
}

impl PublicationFailure {
    /// Preserves authoritative-rename uncertainty at public import/save boundaries.
    fn into_error(self) -> MezError {
        if self.published {
            MezError::new(
                self.error.kind(),
                format!(
                    "active binding checkpoint published but durability is uncertain: {}",
                    self.error,
                ),
            )
        } else {
            self.error
        }
    }
}

/// Rejects empty identifiers before deriving private namespace paths.
fn validate_session_id(id: &str) -> Result<()> {
    if id.trim().is_empty() {
        return Err(MezError::invalid_args(
            "mezzanine session id must not be empty",
        ));
    }
    Ok(())
}

/// Domain-separated, path-safe session namespace; original ids remain in data.
fn session_key(id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"mez-active-session-checkpoint-v1\0");
    digest.update(id.as_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Canonical lowercase content digest for preserved legacy bytes.
fn source_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Reads a finite, regular, single-link file without following symlinks or
/// blocking on special nodes. Detects observed mutation/path replacement.
fn read_snapshot(path: &Path) -> Result<Option<Vec<u8>>> {
    read_snapshot_with_links(path, false)
}

/// Backup digests also admit an extra staging link left by a crashed publisher.
/// These are immutable audit bytes, checked against the expected digest, not
/// writable checkpoint or lock ownership. No orphaned path is deleted blindly.
fn read_snapshot_with_links(path: &Path, backup: bool) -> Result<Option<Vec<u8>>> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC).bits() as i32);
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let before = file.metadata()?;
    if !before.is_file() || (!backup && before.nlink() != 1) || before.len() > MAX_BYTES {
        return Err(MezError::invalid_state(format!(
            "active metadata {} is not a bounded regular single-link file",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    (&mut file).take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let current = fs::symlink_metadata(path)?;
    let revision = |metadata: &fs::Metadata| {
        (
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    };
    if bytes.len() as u64 > MAX_BYTES
        || revision(&before) != revision(&after)
        || revision(&after) != revision(&current)
        || !current.is_file()
    {
        return Err(MezError::conflict(format!(
            "active metadata {} changed during snapshot capture; retry after the writer settles",
            path.display()
        )));
    }
    Ok(Some(bytes))
}

/// Produces content-free row diagnostics, never the rejected field values.
fn row_error(path: &Path, line: usize, row: &str, kind: MezErrorKind) -> MezError {
    let marker = row.split('\t').next().unwrap_or_default();
    let version = marker
        .strip_prefix("mez-agent-session-metadata/")
        .and_then(|value| value.parse::<u64>().ok())
        .map_or_else(|| "unrecognized".to_string(), |version| version.to_string());
    MezError::new(
        kind,
        format!(
            "active metadata {} line {line}: invalid record (version={version}, fields={}); source was not modified",
            path.display(),
            row.split('\t').count(),
        ),
    )
}

/// Parses existing row versions while binding every row to its checkpoint owner.
fn checkpoint_records(
    path: &Path,
    checkpoint: &Checkpoint,
    id: &str,
) -> Result<Vec<AgentSessionMetadata>> {
    if checkpoint.version != VERSION {
        return Err(MezError::invalid_args(format!(
            "active metadata {}: unsupported checkpoint version={}",
            path.display(),
            checkpoint.version
        )));
    }
    if checkpoint.mezzanine_session_id != id {
        return Err(MezError::invalid_args(format!(
            "active metadata {} has mismatched session ownership",
            path.display()
        )));
    }
    checkpoint
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let record = decode_agent_session_metadata(row)
                .map_err(|error| row_error(path, index + 1, row, error.kind()))?;
            if record.mezzanine_session_id != id {
                return Err(MezError::conflict(format!(
                    "active metadata {} row {} belongs to a different session",
                    path.display(),
                    index + 1
                )));
            }
            Ok(record)
        })
        .collect()
}

/// Decodes a current envelope without exposing JSON payloads in errors.
fn read_checkpoint(path: &Path, id: &str) -> Result<Option<Checkpoint>> {
    let Some(bytes) = read_snapshot(path)? else {
        return Ok(None);
    };
    let checkpoint: Checkpoint = serde_json::from_slice(&bytes).map_err(|error| {
        MezError::invalid_args(format!(
            "active metadata {}: malformed checkpoint envelope at JSON line {} column {}",
            path.display(),
            error.line(),
            error.column()
        ))
    })?;
    checkpoint_records(path, &checkpoint, id)?;
    if let Some(recovery) = &checkpoint.legacy_recovery
        && (recovery.line == 0
            || recovery.digest.len() != 64
            || !recovery
                .digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        return Err(MezError::invalid_args(format!(
            "active metadata {}: invalid legacy recovery provenance",
            path.display()
        )));
    }
    if let Some(recovery) = &checkpoint.legacy_recovery {
        let directory = path
            .parent()
            .ok_or_else(|| MezError::invalid_state("active metadata checkpoint has no parent"))?;
        let backup = directory.join(format!("legacy-{}.tsv", recovery.digest));
        let bytes = read_snapshot_with_links(&backup, true)?.ok_or_else(|| {
            MezError::invalid_state("active metadata legacy recovery backup is missing")
        })?;
        if source_digest(&bytes) != recovery.digest {
            return Err(MezError::conflict(
                "active metadata legacy recovery backup bytes changed",
            ));
        }
    }
    Ok(Some(checkpoint))
}

/// Transaction-owned staging inode, removed on prepublication error only.
struct Stage {
    path: PathBuf,
    file: File,
}

impl Stage {
    /// Allocates a private exclusive inode; existing names are never truncated.
    fn create(directory: &Path) -> Result<Self> {
        for _ in 0..32 {
            let sequence = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = directory.join(format!(".stage-{}-{nanos}-{sequence}", std::process::id()));
            let file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags((OFlags::NOFOLLOW | OFlags::CLOEXEC).bits() as i32)
                .open(&path);
            match file {
                Ok(file) => return Ok(Self { path, file }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(MezError::invalid_state(
            "active metadata staging name allocation exhausted",
        ))
    }
}

impl Drop for Stage {
    /// Cleans only the owned inode, never a colliding/replaced staging pathname.
    fn drop(&mut self) {
        if let (Ok(owned), Ok(current)) = (self.file.metadata(), fs::symlink_metadata(&self.path))
            && owned.dev() == current.dev()
            && owned.ino() == current.ino()
            && current.is_file()
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl AgentTranscriptStore {
    /// Injects one exact local I/O phase failure without global test coupling.
    #[cfg(test)]
    pub(crate) fn fail_active_metadata_phase_for_tests(&self, phase: u8) {
        self.active_metadata_fault.store(phase, Ordering::SeqCst);
    }

    /// Consumes only the armed publication/recovery phase fault.
    #[cfg(test)]
    fn check_active_metadata_fault(&self, phase: u8) -> Result<()> {
        if self
            .active_metadata_fault
            .compare_exchange(phase, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Err(MezError::invalid_state(format!(
                "injected active metadata phase {phase} failure"
            )));
        }
        Ok(())
    }
    /// Path to the authoritative versioned checkpoint for one Mezzanine session.
    /// Derivation performs no I/O and grants no session/conversation authority.
    pub fn agent_session_metadata_checkpoint_file(&self, id: &str) -> PathBuf {
        self.root
            .join(DIRECTORY)
            .join(format!("{}.json", session_key(id)))
    }

    /// Creates the private namespace without changing legacy files or modes.
    fn prepare_active_metadata_directory(&self) -> Result<PathBuf> {
        self.ensure_store_dir()?;
        let directory = self.root.join(DIRECTORY);
        match fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let metadata = fs::symlink_metadata(&directory)?;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err(MezError::invalid_state(
                "active metadata namespace must be an owned private directory",
            ));
        }
        // A failed creator or another process may have left an unsynced entry.
        // AlreadyExists is not proof that this namespace survived publication.
        #[cfg(test)]
        self.check_active_metadata_fault(6)?;
        File::open(&self.root)?.sync_all()?;
        Ok(directory)
    }

    /// Owns one session's metadata mutation for at most a bounded lock wait.
    /// Callers holding conversation locks must acquire this lock afterward.
    pub(super) fn lock_agent_session_metadata(&self, id: &str) -> Result<File> {
        validate_session_id(id)?;
        let directory = self.prepare_active_metadata_directory()?;
        let path = directory.join(format!("{}.lock", session_key(id)));
        lock_active_metadata_file(&path)
    }

    /// Preserves complete legacy source durably under its exact content digest.
    /// Exclusive creation is atomic; a reused backup must match every byte.
    fn preserve_legacy_metadata(&self, bytes: &[u8], line: usize) -> Result<LegacyRecovery> {
        let directory = self.root.join(DIRECTORY);
        let digest = source_digest(bytes);
        let _backup_lock =
            lock_active_metadata_file(&directory.join(format!("legacy-{digest}.lock")))?;
        let backup = directory.join(format!("legacy-{digest}.tsv"));
        let mut stage = Stage::create(&directory)?;
        stage.file.write_all(bytes)?;
        stage.file.sync_all()?;
        match fs::hard_link(&stage.path, &backup) {
            Ok(()) => {
                #[cfg(test)]
                self.check_active_metadata_fault(5)?;
                // Retire our extra link before readers require single-link evidence.
                fs::remove_file(&stage.path)?;
                File::open(&directory)?.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if read_snapshot_with_links(&backup, true)?.as_deref() != Some(bytes) {
                    return Err(MezError::conflict(
                        "active metadata recovery backup conflicts with source bytes",
                    ));
                }
                File::open(&directory)?.sync_all()?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(LegacyRecovery { digest, line })
    }

    /// Imports only validated bindings from a stable captured legacy snapshot.
    /// A terminal bracket is quarantined only after a durable full-byte backup.
    fn legacy_checkpoint(&self, id: &str) -> Result<Checkpoint> {
        let path = self.agent_session_metadata_file();
        let Some(bytes) = read_snapshot(&path)? else {
            return Ok(Checkpoint {
                version: VERSION,
                mezzanine_session_id: id.to_string(),
                rows: Vec::new(),
                legacy_recovery: None,
            });
        };
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            MezError::invalid_args(format!(
                "active metadata {} is not UTF-8; source was not modified",
                path.display()
            ))
        })?;
        let nonempty = text
            .lines()
            .enumerate()
            .filter(|(_, row)| !row.trim().is_empty())
            .collect::<Vec<_>>();
        let mut records = Vec::new();
        let mut recovery = None;
        for (position, (index, row)) in nonempty.iter().enumerate() {
            match decode_agent_session_metadata(row) {
                Ok(record) => records.push(record),
                Err(_)
                    if *row == "]"
                        && position + 1 == nonempty.len()
                        && !records.is_empty()
                        && bytes.ends_with(b"]\n") =>
                {
                    recovery = Some(self.preserve_legacy_metadata(&bytes, index + 1)?);
                }
                Err(error) => return Err(row_error(&path, index + 1, row, error.kind())),
            }
        }
        let rows = records
            .into_iter()
            .filter(|record| record.mezzanine_session_id == id)
            .map(|record| encode_agent_session_metadata(&record))
            .collect::<Result<Vec<_>>>()?;
        Ok(Checkpoint {
            version: VERSION,
            mezzanine_session_id: id.to_string(),
            rows,
            legacy_recovery: recovery,
        })
    }

    /// Publishes an already validated envelope while retaining publication phase
    /// on error. No old fixed temporary filename is read, written or removed.
    fn publish_active_metadata(
        &self,
        id: &str,
        checkpoint: &Checkpoint,
    ) -> std::result::Result<usize, PublicationFailure> {
        let mut published = false;
        let result = (|| -> Result<usize> {
            checkpoint_records(
                &self.agent_session_metadata_checkpoint_file(id),
                checkpoint,
                id,
            )?;
            let bytes = serde_json::to_vec(checkpoint).map_err(|_| {
                MezError::invalid_state("active metadata checkpoint encoding failed")
            })?;
            if bytes.len() as u64 > MAX_BYTES {
                return Err(MezError::invalid_args(
                    "active metadata checkpoint exceeds its finite size bound",
                ));
            }
            let directory = self.root.join(DIRECTORY);
            let mut stage = Stage::create(&directory)?;
            stage.file.write_all(&bytes)?;
            #[cfg(test)]
            self.check_active_metadata_fault(1)?;
            stage.file.sync_all()?;
            #[cfg(test)]
            self.check_active_metadata_fault(2)?;
            let owned = stage.file.metadata()?;
            let current = fs::symlink_metadata(&stage.path)?;
            if owned.dev() != current.dev() || owned.ino() != current.ino() || !current.is_file() {
                return Err(MezError::conflict(
                    "active metadata staging inode changed before publication",
                ));
            }
            fs::rename(&stage.path, self.agent_session_metadata_checkpoint_file(id))?;
            published = true;
            #[cfg(test)]
            self.check_active_metadata_fault(3)?;
            File::open(&directory)?.sync_all()?;
            #[cfg(test)]
            self.check_active_metadata_fault(4)?;
            Ok(checkpoint.rows.len())
        })();
        result.map_err(|error| PublicationFailure { error, published })
    }

    /// Loads one authoritative namespace or performs a checked one-time import.
    /// An empty scoped checkpoint is authoritative, never a legacy fallback.
    pub fn load_agent_session_metadata(&self, id: &str) -> Result<Vec<AgentSessionMetadata>> {
        let _lock = self.lock_agent_session_metadata(id)?;
        let path = self.agent_session_metadata_checkpoint_file(id);
        let checkpoint = match read_checkpoint(&path, id)? {
            Some(checkpoint) => checkpoint,
            None => {
                let checkpoint = self.legacy_checkpoint(id)?;
                self.publish_active_metadata(id, &checkpoint)
                    .map_err(PublicationFailure::into_error)?;
                checkpoint
            }
        };
        checkpoint_records(&path, &checkpoint, id)
    }

    /// Replaces only this session's bindings. Other sessions and legacy writers
    /// use independent files, so no shared read/merge can lose their updates.
    pub fn save_agent_session_metadata(
        &self,
        id: &str,
        records: &[AgentSessionMetadata],
    ) -> Result<usize> {
        let _lock = self.lock_agent_session_metadata(id)?;
        self.save_agent_session_metadata_locked(id, records)
            .map_err(PublicationFailure::into_error)
    }

    /// Compound checkpoint entry while the caller owns its session lock.
    pub(super) fn save_agent_session_metadata_locked(
        &self,
        id: &str,
        records: &[AgentSessionMetadata],
    ) -> std::result::Result<usize, PublicationFailure> {
        let result = (|| -> Result<Checkpoint> {
            validate_session_id(id)?;
            for record in records {
                record.validate()?;
                if record.mezzanine_session_id != id {
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
            let previous =
                match read_checkpoint(&self.agent_session_metadata_checkpoint_file(id), id)? {
                    Some(checkpoint) => checkpoint,
                    None => self.legacy_checkpoint(id)?,
                };
            Ok(Checkpoint {
                version: VERSION,
                mezzanine_session_id: id.to_string(),
                rows: records
                    .iter()
                    .map(encode_agent_session_metadata)
                    .collect::<Result<Vec<_>>>()?,
                legacy_recovery: previous.legacy_recovery,
            })
        })();
        let checkpoint = result.map_err(|error| PublicationFailure {
            error,
            published: false,
        })?;
        self.publish_active_metadata(id, &checkpoint)
    }

    /// Returns an actionable content-free notice for preserved legacy damage.
    /// The backup must still match the captured bytes before recovery is reported.
    pub(crate) fn agent_session_metadata_recovery_notice(
        &self,
        id: &str,
    ) -> Result<Option<String>> {
        let _lock = self.lock_agent_session_metadata(id)?;
        let Some(checkpoint) =
            read_checkpoint(&self.agent_session_metadata_checkpoint_file(id), id)?
        else {
            return Ok(None);
        };
        let Some(recovery) = checkpoint.legacy_recovery else {
            return Ok(None);
        };
        let backup = self
            .root
            .join(DIRECTORY)
            .join(format!("legacy-{}.tsv", recovery.digest));
        let bytes = read_snapshot_with_links(&backup, true)?.ok_or_else(|| {
            MezError::invalid_state("active metadata legacy recovery backup is missing")
        })?;
        if source_digest(&bytes) != recovery.digest {
            return Err(MezError::conflict(
                "active metadata legacy recovery backup bytes changed",
            ));
        }
        Ok(Some(format!(
            "agent: imported validated legacy bindings; quarantined a trailing fragment at {} line {}; exact source preserved at {}; legacy file left unchanged",
            self.agent_session_metadata_file().display(),
            recovery.line,
            backup.display()
        )))
    }
}
