//! Configured hook queues, transactions, and bounded results.

use std::collections::BTreeMap;

use crate::hooks::{FocusedShellHookQueue, HookDefinition, HookExecutionResult};
use crate::runtime::service_state::PendingFocusedShellHookTransaction;

/// Owns configured hooks and focused-shell hook execution state.
#[derive(Debug)]
pub(super) struct RuntimeHookState {
    definitions: Vec<HookDefinition>,
    focused_shell_queue: FocusedShellHookQueue,
    next_focused_shell_marker: u64,
    focused_shell_transactions: BTreeMap<String, PendingFocusedShellHookTransaction>,
    focused_shell_results: Vec<HookExecutionResult>,
}

impl Default for RuntimeHookState {
    fn default() -> Self {
        Self {
            definitions: Vec::new(),
            focused_shell_queue: FocusedShellHookQueue::default(),
            next_focused_shell_marker: 1,
            focused_shell_transactions: BTreeMap::new(),
            focused_shell_results: Vec::new(),
        }
    }
}

impl RuntimeHookState {
    pub(super) fn definitions(&self) -> &[HookDefinition] {
        &self.definitions
    }

    pub(super) fn replace_definitions(&mut self, definitions: Vec<HookDefinition>) {
        self.definitions = definitions;
    }

    pub(super) fn focused_shell_queue(&self) -> &FocusedShellHookQueue {
        &self.focused_shell_queue
    }

    pub(super) fn focused_shell_queue_mut(&mut self) -> &mut FocusedShellHookQueue {
        &mut self.focused_shell_queue
    }

    pub(super) fn replace_focused_shell_queue(&mut self, queue: FocusedShellHookQueue) {
        self.focused_shell_queue = queue;
    }

    pub(super) fn allocate_focused_shell_marker(&mut self) -> u64 {
        let marker = self.next_focused_shell_marker;
        self.next_focused_shell_marker = self.next_focused_shell_marker.saturating_add(1);
        marker
    }

    pub(super) fn focused_shell_transactions(
        &self,
    ) -> &BTreeMap<String, PendingFocusedShellHookTransaction> {
        &self.focused_shell_transactions
    }

    pub(super) fn focused_shell_transactions_mut(
        &mut self,
    ) -> &mut BTreeMap<String, PendingFocusedShellHookTransaction> {
        &mut self.focused_shell_transactions
    }

    pub(super) fn focused_shell_results(&self) -> &[HookExecutionResult] {
        &self.focused_shell_results
    }

    pub(super) fn focused_shell_results_mut(&mut self) -> &mut Vec<HookExecutionResult> {
        &mut self.focused_shell_results
    }
}
