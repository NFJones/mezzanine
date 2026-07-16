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
