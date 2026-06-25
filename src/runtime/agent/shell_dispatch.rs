//! Runtime agent shell-action dispatch helpers.
//!
//! This module owns pending shell dispatch detection, readiness/hook waiting,
//! shell action loop guards, apply-patch follow-up dispatch, and conversion of
//! shell dispatch failures into normal action results. It keeps pane-shell
//! execution orchestration out of the runtime agent facade while the low-level
//! pane transaction writer remains in the facade for now.

use super::super::service_state::RuntimeAgentShellDispatchHistory;
use super::*;

/// Label for the turn-volatile context block that nudges concrete action after
/// repeated shell dispatch or successful mutation.
const RUNTIME_ACTION_PRESSURE_LABEL: &str = "action pressure";

/// Current action-pressure phase for one active turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeActionPressurePhase {
    /// Repeated shell dispatch has crossed the configured threshold.
    InspectionStreak {
        /// Consecutive `shell_command` dispatches in the current phase.
        consecutive_shell_dispatches: usize,
        /// Current staged severity for the shell-command streak.
        severity: RuntimeActionPressureSeverity,
    },
    /// A file mutation succeeded and no validation command has succeeded yet.
    MutationAwaitingValidation,
    /// A file mutation and at least one validation command have succeeded.
    MutationValidated,
}

/// Current inspection-streak severity for shell-command pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeActionPressureSeverity {
    /// Early nudge after a short shell-command streak.
    Gentle,
    /// Stronger nudge after a longer shell-command streak.
    Medium,
    /// Highest pressure once the turn has stayed in shell inspection too long.
    Strong,
}

