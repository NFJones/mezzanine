//! Independent durable roots for the existing saved-fork transaction.
//!
//! Both conversation leases remain owned by the caller. A copy preserves the
//! selected epoch and payload identities, retires retrieved grants, and copies
//! required backing into the target namespace. It neither summarizes source nor
//! acknowledges an ordinary append. The fork's existing generation journal owns
//! compensation, so this adds no queue or independently selectable authority.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// Integrity-bound provenance, not a reference required to reopen the child.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CopyOrigin {
    pub(super) conversation: String,
    pub(super) identity: ReplayEpochIdentity,
    pub(super) head: ContextBlockOccurrenceId,
}

impl CopyOrigin {
    pub(super) fn epoch_revision(&self) -> u64 {
        self.identity.epoch_revision
    }

    /// A source copy starts its own CAS chain while retaining the original
    /// canonical epoch. No rewrite, covered membership or parent CAS is claimed.
    pub(super) fn validate(&self, manifest: &Manifest) -> Result<()> {
        mez_agent::transcript::validate_conversation_id(&self.conversation)?;
        Payload {
            sha256: self.identity.sha256.clone(),
            bytes: 0,
        }
        .validate_format()?;
        if !matches!(manifest.version, 8 | 9)
            || manifest.memory_copy_origin.is_some()
            || self.conversation == manifest.conversation_id
            || self.identity.epoch_revision == 0
            || self.identity.revision == 0
            || manifest.epoch_revision != self.identity.epoch_revision
            || manifest.revision != 1
            || manifest.expected_parent.is_some()
            || manifest.parent_head.is_some()
            || self.head != manifest.head
            || !manifest.covered.is_empty()
            || manifest.source_checkpoint != manifest.checkpoint
        {
            return Err(invalid("copied epoch root has invalid original provenance"));
        }
        Ok(())
    }
}

/// Descriptive provenance of one actual frozen memory selection. Its capture
/// identity is not a storage revision, publication receipt or restart claim.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MemoryCopyOrigin {
    pub(super) conversation: String,
    pub(super) epoch_revision: u64,
    pub(super) head: ContextBlockOccurrenceId,
    pub(super) capture: ContextBlockOccurrenceId,
}

impl MemoryCopyOrigin {
    pub(super) fn validate(&self, manifest: &Manifest) -> Result<()> {
        mez_agent::transcript::validate_conversation_id(&self.conversation)?;
        if manifest.version != 9
            || manifest.copy_origin.is_some()
            || self.conversation == manifest.conversation_id
            || self.epoch_revision == 0
            || self.epoch_revision != manifest.epoch_revision
            || self.head != manifest.head
            || manifest.revision != 1
            || manifest.expected_parent.is_some()
            || manifest.parent_head.is_some()
            || !manifest.covered.is_empty()
            || manifest.source_checkpoint != manifest.checkpoint
        {
            return Err(invalid(
                "memory copy has invalid original capture provenance",
            ));
        }
        Ok(())
    }
}

/// Worker-owned immutable inputs, captured while both existing leases are held.
pub(in crate::storage::transcript) struct ReplayEpochCopy {
    target: String,
    origin: CopyOrigin,
    checkpoint: ReplayEpochCheckpoint<Payload>,
    escrow: Vec<escrow::EscrowOccurrence>,
    frontier: Option<u64>,
    archive_frontier: u64,
    payloads: BTreeMap<String, String>,
    /// Original full-row proofs captured while the source lease is held.
    indexed_origins: super::super::TranscriptRowOrigins,
}

impl ReplayEpochCopy {
    /// Freezes one root receipt after its exact copied cut is available.
    pub(super) fn into_envelope(
        self,
        cut: cursor::ArchiveCut,
    ) -> Result<(Envelope, BTreeMap<String, String>)> {
        let manifest = Manifest {
            version: 9,
            conversation_id: self.target,
            epoch_revision: self.origin.epoch_revision(),
            revision: 1,
            publication: ContextBlockOccurrenceId::allocate(),
            expected_parent: None,
            parent_head: None,
            head: self.origin.head,
            covered: Vec::new(),
            frozen_arrival_frontier: self.frontier,
            source_checkpoint: self.checkpoint.clone(),
            checkpoint: self.checkpoint,
            copy_origin: Some(self.origin),
            memory_copy_origin: None,
            source_escrow: Some(self.escrow),
            archive_cut: Some(cut),
        };
        Ok((
            Envelope {
                sha256: digest(&encode(&manifest)?),
                manifest,
            },
            self.payloads,
        ))
    }
}

