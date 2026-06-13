//! Runtime agent local issue action helpers.
//!
//! This module owns provider-produced `issue_add`, `issue_update`,
//! `issue_query`, and `issue_delete` execution after the issues capability
//! exposes those actions.
//! It keeps project resolution and SQLite persistence behind the runtime
//! service so provider turns receive compact structured action results.

use super::*;
use crate::runtime::runtime_effective_config_value;
use std::path::Path;

impl RuntimeSessionService {
    /// Executes provider-produced issue actions for one running turn.
    pub(in crate::runtime) fn execute_running_issue_actions_for_turn(
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
                    "issue_add" | "issue_update" | "issue_query" | "issue_delete"
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
                    MezError::invalid_state("running issue result does not match an action")
                })?;
            if !self
                .append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, &action)?
            {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: {}",
                        runtime_agent_action_summary(&action)
                            .unwrap_or_else(|| "issue action".to_string())
                    ),
                )?;
            }
            execution.action_results[index] = self.execute_issue_action_for_turn(turn, &action)?;
            executed = executed.saturating_add(1);
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        if execution.terminal_state == AgentTurnState::Running
            && runtime_execution_ready_for_provider_continuation(execution)
        {
            for result in execution.action_results.iter().filter(|result| {
                matches!(
                    result.action_type,
                    "issue_add" | "issue_update" | "issue_query" | "issue_delete"
                )
            }) {
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

    fn execute_issue_action_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
    ) -> Result<ActionResult> {
        if !runtime_issues_enabled(self) {
            return ActionResult::failed(
                turn,
                action,
                ActionStatus::Failed,
                "issues_disabled",
                "issue actions require issues.enabled to be true".to_string(),
            );
        }
        let Some(config_root) = self.config_root.clone() else {
            return ActionResult::failed(
                turn,
                action,
                ActionStatus::Failed,
                "issue_store_unavailable",
                "issue actions require a configured config root".to_string(),
            );
        };
        let store = crate::issues::IssueStore::new(runtime_issue_database_path(self, &config_root));
        let project = issue_action_project(self, turn, &config_root);
        match &action.payload {
            AgentActionPayload::IssueAdd {
                kind,
                title,
                body,
                notes,
            } => {
                let result = store.add_issue(
                    project,
                    crate::issues::IssueKind::parse(kind)?,
                    title.clone(),
                    body.clone(),
                    notes.clone(),
                    current_unix_seconds(),
                );
                match result {
                    Ok(record) => Ok(issue_record_action_result(turn, action, "added", &record)),
                    Err(error) => ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message().to_string(),
                    ),
                }
            }
            AgentActionPayload::IssueUpdate {
                id,
                kind,
                title,
                body,
                clear_body,
                notes,
                clear_notes,
            } => {
                let result = store.update_issue(
                    project,
                    id.clone(),
                    crate::issues::IssueUpdate {
                        kind: kind
                            .as_deref()
                            .map(crate::issues::IssueKind::parse)
                            .transpose()?,
                        title: title.clone(),
                        body: body.clone(),
                        clear_body: *clear_body,
                        notes: notes.clone(),
                        clear_notes: *clear_notes,
                    },
                    current_unix_seconds(),
                );
                match result {
                    Ok(result) => Ok(issue_update_action_result(turn, action, &result)),
                    Err(error) => ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message().to_string(),
                    ),
                }
            }
            AgentActionPayload::IssueQuery { kind, text, limit } => {
                let kind = kind
                    .as_deref()
                    .map(crate::issues::IssueKind::parse)
                    .transpose()?;
                let limit = limit.and_then(|value| usize::try_from(value).ok());
                let query = crate::issues::IssueQuery::new(project, kind, text.clone(), limit)?;
                match store.query_issues(&query) {
                    Ok(records) => Ok(issue_query_action_result(turn, action, &records)),
                    Err(error) => ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message().to_string(),
                    ),
                }
            }
            AgentActionPayload::IssueDelete { id } => match store.delete_issue(project, id.clone())
            {
                Ok(result) => Ok(issue_delete_action_result(turn, action, &result)),
                Err(error) => ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    runtime_mezzanine_error_code(error.kind()),
                    error.message().to_string(),
                ),
            },
            _ => Err(MezError::invalid_args(
                "issue execution requires an issue action",
            )),
        }
    }
}

fn runtime_issues_enabled(service: &RuntimeSessionService) -> bool {
    runtime_effective_config_value(&service.config_layers)
        .ok()
        .and_then(|root| {
            root.get("issues")
                .and_then(|issues| issues.get("enabled"))
                .and_then(serde_json::Value::as_bool)
        })
        .unwrap_or(true)
}

fn runtime_issue_database_path(service: &RuntimeSessionService, config_root: &PathBuf) -> PathBuf {
    let configured = runtime_effective_config_value(&service.config_layers)
        .ok()
        .and_then(|root| {
            root.get("issues")
                .and_then(|issues| issues.get("database_path"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    crate::issues::issue_database_path(config_root, configured.as_deref())
}

fn issue_action_project(
    service: &RuntimeSessionService,
    turn: &AgentTurnRecord,
    config_root: &Path,
) -> String {
    service
        .pane_current_working_directory(&turn.pane_id)
        .unwrap_or_else(|| config_root.to_path_buf())
        .pipe(crate::issues::project_key_for_working_directory)
}

fn issue_record_action_result(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    operation: &str,
    record: &crate::issues::IssueRecord,
) -> ActionResult {
    ActionResult::succeeded(
        turn,
        action,
        vec![format!("issue_{operation} {}", record.id)],
        Some(issue_record_json(record).to_string()),
    )
}

fn issue_query_action_result(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    records: &[crate::issues::IssueRecord],
) -> ActionResult {
    ActionResult::succeeded(
        turn,
        action,
        vec![format!("issue_query returned {} records", records.len())],
        Some(
            serde_json::json!({
                "count": records.len(),
                "issues": records.iter().map(issue_record_json).collect::<Vec<_>>(),
            })
            .to_string(),
        ),
    )
}

fn issue_update_action_result(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    result: &crate::issues::UpdateIssueResult,
) -> ActionResult {
    ActionResult::succeeded(
        turn,
        action,
        vec![format!(
            "issue_update {} updated={}",
            result.id, result.updated
        )],
        Some(
            serde_json::json!({
                "id": result.id,
                "project": result.project,
                "updated": result.updated,
                "record": result.record.as_ref().map(issue_record_json),
            })
            .to_string(),
        ),
    )
}

fn issue_delete_action_result(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    result: &crate::issues::DeleteIssueResult,
) -> ActionResult {
    ActionResult::succeeded(
        turn,
        action,
        vec![format!(
            "issue_delete {} deleted={}",
            result.id, result.deleted
        )],
        Some(
            serde_json::json!({
                "id": result.id,
                "project": result.project,
                "deleted": result.deleted,
            })
            .to_string(),
        ),
    )
}

fn issue_record_json(record: &crate::issues::IssueRecord) -> serde_json::Value {
    serde_json::json!({
        "id": record.id,
        "project": record.project,
        "kind": record.kind.as_str(),
        "title": record.title,
        "body": record.body,
        "notes": record.notes,
        "created_at_unix_seconds": record.created_at_unix_seconds,
        "updated_at_unix_seconds": record.updated_at_unix_seconds,
    })
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}

impl<T> Pipe for T {}
