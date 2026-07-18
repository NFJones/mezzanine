//! Asynchronous provider result application for runtime completion events.

use super::super::outcome::RuntimeTerminalActionObservations;
use super::super::{
    AgentTurnExecution, AgentTurnRecord, AgentTurnState, EventKind, ModelProfile,
    ModelTokenUsageKey, Result, RuntimeSessionService, TaskState, json_escape,
    runtime_agent_execution_failure_error, runtime_agent_execution_prompt_display_lines,
    runtime_agent_turn_state_name, runtime_execution_ready_for_provider_continuation,
};

impl RuntimeSessionService {
    /// Runs the apply agent provider execution async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn apply_agent_provider_execution_async(
        &mut self,
        turn: &AgentTurnRecord,
        model_profile: &ModelProfile,
        provider_id: &str,
        mut execution: AgentTurnExecution,
    ) -> Result<AgentTurnExecution> {
        let turn_id = turn.turn_id.as_str();
        self.append_provider_request_audit(turn, model_profile, provider_id, "succeeded")?;
        let response_action_count = execution
            .response
            .action_batch
            .as_ref()
            .map(|batch| batch.actions.len())
            .unwrap_or(0);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "provider_response received provider={} terminal_state={} action_count={} final={}",
                provider_id,
                runtime_agent_turn_state_name(execution.terminal_state),
                response_action_count,
                execution.final_turn
            ),
        )?;
        let token_usage_key =
            ModelTokenUsageKey::new(model_profile.provider.clone(), model_profile.model.clone());
        for (key, usage) in &execution.routing_token_usage_by_model {
            self.integration
                .runtime_metrics_mut()
                .record_provider_token_usage(*usage, *usage, key);
        }
        self.record_agent_provider_token_usage_by_model(
            &turn.pane_id,
            &execution.routing_token_usage_by_model,
        );
        self.integration
            .runtime_metrics_mut()
            .record_provider_response(
                &execution.response,
                execution.latest_response_usage,
                &token_usage_key,
            );
        self.record_agent_provider_token_usage_with_profile(
            &turn.pane_id,
            execution.response.usage,
            execution.latest_response_usage,
            Some(model_profile),
        );
        self.record_agent_provider_quota_usage(&turn.pane_id, &execution.response.quota_usage);
        self.append_agent_trace_maap_response(
            turn,
            &execution.response,
            execution.latest_response_usage,
        )?;
        self.suppress_redundant_rationale_entries(turn, &mut execution)?;
        self.present_agent_response_actions_to_terminal_buffer(&turn.pane_id, &execution)?;
        self.append_agent_execution_assistant_context(turn, &execution)?;
        self.record_agent_copy_output(turn, &execution);
        let mut terminal_observations = RuntimeTerminalActionObservations::default();
        terminal_observations.observe(&execution);
        let skill_actions_executed =
            self.execute_running_skill_actions_for_turn(turn, &mut execution)?;
        terminal_observations.observe(&execution);
        let message_actions_executed =
            self.execute_running_message_actions_for_turn(turn, &mut execution)?;
        terminal_observations.observe(&execution);
        let network_actions_executed = self
            .execute_running_network_actions_for_turn_async(turn, &mut execution)
            .await?;
        terminal_observations.observe(&execution);
        let mcp_actions_executed = self
            .execute_running_mcp_actions_for_turn_async(turn, &mut execution)
            .await?;
        terminal_observations.observe(&execution);
        let spawn_actions_executed =
            self.execute_running_spawn_actions_for_turn(turn, &mut execution)?;
        terminal_observations.observe(&execution);
        let config_actions_executed =
            self.execute_running_config_change_actions_for_turn(turn, &mut execution)?;
        terminal_observations.observe(&execution);
        let memory_actions_executed =
            self.execute_running_memory_actions_for_turn(turn, &mut execution)?;
        terminal_observations.observe(&execution);
        let _issue_actions_executed =
            self.execute_running_issue_actions_for_turn(turn, &mut execution)?;
        terminal_observations.observe(&execution);
        let shell_actions_dispatched =
            self.dispatch_running_shell_actions_to_panes(turn, &mut execution)?;
        terminal_observations.observe(&execution);
        if !terminal_observations.results().is_empty() {
            self.commit_settled_action_results_context(
                &turn.turn_id,
                terminal_observations.results(),
            )?;
        }
        self.append_agent_trace_maap_action_results(
            &turn.pane_id,
            &turn.turn_id,
            "action_results",
            &execution.action_results,
        )?;
        self.record_runtime_agent_patch_results_for_turn(turn, &execution);
        if execution.terminal_state == AgentTurnState::Failed {
            let error = runtime_agent_execution_failure_error(&execution);
            self.append_provider_request_failure_audit(turn, model_profile, provider_id, &error)?;
        }
        if execution.terminal_state == AgentTurnState::Blocked {
            self.apply_permission_request_hooks_for_execution(turn, &mut execution)?;
        }
        self.present_agent_action_outcomes_to_terminal_buffer(&turn.pane_id, &execution)?;
        let failure_feedback_queued = if self.routed_presentation_turn(&turn.turn_id) {
            false
        } else {
            self.queue_agent_failure_feedback_for_correction(
                turn,
                &mut execution,
                "provider_execution_failed_action",
            )?
        };
        self.present_deferred_agent_say_actions_to_terminal_buffer(&turn.pane_id, &execution)?;
        let mut persisted_transcript_entries = 0usize;
        if failure_feedback_queued {
            self.agent_turn_executions_mut().remove(turn_id);
        } else if execution.terminal_state == AgentTurnState::Blocked {
            persisted_transcript_entries =
                self.persist_runtime_agent_turn_execution_transcript(turn, &execution)?;
            self.queue_blocked_approvals_for_execution(turn, &execution)?;
            self.agent_turn_executions_mut()
                .insert(turn_id.to_string(), execution.clone());
            let _ = self.agent.agent_scheduler.block_running(turn_id);
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                "scheduler running -> blocked reason=approval_required",
            )?;
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                "provider_task removed reason=blocked_waiting_approval",
            )?;
            self.agent_turn_ledger_mut()
                .finish_turn(turn_id, AgentTurnState::Blocked)?;
            self.append_agent_trace_turn_transition(
                turn,
                turn.state,
                AgentTurnState::Blocked,
                "approval_required",
            )?;
            self.emit_subagent_task_status(
                turn,
                TaskState::Blocked,
                None,
                "subagent task blocked pending approval",
            )?;
            self.start_ready_agent_turns()?;
        } else if execution.terminal_state != AgentTurnState::Running {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                &format!(
                    "provider_execution terminal_state={} reason=action_results_settled",
                    runtime_agent_turn_state_name(execution.terminal_state)
                ),
            )?;
            persisted_transcript_entries =
                self.persist_runtime_agent_turn_execution_transcript(turn, &execution)?;
            self.emit_subagent_task_result_for_execution(turn, &execution)?;
            if !self.complete_routed_presentation(turn_id, execution.terminal_state)? {
                self.complete_running_agent_turn_and_start_ready(
                    turn,
                    execution.terminal_state,
                    "provider_execution_settled",
                )?;
            }
        } else {
            let waiting_for_joined_subagents =
                self.execution_waiting_for_live_joined_subagents(turn_id, &execution);
            if waiting_for_joined_subagents {
                self.agent_turn_executions_mut()
                    .insert(turn_id.to_string(), execution.clone());
                self.agent.agent_scheduler.wait_running(turn_id)?;
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    turn_id,
                    "scheduler running -> waiting reason=waiting_for_subagents capacity=released",
                )?;
                self.agent.pending_agent_provider_tasks.remove(turn_id);
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    turn_id,
                    "provider_task removed reason=waiting_for_subagents",
                )?;
                self.agent_turn_ledger_mut()
                    .finish_turn(turn_id, AgentTurnState::Blocked)?;
                self.append_agent_trace_turn_transition(
                    turn,
                    turn.state,
                    AgentTurnState::Blocked,
                    "waiting_for_subagents",
                )?;
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    "agent: waiting for subagents to finish",
                )?;
                self.emit_subagent_task_status(
                    turn,
                    TaskState::Blocked,
                    None,
                    "subagent task waiting for child subagents",
                )?;
                self.start_ready_agent_turns()?;
            } else if runtime_execution_ready_for_provider_continuation(&execution) {
                self.agent
                    .pending_agent_provider_tasks
                    .insert(turn_id.to_string());
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    turn_id,
                    "provider_task queued reason=ready_for_provider_continuation",
                )?;
            }
            if !waiting_for_joined_subagents {
                self.agent_turn_executions_mut()
                    .insert(turn_id.to_string(), execution.clone());
            }
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                &format!(
                    "execution stored state=running pending_shell_dispatch={} ready_for_provider_continuation={}",
                    self.execution_has_pending_shell_dispatch(turn_id, &execution),
                    runtime_execution_ready_for_provider_continuation(&execution)
                ),
            )?;
        }
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","provider":"{}","action_results":{},"shell_actions_dispatched":{},"transcript_entries":{}}}"#,
                json_escape(&turn.pane_id),
                json_escape(turn_id),
                runtime_agent_turn_state_name(execution.terminal_state),
                json_escape(provider_id),
                execution.action_results.len(),
                shell_actions_dispatched
                    .saturating_add(mcp_actions_executed)
                    .saturating_add(skill_actions_executed)
                    .saturating_add(network_actions_executed)
                    .saturating_add(message_actions_executed)
                    .saturating_add(spawn_actions_executed)
                    .saturating_add(config_actions_executed)
                    .saturating_add(memory_actions_executed),
                persisted_transcript_entries
            ),
        )?;
        self.set_agent_prompt_display_lines_if_pane_present(
            &turn.pane_id,
            runtime_agent_execution_prompt_display_lines(
                turn_id,
                provider_id,
                &execution,
                shell_actions_dispatched
                    .saturating_add(mcp_actions_executed)
                    .saturating_add(skill_actions_executed)
                    .saturating_add(network_actions_executed)
                    .saturating_add(message_actions_executed)
                    .saturating_add(spawn_actions_executed)
                    .saturating_add(config_actions_executed)
                    .saturating_add(memory_actions_executed),
                persisted_transcript_entries,
            ),
        )?;
        Ok(execution)
    }
}
