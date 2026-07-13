//! Agent turn outcome, failure, and recovery-output helpers.
//!
//! This module owns provider-completion validation, action-result failure
//! classification, model-recovery guidance, and terminal-safe failure output
//! shaping for runtime agent turns. Keeping these helpers outside the service
//! facade leaves the facade focused on state transitions and dispatch.

use super::*;
use crate::agent::is_valid_skill_name;

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
    pub(in crate::runtime) fn queue_agent_failure_feedback_for_correction(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
        reason: &str,
    ) -> Result<bool> {
        if !self.execution_can_feed_failure_to_model(&turn.turn_id, execution) {
            return Ok(false);
        }
        let attempt_limit = self.agent_action_failure_retry_limit.max(1);
        let attempt_keys = runtime_failure_feedback_attempt_keys(&turn.turn_id, execution);
        let unbounded_apply_patch_recovery =
            runtime_execution_uses_unbounded_apply_patch_recovery(execution);
        let mut attempts_after_update = Vec::with_capacity(attempt_keys.len());
        let mut any_budget_remaining = unbounded_apply_patch_recovery;
        for attempt_key in &attempt_keys {
            let attempts = self
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
            .agent_turn_contexts
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
        self.pending_agent_provider_tasks
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
            && !self.running_shell_transactions.values().any(|transaction| {
                transaction.turn_id == turn_id
                    && matches!(
                        &transaction.kind,
                        RunningShellTransactionKind::AgentAction { action_id }
                            if action_id == &result.action_id
                    )
            })
    }

    /// Removes all failure-feedback attempt counters owned by one turn.
    pub(in crate::runtime) fn clear_agent_failure_feedback_attempts_for_turn(
        &mut self,
        turn_id: &str,
    ) {
        let scoped_prefix = format!("{turn_id}:");
        self.agent_turn_failure_feedback_attempts
            .retain(|key, _| key != turn_id && !key.starts_with(&scoped_prefix));
    }

    /// Appends one action result to the active model context if it has not
    /// already been recorded.
    pub(in crate::runtime) fn append_action_result_context_if_absent(
        &mut self,
        turn_id: &str,
        result: &ActionResult,
    ) -> Result<()> {
        let context = self
            .agent_turn_contexts
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

/// Validates provider-completion identity before mutating runtime state.
///
/// Completion events are runtime-owned data crossing an async worker boundary.
/// Identity mismatches are therefore runtime defects and must be converted into
/// failed turns before any partial application can occur.
///
/// # Parameters
/// - `turn`: The active runtime turn.
/// - `agent_id`: The event-level agent identifier.
/// - `turn_id`: The event-level turn identifier.
/// - `execution`: The provider execution payload.
pub(super) fn runtime_validate_provider_completion_identity(
    turn: &AgentTurnRecord,
    agent_id: &AgentId,
    turn_id: &str,
    execution: &AgentTurnExecution,
) -> Result<()> {
    if turn.turn_id != turn_id || execution.request.turn_id != turn_id {
        return Err(MezError::invalid_state(
            "agent provider completion turn id does not match active turn",
        ));
    }
    if turn.agent_id != agent_id.as_str() || execution.request.agent_id != agent_id.as_str() {
        return Err(MezError::invalid_state(
            "agent provider completion agent id does not match active turn",
        ));
    }
    if let Some(batch) = execution.response.action_batch.as_ref()
        && (batch.turn_id != turn_id || batch.agent_id != agent_id.as_str())
    {
        return Err(MezError::invalid_state(
            "agent provider completion action batch identity does not match active turn",
        ));
    }
    Ok(())
}

/// Validates provider-completion action state before runtime application.
///
/// The actor owns terminal state, transcript persistence, and action execution.
/// It must not accept a completion where action results cannot be mapped back
/// to the returned MAAP batch or where the declared terminal state contradicts
/// the action statuses.
///
/// # Parameters
/// - `turn`: The active runtime turn.
/// - `execution`: The provider execution payload.
pub(in crate::runtime) fn runtime_validate_provider_completion_execution(
    turn: &AgentTurnRecord,
    execution: &mut AgentTurnExecution,
) -> Result<()> {
    let Some(batch) = execution.response.action_batch.as_ref() else {
        if runtime_execution_is_missing_batch_terminal_failure(execution) {
            return Ok(());
        }
        if !execution.action_results.is_empty() {
            return Err(MezError::invalid_state(
                "agent provider completion without an action batch included action results",
            ));
        }
        // A missing action batch is always a terminal provider/controller
        // failure. Normalize it here so the turn does not remain stranded
        // in Running state with no progress path.
        execution.terminal_state = AgentTurnState::Failed;
        execution.final_turn = true;
        return Ok(());
    };
    let controller_failure_summary =
        runtime_execution_is_controller_failure_summary(execution, batch);
    let controller_validation_failure =
        runtime_execution_is_controller_validation_failure(execution);
    let controller_terminal_failure = controller_failure_summary || controller_validation_failure;
    if batch.protocol != "maap/1" {
        return Err(MezError::invalid_state(
            "agent provider completion action batch protocol is unsupported",
        ));
    }
    if batch.rationale.trim().is_empty() {
        return Err(MezError::invalid_state(
            "agent provider completion action batch rationale is empty",
        ));
    }
    if batch.actions.is_empty() && !batch.final_turn {
        return Err(MezError::invalid_state(
            "agent provider completion action batch has no actions but is not final",
        ));
    }
    if batch.final_turn != execution.final_turn && !controller_terminal_failure {
        return Err(MezError::invalid_state(
            "agent provider completion final flag does not match action batch",
        ));
    }
    let mut action_types = BTreeMap::new();
    for action in &batch.actions {
        if action.id.trim().is_empty() {
            return Err(MezError::invalid_state(
                "agent provider completion action batch contains an empty action id",
            ));
        }
        if action_types
            .insert(action.id.clone(), action.action_type())
            .is_some()
        {
            return Err(MezError::invalid_state(
                "agent provider completion action batch contains duplicate action ids",
            ));
        }
    }
    let mut result_ids = BTreeSet::new();
    for result in &execution.action_results {
        result.validate_invariants()?;
        if result.protocol != "maap/1" {
            return Err(MezError::invalid_state(
                "agent provider completion action result protocol is unsupported",
            ));
        }
        if result.turn_id != turn.turn_id || result.agent_id != turn.agent_id {
            return Err(MezError::invalid_state(
                "agent provider completion action result identity does not match active turn",
            ));
        }
        if !result_ids.insert(result.action_id.clone()) {
            return Err(MezError::invalid_state(
                "agent provider completion contains duplicate action results",
            ));
        }
        let Some(action_type) = action_types.get(&result.action_id) else {
            return Err(MezError::invalid_state(
                "agent provider completion action result does not match an action",
            ));
        };
        if action_type != &result.action_type {
            return Err(MezError::invalid_state(
                "agent provider completion action result type does not match action",
            ));
        }
    }
    if action_types.len() != result_ids.len() && !controller_terminal_failure {
        return Err(MezError::invalid_state(
            "agent provider completion action batch and result counts differ",
        ));
    }
    let expected_state = runtime_agent_turn_state_from_action_results(
        &execution.action_results,
        execution.final_turn,
    );
    if execution.terminal_state != expected_state && !controller_terminal_failure {
        return Err(MezError::invalid_state(
            "agent provider completion terminal state does not match action results",
        ));
    }
    Ok(())
}

/// Reports whether one provider completion is a terminal failed execution that
/// never produced a parseable MAAP action batch.
///
/// Missing action batches are controller/provider failures, not ordinary
/// progress states. They are valid only when terminal, failed, and result-free.
pub(super) fn runtime_execution_is_missing_batch_terminal_failure(
    execution: &AgentTurnExecution,
) -> bool {
    execution.terminal_state == AgentTurnState::Failed
        && execution.final_turn
        && execution.action_results.is_empty()
}

/// Reports whether one provider completion is a controller-owned terminal
/// failure summary.
///
/// Failure summaries are synthetic: the model supplies a final user-facing
/// `say`, but the runtime-owned turn state remains `failed` because the
/// provider/controller boundary already failed before normal action execution
/// could complete.
pub(super) fn runtime_execution_is_controller_failure_summary(
    execution: &AgentTurnExecution,
    batch: &crate::agent::MaapBatch,
) -> bool {
    execution.terminal_state == AgentTurnState::Failed
        && execution.final_turn
        && execution
            .response
            .raw_text
            .contains("controller_failure_summary:")
        && !batch.actions.is_empty()
        && batch
            .actions
            .iter()
            .all(|action| matches!(action.payload, AgentActionPayload::Say { .. }))
        && execution.action_results.len() == batch.actions.len()
        && execution
            .action_results
            .iter()
            .all(|result| result.action_type == "say" && !result.is_error)
}

/// Reports whether one provider completion is a controller-owned MAAP
/// validation failure.
///
/// The invalid model batch is retained for diagnostics, audit, and transcript
/// evidence, but no action results are produced because the controller rejected
/// the batch before any action could execute.
pub(super) fn runtime_execution_is_controller_validation_failure(
    execution: &AgentTurnExecution,
) -> bool {
    execution.terminal_state == AgentTurnState::Failed
        && execution.final_turn
        && execution.action_results.is_empty()
        && execution
            .response
            .raw_text
            .contains("maap_validation_error:")
}

/// Runs the runtime provider audit error message operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_provider_audit_error_message(message: &str) -> String {
    /// Defines the MAX AUDIT ERROR CHARS const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    const MAX_AUDIT_ERROR_CHARS: usize = 512;
    let mut output = message
        .chars()
        .take(MAX_AUDIT_ERROR_CHARS)
        .collect::<String>();
    if message.chars().count() > MAX_AUDIT_ERROR_CHARS {
        output.push_str("...");
    }
    output
}

/// Carries Runtime Agent Execution Failure state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(super) struct RuntimeAgentExecutionFailure {
    /// Stores the kind value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    kind: crate::error::MezErrorKind,
    /// Stores the stage value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    stage: &'static str,
    /// Stores the message value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    message: String,
    /// Stores the action value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    action: Option<serde_json::Value>,
}

