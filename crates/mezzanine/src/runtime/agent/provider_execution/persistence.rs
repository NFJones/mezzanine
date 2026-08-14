//! Blocking provider-settlement persistence work.
//!
//! The serialized runtime actor validates provider completions and prepares
//! immutable repository/action context. This module performs only SQLite-backed
//! memory and issue actions, preserving family ordering before returning typed
//! results for actor-owned audit, presentation, scheduler, and continuation
//! application.

use super::super::{
    ActionStatus, AgentActionPayload, AgentTurnState, MezError, Result,
    runtime_agent_turn_state_from_action_results,
};
use crate::runtime::{RuntimeAgentProviderPersistenceOutcome, RuntimeAgentProviderPersistenceWork};

/// Executes actor-validated memory and issue actions on a blocking worker.
pub(crate) fn execute_agent_provider_persistence_work(
    work: RuntimeAgentProviderPersistenceWork,
) -> Result<RuntimeAgentProviderPersistenceOutcome> {
    let RuntimeAgentProviderPersistenceWork {
        turn,
        model_profile,
        provider_id,
        execution,
        memory_enabled,
        memory_store,
        memory_scopes,
        memory_default_ttl_days,
        issues_enabled,
        issue_store,
        issue_project,
        mut issue_query_freshness,
        actions_executed_before_persistence,
        settled_action_results_before_persistence,
    } = work;
    let Some(batch) = execution.response.action_batch.as_ref() else {
        return Ok(RuntimeAgentProviderPersistenceOutcome {
            turn,
            model_profile,
            provider_id,
            execution,
            memory_results: Vec::new(),
            issue_results: Vec::new(),
            issue_query_freshness,
            issue_records_changed: false,
            actions_executed_before_persistence,
            settled_action_results_before_persistence,
        });
    };

    let mut projected_results = execution.action_results.clone();
    let mut memory_results = Vec::new();
    for (index, pending) in execution.action_results.iter().enumerate() {
        if pending.status != ActionStatus::Running
            || !matches!(pending.action_type, "memory_search" | "memory_store")
        {
            continue;
        }
        let action = batch
            .actions
            .iter()
            .find(|action| action.id == pending.action_id)
            .ok_or_else(|| {
                MezError::invalid_state("running memory result does not match an action")
            })?;
        let result = super::super::memory::execute_memory_action_with_context(
            &turn,
            action,
            memory_enabled,
            memory_store.as_ref(),
            &memory_scopes,
            memory_default_ttl_days,
        )?;
        projected_results[index] = result.clone();
        memory_results.push((index, result));
    }

    let projected_state =
        runtime_agent_turn_state_from_action_results(&projected_results, execution.final_turn);
    let mut issue_results = Vec::new();
    let mut issue_records_changed = false;
    if projected_state == AgentTurnState::Running {
        for (index, pending) in execution.action_results.iter().enumerate() {
            if pending.status != ActionStatus::Running
                || !matches!(
                    pending.action_type,
                    "issue_add" | "issue_update" | "issue_query" | "issue_delete"
                )
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == pending.action_id)
                .ok_or_else(|| {
                    MezError::invalid_state("running issue result does not match an action")
                })?;
            if !matches!(
                action.payload,
                AgentActionPayload::IssueAdd { .. }
                    | AgentActionPayload::IssueUpdate { .. }
                    | AgentActionPayload::IssueQuery { .. }
                    | AgentActionPayload::IssueDelete { .. }
            ) {
                return Err(MezError::invalid_state(
                    "running issue result has a non-issue action payload",
                ));
            }
            let (result, changed) = super::super::issues::execute_issue_action_with_context(
                &turn,
                action,
                issues_enabled,
                issue_store.as_ref(),
                &issue_project,
                &mut issue_query_freshness,
            )?;
            issue_records_changed = issue_records_changed || changed;
            issue_results.push((index, result));
        }
    }

    Ok(RuntimeAgentProviderPersistenceOutcome {
        turn,
        model_profile,
        provider_id,
        execution,
        memory_results,
        issue_results,
        issue_query_freshness,
        issue_records_changed,
        actions_executed_before_persistence,
        settled_action_results_before_persistence,
    })
}
