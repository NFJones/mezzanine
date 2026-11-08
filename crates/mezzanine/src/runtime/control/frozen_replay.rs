//! Immutable canonical source for NoStore bindings and loop copies.
//!
//! Captured history includes exact membership and original accounting, rather
//! than a numeric request to reread a parent's later manifest. Each target gets
//! a fresh owner at composition; no controller capabilities or expense are
//! inherited. Pure live checkpoints can advance the same in-memory conversation
//! without minting a durable snapshot or clearing fresh retrieval on every read.

use super::{RuntimeAgentHistorySource, RuntimeAgentTranscriptContext, RuntimeSessionService};
use crate::{MezError, Result};
use mez_agent::{AgentContext, ReplayConversationId, ReplayEpochContext};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct RuntimeFrozenReplayHistory(Arc<Frozen>);

/// Exact NoStore selection shared by saved and subagent copy workers. The
/// original binding and immutable live checkpoint fence adoption independently
/// of archive counts. This value grants no execution or persistence authority.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RuntimeFrozenReplayCopySource {
    pub(in crate::runtime::control) selected: RuntimeFrozenReplayHistory,
    binding: Option<RuntimeFrozenReplayHistory>,
    live: Option<(String, mez_agent::ReplayEpochCheckpoint)>,
    epoch: u64,
}

impl std::fmt::Debug for RuntimeFrozenReplayCopySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RuntimeFrozenReplayCopySource")
    }
}

impl RuntimeFrozenReplayCopySource {
    /// Captures only actual NoStore ownership, without filesystem access. Raw
    /// legacy source remains on its existing explicit migration/read path.
    pub(in crate::runtime) fn capture(
        service: &RuntimeSessionService,
        pane: &str,
    ) -> Result<Option<Self>> {
        let session = service
            .agent_shell_store()
            .get(pane)
            .ok_or_else(|| MezError::invalid_state("fork source session is unavailable"))?;
        if !session.ephemeral {
            return Ok(None);
        }
        let binding = service
            .frozen_replay_binding(pane, &session.session_id)
            .cloned();
        let mut live = None;
        let selected = if let Some(turn_id) = session.running_turn_id.as_deref() {
            let turn = service
                .agent_turn_ledger()
                .turn(turn_id)
                .filter(|turn| turn.conversation_id == session.session_id)
                .ok_or_else(|| MezError::conflict("NoStore fork lost its actual active turn"))?;
            let body = service.agent_turn_replay_context(turn_id).ok_or_else(|| {
                MezError::conflict("NoStore fork actual live source is unavailable")
            })?;
            if body.has_replay_owner() {
                let selected = RuntimeFrozenReplayHistory::from_live(
                    ReplayConversationId::new(&session.session_id)?,
                    body,
                    service.agent_turn_history_source(turn_id).cloned(),
                    binding
                        .as_ref()
                        .map_or(0, |source| source.archive_frontier()),
                    binding.as_ref(),
                    Some((
                        crate::storage::transcript::replay_epoch::ReplaySourceOrigin::new(
                            &turn.turn_id,
                            &turn.agent_id,
                            &turn.pane_id,
                        )?,
                        service.agent_turn_imported_history_sequence_high_water(turn_id),
                    )),
                )?;
                live = Some((
                    turn_id.to_owned(),
                    ReplayEpochContext::from_owned_context(
                        ReplayConversationId::new(&session.session_id)?,
                        body.clone(),
                    )?
                    .checkpoint()?,
                ));
                Some(selected)
            } else if binding.is_some() {
                return Err(MezError::conflict(
                    "NoStore live source lost its original canonical owner",
                ));
            } else {
                None
            }
        } else {
            binding.clone()
        };
        selected
            .map(|selected| {
                Ok::<_, MezError>(Self {
                    selected: selected.freeze_for_fork()?,
                    binding,
                    live,
                    epoch: service.agent_compaction_epoch(pane),
                })
            })
            .transpose()
    }

