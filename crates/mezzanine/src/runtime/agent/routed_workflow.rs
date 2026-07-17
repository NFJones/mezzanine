//! Runtime-owned routed worker lifecycle and presentation transitions.
//!
//! Routing classification returns to the serialized runtime actor before any
//! user work runs. The actor keeps the parent turn on its ordinary profile and
//! owns creation, tracking, and eventual presentation of the managed child.

use super::{
    AgentContext, AgentId, AgentTurnExecution, AgentTurnRecord, AgentTurnState, ContextBlock,
    ContextSourceKind, MezError, Result, RuntimeRoutedWorkerSelection, RuntimeSessionService,
    ScheduledWork, current_unix_seconds, runtime_spawn_json_agent_and_turn,
    runtime_subagent_placement_mode, runtime_subagent_spawn_request,
    subagent_task_output_for_execution,
};
use mez_agent::ScheduledWorkKind;
use mez_agent::routed_workflow::{
    ROUTED_HANDOFF_VERSION, RoutedWorkerHandoff, RoutedWorkflowPhase, RoutedWorkflowState,
};

const ROUTED_HANDOFF_MAX_BYTES: usize = 16 * 1024;

const ROUTED_HANDOFF_PROMPT: &str = r#"Return all context needed for the main model to present your completed result safely. Emit one final MAAP say action whose text is exactly one JSON object matching this schema: {"version":1,"result_summary":"...","decisions":["..."],"evidence":["..."],"changes":["..."],"validation":["..."],"assumptions":["..."],"unresolved_risks":["..."],"follow_up_context":["..."]}. Do not perform more work or call tools."#;

const ROUTED_HANDOFF_REPAIR_PROMPT: &str = r#"Your previous handoff was invalid. Emit one final MAAP say action whose text is exactly one valid JSON object with version 1 and string-array fields decisions, evidence, changes, validation, assumptions, unresolved_risks, and follow_up_context, plus string result_summary. Do not use Markdown fences or call tools."#;

/// Inputs for one runtime-owned child turn in a routed workflow.
struct RoutedChildTurnRequest<'a> {
    parent_turn: &'a AgentTurnRecord,
    child_agent_id: &'a str,
    child_pane_id: &'a str,
    prompt: &'a str,
    model_profile: mez_agent::ModelProfile,
    seed_context: Option<AgentContext>,
    initial_capability: Option<mez_agent::AgentCapability>,
    reason: &'a str,
}

impl RuntimeSessionService {
    /// Accepts a completed routing decision at the actor boundary.
    ///
    /// The full managed-child transition is deliberately actor-owned so no
    /// provider worker mutates pane, scheduler, transcript, or subagent state.
    pub(crate) fn apply_routed_worker_selected_transition(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        selection: RuntimeRoutedWorkerSelection,
    ) -> Result<crate::runtime::RuntimeTransition> {
        let turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("routed parent turn is unavailable"))?;
        if turn.agent_id != agent_id.as_str() {
            return Err(MezError::invalid_args(
                "routed worker selection agent id does not match parent turn",
            ));
        }
        if turn.state != mez_agent::AgentTurnState::Running {
            return Ok(crate::runtime::RuntimeTransition::default());
        }

        let parent_session = self
            .agent_shell_store()
            .get(&turn.pane_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("routed parent session is unavailable"))?;
        let original_user_prompt = self
            .agent_turn_contexts()
            .get(turn_id)
            .and_then(|context| {
                context.blocks.iter().find(|block| {
                    block.source == ContextSourceKind::UserInstruction
                        && block.label == "user prompt"
                })
            })
            .map(|block| block.content.clone())
            .ok_or_else(|| MezError::invalid_state("routed parent prompt is unavailable"))?;
        let params = serde_json::json!({
            "parent_agent": { "agent_id": turn.agent_id },
            "placement": "new-window",
            "role": "worker",
            "cooperation_mode": "owned-write",
            "prompt": "",
            "skip_initial_turn": true,
        })
        .to_string();
        let spawn = runtime_subagent_spawn_request(&params, false)?;
        let placement = runtime_subagent_placement_mode(&params)?;
        let spawn_json = self.spawn_runtime_subagent_session_owned(spawn, placement)?;
        let (child_agent_id, _child_display_name, child_turn_id) =
            runtime_spawn_json_agent_and_turn(&spawn_json)?;
        if child_turn_id.is_some() {
            return Err(MezError::invalid_state(
                "routed worker idle spawn unexpectedly created a turn",
            ));
        }
        let child_pane_id = child_agent_id
            .strip_prefix("agent-")
            .ok_or_else(|| MezError::invalid_state("routed worker agent id is invalid"))?
            .to_string();
        let child_conversation_id = format!("routed-{turn_id}-worker");
        self.agent_shell_store_mut()
            .bind_ephemeral_conversation_with_lineage_and_transcript_source(
                &child_pane_id,
                child_conversation_id.clone(),
                0,
                Some(parent_session.prompt_cache_lineage_id.clone()),
                Some(parent_session.session_id.clone()),
                parent_session.transcript_entries,
            )?;
        self.set_agent_routing_override(&child_pane_id, Some(false));

