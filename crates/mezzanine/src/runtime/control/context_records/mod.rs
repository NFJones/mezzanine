//! Structural accounting transport from one actual transcript projection.
//!
//! Indices belong to the exact emitted block order, not a label/text search.
//! Legacy rows establish an explicit archive-position import origin; accounted
//! records preserve their committed origin and cached estimates. Neither path
//! changes source roles or restores instructions. Malformed reserved records
//! retain an error through preparation so partial source cannot reach a model.
//! Repaired authored costs use the same occurrence-bound row projection as
//! legacy costs; stale raw envelopes remain immutable source, not a reason to
//! repeat estimation after each cold replay.
//!
//! Frozen one-head checkpoints use their typed chronology as source throughout
//! composition. The flat projection is checked for exact preservation around a
//! freshly assembled stable prefix and task suffix. Only explicit user admission
//! supersedes prior task instructions; peer admission preserves their protection.
//! Newly supplied execution metadata is confined to the new suffix. This adapter
//! returns model context carrying its actual in-memory replay continuation;
//! this is neither a durable publication nor a transition receipt from storage.

use crate::error::{MezError, Result};
use mez_agent::transcript::{TranscriptContextEvent, TranscriptContextRecord, TranscriptEntry};
use mez_agent::{
    AgentContext, ContextBlock, ContextBlockOccurrenceId, ContextBlockRecord,
    StableContextBlockRecord,
};

mod bootstrap;
mod execution_controls;
mod raw_history;
mod replay_arrivals;

/// Non-model-visible records for the exact projected history prefix.
#[derive(Debug, Clone, Default)]
pub(in crate::runtime) struct RuntimeContextRecords {
    records: Vec<(usize, ContextBlockRecord)>,
    stable: Option<Vec<StableContextBlockRecord>>,
    /// Physical precedence of the original carrier, independent of block order.
    stable_sequence: u64,
    invalid: bool,
    /// Content-free rejection from original pending or stable source qualification.
    source_rejection: Option<(crate::MezErrorKind, String)>,
    row_origins: crate::storage::transcript::TranscriptRowOrigins,
    row_cache: crate::storage::transcript::TranscriptRowRecords,
    rendered_rows: crate::storage::transcript::TranscriptRowRecords,
    /// Original typed reservations address occurrences, never row positions in
    /// a different projection or labels that may repeat across producers.
    source_sequences:
        std::collections::BTreeMap<ContextBlockOccurrenceId, mez_agent::ContextEventSequence>,
    /// Explicit recorded task roots and their first original suffix occurrences.
    task_admissions: std::collections::BTreeMap<ContextBlockOccurrenceId, ContextBlockOccurrenceId>,
    /// Actual zero-model control rows need independent custody before replay.
    control_sequences: std::collections::BTreeSet<u64>,
    execution_controls: Vec<mez_agent::ReplayExecutionState>,
    controls_qualified: bool,
    /// Frozen typed history; never rebuilt from its read-only flat projection.
    replay: Option<ReplayHistory>,
    #[cfg(test)]
    estimate_recomputations: usize,
}

/// Immutable transport for one checked epoch. Current instructions, input and
/// request metadata are composed by the actual turn-admission owner separately.
#[derive(Debug, Clone)]
struct ReplayHistory {
    conversation: mez_agent::ReplayConversationId,
    /// An actual durable source when present. Pure NoStore checkpoints must not
    /// fabricate a committed identity merely to reuse canonical composition.
    snapshot: Option<std::sync::Arc<crate::storage::transcript::replay_epoch::ReplayEpochSnapshot>>,
    /// Original head/tail plus checked new arrivals and actual in-memory
    /// lifecycle witnesses. The committed parent remains independent for CAS.
    body: AgentContext,
    stable_records: Vec<StableContextBlockRecord>,
    blocks: usize,
}

/// One semantic row decoded exactly once before execution-group validation.
pub(super) struct DecodedContextRow {
    pub(super) event: Option<TranscriptContextEvent>,
    pub(super) record: Option<ContextBlockRecord>,
    pub(super) stable: Option<Vec<StableContextBlockRecord>>,
}

