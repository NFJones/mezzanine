//! Immutable single-head source backing and expected-parent publication.
//!
//! The existing conversation mutation lease serializes this writer with archive,
//! editing and persistence owners. Model publications use version ten of the
//! authoritative compaction sidecar; version eleven validates raw revisions in
//! the same epoch, never a parallel selectable history. Payloads are private,
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
//! Checked authored arrivals retain original producer rows through replay and
//! subsequent publication; source classification never grants append rights.
//! Typed actor publication, manual legacy normalization and NoStore copies share
//! the canonical owner. Legacy archive promotion uses the existing metadata
//! transaction under the publisher's held lease. Historical-only APIs refuse
//! these formats; existing persistence and cleanup owners retain required backing.

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

mod compatibility;
mod copy;
mod initial;
mod prefix;
mod raw_import;
pub(crate) use raw_import::ReplayRawHistorySource;
mod raw_revision;
pub(crate) use initial::{InitialReplayEpoch, ReplayForkSource};
mod memory;
pub(crate) use memory::ReplayMemorySource;
mod cursor;
mod escrow;
mod payload;
pub(crate) use escrow::ReplaySourceOrigin;
/// Qualifies recorded lifecycle metadata from the actual leased raw/typed view.
/// Covered source remains escrow proof only, never selected model membership.
pub(crate) fn qualify_execution_controls(
    snapshot: Option<&ReplayEpochSnapshot>,
    conversation: &str,
    rows: &[mez_agent::TranscriptEntry],
) -> Result<Vec<mez_agent::ReplayExecutionState>> {
    if snapshot.is_some_and(|snapshot| snapshot.conversation_id() != conversation) {
        return Err(invalid("execution control has a foreign replay snapshot"));
    }
    let empty = BTreeMap::new();
    escrow::qualify_controls(
        snapshot
            .and_then(|snapshot| snapshot.escrow.as_deref())
            .unwrap_or_default(),
        conversation,
        rows,
        snapshot
            .map(|snapshot| &snapshot.execution_sources)
            .unwrap_or(&empty),
    )
}
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
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ReplayEpochSnapshot {
    root: std::path::PathBuf,
    conversation_id: String,
    identity: ReplayEpochIdentity,
    /// A planned initial receipt is descriptive source, not a durable ACK.
    /// Preparation must recover that exact receipt and check its real parent.
    pending_initial: bool,
    checkpoint: ReplayEpochCheckpoint,
    escrow: Option<Vec<escrow::EscrowOccurrence>>,
    /// Immutable source-only copy dependencies resolved by the original worker.
    copy_backing: BTreeMap<String, String>,
    qualified_rows: BTreeSet<u64>,
    /// Original new authored rows from this same checked logical view. The
    /// runtime must qualify pending accounting/native source before adoption.
    arrivals: Vec<mez_agent::TranscriptEntry>,
    archive_cut: Option<cursor::ArchiveCut>,
    /// Bounded original execution descriptors checked under this snapshot's
    /// mutation lease and archive cut. They cannot select covered model source.
    execution_sources:
        BTreeMap<ContextBlockOccurrenceId, super::row_origins::IndexedExecutionSource>,
}

impl std::fmt::Debug for ReplayEpochSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplayEpochSnapshot")
            .field("conversation_id", &self.conversation_id)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl ReplayEpochSnapshot {
    /// Original indexed archive frontier, not a count of current model members.
    pub(crate) fn covered_archive_frontier(&self) -> u64 {
        self.archive_cut
            .as_ref()
            .map_or(0, |cut| cut.through_sequence())
    }
    /// Original conversation captured with the parent, independent of row count.
    pub(crate) fn conversation_id(&self) -> &str {
        &self.conversation_id
    }

    /// Identifies a candidate authored arrival from this checked logical view.
    /// Pending accounting and native validation still precede actual adoption.
    pub(crate) fn row_is_authored_arrival(&self, sequence: u64) -> bool {
        self.arrivals
            .binary_search_by_key(&sequence, |row| row.sequence)
            .is_ok()
    }

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
    /// A new conversation's independently backed root, never a summary rewrite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    copy_origin: Option<copy::CopyOrigin>,
    /// Frozen in-memory source; never a committed parent publication identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    memory_copy_origin: Option<copy::MemoryCopyOrigin>,
    /// Original one-prefix authority at an explicit source-preserving migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prefix_origin: Option<prefix::PrefixOrigin>,
    /// A content revision keeps the same canonical epoch and immutable head.
    /// The bounded original receipt proves every change without an edit chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    raw_revision_origin: Option<raw_revision::RawRevisionOrigin>,
    /// Absent only in the old v4 prototype, which has no inherited escrow proof.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_escrow: Option<Vec<escrow::EscrowOccurrence>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archive_cut: Option<cursor::ArchiveCut>,
}

