//! Runtime agent macro discovery and managed-step helpers.
//!
//! This module keeps pane-scoped macro catalog discovery beside the skill
//! discovery helpers. It also owns the narrow bridge that lets macro-managed
//! step traffic become ordinary agent-shell turns in a persistent child
//! subagent session.

use super::*;
#[cfg(test)]
use crate::agent::ModelProvider;
use crate::agent::{
    AllowedActionSet, ModelInteractionKind, ModelMessage, ModelMessageRole, ModelProfile,
    ModelRequest, ModelResponse,
};
use crate::macros::{MacroCatalog, MacroDefinition, discover_macro_catalog, load_macro_definition};
use crate::project::TrustDecision;
use crate::runtime::agent_state::RuntimeAgentLoopCompletion;
use crate::runtime::service_state::{
    MacroJudgeDecision, MacroJudgeOutcome, MacroManagedSubagent, MacroRunPhase, MacroRunState,
    MacroRunStep,
};
use crate::runtime::{
    AgentShellCommandOutcome, AgentShellRuntimeContext, RuntimeAgentPromptTurnStart,
    execute_agent_shell_command_with_context,
};
use mez_agent::{ScheduledWork, ScheduledWorkKind};

impl RuntimeSessionService {
    /// Builds the effective macro catalog for one pane.
    ///
    /// User macros are read from the configured user root. Project macros are
    /// included only when the pane is inside a trusted project root.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose current working directory scopes project macros.
    pub(in crate::runtime) fn effective_macro_catalog_for_pane(
        &self,
        pane_id: &str,
    ) -> MacroCatalog {
        let project_root = self.trusted_macro_project_root_for_pane(pane_id);
        discover_macro_catalog(self.config_root.as_deref(), project_root.as_deref())
    }

