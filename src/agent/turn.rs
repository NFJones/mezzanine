//! Agent Turn implementation.
//!
//! This module owns the agent turn boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{AgentTurnState, AgentTurnTrigger, MezError, Result, validate_non_empty};

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
        }
    }

    /// Runs the queue turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn queue_turn(&mut self, mut turn: AgentTurnRecord) -> Result<()> {
        validate_non_empty("turn_id", &turn.turn_id)?;
        validate_non_empty("agent_id", &turn.agent_id)?;
        validate_non_empty("pane_id", &turn.pane_id)?;
        if self
            .turns
            .iter()
            .any(|existing| existing.turn_id == turn.turn_id)
        {
            return Err(MezError::conflict("agent turn id already exists"));
        }
        turn.state = AgentTurnState::Queued;
        self.turns.push(turn);
        Ok(())
    }

    /// Runs the mark turn running operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn mark_turn_running(&mut self, turn_id: &str) -> Result<()> {
        let index = self
            .turns
            .iter()
            .position(|turn| turn.turn_id == turn_id)
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        if self.turns[index].state != AgentTurnState::Queued {
            return Err(MezError::conflict("agent turn is not queued"));
        }
        let agent_id = self.turns[index].agent_id.clone();
        if !self.allow_concurrent_turns
            && self.turns.iter().any(|existing| {
                existing.agent_id == agent_id
                    && existing.state == AgentTurnState::Running
                    && existing.turn_id != turn_id
            })
        {
            return Err(MezError::conflict(
                "agent already has a running turn and concurrent turns are disabled",
            ));
        }
        let turn = self
            .turns
            .get_mut(index)
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        turn.state = AgentTurnState::Running;
        Ok(())
    }

    /// Runs the start turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn start_turn(&mut self, mut turn: AgentTurnRecord) -> Result<()> {
        if !self.allow_concurrent_turns
            && self.turns.iter().any(|existing| {
                existing.agent_id == turn.agent_id && existing.state == AgentTurnState::Running
            })
        {
            return Err(MezError::conflict(
                "agent already has a running turn and concurrent turns are disabled",
            ));
        }
        validate_non_empty("turn_id", &turn.turn_id)?;
        validate_non_empty("agent_id", &turn.agent_id)?;
        validate_non_empty("pane_id", &turn.pane_id)?;
        if self
            .turns
            .iter()
            .any(|existing| existing.turn_id == turn.turn_id)
        {
            return Err(MezError::conflict("agent turn id already exists"));
        }
        turn.state = AgentTurnState::Running;
        self.turns.push(turn);
        Ok(())
    }

    /// Runs the finish turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn finish_turn(&mut self, turn_id: &str, state: AgentTurnState) -> Result<()> {
        if !matches!(
            state,
            AgentTurnState::Completed
                | AgentTurnState::Failed
                | AgentTurnState::Blocked
                | AgentTurnState::Interrupted
        ) {
            return Err(MezError::invalid_args(
                "finish_turn requires a terminal or blocked turn state",
            ));
        }
        let turn = self
            .turns
            .iter_mut()
            .find(|turn| turn.turn_id == turn_id)
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        turn.state = state;
        self.enforce_retention();
        Ok(())
    }

    /// Runs the resume blocked turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn resume_blocked_turn(&mut self, turn_id: &str) -> Result<()> {
        let turn = self
            .turns
            .iter_mut()
            .find(|turn| turn.turn_id == turn_id)
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        if turn.state != AgentTurnState::Blocked {
            return Err(MezError::conflict("agent turn is not blocked"));
        }
        turn.state = AgentTurnState::Running;
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
