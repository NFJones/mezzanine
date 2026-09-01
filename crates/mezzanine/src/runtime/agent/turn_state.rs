//! Agent shell-session, turn-ledger, context, and execution storage access.
//!
//! These four stores form the mutable core of application-side agent turns.
//! They are private fields of `RuntimeAgentComponent`; crate-local accessors
//! keep their ownership explicit while callers migrate toward narrower turn
//! lifecycle operations.

use super::{RuntimeAgentComponent, RuntimeSessionService};
use mez_agent::{AgentContext, AgentShellStore, AgentTurnExecution, AgentTurnLedger};

impl RuntimeAgentComponent {
    /// Splits the mutable shell store and read-only ledger used by control dispatch.
    pub(crate) fn control_turn_state(&mut self) -> (&mut AgentShellStore, &AgentTurnLedger) {
        (&mut self.agent_shell_store, &self.agent_turn_ledger)
    }
}

impl RuntimeSessionService {
    /// Returns pane-scoped agent shell sessions for read-only inspection.
    pub(crate) fn agent_shell_store(&self) -> &AgentShellStore {
        &self.agent.agent_shell_store
    }

    /// Returns pane-scoped agent shell sessions for lifecycle mutation.
    pub(crate) fn agent_shell_store_mut(&mut self) -> &mut AgentShellStore {
        &mut self.agent.agent_shell_store
    }

    /// Returns the canonical agent turn ledger for read-only inspection.
    pub(crate) fn agent_turn_ledger(&self) -> &AgentTurnLedger {
        &self.agent.agent_turn_ledger
    }

    /// Returns the canonical agent turn ledger for lifecycle mutation.
    pub(crate) fn agent_turn_ledger_mut(&mut self) -> &mut AgentTurnLedger {
        &mut self.agent.agent_turn_ledger
    }

    /// Returns assembled provider contexts keyed by turn id.
    pub(crate) fn agent_turn_contexts(&self) -> &std::collections::BTreeMap<String, AgentContext> {
        &self.agent.agent_turn_contexts
    }

    /// Returns assembled provider contexts for agent-internal mutation.
    pub(crate) fn agent_turn_contexts_mut(
        &mut self,
    ) -> &mut std::collections::BTreeMap<String, AgentContext> {
        &mut self.agent.agent_turn_contexts
    }

    /// Records the replayed-history prefix length for one active turn.
    pub(crate) fn set_agent_turn_imported_history_events(
        &mut self,
        turn_id: impl Into<String>,
        event_count: usize,
    ) {
        self.agent
            .agent_turn_imported_history_events
            .insert(turn_id.into(), event_count);
    }

    /// Returns the replayed-history prefix length retained for one active turn.
    pub(crate) fn agent_turn_imported_history_events(&self, turn_id: &str) -> usize {
        self.agent
            .agent_turn_imported_history_events
            .get(turn_id)
            .copied()
            .unwrap_or(0)
    }

    /// Records one newly appended environment snapshot for atomic turn persistence.
    pub(crate) fn set_agent_turn_environment_snapshot(
        &mut self,
        turn_id: impl Into<String>,
        content: impl Into<String>,
    ) {
        self.agent
            .agent_turn_environment_snapshots
            .insert(turn_id.into(), content.into());
    }

    /// Records the frozen current environment projection for one active turn.
    pub(crate) fn set_agent_turn_current_environment_snapshot(
        &mut self,
        turn_id: impl Into<String>,
        content: impl Into<String>,
    ) {
        self.agent
            .agent_turn_current_environment_snapshots
            .insert(turn_id.into(), content.into());
    }

    /// Returns the frozen current environment projection for one active turn.
    pub(crate) fn agent_turn_current_environment_snapshot(&self, turn_id: &str) -> Option<&str> {
        self.agent
            .agent_turn_current_environment_snapshots
            .get(turn_id)
            .map(String::as_str)
    }

    /// Reports whether one turn appended a new durable environment transition.
    pub(crate) fn agent_turn_has_new_environment_snapshot(&self, turn_id: &str) -> bool {
        self.agent
            .agent_turn_environment_snapshots
            .contains_key(turn_id)
    }

    /// Returns action execution state keyed by turn id.
    pub(crate) fn agent_turn_executions(
        &self,
    ) -> &std::collections::BTreeMap<String, AgentTurnExecution> {
        &self.agent.agent_turn_executions
    }

    /// Returns action execution state for agent-internal mutation.
    pub(crate) fn agent_turn_executions_mut(
        &mut self,
    ) -> &mut std::collections::BTreeMap<String, AgentTurnExecution> {
        &mut self.agent.agent_turn_executions
    }
}
