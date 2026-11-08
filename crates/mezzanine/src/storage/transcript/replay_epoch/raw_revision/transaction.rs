//! Joint raw/index/manifest replacement under the existing mutation lease.
//!
//! Before a synced intent, the old epoch remains authoritative. After it, the
//! next lease finishes that exact replacement. Hashes bind complete archive
//! images; the bounded intent contains references, never a second raw journal.

use super::*;
use crate::storage::transcript::{
    CompareAndSwapTranscriptEntryResult as Cas, encoding::encode_transcript_entry,
    row_origins::ContentRowRevision,
};
use mez_agent::transcript::{TranscriptContextEvent as Event, TranscriptExecutionStateRecord};
use mez_agent::{ContextSourceKind, TranscriptEntry, TranscriptRole};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

const INTENT_NAME: &str = ".raw-content-edit.json";
const STAGE_NAME: &str = ".raw-content-edit.tsv";
const INTENT_LIMIT: u64 = 1024 * 1024;

/// Physical identity is independent of canonical record estimates and IDs.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileImage {
    bytes: u64,
    sha256: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u32,
    conversation: String,
    parent: ReplayEpochIdentity,
    original: Payload,
    replacement: Payload,
    directory_archive: bool,
    old_archive: FileImage,
    new_archive: FileImage,
    row: ContentRowRevision,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SealedIntent {
    intent: Intent,
    sha256: String,
}

