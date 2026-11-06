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

struct Frozen {
    conversation: ReplayConversationId,
    history: RuntimeAgentTranscriptContext,
    archive_frontier: u64,
}

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
        Ok(Some(Self(Arc::new(Frozen {
            conversation,
            history,
            archive_frontier,
        }))))
    }

    pub(in crate::runtime) fn conversation(&self) -> &ReplayConversationId {
        &self.0.conversation
    }
    pub(in crate::runtime) fn archive_frontier(&self) -> u64 {
        self.0.archive_frontier
    }
    pub(super) fn history(&self) -> RuntimeAgentTranscriptContext {
        self.0.history.clone()
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
        })))
    }

    /// Retains the actual live body as descriptive continuation, not another
    /// source copy. Fresh post-rewrite retrieval therefore survives later turns.
    pub(super) fn from_live(
        conversation: ReplayConversationId,
        body: &AgentContext,
        source: Option<Arc<RuntimeAgentHistorySource>>,
        archive_frontier: u64,
    ) -> Result<Self> {
        let owner = ReplayEpochContext::from_owned_context(conversation.clone(), body.clone())?;
        let snapshot = source.as_ref().and_then(|source| source.replay.clone());
        let (records, blocks) =
            super::context_records::RuntimeContextRecords::from_replay_checkpoint(
                conversation.clone(),
                owner.checkpoint()?,
                snapshot,
            )?;
        let source = RuntimeAgentHistorySource::with_target_policy(
            source.unwrap_or_else(RuntimeAgentHistorySource::absent),
            true,
        );
        Ok(Self(Arc::new(Frozen {
            conversation,
            archive_frontier,
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
    ) -> Result<()> {
        if !body.has_replay_owner() {
            return Ok(());
        }
        let session = self
            .agent_shell_store()
            .get(pane)
            .filter(|session| session.ephemeral && session.session_id == conversation)
            .ok_or_else(|| MezError::conflict("ephemeral replay target changed"))?;
        let frontier = self
            .frozen_replay_binding(pane, &session.session_id)
            .map_or(0, |source| source.archive_frontier());
        let frozen = RuntimeFrozenReplayHistory::from_live(
            ReplayConversationId::new(conversation)?,
            body,
            source,
            frontier,
        )?;
        self.set_frozen_replay_binding(pane, Some(frozen));
        Ok(())
    }
}
