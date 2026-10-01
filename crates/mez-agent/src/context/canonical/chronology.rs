//! Ordered event append and append-only staged rebase on canonical context.
//!
//! Event identities, provenance, retention and causal ownership are committed
//! together. Candidate validation is atomic and never replays producer work.

use super::{
    AgentContext, AgentContextError, AgentContextResult, CONTEXT_EVENT_SEQUENCE_STRIDE,
    ContextBlock, ContextConversationAppend, ContextEventSequence, ContextExecutionGroupId,
    ContextPlacement, ContextRetention, ContextSemanticKind, ContextSourceKind, ConversationEvent,
    ProviderContinuityOwner, validate_context_block_metadata,
};

impl AgentContext {
    /// Rebases append-only arrivals onto an unpublished compacted candidate.
    /// The live chronology must retain the complete frozen source unchanged.
    /// Arrivals keep their original identity, trust, retention and execution
    /// metadata. Validation is atomic; source rewrites or invalid ownership
    /// leave this candidate unchanged. Stable-prefix refresh is independent.
    pub fn rebase_chronology_suffix(
        &mut self,
        frozen: &[ConversationEvent],
        live: &Self,
    ) -> AgentContextResult<usize> {
        live.validate_durable()?;
        if !live.chronology.starts_with(frozen) {
            return Err(AgentContextError::new(
                "staged compaction frozen source chronology changed",
            ));
        }
        let high_water = frozen.last().map_or(0, |event| event.sequence.get());
        if self.event_sequence_high_water_mark() > high_water {
            return Err(AgentContextError::new(
                "staged compaction candidate already contains unfrozen arrivals",
            ));
        }
        let suffix = &live.chronology[frozen.len()..];
        let mut candidate = self.clone();
        candidate.chronology.extend_from_slice(suffix);
        candidate.next_event_sequence = candidate.next_event_sequence.max(live.next_event_sequence);
        candidate.rebuild_projections();
        candidate.validate_durable()?;
        *self = candidate;
        Ok(suffix.len())
    }

    /// Appends an exact user event at the next chronological sequence.
    pub fn append_user_event(
        &mut self,
        label: impl Into<String>,
        content: impl Into<String>,
    ) -> AgentContextResult<ContextEventSequence> {
        self.append_conversation_event(
            ContextBlock::user_event(label, content),
            ContextSemanticKind::UserEvent,
            ContextRetention::Exact,
            None,
            None,
            false,
        )
    }

    /// Appends an assistant response owned by one execution group.
    pub fn append_assistant_event(
        &mut self,
        label: impl Into<String>,
        content: impl Into<String>,
        execution_group_id: ContextExecutionGroupId,
    ) -> AgentContextResult<ContextEventSequence> {
        self.append_conversation_event(
            ContextBlock::assistant_event(label, content),
            ContextSemanticKind::AssistantEvent,
            ContextRetention::ExecutionGroup,
            Some(execution_group_id),
            None,
            true,
        )
    }

    /// Appends settled evidence owned by one execution group.
    pub fn append_evidence_event(
        &mut self,
        source: ContextSourceKind,
        label: impl Into<String>,
        content: impl Into<String>,
        execution_group_id: ContextExecutionGroupId,
        provider_owner: Option<ProviderContinuityOwner>,
        recoverable_for_compaction: bool,
    ) -> AgentContextResult<ContextEventSequence> {
        self.append_conversation_event(
            ContextBlock::evidence_event(source, label, content),
            ContextSemanticKind::EvidenceEvent,
            ContextRetention::ExecutionGroup,
            Some(execution_group_id),
            provider_owner,
            recoverable_for_compaction,
        )
    }

    /// Appends an exact neutral reference event.
    pub fn append_reference_event(
        &mut self,
        source: ContextSourceKind,
        label: impl Into<String>,
        content: impl Into<String>,
    ) -> AgentContextResult<ContextEventSequence> {
        self.append_conversation_event(
            ContextBlock::reference_event(source, label, content),
            ContextSemanticKind::ReferenceEvent,
            ContextRetention::Exact,
            None,
            None,
            true,
        )
    }

