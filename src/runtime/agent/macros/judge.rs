//! Provider-backed macro judge request and decision application.

use super::super::*;
#[cfg(test)]
use super::helpers::macro_judge_outcome_wire_value;
use super::helpers::{
    macro_judge_decision_from_text, runtime_macro_judge_policy, runtime_macro_judge_task,
};
use super::*;

impl RuntimeSessionService {
    /// Returns whether one parent turn is waiting for a structured macro judge
    /// response rather than an ordinary MAAP action batch.
    ///
    /// Macro judge turns reuse the parent provider task slot so the scheduler
    /// retains a normal progress path, but the provider response is parsed as a
    /// constrained JSON decision and then executed by the runtime.
    ///
    /// # Parameters
    /// - `turn_id`: Parent macro orchestration turn id.
    pub(in crate::runtime) fn macro_judge_step_index_for_turn(
        &self,
        turn_id: &str,
    ) -> Option<usize> {
        match self.macro_runs_by_parent_turn.get(turn_id)?.phase {
            MacroRunPhase::WaitingForJudge { step_index } => Some(step_index),
            _ => None,
        }
    }

    /// Executes one pending macro judge request through the parent model and
    /// applies the validated decision to the runtime-owned macro run.
    ///
    /// The provider response is intentionally not interpreted as MAAP. A valid
    /// `continue` decision dispatches the next scripted child step internally,
    /// while terminal decisions complete or fail the parent turn directly.
    ///
    /// # Parameters
    /// - `provider`: Model provider used for the parent pane.
    /// - `turn`: Parent macro orchestration turn currently running.
    /// - `model_profile`: Parent model profile used to make the judge request.
    /// - `step_index`: Completed step index that needs semantic judgment.
    #[cfg(test)]
    pub(in crate::runtime) fn execute_macro_judge_with_provider<P: ModelProvider>(
        &mut self,
        provider: &P,
        turn: &AgentTurnRecord,
        model_profile: &ModelProfile,
        step_index: usize,
    ) -> Result<AgentTurnExecution> {
        let request = self.macro_judge_model_request(turn, model_profile, step_index)?;
        self.append_provider_request_audit(turn, model_profile, provider.provider_id(), "started")?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "macro_judge_request started provider={} model={} step_index={}",
                provider.provider_id(),
                model_profile.model,
                step_index
            ),
        )?;
        let response = provider.send_request(&request)?;
        let decision =
            self.macro_judge_decision_from_response(&turn.turn_id, step_index, &response)?;
        self.apply_macro_judge_decision(turn, step_index, decision.clone())?;
        self.pending_agent_provider_tasks.remove(&turn.turn_id);
        self.claimed_agent_provider_tasks.remove(&turn.turn_id);
        self.append_provider_request_audit(
            turn,
            model_profile,
            provider.provider_id(),
            "succeeded",
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "macro_judge_response applied outcome={} step_index={}",
                macro_judge_outcome_wire_value(decision.outcome),
                step_index
            ),
        )?;
        Ok(AgentTurnExecution {
            request,
            response,
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: Vec::new(),
            final_turn: false,
            terminal_state: turn.state,
        })
    }

    /// Builds the structured provider request used to judge one completed
    /// macro step.
    pub(in crate::runtime) fn macro_judge_model_request(
        &self,
        turn: &AgentTurnRecord,
        model_profile: &ModelProfile,
        step_index: usize,
    ) -> Result<ModelRequest> {
        let run = self
            .macro_runs_by_parent_turn
            .get(turn.turn_id.as_str())
            .ok_or_else(|| {
                MezError::invalid_state("macro judge requested for unknown macro run")
            })?;
        let step = run
            .steps
            .get(step_index)
            .ok_or_else(|| MezError::invalid_state("macro judge step index is out of range"))?;
        let result = step
            .task_result
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("macro judge requested before child result"))?;
        let next_step = run.steps.get(step_index.saturating_add(1));
        Ok(ModelRequest {
            provider: model_profile.provider.clone(),
            model: model_profile.model.clone(),
            reasoning_effort: model_profile.reasoning_profile.clone(),
            thinking_enabled: model_profile.thinking_enabled(),
            latency_preference: model_profile.latency_preference.clone(),
            prompt_cache_retention: None,
            max_output_tokens: model_profile.max_output_tokens(),
            temperature: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: false,
            interaction_kind: ModelInteractionKind::MacroJudge,
            allowed_actions: AllowedActionSet::for_capability(
                mez_agent::AgentCapability::RespondOnly,
            ),
            stop: None,
            messages: vec![
                ModelMessage {
                    role: ModelMessageRole::System,
                    source: ContextSourceKind::RuntimeHint,
                    content: runtime_macro_judge_policy(),
                },
                ModelMessage {
                    role: ModelMessageRole::User,
                    source: ContextSourceKind::RuntimeHint,
                    content: runtime_macro_judge_task(run, step, result, next_step),
                },
            ],
        })
    }

    /// Parses and validates the structured JSON decision returned by the judge
    /// provider for one macro step.
    pub(super) fn macro_judge_decision_from_response(
        &self,
        turn_id: &str,
        step_index: usize,
        response: &ModelResponse,
    ) -> Result<MacroJudgeDecision> {
        let run = self
            .macro_runs_by_parent_turn
            .get(turn_id)
            .ok_or_else(|| MezError::invalid_state("macro judge response has no macro run"))?;
        macro_judge_decision_from_text(&response.raw_text, run.steps.len(), step_index)
    }

    /// Applies one validated macro judge decision to the parent macro run.
    pub(in crate::runtime) fn apply_macro_judge_provider_response(
        &mut self,
        turn: &AgentTurnRecord,
        step_index: usize,
        response: &ModelResponse,
    ) -> Result<()> {
        let decision =
            self.macro_judge_decision_from_response(&turn.turn_id, step_index, response)?;
        self.apply_macro_judge_decision(turn, step_index, decision)
    }

    /// Applies one validated macro judge decision to the parent macro run.
    pub(super) fn apply_macro_judge_decision(
        &mut self,
        turn: &AgentTurnRecord,
        step_index: usize,
        decision: MacroJudgeDecision,
    ) -> Result<()> {
        let next_step_index = step_index.saturating_add(1);
        if let Some(run) = self
            .macro_runs_by_parent_turn
            .get_mut(turn.turn_id.as_str())
        {
            let Some(step) = run.steps.get_mut(step_index) else {
                return Err(MezError::invalid_state(
                    "macro judge step index disappeared",
                ));
            };
            step.judgment = Some(decision.clone());
        }
        match decision.outcome {
            MacroJudgeOutcome::Continue | MacroJudgeOutcome::ContinueWithAdaptedPrompt => {
                let (child_agent_id, prompt, dispatch_status) = {
                    let run = self
                        .macro_runs_by_parent_turn
                        .get(turn.turn_id.as_str())
                        .ok_or_else(|| {
                            MezError::invalid_state(
                                "macro run disappeared before next step dispatch",
                            )
                        })?;
                    let next_step = run.steps.get(next_step_index).ok_or_else(|| {
                        MezError::invalid_state(
                            "macro judge requested continuation after final step",
                        )
                    })?;
                    let prompt = decision
                        .adapted_prompt
                        .clone()
                        .unwrap_or_else(|| next_step.scripted_prompt.clone());
                    let dispatch_status = if decision.outcome
                        == MacroJudgeOutcome::ContinueWithAdaptedPrompt
                    {
                        format!(
                            "judge adapted next prompt: {}; dispatched to {}; waiting for worker",
                            decision.rationale, run.child_agent_id
                        )
                    } else {
                        format!(
                            "judge continued; dispatched to {}; waiting for worker",
                            run.child_agent_id
                        )
                    };
                    (run.child_agent_id.clone(), prompt, dispatch_status)
                };
                let action = AgentAction {
                    id: format!("macro-step-{}", next_step_index.saturating_add(1)),
                    rationale: "send next macro step".to_string(),
                    payload: AgentActionPayload::SendMessage {
                        recipient: format!("agent:{child_agent_id}"),
                        content_type: "text/plain; charset=utf-8".to_string(),
                        payload: prompt.clone(),
                    },
                };
                let result = self
                    .queue_runtime_macro_step_prompt(
                        turn,
                        &action,
                        next_step_index,
                        &dispatch_status,
                    )?
                    .ok_or_else(|| {
                        MezError::invalid_state("macro judge next step was not accepted")
                    })?;
                if let Some(execution) = self.agent_turn_executions.get_mut(&turn.turn_id)
                    && let Some(batch) = execution.response.action_batch.as_mut()
                {
                    batch.actions.push(action);
                    execution.action_results.push(result);
                    execution.final_turn = false;
                    execution.terminal_state = AgentTurnState::Running;
                }
                self.agent_turn_ledger
                    .finish_turn(&turn.turn_id, AgentTurnState::Blocked)?;
                self.append_agent_trace_turn_transition(
                    turn,
                    AgentTurnState::Running,
                    AgentTurnState::Blocked,
                    "macro_judge_dispatched_next_step",
                )?;
            }
            MacroJudgeOutcome::RetryCurrentStep => {
                let (child_agent_id, prompt, retry_action_id, dispatch_status) = {
                    let run = self
                        .macro_runs_by_parent_turn
                        .get(turn.turn_id.as_str())
                        .ok_or_else(|| {
                            MezError::invalid_state(
                                "macro run disappeared before retry step dispatch",
                            )
                        })?;
                    let current_step = run.steps.get(step_index).ok_or_else(|| {
                        MezError::invalid_state(
                            "macro judge requested retry for missing current step",
                        )
                    })?;
                    let prompt = decision
                        .adapted_prompt
                        .clone()
                        .unwrap_or_else(|| current_step.scripted_prompt.clone());
                    let retry_action_id = format!(
                        "macro-step-{}-retry-{}",
                        step_index.saturating_add(1),
                        current_step
                            .child_turn_id
                            .clone()
                            .unwrap_or_else(|| "current".to_string())
                    );
                    let dispatch_status = format!(
                        "judge requested retry attempt {}: {}; dispatched to {}; waiting for worker",
                        current_step.attempts.saturating_add(1),
                        decision.rationale,
                        run.child_agent_id
                    );
                    (
                        run.child_agent_id.clone(),
                        prompt,
                        retry_action_id,
                        dispatch_status,
                    )
                };
                let action = AgentAction {
                    id: retry_action_id,
                    rationale: "retry current macro step".to_string(),
                    payload: AgentActionPayload::SendMessage {
                        recipient: format!("agent:{child_agent_id}"),
                        content_type: "text/plain; charset=utf-8".to_string(),
                        payload: prompt.clone(),
                    },
                };
                let result = self
                    .queue_runtime_macro_step_prompt(turn, &action, step_index, &dispatch_status)?
                    .ok_or_else(|| {
                        MezError::invalid_state("macro judge retry step was not accepted")
                    })?;
                if let Some(execution) = self.agent_turn_executions.get_mut(&turn.turn_id)
                    && let Some(batch) = execution.response.action_batch.as_mut()
                {
                    batch.actions.push(action);
                    execution.action_results.push(result);
                    execution.final_turn = false;
                    execution.terminal_state = AgentTurnState::Running;
                }
                self.agent_turn_ledger
                    .finish_turn(&turn.turn_id, AgentTurnState::Blocked)?;
                self.append_agent_trace_turn_transition(
                    turn,
                    AgentTurnState::Running,
                    AgentTurnState::Blocked,
                    "macro_judge_retried_current_step",
                )?;
            }
            MacroJudgeOutcome::StopFailure => {
                let message = decision
                    .user_message
                    .as_deref()
                    .unwrap_or(decision.rationale.as_str());
                let (child_agent_id, macro_name, total_steps) = self
                    .macro_runs_by_parent_turn
                    .get(turn.turn_id.as_str())
                    .map(|run| {
                        (
                            Some(run.child_agent_id.clone()),
                            run.macro_name.clone(),
                            run.steps.len(),
                        )
                    })
                    .unwrap_or_else(|| (None, "unknown".to_string(), step_index.saturating_add(1)));
                self.macro_runs_by_parent_turn.remove(&turn.turn_id);
                if let Some(child_agent_id) = child_agent_id.as_deref() {
                    let reason = "macro judge rejected subagent output";
                    self.close_subagent_descendants_for_parent_agent(child_agent_id, reason)?;
                    if let Some(child_pane_id) = runtime_agent_pane_id(child_agent_id) {
                        if self.find_pane_descriptor(child_pane_id.as_str()).is_some() {
                            if let Some(primary_client_id) =
                                self.session.primary_client_id().cloned()
                            {
                                self.dispatch_runtime_pane_close(
                                    &primary_client_id,
                                    &format!(
                                        r#"{{"pane_id":"{}","force":true}}"#,
                                        json_escape(child_pane_id.as_str())
                                    ),
                                )?;
                                self.append_lifecycle_event(
                                    EventKind::AgentStatus,
                                    format!(
                                        r#"{{"pane_id":"{}","agent_id":"{}","state":"closed","reason":"macro_judge_stop_failure","parent_agent_id":"{}","detail":"{}"}}"#,
                                        json_escape(child_pane_id.as_str()),
                                        json_escape(child_agent_id),
                                        json_escape(&turn.agent_id),
                                        json_escape(reason)
                                    ),
                                )?;
                            } else {
                                self.cleanup_removed_pane_runtime_state(child_pane_id.as_str());
                            }
                        } else {
                            self.cleanup_removed_pane_runtime_state(child_pane_id.as_str());
                        }
                    } else {
                        self.deregister_macro_managed_subagent(child_agent_id);
                        self.subagent_lineage.remove(child_agent_id);
                        self.subagent_scope_declarations.remove(child_agent_id);
                        self.subagent_scopes.unregister(child_agent_id);
                    }
                }
                self.append_agent_macro_error_to_terminal_buffer(
                    &turn.pane_id,
                    &macro_name,
                    step_index,
                    total_steps,
                    &format!("stopped: {message}"),
                )?;
                self.complete_running_agent_turn_and_start_ready(
                    turn,
                    AgentTurnState::Failed,
                    "macro_judge_stop_failure",
                )?;
            }
            MacroJudgeOutcome::FinishSuccess => {
                let (child_agent_id, macro_name, total_steps) = self
                    .macro_runs_by_parent_turn
                    .get(turn.turn_id.as_str())
                    .map(|run| {
                        (
                            Some(run.child_agent_id.clone()),
                            run.macro_name.clone(),
                            run.steps.len(),
                        )
                    })
                    .unwrap_or_else(|| (None, "unknown".to_string(), step_index.saturating_add(1)));
                self.macro_runs_by_parent_turn.remove(&turn.turn_id);
                if let Some(child_agent_id) = child_agent_id.as_deref() {
                    let reason = "macro completed successfully";
                    self.close_subagent_descendants_for_parent_agent(child_agent_id, reason)?;
                    if let Some(child_pane_id) = runtime_agent_pane_id(child_agent_id) {
                        if self.find_pane_descriptor(child_pane_id.as_str()).is_some() {
                            if let Some(primary_client_id) =
                                self.session.primary_client_id().cloned()
                            {
                                self.dispatch_runtime_pane_close(
                                    &primary_client_id,
                                    &format!(
                                        r#"{{"pane_id":"{}","force":true}}"#,
                                        json_escape(child_pane_id.as_str())
                                    ),
                                )?;
                                self.append_lifecycle_event(
                                    EventKind::AgentStatus,
                                    format!(
                                        r#"{{"pane_id":"{}","agent_id":"{}","state":"closed","reason":"macro_judge_finish_success","parent_agent_id":"{}","detail":"{}"}}"#,
                                        json_escape(child_pane_id.as_str()),
                                        json_escape(child_agent_id),
                                        json_escape(&turn.agent_id),
                                        json_escape(reason)
                                    ),
                                )?;
                            } else {
                                self.cleanup_removed_pane_runtime_state(child_pane_id.as_str());
                            }
                        } else {
                            self.cleanup_removed_pane_runtime_state(child_pane_id.as_str());
                        }
                    } else {
                        self.deregister_macro_managed_subagent(child_agent_id);
                        self.subagent_lineage.remove(child_agent_id);
                        self.subagent_scope_declarations.remove(child_agent_id);
                        self.subagent_scopes.unregister(child_agent_id);
                    }
                }
                self.append_agent_macro_status_to_terminal_buffer(
                    &turn.pane_id,
                    &macro_name,
                    Some(step_index),
                    total_steps,
                    "completed",
                )?;
                self.complete_running_agent_turn_and_start_ready(
                    turn,
                    AgentTurnState::Completed,
                    "macro_judge_finish_success",
                )?;
            }
        }
        Ok(())
    }
}