    /// Detects text, lifecycle, turn identity, binding or epoch changes after
    /// capture, including a new admission while the original parent was idle.
    /// It does not borrow mutable protection handles as an equality witness.
    pub(in crate::runtime) fn is_current(
        &self,
        service: &RuntimeSessionService,
        pane: &str,
    ) -> bool {
        let conversation = self.selected.conversation();
        service
            .agent_shell_store()
            .get(pane)
            .is_some_and(|session| {
                session.ephemeral
                    && session.session_id == conversation.as_str()
                    && session.running_turn_id.as_deref()
                        == self.live.as_ref().map(|(turn, _)| turn.as_str())
            })
            && service.agent_compaction_epoch(pane) == self.epoch
            && service.frozen_replay_binding(pane, conversation.as_str()) == self.binding.as_ref()
            && self.live.as_ref().is_none_or(|(turn, expected)| {
                service
                    .agent_turn_replay_context(turn)
                    .and_then(|body| {
                        ReplayEpochContext::from_owned_context(conversation.clone(), body.clone())
                            .and_then(|owner| owner.checkpoint())
                            .ok()
                    })
                    .as_ref()
                    == Some(expected)
            })
    }

    pub(in crate::runtime) fn fork_source(
        &self,
    ) -> Result<crate::storage::transcript::replay_epoch::ReplayForkSource> {
        self.selected.fork_source()
    }
}

struct Frozen {
    conversation: ReplayConversationId,
    history: RuntimeAgentTranscriptContext,
    archive_frontier: u64,
    /// Fork provenance is optional capability data. A local capture rejection
    /// must not discard valid NoStore model continuity or fail another rewrite.
    memory: Option<FrozenMemoryCapture>,
}

type FrozenMemoryCapture = std::result::Result<
    Arc<crate::storage::transcript::replay_epoch::ReplayMemorySource>,
    (crate::MezErrorKind, String),
>;

impl std::fmt::Debug for RuntimeFrozenReplayHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RuntimeFrozenReplayHistory")
    }
}
impl PartialEq for RuntimeFrozenReplayHistory {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for RuntimeFrozenReplayHistory {}

impl RuntimeFrozenReplayHistory {
    /// Only an actual typed projection can create a frozen replay selection.
    /// Flat labels/blocks and a row count cannot manufacture the missing head.
    pub(super) fn from_history(
        history: RuntimeAgentTranscriptContext,
        archive_frontier: u64,
    ) -> Result<Option<Self>> {
        let Some(conversation) = history.records.replay_conversation().cloned() else {
            return Ok(None);
        };
        history.records.import_context(history.blocks.clone())?;
        let memory = history
            .records
            .replay_memory_source()
            .or_else(|| {
                history
                    .source
                    .as_ref()
                    .and_then(|source| source.memory.clone())
            })
            .map(Ok);
        Ok(Some(Self(Arc::new(Frozen {
            conversation,
            history,
            archive_frontier,
            memory,
        }))))
    }

    pub(in crate::runtime) fn conversation(&self) -> &ReplayConversationId {
        &self.0.conversation
    }
    pub(in crate::runtime) fn archive_frontier(&self) -> u64 {
        self.0.archive_frontier
    }
    pub(in crate::runtime) fn history(&self) -> RuntimeAgentTranscriptContext {
        self.0.history.clone()
    }

    /// Uses this exact immutable in-memory selection. No count, later manifest
    /// or replacement store may supply missing source or restart authority.
    pub(in crate::runtime) fn fork_source(
        &self,
    ) -> Result<crate::storage::transcript::replay_epoch::ReplayForkSource> {
        let owner = self.0.history.replay_owner()?.ok_or_else(|| {
            MezError::invalid_state("frozen fork has no original canonical source")
        })?;
        let source = match &self.0.memory {
            Some(Ok(source)) => source.fork_source(&owner),
            Some(Err((kind, diagnostic))) => Err(MezError::new(*kind, diagnostic.clone())),
            None => {
                let source = self.0.history.source.as_deref();
                crate::storage::transcript::replay_epoch::ReplayMemorySource::capture(
                    &owner,
                    None,
                    source.and_then(|source| source.replay.as_deref()),
                    None,
                    source.map_or(&[], RuntimeAgentHistorySource::original_rows),
                )?
                .fork_source(&owner)
            }
        }?;
        // Model-only reconstruction has no current trusted policy slots. Keep
        // the separate original accounting snapshot, including an explicit
        // empty baseline, in the bundle consumed by both copy transactions.
        source.with_stable_records(self.0.history.records.stable_records_for_copy()?)
    }

