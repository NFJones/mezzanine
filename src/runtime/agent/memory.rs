//! Runtime agent persistent-memory action helpers.
//!
//! This module owns provider-produced `memory_search` and `memory_store`
//! execution after the capability gate has exposed the memory action surface.
//! It keeps durable-memory reads and writes behind the runtime service so the
//! main agent turn loop only has to settle ordinary MAAP action results.

use super::*;
use mez_agent::memory::{
    MemorySearchActionRecord, memory_action_content, memory_action_limit, memory_action_record_id,
    memory_search_action_result, memory_store_action_result,
};

impl RuntimeSessionService {
    /// Executes provider-produced persistent-memory actions for one running turn.
    pub(in crate::runtime) fn execute_running_memory_actions_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<usize> {
        if execution.terminal_state != AgentTurnState::Running {
            return Ok(0);
        }
        let Some(batch) = execution.response.action_batch.clone() else {
            return Ok(0);
        };
        let mut executed = 0usize;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Running
                || !matches!(
                    execution.action_results[index].action_type,
                    "memory_search" | "memory_store"
                )
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("running memory result does not match an action")
                })?;
            if !self
                .append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, &action)?
            {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: {}",
                        runtime_agent_action_summary(&action)
                            .unwrap_or_else(|| "memory action".to_string())
                    ),
                )?;
            }
            let result = self.execute_memory_action_for_turn(turn, &action)?;
            let outcome = format!("{:?}", result.status).to_ascii_lowercase();
            self.append_agent_memory_action_audit(turn, &action, &outcome)?;
            execution.action_results[index] = result;
            executed = executed.saturating_add(1);
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        if execution.terminal_state == AgentTurnState::Running
            && runtime_execution_ready_for_provider_continuation(execution)
        {
            for result in execution
                .action_results
                .iter()
                .filter(|result| matches!(result.action_type, "memory_search" | "memory_store"))
            {
                self.agent_turn_contexts
                    .get_mut(&turn.turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("running agent turn context is unavailable")
                    })?
                    .blocks
                    .push(ContextBlock {
                        source: ContextSourceKind::ActionResult,
                        label: format!("action result {}", result.action_id),
                        content: action_result_context_content(result),
                    });
            }
            self.pending_agent_provider_tasks
                .insert(turn.turn_id.clone());
        }
        Ok(executed)
    }

    /// Executes one persistent-memory action and converts it into a MAAP result.
    fn execute_memory_action_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
    ) -> Result<ActionResult> {
        if !self.runtime_persistent_memory_enabled() {
            return Ok(ActionResult::failed(
                turn,
                action,
                ActionStatus::Failed,
                "memory_disabled",
                "memory actions require memory.enabled to be true; continue with current action results, MCP, shell, web, or a bounded report instead of retrying memory actions".to_string(),
            )?);
        }
        let Some(config_root) = self.config_root.clone() else {
            return Ok(ActionResult::failed(
                turn,
                action,
                ActionStatus::Failed,
                "memory_store_unavailable",
                "persistent memory actions require a configured config root; continue with direct artifacts, current action results, MCP, shell, web, or a bounded report instead of retrying memory actions".to_string(),
            )?);
        };
        let store = crate::memory::PersistentMemoryStore::under_config_root(config_root);
        match &action.payload {
            AgentActionPayload::MemorySearch { query, limit } => {
                let limit = memory_action_limit(*limit);
                let scopes = self.memory_action_search_scopes(turn);
                match search_runtime_memory_scopes(&store, query, &scopes, limit) {
                    Ok(results) => {
                        let presentation = results
                            .iter()
                            .map(|result| MemorySearchActionRecord {
                                record: &result.record,
                                score: result.score,
                                reason: &result.reason,
                            })
                            .collect::<Vec<_>>();
                        Ok(memory_search_action_result(
                            turn,
                            action,
                            query,
                            &presentation,
                        ))
                    }
                    Err(error) => Ok(ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message().to_string(),
                    )?),
                }
            }
            AgentActionPayload::MemoryStore {
                kind,
                priority,
                scope,
                keywords,
                content,
                expires_in_days,
            } => {
                let result = self.build_memory_store_record(
                    turn,
                    action,
                    kind,
                    *priority,
                    scope.as_deref(),
                    keywords,
                    content,
                    *expires_in_days,
                );
                let record = match result {
                    Ok(record) => record,
                    Err(error) => {
                        return Ok(ActionResult::failed(
                            turn,
                            action,
                            ActionStatus::Failed,
                            runtime_mezzanine_error_code(error.kind()),
                            error.message().to_string(),
                        )?);
                    }
                };
                match store.upsert(record.clone()) {
                    Ok(()) => Ok(memory_store_action_result(turn, action, &record)),
                    Err(error) => Ok(ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message().to_string(),
                    )?),
                }
            }
            _ => Err(MezError::invalid_args(
                "memory execution requires a memory action",
            )),
        }
    }

    /// Builds one persistent memory record from a model-authored store action.
    #[allow(clippy::too_many_arguments)]
    fn build_memory_store_record(
        &self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        kind: &str,
        priority: Option<u64>,
        scope: Option<&str>,
        keywords: &[String],
        content: &str,
        expires_in_days: Option<u64>,
    ) -> Result<mez_agent::memory::MemoryRecord> {
        let now = current_unix_seconds();
        let priority = priority.unwrap_or(50).min(100) as u8;
        let scope = self.memory_action_scope(turn, scope);
        let body = memory_action_content(content, keywords);
        let mut record = mez_agent::memory::MemoryRecord::new_with_defaults(
            memory_action_record_id(turn, action),
            scope,
            now,
            now,
            mez_agent::memory::MemorySource::Agent,
            priority,
            body,
        );
        record.kind = mez_agent::memory::parse_model_writable_kind(kind).map_err(|_| {
            MezError::invalid_args(
                "memory_store kind must be preference, fact, procedure, documentation, research, or warning",
            )
        })?;
        if let Some(days) = expires_in_days {
            let seconds = days.checked_mul(86_400).ok_or_else(|| {
                MezError::invalid_args("memory expires_in_days is too large to store")
            })?;
            record.expiration_duration_seconds = Some(seconds);
            record.expires_at_unix_seconds = Some(now.checked_add(seconds).ok_or_else(|| {
                MezError::invalid_args("memory expiration timestamp is too large to store")
            })?);
        } else if let Some(seconds) = self
            .runtime_memory_default_ttl_days()
            .checked_mul(86_400)
            .filter(|seconds| *seconds > 0)
        {
            record.expiration_duration_seconds = Some(seconds);
            record.expires_at_unix_seconds = Some(now.checked_add(seconds).ok_or_else(|| {
                MezError::invalid_args("memory default_ttl_days is too large to store")
            })?);
        }
        Ok(record)
    }

    /// Returns the runtime-visible persistent scopes for a memory search.
    fn memory_action_search_scopes(
        &self,
        turn: &AgentTurnRecord,
    ) -> Vec<mez_agent::memory::MemoryScope> {
        let mut scopes = vec![mez_agent::memory::MemoryScope::Global];
        if let Some(project_scope) = self.memory_action_project_scope(&turn.pane_id) {
            scopes.push(project_scope);
        }
        scopes
    }

    /// Returns the current pane project scope used by runtime memory actions.
    fn memory_action_project_scope(&self, pane_id: &str) -> Option<mez_agent::memory::MemoryScope> {
        self.pane_current_working_directory(pane_id).map(|root| {
            mez_agent::memory::MemoryScope::Project {
                root: crate::project::discover_project_root(&root)
                    .to_string_lossy()
                    .into_owned(),
            }
        })
    }

    /// Chooses the durable scope for a memory action.
    fn memory_action_scope(
        &self,
        turn: &AgentTurnRecord,
        scope: Option<&str>,
    ) -> mez_agent::memory::MemoryScope {
        match scope.unwrap_or("project") {
            "global" => mez_agent::memory::MemoryScope::Global,
            "project" => self
                .memory_action_project_scope(&turn.pane_id)
                .unwrap_or(mez_agent::memory::MemoryScope::Global),
            _ => mez_agent::memory::MemoryScope::Global,
        }
    }
}

/// Searches all runtime-visible scopes and applies the final bounded limit.
fn search_runtime_memory_scopes(
    store: &crate::memory::PersistentMemoryStore,
    query: &str,
    scopes: &[mez_agent::memory::MemoryScope],
    limit: usize,
) -> Result<Vec<crate::memory::MemorySearchResult>> {
    let mut results = Vec::new();
    for scope in scopes {
        results.extend(store.search(&crate::memory::MemorySearchRequest {
            query: Some(query.to_string()),
            scope: Some(scope.clone()),
            kind: None,
            state: Some(mez_agent::memory::MemoryState::Active),
            source: None,
            limit,
        })?);
    }
    results.sort_by(compare_runtime_memory_results);
    results.truncate(limit);
    Ok(results)
}

/// Orders runtime memory search results by score, recency, and id.
fn compare_runtime_memory_results(
    left: &crate::memory::MemorySearchResult,
    right: &crate::memory::MemorySearchResult,
) -> std::cmp::Ordering {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| {
            right
                .record
                .updated_at_unix_seconds
                .cmp(&left.record.updated_at_unix_seconds)
        })
        .then_with(|| left.record.id.cmp(&right.record.id))
}