/// Runs the runtime agent execution failure error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_agent_execution_failure_error(execution: &AgentTurnExecution) -> MezError {
    let failure = runtime_agent_execution_failure(execution);
    let failure_json = runtime_agent_execution_failure_json(execution, &failure);
    let mut error = MezError::new(failure.kind, failure.message)
        .with_provider_failure_json(failure_json.to_string());
    if !execution.response.raw_text.trim().is_empty() {
        error = error.with_provider_raw_text(execution.response.raw_text.clone());
    }
    error
}

/// Runs the runtime agent execution failure operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_agent_execution_failure(
    execution: &AgentTurnExecution,
) -> RuntimeAgentExecutionFailure {
    if execution.response.action_batch.is_none() {
        if let Some(provider_error) = runtime_embedded_provider_error(&execution.response.raw_text)
        {
            return RuntimeAgentExecutionFailure {
                kind: crate::error::MezErrorKind::InvalidState,
                stage: "provider_error",
                message: provider_error.to_string(),
                action: None,
            };
        }
        return RuntimeAgentExecutionFailure {
            kind: crate::error::MezErrorKind::InvalidState,
            stage: "missing_action_batch",
            message: "model response did not contain a MAAP action batch".to_string(),
            action: None,
        };
    }
    if let Some(validation_error) = execution
        .response
        .raw_text
        .split_once("maap_validation_error:")
        .map(|(_, diagnostic)| diagnostic.trim())
        .filter(|diagnostic| !diagnostic.is_empty())
    {
        return RuntimeAgentExecutionFailure {
            kind: crate::error::MezErrorKind::InvalidArgs,
            stage: "maap_validation",
            message: format!("MAAP validation failed: {validation_error}"),
            action: None,
        };
    }
    if let Some(result) = execution
        .action_results
        .iter()
        .find(|result| result.is_error)
    {
        let (code, message) = result
            .error
            .as_ref()
            .map(|error| (error.code.as_str(), error.message.as_str()))
            .unwrap_or((
                "action_failed",
                "agent action failed without an error object",
            ));
        return RuntimeAgentExecutionFailure {
            kind: crate::error::MezErrorKind::InvalidState,
            stage: "action_result",
            message: format!("agent action {code}: {message}"),
            action: Some(serde_json::json!({
                "action_id": &result.action_id,
                "action_type": result.action_type,
                "status": runtime_action_status_name(result.status),
                "error_code": code,
                "error_message": message
            })),
        };
    }
    RuntimeAgentExecutionFailure {
        kind: crate::error::MezErrorKind::InvalidState,
        stage: "agent_turn_failed",
        message: "agent turn failed without a specific diagnostic".to_string(),
        action: None,
    }
}

/// Returns the runtime provider error diagnostic embedded in a failed response.
fn runtime_embedded_provider_error(raw_text: &str) -> Option<&str> {
    raw_text
        .lines()
        .rev()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("provider_error: "))
        .map(str::trim)
        .filter(|message| !message.is_empty())
}

/// Runs the runtime agent execution failure json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_agent_execution_failure_json(
    execution: &AgentTurnExecution,
    failure: &RuntimeAgentExecutionFailure,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "type": "agent_turn_execution_failure",
        "stage": failure.stage,
        "terminal_state": runtime_agent_turn_state_name(execution.terminal_state),
        "error": {
            "kind": runtime_mezzanine_error_code(failure.kind),
            "message": runtime_provider_audit_error_message(&failure.message)
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
    if let Some(action) = &failure.action {
        value["action"] = action.clone();
    }
    value
}

/// Runs the runtime action status name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_action_status_name(status: ActionStatus) -> &'static str {
    match status {
        ActionStatus::Rejected => "rejected",
        ActionStatus::Blocked => "blocked",
        ActionStatus::Denied => "denied",
        ActionStatus::Running => "running",
        ActionStatus::Succeeded => "succeeded",
        ActionStatus::Failed => "failed",
        ActionStatus::Cancelled => "cancelled",
        ActionStatus::TimedOut => "timed_out",
        ActionStatus::Interrupted => "interrupted",
    }
}

/// Returns true when a failed action result is safe to hand back to the model
/// for bounded self-correction attempts.
pub(super) fn runtime_action_result_is_feedback_candidate(result: &ActionResult) -> bool {
    let Some(error) = result.error.as_ref() else {
        return false;
    };
    if !result.is_error {
        return false;
    }
    if result.action_type == "shell_command"
        && result.status == ActionStatus::Failed
        && error.code == "pane_not_ready"
    {
        return true;
    }
    if runtime_action_result_is_runtime_infrastructure_failure(result) {
        return false;
    }
    if result.status == ActionStatus::TimedOut {
        return runtime_action_type_is_model_correctable(result.action_type)
            && !runtime_error_code_is_non_correctable(error.code.as_str());
    }
    if result.action_type == "spawn_agent"
        && result.status == ActionStatus::Denied
        && error.code == "forbidden"
        && error
            .message
            .to_ascii_lowercase()
            .contains("subagent spawn limit reached")
    {
        return true;
    }
    if result.status != ActionStatus::Failed {
        return false;
    }
    if runtime_action_type_is_model_correctable(result.action_type)
        && !runtime_error_code_is_non_correctable(error.code.as_str())
    {
        return true;
    }
    matches!(
        error.code.as_str(),
        "shell_command_failed"
            | "shell_exit_nonzero"
            | "pane_input_write_failed"
            | "mcp_tool_error"
            | "network_request_failed"
            | "network_http_error"
            | "unsupported_url_scheme"
            | "config_change_failed"
            | "invalid_message_payload"
    ) || (error.code == "invalid_params" && runtime_invalid_params_is_feedback_candidate(result))
        || (result.action_type == "spawn_agent" && error.code != "forbidden")
}

/// Returns true when the failure is runtime infrastructure state rather than
/// model-correctable action input.
pub(super) fn runtime_action_result_is_runtime_infrastructure_failure(
    result: &ActionResult,
) -> bool {
    let Some(error) = result.error.as_ref() else {
        return false;
    };
    let message = error.message.to_ascii_lowercase();
    message.contains("pane process not found")
        || (error.code == "not_found" && message.starts_with("shell_dispatch:"))
}

/// Returns true when one action type is authored by the model and can usually
/// be retried with corrected parameters or a different action choice.
pub(super) fn runtime_action_type_is_model_correctable(action_type: &str) -> bool {
    matches!(
        action_type,
        "apply_patch"
            | "web_search"
            | "fetch_url"
            | "send_message"
            | "spawn_agent"
            | "config_change"
            | "mcp_call"
            | "request_skills"
            | "call_skill"
    )
}

/// Returns true when one action type is dispatched through the pane shell.
///
/// Running shell-backed action results can represent work that is merely
/// waiting for dispatch. If a sibling action already failed before that
/// dispatch happened, those inactive siblings must not prevent model
/// self-correction.
pub(super) fn runtime_action_type_is_shell_backed(action_type: &str) -> bool {
    matches!(action_type, "shell_command" | "apply_patch")
}

/// Returns true when an error represents a policy or user boundary rather than
/// evidence the model can use to correct its own action.
pub(super) fn runtime_error_code_is_non_correctable(error_code: &str) -> bool {
    matches!(
        error_code,
        "forbidden"
            | "policy_forbidden"
            | "denied"
            | "approval_denied"
            | "hook_blocked"
            | "cancelled"
            | "interrupted"
            | "user_cancelled"
    )
}

/// Returns true when `invalid_params` belongs to model-authored action input.
///
/// Invalid parameter failures are generally useful to the model only when the
/// failed action itself supplied the bad argument. Runtime wiring failures and
/// policy outcomes have separate statuses or error codes and are not included.
pub(super) fn runtime_invalid_params_is_feedback_candidate(result: &ActionResult) -> bool {
    runtime_action_type_is_model_correctable(result.action_type)
}

/// Returns true when an action carries enough model-authored explanation to
/// satisfy auto-allow after compact MAAP omits the formerly required rationale.
pub(super) fn runtime_action_supports_auto_allow(action: &AgentAction) -> bool {
    if !action.rationale.trim().is_empty() {
        return true;
    }
    local_action_summary(action)
        .ok()
        .flatten()
        .or_else(|| network_action_summary(action).ok().flatten())
        .is_some_and(|summary| !summary.trim().is_empty())
}

/// Runtime-visible skill context already loaded into one active turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct RuntimeSkillActionContext {
    /// Whether a successful skill catalog result is already present.
    pub(super) catalog_requested: bool,
    /// Skill names whose full `SKILL.md` text is already in context.
    pub(super) loaded_skills: BTreeSet<String>,
}