    /// Returns the trusted project root whose macros may apply to one pane.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose working directory determines project scope.
    fn trusted_macro_project_root_for_pane(&self, pane_id: &str) -> Option<PathBuf> {
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

    /// Registers one spawned subagent as macro-managed for one macro run.
    ///
    /// Future macro orchestration uses this marker after creating the one
    /// persistent child session and parent orchestration turn for a macro run.
    /// Only the owning parent turn may bridge `send_message` traffic into
    /// agent-shell steps, which preserves ordinary MMP behavior for unrelated
    /// ad hoc subagent messages and later parent turns.
    ///
    /// # Parameters
    /// - `child_agent_id`: Runtime child agent id, such as `agent-%2`.
    /// - `parent_turn_id`: Parent macro orchestration turn id.
    /// - `parent_agent_id`: Parent pane agent id that owns the macro.
    /// - `macro_name`: Macro name used for diagnostics and traceability.
    pub fn register_macro_managed_subagent(
        &mut self,
        child_agent_id: &str,
        parent_turn_id: &str,
        parent_agent_id: &str,
        macro_name: &str,
    ) {
        self.macro_managed_subagent_agents.insert(
            child_agent_id.to_string(),
            MacroManagedSubagent {
                parent_turn_id: parent_turn_id.to_string(),
                parent_agent_id: parent_agent_id.to_string(),
                macro_name: macro_name.to_string(),
            },
        );
    }

    /// Removes a subagent from the macro-managed set.
    ///
    /// Must be called whenever a macro-managed child pane closes, fails to
    /// spawn, or is torn down with its parent. This prevents stale entries
    /// from accumulating and prevents recycled pane ids from hijacking
    /// macro bridge routing.
    ///
    /// # Parameters
    /// - `child_agent_id`: Runtime child agent id, such as `agent-%2`.
    pub fn deregister_macro_managed_subagent(&mut self, child_agent_id: &str) {
        self.macro_managed_subagent_agents.remove(child_agent_id);
    }

    /// Records a newly started macro run before any step is submitted.
    ///
    /// The loaded step list is copied into runtime state so later file edits to
    /// the macro definition cannot change an in-flight run. The parent turn id
    /// is the stable run id for the current macro format.
    fn register_macro_run_state(
        &mut self,
        pane_id: &str,
        prompt: &str,
        definition: &MacroDefinition,
        additional_context: Option<&str>,
        started: &RuntimeAgentPromptTurnStart,
        child_agent_id: &str,
    ) {
        let steps = definition
            .steps
            .iter()
            .enumerate()
            .map(|(index, step)| MacroRunStep {
                index,
                attempts: 0,
                scripted_prompt: step.prompt.clone(),
                submitted_prompt: None,
                child_turn_id: None,
                task_result: None,
                judgment: None,
            })
            .collect();
        self.macro_runs_by_parent_turn.insert(
            started.turn_id.clone(),
            MacroRunState {
                run_id: started.turn_id.clone(),
                parent_turn_id: started.turn_id.clone(),
                parent_agent_id: started.agent_id.clone(),
                parent_pane_id: pane_id.to_string(),
                child_agent_id: child_agent_id.to_string(),
                macro_name: definition.summary.name.clone(),
                macro_description: definition.summary.description.clone(),
                invocation_prompt: prompt.to_string(),
                invocation_context: additional_context.map(ToOwned::to_owned),
                steps,
                current_step: 0,
                phase: MacroRunPhase::DispatchingStep { step_index: 0 },
            },
        );
    }

    /// Starts the parent orchestration turn for an explicit `#macro` prompt.
    ///
    /// The runtime loads the configured macro, creates one persistent child
    /// subagent, marks that child as macro-managed for step routing, and
    /// delivers the first scripted step through runtime-owned macro-step
    /// routing before the parent model is asked for a structured judge
    /// decision. The runtime remains responsible for submitting every step;
    /// the parent model only judges completed child results.
    ///
    /// # Parameters
    /// - `pane_id`: Parent pane where the user invoked the macro.
    /// - `prompt`: Original user prompt beginning with `#<macro-name>`.
    pub(in crate::runtime) fn start_agent_macro_prompt_turn(
        &mut self,
        pane_id: &str,
        prompt: &str,
    ) -> Result<RuntimeAgentPromptTurnStart> {
        let invocation = crate::macros::parse_macro_prompt_invocation(prompt)
            .ok_or_else(|| MezError::invalid_args("macro prompt must start with #<macro-name>"))?;
        let catalog = self.effective_macro_catalog_for_pane(pane_id);
        let summary = catalog.get(&invocation.name).cloned().ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                format!("agent macro is not available: #{}", invocation.name),
            )
        })?;
        let definition = load_macro_definition(&summary)?;
        let controller =
            self.session.primary_client_id().cloned().ok_or_else(|| {
                MezError::invalid_state("agent macro requires an attached primary")
            })?;
        let parent_agent_id = format!("agent-{pane_id}");
        let params = serde_json::json!({
            "parent_agent": { "agent_id": parent_agent_id },
            "placement": "new-window",
            "role": "worker",
            "cooperation_mode": "owned-write",
           "prompt": "",
           "skip_initial_turn": true,
        })
        .to_string();
        let spawn = runtime_subagent_spawn_request(&params, false)?;
        let placement = runtime_subagent_placement_mode(&params)?;
        let spawn_json = self.spawn_runtime_subagent(&controller, spawn, placement)?;
        let (child_agent_id, _child_display_name, _child_turn_id) =
            runtime_spawn_json_agent_and_turn(&spawn_json)?;
        // idle spawn: child_turn_id is None, which is expected for macro session
        let _ = _child_turn_id;
        self.append_agent_trace_turn_event(
            pane_id,
            "",
            &format!("macro child spawned idle child_agent_id={}", child_agent_id),
        )?;
        let orchestration_prompt = runtime_macro_parent_orchestration_prompt(
            &definition,
            invocation.additional_context.as_deref(),
            &child_agent_id,
        );
        let started = self.start_agent_prompt_turn_with_cooperation(
            pane_id,
            &orchestration_prompt,
            Some("macro-orchestration".to_string()),
            Some(crate::agent::AgentCapability::Subagent),
        )?;
        self.register_macro_managed_subagent(
            &child_agent_id,
            &started.turn_id,
            &parent_agent_id,
            &definition.summary.name,
        );
        self.register_macro_run_state(
            pane_id,
            prompt,
            &definition,
            invocation.additional_context.as_deref(),
            &started,
            &child_agent_id,
        );
        self.append_agent_macro_status_to_terminal_buffer(
            pane_id,
            &definition.summary.name,
            None,
            definition.steps.len(),
            &format!(
                "started; {} steps; worker {child_agent_id}",
                definition.steps.len()
            ),
        )?;
        self.queue_runtime_owned_first_macro_step(
            pane_id,
            &started,
            &definition,
            invocation.additional_context.as_deref(),
            &child_agent_id,
        )?;
        self.append_agent_trace_turn_event(
            pane_id,
            &started.turn_id,
            &format!(
                "macro orchestration started name={} child_agent_id={}",
                definition.summary.name, child_agent_id
            ),
        )?;
        Ok(started)
    }

    /// Queues the first macro step without requiring the parent model to emit
    /// an initial `send_message` action.
    ///
    /// Macro startup owns the deterministic first handoff so a provider that
    /// returns ordinary text instead of a MAAP batch cannot fail the run before
    /// any child work starts. The synthetic parent execution mirrors the shape
    /// produced by a model-owned `send_message` action so the existing joined
    /// subagent resolution path can append the child result and resume provider
    /// orchestration for later steps.
    fn queue_runtime_owned_first_macro_step(
        &mut self,
        pane_id: &str,
        started: &RuntimeAgentPromptTurnStart,
        definition: &MacroDefinition,
        additional_context: Option<&str>,
        child_agent_id: &str,
    ) -> Result<()> {
        let parent_turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == started.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("macro parent turn disappeared"))?;
        let first_step = definition
            .steps
            .first()
            .ok_or_else(|| MezError::invalid_state("agent macro has no scripted steps"))?;
        let payload =
            runtime_macro_initial_step_prompt(first_step.prompt.as_str(), additional_context);
        let action = AgentAction {
            id: "macro-step-1".to_string(),
            rationale: "send first macro step".to_string(),
            payload: AgentActionPayload::SendMessage {
                recipient: format!("agent:{child_agent_id}"),
                content_type: "text/plain; charset=utf-8".to_string(),
                payload,
            },
        };
        let result = self
            .queue_runtime_macro_step_prompt(
                &parent_turn,
                &action,
                0,
                &format!("dispatched to {child_agent_id}; waiting for worker"),
            )?
            .ok_or_else(|| MezError::invalid_state("runtime-owned macro step was not accepted"))?;
        let batch = MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "send first macro step".to_string(),
            thought: None,
            turn_id: parent_turn.turn_id.clone(),
            agent_id: parent_turn.agent_id.clone(),
            actions: vec![action],
            final_turn: false,
        };
        self.agent_turn_executions.insert(
            parent_turn.turn_id.clone(),
            AgentTurnExecution {
                request: runtime_owned_macro_step_model_request(&parent_turn),
                response: ModelResponse {
                    provider: "runtime".to_string(),
                    model: "macro-orchestration".to_string(),
                    raw_text: "runtime-owned macro first step".to_string(),
                    usage: Default::default(),
                    latest_request_usage: None,
                    quota_usage: Default::default(),
                    action_batch: Some(batch),
                    provider_transcript_events: Vec::new(),
                },
                latest_response_usage: Default::default(),
                routing_token_usage_by_model: Default::default(),
                action_results: vec![result],
                final_turn: false,
                terminal_state: AgentTurnState::Running,
            },
        );
        self.pending_agent_provider_tasks
            .remove(&parent_turn.turn_id);
        self.claimed_agent_provider_tasks
            .remove(&parent_turn.turn_id);
        self.agent_turn_ledger
            .finish_turn(&parent_turn.turn_id, AgentTurnState::Blocked)?;
        self.append_agent_trace_turn_transition(
            &parent_turn,
            AgentTurnState::Running,
            AgentTurnState::Blocked,
            "runtime_owned_macro_first_step",
        )?;
        self.append_agent_trace_turn_event(
            pane_id,
            &parent_turn.turn_id,
            "provider_task removed reason=runtime_owned_macro_first_step",
        )?;
        Ok(())
    }

    /// Queues one runtime-owned macro step and records the submitted child turn.
    ///
    /// This helper preserves the existing macro bridge behavior while adding
    /// explicit per-run step state for harness-owned sequencing.
    fn queue_runtime_macro_step_prompt(
        &mut self,
        parent_turn: &AgentTurnRecord,
        action: &AgentAction,
        step_index: usize,
        dispatch_status: &str,
    ) -> Result<Option<ActionResult>> {
        let AgentActionPayload::SendMessage {
            recipient,
            content_type,
            payload,
        } = &action.payload
        else {
            return Err(MezError::invalid_args(
                "runtime macro step action must send a message",
            ));
        };
        let already_recorded_step_action =
            self.joined_subagent_dependencies
                .values()
                .any(|dependency| {
                    dependency.parent_turn_id == parent_turn.turn_id
                        && dependency.parent_action_id == action.id
                })
                || self.agent_loops_by_pane.values().any(|state| {
                    state.completion.as_ref().is_some_and(|completion| {
                        completion.parent_turn_id == parent_turn.turn_id
                            && completion.parent_action_id == action.id
                    })
                });
        let result = self.queue_macro_managed_message_step(
            parent_turn,
            action,
            recipient.as_str(),
            content_type.as_str(),
            payload.as_str(),
        )?;
        if result.is_some() {
            let child_turn_id = self
                .joined_subagent_dependencies
                .values()
                .find(|dependency| {
                    dependency.parent_turn_id == parent_turn.turn_id
                        && dependency.parent_action_id == action.id
                })
                .map(|dependency| dependency.child_turn_id.clone())
                .or_else(|| {
                    self.agent_loops_by_pane.values().find_map(|state| {
                        state.completion.as_ref().and_then(|completion| {
                            (completion.parent_turn_id == parent_turn.turn_id
                                && completion.parent_action_id == action.id)
                                .then(|| completion.child_turn_id.clone())
                        })
                    })
                });
            if child_turn_id.is_some() && !already_recorded_step_action {
                let (macro_name, total_steps) = self
                    .macro_runs_by_parent_turn
                    .get(parent_turn.turn_id.as_str())
                    .map(|run| (run.macro_name.clone(), run.steps.len()))
                    .ok_or_else(|| {
                        MezError::invalid_state("macro run disappeared before step display")
                    })?;
                self.append_agent_macro_status_to_terminal_buffer(
                    &parent_turn.pane_id,
                    &macro_name,
                    Some(step_index),
                    total_steps,
                    dispatch_status,
                )?;
                self.append_agent_user_prompt_to_terminal_buffer(&parent_turn.pane_id, payload)?;
            }
            if let Some(run) = self
                .macro_runs_by_parent_turn
                .get_mut(parent_turn.turn_id.as_str())
            {
                run.current_step = step_index;
                if let Some(step) = run.steps.get_mut(step_index) {
                    step.attempts = step.attempts.saturating_add(1);
                    step.submitted_prompt = Some(payload.to_string());
                    step.child_turn_id = child_turn_id.clone();
                    step.task_result = None;
                    step.judgment = None;
                }
                if let Some(child_turn_id) = child_turn_id {
                    run.phase = MacroRunPhase::WaitingForStep {
                        step_index,
                        child_turn_id: child_turn_id.clone(),
                    };
                    self.macro_run_by_child_turn
                        .insert(child_turn_id, parent_turn.turn_id.clone());
                } else {
                    run.phase = MacroRunPhase::DispatchingStep { step_index };
                }
            }
        }
        Ok(result)
    }

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
                crate::agent::AgentCapability::RespondOnly,
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
    fn macro_judge_decision_from_response(
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
    fn apply_macro_judge_decision(
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

    /// Starts a normal child agent-shell turn for a macro step message.
    ///
    /// The bridge is intentionally limited to macro-managed child agents and
    /// text payloads. When it applies, the message payload is queued through the
    /// same scheduler and provider path as an ordinary prompt submitted in the
    /// child subagent shell, which preserves slash-command behavior such as
    /// `/loop` while keeping the parent action result tied to the child task
    /// result route.
    ///
    /// # Parameters
    /// - `parent_turn`: Parent turn that emitted the `send_message` action.
    /// - `action`: Parent action whose result should wait for the child step.
    /// - `recipient`: Model-supplied recipient string from the action.
    /// - `content_type`: Canonical MMP content type for the payload.
    /// - `payload`: Text prompt to queue in the child agent shell.
    pub(in crate::runtime) fn queue_macro_managed_message_step(
        &mut self,
        parent_turn: &AgentTurnRecord,
        action: &AgentAction,
        recipient: &str,
        content_type: &str,
        payload: &str,
    ) -> Result<Option<ActionResult>> {
        if content_type != "text/plain; charset=utf-8" {
            return Ok(None);
        }
        let Some(child_agent_id) = macro_message_recipient_agent_id(recipient) else {
            return Ok(None);
        };
        let Some(macro_owner) = self
            .macro_managed_subagent_agents
            .get(child_agent_id.as_str())
        else {
            return Ok(None);
        };
        let Some(child_lineage) = self.subagent_lineage.get(child_agent_id.as_str()) else {
            return Ok(Some(ActionResult::failed(
                parent_turn,
                action,
                ActionStatus::Failed,
                "macro_bridge_error",
                "macro-managed subagent lineage is missing",
            )?));
        };
        let child_parent_agent_id = child_lineage.parent_agent_id.clone();
        let child_display_name = child_lineage.display_name.clone();
        if child_parent_agent_id != parent_turn.agent_id
            || macro_owner.parent_agent_id != parent_turn.agent_id
            || macro_owner.parent_turn_id != parent_turn.turn_id
        {
            return Ok(Some(ActionResult::failed(
                parent_turn,
                action,
                ActionStatus::Failed,
                "macro_bridge_error",
                "macro-managed subagent step recipient does not belong to this macro run",
            )?));
        }
        let child_pane_id = child_agent_id
            .strip_prefix("agent-")
            .ok_or_else(|| MezError::invalid_state("macro-managed child agent id is invalid"))?;
        runtime_pane_by_id(&self.session, child_pane_id)?;
        // --- Idempotency guard: retried parent actions reuse the original
        // step result instead of creating another child turn. Check this before
        // the generic in-flight guard so retries of the same accepted action
        // remain safe while the child turn is still running. ---
        if let Some(existing) = self.joined_subagent_dependencies.values().find(|dep| {
            dep.parent_turn_id == parent_turn.turn_id && dep.parent_action_id == action.id
        }) {
            if self.joined_subagent_dependency_has_live_child(existing) {
                // Still in progress — return the same running result.
                return Ok(Some(ActionResult::running(
                    parent_turn,
                    action,
                    vec![format!(
                        "macro step already in progress for {child_agent_id}; waiting for subagent result"
                    )],
                    Some(format!(
                        r#"{{"recipient":"{}","delivery_status":"accepted","join_policy":"macro_step","join_state":"waiting","child_agent_id":"{}","child_turn_id":"{}","idempotent":true,"error":null}}"#,
                        json_escape(recipient),
                        json_escape(&child_agent_id),
                        json_escape(&existing.child_turn_id)
                    )),
                )));
            }
            // Child turn already reached a terminal state — return idempotent
            // terminal result.
            let child_state = self
                .agent_turn_ledger
                .turns()
                .iter()
                .find(|t| t.turn_id == existing.child_turn_id)
                .map(|t| t.state);
            match child_state {
                Some(AgentTurnState::Completed) => {
                    return Ok(Some(ActionResult::succeeded(
                        parent_turn,
                        action,
                        vec![format!(
                            "macro step already completed by {child_agent_id} (idempotent)"
                        )],
                        Some(format!(
                            r#"{{"recipient":"{}","delivery_status":"completed","join_policy":"macro_step","child_agent_id":"{}","child_turn_id":"{}","idempotent":true,"error":null}}"#,
                            json_escape(recipient),
                            json_escape(&child_agent_id),
                            json_escape(&existing.child_turn_id)
                        )),
                    )));
                }
                Some(AgentTurnState::Failed) | Some(AgentTurnState::Interrupted) => {
                    return Ok(Some(ActionResult::failed(
                        parent_turn,
                        action,
                        ActionStatus::Failed,
                        "macro_step_failed",
                        "macro step previously failed; cannot retry",
                    )?));
                }
                _ => {
                    // Other terminal state — treat as resolved.
                    return Ok(Some(ActionResult::succeeded(
                        parent_turn,
                        action,
                        vec![format!(
                            "macro step already resolved by {child_agent_id} (idempotent)"
                        )],
                        Some(format!(
                            r#"{{"recipient":"{}","delivery_status":"resolved","join_policy":"macro_step","child_agent_id":"{}","child_turn_id":"{}","idempotent":true,"error":null}}"#,
                            json_escape(recipient),
                            json_escape(&child_agent_id),
                            json_escape(&existing.child_turn_id)
                        )),
                    )));
                }
            }
        }
        // --- Ordering guard: reject if a different macro step is already
        // in-flight for this parent turn + child agent pair. ---
        let macro_step_in_flight = self.joined_subagent_dependencies.values().any(|dep| {
            dep.parent_turn_id == parent_turn.turn_id
                && dep.child_agent_id == child_agent_id
                && self.joined_subagent_dependency_has_live_child(dep)
        }) || self.agent_loops_by_pane.values().any(|state| {
            state.completion.as_ref().is_some_and(|completion| {
                completion.parent_turn_id == parent_turn.turn_id
                    && completion.child_agent_id == child_agent_id
            })
        });
        if macro_step_in_flight {
            return Ok(Some(ActionResult::failed(
                parent_turn,
                action,
                ActionStatus::Failed,
                "macro_step_ordering",
                "a macro step is already in flight for this subagent; wait for it to complete before sending the next step",
            )?));
        }
        let mcp_summary = self.mcp_registry.agent_shell_summary();
        let permission_summary = self.permission_policy.agent_shell_summary();
        let parsed_command = execute_agent_shell_command_with_context(
            &mut self.agent_shell_store,
            child_pane_id,
            payload,
            AgentShellRuntimeContext {
                mcp_summary: Some(&mcp_summary),
                permission_summary: Some(&permission_summary),
            },
        );
        if matches!(
            parsed_command.as_ref(),
            Ok(Some(AgentShellCommandOutcome::RequiresRuntime { command, .. })) if command == "loop"
        ) {
            let loop_outcome = match self.execute_agent_shell_loop_command(child_pane_id, payload) {
                Ok(outcome) => outcome,
                Err(error) => {
                    return Ok(Some(ActionResult::failed(
                        parent_turn,
                        action,
                        ActionStatus::Failed,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message().to_string(),
                    )?));
                }
            };
            let child_turn_id = self
                .agent_loop_turns
                .iter()
                .find(|(_, loop_turn)| loop_turn.pane_id == child_pane_id)
                .map(|(turn_id, _)| turn_id.clone())
                .ok_or_else(|| MezError::invalid_state("macro loop did not create a work turn"))?;
            self.subagent_task_routes
                .insert(child_turn_id.clone(), parent_turn.agent_id.clone());
            let loop_state = self
                .agent_loops_by_pane
                .get_mut(child_pane_id)
                .ok_or_else(|| MezError::invalid_state("macro loop controller is unavailable"))?;
            if loop_state.completion.is_some() {
                return Err(MezError::invalid_state(
                    "macro loop controller already has a parent completion",
                ));
            }
            loop_state.completion = Some(RuntimeAgentLoopCompletion {
                parent_turn_id: parent_turn.turn_id.clone(),
                parent_action_id: action.id.clone(),
                child_turn_id: child_turn_id.clone(),
                child_agent_id: child_agent_id.to_string(),
                child_display_name: Some(child_display_name.clone()),
            });
            return Ok(Some(ActionResult::running(
                parent_turn,
                action,
                vec![format!(
                    "macro loop step delivered to {child_agent_id}; waiting for loop result"
                )],
                Some(format!(
                    r#"{{"recipient":"{}","delivery_status":"accepted","join_policy":"macro_step","join_state":"waiting","child_agent_id":"{}","child_turn_id":"{}","command":"loop","outcome":"{}","error":null}}"#,
                    json_escape(recipient),
                    json_escape(&child_agent_id),
                    json_escape(&child_turn_id),
                    json_escape(&format!("{loop_outcome:?}"))
                )),
            )));
        }
        let context = self.agent_context_for_pane_prompt(child_pane_id, payload, 100)?;
        let context = self.apply_agent_shell_preference_context(child_pane_id, context)?;
        let turn_id = self.next_agent_turn_id();
        let created_at_unix_seconds = current_unix_seconds();
        let (model_profile_name, model_profile) =
            self.active_model_profile_for_pane(child_pane_id, &child_agent_id, None)?;
        let turn = AgentTurnRecord {
            turn_id: turn_id.clone(),
            agent_id: child_agent_id.to_string(),
            pane_id: child_pane_id.to_string(),
            trigger: crate::agent::AgentTurnTrigger::LocalMessage,
            started_at_unix_seconds: created_at_unix_seconds,
            policy_profile: "runtime".to_string(),
            model_profile: model_profile_name.clone(),
            parent_turn_id: Some(parent_turn.turn_id.clone()),
            cooperation_mode: Some("macro-step".to_string()),
            state: AgentTurnState::Queued,
            initial_capability: None,
        };
        self.agent_turn_ledger.queue_turn(turn.clone())?;
        self.agent_turn_contexts.insert(turn_id.clone(), context);
        self.agent_turn_model_profiles
            .insert(turn_id.clone(), model_profile);
        self.subagent_task_routes
            .insert(turn_id.clone(), parent_turn.agent_id.clone());
        self.joined_subagent_dependencies.insert(
            turn_id.clone(),
            JoinedSubagentDependency {
                parent_turn_id: parent_turn.turn_id.clone(),
                parent_action_id: action.id.clone(),
                child_turn_id: turn_id.clone(),
                child_agent_id: child_agent_id.to_string(),
                child_display_name: Some(child_display_name.clone()),
            },
        );
        self.append_agent_user_prompt_to_terminal_buffer(child_pane_id, payload)?;
        self.agent_scheduler.enqueue(ScheduledWork {
            turn_id: turn_id.clone(),
            agent_id: child_agent_id.to_string(),
            pane_id: Some(child_pane_id.to_string()),
            kind: ScheduledWorkKind::ShellCapable,
        })?;
        self.append_agent_trace_turn_event(
            child_pane_id,
            &turn_id,
            "created state=queued reason=macro_message_step",
        )?;
        self.append_agent_trace_turn_event(
            child_pane_id,
            &turn_id,
            &format!(
                "context prepared blocks={} model_profile={}",
                self.agent_turn_contexts
                    .get(&turn_id)
                    .map(|context| context.blocks.len())
                    .unwrap_or_default(),
                model_profile_name
            ),
        )?;
        self.append_agent_trace_turn_event(
            child_pane_id,
            &turn_id,
            "scheduler enqueue kind=shell_capable reason=macro_message_step",
        )?;
        self.start_ready_agent_turns()?;
        Ok(Some(ActionResult::running(
            parent_turn,
            action,
            vec![format!(
                "macro step delivered to {child_agent_id}; waiting for subagent result"
            )],
            Some(format!(
                r#"{{"recipient":"{}","delivery_status":"accepted","join_policy":"macro_step","join_state":"waiting","child_agent_id":"{}","child_turn_id":"{}","error":null}}"#,
                json_escape(recipient),
                json_escape(&child_agent_id),
                json_escape(&turn_id)
            )),
        )))
    }
}

/// Returns the target agent id for a direct agent-recipient string.
fn macro_message_recipient_agent_id(recipient: &str) -> Option<String> {
    recipient
        .strip_prefix("agent:")
        .filter(|agent_id| !agent_id.trim().is_empty())
        .map(|id| id.trim().to_owned())
        .or_else(|| {
            recipient
                .starts_with("agent-%")
                .then(|| recipient.to_string())
        })
}

/// Builds the parent model prompt that orchestrates one active macro run.
fn runtime_macro_parent_orchestration_prompt(
    definition: &MacroDefinition,
    additional_context: Option<&str>,
    child_agent_id: &str,
) -> String {
    let mut lines = vec![
        format!("Agent macro invocation: #{}", definition.summary.name),
        format!("Description: {}", definition.summary.description),
        format!("Persistent subagent recipient: agent:{child_agent_id}"),
        "".to_string(),
        "Macro execution rules:".to_string(),
        "- Use the same persistent subagent recipient for every step.".to_string(),
        "- Step 1 has already been sent to the persistent subagent by the runtime; wait for that result before judging whether to continue.".to_string(),
        format!("- The runtime submits every later macro step to `agent:{child_agent_id}` after a valid structured judge decision."),
        "- Judge each completed step with one outcome: continue, continue_with_adapted_prompt, stop_failure, or finish_success.".to_string(),
        "- Each step is interpreted as a normal agent-shell prompt in the subagent, so slash commands such as /loop remain valid.".to_string(),
        "- You may adapt a scripted step to the user's stated intent, but preserve the macro purpose and step order.".to_string(),
        "- After each subagent result, judge success against the step intent, user context, and remaining sequence.".to_string(),
        "- On success, choose a continuation outcome; on failure, choose stop_failure with a concise explanation.".to_string(),
        "- Finish successfully only after all required steps complete in order.".to_string(),
        "".to_string(),
    ];
    if let Some(context) = additional_context.filter(|context| !context.trim().is_empty()) {
        lines.push("User additional context:".to_string());
        lines.push(context.trim().to_string());
        lines.push(String::new());
    }
    lines.push("Scripted steps:".to_string());
    lines.extend(
        definition
            .steps
            .iter()
            .map(|step| format!("{}. {}", step.index, step.prompt)),
    );
    lines.join("\n")
}

/// Builds the runtime-owned first macro-step prompt sent to the child agent.
fn runtime_macro_initial_step_prompt(
    step_prompt: &str,
    additional_context: Option<&str>,
) -> String {
    let Some(context) = additional_context.filter(|context| !context.trim().is_empty()) else {
        return step_prompt.to_string();
    };
    format!(
        "{step_prompt}\n\nUser additional context for this macro invocation:\n{}",
        context.trim()
    )
}

/// Builds a synthetic request record for the runtime-owned macro first step.
fn runtime_owned_macro_step_model_request(parent_turn: &AgentTurnRecord) -> ModelRequest {
    ModelRequest {
        provider: "runtime".to_string(),
        model: "macro-orchestration".to_string(),
        reasoning_effort: None,
        thinking_enabled: None,
        latency_preference: None,
        prompt_cache_retention: None,
        max_output_tokens: None,
        temperature: None,
        prompt_cache_session_id: None,
        prompt_cache_lineage_id: None,
        turn_id: parent_turn.turn_id.clone(),
        agent_id: parent_turn.agent_id.clone(),
        available_mcp_tools: Vec::new(),
        memory_actions_enabled: false,
        issue_actions_enabled: false,
        interaction_kind: ModelInteractionKind::ActionExecution,
        allowed_actions: AllowedActionSet::for_capability(crate::agent::AgentCapability::Subagent),
        stop: None,
        messages: vec![ModelMessage {
            role: ModelMessageRole::User,
            source: ContextSourceKind::TranscriptUser,
            content: "runtime-owned macro first step".to_string(),
        }],
    }
}

/// Returns the stable wire value for one macro judge outcome.
fn macro_judge_outcome_wire_value(outcome: MacroJudgeOutcome) -> &'static str {
    match outcome {
        MacroJudgeOutcome::Continue => "continue",
        MacroJudgeOutcome::ContinueWithAdaptedPrompt => "continue_with_adapted_prompt",
        MacroJudgeOutcome::RetryCurrentStep => "retry_current_step",
        MacroJudgeOutcome::StopFailure => "stop_failure",
        MacroJudgeOutcome::FinishSuccess => "finish_success",
    }
}

