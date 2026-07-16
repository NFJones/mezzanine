//! Loop continuation policy after terminal provider execution.

use super::super::*;

use super::worker::runtime_execution_is_patch_free;

impl RuntimeSessionService {
    /// Continues or stops an active `/loop` controller after one owned turn settles.
    pub(in crate::runtime) fn follow_up_agent_loop_after_terminal_execution(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let Some(loop_turn) = self.agent_loop_turns.remove(&turn.turn_id) else {
            return Ok(());
        };
        if execution.terminal_state != AgentTurnState::Completed {
            if let Some(state) = self.agent_loops_by_pane.remove(&loop_turn.pane_id) {
                self.restore_agent_loop_parent_conversation(&loop_turn.pane_id, &state)?;
            }
            return Ok(());
        }
        match loop_turn.kind {
            RuntimeAgentLoopTurnKind::Work => {
                let iteration_emitted_apply_patch = self
                    .agent_loops_by_pane
                    .get(&loop_turn.pane_id)
                    .map(|state| state.emitted_apply_patch)
                    .unwrap_or_else(|| !runtime_execution_is_patch_free(execution));
                if !iteration_emitted_apply_patch {
                    if let Some(state) = self.agent_loops_by_pane.remove(&loop_turn.pane_id) {
                        self.restore_agent_loop_parent_conversation(&loop_turn.pane_id, &state)?;
                    }
                    self.append_agent_status_text_to_terminal_buffer(
                        &loop_turn.pane_id,
                        &format!(
                            "loop: completed after patch-free iteration {}/{}",
                            loop_turn.iteration,
                            self.agent_loop_limit.max(1)
                        ),
                    )?;
                    return Ok(());
                }
                let Some(mut state) = self.agent_loops_by_pane.get(&loop_turn.pane_id).cloned()
                else {
                    return Ok(());
                };
                if state.iteration >= state.max_iterations {
                    if let Some(state) = self.agent_loops_by_pane.remove(&loop_turn.pane_id) {
                        self.restore_agent_loop_parent_conversation(&loop_turn.pane_id, &state)?;
                    }
                    self.append_agent_status_text_to_terminal_buffer(
                        &loop_turn.pane_id,
                        &format!(
                            "loop: reached iteration limit {}/{} after apply_patch work",
                            state.iteration, state.max_iterations
                        ),
                    )?;
                    return Ok(());
                }
                state.iteration = state.iteration.saturating_add(1);
                state.emitted_apply_patch = false;
                self.agent_loops_by_pane
                    .insert(loop_turn.pane_id.clone(), state.clone());
                if let Err(error) = self.start_agent_loop_work_turn(&loop_turn.pane_id) {
                    if let Some(state) = self.agent_loops_by_pane.remove(&loop_turn.pane_id) {
                        self.restore_agent_loop_parent_conversation(&loop_turn.pane_id, &state)?;
                    }
                    return Err(error);
                }
                self.append_agent_status_text_to_terminal_buffer(
                    &loop_turn.pane_id,
                    &format!(
                        "loop: continuing fresh iteration {}/{}",
                        state.iteration, state.max_iterations
                    ),
                )?;
            }
        }
        Ok(())
    }

    /// Reports whether a completed loop work turn will schedule another iteration.
    pub(in crate::runtime) fn agent_loop_execution_will_continue(
        &self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> bool {
        let Some(loop_turn) = self.agent_loop_turns.get(&turn.turn_id) else {
            return false;
        };
        if execution.terminal_state != AgentTurnState::Completed {
            return false;
        }
        let Some(state) = self.agent_loops_by_pane.get(&loop_turn.pane_id) else {
            return false;
        };
        let emitted_apply_patch =
            state.emitted_apply_patch || !runtime_execution_is_patch_free(execution);
        emitted_apply_patch && state.iteration < state.max_iterations
    }
}