/// Extracts loaded skill state from active model-context blocks.
///
/// This intentionally inspects only explicit context labels and action-result
/// text that the runtime itself produced. It does not parse arbitrary repository
/// files or shell output as authoritative skill state.
pub(super) fn runtime_skill_action_context_from_blocks(
    blocks: &[ContextBlock],
) -> RuntimeSkillActionContext {
    let mut context = RuntimeSkillActionContext::default();
    for block in blocks {
        if block.source == ContextSourceKind::ActionResult
            && block.content.lines().next().is_some_and(|line| {
                line.starts_with("[action_result ")
                    && line.contains(" request_skills ")
                    && line.ends_with(" succeeded]")
            })
        {
            context.catalog_requested = true;
        }
        if let Some(name) = block.label.strip_prefix("explicit skill ")
            && is_valid_skill_name(name)
        {
            context.loaded_skills.insert(name.to_string());
        }
        for line in block.content.lines() {
            let Some(name) = line.strip_prefix("# Skill: ") else {
                continue;
            };
            let name = name.trim();
            if is_valid_skill_name(name) {
                context.loaded_skills.insert(name.to_string());
            }
        }
    }
    context
}

/// Returns true when a failed execution can continue by feeding action failure
/// results back to the model instead of immediately ending the turn.
pub(super) fn runtime_execution_can_feed_failure_to_model(execution: &AgentTurnExecution) -> bool {
    execution.terminal_state == AgentTurnState::Failed
        && execution
            .action_results
            .iter()
            .all(|result| !matches!(result.status, ActionStatus::Running | ActionStatus::Blocked))
        && execution
            .action_results
            .iter()
            .any(runtime_action_result_is_feedback_candidate)
        && execution.action_results.iter().all(|result| {
            !result.is_error
                || runtime_action_result_is_feedback_candidate(result)
                || result.status == ActionStatus::Succeeded
        })
}

/// Returns the failure-feedback attempt keys for one failed execution.
///
/// The turn id scopes cleanup while the hash scopes repeated failures to
/// individual failed action signatures. Bounded retries consult these keys for
/// budget accounting, while unbounded `apply_patch` recovery reuses the same
/// keys only to track repeated identical failures for guidance.
pub(super) fn runtime_failure_feedback_attempt_keys(
    turn_id: &str,
    execution: &AgentTurnExecution,
) -> Vec<String> {
    let mut keys = execution
        .action_results
        .iter()
        .filter(|result| runtime_action_result_is_feedback_candidate(result))
        .map(|result| runtime_failure_feedback_attempt_key_for_result(turn_id, result))
        .collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    keys
}

/// Returns the per-action failure-feedback attempt key for one action result.
pub(super) fn runtime_failure_feedback_attempt_key_for_result(
    turn_id: &str,
    result: &ActionResult,
) -> String {
    let digest = exact_command_sha256(
        DEFAULT_COMMAND_SHELL_CLASSIFICATION,
        &runtime_failure_feedback_action_signature(result),
    );
    format!("{turn_id}:{digest}")
}

/// Returns true when all model-correctable failures in one execution are
/// `apply_patch` failures that should bypass the bounded retry budget.
pub(super) fn runtime_execution_uses_unbounded_apply_patch_recovery(
    execution: &AgentTurnExecution,
) -> bool {
    let mut saw_feedback_candidate = false;
    for result in execution.action_results.iter() {
        if !runtime_action_result_is_feedback_candidate(result) {
            continue;
        }
        saw_feedback_candidate = true;
        if result.action_type != "apply_patch"
            || !runtime_apply_patch_failure_output_contains(result, "hunk did not match")
        {
            return false;
        }
    }
    saw_feedback_candidate
}

/// Returns stable, non-secret material identifying one failed action result.
pub(super) fn runtime_failure_feedback_action_signature(result: &ActionResult) -> String {
    let error_code = result
        .error
        .as_ref()
        .map(|error| error.code.as_str())
        .unwrap_or("");
    let error_message = result
        .error
        .as_ref()
        .map(|error| error.message.as_str())
        .unwrap_or("");
    format!(
        "action type={} status={} error={} message={} content={}\n",
        result.action_type,
        runtime_action_status_name(result.status),
        error_code,
        error_message,
        result.content_text()
    )
}

/// Returns the machine-readable error code attached to an action result.
///
/// # Parameters
/// - `result`: The action result being inspected.
pub(super) fn runtime_action_result_error_code(result: &ActionResult) -> Option<&str> {
    result.error.as_ref().map(|error| error.code.as_str())
}

/// Returns true when one action result carries the requested error code.
///
/// # Parameters
/// - `result`: The action result being inspected.
/// - `code`: The machine-readable error code to match.
pub(super) fn runtime_action_result_has_error_code(result: &ActionResult, code: &str) -> bool {
    runtime_action_result_error_code(result).is_some_and(|error_code| error_code == code)
}

/// Returns true when the result represents a batch-level loop guard failure
/// where showing every sibling failure would flood the pane and model context.
///
/// # Parameters
/// - `result`: The action result being inspected.
pub(super) fn runtime_action_result_is_aggregated_loop_guard_failure(
    result: &ActionResult,
) -> bool {
    let _ = result;
    false
}

/// Builds one bounded pane line for a group of loop guard failures.
///
/// # Parameters
/// - `label`: The human-readable guard label.
/// - `count`: The number of failed sibling actions represented by the line.
/// - `message`: The shared runtime guard diagnostic.
pub(super) fn runtime_loop_guard_failure_summary_line(
    label: &str,
    count: usize,
    message: &str,
) -> String {
    let action_word = if count == 1 { "action" } else { "actions" };
    format!(
        "agent: {label} limit reached; suppressed {count} {action_word} ({})",
        runtime_agent_terminal_preview(message)
    )
}

/// Returns the display label for an aggregated loop-guard error code.
///
/// # Parameters
/// - `code`: The machine-readable error code.
pub(super) fn runtime_loop_guard_failure_label(code: &str) -> Option<&'static str> {
    match code {
        "shell_dispatch_limit_exceeded" => Some("shell dispatch"),
        "network_action_limit_exceeded" => Some("network action"),
        _ => None,
    }
}

/// Returns true when a failed execution includes an `apply_patch` failure.
pub(super) fn runtime_execution_has_apply_patch_failure(execution: &AgentTurnExecution) -> bool {
    execution
        .action_results
        .iter()
        .any(|result| result.is_error && result.action_type == "apply_patch")
}

/// Returns true when a failed execution includes an `apply_patch` hunk
/// mismatch.
pub(super) fn runtime_execution_has_apply_patch_hunk_mismatch(
    execution: &AgentTurnExecution,
) -> bool {
    runtime_execution_has_apply_patch_failure_marker(execution, "hunk did not match")
}

/// Returns true when one `apply_patch` failure output contains the requested marker.
fn runtime_apply_patch_failure_output_contains(result: &ActionResult, marker: &str) -> bool {
    result.is_error
        && result.action_type == "apply_patch"
        && runtime_unrecovered_action_failure_output(result)
            .is_some_and(|output| output.contains(marker))
}

/// Returns true when one `apply_patch` failure output contains the requested marker.
fn runtime_execution_has_apply_patch_failure_marker(
    execution: &AgentTurnExecution,
    marker: &str,
) -> bool {
    execution
        .action_results
        .iter()
        .any(|result| runtime_apply_patch_failure_output_contains(result, marker))
}

/// Returns true when one `apply_patch` failure uses the requested error code.
fn runtime_execution_has_apply_patch_error_code(
    execution: &AgentTurnExecution,
    code: &str,
) -> bool {
    execution.action_results.iter().any(|result| {
        result.is_error
            && result.action_type == "apply_patch"
            && runtime_action_result_has_error_code(result, code)
    })
}

/// Returns true when a failed execution includes config-change validation.
pub(super) fn runtime_execution_has_config_change_failure(execution: &AgentTurnExecution) -> bool {
    execution.action_results.iter().any(|result| {
        result.is_error
            && result.action_type == "config_change"
            && result.error.as_ref().is_some_and(|error| {
                matches!(
                    error.code.as_str(),
                    "config_change_failed" | "invalid_params"
                )
            })
    })
}

/// Returns true when a failed execution includes invalid message payloads.
pub(super) fn runtime_execution_has_invalid_message_payload_failure(
    execution: &AgentTurnExecution,
) -> bool {
    execution.action_results.iter().any(|result| {
        result.is_error
            && result.action_type == "send_message"
            && result
                .error
                .as_ref()
                .is_some_and(|error| error.code == "invalid_message_payload")
    })
}

/// Returns true when a failed execution includes a model-correctable spawn.
pub(super) fn runtime_execution_has_spawn_agent_failure(execution: &AgentTurnExecution) -> bool {
    execution.action_results.iter().any(|result| {
        result.is_error
            && result.action_type == "spawn_agent"
            && matches!(result.status, ActionStatus::Failed | ActionStatus::Denied)
    })
}

/// Returns true when the model tried to rediscover or reload already-present
/// skill context.
pub(super) fn runtime_execution_has_redundant_skill_action_failure(
    execution: &AgentTurnExecution,
) -> bool {
    execution.action_results.iter().any(|result| {
        result.is_error
            && matches!(result.action_type, "request_skills" | "call_skill")
            && result.error.as_ref().is_some_and(|error| {
                matches!(
                    error.code.as_str(),
                    "skill_context_already_loaded" | "skill_catalog_already_requested"
                )
            })
    })
}

