//! Loop settlement policy after terminal provider execution.

use super::super::{
    AgentTurnExecution, AgentTurnRecord, AgentTurnState, MezError, Result,
    RuntimeAgentLoopContinuation, RuntimeAgentLoopSettlement, RuntimeAgentLoopTurnKind,
    RuntimeSessionService,
};
use mez_agent::outcome::runtime_execution_has_apply_patch_action;

/// Returns whether the model's final response declared the configured loop
/// goal satisfied using the controller-owned completion marker.
fn runtime_agent_loop_goal_met(execution: &AgentTurnExecution) -> bool {
    execution.response.action_batch.as_ref().and_then(|batch| {
        batch.actions.iter().rev().find_map(|action| {
            let mez_agent::AgentActionPayload::Say {
                status: mez_agent::SayStatus::Final,
                text,
                ..
            } = &action.payload
            else {
                return None;
            };
            text.lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .map(str::trim)
        })
    }) == Some("[loop goal: met]")
}

impl RuntimeSessionService {
    /// Records that a loop-owned turn emitted an `apply_patch` action.
    ///
    /// Provider continuations replace the active response batch after actions settle, so
    /// loop accounting must retain this fact independently of the terminal batch.
    pub(crate) fn record_agent_loop_apply_patch_for_turn(&mut self, turn_id: &str) {
        let Some(loop_id) = self
            .agent
            .agent_loop_turns
            .get(turn_id)
            .map(|loop_turn| loop_turn.loop_id.clone())
        else {
            return;
        };
        if let Some(state) = self.agent.agent_loops_by_id.get_mut(&loop_id) {
            state.emitted_apply_patch = true;
        }
    }