    /// Freezes descriptive lifecycle and accounting before the worker can run.
    /// Parent protection handles cannot advance this capture through shared
    /// live Arcs; absent policy bytes cannot erase the original stable records.
    pub(in crate::runtime) fn freeze_for_fork(&self) -> Result<Self> {
        let mut history = self.history();
        let owner = history
            .replay_owner()?
            .ok_or_else(|| MezError::invalid_state("fork source lost its canonical owner"))?;
        let checkpoint = owner
            .checkpoint()?
            .with_stable_records(history.records.stable_records_for_copy()?)?;
        let (records, blocks) =
            super::context_records::RuntimeContextRecords::from_replay_checkpoint(
                self.0.conversation.clone(),
                checkpoint,
                history.records.replay_snapshot(),
            )?;
        history.records = records;
        history.blocks = blocks;
        Ok(Self(Arc::new(Frozen {
            conversation: self.0.conversation.clone(),
            history,
            archive_frontier: self.0.archive_frontier,
            memory: self.0.memory.clone(),
        })))
    }

    /// A new binding is a source copy, even if its caller chooses the same CID.
    /// Later reads of that binding do not repeat this revocation transition.
    fn for_target(&self, target: ReplayConversationId) -> Result<Self> {
        let mut history = self.history();
        history.records.rebind_replay_copy(target.clone())?;
        history.source = Some(RuntimeAgentHistorySource::with_target_policy(
            history
                .source
                .unwrap_or_else(RuntimeAgentHistorySource::absent),
            true,
        ));
        Ok(Self(Arc::new(Frozen {
            conversation: target,
            history,
            archive_frontier: self.0.archive_frontier,
            memory: self.0.memory.clone(),
        })))
    }

    /// Retains the actual live body as descriptive continuation, not another
    /// source copy. Fresh post-rewrite retrieval therefore survives later turns.
    pub(super) fn from_live(
        conversation: ReplayConversationId,
        body: &AgentContext,
        source: Option<Arc<RuntimeAgentHistorySource>>,
        archive_frontier: u64,
        previous: Option<&Self>,
        producer: Option<(
            crate::storage::transcript::replay_epoch::ReplaySourceOrigin,
            u64,
        )>,
    ) -> Result<Self> {
        let owner = ReplayEpochContext::from_owned_context(conversation.clone(), body.clone())?;
        let snapshot = source.as_ref().and_then(|source| source.replay.clone());
        let memory = crate::storage::transcript::replay_epoch::ReplayMemorySource::capture(
            &owner,
            previous
                .and_then(|previous| previous.0.memory.as_ref())
                .and_then(|capture| capture.as_ref().ok())
                .map(Arc::as_ref)
                .or_else(|| source.as_ref().and_then(|source| source.memory.as_deref())),
            snapshot.as_deref(),
            producer
                .as_ref()
                .map(|(origin, boundary)| (origin, *boundary)),
            source
                .as_deref()
                .map_or(&[], RuntimeAgentHistorySource::original_rows),
        )
        .map(Arc::new)
        .map_err(|error| (error.kind(), error.message().to_owned()));
        let (records, blocks) = super::context_records::RuntimeContextRecords::from_replay_owner(
            conversation.clone(),
            owner,
            snapshot,
        )?;
        let source = RuntimeAgentHistorySource::with_target_policy(
            source.unwrap_or_else(RuntimeAgentHistorySource::absent),
            true,
        );
        Ok(Self(Arc::new(Frozen {
            conversation,
            archive_frontier,
            memory: Some(memory),
            history: RuntimeAgentTranscriptContext {
                source: Some(source),
                blocks,
                records,
                execution_events: Vec::new(),
                provider_history_repair_identity: None,
            },
        })))
    }
}

impl RuntimeSessionService {
    /// Captures once before an ephemeral source binding changes the pane.
    /// Legacy history returns None and keeps its existing explicit row contract.
    #[cfg(test)]
    pub(crate) fn freeze_agent_replay_history(
        &self,
        pane: &str,
    ) -> Result<Option<RuntimeFrozenReplayHistory>> {
        self.check_agent_history_admission(pane)?;
        self.prepare_runtime_agent_prompt_history_work(pane)
            .freeze_replay_history()
    }