/// Returns true when a failed execution includes any of the listed action types.
pub(super) fn runtime_execution_has_failed_action_type(
    execution: &AgentTurnExecution,
    action_types: &[&str],
) -> bool {
    execution
        .action_results
        .iter()
        .any(|result| result.is_error && action_types.contains(&result.action_type))
}

/// Returns true when a failed execution includes a failed filesystem mutation.
pub(super) fn runtime_execution_has_mutation_failure(execution: &AgentTurnExecution) -> bool {
    runtime_execution_has_failed_action_type(execution, &["apply_patch"])
}

/// Returns true when a failed execution includes a shell-backed file operation.
pub(super) fn runtime_execution_has_file_operation_failure(execution: &AgentTurnExecution) -> bool {
    runtime_execution_has_failed_action_type(execution, &["apply_patch"])
}

/// Builds model-facing recovery guidance that prevents unsupported success claims.
pub(super) fn runtime_failure_feedback_evidence_guidance(
    execution: &AgentTurnExecution,
) -> Option<&'static str> {
    runtime_execution_has_mutation_failure(execution).then_some(
        "Mutation-evidence rule: no successful mutation has occurred after the failed action(s) in this recovery context. Do not claim the task is implemented, changed, updated, fixed, applied, run, or executed until a later mutation action succeeds and you verify it. Reads, git status, and git diff after a failed mutation prove only current file state; they do not prove your attempted edit landed. If current files appear changed without mutation-action proof, say the current file/diff shows that state rather than claiming you performed the change.",
    )
}

/// Builds extra model-facing recovery guidance for known failure modes.
pub(super) fn runtime_failure_feedback_specific_guidance(
    execution: &AgentTurnExecution,
    pane_cwd: Option<&str>,
) -> Option<String> {
    if runtime_execution_has_apply_patch_failure(execution) {
        let unsafe_paths = runtime_apply_patch_unsafe_paths(execution);
        if !unsafe_paths.is_empty() {
            let cwd_hint = pane_cwd
                .filter(|cwd| !cwd.trim().is_empty())
                .map(|cwd| format!(" Current pane working directory: {cwd}."))
                .unwrap_or_default();
            return Some(format!(
                "Apply-patch recovery: apply_patch rejected unsafe patch path(s): {}.{cwd_hint} Reissue the Mezzanine patch with file header paths relative to the pane current working directory, for example `src/conf/document.rs`, and never use absolute paths or `..` traversal in apply_patch headers.",
                unsafe_paths.join(", ")
            ));
        }
        let paths = runtime_apply_patch_failed_paths(execution);
        let path_hint = if paths.is_empty() {
            String::new()
        } else {
            format!(" Affected path(s): {}.", paths.join(", "))
        };
        if runtime_execution_has_apply_patch_error_code(execution, "invalid_params") {
            return Some(
                "Apply-patch recovery: the patch payload was rejected by Mezzanine validation before execution. Next step: correct the patch structure from the action result diagnostic before retrying. Do not treat this as a file-context mismatch or start with file rereads unless another diagnostic points to current file contents. Reissue a valid Mezzanine patch block that starts with *** Begin Patch, or use shell_command only for local inspection, path operations, validation, or raw unified diffs that apply_patch cannot express."
                    .to_string(),
            );
        }
        if runtime_execution_has_apply_patch_error_code(execution, "pane_input_write_failed") {
            return Some(format!(
                "Apply-patch recovery: the runtime could not deliver the generated patch command to the pane shell. Next step: use the action result to retry with a smaller Mezzanine patch or another bounded corrective action instead of treating this as a file-context mismatch. If another attempt is needed, break large mutations into smaller apply_patch actions after any necessary inspection.{path_hint} Use shell_command only for local inspection, path operations, validation, or raw unified diffs that apply_patch cannot express."
            ));
        }
        if runtime_execution_has_apply_patch_error_code(
            execution,
            "apply_patch_execution_mode_changed",
        ) {
            return Some(
                "Apply-patch recovery: the apply_patch action was aborted because the selected local-action execution mode changed after dispatch. Next step: retry the patch as a fresh action after the desired shell mode is stable; do not mix native and pane-shell phases within one apply_patch action."
                    .to_string(),
            );
        }
        if runtime_execution_has_apply_patch_error_code(
            execution,
            "apply_patch_read_transport_incomplete",
        ) || runtime_execution_has_apply_patch_error_code(
            execution,
            "apply_patch_transport_incomplete",
        ) {
            return Some(format!(
                "Apply-patch recovery: the pane-shell apply_patch transport was truncated or incomplete before Mezzanine could finish the read/write boundary. Next step: retry with a smaller patch, split large multi-file changes into separate apply_patch actions, or use a bounded shell_command edit fallback if the transport boundary keeps failing. Do not treat this as stale file context or a hunk mismatch unless a later diagnostic says so.{path_hint}"
            ));
        }
        if runtime_execution_has_apply_patch_error_code(
            execution,
            "apply_patch_payload_cap_exceeded",
        ) {
            return Some(format!(
                "Apply-patch recovery: the apply_patch payload exceeded a transport boundary. Next step: split the patch into smaller apply_patch actions by file or owner range before retrying.{path_hint} Do not reread files unless needed to build a smaller exact patch."
            ));
        }
        if runtime_execution_has_apply_patch_error_code(
            execution,
            "apply_patch_snapshot_checksum_mismatch",
        ) || runtime_execution_has_apply_patch_error_code(
            execution,
            "apply_patch_snapshot_byte_count_mismatch",
        ) {
            return Some(format!(
                "Apply-patch recovery: the apply_patch read/write snapshot verification failed, so the target changed or the captured bytes did not match before writing. Next step: inspect the affected path(s), rebuild the patch against current contents, and retry with a fresh Mezzanine patch.{path_hint}"
            ));
        }
        if runtime_execution_has_apply_patch_failure_marker(
            execution,
            "replacement_hint_next_step=skip_or_reconcile_already_applied_change",
        ) || runtime_execution_has_apply_patch_failure_marker(
            execution,
            "suggested_next_step=skip_or_reconcile_already_applied_change",
        ) {
            return Some(format!(
                "Apply-patch recovery: the mismatch diagnostic indicates the intended replacement may already be present in the current file. Next step: inspect the affected path(s) and reconcile current contents before emitting another mutation. If the intended change is already present, skip the stale hunk or report the current file state instead of replaying the same patch. If more edits are still needed, emit a smaller fresh Mezzanine *** Begin Patch block against the current file contents rather than retrying substantially the same patch.{path_hint} Use shell_command only for local inspection, path operations, validation, or raw unified diffs that apply_patch cannot express."
            ));
        }
        if runtime_execution_has_apply_patch_failure_marker(
            execution,
            "suggested_next_step=fix_or_refresh_header_anchor",
        ) {
            return Some(format!(
                "Apply-patch recovery: the mismatch diagnostic indicates the hunk header anchor was not found in order. Next step: inspect the affected path(s) around the intended owner region, then refresh or correct the @@ header anchor before emitting another mutation. Do not reuse a stale or misplaced anchor, and do not retry substantially the same patch.{path_hint} Use shell_command only for local inspection, path operations, validation, or raw unified diffs that apply_patch cannot express."
            ));
        }
        if runtime_execution_has_apply_patch_failure_marker(
            execution,
            "suggested_next_step=reread_candidate_regions",
        ) || runtime_execution_has_apply_patch_failure_marker(
            execution,
            "suggested_candidate_read_range(s):",
        ) {
            return Some(format!(
                "Apply-patch recovery: the mismatch diagnostic indicates repeated or ambiguous candidate regions in the current target file. Next step: inspect the suggested candidate range(s) or other repeated owner regions with a bounded shell_command before emitting another mutation. Do not retry substantially the same patch or focus only on one generic line range. After reading current context, emit a smaller fresh Mezzanine *** Begin Patch block with distinctive @@ header anchors for the intended region.{path_hint} Use shell_command only for local inspection, path operations, validation, or raw unified diffs that apply_patch cannot express."
            ));
        }
        if runtime_execution_has_apply_patch_hunk_mismatch(execution) {
            return Some(format!(
                "Apply-patch recovery: if a hunk did not match or the patch did not apply, the exact old-context lines were not found in the current target file or matched ambiguously; this is not necessarily a stale-file condition. Next step: first inspect the affected path(s) with a bounded shell_command, especially around any reported line number(s), before emitting another mutation. Do not retry substantially the same patch. After reading current context, emit a smaller fresh Mezzanine *** Begin Patch block against the current file contents, using distinctive @@ header anchors for repeated or ambiguous regions.{path_hint} Use shell_command only for local inspection, path operations, validation, or raw unified diffs that apply_patch cannot express."
            ));
        }
        return Some(format!(
            "Apply-patch recovery: the patch failed before a hunk-mismatch diagnosis was available. Next step: use the action result to correct the reported validation, transport, or execution precondition issue before retrying. Do not assume the current file context is stale or ambiguous unless a later diagnostic says so.{path_hint} If another attempt is needed, emit the smallest corrected Mezzanine patch or bounded inspection command justified by the reported error."
        ));
    }
    if runtime_execution_has_config_change_failure(execution) {
        return Some(
            "Config-change recovery: the configuration mutation was rejected by runtime validation. Next step: use the action result to correct the setting path, operation, or value. If the diagnostic shows the requested configuration is invalid, report that blocker instead of retrying the same invalid change."
                .to_string(),
        );
    }
    if runtime_execution_has_invalid_message_payload_failure(execution) {
        return Some(
            "Message recovery: the local message payload was rejected by protocol validation. Next step: correct the content_type and payload shape from the diagnostic, or use a plain text payload when structured delivery is not required."
                .to_string(),
        );
    }
    if runtime_execution_has_spawn_agent_failure(execution) {
        return Some(
            "Spawn-agent recovery: the subagent request was rejected before a child task was created. Next step: correct the role, placement, cooperation mode, or scope fields from the diagnostic, or continue locally if delegation is not necessary."
                .to_string(),
        );
    }
    if runtime_execution_has_pane_not_ready_shell_failure(execution) {
        return Some(
            "Shell-readiness recovery: the shell-backed action never reached the pane because Mezzanine knew the pane was not at a safe shell boundary. Use the readiness diagnostic from the failed action result to report the blockage, tell the user to exit the foreground interactive UI or return to the shell prompt, or wait for a later readiness change before retrying shell-backed work. Do not repeat the same shell_command immediately."
                .to_string(),
        );
    }
    if runtime_execution_has_redundant_skill_action_failure(execution) {
        return Some(
            "Skill recovery: the requested skill catalog or skill context is already loaded for this turn. Do not call request_skills or call_skill again merely to confirm the workflow. Use the loaded skill instructions and emit the next concrete action; if the needed action family is not currently allowed, request it with request_capability."
                .to_string(),
        );
    }
    None
}

