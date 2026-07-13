//! Runtime agent skill discovery and invocation helpers.
//!
//! This module owns the runtime side of non-effecting MAAP skill actions. It
//! keeps catalog lookup, duplicate-call suppression, and skill-context
//! materialization together so the main runtime agent facade can focus on turn
//! orchestration.

use super::*;
use crate::agent::is_valid_skill_name;
use crate::project::TrustDecision;
use crate::skills::{SkillCatalog, discover_skill_catalog, load_skill_document};

impl RuntimeSessionService {
    /// Builds the effective skill catalog for one pane.
    ///
    /// User skills are always read from the configured user root. Project
    /// skills are included only when the pane is inside a trusted project root.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose current working directory scopes project skills.
    pub(in crate::runtime) fn effective_skill_catalog_for_pane(
        &self,
        pane_id: &str,
    ) -> SkillCatalog {
        let project_root = self.trusted_skill_project_root_for_pane(pane_id);
        discover_skill_catalog(self.config_root.as_deref(), project_root.as_deref())
    }

    /// Returns the trusted project root whose skills may apply to one pane.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose working directory determines project scope.
    fn trusted_skill_project_root_for_pane(&self, pane_id: &str) -> Option<PathBuf> {
        let working_directory = self.pane_current_working_directory(pane_id)?;
        let store = self.project_trust_store.as_ref()?;
        store
            .records()
            .filter(|record| record.state == TrustDecision::Trusted)
            .find(|record| {
                runtime_path_under_project_root(&working_directory, &record.project_root)
            })
            .map(|record| record.project_root.clone())
    }

    /// Builds the currently loaded skill context state for one active turn.
    ///
    /// Explicit `$skill` prompt expansion and successful `call_skill` results
    /// both place full skill text in the model context. The runtime uses that
    /// context as the source of truth for suppressing redundant non-effecting
    /// skill actions before they become unbounded provider continuations.
    fn runtime_skill_action_context_for_turn(
        &self,
        turn_id: &str,
    ) -> Result<RuntimeSkillActionContext> {
        let context = self
            .agent_turn_contexts
            .get(turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        Ok(runtime_skill_action_context_from_blocks(&context.blocks))
    }

    /// Returns a model-correctable failure for redundant skill actions.
    ///
    /// Skill lookup and load actions are non-effecting and normally trigger a
    /// provider continuation. When the requested skill context is already
    /// present, treating the duplicate as another success can produce a
    /// discovery/load loop. This result keeps the attempted action visible while
    /// steering the next provider turn toward real work.
    fn redundant_skill_action_failure(
        turn: &AgentTurnRecord,
        action: &AgentAction,
        code: &'static str,
        message: impl Into<String>,
    ) -> Result<ActionResult> {
        ActionResult::failed(turn, action, ActionStatus::Failed, code, message.into())
    }

    /// Executes a runtime-owned skill lookup or skill-load action.
    ///
    /// # Parameters
    /// - `turn`: Active turn receiving the action result.
    /// - `action`: `request_skills` or `call_skill` action to execute.
    fn execute_skill_action_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        skill_context: &mut RuntimeSkillActionContext,
    ) -> Result<ActionResult> {
        let catalog = self.effective_skill_catalog_for_pane(&turn.pane_id);
        match &action.payload {
            AgentActionPayload::RequestSkills => {
                if !skill_context.loaded_skills.is_empty() {
                    return Self::redundant_skill_action_failure(
                        turn,
                        action,
                        "skill_context_already_loaded",
                        format!(
                            "skill context is already loaded for this turn: {}; use the loaded skill guidance or request the missing action capability instead of discovering skills again",
                            skill_context
                                .loaded_skills
                                .iter()
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(",")
                        ),
                    );
                }
                if skill_context.catalog_requested {
                    return Self::redundant_skill_action_failure(
                        turn,
                        action,
                        "skill_catalog_already_requested",
                        "the effective skill catalog has already been returned for this turn; use an available skill or request the missing action capability instead of requesting the catalog again",
                    );
                }
                skill_context.catalog_requested = true;
                Ok(ActionResult::succeeded(
                    turn,
                    action,
                    vec![catalog.model_catalog_text()],
                    Some(catalog.structured_json()),
                ))
            }
            AgentActionPayload::CallSkill {
                name,
                additional_context,
            } => {
                if !is_valid_skill_name(name) {
                    return ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        "invalid_skill_name",
                        "skill name must contain only lowercase letters, digits, and hyphens",
                    );
                }
                let Some(summary) = catalog.get(name) else {
                    let available = if catalog.skills.is_empty() {
                        "none".to_string()
                    } else {
                        catalog.names().join(",")
                    };
                    return ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        "skill_not_found",
                        format!("skill {name:?} is not available; available skills: {available}"),
                    );
                };
                if skill_context.loaded_skills.contains(name) {
                    return Self::redundant_skill_action_failure(
                        turn,
                        action,
                        "skill_context_already_loaded",
                        format!(
                            "skill {name:?} is already loaded for this turn; use the loaded skill guidance or request the missing action capability instead of loading it again"
                        ),
                    );
                }
                let document = match load_skill_document(summary) {
                    Ok(document) => document,
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
                let content = self
                    .runtime_skill_context_text(document.clone(), additional_context.as_deref())?;
                let result = ActionResult::succeeded(
                    turn,
                    action,
                    vec![content],
                    Some(
                        serde_json::json!({
                            "name": &document.summary.name,
                            "source": document.summary.source.as_str(),
                            "path": document.summary.path.to_string_lossy(),
                            "skill_bytes": document.text.len(),
                            "additional_context_bytes": additional_context.as_deref().map(str::len).unwrap_or(0),
                        })
                        .to_string(),
                    ),
                );
                skill_context.loaded_skills.insert(name.clone());
                Ok(result)
            }
            _ => Err(MezError::invalid_args(
                "skill execution requires request_skills or call_skill action",
            )),
        }
    }

    /// Executes any provider-produced non-effecting skill actions and appends
    /// their results to running turn context for provider continuation.
    ///
    /// # Parameters
    /// - `turn`: Active turn containing the running action results.
    /// - `execution`: Provider execution whose pending skill results are updated.
    pub(super) fn execute_running_skill_actions_for_turn(
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
        let mut skill_context = self.runtime_skill_action_context_for_turn(&turn.turn_id)?;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Running
                || !matches!(
                    execution.action_results[index].action_type,
                    "request_skills" | "call_skill"
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
                    MezError::invalid_state("running skill result does not match an action")
                })?;
            if !self
                .append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, &action)?
            {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: {}",
                        runtime_agent_action_summary(&action)
                            .unwrap_or_else(|| "skill action".to_string())
                    ),
                )?;
            }
            execution.action_results[index] =
                self.execute_skill_action_for_turn(turn, &action, &mut skill_context)?;
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
                .filter(|result| matches!(result.action_type, "request_skills" | "call_skill"))
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
}
