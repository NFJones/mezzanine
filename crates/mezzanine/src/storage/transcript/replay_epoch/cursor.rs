//! Immutable indexed archive cuts for one authoritative replay publication.
//!
//! Preparation validates new rows against original source and hashes the frozen
//! prefix before exposing a proposal. Publication rechecks that exact prefix and
//! syncs it under the held lease. Ordinary reads verify a bounded boundary anchor
//! and required index references, then decode only the later tail. They do not
//! claim to audit arbitrary damage in older covered archive bytes; current model
//! payloads remain independently required. No cut acknowledges a pending batch.

use super::{
    AgentTranscriptStore, Payload, ReplayEpochContext, ReplayEpochSnapshot, escrow, invalid,
    payload,
};
use crate::Result;
use crate::storage::transcript::{
    TranscriptRowOrigins, encoding::decode_archive_transcript_entry, row_origins::IndexedRowImage,
    store::ConversationTranscriptView,
};
use mez_agent::{
    ContextBlockOccurrenceId, TranscriptEntry, TranscriptRole, transcript::TranscriptContextRecord,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    os::unix::fs::OpenOptionsExt,
};

const ANCHOR_BYTES: u64 = 4096;

/// Physical and logical domains are bound by actual contiguous indexed rows,
/// never pane counts. The boundary anchor is portable across exact saved copies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::storage::transcript) struct ArchiveCut {
    through_sequence: u64,
    byte_offset: u64,
    prefix_sha256: String,
    anchor_sha256: String,
    last_row: Option<IndexedRowImage>,
}

impl ArchiveCut {
    /// Physical frontier captured with this epoch, independent of raw members.
    pub(super) fn through_sequence(&self) -> u64 {
        self.through_sequence
    }
    fn empty() -> Self {
        Self {
            through_sequence: 0,
            byte_offset: 0,
            prefix_sha256: payload::digest(&[]),
            anchor_sha256: payload::digest(&[]),
            last_row: None,
        }
    }

    pub(super) fn validate(&self) -> Result<()> {
        if let Some(row) = &self.last_row {
            Payload {
                sha256: row.digest.clone(),
                bytes: 0,
            }
            .validate_format()?;
        }
        for digest in [&self.prefix_sha256, &self.anchor_sha256] {
            Payload {
                sha256: digest.clone(),
                bytes: 0,
            }
            .validate_format()?;
        }
        if self.through_sequence == 0 {
            if self != &Self::empty() {
                return Err(invalid("invalid empty archive cut"));
            }
        } else if self.byte_offset == 0
            || self
                .last_row
                .as_ref()
                .is_none_or(|row| row.sequence != self.through_sequence)
        {
            return Err(invalid("archive cut has no exact final row"));
        }
        Ok(())
    }

    fn verify(
        &self,
        store: &AgentTranscriptStore,
        cid: &str,
        full: bool,
        allow_absent: bool,
    ) -> Result<()> {
        self.validate()?;
        let Some(mut file) = open_archive(store, cid, allow_absent && self.through_sequence == 0)?
        else {
            return Ok(());
        };
        if file.metadata()?.len() < self.byte_offset {
            return Err(invalid("archive is shorter than its committed cut"));
        }
        if anchor(&mut file, self.byte_offset)? != self.anchor_sha256 {
            return Err(invalid("archive cut boundary changed"));
        }
        if let Some(expected) = &self.last_row
            && store
                .indexed_row_reference_locked(cid, expected.sequence)?
                .as_ref()
                != Some(expected)
        {
            return Err(invalid("required archive cut image changed or disappeared"));
        }
        if full && prefix_digest(&mut file, &Self::empty(), self.byte_offset)? != self.prefix_sha256
        {
            return Err(invalid("frozen archive prefix changed before publication"));
        }
        Ok(())
    }

    /// Model reads need only current payloads and still-unsettled backing. An
    /// archived original additionally requires its real indexed placement inside
    /// this cut before its old immutable payload can cease being active backing.
    pub(super) fn verify_archived(
        &self,
        store: &AgentTranscriptStore,
        cid: &str,
        items: &[escrow::EscrowOccurrence],
    ) -> Result<()> {
        self.verify(store, cid, false, false)?;
        for item in items.iter().filter(|item| item.is_archived()) {
            let reference = store
                .indexed_occurrence_reference_locked(
                    cid,
                    item.occurrence_id(),
                    self.through_sequence,
                )?
                .ok_or_else(|| invalid("archived original has no required index reference"))?;
            if reference.sequence > self.through_sequence {
                return Err(invalid("archived original is beyond the committed cut"));
            }
        }
        Ok(())
    }