impl Manifest {
    fn is_source_preserving_root(&self) -> bool {
        self.copy_origin.is_some()
            || self.memory_copy_origin.is_some()
            || self.prefix_origin.is_some()
    }
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
    backing: OriginalSourceBacking,
}

/// Original producer and immutable history captured beside the actual source.
/// This is descriptive custody input, not an execution or ordinary append ACK.
pub(crate) struct ReplaySourceCapture<'a> {
    origin: ReplaySourceOrigin,
    history: Option<&'a ReplayEpochSnapshot>,
    raw_rows: &'a [mez_agent::TranscriptEntry],
    legacy_raw: Option<&'a ReplayRawHistorySource>,
}

/// Exact backing inputs retained with the proposal through uncertain I/O.
#[derive(Clone)]
struct OriginalSourceBacking {
    origin: ReplaySourceOrigin,
    /// First-head migration retains the original raw read, not a typed parent.
    legacy_raw: Option<ReplayRawHistorySource>,
    inherited: Vec<escrow::EscrowOccurrence>,
    arrivals: Vec<mez_agent::TranscriptEntry>,
    archive: cursor::ArchiveCapture,
}

impl ReplaySourceOrigin {
    /// Pairs this turn's producer with its original checked history, when any.
    /// Imported arrivals require that history capture so a later caller cannot
    /// substitute its own turn/agent/pane for their original provenance.
    pub(crate) fn capture(self, history: Option<&ReplayEpochSnapshot>) -> ReplaySourceCapture<'_> {
        ReplaySourceCapture {
            origin: self,
            history,
            raw_rows: &[],
            legacy_raw: None,
        }
    }

    /// Captures original accounted raw images before a first head exists.
    /// Preparation verifies their framing against current canonical members and
    /// the physical archive under the existing mutation lease. This does not
    /// settle pending ordinary appends or grant execution completion.
    pub(crate) fn capture_raw(
        self,
        rows: &[mez_agent::TranscriptEntry],
    ) -> ReplaySourceCapture<'_> {
        ReplaySourceCapture {
            origin: self,
            history: None,
            raw_rows: rows,
            legacy_raw: None,
        }
    }

    /// Carries one original checked raw projection into the sole head writer.
    pub(crate) fn capture_legacy_raw(
        self,
        raw: &ReplayRawHistorySource,
    ) -> ReplaySourceCapture<'_> {
        ReplaySourceCapture {
            origin: self,
            history: None,
            raw_rows: &[],
            legacy_raw: Some(raw),
        }
    }
}

/// Nonserializable acknowledgment minted after the original store's complete
/// durability barrier. It names one exact proposal and archive; descriptive
/// loaded identities cannot stand in for this live adoption receipt.
#[derive(Debug, Clone)]
pub(crate) struct ReplayEpochCommit {
    identity: ReplayEpochIdentity,
    root: std::path::PathBuf,
    /// Checked physical position observed under this publication's held lease.
    position: mez_agent::AgentTranscriptPosition,
}

impl ReplayEpochCommit {
    /// Reports audit-row position, separately from retained model membership.
    /// This does not settle any ordinary append or select model source.
    pub(crate) fn position(&self) -> mez_agent::AgentTranscriptPosition {
        self.position
    }