impl RuntimeContextRecords {
    /// Validates a frozen one-head epoch and exposes its exact read-only flat
    /// projection to existing prompt composition. An owned canonical body keeps
    /// source sequences and any later qualified lifecycle transitions intact.
    pub(super) fn from_replay_snapshot(
        conversation_id: &str,
        snapshot: crate::storage::transcript::replay_epoch::ReplayEpochSnapshot,
    ) -> Result<(Self, Vec<ContextBlock>)> {
        let conversation = mez_agent::ReplayConversationId::new(conversation_id)?;
        Self::from_replay_checkpoint(
            conversation,
            snapshot.checkpoint().clone(),
            Some(std::sync::Arc::new(snapshot)),
        )
    }

    /// Composes a checked pure checkpoint with optional original durable source.
    /// Descriptive NoStore state remains distinct from a publication receipt.
    pub(super) fn from_replay_checkpoint(
        conversation: mez_agent::ReplayConversationId,
        checkpoint: mez_agent::ReplayEpochCheckpoint,
        snapshot: Option<
            std::sync::Arc<crate::storage::transcript::replay_epoch::ReplayEpochSnapshot>,
        >,
    ) -> Result<(Self, Vec<ContextBlock>)> {
        let stable_records = checkpoint.stable_records().to_vec();
        let owner = mez_agent::ReplayEpochContext::from_checkpoint(
            conversation.clone(),
            checkpoint,
            AgentContext::empty(),
        )?;
        let (mut records, blocks) = Self::from_replay_owner(conversation, owner, snapshot)?;
        records.replay.as_mut().ok_or_else(invalid)?.stable_records = stable_records;
        Ok((records, blocks))
    }

    /// Preserves actual in-memory lifecycle witnesses through history transport.
    /// Only an explicit source copy restores a fresh descriptive incarnation.
    pub(super) fn from_replay_owner(
        conversation: mez_agent::ReplayConversationId,
        mut owner: mez_agent::ReplayEpochContext,
        snapshot: Option<
            std::sync::Arc<crate::storage::transcript::replay_epoch::ReplayEpochSnapshot>,
        >,
    ) -> Result<(Self, Vec<ContextBlock>)> {
        let stable_records = owner.context().stable_block_records();
        owner.replace_stable_slots(Vec::new())?;
        let blocks = owner.context().blocks().to_vec();
        let records = blocks
            .iter()
            .enumerate()
            .zip(owner.context().chronology())
            .map(|((index, _), event)| (index, event.record()))
            .collect();
        Ok((
            Self {
                records,
                replay: Some(ReplayHistory {
                    conversation,
                    body: owner.into_owned_context(),
                    stable_records,
                    snapshot,
                    blocks: blocks.len(),
                }),
                ..Self::default()
            },
            blocks,
        ))
    }

    /// Appends only new accounted rows to the original typed base. The renderer
    /// has already excluded qualified copies; canonical import validates native
    /// identities against that unchanged base and preserves explicit sequences.
    /// Only explicit accounted task admissions replay instruction supersession;
    /// ordinary source grants no completion or transport/append receipt.
    pub(super) fn append_replay_arrivals(
        &mut self,
        blocks: Vec<ContextBlock>,
        arrivals: &Self,
        execution: &[mez_agent::ImportedExecutionEvent],
    ) -> Result<Vec<ContextBlock>> {
        arrivals.ensure_valid()?;
        let replay = self.replay.as_ref().ok_or_else(invalid)?;
        let owner = mez_agent::ReplayEpochContext::from_owned_context(
            replay.conversation.clone(),
            replay.body.clone(),
        )?;
        let owner = replay_arrivals::append(
            owner,
            replay.conversation.clone(),
            blocks,
            arrivals,
            execution,
        )?;
        let blocks = owner.context().blocks().to_vec();
        self.records = owner
            .context()
            .chronology()
            .iter()
            .enumerate()
            .map(|(index, event)| (index, event.record()))
            .collect();
        self.stable = arrivals.stable.clone();
        self.stable_sequence = arrivals.stable_sequence;
        self.row_origins = arrivals.row_origins.clone();
        self.row_cache = arrivals.row_cache.clone();
        self.rendered_rows = arrivals.rendered_rows.clone();
        self.source_sequences = arrivals.source_sequences.clone();
        let replay = self.replay.as_mut().ok_or_else(invalid)?;
        replay.body = owner.into_owned_context();
        replay.blocks = blocks.len();
        Ok(blocks)
    }

