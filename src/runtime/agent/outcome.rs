//! Agent turn outcome, failure, and recovery-output helpers.
//!
//! This module owns provider-completion validation, action-result failure
//! classification, model-recovery guidance, and terminal-safe failure output
//! shaping for runtime agent turns. Keeping these helpers outside the service
//! facade leaves the facade focused on state transitions and dispatch.

use super::{
    ActionPresentationInput, ActionResult, ActionStatus, AgentAction, AgentActionPayload,
    AgentTurnExecution, AgentTurnRecord, AgentTurnState, BTreeSet, BlockedApprovalRequest,
    ContextBlock, ContextSourceKind, MezError, Result, RuntimeSessionService, action_outcome_line,
    action_rationale_repeats_visible_summary, action_result_context_content, action_summary,
    current_unix_seconds, local_action_plan, network_action_plan, runtime_action_result_error_code,
    runtime_action_result_is_aggregated_loop_guard_failure,
    runtime_action_result_is_feedback_candidate, runtime_action_type_is_shell_backed,
    runtime_agent_terminal_preview, runtime_agent_turn_duration_display,
    runtime_agent_turn_state_name, runtime_execution_can_feed_failure_to_model,
    runtime_execution_uses_unbounded_apply_patch_recovery, runtime_failure_feedback_attempt_keys,
    runtime_failure_feedback_evidence_guidance, runtime_failure_feedback_loop_guard_aggregate_note,
    runtime_failure_feedback_repeat_guidance, runtime_failure_feedback_specific_guidance,
    runtime_failure_feedback_status_line, runtime_mezzanine_error_code,
    runtime_provider_audit_error_message,
};