    /// Captures the exact acknowledged base for later source qualification.
    /// Uses only this receipt's immutable proposal; no latest-parent reread or
    /// cold-owner replacement can retarget the live actor after the write.
    pub(crate) fn snapshot(
        &self,
        store: &AgentTranscriptStore,
        proposal: &ReplayEpochPublication,
    ) -> Result<ReplayEpochSnapshot> {
        if self.root != store.root || self.identity != proposal.identity() {
            return Err(invalid(
                "replay snapshot does not match acknowledged proposal",
            ));
        }
        Ok(ReplayEpochSnapshot {
            root: self.root.clone(),
            conversation_id: proposal.envelope.manifest.conversation_id.clone(),
            identity: self.identity.clone(),
            pending_initial: false,
            checkpoint: proposal
                .prepared
                .candidate()
                .checkpoint()
                .map_err(context_error)?,
            escrow: proposal.envelope.manifest.source_escrow.clone(),
            copy_backing: escrow::copy_backing(
                proposal
                    .envelope
                    .manifest
                    .source_escrow
                    .as_deref()
                    .unwrap_or_default(),
                proposal.prepared.candidate(),
                |reference| {
                    proposal
                        .payloads
                        .get(&reference.sha256)
                        .cloned()
                        .map(Ok)
                        .unwrap_or_else(|| {
                            payload::read(
                                &self.root.join(&proposal.envelope.manifest.conversation_id),
                                reference,
                            )
                        })
                },
            )?,
            qualified_rows: BTreeSet::new(),
            arrivals: Vec::new(),
            archive_cut: proposal.envelope.manifest.archive_cut.clone(),
            execution_sources: BTreeMap::new(),
        })
    }

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
pub(crate) enum Phase {
    BeforePayloadSync,
    BeforeSourceReceiptSync,
    BeforeManifestSwap,
    AfterManifestSwap,
    BeforeRequiredMarker,
    BeforeAcknowledgment,
    BeforeRawEditIntent,
    AfterRawEditIntent,
    AfterRawEditArchive,
    AfterRawEditIndex,
    AfterRawEditManifest,
    BeforeRawEditAcknowledgment,
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
        backing: OriginalSourceBacking,
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
            version: 10,
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
            copy_origin: None,
            memory_copy_origin: None,
            prefix_origin: None,
            raw_revision_origin: None,
            source_escrow: Some(escrow::publication_source(
                &backing.inherited,
                source,
                &backing.origin,
                &backing.arrivals,
                &backing.archive.archived,
            )?),
            archive_cut: Some(backing.archive.cut.clone()),
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
            backing,
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
        if envelope.manifest.is_source_preserving_root() {
            // Header validation binds both checkpoints to the same frozen
            // copied projection. A copy performs no semantic replacement.
            return Ok(candidate);
        }
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
    /// remains pure. Imported arrivals must supply their original history
    /// capture; a later live producer cannot replace their provenance. A foreign
    /// store/parent capture or changed source refuses before writing anything.
    /// Old v4 backing needs an explicit transition, never guessed
    /// missing originals or provider work during resume.
    pub(crate) fn prepare_replay_epoch(
        &self,
        capture: ReplaySourceCapture<'_>,
        conversation_id: &str,
        expected_parent: Option<ReplayEpochIdentity>,
        source: &ReplayEpochContext,
        plan: ReplayCompactionPlan,
        summary: ReplayHeadPublication,
    ) -> Result<ReplayEpochPublication> {
        let has_archive = self.existing_transcript_path_for(conversation_id)?.exists();
        let has_existing_state = has_archive || self.root.join(conversation_id).exists();
        let lease = (expected_parent.is_some() || has_existing_state)
            .then(|| self.acquire_conversation_lock(conversation_id))
            .transpose()?;
        if let Some(lease) = &lease {
            self.recover_accounted_appends_for_replay_locked(conversation_id, lease)?;
        }
        let current = if expected_parent.is_some() {
            self.read_replay_envelope_locked(conversation_id)?
        } else {
            None
        };
        if current.as_ref().map(Envelope::identity) != expected_parent {
            return Err(MezError::conflict(
                if capture
                    .history
                    .is_some_and(|history| history.pending_initial)
                {
                    "initial replay receipt has not established its planned parent"
                } else {
                    "replay preparation parent changed"
                },
            ));
        }
        if capture.history.is_some_and(|capture| {
            capture.root != self.root || Some(&capture.identity) != expected_parent.as_ref()
        }) {
            return Err(MezError::conflict(
                "replay arrival capture has a different original store or parent",
            ));
        }
        let legacy = if let Some(raw) = capture.legacy_raw {
            if expected_parent.is_some()
                || capture.history.is_some()
                || !capture.raw_rows.is_empty()
            {
                return Err(MezError::conflict(
                    "raw normalization has another publication base",
                ));
            }
            Some(raw.prepare(self, source)?)
        } else {
            None
        };
        let inherited =
            match &current {
                Some(current) => {
                    current.verify_receipt(&self.root.join(conversation_id))?;
                    current.manifest.source_escrow.as_deref().ok_or_else(|| invalid(
                    "version-four replay requires explicit original-source escrow migration",
                ))?
                }
                None => legacy
                    .as_ref()
                    .map_or(&[][..], |(items, _)| items.as_slice()),
            };
        if !capture.raw_rows.is_empty()
            && (expected_parent.is_some()
                || capture
                    .raw_rows
                    .iter()
                    .any(|row| row.conversation_id != conversation_id))
        {
            return Err(MezError::conflict(
                "raw replay capture has a different conversation or parent",
            ));
        }
        let arrivals = capture.history.map_or_else(
            || capture.raw_rows.to_vec(),
            |capture| capture.arrivals.clone(),
        );
        let source_backing =
            escrow::qualification_source(inherited, source, &capture.origin, &arrivals)?;
        let mut archive = cursor::ArchiveCapture::prepare(
            self,
            conversation_id,
            current
                .as_ref()
                .and_then(|current| current.manifest.archive_cut.as_ref())
                .or_else(|| legacy.as_ref().map(|(_, cut)| cut)),
            &source_backing,
            source,
            &plan
                .summary_input()
                .iter()
                .map(|event| event.record().occurrence_id())
                .collect::<Vec<_>>(),
            expected_parent.is_none() && !has_archive,
        )?;
        cursor::ArchiveCapture::validate_raw_capture(&archive.rows, &arrivals)?;
        if legacy.is_some() {
            // This is checked raw provenance for the first head, not a parent
            // manifest. Later physical rows were qualified by prepare above.
            archive.previous = None;
        }
        ReplayEpochPublication::prepare(
            conversation_id,
            expected_parent,
            source,
            plan,
            summary,
            OriginalSourceBacking {
                origin: capture.origin,
                legacy_raw: capture.legacy_raw.cloned(),
                inherited: inherited.to_vec(),
                arrivals,
                archive,
            },
        )
    }

