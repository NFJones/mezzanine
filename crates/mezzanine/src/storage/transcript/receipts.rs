//! Durable append receipts for a transcript worker's uncertain commit window.
//!
//! Receipts retain canonical encoded rows independently of the archive. All
//! admission and retirement changes share the conversation's append lock; a
//! receipt is acknowledged only after its file and parent directory are synced.
//! Canonical filenames route scans to exact locked conversations before any
//! file is opened; routing never substitutes for full content validation.
//! Unknown names fail closed, while another conversation's recognized receipt
//! is checked by its own owner or global recovery, not unrelated admission.
//! Global recovery discovers identities, then re-enumerates under each exact
//! lock so a receipt retired before ownership acquisition is benign. Its
//! accumulated snapshot is preflighted in full before any archive is changed;
//! new conversations discovered by a later scan remain for the next recovery.

use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use mez_agent::transcript::{TranscriptEntry, validate_conversation_id};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{MezError, MezErrorKind, Result};

use super::encoding::{decode_transcript_entry, encode_transcript_entry};
use super::fs::{set_private_dir_permissions, set_private_file_permissions};
use super::types::AgentTranscriptStore;

const RECEIPT_VERSION: u64 = 1;

/// One-shot test handshake after receipt path enumeration, before opening files.
#[cfg(test)]
#[derive(Debug)]
pub(super) struct ReceiptEnumerationBarrier {
    /// Reports that the path snapshot has been captured.
    observed: std::sync::mpsc::Sender<()>,
    /// Releases the scan after a competing conversation finishes settlement.
    resume: std::sync::mpsc::Receiver<()>,
}

#[derive(Serialize, Deserialize)]
struct AppendReceipt {
    version: u64,
    conversation_id: String,
    first_sequence: u64,
    generation: u64,
    digest: String,
    rows: Vec<String>,
}

impl AppendReceipt {
    fn new(entries: &[TranscriptEntry], generation: u64) -> Result<Self> {
        let first = entries
            .first()
            .ok_or_else(|| MezError::invalid_args("empty transcript receipt"))?;
        if generation == 0
            || entries
                .iter()
                .any(|entry| entry.conversation_id != first.conversation_id)
            || entries
                .windows(2)
                .any(|pair| pair[0].sequence.checked_add(1) != Some(pair[1].sequence))
        {
            return Err(MezError::invalid_args(
                "transcript receipt requires one contiguous conversation and nonzero generation",
            ));
        }
        let rows = entries
            .iter()
            .map(encode_transcript_entry)
            .collect::<Result<Vec<_>>>()?;
        let digest = digest_rows(&rows);
        Ok(Self {
            version: RECEIPT_VERSION,
            conversation_id: first.conversation_id.clone(),
            first_sequence: first.sequence,
            generation,
            digest,
            rows,
        })
    }

    fn entries(&self) -> Result<Vec<TranscriptEntry>> {
        if self.version != RECEIPT_VERSION
            || self.rows.is_empty()
            || self.generation == 0
            || self.digest != digest_rows(&self.rows)
        {
            return Err(MezError::invalid_state(
                "transcript receipt identity is invalid",
            ));
        }
        let entries = self
            .rows
            .iter()
            .map(|row| decode_transcript_entry(row))
            .collect::<Result<Vec<_>>>()?;
        let check = Self::new(&entries, self.generation)?;
        if check.conversation_id != self.conversation_id
            || check.first_sequence != self.first_sequence
        {
            return Err(MezError::invalid_state(
                "transcript receipt rows do not match identity",
            ));
        }
        Ok(entries)
    }

    fn filename(&self) -> String {
        format!(
            "{}-{:020}-{:020}.json",
            self.conversation_id, self.first_sequence, self.generation
        )
    }
}

