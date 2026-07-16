//! Runtime agent skill discovery and invocation helpers.
//!
//! This module owns the runtime side of non-effecting MAAP skill actions. It
//! keeps catalog lookup, duplicate-call suppression, and skill-context
//! materialization together so the main runtime agent facade can focus on turn
//! orchestration.

use super::{
    ActionResult, ActionStatus, AgentAction, AgentTurnExecution, AgentTurnRecord, AgentTurnState,
    ContextBlock, ContextSourceKind, MezError, PathBuf, Result, RuntimeSessionService,
    action_result_context_content, runtime_agent_action_summary,
    runtime_agent_turn_state_from_action_results,
    runtime_execution_ready_for_provider_continuation, runtime_mezzanine_error_code,
    runtime_path_under_project_root,
};
use crate::project::TrustDecision;
use crate::skills::{discover_skill_catalog, load_skill_document};
use mez_agent::{
    SkillActionContext, SkillActionPlan, SkillCatalog, plan_skill_action,
    skill_action_context_from_blocks, skill_load_action_result,
};

impl RuntimeSessionService {
    /// Builds the effective skill catalog for one pane.
    ///
    /// User skills are always read from the configured user root. Project
    /// skills are included only when the pane is inside a trusted project root.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose current working directory scopes project skills.
    pub(crate) fn effective_skill_catalog_for_pane(&self, pane_id: &str) -> SkillCatalog {
        let project_root = self.trusted_skill_project_root_for_pane(pane_id);
        discover_skill_catalog(self.integration.config_root(), project_root.as_deref())
    }

    /// Returns the trusted project root whose skills may apply to one pane.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose working directory determines project scope.
    fn trusted_skill_project_root_for_pane(&self, pane_id: &str) -> Option<PathBuf> {
        let working_directory = self.pane_current_working_directory(pane_id)?;
        let store = self.integration.project_trust_store()?;
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
    fn runtime_skill_action_context_for_turn(&self, turn_id: &str) -> Result<SkillActionContext> {
        let context = self
            .agent_turn_contexts()
            .get(turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        Ok(skill_action_context_from_blocks(&context.blocks))
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
        skill_context: &mut SkillActionContext,
    ) -> Result<ActionResult> {
        let catalog = self.effective_skill_catalog_for_pane(&turn.pane_id);
        match plan_skill_action(turn, action, &catalog, skill_context)? {
            SkillActionPlan::Result(result) => Ok(result),
            SkillActionPlan::Load {
                summary,
                additional_context,
            } => {
                let document = match load_skill_document(&summary) {
                    Ok(document) => document,
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
                let content = self
                    .runtime_skill_context_text(document.clone(), additional_context.as_deref())?;
                let result = skill_load_action_result(
                    turn,
                    action,
                    &document,
                    additional_context.as_deref(),
                    content,
                );
                skill_context
                    .loaded_skills
                    .insert(document.summary.name.clone());
                Ok(result)
            }
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
                self.agent_turn_contexts_mut()
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
            self.agent
                .pending_agent_provider_tasks
                .insert(turn.turn_id.clone());
        }
        Ok(executed)
    }
}