        let child_turn = self.queue_routed_child_turn(RoutedChildTurnRequest {
            parent_turn: &turn,
            child_agent_id: &child_agent_id,
            child_pane_id: &child_pane_id,
            prompt: &original_user_prompt,
            model_profile: selection.worker_profile.clone(),
            seed_context: None,
            initial_capability: None,
            reason: "routed_worker_execute",
        })?;
        self.agent
            .routed_workflow_by_child_turn
            .insert(child_turn.turn_id.clone(), turn.turn_id.clone());
        let child_context = self
            .agent_turn_contexts()
            .get(&child_turn.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("routed child context was not recorded"))?;
        self.agent
            .routed_child_contexts_by_parent_turn
            .insert(turn.turn_id.clone(), child_context);
        self.agent
            .routed_child_profiles_by_parent_turn
            .insert(turn.turn_id.clone(), selection.worker_profile.clone());
        self.agent.routed_workflows_by_parent_turn.insert(
            turn.turn_id.clone(),
            RoutedWorkflowState {
                run_id: turn.turn_id.clone(),
                parent_agent_id: turn.agent_id.clone(),
                parent_pane_id: turn.pane_id.clone(),
                parent_conversation_id: parent_session.session_id,
                parent_transcript_entries: parent_session.transcript_entries,
                original_user_prompt,
                main_model_profile: turn.model_profile.clone(),
                worker_model_profile: Some(selection.worker_profile.model.clone()),
                child_agent_id: Some(child_agent_id.clone()),
                child_conversation_id: Some(child_conversation_id),
                child_turn_id: Some(child_turn.turn_id.clone()),
                worker_final_result: None,
                handoff: None,
                handoff_repair_attempts: 0,
                error_explanation_attempted: false,
                phase: RoutedWorkflowPhase::WaitingForWorkerResult,
                diagnostic: selection.fallback.clone(),
            },
        );
        for (key, usage) in &selection.routing_token_usage_by_model {
            self.integration
                .runtime_metrics_mut()
                .record_provider_token_usage(*usage, *usage, key);
        }
        self.record_agent_provider_token_usage_by_model(
            &turn.pane_id,
            &selection.routing_token_usage_by_model,
        );
        if let Some(summary) = selection.decision_summary.as_deref() {
            self.append_agent_status_text_to_terminal_buffer(
                &turn.pane_id,
                &format!("agent: routing selected {summary}"),
            )?;
        } else if let Some(fallback) = selection.fallback.as_deref() {
            self.append_agent_status_text_to_terminal_buffer(
                &turn.pane_id,
                &format!(
                    "agent: routing fallback worker {}: {fallback}",
                    selection.worker_profile.model
                ),
            )?;
        }
        self.agent.agent_scheduler.block_running(turn_id)?;
        self.agent_turn_ledger_mut()
            .finish_turn(turn_id, AgentTurnState::Blocked)?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!(
                "routed_worker selected provider={} model={} child_agent={} child_turn={}",
                selection.worker_profile.provider,
                selection.worker_profile.model,
                child_agent_id,
                child_turn.turn_id
            ),
        )?;
        self.start_ready_agent_turns()?;
        Ok(self.runtime_transition_with_render(
            true,
            Some(crate::runtime::RenderInvalidationReason::FullRedraw),
        ))
    }

    /// Advances a routed workflow after one managed child turn settles.
    pub(crate) fn handle_routed_child_execution_result(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<bool> {
        let Some(parent_turn_id) = self
            .agent
            .routed_workflow_by_child_turn
            .get(&turn.turn_id)
            .cloned()
        else {
            return Ok(false);
        };
        let state = self
            .agent
            .routed_workflows_by_parent_turn
            .get(&parent_turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("routed workflow state is unavailable"))?;
        if state.child_turn_id.as_deref() != Some(turn.turn_id.as_str())
            || !matches!(
                state.phase,
                RoutedWorkflowPhase::WaitingForWorkerResult
                    | RoutedWorkflowPhase::WaitingForHandoff
            )
        {
            return Ok(true);
        }
        let parent_turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|candidate| candidate.turn_id == parent_turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("routed parent turn is unavailable"))?;
        let child_agent_id = state
            .child_agent_id
            .clone()
            .ok_or_else(|| MezError::invalid_state("routed child agent is unavailable"))?;
        let child_pane_id = child_agent_id
            .strip_prefix("agent-")
            .ok_or_else(|| MezError::invalid_state("routed child agent id is invalid"))?
            .to_string();
        let output = subagent_task_output_for_execution(execution);

        match state.phase {
            RoutedWorkflowPhase::WaitingForWorkerResult => {
                if execution.terminal_state != AgentTurnState::Completed {
                    if let Some(workflow) = self
                        .agent
                        .routed_workflows_by_parent_turn
                        .get_mut(&parent_turn_id)
                    {
                        workflow.worker_final_result = Some(output.clone());
                        workflow.diagnostic =
                            Some("routed worker failed before handoff".to_string());
                    }
                    self.ready_routed_parent_for_error_explanation(
                        &parent_turn,
                        "worker",
                        &output,
                        "routed worker failed before handoff",
                    )?;
                    self.agent.subagent_task_routes.remove(&turn.turn_id);
                    return Ok(true);
                }
                let Some(mut handoff_context) = self
                    .agent
                    .routed_child_contexts_by_parent_turn
                    .get(&parent_turn_id)
                    .cloned()
                else {
                    self.ready_routed_parent_for_error_explanation(
                        &parent_turn,
                        "summary request",
                        &output,
                        "routed worker context snapshot is unavailable",
                    )?;
                    self.agent.subagent_task_routes.remove(&turn.turn_id);
                    return Ok(true);
                };
                let Some(child_profile) = self
                    .agent
                    .routed_child_profiles_by_parent_turn
                    .get(&parent_turn_id)
                    .cloned()
                else {
                    self.ready_routed_parent_for_error_explanation(
                        &parent_turn,
                        "summary request",
                        &output,
                        "routed worker profile snapshot is unavailable",
                    )?;
                    self.agent.subagent_task_routes.remove(&turn.turn_id);
                    return Ok(true);
                };
                handoff_context.blocks.push(ContextBlock {
                    source: ContextSourceKind::RoutedHandoff,
                    placement: mez_agent::ContextPlacement::ConversationAppend,
                    label: "routed worker exact final result".to_string(),
                    content: output.clone(),
                });
                self.agent
                    .routed_child_contexts_by_parent_turn
                    .insert(parent_turn_id.clone(), handoff_context.clone());
                let handoff_turn = match self.queue_routed_child_turn(RoutedChildTurnRequest {
                    parent_turn: &parent_turn,
                    child_agent_id: &child_agent_id,
                    child_pane_id: &child_pane_id,
                    prompt: ROUTED_HANDOFF_PROMPT,
                    model_profile: child_profile,
                    seed_context: Some(handoff_context),
                    initial_capability: Some(mez_agent::AgentCapability::RespondOnly),
                    reason: "routed_worker_handoff",
                }) {
                    Ok(turn) => turn,
                    Err(error) => {
                        self.ready_routed_parent_for_error_explanation(
                            &parent_turn,
                            "summary request",
                            &output,
                            &error.to_string(),
                        )?;
                        self.agent.subagent_task_routes.remove(&turn.turn_id);
                        return Ok(true);
                    }
                };
                self.agent
                    .routed_workflow_by_child_turn
                    .insert(handoff_turn.turn_id.clone(), parent_turn_id.clone());
                if let Some(workflow) = self
                    .agent
                    .routed_workflows_by_parent_turn
                    .get_mut(&parent_turn_id)
                {
                    workflow.worker_final_result = Some(output);
                    workflow.child_turn_id = Some(handoff_turn.turn_id);
                    workflow.phase = RoutedWorkflowPhase::WaitingForHandoff;
                }
            }
            RoutedWorkflowPhase::WaitingForHandoff
                if execution.terminal_state != AgentTurnState::Completed =>
            {
                let final_result = state.worker_final_result.as_deref().unwrap_or_default();
                self.ready_routed_parent_for_error_explanation(
                    &parent_turn,
                    "summary request",
                    final_result,
                    "routed handoff provider step failed",
                )?;
            }
            RoutedWorkflowPhase::WaitingForHandoff => match parse_routed_worker_handoff(&output) {
                Ok(handoff) => {
                    let final_result = state.worker_final_result.as_deref().unwrap_or_default();
                    if let Some(workflow) = self
                        .agent
                        .routed_workflows_by_parent_turn
                        .get_mut(&parent_turn_id)
                    {
                        workflow.handoff = Some(handoff.clone());
                    }
                    self.ready_routed_parent_for_presentation(
                        &parent_turn,
                        final_result,
                        Some(&handoff),
                        None,
                    )?;
                }
                Err(error) if state.can_repair_handoff() => {
                    let Some(child_profile) = self
                        .agent
                        .routed_child_profiles_by_parent_turn
                        .get(&parent_turn_id)
                        .cloned()
                    else {
                        self.ready_routed_parent_for_error_explanation(
                            &parent_turn,
                            "summary repair",
                            state.worker_final_result.as_deref().unwrap_or_default(),
                            "routed worker profile snapshot is unavailable",
                        )?;
                        self.agent.subagent_task_routes.remove(&turn.turn_id);
                        return Ok(true);
                    };
                    let repair_turn = match self.queue_routed_child_turn(RoutedChildTurnRequest {
                        parent_turn: &parent_turn,
                        child_agent_id: &child_agent_id,
                        child_pane_id: &child_pane_id,
                        prompt: ROUTED_HANDOFF_REPAIR_PROMPT,
                        model_profile: child_profile,
                        seed_context: self
                            .agent
                            .routed_child_contexts_by_parent_turn
                            .get(&parent_turn_id)
                            .cloned(),
                        initial_capability: Some(mez_agent::AgentCapability::RespondOnly),
                        reason: "routed_worker_handoff_repair",
                    }) {
                        Ok(turn) => turn,
                        Err(queue_error) => {
                            self.ready_routed_parent_for_error_explanation(
                                &parent_turn,
                                "summary repair",
                                state.worker_final_result.as_deref().unwrap_or_default(),
                                &queue_error.to_string(),
                            )?;
                            self.agent.subagent_task_routes.remove(&turn.turn_id);
                            return Ok(true);
                        }
                    };
                    self.agent
                        .routed_workflow_by_child_turn
                        .insert(repair_turn.turn_id.clone(), parent_turn_id.clone());
                    if let Some(workflow) = self
                        .agent
                        .routed_workflows_by_parent_turn
                        .get_mut(&parent_turn_id)
                    {
                        workflow.handoff_repair_attempts =
                            workflow.handoff_repair_attempts.saturating_add(1);
                        workflow.child_turn_id = Some(repair_turn.turn_id);
                        workflow.diagnostic = Some(error.to_string());
                    }
                }
                Err(error) => {
                    let final_result = state.worker_final_result.as_deref().unwrap_or_default();
                    let diagnostic = error.to_string();
                    self.ready_routed_parent_for_error_explanation(
                        &parent_turn,
                        "summary parse",
                        final_result,
                        &diagnostic,
                    )?;
                }
            },
            _ => {
                return Err(MezError::invalid_state(
                    "routed child completed in an unexpected workflow phase",
                ));
            }
        }
        self.agent.subagent_task_routes.remove(&turn.turn_id);
        Ok(true)
    }

    /// Adds a routed diagnostic to the parent context and queues one explanation.
    fn ready_routed_parent_for_error_explanation(
        &mut self,
        parent_turn: &AgentTurnRecord,
        stage: &str,
        child_output: &str,
        diagnostic: &str,
    ) -> Result<()> {
        let context = self
            .agent_turn_contexts_mut()
            .get_mut(&parent_turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("routed parent context is unavailable"))?;
        if !child_output.is_empty() {
            context.blocks.push(ContextBlock {
                source: ContextSourceKind::RoutedHandoff,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "routed worker exact final result".to_string(),
                content: child_output.to_string(),
            });
        }
        context.blocks.push(ContextBlock {
            source: ContextSourceKind::RoutedHandoff,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "routed workflow failure".to_string(),
            content: format!("stage={stage}\ndiagnostic={diagnostic}"),
        });
        context.blocks.push(ContextBlock {
            source: ContextSourceKind::RuntimeHint,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            label: "routed failure explanation".to_string(),
            content: "Explain why the routed operation failed. Use the stored diagnostic and any exact worker output as evidence, do not claim the routed operation succeeded, and respond only without executing actions.".to_string(),
        });
        if let Some(workflow) = self
            .agent
            .routed_workflows_by_parent_turn
            .get_mut(&parent_turn.turn_id)
        {
            workflow.phase = RoutedWorkflowPhase::ReadyForErrorExplanation;
            workflow.error_explanation_attempted = true;
            workflow.diagnostic = Some(format!("{stage}: {diagnostic}"));
        }
        self.agent
            .routed_presentation_turns
            .insert(parent_turn.turn_id.clone());
        let _ = self
            .agent
            .agent_scheduler
            .resume_blocked(&parent_turn.turn_id);
        if parent_turn.state == AgentTurnState::Blocked {
            self.agent_turn_ledger_mut()
                .resume_blocked_turn(&parent_turn.turn_id)?;
        }
        self.agent
            .pending_agent_provider_tasks
            .insert(parent_turn.turn_id.clone());
        self.append_agent_status_text_to_terminal_buffer(
            &parent_turn.pane_id,
            "agent: routed workflow failed; explaining with main model",
        )?;
        self.release_routed_child_for_close(parent_turn)?;
        Ok(())
    }

    /// Adds routed evidence to the parent context and queues main-model presentation.
    fn ready_routed_parent_for_presentation(
        &mut self,
        parent_turn: &AgentTurnRecord,
        final_result: &str,
        handoff: Option<&RoutedWorkerHandoff>,
        diagnostic: Option<&str>,
    ) -> Result<()> {
        let context = self
            .agent_turn_contexts_mut()
            .get_mut(&parent_turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("routed parent context is unavailable"))?;
        context.blocks.push(ContextBlock {
            source: ContextSourceKind::RoutedHandoff,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "routed worker exact final result".to_string(),
            content: final_result.to_string(),
        });
        let handoff_content = match handoff {
            Some(handoff) => serde_json::to_string(handoff).map_err(|error| {
                MezError::invalid_state(format!("failed to encode routed handoff: {error}"))
            })?,
            None => format!(
                "handoff unavailable: {}",
                diagnostic.unwrap_or("worker did not provide valid handoff context")
            ),
        };
        context.blocks.push(ContextBlock {
            source: ContextSourceKind::RoutedHandoff,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "routed worker handoff context".to_string(),
            content: handoff_content,
        });
        context.blocks.push(ContextBlock {
            source: ContextSourceKind::RuntimeHint,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            label: "routed result presentation".to_string(),
            content: "Present the routed worker result to the user. Preserve material caveats, validation status, and unresolved risks. Do not claim unsupported work. Respond only; do not execute actions.".to_string(),
        });
        if let Some(workflow) = self
            .agent
            .routed_workflows_by_parent_turn
            .get_mut(&parent_turn.turn_id)
        {
            workflow.phase = RoutedWorkflowPhase::ReadyForPresentation;
            if handoff.is_none() {
                workflow.diagnostic = diagnostic.map(str::to_string);
            }
        }
        self.agent
            .routed_presentation_turns
            .insert(parent_turn.turn_id.clone());
        let _ = self
            .agent
            .agent_scheduler
            .resume_blocked(&parent_turn.turn_id);
        if parent_turn.state == AgentTurnState::Blocked {
            self.agent_turn_ledger_mut()
                .resume_blocked_turn(&parent_turn.turn_id)?;
        }
        self.agent
            .pending_agent_provider_tasks
            .insert(parent_turn.turn_id.clone());
        self.append_agent_status_text_to_terminal_buffer(
            &parent_turn.pane_id,
            "agent: routed worker context received; presenting with main model",
        )?;
        self.release_routed_child_for_close(parent_turn)?;
        Ok(())
    }

    /// Releases one managed routed child after its final workflow step.
    fn release_routed_child_for_close(&mut self, parent_turn: &AgentTurnRecord) -> Result<()> {
        let child_agent_id = self
            .agent
            .routed_workflows_by_parent_turn
            .get(&parent_turn.turn_id)
            .and_then(|workflow| workflow.child_agent_id.clone())
            .ok_or_else(|| MezError::invalid_state("routed child agent is unavailable"))?;
        let child_pane_id = child_agent_id
            .strip_prefix("agent-")
            .ok_or_else(|| MezError::invalid_state("routed child agent id is invalid"))?;
        self.remove_subagent_authority_state(&child_agent_id);
        self.integration
            .model_profile_overrides_mut()
            .agent_profiles
            .remove(&child_agent_id);
        self.agent
            .pending_terminal_subagent_pane_closes
            .insert(child_pane_id.to_string());
        Ok(())
    }

    /// Returns whether one provider request is the main-model presentation phase.
    pub(crate) fn routed_presentation_turn(&self, turn_id: &str) -> bool {
        self.agent.routed_presentation_turns.contains(turn_id)
    }

    /// Settles routed workflow state after the main-model presentation finishes.
    pub(crate) fn complete_routed_presentation(
        &mut self,
        turn_id: &str,
        terminal_state: AgentTurnState,
    ) -> Result<bool> {
        let Some(terminal_phase) = routed_presentation_terminal_phase(terminal_state) else {
            return Ok(false);
        };
        if !self.agent.routed_presentation_turns.contains(turn_id) {
            return Ok(false);
        }
        if terminal_phase == RoutedWorkflowPhase::Completed {
            self.agent.routed_presentation_turns.remove(turn_id);
            self.agent.routed_workflows_by_parent_turn.remove(turn_id);
            self.clear_routed_workflow_runtime_state(turn_id);
            return Ok(false);
        }
        let diagnostic = match terminal_state {
            AgentTurnState::Failed => "routed parent presentation failed".to_string(),
            AgentTurnState::Interrupted => "routed parent presentation was interrupted".to_string(),
            _ => unreachable!("terminal phase mapping excludes non-error states"),
        };
        let should_explain = self
            .agent
            .routed_workflows_by_parent_turn
            .get(turn_id)
            .is_some_and(|workflow| !workflow.error_explanation_attempted);
        if should_explain {
            let context = self
                .agent_turn_contexts_mut()
                .get_mut(turn_id)
                .ok_or_else(|| MezError::invalid_state("routed parent context is unavailable"))?;
            context.blocks.push(ContextBlock {
                source: ContextSourceKind::RoutedHandoff,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label: "routed workflow failure".to_string(),
                content: format!("stage=parent presentation\ndiagnostic={diagnostic}"),
            });
            context.blocks.push(ContextBlock {
                source: ContextSourceKind::RuntimeHint,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                label: "routed failure explanation".to_string(),
                content: "Explain why routed result presentation failed. Use the stored diagnostic as evidence, do not claim success, and respond only without executing actions.".to_string(),
            });
            if let Some(workflow) = self.agent.routed_workflows_by_parent_turn.get_mut(turn_id) {
                workflow.phase = RoutedWorkflowPhase::ExplainingError;
                workflow.error_explanation_attempted = true;
                workflow.diagnostic = Some(diagnostic);
            }
            self.agent
                .pending_agent_provider_tasks
                .insert(turn_id.to_string());
            return Ok(true);
        }
        self.agent.routed_presentation_turns.remove(turn_id);
        if let Some(workflow) = self.agent.routed_workflows_by_parent_turn.get_mut(turn_id) {
            workflow.phase = terminal_phase;
            workflow.diagnostic = Some(diagnostic);
        }
        self.clear_routed_workflow_runtime_state(turn_id);
        Ok(false)
    }

    /// Clears runtime-only routed snapshots and superseded child mappings.
    fn clear_routed_workflow_runtime_state(&mut self, turn_id: &str) {
        self.agent
            .routed_workflow_by_child_turn
            .retain(|_, parent| parent != turn_id);
        self.agent
            .routed_child_contexts_by_parent_turn
            .remove(turn_id);
        self.agent
            .routed_child_profiles_by_parent_turn
            .remove(turn_id);
    }

    /// Queues one managed routed-child prompt through the ordinary agent path.
    fn queue_routed_child_turn(
        &mut self,
        request: RoutedChildTurnRequest<'_>,
    ) -> Result<AgentTurnRecord> {
        let RoutedChildTurnRequest {
            parent_turn,
            child_agent_id,
            child_pane_id,
            prompt,
            model_profile,
            seed_context,
            initial_capability,
            reason,
        } = request;
        let context = match seed_context {
            Some(mut context) => {
                context.blocks.push(ContextBlock {
                    source: ContextSourceKind::UserInstruction,
                    placement: mez_agent::ContextPlacement::EphemeralTail,
                    label: "routed workflow prompt".to_string(),
                    content: prompt.to_string(),
                });
                AgentContext::new(context.blocks)?
            }
            None => {
                let context = self.agent_context_for_pane_prompt(child_pane_id, prompt, 100)?;
                self.apply_agent_shell_preference_context(child_pane_id, context)?
            }
        };
        let turn_id = self.next_agent_turn_id();
        let turn = AgentTurnRecord {
            turn_id: turn_id.clone(),
            agent_id: child_agent_id.to_string(),
            pane_id: child_pane_id.to_string(),
            trigger: mez_agent::AgentTurnTrigger::LocalMessage,
            started_at_unix_seconds: current_unix_seconds(),
            policy_profile: "runtime".to_string(),
            model_profile: format!("routed:{}", model_profile.model),
            parent_turn_id: Some(parent_turn.turn_id.clone()),
            cooperation_mode: Some("routed-worker".to_string()),
            state: AgentTurnState::Queued,
            initial_capability,
        };
        self.agent_turn_ledger_mut().queue_turn(turn.clone())?;
        self.agent_turn_contexts_mut()
            .insert(turn_id.clone(), context);
        self.agent
            .agent_turn_model_profiles
            .insert(turn_id.clone(), model_profile);
        self.agent
            .subagent_task_routes
            .insert(turn_id.clone(), parent_turn.agent_id.clone());
        self.append_agent_user_prompt_to_terminal_buffer(child_pane_id, prompt)?;
        self.agent.agent_scheduler.enqueue(ScheduledWork {
            turn_id: turn_id.clone(),
            agent_id: child_agent_id.to_string(),
            pane_id: Some(child_pane_id.to_string()),
            kind: ScheduledWorkKind::ShellCapable,
        })?;
        self.append_agent_trace_turn_event(
            child_pane_id,
            &turn_id,
            &format!("created state=queued reason={reason}"),
        )?;
        Ok(turn)
    }
}

