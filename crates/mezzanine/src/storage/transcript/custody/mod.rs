//! Original physical destination for one canonical messaging custody operation.
//!
//! Native recipient input and neutral sender terminal handoff share physical
//! ownership while retaining distinct typed sources, namespaces and row kinds.
//! The original MMP owner must persist preparation before the final append and
//! publish its canonical commit journal only after `seal` succeeds. A retained
//! conversation lease spans both journals and the actual append. Preparation
//! freezes physical identities, the complete original prefix and exact row
//! bytes, so a retry cannot allocate a later sequence or adopt a replacement
//! archive. No method schedules a turn, grants native ownership, consumes an
//! outcome or ACKs mail. Native v3 persistence bytes remain unchanged.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::PathBuf;

use mez_agent::transcript::{TranscriptEntry, validate_conversation_id};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::AgentTranscriptStore;

mod receipt;
mod source;
use super::encoding::{decode_transcript_entry, encode_transcript_entry};
use crate::error::{MezError, Result};
pub(crate) use receipt::{
    CustodyAppendPlan, MAX_CUSTODY_APPEND_BYTES, MAX_CUSTODY_APPEND_ROWS,
    validate_custody_append_prefix,
};
pub(crate) use source::{CustodyAnchor, CustodyReceiptKind, CustodySource};

/// Conservative encoded bound including escaping of a bounded native frame.
const MAX_ROW_BYTES: usize = 16 * 1024 * 1024;

/// Descriptive filesystem identity, never authority to open a caller path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    device: u64,
    inode: u64,
}
impl Identity {
    /// Captures an already-open kernel object.
    fn of(file: &File) -> Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

/// Bodyless original append position, durable in the original MMP preparation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CustodyTranscriptPlan {
    root: Identity,
    directory: Identity,
    archive: Identity,
    conversation_id: String,
    prefix_bytes: u64,
    prefix_sha256: [u8; 32],
    /// Exact selected canonical archive row, independent of transport sequence.
    pub archive_sequence: u64,
    /// Digest of the complete encoded row including its newline terminator.
    pub canonical_sha256: [u8; 32],
    row_bytes: usize,
}
impl CustodyTranscriptPlan {
    /// Checks persisted framing before any bounded read or path derivation.
    pub(crate) fn validate(&self) -> Result<()> {
        self.validate_span(MAX_ROW_BYTES)
    }

