//! Queueing for hooks that must run inside the focused shell.
//!
//! The queue preserves ordering for shell-bound hooks and blocks dispatch when a
//! required focused shell is unavailable.

use crate::error::{MezError, Result};

use super::execution::execute_focused_shell_hook;
use super::types::{
    FocusedShellExecutor, FocusedShellHookDispatch, FocusedShellHookDispatchStatus,
    FocusedShellHookQueue, FocusedShellHookQueueEntry, HookExecutionPlan,
};

impl FocusedShellHookQueue {
    /// Runs the enqueue operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn enqueue(&mut self, plan: HookExecutionPlan) -> Result<u64> {
        if !plan.run_in_focused_shell {
            return Err(MezError::invalid_args(
                "focused-shell hook queue accepts only focused-shell hook plans",
            ));
        }
        let sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("focused-shell hook sequence overflow"))?;
        self.next_sequence = sequence;
        self.pending
            .push_back(FocusedShellHookQueueEntry { sequence, plan });
        Ok(sequence)
    }

    /// Runs the dispatch next operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn dispatch_next(
        &mut self,
        shell_available: bool,
        executor: &mut impl FocusedShellExecutor,
    ) -> Result<Option<FocusedShellHookDispatch>> {
        let Some(entry) = self.pending.front().cloned() else {
            return Ok(None);
        };
        if entry.plan.blocks_on_shell_availability && !shell_available {
            return Ok(Some(FocusedShellHookDispatch {
                sequence: entry.sequence,
                status: FocusedShellHookDispatchStatus::BlockedOnShell,
                result: None,
                hook_id: entry.plan.hook_id.clone(),
            }));
        }
        let entry = self
            .pending
            .pop_front()
            .ok_or_else(|| MezError::invalid_state("focused-shell hook queue lost entry"))?;
        let result = execute_focused_shell_hook(&entry.plan, executor)?;
        Ok(Some(FocusedShellHookDispatch {
            sequence: entry.sequence,
            status: FocusedShellHookDispatchStatus::Executed,
            result: Some(result),
            hook_id: entry.plan.hook_id.clone(),
        }))
    }

    /// Runs the len operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Runs the is empty operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Runs the front plan operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn front_plan(&self) -> Option<&HookExecutionPlan> {
        self.pending.front().map(|entry| &entry.plan)
    }
}
