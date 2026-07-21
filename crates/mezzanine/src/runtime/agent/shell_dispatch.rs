//! Runtime agent shell-action dispatch helpers.
//!
//! This module owns pending shell dispatch detection, readiness/hook waiting,
//! shell action loop guards, apply-patch follow-up dispatch, and conversion of
//! shell dispatch failures into normal action results. It keeps pane-shell
//! execution orchestration out of the runtime agent facade while the low-level
//! pane transaction writer remains in the facade for now.

use super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentTurnExecution,
    AgentTurnRecord, AgentTurnState, ApplyPatchTransactionPhase, BTreeSet, EventKind, HookEvent,
    MezError, PaneReadinessState, PendingFocusedShellHookContinuation, Result,
    RunningShellTransactionKind, RunningShellTransactionRef, RuntimeApplyPatchBatchState,
    RuntimeHookPipelineBlock, RuntimeHookPipelineDecision, RuntimeSessionService,
    apply_patch_error_plan, apply_patch_read_plan_for_paths, apply_patch_touched_paths,
    apply_patch_transaction_phase, apply_patch_write_plan_from_read_output,
    apply_patch_write_plan_from_read_outputs, decode_shell_output_transport_with_diagnostics,
    json_escape, local_action_plan, runtime_action_result_is_suppressed_duplicate_file_mutation,
    runtime_agent_action_rejects_duplicate_success, runtime_agent_context_command,
    runtime_agent_execution_prompt_display_lines, runtime_agent_terminal_preview,
    runtime_agent_turn_state_from_action_results, runtime_agent_turn_state_name,
    runtime_mezzanine_error_code, runtime_pane_readiness_state_name,
    runtime_pre_shell_hook_payload,
};
use crate::runtime::RuntimeSandboxFallbackAudit;

impl RuntimeSessionService {
    /// Appends one transported read chunk to an active apply-patch batch.
    pub(crate) fn append_apply_patch_batch_transport(
        &mut self,
        state_key: &str,
        transport_chunk: &[u8],
    ) {
        if let Some(state) = self.agent.apply_patch_batch_states.get_mut(state_key) {
            state
                .current_read_transport
                .extend_from_slice(transport_chunk);
        }
    }

    /// Builds the state key for one batched shell-backed `apply_patch` action.
    pub(crate) fn apply_patch_batch_state_key(turn_id: &str, action_id: &str) -> String {
        format!("{turn_id}/{action_id}")
    }

    /// Replaces the initial shell-backed `apply_patch` read plan with the next
    /// one-path batch read plan.
    fn prepare_apply_patch_batched_read_plan(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        plan: &mut mez_agent::LocalActionPlan,
    ) -> Result<()> {
        let AgentActionPayload::ApplyPatch { patch, .. } = &action.payload else {
            return Ok(());
        };
        let key = Self::apply_patch_batch_state_key(&turn.turn_id, &action.id);
        if !self.agent.apply_patch_batch_states.contains_key(&key) {
            self.agent.apply_patch_batch_states.insert(
                key.clone(),
                RuntimeApplyPatchBatchState {
                    remaining_paths: apply_patch_touched_paths(patch)?,
                    current_path: None,
                    current_read_transport: Vec::new(),
                    read_outputs: Vec::new(),
                },
            );
        }
        if let Some(state) = self.agent.apply_patch_batch_states.get_mut(&key)
            && !state.remaining_paths.is_empty()
        {
            let path = state.remaining_paths.remove(0);
            let mut paths = BTreeSet::new();
            paths.insert(path.clone());
            state.current_path = Some(path);
            *plan = apply_patch_read_plan_for_paths(&paths);
        }
        Ok(())
    }