fn digest_rows(rows: &[String]) -> String {
    let mut digest = Sha256::new();
    for row in rows {
        digest.update((row.len() as u64).to_be_bytes());
        digest.update(row.as_bytes());
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Parses canonical filename routing identity without opening or trusting rows.
/// Staging files are not accepted receipts; unknown names fail closed even in a
/// scoped scan because they cannot safely be assigned to another lock owner.
fn receipt_conversation_id(path: &Path) -> Result<Option<&str>> {
    let invalid = || MezError::invalid_state("transcript receipt filename is invalid");
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(invalid)?;
    let staging = name.ends_with(".json.tmp");
    let name = if staging {
        &name[..name.len() - 4]
    } else {
        name
    };
    let stem = name.strip_suffix(".json").ok_or_else(invalid)?;
    let (prefix, generation) = stem.rsplit_once('-').ok_or_else(invalid)?;
    let (conversation_id, sequence) = prefix.rsplit_once('-').ok_or_else(invalid)?;
    validate_conversation_id(conversation_id)?;
    for number in [sequence, generation] {
        if number.len() != 20
            || !number.bytes().all(|byte| byte.is_ascii_digit())
            || number.parse::<u64>().ok().is_none_or(|value| value == 0)
        {
            return Err(invalid());
        }
    }
    Ok((!staging).then_some(conversation_id))
}

impl AgentTranscriptStore {
    fn append_receipt_directory(&self) -> PathBuf {
        self.root.join(".append-receipts")
    }

    /// Enumerates paths only, without claiming any conversation's receipt content.
    fn append_receipt_paths(&self) -> Result<Vec<PathBuf>> {
        let directory = self.append_receipt_directory();
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                return Err(MezError::invalid_state(
                    "transcript receipt directory is not a real directory",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        }
        let mut paths = fs::read_dir(&directory)?
            .map(|result| result.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        #[cfg(test)]
        self.wait_at_receipt_enumeration_barrier()?;
        Ok(paths)
    }

    /// Reads only identities whose exact append locks the caller already owns.
    /// Cooperative admission/settlement cannot remove a selected path while its
    /// lock is held, so every selected stat/read error remains a hard failure.
    fn read_locked_append_receipts(
        &self,
        conversation_ids: &[&str],
    ) -> Result<Vec<(PathBuf, AppendReceipt, Vec<TranscriptEntry>)>> {
        let paths = self.append_receipt_paths()?;
        paths
            .into_iter()
            .filter_map(|path| match receipt_conversation_id(&path) {
                Ok(Some(id)) if conversation_ids.contains(&id) => Some(Ok(path)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .map(|path| {
                let path = path?;
                if !fs::symlink_metadata(&path)?.file_type().is_file() {
                    return Err(MezError::invalid_state(
                        "transcript receipt is not a regular file",
                    ));
                }
                let receipt: AppendReceipt =
                    serde_json::from_slice(&fs::read(&path)?).map_err(|error| {
                        MezError::invalid_state(format!(
                            "transcript receipt decode failed: {error}"
                        ))
                    })?;
                validate_conversation_id(&receipt.conversation_id)?;
                if path
                    .file_name()
                    .is_none_or(|name| name != receipt.filename().as_str())
                {
                    return Err(MezError::invalid_state(
                        "transcript receipt filename does not match identity",
                    ));
                }
                let entries = receipt.entries()?;
                Ok((path, receipt, entries))
            })
            .collect()
    }

    /// Captures a rolling recovery snapshot under each exact conversation lock.
    /// Initial names provide lock routing only. Fresh enumeration after locking
    /// omits benign prior retirement without ignoring errors on selected paths.
    /// Locks are released here before recovery calls the locking append API.
    fn read_append_receipts(&self) -> Result<Vec<(PathBuf, AppendReceipt, Vec<TranscriptEntry>)>> {
        let mut conversation_ids = std::collections::BTreeSet::new();
        for path in self.append_receipt_paths()? {
            if let Some(id) = receipt_conversation_id(&path)? {
                conversation_ids.insert(id.to_owned());
            }
        }
        let mut receipts = Vec::new();
        for id in conversation_ids {
            let _lock = self.acquire_conversation_lock(&id)?;
            receipts.extend(self.read_locked_append_receipts(&[&id])?);
        }
        Ok(receipts)
    }

    /// Gates one receipt scan on this handle or its clones, without global state.
    #[cfg(test)]
    pub(crate) fn gate_next_receipt_enumeration_for_tests(
        &self,
        observed: std::sync::mpsc::Sender<()>,
        resume: std::sync::mpsc::Receiver<()>,
    ) {
        *self.receipt_enumeration_barrier.lock().unwrap() =
            Some(ReceiptEnumerationBarrier { observed, resume });
    }

    /// Consumes the per-store handshake without retaining its mutex while paused.
    #[cfg(test)]
    fn wait_at_receipt_enumeration_barrier(&self) -> Result<()> {
        let barrier = self
            .receipt_enumeration_barrier
            .lock()
            .map_err(|_| MezError::invalid_state("receipt enumeration test barrier poisoned"))?
            .take();
        if let Some(barrier) = barrier {
            barrier.observed.send(()).map_err(|_| {
                MezError::invalid_state("receipt enumeration test observer disconnected")
            })?;
            barrier
                .resume
                .recv_timeout(std::time::Duration::from_secs(10))
                .map_err(|error| {
                    MezError::invalid_state(format!(
                        "receipt enumeration test barrier failed: {error}"
                    ))
                })?;
        }
        Ok(())
    }

    /// Refuses a saved fork while either locked identity has retained receipt
    /// ownership. This is only an admission fence: the existing persistence
    /// owner must settle accepted rows, and no receipt is replayed or retired.
    pub(super) fn ensure_fork_receipts_settled(&self, source: &str, target: &str) -> Result<()> {
        for (_, receipt, _) in self.read_locked_append_receipts(&[source, target])? {
            if receipt.conversation_id == source || receipt.conversation_id == target {
                return Err(MezError::conflict(
                    "saved fork requires source and target append receipts to settle",
                ));
            }
        }
        Ok(())
    }

    /// Durably accepts one immutable, validated batch before caller acknowledgment.
    /// Conflicting overlapping receipts are rejected under conversation ownership.
    pub(crate) fn accept_append_receipt(
        &self,
        entries: &[TranscriptEntry],
        generation: u64,
    ) -> Result<()> {
        let receipt = AppendReceipt::new(entries, generation)?;
        let _lock = self.acquire_conversation_lock(&receipt.conversation_id)?;
        // Admission must not leave a replayable receipt for a sequence that
        // already belongs to different durable content. Check the complete
        // archive off-actor so an older batch cannot hide behind a long tail.
        let committed_count = match self.inspect(&receipt.conversation_id) {
            Ok(durable) => {
                let count = self.validate_restored_transcript(&receipt.conversation_id)?;
                if entries.iter().any(|entry| {
                    usize::try_from(entry.sequence - 1)
                        .ok()
                        .and_then(|index| durable.get(index))
                        .is_some_and(|row| row != entry)
                }) {
                    return Err(MezError::conflict(
                        "transcript receipt conflicts with durable contents",
                    ));
                }
                count
            }
            Err(error) if error.kind() == MezErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        let pending = self.read_locked_append_receipts(&[&receipt.conversation_id])?;
        let mut next_sequence = committed_count
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("transcript receipt sequence overflow"))?;
        let covered = pending
            .iter()
            .filter(|(_, existing, _)| existing.conversation_id == receipt.conversation_id)
            .flat_map(|(_, _, rows)| rows.iter().map(|row| row.sequence))
            .collect::<std::collections::BTreeSet<_>>();
        if receipt.first_sequence > next_sequence {
            for sequence in covered.range(next_sequence..receipt.first_sequence) {
                if *sequence != next_sequence {
                    break;
                }
                next_sequence = next_sequence.checked_add(1).ok_or_else(|| {
                    MezError::invalid_state("transcript receipt sequence overflow")
                })?;
            }
            if receipt.first_sequence > next_sequence {
                return Err(MezError::invalid_state(
                    "transcript receipt is waiting for an earlier entry",
                ));
            }
        }
        for (_, existing, rows) in pending {
            if existing.conversation_id != receipt.conversation_id {
                continue;
            }
            if existing.first_sequence == receipt.first_sequence
                && existing.generation == generation
            {
                if rows == entries {
                    let directory = self.append_receipt_directory();
                    set_private_dir_permissions(&directory)?;
                    set_private_file_permissions(&directory.join(existing.filename()))?;
                    fs::File::open(&directory)?.sync_all()?;
                    fs::File::open(&self.root)?.sync_all()?;
                    return Ok(());
                }
                return Err(MezError::conflict("transcript receipt identity changed"));
            }
            if rows.iter().any(|row| {
                entries
                    .iter()
                    .any(|entry| row.sequence == entry.sequence && row != entry)
            }) {
                return Err(MezError::conflict(
                    "transcript receipt overlaps conflicting content",
                ));
            }
        }
        let directory = self.append_receipt_directory();
        fs::create_dir_all(&directory)?;
        set_private_dir_permissions(&directory)?;
        let path = directory.join(receipt.filename());
        let temporary = path.with_extension("json.tmp");
        match fs::symlink_metadata(&temporary) {
            Ok(metadata) if metadata.file_type().is_file() => fs::remove_file(&temporary)?,
            Ok(_) => {
                return Err(MezError::invalid_state(
                    "transcript receipt staging path is not a regular file",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let bytes = serde_json::to_vec(&receipt).map_err(|error| {
            MezError::invalid_state(format!("transcript receipt encode failed: {error}"))
        })?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary)?;
        set_private_file_permissions(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, &path)?;
        fs::File::open(&directory)?.sync_all()?;
        fs::File::open(&self.root)?.sync_all()?;
        Ok(())
    }

    /// Returns exact pending rows in chronological order for recovery tests.
    #[cfg(test)]
    pub(crate) fn pending_append_receipts(&self) -> Result<Vec<Vec<TranscriptEntry>>> {
        let mut receipts = self.read_append_receipts()?;
        receipts.sort_by(|left, right| {
            (
                &left.1.conversation_id,
                left.1.first_sequence,
                left.1.generation,
            )
                .cmp(&(
                    &right.1.conversation_id,
                    right.1.first_sequence,
                    right.1.generation,
                ))
        });
        Ok(receipts.into_iter().map(|(_, _, rows)| rows).collect())
    }

    /// Reconciles accepted batches with the durable archive off the actor.
    /// Failure leaves the receipt available for a later checked recovery.
    pub(crate) fn recover_append_receipts(&self) -> Result<()> {
        let receipts = self.read_append_receipts()?;
        // Reject mutually inconsistent retained claims before any append can
        // make one side of the conflict durable. A damaged journal remains
        // available for diagnosis rather than being partially replayed.
        let mut claimed = std::collections::BTreeMap::new();
        for (_, receipt, entries) in &receipts {
            for entry in entries {
                let key = (receipt.conversation_id.as_str(), entry.sequence);
                if claimed.insert(key, entry).is_some_and(|old| old != entry) {
                    return Err(MezError::conflict(
                        "transcript recovery receipts conflict at the same sequence",
                    ));
                }
            }
        }
        // Detect conflicting committed history across every conversation
        // before replay mutates any archive. The append path repeats these
        // checks under its conversation lock when it actually writes.
        for conversation_id in receipts
            .iter()
            .map(|(_, receipt, _)| receipt.conversation_id.as_str())
            .collect::<std::collections::BTreeSet<_>>()
        {
            let committed_count = match self.inspect(conversation_id) {
                Ok(durable) => {
                    let count = self.validate_restored_transcript(conversation_id)?;
                    for row in durable {
                        if claimed
                            .get(&(conversation_id, row.sequence))
                            .is_some_and(|expected| **expected != row)
                        {
                            return Err(MezError::conflict(
                                "transcript recovery receipt conflicts with durable contents",
                            ));
                        }
                    }
                    count
                }
                Err(error) if error.kind() == MezErrorKind::NotFound => 0,
                Err(error) => return Err(error),
            };
            let mut next = committed_count
                .checked_add(1)
                .ok_or_else(|| MezError::invalid_state("transcript recovery sequence overflow"))?;
            for (&(_, sequence), _) in
                claimed.range((conversation_id, next)..=(conversation_id, u64::MAX))
            {
                if sequence != next {
                    return Err(MezError::invalid_state(
                        "transcript recovery receipts are missing an earlier entry",
                    ));
                }
                next = next.checked_add(1).ok_or_else(|| {
                    MezError::invalid_state("transcript recovery sequence overflow")
                })?;
            }
        }
        let mut receipts = receipts
            .into_iter()
            .map(|(_, receipt, entries)| (receipt, entries))
            .collect::<Vec<_>>();
        receipts.sort_by(|left, right| {
            (
                &left.0.conversation_id,
                left.0.first_sequence,
                left.0.generation,
            )
                .cmp(&(
                    &right.0.conversation_id,
                    right.0.first_sequence,
                    right.0.generation,
                ))
        });
        for (receipt, entries) in receipts {
            self.append_many(&entries)?;
            self.settle_append_receipt(&entries, receipt.generation)?;
        }
        Ok(())
    }

    /// Retires only a matching durable receipt after exact archive reconciliation.
    pub(crate) fn settle_append_receipt(
        &self,
        entries: &[TranscriptEntry],
        generation: u64,
    ) -> Result<()> {
        let receipt = AppendReceipt::new(entries, generation)?;
        let _lock = self.acquire_conversation_lock(&receipt.conversation_id)?;
        let directory = self.append_receipt_directory();
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                return Err(MezError::invalid_state(
                    "transcript receipt directory is not a real directory",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return self.verify_missing_append_receipt(entries);
            }
            Err(error) => return Err(error.into()),
        }
        let path = directory.join(receipt.filename());
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => {
                return Err(MezError::invalid_state(
                    "transcript receipt is not a regular file",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return self.verify_missing_append_receipt(entries);
            }
            Err(error) => return Err(error.into()),
        }
        let stored = fs::read(&path)?;
        let actual: AppendReceipt = serde_json::from_slice(&stored).map_err(|error| {
            MezError::invalid_state(format!("transcript receipt decode failed: {error}"))
        })?;
        if actual.version != receipt.version
            || actual.conversation_id != receipt.conversation_id
            || actual.first_sequence != receipt.first_sequence
            || actual.generation != receipt.generation
            || actual.digest != receipt.digest
            || actual.entries()? != entries
        {
            return Err(MezError::conflict(
                "transcript receipt changed before settlement",
            ));
        }
        // The receipt is the only recovery source until every row is proven
        // durable. A partial or damaged archive must leave it replayable.
        self.validate_restored_transcript(&receipt.conversation_id)?;
        let archive = self.inspect(&receipt.conversation_id)?;
        if entries.iter().any(|entry| {
            usize::try_from(entry.sequence - 1)
                .ok()
                .and_then(|index| archive.get(index))
                .is_none_or(|durable| durable != entry)
        }) {
            return Err(MezError::invalid_state(
                "transcript receipt has uncommitted or conflicting rows",
            ));
        }
        fs::remove_file(&path)?;
        fs::File::open(&directory)?.sync_all()?;
        Ok(())
    }

    /// Treats an absent receipt as an idempotent late settlement only when the
    /// entire exact batch is already committed in a valid archive.
    fn verify_missing_append_receipt(&self, entries: &[TranscriptEntry]) -> Result<()> {
        let conversation_id = &entries[0].conversation_id;
        self.validate_restored_transcript(conversation_id)?;
        let archive = self.inspect(conversation_id)?;
        if entries.iter().any(|entry| {
            usize::try_from(entry.sequence - 1)
                .ok()
                .and_then(|index| archive.get(index))
                != Some(entry)
        }) {
            return Err(MezError::invalid_state(
                "missing transcript receipt has uncommitted or conflicting rows",
            ));
        }
        Ok(())
    }
}
