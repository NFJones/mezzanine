//! Immutable single-head source backing and expected-parent publication.
//!
//! The existing conversation mutation lease serializes this writer with archive,
//! editing and persistence owners. Version five extends the same authoritative
//! compaction sidecar, never a parallel selectable history. Payloads are private,
//! content-addressed immutable files; retained bytes are reused across epochs.
//! A prepared proposal retains its original publication identity and summary so
//! uncertain acknowledgments can settle the exact committed result. It neither
//! dispatches model work nor asserts ordinary transcript append commitment.
//! Immutable source/proposal receipts also preserve covered live evidence until
//! ordinary audit settlement; they are never an alternative replay selection.
//! Required per-publication escrow carries earlier unsettled originals and
//! their turn provenance. Late exact copies are qualified for deduplication,
//! never expanded into model history. The v4 prototype remains readable only at
//! its frozen empty-archive boundary and requires an explicit escrow transition.
//! Actual actor lifecycle, new arrival custody, cursor/settlement, legacy
//! migration and cleanup remain consumers; legacy-only APIs refuse these formats.

use super::AgentTranscriptStore;
use crate::{MezError, Result};
use mez_agent::{
    AgentContext, ContextBlockOccurrenceId, ReplayCompactionPlan, ReplayConversationId,
    ReplayEpochCheckpoint, ReplayEpochContext, ReplayHeadPublication, ReplayPreparedCompaction,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
};

mod escrow;
mod payload;
pub(crate) use escrow::ReplaySourceOrigin;
use payload::{Payload, bounded_read, digest, sync_directory};
#[cfg(test)]
mod tests;

/// Uses the existing authority filename and its required-state metadata.
const MANIFEST_NAME: &str = "compaction-epoch.json";
/// Bounds metadata independently from referenced payload bytes.
const MANIFEST_LIMIT: u64 = 16 * 1024 * 1024;
/// Bounds the complete current source, not a lifetime audit-history scan.
const SOURCE_LIMIT: usize = 64 * 1024 * 1024;

/// Exact committed parent, distinct from attempt generations and cache lineage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplayEpochIdentity {
    epoch_revision: u64,
    revision: u64,
    publication: ContextBlockOccurrenceId,
    sha256: String,
}

/// Coherent committed identity and checked source from one mutation lease.
/// It is immutable history, not a live owner or an execution capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplayEpochSnapshot {
    identity: ReplayEpochIdentity,
    checkpoint: ReplayEpochCheckpoint,
    escrow: Option<Vec<escrow::EscrowOccurrence>>,
    qualified_rows: BTreeSet<u64>,
}

impl ReplayEpochSnapshot {
    /// Borrows the exact committed identity captured with this checked source.
    pub(crate) fn identity(&self) -> &ReplayEpochIdentity {
        &self.identity
    }

    /// Borrows immutable checked source without exposing mutation of the
    /// identity/source association to prompt projection or control adapters.
    pub(crate) fn checkpoint(&self) -> &ReplayEpochCheckpoint {
        &self.checkpoint
    }

    /// Identifies an exact original source copy checked under the same lease.
    /// This permits only deduplication, never model-source or execution adoption.
    pub(crate) fn row_is_qualified_copy(&self, sequence: u64) -> bool {
        self.qualified_rows.contains(&sequence)
    }
}

/// Complete current authority; old covered source is not a replay membership.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    conversation_id: String,
    epoch_revision: u64,
    revision: u64,
    publication: ContextBlockOccurrenceId,
    expected_parent: Option<ReplayEpochIdentity>,
    parent_head: Option<ContextBlockOccurrenceId>,
    head: ContextBlockOccurrenceId,
    covered: Vec<ContextBlockOccurrenceId>,
    frozen_arrival_frontier: Option<u64>,
    /// Immutable pre-rewrite source, used for audit and publication proof only.
    /// Ordinary replay never expands these covered occurrences into raw input.
    source_checkpoint: ReplayEpochCheckpoint<Payload>,
    checkpoint: ReplayEpochCheckpoint<Payload>,
    /// Absent only in the old v4 prototype, which has no inherited escrow proof.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_escrow: Option<Vec<escrow::EscrowOccurrence>>,
}