    /// Shares the original coherent identity/source with frozen history owners.
    pub(super) fn replay_snapshot(
        &self,
    ) -> Option<std::sync::Arc<crate::storage::transcript::replay_epoch::ReplayEpochSnapshot>> {
        self.replay
            .as_ref()
            .and_then(|history| history.snapshot.clone())
    }

    pub(super) fn replay_conversation(&self) -> Option<&mez_agent::ReplayConversationId> {
        self.replay.as_ref().map(|history| &history.conversation)
    }

    /// Rebinds only an explicitly copied immutable source. Original checkpoint
    /// validation and restrictive retrieval retirement precede any new admission.
    pub(super) fn rebind_replay_copy(
        &mut self,
        target: mez_agent::ReplayConversationId,
    ) -> Result<()> {
        self.ensure_valid()?;
        let replay = self
            .replay
            .as_mut()
            .ok_or_else(|| MezError::conflict("source copy has no original replay checkpoint"))?;
        let owner = mez_agent::ReplayEpochContext::from_owned_context(
            replay.conversation.clone(),
            replay.body.clone(),
        )?;
        let checkpoint = owner.checkpoint()?.for_conversation_copy(target.clone())?;
        replay.body = mez_agent::ReplayEpochContext::from_checkpoint(
            target.clone(),
            checkpoint,
            AgentContext::empty(),
        )?
        .into_owned_context();
        replay.conversation = target;
        Ok(())
    }

    /// Captures original placement and descriptive costs before rendering any
    /// blocks. Structural row positions select caches; actual rendering restores
    /// them before use, so no temporary estimated records are constructed.
    pub(super) fn for_rows(
        accounting: Option<&crate::storage::transcript::ConversationReplaySnapshot>,
        pending: Option<&crate::runtime::persistence::RuntimePendingTranscriptContext>,
    ) -> Self {
        if let (Some(snapshot), Some(pending)) = (accounting, pending) {
            return match pending.projected_accounting(snapshot) {
                Ok((row_origins, row_cache)) => Self {
                    row_origins,
                    row_cache,
                    ..Self::default()
                },
                Err(error) => Self {
                    invalid: true,
                    source_rejection: Some((error.kind(), error.message().to_owned())),
                    ..Self::default()
                },
            }
            .with_stable_snapshot(snapshot.stable_records.as_ref());
        }
        accounting
            .map_or_else(Self::default, |snapshot| Self {
                row_origins: snapshot.row_origins.clone(),
                row_cache: snapshot.row_records.clone(),
                ..Self::default()
            })
            .with_stable_snapshot(accounting.and_then(|snapshot| snapshot.stable_records.as_ref()))
    }

    /// Seeds the same checked archive scope, including covered carriers. Unknown
    /// reserved provenance withholds adoption instead of borrowing older IDs.
    fn with_stable_snapshot(
        mut self,
        snapshot: Option<&crate::storage::transcript::TranscriptStableSnapshot>,
    ) -> Self {
        if let Some(snapshot) = snapshot {
            self.stable_sequence = snapshot.sequence();
            match snapshot.records() {
                Ok(records) => self.stable = Some(records.to_vec()),
                Err(error) => {
                    self.invalid = true;
                    if self.source_rejection.is_none() {
                        self.source_rejection = Some((error.kind(), error.message().to_owned()));
                    }
                }
            }
        }
        self
    }

