//! Same-turn waiting for model-originated MMP peer mail.
//!
//! This module owns the boundary between a provider-produced `wait` action and
//! the runtime scheduler. A peer wait releases provider capacity while retaining
//! the turn, agent, conversation, and pane claims. Only model-originated MMP mail
//! may settle the action and fairly reacquire capacity; approvals, subprocesses,
//! user input, timers, and runtime-authored bridge traffic are not wake sources.

use super::{
    ActionResult, ActionStatus, AgentActionPayload, AgentTurnExecution, AgentTurnRecord,
    AgentTurnState, MezError, Result, RuntimeSessionService, TaskState, current_unix_millis,
    runtime_agent_turn_state_from_action_results,
};

/// Reports whether an execution is parked solely on one MAAP `wait` action.
fn execution_has_pending_peer_wait(execution: &AgentTurnExecution) -> bool {
    execution.terminal_state == AgentTurnState::Running
        && execution
            .action_results
            .iter()
            .any(|result| result.action_type == "wait" && result.status == ActionStatus::Running)
        && execution
            .action_results
            .iter()
            .all(|result| result.action_type == "wait" || result.status == ActionStatus::Succeeded)
}

impl RuntimeSessionService {
    /// Reports whether one turn is parked specifically for MMP peer mail.
    pub(crate) fn agent_turn_is_waiting_for_peer_message(&self, turn_id: &str) -> bool {
        self.agent
            .agent_peer_wait_remaining_timeout_ms
            .contains_key(turn_id)
    }

    /// Parks a provider execution whose only pending work is `wait`.
    ///
    /// Returns `true` when the scheduler and ledger were transitioned. The
    /// caller should immediately run one message-delivery pass after this
    /// returns so mail already pending at the park boundary cannot be lost.
    pub(crate) fn park_agent_turn_for_peer_message(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<bool> {
        if !execution_has_pending_peer_wait(execution) {
            return Ok(false);
        }
        let now_ms = current_unix_millis();
        let remaining_timeout_ms = mez_agent::agent_turn_remaining_timeout_ms(
            turn.started_at_unix_seconds,
            turn.deadline_at_unix_millis,
            now_ms,
        )
        .ok_or_else(|| MezError::invalid_state("agent turn deadline expired before peer wait"))?;

        self.agent_turn_executions_mut()
            .insert(turn.turn_id.clone(), execution.clone());
        self.agent
            .agent_peer_wait_remaining_timeout_ms
            .insert(turn.turn_id.clone(), remaining_timeout_ms);
        self.agent.agent_scheduler.wait_running(&turn.turn_id)?;
        self.agent
            .pending_agent_provider_tasks
            .remove(&turn.turn_id);
        self.agent
            .claimed_agent_provider_tasks
            .remove(&turn.turn_id);
        self.agent_turn_ledger_mut()
            .finish_turn(&turn.turn_id, AgentTurnState::Blocked)?;
        self.reconcile_active_turn_sleep_inhibition();
        self.append_agent_trace_turn_transition(
            turn,
            turn.state,
            AgentTurnState::Blocked,
            "waiting_for_peer_message",
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            "scheduler running -> waiting reason=waiting_for_peer_message capacity=released",
        )?;
        self.append_agent_status_text_to_terminal_buffer(
            &turn.pane_id,
            "agent: waiting for MMP peer message",
        )?;
        self.emit_subagent_task_status(
            turn,
            TaskState::Blocked,
            None,
            "subagent task waiting for MMP peer message",
        )?;
        self.start_ready_agent_turns()?;
        Ok(true)
    }

    /// Settles a pending `wait` and fairly resumes the same turn.
    pub(crate) fn resume_agent_peer_wait(
        &mut self,
        turn: &AgentTurnRecord,
        message_count: usize,
    ) -> Result<bool> {
        if message_count == 0 || !self.agent_turn_is_waiting_for_peer_message(&turn.turn_id) {
            return Ok(false);
        }
        if turn.state != AgentTurnState::Blocked
            || !self
                .agent
                .agent_scheduler
                .waiting_turns()
                .any(|work| work.turn_id == turn.turn_id)
        {
            return Ok(false);
        }

        let mut execution = self
            .agent_turn_executions()
            .get(&turn.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("peer-wait execution is unavailable"))?;
        let batch =
            execution.response.action_batch.as_ref().ok_or_else(|| {
                MezError::invalid_state("peer-wait execution has no action batch")
            })?;
        let wait_action = batch
            .actions
            .iter()
            .find(|action| matches!(action.payload, AgentActionPayload::Wait))
            .cloned()
            .ok_or_else(|| MezError::invalid_state("peer-wait execution has no wait action"))?;
        let result_index = execution
            .action_results
            .iter()
            .position(|result| {
                result.action_id == wait_action.id
                    && result.action_type == "wait"
                    && result.status == ActionStatus::Running
            })
            .ok_or_else(|| MezError::invalid_state("peer-wait action result is unavailable"))?;
        let settled = ActionResult::succeeded(
            turn,
            &wait_action,
            vec![format!("resumed after {message_count} MMP peer message(s)")],
            Some(
                serde_json::json!({
                    "state": "resumed",
                    "reason": "peer_message",
                    "messages": message_count,
                })
                .to_string(),
            ),
        );
        execution.action_results[result_index] = settled.clone();
        execution.final_turn = false;
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        self.commit_settled_action_results_context(&turn.turn_id, &[settled])?;
        self.agent_turn_executions_mut()
            .insert(turn.turn_id.clone(), execution);

        let remaining_timeout_ms = self
            .agent
            .agent_peer_wait_remaining_timeout_ms
            .remove(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("peer-wait timeout budget is unavailable"))?;
        self.agent_turn_ledger_mut().set_turn_deadline(
            &turn.turn_id,
            current_unix_millis().saturating_add(remaining_timeout_ms),
        )?;
        self.agent.agent_scheduler.requeue_waiting(&turn.turn_id)?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            "scheduler waiting -> queued reason=peer_message_arrival capacity=reacquire",
        )?;
        self.append_agent_status_text_to_terminal_buffer(
            &turn.pane_id,
            "agent: MMP peer message received; continuing",
        )?;
        self.start_ready_agent_turns()?;
        Ok(true)
    }
}
