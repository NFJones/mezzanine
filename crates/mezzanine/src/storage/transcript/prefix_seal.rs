//! Qualified physical RAW-prefix verification and synchronization.
//!
//! An expectation contains only a full-field digest, inclusive frontier and
//! captured maximum encoded-row size. It is not proof of I/O. Verification
//! holds the existing conversation append lease, waits for overlapping recovery
//! receipts, streams only the required prefix, then syncs its exact open file
//! and owning directories. It never appends, repairs, deletes or publishes an
//! epoch. A returned seal is historical qualified evidence, not permanent
//! retention or confinement against noncooperating writers after this boundary.

use super::AgentTranscriptStore;
use super::encoding::{decode_transcript_entry, encode_transcript_entry};
use crate::error::{MezError, Result};
use mez_agent::transcript::{TranscriptEntry, validate_conversation_id};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

/// Immutable, bounded metadata for actual checked logical rows.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct TranscriptPrefixExpectation {
    /// Original logical archive target; caller labels cannot change its rows.
    conversation_id: String,
    /// Inclusive minimal committed prefix required by the checked source.
    through: u64,
    /// Length-framed canonical TSV includes every row field and occurrence.
    digest: [u8; 32],
    /// Bounds one physical read from the exact captured source, including CRLF.
    max_line_bytes: u64,
}

/// Only the genuine locked read/sync path constructs this physical fact.
#[derive(Clone)]
pub(crate) struct TranscriptPrefixSeal {
    /// Original logical destination even when legacy RAW storage is selected.
    path: PathBuf,
    /// Exact immutable source expected by this verification.
    source: TranscriptPrefixExpectation,
}

impl std::fmt::Debug for TranscriptPrefixExpectation {
    /// Source data and digests remain private even in assertion diagnostics.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TranscriptPrefixExpectation")
    }
}
impl std::fmt::Debug for TranscriptPrefixSeal {
    /// Only physical receipt ownership may interpret the qualified fact.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TranscriptPrefixSeal")
    }
}

impl TranscriptPrefixExpectation {
    /// Captures contiguous original checked rows without retaining a second
    /// history. Empty input is an intentional zero frontier, never a fake row.
    pub(crate) fn capture<'a>(
        conversation_id: &str,
        rows: impl IntoIterator<Item = &'a TranscriptEntry>,
    ) -> Result<Self> {
        validate_conversation_id(conversation_id)?;
        let mut through = 0u64;
        let mut max_line_bytes = 2u64;
        let mut digest = Sha256::new();
        digest.update(b"mez-physical-transcript-prefix-v1\0");
        for row in rows {
            row.validate()?;
            if row.conversation_id != conversation_id
                || through.checked_add(1) != Some(row.sequence)
            {
                return Err(MezError::invalid_state(
                    "physical source expectation is not its original contiguous prefix",
                ));
            }
            let encoded = encode_transcript_entry(row)?;
            let length = u64::try_from(encoded.len())
                .map_err(|_| MezError::invalid_state("physical source row length exhausted"))?;
            max_line_bytes =
                max_line_bytes.max(length.checked_add(2).ok_or_else(|| {
                    MezError::invalid_state("physical source row length exhausted")
                })?);
            digest.update(length.to_le_bytes());
            digest.update(encoded.as_bytes());
            through = row.sequence;
        }
        Ok(Self {
            conversation_id: conversation_id.into(),
            through,
            digest: digest.finalize().into(),
            max_line_bytes,
        })
    }
}

impl TranscriptPrefixSeal {
    /// Compares immutable binding only; the containing receipt must separately
    /// prove that its own genuine worker completed and its handle was joined.
    pub(crate) fn matches(
        &self,
        path: &std::path::Path,
        source: &TranscriptPrefixExpectation,
    ) -> bool {
        self.path == path && self.source == *source
    }
}

/// Full open-file observation detects replacement, truncation and mutation
/// around read/sync under the cooperating append lock, including inode ABA.
fn unchanged(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    left.is_file()
        && right.is_file()
        && left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}

impl AgentTranscriptStore {
    /// Verifies and syncs exactly the original required RAW prefix off-actor.
    /// Missing/short data or overlapping recovery ownership returns None;
    /// corruption, changed data, failed sync or source rebinding returns an
    /// error. No append, receipt recovery/retirement, epoch or action is replayed.
    pub(crate) fn seal_transcript_prefix(
        &self,
        source: &TranscriptPrefixExpectation,
    ) -> Result<Option<TranscriptPrefixSeal>> {
        let lease = self.acquire_conversation_lock(&source.conversation_id)?;
        if self.active_delete_pending(&source.conversation_id)? {
            return Err(MezError::invalid_state(
                "physical archive lifecycle is unsettled",
            ));
        }
        if self.prefix_has_pending_append_receipts(
            &source.conversation_id,
            source.through,
            &lease,
        )? {
            return Ok(None);
        }
        let path = self.existing_transcript_path_for(&source.conversation_id)?;
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let before = file.metadata()?;
        if !before.is_file() {
            return Err(MezError::invalid_state(
                "physical archive source is not a regular file",
            ));
        }
        let mut reader = BufReader::new(&file);
        let mut digest = Sha256::new();
        digest.update(b"mez-physical-transcript-prefix-v1\0");
        let mut sequence = 0u64;
        let mut line = String::new();
        while sequence < source.through {
            line.clear();
            if reader
                .by_ref()
                .take(source.max_line_bytes)
                .read_line(&mut line)?
                == 0
            {
                return Ok(None);
            }
            if !line.ends_with('\n') {
                return Err(MezError::invalid_state(
                    "physical archive row is incomplete or exceeds its captured bound",
                ));
            }
            let row = decode_transcript_entry(line.trim_end_matches(['\r', '\n']))?;
            if row.conversation_id != source.conversation_id
                || sequence.checked_add(1) != Some(row.sequence)
            {
                return Err(MezError::invalid_state(
                    "physical archive prefix has missing, reordered or foreign rows",
                ));
            }
            let encoded = encode_transcript_entry(&row)?;
            digest.update((encoded.len() as u64).to_le_bytes());
            digest.update(encoded.as_bytes());
            sequence = row.sequence;
        }
        let actual: [u8; 32] = digest.finalize().into();
        if actual != source.digest {
            return Err(MezError::conflict(
                "physical archive prefix differs from checked source",
            ));
        }
        if !unchanged(&before, &file.metadata()?) || !unchanged(&before, &std::fs::metadata(&path)?)
        {
            return Err(MezError::conflict(
                "physical archive source changed during verification",
            ));
        }
        file.sync_all()?;
        let parent = path.parent().ok_or_else(|| {
            MezError::invalid_state("physical archive source has no owning directory")
        })?;
        File::open(parent)?.sync_all()?;
        if parent != self.root.as_path() {
            File::open(&self.root)?.sync_all()?;
        }
        if !unchanged(&before, &file.metadata()?) || !unchanged(&before, &std::fs::metadata(&path)?)
        {
            return Err(MezError::conflict(
                "physical archive source changed during synchronization",
            ));
        }
        let logical = self.transcript_path(&source.conversation_id)?;
        Ok(Some(TranscriptPrefixSeal {
            path: logical,
            source: source.clone(),
        }))
    }
}