    /// Complete later snapshots replace earlier ones; an empty list explicitly
    /// retires slots. Selected older rows cannot rewind a checked source capture.
    fn observe_stable(&mut self, sequence: u64, stable: Option<&[StableContextBlockRecord]>) {
        if sequence > self.stable_sequence
            && let Some(stable) = stable
        {
            self.stable_sequence = sequence;
            self.stable = Some(stable.to_vec());
        }
    }

    /// Returns row records needing publication or retaining an existing cache.
    /// The storage owner separately filters committed versus pending membership.
    pub(in crate::runtime) fn rendered_row_records(
        &self,
    ) -> Result<Vec<(u64, ContextBlockRecord)>> {
        self.ensure_valid()?;
        Ok(self
            .rendered_rows
            .iter()
            .map(|(sequence, record)| (*sequence, *record))
            .collect())
    }

    /// Returns only actual committed repairs to the original leased publisher;
    /// proposed pending IDs never become durable cache authority prematurely.
    pub(in crate::runtime) fn committed_row_records(
        &self,
        snapshot: &crate::storage::transcript::ConversationReplaySnapshot,
    ) -> Result<Vec<(u64, ContextBlockRecord)>> {
        let committed = snapshot
            .view
            .committed
            .iter()
            .map(|row| row.sequence)
            .collect::<std::collections::BTreeSet<_>>();
        Ok(self
            .rendered_row_records()?
            .into_iter()
            .filter(|(sequence, _)| committed.contains(sequence))
            .collect())
    }

    /// Original checked placement used by this actual canonical render.
    pub(super) fn source_row_origins(&self) -> &crate::storage::transcript::TranscriptRowOrigins {
        &self.row_origins
    }

    /// Descriptive source caches selected before constructing this projection.
    pub(super) fn source_row_records(&self) -> &crate::storage::transcript::TranscriptRowRecords {
        &self.row_cache
    }

    /// Counts actual missing/stale row estimate constructions in one renderer.
    /// Test-only state does not affect production caching or source authority.
    #[cfg(test)]
    pub(in crate::runtime) fn estimate_recomputations(&self) -> usize {
        self.estimate_recomputations
    }

    /// Associates an actual summary record with its exact emitted block slot.
    pub(super) fn push_indexed_record(&mut self, index: usize, record: ContextBlockRecord) {
        let position = self
            .records
            .partition_point(|(existing, _)| *existing < index);
        if self
            .records
            .get(position)
            .is_some_and(|(existing, _)| *existing == index)
        {
            self.invalid = true;
        } else {
            self.records.insert(position, (index, record));
        }
    }

    /// Withholds adoption when a required typed summary slot is absent.
    pub(super) fn mark_invalid(&mut self) {
        self.invalid = true;
    }

    /// Retains the actual source failure through infallible projection helpers.
    pub(super) fn reject_source(&mut self, error: &MezError) {
        self.invalid = true;
        self.source_rejection = Some((error.kind(), error.message().to_owned()));
    }

