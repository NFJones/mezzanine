//! Exact task ownership and explicit instruction/peer retirement. Completion
//! changes retention only; source bytes, authorship and original IDs survive.

use super::*;

impl ReplayEpochContext {
    /// Allocates a conversation-bound task handle. Checked monotonic generations
    /// prevent reuse after retirement; overflow leaves this owner unchanged.
    pub fn begin_instruction_task(&mut self) -> AgentContextResult<ReplayInstructionOwner> {
        let generation = self.next_instruction_owner;
        let next = generation
            .checked_add(1)
            .ok_or_else(|| AgentContextError::new("instruction owner generation exhausted"))?;
        self.instruction_owners.insert(generation, BTreeSet::new());
        self.next_instruction_owner = next;
        Ok(ReplayInstructionOwner {
            owner_identity: self.owner_identity.clone(),
            conversation: self.conversation.clone(),
            generation,
        })
    }

    /// Associates existing exact task occurrences with one active owner. Unknown,
    /// already historical, noninstruction or differently owned source rejects
    /// atomically. Stable authority cannot be bound to a lossy lifecycle.
    pub fn bind_instruction_occurrences(
        &mut self,
        owner: &ReplayInstructionOwner,
        sequences: &[ContextEventSequence],
    ) -> AgentContextResult<()> {
        self.check_instruction_owner(owner)?;
        let requested = sequences.iter().copied().collect::<BTreeSet<_>>();
        if requested.len() != sequences.len()
            || requested.iter().any(|sequence| {
                !self.context.chronology().iter().any(|event| {
                    event.sequence() == *sequence
                        && instruction_source_can_complete(event.block())
                        && event.retention() == ContextRetention::Exact
                }) || self.instruction_owners.iter().any(|(generation, events)| {
                    *generation != owner.generation && events.contains(sequence)
                })
            })
        {
            return Err(AgentContextError::new(
                "instruction ownership does not match exact source",
            ));
        }
        self.instruction_owners
            .get_mut(&owner.generation)
            .ok_or_else(|| AgentContextError::new("instruction owner disappeared before binding"))?
            .extend(requested);
        Ok(())
    }

    /// Appends one exact user/task instruction under its current task handle.
    /// Invalid source, stale handle or canonical ordering fails before adoption.
    pub fn append_instruction(
        &mut self,
        owner: &ReplayInstructionOwner,
        block: ContextBlock,
    ) -> AgentContextResult<ContextEventSequence> {
        self.check_instruction_owner(owner)?;
        if !instruction_source_can_complete(&block) {
            return Err(AgentContextError::new(
                "source is not a task-owned chronological instruction",
            ));
        }
        let mut candidate = self.clone();
        let sequences = candidate
            .context
            .append_conversation_events([ContextConversationAppend {
                semantic_kind: block.semantic_kind(),
                block,
                retention: ContextRetention::Exact,
                execution_group_id: None,
                provider_owner: None,
                recoverable_for_compaction: true,
            }])
            .map_err(|_| {
                AgentContextError::new("replay instruction admission failed canonical validation")
            })?;
        let sequence = sequences[0];
        candidate.record_arrivals(&sequences);
        candidate.bind_instruction_occurrences(owner, &sequences)?;
        candidate.validate_membership()?;
        *self = candidate;
        Ok(sequence)
    }

    /// Retires the exact task after explicit completion. Its instructions become
    /// historical source without changing text, authorship or origin. A stale or
    /// foreign handle cannot retire later work. No provider effect is replayed.
    pub fn complete_instruction_task(
        &mut self,
        owner: &ReplayInstructionOwner,
    ) -> AgentContextResult<()> {
        let sequences = self.check_instruction_owner(owner)?.clone();
        let mut candidate = self.clone();
        for sequence in sequences {
            let event = candidate
                .context
                .chronology
                .iter_mut()
                .find(|event| event.sequence == sequence)
                .ok_or_else(|| AgentContextError::new("active instruction source disappeared"))?;
            if !instruction_source_can_complete(&event.block)
                || event.retention != ContextRetention::Exact
            {
                return Err(AgentContextError::new(
                    "active instruction source changed before completion",
                ));
            }
            event.retention = ContextRetention::Summarizable;
            candidate
                .context
                .historical_instruction_events
                .insert(sequence);
        }
        candidate.instruction_owners.remove(&owner.generation);
        candidate.context.rebuild_projections();
        candidate.validate_membership()?;
        *self = candidate;
        Ok(())
    }

    /// Atomically completes the replaced task and allocates its successor. A
    /// generation overflow or stale predecessor leaves both context and owners
    /// unchanged; supersession is explicit, never inferred from newer prose.
    pub fn supersede_instruction_task(
        &mut self,
        owner: &ReplayInstructionOwner,
    ) -> AgentContextResult<ReplayInstructionOwner> {
        let mut candidate = self.clone();
        candidate.complete_instruction_task(owner)?;
        let next = candidate.begin_instruction_task()?;
        *self = candidate;
        Ok(next)
    }

    /// Appends untrusted peer source and protects it until its exact local
    /// handoff commits. This is neither a message ACK nor a caller permission.
    pub fn append_pending_peer(
        &mut self,
        label: impl Into<String>,
        content: impl Into<String>,
    ) -> AgentContextResult<ContextEventSequence> {
        let mut candidate = self.clone();
        let sequence = candidate
            .context
            .append_peer_message_event(label, content)
            .map_err(|_| {
                AgentContextError::new("replay peer admission failed canonical validation")
            })?;
        candidate.context.retain_pending_peer_message(sequence)?;
        candidate.record_arrivals(&[sequence]);
        candidate.validate_membership()?;
        *self = candidate;
        Ok(sequence)
    }

    /// Releases only the identified peer occurrence after actual handoff. Source
    /// remains in current membership and is not acknowledged or executed here.
    pub fn release_peer(&mut self, sequence: ContextEventSequence) -> bool {
        self.context.release_pending_peer_message(sequence)
    }

    /// Resolves one active generation without permitting handle reuse.
    fn check_instruction_owner(
        &self,
        owner: &ReplayInstructionOwner,
    ) -> AgentContextResult<&BTreeSet<ContextEventSequence>> {
        if owner.conversation != self.conversation || owner.owner_identity != self.owner_identity {
            return Err(AgentContextError::new(
                "instruction owner belongs to another conversation",
            ));
        }
        self.instruction_owners
            .get(&owner.generation)
            .ok_or_else(|| AgentContextError::new("instruction owner is no longer active"))
    }
}
