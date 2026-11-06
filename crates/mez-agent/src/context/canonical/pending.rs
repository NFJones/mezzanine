//! Runtime-owned protection for execution groups with outstanding obligations.
//!
//! Later peer mail or a separate assistant response does not close an earlier
//! joined task. The runtime retains that exact group until its final dependency
//! retires. In-memory allocations survive checked clones and preparation; a new
//! protection interval receives a fresh allocation even with unchanged source.
//! The map preserves idempotent retention within the current interval,
//! but supplies no action, provider or completion authority and is never part of
//! persisted transcript metadata or model-visible messages.

use super::{
    AgentContext, AgentContextError, AgentContextResult, ContextEventSequence,
    ContextExecutionGroupId, ContextSourceKind,
};

/// One opaque in-memory protection interval. Clones share its allocation;
/// renewing protection after release allocates another one even at the same
/// frontier. It is never persisted, projected or interpreted as effect authority.
#[derive(Debug, Clone)]
pub(super) struct ContextExecutionProtection(std::sync::Arc<()>);

impl ContextExecutionProtection {
    /// Allocates a distinct interval without a reusable numeric generation.
    pub(super) fn new() -> Self {
        Self(std::sync::Arc::new(()))
    }
}

impl PartialEq for ContextExecutionProtection {
    /// Compares the original allocation, not its unit payload or public labels.
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for ContextExecutionProtection {}

impl AgentContext {
    /// Protects an already committed peer event until its request owner's context
    /// and wake handoff commits. Missing/non-peer sequences reject without
    /// creating phantom history. Repeated retention is idempotent and does not
    /// acknowledge a message, grant caller authority, or mark progress.
    pub fn retain_pending_peer_message(
        &mut self,
        sequence: ContextEventSequence,
    ) -> AgentContextResult<()> {
        let Some(event) = self.chronology.iter().find(|event| {
            event.sequence() == sequence && event.block().source == ContextSourceKind::PeerMessage
        }) else {
            return Err(AgentContextError::new(
                "pending peer source is absent from context",
            ));
        };
        if let Some(continuation) = &mut self.replay_continuation {
            continuation.protect_peer(sequence, event.record().occurrence_id());
        }
        self.pending_peer_messages.insert(sequence);
        Ok(())
    }

    /// Releases one runtime-owned peer source after handoff or conversation
    /// retirement. This changes no event, cursor, result, or scheduler state.
    /// A retained replay owner records this actual controller transition. An
    /// invalid continuation refuses locally and leaves protection unchanged.
    pub fn release_pending_peer_message(&mut self, sequence: ContextEventSequence) -> bool {
        if self.has_replay_owner() {
            return self
                .update_replay_owner(|owner| owner.release_peer(sequence))
                .unwrap_or(false);
        }
        self.pending_peer_messages.remove(&sequence)
    }

    /// Reports exact runtime protection without inferring ownership from text.
    pub fn peer_message_is_pending(&self, sequence: ContextEventSequence) -> bool {
        self.pending_peer_messages.contains(&sequence)
    }

    /// Protects an existing execution group from compaction while its runtime
    /// owner has outstanding work. Repeated pending retention is idempotent;
    /// re-protection after release allocates a fresh interval. A missing
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
        self.activate_execution_protection(group)?;
        Ok(())
    }

    /// Activates or idempotently retains actual source protection. A stale body
    /// cannot replace a still-pending private allocation with an older snapshot.
    /// Callers admitting a new event validate its group after the atomic append.
    pub(super) fn activate_execution_protection(
        &mut self,
        group: &ContextExecutionGroupId,
    ) -> AgentContextResult<ContextExecutionProtection> {
        let previous = self.pending_execution_groups.get(group).cloned();
        let renewed = previous.is_none();
        let protection = previous.unwrap_or_else(ContextExecutionProtection::new);
        if let Some(continuation) = &mut self.replay_continuation {
            continuation.protect_group(group, &protection, renewed)?;
        }
        self.pending_execution_groups
            .insert(group.clone(), protection.clone());
        Ok(protection)
    }

    /// Releases protection after the runtime owner retires the group's final
    /// obligation. Returns whether protection existed; it does not acknowledge
    /// any context append, child result, native result, hook or scheduler wake.
    /// Retained replay ownership requires the original typed execution handle
    /// through `update_replay_owner` and `finish_execution_group`. This unfenced
    /// API returns false for owned replay source: a stale caller must not sample
    /// a newer frontier and retire work admitted after its original obligation.
    pub fn release_pending_execution_group(&mut self, group: &ContextExecutionGroupId) -> bool {
        if self.has_replay_owner() {
            return false;
        }
        self.pending_execution_groups.remove(group).is_some()
    }

    /// Reports runtime-owned pending protection without inspecting model text.
    pub fn execution_group_is_pending(&self, group: &ContextExecutionGroupId) -> bool {
        self.pending_execution_groups.contains_key(group)
    }
}