fn parse_routed_worker_handoff(output: &str) -> Result<RoutedWorkerHandoff> {
    let trimmed = output.trim();
    let json = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    let handoff: RoutedWorkerHandoff = serde_json::from_str(json).map_err(|error| {
        MezError::invalid_state(format!("invalid routed handoff JSON: {error}"))
    })?;
    if handoff.version != ROUTED_HANDOFF_VERSION {
        return Err(MezError::invalid_state(format!(
            "invalid routed handoff version {}; expected {}",
            handoff.version, ROUTED_HANDOFF_VERSION
        )));
    }
    handoff
        .validate(ROUTED_HANDOFF_MAX_BYTES)
        .map_err(MezError::invalid_state)?;
    Ok(handoff)
}

/// Maps a settled parent presentation turn to its routed terminal phase.
fn routed_presentation_terminal_phase(
    terminal_state: AgentTurnState,
) -> Option<RoutedWorkflowPhase> {
    match terminal_state {
        AgentTurnState::Completed => Some(RoutedWorkflowPhase::Completed),
        AgentTurnState::Failed => Some(RoutedWorkflowPhase::Failed),
        AgentTurnState::Interrupted => Some(RoutedWorkflowPhase::Interrupted),
        AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies routed presentation outcomes preserve each supported terminal
    /// state and reject scheduler states that have not settled.
    #[test]
    fn routed_presentation_terminal_phase_tracks_provider_outcomes() {
        assert_eq!(
            routed_presentation_terminal_phase(AgentTurnState::Completed),
            Some(RoutedWorkflowPhase::Completed)
        );
        assert_eq!(
            routed_presentation_terminal_phase(AgentTurnState::Failed),
            Some(RoutedWorkflowPhase::Failed)
        );
        assert_eq!(
            routed_presentation_terminal_phase(AgentTurnState::Interrupted),
            Some(RoutedWorkflowPhase::Interrupted)
        );
        assert_eq!(
            routed_presentation_terminal_phase(AgentTurnState::Running),
            None
        );
    }
}
