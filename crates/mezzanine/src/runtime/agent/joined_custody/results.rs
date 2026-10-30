//! Immutable received-result preparation at the original joined action target.
//!
//! Local terminal lifecycle code supplies observation facts. Preparation retains
//! the first exact value under existing dependency custody before context/slot
//! mutation. It is not proof of child completion, append, hook, presentation,
//! provider admission or scheduler wake. The existing apply path still requires
//! the original live execution group; different-group coordination is separate.

use super::*;
use mez_agent::ActionResult;

/// One original-target result value; none of its fields grant effect authority.
pub(crate) struct PreparedJoinedTaskResult {
    /// Exact child attempt and parent action that received the local observation.
    dependency: JoinedSubagentDependency,
    /// Immutable captured source when available; reconstructed callers remain live-only.
    source: Option<Arc<JoinedExecutionSource>>,
    /// Original action, never substituted from a later response's action batch.
    action: AgentAction,
    /// Exact received result body, held independently of the latest execution.
    result: ActionResult,
}

impl std::fmt::Debug for PreparedJoinedTaskResult {
    /// Formats identity without child output, task text or provider payloads.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedJoinedTaskResult")
            .field("parent_turn_id", &self.dependency.parent_turn_id)
            .field("child_turn_id", &self.dependency.child_turn_id)
            .finish_non_exhaustive()
    }
}

impl PreparedJoinedTaskResult {
    /// Borrows the retained value without acknowledging context or task settlement.
    pub(crate) fn result(&self) -> &ActionResult {
        &self.result
    }

    /// Finds this result's target only in its original live source. Captured
    /// source never falls back to a new group with reused ids. Missing, changed,
    /// or foreign action/result slots fail before mutation. Reconstructed
    /// live-only callers retain their existing action and result identity checks.
    pub(crate) fn matching_result_slot(&self, execution: &AgentTurnExecution) -> Result<usize> {
        if let Some(source) = &self.source
            && super::super::provider_execution::provider_log_execution_group_id(execution)?
                != source.group
        {
            return Err(MezError::conflict(
                "joined result source differs from the live execution",
            ));
        }
        let action = execution
            .response
            .action_batch
            .as_ref()
            .and_then(|batch| {
                batch
                    .actions
                    .iter()
                    .find(|action| action.id == self.action.id)
            })
            .ok_or_else(|| MezError::invalid_state("joined subagent parent action is missing"))?;
        if action != &self.action {
            return Err(MezError::conflict(
                "joined result differs from its original spawn action",
            ));
        }
        let index = execution
            .action_results
            .iter()
            .position(|result| result.action_id == self.action.id)
            .ok_or_else(|| MezError::invalid_state("joined subagent parent result is missing"))?;
        let slot = &execution.action_results[index];
        if slot.turn_id != self.result.turn_id
            || slot.agent_id != self.result.agent_id
            || slot.action_type != self.result.action_type
        {
            return Err(MezError::conflict("joined result target identity changed"));
        }
        Ok(index)
    }
}

impl RuntimeSessionService {
    /// Prepares an exact result from a locally validated terminal observation.
    /// Captured joins use the original action without reading the latest batch;
    /// repeated exact input returns the same receipt, conflicting input preserves
    /// the first receipt and fails. Source/binding validation precedes retention.
    /// Current reconstructed dependencies without captured custody remain live-only.
    pub(crate) fn prepare_joined_task_result(
        &mut self,
        dependency: &JoinedSubagentDependency,
        success: bool,
        summary: &str,
        output: &str,
    ) -> Result<Arc<PreparedJoinedTaskResult>> {
        let parent = self
            .agent_turn_ledger()
            .turn(&dependency.parent_turn_id)
            .ok_or_else(|| MezError::invalid_state("joined subagent parent turn is missing"))?;
        if !self
            .agent_turn_contexts()
            .contains_key(&dependency.parent_turn_id)
        {
            return Err(MezError::invalid_state(
                "joined subagent parent context is missing",
            ));
        }
        let retained = self.retained_joined_spawn_action(dependency)?;
        let action = match retained {
            Some(action) => action,
            None => self
                .agent_turn_executions()
                .get(&dependency.parent_turn_id)
                .and_then(|execution| execution.response.action_batch.as_ref())
                .and_then(|batch| {
                    batch
                        .actions
                        .iter()
                        .find(|action| action.id == dependency.parent_action_id)
                })
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("joined subagent parent action is missing")
                })?,
        };
        let child_label = super::super::runtime_subagent_display_label(
            &dependency.child_agent_id,
            dependency.child_display_name.as_deref(),
        );
        let result_summary = if success {
            format!("subagent {child_label} completed: {summary}")
        } else {
            format!("subagent {child_label} failed: {summary}")
        };
        let structured = format!(
            r#"{{"join_policy":"join","join_state":"completed","child_agent_id":"{}","child_display_name":{},"child_turn_id":"{}","task_result":{{"success":{},"summary":"{}","output":"{}"}}}}"#,
            super::super::json_escape(&dependency.child_agent_id),
            dependency
                .child_display_name
                .as_deref()
                .map(|name| format!(r#""{}""#, super::super::json_escape(name)))
                .unwrap_or_else(|| "null".to_string()),
            super::super::json_escape(&dependency.child_turn_id),
            success,
            super::super::json_escape(summary),
            super::super::json_escape(output),
        );
        let result =
            ActionResult::succeeded(parent, &action, vec![result_summary], Some(structured));
        let source = self
            .agent
            .joined_parent_executions
            .get(&dependency.child_turn_id)
            .map(|owner| Arc::clone(&owner.source));
        if let Some(existing) = self
            .agent
            .joined_parent_executions
            .get(&dependency.child_turn_id)
            .and_then(|owner| owner.prepared_result.as_ref())
        {
            if existing.dependency != *dependency
                || existing.action != action
                || existing.result != result
            {
                return Err(MezError::conflict(
                    "joined task result conflicts with its first received value",
                ));
            }
            return Ok(Arc::clone(existing));
        }
        let prepared = Arc::new(PreparedJoinedTaskResult {
            dependency: dependency.clone(),
            source,
            action,
            result,
        });
        if let Some(owner) = self
            .agent
            .joined_parent_executions
            .get_mut(&dependency.child_turn_id)
        {
            owner.prepared_result = Some(Arc::clone(&prepared));
        }
        Ok(prepared)
    }

    /// A received terminal value cannot silently follow a new child attempt.
    /// The existing redirection owner must settle that value before transfer.
    pub(in crate::runtime::agent) fn joined_task_result_preparation_pending(
        &self,
        child_turn_id: &str,
    ) -> bool {
        self.agent
            .joined_parent_executions
            .get(child_turn_id)
            .is_some_and(|owner| owner.prepared_result.is_some())
    }
}