impl AgentTranscriptStore {
    /// Captures the sole typed authority and required backing without following
    /// covered ancestors or creating a target. Source payloads never become
    /// shared mutable files or mandatory parent references in the child.
    pub(in crate::storage::transcript) fn prepare_replay_copy_locked(
        &self,
        source: &str,
        target: &str,
        rows: &[mez_agent::TranscriptEntry],
    ) -> Result<Option<ReplayEpochCopy>> {
        if !self.has_replay_manifest(source)? {
            return Ok(None);
        }
        let envelope = self
            .read_replay_envelope_locked(source)?
            .ok_or_else(|| invalid("copied source lost its authoritative epoch"))?;
        let directory = self.root.join(source);
        envelope.verify_receipt(&directory)?;
        envelope.verify_escrow_payloads(&directory)?;
        let owner = envelope.restore(AgentContext::empty(), |reference| {
            payload::read(&directory, &reference)
        })?;
        let escrow = envelope
            .manifest
            .source_escrow
            .as_ref()
            .ok_or_else(|| invalid("copied source requires explicit escrow migration"))?;
        let cut = envelope
            .manifest
            .archive_cut
            .as_ref()
            .ok_or_else(|| invalid("copied source requires explicit archive-cut migration"))?;
        cut.verify_archived(self, source, escrow)?;
        cut.verify_complete_source(self, source)?;
        let indexed_origins = self.available_indexed_row_origins_locked(source, rows)?;
        if rows
            .iter()
            .filter(|row| row.sequence <= cut.through_sequence())
            .count() as u64
            != cut.through_sequence()
            || rows.iter().any(|row| {
                row.sequence <= cut.through_sequence()
                    && !indexed_origins.contains_key(&row.sequence)
            })
        {
            return Err(invalid("saved copy lost original indexed archive source"));
        }
        let checkpoint = owner
            .checkpoint()?
            .with_stable_records(envelope.manifest.checkpoint.stable_records().to_vec())?
            .for_conversation_copy(ReplayConversationId::new(target)?)?;
        let mut payloads = BTreeMap::new();
        let checkpoint = externalize(checkpoint, &mut payloads)?;
        payloads.extend(escrow::copy_backing(escrow, &owner, |reference| {
            payload::read(&directory, reference)
        })?);
        for item in escrow.iter().filter(|item| !item.is_archived()) {
            let reference = item.payload();
            if let std::collections::btree_map::Entry::Vacant(entry) =
                payloads.entry(reference.sha256.clone())
            {
                entry.insert(payload::read(&directory, reference)?);
            }
        }
        Ok(Some(ReplayEpochCopy {
            target: target.to_owned(),
            origin: CopyOrigin {
                conversation: source.to_owned(),
                identity: envelope.identity(),
                head: envelope.manifest.head,
            },
            checkpoint,
            escrow: escrow.clone(),
            frontier: envelope.manifest.frozen_arrival_frontier,
            archive_frontier: cut.through_sequence(),
            payloads,
            indexed_origins,
        }))
    }

    /// Installs the independent root after the fork owner copied and indexed
    /// ordinary rows. The original archive frontier is translated, never
    /// extended past later authored arrivals. All filesystem work stays on the
    /// existing command worker and shares its generation-safe compensation.
    pub(in crate::storage::transcript) fn publish_replay_copy_locked(
        &self,
        copy: ReplayEpochCopy,
        rows: &[mez_agent::TranscriptEntry],
        cancelled: &[&AtomicBool],
    ) -> Result<()> {
        let check_cancelled = || {
            if cancelled.iter().any(|flag| flag.load(Ordering::Acquire)) {
                Err(invalid("saved epoch copy cancelled before adoption"))
            } else {
                Ok(())
            }
        };
        let phase = |_phase: Phase| {
            check_cancelled()?;
            #[cfg(test)]
            self.apply_replay_epoch_fault(_phase)?;
            Ok::<_, MezError>(())
        };
        check_cancelled()?;
        let directory = self.root.join(&copy.target);
        if directory.join(MANIFEST_NAME).try_exists()? {
            return Err(MezError::conflict(
                "copied target already has epoch authority",
            ));
        }
        // A typed source may have a head and no ordinary rows. Its fresh
        // target still needs a genuine empty audit archive before cut checks.
        self.ensure_session_dir(&copy.target)?;
        let archive = self.transcript_path(&copy.target)?;
        if copy.archive_frontier == 0 && !archive.try_exists()? {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&archive)?;
        }
        self.install_copied_row_images_locked(&copy.target, rows, &copy.indexed_origins)?;
        let cut =
            cursor::ArchiveCut::for_copied_archive(self, &copy.target, copy.archive_frontier)?;
        cut.verify_archived(self, &copy.target, &copy.escrow)?;
        let (envelope, payloads) = copy.into_envelope(cut)?;
        let bytes = encode(&envelope)?;
        if bytes.len() as u64 > MANIFEST_LIMIT {
            return Err(invalid("copied epoch metadata exceeds publication limit"));
        }
        ReplayEpochPublication::validate_candidate(&envelope, &payloads, AgentContext::empty())?;
        for (hash, content) in payloads {
            payload::publish(&directory, &hash, content.as_bytes(), || {
                phase(Phase::BeforePayloadSync)
            })?;
        }
        payload::publish(&directory, &digest(&bytes), &bytes, || {
            phase(Phase::BeforeSourceReceiptSync)
        })?;
        fs::File::open(self.transcript_path(&envelope.manifest.conversation_id)?)?.sync_all()?;
        let temporary = directory.join(".replay-copy.tmp");
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            phase(Phase::BeforeManifestSwap)?;
            fs::rename(&temporary, directory.join(MANIFEST_NAME))?;
            phase(Phase::AfterManifestSwap)?;
            sync_directory(&directory)?;
            phase(Phase::BeforeRequiredMarker)?;
            self.require_compaction_epoch_locked(&envelope.manifest.conversation_id)?;
            sync_directory(&directory)?;
            sync_directory(&self.root)?;
            phase(Phase::BeforeAcknowledgment)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}
