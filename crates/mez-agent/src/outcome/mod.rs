//! Provider-independent completion validation and failure recovery policy.
//!
//! This module owns completion consistency, action-failure classification,
//! retry signatures, recovery guidance, bounded diagnostics, and neutral
//! user-facing action summaries. Product runtime mutation, clocks, persistence,
//! audit envelopes, and terminal application remain in the root adapter.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use crate::permissions::{DEFAULT_COMMAND_SHELL_CLASSIFICATION, exact_command_sha256};
use crate::*;

/// Failure returned by provider-completion consistency validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeError {
    message: String,
}

impl OutcomeError {
    /// Creates an invalid completion-state error.
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the diagnostic message for product error projection.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for OutcomeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for OutcomeError {}

/// Result returned by completion consistency validation.
pub type OutcomeResult<T> = Result<T, OutcomeError>;

mod presentation;

pub use presentation::*;

/// Returns the stable state name used by neutral recovery diagnostics.
fn outcome_turn_state_name(state: AgentTurnState) -> &'static str {
    match state {
        AgentTurnState::Queued => "queued",
        AgentTurnState::Running => "running",
        AgentTurnState::Blocked => "blocked",
        AgentTurnState::Completed => "completed",
        AgentTurnState::Failed => "failed",
        AgentTurnState::Interrupted => "interrupted",
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
pub fn runtime_validate_provider_completion_identity(
    turn: &AgentTurnRecord,
    agent_id: &str,
    turn_id: &str,
    execution: &AgentTurnExecution,
) -> OutcomeResult<()> {
    if turn.turn_id != turn_id || execution.request.turn_id != turn_id {
        return Err(OutcomeError::invalid_state(
            "agent provider completion turn id does not match active turn",
        ));
    }
    if turn.agent_id != agent_id || execution.request.agent_id != agent_id {
        return Err(OutcomeError::invalid_state(
            "agent provider completion agent id does not match active turn",
        ));
    }
    if let Some(batch) = execution.response.action_batch.as_ref()
        && (batch.turn_id != turn_id || batch.agent_id != agent_id)
    {
        return Err(OutcomeError::invalid_state(
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
pub fn runtime_validate_provider_completion_execution(
    turn: &AgentTurnRecord,
    execution: &mut AgentTurnExecution,
) -> OutcomeResult<()> {
    let Some(batch) = execution.response.action_batch.as_ref() else {
        if runtime_execution_is_missing_batch_terminal_failure(execution) {
            return Ok(());
        }
        if !execution.action_results.is_empty() {
            return Err(OutcomeError::invalid_state(
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
        return Err(OutcomeError::invalid_state(
            "agent provider completion action batch protocol is unsupported",
        ));
    }
    if batch.rationale.trim().is_empty() {
        return Err(OutcomeError::invalid_state(
            "agent provider completion action batch rationale is empty",
        ));
    }
    if batch.actions.is_empty() && !batch.final_turn {
        return Err(OutcomeError::invalid_state(
            "agent provider completion action batch has no actions but is not final",
        ));
    }
    if batch.final_turn != execution.final_turn && !controller_terminal_failure {
        return Err(OutcomeError::invalid_state(
            "agent provider completion final flag does not match action batch",
        ));
    }
    let mut action_types = BTreeMap::new();
    for action in &batch.actions {
        if action.id.trim().is_empty() {
            return Err(OutcomeError::invalid_state(
                "agent provider completion action batch contains an empty action id",
            ));
        }
        if action_types
            .insert(action.id.clone(), action.action_type())
            .is_some()
        {
            return Err(OutcomeError::invalid_state(
                "agent provider completion action batch contains duplicate action ids",
            ));
        }
    }
    let mut result_ids = BTreeSet::new();
    for result in &execution.action_results {
        result
            .validate_invariants()
            .map_err(|error| OutcomeError::invalid_state(error.to_string()))?;
        if result.protocol != "maap/1" {
            return Err(OutcomeError::invalid_state(
                "agent provider completion action result protocol is unsupported",
            ));
        }
        if result.turn_id != turn.turn_id || result.agent_id != turn.agent_id {
            return Err(OutcomeError::invalid_state(
                "agent provider completion action result identity does not match active turn",
            ));
        }
        if !result_ids.insert(result.action_id.clone()) {
            return Err(OutcomeError::invalid_state(
                "agent provider completion contains duplicate action results",
            ));
        }
        let Some(action_type) = action_types.get(&result.action_id) else {
            return Err(OutcomeError::invalid_state(
                "agent provider completion action result does not match an action",
            ));
        };
        if action_type != &result.action_type {
            return Err(OutcomeError::invalid_state(
                "agent provider completion action result type does not match action",
            ));
        }
    }
    if action_types.len() != result_ids.len() && !controller_terminal_failure {
        return Err(OutcomeError::invalid_state(
            "agent provider completion action batch and result counts differ",
        ));
    }
    let expected_state =
        turn_state_from_action_results(&execution.action_results, execution.final_turn);
    if execution.terminal_state != expected_state && !controller_terminal_failure {
        return Err(OutcomeError::invalid_state(
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
pub fn runtime_execution_is_missing_batch_terminal_failure(execution: &AgentTurnExecution) -> bool {
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
pub fn runtime_execution_is_controller_failure_summary(
    execution: &AgentTurnExecution,
    batch: &MaapBatch,
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
pub fn runtime_execution_is_controller_validation_failure(execution: &AgentTurnExecution) -> bool {
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
pub fn runtime_provider_audit_error_message(message: &str) -> String {
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

/// Runs the runtime action status name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn runtime_action_status_name(status: ActionStatus) -> &'static str {
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
pub fn runtime_action_result_is_feedback_candidate(result: &ActionResult) -> bool {
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
pub fn runtime_action_result_is_runtime_infrastructure_failure(result: &ActionResult) -> bool {
    let Some(error) = result.error.as_ref() else {
        return false;
    };
    let message = error.message.to_ascii_lowercase();
    message.contains("pane process not found")
        || (error.code == "not_found" && message.starts_with("shell_dispatch:"))
}

/// Returns true when one action type is authored by the model and can usually
/// be retried with corrected parameters or a different action choice.
pub fn runtime_action_type_is_model_correctable(action_type: &str) -> bool {
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
pub fn runtime_action_type_is_shell_backed(action_type: &str) -> bool {
    matches!(action_type, "shell_command" | "apply_patch")
}

/// Returns true when an error represents a policy or user boundary rather than
/// evidence the model can use to correct its own action.
pub fn runtime_error_code_is_non_correctable(error_code: &str) -> bool {
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
pub fn runtime_invalid_params_is_feedback_candidate(result: &ActionResult) -> bool {
    runtime_action_type_is_model_correctable(result.action_type)
}

/// Runtime-visible skill context already loaded into one active turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeSkillActionContext {
    /// Whether a successful skill catalog result is already present.
    pub catalog_requested: bool,
    /// Skill names whose full `SKILL.md` text is already in context.
    pub loaded_skills: BTreeSet<String>,
}

/// Extracts loaded skill state from active model-context blocks.
///
/// This intentionally inspects only explicit context labels and action-result
/// text that the runtime itself produced. It does not parse arbitrary repository
/// files or shell output as authoritative skill state.
pub fn runtime_skill_action_context_from_blocks(
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
pub fn runtime_execution_can_feed_failure_to_model(execution: &AgentTurnExecution) -> bool {
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
pub fn runtime_failure_feedback_attempt_keys(
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
pub fn runtime_failure_feedback_attempt_key_for_result(
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
pub fn runtime_execution_uses_unbounded_apply_patch_recovery(
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
pub fn runtime_failure_feedback_action_signature(result: &ActionResult) -> String {
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
pub fn runtime_action_result_error_code(result: &ActionResult) -> Option<&str> {
    result.error.as_ref().map(|error| error.code.as_str())
}

/// Returns true when one action result carries the requested error code.
///
/// # Parameters
/// - `result`: The action result being inspected.
/// - `code`: The machine-readable error code to match.
pub fn runtime_action_result_has_error_code(result: &ActionResult, code: &str) -> bool {
    runtime_action_result_error_code(result).is_some_and(|error_code| error_code == code)
}

/// Returns true when the result represents a batch-level loop guard failure
/// where showing every sibling failure would flood the pane and model context.
///
/// # Parameters
/// - `result`: The action result being inspected.
pub fn runtime_action_result_is_aggregated_loop_guard_failure(result: &ActionResult) -> bool {
    let _ = result;
    false
}

/// Builds one bounded pane line for a group of loop guard failures.
///
/// # Parameters
/// - `label`: The human-readable guard label.
/// - `count`: The number of failed sibling actions represented by the line.
/// - `message`: The shared runtime guard diagnostic.
pub fn runtime_loop_guard_failure_summary_line(label: &str, count: usize, message: &str) -> String {
    let action_word = if count == 1 { "action" } else { "actions" };
    format!(
        "agent: {label} limit reached; suppressed {count} {action_word} ({})",
        outcome_terminal_preview(message)
    )
}

/// Produces a bounded single-line preview for neutral recovery diagnostics.
fn outcome_terminal_preview(value: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 240;
    let mut preview = value
        .trim()
        .chars()
        .take(MAX_PREVIEW_CHARS)
        .map(|character| {
            if matches!(character, '\r' | '\n') || character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if value.trim().chars().count() > MAX_PREVIEW_CHARS {
        preview.push_str("...");
    }
    preview
}

/// Returns the display label for an aggregated loop-guard error code.
///
/// # Parameters
/// - `code`: The machine-readable error code.
pub fn runtime_loop_guard_failure_label(code: &str) -> Option<&'static str> {
    match code {
        "shell_dispatch_limit_exceeded" => Some("shell dispatch"),
        "network_action_limit_exceeded" => Some("network action"),
        _ => None,
    }
}

/// Returns true when a failed execution includes an `apply_patch` failure.
pub fn runtime_execution_has_apply_patch_failure(execution: &AgentTurnExecution) -> bool {
    execution
        .action_results
        .iter()
        .any(|result| result.is_error && result.action_type == "apply_patch")
}

/// Returns true when a failed execution includes an `apply_patch` hunk
/// mismatch.
pub fn runtime_execution_has_apply_patch_hunk_mismatch(execution: &AgentTurnExecution) -> bool {
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
pub fn runtime_execution_has_config_change_failure(execution: &AgentTurnExecution) -> bool {
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
pub fn runtime_execution_has_invalid_message_payload_failure(
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
pub fn runtime_execution_has_spawn_agent_failure(execution: &AgentTurnExecution) -> bool {
    execution.action_results.iter().any(|result| {
        result.is_error
            && result.action_type == "spawn_agent"
            && matches!(result.status, ActionStatus::Failed | ActionStatus::Denied)
    })
}

/// Returns true when the model tried to rediscover or reload already-present
/// skill context.
pub fn runtime_execution_has_redundant_skill_action_failure(
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
pub fn runtime_execution_has_failed_action_type(
    execution: &AgentTurnExecution,
    action_types: &[&str],
) -> bool {
    execution
        .action_results
        .iter()
        .any(|result| result.is_error && action_types.contains(&result.action_type))
}

/// Returns true when a failed execution includes a failed filesystem mutation.
pub fn runtime_execution_has_mutation_failure(execution: &AgentTurnExecution) -> bool {
    runtime_execution_has_failed_action_type(execution, &["apply_patch"])
}

/// Returns true when a failed execution includes a shell-backed file operation.
pub fn runtime_execution_has_file_operation_failure(execution: &AgentTurnExecution) -> bool {
    runtime_execution_has_failed_action_type(execution, &["apply_patch"])
}

/// Builds model-facing recovery guidance that prevents unsupported success claims.
pub fn runtime_failure_feedback_evidence_guidance(
    execution: &AgentTurnExecution,
) -> Option<&'static str> {
    runtime_execution_has_mutation_failure(execution).then_some(
        "Mutation-evidence rule: no successful mutation has occurred after the failed action(s) in this recovery context. Do not claim the task is implemented, changed, updated, fixed, applied, run, or executed until a later mutation action succeeds and you verify it. Reads, git status, and git diff after a failed mutation prove only current file state; they do not prove your attempted edit landed. If current files appear changed without mutation-action proof, say the current file/diff shows that state rather than claiming you performed the change.",
    )
}

/// Builds extra model-facing recovery guidance for known failure modes.
pub fn runtime_failure_feedback_specific_guidance(
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
pub fn runtime_failure_feedback_repeat_guidance(
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
pub fn runtime_failure_feedback_loop_guard_aggregate_note(
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
pub fn runtime_failure_feedback_status_line(
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
pub fn runtime_recovery_unavailable_detail(execution: &AgentTurnExecution) -> String {
    if execution.terminal_state != AgentTurnState::Failed {
        return format!(
            "turn state is {}, not failed",
            outcome_turn_state_name(execution.terminal_state)
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
pub fn runtime_action_result_summary(result: &ActionResult) -> String {
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
pub fn runtime_join_bounded_summaries(summaries: &[String]) -> String {
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
pub fn runtime_unrecovered_failure_reason(
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
pub fn runtime_apply_patch_unsafe_paths(execution: &AgentTurnExecution) -> Vec<String> {
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
pub fn runtime_apply_patch_failed_paths(execution: &AgentTurnExecution) -> Vec<String> {
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
pub fn runtime_patch_failure_path_from_diagnostic(text: &str) -> Option<String> {
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

/// Returns true when an action result represents a terminal failure that may
/// need a final user-facing diagnostic if recovery is unavailable.
pub fn runtime_action_result_is_terminal_failure(result: &ActionResult) -> bool {
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
pub fn runtime_unrecovered_action_failure_output(result: &ActionResult) -> Option<String> {
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
pub fn runtime_action_failure_content_is_generic_status(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed == "shell command accepted for pane execution"
        || trimmed.starts_with("shell command exited with status ")
        || trimmed == "shell command timed out"
        || trimmed == "shell command was interrupted"
}

/// Extracts a string from nested action-result structured content.
pub fn runtime_action_result_structured_string(
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
pub fn runtime_unrecovered_failure_output_lines(action: &AgentAction, output: &str) -> Vec<String> {
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
pub fn runtime_failure_output_without_prompt_prefix(line: &str) -> &str {
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
pub fn runtime_failure_output_line_is_wrapper_noise(trimmed: &str) -> bool {
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
pub fn runtime_failure_output_line_is_generated_action_echo(
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
pub fn runtime_action_is_apply_patch(action: &AgentAction) -> bool {
    matches!(&action.payload, AgentActionPayload::ApplyPatch { .. })
}

/// Returns true when the failed action was lowered into a Mezzanine-generated
/// shell command whose wrapper echo is not useful final diagnostic output.
pub fn runtime_action_is_generated_semantic_shell_action(action: &AgentAction) -> bool {
    matches!(&action.payload, AgentActionPayload::ApplyPatch { .. })
}

/// Returns a concise fallback diagnostic for generated semantic actions when
/// the captured output contains wrapper fragments but no actionable error line.
pub fn runtime_generic_semantic_failure_diagnostic(action: &AgentAction) -> String {
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
pub fn runtime_failure_output_line_looks_like_diagnostic(line: &str) -> bool {
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
pub fn runtime_sanitized_failure_output_line(line: &str) -> String {
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
pub fn runtime_truncate_to_utf8_boundary(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one active turn fixture for completion validation.
    fn turn() -> AgentTurnRecord {
        AgentTurnRecord {
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            pane_id: "%1".to_string(),
            trigger: AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: 1,
            policy_profile: "default".to_string(),
            model_profile: "default".to_string(),
            parent_turn_id: None,
            state: AgentTurnState::Running,
            cooperation_mode: None,
            initial_capability: None,
        }
    }

    /// Builds one result-free provider execution fixture.
    fn execution() -> AgentTurnExecution {
        AgentTurnExecution {
            request: ModelRequest {
                provider: "test".to_string(),
                model: "test-model".to_string(),
                reasoning_effort: None,
                thinking_enabled: None,
                latency_preference: None,
                prompt_cache_retention: None,
                max_output_tokens: None,
                temperature: None,
                prompt_cache_session_id: None,
                prompt_cache_lineage_id: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-1".to_string(),
                available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
                issue_actions_enabled: false,
                interaction_kind: ModelInteractionKind::ActionExecution,
                allowed_actions: AllowedActionSet::say_only(),
                stop: None,
                messages: Vec::new(),
            },
            response: ModelResponse {
                provider: "test".to_string(),
                model: "test-model".to_string(),
                raw_text: "provider returned no action batch".to_string(),
                usage: ModelTokenUsage::default(),
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: ModelTokenUsage::default(),
            routing_token_usage_by_model: BTreeMap::new(),
            action_results: Vec::new(),
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        }
    }

    /// Builds one shell action used to exercise neutral presentation policy.
    fn shell_action() -> AgentAction {
        AgentAction {
            id: "shell-1".to_string(),
            rationale: "Inspect repository".to_string(),
            payload: AgentActionPayload::ShellCommand {
                summary: "Inspect repository".to_string(),
                command: "git status --short".to_string(),
                interactive: false,
                stateful: false,
                timeout_ms: None,
            },
        }
    }

    /// Builds the validated local plan supplied by a product shell adapter.
    fn shell_plan() -> LocalActionPlan {
        LocalActionPlan {
            kind: LocalActionKind::ShellCommand,
            summary: "Inspect repository".to_string(),
            command: "git status --short".to_string(),
            policy_command: "git status --short".to_string(),
            interactive: false,
            stateful: false,
            timeout_ms: None,
            display_output_after_completion: false,
        }
    }

    /// Verifies completion identity mismatches fail before runtime mutation.
    #[test]
    fn provider_completion_identity_rejects_mismatched_agent() {
        let error = runtime_validate_provider_completion_identity(
            &turn(),
            "other-agent",
            "turn-1",
            &execution(),
        )
        .unwrap_err();
        assert!(error.message().contains("agent id does not match"));
    }

    /// Verifies a missing action batch becomes one terminal failed execution.
    #[test]
    fn provider_completion_normalizes_missing_batch_to_terminal_failure() {
        let mut execution = execution();
        runtime_validate_provider_completion_execution(&turn(), &mut execution).unwrap();
        assert_eq!(execution.terminal_state, AgentTurnState::Failed);
        assert!(execution.final_turn);
    }

    /// Verifies model-authored network failures remain eligible for feedback.
    #[test]
    fn action_failure_classification_accepts_correctable_network_errors() {
        let result = ActionResult::failed(
            &turn(),
            &AgentAction {
                id: "fetch".to_string(),
                rationale: "fetch required source".to_string(),
                payload: AgentActionPayload::FetchUrl {
                    url: "https://example.test".to_string(),
                    format: None,
                    max_bytes: None,
                },
            },
            ActionStatus::Failed,
            "network_http_error",
            "HTTP 503",
        )
        .unwrap();
        assert!(runtime_action_result_is_feedback_candidate(&result));
    }

    /// Verifies neutral summary and rationale suppression consume the explicit
    /// validated plan supplied by the product rather than re-lowering actions.
    #[test]
    fn action_presentation_uses_explicit_local_plan() {
        let action = shell_action();
        let plan = shell_plan();
        let input = ActionPresentationInput {
            local_plan: Some(&plan),
            ..ActionPresentationInput::default()
        };

        assert_eq!(
            action_summary(&action, input).as_deref(),
            Some("Inspect repository")
        );
        assert!(!action_rationale_repeats_visible_summary(&action, input));

        let mut repeated = action;
        repeated.rationale = "git status --short".to_string();
        assert!(action_rationale_repeats_visible_summary(&repeated, input));
    }

    /// Verifies conversational text is normalized once and suppresses a batch
    /// rationale that would otherwise repeat already-visible assistant output.
    #[test]
    fn batch_presentation_suppresses_repeated_conversational_text() {
        let batch = MaapBatch {
            protocol: "maap/1".to_string(),
            rationale: "thinking:  Ready   to continue".to_string(),
            thought: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            actions: vec![AgentAction {
                id: "say-1".to_string(),
                rationale: String::new(),
                payload: AgentActionPayload::Say {
                    status: SayStatus::Progress,
                    text: "ready to continue".to_string(),
                    content_type: "text/plain".to_string(),
                },
            }],
            final_turn: false,
        };

        let visible = batch_visible_action_texts(&batch);
        assert_eq!(visible, vec!["ready to continue"]);
        assert!(batch_rationale_repeats_visible_text(&batch, &visible));
    }

    /// Verifies duplicate mutation and runtime-visible-effect classifications
    /// remain intrinsic properties of canonical actions and results.
    #[test]
    fn action_presentation_classifies_file_mutation_duplicates() {
        let action = AgentAction {
            id: "patch-1".to_string(),
            rationale: "Update the file".to_string(),
            payload: AgentActionPayload::ApplyPatch {
                patch: "*** Begin Patch\n*** End Patch".to_string(),
                strip: None,
            },
        };
        let result = ActionResult::succeeded(
            &turn(),
            &action,
            Vec::new(),
            Some(r#"{"guard":"repeated_successful_file_mutation"}"#.to_string()),
        );

        assert!(action_has_runtime_visible_effect(&action));
        assert!(action_rejects_duplicate_success(&action));
        assert!(action_result_is_suppressed_duplicate_file_mutation(&result));
    }

    /// Verifies outcome selection hides local command targets by default and
    /// exposes them only when the product explicitly enables detailed output.
    #[test]
    fn action_outcome_respects_product_target_visibility() {
        let action = shell_action();
        let plan = shell_plan();
        let result = ActionResult::blocked(
            &turn(),
            &action,
            Vec::new(),
            r#"{"approval":{"state":"pending"}}"#.to_string(),
        );
        let hidden = action_outcome_line(
            &action,
            &result,
            ActionPresentationInput {
                local_plan: Some(&plan),
                ..ActionPresentationInput::default()
            },
        )
        .unwrap();
        let visible = action_outcome_line(
            &action,
            &result,
            ActionPresentationInput {
                local_plan: Some(&plan),
                show_runtime_target: true,
                ..ActionPresentationInput::default()
            },
        )
        .unwrap();

        assert_eq!(hidden.line, "agent: Inspect repository (awaiting approval)");
        assert_eq!(
            visible.line,
            "agent: shell command awaiting approval: git status --short"
        );
        assert!(!hidden.is_error);
        assert!(!visible.is_error);
    }
}
