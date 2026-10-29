//! Agent shell-session, turn-ledger, context, and execution storage access.
//!
//! These four stores form the mutable core of application-side agent turns.
//! They are private fields of `RuntimeAgentComponent`; crate-local accessors
//! keep their ownership explicit while callers migrate toward narrower turn
//! lifecycle operations. Native workspace intent is selected through the exact
//! session owner or installed same-pane loop binding without rewriting observed
//! pane cwd or borrowing another pane.

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

    /// Returns the agent workspace directory without rewriting pane observations.
    /// Exact native navigation intent (including controller-owned ephemeral
    /// loop work) wins only in Native mode; initial state,
    /// other modes and panes without an agent retain the existing best-known
    /// pane-directory fallback. The launch owner independently qualifies its
    /// initial native root context; this getter grants no filesystem authority.
    pub(crate) fn agent_effective_working_directory(
        &self,
        pane_id: &str,
    ) -> Option<std::path::PathBuf> {
        if let Some(directory) = self.native_agent_working_directory(pane_id) {
            return Some(directory.to_path_buf());
        }
        self.pane_current_working_directory(pane_id)
    }

    /// Selects explicit native workspace data for an exact session owner or
    /// the current work CID installed by its same-pane loop controller. The
    /// frozen parent workspace survives private history isolation; copied
    /// transcript-source labels, unrelated CIDs and other panes do not inherit
    /// it. This selection grants no filesystem, cleanup or parent-turn rights.
    pub(crate) fn native_agent_working_directory(&self, pane_id: &str) -> Option<&std::path::Path> {
        self.native_agent_directory_snapshot(pane_id)
            .map(|snapshot| snapshot.directory())
    }

    /// Returns one qualified cwd/discovery generation for direct native work or
    /// an actual same-pane loop binding. Consumers share this lookup so paths
    /// and instruction sources cannot diverge across private history isolation.
    pub(crate) fn native_agent_directory_snapshot(
        &self,
        pane_id: &str,
    ) -> Option<&mez_agent::AgentNativeWorkingDirectory> {
        if self.effective_agent_shell_mode_for_pane(pane_id)
            != crate::runtime::config::ShellMode::Native
        {
            return None;
        }
        let session = self.agent_shell_store().get(pane_id)?;
        if session.pane_id != pane_id {
            return None;
        }
        if let Some(snapshot) = session.native_directory_snapshot() {
            return Some(snapshot);
        }
        let controller = self.agent_loop_state(pane_id)?;
        if !session.ephemeral
            || controller.invoking_pane_id != pane_id
            || controller.execution_pane_id != pane_id
            || controller.work_conversation_id.as_deref() != Some(session.session_id.as_str())
        {
            return None;
        }
        controller
            .parent_native_directory
            .as_ref()
            .filter(|snapshot| snapshot.is_owned_by(pane_id, &controller.parent_conversation_id))
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

    /// Records the replayed-history sequence boundary for one active turn.
    pub(crate) fn set_agent_turn_imported_history_sequence_high_water(
        &mut self,
        turn_id: impl Into<String>,
        sequence_high_water: u64,
    ) {
        self.agent
            .agent_turn_imported_history_sequence_high_water
            .insert(turn_id.into(), sequence_high_water);
    }

    /// Returns the replayed-history sequence boundary retained for one active turn.
    pub(crate) fn agent_turn_imported_history_sequence_high_water(&self, turn_id: &str) -> u64 {
        self.agent
            .agent_turn_imported_history_sequence_high_water
            .get(turn_id)
            .copied()
            .unwrap_or_default()
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