impl RuntimeSessionService {
    /// Runs the execution has pending shell dispatch operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn execution_has_pending_shell_dispatch(
        &self,
        turn_id: &str,
        execution: &AgentTurnExecution,
    ) -> bool {
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
                    && !self.running_shell_transactions.values().any(|transaction| {
                        transaction.turn_id == turn_id
                            && matches!(
                                &transaction.kind,
                                RunningShellTransactionKind::AgentAction { action_id }
                                    if action_id == &result.action_id
                            )
                    })
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
        self.focused_shell_hook_transactions
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
    pub(in crate::runtime) fn turn_has_running_readiness_probe(&self, turn_id: &str) -> bool {
        self.running_shell_transactions.values().any(|transaction| {
            transaction.turn_id == turn_id
                && transaction.kind == RunningShellTransactionKind::ReadinessProbe
        })
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
    pub(in crate::runtime) fn record_shell_dispatch_history(
        &mut self,
        turn_id: &str,
        command: &str,
    ) {
        self.agent_turn_shell_dispatch_history
            .entry(turn_id.to_string())
            .or_default()
            .record(command.to_string());
        self.refresh_agent_action_pressure_context(turn_id);
    }

    /// Records a shell command that exited successfully for loop detection and
    /// mutation/validation phase tracking.
    pub(in crate::runtime) fn record_shell_dispatch_success(
        &mut self,
        turn_id: &str,
        command: &str,
        action: &AgentAction,
    ) {
        self.agent_turn_shell_dispatch_history
            .entry(turn_id.to_string())
            .or_default()
            .record_success(
                command.to_string(),
                action,
                runtime_shell_command_looks_like_validation(command),
            );
        self.refresh_agent_action_pressure_context(turn_id);
    }

    /// Resets the inspection streak when a provider batch takes a different
    /// runtime-visible action.
    pub(super) fn reset_action_pressure_after_non_shell_effects(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) {
        let has_non_shell_effect = execution
            .response
            .action_batch
            .as_ref()
            .is_some_and(|batch| {
                batch.actions.iter().any(|action| {
                    runtime_agent_action_has_runtime_visible_effect(action)
                        && !matches!(action.payload, AgentActionPayload::ShellCommand { .. })
                })
            });
        if !has_non_shell_effect {
            return;
        }
        if let Some(history) = self
            .agent_turn_shell_dispatch_history
            .get_mut(&turn.turn_id)
        {
            history.reset_successive_shell_commands();
        }
        self.refresh_agent_action_pressure_context(&turn.turn_id);
    }

    /// Updates the active-turn action-pressure context block.
    fn refresh_agent_action_pressure_context(&mut self, turn_id: &str) {
        let threshold = self
            .agent_implementation_pressure_after_shell_actions
            .max(1);
        let phase = self
            .agent_turn_shell_dispatch_history
            .get(turn_id)
            .and_then(|history| runtime_action_pressure_phase(history, threshold));
        let Some(context) = self.agent_turn_contexts.get_mut(turn_id) else {
            return;
        };
        context.blocks.retain(|block| {
            block.source != ContextSourceKind::RuntimeHint
                || block.label != RUNTIME_ACTION_PRESSURE_LABEL
        });
        if let Some(phase) = phase {
            context.blocks.push(ContextBlock {
                source: ContextSourceKind::RuntimeHint,
                label: RUNTIME_ACTION_PRESSURE_LABEL.to_string(),
                content: runtime_action_pressure_context_content(phase),
            });
        }
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
        self.agent_turn_network_action_history
            .entry(turn_id.to_string())
            .or_default()
            .record(request.to_string());
    }

    /// Runs the dispatch stored running shell actions operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn dispatch_stored_running_shell_actions(
        &mut self,
        turn_id: &str,
    ) -> Result<Option<AgentTurnExecution>> {
        let Some(mut execution) = self.agent_turn_executions.get(turn_id).cloned() else {
            return Ok(None);
        };
        if !self.execution_has_pending_shell_dispatch(turn_id, &execution) {
            return Ok(None);
        }
        let turn = self
            .agent_turn_ledger
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
                self.agent_turn_executions.remove(turn_id);
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
        } else {
            self.agent_turn_executions
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
    pub(in crate::runtime) fn fail_pending_shell_action_for_hook_block(
        &mut self,
        continuation: &PendingFocusedShellHookContinuation,
        block: &RuntimeHookPipelineBlock,
    ) -> Result<usize> {
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == continuation.turn_id)
            .cloned()
        else {
            return Ok(0);
        };
        let Some(mut execution) = self
            .agent_turn_executions
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
        self.agent_turn_executions
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
                && let Some(loop_turn) = self.agent_loop_turns.get(&turn.turn_id)
                && let Some(state) = self.agent_loops_by_pane.get_mut(&loop_turn.pane_id)
            {
                state.emitted_apply_patch = true;
            }
            let plan = match local_action_plan(action) {
                Ok(Some(plan)) => plan,
                Ok(None) => continue,
                Err(error) => {
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
            if self.agent_local_action_executor_for_pane(&turn.pane_id)
                == RuntimeLocalActionExecutor::Native
            {
                let marker = runtime_marker_for_action(turn, &action.id)?;
                let Some(working_directory) = self.pane_current_working_directory(&turn.pane_id)
                else {
                    let error = MezError::invalid_state(format!(
                        "native local action executor has no working directory for pane {}",
                        turn.pane_id
                    ));
                    execution.action_results[index] = self.shell_action_runtime_error_result(
                        turn,
                        action,
                        command,
                        "native_local_action_cwd",
                        &error,
                    )?;
                    continue;
                };
                if !self
                    .append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, action)?
                {
                    if matches!(action.payload, AgentActionPayload::ShellCommand { .. }) {
                        self.append_agent_command_preview_to_terminal_buffer(
                            &turn.pane_id,
                            command,
                        )?;
                    } else {
                        self.append_agent_status_text_to_terminal_buffer(
                            &turn.pane_id,
                            &runtime_agent_shell_status(action, "native local action"),
                        )?;
                    }
                }
                let output_preview_lines = self.terminal_shell_output_preview_lines;
                let progress_turn_id = turn.turn_id.clone();
                let progress_action_id = action.id.clone();
                let progress_pane_id = turn.pane_id.clone();
                let native_result = {
                    let mut native_executor = NativeShellLocalExecutor::new(
                        self.session.shell.path(),
                        &working_directory,
                    )
                    .with_output_progress(|output| {
                        if self.agent_shell_transaction_action_shows_live_output(
                            &progress_turn_id,
                            &progress_action_id,
                        ) {
                            let lines =
                                crate::runtime::processes::output_filter::latest_agent_shell_transaction_output_lines(output, output_preview_lines);
                            if !lines.is_empty() {
                                self.append_agent_shell_output_status_lines_to_terminal_buffer(
                                    &progress_pane_id,
                                    &lines,
                                )?;
                            }
                        }
                        Ok(())
                    });
                    execute_local_action(turn, action, marker, &mut native_executor)
                };
                let result = match native_result {
                    Ok(result) => result,
                    Err(error) => self.shell_action_runtime_error_result(
                        turn,
                        action,
                        command,
                        "native_local_action",
                        &error,
                    )?,
                };
                execution.action_results[index] = result;
                if self.agent_action_result_renders_in_normal_mode(action) {
                    let result_text = execution.action_results[index].content_text();
                    if !result_text.trim().is_empty() {
                        self.append_agent_action_result_text_to_terminal_buffer(
                            &turn.pane_id,
                            action,
                            &execution.action_results[index],
                            &result_text,
                        )?;
                    }
                }
                self.append_action_result_context_if_absent(
                    &turn.turn_id,
                    &execution.action_results[index],
                )?;
                dispatched = dispatched.saturating_add(1);
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    &format!(
                        "action {} executed native_local_action executed_count={}",
                        action.id, dispatched
                    ),
                )?;
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
                    self.runtime_metrics.record_shell_action_batch(dispatched);
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
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!("action {} waiting reason=pane_readiness_busy", action.id),
                            )?;
                        }
                    }
                    self.runtime_metrics.record_shell_action_batch(dispatched);
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
                    self.runtime_metrics.record_shell_action_batch(dispatched);
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
                    self.runtime_metrics.record_shell_action_batch(dispatched);
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
                        Some(shell_command_structured_content_json(
                            action,
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
                        )?);
                    self.append_agent_status_text_to_terminal_buffer(
                        &turn.pane_id,
                        "agent: shell command waiting for pre-action hook",
                    )?;
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!("action {} waiting reason=pre_shell_hook_pending", action.id),
                    )?;
                    self.runtime_metrics.record_shell_action_batch(dispatched);
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
            if let Err(error) = self.dispatch_shell_action_to_pane(
                turn,
                action,
                command,
                plan.stateful,
                plan.timeout_ms,
            ) {
                execution.action_results[index] = self.shell_action_runtime_error_result(
                    turn,
                    action,
                    command,
                    "shell_dispatch",
                    &error,
                )?;
                continue;
            }
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
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        self.runtime_metrics.record_shell_action_batch(dispatched);
        Ok(dispatched)
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
    pub(in crate::runtime) fn dispatch_apply_patch_followup_if_needed(
        &mut self,
        turn: &AgentTurnRecord,
        action_id: &str,
        transaction: &RunningShellTransactionRef,
        exit_code: i32,
    ) -> Result<bool> {
        if exit_code != 0
            || apply_patch_transaction_phase(&transaction.command)
                != Some(ApplyPatchTransactionPhase::Read)
        {
            return Ok(false);
        }
        let execution = self
            .agent_turn_executions
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
        let AgentActionPayload::ApplyPatch { patch, .. } = &action.payload else {
            return Ok(false);
        };
        let decoded_output =
            decode_shell_output_transport_with_diagnostics(&transaction.observed_output_preview);
        let write_plan = if transaction.observed_output_truncated
            || decoded_output.diagnostics.transport_incomplete()
            || decoded_output.diagnostics.output_truncated()
        {
            apply_patch_error_plan(
                "apply_patch read phase output was truncated or transport-incomplete before Rust could build the write phase",
            )
        } else {
            apply_patch_write_plan_from_read_output(patch, &decoded_output.output)
                .unwrap_or_else(|error| apply_patch_error_plan(error.message()))
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
            &write_plan.command,
            write_plan.stateful,
            write_plan.timeout_ms,
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
        let error_kind = runtime_mezzanine_error_code(error.kind());
        let error_message = format!("{stage}: {}", error.message());
        let mut result = ActionResult::failed(
            turn,
            action,
            ActionStatus::Failed,
            error_kind,
            error_message.clone(),
        )?;
        result.structured_content_json = Some(
            serde_json::json!({
                "state": "dispatch_failed",
                "stage": stage,
                "command": runtime_agent_context_command(action, command),
                "error": {
                    "kind": error_kind,
                    "message": error.message()
                }
            })
            .to_string(),
        );
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
        let _ = self.append_agent_shell_command_audit(turn, action, command, "failed");
        Ok(result)
    }
}

