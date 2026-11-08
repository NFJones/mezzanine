//! Deterministic migration of an existing one-prefix authority.
//!
//! This transition preserves its original head and exact active raw projection.
//! It neither summarizes covered history nor acknowledges ordinary pending rows.
//! The original sidecar is immutable integrity-bound provenance in the same
//! publication receipt. One held conversation lease fences source and swap.

use super::super::AgentCompactionEpoch;
use super::*;
use mez_agent::{ContextBlockRecord, TranscriptEntry};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrefixOrigin {
    pub(super) authority: Payload,
    through_sequence: u64,
    head: ContextBlockOccurrenceId,
}

impl PrefixOrigin {
    pub(super) fn validate(&self, manifest: &Manifest) -> Result<()> {
        self.authority.validate_format()?;
        if self.authority.bytes as u64 > MANIFEST_LIMIT
            || manifest.version != 10
            || manifest.copy_origin.is_some()
            || manifest.memory_copy_origin.is_some()
            || manifest.epoch_revision != 1
            || manifest.revision != 1
            || manifest.expected_parent.is_some()
            || manifest.parent_head.is_some()
            || !manifest.covered.is_empty()
            || manifest.head != self.head
            || manifest.source_checkpoint != manifest.checkpoint
            || manifest
                .archive_cut
                .as_ref()
                .is_none_or(|cut| cut.through_sequence() < self.through_sequence)
        {
            return Err(invalid("legacy prefix migration has invalid provenance"));
        }
        Ok(())
    }

    pub(super) fn validate_owner(&self, owner: &ReplayEpochContext, original: &str) -> Result<()> {
        let old: AgentCompactionEpoch = serde_json::from_str(original)
            .map_err(|_| invalid("legacy prefix provenance is not a complete epoch"))?;
        let event = owner
            .context()
            .chronology()
            .first()
            .ok_or_else(|| invalid("migrated prefix head is missing"))?;
        if old.version != 3
            || !old.ranges.is_empty()
            || old.conversation_id != owner.conversation_id().as_str()
            || old.through_sequence != self.through_sequence
            || old.summary != event.block().content
            || old
                .summary_records
                .as_ref()
                .and_then(|records| records.prefix)
                != Some(event.record())
            || event.record().occurrence_id() != self.head
        {
            return Err(invalid(
                "migrated prefix differs from its original authority",
            ));
        }
        Ok(())
    }
}

