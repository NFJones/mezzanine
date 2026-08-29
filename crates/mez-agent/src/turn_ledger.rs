//! Provider-independent agent turn records and ledger state machine.
//!
//! This module owns the agent turn boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use crate::{
    AgentTurnLedgerError, AgentTurnLedgerResult, AgentTurnState, AgentTurnTrigger,
    validate_turn_required,
};

// Agent turn records and ledger.

/// Defines the MAX TERMINAL TURNS RETAINED const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const MAX_TERMINAL_TURNS_RETAINED: usize = 4096;

/// Carries Agent Turn Record state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTurnRecord {
    /// Stores the turn id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub turn_id: String,
    /// Immutable conversation that owns this turn for its full lifecycle.
    pub conversation_id: String,
    /// Stores the agent id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub agent_id: String,
    /// Stores the pane id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub pane_id: String,
    /// Stores the trigger value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub trigger: AgentTurnTrigger,
    /// Stores the started at unix seconds value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub started_at_unix_seconds: u64,
    /// Absolute Unix-millisecond deadline snapshotted when this turn is created.
    ///
    /// A zero value is accepted for synthetic and compatibility records and
    /// derives the historical default deadline from `started_at_unix_seconds`.
    pub deadline_at_unix_millis: u64,
    /// Stores the policy profile value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub policy_profile: String,
    /// Stores the model profile value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub model_profile: String,
    /// Stores the parent turn id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub parent_turn_id: Option<String>,
    /// Stores the state value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub state: AgentTurnState,
    /// Stores the cooperation mode value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub cooperation_mode: Option<String>,
    /// Optional capability to pre-seed the initial allowed-action surface.
    ///
    /// When set, the first provider request uses `AllowedActionSet::for_capability`
    /// instead of `capability_decision()`, so the model can emit executable actions
    /// without a separate capability-request round-trip.
    pub initial_capability: Option<crate::AgentCapability>,
}

impl crate::AgentTurnResultIdentity for AgentTurnRecord {
    fn turn_id(&self) -> &str {
        &self.turn_id
    }

    fn agent_id(&self) -> &str {
        &self.agent_id
    }
}

/// Carries Agent Turn Ledger state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTurnLedger {
    /// Stores the allow concurrent turns value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) allow_concurrent_turns: bool,
    /// Stores the turns value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) turns: Vec<AgentTurnRecord>,
    /// Exact retained index for each turn identifier.
    turn_indices: std::collections::BTreeMap<String, usize>,
    /// Newest retained turn identifier for each pane.
    latest_turn_by_pane: std::collections::BTreeMap<String, String>,
    /// Running turn identifiers used by animation-time status queries.
    running_turn_ids: std::collections::BTreeSet<String>,
    /// Number of running turns currently owned by each pane.
    running_turn_counts_by_pane: std::collections::BTreeMap<String, usize>,
    /// Monotonic generation advanced by semantic ledger mutations.
    semantic_generation: u64,
}

impl Default for AgentTurnLedger {
    fn default() -> Self {
        Self::new(false)
    }
}

impl AgentTurnLedger {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(allow_concurrent_turns: bool) -> Self {
        Self {
            allow_concurrent_turns,
            turns: Vec::new(),
            turn_indices: std::collections::BTreeMap::new(),
            latest_turn_by_pane: std::collections::BTreeMap::new(),
            running_turn_ids: std::collections::BTreeSet::new(),
            running_turn_counts_by_pane: std::collections::BTreeMap::new(),
            semantic_generation: 0,
        }
    }