/// Returns the current action-pressure phase for one active turn.
fn runtime_action_pressure_phase(
    history: &RuntimeAgentShellDispatchHistory,
    threshold: usize,
) -> Option<RuntimeActionPressurePhase> {
    if history.successful_file_mutation_this_turn
        && history.successful_validation_after_file_mutation
    {
        return Some(RuntimeActionPressurePhase::MutationValidated);
    }
    if history.successful_file_mutation_this_turn {
        return Some(RuntimeActionPressurePhase::MutationAwaitingValidation);
    }
    let consecutive_shell_dispatches = history.consecutive_shell_dispatches;
    if consecutive_shell_dispatches >= threshold {
        let severity = runtime_action_pressure_severity(consecutive_shell_dispatches, threshold);
        return Some(RuntimeActionPressurePhase::InspectionStreak {
            consecutive_shell_dispatches,
            severity,
        });
    }
    None
}

/// Returns the current shell-command inspection severity.
fn runtime_action_pressure_severity(
    consecutive_shell_dispatches: usize,
    threshold: usize,
) -> RuntimeActionPressureSeverity {
    let medium_threshold = threshold.saturating_mul(2).max(6);
    let strong_threshold = threshold.saturating_mul(3).max(10);
    if consecutive_shell_dispatches >= strong_threshold {
        RuntimeActionPressureSeverity::Strong
    } else if consecutive_shell_dispatches >= medium_threshold {
        RuntimeActionPressureSeverity::Medium
    } else {
        RuntimeActionPressureSeverity::Gentle
    }
}