impl AgentTranscriptStore {
    /// Converts a worker's checked full durable suffix under an expected old
    /// authority. Append-only arrivals after capture stay beyond the frozen cut.
    /// Changed source/authority refuses before payload publication. A retry after
    /// an ambiguous swap recognizes the exact original imported authority.
    pub(crate) fn normalize_legacy_prefix(
        &self,
        conversation: &str,
        expected: &AgentCompactionEpoch,
        owner: &ReplayEpochContext,
        captured: &[TranscriptEntry],
        project_backing: impl FnOnce(
            &super::super::ConversationReplaySnapshot,
        ) -> Result<(AgentContext, Vec<(u64, ContextBlockRecord)>)>,
    ) -> Result<()> {
        let _lease = self.acquire_conversation_lock(conversation)?;
        let directory = self.session_dir_path(conversation)?;
        // Grouped wire framing is descriptive source, not an original producer
        // interval or completion witness. The pure import protects all groups;
        // a reconstructed checkpoint cannot bypass that import contract.
        if !owner.execution_states().map_err(context_error)?.is_empty()
            || owner
                .context()
                .chronology()
                .iter()
                .filter_map(|event| event.execution_group_id())
                .any(|group| !owner.context().execution_group_is_pending(group))
        {
            return Err(invalid(
                "legacy prefix cannot qualify producer lifecycle claims",
            ));
        }
        // Migration already qualifies the full original archive. Preserve the
        // latest stable accounting at its frozen cut even when the carrier is
        // covered by the old prefix. These are descriptive records only; cold
        // composition still obtains every instruction byte from current policy.
        let through = captured
            .last()
            .map_or(expected.through_sequence, |row| row.sequence);
        let rows = self.inspect_validated_transcript(conversation)?;
        let mut stable = super::super::stable_records::StableRecordCapture::new(Some(through));
        for row in &rows {
            stable.observe(row);
        }
        let baseline = self.initial_row_accounting_locked(conversation)?;
        let stable = stable.finish();
        let stable_records = match stable.as_ref() {
            Some(snapshot) => snapshot.records()?.to_vec(),
            None => baseline
                .as_ref()
                .and_then(|accounting| accounting.stable_records())
                .unwrap_or_default()
                .to_vec(),
        };
        let proposed_checkpoint = owner.checkpoint()?.with_stable_records(stable_records)?;
        if self.has_replay_manifest(conversation)? {
            let envelope = self
                .read_replay_envelope_locked(conversation)?
                .ok_or_else(|| invalid("prefix migration authority disappeared"))?;
            let origin = envelope
                .manifest
                .prefix_origin
                .as_ref()
                .ok_or_else(|| MezError::conflict("prefix migration authority changed"))?;
            let original = payload::read(&directory, &origin.authority)?;
            let actual: AgentCompactionEpoch = serde_json::from_str(&original)
                .map_err(|_| invalid("invalid original prefix authority"))?;
            if actual != *expected {
                return Err(MezError::conflict("prefix migration authority changed"));
            }
            let checkpoint = envelope
                .manifest
                .checkpoint
                .clone()
                .try_map_payloads(|_, reference| payload::read(&directory, &reference))?;
            let through = envelope
                .manifest
                .archive_cut
                .as_ref()
                .ok_or_else(|| invalid("prefix migration lost its original cut"))?
                .through_sequence();
            if proposed_checkpoint != checkpoint
                || u64::try_from(captured.len()).ok()
                    != through.checked_sub(actual.through_sequence)
            {
                return Err(MezError::conflict("prefix migration proposal changed"));
            }
            for (offset, row) in captured.iter().enumerate() {
                let sequence = actual
                    .through_sequence
                    .checked_add(offset as u64)
                    .and_then(|value| value.checked_add(1))
                    .ok_or_else(|| invalid("prefix migration source sequence exhausted"))?;
                let indexed = self
                    .indexed_row_reference_locked(conversation, sequence)?
                    .ok_or_else(|| invalid("prefix migration lost an original row image"))?;
                if row.conversation_id != conversation
                    || row.sequence != sequence
                    || self.transcript_entry_revision(row)? != indexed.digest
                {
                    return Err(MezError::conflict("prefix migration raw source changed"));
                }
            }
            self.replay_epoch_snapshot_locked(conversation)?;
            return Ok(());
        }
        self.check_replay_compatibility_locked(conversation)?;
        let actual = self
            .compaction_epoch_locked(conversation)?
            .ok_or_else(|| MezError::conflict("prefix migration authority disappeared"))?;
        if actual != *expected
            || !actual.ranges.is_empty()
            || actual.conversation_id != conversation
            || owner.conversation_id().as_str() != conversation
            || owner.revision().get() != 1
        {
            return Err(MezError::conflict("prefix migration authority changed"));
        }
        let suffix = rows
            .iter()
            .filter(|row| row.sequence > actual.through_sequence)
            .take(captured.len())
            .collect::<Vec<_>>();
        if suffix.len() != captured.len() || !suffix.iter().copied().eq(captured.iter()) {
            return Err(MezError::conflict("prefix migration raw source changed"));
        }
        let through = captured
            .last()
            .map_or(actual.through_sequence, |row| row.sequence);
        let frozen = rows
            .iter()
            .filter(|row| row.sequence <= through)
            .cloned()
            .collect::<Vec<_>>();
        // Original row images and costs are descriptive migration state, never
        // authored append receipts or evidence of execution completion.
        self.capture_legacy_prefix_images_locked(conversation, &frozen)?;
        let escrow::LegacyPrefixSource {
            items,
            mut payloads,
        } = escrow::legacy_prefix_source(
            self,
            conversation,
            actual.through_sequence,
            through,
            owner,
            &frozen,
            || {
                let origins = self.transcript_row_origins_locked(conversation, &frozen)?;
                let records = self.transcript_row_records_locked(conversation, &origins)?;
                let snapshot = super::super::ConversationReplaySnapshot {
                    epoch: None,
                    replay: None,
                    view: super::super::store::ConversationTranscriptView {
                        committed: frozen.clone(),
                        logical: frozen.clone(),
                    },
                    row_origins: origins,
                    row_records: records,
                    stable_records: None,
                    initial_stable_records: None,
                    settled_pending_origins: Default::default(),
                };
                let (context, updates) = project_backing(&snapshot)?;
                self.save_rendered_row_records_locked(conversation, &snapshot, &updates)?;
                Ok(context)
            },
        )?;
        let cut = cursor::ArchiveCut::for_copied_archive(self, conversation, through)?;
        let original = bounded_read(&directory.join(MANIFEST_NAME), MANIFEST_LIMIT)?;
        let original = String::from_utf8(original)
            .map_err(|_| invalid("original prefix authority is not UTF-8"))?;
        let head = owner
            .head()
            .ok_or_else(|| invalid("migrated source has no head"))?
            .occurrence_id();
        let origin = PrefixOrigin {
            authority: Payload::new(original.as_bytes()),
            through_sequence: actual.through_sequence,
            head,
        };
        origin.validate_owner(owner, &original)?;
        payloads.insert(origin.authority.sha256.clone(), original);
        let checkpoint = externalize(proposed_checkpoint, &mut payloads)?;
        let manifest = Manifest {
            version: 10,
            conversation_id: conversation.into(),
            epoch_revision: 1,
            revision: 1,
            publication: ContextBlockOccurrenceId::allocate(),
            expected_parent: None,
            parent_head: None,
            head,
            covered: Vec::new(),
            frozen_arrival_frontier: owner
                .context()
                .chronology()
                .last()
                .map(|event| event.sequence().get()),
            source_checkpoint: checkpoint.clone(),
            checkpoint,
            copy_origin: None,
            memory_copy_origin: None,
            prefix_origin: Some(origin),
            raw_revision_origin: None,
            source_escrow: Some(items),
            archive_cut: Some(cut),
        };
        let envelope = Envelope {
            sha256: digest(&encode(&manifest)?),
            manifest,
        };
        ReplayEpochPublication::validate_candidate(&envelope, &payloads, AgentContext::empty())?;
        let phase = |_phase: Phase| {
            #[cfg(test)]
            self.apply_replay_epoch_fault(_phase)?;
            Ok::<_, MezError>(())
        };
        for (hash, content) in payloads {
            payload::publish(&directory, &hash, content.as_bytes(), || {
                phase(Phase::BeforePayloadSync)
            })?;
        }
        let bytes = encode(&envelope)?;
        payload::publish(&directory, &digest(&bytes), &bytes, || {
            phase(Phase::BeforeSourceReceiptSync)
        })?;
        fs::File::open(self.existing_transcript_path_for(conversation)?)?.sync_all()?;
        let temporary = directory.join(".prefix-migration.tmp");
        let result = (|| {
            if temporary.try_exists()? {
                fs::remove_file(&temporary)?;
            }
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
            self.require_compaction_epoch_locked(conversation)?;
            sync_directory(&directory)?;
            phase(Phase::BeforeAcknowledgment)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}
