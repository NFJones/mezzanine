//! Exact original canonical backing shared by completion adapters.
//!
//! A retained failed or correction source never falls back to a new live value.
//! Reads validate original turn, agent and execution group; writes return to that
//! same selected owner synchronously. Context commits retain original provider
//! tool-call attribution even after Stop has moved the backing out of live maps.
//! This helper neither authorizes effects nor acknowledges archive/publication.

use super::*;

/// Actual existing canonical source selected before projecting outcome facts.
#[derive(Clone, Copy)]
pub(super) enum CompletionExecutionSource {
    /// Original execution and context remain live.
    Live,
    /// Forced retirement moved the actual original backing into its group.
    Failed,
    /// The original correction execution remains under the same group.
    Correction,
}

impl RuntimeSessionService {
    /// Reads only the exact original execution; captured absence is final.
    pub(super) fn completion_original_execution(
        &self,
        turn: &AgentTurnRecord,
        expected_group: &mez_agent::ContextExecutionGroupId,
    ) -> Result<(CompletionExecutionSource, AgentTurnExecution)> {
        let group = self
            .agent
            .completion_settlements
            .groups
            .get(&turn.turn_id)
            .filter(|group| &group.group == expected_group)
            .ok_or_else(|| MezError::invalid_state("original completion group changed"))?;
        let (source, execution) = if let Some(backing) = &group.failed_backing {
            (
                CompletionExecutionSource::Failed,
                backing.execution.as_ref(),
            )
        } else if let retirement::CompletionCapture::Captured(execution) =
            &group.correction_execution
        {
            (CompletionExecutionSource::Correction, execution.as_ref())
        } else {
            (
                CompletionExecutionSource::Live,
                self.agent_turn_executions().get(&turn.turn_id),
            )
        };
        let execution = execution.ok_or_else(|| {
            MezError::invalid_state("completion original execution backing unavailable")
        })?;
        if super::super::provider_execution::provider_log_execution_group_id(execution)?
            != *expected_group
            || execution.request.turn_id != turn.turn_id
            || execution.request.agent_id != turn.agent_id
        {
            return Err(MezError::conflict(
                "completion latest execution is not its original source",
            ));
        }
        Ok((source, execution.clone()))
    }

    /// Returns updated facts to the same synchronously selected canonical owner.
    pub(super) fn store_completion_original_execution(
        &mut self,
        turn_id: &str,
        source: CompletionExecutionSource,
        execution: AgentTurnExecution,
    ) -> Result<()> {
        match source {
            CompletionExecutionSource::Live => {
                self.agent_turn_executions_mut()
                    .insert(turn_id.to_owned(), execution);
            }
            CompletionExecutionSource::Failed => {
                let backing = self
                    .agent
                    .completion_settlements
                    .groups
                    .get_mut(turn_id)
                    .and_then(|group| group.failed_backing.as_mut())
                    .ok_or_else(|| {
                        MezError::invalid_state("completion failed backing disappeared")
                    })?;
                backing.execution = retirement::CompletionCapture::Captured(Some(execution));
            }
            CompletionExecutionSource::Correction => {
                let group = self
                    .agent
                    .completion_settlements
                    .groups
                    .get_mut(turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("completion correction backing disappeared")
                    })?;
                group.correction_execution =
                    retirement::CompletionCapture::Captured(Some(execution));
            }
        }
        Ok(())
    }

    /// Commits original result chronology and provider-native result attribution.
    pub(super) fn commit_completion_original_context(
        &mut self,
        turn_id: &str,
        expected_group: &mez_agent::ContextExecutionGroupId,
        action_id: &str,
        source: CompletionExecutionSource,
        result: &ActionResult,
    ) -> Result<()> {
        if matches!(
            source,
            CompletionExecutionSource::Live | CompletionExecutionSource::Correction
        ) {
            if self
                .agent
                .agent_execution_groups_by_turn
                .get(turn_id)
                .and_then(|groups| groups.get(action_id))
                != Some(expected_group)
            {
                return Err(MezError::conflict(
                    "completion original context group binding changed",
                ));
            }
            return self.append_action_result_context_if_absent(turn_id, result);
        }
        let group = self
            .agent
            .completion_settlements
            .groups
            .get_mut(turn_id)
            .filter(|group| &group.group == expected_group)
            .ok_or_else(|| {
                MezError::invalid_state("completion original context group unavailable")
            })?;
        let action_ids = group
            .actions
            .iter()
            .map(|(id, _, _)| id.clone())
            .collect::<Vec<_>>();
        let context = group
            .failed_backing
            .as_mut()
            .and_then(|backing| match &mut backing.context {
                retirement::CompletionCapture::Captured(context) => context.as_mut(),
                retirement::CompletionCapture::NotCaptured => None,
            })
            .ok_or_else(|| {
                MezError::invalid_state("completion original captured context unavailable")
            })?;
        context
            .commit_settled_action_results_in_group(
                std::slice::from_ref(result),
                expected_group.clone(),
            )
            .map_err(|error| MezError::invalid_state(error.to_string()))?;
        super::super::outcome::append_provider_tool_results_in_group(
            context,
            expected_group,
            &action_ids,
            &group.provider_calls,
        )
    }
}
