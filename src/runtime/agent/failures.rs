//! Runtime agent failure settlement helpers.
//!
//! This module owns terminal failure conversion for provider completion,
//! provider errors, hook blocks, and shell transaction failures. It keeps
//! cleanup, transcript persistence, subagent result emission, and status display
//! decisions grouped around failure handling.

use super::*;

impl RuntimeSessionService {
    /// Settles a provider completion that failed while applying runtime state.
    ///
    /// Provider workers submit deterministic completions back to the runtime
    /// actor after the claim lease has been cleared. Any error while applying
    /// those completions must therefore become a pane-local failed turn rather
    /// than an actor-boundary error that can strand a running turn or terminate
    /// the daemon request path.
    ///
    /// # Parameters
    /// - `turn`: The active turn being settled.
    /// - `provider_id`: The provider that produced the completion.
    /// - `model_profile`: The effective model profile for the completion.
    /// - `error`: The runtime application error to surface.
    pub(in crate::runtime) fn fail_agent_turn_after_provider_completion_application_error(
        &mut self,
        turn: &AgentTurnRecord,
        provider_id: &str,
        model_profile: Option<&ModelProfile>,
        error: &MezError,
    ) {
        let _ = self.append_agent_trace_turn_event(
        &turn.pane_id,
        &turn.turn_id,
        &format!(
            "provider_execution failed reason=completion_application_error error_kind={} error={}",
            runtime_mezzanine_error_code(error.kind()),
            runtime_agent_terminal_preview(error.message())
        ),
    );
        let current_state = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|candidate| candidate.turn_id == turn.turn_id)
            .map(|candidate| candidate.state);
        if !matches!(
            current_state,
            Some(AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked)
        ) {
            return;
        }
        if let Some(model_profile) = model_profile
            && self
                .fail_agent_turn_for_provider_error(turn, provider_id, model_profile, error)
                .is_ok()
        {
            return;
        }