impl AgentTranscriptStore {
    /// User-facing typed CAS. The caller already owns the original conversation
    /// lease; every source qualification precedes the first edit write.
    pub(in crate::storage::transcript) fn compare_and_swap_replay_content_locked(
        &self,
        cid: &str,
        sequence: u64,
        pane: &str,
        revision: &str,
        content: String,
        lease: &File,
    ) -> Result<Cas> {
        let mut envelope = self
            .read_replay_envelope_locked(cid)?
            .ok_or_else(|| invalid("content edit lost its original authority"))?;
        let directory = self.root.join(cid);
        envelope.verify_receipt(&directory)?;
        let owner = envelope.restore(AgentContext::empty(), |reference| {
            payload::read(&directory, &reference)
        })?;
        envelope.verify_escrow_payloads(&directory)?;
        let cut =
            envelope.manifest.archive_cut.as_ref().ok_or_else(|| {
                invalid("content edit requires explicit archive/source migration")
            })?;
        cut.verify_complete_source(self, cid)?;
        cut.verify_archived(
            self,
            cid,
            envelope
                .manifest
                .source_escrow
                .as_deref()
                .unwrap_or_default(),
        )?;
        let path = self.existing_transcript_path_for(cid)?;
        let old_archive = file_image(&path)?;
        let mut rows = self.inspect(cid)?;
        if rows
            .iter()
            .enumerate()
            .any(|(index, row)| row.conversation_id != cid || row.sequence != index as u64 + 1)
        {
            return Err(invalid(
                "content edit archive has foreign or unordered rows",
            ));
        }
        let Some(index) = rows.iter().position(|row| row.sequence == sequence) else {
            return Ok(Cas::Deleted);
        };
        let original = rows[index].clone();
        let authored = content::semantic(&original)?;
        let image = self.indexed_row_reference_locked(cid, sequence)?;
        if image.as_ref().is_some_and(|image| {
            self.transcript_entry_revision(&original).ok().as_ref() != Some(&image.digest)
        }) {
            return Err(invalid("content edit original full-row image changed"));
        }
        let known = authored
            .as_ref()
            .and_then(|(_, record)| record.as_ref())
            .map(|record| record.record().occurrence_id())
            .or_else(|| image.as_ref().map(|image| image.origin));
        if let (Some(id), Some(image)) = (known, &image)
            && id != image.origin
        {
            return Err(invalid("content edit original occurrence changed"));
        }
        let selected = known.and_then(|id| {
            owner
                .context()
                .chronology()
                .iter()
                .skip(1)
                .find(|event| event.record().occurrence_id() == id)
        });
        let zero = zero_projection(&rows)?;
        let covered = known.is_some_and(|id| {
            envelope.manifest.covered.contains(&id)
                || (selected.is_none()
                    && envelope
                        .manifest
                        .source_escrow
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .any(|item| item.occurrence_id() == id))
        }) || (sequence <= cut.through_sequence()
            && selected.is_none()
            && !zero.contains(&index));
        if covered {
            return Err(crate::MezError::invalid_state(
                "cannot edit transcript history already represented by a compaction epoch",
            ));
        }
        if original.pane_id != pane {
            return Ok(Cas::WrongPane);
        }
        let current_revision = self.transcript_entry_revision(&original)?;
        if current_revision != revision {
            return Ok(Cas::Stale { current_revision });
        }
        if self.has_unsettled_append_receipts_locked(cid, lease)? {
            return Err(crate::MezError::conflict(
                "transcript content edit requires append receipts to settle",
            ));
        }
        self.ensure_custody_receipts_settled_locked(cid)?;
        let mut updated = original.clone();
        updated.content = content;
        updated.validate()?;
        for row in [&original, &updated] {
            if row.role == TranscriptRole::System
                && TranscriptExecutionStateRecord::from_transcript_content(&row.content)?.is_some()
            {
                return Err(invalid(
                    "cannot edit original execution lifecycle control records",
                ));
            }
        }
        if updated == original {
            return Ok(Cas::Updated(original));
        }
        // Display/audit edits remain zero-model, including when the body becomes
        // empty. Semantic/control envelopes cannot enter through that exception.
        rows[index] = updated.clone();
        let new_zero = zero_projection(&rows)?;
        let audit = selected.is_none()
            && zero.contains(&index)
            && new_zero.contains(&index)
            && authored.is_none()
            && content::semantic(&updated)?.is_none();
        let (block, record) = if audit {
            (None, None)
        } else {
            content::revise(&original, &mut updated, selected.map(|event| event.block()))?
        };
        // Cached accounting and JSON formatting are not source changes. An
        // editor may submit stale estimates that normalize to the original row.
        if updated == original {
            return Ok(Cas::Updated(original));
        }
        rows[index] = updated.clone();
        let origin = *self
            .transcript_row_origins_locked(cid, std::slice::from_ref(&original))?
            .get(&sequence)
            .ok_or_else(|| invalid("content edit has no original placement"))?;
        if known.is_some_and(|known| known != origin) {
            return Err(invalid("content edit original row placement changed"));
        }
        if sequence <= cut.through_sequence() && image.is_none() {
            return Err(invalid("content edit lost a required archived row image"));
        }
        let mut checkpoint = inline_checkpoint(&envelope.manifest.checkpoint, &owner)?;
        let record = if let Some(selected) = selected {
            let block = block
                .ok_or_else(|| invalid("content edit cannot remove a retained model member"))?;
            checkpoint = checkpoint
                .revise_raw_member(origin, block.content)
                .map_err(context_error)?;
            let revised = ReplayEpochContext::from_checkpoint(
                owner.conversation_id().clone(),
                checkpoint.clone(),
                AgentContext::empty(),
            )
            .map_err(context_error)?;
            Some(
                revised
                    .context()
                    .chronology()
                    .iter()
                    .find(|event| {
                        event.record().occurrence_id() == selected.record().occurrence_id()
                    })
                    .ok_or_else(|| invalid("content edit lost its selected occurrence"))?
                    .record(),
            )
        } else {
            record
        };
        if selected.is_none() {
            validate_arrival_execution(self, cid, &envelope, &rows, &updated)?;
        }
        let revised_owner = ReplayEpochContext::from_checkpoint(
            owner.conversation_id().clone(),
            checkpoint.clone(),
            AgentContext::empty(),
        )
        .map_err(context_error)?;
        let mut payloads = BTreeMap::new();
        let base = match &envelope.manifest.raw_revision_origin {
            Some(origin) => origin.base.clone(),
            None => {
                let bytes = String::from_utf8(encode(&envelope)?)
                    .map_err(|_| invalid("content edit receipt is not UTF-8"))?;
                let reference = Payload::new(bytes.as_bytes());
                payloads.insert(reference.sha256.clone(), bytes);
                reference
            }
        };
        let parent = envelope.identity();
        let original_receipt = Payload::new(&encode(&envelope)?);
        let through = cut.through_sequence();
        let patch = ContentRowRevision {
            sequence,
            origin,
            before: image,
            after_digest: self.transcript_entry_revision(&updated)?,
            record,
        };
        self.validate_content_row_revision_locked(cid, &patch)?;
        let last = if sequence == through {
            patch.after()
        } else if through > 0 {
            self.indexed_row_reference_locked(cid, through)?
        } else {
            None
        };
        envelope.manifest.archive_cut = Some(cursor::ArchiveCut::for_revised_rows(
            cid, &rows, through, last,
        )?);
        envelope.manifest.version = 11;
        envelope.manifest.revision = parent
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("content edit revision exhausted"))?;
        envelope.manifest.publication = ContextBlockOccurrenceId::allocate();
        envelope.manifest.expected_parent = Some(parent.clone());
        envelope.manifest.parent_head = Some(envelope.manifest.head);
        envelope.manifest.copy_origin = None;
        envelope.manifest.memory_copy_origin = None;
        envelope.manifest.prefix_origin = None;
        envelope.manifest.raw_revision_origin = Some(RawRevisionOrigin { base });
        envelope.manifest.checkpoint = externalize(checkpoint, &mut payloads)?;
        envelope.manifest.source_escrow = envelope.manifest.source_escrow.map(|items| {
            items
                .into_iter()
                .map(|item| item.with_revised_payload(&revised_owner))
                .collect()
        });
        envelope.sha256 = digest(&encode(&envelope.manifest)?);
        let resolve = |reference: Payload| match payloads.get(&reference.sha256) {
            Some(content) => {
                reference.validate(content.as_bytes())?;
                Ok(content.clone())
            }
            None => payload::read(&directory, &reference),
        };
        envelope.restore(AgentContext::empty(), resolve)?;
        let bytes = encode(&envelope)?;
        if bytes.len() as u64 > MANIFEST_LIMIT {
            return Err(invalid("content edit manifest exceeds size limit"));
        }
        let replacement = Payload::new(&bytes);
        for (hash, content) in payloads {
            payload::publish(&directory, &hash, content.as_bytes(), || {
                self.raw_edit_phase(Phase::BeforePayloadSync)
            })?;
        }
        payload::publish(&directory, &replacement.sha256, &bytes, || {
            self.raw_edit_phase(Phase::BeforeSourceReceiptSync)
        })?;
        let staged = directory.join(STAGE_NAME);
        {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&staged)?;
            for row in &rows {
                file.write_all(encode_transcript_entry(row)?.as_bytes())?;
                file.write_all(b"\n")?;
            }
            file.sync_all()?;
        }
        sync_directory(&directory)?;
        if file_image(&path)? != old_archive
            || self
                .read_replay_envelope_locked(cid)?
                .as_ref()
                .map(Envelope::identity)
                != Some(parent.clone())
        {
            return Err(crate::MezError::conflict(
                "content edit frozen source changed before intent",
            ));
        }
        let intent = Intent {
            version: 1,
            conversation: cid.into(),
            parent,
            original: original_receipt,
            replacement,
            directory_archive: path == self.transcript_path_for(cid)?,
            old_archive,
            new_archive: file_image(&staged)?,
            row: patch,
        };
        let sealed = SealedIntent {
            sha256: digest(&encode(&intent)?),
            intent,
        };
        self.raw_edit_phase(Phase::BeforeRawEditIntent)?;
        write_private(&directory, INTENT_NAME, &encode(&sealed)?)?;
        self.raw_edit_phase(Phase::AfterRawEditIntent)?;
        self.recover_replay_raw_revision_locked(cid)?;
        self.raw_edit_phase(Phase::BeforeRawEditAcknowledgment)?;
        Ok(Cas::Updated(updated))
    }

    /// Called before all ordinary/fork mutation recovery, with that lease still
    /// held. A complete old/new image is mandatory; unknown substitutions retain
    /// the intent and refuse, without repairing IDs or generating another head.
    pub(in crate::storage::transcript) fn recover_replay_raw_revision_locked(
        &self,
        cid: &str,
    ) -> Result<()> {
        let directory = self.root.join(cid);
        let bytes = match bounded_read(&directory.join(INTENT_NAME), INTENT_LIMIT) {
            Ok(bytes) => bytes,
            Err(error) if error.io_kind() == Some(std::io::ErrorKind::NotFound) => return Ok(()),
            Err(error) => return Err(error),
        };
        let sealed: SealedIntent =
            serde_json::from_slice(&bytes).map_err(|_| invalid("invalid content edit intent"))?;
        let intent = sealed.intent;
        if intent.version != 1
            || intent.conversation != cid
            || sealed.sha256 != digest(&encode(&intent)?)
        {
            return Err(invalid("content edit intent integrity failed"));
        }
        intent.replacement.validate_format()?;
        intent.original.validate_format()?;
        for image in [&intent.old_archive, &intent.new_archive] {
            Payload {
                bytes: 0,
                sha256: image.sha256.clone(),
            }
            .validate_format()?;
        }
        intent.row.validate()?;
        if intent.replacement.bytes as u64 > MANIFEST_LIMIT
            || intent.original.bytes as u64 > MANIFEST_LIMIT
        {
            return Err(invalid("content edit replacement exceeds size limit"));
        }
        let original_bytes = payload::read(&directory, &intent.original)?;
        let original: Envelope = serde_json::from_str(&original_bytes)
            .map_err(|_| invalid("invalid original content edit receipt"))?;
        original.validate(cid)?;
        if original.identity() != intent.parent {
            return Err(invalid("content edit original parent changed"));
        }
        let original_owner = original.restore(AgentContext::empty(), |reference| {
            payload::read(&directory, &reference)
        })?;
        let bytes = payload::read(&directory, &intent.replacement)?;
        let replacement: Envelope = serde_json::from_str(&bytes)
            .map_err(|_| invalid("invalid content edit replacement"))?;
        replacement.validate(cid)?;
        let owner = replacement.restore(AgentContext::empty(), |reference| {
            payload::read(&directory, &reference)
        })?;
        replacement.verify_escrow_payloads(&directory)?;
        if replacement.manifest.version != 11
            || replacement.manifest.expected_parent.as_ref() != Some(&intent.parent)
        {
            return Err(invalid("content edit replacement parent changed"));
        }
        let mut expected = inline_checkpoint(&original.manifest.checkpoint, &original_owner)?;
        if let Some(member) = original_owner
            .members()
            .iter()
            .find(|member| member.occurrence_id() == intent.row.origin)
        {
            let event = owner
                .context()
                .chronology()
                .iter()
                .find(|event| event.record().occurrence_id() == member.occurrence_id())
                .ok_or_else(|| invalid("content edit recovery lost retained member"))?;
            expected = expected
                .revise_raw_member(member.occurrence_id(), event.block().content.clone())
                .map_err(context_error)?;
            if intent.row.record != Some(event.record()) {
                return Err(invalid(
                    "content edit recovery accounting differs from retained source",
                ));
            }
        }
        if expected != inline_checkpoint(&replacement.manifest.checkpoint, &owner)? {
            return Err(invalid(
                "content edit recovery changed unrelated checkpoint source",
            ));
        }
        let current = self
            .read_replay_envelope_locked(cid)?
            .ok_or_else(|| invalid("content edit required authority disappeared"))?;
        current.verify_receipt(&directory)?;
        if current.identity() != intent.parent && current != replacement {
            return Err(crate::MezError::conflict("content edit authority changed"));
        }
        let path = if intent.directory_archive {
            self.transcript_path_for(cid)?
        } else {
            self.legacy_transcript_path_for(cid)?
        };
        let actual = file_image(&path)?;
        if actual != intent.old_archive && actual != intent.new_archive {
            return Err(crate::MezError::conflict(
                "content edit archive source changed",
            ));
        }
        if actual == intent.old_archive && actual != intent.new_archive {
            let staged = directory.join(STAGE_NAME);
            if file_image(&staged)? != intent.new_archive {
                return Err(invalid("content edit staged archive changed"));
            }
            fs::rename(staged, &path)?;
            sync_directory(
                path.parent()
                    .ok_or_else(|| invalid("content edit archive has no parent"))?,
            )?;
        }
        self.raw_edit_phase(Phase::AfterRawEditArchive)?;
        self.apply_content_row_revision_locked(cid, &intent.row)?;
        self.raw_edit_phase(Phase::AfterRawEditIndex)?;
        replacement
            .manifest
            .archive_cut
            .as_ref()
            .ok_or_else(|| invalid("content edit has no archive cut"))?
            .verify_complete_source(self, cid)?;
        replacement
            .manifest
            .archive_cut
            .as_ref()
            .ok_or_else(|| invalid("content edit has no archive cut"))?
            .verify_archived(
                self,
                cid,
                replacement
                    .manifest
                    .source_escrow
                    .as_deref()
                    .unwrap_or_default(),
            )?;
        if current != replacement {
            write_private(&directory, MANIFEST_NAME, bytes.as_bytes())?;
        }
        sync_directory(&directory)?;
        self.require_compaction_epoch_locked(cid)?;
        self.raw_edit_phase(Phase::AfterRawEditManifest)?;
        let rows = self.inspect(cid)?;
        let updated = rows
            .iter()
            .find(|row| row.sequence == intent.row.sequence)
            .ok_or_else(|| invalid("content edit selected row disappeared"))?;
        if self.transcript_entry_revision(updated)? != intent.row.after_digest {
            return Err(invalid(
                "content edit selected row differs from replacement",
            ));
        }
        self.finish_content_edit_metadata_locked(cid, &rows)?;
        fs::remove_file(directory.join(INTENT_NAME))?;
        sync_directory(&directory)?;
        Ok(())
    }

    fn raw_edit_phase(&self, _phase: Phase) -> Result<()> {
        #[cfg(test)]
        self.apply_replay_epoch_fault(_phase)?;
        Ok(())
    }
}