    /// Runs the queue turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn queue_turn(&mut self, mut turn: AgentTurnRecord) -> AgentTurnLedgerResult<()> {
        validate_turn_required("turn_id", &turn.turn_id)?;
        validate_turn_required("conversation_id", &turn.conversation_id)?;
        validate_turn_required("agent_id", &turn.agent_id)?;
        validate_turn_required("pane_id", &turn.pane_id)?;
        if self.turn_indices.contains_key(&turn.turn_id) {
            return Err(AgentTurnLedgerError::conflict(
                "agent turn id already exists",
            ));
        }
        turn.state = AgentTurnState::Queued;
        self.turns.push(turn);
        self.index_appended_turn();
        Ok(())
    }

    /// Runs the mark turn running operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn mark_turn_running(&mut self, turn_id: &str) -> AgentTurnLedgerResult<()> {
        let index = self
            .turn_indices
            .get(turn_id)
            .copied()
            .ok_or_else(|| AgentTurnLedgerError::not_found("turn not found"))?;
        if self.turns[index].state != AgentTurnState::Queued {
            return Err(AgentTurnLedgerError::conflict("agent turn is not queued"));
        }
        let agent_id = self.turns[index].agent_id.clone();
        if !self.allow_concurrent_turns
            && self.turns.iter().any(|existing| {
                existing.agent_id == agent_id
                    && existing.state == AgentTurnState::Running
                    && existing.turn_id != turn_id
            })
        {
            return Err(AgentTurnLedgerError::conflict(
                "agent already has a running turn and concurrent turns are disabled",
            ));
        }
        let turn = self
            .turns
            .get_mut(index)
            .ok_or_else(|| AgentTurnLedgerError::not_found("turn not found"))?;
        let pane_id = turn.pane_id.clone();
        let previous = turn.state;
        turn.state = AgentTurnState::Running;
        self.index_state_transition(turn_id, &pane_id, previous, AgentTurnState::Running);
        Ok(())
    }

    /// Runs the start turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn start_turn(&mut self, mut turn: AgentTurnRecord) -> AgentTurnLedgerResult<()> {
        if !self.allow_concurrent_turns
            && self.turns.iter().any(|existing| {
                existing.agent_id == turn.agent_id && existing.state == AgentTurnState::Running
            })
        {
            return Err(AgentTurnLedgerError::conflict(
                "agent already has a running turn and concurrent turns are disabled",
            ));
        }
        validate_turn_required("turn_id", &turn.turn_id)?;
        validate_turn_required("conversation_id", &turn.conversation_id)?;
        validate_turn_required("agent_id", &turn.agent_id)?;
        validate_turn_required("pane_id", &turn.pane_id)?;
        if self.turn_indices.contains_key(&turn.turn_id) {
            return Err(AgentTurnLedgerError::conflict(
                "agent turn id already exists",
            ));
        }
        turn.state = AgentTurnState::Running;
        self.turns.push(turn);
        self.index_appended_turn();
        Ok(())
    }

    /// Runs the finish turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn finish_turn(
        &mut self,
        turn_id: &str,
        state: AgentTurnState,
    ) -> AgentTurnLedgerResult<()> {
        if !matches!(
            state,
            AgentTurnState::Completed
                | AgentTurnState::Failed
                | AgentTurnState::Blocked
                | AgentTurnState::Interrupted
        ) {
            return Err(AgentTurnLedgerError::invalid_args(
                "finish_turn requires a terminal or blocked turn state",
            ));
        }
        let index = self
            .turn_indices
            .get(turn_id)
            .copied()
            .ok_or_else(|| AgentTurnLedgerError::not_found("turn not found"))?;
        let turn = self
            .turns
            .get_mut(index)
            .ok_or_else(|| AgentTurnLedgerError::not_found("turn not found"))?;
        if terminal_turn_state(turn.state) {
            return Err(AgentTurnLedgerError::conflict(
                "agent turn is already terminal",
            ));
        }
        let pane_id = turn.pane_id.clone();
        let previous = turn.state;
        turn.state = state;
        self.index_state_transition(turn_id, &pane_id, previous, state);
        self.enforce_retention();
        Ok(())
    }

    /// Runs the resume blocked turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn resume_blocked_turn(&mut self, turn_id: &str) -> AgentTurnLedgerResult<()> {
        let index = self
            .turn_indices
            .get(turn_id)
            .copied()
            .ok_or_else(|| AgentTurnLedgerError::not_found("turn not found"))?;
        let turn = self
            .turns
            .get_mut(index)
            .ok_or_else(|| AgentTurnLedgerError::not_found("turn not found"))?;
        if turn.state != AgentTurnState::Blocked {
            return Err(AgentTurnLedgerError::conflict("agent turn is not blocked"));
        }
        let pane_id = turn.pane_id.clone();
        let previous = turn.state;
        turn.state = AgentTurnState::Running;
        self.index_state_transition(turn_id, &pane_id, previous, AgentTurnState::Running);
        Ok(())
    }

    /// Replaces the action capability used for subsequent requests of one turn.
    ///
    /// This supports bounded workflow continuations that must narrow an
    /// already-running turn to a response-only action surface.
    pub fn set_turn_capability(
        &mut self,
        turn_id: &str,
        capability: crate::AgentCapability,
    ) -> AgentTurnLedgerResult<()> {
        let index = self
            .turn_indices
            .get(turn_id)
            .copied()
            .ok_or_else(|| AgentTurnLedgerError::not_found("turn not found"))?;
        let turn = self
            .turns
            .get_mut(index)
            .ok_or_else(|| AgentTurnLedgerError::not_found("turn not found"))?;
        if terminal_turn_state(turn.state) {
            return Err(AgentTurnLedgerError::conflict(
                "cannot change capability for a terminal turn",
            ));
        }
        turn.initial_capability = Some(capability);
        Ok(())
    }

    /// Runs the turns operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn turns(&self) -> &[AgentTurnRecord] {
        &self.turns
    }

    /// Returns one retained turn by exact identifier in logarithmic time.
    pub fn turn(&self, turn_id: &str) -> Option<&AgentTurnRecord> {
        self.turn_indices
            .get(turn_id)
            .and_then(|index| self.turns.get(*index))
    }

    /// Returns whether one exact retained turn is currently running.
    pub fn turn_is_running(&self, turn_id: &str) -> bool {
        self.running_turn_ids.contains(turn_id)
    }

    /// Returns the newest retained turn for one pane without scanning history.
    pub fn latest_turn_for_pane(&self, pane_id: &str) -> Option<&AgentTurnRecord> {
        self.latest_turn_by_pane
            .get(pane_id)
            .and_then(|turn_id| self.turn(turn_id))
    }

    /// Counts running turns for a bounded pane set without scanning retained turns.
    pub fn running_turn_count_for_panes<'a>(
        &self,
        pane_ids: impl IntoIterator<Item = &'a str>,
    ) -> usize {
        pane_ids
            .into_iter()
            .map(|pane_id| {
                self.running_turn_counts_by_pane
                    .get(pane_id)
                    .copied()
                    .unwrap_or_default()
            })
            .sum()
    }

    /// Returns the generation of indexed semantic ledger state.
    pub fn semantic_generation(&self) -> u64 {
        self.semantic_generation
    }

    /// Registers one newly appended turn in every derived index.
    fn index_appended_turn(&mut self) {
        let Some((index, turn)) = self
            .turns
            .len()
            .checked_sub(1)
            .and_then(|index| self.turns.get(index).map(|turn| (index, turn)))
        else {
            return;
        };
        self.turn_indices.insert(turn.turn_id.clone(), index);
        self.latest_turn_by_pane
            .insert(turn.pane_id.clone(), turn.turn_id.clone());
        if turn.state == AgentTurnState::Running {
            self.running_turn_ids.insert(turn.turn_id.clone());
            *self
                .running_turn_counts_by_pane
                .entry(turn.pane_id.clone())
                .or_default() += 1;
        }
        self.semantic_generation = self.semantic_generation.wrapping_add(1);
    }

    /// Applies one retained turn state transition to the running indexes.
    fn index_state_transition(
        &mut self,
        turn_id: &str,
        pane_id: &str,
        previous: AgentTurnState,
        current: AgentTurnState,
    ) {
        if previous == current {
            return;
        }
        if previous == AgentTurnState::Running {
            self.running_turn_ids.remove(turn_id);
            if let Some(count) = self.running_turn_counts_by_pane.get_mut(pane_id) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.running_turn_counts_by_pane.remove(pane_id);
                }
            }
        }
        if current == AgentTurnState::Running {
            self.running_turn_ids.insert(turn_id.to_string());
            *self
                .running_turn_counts_by_pane
                .entry(pane_id.to_string())
                .or_default() += 1;
        }
        self.semantic_generation = self.semantic_generation.wrapping_add(1);
    }

    /// Rebuilds derived indexes after retention changes retained vector offsets.
    fn rebuild_indexes(&mut self) {
        self.turn_indices.clear();
        self.latest_turn_by_pane.clear();
        self.running_turn_ids.clear();
        self.running_turn_counts_by_pane.clear();
        for (index, turn) in self.turns.iter().enumerate() {
            self.turn_indices.insert(turn.turn_id.clone(), index);
            self.latest_turn_by_pane
                .insert(turn.pane_id.clone(), turn.turn_id.clone());
            if turn.state == AgentTurnState::Running {
                self.running_turn_ids.insert(turn.turn_id.clone());
                *self
                    .running_turn_counts_by_pane
                    .entry(turn.pane_id.clone())
                    .or_default() += 1;
            }
        }
    }

    /// Runs the enforce retention operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn enforce_retention(&mut self) {
        let terminal_count = self
            .turns
            .iter()
            .filter(|turn| terminal_turn_state(turn.state))
            .count();
        let mut terminal_to_remove = terminal_count.saturating_sub(MAX_TERMINAL_TURNS_RETAINED);
        if terminal_to_remove == 0 {
            return;
        }
        self.turns.retain(|turn| {
            if terminal_to_remove > 0 && terminal_turn_state(turn.state) {
                terminal_to_remove -= 1;
                return false;
            }
            true
        });
        self.rebuild_indexes();
        self.semantic_generation = self.semantic_generation.wrapping_add(1);
    }
}

/// Runs the terminal turn state operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn terminal_turn_state(state: AgentTurnState) -> bool {
    matches!(
        state,
        AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
    )
}

#[cfg(test)]
mod tests;