        let _ = self.append_agent_error_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent: provider completion failed while applying runtime state: {}",
                error.message()
            ),
        );
        let _ = self.agent.agent_scheduler.complete(&turn.turn_id);
        let _ = self
            .agent_turn_ledger_mut()
            .finish_turn(&turn.turn_id, AgentTurnState::Failed);
        let _ = self.append_agent_trace_turn_transition(
            turn,
            current_state.unwrap_or(turn.state),
            AgentTurnState::Failed,
            "completion_application_error_fallback",
        );
        if self
            .agent_shell_store()
            .get(&turn.pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            == Some(turn.turn_id.as_str())
        {
            let _ = self
                .agent_shell_store_mut()
                .finish_turn(&turn.pane_id, &turn.turn_id);
        }
        self.agent_turn_contexts_mut().remove(&turn.turn_id);
        self.agent_turn_executions_mut().remove(&turn.turn_id);
        self.clear_agent_turn_steering(&turn.turn_id);
        self.clear_agent_failure_feedback_attempts_for_turn(&turn.turn_id);
        self.agent
            .agent_turn_shell_dispatch_history
            .remove(&turn.turn_id);
        self.agent
            .agent_turn_network_action_history
            .remove(&turn.turn_id);
        self.clear_joined_subagent_dependencies_for_turn(&turn.turn_id);
        self.clear_agent_pre_shell_hook_completions_for_turn(&turn.turn_id);
        self.agent.agent_turn_model_profiles.remove(&turn.turn_id);
        self.agent
            .pending_agent_provider_tasks
            .remove(&turn.turn_id);
        self.agent
            .claimed_agent_provider_tasks
            .remove(&turn.turn_id);
        self.clear_blocked_agent_approvals_for_turn(&turn.turn_id);
        let _ = self.start_ready_agent_turns();
    }

    /// Runs the fail agent turn for provider error operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn fail_agent_turn_for_provider_error(
        &mut self,
        turn: &AgentTurnRecord,
        provider_id: &str,
        model_profile: &ModelProfile,
        error: &MezError,
    ) -> Result<()> {
        self.refresh_agent_turn_project_guidance_context(turn)?;
        let context = self
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let request = assemble_model_request(model_profile, turn, &context)?;
        let mut raw_text = match error.provider_raw_text() {
            Some(raw_text) => format!("{raw_text}\nprovider_error: {error}"),
            None => format!("provider_error: {error}"),
        };
        let safe_fallbacks = self
            .provider_registry
            .safe_fallback_profiles(&turn.model_profile)?;
        if !safe_fallbacks.is_empty() {
            raw_text.push_str("\nsafe_fallback_profiles: ");
            raw_text.push_str(&safe_fallbacks.join(","));
        }
        let execution = AgentTurnExecution {
            request,
            response: ModelResponse {
                provider: provider_id.to_string(),
                model: model_profile.model.clone(),
                raw_text,
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: Vec::new(),
            final_turn: true,
            terminal_state: AgentTurnState::Failed,
        };
        self.agent_turn_executions_mut()
            .insert(turn.turn_id.clone(), execution.clone());
        let transcript_entries =
            self.persist_runtime_agent_turn_execution_transcript(turn, &execution)?;
        self.emit_subagent_task_result_for_execution(turn, &execution)?;
        self.complete_running_agent_turn_and_start_ready(
            turn,
            AgentTurnState::Failed,
            "provider_error",
        )?;
        if turn.cooperation_mode.as_deref() == Some("macro-orchestration") {
            let parent_agent_id = format!("agent-{}", turn.pane_id);
            let _ = self.close_subagent_descendants_for_parent_agent(
                &parent_agent_id,
                "macro orchestration turn failed",
            );
        }
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"failed","provider":"{}","error":"provider_error","transcript_entries":{}}}"#,
                json_escape(&turn.pane_id),
                json_escape(&turn.turn_id),
                json_escape(provider_id),
                transcript_entries
            ),
        )?;
        self.set_agent_prompt_display_lines_if_pane_present(
            &turn.pane_id,
            runtime_agent_execution_prompt_display_lines(
                &turn.turn_id,
                provider_id,
                &execution,
                0,
                transcript_entries,
            ),
        )?;
        Ok(())
    }

    /// Settles a running shell action as a runtime failure after its external
    /// pane transaction fails to reach a normal action-result boundary.
    pub(in crate::runtime) fn fail_running_shell_transaction_action(
        &mut self,
        transaction_ref: &RunningShellTransactionRef,
        marker: &str,
        failure: RuntimeShellTransactionActionFailure,
    ) -> Result<usize> {
        let Some(turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == transaction_ref.turn_id)
            .cloned()
        else {
            return Ok(0);
        };
        let maybe_failure = {
            let execution = self
                .agent_turn_executions_mut()
                .get_mut(&turn.turn_id)
                .ok_or_else(|| MezError::invalid_state("running agent execution is unavailable"))?;
            let batch = execution.response.action_batch.as_ref().ok_or_else(|| {
                MezError::invalid_state("running agent execution has no action batch")
            })?;
            let Some(action) = batch
                .actions
                .iter()
                .find(|action| action.id == failure.action_id)
                .cloned()
            else {
                // A timeout/failure for an already-superseded action is stale.
                return Ok(0);
            };
            let Some(result_index) = execution
                .action_results
                .iter()
                .position(|result| result.action_id == failure.action_id)
            else {
                // A timeout/failure for an already-superseded result is stale.
                return Ok(0);
            };
            if execution.action_results[result_index].status != ActionStatus::Running {
                None
            } else {
                let plan = local_action_plan(&action)?.ok_or_else(|| {
                    MezError::invalid_state(
                        "shell transaction failure does not match shell-backed action payload",
                    )
                })?;
                let structured_content = mez_agent::shell_action_structured_content_json(
                    &action,
                    &plan,
                    Some("pane_shell"),
                    failure.sent_to_pane,
                    serde_json::Value::Null,
                    &[],
                    failure.terminal_observation.clone(),
                );
                let mut result = ActionResult::failed(
                    &turn,
                    &action,
                    failure.status,
                    failure.code.clone(),
                    failure.message.clone(),
                )?;
                result.structured_content_json = Some(structured_content);
                execution.action_results[result_index] = result;
                execution.terminal_state = runtime_agent_turn_state_from_action_results(
                    &execution.action_results,
                    execution.final_turn,
                );
                let observed_result = execution.action_results[result_index].clone();
                let terminal_state = execution.terminal_state;
                let transition_trace = format!(
                    "action {} {} reason={} terminal_state={}",
                    failure.action_id,
                    runtime_action_status_name(failure.status),
                    failure.trace_reason,
                    runtime_agent_turn_state_name(terminal_state)
                );
                Some((
                    execution.clone(),
                    observed_result,
                    terminal_state,
                    transition_trace,
                ))
            }
        };
        let Some((mut execution, observed_result, mut terminal_state, transition_trace)) =
            maybe_failure
        else {
            return Ok(0);
        };

        self.append_agent_trace_turn_event(&turn.pane_id, &turn.turn_id, &transition_trace)?;
        self.append_agent_trace_maap_action_results(
            &turn.pane_id,
            &turn.turn_id,
            "shell_transaction_failure_action_result",
            std::slice::from_ref(&observed_result),
        )?;
        self.record_runtime_agent_patch_results_for_turn(&turn, &execution);
        self.append_agent_error_text_to_terminal_buffer(
            &turn.pane_id,
            &format!("agent: {}", failure.message),
        )?;
        self.present_agent_action_outcomes_to_terminal_buffer(&turn.pane_id, &execution)?;
        self.append_runtime_agent_execution_failure_audit(&turn, &execution)?;
        let transcript_entries = if self.queue_agent_failure_feedback_for_correction(
            &turn,
            &mut execution,
            "shell_transaction_runtime_failure",
        )? {
            self.agent_turn_executions_mut().remove(&turn.turn_id);
            terminal_state = AgentTurnState::Running;
            0
        } else {
            let transcript_entries =
                self.persist_runtime_agent_turn_execution_transcript(&turn, &execution)?;
            self.emit_subagent_task_result_for_execution(&turn, &execution)?;
            self.complete_running_agent_turn_and_start_ready(
                &turn,
                terminal_state,
                &failure.trace_reason,
            )?;
            transcript_entries
        };
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","shell_transaction":"failed","marker":"{}","reason":"{}","transcript_entries":{}}}"#,
                json_escape(&turn.pane_id),
                json_escape(&turn.turn_id),
                runtime_agent_turn_state_name(terminal_state),
                json_escape(marker),
                json_escape(&failure.trace_reason),
                transcript_entries
            ),
        )?;
        self.set_agent_prompt_display_lines_if_pane_present(
            &turn.pane_id,
            runtime_agent_execution_prompt_display_lines(
                &turn.turn_id,
                &execution.response.provider,
                &execution,
                0,
                transcript_entries,
            ),
        )?;
        Ok(1)
    }

    /// Records provider-style audit metadata for an execution that failed after
    /// the provider response was accepted by the runtime.
    pub(in crate::runtime) fn append_runtime_agent_execution_failure_audit(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let Some(model_profile) = self
            .agent
            .agent_turn_model_profiles
            .get(&turn.turn_id)
            .cloned()
        else {
            return Ok(());
        };
        let error = runtime_agent_execution_failure_error(execution);
        let provider_id = model_profile.provider.clone();
        self.append_provider_request_failure_audit(turn, &model_profile, &provider_id, &error)
    }

    /// Runs the fail agent turn for hook block operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn fail_agent_turn_for_hook_block(
        &mut self,
        turn: &AgentTurnRecord,
        model_profile: &ModelProfile,
        block: &RuntimeHookPipelineBlock,
    ) -> Result<()> {
        self.refresh_agent_turn_project_guidance_context(turn)?;
        let context = self
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let request = assemble_model_request(model_profile, turn, &context)?;
        let execution = AgentTurnExecution {
            request,
            response: ModelResponse {
                provider: "runtime".to_string(),
                model: model_profile.model.clone(),
                raw_text: format!(
                    "hook_blocked: hook_id={} event={} message={}",
                    block.hook_id,
                    runtime_hook_event_name(block.event),
                    block.message
                ),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: Vec::new(),
            final_turn: true,
            terminal_state: AgentTurnState::Failed,
        };
        let transcript_entries =
            self.persist_runtime_agent_turn_execution_transcript(turn, &execution)?;
        self.emit_subagent_task_result_for_execution(turn, &execution)?;
        self.complete_running_agent_turn_and_start_ready(
            turn,
            AgentTurnState::Failed,
            "hook_blocked",
        )?;
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"failed","error":"hook_blocked","hook_id":"{}","transcript_entries":{}}}"#,
                json_escape(&turn.pane_id),
                json_escape(&turn.turn_id),
                json_escape(&block.hook_id),
                transcript_entries
            ),
        )?;
        self.set_agent_prompt_display_lines_if_pane_present(
            &turn.pane_id,
            runtime_agent_execution_prompt_display_lines(
                &turn.turn_id,
                "runtime",
                &execution,
                0,
                transcript_entries,
            ),
        )?;
        Ok(())
    }
}