    /// Read-only bounded format probe, without source resolution or adoption.
    /// Canonical consumers call it under their existing mutation lease; public
    /// discovery may probe an atomic header without acquiring a recursive lock.
    pub(super) fn has_replay_manifest(&self, conversation_id: &str) -> Result<bool> {
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

    /// Qualifies typed discovery for an actually empty physical archive. This
    /// reads only the atomic header and its exact immutable receipt; it neither
    /// adopts history nor certifies replay payload/cut readiness. The canonical
    /// loader still checks those independently. No mutation or flock acquisition
    /// occurs, including when presentation/catalog upkeep already holds it.
    pub(super) fn has_empty_replay_discovery(&self, conversation_id: &str) -> Result<bool> {
        if !self.has_replay_manifest(conversation_id)? {
            return Ok(false);
        }
        let envelope = self
            .read_replay_envelope_locked(conversation_id)?
            .ok_or_else(|| invalid("typed discovery authority disappeared"))?;
        envelope.verify_receipt(&self.root.join(conversation_id))?;
        if envelope
            .manifest
            .archive_cut
            .as_ref()
            .is_some_and(|cut| cut.through_sequence() > 0)
        {
            return Err(invalid("empty archive lost its committed indexed prefix"));
        }
        Ok(true)
    }

    /// Captures a checked current checkpoint without copying saved instructions.
    /// Only called with the existing conversation lease; source resolution and
    /// marker recovery use that same lease and never recursively acquire flock.
    pub(super) fn replay_epoch_snapshot_locked(
        &self,
        conversation_id: &str,
    ) -> Result<Option<ReplayEpochSnapshot>> {
        if !self.has_replay_manifest(conversation_id)? {
            return Ok(None);
        }
        let envelope = self
            .read_replay_envelope_locked(conversation_id)?
            .ok_or_else(|| invalid("required replay authority disappeared"))?;
        let directory = self.root.join(conversation_id);
        envelope.verify_receipt(&directory)?;
        let owner = envelope.restore(AgentContext::empty(), |reference| {
            payload::read(&directory, &reference)
        })?;
        envelope.verify_escrow_payloads(&directory)?;
        if let Some(cut) = &envelope.manifest.archive_cut {
            cut.verify_archived(
                self,
                conversation_id,
                envelope
                    .manifest
                    .source_escrow
                    .as_deref()
                    .unwrap_or_default(),
            )?;
        }
        let checkpoint = owner
            .checkpoint()
            .map_err(context_error)?
            .with_stable_records(envelope.manifest.checkpoint.stable_records().to_vec())
            .map_err(context_error)?;
        self.require_compaction_epoch_locked(conversation_id)?;
        sync_directory(&directory)?;
        Ok(Some(ReplayEpochSnapshot {
            root: self.root.clone(),
            conversation_id: conversation_id.to_owned(),
            identity: envelope.identity(),
            pending_initial: false,
            checkpoint,
            escrow: envelope.manifest.source_escrow.clone(),
            copy_backing: escrow::copy_backing(
                envelope
                    .manifest
                    .source_escrow
                    .as_deref()
                    .unwrap_or_default(),
                &owner,
                |reference| payload::read(&directory, reference),
            )?,
            qualified_rows: BTreeSet::new(),
            arrivals: Vec::new(),
            archive_cut: envelope.manifest.archive_cut.clone(),
            execution_sources: BTreeMap::new(),
        }))
    }

    /// Qualifies late original rows without selecting their covered source for
    /// replay. The caller already owns the conversation lease and checked row
    /// view. New authored rows retain their original images for the following
    /// accounting/combined-source gate; this classification alone cannot adopt
    /// arrivals or acknowledge an ordinary append.
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
        let execution_sources = self.indexed_execution_sources_locked(
            conversation_id,
            rows,
            snapshot.covered_archive_frontier(),
        )?;
        let qualified = escrow::qualify_rows(
            escrow,
            conversation_id,
            rows,
            &execution_sources,
            escrow::RowAdmission::AuthoredArrivals,
            |reference| payload::read(&self.root.join(conversation_id), reference),
        )?;
        snapshot.qualified_rows = qualified.copies;
        snapshot.arrivals = qualified.arrivals;
        snapshot.execution_sources = execution_sources;
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
        self.publish_replay_epoch_with_cancellation(proposal, None)
    }