/// Returns extra guidance for repeated failures with the same retry signature.
pub(super) fn runtime_failure_feedback_repeat_guidance(
    execution: &AgentTurnExecution,
    attempt: usize,
) -> Option<String> {
    if attempt <= 1 {
        return None;
    }
    if runtime_execution_has_apply_patch_failure(execution) && attempt >= 5 {
        return Some(
            "\nRepeated apply-patch recovery: five consecutive apply_patch failures reached the shell-edit fallback threshold. Do not emit another apply_patch action for the next mutation attempt; use a bounded shell_command with conventional file-edit tooling such as python, sed, or ed, and include a short note that this shell-edit fallback is intentional."
                .to_string(),
        );
    }
    if runtime_execution_has_apply_patch_hunk_mismatch(execution) {
        return Some(
            "\nRepeated apply-patch recovery: the same failure signature repeated. Do not emit another apply_patch action until you have read the affected target file or otherwise obtained current target context."
                .to_string(),
        );
    }
    None
}

/// Returns true when one failed execution contains a shell action that never
/// reached the pane because readiness blocked dispatch.
fn runtime_execution_has_pane_not_ready_shell_failure(execution: &AgentTurnExecution) -> bool {
    execution.action_results.iter().any(|result| {
        result.action_type == "shell_command"
            && runtime_action_result_has_error_code(result, "pane_not_ready")
    })
}