impl RuntimeSessionService {
    /// Queues one provider continuation after model-correctable action
    /// failures.
    ///
    /// Most model-correctable failures consume a bounded recovery budget keyed
    /// by stable failed-action signatures. `apply_patch` failures remain
    /// model-correctable but intentionally bypass that budget so the model can
    /// iterate on patch context as needed.
    ///
    /// Real execution failures are useful model context: a bad shell command,
    /// timeout, or failed tool call often gives the model enough information to
    /// correct itself. Policy denials, rejected actions, cancellations, and
    /// user interrupts are intentionally excluded because repeating them would
    /// violate user intent or approval boundaries.
    pub(crate) fn queue_agent_failure_feedback_for_correction(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
        reason: &str,
    ) -> Result<bool> {
        if !self.execution_can_feed_failure_to_model(&turn.turn_id, execution) {
            return Ok(false);
        }
        let attempt_limit = self.agent_action_failure_retry_limit();
        let attempt_keys = runtime_failure_feedback_attempt_keys(&turn.turn_id, execution);
        let unbounded_apply_patch_recovery =
            runtime_execution_uses_unbounded_apply_patch_recovery(execution);
        let mut attempts_after_update = Vec::with_capacity(attempt_keys.len());
        let mut any_budget_remaining = unbounded_apply_patch_recovery;
        for attempt_key in &attempt_keys {
            let attempts = self
                .agent
                .agent_turn_failure_feedback_attempts
                .entry(attempt_key.clone())
                .or_insert(0);
            if unbounded_apply_patch_recovery {
                *attempts += 1;
            } else if *attempts < attempt_limit {
                *attempts += 1;
                any_budget_remaining = true;
            }
            attempts_after_update.push(*attempts);
        }
        let exhausted = !unbounded_apply_patch_recovery && !any_budget_remaining;
        let attempt = attempts_after_update.into_iter().max().unwrap_or(0);
        if exhausted {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                "failure feedback exhausted; ending turn as failed",
            )?;
            return Ok(false);
        }
        let pane_cwd = self
            .pane_current_working_directory(&turn.pane_id)
            .map(|path| path.to_string_lossy().into_owned());
        let context = self
            .agent_turn_contexts_mut()
            .get_mut(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let mut aggregated_loop_guard_codes = BTreeSet::new();
        for result in execution.action_results.iter().filter(|result| {
            !matches!(result.status, ActionStatus::Running | ActionStatus::Blocked)
        }) {
            if runtime_action_result_is_aggregated_loop_guard_failure(result)
                && let Some(code) = runtime_action_result_error_code(result)
                && !aggregated_loop_guard_codes.insert(code.to_string())
            {
                continue;
            }
            let label_prefix = if result.is_error {
                "action failure"
            } else {
                "action result"
            };
            let label = format!("{label_prefix} {}", result.action_id);
            let content = action_result_context_content(result);
            if !context
                .blocks
                .iter()
                .any(|block| block.label == label && block.content == content)
            {
                context.blocks.push(ContextBlock {
                    source: ContextSourceKind::ActionResult,
                    label,
                    content,
                });
            }
        }
        let specific_guidance =
            runtime_failure_feedback_specific_guidance(execution, pane_cwd.as_deref())
                .map(|guidance| format!("\n{guidance}"))
                .unwrap_or_default();
        let repeat_guidance =
            runtime_failure_feedback_repeat_guidance(execution, attempt).unwrap_or_default();
        let evidence_guidance = runtime_failure_feedback_evidence_guidance(execution)
            .map(|guidance| format!("\n{guidance}"))
            .unwrap_or_default();
        let aggregate_guidance = runtime_failure_feedback_loop_guard_aggregate_note(execution)
            .map(|guidance| format!("\n{guidance}"))
            .unwrap_or_default();
        let budget_header = if unbounded_apply_patch_recovery {
            String::new()
        } else {
            format!(
                "attempt={} max={}\n                 ",
                attempt, attempt_limit
            )
        };
        context.blocks.push(ContextBlock {
            source: ContextSourceKind::RuntimeHint,
            label: "action failure feedback".to_string(),
            content: format!(
                "[ephemeral action failure feedback]\n\
                 {}One or more actions failed during this turn. Use the action result context above to correct the plan for the same user request. Do not repeat an identical failed action unless you changed the inputs or can explain why the repeat is necessary. Emit a new MAAP action batch with a visible or executable next step.{}{}{}{}",
                budget_header,
                evidence_guidance,
                specific_guidance,
                repeat_guidance,
                aggregate_guidance
            ),
        });
        execution.final_turn = false;
        execution.terminal_state = AgentTurnState::Running;
        self.agent
            .pending_agent_provider_tasks
            .insert(turn.turn_id.clone());
        let feedback_status_line = runtime_failure_feedback_status_line(
            execution,
            attempt,
            attempt_limit,
            unbounded_apply_patch_recovery,
        );
        self.append_agent_status_text_to_terminal_buffer(&turn.pane_id, &feedback_status_line)?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "provider_task queued reason=action_failure_feedback source={}",
                reason
            ),
        )?;
        Ok(true)
    }

    /// Returns true when a failed execution can be retried by the model.
    ///
    /// Most failed executions must be fully settled before feedback is safe.
    /// A common exception is a sequential shell-backed batch: the first file
    /// action may fail before execution while later shell-backed siblings are
    /// still only pending dispatch. Those inactive siblings have not reached
    /// the pane and should not block bounded self-correction.
    ///
    /// # Parameters
    /// - `turn_id`: The owning turn id used to inspect active shell work.
    /// - `execution`: The failed execution being evaluated.
    fn execution_can_feed_failure_to_model(
        &self,
        turn_id: &str,
        execution: &AgentTurnExecution,
    ) -> bool {
        if runtime_execution_can_feed_failure_to_model(execution) {
            return true;
        }
        if execution.terminal_state != AgentTurnState::Failed {
            return false;
        }
        if !execution
            .action_results
            .iter()
            .any(runtime_action_result_is_feedback_candidate)
        {
            return false;
        }
        execution.action_results.iter().all(|result| {
            if result.is_error {
                return runtime_action_result_is_feedback_candidate(result);
            }
            if matches!(result.status, ActionStatus::Succeeded) {
                return true;
            }
            self.action_result_is_inactive_pending_shell_sibling(turn_id, result)
        })
    }

    /// Returns true when a running shell-backed sibling has not reached the
    /// pane shell yet.
    ///
    /// # Parameters
    /// - `turn_id`: The owning turn id.
    /// - `result`: The action result being classified.
    fn action_result_is_inactive_pending_shell_sibling(
        &self,
        turn_id: &str,
        result: &ActionResult,
    ) -> bool {
        result.status == ActionStatus::Running
            && !result.is_error
            && runtime_action_type_is_shell_backed(result.action_type)
            && !self.agent_action_has_running_shell_transaction(turn_id, &result.action_id)
    }

    /// Removes all failure-feedback attempt counters owned by one turn.
    pub(crate) fn clear_agent_failure_feedback_attempts_for_turn(&mut self, turn_id: &str) {
        let scoped_prefix = format!("{turn_id}:");
        self.agent
            .agent_turn_failure_feedback_attempts
            .retain(|key, _| key != turn_id && !key.starts_with(&scoped_prefix));
    }

    /// Appends one action result to the active model context if it has not
    /// already been recorded.
    pub(crate) fn append_action_result_context_if_absent(
        &mut self,
        turn_id: &str,
        result: &ActionResult,
    ) -> Result<()> {
        let context = self
            .agent_turn_contexts_mut()
            .get_mut(turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let label = format!("action result {}", result.action_id);
        let content = action_result_context_content(result);
        if !context
            .blocks
            .iter()
            .any(|block| block.label == label && block.content == content)
        {
            context.blocks.push(ContextBlock {
                source: ContextSourceKind::ActionResult,
                label,
                content,
            });
        }
        Ok(())
    }
}