    /// Appends one peer agent message as a lower-priority reference event.
    ///
    /// Peer text is untrusted data. The block carries reference-event semantics
    /// and summarizable retention, so it can be compacted before direct user
    /// prompts or mid-turn steering while remaining part of canonical
    /// chronology until consumption.
    pub fn append_peer_message_event(
        &mut self,
        label: impl Into<String>,
        content: impl Into<String>,
    ) -> AgentContextResult<ContextEventSequence> {
        self.append_conversation_event(
            ContextBlock::reference_event(ContextSourceKind::PeerMessage, label, content),
            ContextSemanticKind::ReferenceEvent,
            ContextRetention::Summarizable,
            None,
            None,
            true,
        )
    }

    /// Appends a typed task prelude before the first direct-user event.
    pub fn append_task_prelude(
        &mut self,
        source: ContextSourceKind,
        label: impl Into<String>,
        content: impl Into<String>,
        retention: ContextRetention,
    ) -> AgentContextResult<ContextEventSequence> {
        self.append_conversation_event(
            ContextBlock::task_prelude(source, label, content),
            ContextSemanticKind::TaskPrelude,
            retention,
            None,
            None,
            true,
        )
    }

    /// Appends one producer-classified chronological event.
    pub(super) fn append_conversation_event(
        &mut self,
        block: ContextBlock,
        semantic_kind: ContextSemanticKind,
        retention: ContextRetention,
        execution_group_id: Option<ContextExecutionGroupId>,
        provider_owner: Option<ProviderContinuityOwner>,
        recoverable_for_compaction: bool,
    ) -> AgentContextResult<ContextEventSequence> {
        let mut sequences = self.append_conversation_events([ContextConversationAppend {
            block,
            semantic_kind,
            retention,
            execution_group_id,
            provider_owner,
            recoverable_for_compaction,
        }])?;
        Ok(sequences
            .pop()
            .expect("one appended context event must allocate one sequence"))
    }

    /// Atomically appends producer-classified chronological events in order.
    ///
    /// Every event is validated before commit, and the canonical block and
    /// metadata projections are rebuilt and validated once for the batch.
    /// Failure leaves the original context unchanged.
    pub fn append_conversation_events(
        &mut self,
        events: impl IntoIterator<Item = ContextConversationAppend>,
    ) -> AgentContextResult<Vec<ContextEventSequence>> {
        let mut candidate = self.clone();
        let events = events.into_iter().collect::<Vec<_>>();
        let mut sequences = Vec::with_capacity(events.len());
        candidate.chronology.reserve(events.len());
        for event in events {
            if event.block.placement != ContextPlacement::ConversationAppend {
                return Err(AgentContextError::new(
                    "chronological events must use conversation-append placement",
                ));
            }
            let sequence = candidate.allocate_event_sequence()?;
            let event = ConversationEvent {
                block: event.block,
                semantic_kind: event.semantic_kind,
                retention: event.retention,
                sequence,
                execution_group_id: event.execution_group_id,
                provider_owner: event.provider_owner,
                recoverable_for_compaction: event.recoverable_for_compaction,
            };
            validate_context_block_metadata(
                candidate.stable_slots.len() + candidate.chronology.len(),
                &event.block,
                &event.metadata(),
            )?;
            candidate.chronology.push(event);
            sequences.push(sequence);
        }
        candidate.rebuild_projections();
        candidate.validate_stored_metadata()?;
        *self = candidate;
        Ok(sequences)
    }

    /// Allocates the next non-zero chronological sequence.
    pub(super) fn allocate_event_sequence(&mut self) -> AgentContextResult<ContextEventSequence> {
        let sequence = ContextEventSequence::new(self.next_event_sequence)?;
        self.next_event_sequence = self
            .next_event_sequence
            .checked_add(CONTEXT_EVENT_SEQUENCE_STRIDE)
            .ok_or_else(|| AgentContextError::new("context event sequence exhausted"))?;
        Ok(sequence)
    }
}
