//! Runtime agent persistent-memory action helpers.
//!
//! This module owns provider-produced `memory_search` and `memory_store`
//! execution after the capability gate has exposed the memory action surface.
//! It keeps durable-memory reads and writes behind the runtime service so the
//! main agent turn loop only has to settle ordinary MAAP action results.

use super::*;

const DEFAULT_MEMORY_ACTION_LIMIT: usize = 5;
const MAX_MEMORY_ACTION_LIMIT: usize = 20;

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
            return ActionResult::failed(
                turn,
                action,
                ActionStatus::Failed,
                "memory_disabled",
                "memory actions require memory.enabled to be true".to_string(),
            );
        }
        let Some(config_root) = self.config_root.clone() else {
            return ActionResult::failed(
                turn,
                action,
                ActionStatus::Failed,
                "memory_store_unavailable",
                "persistent memory actions require a configured config root".to_string(),
            );
        };
        let store = crate::memory::PersistentMemoryStore::under_config_root(config_root);
        match &action.payload {
            AgentActionPayload::MemorySearch { query, limit } => {
                let limit = memory_action_limit(*limit);
                let request = crate::memory::MemorySearchRequest {
                    query: Some(query.clone()),
                    scope: None,
                    kind: None,
                    state: Some(crate::memory::MemoryState::Active),
                    source: None,
                    limit,
                };
                match store.search(&request) {
                    Ok(results) => Ok(memory_search_action_result(turn, action, query, &results)),
                    Err(error) => ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message().to_string(),
                    ),
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
                        return ActionResult::failed(
                            turn,
                            action,
                            ActionStatus::Failed,
                            runtime_mezzanine_error_code(error.kind()),
                            error.message().to_string(),
                        );
                    }
                };
                match store.upsert(record.clone()) {
                    Ok(()) => Ok(memory_store_action_result(turn, action, &record)),
                    Err(error) => ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message().to_string(),
                    ),
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
    ) -> Result<crate::memory::MemoryRecord> {
        let now = current_unix_seconds();
        let priority = priority.unwrap_or(50).min(100) as u8;
        let scope = self.memory_action_scope(turn, scope);
        let body = memory_action_content(content, keywords);
        let mut record = crate::memory::MemoryRecord::new_with_defaults(
            memory_action_record_id(turn, action),
            scope,
            now,
            now,
            crate::memory::MemorySource::Agent,
            priority,
            body,
        );
        record.kind = memory_action_kind(kind)?;
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
            record.expires_at_unix_seconds = now.checked_add(seconds);
        }
        Ok(record)
    }

    /// Chooses the durable scope for a memory action.
    fn memory_action_scope(
        &self,
        turn: &AgentTurnRecord,
        scope: Option<&str>,
    ) -> crate::memory::MemoryScope {
        match scope.unwrap_or("project") {
            "global" => crate::memory::MemoryScope::Global,
            "project" => self
                .pane_current_working_directory(&turn.pane_id)
                .map(|root| crate::memory::MemoryScope::Project {
                    root: root.to_string_lossy().into_owned(),
                })
                .unwrap_or(crate::memory::MemoryScope::Global),
            _ => crate::memory::MemoryScope::Global,
        }
    }
}

/// Returns the bounded search result limit for a memory action.
fn memory_action_limit(limit: Option<u64>) -> usize {
    limit
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(DEFAULT_MEMORY_ACTION_LIMIT)
        .clamp(1, MAX_MEMORY_ACTION_LIMIT)
}

/// Parses the model-facing kind label into the durable memory taxonomy.
fn memory_action_kind(kind: &str) -> Result<crate::memory::MemoryKind> {
    match kind.trim().to_ascii_lowercase().as_str() {
        "preference" => Ok(crate::memory::MemoryKind::Preference),
        "fact" => Ok(crate::memory::MemoryKind::Fact),
        "procedure" => Ok(crate::memory::MemoryKind::Procedure),
        "episode" => Ok(crate::memory::MemoryKind::Episode),
        "warning" => Ok(crate::memory::MemoryKind::Warning),
        "scratch" => Ok(crate::memory::MemoryKind::Scratch),
        _ => Err(MezError::invalid_args("memory kind is not supported")),
    }
}

/// Builds durable content with optional keyword anchors.
fn memory_action_content(content: &str, keywords: &[String]) -> String {
    let keywords = keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| !keyword.is_empty())
        .collect::<Vec<_>>();
    if keywords.is_empty() {
        content.to_string()
    } else {
        format!("{}\n\nKeywords: {}", content.trim(), keywords.join(", "))
    }
}

/// Builds a stable idempotent record id for one memory store action.
fn memory_action_record_id(turn: &AgentTurnRecord, action: &AgentAction) -> String {
    format!("agent:{}:{}", turn.turn_id, action.id)
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// Builds the MAAP result for a successful memory search.
fn memory_search_action_result(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    query: &str,
    results: &[crate::memory::MemorySearchResult],
) -> ActionResult {
    let content = if results.is_empty() {
        vec!["memory_search returned 0 records".to_string()]
    } else {
        results
            .iter()
            .map(|result| {
                format!(
                    "{} score={:.3}: {}",
                    result.record.id,
                    result.score,
                    memory_action_preview(&result.record.content)
                )
            })
            .collect()
    };
    let records = results
        .iter()
        .map(|result| {
            serde_json::json!({
                "id": result.record.id,
                "scope": format!("{:?}", result.record.scope),
                "kind": format!("{:?}", result.record.kind),
                "priority": result.record.priority,
                "score": result.score,
                "reason": result.reason,
                "content": result.record.content,
            })
        })
        .collect::<Vec<_>>();
    ActionResult::succeeded(
        turn,
        action,
        content,
        Some(
            serde_json::json!({
                "query": query,
                "count": results.len(),
                "results": records,
            })
            .to_string(),
        ),
    )
}

/// Builds the MAAP result for a successful memory store action.
fn memory_store_action_result(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    record: &crate::memory::MemoryRecord,
) -> ActionResult {
    ActionResult::succeeded(
        turn,
        action,
        vec![format!("stored memory {}", record.id)],
        Some(
            serde_json::json!({
                "id": record.id,
                "scope": format!("{:?}", record.scope),
                "kind": format!("{:?}", record.kind),
                "priority": record.priority,
                "expires_at_unix_seconds": record.expires_at_unix_seconds,
            })
            .to_string(),
        ),
    )
}

/// Returns a bounded single-line preview for memory action output.
fn memory_action_preview(content: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 160;
    let text = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= MAX_PREVIEW_CHARS {
        return text;
    }
    let mut preview = text.chars().take(MAX_PREVIEW_CHARS).collect::<String>();
    preview.push('…');
    preview
}