/// Runs the runtime agent execution failure error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_agent_execution_failure_error(execution: &AgentTurnExecution) -> MezError {
    let failure = mez_agent::outcome::classify_agent_execution_failure(execution);
    let failure_json = runtime_agent_execution_failure_json(execution, &failure);
    let kind = match failure.kind() {
        mez_agent::outcome::AgentExecutionFailureKind::InvalidState => {
            crate::error::MezErrorKind::InvalidState
        }
        mez_agent::outcome::AgentExecutionFailureKind::InvalidArguments => {
            crate::error::MezErrorKind::InvalidArgs
        }
    };
    let mut error =
        MezError::new(kind, failure.message()).with_provider_failure_json(failure_json.to_string());
    if !execution.response.raw_text.trim().is_empty() {
        error = error.with_provider_raw_text(execution.response.raw_text.clone());
    }
    error
}

/// Runs the runtime agent execution failure json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_agent_execution_failure_json(
    execution: &AgentTurnExecution,
    failure: &mez_agent::outcome::AgentExecutionFailure,
) -> serde_json::Value {
    let kind = match failure.kind() {
        mez_agent::outcome::AgentExecutionFailureKind::InvalidState => {
            crate::error::MezErrorKind::InvalidState
        }
        mez_agent::outcome::AgentExecutionFailureKind::InvalidArguments => {
            crate::error::MezErrorKind::InvalidArgs
        }
    };
    let mut value = serde_json::json!({
        "type": "agent_turn_execution_failure",
        "stage": failure.stage(),
        "terminal_state": runtime_agent_turn_state_name(execution.terminal_state),
        "error": {
            "kind": runtime_mezzanine_error_code(kind),
            "message": runtime_provider_audit_error_message(failure.message())
        },
        "response": {
            "raw_text_bytes": execution.response.raw_text.len(),
            "action_batch_present": execution.response.action_batch.is_some(),
            "action_count": execution
                .response
                .action_batch
                .as_ref()
                .map(|batch| batch.actions.len())
                .unwrap_or(0)
        },
        "action_results": {
            "count": execution.action_results.len(),
            "error_count": execution
                .action_results
                .iter()
                .filter(|result| result.is_error)
                .count()
        }
    });
    if let Some(action) = failure.action() {
        value["action"] = serde_json::json!({
            "action_id": action.action_id(),
            "action_type": action.action_type(),
            "status": action.status(),
            "error_code": action.error_code(),
            "error_message": action.error_message()
        });
    }
    value
}

/// Returns the validated product action plans supplied to neutral lower-crate
/// presentation and policy decisions.
fn runtime_agent_action_plans(
    action: &AgentAction,
) -> (
    Option<mez_agent::LocalActionPlan>,
    Option<mez_agent::NetworkActionPlan>,
) {
    (
        local_action_plan(action).ok().flatten(),
        network_action_plan(action),
    )
}