    /// Runs the execution has pending shell dispatch operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn execution_has_pending_shell_dispatch(
        &self,
        turn_id: &str,
        execution: &AgentTurnExecution,
    ) -> bool {
        if self.agent.sandbox_failure_assessments.contains_key(turn_id) {
            return false;
        }
        let batch = execution.response.action_batch.as_ref();
        execution.terminal_state == AgentTurnState::Running
            && execution.action_results.iter().any(|result| {
                let local_shell_backed = batch
                    .and_then(|batch| {
                        batch
                            .actions
                            .iter()
                            .find(|action| action.id == result.action_id)
                    })
                    .and_then(|action| local_action_plan(action).ok().flatten())
                    .is_some();
                result.status == ActionStatus::Running
                    && local_shell_backed
                    && !self.agent_action_has_pending_pre_shell_hook(turn_id, &result.action_id)
                    && !self.agent_action_has_running_shell_transaction(turn_id, &result.action_id)
            })
    }

    /// Runs the agent action has pending pre shell hook operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn agent_action_has_pending_pre_shell_hook(
        &self,
        turn_id: &str,
        action_id: &str,
    ) -> bool {
        self.integration
            .focused_shell_hook_transactions()
            .values()
            .any(|pending| {
                pending.continuation.as_ref().is_some_and(|continuation| {
                    continuation.turn_id == turn_id && continuation.action_id == action_id
                })
            })
    }

    /// Runs the turn has running readiness probe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn turn_has_running_readiness_probe(&self, turn_id: &str) -> bool {
        self.turn_has_running_shell_transaction_kind(
            turn_id,
            &RunningShellTransactionKind::ReadinessProbe,
        )
    }

    /// Returns a local result when a shell-backed mutation has already
    /// succeeded with the exact same generated command in the current turn.
    ///
    /// This intentionally does not cap the number of shell dispatches in a
    /// turn. Failed shell commands are model-visible results, and large but
    /// finite inspection batches should be allowed to run.
    fn shell_dispatch_loop_guard_failure(
        &self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        command: &str,
    ) -> Result<Option<ActionResult>> {
        let history = self
            .agent
            .agent_turn_shell_dispatch_history
            .get(&turn.turn_id)
            .cloned()
            .unwrap_or_default();
        let dispatched_count = history.dispatched_count();
        let successful_duplicate_count = history.exact_success_count(command);
        if runtime_agent_action_rejects_duplicate_success(action) && successful_duplicate_count > 0
        {
            let context_command = runtime_agent_context_command(action, command);
            return Ok(Some(ActionResult::succeeded(
                turn,
                action,
                vec![
                    "duplicate file mutation skipped because the same mutation already succeeded"
                        .to_string(),
                ],
                Some(format!(
                    r#"{{"guard":"shell_dispatch_loop","reason":"repeated_successful_file_mutation","command":"{}","dispatch_count":{},"successful_duplicate_count":{}}}"#,
                    json_escape(&context_command),
                    dispatched_count,
                    successful_duplicate_count
                )),
            )));
        }
        Ok(None)
    }

    /// Runs the record shell dispatch history operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn record_shell_dispatch_history(&mut self, turn_id: &str, command: &str) {
        self.agent
            .agent_turn_shell_dispatch_history
            .entry(turn_id.to_string())
            .or_default()
            .record(command.to_string());
    }

    /// Records a shell command that exited successfully for loop detection and
    /// mutation/validation phase tracking.
    pub(crate) fn record_shell_dispatch_success(&mut self, turn_id: &str, command: &str) {
        self.agent
            .agent_turn_shell_dispatch_history
            .entry(turn_id.to_string())
            .or_default()
            .record_success(command.to_string());
    }

    /// Keeps the network action dispatch boundary symmetrical with shell
    /// actions without enforcing a count-based per-turn cap.
    pub(super) fn network_action_loop_guard_failure(
        &self,
        _turn: &AgentTurnRecord,
        _action: &AgentAction,
        _request: &str,
    ) -> Result<Option<ActionResult>> {
        Ok(None)
    }

    /// Records a runtime-owned network request for loop detection.
    pub(super) fn record_network_action_history(&mut self, turn_id: &str, request: &str) {
        self.agent
            .agent_turn_network_action_history
            .entry(turn_id.to_string())
            .or_default()
            .record(request.to_string());
    }

    /// Runs the dispatch stored running shell actions operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn dispatch_stored_running_shell_actions(
        &mut self,
        turn_id: &str,
    ) -> Result<Option<AgentTurnExecution>> {
        let Some(mut execution) = self.agent_turn_executions().get(turn_id).cloned() else {
            return Ok(None);
        };
        if !self.execution_has_pending_shell_dispatch(turn_id, &execution) {
            return Ok(None);
        }
        let turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            "pending_shell_dispatch resume started",
        )?;
        let dispatched = self.dispatch_running_shell_actions_to_panes(&turn, &mut execution)?;
        self.record_runtime_agent_patch_results_for_turn(&turn, &execution);
        let mut terminal_state = execution.terminal_state;
        let mut transcript_entries = 0usize;
        if terminal_state == AgentTurnState::Blocked {
            self.apply_permission_request_hooks_for_execution(&turn, &mut execution)?;
            terminal_state = execution.terminal_state;
        }
        if matches!(
            terminal_state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            let failure_feedback_queued = if terminal_state == AgentTurnState::Failed {
                self.append_runtime_agent_execution_failure_audit(&turn, &execution)?;
                self.queue_agent_failure_feedback_for_correction(
                    &turn,
                    &mut execution,
                    "pending_shell_dispatch_failed_action",
                )?
            } else {
                false
            };
            if failure_feedback_queued {
                self.agent_turn_executions_mut().remove(turn_id);
                terminal_state = AgentTurnState::Running;
            } else {
                transcript_entries =
                    self.persist_runtime_agent_turn_execution_transcript(&turn, &execution)?;
                self.emit_subagent_task_result_for_execution(&turn, &execution)?;
                self.complete_running_agent_turn_and_start_ready(
                    &turn,
                    terminal_state,
                    "pending_shell_dispatch_settled",
                )?;
            }
        } else if terminal_state == AgentTurnState::Blocked {
            transcript_entries =
                self.persist_runtime_agent_turn_execution_transcript(&turn, &execution)?;
            self.queue_blocked_approvals_for_execution(&turn, &execution)?;
            self.agent_turn_executions_mut()
                .insert(turn_id.to_string(), execution.clone());
            let _ = self.agent.agent_scheduler.block_running(turn_id);
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            self.agent_turn_ledger_mut()
                .finish_turn(turn_id, AgentTurnState::Blocked)?;
            self.append_agent_trace_turn_transition(
                &turn,
                turn.state,
                AgentTurnState::Blocked,
                "bubblewrap_preparation_fallback_approval",
            )?;
            self.start_ready_agent_turns()?;
        } else {
            self.agent_turn_executions_mut()
                .insert(turn_id.to_string(), execution.clone());
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                &format!(
                    "pending_shell_dispatch stored state={} dispatched={}",
                    runtime_agent_turn_state_name(terminal_state),
                    dispatched
                ),
            )?;
        }
        self.present_agent_action_outcomes_to_terminal_buffer(&turn.pane_id, &execution)?;
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","pending_shell_dispatch":true,"shell_actions_dispatched":{},"transcript_entries":{}}}"#,
                json_escape(&turn.pane_id),
                json_escape(turn_id),
                runtime_agent_turn_state_name(terminal_state),
                dispatched,
                transcript_entries
            ),
        )?;
        self.set_agent_prompt_display_lines_if_pane_present(
            &turn.pane_id,
            runtime_agent_execution_prompt_display_lines(
                turn_id,
                &execution.response.provider,
                &execution,
                dispatched,
                transcript_entries,
            ),
        )?;
        Ok(Some(execution))
    }

    /// Runs the fail pending shell action for hook block operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn fail_pending_shell_action_for_hook_block(
        &mut self,
        continuation: &PendingFocusedShellHookContinuation,
        block: &RuntimeHookPipelineBlock,
    ) -> Result<usize> {
        let Some(turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == continuation.turn_id)
            .cloned()
        else {
            return Ok(0);
        };
        let Some(mut execution) = self
            .agent_turn_executions()
            .get(&continuation.turn_id)
            .cloned()
        else {
            return Ok(0);
        };
        let batch = execution.response.action_batch.as_ref().ok_or_else(|| {
            MezError::invalid_state("running agent execution has no action batch")
        })?;
        let action = batch
            .actions
            .iter()
            .find(|action| action.id == continuation.action_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("hook continuation action is unavailable"))?;
        let result_index = execution
            .action_results
            .iter()
            .position(|result| result.action_id == continuation.action_id)
            .ok_or_else(|| MezError::invalid_state("hook continuation result is unavailable"))?;
        if execution.action_results[result_index].status != ActionStatus::Running {
            return Ok(0);
        }
        let mut blocked = ActionResult::failed(
            &turn,
            &action,
            ActionStatus::Denied,
            "hook_blocked",
            block.message.clone(),
        )?;
        blocked.structured_content_json = Some(block.structured_json());
        execution.action_results[result_index] = blocked.clone();
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        self.agent_turn_executions_mut()
            .insert(continuation.turn_id.clone(), execution.clone());
        self.append_agent_error_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent: shell command blocked by hook {}: {}",
                block.hook_id, block.message
            ),
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "action {} denied reason=pre_shell_hook hook={}",
                action.id, block.hook_id
            ),
        )?;
        self.present_agent_action_outcomes_to_terminal_buffer(&turn.pane_id, &execution)?;
        if matches!(
            execution.terminal_state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            let transcript_entries =
                self.persist_runtime_agent_turn_execution_transcript(&turn, &execution)?;
            self.emit_subagent_task_result_for_execution(&turn, &execution)?;
            self.complete_running_agent_turn_and_start_ready(
                &turn,
                execution.terminal_state,
                "pre_shell_hook_blocked",
            )?;
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","hook_blocked":true,"hook_id":"{}","transcript_entries":{}}}"#,
                    json_escape(&turn.pane_id),
                    json_escape(&turn.turn_id),
                    runtime_agent_turn_state_name(execution.terminal_state),
                    json_escape(&block.hook_id),
                    transcript_entries
                ),
            )?;
        }
        Ok(1)
    }

    /// Runs the dispatch running shell actions to panes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_running_shell_actions_to_panes(
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
        let mut dispatched = 0usize;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Running {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .ok_or_else(|| {
                    MezError::invalid_state("running shell result does not match an action")
                })?;
            if matches!(action.payload, AgentActionPayload::ApplyPatch { .. })
                && let Some(loop_turn) = self.agent.agent_loop_turns.get(&turn.turn_id)
                && let Some(state) = self.agent.agent_loops_by_id.get_mut(&loop_turn.loop_id)
            {
                state.emitted_apply_patch = true;
            }
            let mut plan = match local_action_plan(action) {
                Ok(Some(plan)) => plan,
                Ok(None) => continue,
                Err(error) => {
                    let error = MezError::from(error);
                    let command = match &action.payload {
                        AgentActionPayload::ShellCommand { command, .. } => command.as_str(),
                        _ => "",
                    };
                    execution.action_results[index] = self.shell_action_runtime_error_result(
                        turn,
                        action,
                        command,
                        "local_action_plan",
                        &error,
                    )?;
                    continue;
                }
            };
            if matches!(action.payload, AgentActionPayload::ApplyPatch { .. }) {
                self.prepare_apply_patch_batched_read_plan(turn, action, &mut plan)?;
            }
            let command = plan.command.as_str();
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "action {} type={} readiness={}",
                    action.id,
                    action.action_type(),
                    runtime_pane_readiness_state_name(self.pane_readiness_state(&turn.pane_id))
                ),
            )?;
            if let Some(result) = self.shell_dispatch_loop_guard_failure(turn, action, command)? {
                let suppressed_duplicate =
                    runtime_action_result_is_suppressed_duplicate_file_mutation(&result);
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    &format!(
                        "action {} {} reason=shell_dispatch_loop_guard",
                        action.id,
                        if suppressed_duplicate {
                            "succeeded"
                        } else {
                            "failed"
                        }
                    ),
                )?;
                if suppressed_duplicate {
                    self.append_action_result_context_if_absent(&turn.turn_id, &result)?;
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "action {} continuing turn reason=duplicate_successful_file_mutation",
                            action.id
                        ),
                    )?;
                } else {
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "action {} failed reason=shell_dispatch_loop_guard",
                            action.id
                        ),
                    )?;
                }
                execution.action_results[index] = result;
                continue;
            }
            match self.pane_readiness_state(&turn.pane_id) {
                PaneReadinessState::Ready => {}
                PaneReadinessState::Unknown
                | PaneReadinessState::PromptCandidate
                | PaneReadinessState::Degraded => {
                    if !self.turn_has_running_readiness_probe(&turn.turn_id) {
                        let status = if self.agent_verbose_enabled(&turn.pane_id)
                            || self.agent_trace_enabled(&turn.pane_id)
                        {
                            format!(
                                "agent: shell command waiting for shell readiness: {}",
                                runtime_agent_terminal_preview(command)
                            )
                        } else {
                            "agent: shell command waiting for shell readiness".to_string()
                        };
                        self.append_agent_status_text_to_terminal_buffer(&turn.pane_id, &status)?;
                        if let Err(error) = self.dispatch_readiness_probe_to_pane(turn) {
                            execution.action_results[index] = self
                                .shell_action_runtime_error_result(
                                    turn,
                                    action,
                                    command,
                                    "readiness_probe_dispatch",
                                    &error,
                                )?;
                            continue;
                        }
                        self.append_agent_trace_turn_event(
                            &turn.pane_id,
                            &turn.turn_id,
                            &format!("action {} waiting reason=readiness_probe_sent", action.id),
                        )?;
                    } else {
                        self.append_agent_trace_turn_event(
                            &turn.pane_id,
                            &turn.turn_id,
                            &format!(
                                "action {} waiting reason=readiness_probe_already_running",
                                action.id
                            ),
                        )?;
                    }
                    self.integration
                        .runtime_metrics_mut()
                        .record_shell_action_batch(dispatched);
                    return Ok(dispatched);
                }
                PaneReadinessState::Busy => {
                    match self.pane_foreground_primary_shell_state(&turn.pane_id) {
                        Some(true) => {
                            self.set_pane_readiness(
                                &turn.pane_id,
                                PaneReadinessState::PromptCandidate,
                            );
                            self.append_agent_status_text_to_terminal_buffer(
                                &turn.pane_id,
                                "agent: shell readiness looked stale; probing before pending shell command",
                            )?;
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "pane_readiness busy -> prompt-candidate reason=stale_busy_dispatch_recovery action={}",
                                    action.id
                                ),
                            )?;
                            if let Err(error) = self.dispatch_readiness_probe_to_pane(turn) {
                                execution.action_results[index] = self
                                    .shell_action_runtime_error_result(
                                        turn,
                                        action,
                                        command,
                                        "readiness_probe_dispatch",
                                        &error,
                                    )?;
                                continue;
                            }
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "action {} waiting reason=stale_busy_readiness_probe_sent",
                                    action.id
                                ),
                            )?;
                        }
                        None => {
                            self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Degraded);
                            self.append_agent_status_text_to_terminal_buffer(
                                &turn.pane_id,
                                "agent: shell readiness metadata unavailable; probing before pending shell command",
                            )?;
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "pane_readiness busy -> degraded reason=unknown_busy_dispatch_recovery action={}",
                                    action.id
                                ),
                            )?;
                            if let Err(error) = self.dispatch_readiness_probe_to_pane(turn) {
                                execution.action_results[index] = self
                                    .shell_action_runtime_error_result(
                                        turn,
                                        action,
                                        command,
                                        "readiness_probe_dispatch",
                                        &error,
                                    )?;
                                continue;
                            }
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "action {} waiting reason=unknown_busy_readiness_probe_sent",
                                    action.id
                                ),
                            )?;
                        }
                        Some(false) => {
                            let attempts = self.pending_shell_dispatch_blocked_recovery_attempts(
                                &turn.turn_id,
                                &action.id,
                            );
                            if attempts >= 3 {
                                let message = format!(
                                    "pane {} kept a non-shell foreground process active; shell command was not dispatched",
                                    turn.pane_id
                                );
                                let mut result = ActionResult::failed(
                                    turn,
                                    action,
                                    ActionStatus::Denied,
                                    "foreground_process_blocked_dispatch",
                                    message.clone(),
                                )?;
                                result.structured_content_json = Some(format!(
                                    r#"{{"state":"dispatch_blocked","reason":"foreground_process_active","attempts":{},"command":"{}"}}"#,
                                    attempts,
                                    json_escape(&runtime_agent_context_command(action, command))
                                ));
                                execution.action_results[index] = result;
                                self.clear_pending_shell_dispatch_blocked_recovery_attempt(
                                    &turn.turn_id,
                                    &action.id,
                                );
                                self.append_agent_error_text_to_terminal_buffer(
                                    &turn.pane_id,
                                    &format!("agent: {message}"),
                                )?;
                                self.append_agent_trace_turn_event(
                                    &turn.pane_id,
                                    &turn.turn_id,
                                    &format!(
                                        "action {} failed reason=foreground_process_blocked_dispatch attempts={}",
                                        action.id, attempts
                                    ),
                                )?;
                                break;
                            }
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "action {} waiting reason=pane_readiness_busy attempts={}",
                                    action.id, attempts
                                ),
                            )?;
                        }
                    }
                    self.integration
                        .runtime_metrics_mut()
                        .record_shell_action_batch(dispatched);
                    return Ok(dispatched);
                }
                PaneReadinessState::Probing => {
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "action {} waiting reason=pane_readiness_{}",
                            action.id,
                            runtime_pane_readiness_state_name(
                                self.pane_readiness_state(&turn.pane_id)
                            )
                        ),
                    )?;
                    self.integration
                        .runtime_metrics_mut()
                        .record_shell_action_batch(dispatched);
                    return Ok(dispatched);
                }
                state @ (PaneReadinessState::FullScreen
                | PaneReadinessState::PasswordPrompt
                | PaneReadinessState::InteractiveBlocked)
                    if self.pane_foreground_primary_shell_state(&turn.pane_id) == Some(true) =>
                {
                    self.set_pane_readiness(&turn.pane_id, PaneReadinessState::PromptCandidate);
                    self.append_agent_status_text_to_terminal_buffer(
                        &turn.pane_id,
                        "agent: shell interactivity block looked stale; probing before pending shell command",
                    )?;
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "pane_readiness {} -> prompt-candidate reason=stale_interactive_blocked_dispatch_recovery action={}",
                            runtime_pane_readiness_state_name(state),
                            action.id
                        ),
                    )?;
                    if !self.turn_has_running_readiness_probe(&turn.turn_id) {
                        if let Err(error) = self.dispatch_readiness_probe_to_pane(turn) {
                            execution.action_results[index] = self
                                .shell_action_runtime_error_result(
                                    turn,
                                    action,
                                    command,
                                    "readiness_probe_dispatch",
                                    &error,
                                )?;
                            continue;
                        }
                        self.append_agent_trace_turn_event(
                            &turn.pane_id,
                            &turn.turn_id,
                            &format!(
                                "action {} waiting reason=stale_interactive_blocked_readiness_probe_sent",
                                action.id
                            ),
                        )?;
                    } else {
                        self.append_agent_trace_turn_event(
                            &turn.pane_id,
                            &turn.turn_id,
                            &format!(
                                "action {} waiting reason=stale_interactive_blocked_readiness_probe_already_running",
                                action.id
                            ),
                        )?;
                    }
                    self.integration
                        .runtime_metrics_mut()
                        .record_shell_action_batch(dispatched);
                    return Ok(dispatched);
                }
                state => {
                    let message = format!(
                        "pane {} is not ready for agent shell input: {}",
                        turn.pane_id,
                        runtime_pane_readiness_state_name(state)
                    );
                    let mut result = ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        "pane_not_ready",
                        message.clone(),
                    )?;
                    result.structured_content_json = Some(format!(
                        r#"{{"state":"not_ready","readiness_state":"{}","command":"{}"}}"#,
                        runtime_pane_readiness_state_name(state),
                        json_escape(&runtime_agent_context_command(action, command))
                    ));
                    execution.action_results[index] = result;
                    self.append_agent_error_text_to_terminal_buffer(
                        &turn.pane_id,
                        &format!("agent: {message}"),
                    )?;
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "action {} failed reason=pane_not_ready readiness={}",
                            action.id,
                            runtime_pane_readiness_state_name(state)
                        ),
                    )?;
                    break;
                }
            }
            let hook_decision = self.run_configured_pre_action_hooks_with_continuation(
                HookEvent::PreShellCommand,
                &runtime_pre_shell_hook_payload(turn, action, command),
                Some(PendingFocusedShellHookContinuation {
                    turn_id: turn.turn_id.clone(),
                    action_id: action.id.clone(),
                }),
            )?;
            match hook_decision {
                RuntimeHookPipelineDecision::Continue => {}
                RuntimeHookPipelineDecision::Pending => {
                    execution.action_results[index].structured_content_json =
                        Some(mez_agent::shell_action_structured_content_json(
                            action,
                            &plan,
                            Some("pane_shell"),
                            false,
                            serde_json::json!({
                                "state": "pre_shell_hook_pending",
                                "kind": action.action_type(),
                                "action_id": action.id.as_str(),
                                "command": runtime_agent_context_command(action, command)
                            }),
                            &[],
                            serde_json::json!({"state":"pre_shell_hook_pending"}),
                        ));
                    self.append_agent_status_text_to_terminal_buffer(
                        &turn.pane_id,
                        "agent: shell command waiting for pre-action hook",
                    )?;
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!("action {} waiting reason=pre_shell_hook_pending", action.id),
                    )?;
                    self.integration
                        .runtime_metrics_mut()
                        .record_shell_action_batch(dispatched);
                    return Ok(dispatched);
                }
                RuntimeHookPipelineDecision::Block(block) => {
                    let mut blocked = ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Denied,
                        "hook_blocked",
                        block.message.clone(),
                    )?;
                    blocked.structured_content_json = Some(block.structured_json());
                    execution.action_results[index] = blocked;
                    self.append_agent_error_text_to_terminal_buffer(
                        &turn.pane_id,
                        &format!(
                            "agent: shell command blocked by hook {}: {}",
                            block.hook_id, block.message
                        ),
                    )?;
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "action {} denied reason=pre_shell_hook hook={}",
                            action.id, block.hook_id
                        ),
                    )?;
                    continue;
                }
            }
            let permission_evaluation = execution.action_results[index]
                .permission_evaluation
                .clone();
            let sandbox_bypassed =
                self.activate_sandbox_bypass_after_approval(&turn.turn_id, &action.id);
            if !sandbox_bypassed {
                match self.ensure_bubblewrap_path_resolution_for_action(
                    turn,
                    &action.id,
                    permission_evaluation.as_deref(),
                ) {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        execution.action_results[index] = self.shell_action_runtime_error_result(
                            turn,
                            action,
                            command,
                            "bubblewrap_path_resolution",
                            &error,
                        )?;
                        continue;
                    }
                }
                match self.ensure_bubblewrap_capability_for_action(turn, &action.id) {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(error) => {
                        execution.action_results[index] = self.shell_action_runtime_error_result(
                            turn,
                            action,
                            command,
                            "bubblewrap_capability_probe",
                            &error,
                        )?;
                        continue;
                    }
                }
            }
            let dispatch_outcome = self.dispatch_shell_action_to_pane(
                turn,
                action,
                super::shell_state::ShellActionDispatch {
                    command,
                    stateful: plan.stateful,
                    interactive: plan.interactive,
                    timeout_ms: plan.timeout_ms,
                    permission_evaluation: permission_evaluation.as_deref(),
                },
            );
            match dispatch_outcome {
                Ok(super::shell_state::ShellActionDispatchOutcome::Dispatched) => {
                    self.record_shell_dispatch_history(&turn.turn_id, command);
                    dispatched = dispatched.saturating_add(1);
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "action {} dispatched shell_transaction dispatched_count={}",
                            action.id, dispatched
                        ),
                    )?;
                    break;
                }
                Ok(super::shell_state::ShellActionDispatchOutcome::SandboxFallbackEligible {
                    marker,
                    proof,
                }) => {
                    self.mark_sandbox_preparation_fallback_pending(
                        turn, action, execution, index, &marker, &proof,
                    )?;
                    continue;
                }
                Err(error) => {
                    execution.action_results[index] = self.shell_action_runtime_error_result(
                        turn,
                        action,
                        command,
                        "shell_dispatch",
                        &error,
                    )?;
                    continue;
                }
            }
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        self.integration
            .runtime_metrics_mut()
            .record_shell_action_batch(dispatched);
        Ok(dispatched)
    }

    /// Marks one typed Bubblewrap preparation failure as approval-pending.
    ///
    /// The caller owns the in-flight execution, so this transition mutates
    /// that execution in place and records only the exact fallback identity.
    /// Ordinary provider or stored-dispatch settlement then persists the
    /// blocked execution and queues the approval through the shared path.
    fn mark_sandbox_preparation_fallback_pending(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        execution: &mut AgentTurnExecution,
        result_index: usize,
        marker: &str,
        proof: &str,
    ) -> Result<()> {
        let plan = local_action_plan(action)?.ok_or_else(|| {
            MezError::invalid_state("sandbox preparation fallback action is not shell-backed")
        })?;
        let evaluation = execution.action_results[result_index]
            .permission_evaluation
            .clone()
            .ok_or_else(|| {
                MezError::invalid_state(
                    "sandbox preparation fallback requires a permission evaluation",
                )
            })?;
        if evaluation.decision != mez_agent::permissions::RuleDecision::Prompt {
            return Err(MezError::invalid_state(
                "sandbox preparation fallback requires an originally prompted action",
            ));
        }
        let mut blocked = ActionResult::blocked(
            turn,
            action,
            vec![
                "Bubblewrap could not represent the approved policy requirements before payload execution"
                    .to_string(),
                "approval is required for one exact unsandboxed retry".to_string(),
            ],
            mez_agent::shell_action_structured_content_json(
                action,
                &plan,
                Some("pane_shell"),
                true,
                serde_json::json!({
                    "state": "pending",
                    "kind": action.action_type(),
                    "action_id": action.id.as_str(),
                    "command": plan.policy_command,
                    "sandbox_fallback": {
                        "backend": "bubblewrap",
                        "reason": "preparation_failure",
                        "proof": proof,
                        "payload_exec_proven": false,
                        "partial_effect_warning": false
                    }
                }),
                &evaluation.matched_rule_ids,
                serde_json::json!({
                    "source": "runtime",
                    "marker": marker,
                    "boundary_state": "preparation_failure",
                    "payload_exec_proven": false,
                    "partial_effect_warning": false
                }),
            ),
        );
        blocked.permission_evaluation = Some(evaluation);
        execution.action_results[result_index] = blocked;
        self.agent.sandbox_fallback_audits.insert(
            (turn.turn_id.clone(), action.id.clone()),
            RuntimeSandboxFallbackAudit {
                reason: "preparation_failure".to_string(),
                proof: proof.to_string(),
                partial_effect_warning: false,
                approving_client_id: None,
            },
        );
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "action {} blocked reason=bubblewrap_preparation_failure marker={marker}",
                action.id
            ),
        )?;
        Ok(())
    }

    /// Dispatches the verified write phase for a completed `apply_patch`
    /// snapshot transaction.
    ///
    /// `apply_patch` is multi-phase by design: the first shell transaction only
    /// snapshots remote file bytes, Rust applies the Mezzanine patch internally, and
    /// the second shell transaction verifies the snapshots and writes final bytes.
    /// Returning `true` means the original action remains running while the
    /// generated write transaction settles.
    ///
    /// # Parameters
    /// - `turn`: The running agent turn that owns the action.
    /// - `action_id`: The action whose read transaction completed.
    /// - `transaction`: The completed read transaction state.
    /// - `exit_code`: The shell exit status observed for the read transaction.
    pub(crate) fn dispatch_apply_patch_followup_if_needed(
        &mut self,
        turn: &AgentTurnRecord,
        action_id: &str,
        transaction: &RunningShellTransactionRef,
        exit_code: i32,
    ) -> Result<bool> {
        let state_key = Self::apply_patch_batch_state_key(&turn.turn_id, action_id);
        if exit_code != 0 {
            self.agent.apply_patch_batch_states.remove(&state_key);
            return Ok(false);
        }
        if apply_patch_transaction_phase(&transaction.command)
            != Some(ApplyPatchTransactionPhase::Read)
        {
            return Ok(false);
        }
        let execution = self
            .agent_turn_executions()
            .get(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("running agent execution is unavailable"))?;
        let batch = execution.response.action_batch.as_ref().ok_or_else(|| {
            MezError::invalid_state("running agent execution has no action batch")
        })?;
        let action = batch
            .actions
            .iter()
            .find(|action| action.id == action_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("shell transaction does not match an action"))?;
        let permission_evaluation = execution
            .action_results
            .iter()
            .find(|result| result.action_id == action_id)
            .and_then(|result| result.permission_evaluation.clone());
        let AgentActionPayload::ApplyPatch { patch, .. } = &action.payload else {
            return Ok(false);
        };
        let write_plan = if let Some(mut state) =
            self.agent.apply_patch_batch_states.remove(&state_key)
        {
            let retained_transport;
            let retained_transport = if state.current_read_transport.is_empty() {
                transaction.observed_output_preview.as_str()
            } else {
                retained_transport = String::from_utf8_lossy(&state.current_read_transport);
                retained_transport.as_ref()
            };
            let decoded_output = decode_shell_output_transport_with_diagnostics(retained_transport);
            if (state.current_read_transport.is_empty() && transaction.observed_output_truncated)
                || decoded_output.diagnostics.transport_incomplete()
                || decoded_output.diagnostics.output_truncated()
            {
                apply_patch_error_plan(
                    "apply_patch read phase output was truncated or transport-incomplete before Rust could build the write phase",
                )
            } else {
                state.read_outputs.push(decoded_output.output);
                state.current_read_transport.clear();
                state.current_path = None;
                if !state.remaining_paths.is_empty() {
                    let path = state.remaining_paths.remove(0);
                    let mut paths = BTreeSet::new();
                    paths.insert(path.clone());
                    state.current_path = Some(path);
                    let read_plan = apply_patch_read_plan_for_paths(&paths);
                    self.agent.apply_patch_batch_states.insert(state_key, state);
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "action {} apply_patch_phase=read reason=next_batch_read",
                            action.id
                        ),
                    )?;
                    self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Ready);
                    self.dispatch_shell_action_to_pane(
                        turn,
                        &action,
                        super::shell_state::ShellActionDispatch {
                            command: &read_plan.command,
                            stateful: read_plan.stateful,
                            interactive: read_plan.interactive,
                            timeout_ms: read_plan.timeout_ms,
                            permission_evaluation: permission_evaluation.as_deref(),
                        },
                    )?;
                    return Ok(true);
                }
                apply_patch_write_plan_from_read_outputs(patch, &state.read_outputs)
                    .unwrap_or_else(|error| apply_patch_error_plan(error.message()))
            }
        } else {
            let decoded_output = decode_shell_output_transport_with_diagnostics(
                &transaction.observed_output_preview,
            );
            if transaction.observed_output_truncated
                || decoded_output.diagnostics.transport_incomplete()
                || decoded_output.diagnostics.output_truncated()
            {
                apply_patch_error_plan(
                    "apply_patch read phase output was truncated or transport-incomplete before Rust could build the write phase",
                )
            } else {
                apply_patch_write_plan_from_read_output(patch, &decoded_output.output)
                    .unwrap_or_else(|error| apply_patch_error_plan(error.message()))
            }
        };

        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "action {} apply_patch_phase=write reason=read_phase_completed",
                action.id
            ),
        )?;
        self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Ready);
        self.dispatch_shell_action_to_pane(
            turn,
            &action,
            super::shell_state::ShellActionDispatch {
                command: &write_plan.command,
                stateful: write_plan.stateful,
                interactive: write_plan.interactive,
                timeout_ms: write_plan.timeout_ms,
                permission_evaluation: permission_evaluation.as_deref(),
            },
        )?;
        Ok(true)
    }

    /// Converts a local shell dispatch failure into a normal agent action
    /// result instead of allowing the async provider service to fail upward.
    ///
    /// Runtime shell dispatch sits after provider completion, so pane I/O,
    /// readiness-probe, or terminal-presentation failures are actionable agent
    /// failures rather than daemon supervision failures. The returned result is
    /// structured for transcript/audit/debug consumers, and the best-effort pane
    /// log keeps the active user informed when the pane still exists.
    fn shell_action_runtime_error_result(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        command: &str,
        stage: &str,
        error: &MezError,
    ) -> Result<ActionResult> {
        self.append_sandbox_fallback_result_audit(&turn.turn_id, &action.id, "failed")?;
        self.clear_sandbox_bypass_for_action(&turn.turn_id, &action.id);
        let error_kind = runtime_mezzanine_error_code(error.kind());
        let error_message = format!("{stage}: {}", error.message());
        let mut result = ActionResult::failed(
            turn,
            action,
            ActionStatus::Failed,
            error_kind,
            error_message.clone(),
        )?;
        let execution_transport = "pane_shell";
        let plan = local_action_plan(action)?.ok_or_else(|| {
            MezError::invalid_state("shell dispatch failure requires a shell-backed action")
        })?;
        result.structured_content_json = Some(mez_agent::shell_action_structured_content_json(
            action,
            &plan,
            Some(execution_transport),
            false,
            serde_json::Value::Null,
            &[],
            serde_json::json!({
                "state": "dispatch_failed",
                "stage": stage,
                "command": runtime_agent_context_command(action, command),
                "error": {
                    "kind": error_kind,
                    "message": error.message()
                }
            }),
        ));
        let _ = self.append_agent_error_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent: shell command failed before execution: {}",
                error.message()
            ),
        );
        let _ = self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "action {} failed reason={} error_kind={} message={}",
                action.id,
                stage,
                error_kind,
                error.message()
            ),
        );
        let _ = self.append_agent_shell_command_audit(turn, action, command, None, None, "failed");
        Ok(result)
    }
}
