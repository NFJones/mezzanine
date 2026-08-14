//! Runtime agent local issue action helpers.
//!
//! This module owns provider-produced `issue_add`, `issue_update`,
//! `issue_query`, and `issue_delete` execution after the issues capability
//! exposes those actions.
//! It keeps project resolution and SQLite persistence behind the runtime
//! service so provider turns receive compact structured action results.

use super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentTurnExecution,
    AgentTurnRecord, AgentTurnState, BTreeMap, MezError, PathBuf, Result, RuntimeSessionService,
    current_unix_seconds, runtime_agent_action_summary,
    runtime_agent_turn_state_from_action_results, runtime_mezzanine_error_code,
};
use crate::runtime::runtime_effective_config_value;
use mez_agent::issues::{
    issue_delete_action_result, issue_query_action_result, issue_query_freshness_key,
    issue_query_freshness_skip_action_result, issue_record_action_result,
    issue_update_action_result,
};
use std::path::Path;

impl RuntimeSessionService {
    /// Executes provider-produced issue actions for one running turn.
    pub(crate) fn execute_running_issue_actions_for_turn(
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
        Ok(executed)
    }

    fn execute_issue_action_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
    ) -> Result<ActionResult> {
        let enabled = runtime_issues_enabled(self);
        let config_root = self
            .integration
            .config_root()
            .map(|path| path.to_path_buf());
        let store = config_root.as_ref().map(|config_root| {
            crate::storage::issues::IssueStore::from_database_path(runtime_issue_database_path(
                self,
                config_root,
            ))
        });
        let project = config_root
            .as_ref()
            .map(|config_root| issue_action_project(self, turn, config_root))
            .unwrap_or_default();
        let mut freshness = self
            .agent
            .agent_turn_issue_query_freshness
            .remove(&turn.turn_id)
            .unwrap_or_default();
        let (result, records_changed) = execute_issue_action_with_context(
            turn,
            action,
            enabled,
            store.as_ref(),
            &project,
            &mut freshness,
        )?;
        if !freshness.is_empty() {
            self.agent
                .agent_turn_issue_query_freshness
                .insert(turn.turn_id.clone(), freshness);
        }
        if records_changed {
            self.invalidate_agent_prompt_selector_extra_candidates();
        }
        Ok(result)
    }
}