    /// A canonical receipt may own a bounded contiguous batch as well as its
    /// final native row. Single-row MMP plans retain the narrower row bound.
    fn validate_span(&self, max_bytes: usize) -> Result<()> {
        validate_conversation_id(&self.conversation_id)?;
        if self.conversation_id.len() > 256
            || self.archive_sequence == 0
            || self.row_bytes == 0
            || self.row_bytes > max_bytes
            || self.root.inode == 0
            || self.directory.inode == 0
            || self.archive.inode == 0
            || self.canonical_sha256 == [0; 32]
        {
            return Err(unavailable());
        }
        Ok(())
    }
    /// Historical c1 may survive legitimate target cleanup. This checks only
    /// the named original root before optional receipt retirement; it grants no
    /// right to open or mutate a replacement conversation/archive.
    pub(crate) fn root_matches_store(&self, store: &AgentTranscriptStore) -> Result<bool> {
        match open_directory(&store.root) {
            Ok(root) => Ok(Identity::of(&root)? == self.root),
            Err(error) if error.io_kind() == Some(std::io::ErrorKind::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Binds the preparation to the original frozen native conversation.
    pub(crate) fn conversation_id(&self) -> &str {
        &self.conversation_id
    }
}

/// Live original archive and ordinary mutation lease; never serialized.
pub(crate) struct CustodyTranscriptLease {
    store: AgentTranscriptStore,
    conversation_id: String,
    lease: File,
    root: File,
    directory: File,
    archive: File,
    path: PathBuf,
}

impl AgentTranscriptStore {
    /// Probes and recovers canonical ownership under one actual conversation
    /// lock. Only the original MMP root supplies its verified source/journal
    /// snapshot; this method never seals input, ACKs mail or schedules work.
    /// A lookup-only absence can abort an incomplete unpublished stage only
    /// with neither original journal, before any canonical byte was permitted.
    pub(crate) fn recover_custody_lease(
        &self,
        anchor: &impl CustodyAnchor,
        store_id: &[u8; 16],
        occurrence: &[u8; 32],
        expected: Option<(&CustodySource, &CustodyTranscriptPlan)>,
        application_projection: bool,
    ) -> Result<(
        Option<std::sync::Arc<super::CustodyAppendReceipt>>,
        Option<CustodyTranscriptLease>,
    )> {
        let conversation = anchor.conversation_id();
        validate_conversation_id(conversation)?;
        if !self.root.try_exists()? {
            return if expected.is_none() && !application_projection {
                Ok((None, None))
            } else {
                Err(unavailable())
            };
        }
        let _root = open_directory(&self.root)?;
        let lock = self.acquire_conversation_lock(conversation)?;
        let canonical =
            self.custody_append_receipt_locked(conversation, anchor.kind(), occurrence, &lock)?;
        let stage = if canonical.is_none() {
            self.custody_stage_locked(conversation, anchor.kind(), occurrence, &lock)?
        } else {
            None
        };
        if canonical.is_none() && stage.is_none() && expected.is_none() && !application_projection {
            return Ok((None, None));
        }
        let create =
            canonical.is_none() && stage.is_none() && expected.is_none() && application_projection;
        let lease = self.custody_lease_locked(
            conversation,
            create,
            Some((anchor.kind(), occurrence)),
            lock,
        )?;
        let checked = |plan: &CustodyAppendPlan, rows: &[TranscriptEntry]| -> Result<()> {
            if !anchor.matches(plan.source(), store_id)
                || plan.source().occurrence() != *occurrence
                || expected
                    .is_some_and(|(source, input)| source != plan.source() || input != plan.input())
            {
                return Err(unavailable());
            }
            lease.check_custody_append_plan(plan, rows)
        };
        let canonical = match (canonical, stage) {
            (Some(canonical), _) => {
                checked(canonical.plan(), &canonical.rows()?)?;
                Some(canonical)
            }
            (None, Some(stage)) if stage.plan().is_some() => {
                checked(stage.plan().ok_or_else(unavailable)?, &stage.rows()?)?;
                Some(stage.publish(self, &lease.lease)?)
            }
            (None, Some(stage)) => {
                // A durable preparation can only follow final publication;
                // an incomplete stage alongside p1 is inconsistent, not absent.
                if expected.is_some() {
                    return Err(unavailable());
                }
                if !application_projection {
                    stage.abort_incomplete()?;
                    return Ok((None, None));
                }
                None
            }
            (None, None) => None,
        };
        Ok((canonical, Some(lease)))
    }

    /// Opens only the actual existing conversation. Missing backing refuses;
    /// creation is restricted to its empty private history file. The caller
    /// holds this lease through original preparation, append and commit journal.
    #[cfg(test)]
    pub(crate) fn custody_lease(
        &self,
        conversation_id: &str,
        create_empty: bool,
        occurrence: Option<(CustodyReceiptKind, &[u8; 32])>,
    ) -> Result<CustodyTranscriptLease> {
        validate_conversation_id(conversation_id)?;
        let lease = self.acquire_conversation_lock(conversation_id)?;
        self.custody_lease_locked(conversation_id, create_empty, occurrence, lease)
    }

    /// Builds the physical owner from this exact already-held conversation
    /// lock; cold receipt recovery must never release/reacquire between probe
    /// and promotion. Only this module supplies that real matching lock.
    fn custody_lease_locked(
        &self,
        conversation_id: &str,
        create_empty: bool,
        occurrence: Option<(CustodyReceiptKind, &[u8; 32])>,
        lease: File,
    ) -> Result<CustodyTranscriptLease> {
        let root = open_directory(&self.root)?;
        let directory = open_directory(&self.root.join(conversation_id))?;
        let foreign_receipts = match occurrence {
            Some((kind, occurrence)) => {
                self.has_foreign_custody_receipts_locked(conversation_id, kind, occurrence, &lease)?
            }
            None => self.has_unsettled_append_receipts_locked(conversation_id, &lease)?,
        };
        if self.active_delete_pending(conversation_id)? || foreign_receipts {
            return Err(unavailable());
        }
        let path = self.root.join(conversation_id).join("history.tsv");
        // Legacy backing is not silently promoted into a new empty history.
        if self.existing_transcript_path_for(conversation_id)? != path {
            return Err(unavailable());
        }
        let archive = OpenOptions::new()
            .read(true)
            .write(true)
            .create(create_empty)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)?;
        if !archive.metadata()?.is_file() || archive.metadata()?.nlink() != 1 {
            return Err(unavailable());
        }
        archive.sync_all()?;
        directory.sync_all()?;
        root.sync_all()?;
        let opened = CustodyTranscriptLease {
            store: self.clone(),
            conversation_id: conversation_id.into(),
            lease,
            root,
            directory,
            archive,
            path,
        };
        opened.validate_paths()?;
        Ok(opened)
    }
}

impl CustodyTranscriptLease {
    /// Reads only this lease's occurrence without recursively acquiring its
    /// kernel lock. Used for idempotent c1 image retirement after worker loss.
    pub(crate) fn lookup_append_receipt(
        &self,
        kind: CustodyReceiptKind,
        occurrence: &[u8; 32],
    ) -> Result<Option<std::sync::Arc<super::CustodyAppendReceipt>>> {
        self.store.custody_append_receipt_locked(
            &self.conversation_id,
            kind,
            occurrence,
            &self.lease,
        )
    }

    /// Freezes the actual next sequence and complete validated physical prefix.
    /// The caller must durably retain the returned plan before appending.
    #[cfg(test)]
    pub(crate) fn prepare(&self, row: &mut TranscriptEntry) -> Result<CustodyTranscriptPlan> {
        self.validate_paths()?;
        self.store
            .validate_restored_transcript(&self.conversation_id)?;
        row.sequence = self.store.next_sequence(&self.conversation_id)?;
        if row.conversation_id != self.conversation_id {
            return Err(unavailable());
        }
        let encoded = encoded_row(row)?;
        let prefix_bytes = self.archive.metadata()?.len();
        let prefix_sha256 = prefix_digest(&self.archive, prefix_bytes)?;
        self.validate_paths()?;
        let plan = CustodyTranscriptPlan {
            root: Identity::of(&self.root)?,
            directory: Identity::of(&self.directory)?,
            archive: Identity::of(&self.archive)?,
            conversation_id: self.conversation_id.clone(),
            prefix_bytes,
            prefix_sha256,
            archive_sequence: row.sequence,
            canonical_sha256: Sha256::digest(&encoded).into(),
            row_bytes: encoded.len(),
        };
        plan.validate()?;
        Ok(plan)
    }

    /// Rechecks a retained first preparation before publishing it after loss.
    pub(crate) fn check_preparation(&self, plan: &CustodyTranscriptPlan) -> Result<()> {
        self.validate_plan(plan)
    }

    /// Appends or completes only the exact prepared row. A partial own write
    /// can be reconciled from its frozen bytes; foreign suffixes fail closed.
    /// Derived transcript indexes use the ordinary reconciliation path.
    #[cfg(test)]
    pub(crate) fn append(&self, plan: &CustodyTranscriptPlan, row: &TranscriptEntry) -> Result<()> {
        self.validate_plan(plan)?;
        let encoded = encoded_row(row)?;
        if row.sequence != plan.archive_sequence
            || row.conversation_id != plan.conversation_id
            || encoded.len() != plan.row_bytes
            || <[u8; 32]>::from(Sha256::digest(&encoded)) != plan.canonical_sha256
        {
            return Err(unavailable());
        }
        let mut file = &self.archive;
        file.seek(SeekFrom::Start(plan.prefix_bytes))?;
        let mut present = Vec::new();
        Read::by_ref(&mut file)
            .take(plan.row_bytes as u64)
            .read_to_end(&mut present)?;
        if !encoded.starts_with(&present) {
            return Err(unavailable());
        }
        if present.len() < encoded.len() {
            file.write_all(&encoded[present.len()..])?;
        }
        file.sync_all()?;
        self.directory.sync_all()?;
        self.root.sync_all()?;
        self.validate_plan(plan)?;
        self.store
            .append_conversation_rows_locked(&self.conversation_id, &[row], &self.lease)?;
        self.seal(plan).map(|_| ())
    }

    /// Requalifies the actual original complete row and syncs it before the MMP
    /// canonical commit may be journaled. Missing/partial/wrong rows refuse.
    pub(crate) fn seal(&self, plan: &CustodyTranscriptPlan) -> Result<TranscriptEntry> {
        self.validate_plan(plan)?;
        let mut file = &self.archive;
        file.seek(SeekFrom::Start(plan.prefix_bytes))?;
        let mut bytes = Vec::new();
        file.take(plan.row_bytes as u64).read_to_end(&mut bytes)?;
        if bytes.len() != plan.row_bytes
            || bytes.last() != Some(&b'\n')
            || <[u8; 32]>::from(Sha256::digest(&bytes)) != plan.canonical_sha256
        {
            return Err(unavailable());
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| unavailable())?;
        let row = decode_transcript_entry(text.trim_end_matches('\n'))?;
        if row.conversation_id != plan.conversation_id || row.sequence != plan.archive_sequence {
            return Err(unavailable());
        }
        self.archive.sync_all()?;
        self.directory.sync_all()?;
        self.root.sync_all()?;
        self.validate_plan(plan)?;
        Ok(row)
    }

    /// Every attempt stays bound to the first physical destination and prefix.
    fn validate_plan(&self, plan: &CustodyTranscriptPlan) -> Result<()> {
        self.validate_plan_span(plan, MAX_ROW_BYTES)
    }

    /// Applies the same original-backing/prefix checks to a bounded batch plan.
    fn validate_plan_span(&self, plan: &CustodyTranscriptPlan, max_bytes: usize) -> Result<()> {
        plan.validate_span(max_bytes)?;
        self.validate_paths()?;
        if plan.conversation_id != self.conversation_id
            || plan.root != Identity::of(&self.root)?
            || plan.directory != Identity::of(&self.directory)?
            || plan.archive != Identity::of(&self.archive)?
            || prefix_digest(&self.archive, plan.prefix_bytes)? != plan.prefix_sha256
        {
            return Err(unavailable());
        }
        Ok(())
    }
    /// Checks both retained descriptors and currently named objects.
    fn validate_paths(&self) -> Result<()> {
        if Identity::of(&open_directory(&self.store.root)?)? != Identity::of(&self.root)?
            || Identity::of(&open_directory(
                &self.store.root.join(&self.conversation_id),
            )?)? != Identity::of(&self.directory)?
        {
            return Err(unavailable());
        }
        let named = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.path)?;
        if Identity::of(&named)? != Identity::of(&self.archive)?
            || !named.metadata()?.is_file()
            || named.metadata()?.nlink() != 1
        {
            return Err(unavailable());
        }
        Ok(())
    }
}

/// Encodes one bounded exact physical row including its complete terminator.
fn encoded_row(row: &TranscriptEntry) -> Result<Vec<u8>> {
    let mut bytes = encode_transcript_entry(row)?.into_bytes();
    bytes.push(b'\n');
    if bytes.len() > MAX_ROW_BYTES {
        return Err(unavailable());
    }
    Ok(bytes)
}
/// Hashes the complete original prefix with fixed-size scratch memory.
fn prefix_digest(file: &File, bytes: u64) -> Result<[u8; 32]> {
    Ok(prefix_hasher(file, bytes)?.finalize().into())
}

/// Retains the original streaming digest state so receipt preparation can bind
/// preceding canonical rows before those exact bytes reach the archive.
fn prefix_hasher(file: &File, bytes: u64) -> Result<Sha256> {
    if file.metadata()?.len() < bytes {
        return Err(unavailable());
    }
    let mut file = file;
    file.seek(SeekFrom::Start(0))?;
    let mut limited = file.take(bytes);
    let mut hash = Sha256::new();
    let mut scratch = [0u8; 8192];
    let mut copied = 0u64;
    loop {
        let count = limited.read(&mut scratch)?;
        if count == 0 {
            break;
        }
        copied += count as u64;
        hash.update(&scratch[..count]);
    }
    if copied != bytes {
        return Err(unavailable());
    }
    Ok(hash)
}
/// Opens only an actual directory, without following its final symlink.
fn open_directory(path: &std::path::Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(path)?;
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(unavailable());
    }
    Ok(file)
}
/// Refusals intentionally omit paths and private native input.
fn unavailable() -> MezError {
    MezError::invalid_state("original canonical custody transcript unavailable")
}

#[cfg(test)]
mod tests;
