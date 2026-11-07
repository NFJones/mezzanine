//! Typed fork source and the existing initial append receipt's complete payload.
//!
//! A copied checkpoint does not acknowledge its parent's ordinary writes. Its
//! original raw occurrences receive independent contiguous child rows. The
//! immutable initial owner binds those rows, payloads and one planned root; only
//! receipt reconciliation can make that root durable. A lost callback retries
//! these exact bytes rather than allocating a new publication.

use super::*;
use mez_agent::transcript::TranscriptContextRecord;
use mez_agent::{TranscriptEntry, TranscriptRole};

/// Pure worker capture before the actor allocates a child conversation.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ReplayForkSource {
    conversation: String,
    origin: Option<ForkOrigin>,
    checkpoint: ReplayEpochCheckpoint,
    escrow: Vec<escrow::EscrowOccurrence>,
    rows: Vec<TranscriptEntry>,
    control_origins: BTreeMap<u64, ContextBlockOccurrenceId>,
    backing_payloads: BTreeMap<String, String>,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum ForkOrigin {
    Durable(copy::CopyOrigin),
    Memory(copy::MemoryCopyOrigin),
}

impl ForkOrigin {
    fn conversation(&self) -> &str {
        match self {
            Self::Durable(origin) => &origin.conversation,
            Self::Memory(origin) => &origin.conversation,
        }
    }
    fn epoch_revision(&self) -> u64 {
        match self {
            Self::Durable(origin) => origin.epoch_revision(),
            Self::Memory(origin) => origin.epoch_revision,
        }
    }
    fn head(&self) -> ContextBlockOccurrenceId {
        match self {
            Self::Durable(origin) => origin.head,
            Self::Memory(origin) => origin.head,
        }
    }
}

/// Version-seven initial append receipt data, never a committed acknowledgment.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InitialReplayEpoch {
    envelope: Envelope,
    payloads: BTreeMap<String, String>,
}

impl std::fmt::Debug for ReplayForkSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReplayForkSource")
    }
}
impl std::fmt::Debug for InitialReplayEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InitialReplayEpoch")
    }
}

impl ReplayEpochSnapshot {
    /// Captures only selected current raw source. Actual archived wire images
    /// preserve native framing; unarchived canonical source is projected from
    /// its original escrow without claiming ordinary append settlement.
    pub(crate) fn fork_source(
        &self,
        owner: &ReplayEpochContext,
        original_rows: &[TranscriptEntry],
    ) -> Result<ReplayForkSource> {
        let parent = ReplayEpochContext::from_checkpoint(
            ReplayConversationId::new(self.conversation_id.clone())?,
            self.checkpoint.clone(),
            AgentContext::empty(),
        )?;
        owner.validate_source_extension(&parent)?;
        let inherited = self
            .escrow
            .as_deref()
            .ok_or_else(|| invalid("forked typed source requires original escrow migration"))?;
        let producer = inherited
            .first()
            .map(|item| item.original_origin().clone())
            .or_else(|| {
                self.arrivals.first().and_then(|row| {
                    ReplaySourceOrigin::new(&row.turn_id, &row.agent_id, &row.pane_id).ok()
                })
            });
        let needed = owner
            .context()
            .chronology()
            .iter()
            .skip(usize::from(owner.head().is_some()))
            .map(|event| event.record().occurrence_id())
            .collect::<BTreeSet<_>>();
        let items = if needed.is_empty() {
            Vec::new()
        } else {
            escrow::qualification_source(
                inherited,
                owner,
                &producer.ok_or_else(|| invalid("fork source original producer is missing"))?,
                &self.arrivals,
            )?
        };
        let items = escrow::copy_selection(&items, &needed)?
            .into_iter()
            .map(|item| item.for_unsettled_copy())
            .collect::<Vec<_>>();
        escrow::validate(&items)?;
        escrow::validate_current(&items, owner, &[])?;
        ReplayForkSource::from_parts(
            Some(ForkOrigin::Durable(copy::CopyOrigin {
                conversation: self.conversation_id.clone(),
                identity: self.identity.clone(),
                head: owner
                    .head()
                    .ok_or_else(|| invalid("fork source has no typed head"))?
                    .occurrence_id(),
            })),
            owner,
            items,
            original_rows,
            self.copy_backing.clone(),
        )
    }
}