/// Executes one issue action from immutable worker context.
pub(crate) fn execute_issue_action_with_context(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    enabled: bool,
    store: Option<&crate::storage::issues::IssueStore>,
    project: &str,
    freshness: &mut BTreeMap<String, String>,
) -> Result<(ActionResult, bool)> {
    if !enabled {
        return Ok((
            ActionResult::failed(
                turn,
                action,
                ActionStatus::Failed,
                "issues_disabled",
                "issue actions require issues.enabled to be true".to_string(),
            )?,
            false,
        ));
    }
    let Some(store) = store else {
        return Ok((
            ActionResult::failed(
                turn,
                action,
                ActionStatus::Failed,
                "issue_store_unavailable",
                "issue actions require a configured config root".to_string(),
            )?,
            false,
        ));
    };
    match &action.payload {
        AgentActionPayload::IssueAdd {
            kind,
            state,
            title,
            body,
            notes,
            depends_on,
        } => {
            let result = store.add_issue_with_dependencies(
                mez_agent::issues::NewIssueRecord {
                    project: project.to_string(),
                    kind: mez_agent::issues::IssueKind::parse(kind)?,
                    state: state
                        .as_deref()
                        .map(mez_agent::issues::IssueState::parse)
                        .transpose()?,
                    title: title.clone(),
                    body: body.clone(),
                    notes: notes.clone(),
                    depends_on: depends_on.clone(),
                },
                current_unix_seconds(),
            );
            match result {
                Ok(record) => {
                    freshness.clear();
                    Ok((
                        issue_record_action_result(turn, action, "added", &record),
                        true,
                    ))
                }
                Err(error) => Ok((
                    ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message().to_string(),
                    )?,
                    false,
                )),
            }
        }
        AgentActionPayload::IssueUpdate {
            id,
            kind,
            state,
            title,
            body,
            clear_body,
            notes,
            clear_notes,
            depends_on,
            clear_depends_on,
        } => {
            let result = store.update_issue(
                project.to_string(),
                id.clone(),
                mez_agent::issues::IssueUpdate {
                    kind: kind
                        .as_deref()
                        .map(mez_agent::issues::IssueKind::parse)
                        .transpose()?,
                    state: state
                        .as_deref()
                        .map(mez_agent::issues::IssueState::parse)
                        .transpose()?,
                    title: title.clone(),
                    body: body.clone(),
                    clear_body: *clear_body,
                    notes: notes.clone(),
                    clear_notes: *clear_notes,
                    depends_on: depends_on.clone(),
                    clear_depends_on: *clear_depends_on,
                },
                current_unix_seconds(),
            );
            match result {
                Ok(result) => {
                    if result.updated {
                        freshness.clear();
                    }
                    let updated = result.updated;
                    Ok((issue_update_action_result(turn, action, &result), updated))
                }
                Err(error) => Ok((
                    ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message().to_string(),
                    )?,
                    false,
                )),
            }
        }
        AgentActionPayload::IssueQuery {
            kind,
            state,
            text,
            limit,
            refresh,
        } => {
            let kind = kind
                .as_deref()
                .map(mez_agent::issues::IssueKind::parse)
                .transpose()?;
            let state = state
                .as_deref()
                .map(mez_agent::issues::IssueState::parse)
                .transpose()?;
            let limit = limit.and_then(|value| usize::try_from(value).ok());
            let query = mez_agent::issues::IssueQuery::new_with_state(
                project.to_string(),
                kind,
                state.or(Some(mez_agent::issues::IssueState::Open)),
                text.clone(),
                limit,
            )?;
            let freshness_key = issue_query_freshness_key(&query);
            if !*refresh && let Some(reused_action_id) = freshness.get(&freshness_key) {
                return Ok((
                    issue_query_freshness_skip_action_result(
                        turn,
                        action,
                        &query,
                        reused_action_id,
                    ),
                    false,
                ));
            }
            match store.query_issues(&query) {
                Ok(records) => {
                    let result = issue_query_action_result(turn, action, &query, &records);
                    freshness.insert(freshness_key, action.id.clone());
                    Ok((result, false))
                }
                Err(error) => Ok((
                    ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message().to_string(),
                    )?,
                    false,
                )),
            }
        }
        AgentActionPayload::IssueDelete { id } => {
            match store.delete_issue(project.to_string(), id.clone()) {
                Ok(result) => {
                    if result.deleted {
                        freshness.clear();
                    }
                    let deleted = result.deleted;
                    Ok((issue_delete_action_result(turn, action, &result), deleted))
                }
                Err(error) => Ok((
                    ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message().to_string(),
                    )?,
                    false,
                )),
            }
        }
        _ => Err(MezError::invalid_args(
            "issue execution requires an issue action",
        )),
    }
}

pub(super) fn runtime_issues_enabled(service: &RuntimeSessionService) -> bool {
    runtime_effective_config_value(service.integration.config_layers())
        .ok()
        .and_then(|root| {
            root.get("issues")
                .and_then(|issues| issues.get("enabled"))
                .and_then(serde_json::Value::as_bool)
        })
        .unwrap_or(true)
}

pub(crate) fn runtime_issue_database_path(
    service: &RuntimeSessionService,
    config_root: &PathBuf,
) -> crate::storage::issues::IssueDatabasePath {
    let configured = runtime_effective_config_value(service.integration.config_layers())
        .ok()
        .and_then(|root| {
            root.get("issues")
                .and_then(|issues| issues.get("database_path"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    crate::storage::issues::issue_database_location(config_root, configured.as_deref())
}

pub(crate) fn issue_action_project(
    service: &RuntimeSessionService,
    turn: &AgentTurnRecord,
    config_root: &Path,
) -> String {
    service
        .pane_current_working_directory(&turn.pane_id)
        .unwrap_or_else(|| config_root.to_path_buf())
        .pipe(crate::storage::issues::project_key_for_working_directory)
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}

impl<T> Pipe for T {}