/// Integrity covers all semantic framing, source references and membership.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    manifest: Manifest,
    sha256: String,
}

/// Exact immutable proposal retained by the persistence owner until settlement.
/// It may be cloned across worker retry, never regenerated after uncertain I/O.
#[derive(Clone)]
pub(crate) struct ReplayEpochPublication {
    envelope: Envelope,
    payloads: BTreeMap<String, String>,
    /// Actual controller owner retains nonserializable lifecycle witnesses.
    /// Its checkpoint must exactly match the immutable recorded source.
    prepared: ReplayPreparedCompaction,
    origin: ReplaySourceOrigin,
    inherited: Vec<escrow::EscrowOccurrence>,
}

/// Nonserializable acknowledgment minted after the original store's complete
/// durability barrier. It names one exact proposal and archive; descriptive
/// loaded identities cannot stand in for this live adoption receipt.
#[derive(Debug, Clone)]
pub(crate) struct ReplayEpochCommit {
    identity: ReplayEpochIdentity,
    root: std::path::PathBuf,
}

impl ReplayEpochCommit {
    /// Borrows the exact committed publication identity for owner bookkeeping.
    pub(crate) fn identity(&self) -> &ReplayEpochIdentity {
        &self.identity
    }

    /// Adopts only this acknowledged proposal into its original live body.
    /// The caller supplies its frozen source store, never a newly looked-up
    /// installation. Wrong archives/proposals/incarnations reject atomically.
    /// Duplicate acknowledgments preserve current instructions and arrivals;
    /// they neither reload a cold owner nor acknowledge ordinary append work.
    pub(crate) fn adopt(
        &self,
        original_store: &AgentTranscriptStore,
        proposal: &ReplayEpochPublication,
        context: &mut AgentContext,
    ) -> Result<()> {
        if self.root != original_store.root || self.identity != proposal.identity() {
            return Err(MezError::conflict(
                "replay acknowledgment does not match original store and proposal",
            ));
        }
        context
            .update_replay_owner(|owner| {
                owner.adopt_compaction_publication(&proposal.prepared)?;
                Ok(())
            })
            .map_err(context_error)
    }
}

/// Fault boundaries mirror actual ordered storage transitions, not dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    BeforePayloadSync,
    BeforeSourceReceiptSync,
    BeforeManifestSwap,
    AfterManifestSwap,
    BeforeRequiredMarker,
    BeforeAcknowledgment,
}

impl ReplayEpochPublication {
    /// Applies the actual pure publication once and freezes its exact output.
    /// Parent revision and source incarnation must match; source remains owned
    /// by the caller. Payload extraction changes neither event IDs nor costs.
    /// This allocates no files and dispatches no provider or execution effects.
    fn prepare(
        conversation_id: &str,
        expected_parent: Option<ReplayEpochIdentity>,
        source: &ReplayEpochContext,
        plan: ReplayCompactionPlan,
        summary: ReplayHeadPublication,
        origin: &ReplaySourceOrigin,
        inherited: &[escrow::EscrowOccurrence],
    ) -> Result<Self> {
        mez_agent::transcript::validate_conversation_id(conversation_id)?;
        if source.revision().get()
            != expected_parent
                .as_ref()
                .map_or(0, |parent| parent.epoch_revision)
            || !plan.requires_model_summary()
        {
            return Err(MezError::conflict(
                "replay source does not match its expected parent",
            ));
        }
        let covered = plan
            .summary_input()
            .iter()
            .filter(|event| !mez_agent::context_block_is_compaction_summary(event.block()))
            .map(|event| event.record().occurrence_id())
            .collect();
        let frontier = plan.arrival_frontier().map(|sequence| sequence.get());
        let prepared = source
            .prepare_compaction_publication(plan, summary)
            .map_err(context_error)?;
        let candidate = prepared.candidate();
        let head = candidate
            .head()
            .ok_or_else(|| invalid("publication has no summary head"))?
            .occurrence_id();
        let mut payloads = BTreeMap::new();
        let source_checkpoint =
            externalize(source.checkpoint().map_err(context_error)?, &mut payloads)?;
        let checkpoint = externalize(
            candidate.checkpoint().map_err(context_error)?,
            &mut payloads,
        )?;
        let manifest = Manifest {
            version: 5,
            conversation_id: conversation_id.to_owned(),
            epoch_revision: candidate.revision().get(),
            revision: expected_parent
                .as_ref()
                .map_or(0, |parent| parent.revision)
                .checked_add(1)
                .ok_or_else(|| invalid("storage revision exhausted"))?,
            publication: ContextBlockOccurrenceId::allocate(),
            expected_parent,
            parent_head: source.head().map(|head| head.occurrence_id()),
            head,
            covered,
            frozen_arrival_frontier: frontier,
            source_checkpoint,
            checkpoint,
            source_escrow: Some(escrow::extend(inherited, source, origin)?),
        };
        let envelope = Envelope {
            sha256: digest(&encode(&manifest)?),
            manifest,
        };
        if encode(&envelope)?.len() as u64 > MANIFEST_LIMIT {
            return Err(invalid("manifest exceeds publication size limit"));
        }
        // Detect foreign source namespaces at proposal creation, before I/O.
        Self::validate_candidate(&envelope, &payloads, AgentContext::empty())?;
        Ok(Self {
            envelope,
            payloads,
            prepared,
            origin: origin.clone(),
            inherited: inherited.to_vec(),
        })
    }