/// Builds the system policy for a structured macro judge request.
fn runtime_macro_judge_policy() -> String {
    [
        "You are judging one completed Mezzanine agent macro step.",
        "Return only JSON matching the requested macro-judge schema.",
        "Choose continue only when the completed step satisfied its intent and another scripted step remains.",
        "Choose continue_with_adapted_prompt only when another step remains and the next prompt needs bounded adaptation.",
        "Choose retry_current_step when the completed step looks incomplete but recoverable and the same scripted step should be retried, optionally with a bounded adapted prompt.",
        "Choose stop_failure when the completed step did not satisfy its intent or continuation would violate the macro purpose.",
        "Choose finish_success only after the final required step completed successfully.",
    ]
    .join("\n")
}

/// Builds the user task for a structured macro judge request.
fn runtime_macro_judge_task(
    run: &MacroRunState,
    step: &MacroRunStep,
    result: &crate::runtime::service_state::MacroStepTaskResult,
    next_step: Option<&MacroRunStep>,
) -> String {
    let mut value = serde_json::json!({
        "macro_name": run.macro_name,
        "macro_description": run.macro_description,
        "invocation_prompt": run.invocation_prompt,
        "invocation_context": run.invocation_context,
        "completed_step": {
            "index": step.index,
            "scripted_prompt": step.scripted_prompt,
            "submitted_prompt": step.submitted_prompt,
            "child_turn_id": step.child_turn_id,
            "task_result": {
                "success": result.success,
                "summary": result.summary,
                "output": result.output,
            }
        },
        "prior_steps": run.steps.iter().filter(|candidate| candidate.index < step.index).map(|candidate| {
            serde_json::json!({
                "index": candidate.index,
                "scripted_prompt": candidate.scripted_prompt,
                "task_result": candidate.task_result.as_ref().map(|task_result| serde_json::json!({
                    "success": task_result.success,
                    "summary": task_result.summary,
                })),
                "judgment": candidate.judgment.as_ref().map(|judgment| serde_json::json!({
                    "outcome": macro_judge_outcome_wire_value(judgment.outcome),
                    "step_success": judgment.step_success,
                    "rationale": judgment.rationale,
                })),
            })
        }).collect::<Vec<_>>(),
        "next_step": next_step.map(|next_step| serde_json::json!({
            "index": next_step.index,
            "scripted_prompt": next_step.scripted_prompt,
        })),
    });
    value["instructions"] = serde_json::json!(
        "Judge whether the completed step satisfies the macro intent and select the next runtime action, including retry_current_step for incomplete but recoverable output."
    );
    value.to_string()
}

