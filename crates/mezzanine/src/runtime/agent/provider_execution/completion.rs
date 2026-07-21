//! Provider completion ingress and settlement handoff.

use super::super::{
    AgentId, AgentTurnExecution, AgentTurnState, MezError, Result, RuntimeSessionService,
    runtime_validate_provider_completion_execution, runtime_validate_provider_completion_identity,
};

impl RuntimeSessionService {
    /// Applies a provider-worker completion event through actor-owned runtime
    /// ingress.
    ///
    /// Async provider workers perform network I/O outside the runtime actor and
    /// submit the deterministic turn execution back through this path. The
    /// completion event is validated against the active turn before it can
    /// update transcript, audit, scheduler, approval, prompt, or terminal state.
    pub async fn apply_agent_provider_completed_event(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        mut execution: AgentTurnExecution,
    ) -> Result<bool> {
        self.require_live()?;
        let Some(turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            self.agent.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.state != AgentTurnState::Running {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            self.agent.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        let Some(mut model_profile) = self.agent.agent_turn_model_profiles.get(turn_id).cloned()
        else {
            let error = MezError::invalid_state("runtime agent turn has no model profile");
            self.fail_agent_turn_after_provider_completion_application_error(
                &turn,
                execution.response.provider.as_str(),
                None,
                &error,
            );
            return Ok(true);
        };
        if let Err(error) = runtime_validate_provider_completion_identity(
            &turn,
            agent_id.as_str(),
            turn_id,
            &execution,
        ) {
            let error = MezError::from(error);
            let provider_id = execution.response.provider.clone();
            self.fail_agent_turn_after_provider_completion_application_error(
                &turn,
                &provider_id,
                Some(&model_profile),
                &error,
            );
            return Ok(true);
        }
        let consumed_high_water_mark = self
            .agent
            .claimed_agent_provider_tasks
            .get(turn_id)
            .map(|claim| claim.context_event_high_water_mark);
        let current_high_water_mark = self
            .agent_turn_contexts()
            .get(turn_id)
            .map(mez_agent::AgentContext::event_sequence_high_water_mark)
            .unwrap_or_default();
        if consumed_high_water_mark.is_some_and(|consumed| current_high_water_mark > consumed) {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            self.agent.claimed_agent_provider_tasks.remove(turn_id);
            self.agent
                .pending_agent_provider_tasks
                .insert(turn_id.to_string());
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                &format!(
                    "provider_response discarded reason=stale_context consumed_event_sequence={} current_event_sequence={}",
                    consumed_high_water_mark.unwrap_or_default(),
                    current_high_water_mark
                ),
            )?;
            return Ok(true);
        }
        if execution.request.interaction_kind
            == mez_agent::ModelInteractionKind::SandboxFailureAssessment
        {
            let provider_id = execution.response.provider.clone();
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            self.agent.claimed_agent_provider_tasks.remove(turn_id);
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                "provider_task completed reason=sandbox_failure_assessment_provider_event",
            )?;
            if let Err(error) =
                self.apply_sandbox_failure_assessment_provider_response(&turn, &execution.response)
            {
                self.append_agent_trace_provider_error(
                    &turn,
                    &provider_id,
                    &model_profile,
                    &error,
                )?;
                let _ = self.settle_pending_sandbox_failure_assessment(
                    turn_id,
                    "assessment_application_error",
                )?;
            }
            return Ok(true);
        }
        if execution.request.interaction_kind == mez_agent::ModelInteractionKind::MacroJudge {
            let Some(step_index) = self.macro_judge_step_index_for_turn(turn_id) else {
                let error = MezError::invalid_state(
                    "macro judge completion has no pending macro judge step",
                );
                let provider_id = execution.response.provider.clone();
                self.fail_agent_turn_after_provider_completion_application_error(
                    &turn,
                    &provider_id,
                    Some(&model_profile),
                    &error,
                );
                return Ok(true);
            };
            let provider_id = execution.response.provider.clone();
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            self.agent.claimed_agent_provider_tasks.remove(turn_id);
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                "provider_task completed reason=macro_judge_provider_event",
            )?;
            if let Err(error) =
                self.apply_macro_judge_provider_response(&turn, step_index, &execution.response)
            {
                self.fail_agent_turn_after_provider_completion_application_error(
                    &turn,
                    &provider_id,
                    Some(&model_profile),
                    &error,
                );
            }
            return Ok(true);
        }
        if let Err(error) = runtime_validate_provider_completion_execution(&turn, &mut execution) {
            let error = MezError::from(error);
            let provider_id = execution.response.provider.clone();
            self.fail_agent_turn_after_provider_completion_application_error(
                &turn,
                &provider_id,
                Some(&model_profile),
                &error,
            );
            return Ok(true);
        }
        let execution_profile = mez_agent::apply_auto_sizing_execution_profile(
            model_profile.clone(),
            &execution.request,
        );
        if execution_profile != model_profile {
            self.agent
                .agent_turn_model_profiles
                .insert(turn_id.to_string(), execution_profile.clone());
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                &format!(
                    "auto_sizing applied provider={} model={} reasoning={}",
                    execution_profile.provider,
                    execution_profile.model,
                    execution_profile
                        .reasoning_profile
                        .as_deref()
                        .unwrap_or("none")
                ),
            )?;
            model_profile = execution_profile;
        }
        self.agent.pending_agent_provider_tasks.remove(turn_id);
        self.agent.claimed_agent_provider_tasks.remove(turn_id);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            "provider_task completed reason=typed_provider_event",
        )?;
        let provider_id = execution.response.provider.clone();
        if let Err(error) = self
            .apply_agent_provider_execution_async(&turn, &model_profile, &provider_id, execution)
            .await
        {
            self.fail_agent_turn_after_provider_completion_application_error(
                &turn,
                &provider_id,
                Some(&model_profile),
                &error,
            );
        }
        Ok(true)
    }
}
