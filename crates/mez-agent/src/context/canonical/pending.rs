//! Runtime-owned protection for execution groups with outstanding obligations.
//!
//! Later peer mail or a separate assistant response does not close an earlier
//! joined task. The runtime retains that exact group until its final dependency
//! retires. In-memory allocations survive checked clones and preparation; a new
//! protection interval receives a fresh allocation even with unchanged source.
//! The map preserves idempotent retention within the current interval,
//! but supplies no action, provider or completion authority. Descriptive interval
//! identity may be checkpointed; the live allocation never crosses persistence
//! or appears in model-visible messages.

use super::epoch::ExecutionInterval;
use super::{
    AgentContext, AgentContextError, AgentContextResult, ContextEventSequence,
    ContextExecutionGroupId, ContextSemanticKind, ContextSourceKind, ConversationEvent,
};

/// Controller observations carry no assistant/action/native obligation. This
/// checks typed metadata, never a display label or a claimed completion string.
pub(super) fn observation_only_event(event: &ConversationEvent) -> bool {
    event.block().source == ContextSourceKind::CommittedEvidence
        && event.semantic_kind() == ContextSemanticKind::EvidenceEvent
        && event.provider_owner().is_none()
}

/// One opaque in-memory protection interval. Clones share its allocation;
/// renewing protection after release allocates another one even at the same
/// frontier. Its allocation is never persisted or interpreted as effect authority.
#[derive(Debug, Clone)]
pub(super) struct ContextExecutionProtection {
    allocation: std::sync::Arc<()>,
    interval: ExecutionInterval,
}

impl ContextExecutionProtection {
    /// Restores descriptive identity inside a fresh, non-reusable live allocation.
    pub(super) fn new(interval: ExecutionInterval) -> Self {
        Self {
            allocation: std::sync::Arc::new(()),
            interval,
        }
    }

    pub(super) fn interval(&self) -> &ExecutionInterval {
        &self.interval
    }

    /// Original-source qualification refines a provisional description, not the
    /// already admitted live protection interval. Its captured fence stays valid.
    pub(super) fn with_interval(&self, interval: ExecutionInterval) -> Self {
        Self {
            allocation: self.allocation.clone(),
            interval,
        }
    }
}

impl PartialEq for ContextExecutionProtection {
    /// Compares the original allocation, not its unit payload or public labels.
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.allocation, &other.allocation)
    }
}
impl Eq for ContextExecutionProtection {}

impl AgentContext {
    /// Nonempty groups consisting solely of runtime observations cannot mint
    /// execution fences. Existing protection is never released by this query.
    pub(super) fn execution_group_is_observational(&self, group: &ContextExecutionGroupId) -> bool {
        let mut members = self
            .chronology()
            .iter()
            .filter(|event| event.execution_group_id() == Some(group))
            .peekable();
        members.peek().is_some() && members.all(observation_only_event)
    }

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
        let protection = match previous {
            Some(previous) => previous,
            None => ContextExecutionProtection::new(ExecutionInterval::after(
                self.execution_intervals.get(group),
            )?),
        };
        if let Some(continuation) = &mut self.replay_continuation {
            continuation.protect_group(group, &protection, renewed)?;
        }
        self.pending_execution_groups
            .insert(group.clone(), protection.clone());
        self.execution_intervals
            .insert(group.clone(), protection.interval().clone());
        Ok(protection)
    }

    /// Imported source has no local producer interval until separately qualified
    /// lifecycle metadata binds it. Keep a fresh live protection allocation;
    /// arbitrary imported bytes cannot export a new original completion claim.
    pub(super) fn protect_unqualified_imported_group(
        &mut self,
        group: &ContextExecutionGroupId,
    ) -> AgentContextResult<()> {
        let interval = ExecutionInterval::unqualified_after(self.execution_intervals.get(group));
        let protection = ContextExecutionProtection::new(interval.clone());
        if let Some(continuation) = &mut self.replay_continuation {
            continuation.protect_group(group, &protection, true)?;
        }
        self.execution_intervals.insert(group.clone(), interval);
        self.pending_execution_groups
            .insert(group.clone(), protection);
        Ok(())
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
        let released = self.pending_execution_groups.remove(group).is_some();
        if released {
            // Unowned compatibility context has no canonical producer receipt.
            // Fenced epoch retirement retains descriptive ancestry separately.
            self.execution_intervals.remove(group);
        }
        released
    }

    /// Reports runtime-owned pending protection without inspecting model text.
    pub fn execution_group_is_pending(&self, group: &ContextExecutionGroupId) -> bool {
        self.pending_execution_groups.contains_key(group)
    }
}
