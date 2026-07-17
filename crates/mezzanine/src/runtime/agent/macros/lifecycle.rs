//! Macro catalog, run registration, and step dispatch lifecycle.

use super::super::{
    ActionResult, AgentAction, AgentActionPayload, AgentTurnExecution, AgentTurnRecord,
    AgentTurnState, MaapBatch, MacroManagedSubagent, MezError, ModelResponse, PathBuf, Result,
    RuntimeSessionService, runtime_path_under_project_root, runtime_spawn_json_agent_and_turn,
    runtime_subagent_placement_mode, runtime_subagent_spawn_request,
};
use super::{
    MacroCatalog, MacroDefinition, MacroRunPhase, MacroRunRegistration,
    RuntimeAgentPromptTurnStart, TrustDecision, discover_macro_catalog, load_macro_definition,
    macro_initial_step_prompt, macro_parent_orchestration_prompt, macro_run_state,
    macro_step_model_request, parse_macro_prompt_invocation,
};

impl RuntimeSessionService {
    /// Builds the effective macro catalog for one pane.
    ///
    /// User macros are read from the configured user root. Project macros are
    /// included only when the pane is inside a trusted project root.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose current working directory scopes project macros.
    pub(crate) fn effective_macro_catalog_for_pane(&self, pane_id: &str) -> MacroCatalog {
        let project_root = self.trusted_macro_project_root_for_pane(pane_id);
        discover_macro_catalog(self.integration.config_root(), project_root.as_deref())
    }

    /// Returns the trusted project root whose macros may apply to one pane.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose working directory determines project scope.
    pub(super) fn trusted_macro_project_root_for_pane(&self, pane_id: &str) -> Option<PathBuf> {
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
        self.agent.macro_managed_subagent_agents.insert(
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
        self.agent
            .macro_managed_subagent_agents
            .remove(child_agent_id);
    }

    /// Records a newly started macro run before any step is submitted.
    ///
    /// The loaded step list is copied into runtime state so later file edits to
    /// the macro definition cannot change an in-flight run. The parent turn id
    /// is the stable run id for the current macro format.
    pub(super) fn register_macro_run_state(
        &mut self,
        pane_id: &str,
        prompt: &str,
        definition: &MacroDefinition,
        additional_context: Option<&str>,
        started: &RuntimeAgentPromptTurnStart,
        child_agent_id: &str,
    ) {
        self.agent.macro_runs_by_parent_turn.insert(
            started.turn_id.clone(),
            macro_run_state(
                definition,
                MacroRunRegistration {
                    parent_turn_id: &started.turn_id,
                    parent_agent_id: &started.agent_id,
                    parent_pane_id: pane_id,
                    child_agent_id,
                    invocation_prompt: prompt,
                    invocation_context: additional_context,
                },
            ),
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
    pub(crate) fn start_agent_macro_prompt_turn(
        &mut self,
        pane_id: &str,
        prompt: &str,
    ) -> Result<RuntimeAgentPromptTurnStart> {
        let invocation = parse_macro_prompt_invocation(prompt)
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
        let orchestration_prompt = macro_parent_orchestration_prompt(
            &definition,
            invocation.additional_context.as_deref(),
            &child_agent_id,
        );
        let started = self.start_agent_prompt_turn_with_cooperation(
            pane_id,
            &orchestration_prompt,
            Some("macro-orchestration".to_string()),
            Some(mez_agent::AgentCapability::Subagent),
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
    pub(super) fn queue_runtime_owned_first_macro_step(
        &mut self,
        pane_id: &str,
        started: &RuntimeAgentPromptTurnStart,
        definition: &MacroDefinition,
        additional_context: Option<&str>,
        child_agent_id: &str,
    ) -> Result<()> {
        let parent_turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == started.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("macro parent turn disappeared"))?;
        let first_step = definition
            .steps
            .first()
            .ok_or_else(|| MezError::invalid_state("agent macro has no scripted steps"))?;
        let payload = macro_initial_step_prompt(first_step.prompt.as_str(), additional_context);
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
        self.agent_turn_executions_mut().insert(
            parent_turn.turn_id.clone(),
            AgentTurnExecution {
                request: macro_step_model_request(&parent_turn),
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
        self.agent
            .pending_agent_provider_tasks
            .remove(&parent_turn.turn_id);
        self.agent
            .claimed_agent_provider_tasks
            .remove(&parent_turn.turn_id);
        self.agent_turn_ledger_mut()
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
    pub(super) fn queue_runtime_macro_step_prompt(
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
            self.agent
                .joined_subagent_dependencies
                .values()
                .any(|dependency| {
                    dependency.parent_turn_id == parent_turn.turn_id
                        && dependency.parent_action_id == action.id
                })
                || self.agent.agent_loops_by_id.values().any(|state| {
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
                .agent
                .joined_subagent_dependencies
                .values()
                .find(|dependency| {
                    dependency.parent_turn_id == parent_turn.turn_id
                        && dependency.parent_action_id == action.id
                })
                .map(|dependency| dependency.child_turn_id.clone())
                .or_else(|| {
                    self.agent.agent_loops_by_id.values().find_map(|state| {
                        state.completion.as_ref().and_then(|completion| {
                            (completion.parent_turn_id == parent_turn.turn_id
                                && completion.parent_action_id == action.id)
                                .then(|| completion.child_turn_id.clone())
                        })
                    })
                });
            if child_turn_id.is_some() && !already_recorded_step_action {
                let (macro_name, total_steps) = self
                    .agent
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
                .agent
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
                    self.agent
                        .macro_run_by_child_turn
                        .insert(child_turn_id, parent_turn.turn_id.clone());
                } else {
                    run.phase = MacroRunPhase::DispatchingStep { step_index };
                }
            }
        }
        Ok(result)
    }
}
