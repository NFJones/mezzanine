//! Loop settlement policy after terminal provider execution.

use super::super::{
    AgentTurnExecution, AgentTurnRecord, AgentTurnState, Result, RuntimeAgentLoopSettlement,
    RuntimeAgentLoopTurnKind, RuntimeSessionService,
};
use mez_agent::outcome::runtime_execution_has_apply_patch_action;

impl RuntimeSessionService {
    /// Consumes one terminal loop-owned turn and either continues or terminates its logical loop.
    pub(crate) fn settle_agent_loop_after_terminal_execution(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<RuntimeAgentLoopSettlement> {
        let Some(loop_turn) = self.agent.agent_loop_turns.remove(&turn.turn_id) else {
            return Ok(RuntimeAgentLoopSettlement::NotOwned);
        };
        if execution.terminal_state != AgentTurnState::Completed {
            let completion =
                if let Some(state) = self.remove_agent_loop_state_by_id(&loop_turn.loop_id) {
                    self.restore_agent_loop_parent_conversation(&state.invoking_pane_id, &state)?;
                    state.completion
                } else {
                    None
                };
            return Ok(RuntimeAgentLoopSettlement::Terminal { completion });
        }
        match loop_turn.kind {
            RuntimeAgentLoopTurnKind::Work => {
                let Some(state) = self.agent_loop_state_by_id(&loop_turn.loop_id).cloned() else {
                    return Ok(RuntimeAgentLoopSettlement::Terminal { completion: None });
                };
                let emitted_apply_patch = state.emitted_apply_patch
                    || runtime_execution_has_apply_patch_action(execution);
                if !emitted_apply_patch {
                    let state = self
                        .remove_agent_loop_state_by_id(&loop_turn.loop_id)
                        .expect("logical loop state was read immediately before removal");
                    self.restore_agent_loop_parent_conversation(&state.invoking_pane_id, &state)?;
                    self.append_agent_status_text_to_terminal_buffer(
                        &state.invoking_pane_id,
                        &format!(
                            "loop: completed after patch-free iteration {}/{}",
                            loop_turn.iteration,
                            self.agent.agent_loop_limit.max(1)
                        ),
                    )?;
                    return Ok(RuntimeAgentLoopSettlement::Terminal {
                        completion: state.completion,
                    });
                }
                if state.iteration >= state.max_iterations {
                    let state = self
                        .remove_agent_loop_state_by_id(&loop_turn.loop_id)
                        .expect("logical loop state was read immediately before removal");
                    self.restore_agent_loop_parent_conversation(&state.invoking_pane_id, &state)?;
                    self.append_agent_status_text_to_terminal_buffer(
                        &state.invoking_pane_id,
                        &format!(
                            "loop: reached iteration limit {}/{} after apply_patch work",
                            state.iteration, state.max_iterations
                        ),
                    )?;
                    return Ok(RuntimeAgentLoopSettlement::Terminal {
                        completion: state.completion,
                    });
                }
                let mut next_state = state;
                next_state.iteration = next_state.iteration.saturating_add(1);
                next_state.emitted_apply_patch = false;
                *self
                    .agent_loop_state_mut_by_id(&loop_turn.loop_id)
                    .expect("logical loop state was read immediately before update") =
                    next_state.clone();
                if let Err(error) = self.start_agent_loop_work_turn(&next_state.execution_pane_id) {
                    if let Some(state) = self.remove_agent_loop_state_by_id(&loop_turn.loop_id) {
                        self.restore_agent_loop_parent_conversation(
                            &state.invoking_pane_id,
                            &state,
                        )?;
                        if let Some(parent_turn_id) = state.routed_parent_turn_id.as_deref() {
                            self.fail_routed_loop_continuation(
                                parent_turn_id,
                                &turn.turn_id,
                                error.message(),
                            )?;
                            return Ok(RuntimeAgentLoopSettlement::Terminal {
                                completion: state.completion,
                            });
                        }
                    }
                    return Err(error);
                }
                self.append_agent_status_text_to_terminal_buffer(
                    &next_state.invoking_pane_id,
                    &format!(
                        "loop: continuing fresh iteration {}/{}",
                        next_state.iteration, next_state.max_iterations
                    ),
                )?;
                Ok(RuntimeAgentLoopSettlement::Continued)
            }
        }
    }
}