    /// Installs a NoStore source from its actual captured checkpoint. Lower
    /// binding failure restores the original session; no count-only fallback.
    pub(crate) fn bind_ephemeral_replay_history(
        &mut self,
        pane: &str,
        target: &str,
        source: &RuntimeFrozenReplayHistory,
        lineage: Option<String>,
    ) -> Result<(String, u64)> {
        let selected = source.for_target(ReplayConversationId::new(target)?)?;
        let previous = self
            .agent_shell_store()
            .get(pane)
            .cloned()
            .ok_or_else(|| MezError::conflict("ephemeral replay pane is unavailable"))?;
        let bound = self
            .agent_shell_store_mut()
            .bind_ephemeral_conversation_with_lineage_and_transcript_source(
                pane,
                target,
                0,
                lineage,
                Some(source.conversation().as_str().to_owned()),
                source.archive_frontier(),
            )
            .map(|session| (session.session_id.clone(), session.transcript_entries));
        let bound = match bound {
            Ok(bound) => bound,
            Err(error) => {
                self.agent_shell_store_mut()
                    .restore_session(pane, previous)?;
                return Err(error.into());
            }
        };
        self.set_frozen_replay_binding(pane, Some(selected));
        Ok(bound)
    }

    /// Publishes in-memory continuity only for the admitted target conversation.
    /// Does not sample a replacement store or claim restart durability.
    pub(crate) fn retain_ephemeral_replay_body(
        &mut self,
        pane: &str,
        conversation: &str,
        body: &AgentContext,
        source: Option<Arc<RuntimeAgentHistorySource>>,
        producer: Option<(
            crate::storage::transcript::replay_epoch::ReplaySourceOrigin,
            u64,
        )>,
        before_rewrite: Option<&AgentContext>,
    ) -> Result<()> {
        if !body.has_replay_owner() {
            return Ok(());
        }
        let session = self
            .agent_shell_store()
            .get(pane)
            .filter(|session| session.ephemeral && session.session_id == conversation)
            .ok_or_else(|| MezError::conflict("ephemeral replay target changed"))?;
        let previous = self
            .frozen_replay_binding(pane, &session.session_id)
            .cloned();
        let frontier = previous
            .as_ref()
            .map_or(0, |source| source.archive_frontier());
        // Capture original boundary bytes before a rewrite removes them from
        // model membership. This remains local until accepted adoption; no
        // intermediate binding or persistence acknowledgment is installed.
        let before = before_rewrite
            .map(|body| {
                RuntimeFrozenReplayHistory::from_live(
                    ReplayConversationId::new(conversation)?,
                    body,
                    source.clone(),
                    frontier,
                    previous.as_ref(),
                    producer.clone(),
                )
            })
            .transpose()?;
        let frozen = RuntimeFrozenReplayHistory::from_live(
            ReplayConversationId::new(conversation)?,
            body,
            source,
            frontier,
            before.as_ref().or(previous.as_ref()),
            producer,
        )?;
        self.set_frozen_replay_binding(pane, Some(frozen));
        Ok(())
    }
}