    /// Checks logical cancellation while holding the actual mutation lease.
    /// Once commit begins, ordinary old-or-new recovery still applies; a queued
    /// cancelled writer cannot change authority after a newer reader passes it.
    pub(crate) fn publish_replay_epoch_unless_cancelled(
        &self,
        proposal: &ReplayEpochPublication,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<ReplayEpochCommit> {
        self.publish_replay_epoch_with_cancellation(proposal, Some(cancelled))
    }

    fn publish_replay_epoch_with_cancellation(
        &self,
        proposal: &ReplayEpochPublication,
        cancelled: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<ReplayEpochCommit> {
        let _lease = self.acquire_conversation_lock(&proposal.envelope.manifest.conversation_id)?;
        if cancelled.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
            return Err(MezError::conflict(
                "canonical publication cancelled before commit",
            ));
        }
        let identity = self.publish_replay_epoch_locked(proposal, &_lease, |_phase| {
            #[cfg(test)]
            self.apply_replay_epoch_fault(_phase)?;
            Ok(())
        })?;
        // Keep this observation in the same mutation lease as the swap. It is
        // bounded by the frozen cut and cannot substitute a newer head or make
        // an ordinary pending batch durable merely through epoch acceptance.
        let cut = &proposal.backing.archive.cut;
        let rows = self.replay_archive_tail_locked(
            &proposal.envelope.manifest.conversation_id,
            cut,
            false,
        )?;
        let logical_high_water = rows
            .last()
            .map_or(cut.through_sequence(), |row| row.sequence);
        let position = mez_agent::AgentTranscriptPosition {
            logical_high_water,
            active_raw_entries: logical_high_water
                .checked_sub(cut.through_sequence())
                .ok_or_else(|| invalid("publication position precedes its frozen archive cut"))?,
        };
        Ok(ReplayEpochCommit {
            identity,
            root: self.root.clone(),
            position,
        })
    }

    /// Installs a bounded fault shared only by this store's physical clones.
    /// Tests consume it at the real writer boundary, never fabricate an ACK.
    #[cfg(test)]
    pub(crate) fn inject_replay_epoch_fault(&self, phase: Phase, attempts: u8, panic: bool) {
        *self.replay_epoch_fault.lock().unwrap() = Some((phase, attempts, panic));
    }

    #[cfg(test)]
    fn apply_replay_epoch_fault(&self, phase: Phase) -> Result<()> {
        let panic = {
            let mut fault = self.replay_epoch_fault.lock().unwrap();
            match fault.as_mut() {
                Some((selected, remaining, panic)) if *selected == phase && *remaining > 0 => {
                    *remaining -= 1;
                    Some(*panic)
                }
                _ => None,
            }
        };
        match panic {
            Some(true) => panic!("injected replay publication worker panic at {phase:?}"),
            Some(false) => Err(MezError::new(
                crate::MezErrorKind::Io,
                format!("injected replay publication failure at {phase:?}"),
            )),
            None => Ok(()),
        }
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
        let owner = envelope.restore(prefix, |reference| payload::read(&directory, &reference))?;
        envelope.verify_escrow_payloads(&directory)?;
        if let Some(cut) = &envelope.manifest.archive_cut {
            cut.verify_archived(
                self,
                conversation_id,
                envelope
                    .manifest
                    .source_escrow
                    .as_deref()
                    .unwrap_or_default(),
            )?;
        }
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
            if inherited != proposal.backing.inherited {
                return Err(invalid(
                    "replay publication changed its captured parent escrow",
                ));
            }
        }
        if manifest.archive_cut.as_ref() != Some(&proposal.backing.archive.cut)
            || !exact_retry
                && current
                    .as_ref()
                    .and_then(|current| current.manifest.archive_cut.as_ref())
                    != proposal.backing.archive.previous.as_ref()
        {
            return Err(invalid("publication changed its frozen archive cut"));
        }
        let source_backing = escrow::qualification_source(
            &proposal.backing.inherited,
            proposal.prepared.source(),
            &proposal.backing.origin,
            &proposal.backing.arrivals,
        )?;
        proposal.backing.archive.verify(
            self,
            &manifest.conversation_id,
            &source_backing,
            escrow::RowAdmission::Publication {
                source: proposal.prepared.source(),
                covered: &manifest.covered,
            },
            |reference| {
                if let Some(content) = proposal.payloads.get(&reference.sha256) {
                    reference.validate(content.as_bytes())?;
                    Ok(content.clone())
                } else {
                    payload::read(&directory, reference)
                }
            },
            manifest.expected_parent.is_none(),
        )?;
        if let Some(raw) = &proposal.backing.legacy_raw {
            if manifest.expected_parent.is_some() {
                return Err(invalid("raw normalization cannot replace a typed parent"));
            }
            // Exact retry already owns this manifest's immutable source receipt.
            // A first physical publication must independently requalify the
            // original raw producer images under this held mutation lease.
            if !exact_retry {
                let (original, _) = raw.prepare(self, proposal.prepared.source())?;
                if original != proposal.backing.inherited {
                    return Err(invalid("raw normalization changed original source escrow"));
                }
            }
        }
        if manifest.expected_parent.is_none()
            && !proposal.backing.inherited.is_empty()
            && proposal.backing.legacy_raw.is_none()
            || manifest.source_escrow.as_deref()
                != Some(
                    escrow::publication_source(
                        &proposal.backing.inherited,
                        proposal.prepared.source(),
                        &proposal.backing.origin,
                        &proposal.backing.arrivals,
                        &proposal.backing.archive.archived,
                    )?
                    .as_slice(),
                )
        {
            return Err(invalid(
                "replay publication has unqualified original-source escrow",
            ));
        }
        for item in manifest
            .source_escrow
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|item| !item.is_archived())
        {
            if !proposal.payloads.contains_key(&item.payload().sha256) {
                payload::read(&directory, item.payload())?;
            }
        }
        self.recover_accounted_appends_for_replay_locked(&manifest.conversation_id, lease)?;
        let rows = match self.replay_archive_tail_locked(
            &manifest.conversation_id,
            &proposal.backing.archive.cut,
            manifest.expected_parent.is_none(),
        ) {
            Ok(rows) => rows,
            Err(error)
                if manifest.expected_parent.is_none()
                    && error.kind() == crate::MezErrorKind::NotFound =>
            {
                Vec::new()
            }
            Err(error) => return Err(error),
        };
        cursor::ArchiveCapture::validate_raw_capture(&rows, &proposal.backing.arrivals)?;
        let execution_sources = self.indexed_execution_sources_locked(
            &manifest.conversation_id,
            &rows,
            proposal.backing.archive.cut.through_sequence(),
        )?;
        escrow::qualify_rows(
            manifest.source_escrow.as_deref().unwrap_or_default(),
            &manifest.conversation_id,
            &rows,
            &execution_sources,
            escrow::RowAdmission::Publication {
                source: proposal.prepared.source(),
                covered: &manifest.covered,
            },
            |reference| {
                if let Some(content) = proposal.payloads.get(&reference.sha256) {
                    reference.validate(content.as_bytes())?;
                    Ok(content.clone())
                } else {
                    payload::read(&directory, reference)
                }
            },
        )?;
        self.migrate_replay_archive_layout_locked(&manifest.conversation_id, lease)?;
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
        for item in manifest
            .source_escrow
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|item| !item.is_archived())
        {
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
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                fs::File::open(&archive)?.sync_all()?;
            }
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
        let items = self.manifest.source_escrow.as_deref().unwrap_or_default();
        let mut current = BTreeSet::new();
        self.manifest
            .checkpoint
            .clone()
            .try_map_payloads(|id, payload| {
                if id != self.manifest.head {
                    current.insert(id);
                }
                Ok::<_, MezError>(payload)
            })?;
        let dependencies = if items.is_empty() {
            Vec::new()
        } else {
            escrow::copy_selection(items, &current)?
        };
        let required = dependencies
            .iter()
            .map(|item| item.occurrence_id())
            .collect::<BTreeSet<_>>();
        for item in items
            .iter()
            .filter(|item| !item.is_archived() || required.contains(&item.occurrence_id()))
        {
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
        if let Some(origin) = &manifest.copy_origin {
            origin.validate(manifest)?;
        }
        if let Some(origin) = &manifest.memory_copy_origin {
            origin.validate(manifest)?;
        }
        if let Some(origin) = &manifest.prefix_origin {
            origin.validate(manifest)?;
        }
        if let Some(origin) = &manifest.raw_revision_origin {
            origin.validate(manifest)?;
        }
        let initial_epoch = manifest
            .copy_origin
            .as_ref()
            .map(copy::CopyOrigin::epoch_revision)
            .or_else(|| {
                manifest
                    .memory_copy_origin
                    .as_ref()
                    .map(|origin| origin.epoch_revision)
            })
            .unwrap_or(1);
        if !matches!(
            (
                manifest.version,
                manifest.source_escrow.is_some(),
                manifest.archive_cut.is_some()
            ),
            (4, false, false) | (5, true, false) | (6..=11, true, true)
        ) || manifest.conversation_id != conversation_id
            || (manifest.version == 11) != manifest.raw_revision_origin.is_some()
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
                .map_or(Some(initial_epoch), |parent| {
                    if manifest.raw_revision_origin.is_some() {
                        Some(parent.epoch_revision)
                    } else {
                        parent.epoch_revision.checked_add(1)
                    }
                })
                != Some(manifest.epoch_revision)
            || manifest
                .frozen_arrival_frontier
                .is_none_or(|frontier| frontier == 0)
            || manifest.expected_parent.is_some() != manifest.parent_head.is_some()
            || (manifest.covered.is_empty()
                && !manifest.is_source_preserving_root()
                && manifest.raw_revision_origin.is_none())
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
            if manifest.version < 7
                && escrow
                    .iter()
                    .any(escrow::EscrowOccurrence::has_task_admission)
            {
                return Err(invalid(
                    "task boundary requires version-seven source backing",
                ));
            }
            if manifest.version < 6 && escrow.iter().any(escrow::EscrowOccurrence::is_archived) {
                return Err(invalid(
                    "legacy source escrow cannot claim archive settlement",
                ));
            }
        }
        if let Some(cut) = &manifest.archive_cut {
            cut.validate()?;
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
        if let Some(origin) = &self.manifest.prefix_origin {
            let original = resolve(origin.authority.clone())?;
            origin.authority.validate(original.as_bytes())?;
            origin.validate_owner(&owner, &original)?;
        }
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
        if let Some(origin) = &self.manifest.raw_revision_origin {
            origin.verify(&self.manifest, &owner, &mut resolve)?;
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