/// Builds a model-facing note for aggregated runtime loop-guard failures.
///
/// # Parameters
/// - `execution`: The failed action execution being converted into recovery
///   context.
pub(super) fn runtime_failure_feedback_loop_guard_aggregate_note(
    execution: &AgentTurnExecution,
) -> Option<String> {
    let mut lines = Vec::new();
    for (code, label) in [
        (
            "shell_dispatch_limit_exceeded",
            runtime_loop_guard_failure_label("shell_dispatch_limit_exceeded")
                .unwrap_or("shell dispatch"),
        ),
        (
            "network_action_limit_exceeded",
            runtime_loop_guard_failure_label("network_action_limit_exceeded")
                .unwrap_or("network action"),
        ),
    ] {
        let count = execution
            .action_results
            .iter()
            .filter(|result| runtime_action_result_has_error_code(result, code))
            .count();
        if count > 1 {
            lines.push(format!(
                "Aggregated loop-guard failures: {label} suppressed {count} sibling actions; one representative action result is included above."
            ));
        }
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// Builds the normal-mode status line for a queued failure-correction pass.
pub(super) fn runtime_failure_feedback_status_line(
    execution: &AgentTurnExecution,
    attempt: usize,
    attempt_limit: usize,
    unbounded_apply_patch_recovery: bool,
) -> String {
    let reason = if runtime_execution_has_apply_patch_hunk_mismatch(execution) {
        "patch hunk mismatch"
    } else if runtime_execution_has_apply_patch_failure(execution) {
        "patch failed"
    } else if runtime_execution_has_file_operation_failure(execution) {
        "file action failed"
    } else if runtime_execution_has_redundant_skill_action_failure(execution) {
        "skill context already loaded"
    } else {
        "action failure"
    };
    if unbounded_apply_patch_recovery {
        format!("agent: action failed; asking model to recover ({reason})")
    } else {
        format!(
            "agent: action failed; asking model to recover ({attempt}/{attempt_limit}, {reason})"
        )
    }
}

/// Describes why one failed execution cannot be fed back for correction.
pub(super) fn runtime_recovery_unavailable_detail(execution: &AgentTurnExecution) -> String {
    if execution.terminal_state != AgentTurnState::Failed {
        return format!(
            "turn state is {}, not failed",
            runtime_agent_turn_state_name(execution.terminal_state)
        );
    }

    let pending = execution
        .action_results
        .iter()
        .filter(|result| matches!(result.status, ActionStatus::Running | ActionStatus::Blocked))
        .map(runtime_action_result_summary)
        .collect::<Vec<_>>();
    if !pending.is_empty() {
        return format!(
            "action result(s) are still pending or blocked: {}",
            runtime_join_bounded_summaries(&pending)
        );
    }

    if !execution
        .action_results
        .iter()
        .any(|result| result.is_error)
    {
        return "no failed action result was available".to_string();
    }

    let candidates = execution
        .action_results
        .iter()
        .filter(|result| runtime_action_result_is_feedback_candidate(result))
        .count();
    if candidates == 0 {
        let non_correctable = execution
            .action_results
            .iter()
            .filter(|result| result.is_error)
            .map(runtime_action_result_summary)
            .collect::<Vec<_>>();
        return format!(
            "no model-correctable action failure was present; non-correctable result(s): {}",
            runtime_join_bounded_summaries(&non_correctable)
        );
    }

    let blockers = execution
        .action_results
        .iter()
        .filter(|result| {
            result.is_error
                && !runtime_action_result_is_feedback_candidate(result)
                && result.status != ActionStatus::Succeeded
        })
        .map(runtime_action_result_summary)
        .collect::<Vec<_>>();
    if !blockers.is_empty() {
        return format!(
            "the batch also contained non-correctable failure(s): {}",
            runtime_join_bounded_summaries(&blockers)
        );
    }

    "no eligible correction path was found".to_string()
}

/// Summarizes one action result for user-facing recovery diagnostics.
pub(super) fn runtime_action_result_summary(result: &ActionResult) -> String {
    let code = result
        .error
        .as_ref()
        .map(|error| error.code.as_str())
        .unwrap_or("no_error_code");
    format!(
        "{} {} {} {}",
        result.action_id,
        result.action_type,
        runtime_action_status_name(result.status),
        code
    )
}

/// Joins a bounded number of summaries without flooding the pane.
pub(super) fn runtime_join_bounded_summaries(summaries: &[String]) -> String {
    const SUMMARY_LIMIT: usize = 3;
    let mut joined = summaries
        .iter()
        .take(SUMMARY_LIMIT)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if summaries.len() > SUMMARY_LIMIT {
        joined.push_str(&format!(", ... ({} more)", summaries.len() - SUMMARY_LIMIT));
    }
    joined
}

/// Builds the final user-facing recovery reason for a failed turn.
///
/// # Parameters
/// - `turn_id`: The turn whose retry counters scope the failed action.
/// - `execution`: The failed turn execution.
/// - `attempt_limit`: The configured retry budget.
/// - `attempts`: The runtime retry counters keyed by failed-action signature.
pub(super) fn runtime_unrecovered_failure_reason(
    turn_id: &str,
    execution: &AgentTurnExecution,
    attempt_limit: usize,
    attempts: &BTreeMap<String, usize>,
) -> String {
    if !runtime_execution_can_feed_failure_to_model(execution) {
        return format!(
            "recovery unavailable: {}",
            runtime_recovery_unavailable_detail(execution)
        );
    }
    if runtime_execution_uses_unbounded_apply_patch_recovery(execution) {
        return "recovery unavailable: no model-correction continuation was queued after the apply_patch failure"
            .to_string();
    }
    let attempt_limit = attempt_limit.max(1);
    let attempt = runtime_failure_feedback_attempt_keys(turn_id, execution)
        .iter()
        .filter_map(|key| attempts.get(key).copied())
        .max()
        .unwrap_or(0);
    if attempt >= attempt_limit {
        format!("recovery exhausted after {attempt}/{attempt_limit} attempts")
    } else {
        format!(
            "recovery unavailable: correction budget remained ({attempt}/{attempt_limit} attempts used) but no model-correction continuation was queued"
        )
    }
}

/// Extracts unsafe path diagnostics from failed `apply_patch` actions.
pub(super) fn runtime_apply_patch_unsafe_paths(execution: &AgentTurnExecution) -> Vec<String> {
    let mut paths = Vec::new();
    for result in execution
        .action_results
        .iter()
        .filter(|result| result.action_type == "apply_patch")
    {
        let Some(output) = runtime_unrecovered_action_failure_output(result) else {
            continue;
        };
        for line in output.replace("\r\n", "\n").replace('\r', "\n").lines() {
            let trimmed = runtime_failure_output_without_prompt_prefix(line).trim();
            let Some(path) = trimmed.strip_prefix("apply_patch: unsafe patch path: ") else {
                continue;
            };
            let path = path.trim();
            if !path.is_empty() && !paths.iter().any(|existing| existing == path) {
                paths.push(path.to_string());
            }
        }
    }
    paths
}

/// Extracts affected paths from common `apply_patch` and `git apply`
/// diagnostics.
pub(super) fn runtime_apply_patch_failed_paths(execution: &AgentTurnExecution) -> Vec<String> {
    let mut paths = Vec::new();
    for result in execution
        .action_results
        .iter()
        .filter(|result| result.action_type == "apply_patch")
    {
        let Some(output) = runtime_unrecovered_action_failure_output(result) else {
            continue;
        };
        for line in output.replace("\r\n", "\n").replace('\r', "\n").lines() {
            let trimmed = runtime_failure_output_without_prompt_prefix(line).trim();
            let path = trimmed
                .strip_prefix("apply_patch: hunk did not match: ")
                .or_else(|| trimmed.strip_prefix("error: patch failed: "))
                .or_else(|| trimmed.strip_prefix("error: "))
                .and_then(runtime_patch_failure_path_from_diagnostic);
            if let Some(path) = path
                && !paths.iter().any(|existing| existing == &path)
            {
                paths.push(path);
            }
        }
    }
    paths
}

/// Parses one path-like prefix from a patch diagnostic line.
pub(super) fn runtime_patch_failure_path_from_diagnostic(text: &str) -> Option<String> {
    let candidate = text
        .split_once(": patch does not apply")
        .map(|(path, _)| path)
        .unwrap_or(text)
        .split_once(':')
        .map(|(path, _)| path)
        .unwrap_or(text)
        .trim();
    if candidate.is_empty()
        || candidate.contains(char::is_whitespace)
        || candidate.starts_with("patch")
        || candidate.starts_with("hunk ")
    {
        None
    } else {
        Some(candidate.to_string())
    }
}

/// Produces a single-line, user-facing preview for action summaries that may
/// contain multiline commands or large provider payloads.
pub(super) fn runtime_agent_terminal_preview(value: &str) -> String {
    /// Defines the MAX AGENT ACTION PREVIEW CHARS const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    const MAX_AGENT_ACTION_PREVIEW_CHARS: usize = 240;
    let trimmed = value.trim();
    let mut preview = String::new();
    let mut chars = trimmed.chars();
    for _ in 0..MAX_AGENT_ACTION_PREVIEW_CHARS {
        let Some(ch) = chars.next() else {
            return preview;
        };
        preview.push(match ch {
            '\r' | '\n' => ' ',
            ch if ch.is_control() => ' ',
            ch => ch,
        });
    }
    if chars.next().is_some() {
        preview.push_str("...");
    }
    preview
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
    match &action.payload {
        AgentActionPayload::MemorySearch { query, .. } => {
            return Some(format!(
                "memory search {}",
                runtime_agent_terminal_preview(query)
            ));
        }
        AgentActionPayload::MemoryStore { kind, .. } => {
            return Some(format!(
                "memory store {}",
                runtime_agent_terminal_preview(kind)
            ));
        }
        _ => {}
    }
    let summary = local_action_summary(action)
        .ok()
        .flatten()
        .or_else(|| network_action_summary(action).ok().flatten())?;
    let summary = runtime_agent_terminal_preview(&summary);
    (!summary.trim().is_empty()).then_some(summary)
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
    let rationale = normalize_agent_user_visible_text(&action.rationale);
    if rationale.is_empty() {
        return false;
    }
    if matches!(action.payload, AgentActionPayload::Say { .. }) {
        return true;
    }
    if !matches!(action.payload, AgentActionPayload::ShellCommand { .. })
        && let Some(summary) = runtime_agent_action_summary(action)
        && rationale == normalize_agent_user_visible_text(&summary)
    {
        return true;
    }
    match &action.payload {
        AgentActionPayload::ShellCommand { command, .. } => {
            rationale == normalize_agent_user_visible_text(command)
        }
        AgentActionPayload::Say { text, .. }
        | AgentActionPayload::RequestCapability { reason: text, .. } => {
            rationale == normalize_agent_user_visible_text(text)
        }
        AgentActionPayload::Abort { reason } => {
            rationale == normalize_agent_user_visible_text(reason)
        }
        AgentActionPayload::McpCall { .. }
        | AgentActionPayload::SendMessage { .. }
        | AgentActionPayload::SpawnAgent { .. }
        | AgentActionPayload::ConfigChange { .. }
        | AgentActionPayload::MemorySearch { .. }
        | AgentActionPayload::MemoryStore { .. }
        | AgentActionPayload::IssueAdd { .. }
        | AgentActionPayload::IssueUpdate { .. }
        | AgentActionPayload::IssueQuery { .. }
        | AgentActionPayload::IssueDelete { .. }
        | AgentActionPayload::RequestSkills
        | AgentActionPayload::CallSkill { .. } => false,
        AgentActionPayload::ApplyPatch { .. }
        | AgentActionPayload::WebSearch { .. }
        | AgentActionPayload::FetchUrl { .. } => false,
        AgentActionPayload::Complete => false,
    }
}

/// Returns normalized user-visible text emitted by conversational actions in a
/// batch. These values are already rendered as assistant output, so matching
/// rationales should not be repeated as thinking/comment lines.
pub(super) fn runtime_agent_batch_visible_action_texts(batch: &MaapBatch) -> Vec<String> {
    batch
        .actions
        .iter()
        .filter_map(|action| match &action.payload {
            AgentActionPayload::Say { text, .. } => Some(text),
            AgentActionPayload::Abort { reason } => Some(reason),
            _ => None,
        })
        .map(|text| normalize_agent_user_visible_text(text))
        .filter(|text| !text.is_empty())
        .collect()
}

/// Returns true when the batch rationale repeats text already rendered by a
/// conversational action in the same provider response.
pub(super) fn runtime_agent_batch_rationale_repeats_visible_batch_text(
    batch: &MaapBatch,
    visible_texts: &[String],
) -> bool {
    let rationale = normalize_agent_user_visible_text(&batch.rationale);
    !rationale.is_empty() && visible_texts.iter().any(|text| text == &rationale)
}

/// Returns whether the action will produce runtime-visible output after it is
/// executed rather than purely conversational assistant text.
pub(super) fn runtime_agent_action_has_runtime_visible_effect(action: &AgentAction) -> bool {
    matches!(
        action.payload,
        AgentActionPayload::ShellCommand { .. }
            | AgentActionPayload::ApplyPatch { .. }
            | AgentActionPayload::WebSearch { .. }
            | AgentActionPayload::FetchUrl { .. }
            | AgentActionPayload::McpCall { .. }
            | AgentActionPayload::RequestSkills
            | AgentActionPayload::CallSkill { .. }
            | AgentActionPayload::SendMessage { .. }
            | AgentActionPayload::SpawnAgent { .. }
            | AgentActionPayload::ConfigChange { .. }
            | AgentActionPayload::MemorySearch { .. }
            | AgentActionPayload::MemoryStore { .. }
    )
}

/// Returns whether a successful duplicate dispatch of this action should be
/// treated as a loop instead of re-running the same file mutation.
pub(super) fn runtime_agent_action_rejects_duplicate_success(action: &AgentAction) -> bool {
    matches!(action.payload, AgentActionPayload::ApplyPatch { .. })
}

/// Returns whether the guard result represents a duplicate file mutation that
/// was skipped because the identical mutation already succeeded in this turn.
pub(super) fn runtime_action_result_is_suppressed_duplicate_file_mutation(
    result: &ActionResult,
) -> bool {
    result.status == ActionStatus::Succeeded
        && result
            .structured_content_json
            .as_deref()
            .is_some_and(|content| content.contains("repeated_successful_file_mutation"))
}

/// Returns true when an action rationale repeats text already rendered by a
/// nearby conversational action in the same provider response.
pub(super) fn runtime_agent_action_rationale_repeats_visible_batch_text(
    action: &AgentAction,
    visible_texts: &[String],
) -> bool {
    let rationale = normalize_agent_user_visible_text(&action.rationale);
    !rationale.is_empty() && visible_texts.iter().any(|text| text == &rationale)
}

/// Runs the normalize agent user visible text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn normalize_agent_user_visible_text(value: &str) -> String {
    let trimmed = value.trim_start();
    trimmed
        .strip_prefix("agent thinking:")
        .or_else(|| trimmed.strip_prefix("thinking:"))
        .map(str::trim_start)
        .unwrap_or(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// Returns the short action kind and target that should be visible when runtime
/// gates prevent an agent-planned action from reaching its normal execution UI.
pub(super) fn runtime_agent_user_action_phrase(
    action: &AgentAction,
) -> Option<(&'static str, String)> {
    if let Ok(Some(plan)) = local_action_plan(action) {
        let kind = if matches!(action.payload, AgentActionPayload::ShellCommand { .. }) {
            "shell command"
        } else {
            "local action"
        };
        return Some((kind, runtime_agent_terminal_preview(&plan.command)));
    }
    if let Ok(Some(plan)) = network_action_plan(action) {
        let kind = match action.payload {
            AgentActionPayload::WebSearch { .. } => "web search",
            AgentActionPayload::FetchUrl { .. } => "URL fetch",
            _ => "network action",
        };
        return Some((kind, runtime_agent_terminal_preview(&plan.policy_command)));
    }
    match &action.payload {
        AgentActionPayload::McpCall { server, tool, .. } => Some((
            "MCP call",
            format!(
                "{}/{}",
                runtime_agent_terminal_preview(server),
                runtime_agent_terminal_preview(tool)
            ),
        )),
        AgentActionPayload::SendMessage { recipient, .. } => {
            Some(("message", runtime_agent_terminal_preview(recipient)))
        }
        AgentActionPayload::SpawnAgent { role, .. } => {
            Some(("subagent spawn", runtime_agent_terminal_preview(role)))
        }
        AgentActionPayload::ConfigChange {
            setting_path,
            operation,
            ..
        } => Some((
            "config change",
            format!(
                "{} {}",
                runtime_agent_terminal_preview(operation),
                runtime_agent_terminal_preview(setting_path)
            ),
        )),
        AgentActionPayload::MemorySearch { query, .. } => {
            Some(("memory search", runtime_agent_terminal_preview(query)))
        }
        AgentActionPayload::MemoryStore { kind, .. } => {
            Some(("memory store", runtime_agent_terminal_preview(kind)))
        }
        AgentActionPayload::IssueAdd { title, .. } => {
            Some(("issue add", runtime_agent_terminal_preview(title)))
        }
        AgentActionPayload::IssueUpdate { id, .. } => {
            Some(("issue update", runtime_agent_terminal_preview(id)))
        }
        AgentActionPayload::IssueQuery { text, .. } => Some((
            "issue query",
            text.as_deref()
                .map(runtime_agent_terminal_preview)
                .unwrap_or_else(|| "current project".to_string()),
        )),
        AgentActionPayload::IssueDelete { id } => {
            Some(("issue delete", runtime_agent_terminal_preview(id)))
        }
        AgentActionPayload::RequestSkills => Some(("skill lookup", "available skills".to_string())),
        AgentActionPayload::CallSkill { name, .. } => {
            Some(("skill load", runtime_agent_terminal_preview(name)))
        }
        AgentActionPayload::Say { .. }
        | AgentActionPayload::RequestCapability { .. }
        | AgentActionPayload::Complete
        | AgentActionPayload::Abort { .. }
        | AgentActionPayload::ShellCommand { .. }
        | AgentActionPayload::ApplyPatch { .. }
        | AgentActionPayload::WebSearch { .. }
        | AgentActionPayload::FetchUrl { .. } => None,
    }
}

/// Formats the error suffix for a failed action result without exposing
/// multiline provider payloads directly in the status prefix.
pub(super) fn runtime_agent_action_error_suffix(result: &ActionResult) -> String {
    let detail = result
        .error
        .as_ref()
        .map(|error| {
            if error.code.trim().is_empty() {
                error.message.clone()
            } else if error.message.trim().is_empty() {
                error.code.clone()
            } else {
                format!("{}: {}", error.code, error.message)
            }
        })
        .or_else(|| {
            let content = result.content_text();
            (!content.trim().is_empty()).then_some(content)
        })
        .map(|detail| runtime_agent_terminal_preview(&detail))
        .unwrap_or_default();
    if detail.is_empty() {
        String::new()
    } else {
        format!(" ({detail})")
    }
}

/// Builds a terse warning for fetch failures that are already being fed back to
/// the model for bounded correction.
pub(super) fn runtime_agent_recoverable_network_warning_line(
    action: &AgentAction,
    result: &ActionResult,
) -> Option<String> {
    if !matches!(action.payload, AgentActionPayload::FetchUrl { .. })
        || !runtime_action_result_is_feedback_candidate(result)
    {
        return None;
    }
    let detail = runtime_fetch_url_status_label(result)
        .or_else(|| {
            result
                .error
                .as_ref()
                .map(|error| runtime_agent_terminal_preview(&error.message))
        })
        .filter(|detail| !detail.trim().is_empty())
        .map(|detail| format!(" ({detail})"))
        .unwrap_or_default();
    Some(format!(
        "agent warning: URL fetch failed{detail}; model received the response details for recovery"
    ))
}

/// Returns a compact HTTP status label from fetch structured content.
pub(super) fn runtime_fetch_url_status_label(result: &ActionResult) -> Option<String> {
    let value: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_deref()?).ok()?;
    let status = value
        .get("response")
        .and_then(|response| response.get("status_code"))
        .and_then(serde_json::Value::as_u64)?;
    Some(format!("HTTP {status}"))
}

/// Builds the model-facing wrapper for mid-turn user steering input.
pub(super) fn runtime_agent_turn_steering_context_content(
    steering: &RuntimeAgentTurnSteering,
) -> String {
    format!(
        "[user steering input during active turn]\n\
submitted_at_unix_seconds={}\n\
The user added this instruction while the current turn was already in progress.\n\
Incorporate it into the current task from this point forward. Do not restart\n\
completed work unless necessary. If this conflicts with earlier instructions,\n\
the newer user instruction takes precedence.\n\n\
User input:\n{}",
        steering.submitted_at_unix_seconds, steering.input
    )
}

/// Builds a concise default-visible line for a runtime action that could not
/// reach its usual visible execution path.
pub(super) fn runtime_agent_action_outcome_line(
    action: &AgentAction,
    result: &ActionResult,
    show_shell_details: bool,
) -> Option<(bool, String)> {
    let (kind, target) = runtime_agent_user_action_phrase(action)?;
    let runtime_owned_action = local_action_plan(action).ok().flatten().is_some()
        || network_action_plan(action).ok().flatten().is_some();
    let target = if runtime_owned_action && !show_shell_details {
        None
    } else {
        Some(target)
    };
    match result.status {
        ActionStatus::Blocked => Some((
            false,
            if let Some(summary) = runtime_agent_action_summary(action).filter(|_| target.is_none())
            {
                format!("agent: {summary} (awaiting approval)")
            } else if let Some(target) = target {
                format!("agent: {kind} awaiting approval: {target}")
            } else {
                format!("agent: {kind} awaiting approval")
            },
        )),
        ActionStatus::Rejected
        | ActionStatus::Denied
        | ActionStatus::Failed
        | ActionStatus::Cancelled
        | ActionStatus::TimedOut
        | ActionStatus::Interrupted => {
            if let Some(line) = runtime_agent_recoverable_network_warning_line(action, result) {
                return Some((false, line));
            }
            let detail = runtime_agent_action_error_suffix(result);
            let failure_phase = if network_action_plan(action).ok().flatten().is_some()
                && result.content.is_empty()
            {
                ""
            } else {
                " before execution"
            };
            Some((
                true,
                if let Some(summary) =
                    runtime_agent_action_summary(action).filter(|_| target.is_none())
                {
                    format!(
                        "agent: {summary} ({kind} {}{failure_phase}{detail})",
                        runtime_action_status_name(result.status)
                    )
                } else if let Some(target) = target {
                    format!(
                        "agent: {kind} {}{failure_phase}: {target}{detail}",
                        runtime_action_status_name(result.status),
                    )
                } else {
                    format!(
                        "agent: {kind} {}{failure_phase}{detail}",
                        runtime_action_status_name(result.status),
                    )
                },
            ))
        }
        ActionStatus::Running | ActionStatus::Succeeded => None,
    }
}

/// Returns true when an action result represents a terminal failure that may
/// need a final user-facing diagnostic if recovery is unavailable.
pub(super) fn runtime_action_result_is_terminal_failure(result: &ActionResult) -> bool {
    matches!(
        result.status,
        ActionStatus::Rejected
            | ActionStatus::Denied
            | ActionStatus::Failed
            | ActionStatus::Cancelled
            | ActionStatus::TimedOut
            | ActionStatus::Interrupted
    )
}

/// Returns output detail worth showing when a failed action is no longer being
/// fed back to the model for correction.
pub(super) fn runtime_unrecovered_action_failure_output(result: &ActionResult) -> Option<String> {
    let content = result.content_text();
    if !content.trim().is_empty() && !runtime_action_failure_content_is_generic_status(&content) {
        return Some(content);
    }
    runtime_action_result_structured_string(
        result,
        &["terminal_observation", "combined_output_preview"],
    )
    .filter(|output| !output.trim().is_empty())
}

/// Returns true when result content merely repeats the compact status already
/// present in the action result.
pub(super) fn runtime_action_failure_content_is_generic_status(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed == "shell command accepted for pane execution"
        || trimmed.starts_with("shell command exited with status ")
        || trimmed == "shell command timed out"
        || trimmed == "shell command was interrupted"
}

/// Extracts a string from nested action-result structured content.
pub(super) fn runtime_action_result_structured_string(
    result: &ActionResult,
    path: &[&str],
) -> Option<String> {
    let mut value: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_deref()?).ok()?;
    for key in path {
        value = value.get(*key)?.clone();
    }
    value.as_str().map(str::to_string)
}

/// Builds bounded, sanitized diagnostic output lines for final failed actions.
pub(in crate::runtime) fn runtime_unrecovered_failure_output_lines(
    action: &AgentAction,
    output: &str,
) -> Vec<String> {
    const MAX_FAILURE_DIAGNOSTIC_LINES: usize = 12;
    const MAX_FAILURE_DIAGNOSTIC_BYTES: usize = 4 * 1024;

    let normalized = output
        .trim_end_matches(['\r', '\n'])
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut candidates = Vec::new();
    for line in normalized.lines() {
        let line = runtime_failure_output_without_prompt_prefix(line);
        let line = runtime_sanitized_failure_output_line(line);
        let trimmed = line.trim();
        if trimmed.is_empty()
            || runtime_failure_output_line_is_wrapper_noise(trimmed)
            || runtime_failure_output_line_is_generated_action_echo(action, trimmed)
        {
            continue;
        }
        candidates.push(line.trim_end().to_string());
    }

    if runtime_action_is_generated_semantic_shell_action(action) {
        let diagnostic_lines: Vec<String> = candidates
            .iter()
            .filter(|line| runtime_failure_output_line_looks_like_diagnostic(line))
            .cloned()
            .collect();
        if !diagnostic_lines.is_empty() {
            candidates = diagnostic_lines;
        } else if !normalized.trim().is_empty() {
            candidates = vec![runtime_generic_semantic_failure_diagnostic(action)];
        }
    }

    if candidates.is_empty() && !normalized.trim().is_empty() {
        candidates.push("[mez: failure output contained only shell wrapper traffic]".to_string());
    }

    let mut lines = Vec::new();
    let mut used_bytes = 0usize;
    let mut truncated = false;
    for mut line in candidates {
        if lines.len() >= MAX_FAILURE_DIAGNOSTIC_LINES {
            truncated = true;
            break;
        }
        let remaining = MAX_FAILURE_DIAGNOSTIC_BYTES.saturating_sub(used_bytes);
        if remaining == 0 {
            truncated = true;
            break;
        }
        if line.len() > remaining {
            line = runtime_truncate_to_utf8_boundary(&line, remaining);
            line.push_str("...");
            truncated = true;
            lines.push(line);
            break;
        }
        used_bytes = used_bytes.saturating_add(line.len()).saturating_add(1);
        lines.push(line);
    }
    if truncated {
        lines.push("[mez: failure output truncated for pane display]".to_string());
    }
    lines
}

/// Removes prompt glyphs and prompt prefixes from one captured failure line.
pub(super) fn runtime_failure_output_without_prompt_prefix(line: &str) -> &str {
    let mut remaining = line;
    loop {
        let trimmed = remaining.trim_start();
        if let Some(next) = trimmed.strip_prefix("$ ") {
            remaining = next;
            continue;
        }
        if let Some(next) = trimmed.strip_prefix("> ") {
            remaining = next;
            continue;
        }
        if let Some(next) = trimmed.strip_prefix("# ") {
            remaining = next;
            continue;
        }
        if let Some(next) = trimmed.strip_prefix("∙") {
            remaining = next;
            continue;
        }
        if let Some(next) = trimmed.strip_prefix("") {
            remaining = next;
            continue;
        }
        return remaining;
    }
}

/// Returns true for shell harness traffic that should not appear in final
/// unrecovered failure diagnostics.
pub(super) fn runtime_failure_output_line_is_wrapper_noise(trimmed: &str) -> bool {
    trimmed == ""
        || trimmed == "∙"
        || trimmed == "done"
        || trimmed.starts_with("__mez_tx_")
        || trimmed.starts_with("unset -f __mez_tx_")
        || trimmed.starts_with("MEZ_MARKER_TOKEN=")
        || trimmed.starts_with("MEZ_TURN=")
        || trimmed.starts_with("MEZ_AGENT=")
        || trimmed.starts_with("MEZ_PANE=")
        || trimmed.starts_with("MEZ_STTY_STATE=")
        || trimmed.starts_with("MEZ_PATCH=")
        || trimmed.starts_with("MEZ_PATCH_SCRIPT=")
        || trimmed.starts_with("MEZ_OLD=")
        || trimmed.starts_with("MEZ_NEW=")
        || trimmed.starts_with("MEZ_EDIT_SCRIPT=")
        || trimmed.starts_with("MEZ_PYTHON=")
        || trimmed.starts_with("MEZ_STATUS=")
        || trimmed.starts_with("MEZ_RESTORE_")
        || trimmed.starts_with("MEZ_HISTORY_")
        || trimmed.starts_with("HISTFILE=/dev/null")
        || trimmed.starts_with("MEZ_COMMAND_")
        || trimmed.starts_with("unset MEZ_")
        || trimmed.starts_with("set +o history")
        || trimmed.starts_with("set -o history")
        || trimmed.starts_with("history -d ")
        || trimmed.starts_with("command printf '\\033]133")
        || trimmed.starts_with("printf '\\033]133")
        || trimmed.starts_with("command env -u MEZ_MARKER_TOKEN")
        || trimmed.starts_with("env -u MEZ_MARKER_TOKEN")
        || trimmed.contains("mez_marker=")
}

/// Returns true for generated command echo lines that are wrapper details, not
/// the action failure itself.
pub(super) fn runtime_failure_output_line_is_generated_action_echo(
    action: &AgentAction,
    trimmed: &str,
) -> bool {
    let is_shell_wrapper_command = trimmed.starts_with("MEZ_PATCH=")
        || trimmed.starts_with("MEZ_PATCH_SCRIPT=")
        || trimmed.starts_with("MEZ_OLD=")
        || trimmed.starts_with("MEZ_NEW=")
        || trimmed.starts_with("MEZ_EDIT_SCRIPT=")
        || trimmed.starts_with("MEZ_PYTHON=")
        || trimmed.starts_with("git apply -- \"$MEZ_PATCH\"")
        || trimmed.starts_with("\"$MEZ_PYTHON\" \"$MEZ_PATCH_SCRIPT\"")
        || trimmed.starts_with("\"$MEZ_PYTHON\" \"$MEZ_EDIT_SCRIPT\"")
        || trimmed.starts_with("rm -f -- \"$MEZ_PATCH\"")
        || trimmed.starts_with("rm -f -- \"$MEZ_BEFORE\" \"$MEZ_OLD\"")
        || trimmed.starts_with("exit \"$MEZ_STATUS\"")
        || trimmed.starts_with("printf %s '*** ")
        || trimmed.starts_with("printf '%s' '*** ");

    if is_shell_wrapper_command {
        return true;
    }

    runtime_action_is_apply_patch(action)
        && (trimmed.starts_with("*** Begin Patch")
            || trimmed.starts_with("*** Update File:")
            || trimmed.starts_with("*** Add File:")
            || trimmed.starts_with("*** Delete File:")
            || trimmed.starts_with("*** Move to:")
            || trimmed.starts_with("*** End Patch")
            || trimmed.starts_with("*** End of File")
            || trimmed.starts_with("@@"))
}

/// Returns true when an action is a Mezzanine patch action.
pub(super) fn runtime_action_is_apply_patch(action: &AgentAction) -> bool {
    matches!(&action.payload, AgentActionPayload::ApplyPatch { .. })
}

/// Returns true when the failed action was lowered into a Mezzanine-generated
/// shell command whose wrapper echo is not useful final diagnostic output.
pub(super) fn runtime_action_is_generated_semantic_shell_action(action: &AgentAction) -> bool {
    matches!(&action.payload, AgentActionPayload::ApplyPatch { .. })
}

/// Returns a concise fallback diagnostic for generated semantic actions when
/// the captured output contains wrapper fragments but no actionable error line.
pub(super) fn runtime_generic_semantic_failure_diagnostic(action: &AgentAction) -> String {
    match &action.payload {
        AgentActionPayload::ApplyPatch { .. } => {
            "apply_patch failed without an actionable patch diagnostic. Next step: inspect the current target file with a bounded shell_command, then retry with a smaller fresh Mezzanine *** Begin Patch block."
                .to_string()
        }
        _ => "[mez: failure output contained only shell wrapper traffic]".to_string(),
    }
}

/// Returns true when a captured failure line looks like the actionable
/// diagnostic, not command echo.
pub(super) fn runtime_failure_output_line_looks_like_diagnostic(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("apply_patch:")
        || lower.contains("error:")
        || lower.contains("fatal:")
        || lower.contains("failed")
        || lower.contains("hunk did not match")
        || lower.contains("no valid patches")
        || lower.contains("patch failed")
        || lower.contains("cannot ")
        || lower.contains("missing ")
        || lower.contains("no such ")
        || lower.contains("not ")
        || lower.contains("required")
        || lower.contains("invalid")
        || lower.contains("unsupported")
        || lower.contains("denied")
        || lower.contains("mismatch")
        || lower.contains("unsafe")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("exceeded")
        || lower.contains("refusing ")
}

/// Removes terminal control bytes from one final failure diagnostic line.
pub(super) fn runtime_sanitized_failure_output_line(line: &str) -> String {
    line.chars()
        .map(|ch| {
            if ch == '\t' || !ch.is_control() {
                ch
            } else {
                ' '
            }
        })
        .collect()
}

/// Truncates text to a UTF-8 boundary at or below the requested byte limit.
pub(super) fn runtime_truncate_to_utf8_boundary(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}