impl ReplayForkSource {
    /// Projects current originals only after their metadata was qualified by
    /// the actual durable snapshot or immutable memory-capture owner.
    pub(super) fn from_parts(
        origin: Option<ForkOrigin>,
        owner: &ReplayEpochContext,
        items: Vec<escrow::EscrowOccurrence>,
        original_rows: &[TranscriptEntry],
        backing_payloads: BTreeMap<String, String>,
    ) -> Result<Self> {
        escrow::validate(&items)?;
        escrow::validate_current(&items, owner, &[])?;
        let needed = owner
            .context()
            .chronology()
            .iter()
            .skip(usize::from(owner.head().is_some()))
            .map(|event| event.record().occurrence_id())
            .collect::<BTreeSet<_>>();
        let mut authored = BTreeMap::new();
        for row in original_rows
            .iter()
            .filter(|row| row.role == TranscriptRole::System)
        {
            if let Some(record) = TranscriptContextRecord::from_transcript_content(&row.content)?
                && needed.contains(&record.record().occurrence_id())
                && authored
                    .insert(record.record().occurrence_id(), (row, record))
                    .is_some()
            {
                return Err(invalid("fork source has duplicate original wire images"));
            }
        }
        let contents = owner
            .context()
            .chronology()
            .iter()
            .map(|event| {
                (
                    event.record().occurrence_id(),
                    event.block().content.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut rows = Vec::new();
        for item in items
            .iter()
            .filter(|item| needed.contains(&item.occurrence_id()))
        {
            let content = contents
                .get(&item.occurrence_id())
                .ok_or_else(|| invalid("fork source original content is missing"))?;
            let mut row = if let Some((row, record)) = authored.get(&item.occurrence_id()) {
                if !item.matches(row, record, content) {
                    return Err(invalid("fork source original wire image changed"));
                }
                (*row).clone()
            } else {
                item.copy_row(content.clone(), owner.conversation_id().as_str())?
            };
            row.sequence = rows.len() as u64 + 1;
            rows.push(row);
        }
        Ok(ReplayForkSource {
            conversation: owner.conversation_id().as_str().into(),
            origin,
            checkpoint: owner.checkpoint()?,
            escrow: items,
            rows,
            control_origins: BTreeMap::new(),
            backing_payloads,
        })
    }
}

impl ReplayForkSource {
    /// The captured conversation is descriptive provenance, never a promise
    /// that an unsaved source has storage or restart authority.
    pub(crate) fn conversation_id(&self) -> &str {
        &self.conversation
    }

    /// Builds one independent initial copy from this immutable selection.
    /// Canonical members keep their IDs/costs; controls get only structural
    /// row placement. The caller's real persistence owner must commit it.
    pub(crate) fn initial_batch(
        &self,
        target: &str,
        created_at: u64,
    ) -> Result<super::super::TranscriptAppendBatch> {
        use super::super::{InitialTranscriptRowAccounting, TranscriptAppendBatch};
        if target == self.conversation {
            return Err(MezError::conflict(
                "fork target must differ from its source",
            ));
        }
        let mut origins = BTreeMap::new();
        let mut records = BTreeMap::new();
        let mut rows = Vec::with_capacity(self.rows.len());
        for original in &self.rows {
            let mut row = original.clone();
            row.conversation_id = target.into();
            row.created_at_unix_seconds = created_at;
            if let Some(record) = TranscriptContextRecord::from_transcript_content(&row.content)? {
                origins.insert(row.sequence, record.record().occurrence_id());
                records.insert(row.sequence, record.record());
            } else {
                origins.insert(
                    row.sequence,
                    *self
                        .control_origins
                        .get(&row.sequence)
                        .ok_or_else(|| invalid("copied raw control placement is missing"))?,
                );
            }
            rows.push(row);
        }
        let accounting = InitialTranscriptRowAccounting::new(origins, records)
            .with_stable_records(Some(self.checkpoint.stable_records().to_vec()))?;
        let batch = if self.has_head() {
            TranscriptAppendBatch::initial_replay(self.for_target(target)?, rows)?
        } else {
            TranscriptAppendBatch::rows(rows)?
        };
        batch.with_initial_row_accounting(accounting)
    }

    /// A raw capture has no summary identity or compaction manifest to copy.
    pub(crate) fn has_head(&self) -> bool {
        self.origin.is_some()
    }

    /// Zero-model controls need physical placement, never a model cost record.
    pub(crate) fn control_origins(&self) -> &BTreeMap<u64, ContextBlockOccurrenceId> {
        &self.control_origins
    }

    pub(super) fn append_raw_controls(
        &mut self,
        controls: &[(ContextBlockOccurrenceId, TranscriptEntry)],
    ) -> Result<()> {
        if self.has_head() {
            return Err(invalid(
                "raw lifecycle controls cannot extend a copied head",
            ));
        }
        for (origin, row) in controls {
            let mut row = row.clone();
            row.sequence = self.rows.len() as u64 + 1;
            self.control_origins.insert(row.sequence, *origin);
            self.rows.push(row);
        }
        Ok(())
    }

    pub(crate) fn rows(&self) -> &[TranscriptEntry] {
        &self.rows
    }

    /// Rebinds ownership once, before the initial receipt is admitted. The
    /// empty cut claims no child row settled; all source remains backed until
    /// the original receipt actually reconciles the independently copied rows.
    pub(crate) fn for_target(&self, target: &str) -> Result<InitialReplayEpoch> {
        let origin = self
            .origin
            .as_ref()
            .ok_or_else(|| invalid("raw initial source has no compaction publication identity"))?;
        let frontier = ReplayEpochContext::from_checkpoint(
            ReplayConversationId::new(origin.conversation())?,
            self.checkpoint.clone(),
            AgentContext::empty(),
        )?
        .context()
        .chronology()
        .last()
        .map(|event| event.sequence().get());
        let checkpoint = self
            .checkpoint
            .clone()
            .for_conversation_copy(ReplayConversationId::new(target)?)?;
        let mut payloads = self.backing_payloads.clone();
        let checkpoint = externalize(checkpoint, &mut payloads)?;
        let (copy_origin, memory_copy_origin) = match origin {
            ForkOrigin::Durable(origin) => (Some(origin.clone()), None),
            ForkOrigin::Memory(origin) => (None, Some(origin.clone())),
        };
        let manifest = Manifest {
            version: 9,
            conversation_id: target.into(),
            epoch_revision: origin.epoch_revision(),
            revision: 1,
            publication: ContextBlockOccurrenceId::allocate(),
            expected_parent: None,
            parent_head: None,
            head: origin.head(),
            covered: Vec::new(),
            frozen_arrival_frontier: frontier,
            source_checkpoint: checkpoint.clone(),
            checkpoint,
            copy_origin,
            memory_copy_origin,
            source_escrow: Some(self.escrow.clone()),
            archive_cut: Some(cursor::ArchiveCut::empty()),
        };
        let envelope = Envelope {
            sha256: digest(&encode(&manifest)?),
            manifest,
        };
        let initial = InitialReplayEpoch { envelope, payloads };
        initial.owner()?;
        Ok(initial)
    }
}

impl InitialReplayEpoch {
    pub(crate) fn conversation_id(&self) -> &str {
        &self.envelope.manifest.conversation_id
    }

    pub(crate) fn owner(&self) -> Result<ReplayEpochContext> {
        if !self.envelope.manifest.is_copy()
            || self.envelope.manifest.archive_cut.as_ref() != Some(&cursor::ArchiveCut::empty())
            || self
                .envelope
                .manifest
                .source_escrow
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|item| item.is_archived())
            || encode(self)?.len() > SOURCE_LIMIT + MANIFEST_LIMIT as usize
        {
            return Err(invalid(
                "initial typed root has invalid publication ownership",
            ));
        }
        ReplayEpochPublication::validate_candidate(
            &self.envelope,
            &self.payloads,
            AgentContext::empty(),
        )
    }

    /// Validates complete raw placement separately from the optional head.
    pub(crate) fn validate_rows(&self, rows: &[TranscriptEntry]) -> Result<()> {
        let owner = self.owner()?;
        let mut ids = BTreeSet::new();
        for row in rows {
            if row.conversation_id != self.conversation_id() || row.role != TranscriptRole::System {
                return Err(invalid("initial typed root has a foreign raw row"));
            }
            let record = TranscriptContextRecord::from_transcript_content(&row.content)?
                .ok_or_else(|| invalid("initial typed raw row has no original accounting"))?;
            if !ids.insert(record.record().occurrence_id()) {
                return Err(invalid("initial typed raw occurrence is duplicated"));
            }
        }
        let needed = owner
            .context()
            .chronology()
            .iter()
            .skip(1)
            .map(|event| event.record().occurrence_id())
            .collect::<BTreeSet<_>>();
        if ids != needed {
            return Err(invalid(
                "initial typed rows do not cover exact raw membership",
            ));
        }
        let result = escrow::qualify_rows(
            self.envelope
                .manifest
                .source_escrow
                .as_deref()
                .unwrap_or_default(),
            self.conversation_id(),
            rows,
            &BTreeMap::new(),
            escrow::RowAdmission::AuthoredArrivals,
            |reference| {
                self.payloads
                    .get(&reference.sha256)
                    .cloned()
                    .ok_or_else(|| invalid("initial typed raw payload is missing"))
            },
        )?;
        if result.copies.len() != rows.len() || !result.arrivals.is_empty() {
            return Err(invalid("initial typed raw rows changed original source"));
        }
        Ok(())
    }

    /// Explicit planned identity used only after initial admission. Subsequent
    /// publication first recovers that receipt and checks the actual durable
    /// identity under its lease; this descriptor cannot acknowledge a write.
    pub(crate) fn pending_snapshot(
        &self,
        store: &AgentTranscriptStore,
    ) -> Result<ReplayEpochSnapshot> {
        Ok(ReplayEpochSnapshot {
            root: store.root.clone(),
            conversation_id: self.conversation_id().into(),
            identity: self.envelope.identity(),
            pending_initial: true,
            checkpoint: self.owner()?.checkpoint()?,
            escrow: self.envelope.manifest.source_escrow.clone(),
            copy_backing: escrow::copy_backing(
                self.envelope
                    .manifest
                    .source_escrow
                    .as_deref()
                    .unwrap_or_default(),
                &self.owner()?,
                |reference| {
                    self.payloads
                        .get(&reference.sha256)
                        .cloned()
                        .ok_or_else(|| invalid("initial copy dependency payload is missing"))
                },
            )?,
            qualified_rows: BTreeSet::new(),
            arrivals: Vec::new(),
            archive_cut: self.envelope.manifest.archive_cut.clone(),
            execution_sources: BTreeMap::new(),
        })
    }
}

impl AgentTranscriptStore {
    /// Checks complete initial identity before rows or receipt files mutate.
    pub(in crate::storage::transcript) fn initial_replay_matches_locked(
        &self,
        initial: &InitialReplayEpoch,
    ) -> Result<bool> {
        initial.owner()?;
        // The exact initial receipt may own a required target before its first
        // manifest exists. Admission separately requires that retained owner;
        // ordinary readers still refuse a missing required manifest.
        if !self
            .root
            .join(initial.conversation_id())
            .join(MANIFEST_NAME)
            .try_exists()?
        {
            return Ok(false);
        }
        let current = self.read_replay_envelope_locked(initial.conversation_id())?;
        if current
            .as_ref()
            .is_some_and(|current| current != &initial.envelope)
        {
            return Err(MezError::conflict(
                "initial typed root differs from existing authority",
            ));
        }
        Ok(current.is_some())
    }

    /// Reconciles exactly the journal-owned root, including a swapped manifest
    /// whose required marker or acknowledgment was lost. No new identity is made.
    pub(in crate::storage::transcript) fn publish_initial_replay_locked(
        &self,
        initial: &InitialReplayEpoch,
    ) -> Result<()> {
        self.initial_replay_matches_locked(initial)?;
        let phase = |_phase: Phase| {
            #[cfg(test)]
            self.apply_replay_epoch_fault(_phase)?;
            Ok::<_, MezError>(())
        };
        let directory = self.root.join(initial.conversation_id());
        for (hash, content) in &initial.payloads {
            payload::publish(&directory, hash, content.as_bytes(), || {
                phase(Phase::BeforePayloadSync)
            })?;
        }
        let bytes = encode(&initial.envelope)?;
        payload::publish(&directory, &digest(&bytes), &bytes, || {
            phase(Phase::BeforeSourceReceiptSync)
        })?;
        let archive = self.transcript_path(initial.conversation_id())?;
        // Head-only receipts own a real empty archive; never truncate a tail.
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&archive)
        {
            Ok(file) => file.sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                fs::File::open(&archive)?.sync_all()?
            }
            Err(error) => return Err(error.into()),
        }
        let temporary = directory.join(".initial-replay.tmp");
        let result = (|| {
            // Only this retained receipt can own/recover its private staging file.
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
            self.require_compaction_epoch_locked(initial.conversation_id())?;
            sync_directory(&directory)?;
            sync_directory(&self.root)?;
            phase(Phase::BeforeAcknowledgment)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    /// Settlement verifies the exact root receipt and all backing, rather than
    /// treating equal rows or a matching header as complete initial authority.
    pub(in crate::storage::transcript) fn verify_initial_replay_locked(
        &self,
        initial: &InitialReplayEpoch,
    ) -> Result<()> {
        if !self.initial_replay_matches_locked(initial)?
            || !self.compaction_epoch_marker_is_committed(initial.conversation_id())?
        {
            return Err(invalid("initial typed root is not fully committed"));
        }
        let directory = self.root.join(initial.conversation_id());
        initial.envelope.verify_receipt(&directory)?;
        initial.envelope.verify_escrow_payloads(&directory)?;
        initial
            .envelope
            .restore(AgentContext::empty(), |reference| {
                payload::read(&directory, &reference)
            })?;
        Ok(())
    }
}