    /// Returns the retained exact publication identity for callback correlation.
    pub(crate) fn identity(&self) -> ReplayEpochIdentity {
        self.envelope.identity()
    }

    /// Validates source and header consistency using supplied immutable payloads.
    fn validate_candidate(
        envelope: &Envelope,
        payloads: &BTreeMap<String, String>,
        prefix: AgentContext,
    ) -> Result<ReplayEpochContext> {
        let resolve = |reference: Payload| {
            let content = payloads
                .get(&reference.sha256)
                .ok_or_else(|| invalid("proposal payload missing"))?;
            reference.validate(content.as_bytes())?;
            Ok(content.clone())
        };
        let candidate = envelope.restore(prefix, resolve)?;
        let mut source_bytes = 0usize;
        let source = ReplayEpochContext::from_checkpoint(
            ReplayConversationId::new(envelope.manifest.conversation_id.clone())
                .map_err(context_error)?,
            envelope
                .manifest
                .source_checkpoint
                .clone()
                .try_map_payloads(|_, reference| {
                    source_bytes = source_bytes
                        .checked_add(reference.bytes)
                        .filter(|n| *n <= SOURCE_LIMIT)
                        .ok_or_else(|| invalid("source receipt exceeds load size limit"))?;
                    resolve(reference)
                })?,
            AgentContext::empty(),
        )
        .map_err(context_error)?;
        let covered = &envelope.manifest.covered;
        if let Some(escrow) = &envelope.manifest.source_escrow {
            escrow::validate_current(escrow, &source, covered)?;
        }
        let count = covered.len();
        let raw_offset = usize::from(source.head().is_some());
        let source_raw = &source.context().chronology()[raw_offset..];
        if source.revision().get().checked_add(1) != Some(candidate.revision().get())
            || source.head().map(|head| head.occurrence_id()) != envelope.manifest.parent_head
            || count > source.members().len()
            || !source
                .members()
                .iter()
                .take(count)
                .map(|member| member.occurrence_id())
                .eq(covered.iter().copied())
            || candidate.members() != &source.members()[count..]
            || candidate.context().chronology()[1..] != source_raw[count..]
            || source.head().map(|head| head.occurrence_id()) == Some(envelope.manifest.head)
            || envelope.manifest.source_checkpoint.next_event_sequence()
                != envelope.manifest.checkpoint.next_event_sequence()
            || source
                .context()
                .chronology()
                .first()
                .map(|event| event.sequence())
                != candidate
                    .context()
                    .chronology()
                    .first()
                    .map(|event| event.sequence())
        {
            return Err(invalid(
                "immutable source receipt does not qualify the proposed rewrite",
            ));
        }
        let selected_groups = source_raw[..count]
            .iter()
            .filter_map(|event| event.execution_group_id())
            .collect::<BTreeSet<_>>();
        if source_raw[..count].iter().any(|event| {
            event.retention() == mez_agent::ContextRetention::Exact
                || !event.recoverable_for_compaction()
                || source.context().peer_message_is_pending(event.sequence())
                || event
                    .execution_group_id()
                    .is_some_and(|group| source.context().execution_group_is_pending(group))
        }) || source_raw[count..]
            .iter()
            .filter_map(|event| event.execution_group_id())
            .any(|group| selected_groups.contains(group))
            || !source.context().chronology().iter().any(|event| {
                Some(event.sequence().get()) == envelope.manifest.frozen_arrival_frontier
            })
            || source_raw[count - 1].sequence().get()
                > envelope.manifest.frozen_arrival_frontier.unwrap_or(0)
        {
            return Err(invalid(
                "immutable source receipt has protected or unfrozen covered members",
            ));
        }
        Ok(candidate)
    }
}

