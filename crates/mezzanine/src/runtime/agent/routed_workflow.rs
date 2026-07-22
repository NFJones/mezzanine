//! Product adapter for routed-worker lifecycle and presentation effects.
//!
//! Routing classification returns to the serialized runtime actor before any
//! user work runs. The actor keeps the parent turn on its ordinary profile and
//! owns creation, tracking, provider dispatch, cancellation, persistence, and
//! eventual presentation of the managed child. Provider-independent handoff,
//! repair, failure, cancellation, and terminal decisions are planned by
//! `mez-agent::routed_workflow` and interpreted here against runtime services.

use super::{
    AgentContext, AgentId, AgentTurnExecution, AgentTurnRecord, AgentTurnState,
    AutoSizingRoutingPolicy, AutoSizingRoutingSelection, ContextSourceKind, MezError, Result,
    RuntimeAgentLoopCompletion, RuntimeAgentLoopState, RuntimeAgentLoopTurn, RuntimeSessionService,
    ScheduledWork, current_unix_seconds, runtime_spawn_json_agent_and_turn,
    runtime_subagent_placement_mode, runtime_subagent_spawn_request,
};
use mez_agent::routed_workflow::{
    RoutedFailurePlan, RoutedPresentationCompletionPlan, RoutedWorkerCompletionPlan,
    RoutedWorkerHandoff, RoutedWorkflowEvent, RoutedWorkflowPhase, RoutedWorkflowState,
    RoutedWorkflowTransitionPlan, insert_routed_context_blocks, plan_routed_workflow_transition,
    routed_failure_context_blocks, routed_presentation_context_blocks, routed_worker_seed_context,
};
use mez_agent::{ModelProfile, ScheduledWorkKind};

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
    /// Returns the active parent workflow that authoritatively owns one child.
    ///
    /// The reverse index is accepted only when it agrees with workflow state;
    /// a workflow scan recovers safely when that optimization is absent.
    pub(crate) fn routed_parent_turn_id_for_child(&self, child_turn_id: &str) -> Option<String> {
        if let Some(parent_turn_id) = self.agent.routed_workflow_by_child_turn.get(child_turn_id)
            && self
                .agent
                .routed_workflows_by_parent_turn
                .get(parent_turn_id)
                .is_some_and(|workflow| workflow.child_turn_id.as_deref() == Some(child_turn_id))
        {
            return Some(parent_turn_id.clone());
        }
        self.agent
            .routed_workflows_by_parent_turn
            .iter()
            .find(|(_, workflow)| workflow.child_turn_id.as_deref() == Some(child_turn_id))
            .map(|(parent_turn_id, _)| parent_turn_id.clone())
    }

    /// Makes a routed parent eligible for its next provider request without
    /// bypassing scheduler capacity or fairness.
    ///
    /// Ordinary routed completion moves a dependency-waiting parent back to
    /// the ready queue. Setup failures occur before the parent enters waiting
    /// state, so their already-running parent can queue the bounded explanation
    /// directly without a synthetic release/reacquire cycle.
    fn queue_routed_parent_provider_continuation(
        &mut self,
        parent_turn: &AgentTurnRecord,
        reason: &str,
    ) -> Result<()> {
        if self
            .agent
            .agent_scheduler
            .waiting_turns()
            .any(|work| work.turn_id == parent_turn.turn_id)
        {
            self.agent
                .agent_scheduler
                .requeue_waiting(&parent_turn.turn_id)?;
            let _ = self.append_routed_parent_continuation_trace(
                &parent_turn.pane_id,
                &parent_turn.turn_id,
                &format!("scheduler waiting -> queued reason={reason} capacity=reacquire"),
            );
        } else if self
            .agent
            .agent_scheduler
            .running_turns()
            .any(|work| work.turn_id == parent_turn.turn_id)
        {
            self.agent
                .pending_agent_provider_tasks
                .insert(parent_turn.turn_id.clone());
            let _ = self.append_routed_parent_continuation_trace(
                &parent_turn.pane_id,
                &parent_turn.turn_id,
                &format!("provider_task queued reason={reason}_already_running"),
            );
        } else if !self
            .agent
            .agent_scheduler
            .queued_turns()
            .any(|work| work.turn_id == parent_turn.turn_id)
        {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotFound,
                "routed parent scheduler work is unavailable",
            ));
        }
        self.start_ready_agent_turns()?;
        Ok(())
    }

    /// Records a continuation diagnostic without allowing observability to
    /// invalidate the already-committed scheduler transition.
    fn append_routed_parent_continuation_trace(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        message: &str,
    ) -> Result<()> {
        #[cfg(test)]
        if std::mem::take(&mut self.agent.fail_routed_parent_continuation_trace) {
            return Err(MezError::invalid_state(
                "injected routed parent continuation trace failure",
            ));
        }
        self.append_agent_trace_turn_event(pane_id, turn_id, message)
    }

    /// Accepts a completed routing decision at the actor boundary.
    ///
    /// Root turns retain the managed-child workflow, while existing subagents
    /// apply the selected profile to their current turn and redispatch it.
    pub(crate) fn apply_routing_selected_transition(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        selection: AutoSizingRoutingSelection,
    ) -> Result<crate::runtime::RuntimeTransition> {
        let turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("routed turn is unavailable"))?;
        if turn.agent_id != agent_id.as_str() {
            return Err(MezError::invalid_args(
                "routing selection agent id does not match turn",
            ));
        }
        if turn.state != mez_agent::AgentTurnState::Running {
            return Ok(crate::runtime::RuntimeTransition::default());
        }
        if self.agent_turn_routing_applied(turn_id) {
            return Ok(crate::runtime::RuntimeTransition::default());
        }
        if self.auto_sizing_routing_policy_for_turn(&turn) == AutoSizingRoutingPolicy::InPlace {
            return self.commit_in_place_routing_selection(&turn, selection);
        }
        if self
            .agent
            .routed_workflows_by_parent_turn
            .contains_key(turn_id)
        {
            return Ok(crate::runtime::RuntimeTransition::default());
        }
        match self.commit_routed_worker_selected_transition(agent_id, turn_id, selection) {
            Ok(transition) => Ok(transition),
            Err(error) => {
                self.recover_routed_worker_selection_failure(&turn, error.message())?;
                Ok(self.runtime_transition_with_render(
                    true,
                    Some(crate::runtime::RenderInvalidationReason::FullRedraw),
                ))
            }
        }
    }

    /// Applies one routing selection to an existing subagent turn exactly once.
    fn commit_in_place_routing_selection(
        &mut self,
        turn: &AgentTurnRecord,
        selection: AutoSizingRoutingSelection,
    ) -> Result<crate::runtime::RuntimeTransition> {
        if !self.mark_agent_turn_routing_applied(turn.turn_id.clone()) {
            return Ok(crate::runtime::RuntimeTransition::default());
        }
        self.set_agent_turn_model_profile(turn.turn_id.clone(), selection.selected_profile.clone());
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
                    "agent: routing fallback model {}: {fallback}",
                    selection.selected_profile.model
                ),
            )?;
        }
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "routing selected policy=in_place provider={} model={}",
                selection.selected_profile.provider, selection.selected_profile.model
            ),
        )?;
        self.queue_agent_provider_task(turn.turn_id.clone());
        Ok(self.runtime_transition_with_render(
            true,
            Some(crate::runtime::RenderInvalidationReason::FullRedraw),
        ))
    }

    /// Commits one routed worker selection after the parent boundary is valid.
    fn commit_routed_worker_selected_transition(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        selection: AutoSizingRoutingSelection,
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
        let parent_context = self
            .agent_turn_contexts()
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("routed parent context is unavailable"))?;
        let original_user_prompt = parent_context
            .blocks()
            .iter()
            .find(|block| {
                block.source == ContextSourceKind::UserInstruction && block.label == "user prompt"
            })
            .map(|block| block.content.clone())
            .ok_or_else(|| MezError::invalid_state("routed parent prompt is unavailable"))?;
        let worker_seed_context =
            routed_worker_seed_context(&parent_context, &original_user_prompt);
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
        let child_pane_id = child_agent_id
            .strip_prefix("agent-")
            .ok_or_else(|| MezError::invalid_state("routed worker agent id is invalid"))?
            .to_string();
        let setup_result = (|| {
            #[cfg(test)]
            if std::mem::take(&mut self.agent.fail_routed_worker_after_spawn) {
                return Err(MezError::invalid_state(
                    "injected routed worker post-spawn setup failure",
                ));
            }
            if child_turn_id.is_some() {
                return Err(MezError::invalid_state(
                    "routed worker idle spawn unexpectedly created a turn",
                ));
            }
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
            self.set_agent_routing_override(
                &child_pane_id,
                Some(self.agent_routing_enabled_for_pane(&turn.pane_id)),
            );
            self.set_agent_auto_sizing_override(
                &child_pane_id,
                Some(self.agent_auto_sizing_for_pane(&turn.pane_id).clone()),
            );

            let child_turn = self.queue_routed_child_turn(RoutedChildTurnRequest {
                parent_turn: &turn,
                child_agent_id: &child_agent_id,
                child_pane_id: &child_pane_id,
                prompt: &original_user_prompt,
                model_profile: selection.selected_profile.clone(),
                seed_context: Some(worker_seed_context),
                initial_capability: None,
                reason: "routed_worker_execute",
            })?;
            self.adopt_routed_worker_loop(&turn.turn_id, &child_turn, &selection.selected_profile)?;
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
                .insert(turn.turn_id.clone(), selection.selected_profile.clone());
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
                    worker_model_profile: Some(selection.selected_profile.model.clone()),
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
                        selection.selected_profile.model
                    ),
                )?;
            }
            self.agent.agent_scheduler.wait_running(turn_id)?;
            self.agent_turn_ledger_mut()
                .finish_turn(turn_id, AgentTurnState::Blocked)?;
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                &format!(
                    "routed_worker selected provider={} model={} child_agent={} child_turn={}",
                    selection.selected_profile.provider,
                    selection.selected_profile.model,
                    child_agent_id,
                    child_turn.turn_id
                ),
            )?;
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                "scheduler running -> waiting reason=waiting_for_routed_worker capacity=released",
            )?;
            self.start_ready_agent_turns()?;
            Ok(self.runtime_transition_with_render(
                true,
                Some(crate::runtime::RenderInvalidationReason::FullRedraw),
            ))
        })();
        if setup_result.is_err() {
            self.cleanup_failed_subagent_spawn(
                None,
                &child_pane_id,
                &child_agent_id,
                child_turn_id.as_deref(),
            );
        }
        setup_result
    }

    /// Transfers a routed classifier's logical loop ownership to its selected worker.
    fn adopt_routed_worker_loop(
        &mut self,
        parent_turn_id: &str,
        child_turn: &AgentTurnRecord,
        worker_profile: &ModelProfile,
    ) -> Result<()> {
        let Some(parent_loop_turn) = self.remove_agent_loop_turn(parent_turn_id) else {
            return Ok(());
        };
        let loop_id = parent_loop_turn.loop_id.clone();
        let state = self
            .agent_loop_state_mut_by_id(&loop_id)
            .ok_or_else(|| MezError::invalid_state("routed parent loop state is unavailable"))?;
        state.execution_pane_id = child_turn.pane_id.clone();
        state.routed_parent_turn_id = Some(parent_turn_id.to_string());
        state.routed_worker_profile = Some(worker_profile.clone());
        self.agent
            .agent_loop_by_pane
            .insert(child_turn.pane_id.clone(), loop_id.clone());
        self.insert_agent_loop_turn(
            child_turn.turn_id.clone(),
            RuntimeAgentLoopTurn {
                loop_id,
                pane_id: child_turn.pane_id.clone(),
                kind: parent_loop_turn.kind,
                iteration: parent_loop_turn.iteration,
            },
        );
        Ok(())
    }

    /// Registers a continued routed-loop turn with its pinned worker workflow.
    pub(crate) fn register_routed_loop_continuation(
        &mut self,
        state: &RuntimeAgentLoopState,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        let (Some(parent_turn_id), Some(worker_profile)) = (
            state.routed_parent_turn_id.as_deref(),
            state.routed_worker_profile.as_ref(),
        ) else {
            return Ok(());
        };
        let parent_agent_id = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|candidate| candidate.turn_id == parent_turn_id)
            .map(|parent| parent.agent_id.clone())
            .ok_or_else(|| MezError::invalid_state("routed loop parent turn is unavailable"))?;
        self.agent
            .agent_turn_model_profiles
            .insert(turn.turn_id.clone(), worker_profile.clone());
        self.agent
            .routed_workflow_by_child_turn
            .insert(turn.turn_id.clone(), parent_turn_id.to_string());
        self.agent
            .subagent_task_routes
            .insert(turn.turn_id.clone(), parent_agent_id);
        let workflow = self
            .agent
            .routed_workflows_by_parent_turn
            .get_mut(parent_turn_id)
            .ok_or_else(|| MezError::invalid_state("routed loop workflow is unavailable"))?;
        workflow.child_turn_id = Some(turn.turn_id.clone());
        Ok(())
    }

    /// Converts a routed loop continuation queue failure into parent recovery.
    ///
    /// Loop settlement has already removed the controller and restored the
    /// invoking conversation before this transition runs. The routed parent
    /// therefore receives one bounded response-only explanation while late
    /// results for the superseded worker turn remain handled no-ops.
    pub(crate) fn fail_routed_loop_continuation(
        &mut self,
        parent_turn_id: &str,
        child_turn_id: &str,
        diagnostic: &str,
    ) -> Result<()> {
        let parent_turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == parent_turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("routed loop parent turn is unavailable"))?;
        if let Some(workflow) = self
            .agent
            .routed_workflows_by_parent_turn
            .get_mut(parent_turn_id)
        {
            workflow.diagnostic = Some(format!("loop continuation queue: {diagnostic}"));
        }
        self.ready_routed_parent_for_error_explanation(
            &parent_turn,
            "loop continuation queue",
            "",
            diagnostic,
        )?;
        self.agent.subagent_task_routes.remove(child_turn_id);
        Ok(())
    }

    /// Rolls back partial routed-child setup and queues one bounded explanation.
    fn recover_routed_worker_selection_failure(
        &mut self,
        parent_turn: &AgentTurnRecord,
        diagnostic: &str,
    ) -> Result<()> {
        let child_turns = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .filter(|turn| {
                turn.parent_turn_id.as_deref() == Some(parent_turn.turn_id.as_str())
                    && turn.cooperation_mode.as_deref() == Some("routed-worker")
            })
            .cloned()
            .collect::<Vec<_>>();
        for child_turn in child_turns {
            self.agent
                .routed_workflow_by_child_turn
                .remove(&child_turn.turn_id);
            self.agent.subagent_task_routes.remove(&child_turn.turn_id);
            let _ = self.agent.agent_scheduler.cancel(&child_turn.turn_id);
            let _ = self.cancel_live_shell_transactions_for_turn(&child_turn.turn_id);
            self.agent
                .pending_agent_provider_tasks
                .remove(&child_turn.turn_id);
            self.agent
                .claimed_agent_provider_tasks
                .remove(&child_turn.turn_id);
            self.agent
                .pending_terminal_subagent_pane_closes
                .insert(child_turn.pane_id.clone());
            self.remove_subagent_authority_state(&child_turn.agent_id);
            self.integration
                .model_profile_overrides_mut()
                .agent_profiles
                .remove(&child_turn.agent_id);
            let running_in_shell = self
                .agent_shell_store()
                .get(&child_turn.pane_id)
                .and_then(|session| session.running_turn_id.as_deref())
                == Some(child_turn.turn_id.as_str());
            if running_in_shell {
                let _ = self.finish_agent_turn(
                    &child_turn.pane_id,
                    &child_turn.turn_id,
                    AgentTurnState::Interrupted,
                );
            } else if matches!(
                child_turn.state,
                AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
            ) {
                let _ = self.finish_agent_turn_without_shell_session(
                    &child_turn,
                    AgentTurnState::Interrupted,
                );
            }
        }
        self.agent
            .routed_workflow_by_child_turn
            .retain(|_, parent| parent != &parent_turn.turn_id);
        self.agent
            .routed_child_contexts_by_parent_turn
            .remove(&parent_turn.turn_id);
        self.agent
            .routed_child_profiles_by_parent_turn
            .remove(&parent_turn.turn_id);
        let parent_session = self.agent_shell_store().get(&parent_turn.pane_id);
        self.agent.routed_workflows_by_parent_turn.insert(
            parent_turn.turn_id.clone(),
            RoutedWorkflowState {
                run_id: parent_turn.turn_id.clone(),
                parent_agent_id: parent_turn.agent_id.clone(),
                parent_pane_id: parent_turn.pane_id.clone(),
                parent_conversation_id: parent_session
                    .map(|session| session.session_id.clone())
                    .unwrap_or_else(|| parent_turn.pane_id.clone()),
                parent_transcript_entries: parent_session
                    .map(|session| session.transcript_entries)
                    .unwrap_or(0),
                original_user_prompt: String::new(),
                main_model_profile: parent_turn.model_profile.clone(),
                worker_model_profile: None,
                child_agent_id: None,
                child_conversation_id: None,
                child_turn_id: None,
                worker_final_result: None,
                handoff: None,
                handoff_repair_attempts: 0,
                error_explanation_attempted: false,
                phase: RoutedWorkflowPhase::WaitingForWorkerResult,
                diagnostic: Some(diagnostic.to_string()),
            },
        );
        self.ready_routed_parent_for_error_explanation(
            parent_turn,
            "worker selection setup",
            "",
            diagnostic,
        )
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
            return Ok(turn.cooperation_mode.as_deref() == Some("routed-worker"));
        };
        let state = self
            .agent
            .routed_workflows_by_parent_turn
            .get(&parent_turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("routed workflow state is unavailable"))?;
        if state.child_turn_id.as_deref() != Some(turn.turn_id.as_str()) {
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
        let transition =
            plan_routed_workflow_transition(&state, RoutedWorkflowEvent::ChildSettled(execution))
                .map_err(MezError::invalid_state)?;
        let RoutedWorkflowTransitionPlan::Worker(plan) = transition else {
            return Ok(true);
        };

        match plan {
            RoutedWorkerCompletionPlan::RequestHandoff {
                worker_final_result,
                prompt,
                exact_result_block,
            } => {
                let Some(mut handoff_context) = self
                    .agent_turn_contexts()
                    .get(&turn.turn_id)
                    .cloned()
                    .or_else(|| {
                        self.agent
                            .routed_child_contexts_by_parent_turn
                            .get(&parent_turn_id)
                            .cloned()
                    })
                else {
                    self.ready_routed_parent_for_error_explanation(
                        &parent_turn,
                        "summary request",
                        &worker_final_result,
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
                        &worker_final_result,
                        "routed worker profile snapshot is unavailable",
                    )?;
                    self.agent.subagent_task_routes.remove(&turn.turn_id);
                    return Ok(true);
                };
                insert_routed_context_blocks(&mut handoff_context, vec![exact_result_block])?;
                self.agent
                    .routed_child_contexts_by_parent_turn
                    .insert(parent_turn_id.clone(), handoff_context.clone());
                let handoff_turn = match self.queue_routed_child_turn(RoutedChildTurnRequest {
                    parent_turn: &parent_turn,
                    child_agent_id: &child_agent_id,
                    child_pane_id: &child_pane_id,
                    prompt,
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
                            &worker_final_result,
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
                    workflow.worker_final_result = Some(worker_final_result);
                    workflow.child_turn_id = Some(handoff_turn.turn_id);
                    workflow.phase = RoutedWorkflowPhase::WaitingForHandoff;
                }
            }
            RoutedWorkerCompletionPlan::RepairHandoff {
                worker_final_result,
                prompt,
                diagnostic,
                context_blocks,
            } => {
                let Some(child_profile) = self
                    .agent
                    .routed_child_profiles_by_parent_turn
                    .get(&parent_turn_id)
                    .cloned()
                else {
                    self.ready_routed_parent_for_error_explanation(
                        &parent_turn,
                        "summary repair",
                        &worker_final_result,
                        "routed worker profile snapshot is unavailable",
                    )?;
                    self.agent.subagent_task_routes.remove(&turn.turn_id);
                    return Ok(true);
                };
                let existing_repair_context = self
                    .agent_turn_contexts()
                    .get(&turn.turn_id)
                    .cloned()
                    .or_else(|| {
                        self.agent
                            .routed_child_contexts_by_parent_turn
                            .get(&parent_turn_id)
                            .cloned()
                    });
                let repair_context = if let Some(mut context) = existing_repair_context {
                    insert_routed_context_blocks(&mut context, context_blocks)?;
                    context
                } else {
                    AgentContext::import_durable_blocks(context_blocks)
                        .map_err(|error| MezError::invalid_state(error.to_string()))?
                };
                let repair_turn = match self.queue_routed_child_turn(RoutedChildTurnRequest {
                    parent_turn: &parent_turn,
                    child_agent_id: &child_agent_id,
                    child_pane_id: &child_pane_id,
                    prompt,
                    model_profile: child_profile,
                    seed_context: Some(repair_context),
                    initial_capability: Some(mez_agent::AgentCapability::RespondOnly),
                    reason: "routed_worker_handoff_repair",
                }) {
                    Ok(turn) => turn,
                    Err(queue_error) => {
                        self.ready_routed_parent_for_error_explanation(
                            &parent_turn,
                            "summary repair",
                            &worker_final_result,
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
                    workflow.diagnostic = Some(diagnostic);
                }
            }
            RoutedWorkerCompletionPlan::Present {
                worker_final_result,
                handoff,
            } => {
                if let Some(workflow) = self
                    .agent
                    .routed_workflows_by_parent_turn
                    .get_mut(&parent_turn_id)
                {
                    workflow.handoff = Some(handoff.clone());
                }
                self.ready_routed_parent_for_presentation(
                    &parent_turn,
                    &worker_final_result,
                    Some(&handoff),
                    None,
                )?;
            }
            RoutedWorkerCompletionPlan::ExplainFailure(failure) => {
                self.apply_routed_failure_plan(&parent_turn, failure)?;
            }
        }
        self.agent.subagent_task_routes.remove(&turn.turn_id);
        Ok(true)
    }

    /// Settles cancellation of one managed routed child through the parent.
    ///
    /// The generic subagent cancellation path cannot resume a blocked routed
    /// parent. This transition records a phase-specific diagnostic, queues the
    /// single response-only main-model explanation, and removes superseded
    /// child indexes before ordinary child-turn cleanup continues.
    pub(crate) fn handle_routed_child_cancellation(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<bool> {
        let parent_turn_id = self.routed_parent_turn_id_for_child(&turn.turn_id);
        let Some(parent_turn_id) = parent_turn_id else {
            return Ok(false);
        };
        let state = self
            .agent
            .routed_workflows_by_parent_turn
            .get(&parent_turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("routed workflow state is unavailable"))?;
        let transition =
            plan_routed_workflow_transition(&state, RoutedWorkflowEvent::ChildCancelled)
                .map_err(MezError::invalid_state)?;
        let RoutedWorkflowTransitionPlan::ChildCancellation(failure) = transition else {
            self.agent
                .routed_workflow_by_child_turn
                .remove(&turn.turn_id);
            self.agent.subagent_task_routes.remove(&turn.turn_id);
            return Ok(true);
        };
        let parent_turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|candidate| candidate.turn_id == parent_turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("routed parent turn is unavailable"))?;
        self.apply_routed_failure_plan(&parent_turn, failure)?;
        self.clear_routed_workflow_runtime_state(&parent_turn_id);
        self.agent.subagent_task_routes.remove(&turn.turn_id);
        Ok(true)
    }

    /// Recovers an active routed workflow whose child has no execution record.
    pub(crate) fn handle_routed_child_missing_execution(
        &mut self,
        turn: &AgentTurnRecord,
        terminal_state: AgentTurnState,
    ) -> Result<bool> {
        let Some(parent_turn_id) = self.routed_parent_turn_id_for_child(&turn.turn_id) else {
            return Ok(false);
        };
        let state = self
            .agent
            .routed_workflows_by_parent_turn
            .get(&parent_turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("routed workflow state is unavailable"))?;
        let transition = plan_routed_workflow_transition(
            &state,
            RoutedWorkflowEvent::ChildTerminatedWithoutExecution(terminal_state),
        )
        .map_err(MezError::invalid_state)?;
        let RoutedWorkflowTransitionPlan::ChildMissingExecution(failure) = transition else {
            self.agent
                .routed_workflow_by_child_turn
                .remove(&turn.turn_id);
            self.agent.subagent_task_routes.remove(&turn.turn_id);
            return Ok(true);
        };
        let parent_turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|candidate| candidate.turn_id == parent_turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("routed parent turn is unavailable"))?;
        self.apply_routed_failure_plan(&parent_turn, failure)?;
        self.clear_routed_workflow_runtime_state(&parent_turn_id);
        self.agent.subagent_task_routes.remove(&turn.turn_id);
        Ok(true)
    }

    /// Cancels the active managed child when its blocked routed parent stops.
    ///
    /// Parent interruption is terminal for the whole managed workflow. The
    /// child is settled through its ordinary lifecycle cleanup while routed
    /// ownership still identifies it, then all join and presentation state is
    /// removed so late provider results become handled no-ops.
    pub(crate) fn cancel_routed_workflow_for_parent(
        &mut self,
        parent_turn_id: &str,
    ) -> Result<bool> {
        let Some(state) = self
            .agent
            .routed_workflows_by_parent_turn
            .get(parent_turn_id)
            .cloned()
        else {
            return Ok(false);
        };
        if let Some(loop_id) = self
            .agent
            .agent_loops_by_id
            .values()
            .find(|loop_state| loop_state.routed_parent_turn_id.as_deref() == Some(parent_turn_id))
            .map(|loop_state| loop_state.loop_id.clone())
            && let Some(loop_state) = self.remove_agent_loop_state_by_id(&loop_id)
        {
            self.restore_agent_loop_parent_conversation(&loop_state.invoking_pane_id, &loop_state)?;
            if let Some(completion) = loop_state.completion {
                self.retain_routed_loop_completion(parent_turn_id, completion)?;
            }
        }
        let mut cleanup_error = None;
        if let Some(child_agent_id) = state.child_agent_id.as_deref() {
            self.remove_subagent_authority_state(child_agent_id);
            self.integration
                .model_profile_overrides_mut()
                .agent_profiles
                .remove(child_agent_id);
        }
        if let Some(child_turn_id) = state.child_turn_id.as_deref() {
            let _ = self.cancel_agent_work(child_turn_id);
            if let Err(error) = self.cancel_live_shell_transactions_for_turn(child_turn_id) {
                cleanup_error = Some(error);
            }
            self.remove_pending_agent_provider_task(child_turn_id);
            self.remove_claimed_agent_provider_task(child_turn_id);
            self.clear_blocked_agent_approvals_for_turn(child_turn_id);
            self.agent
                .routed_workflow_by_child_turn
                .remove(child_turn_id);
            self.agent.subagent_task_routes.remove(child_turn_id);
            if let Some(workflow) = self
                .agent
                .routed_workflows_by_parent_turn
                .get_mut(parent_turn_id)
            {
                workflow.child_turn_id = None;
            }
            if let Some(child_turn) = self
                .agent_turn_ledger()
                .turns()
                .iter()
                .find(|turn| turn.turn_id == child_turn_id)
                .cloned()
            {
                let child_is_terminal = matches!(
                    child_turn.state,
                    AgentTurnState::Completed
                        | AgentTurnState::Failed
                        | AgentTurnState::Interrupted
                );
                if !child_is_terminal {
                    self.agent
                        .pending_terminal_subagent_pane_closes
                        .insert(child_turn.pane_id.clone());
                    if self
                        .agent_shell_store()
                        .get(&child_turn.pane_id)
                        .and_then(|session| session.running_turn_id.as_deref())
                        == Some(child_turn_id)
                    {
                        if let Err(error) = self.finish_agent_turn(
                            &child_turn.pane_id,
                            child_turn_id,
                            AgentTurnState::Interrupted,
                        ) && cleanup_error.is_none()
                        {
                            cleanup_error = Some(error);
                        }
                    } else {
                        if let Err(error) = self.finish_agent_turn_without_shell_session(
                            &child_turn,
                            AgentTurnState::Interrupted,
                        ) && cleanup_error.is_none()
                        {
                            cleanup_error = Some(error);
                        }
                    }
                }
            }
        }
        self.agent
            .routed_workflows_by_parent_turn
            .remove(parent_turn_id);
        self.clear_routed_workflow_runtime_state(parent_turn_id);
        self.agent.routed_presentation_turns.remove(parent_turn_id);
        if let Some(error) = cleanup_error {
            return Err(error);
        }
        Ok(true)
    }

    /// Applies one provider-neutral routed failure plan at the runtime boundary.
    fn apply_routed_failure_plan(
        &mut self,
        parent_turn: &AgentTurnRecord,
        failure: RoutedFailurePlan,
    ) -> Result<()> {
        let RoutedFailurePlan {
            stage,
            child_output,
            diagnostic,
            parent_context_blocks,
            worker_final_result_update,
        } = failure;
        if !parent_context_blocks.is_empty() {
            let context = self
                .agent_turn_contexts_mut()
                .get_mut(&parent_turn.turn_id)
                .ok_or_else(|| MezError::invalid_state("routed parent context is unavailable"))?;
            insert_routed_context_blocks(context, parent_context_blocks)?;
        }
        if let Some(worker_final_result) = worker_final_result_update
            && let Some(workflow) = self
                .agent
                .routed_workflows_by_parent_turn
                .get_mut(&parent_turn.turn_id)
        {
            workflow.worker_final_result = Some(worker_final_result);
        }
        self.ready_routed_parent_for_error_explanation(
            parent_turn,
            &stage,
            &child_output,
            &diagnostic,
        )
    }

    /// Adds a routed diagnostic to the parent context and queues one explanation.
    fn ready_routed_parent_for_error_explanation(
        &mut self,
        parent_turn: &AgentTurnRecord,
        stage: &str,
        child_output: &str,
        diagnostic: &str,
    ) -> Result<()> {
        if !self
            .agent_turn_contexts()
            .contains_key(&parent_turn.turn_id)
        {
            if let Some(workflow) = self
                .agent
                .routed_workflows_by_parent_turn
                .get_mut(&parent_turn.turn_id)
            {
                workflow.phase = RoutedWorkflowPhase::Failed;
                workflow.error_explanation_attempted = true;
                workflow.diagnostic = Some(format!(
                    "{stage}: {diagnostic}; routed parent context is unavailable"
                ));
            }
            self.agent
                .routed_presentation_turns
                .remove(&parent_turn.turn_id);
            self.append_agent_status_text_to_terminal_buffer(
                &parent_turn.pane_id,
                "agent: routed workflow failed without parent context",
            )?;
            self.release_routed_child_for_close(parent_turn)?;
            self.finish_agent_turn(
                &parent_turn.pane_id,
                &parent_turn.turn_id,
                AgentTurnState::Failed,
            )?;
            self.agent
                .routed_workflows_by_parent_turn
                .remove(&parent_turn.turn_id);
            self.clear_routed_workflow_runtime_state(&parent_turn.turn_id);
            return Ok(());
        }
        let context = self
            .agent_turn_contexts_mut()
            .get_mut(&parent_turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("routed parent context is unavailable"))?;
        insert_routed_context_blocks(
            context,
            routed_failure_context_blocks(stage, child_output, diagnostic),
        )?;
        context.validate_durable()?;
        self.agent.agent_turn_interaction_kinds.insert(
            parent_turn.turn_id.clone(),
            mez_agent::ModelInteractionKind::RoutedFailureExplanation,
        );
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
        self.agent_turn_ledger_mut().set_turn_capability(
            &parent_turn.turn_id,
            mez_agent::AgentCapability::RespondOnly,
        )?;
        self.queue_routed_parent_provider_continuation(parent_turn, "routed_failure_ready")?;
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
        let context_blocks = routed_presentation_context_blocks(final_result, handoff, diagnostic)
            .map_err(MezError::invalid_state)?;
        let context = self
            .agent_turn_contexts_mut()
            .get_mut(&parent_turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("routed parent context is unavailable"))?;
        insert_routed_context_blocks(context, context_blocks)?;
        context.validate_durable()?;
        self.agent.agent_turn_interaction_kinds.insert(
            parent_turn.turn_id.clone(),
            mez_agent::ModelInteractionKind::RoutedPresentation,
        );
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
        self.agent_turn_ledger_mut().set_turn_capability(
            &parent_turn.turn_id,
            mez_agent::AgentCapability::RespondOnly,
        )?;
        self.queue_routed_parent_provider_continuation(parent_turn, "routed_presentation_ready")?;
        self.append_agent_status_text_to_terminal_buffer(
            &parent_turn.pane_id,
            "agent: routed worker context received; presenting with main model",
        )?;
        self.release_routed_child_for_close(parent_turn)?;
        Ok(())
    }

    /// Releases one managed routed child after its final workflow step.
    fn release_routed_child_for_close(&mut self, parent_turn: &AgentTurnRecord) -> Result<()> {
        let Some(child_agent_id) = self
            .agent
            .routed_workflows_by_parent_turn
            .get(&parent_turn.turn_id)
            .and_then(|workflow| workflow.child_agent_id.clone())
        else {
            return Ok(());
        };
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

    /// Marks one turn as a routed presentation for focused transcript tests.
    #[cfg(test)]
    pub(crate) fn mark_routed_presentation_turn_for_tests(&mut self, turn_id: &str) {
        self.agent
            .routed_presentation_turns
            .insert(turn_id.to_string());
    }

    /// Retains one macro-loop join until routed parent presentation settles.
    pub(crate) fn retain_routed_loop_completion(
        &mut self,
        parent_turn_id: &str,
        completion: RuntimeAgentLoopCompletion,
    ) -> Result<()> {
        if let Some(existing) = self
            .agent
            .routed_loop_completions_by_parent_turn
            .get(parent_turn_id)
        {
            if existing == &completion {
                return Ok(());
            }
            return Err(MezError::invalid_state(
                "routed parent already owns a different loop completion",
            ));
        }
        self.agent
            .routed_loop_completions_by_parent_turn
            .insert(parent_turn_id.to_string(), completion);
        Ok(())
    }

    /// Settles routed workflow state after the main-model presentation finishes.
    pub(crate) fn complete_routed_presentation(
        &mut self,
        turn_id: &str,
        terminal_state: AgentTurnState,
    ) -> Result<bool> {
        if !self.agent.routed_presentation_turns.contains(turn_id) {
            return Ok(false);
        }
        let Some(state) = self
            .agent
            .routed_workflows_by_parent_turn
            .get(turn_id)
            .cloned()
        else {
            self.agent.routed_presentation_turns.remove(turn_id);
            self.clear_routed_workflow_runtime_state(turn_id);
            return Ok(false);
        };
        let transition = plan_routed_workflow_transition(
            &state,
            RoutedWorkflowEvent::PresentationSettled(terminal_state),
        )
        .map_err(MezError::invalid_state)?;
        let RoutedWorkflowTransitionPlan::Presentation(plan) = transition else {
            return Ok(false);
        };
        match plan {
            RoutedPresentationCompletionPlan::Complete => {
                self.agent.routed_presentation_turns.remove(turn_id);
                self.agent.routed_workflows_by_parent_turn.remove(turn_id);
                self.clear_routed_workflow_runtime_state(turn_id);
                Ok(false)
            }
            RoutedPresentationCompletionPlan::FinishErrorExplanation { diagnostic: _ } => {
                self.agent.routed_presentation_turns.remove(turn_id);
                self.agent.routed_workflows_by_parent_turn.remove(turn_id);
                self.clear_routed_workflow_runtime_state(turn_id);
                Ok(false)
            }
            RoutedPresentationCompletionPlan::ExplainFailure {
                diagnostic,
                context_blocks,
            } => {
                let context = self
                    .agent_turn_contexts_mut()
                    .get_mut(turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("routed parent context is unavailable")
                    })?;
                insert_routed_context_blocks(context, context_blocks)?;
                context.validate_durable()?;
                self.agent.agent_turn_interaction_kinds.insert(
                    turn_id.to_string(),
                    mez_agent::ModelInteractionKind::RoutedFailureExplanation,
                );
                if let Some(workflow) = self.agent.routed_workflows_by_parent_turn.get_mut(turn_id)
                {
                    workflow.phase = RoutedWorkflowPhase::ExplainingError;
                    workflow.error_explanation_attempted = true;
                    workflow.diagnostic = Some(diagnostic);
                }
                self.agent
                    .pending_agent_provider_tasks
                    .insert(turn_id.to_string());
                Ok(true)
            }
            RoutedPresentationCompletionPlan::Fail {
                terminal_phase: _,
                diagnostic: _,
            } => {
                self.agent.routed_presentation_turns.remove(turn_id);
                self.agent.routed_workflows_by_parent_turn.remove(turn_id);
                self.clear_routed_workflow_runtime_state(turn_id);
                Ok(false)
            }
        }
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
        let mut context = match seed_context {
            Some(mut context) => {
                context.append_reference_event(
                    ContextSourceKind::LocalMessage,
                    "routed controller task",
                    prompt,
                )?;
                context
            }
            None => {
                let mut context = self.agent_context_for_pane_prompt(child_pane_id, prompt, 100)?;
                context.reclassify_user_event_as_reference(
                    prompt,
                    ContextSourceKind::LocalMessage,
                    "routed controller task",
                )?;
                self.apply_agent_shell_preference_context(child_pane_id, context)?
            }
        };
        context.set_metadata(
            self.agent_shell_store()
                .get(child_pane_id)
                .map(|session| {
                    mez_agent::ModelContextMetadata::new(
                        Some(session.session_id.clone()),
                        Some(session.prompt_cache_lineage_id.clone()),
                    )
                })
                .unwrap_or_default(),
        );
        context.validate_durable()?;
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
        self.append_agent_parent_prompt_to_terminal_buffer(child_pane_id, prompt)?;
        self.agent_turn_ledger_mut().queue_turn(turn.clone())?;
        self.agent_turn_contexts_mut()
            .insert(turn_id.clone(), context);
        if let Some(interaction_kind) = match reason {
            "routed_worker_handoff" => Some(mez_agent::ModelInteractionKind::RoutedHandoff),
            "routed_worker_handoff_repair" => {
                Some(mez_agent::ModelInteractionKind::RoutedHandoffRepair)
            }
            _ => None,
        } {
            self.agent
                .agent_turn_interaction_kinds
                .insert(turn_id.clone(), interaction_kind);
        }
        self.agent
            .agent_turn_model_profiles
            .insert(turn_id.clone(), model_profile);
        self.mark_agent_turn_routing_applied(turn_id.clone());
        self.agent
            .subagent_task_routes
            .insert(turn_id.clone(), parent_turn.agent_id.clone());
        self.agent.agent_scheduler.enqueue(ScheduledWork {
            turn_id: turn_id.clone(),
            agent_id: child_agent_id.to_string(),
            pane_id: Some(child_pane_id.to_string()),
            kind: ScheduledWorkKind::ShellCapable,
        })?;
        #[cfg(test)]
        let trace_result = if std::mem::take(&mut self.agent.fail_routed_child_enqueue_trace) {
            Err(MezError::invalid_state(
                "injected routed child enqueue trace failure",
            ))
        } else {
            self.append_agent_trace_turn_event(
                child_pane_id,
                &turn_id,
                &format!("created state=queued reason={reason}"),
            )
        };
        #[cfg(not(test))]
        let trace_result = self.append_agent_trace_turn_event(
            child_pane_id,
            &turn_id,
            &format!("created state=queued reason={reason}"),
        );
        let _ = trace_result;
        Ok(turn)
    }
}
