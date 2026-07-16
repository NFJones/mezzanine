//! Agent turn outcome, failure, and recovery-output helpers.
//!
//! This module owns provider-completion validation, action-result failure
//! classification, model-recovery guidance, and terminal-safe failure output
//! shaping for runtime agent turns. Keeping these helpers outside the service
//! facade leaves the facade focused on state transitions and dispatch.

use super::*;

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

/// Returns whether an action carries enough model-authored context for auto-allow.
pub(super) fn runtime_action_supports_auto_allow(action: &AgentAction) -> bool {
    if !action.rationale.trim().is_empty() {
        return true;
    }
    local_action_summary(action)
        .ok()
        .flatten()
        .or_else(|| network_action_summary(action))
        .is_some_and(|summary| !summary.trim().is_empty())
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
        .or_else(|| network_action_summary(action))?;
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
    if let Some(plan) = network_action_plan(action) {
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
    let runtime_owned_action =
        local_action_plan(action).ok().flatten().is_some() || network_action_plan(action).is_some();
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
            let failure_phase =
                if network_action_plan(action).is_some() && result.content.is_empty() {
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