/// Parses and validates one structured macro judge response.
fn macro_judge_decision_from_text(
    text: &str,
    step_count: usize,
    step_index: usize,
) -> Result<MacroJudgeDecision> {
    let value: serde_json::Value = serde_json::from_str(text.trim()).map_err(|error| {
        MezError::invalid_args(format!(
            "macro judge response invalid after step {}: expected JSON object: {error}",
            step_index.saturating_add(1)
        ))
    })?;
    let object = value.as_object().ok_or_else(|| {
        MezError::invalid_args(format!(
            "macro judge response invalid after step {}: expected JSON object",
            step_index.saturating_add(1)
        ))
    })?;
    let outcome = object
        .get("outcome")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_args("macro judge response missing outcome"))?
        .parse::<MacroJudgeOutcome>()
        .map_err(MezError::invalid_args)?;
    let step_success = object
        .get("step_success")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| MezError::invalid_args("macro judge response missing step_success"))?;
    let rationale = object
        .get("rationale")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| MezError::invalid_args("macro judge response missing rationale"))?
        .to_string();
    let adapted_prompt = object
        .get("adapted_prompt")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let user_message = object
        .get("user_message")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let final_step = step_index.saturating_add(1) >= step_count;
    match outcome {
        MacroJudgeOutcome::Continue if final_step => {
            return Err(MezError::invalid_args(
                "macro judge cannot continue after the final step",
            ));
        }
        MacroJudgeOutcome::ContinueWithAdaptedPrompt if final_step => {
            return Err(MezError::invalid_args(
                "macro judge cannot adapt a next prompt after the final step",
            ));
        }
        MacroJudgeOutcome::ContinueWithAdaptedPrompt if adapted_prompt.is_none() => {
            return Err(MezError::invalid_args(
                "macro judge adapted continuation requires adapted_prompt",
            ));
        }
        MacroJudgeOutcome::StopFailure if user_message.is_none() => {
            return Err(MezError::invalid_args(
                "macro judge stop_failure requires user_message",
            ));
        }
        MacroJudgeOutcome::FinishSuccess if !final_step => {
            return Err(MezError::invalid_args(
                "macro judge cannot finish before the final step",
            ));
        }
        _ => {}
    }
    Ok(MacroJudgeDecision {
        outcome,
        step_success,
        rationale,
        adapted_prompt,
        user_message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that a macro judge can only continue with an adapted prompt
    /// when another scripted step remains and the adapted prompt is non-empty.
    /// This protects the harness-owned continuation path from dispatching an
    /// empty or out-of-order next macro step after structured provider output.
    #[test]
    fn macro_judge_decision_validates_adapted_continuation() {
        let decision = macro_judge_decision_from_text(
            r#"{"outcome":"continue_with_adapted_prompt","step_success":true,"rationale":"step passed","adapted_prompt":"Run the next step with the observed id.","user_message":null}"#,
            2,
            0,
        )
        .unwrap();
        assert_eq!(
            decision.outcome,
            MacroJudgeOutcome::ContinueWithAdaptedPrompt
        );
        assert_eq!(
            decision.adapted_prompt.as_deref(),
            Some("Run the next step with the observed id.")
        );

        let missing_prompt = macro_judge_decision_from_text(
            r#"{"outcome":"continue_with_adapted_prompt","step_success":true,"rationale":"step passed","adapted_prompt":null,"user_message":null}"#,
            2,
            0,
        )
        .unwrap_err();
        assert!(
            missing_prompt
                .message()
                .contains("adapted continuation requires adapted_prompt"),
            "{missing_prompt}"
        );

        let final_step = macro_judge_decision_from_text(
            r#"{"outcome":"continue_with_adapted_prompt","step_success":true,"rationale":"step passed","adapted_prompt":"extra work","user_message":null}"#,
            1,
            0,
        )
        .unwrap_err();
        assert!(
            final_step
                .message()
                .contains("cannot adapt a next prompt after the final step"),
            "{final_step}"
        );
    }

    /// Verifies that recoverable macro judge decisions can retry the current
    /// step without advancing the macro, including on the final scripted step.
    /// This keeps incomplete-but-fixable subagent output on a runtime-owned
    /// retry path instead of forcing failure or out-of-order continuation.
    #[test]
    fn macro_judge_decision_allows_retry_current_step() {
        let retry = macro_judge_decision_from_text(
            r#"{"outcome":"retry_current_step","step_success":false,"rationale":"the subagent asked for clarification but can retry with a narrower prompt","adapted_prompt":"Inspect the release notes directly and list blockers.","user_message":null}"#,
            2,
            0,
        )
        .unwrap();
        assert_eq!(retry.outcome, MacroJudgeOutcome::RetryCurrentStep);
        assert_eq!(
            retry.adapted_prompt.as_deref(),
            Some("Inspect the release notes directly and list blockers.")
        );

        let final_step = macro_judge_decision_from_text(
            r#"{"outcome":"retry_current_step","step_success":false,"rationale":"the final step was incomplete but recoverable","adapted_prompt":null,"user_message":null}"#,
            1,
            0,
        )
        .unwrap();
        assert_eq!(final_step.outcome, MacroJudgeOutcome::RetryCurrentStep);
    }

    /// Verifies that terminal macro judge decisions are position-sensitive:
    /// `finish_success` is accepted only after the last scripted step and
    /// `stop_failure` must include a user-visible explanation. These checks
    /// keep invalid structured judge output from becoming a stranded parent
    /// turn or a generic missing-MAAP failure.
    #[test]
    fn macro_judge_decision_validates_terminal_outcomes() {
        let finish = macro_judge_decision_from_text(
            r#"{"outcome":"finish_success","step_success":true,"rationale":"all steps completed","adapted_prompt":null,"user_message":null}"#,
            2,
            1,
        )
        .unwrap();
        assert_eq!(finish.outcome, MacroJudgeOutcome::FinishSuccess);

        let early_finish = macro_judge_decision_from_text(
            r#"{"outcome":"finish_success","step_success":true,"rationale":"done early","adapted_prompt":null,"user_message":null}"#,
            2,
            0,
        )
        .unwrap_err();
        assert!(
            early_finish
                .message()
                .contains("cannot finish before the final step"),
            "{early_finish}"
        );

        let missing_message = macro_judge_decision_from_text(
            r#"{"outcome":"stop_failure","step_success":false,"rationale":"step failed","adapted_prompt":null,"user_message":null}"#,
            2,
            0,
        )
        .unwrap_err();
        assert!(
            missing_message
                .message()
                .contains("stop_failure requires user_message"),
            "{missing_message}"
        );
    }

    /// Verifies that `macro_message_recipient_agent_id` trims whitespace
    /// from the extracted agent id after the `agent:` prefix, so that
    /// recipients like `"agent: agent-%3"` or `"agent:agent-%3 "` are
    /// correctly routed through the macro bridge instead of silently
    /// falling back to plain MMP delivery.
    #[test]
    fn macro_recipient_trims_whitespace_after_agent_prefix() {
        // Leading whitespace after `agent:`
        assert_eq!(
            macro_message_recipient_agent_id("agent: agent-%5"),
            Some("agent-%5".to_string())
        );
        // Trailing whitespace
        assert_eq!(
            macro_message_recipient_agent_id("agent:agent-%7 "),
            Some("agent-%7".to_string())
        );
        // Both leading and trailing whitespace
        assert_eq!(
            macro_message_recipient_agent_id("agent:  agent-%9  "),
            Some("agent-%9".to_string())
        );
        // Only whitespace after agent: should still be filtered (empty after trim)
        assert_eq!(macro_message_recipient_agent_id("agent:   "), None);
        // Normal untrimmed case still works
        assert_eq!(
            macro_message_recipient_agent_id("agent:agent-%3"),
            Some("agent-%3".to_string())
        );
        // Bare agent-% pattern (no agent: prefix) still works
        assert_eq!(
            macro_message_recipient_agent_id("agent-%12"),
            Some("agent-%12".to_string())
        );
    }

    /// Verifies that `deregister_macro_managed_subagent` removes an agent
    /// from the macro-managed set, preventing stale entries from accumulating
    /// and preventing recycled pane ids from hijacking macro bridge routing.
    #[test]
    fn deregister_macro_managed_removes_agent_from_set() {
        let fixture = crate::test_support::runtime::RuntimeServiceFixture::new();
        let mut service = fixture.build();
        let agent_id = "agent-%99";

        // Initially empty
        assert!(!service.macro_managed_subagent_agents.contains_key(agent_id));

        // Register
        service.register_macro_managed_subagent(agent_id, "turn-99", "agent-%1", "test-macro");
        assert!(service.macro_managed_subagent_agents.contains_key(agent_id));

        // Deregister
        service.deregister_macro_managed_subagent(agent_id);
        assert!(!service.macro_managed_subagent_agents.contains_key(agent_id));

        // Deregistering an already-absent id is a no-op
        service.deregister_macro_managed_subagent(agent_id);
        assert!(!service.macro_managed_subagent_agents.contains_key(agent_id));
    }
}