    /// Captures malformed reserved envelopes as an explicit pending failure.
    /// Unrelated/legacy rows remain readable and supply no authored cost claim.
    pub(super) fn decode_row(
        &mut self,
        entry: &TranscriptEntry,
        observe_stable: bool,
    ) -> DecodedContextRow {
        if entry.role != mez_agent::transcript::TranscriptRole::System {
            return DecodedContextRow {
                event: None,
                record: None,
                stable: None,
            };
        }
        match mez_agent::transcript::TranscriptExecutionStateRecord::from_transcript_content(
            &entry.content,
        ) {
            Ok(Some(_)) => {
                self.control_sequences.insert(entry.sequence);
                return DecodedContextRow {
                    event: None,
                    record: None,
                    stable: None,
                };
            }
            Err(_) => {
                self.invalid = true;
                return DecodedContextRow {
                    event: None,
                    record: None,
                    stable: None,
                };
            }
            Ok(None) => {}
        }
        match TranscriptContextRecord::from_transcript_content(&entry.content) {
            Ok(Some(record)) => {
                let (event, record, stable) = record.into_parts();
                if observe_stable {
                    self.observe_stable(entry.sequence, stable.as_deref());
                }
                let sequence = match &event {
                    TranscriptContextEvent::UserEvent { event_sequence, .. }
                    | TranscriptContextEvent::TaskAdmission { event_sequence, .. }
                    | TranscriptContextEvent::McpCatalogSnapshot { event_sequence, .. }
                    | TranscriptContextEvent::PromptBoundary {
                        event_sequence: Some(event_sequence),
                        ..
                    } => Some(*event_sequence),
                    _ => None,
                };
                if observe_stable
                    && let TranscriptContextEvent::TaskAdmission { task_start, .. } = &event
                {
                    self.task_admissions
                        .insert(record.occurrence_id(), *task_start);
                }
                if let Some(sequence) = sequence {
                    match mez_agent::ContextEventSequence::new(sequence) {
                        Ok(sequence) => {
                            self.source_sequences
                                .insert(record.occurrence_id(), sequence);
                        }
                        Err(_) => self.invalid = true,
                    }
                }
                DecodedContextRow {
                    event: Some(event),
                    record: Some(record),
                    stable,
                }
            }
            Ok(None) => DecodedContextRow {
                event: TranscriptContextEvent::from_transcript_content(&entry.content),
                record: None,
                stable: None,
            },
            Err(_) => {
                self.invalid = true;
                DecodedContextRow {
                    event: None,
                    record: None,
                    stable: None,
                }
            }
        }
    }

    /// Adds one exact structural source. Legacy position is the durable archive
    /// row, never the newly allocated local context sequence or current pane.
    pub(super) fn push_block(
        &mut self,
        blocks: &mut Vec<ContextBlock>,
        block: ContextBlock,
        entry: &TranscriptEntry,
        record: Option<ContextBlockRecord>,
        stable: Option<&[StableContextBlockRecord]>,
    ) -> Option<ContextBlockRecord> {
        let record = self.restore_row_record(&block, entry, record);
        self.observe_stable(entry.sequence, stable);
        let record = match record {
            Ok(record) => {
                self.records.push((blocks.len(), record));
                Some(record)
            }
            Err(_) => {
                self.invalid = true;
                None
            }
        };
        blocks.push(block);
        record
    }

    /// Validates source identity before consulting descriptive cached costs.
    /// A valid raw authored record needs no duplicate cache; a repaired one is
    /// retained by the leased publisher without rewriting its original bytes.
    fn restore_row_record(
        &mut self,
        block: &ContextBlock,
        entry: &TranscriptEntry,
        authored: Option<ContextBlockRecord>,
    ) -> mez_agent::AgentContextResult<ContextBlockRecord> {
        let placement = self.row_origins.get(&entry.sequence).copied();
        let origin = if let Some(record) = authored {
            record.validate_format()?;
            record.occurrence_id()
        } else if let Some(origin) = placement {
            origin
        } else {
            ContextBlockOccurrenceId::for_legacy_archive_row(
                &entry.conversation_id,
                entry.sequence,
                0,
            )?
        };
        if placement.is_some_and(|placed| placed != origin) {
            return Err(mez_agent::AgentContextError::new(
                "authored row placement origin mismatch",
            ));
        }
        let cached = self.row_cache.get(&entry.sequence).copied();
        let prior = cached.or(authored);
        let restored = match prior {
            Some(record) if record.occurrence_id() == origin => record.restored(block)?,
            Some(_) => {
                return Err(mez_agent::AgentContextError::new(
                    "transcript row cache origin mismatch",
                ));
            }
            None => ContextBlockRecord::for_occurrence(origin, block)?,
        };
        #[cfg(test)]
        if Some(restored) != prior {
            self.estimate_recomputations += 1;
        }
        if authored.is_none() || cached.is_some() || Some(restored) != authored {
            self.rendered_rows.insert(entry.sequence, restored);
        }
        Ok(restored)
    }