    /// Durable and actor-captured pending rows share this original-placement
    /// guard. Index loss cannot make an already archived occurrence new again.
    fn reject_reentry(
        &self,
        store: &AgentTranscriptStore,
        cid: &str,
        row: &TranscriptEntry,
    ) -> Result<()> {
        if self.through_sequence > 0
            && row.role == TranscriptRole::System
            && let Some(authored) = TranscriptContextRecord::from_transcript_content(&row.content)?
            && store
                .indexed_occurrence_reference_locked(
                    cid,
                    authored.record().occurrence_id(),
                    self.through_sequence,
                )?
                .is_some_and(|old| old.sequence <= self.through_sequence)
        {
            return Err(invalid(
                "an archived occurrence reappeared after its committed cut",
            ));
        }
        Ok(())
    }
}

/// Frozen preparation input retained beside the actual proposal through retry.
#[derive(Clone)]
pub(super) struct ArchiveCapture {
    pub(super) previous: Option<ArchiveCut>,
    pub(super) cut: ArchiveCut,
    pub(super) rows: Vec<TranscriptEntry>,
    pub(super) archived: BTreeSet<ContextBlockOccurrenceId>,
}

impl ArchiveCapture {
    pub(super) fn prepare(
        store: &AgentTranscriptStore,
        cid: &str,
        previous: Option<&ArchiveCut>,
        items: &[escrow::EscrowOccurrence],
        source: &ReplayEpochContext,
        allow_absent: bool,
    ) -> Result<Self> {
        store.ensure_row_digest_lookup_index_locked(cid)?;
        let base = previous.cloned().unwrap_or_else(ArchiveCut::empty);
        let window = read_window(store, cid, &base, allow_absent)?;
        let current = source
            .context()
            .chronology()
            .iter()
            .map(|event| {
                (
                    payload::digest(event.block().content.as_bytes()),
                    event.block().content.as_str(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let qualified = escrow::qualify_rows(
            items,
            cid,
            &window.rows,
            escrow::RowAdmission::KnownCopies,
            |reference| {
                if let Some(content) = current.get(&reference.sha256) {
                    reference.validate(content.as_bytes())?;
                    Ok((*content).to_owned())
                } else {
                    payload::read(&store.root.join(cid), reference)
                }
            },
        )?;
        let indexed = store.available_indexed_row_origins_locked(cid, &window.rows)?;
        let prefix = window
            .rows
            .iter()
            .take_while(|row| indexed.contains_key(&row.sequence))
            .count();
        let mut cut = base.clone();
        if prefix > 0 {
            let row = &window.rows[prefix - 1];
            cut.through_sequence = row.sequence;
            cut.byte_offset = window.ends[prefix - 1];
            cut.last_row = Some(IndexedRowImage {
                sequence: row.sequence,
                origin: indexed[&row.sequence],
                digest: store.transcript_entry_revision(row)?,
            });
        }
        if let Some(mut file) = open_archive(store, cid, allow_absent)? {
            // One streaming pass also checks the old prefix before extending it;
            // newly observed bytes cannot certify an altered previous cut.
            cut.prefix_sha256 = prefix_digest(&mut file, &base, cut.byte_offset)?;
            cut.anchor_sha256 = anchor(&mut file, cut.byte_offset)?;
        }
        let archived = window
            .rows
            .iter()
            .take(prefix)
            .filter(|row| qualified.copies.contains(&row.sequence))
            .map(|row| indexed[&row.sequence])
            .collect();
        Ok(Self {
            previous: previous.cloned(),
            cut,
            rows: window.rows,
            archived,
        })
    }

    /// Revalidates the frozen cut/index/source before any publication writes.
    /// Later rows are checked separately; they do not mutate this proposal.
    pub(super) fn verify(
        &self,
        store: &AgentTranscriptStore,
        cid: &str,
        items: &[escrow::EscrowOccurrence],
        resolve: impl FnMut(&Payload) -> Result<String>,
        allow_absent: bool,
    ) -> Result<()> {
        self.cut.verify(store, cid, true, allow_absent)?;
        if self.previous.as_ref().is_some_and(|old| {
            old.byte_offset > self.cut.byte_offset
                || old.through_sequence > self.cut.through_sequence
        }) {
            return Err(invalid("archive cut rewound its original source"));
        }
        let qualified = escrow::qualify_rows(
            items,
            cid,
            &self.rows,
            escrow::RowAdmission::KnownCopies,
            resolve,
        )?;
        let prefix = self
            .rows
            .iter()
            .take_while(|row| row.sequence <= self.cut.through_sequence)
            .cloned()
            .collect::<Vec<_>>();
        let indexed = store
            .indexed_row_origins_locked(cid, &prefix)?
            .ok_or_else(|| invalid("required frozen prefix images disappeared"))?;
        let archived = prefix
            .iter()
            .filter(|row| qualified.copies.contains(&row.sequence))
            .map(|row| indexed[&row.sequence])
            .collect::<BTreeSet<_>>();
        if archived != self.archived {
            return Err(invalid("archive cut changed original source settlement"));
        }
        Ok(())
    }
}

struct Window {
    rows: Vec<TranscriptEntry>,
    ends: Vec<u64>,
}

fn read_window(
    store: &AgentTranscriptStore,
    cid: &str,
    cut: &ArchiveCut,
    allow_absent: bool,
) -> Result<Window> {
    cut.verify(store, cid, false, allow_absent)?;
    let Some(mut file) = open_archive(store, cid, allow_absent && cut.through_sequence == 0)?
    else {
        return Ok(Window {
            rows: Vec::new(),
            ends: Vec::new(),
        });
    };
    file.seek(SeekFrom::Start(cut.byte_offset))?;
    let mut reader = BufReader::new(file);
    let mut rows = Vec::new();
    let mut ends = Vec::new();
    let mut offset = cut.byte_offset;
    let mut sequence = cut.through_sequence;
    let mut line = String::new();
    loop {
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        if !line.ends_with('\n') {
            return Err(invalid("archive tail has an unterminated row"));
        }
        let row = decode_archive_transcript_entry(line.trim_end_matches(['\r', '\n']))?;
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| invalid("archive tail sequence exhausted"))?;
        if row.conversation_id != cid || row.sequence != sequence {
            return Err(invalid("archive tail is missing, reordered or foreign"));
        }
        cut.reject_reentry(store, cid, &row)?;
        offset = offset
            .checked_add(u64::try_from(bytes).map_err(|_| invalid("archive row length exhausted"))?)
            .ok_or_else(|| invalid("archive offset exhausted"))?;
        rows.push(row);
        ends.push(offset);
        line.clear();
    }
    Ok(Window { rows, ends })
}

impl AgentTranscriptStore {
    /// Proves only that this original ordinary batch needs no accounting repair
    /// for typed replay. The caller holds its conversation lease. This is not a
    /// physical-source settlement proof: the original receipt must remain with
    /// its ordinary owner until that owner checks actual bytes and acknowledges.
    pub(in crate::storage::transcript) fn replay_cut_covers_complete_accounting_locked(
        &self,
        batch: &crate::storage::transcript::TranscriptAppendBatch,
    ) -> Result<bool> {
        let Some(accounting) = batch.row_accounting() else {
            return Ok(false);
        };
        if batch.has_initial_authority() {
            return Ok(false);
        }
        accounting.validate(batch.entries())?;
        let Some(snapshot) = self.replay_epoch_snapshot_locked(batch.conversation_id())? else {
            return Ok(false);
        };
        let Some(cut) = &snapshot.archive_cut else {
            return Ok(false);
        };
        if batch
            .entries()
            .last()
            .is_none_or(|row| row.sequence > cut.through_sequence)
        {
            return Ok(false);
        }
        let originals = self
            .indexed_row_origins_locked(batch.conversation_id(), batch.entries())?
            .ok_or_else(|| invalid("required archived receipt images disappeared"))?;
        if &originals != accounting.origins() {
            return Err(invalid(
                "archived receipt has a different original append owner",
            ));
        }
        self.verify_ordinary_accounting_committed_locked(batch)?;
        Ok(true)
    }

    /// Reads only the post-cut tail and verifies omitted old pending rows against
    /// full-field index images. The runtime must also check original batch IDs.
    pub(in crate::storage::transcript) fn replay_cursor_view_locked(
        &self,
        cid: &str,
        snapshot: &ReplayEpochSnapshot,
        pending: &[TranscriptEntry],
    ) -> Result<Option<(ConversationTranscriptView, TranscriptRowOrigins)>> {
        let Some(cut) = &snapshot.archive_cut else {
            return Ok(None);
        };
        if self.active_delete_pending(cid)? {
            return Err(invalid(
                "conversation publication or deletion is incomplete",
            ));
        }
        let window = read_window(self, cid, cut, false)?;
        let mut old = BTreeMap::<u64, &TranscriptEntry>::new();
        let mut logical = window
            .rows
            .iter()
            .map(|row| (row.sequence, row.clone()))
            .collect::<BTreeMap<_, _>>();
        for row in pending {
            row.validate()?;
            if row.conversation_id != cid {
                return Err(invalid("pending cursor source is foreign"));
            }
            if row.sequence <= cut.through_sequence {
                if old
                    .insert(row.sequence, row)
                    .is_some_and(|previous| previous != row)
                {
                    return Err(invalid("pending cursor prefix rows conflict"));
                }
            } else if let Some(previous) = logical.get(&row.sequence) {
                if previous != row {
                    return Err(invalid("pending cursor tail conflicts with durable source"));
                }
            } else {
                logical.insert(row.sequence, row.clone());
            }
        }
        let old = old.into_values().cloned().collect::<Vec<_>>();
        let settled = self
            .indexed_row_origins_locked(cid, &old)?
            .ok_or_else(|| invalid("required old pending row images disappeared"))?;
        let logical = logical.into_values().collect::<Vec<_>>();
        let mut expected = cut.through_sequence;
        for row in &logical {
            cut.reject_reentry(self, cid, row)?;
            expected = expected
                .checked_add(1)
                .ok_or_else(|| invalid("logical cursor sequence exhausted"))?;
            if row.sequence != expected {
                return Err(invalid("logical cursor source has a gap"));
            }
        }
        Ok(Some((
            ConversationTranscriptView {
                committed: window.rows,
                logical,
            },
            settled,
        )))
    }

    pub(super) fn replay_archive_tail_locked(
        &self,
        cid: &str,
        cut: &ArchiveCut,
        allow_absent: bool,
    ) -> Result<Vec<TranscriptEntry>> {
        Ok(read_window(self, cid, cut, allow_absent)?.rows)
    }
}

fn open_archive(
    store: &AgentTranscriptStore,
    cid: &str,
    allow_absent: bool,
) -> Result<Option<File>> {
    let path = store.existing_transcript_path_for(cid)?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            let file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)?;
            if !file.metadata()?.is_file() {
                return Err(invalid("archive cut source changed to a nonregular file"));
            }
            Ok(Some(file))
        }
        Ok(_) => Err(invalid("archive cut source is not a regular file")),
        Err(error) if allow_absent && error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn anchor(file: &mut File, offset: u64) -> Result<String> {
    let start = offset.saturating_sub(ANCHOR_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = vec![
        0;
        usize::try_from(offset - start)
            .map_err(|_| invalid("archive anchor is too large"))?
    ];
    file.read_exact(&mut bytes)?;
    if offset > 0 && bytes.last() != Some(&b'\n') {
        return Err(invalid("archive cut is not a row boundary"));
    }
    Ok(payload::digest(&bytes))
}

fn prefix_digest(file: &mut File, previous: &ArchiveCut, through: u64) -> Result<String> {
    if through < previous.byte_offset {
        return Err(invalid("archive prefix offset rewound"));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut position = 0u64;
    let mut buffer = [0; 64 * 1024];
    for boundary in [previous.byte_offset, through] {
        while position < boundary {
            let limit = usize::try_from((boundary - position).min(buffer.len() as u64))
                .map_err(|_| invalid("archive prefix chunk overflow"))?;
            file.read_exact(&mut buffer[..limit])?;
            hash.update(&buffer[..limit]);
            position += limit as u64;
        }
        if boundary == previous.byte_offset && finish_digest(hash.clone()) != previous.prefix_sha256
        {
            return Err(invalid("previous archive prefix changed"));
        }
    }
    Ok(finish_digest(hash))
}

fn finish_digest(hash: Sha256) -> String {
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
