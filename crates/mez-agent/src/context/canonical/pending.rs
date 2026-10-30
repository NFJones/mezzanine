//! Runtime-owned protection for execution groups with outstanding obligations.
//!
//! Later peer mail or a separate assistant response does not close an earlier
//! joined task. The runtime retains that exact group until its final dependency
//! retires. This in-memory set survives checked context clones and preparation,
//! but supplies no action, provider or completion authority and is never part of
//! persisted transcript metadata or model-visible messages.

use super::{AgentContext, AgentContextError, AgentContextResult, ContextExecutionGroupId};

impl AgentContext {
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