/// Builds the model-facing action-pressure hint for one active turn.
fn runtime_action_pressure_context_content(phase: RuntimeActionPressurePhase) -> String {
    let phase_message = match phase {
        RuntimeActionPressurePhase::InspectionStreak {
            consecutive_shell_dispatches,
            severity,
        } => {
            let severity_message = match severity {
                RuntimeActionPressureSeverity::Gentle => {
                    "Apply gentle pressure now: stop broadening discovery unless one named missing fact still blocks the next implementation, validation, or report action."
                }
                RuntimeActionPressureSeverity::Medium => {
                    "Apply medium pressure now: prefer the next implementation, focused regression test, execution-based validation, or final-report action instead of further shell discovery."
                }
                RuntimeActionPressureSeverity::Strong => {
                    "Apply strong pressure now: do not continue shell discovery without a concrete justification from recent evidence for why another shell_command is required before acting."
                }
            };
            format!(
                "This turn has already dispatched {consecutive_shell_dispatches} consecutive shell_command actions. {severity_message}"
            )
        }
        RuntimeActionPressurePhase::MutationAwaitingValidation => {
            "A file mutation has already succeeded this turn. Prefer execution-based validation, required format/build/lint/test commands, focused diff/status review, or final report now.".to_string()
        }
        RuntimeActionPressurePhase::MutationValidated => {
            "A file mutation and at least one validation command have already succeeded this turn. Run any remaining repository-required validation, commit or handoff step, or final report now.".to_string()
        }
    };
    format!(
        "{phase_message} \
         Continue following active repository guidance, validation, documentation, and handoff requirements. \
         Do not edit repository instruction or guidance files merely to satisfy this acceleration hint; change them only when the user explicitly requested guidance changes or they are part of the task. \
         Use another shell_command only for one named missing fact that would make the next edit, execution-based validation, repair, commit, or report wrong. \
         This is advisory context, not a failed action result, and it does not relax repository rules or permission/capability requirements."
    )
}

/// Returns whether a shell command appears to be execution-based validation.
fn runtime_shell_command_looks_like_validation(command: &str) -> bool {
    let command = command.to_ascii_lowercase();
    [
        "cargo test",
        "cargo check",
        "cargo clippy",
        "cargo fmt",
        "just test",
        "just check",
        "just clippy",
        "just fmt",
        "npm test",
        "pnpm test",
        "yarn test",
        "pytest",
        "go test",
        "git diff --check",
    ]
    .iter()
    .any(|needle| command.contains(needle))
}