    /// Consumes one terminal loop-owned turn and either continues or terminates its logical loop.
    pub(crate) fn settle_agent_loop_after_terminal_execution(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<RuntimeAgentLoopSettlement> {
        let Some(loop_turn) = self.agent_loop_turn(&turn.turn_id).cloned() else {
            return Ok(RuntimeAgentLoopSettlement::NotOwned);
        };
        if execution.terminal_state != AgentTurnState::Completed {
            let Some(state) = self.agent_loop_state_by_id(&loop_turn.loop_id).cloned() else {
                return Ok(RuntimeAgentLoopSettlement::Terminal { completion: None });
            };
            let objective = self.preflight_agent_loop_parent_objective(&state)?;
            self.restore_agent_loop_parent_conversation(
                &state.invoking_pane_id,
                &state,
                objective.as_deref(),
            )?;
            self.remove_agent_loop_state_by_id(&loop_turn.loop_id);
            let completion = state.completion;
            return Ok(RuntimeAgentLoopSettlement::Terminal { completion });
        }
        match loop_turn.kind {
            RuntimeAgentLoopTurnKind::Work => {
                let Some(state) = self.agent_loop_state_by_id(&loop_turn.loop_id).cloned() else {
                    return Ok(RuntimeAgentLoopSettlement::Terminal { completion: None });
                };
                let emitted_apply_patch = state.emitted_apply_patch
                    || runtime_execution_has_apply_patch_action(execution);
                let goal_met = state.goal.is_some() && runtime_agent_loop_goal_met(execution);
                let iteration_requires_continuation = if state.goal.is_some() {
                    !goal_met
                } else {
                    emitted_apply_patch
                };
                if !iteration_requires_continuation {
                    let objective = self.preflight_agent_loop_parent_objective(&state)?;
                    self.restore_agent_loop_parent_conversation(
                        &state.invoking_pane_id,
                        &state,
                        objective.as_deref(),
                    )?;
                    self.remove_agent_loop_state_by_id(&loop_turn.loop_id);
                    let completion_reason = if state.goal.is_some() {
                        "goal met"
                    } else {
                        "patch-free"
                    };
                    self.append_agent_status_text_to_terminal_buffer(
                        &state.invoking_pane_id,
                        &format!(
                            "loop: completed after {completion_reason} iteration {}/{}",
                            loop_turn.iteration, state.max_iterations
                        ),
                    )?;
                    return Ok(RuntimeAgentLoopSettlement::Terminal {
                        completion: state.completion,
                    });
                }
                if state.iteration >= state.max_iterations {
                    let objective = self.preflight_agent_loop_parent_objective(&state)?;
                    self.restore_agent_loop_parent_conversation(
                        &state.invoking_pane_id,
                        &state,
                        objective.as_deref(),
                    )?;
                    self.remove_agent_loop_state_by_id(&loop_turn.loop_id);
                    self.append_agent_status_text_to_terminal_buffer(
                        &state.invoking_pane_id,
                        &format!(
                            "loop: reached iteration limit {}/{} after {}",
                            state.iteration,
                            state.max_iterations,
                            if state.goal.is_some() {
                                "goal remained unmet"
                            } else {
                                "apply_patch work"
                            }
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
                // Fork and new iterations bind a fresh ephemeral conversation,
                // which the pane shell refuses while this settled turn still
                // owns its running-turn slot. Record the pending iteration and
                // let authoritative completion start it.
                self.defer_agent_loop_continuation(
                    &turn.turn_id,
                    RuntimeAgentLoopContinuation {
                        loop_id: loop_turn.loop_id.clone(),
                        pane_id: next_state.execution_pane_id.clone(),
                        iteration: next_state.iteration,
                    },
                );
                Ok(RuntimeAgentLoopSettlement::Continued)
            }
        }
    }

    /// Starts one deferred loop continuation after its predecessor settled.
    ///
    /// The finished turn's shell session releases the pane's running-turn slot
    /// before this runs, so a fork/new iteration can bind its next ephemeral
    /// conversation. A duplicate terminal result finds no pending record and is
    /// inert instead of starting a second iteration.
    pub(crate) fn start_deferred_agent_loop_continuation(
        &mut self,
        settled_turn_id: &str,
    ) -> Result<()> {
        let Some(continuation) = self.take_agent_loop_continuation(settled_turn_id) else {
            return Ok(());
        };
        let Some(state) = self.agent_loop_state_by_id(&continuation.loop_id).cloned() else {
            return Ok(());
        };
        match self.start_agent_loop_work_turn(&continuation.pane_id) {
            Ok(_) => {
                self.append_agent_status_text_to_terminal_buffer(
                    &state.invoking_pane_id,
                    &format!(
                        "loop: continuing fresh iteration {}/{}",
                        continuation.iteration, state.max_iterations
                    ),
                )?;
                Ok(())
            }
            Err(error) => self.recover_failed_agent_loop_continuation(
                &continuation.loop_id,
                settled_turn_id,
                error,
            ),
        }
    }

    /// Terminates one loop whose next iteration could not start after
    /// settlement.
    ///
    /// The settled turn keeps its own completed execution and effects; only the
    /// pending iteration is abandoned. Routed controllers resume the blocked
    /// parent through the existing bounded explanation path, while an ordinary
    /// controller reports the failure on its invoking pane because the turn it
    /// belonged to has already settled.
    fn recover_failed_agent_loop_continuation(
        &mut self,
        loop_id: &str,
        settled_turn_id: &str,
        error: MezError,
    ) -> Result<()> {
        let Some(state) = self.agent_loop_state_by_id(loop_id).cloned() else {
            return Err(error);
        };
        let objective = self.preflight_agent_loop_parent_objective(&state)?;
        self.restore_agent_loop_parent_conversation(
            &state.invoking_pane_id,
            &state,
            objective.as_deref(),
        )?;
        self.remove_agent_loop_state_by_id(loop_id);
        if let Some(parent_turn_id) = state.routed_parent_turn_id.clone() {
            self.fail_routed_loop_continuation(
                parent_turn_id.as_str(),
                settled_turn_id,
                error.message(),
            )?;
            if let Some(completion) = state.completion.clone() {
                self.retain_routed_loop_completion(parent_turn_id.as_str(), completion)?;
            }
            return Ok(());
        }
        self.append_agent_trace_turn_event(
            &state.execution_pane_id,
            settled_turn_id,
            &format!("loop continuation failed reason={}", error.message()),
        )?;
        self.append_agent_status_text_to_terminal_buffer(
            &state.invoking_pane_id,
            &format!(
                "loop: stopped after iteration {} because the next iteration could not start: {}",
                state.iteration,
                error.message()
            ),
        )?;
        Ok(())
    }
}
