//! Explicit import of one existing persisted prefix, without model inference.
//!
//! The adapter must first qualify its original sidecar, summary record and exact
//! active suffix. This pure boundary checks their canonical shape and preserves
//! every event; it cannot inspect an archive or acknowledge durable migration.

use super::super::ContextBlockRecord;
use super::*;

impl ReplayEpochContext {
    /// Adopts one externally qualified historical prefix as the first typed head.
    /// `context` must start with its actual `CompactionPrefix` event and `head`
    /// must be that event's original complete record. No summary label, Memory
    /// UUID, covered raw occurrence or second summary can establish this role.
    ///
    /// The existing text, record, sequence and exact suffix survive unchanged.
    /// Missing old lifecycle provenance stays conservative: execution groups and
    /// peers remain pending, instructions retain their existing protection, and
    /// no task handle, closure witness, effect receipt or publication ACK is
    /// reconstructed. The adapter still owns atomic persisted-format migration,
    /// source/frontier validation and accepted-only provider lineage reset.
    pub fn from_legacy_prefix(
        conversation: ReplayConversationId,
        mut context: AgentContext,
        head: ContextBlockRecord,
    ) -> AgentContextResult<Self> {
        if context.has_replay_owner() {
            return Err(AgentContextError::new(
                "legacy prefix import already has a replay owner",
            ));
        }
        context.validate_durable()?;
        let first = context
            .chronology()
            .first()
            .ok_or_else(|| AgentContextError::new("legacy prefix import has no summary event"))?;
        if first.block().source != ContextSourceKind::CompactionPrefix
            || first.record() != head
            || first.semantic_kind() != super::super::ContextSemanticKind::ReferenceEvent
            || first.retention() != ContextRetention::Summarizable
            || first.execution_group_id().is_some()
            || first.provider_owner().is_some()
            || !first.recoverable_for_compaction()
        {
            return Err(AgentContextError::new(
                "legacy prefix import does not match its explicit summary record",
            ));
        }
        crate::context_compaction::validate_model_compaction_summary(
            &first.block().content,
            usize::MAX,
            false,
        )
        .map_err(|_| AgentContextError::new("legacy prefix import has an unusable summary"))?;
        let first = context.chronology.remove(0);
        context.rebuild_projections();
        // Historical grouping describes retained source, not a local producer
        // interval. Install conservative protection before the ordinary owner
        // constructor can allocate a fresh known interval for an unowned group.
        let groups = context
            .chronology()
            .iter()
            .filter_map(|event| event.execution_group_id().cloned())
            .collect::<BTreeSet<_>>();
        for group in groups {
            context.protect_unqualified_imported_group(&group)?;
        }
        // The ordinary constructor rejects additional summaries and protects
        // peers. Removing only the checked head does not
        // retire a task, invent closure, or reintroduce any covered source.
        Self::new(conversation, context)?.finish_legacy_prefix(first)
    }

    /// Attaches an explicitly qualified old head to initial raw source whose
    /// original task boundaries were restored through the task-admission port.
    /// This avoids flattening several tasks into one active-prompt import.
    /// Original raw sequences, records and instruction lifecycle survive; the
    /// summary receives only an ordering slot before that source. Controller
    /// handles, native closure and peer acknowledgments never survive import.
    pub fn import_legacy_prefix_head(
        mut self,
        block: ContextBlock,
        head: ContextBlockRecord,
        explicit: &[crate::ImportedExecutionEvent],
    ) -> AgentContextResult<Self> {
        if self.revision.get() != 0 || self.head.is_some() {
            return Err(AgentContextError::new(
                "legacy head requires initial raw source",
            ));
        }
        let mut header =
            AgentContext::import_durable_blocks_with_records(vec![block], &[(0, head)], &[])?;
        let mut first = header
            .chronology
            .pop()
            .ok_or_else(|| AgentContextError::new("legacy prefix import has no summary event"))?;
        if first.block().source != ContextSourceKind::CompactionPrefix || first.record() != head {
            return Err(AgentContextError::new(
                "legacy prefix import does not match its explicit summary record",
            ));
        }
        crate::context_compaction::validate_model_compaction_summary(
            &first.block().content,
            usize::MAX,
            false,
        )
        .map_err(|_| AgentContextError::new("legacy prefix import has an unusable summary"))?;
        if let Some(raw) = self.context.chronology.first() {
            first.sequence =
                ContextEventSequence::new(raw.sequence().get().checked_sub(1).ok_or_else(
                    || AgentContextError::new("legacy prefix has no ordering slot"),
                )?)?;
        }
        // Grouping is over the complete head and raw source, independently of
        // the task partitions used to restore instruction lifecycle. Explicit
        // producer framing remains authoritative over inferred legacy groups.
        self.context.chronology.insert(0, first);
        self.context.rebuild_projections();
        self.context.normalize_legacy_import_framing(explicit)?;
        let first = self.context.chronology.remove(0);
        self.context.rebuild_projections();
        self.finish_legacy_prefix(first)
    }

    fn finish_legacy_prefix(mut self, first: ConversationEvent) -> AgentContextResult<Self> {
        let groups = self
            .context
            .chronology
            .iter()
            .filter_map(|event| event.execution_group_id().cloned())
            .collect::<BTreeSet<_>>();
        for group in groups {
            self.context.protect_unqualified_imported_group(&group)?;
        }
        let peers = self
            .context
            .chronology
            .iter()
            .filter(|event| event.block().source == ContextSourceKind::PeerMessage)
            .map(ConversationEvent::sequence)
            .collect::<Vec<_>>();
        for peer in peers {
            self.context.retain_pending_peer_message(peer)?;
        }
        let tasks = std::mem::take(&mut self.source_lifecycle.task_admissions);
        self.source_lifecycle = ReplaySourceLifecycle::default();
        self.source_lifecycle.task_admissions = tasks;
        self.owner_identity = ReplayOwnerIdentity::new();
        self.instruction_owners.clear();
        self.next_instruction_owner = 1;
        let head = first.record();
        self.context.next_event_sequence = self.context.next_event_sequence.max(
            first
                .sequence()
                .get()
                .checked_add(super::super::CONTEXT_EVENT_SEQUENCE_STRIDE)
                .ok_or_else(|| AgentContextError::new("legacy prefix reservation exhausted"))?,
        );
        self.context.chronology.insert(0, first);
        self.context.rebuild_projections();
        self.revision = ReplayEpochRevision(1);
        self.head = Some(ReplaySummaryIdentity {
            conversation: self.conversation.clone(),
            revision: self.revision,
            occurrence_id: head.occurrence_id(),
        });
        self.bind_mcp_retrieval_scope();
        self.revoke_retrieved_mcp_grants();
        self.initialize_accounting()?;
        self.validate_membership()?;
        Ok(self)
    }
}