impl AgentTranscriptStore {
    /// Freezes one proposal with actual inherited unsettled backing before any
    /// uncertain write. Parent lookup shares the existing conversation lease;
    /// this preparation belongs on the storage worker. A first no-archive source
    /// remains pure. Old v4 backing needs an explicit transition, never guessed
    /// missing originals or provider work during resume.
    pub(crate) fn prepare_replay_epoch(
        &self,
        origin: ReplaySourceOrigin,
        conversation_id: &str,
        expected_parent: Option<ReplayEpochIdentity>,
        source: &ReplayEpochContext,
        plan: ReplayCompactionPlan,
        summary: ReplayHeadPublication,
    ) -> Result<ReplayEpochPublication> {
        let _lease = expected_parent
            .as_ref()
            .map(|_| self.acquire_conversation_lock(conversation_id))
            .transpose()?;
        let current = if expected_parent.is_some() {
            self.read_replay_envelope_locked(conversation_id)?
        } else {
            None
        };
        if current.as_ref().map(Envelope::identity) != expected_parent {
            return Err(MezError::conflict("replay preparation parent changed"));
        }
        let inherited =
            match &current {
                Some(current) => {
                    current.verify_receipt(&self.root.join(conversation_id))?;
                    current.manifest.source_escrow.as_deref().ok_or_else(|| invalid(
                    "version-four replay requires explicit original-source escrow migration",
                ))?
                }
                None => &[],
            };
        ReplayEpochPublication::prepare(
            conversation_id,
            expected_parent,
            source,
            plan,
            summary,
            &origin,
            inherited,
        )
    }