/// Returns a command string safe for model context, transcripts, and status
/// payloads. Raw shell commands remain visible, while runtime-generated
/// semantic commands are represented by their compact policy command so inline
/// file contents are not retained or replayed.
pub(super) fn runtime_agent_context_command(action: &AgentAction, command: &str) -> String {
    if matches!(action.payload, AgentActionPayload::ShellCommand { .. }) {
        return command.to_string();
    }
    local_action_plan(action)
        .ok()
        .flatten()
        .map(|plan| plan.policy_command)
        .unwrap_or_else(|| runtime_agent_terminal_preview(command))
}

/// Converts compact internal transition labels into readable diagnostic text.
///
/// Debug and trace output still needs to be precise enough for state-machine
/// diagnosis, but it should read as an operator-facing sentence fragment rather
/// than as raw key/value telemetry.
pub(super) fn runtime_humanize_agent_diagnostic(value: &str) -> String {
    value
        .trim()
        .replace('_', " ")
        .replace(" reason=", ", reason: ")
        .replace(" provider=", ", provider: ")
        .replace(" model=", ", model: ")
        .replace(" context_blocks=", ", context blocks: ")
        .replace(" terminal_state=", ", terminal state: ")
        .replace(" pending_shell_dispatch=", ", pending shell dispatch: ")
        .replace(
            " ready_for_provider_continuation=",
            ", ready for provider continuation: ",
        )
        .replace("=", ": ")
}

/// Runs the runtime agent pending approval log line operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_agent_pending_approval_log_line(approval: &BlockedApprovalRequest) -> String {
    format!(
        "agent approval {} pending: {} {} (approve with /approve {})",
        approval.id,
        approval.action_kind,
        runtime_agent_terminal_preview(&approval.action_summary),
        approval.id
    )
}

/// Runs the runtime agent shell summary operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_agent_action_summary(action: &AgentAction) -> Option<String> {
    let (local_plan, network_plan) = runtime_agent_action_plans(action);
    action_summary(
        action,
        ActionPresentationInput {
            local_plan: local_plan.as_ref(),
            network_plan: network_plan.as_ref(),
            show_runtime_target: false,
        },
    )
}

/// Runs the runtime agent shell status operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_agent_shell_status(action: &AgentAction, fallback: &str) -> String {
    format!(
        "agent: {}",
        runtime_agent_action_summary(action).unwrap_or_else(|| fallback.to_string())
    )
}

/// Builds the terminal footer that replaces the live working timer.
pub(super) fn runtime_agent_finished_footer_line(
    turn: &AgentTurnRecord,
    state: AgentTurnState,
) -> Option<String> {
    let elapsed = runtime_agent_turn_duration_display(
        current_unix_seconds().saturating_sub(turn.started_at_unix_seconds),
    );
    match state {
        AgentTurnState::Completed => Some(format!("Worked for {elapsed}")),
        AgentTurnState::Failed => Some(format!("Failed after {elapsed}")),
        AgentTurnState::Interrupted => Some(format!("Stopped after {elapsed}")),
        AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked => None,
    }
}

/// Runs the runtime agent action rationale repeats visible summary operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_agent_action_rationale_repeats_visible_summary(action: &AgentAction) -> bool {
    let (local_plan, network_plan) = runtime_agent_action_plans(action);
    action_rationale_repeats_visible_summary(
        action,
        ActionPresentationInput {
            local_plan: local_plan.as_ref(),
            network_plan: network_plan.as_ref(),
            show_runtime_target: false,
        },
    )
}

/// Builds a concise default-visible line for a runtime action that could not
/// reach its usual visible execution path.
pub(super) fn runtime_agent_action_outcome_line(
    action: &AgentAction,
    result: &ActionResult,
    show_shell_details: bool,
) -> Option<(bool, String)> {
    let (local_plan, network_plan) = runtime_agent_action_plans(action);
    action_outcome_line(
        action,
        result,
        ActionPresentationInput {
            local_plan: local_plan.as_ref(),
            network_plan: network_plan.as_ref(),
            show_runtime_target: show_shell_details,
        },
    )
    .map(|outcome| (outcome.is_error, outcome.line))
}
