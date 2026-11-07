//! Local Say results have no asynchronous completion-group owner. Their exact
//! replay fence is captured at the actual result append and retired only after
//! presentation succeeds. This owns no source body, effect or persistence ACK;
//! mixed batches and all asynchronous obligations retain their existing owner.

use super::super::{AgentTurnExecution, AgentTurnRecord, MezError, Result, RuntimeSessionService};
use super::provider_log_execution_group_id;
use mez_agent::{ActionStatus, AgentActionPayload, AgentTurnState, ReplayExecutionOwner};
use std::collections::BTreeSet;

impl RuntimeSessionService {
    /// Captures only newly committed successful local Say results. Duplicate
    /// application cannot borrow a newer source/protection interval. No source
    /// is closed here; failed/foreign/mixed/unfinished executions grant no fence.
    pub(super) fn capture_settled_inline_say_replay(
        &self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
        committed: usize,
    ) -> Result<Option<ReplayExecutionOwner>> {
        let local_says = committed > 0
            && !self.completion_owns_transcript_policy(&turn.turn_id)
            && execution.request.turn_id == turn.turn_id
            && execution.request.agent_id == turn.agent_id
            && matches!(
                execution.terminal_state,
                AgentTurnState::Running | AgentTurnState::Completed
            )
            && execution
                .response
                .action_batch
                .as_ref()
                .is_some_and(|batch| {
                    !batch.actions.is_empty()
                        && batch.actions.len() == execution.action_results.len()
                        && batch
                            .actions
                            .iter()
                            .map(|action| &action.id)
                            .collect::<BTreeSet<_>>()
                            .len()
                            == batch.actions.len()
                        && batch.actions.iter().all(|action| {
                            matches!(action.payload, AgentActionPayload::Say { .. })
                                && execution
                                    .action_results
                                    .iter()
                                    .filter(|result| {
                                        result.action_id == action.id
                                            && result.action_type == "say"
                                            && result.status == ActionStatus::Succeeded
                                    })
                                    .count()
                                    == 1
                        })
                });
        if !local_says {
            return Ok(None);
        }
        let group = provider_log_execution_group_id(execution)?;
        self.agent_turn_contexts()
            .get(&turn.turn_id)
            .filter(|context| context.has_replay_owner())
            .map(|context| {
                context
                    .replay_execution_append_owner(&group)
                    .cloned()
                    .ok_or_else(|| {
                        MezError::conflict("inline response original replay fence is unavailable")
                    })
            })
            .transpose()
    }

    /// Completes the captured inline source only after actual presentation.
    /// Failed paint or a changed append/protection interval stays pending. This
    /// never reacquires a newer fence or retires asynchronous effect owners.
    pub(super) fn present_settled_inline_say_execution(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
        receipt: Option<&ReplayExecutionOwner>,
    ) -> Result<()> {
        if self
            .agent_shell_store()
            .get(&turn.pane_id)
            .is_none_or(|session| session.session_id != turn.conversation_id)
        {
            return Err(MezError::conflict(
                "inline response original conversation changed",
            ));
        }
        self.present_deferred_agent_say_actions_to_terminal_buffer(&turn.pane_id, execution)?;
        if let Some(receipt) = receipt {
            self.agent_turn_contexts_mut()
                .get_mut(&turn.turn_id)
                .ok_or_else(|| {
                    MezError::conflict("inline response original context is unavailable")
                })?
                .update_replay_owner(|owner| {
                    if owner.execution_group_finished(receipt) {
                        Ok(())
                    } else {
                        owner.finish_execution_group(receipt)
                    }
                })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