    /// Identifies the one-head format under the caller's existing mutation
    /// lease. This probe neither resolves source nor grants adoption; the typed
    /// loader still validates the complete authority and required backing.
    pub(super) fn has_replay_manifest_locked(&self, conversation_id: &str) -> Result<bool> {
        mez_agent::transcript::validate_conversation_id(conversation_id)?;
        let bytes = match bounded_read(
            &self.root.join(conversation_id).join(MANIFEST_NAME),
            MANIFEST_LIMIT,
        ) {
            Ok(bytes) => bytes,
            Err(error) if error.io_kind() == Some(std::io::ErrorKind::NotFound) => {
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        #[derive(Deserialize)]
        struct Probe {
            manifest: Option<serde::de::IgnoredAny>,
        }
        let probe: Probe =
            serde_json::from_slice(&bytes).map_err(|_| invalid("invalid epoch authority"))?;
        Ok(probe.manifest.is_some())
    }

    /// Captures a checked current checkpoint without copying saved instructions.
    /// Only called with the existing conversation lease; source resolution and
    /// marker recovery use that same lease and never recursively acquire flock.
    pub(super) fn replay_epoch_snapshot_locked(
        &self,
        conversation_id: &str,
    ) -> Result<Option<ReplayEpochSnapshot>> {
        if !self.has_replay_manifest_locked(conversation_id)? {
            return Ok(None);
        }
        let envelope = self
            .read_replay_envelope_locked(conversation_id)?
            .ok_or_else(|| invalid("required replay authority disappeared"))?;
        let directory = self.root.join(conversation_id);
        envelope.verify_receipt(&directory)?;
        envelope.verify_escrow_payloads(&directory)?;
        let owner = envelope.restore(AgentContext::empty(), |reference| {
            payload::read(&directory, &reference)
        })?;
        let checkpoint = owner
            .checkpoint()
            .map_err(context_error)?
            .with_stable_records(envelope.manifest.checkpoint.stable_records().to_vec())
            .map_err(context_error)?;
        self.require_compaction_epoch_locked(conversation_id)?;
        sync_directory(&directory)?;
        Ok(Some(ReplayEpochSnapshot {
            identity: envelope.identity(),
            checkpoint,
            escrow: envelope.manifest.source_escrow.clone(),
            qualified_rows: BTreeSet::new(),
        }))
    }

    /// Qualifies late original rows without selecting their covered source for
    /// replay. The caller already owns the conversation lease and checked row
    /// view. Unknown semantic arrivals still require the following runtime
    /// projection gate; this method grants only exact-copy deduplication.
    pub(super) fn qualify_replay_source_copies_locked(
        &self,
        conversation_id: &str,
        snapshot: &mut ReplayEpochSnapshot,
        rows: &[mez_agent::TranscriptEntry],
    ) -> Result<()> {
        let Some(escrow) = &snapshot.escrow else {
            if !rows.is_empty() {
                return Err(MezError::conflict(
                    "version-four replay rows lack original source escrow",
                ));
            }
            return Ok(());
        };
        snapshot.qualified_rows =
            escrow::qualify_rows(escrow, conversation_id, rows, |reference| {
                payload::read(&self.root.join(conversation_id), reference)
            })?;
        Ok(())
    }

    /// Publishes a retained proposal synchronously on a persistence worker. All
    /// helpers share this single held conversation lease. Retry settles only an
    /// exact already-committed identity; competing parents or equal-text new
    /// proposals conflict. Errors after swap may mean committed: retain proposal.
    pub(crate) fn publish_replay_epoch(
        &self,
        proposal: &ReplayEpochPublication,
    ) -> Result<ReplayEpochCommit> {
        let _lease = self.acquire_conversation_lock(&proposal.envelope.manifest.conversation_id)?;
        let identity = self.publish_replay_epoch_locked(proposal, &_lease, |_| Ok(()))?;
        Ok(ReplayEpochCommit {
            identity,
            root: self.root.clone(),
        })
    }

    /// Loads only current referenced source, checking required-state, integrity
    /// and canonical invariants. The caller supplies current trusted policy and
    /// request metadata. Missing/corrupt required source refuses this operation.
    pub(crate) fn load_replay_epoch(
        &self,
        conversation_id: &str,
        prefix: AgentContext,
    ) -> Result<Option<(ReplayEpochIdentity, ReplayEpochContext)>> {
        let _lease = self.acquire_conversation_lock(conversation_id)?;
        let Some(envelope) = self.read_replay_envelope_locked(conversation_id)? else {
            return Ok(None);
        };
        let directory = self.ensure_session_dir(conversation_id)?;
        envelope.verify_receipt(&directory)?;
        envelope.verify_escrow_payloads(&directory)?;
        let owner = envelope.restore(prefix, |reference| payload::read(&directory, &reference))?;
        // A prior worker may have died after the durable latest swap but before
        // its required-state marker. Complete that same publication's durability
        // before exposing it, so later sidecar loss cannot look like raw absence.
        self.require_compaction_epoch_locked(conversation_id)?;
        sync_directory(&directory)?;
        Ok(Some((envelope.identity(), owner)))
    }

    /// Performs the complete ordered transaction without reacquiring flock.
    /// The hook exposes fault seams to isolated storage tests only.
    fn publish_replay_epoch_locked(
        &self,
        proposal: &ReplayEpochPublication,
        lease: &std::fs::File,
        mut hook: impl FnMut(Phase) -> Result<()>,
    ) -> Result<ReplayEpochIdentity> {
        let manifest = &proposal.envelope.manifest;
        let current = self.read_replay_envelope_locked(&manifest.conversation_id)?;
        let exact_retry = current
            .as_ref()
            .is_some_and(|current| current == &proposal.envelope);
        if !exact_retry
            && (current.as_ref().map(Envelope::identity) != manifest.expected_parent
                || current.as_ref().map(|current| current.manifest.head) != manifest.parent_head)
        {
            return Err(MezError::conflict(
                "replay epoch parent changed before publication",
            ));
        }
        ReplayEpochPublication::validate_candidate(
            &proposal.envelope,
            &proposal.payloads,
            AgentContext::empty(),
        )?;
        let recorded_source = proposal
            .prepared
            .source()
            .checkpoint()
            .map_err(context_error)?
            .try_map_payloads(|_, content| Ok::<_, MezError>(Payload::new(content.as_bytes())))?;
        if recorded_source != manifest.source_checkpoint {
            return Err(invalid(
                "controller source does not match immutable source receipt",
            ));
        }
        let directory = self.root.join(&manifest.conversation_id);
        if let Some(current) = current.as_ref().filter(|_| !exact_retry) {
            current.verify_receipt(&directory)?;
            let parent = current.restore(AgentContext::empty(), |reference| {
                payload::read(&directory, &reference)
            })?;
            proposal
                .prepared
                .source()
                .validate_source_extension(&parent)
                .map_err(context_error)?;
            let inherited = current.manifest.source_escrow.as_deref().ok_or_else(|| {
                invalid("version-four replay requires explicit original-source escrow migration")
            })?;
            if inherited != proposal.inherited {
                return Err(invalid(
                    "replay publication changed its captured parent escrow",
                ));
            }
        }
        if manifest.expected_parent.is_none() && !proposal.inherited.is_empty()
            || manifest.source_escrow.as_deref()
                != Some(
                    escrow::extend(
                        &proposal.inherited,
                        proposal.prepared.source(),
                        &proposal.origin,
                    )?
                    .as_slice(),
                )
        {
            return Err(invalid(
                "replay publication has unqualified original-source escrow",
            ));
        }
        for item in manifest.source_escrow.as_deref().unwrap_or_default() {
            if !proposal.payloads.contains_key(&item.payload().sha256) {
                payload::read(&directory, item.payload())?;
            }
        }
        self.recover_accounted_appends_for_replay_locked(&manifest.conversation_id, lease)?;
        let rows = match self.inspect_validated_transcript(&manifest.conversation_id) {
            Ok(rows) => rows,
            Err(error)
                if manifest.expected_parent.is_none()
                    && error.kind() == crate::MezErrorKind::NotFound =>
            {
                Vec::new()
            }
            Err(error) => return Err(error),
        };
        escrow::qualify_rows(
            manifest.source_escrow.as_deref().unwrap_or_default(),
            &manifest.conversation_id,
            &rows,
            |reference| {
                if let Some(content) = proposal.payloads.get(&reference.sha256) {
                    reference.validate(content.as_bytes())?;
                    Ok(content.clone())
                } else {
                    payload::read(&directory, reference)
                }
            },
        )?;
        let directory = self.ensure_session_dir(&manifest.conversation_id)?;
        if self.existing_transcript_path_for(&manifest.conversation_id)?
            != self.transcript_path(&manifest.conversation_id)?
        {
            return Err(invalid("legacy flat archive requires explicit migration"));
        }
        sync_directory(&self.root)?;
        let parent = self.root.parent().unwrap_or(&self.root);
        sync_directory(if parent.as_os_str().is_empty() {
            std::path::Path::new(".")
        } else {
            parent
        })?;
        for (hash, content) in &proposal.payloads {
            payload::publish(&directory, hash, content.as_bytes(), || {
                hook(Phase::BeforePayloadSync)
            })?;
        }
        let mut inherited_payloads = BTreeSet::new();
        for item in manifest.source_escrow.as_deref().unwrap_or_default() {
            let reference = item.payload();
            if !proposal.payloads.contains_key(&reference.sha256)
                && inherited_payloads.insert(&reference.sha256)
            {
                let content = payload::read(&directory, reference)?;
                payload::publish(&directory, &reference.sha256, content.as_bytes(), || {
                    hook(Phase::BeforePayloadSync)
                })?;
            }
        }
        // The immutable complete source/proposal receipt precedes the latest
        // authority swap. It is audit evidence, not an ordinary append ACK or
        // permission to select this proposal when latest points elsewhere.
        let receipt = encode(&proposal.envelope)?;
        payload::publish(&directory, &digest(&receipt), &receipt, || {
            hook(Phase::BeforeSourceReceiptSync)
        })?;
        // An empty ordinary archive is a real zero-row state, never a claim that
        // live source has appended. Referenced source is durable independently.
        let archive = self.transcript_path(&manifest.conversation_id)?;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&archive)
        {
            Ok(file) => file.sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        if !exact_retry {
            let temporary = directory.join(".replay-epoch.tmp");
            let result = (|| {
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&temporary)?;
                file.write_all(&encode(&proposal.envelope)?)?;
                file.sync_all()?;
                hook(Phase::BeforeManifestSwap)?;
                fs::rename(&temporary, directory.join(MANIFEST_NAME))?;
                hook(Phase::AfterManifestSwap)?;
                Ok::<_, MezError>(())
            })();
            if result.is_err() {
                let _ = fs::remove_file(&temporary);
            }
            result?;
        }
        sync_directory(&directory)?;
        hook(Phase::BeforeRequiredMarker)?;
        self.require_compaction_epoch_locked(&manifest.conversation_id)?;
        sync_directory(&directory)?;
        hook(Phase::BeforeAcknowledgment)?;
        Ok(proposal.identity())
    }

    /// Reads the one authority. Legacy projections require explicit migration;
    /// they are never overwritten or treated as an absent expected parent.
    fn read_replay_envelope_locked(&self, conversation_id: &str) -> Result<Option<Envelope>> {
        let path = self.root.join(conversation_id).join(MANIFEST_NAME);
        let bytes = match bounded_read(&path, MANIFEST_LIMIT) {
            Ok(bytes) => bytes,
            Err(error) if error.io_kind() == Some(std::io::ErrorKind::NotFound) => {
                self.compaction_epoch_locked(conversation_id)?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let envelope: Envelope = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("invalid or legacy manifest; explicit migration required"))?;
        envelope.validate(conversation_id)?;
        Ok(Some(envelope))
    }
}

impl Envelope {
    /// Unsettled audit references remain required until actual settlement can
    /// retire them. Validate unique payloads without decoding old model history.
    fn verify_escrow_payloads(&self, directory: &std::path::Path) -> Result<()> {
        let mut checked = BTreeSet::new();
        for item in self.manifest.source_escrow.as_deref().unwrap_or_default() {
            if checked.insert(&item.payload().sha256) {
                payload::read(directory, item.payload())?;
            }
        }
        Ok(())
    }

    /// Checks the exact immutable source/proposal receipt without resolving
    /// covered payloads or walking parent epochs during ordinary model reads.
    fn verify_receipt(&self, directory: &std::path::Path) -> Result<()> {
        let bytes = encode(self)?;
        payload::read(directory, &Payload::new(&bytes))?;
        Ok(())
    }

    /// Verifies complete header integrity and expected-parent revision shape.
    fn validate(&self, conversation_id: &str) -> Result<()> {
        let manifest = &self.manifest;
        if !matches!(
            (manifest.version, manifest.source_escrow.is_some()),
            (4, false) | (5, true)
        ) || manifest.conversation_id != conversation_id
            || manifest.revision == 0
            || manifest.epoch_revision == 0
            || self.sha256 != digest(&encode(manifest)?)
            || manifest
                .expected_parent
                .as_ref()
                .map_or(Some(1), |parent| parent.revision.checked_add(1))
                != Some(manifest.revision)
            || manifest
                .expected_parent
                .as_ref()
                .map_or(Some(1), |parent| parent.epoch_revision.checked_add(1))
                != Some(manifest.epoch_revision)
            || manifest
                .frozen_arrival_frontier
                .is_none_or(|frontier| frontier == 0)
            || manifest.expected_parent.is_some() != manifest.parent_head.is_some()
            || manifest.covered.is_empty()
            || manifest
                .covered
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != manifest.covered.len()
        {
            return Err(invalid("manifest integrity, identity or parent is invalid"));
        }
        if let Some(escrow) = &manifest.source_escrow {
            escrow::validate(escrow)?;
        }
        Ok(())
    }

    /// Returns exact manifest identity, not equality of summary text.
    fn identity(&self) -> ReplayEpochIdentity {
        ReplayEpochIdentity {
            epoch_revision: self.manifest.epoch_revision,
            revision: self.manifest.revision,
            publication: self.manifest.publication,
            sha256: self.sha256.clone(),
        }
    }

    /// Resolves only current members then uses the single canonical adopter.
    fn restore(
        &self,
        prefix: AgentContext,
        mut resolve: impl FnMut(Payload) -> Result<String>,
    ) -> Result<ReplayEpochContext> {
        self.validate(&self.manifest.conversation_id)?;
        let mut bytes = 0usize;
        let checkpoint = self
            .manifest
            .checkpoint
            .clone()
            .try_map_payloads(|_, reference| {
                bytes = bytes
                    .checked_add(reference.bytes)
                    .filter(|n| *n <= SOURCE_LIMIT)
                    .ok_or_else(|| invalid("source exceeds load size limit"))?;
                resolve(reference)
            })?;
        let conversation = ReplayConversationId::new(self.manifest.conversation_id.clone())
            .map_err(context_error)?;
        let owner = ReplayEpochContext::from_checkpoint(conversation, checkpoint, prefix)
            .map_err(context_error)?;
        let covered = self
            .manifest
            .covered
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        if owner.revision().get() != self.manifest.epoch_revision
            || owner.head().map(|head| head.occurrence_id()) != Some(self.manifest.head)
            || covered.contains(&self.manifest.head)
            || owner
                .members()
                .iter()
                .any(|member| covered.contains(&member.occurrence_id()))
            || owner.context().chronology().first().is_none_or(|head| {
                self.manifest
                    .frozen_arrival_frontier
                    .is_none_or(|frontier| frontier < head.sequence().get())
            })
        {
            return Err(invalid(
                "checkpoint does not match manifest head or revision",
            ));
        }
        if let Some(escrow) = &self.manifest.source_escrow {
            escrow::validate_current(escrow, &owner, &self.manifest.covered)?;
        }
        Ok(owner)
    }
}

/// Extracts bounded source while reusing already captured immutable bytes.
/// Both pre-rewrite audit source and current replay retain their own typed
/// framing/IDs; only identical payload bytes share content-addressed backing.
fn externalize(
    checkpoint: ReplayEpochCheckpoint,
    payloads: &mut BTreeMap<String, String>,
) -> Result<ReplayEpochCheckpoint<Payload>> {
    let mut bytes = 0usize;
    checkpoint.try_map_payloads(|_, content| {
        bytes = bytes
            .checked_add(content.len())
            .filter(|n| *n <= SOURCE_LIMIT)
            .ok_or_else(|| invalid("source exceeds publication size limit"))?;
        let reference = Payload::new(content.as_bytes());
        payloads.entry(reference.sha256.clone()).or_insert(content);
        Ok(reference)
    })
}

/// Serializes exactly one strict format, reporting no source content on error.
fn encode(value: &impl Serialize) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|_| invalid("manifest serialization failed"))
}

/// Preserves canonical validation context without logging checkpoint payloads.
fn context_error(error: mez_agent::AgentContextError) -> MezError {
    invalid(&format!("canonical validation failed: {error}"))
}

/// Returns a bounded source-free storage diagnostic.
fn invalid(reason: &str) -> MezError {
    MezError::invalid_state(format!("single-head replay epoch: {reason}"))
}

impl std::fmt::Debug for ReplayEpochPublication {
    /// Keeps model/source bytes out of worker and callback diagnostics.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReplayEpochPublication")
            .field("identity", &self.identity())
            .finish_non_exhaustive()
    }
}
