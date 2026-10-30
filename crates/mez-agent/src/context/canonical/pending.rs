//! Runtime-owned protection for execution groups with outstanding obligations.
//!
//! Later peer mail or a separate assistant response does not close an earlier
//! joined task. The runtime retains that exact group until its final dependency
//! retires. This in-memory set survives checked context clones and preparation,
//! but supplies no action, provider or completion authority and is never part of
//! persisted transcript metadata or model-visible messages.

use super::{
    AgentContext, AgentContextError, AgentContextResult, ContextEventSequence,
    ContextExecutionGroupId, ContextSourceKind,
};

impl AgentContext {
    /// Protects an already committed peer event until its request owner's context
    /// and wake handoff commits. Missing/non-peer sequences reject without
    /// creating phantom history. Repeated retention is idempotent and does not
    /// acknowledge a message, grant caller authority, or mark progress.
    pub fn retain_pending_peer_message(
        &mut self,
        sequence: ContextEventSequence,
    ) -> AgentContextResult<()> {
        if !self.chronology.iter().any(|event| {
            event.sequence() == sequence && event.block().source == ContextSourceKind::PeerMessage
        }) {
            return Err(AgentContextError::new(
                "pending peer source is absent from context",
            ));
        }
        self.pending_peer_messages.insert(sequence);
        Ok(())
    }

    /// Releases one runtime-owned peer source after handoff or conversation
    /// retirement. This changes no event, cursor, result, or scheduler state.
    pub fn release_pending_peer_message(&mut self, sequence: ContextEventSequence) -> bool {
        self.pending_peer_messages.remove(&sequence)
    }

    /// Reports exact runtime protection without inferring ownership from text.
    pub fn peer_message_is_pending(&self, sequence: ContextEventSequence) -> bool {
        self.pending_peer_messages.contains(&sequence)
    }

    /// Protects an existing execution group from compaction while its runtime
    /// owner has outstanding work. Repeated retention is idempotent. A missing
    /// group fails without inserting state; ids cannot create phantom history.
    pub fn retain_pending_execution_group(
        &mut self,
        group: &ContextExecutionGroupId,
    ) -> AgentContextResult<()> {
        if !self
            .chronology
            .iter()
            .any(|event| event.execution_group_id() == Some(group))
        {
            return Err(AgentContextError::new(
                "pending execution group is absent from context",
            ));
        }
        self.pending_execution_groups.insert(group.clone());
        Ok(())
    }

    /// Releases protection after the runtime owner retires the group's final
    /// obligation. Returns whether protection existed; it does not acknowledge
    /// any context append, child result, native result, hook or scheduler wake.
    pub fn release_pending_execution_group(&mut self, group: &ContextExecutionGroupId) -> bool {
        self.pending_execution_groups.remove(group)
    }

    /// Reports runtime-owned pending protection without inspecting model text.
    pub fn execution_group_is_pending(&self, group: &ContextExecutionGroupId) -> bool {
        self.pending_execution_groups.contains(group)
    }
}