    /// Shifts history indices when a real memory/head prefix is prepended.
    /// Overflow withholds later adoption instead of wrapping an association.
    pub(super) fn prepend_blocks(&mut self, count: usize) {
        if count != 0 && self.replay.is_some() {
            self.invalid = true;
            return;
        }
        for (index, _) in &mut self.records {
            match index.checked_add(count) {
                Some(value) => *index = value,
                None => self.invalid = true,
            }
        }
    }

    /// Returns records for an actual final flat block projection. All source
    /// history is chronological; stable-prefix authority is assembled separately.
    pub(in crate::runtime) fn indices_with_offset(
        &self,
        offset: usize,
    ) -> Result<Vec<(usize, ContextBlockRecord)>> {
        self.ensure_valid()?;
        self.records
            .iter()
            .map(|(index, record)| {
                index
                    .checked_add(offset)
                    .map(|index| (index, *record))
                    .ok_or_else(invalid)
            })
            .collect()
    }

    /// Constructs a real provider context without recounting valid history
    /// records before their adoption. Actual authority is still supplied by
    /// the freshly composed stable prefix, never by saved slot text.
    pub(in crate::runtime) fn import_context(
        &self,
        blocks: Vec<ContextBlock>,
    ) -> Result<AgentContext> {
        self.import_context_with_stable_sources(blocks, &[])
    }

    /// Uses actual producer keys for current live stable slots. Bare ordered
    /// imports remain an explicit legacy/history boundary, never label matching.
    pub(in crate::runtime) fn import_context_with_stable_sources(
        &self,
        blocks: Vec<ContextBlock>,
        sources: &[(usize, mez_agent::StableContextSlotId)],
    ) -> Result<AgentContext> {
        self.import_composed_context(blocks, sources, &[], false, None)
    }

    /// Builds a new user task over frozen history. Only this explicit user
    /// boundary retires prior exact task instructions; history-only import keeps
    /// their original protection. Imported execution metadata is applied solely
    /// to the newly composed suffix, never to frozen native history.
    /// `admission` supplies the actual bound conversation and captured model
    /// history length. None is an unbound preview; frozen replay already carries
    /// its original scope. These are structural inputs, not archive ACKs.
    pub(in crate::runtime) fn import_context_for_new_task(
        &self,
        blocks: Vec<ContextBlock>,
        sources: &[(usize, mez_agent::StableContextSlotId)],
        execution: &[mez_agent::ImportedExecutionEvent],
        admission: Option<(&str, usize)>,
    ) -> Result<AgentContext> {
        self.import_composed_context(blocks, sources, execution, true, admission)
    }

    /// Composes a peer-triggered turn without retiring the user's protected
    /// instructions. Receiving lower-priority peer data is not task completion
    /// or authority to supersede user intent. Execution ownership stays scoped
    /// to the new suffix just as at ordinary user admission.
    /// An explicit empty-history admission establishes ownership before new
    /// peer input is appended; it creates neither a user root nor a handoff ACK.
    pub(in crate::runtime) fn import_context_for_peer_task(
        &self,
        blocks: Vec<ContextBlock>,
        sources: &[(usize, mez_agent::StableContextSlotId)],
        execution: &[mez_agent::ImportedExecutionEvent],
        admission: Option<(&str, usize)>,
    ) -> Result<AgentContext> {
        self.import_composed_context(blocks, sources, execution, false, admission)
    }