fn file_image(path: &Path) -> Result<FileImage> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("content edit archive is not regular"));
    }
    let mut sha = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .ok_or_else(|| invalid("content edit archive size exhausted"))?;
        sha.update(&buffer[..count]);
    }
    Ok(FileImage {
        bytes,
        sha256: sha
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    })
}

fn write_private(directory: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let temporary = directory.join(format!("{name}.tmp"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, directory.join(name))?;
    sync_directory(directory)
}

/// Groups used only for display suppression still require exact original
/// ordinal ordering and producer scope; arbitrary tool text grants no group.
fn zero_projection(rows: &[TranscriptEntry]) -> Result<BTreeSet<usize>> {
    let mut events = Vec::with_capacity(rows.len());
    let mut groups = BTreeSet::new();
    let mut seen = BTreeMap::new();
    let mut invalid_groups = BTreeSet::new();
    for row in rows {
        if row.role == TranscriptRole::System
            && TranscriptExecutionStateRecord::from_transcript_content(&row.content)?.is_some()
        {
            events.push(None);
            continue;
        }
        let event = content::semantic(row)?.map(|(event, _)| event);
        if let Some(Event::ExecutionBlock {
            source,
            execution_group_id: Some(group),
            ordinal: Some(ordinal),
            ..
        }) = &event
        {
            let producer = (
                row.turn_id.clone(),
                row.agent_id.clone(),
                row.pane_id.clone(),
            );
            let previous = seen
                .entry(group.clone())
                .or_insert((0u64, producer.clone(), false));
            if previous.0.checked_add(1) != Some(*ordinal)
                || previous.1 != producer
                || matches!(
                    source,
                    ContextSourceKind::ActionResult | ContextSourceKind::TranscriptTool
                ) && !previous.2
            {
                invalid_groups.insert(group.clone());
            }
            previous.0 = *ordinal;
            previous.2 |= *source == ContextSourceKind::TranscriptAssistant;
            groups.insert(group.clone());
        }
        events.push(event);
    }
    groups.retain(|group| !invalid_groups.contains(group));
    Ok(mez_agent::transcript::transcript_zero_projection_indices(
        rows, &events, &groups,
    ))
}

fn validate_arrival_execution(
    store: &AgentTranscriptStore,
    cid: &str,
    envelope: &Envelope,
    rows: &[TranscriptEntry],
    row: &TranscriptEntry,
) -> Result<()> {
    let Some((
        Event::ExecutionBlock {
            execution_group_id: Some(group),
            ..
        },
        _,
    )) = content::semantic(row)?
    else {
        return Ok(());
    };
    let through = envelope
        .manifest
        .archive_cut
        .as_ref()
        .map_or(0, |cut| cut.through_sequence());
    let indexed = store.indexed_execution_sources_locked(cid, rows, through)?;
    let controls = escrow::qualify_controls(
        envelope
            .manifest
            .source_escrow
            .as_deref()
            .unwrap_or_default(),
        cid,
        rows,
        &indexed,
    )?;
    let state = controls
        .iter()
        .filter(|state| state.group() == group.as_str())
        .max_by_key(|state| state.generation())
        .filter(|state| !state.is_pending())
        .ok_or_else(|| invalid("content edit cannot replace pending execution source"))?;
    content::validate_closed_group(cid, rows, &group, state)
}