    /// Composes trusted current prefix and suffix around the immutable canonical
    /// snapshot. An edited/dropped/reordered flat history is a source conflict,
    /// never a request to import a replacement chronology.
    fn import_composed_context(
        &self,
        mut blocks: Vec<ContextBlock>,
        sources: &[(usize, mez_agent::StableContextSlotId)],
        execution: &[mez_agent::ImportedExecutionEvent],
        new_task: bool,
        admission: Option<(&str, usize)>,
    ) -> Result<AgentContext> {
        self.ensure_valid()?;
        let prefix = blocks
            .iter()
            .take_while(|block| block.placement == mez_agent::ContextPlacement::StablePrefix)
            .count();
        if let Some((conversation, history)) = admission {
            mez_agent::ReplayConversationId::new(conversation)?;
            if prefix
                .checked_add(history)
                .is_none_or(|end| end > blocks.len())
                || history == 0 && !self.records.is_empty()
            {
                return Err(MezError::conflict(
                    "task admission history boundary is invalid",
                ));
            }
        }
        if let Some(replay) = &self.replay {
            if admission.is_some_and(|(conversation, history)| {
                conversation != replay.conversation.as_str() || history != replay.blocks
            }) {
                return Err(MezError::conflict(
                    "task admission does not match its captured history",
                ));
            }
            let history_end = prefix
                .checked_add(replay.blocks)
                .filter(|end| *end <= blocks.len())
                .ok_or_else(|| MezError::conflict("frozen replay history is missing"))?;
            let suffix = blocks.split_off(history_end);
            let frozen = blocks.split_off(prefix);
            let stable = self.stable.as_deref().unwrap_or(&replay.stable_records);
            let current_prefix = AgentContext::import_durable_blocks_with_stable_sources(
                blocks,
                &[],
                stable,
                sources,
            )?;
            let mut owner = mez_agent::ReplayEpochContext::from_owned_context(
                replay.conversation.clone(),
                replay.body.clone(),
            )?;
            owner.replace_stable_slots(current_prefix.stable_slots().to_vec())?;
            if !owner
                .context()
                .chronology()
                .iter()
                .map(|event| event.block())
                .eq(frozen.iter())
            {
                return Err(MezError::conflict(
                    "frozen replay history changed during prompt composition",
                ));
            }
            let suffix_records = self
                .records
                .iter()
                .filter(|(index, _)| *index >= replay.blocks)
                .map(|(index, record)| (index - replay.blocks, *record))
                .collect::<Vec<_>>();
            if new_task {
                let prompt = suffix
                    .iter()
                    .position(|block| {
                        block.source == mez_agent::ContextSourceKind::UserInstruction
                            && block.label == "user prompt"
                    })
                    .ok_or_else(|| MezError::conflict("new task has no original user root"))?;
                owner.admit_instruction_task(suffix, &suffix_records, execution, prompt)?;
                return Ok(owner.into_owned_context());
            }
            let mut context = owner.into_owned_context();
            context.append_imported_history_with_execution(suffix, &suffix_records, execution)?;
            return Ok(context);
        }
        if let Some((conversation, 0)) = admission
            && self.records.is_empty()
        {
            if blocks
                .iter()
                .any(mez_agent::context_block_is_compaction_summary)
            {
                return Err(MezError::conflict(
                    "initial empty history contains summary source",
                ));
            }
            return bootstrap::fresh_context(
                self,
                blocks,
                sources,
                execution,
                new_task,
                conversation,
            );
        }
        let records = self.indices_with_offset(prefix)?;
        let mut context = AgentContext::import_durable_blocks_with_stable_sources(
            blocks,
            &records,
            self.stable.as_deref().unwrap_or_default(),
            sources,
        )?;
        context.restore_imported_execution_events(execution)?;
        Ok(context)
    }

    /// Rejects malformed/future reserved records before a source is published.
    pub(in crate::runtime) fn ensure_valid(&self) -> Result<()> {
        if let Some((kind, message)) = &self.source_rejection {
            return Err(MezError::new(
                *kind,
                format!("transcript accounting: {message}"),
            ));
        }
        if self.invalid || !self.control_sequences.is_empty() && !self.controls_qualified {
            return Err(invalid());
        }
        Ok(())
    }
}

/// Emits no source, identity, paths or authored metadata in diagnostics.
fn invalid() -> MezError {
    MezError::invalid_state("invalid accounted context history projection")
}

#[cfg(test)]
mod tests;
