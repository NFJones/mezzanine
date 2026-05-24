//! Runtime Agent implementation.
//!
//! This module owns the runtime agent boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

#[cfg(test)]
use super::runtime_execute_auto_sizing_with_provider;
use super::types::{RuntimeAgentPatchRecord, RuntimeAgentProviderClaim, RuntimeAgentTurnSteering};
use super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentContext, AgentId,
    AgentShellSession, AgentShellVisibility, AgentTurnExecution, AgentTurnRecord, AgentTurnState,
    AuditActor, AuditRecord, BTreeMap, BTreeSet, BlockedAgentApprovalRef, BlockedApprovalRequest,
    ContextBlock, ContextSourceKind, DEFAULT_COMMAND_SHELL_CLASSIFICATION,
    DEFAULT_PROVIDER_TIMEOUT_MS, DeferredAgentTranscriptWrite, Envelope, EventKind, HookEvent,
    JoinedSubagentDependency, McpToolCallRequest, MezError, ModelProfile, ModelRequest,
    ModelResponse, ModelTokenUsage, PaneId, PaneReadinessState, PathBuf, PathScopes,
    PendingFocusedShellHookContinuation, PermissionPolicy, ProviderQuotaUsage,
    ReadinessOverrideRevocation, Recipient, ReqwestProviderHttpTransport, Result, RuleDecision,
    RunningShellTransactionKind, RunningShellTransactionRef, RuntimeAgentCopyOutput,
    RuntimeAgentProviderDispatch, RuntimeAgentProviderDispatchProvider, RuntimeAgentProviderTask,
    RuntimeAutoSizingDispatch, RuntimeAutoSizingTargetProfile, RuntimeHookPipelineBlock,
    RuntimeHookPipelineDecision, RuntimeMcpActionExecutor, RuntimeSessionService,
    RuntimeShellTransactionActionFailure, SenderIdentity, ShellTransaction,
    ShellTransactionOutputTransport, SubagentScopeDeclaration, SubagentSpawnRequest,
    SubagentWaitPolicy, TaskResultPayload, TaskState, TaskStatusPayload, TranscriptEntry,
    TranscriptRole, action_result_context_content, append_mcp_context,
    assemble_model_request_with_retained_tail_percent,
    compact_model_context_for_budget_with_retained_tail_percent, current_unix_millis,
    current_unix_seconds, decode_shell_output_transport, discover_project_root,
    exact_command_sha256, execute_mcp_action_through_runtime,
    execute_mcp_action_through_runtime_async, execute_network_action_with_transport_async,
    json_escape, local_action_plan, local_action_summary, network_action_plan,
    network_action_summary, next_transcript_sequence, runtime_agent_turn_duration_display,
    runtime_agent_turn_start_hook_payload, runtime_agent_turn_state_from_action_results,
    runtime_agent_turn_state_name, runtime_apply_auto_sizing_execution_profile,
    runtime_apply_persisted_config_mutation_batch,
    runtime_auto_sizing_reasoning_levels_for_profile, runtime_blocked_approval_request,
    runtime_cooperation_mode, runtime_cooperation_mode_name,
    runtime_execution_ready_for_provider_continuation, runtime_hook_event_name,
    runtime_marker_for_action, runtime_mcp_error_code, runtime_message_recipient,
    runtime_mezzanine_error_code, runtime_pane_by_id, runtime_pane_readiness_state_name,
    runtime_path_under_project_root, runtime_permission_preset_name,
    runtime_permission_request_hook_payload, runtime_post_mcp_hook_payload,
    runtime_pre_mcp_hook_payload, runtime_pre_shell_hook_payload, runtime_set_theme_command,
    runtime_subagent_placement_mode, runtime_subagent_spawn_request, set_project_guidance_context,
    shell_command_structured_content_json, transcript_entries_for_execution,
    validate_mmp_payload_metadata,
};
#[cfg(test)]
use crate::agent::{AgentTurnLedger, AgentTurnRunner, ModelProvider};
use crate::agent::{
    ApplyPatchTransactionPhase, MaapBatch, apply_patch_error_plan, apply_patch_transaction_phase,
    apply_patch_write_plan_from_read_output,
    deepseek_provider_from_auth_store_with_provider_options,
    openai_prompt_cache_diagnostics_for_request,
    openai_provider_from_auth_store_with_provider_options,
};
use crate::agent::{SayStatus, assistant_context_content_for_execution};
#[cfg(test)]
use crate::agent::{
    provider_error_is_context_limit_exceeded, provider_error_is_output_limit_exceeded,
};
use crate::command::CommandInvocation;
use crate::config::{
    ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation, ConfigMutationValue,
    ConfigPaths, ConfigScope,
};
use crate::project::TrustDecision;
use crate::skills::{
    SkillCatalog, discover_skill_catalog, is_valid_skill_name, load_skill_document,
};

mod progress;
mod trace;

use progress::*;
use trace::*;

// Agent turn execution, provider polling, action dispatch, and approvals.

/// Defines the RUNTIME AGENT DEFAULT SHELL ACTION TIMEOUT MS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const RUNTIME_AGENT_TURN_TIMEOUT_MS: u64 = 30 * 60 * 1000;
/// Maximum in-process provider context-limit retries for test providers.
#[cfg(test)]
const RUNTIME_PROVIDER_CONTEXT_LIMIT_RETRY_LIMIT: u32 = 2;
/// Maximum in-process provider output-limit retries for test providers.
#[cfg(test)]
const RUNTIME_PROVIDER_OUTPUT_LIMIT_RETRY_LIMIT: u32 = 2;
/// Label for ephemeral active-turn context that guides output-limit retries.
const RUNTIME_PROVIDER_OUTPUT_LIMIT_RETRY_LABEL: &str = "provider output-limit retry guidance";
/// Returns the remaining turn-wide shell execution budget.
///
/// Individual model actions do not choose their own timeout. Shell-backed
/// transactions inherit the active turn's remaining default budget so a chain
/// of actions cannot outlive the enclosing turn indefinitely.
fn runtime_agent_turn_remaining_timeout_ms(turn: &AgentTurnRecord) -> u64 {
    let started_at_ms = turn.started_at_unix_seconds.saturating_mul(1000);
    let elapsed_ms = current_unix_millis().saturating_sub(started_at_ms);
    RUNTIME_AGENT_TURN_TIMEOUT_MS
        .saturating_sub(elapsed_ms)
        .max(1)
}

/// Returns the bounded timeout for one shell action.
///
/// The action-level timeout is an inner bound. The turn-wide budget remains the
/// outer cap so no command can outlive the enclosing agent turn.
fn runtime_shell_action_timeout_ms(turn: &AgentTurnRecord, timeout_ms: Option<u64>) -> u64 {
    let remaining = runtime_agent_turn_remaining_timeout_ms(turn);
    timeout_ms
        .map(|timeout_ms| timeout_ms.min(remaining))
        .unwrap_or(remaining)
        .max(1)
}

/// Formats last observed provider input-token usage for one model profile.
///
/// The display is a bounded status indicator, so accepted provider responses
/// whose token count exceeds the configured profile window saturate at `100%`
/// instead of rendering impossible percentages above the full window.
fn runtime_agent_provider_context_usage_display(
    profile: &ModelProfile,
    usage: ModelTokenUsage,
) -> Option<String> {
    if usage.input_tokens == 0 {
        return None;
    }
    let budget_tokens = u64::try_from(profile.context_window_tokens().max(1)).unwrap_or(u64::MAX);
    let percentage = usage
        .input_tokens
        .saturating_mul(100)
        .saturating_add(budget_tokens / 2)
        / budget_tokens;
    Some(format!("{}%", percentage.min(100)))
}

/// Returns the most restrictive main-provider context profile for an
/// auto-sizing dispatch.
///
/// Before the router decision has been applied, any configured target bucket
/// may become the actual model for the first normal provider request. Context
/// pressure checks therefore need the smallest target window rather than the
/// default profile's potentially larger window.
fn runtime_auto_sizing_minimum_context_profile(
    default_profile: &ModelProfile,
    auto_sizing: Option<&RuntimeAutoSizingDispatch>,
) -> ModelProfile {
    let mut selected = default_profile;
    if let Some(auto_sizing) = auto_sizing {
        for candidate in [
            &auto_sizing.default_profile,
            &auto_sizing.small.profile,
            &auto_sizing.medium.profile,
            &auto_sizing.large.profile,
        ] {
            if candidate.context_window_tokens() < selected.context_window_tokens() {
                selected = candidate;
            }
        }
    }
    selected.clone()
}

/// Runs the runtime agent execution prompt display lines operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_agent_execution_prompt_display_lines(
    turn_id: &str,
    provider_id: &str,
    execution: &AgentTurnExecution,
    dispatched_actions: usize,
    transcript_entries: usize,
) -> Vec<String> {
    let state = runtime_agent_turn_state_name(execution.terminal_state);
    let mut lines = vec![format!("agent: turn {turn_id} {state}")];
    lines.push(format!("agent: provider {provider_id} responded"));
    if dispatched_actions > 0 {
        lines.push(format!("agent: dispatched {dispatched_actions} actions"));
    }
    if transcript_entries > 0 {
        lines.push(format!(
            "agent: recorded {transcript_entries} transcript entries"
        ));
    }
    match execution.terminal_state {
        AgentTurnState::Completed if execution.response.action_batch.is_none() => {
            lines.extend(
                execution
                    .response
                    .raw_text
                    .lines()
                    .take(200)
                    .map(ToOwned::to_owned),
            );
        }
        AgentTurnState::Completed => {}
        AgentTurnState::Failed => {
            lines.extend(
                execution
                    .response
                    .raw_text
                    .lines()
                    .take(200)
                    .map(ToOwned::to_owned),
            );
        }
        AgentTurnState::Blocked => {
            lines.push("agent: blocked pending approval".to_string());
        }
        AgentTurnState::Running => {
            lines.push("agent: waiting for pane, tool, or provider continuation".to_string());
        }
        AgentTurnState::Queued | AgentTurnState::Interrupted => {}
    }
    lines
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
fn runtime_validate_provider_completion_identity(
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
pub(super) fn runtime_validate_provider_completion_execution(
    turn: &AgentTurnRecord,
    execution: &AgentTurnExecution,
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
        return Err(MezError::invalid_state(
            "agent provider completion without an action batch must be a terminal failed execution",
        ));
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
fn runtime_execution_is_missing_batch_terminal_failure(execution: &AgentTurnExecution) -> bool {
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
fn runtime_execution_is_controller_failure_summary(
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
fn runtime_execution_is_controller_validation_failure(execution: &AgentTurnExecution) -> bool {
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
fn runtime_provider_audit_error_message(message: &str) -> String {
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
struct RuntimeAgentExecutionFailure {
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
fn runtime_agent_execution_failure_error(execution: &AgentTurnExecution) -> MezError {
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
fn runtime_agent_execution_failure(execution: &AgentTurnExecution) -> RuntimeAgentExecutionFailure {
    if execution.response.action_batch.is_none() {
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

/// Runs the runtime agent execution failure json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_agent_execution_failure_json(
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
fn runtime_action_status_name(status: ActionStatus) -> &'static str {
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
fn runtime_action_result_is_feedback_candidate(result: &ActionResult) -> bool {
    let Some(error) = result.error.as_ref() else {
        return false;
    };
    if !result.is_error {
        return false;
    }
    if result.action_type == "shell_command" {
        return false;
    }
    if runtime_action_result_is_runtime_infrastructure_failure(result) {
        return false;
    }
    if result.status == ActionStatus::TimedOut {
        return runtime_action_type_is_model_correctable(result.action_type)
            && !runtime_error_code_is_non_correctable(error.code.as_str());
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
fn runtime_action_result_is_runtime_infrastructure_failure(result: &ActionResult) -> bool {
    let Some(error) = result.error.as_ref() else {
        return false;
    };
    let message = error.message.to_ascii_lowercase();
    message.contains("pane process not found")
        || (error.code == "not_found" && message.starts_with("shell_dispatch:"))
}

/// Returns true when one action type is authored by the model and can usually
/// be retried with corrected parameters or a different action choice.
fn runtime_action_type_is_model_correctable(action_type: &str) -> bool {
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
fn runtime_action_type_is_shell_backed(action_type: &str) -> bool {
    matches!(action_type, "shell_command" | "apply_patch")
}

/// Returns true when an error represents a policy or user boundary rather than
/// evidence the model can use to correct its own action.
fn runtime_error_code_is_non_correctable(error_code: &str) -> bool {
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
fn runtime_invalid_params_is_feedback_candidate(result: &ActionResult) -> bool {
    runtime_action_type_is_model_correctable(result.action_type)
}

/// Returns true when an action carries enough model-authored explanation to
/// satisfy auto-allow after compact MAAP omits the formerly required rationale.
fn runtime_action_supports_auto_allow(action: &AgentAction) -> bool {
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
struct RuntimeSkillActionContext {
    /// Whether a successful skill catalog result is already present.
    catalog_requested: bool,
    /// Skill names whose full `SKILL.md` text is already in context.
    loaded_skills: BTreeSet<String>,
}

/// Extracts loaded skill state from active model-context blocks.
///
/// This intentionally inspects only explicit context labels and action-result
/// text that the runtime itself produced. It does not parse arbitrary repository
/// files or shell output as authoritative skill state.
fn runtime_skill_action_context_from_blocks(blocks: &[ContextBlock]) -> RuntimeSkillActionContext {
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
fn runtime_execution_can_feed_failure_to_model(execution: &AgentTurnExecution) -> bool {
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
/// The turn id scopes cleanup while the hash scopes attempts to individual
/// failed action signatures. A batch with several model-correctable failures
/// therefore receives one retry budget per failed action instead of amortizing
/// a shared budget across unrelated failures.
fn runtime_failure_feedback_attempt_keys(
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
fn runtime_failure_feedback_attempt_key_for_result(turn_id: &str, result: &ActionResult) -> String {
    let digest = exact_command_sha256(
        DEFAULT_COMMAND_SHELL_CLASSIFICATION,
        &runtime_failure_feedback_action_signature(result),
    );
    format!("{turn_id}:{digest}")
}

/// Returns stable, non-secret material identifying one failed action result.
fn runtime_failure_feedback_action_signature(result: &ActionResult) -> String {
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
fn runtime_action_result_error_code(result: &ActionResult) -> Option<&str> {
    result.error.as_ref().map(|error| error.code.as_str())
}

/// Returns true when one action result carries the requested error code.
///
/// # Parameters
/// - `result`: The action result being inspected.
/// - `code`: The machine-readable error code to match.
fn runtime_action_result_has_error_code(result: &ActionResult, code: &str) -> bool {
    runtime_action_result_error_code(result).is_some_and(|error_code| error_code == code)
}

/// Returns true when the result represents a batch-level loop guard failure
/// where showing every sibling failure would flood the pane and model context.
///
/// # Parameters
/// - `result`: The action result being inspected.
fn runtime_action_result_is_aggregated_loop_guard_failure(result: &ActionResult) -> bool {
    let _ = result;
    false
}

/// Builds one bounded pane line for a group of loop guard failures.
///
/// # Parameters
/// - `label`: The human-readable guard label.
/// - `count`: The number of failed sibling actions represented by the line.
/// - `message`: The shared runtime guard diagnostic.
fn runtime_loop_guard_failure_summary_line(label: &str, count: usize, message: &str) -> String {
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
fn runtime_loop_guard_failure_label(code: &str) -> Option<&'static str> {
    match code {
        "shell_dispatch_limit_exceeded" => Some("shell dispatch"),
        "network_action_limit_exceeded" => Some("network action"),
        _ => None,
    }
}

/// Returns true when a failed execution includes an `apply_patch` failure.
fn runtime_execution_has_apply_patch_failure(execution: &AgentTurnExecution) -> bool {
    execution
        .action_results
        .iter()
        .any(|result| result.is_error && result.action_type == "apply_patch")
}

/// Returns true when a failed execution includes an `apply_patch` hunk
/// mismatch.
fn runtime_execution_has_apply_patch_hunk_mismatch(execution: &AgentTurnExecution) -> bool {
    execution
        .action_results
        .iter()
        .filter(|result| result.is_error && result.action_type == "apply_patch")
        .filter_map(runtime_unrecovered_action_failure_output)
        .any(|output| output.to_ascii_lowercase().contains("hunk did not match"))
}

/// Returns true when a failed execution includes config-change validation.
fn runtime_execution_has_config_change_failure(execution: &AgentTurnExecution) -> bool {
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
fn runtime_execution_has_invalid_message_payload_failure(execution: &AgentTurnExecution) -> bool {
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
fn runtime_execution_has_spawn_agent_failure(execution: &AgentTurnExecution) -> bool {
    execution.action_results.iter().any(|result| {
        result.is_error
            && result.action_type == "spawn_agent"
            && result.status == ActionStatus::Failed
    })
}

/// Returns true when the model tried to rediscover or reload already-present
/// skill context.
fn runtime_execution_has_redundant_skill_action_failure(execution: &AgentTurnExecution) -> bool {
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
fn runtime_execution_has_failed_action_type(
    execution: &AgentTurnExecution,
    action_types: &[&str],
) -> bool {
    execution
        .action_results
        .iter()
        .any(|result| result.is_error && action_types.contains(&result.action_type))
}

/// Returns true when a failed execution includes a failed filesystem mutation.
fn runtime_execution_has_mutation_failure(execution: &AgentTurnExecution) -> bool {
    runtime_execution_has_failed_action_type(execution, &["apply_patch"])
}

/// Returns true when a failed execution includes a shell-backed file operation.
fn runtime_execution_has_file_operation_failure(execution: &AgentTurnExecution) -> bool {
    runtime_execution_has_failed_action_type(execution, &["apply_patch"])
}

/// Builds model-facing recovery guidance that prevents unsupported success claims.
fn runtime_failure_feedback_evidence_guidance(
    execution: &AgentTurnExecution,
) -> Option<&'static str> {
    runtime_execution_has_mutation_failure(execution).then_some(
        "Mutation-evidence rule: no successful mutation has occurred after the failed action(s) in this recovery context. Do not claim the task is implemented, changed, updated, fixed, applied, run, or executed until a later mutation action succeeds and you verify it. Reads, git status, and git diff after a failed mutation prove only current file state; they do not prove your attempted edit landed. If current files appear changed without mutation-action proof, say the current file/diff shows that state rather than claiming you performed the change.",
    )
}

/// Builds extra model-facing recovery guidance for known failure modes.
fn runtime_failure_feedback_specific_guidance(
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
        return Some(format!(
            "Apply-patch recovery: if a hunk did not match or the patch did not apply, the exact old-context lines were not found in the current target file or matched ambiguously; this is not necessarily a stale-file condition. Next step: first inspect the affected path(s) with a bounded shell_command, especially around any reported line number(s), before emitting another mutation. Do not retry substantially the same patch. After reading current context, emit a smaller fresh Mezzanine *** Begin Patch block against the current file contents, using distinctive @@ header anchors for repeated or ambiguous regions.{path_hint} Use shell_command only for local inspection, path operations, validation, or raw unified diffs that apply_patch cannot express."
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
    if runtime_execution_has_redundant_skill_action_failure(execution) {
        return Some(
            "Skill recovery: the requested skill catalog or skill context is already loaded for this turn. Do not call request_skills or call_skill again merely to confirm the workflow. Use the loaded skill instructions and emit the next concrete action; if the needed action family is not currently allowed, request it with request_capability."
                .to_string(),
        );
    }
    None
}

/// Returns extra guidance for repeated failures with the same retry signature.
fn runtime_failure_feedback_repeat_guidance(
    execution: &AgentTurnExecution,
    attempt: usize,
) -> Option<String> {
    if attempt <= 1 {
        return None;
    }
    if runtime_execution_has_apply_patch_hunk_mismatch(execution) {
        return Some(
            "\nRepeated apply-patch recovery: this failure signature has already consumed a recovery attempt. Do not emit another apply_patch action until you have read the affected target file or otherwise obtained current target context."
                .to_string(),
        );
    }
    None
}

/// Builds a model-facing note for aggregated runtime loop-guard failures.
///
/// # Parameters
/// - `execution`: The failed action execution being converted into recovery
///   context.
fn runtime_failure_feedback_loop_guard_aggregate_note(
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
fn runtime_failure_feedback_status_line(
    execution: &AgentTurnExecution,
    attempt: usize,
    attempt_limit: usize,
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
    format!("agent: action failed; asking model to recover ({attempt}/{attempt_limit}, {reason})")
}

/// Describes why one failed execution cannot be fed back for correction.
fn runtime_recovery_unavailable_detail(execution: &AgentTurnExecution) -> String {
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
fn runtime_action_result_summary(result: &ActionResult) -> String {
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
fn runtime_join_bounded_summaries(summaries: &[String]) -> String {
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
fn runtime_unrecovered_failure_reason(
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
fn runtime_apply_patch_unsafe_paths(execution: &AgentTurnExecution) -> Vec<String> {
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
fn runtime_apply_patch_failed_paths(execution: &AgentTurnExecution) -> Vec<String> {
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
fn runtime_patch_failure_path_from_diagnostic(text: &str) -> Option<String> {
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
fn runtime_agent_terminal_preview(value: &str) -> String {
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
fn runtime_agent_context_command(action: &AgentAction, command: &str) -> String {
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
fn runtime_humanize_agent_diagnostic(value: &str) -> String {
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
fn runtime_agent_pending_approval_log_line(approval: &BlockedApprovalRequest) -> String {
    format!(
        "agent approval {} pending: {} {} (approve with /approve {})",
        approval.id,
        approval.action_kind,
        runtime_agent_terminal_preview(&approval.action_summary),
        approval.id
    )
}

/// Runs the runtime config change value json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_config_change_value_json(value: Option<&str>) -> Result<String> {
    let Some(value) = value else {
        return Err(MezError::invalid_args(
            "approved config_change set operation requires a value",
        ));
    };
    match serde_json::from_str::<serde_json::Value>(value) {
        Ok(value) => Ok(value.to_string()),
        Err(_) => Ok(format!("\"{}\"", json_escape(value))),
    }
}

/// Returns a config-change value as one string suitable for command reuse.
///
/// Model-authored config changes usually arrive as raw strings, but recovery
/// turns can also echo a JSON string literal. Accept both forms while rejecting
/// non-string JSON values for command paths that require names.
fn runtime_config_change_string_value(setting_path: &str, value: Option<&str>) -> Result<String> {
    let Some(value) = value else {
        return Err(MezError::invalid_args(format!(
            "approved config_change set operation for {setting_path} requires a value"
        )));
    };
    match serde_json::from_str::<serde_json::Value>(value) {
        Ok(serde_json::Value::String(value)) => Ok(value),
        Ok(_) => Err(MezError::invalid_args(format!(
            "config_change {setting_path} requires a string value"
        ))),
        Err(_) => Ok(value.to_string()),
    }
}

/// Returns whether a config-change operation sets a scalar value.
fn runtime_config_change_operation_sets_value(operation: &str) -> bool {
    matches!(
        operation.trim().to_ascii_lowercase().as_str(),
        "set" | "replace" | "update"
    )
}

/// Runs the runtime config change control request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_config_change_control_request(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    setting_path: &str,
    operation: &str,
    value: Option<&str>,
    persist_target_json: &str,
    idempotency_suffix: &str,
) -> Result<String> {
    match operation.trim().to_ascii_lowercase().as_str() {
        "set" | "replace" | "update" => {
            let value = runtime_config_change_value_json(value)?;
            let idempotency_key = runtime_config_change_idempotency_key(
                turn,
                action,
                RuntimeConfigChangeIdempotency {
                    method: "config/set",
                    setting_path,
                    operation,
                    value_json: Some(value.as_str()),
                    persist_target_json,
                    suffix: idempotency_suffix,
                },
            );
            Ok(format!(
                r#"{{"jsonrpc":"2.0","id":"agent-config-change","method":"config/set","params":{{"path":"{}","value":{},"persist":{},"idempotency_key":"{}"}}}}"#,
                json_escape(setting_path),
                value,
                persist_target_json,
                json_escape(&idempotency_key)
            ))
        }
        "unset" | "remove" | "delete" | "reset" => {
            let idempotency_key = runtime_config_change_idempotency_key(
                turn,
                action,
                RuntimeConfigChangeIdempotency {
                    method: "config/unset",
                    setting_path,
                    operation,
                    value_json: None,
                    persist_target_json,
                    suffix: idempotency_suffix,
                },
            );
            Ok(format!(
                r#"{{"jsonrpc":"2.0","id":"agent-config-change","method":"config/unset","params":{{"path":"{}","persist":{},"idempotency_key":"{}"}}}}"#,
                json_escape(setting_path),
                persist_target_json,
                json_escape(&idempotency_key)
            ))
        }
        _ => Err(MezError::invalid_args(
            "config_change operation must be set, replace, update, unset, remove, delete, or reset",
        )),
    }
}

/// Returns the setting path for one config-change action.
fn runtime_config_change_setting_path(action: &AgentAction) -> Option<&str> {
    match &action.payload {
        AgentActionPayload::ConfigChange { setting_path, .. } => Some(setting_path.as_str()),
        _ => None,
    }
}

/// Returns the operation name for one config-change action.
fn runtime_config_change_operation_name(action: &AgentAction) -> &str {
    match &action.payload {
        AgentActionPayload::ConfigChange { operation, .. } => operation.as_str(),
        _ => "unknown",
    }
}

/// Reports whether a config change can be folded into a theme scalar batch.
fn runtime_config_change_action_is_theme_scalar_batchable(action: &AgentAction) -> bool {
    let AgentActionPayload::ConfigChange {
        setting_path,
        operation,
        ..
    } = &action.payload
    else {
        return false;
    };
    let operation = operation.trim().to_ascii_lowercase();
    let supported_operation = matches!(
        operation.as_str(),
        "set" | "replace" | "update" | "unset" | "remove" | "delete" | "reset"
    );
    supported_operation
        && (setting_path.starts_with("theme.aliases.") || setting_path.starts_with("theme.colors."))
}

/// Returns the model-facing approval state for an accepted config change.
fn runtime_config_change_approval_state(
    permission_policy: &PermissionPolicy,
    action: &AgentAction,
) -> &'static str {
    if permission_policy.approval_bypass() {
        "bypassed"
    } else if permission_policy.approval_policy == crate::permissions::ApprovalPolicy::AutoAllow
        && runtime_action_supports_auto_allow(action)
    {
        "auto_allowed"
    } else {
        "full_access"
    }
}

/// Converts a model-authored config-change action into a validated mutation.
fn runtime_config_change_mutation_from_action(action: &AgentAction) -> Result<ConfigMutation> {
    let AgentActionPayload::ConfigChange {
        setting_path,
        operation,
        value,
    } = &action.payload
    else {
        return Err(MezError::invalid_args(
            "config_change batch requires config_change actions",
        ));
    };
    let operation = match operation.trim().to_ascii_lowercase().as_str() {
        "set" | "replace" | "update" => {
            ConfigMutationOperation::Set(runtime_config_change_mutation_value(value.as_deref())?)
        }
        "unset" | "remove" | "delete" | "reset" => ConfigMutationOperation::Unset,
        _ => {
            return Err(MezError::invalid_args(format!(
                "unsupported config_change operation `{operation}`"
            )));
        }
    };
    Ok(ConfigMutation {
        path: setting_path.clone(),
        operation,
    })
}

/// Converts one model-authored config-change value into a scalar config value.
fn runtime_config_change_mutation_value(value: Option<&str>) -> Result<ConfigMutationValue> {
    let value_json = runtime_config_change_value_json(value)?;
    match serde_json::from_str::<serde_json::Value>(&value_json) {
        Ok(serde_json::Value::String(value)) => Ok(ConfigMutationValue::String(value)),
        Ok(serde_json::Value::Bool(value)) => Ok(ConfigMutationValue::Boolean(value)),
        Ok(serde_json::Value::Number(value)) => value
            .as_i64()
            .map(ConfigMutationValue::Integer)
            .ok_or_else(|| MezError::invalid_args("config_change integer value is invalid")),
        Ok(serde_json::Value::Array(values)) => {
            let mut strings = Vec::with_capacity(values.len());
            for value in values {
                let serde_json::Value::String(value) = value else {
                    return Err(MezError::invalid_args(
                        "config_change string arrays must contain only strings",
                    ));
                };
                strings.push(value);
            }
            Ok(ConfigMutationValue::StringArray(strings))
        }
        Ok(serde_json::Value::Object(_) | serde_json::Value::Null) => Err(MezError::invalid_args(
            "config_change supports only string, integer, boolean, or string-array values",
        )),
        Err(error) => Err(MezError::invalid_args(format!(
            "config_change value is invalid JSON: {error}"
        ))),
    }
}

/// Holds the request material used to build one config-change idempotency key.
struct RuntimeConfigChangeIdempotency<'a> {
    /// The JSON-RPC control method being requested.
    method: &'a str,
    /// The config setting path being changed.
    setting_path: &'a str,
    /// The model-requested config operation.
    operation: &'a str,
    /// The canonical JSON value for set-like operations.
    value_json: Option<&'a str>,
    /// The canonical JSON persist target.
    persist_target_json: &'a str,
    /// The per-action sequencing suffix.
    suffix: &'a str,
}

/// Builds a payload-sensitive control idempotency key for one config change.
///
/// Model-authored action ids are synthesized by Mezzanine, but recovery and
/// compatibility paths can still produce duplicate local ids for separate
/// mutations. Include a stable request fingerprint so different settings do
/// not collide in the JSON-RPC control idempotency cache.
fn runtime_config_change_idempotency_key(
    turn: &AgentTurnRecord,
    action: &AgentAction,
    request: RuntimeConfigChangeIdempotency<'_>,
) -> String {
    let material = format!(
        "method={}\npath={}\noperation={}\nvalue={}\npersist={}",
        request.method,
        request.setting_path,
        request.operation.trim().to_ascii_lowercase(),
        request.value_json.unwrap_or("null"),
        request.persist_target_json
    );
    let digest = exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, &material);
    format!(
        "agent-config-change-{}-{}-{}-{}",
        turn.turn_id,
        action.id,
        request.suffix,
        &digest[..24]
    )
}

/// Runs the runtime agent shell summary operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_agent_action_summary(action: &AgentAction) -> Option<String> {
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
fn runtime_agent_shell_status(action: &AgentAction, fallback: &str) -> String {
    format!(
        "agent: {}",
        runtime_agent_action_summary(action).unwrap_or_else(|| fallback.to_string())
    )
}

/// Builds the terminal footer that replaces the live working timer.
fn runtime_agent_finished_footer_line(
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
fn runtime_agent_action_rationale_repeats_visible_summary(action: &AgentAction) -> bool {
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
fn runtime_agent_batch_visible_action_texts(batch: &MaapBatch) -> Vec<String> {
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
fn runtime_agent_batch_rationale_repeats_visible_batch_text(
    batch: &MaapBatch,
    visible_texts: &[String],
) -> bool {
    let rationale = normalize_agent_user_visible_text(&batch.rationale);
    !rationale.is_empty() && visible_texts.iter().any(|text| text == &rationale)
}

/// Returns whether the action will produce runtime-visible output after it is
/// executed rather than purely conversational assistant text.
fn runtime_agent_action_has_runtime_visible_effect(action: &AgentAction) -> bool {
    matches!(
        action.payload,
        AgentActionPayload::ShellCommand { .. }
            | AgentActionPayload::ApplyPatch { .. }
            | AgentActionPayload::WebSearch { .. }
            | AgentActionPayload::FetchUrl { .. }
            | AgentActionPayload::McpCall { .. }
            | AgentActionPayload::SendMessage { .. }
            | AgentActionPayload::SpawnAgent { .. }
            | AgentActionPayload::ConfigChange { .. }
    )
}

/// Returns whether a successful duplicate dispatch of this action should be
/// treated as a loop instead of re-running the same file mutation.
fn runtime_agent_action_rejects_duplicate_success(action: &AgentAction) -> bool {
    matches!(action.payload, AgentActionPayload::ApplyPatch { .. })
}

/// Returns whether the guard result represents a duplicate file mutation that
/// was skipped because the identical mutation already succeeded in this turn.
fn runtime_action_result_is_suppressed_duplicate_file_mutation(result: &ActionResult) -> bool {
    result.status == ActionStatus::Succeeded
        && result
            .structured_content_json
            .as_deref()
            .is_some_and(|content| content.contains("repeated_successful_file_mutation"))
}

/// Returns true when an action rationale repeats text already rendered by a
/// nearby conversational action in the same provider response.
fn runtime_agent_action_rationale_repeats_visible_batch_text(
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
fn normalize_agent_user_visible_text(value: &str) -> String {
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
fn runtime_agent_user_action_phrase(action: &AgentAction) -> Option<(&'static str, String)> {
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
fn runtime_agent_action_error_suffix(result: &ActionResult) -> String {
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
fn runtime_agent_recoverable_network_warning_line(
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
fn runtime_fetch_url_status_label(result: &ActionResult) -> Option<String> {
    let value: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_deref()?).ok()?;
    let status = value
        .get("response")
        .and_then(|response| response.get("status_code"))
        .and_then(serde_json::Value::as_u64)?;
    Some(format!("HTTP {status}"))
}

/// Builds the model-facing wrapper for mid-turn user steering input.
fn runtime_agent_turn_steering_context_content(steering: &RuntimeAgentTurnSteering) -> String {
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
fn runtime_agent_action_outcome_line(
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
fn runtime_action_result_is_terminal_failure(result: &ActionResult) -> bool {
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
fn runtime_unrecovered_action_failure_output(result: &ActionResult) -> Option<String> {
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
fn runtime_action_failure_content_is_generic_status(content: &str) -> bool {
    let trimmed = content.trim();
    trimmed == "shell command accepted for pane execution"
        || trimmed.starts_with("shell command exited with status ")
        || trimmed == "shell command timed out"
        || trimmed == "shell command was interrupted"
}

/// Extracts a string from nested action-result structured content.
fn runtime_action_result_structured_string(result: &ActionResult, path: &[&str]) -> Option<String> {
    let mut value: serde_json::Value =
        serde_json::from_str(result.structured_content_json.as_deref()?).ok()?;
    for key in path {
        value = value.get(*key)?.clone();
    }
    value.as_str().map(str::to_string)
}

/// Builds bounded, sanitized diagnostic output lines for final failed actions.
pub(super) fn runtime_unrecovered_failure_output_lines(
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
fn runtime_failure_output_without_prompt_prefix(line: &str) -> &str {
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
fn runtime_failure_output_line_is_wrapper_noise(trimmed: &str) -> bool {
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
fn runtime_failure_output_line_is_generated_action_echo(
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
fn runtime_action_is_apply_patch(action: &AgentAction) -> bool {
    matches!(&action.payload, AgentActionPayload::ApplyPatch { .. })
}

/// Returns true when the failed action was lowered into a Mezzanine-generated
/// shell command whose wrapper echo is not useful final diagnostic output.
fn runtime_action_is_generated_semantic_shell_action(action: &AgentAction) -> bool {
    matches!(&action.payload, AgentActionPayload::ApplyPatch { .. })
}

/// Returns a concise fallback diagnostic for generated semantic actions when
/// the captured output contains wrapper fragments but no actionable error line.
fn runtime_generic_semantic_failure_diagnostic(action: &AgentAction) -> String {
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
fn runtime_failure_output_line_looks_like_diagnostic(line: &str) -> bool {
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
fn runtime_sanitized_failure_output_line(line: &str) -> String {
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
fn runtime_truncate_to_utf8_boundary(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

impl RuntimeSessionService {
    /// Records text in the bounded hidden per-pane trace log.
    fn record_agent_pane_trace_log_text(&mut self, pane_id: &str, text: &str) {
        let Some(log) = (!text.trim().is_empty()).then(|| {
            self.agent_pane_trace_logs
                .entry(pane_id.to_string())
                .or_default()
        }) else {
            return;
        };
        for line in text.trim_end_matches(['\r', '\n']).lines() {
            if !line.trim().is_empty() {
                log.push(runtime_bounded_trace_text(line));
            }
        }
        runtime_trim_agent_pane_trace_log(log);
    }

    /// Returns the retained trace log text for one pane.
    pub(super) fn agent_pane_trace_log_text(&self, pane_id: &str) -> Option<String> {
        let log = self.agent_pane_trace_logs.get(pane_id)?;
        (!log.is_empty()).then(|| log.join("\n"))
    }

    /// Runs the agent diagnostic label operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn agent_diagnostic_label(&self, pane_id: &str) -> Option<&'static str> {
        self.agent_diagnostic_level_name(pane_id)
    }

    /// Runs the append agent trace turn transition operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_agent_trace_turn_transition(
        &mut self,
        turn: &AgentTurnRecord,
        from: AgentTurnState,
        to: AgentTurnState,
        reason: &str,
    ) -> Result<()> {
        let trace_line = format!(
            "agent trace: turn {} moved from {} to {} ({})",
            turn.turn_id,
            runtime_agent_turn_state_name(from),
            runtime_agent_turn_state_name(to),
            runtime_agent_terminal_preview(&runtime_humanize_agent_diagnostic(reason))
        );
        self.record_agent_pane_trace_log_text(&turn.pane_id, &trace_line);
        let Some(label) = self.agent_diagnostic_label(&turn.pane_id) else {
            return Ok(());
        };
        self.append_agent_status_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent {label}: turn {} moved from {} to {} ({})",
                turn.turn_id,
                runtime_agent_turn_state_name(from),
                runtime_agent_turn_state_name(to),
                runtime_agent_terminal_preview(&runtime_humanize_agent_diagnostic(reason))
            ),
        )
    }

    /// Runs the append agent trace turn event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_agent_trace_turn_event(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        message: &str,
    ) -> Result<()> {
        if self.find_pane_descriptor(pane_id).is_none() {
            return Ok(());
        }
        let trace_line = format!(
            "agent trace: turn {}: {}",
            turn_id,
            runtime_agent_terminal_preview(&runtime_humanize_agent_diagnostic(message))
        );
        self.record_agent_pane_trace_log_text(pane_id, &trace_line);
        let Some(label) = self.agent_diagnostic_label(pane_id) else {
            return Ok(());
        };
        let message = if self.agent_trace_enabled(pane_id) {
            message.to_string()
        } else {
            runtime_sanitize_agent_diagnostic_text(message)
        };
        self.append_agent_status_text_to_terminal_buffer(
            pane_id,
            &format!(
                "agent {label}: turn {}: {}",
                turn_id,
                runtime_agent_terminal_preview(&runtime_humanize_agent_diagnostic(&message))
            ),
        )
    }

    /// Appends a MAAP diagnostic while retaining a possibly fuller trace value.
    fn append_agent_trace_maap_value_with_retained(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        label: &str,
        display_value: serde_json::Value,
        retained_value: serde_json::Value,
    ) -> Result<()> {
        let retained_value = runtime_bounded_trace_value_strings(retained_value);
        let retained_body = serde_json::to_string_pretty(&retained_value).map_err(|error| {
            MezError::invalid_state(format!("MAAP trace JSON encoding failed: {error}"))
        })?;
        self.record_agent_pane_trace_log_text(
            pane_id,
            &format!("agent trace: turn {turn_id}: MAAP {label}\n{retained_body}"),
        );
        let Some(level_label) = self.agent_diagnostic_label(pane_id) else {
            return Ok(());
        };
        let value = runtime_bounded_trace_value_strings(display_value);
        let body = serde_json::to_string_pretty(&value).map_err(|error| {
            MezError::invalid_state(format!("MAAP trace JSON encoding failed: {error}"))
        })?;
        self.append_agent_status_text_to_terminal_buffer(
            pane_id,
            &format!("agent {level_label}: turn {turn_id}: MAAP {label}\n{body}"),
        )
    }

    /// Runs the append agent trace maap request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_agent_trace_maap_request(
        &mut self,
        turn: &AgentTurnRecord,
        request: &ModelRequest,
    ) -> Result<()> {
        self.append_agent_trace_maap_value_with_retained(
            &turn.pane_id,
            &turn.turn_id,
            "request",
            runtime_model_request_trace_json(
                request,
                self.agent_trace_enabled(&turn.pane_id),
                true,
            ),
            runtime_model_request_trace_json(request, true, true),
        )
    }

    /// Records the provider request shape that the runtime is about to submit.
    fn record_runtime_provider_request_shape_for_context(
        &mut self,
        model_profile: &ModelProfile,
        turn: &AgentTurnRecord,
        context: &AgentContext,
        available_mcp_tools: &[crate::mcp::McpPromptTool],
    ) {
        let Ok(mut request) = assemble_model_request_with_retained_tail_percent(
            model_profile,
            turn,
            context,
            self.agent_compaction_raw_retention_percent,
        ) else {
            return;
        };
        request.available_mcp_tools = available_mcp_tools.to_vec();
        let (diagnostics, diagnostics_failed) = if request.provider == "openai" {
            match openai_prompt_cache_diagnostics_for_request(&request) {
                Ok(diagnostics) => (Some(diagnostics), false),
                Err(_) => (None, true),
            }
        } else {
            (None, false)
        };
        self.runtime_metrics.record_provider_request_shape(
            &request,
            diagnostics.as_ref(),
            diagnostics_failed,
        );
    }

    /// Runs the append agent trace maap response operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_agent_trace_maap_response(
        &mut self,
        turn: &AgentTurnRecord,
        response: &ModelResponse,
    ) -> Result<()> {
        self.append_agent_trace_maap_value_with_retained(
            &turn.pane_id,
            &turn.turn_id,
            "response",
            runtime_model_response_trace_json(response, self.agent_trace_enabled(&turn.pane_id)),
            runtime_model_response_trace_json(response, true),
        )
    }

    /// Runs the append agent trace maap action results operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_agent_trace_maap_action_results(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        label: &str,
        results: &[ActionResult],
    ) -> Result<()> {
        self.append_agent_trace_maap_value_with_retained(
            pane_id,
            turn_id,
            label,
            runtime_action_results_trace_json(results, self.agent_trace_enabled(pane_id)),
            runtime_action_results_trace_json(results, true),
        )
    }

    /// Runs the append agent trace provider error operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_agent_trace_provider_error(
        &mut self,
        turn: &AgentTurnRecord,
        provider_id: &str,
        model_profile: &ModelProfile,
        error: &MezError,
    ) -> Result<()> {
        let display_include_shell_view = self.agent_trace_enabled(&turn.pane_id);
        let display_value = runtime_agent_provider_error_trace_json(
            provider_id,
            model_profile,
            error,
            display_include_shell_view,
        );
        let retained_value =
            runtime_agent_provider_error_trace_json(provider_id, model_profile, error, true);
        self.append_agent_trace_maap_value_with_retained(
            &turn.pane_id,
            &turn.turn_id,
            "provider_error",
            display_value,
            retained_value,
        )
    }

    /// Runs the start agent turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn start_agent_turn(&mut self, turn: AgentTurnRecord) -> Result<AgentShellSession> {
        self.require_live()?;
        runtime_pane_by_id(&self.session, &turn.pane_id)?;
        self.agent_shell_store
            .ensure_session(turn.pane_id.as_str())?;
        if self
            .agent_shell_store
            .get(turn.pane_id.as_str())
            .and_then(|session| session.running_turn_id.as_deref())
            .is_some()
        {
            return Err(MezError::conflict(
                "agent shell session already has a running turn",
            ));
        }

        self.agent_turn_ledger.start_turn(turn.clone())?;
        self.runtime_metrics.record_agent_turn_started();
        self.agent_shell_store
            .start_turn(turn.pane_id.as_str(), turn.turn_id.clone())?;
        self.append_agent_trace_turn_transition(
            &turn,
            turn.state,
            AgentTurnState::Running,
            "runtime_start_agent_turn",
        )?;
        self.checkpoint_agent_session_metadata()?;
        self.agent_shell_store
            .get(turn.pane_id.as_str())
            .cloned()
            .ok_or_else(|| MezError::invalid_state("started agent shell session was not retained"))
    }

    /// Clears joined-subagent dependencies owned by or waiting on a turn.
    pub(super) fn clear_joined_subagent_dependencies_for_turn(&mut self, turn_id: &str) {
        self.joined_subagent_dependencies
            .retain(|child_turn_id, dependency| {
                child_turn_id != turn_id
                    && dependency.parent_turn_id != turn_id
                    && dependency.child_turn_id != turn_id
            });
    }

    /// Reports whether one joined-subagent dependency still has a live child
    /// turn that can make progress.
    pub(super) fn joined_subagent_dependency_has_live_child(
        &self,
        dependency: &JoinedSubagentDependency,
    ) -> bool {
        self.agent_turn_ledger.turns().iter().any(|turn| {
            turn.turn_id == dependency.child_turn_id
                && matches!(
                    turn.state,
                    AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
                )
        })
    }

    /// Reports whether a running parent execution is waiting on a live joined
    /// subagent dependency.
    ///
    /// A running `spawn_agent` action only represents progress when it still
    /// maps to the child turn that was created for that specific parent action.
    /// Stale action results without a live dependency must not mask a stranded
    /// parent turn.
    pub(super) fn execution_waiting_for_live_joined_subagents(
        &self,
        parent_turn_id: &str,
        execution: &AgentTurnExecution,
    ) -> bool {
        execution.terminal_state == AgentTurnState::Running
            && execution.action_results.iter().any(|result| {
                result.action_type == "spawn_agent"
                    && result.status == ActionStatus::Running
                    && self
                        .joined_subagent_dependencies
                        .values()
                        .any(|dependency| {
                            dependency.parent_turn_id == parent_turn_id
                                && dependency.parent_action_id == result.action_id
                                && self.joined_subagent_dependency_has_live_child(dependency)
                        })
            })
    }

    /// Queues one bounded provider continuation after model-correctable action
    /// failures.
    ///
    /// Real execution failures are useful model context: a bad shell command,
    /// timeout, or failed tool call often gives the model enough information to
    /// correct itself. Policy denials, rejected actions, cancellations, and
    /// user interrupts are intentionally excluded because repeating them would
    /// violate user intent or approval boundaries.
    pub(super) fn queue_agent_failure_feedback_for_correction(
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
        let mut attempts_after_update = Vec::with_capacity(attempt_keys.len());
        let mut any_budget_remaining = false;
        for attempt_key in &attempt_keys {
            let attempts = self
                .agent_turn_failure_feedback_attempts
                .entry(attempt_key.clone())
                .or_insert(0);
            if *attempts < attempt_limit {
                *attempts += 1;
                any_budget_remaining = true;
            }
            attempts_after_update.push(*attempts);
        }
        let exhausted = !any_budget_remaining;
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
        context.blocks.push(ContextBlock {
            source: ContextSourceKind::LocalMessage,
            label: "action failure feedback".to_string(),
            content: format!(
                "[ephemeral action failure feedback]\n\
                 attempt={} max={}\n\
                 One or more actions failed during this turn. Use the action result context above to correct the plan for the same user request. Do not repeat an identical failed action unless you changed the inputs or can explain why the repeat is necessary. Emit a new MAAP action batch with a visible or executable next step.{}{}{}{}",
                attempt,
                attempt_limit,
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
        let feedback_status_line =
            runtime_failure_feedback_status_line(execution, attempt, attempt_limit);
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
    pub(super) fn clear_agent_failure_feedback_attempts_for_turn(&mut self, turn_id: &str) {
        let scoped_prefix = format!("{turn_id}:");
        self.agent_turn_failure_feedback_attempts
            .retain(|key, _| key != turn_id && !key.starts_with(&scoped_prefix));
    }

    /// Appends one action result to the active model context if it has not
    /// already been recorded.
    fn append_action_result_context_if_absent(
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

    /// Updates agent prompt display lines only while the pane still exists.
    ///
    /// Terminal subagent completion can close the child pane as part of final
    /// cleanup after the parent receives a task result. Late presentation
    /// updates for that child are no longer meaningful and must not turn a
    /// successful terminal cleanup path into a `pane not found` failure.
    fn set_agent_prompt_display_lines_if_pane_present(
        &mut self,
        pane_id: &str,
        display_lines: Vec<String>,
    ) -> Result<()> {
        if self.find_pane_descriptor(pane_id).is_none() {
            return Ok(());
        }
        self.set_agent_prompt_display_lines(pane_id, display_lines)
    }

    /// Runs the finish agent turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn finish_agent_turn(
        &mut self,
        pane_id: &str,
        turn_id: &str,
        state: AgentTurnState,
    ) -> Result<AgentShellSession> {
        self.require_live()?;
        runtime_pane_by_id(&self.session, pane_id)?;
        let running_turn = self
            .agent_shell_store
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            .ok_or_else(|| MezError::invalid_state("agent shell session has no running turn"))?;
        if running_turn != turn_id {
            return Err(MezError::invalid_args(
                "finished turn does not match running agent shell turn",
            ));
        }
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;

        if state == AgentTurnState::Failed
            && let Some(execution) = self.agent_turn_executions.get(turn_id).cloned()
            && execution.terminal_state == AgentTurnState::Failed
        {
            let reason = runtime_unrecovered_failure_reason(
                turn_id,
                &execution,
                self.agent_action_failure_retry_limit,
                &self.agent_turn_failure_feedback_attempts,
            );
            self.present_unrecovered_agent_failure_diagnostics_to_terminal_buffer(
                pane_id, &execution, &reason,
            )?;
        }
        if matches!(
            state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            self.emit_subagent_task_result_for_state(&turn, state)?;
        }
        if let Some(footer) = runtime_agent_finished_footer_line(&turn, state) {
            self.append_agent_status_text_to_terminal_buffer(pane_id, &footer)?;
        }
        let previous_state = turn.state;
        self.runtime_metrics.record_agent_turn_finished(state);
        self.agent_turn_ledger.finish_turn(turn_id, state)?;
        self.append_agent_trace_turn_transition(&turn, previous_state, state, "finish_agent_turn")?;
        self.agent_turn_contexts.remove(turn_id);
        self.agent_turn_executions.remove(turn_id);
        self.agent_turn_pending_steering.remove(turn_id);
        self.clear_agent_failure_feedback_attempts_for_turn(turn_id);
        self.agent_turn_shell_dispatch_history.remove(turn_id);
        self.agent_turn_network_action_history.remove(turn_id);
        self.clear_joined_subagent_dependencies_for_turn(turn_id);
        self.clear_agent_pre_shell_hook_completions_for_turn(turn_id);
        self.agent_turn_model_profiles.remove(turn_id);
        self.pending_agent_provider_tasks.remove(turn_id);
        self.claimed_agent_provider_tasks.remove(turn_id);
        self.blocked_agent_approval_refs
            .retain(|_, approval_ref| approval_ref.turn_id != turn_id);
        let finished = self
            .agent_shell_store
            .finish_turn(pane_id, turn_id)?
            .clone();
        if finished.visibility == AgentShellVisibility::Hidden {
            self.advance_pane_shell_prompt_after_agent_exit(pane_id)?;
        }
        if matches!(
            state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            self.close_terminal_subagent_pane_if_pending(&turn)?;
        }
        self.start_ready_agent_turns()?;
        self.checkpoint_agent_session_metadata()?;
        Ok(finished)
    }

    /// Cleans up a queued or blocked turn that is not currently bound as the
    /// pane shell's running turn.
    ///
    /// Some scheduler states, especially queued work and blocked turns whose
    /// scheduler slot has been released, need terminal cleanup without going
    /// through `AgentShellStore::finish_turn`. Callers are responsible for
    /// emitting any transcript or task-result records before using this helper.
    pub(super) fn finish_agent_turn_without_shell_session(
        &mut self,
        turn: &AgentTurnRecord,
        state: AgentTurnState,
    ) -> Result<AgentShellSession> {
        if state == AgentTurnState::Failed
            && let Some(execution) = self.agent_turn_executions.get(&turn.turn_id).cloned()
            && execution.terminal_state == AgentTurnState::Failed
        {
            let reason = runtime_unrecovered_failure_reason(
                &turn.turn_id,
                &execution,
                self.agent_action_failure_retry_limit,
                &self.agent_turn_failure_feedback_attempts,
            );
            self.present_unrecovered_agent_failure_diagnostics_to_terminal_buffer(
                &turn.pane_id,
                &execution,
                &reason,
            )?;
        }
        if let Some(footer) = runtime_agent_finished_footer_line(turn, state) {
            self.append_agent_status_text_to_terminal_buffer(&turn.pane_id, &footer)?;
        }
        self.runtime_metrics.record_agent_turn_finished(state);
        self.agent_turn_ledger.finish_turn(&turn.turn_id, state)?;
        self.append_agent_trace_turn_transition(
            turn,
            turn.state,
            state,
            "finish_agent_turn_without_shell_session",
        )?;
        self.agent_turn_contexts.remove(&turn.turn_id);
        self.agent_turn_executions.remove(&turn.turn_id);
        self.agent_turn_pending_steering.remove(&turn.turn_id);
        self.clear_agent_failure_feedback_attempts_for_turn(&turn.turn_id);
        self.agent_turn_shell_dispatch_history.remove(&turn.turn_id);
        self.agent_turn_network_action_history.remove(&turn.turn_id);
        self.clear_joined_subagent_dependencies_for_turn(&turn.turn_id);
        self.clear_agent_pre_shell_hook_completions_for_turn(&turn.turn_id);
        self.agent_turn_model_profiles.remove(&turn.turn_id);
        self.pending_agent_provider_tasks.remove(&turn.turn_id);
        self.claimed_agent_provider_tasks.remove(&turn.turn_id);
        self.blocked_agent_approval_refs
            .retain(|_, approval_ref| approval_ref.turn_id != turn.turn_id);
        let session = self
            .agent_shell_store
            .ensure_session(&turn.pane_id)?
            .clone();
        if matches!(
            state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            self.close_terminal_subagent_pane_if_pending(turn)?;
        }
        self.start_ready_agent_turns()?;
        self.checkpoint_agent_session_metadata()?;
        Ok(session)
    }

    /// Completes the scheduler entry for one running turn and finishes the
    /// terminal runtime turn through the cleanup path that matches its current
    /// pane shell binding.
    ///
    /// # Parameters
    /// - `turn`: The running turn being settled.
    /// - `state`: The terminal turn state to record.
    /// - `reason`: Trace reason attached to the scheduler transition.
    pub(super) fn complete_running_agent_turn_and_start_ready(
        &mut self,
        turn: &AgentTurnRecord,
        state: AgentTurnState,
        reason: &str,
    ) -> Result<AgentShellSession> {
        let _ = self.agent_scheduler.complete(&turn.turn_id);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "scheduler running -> {} reason={}",
                runtime_agent_turn_state_name(state),
                reason
            ),
        )?;
        if self
            .agent_shell_store
            .get(&turn.pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            == Some(turn.turn_id.as_str())
        {
            self.finish_agent_turn(&turn.pane_id, &turn.turn_id, state)
        } else {
            self.finish_agent_turn_without_shell_session(turn, state)
        }
    }

    /// Starts all scheduler work that is runnable in the current runtime
    /// state and emits scheduler-start status events for those turns.
    pub(super) fn start_ready_agent_turns(&mut self) -> Result<usize> {
        self.start_ready_agent_turns_suppressing_status_for(None)
    }

    /// Starts all scheduler work that is runnable in the current runtime
    /// state while suppressing the scheduler-start status event for a selected
    /// turn.
    ///
    /// The prompt-submission path uses this to preserve the existing
    /// model-profile-bearing status event for the newly submitted turn while
    /// still draining older runnable scheduler entries.
    pub(super) fn start_ready_agent_turns_suppressing_status_for(
        &mut self,
        suppressed_turn_id: Option<&str>,
    ) -> Result<usize> {
        let mut started = 0usize;
        while let Some(running) = self.agent_scheduler.start_ready() {
            let turn = self
                .agent_turn_ledger
                .turns()
                .iter()
                .find(|turn| turn.turn_id == running.turn_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("scheduled turn is missing from runtime ledger")
                })?;
            self.agent_turn_ledger.mark_turn_running(&running.turn_id)?;
            self.agent_shell_store
                .start_turn(&turn.pane_id, running.turn_id.clone())?;
            self.pending_agent_provider_tasks
                .insert(running.turn_id.clone());
            self.append_agent_trace_turn_transition(
                &turn,
                AgentTurnState::Queued,
                AgentTurnState::Running,
                "scheduler_start",
            )?;
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &running.turn_id,
                "provider_task queued reason=scheduler_start",
            )?;
            if suppressed_turn_id != Some(running.turn_id.as_str()) {
                self.append_lifecycle_event(
                    EventKind::AgentStatus,
                    format!(
                        r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"running","scheduler_started":true}}"#,
                        json_escape(&turn.pane_id),
                        json_escape(&running.turn_id)
                    ),
                )?;
            }
            started = started.saturating_add(1);
        }
        if started > 0 {
            self.checkpoint_agent_session_metadata()?;
        }
        Ok(started)
    }

    /// Fails queued, running, or blocked agent turns that belong to panes being
    /// closed outside the normal agent lifecycle.
    ///
    /// Shutdown paths call this before removing panes from the session model so
    /// final subagent task results can still identify the child pane, scheduled
    /// work is cancelled, pending shell transactions are interrupted, and any
    /// active subagent write scope is released.
    pub(super) fn fail_agent_turns_for_pane_shutdown(
        &mut self,
        pane_ids: &[String],
        reason: &str,
    ) -> Result<usize> {
        let pane_ids = pane_ids
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        if pane_ids.is_empty() {
            return Ok(0);
        }
        for pane_id in &pane_ids {
            let parent_agent_id = format!("agent-{pane_id}");
            self.close_subagent_descendants_for_parent_agent(&parent_agent_id, reason)?;
        }
        for pane_id in &pane_ids {
            self.pane_closing.insert((*pane_id).to_string());
        }
        let turns = self
            .agent_turn_ledger
            .turns()
            .iter()
            .filter(|turn| {
                pane_ids.contains(turn.pane_id.as_str())
                    && matches!(
                        turn.state,
                        AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut failed = 0usize;
        for turn in turns {
            let _ = self.agent_scheduler.cancel(&turn.turn_id);
            self.cancel_live_shell_transactions_for_turn(&turn.turn_id)?;
            let running_in_shell = self
                .agent_shell_store
                .get(&turn.pane_id)
                .and_then(|session| session.running_turn_id.as_deref())
                == Some(turn.turn_id.as_str());
            if running_in_shell {
                self.finish_agent_turn(&turn.pane_id, &turn.turn_id, AgentTurnState::Failed)?;
            } else {
                self.emit_subagent_task_result_for_state(&turn, AgentTurnState::Failed)?;
                self.agent_turn_ledger
                    .finish_turn(&turn.turn_id, AgentTurnState::Failed)?;
                self.append_agent_trace_turn_transition(
                    &turn,
                    turn.state,
                    AgentTurnState::Failed,
                    "pane_shutdown_without_shell_session",
                )?;
                self.agent_turn_contexts.remove(&turn.turn_id);
                self.agent_turn_executions.remove(&turn.turn_id);
                self.agent_turn_pending_steering.remove(&turn.turn_id);
                self.agent_turn_shell_dispatch_history.remove(&turn.turn_id);
                self.agent_turn_network_action_history.remove(&turn.turn_id);
                self.clear_joined_subagent_dependencies_for_turn(&turn.turn_id);
                self.clear_agent_pre_shell_hook_completions_for_turn(&turn.turn_id);
                self.agent_turn_model_profiles.remove(&turn.turn_id);
                self.pending_agent_provider_tasks.remove(&turn.turn_id);
                self.blocked_agent_approval_refs
                    .retain(|_, approval_ref| approval_ref.turn_id != turn.turn_id);
            }
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"failed","reason":"{}"}}"#,
                    json_escape(&turn.pane_id),
                    json_escape(&turn.turn_id),
                    json_escape(reason)
                ),
            )?;
            failed = failed.saturating_add(1);
        }
        self.start_ready_agent_turns()?;
        Ok(failed)
    }

    /// Closes every live subagent pane descended from one parent agent.
    ///
    /// # Parameters
    /// - `parent_agent_id`: Agent id whose child delegation tree should close.
    /// - `reason`: Human-readable shutdown reason used in trace/status events.
    pub(super) fn close_subagent_descendants_for_parent_agent(
        &mut self,
        parent_agent_id: &str,
        reason: &str,
    ) -> Result<usize> {
        let descendants = self.subagent_descendant_agent_ids_for_parent(parent_agent_id);
        if descendants.is_empty() {
            return Ok(0);
        }
        let Some(primary_client_id) = self.session.primary_client_id().cloned() else {
            for agent_id in descendants {
                if let Some(pane_id) = runtime_agent_pane_id(&agent_id) {
                    self.cleanup_removed_pane_runtime_state(pane_id.as_str());
                }
            }
            return Ok(0);
        };
        let mut closed = 0usize;
        for agent_id in descendants {
            let Some(pane_id) = runtime_agent_pane_id(&agent_id) else {
                self.subagent_lineage.remove(&agent_id);
                self.subagent_scope_declarations.remove(&agent_id);
                self.subagent_scopes.unregister(&agent_id);
                continue;
            };
            let pane_id_string = pane_id.to_string();
            if runtime_pane_by_id(&self.session, &pane_id_string).is_ok() {
                let params = format!(
                    r#"{{"pane_id":"{}","force":true}}"#,
                    json_escape(&pane_id_string)
                );
                self.dispatch_runtime_pane_close(&primary_client_id, &params)?;
                closed = closed.saturating_add(1);
                self.append_lifecycle_event(
                    EventKind::AgentStatus,
                    format!(
                        r#"{{"pane_id":"{}","agent_id":"{}","state":"closed","reason":"parent_agent_closed","parent_agent_id":"{}","detail":"{}"}}"#,
                        json_escape(&pane_id_string),
                        json_escape(&agent_id),
                        json_escape(parent_agent_id),
                        json_escape(reason)
                    ),
                )?;
            } else {
                self.cleanup_removed_pane_runtime_state(&pane_id_string);
            }
        }
        Ok(closed)
    }

    /// Returns descendant subagent ids deepest-first for deterministic closure.
    ///
    /// # Parameters
    /// - `parent_agent_id`: Direct or root parent agent id to inspect.
    fn subagent_descendant_agent_ids_for_parent(&self, parent_agent_id: &str) -> Vec<String> {
        let mut descendants = self
            .subagent_lineage
            .iter()
            .filter(|(_agent_id, lineage)| {
                lineage.parent_agent_id == parent_agent_id
                    || self.subagent_lineage_has_ancestor(lineage, parent_agent_id)
            })
            .map(|(agent_id, lineage)| (lineage.depth, agent_id.clone()))
            .collect::<Vec<_>>();
        descendants.sort_by(|(left_depth, left_agent), (right_depth, right_agent)| {
            right_depth
                .cmp(left_depth)
                .then_with(|| left_agent.cmp(right_agent))
        });
        descendants
            .into_iter()
            .map(|(_depth, agent_id)| agent_id)
            .collect()
    }

    /// Reports whether a lineage record is below a target ancestor.
    ///
    /// # Parameters
    /// - `lineage`: Child lineage to walk upward.
    /// - `ancestor_agent_id`: Candidate ancestor agent id.
    fn subagent_lineage_has_ancestor(
        &self,
        lineage: &super::types::RuntimeSubagentLineage,
        ancestor_agent_id: &str,
    ) -> bool {
        let mut current_parent = lineage.parent_agent_id.as_str();
        while !current_parent.is_empty() {
            if current_parent == ancestor_agent_id {
                return true;
            }
            let Some(parent_lineage) = self.subagent_lineage.get(current_parent) else {
                return false;
            };
            current_parent = parent_lineage.parent_agent_id.as_str();
        }
        false
    }

    /// Refreshes project guidance blocks on a stored turn context.
    ///
    /// Provider continuations can happen after file mutations and shell output
    /// observations. This keeps the discovered `AGENTS.md` content in every
    /// provider-bound context without duplicating stale guidance blocks.
    pub(super) fn refresh_agent_turn_project_guidance_context(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        let Some(instruction_files) = self
            .pane_instruction_files
            .get(&turn.pane_id)
            .cloned()
            .filter(|files| !files.is_empty())
        else {
            return Ok(());
        };
        let context = self
            .agent_turn_contexts
            .get(&turn.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let context = set_project_guidance_context(context, &instruction_files, 2)?;
        self.agent_turn_contexts
            .insert(turn.turn_id.clone(), context);
        Ok(())
    }

    /// Drains user steering prompts into the next provider-bound context.
    ///
    /// Mid-turn input is intentionally not treated as a new conversation turn.
    /// The block template tells the model that the text was submitted while
    /// the current turn was already active and should be incorporated from the
    /// next action boundary forward.
    fn drain_pending_agent_turn_steering_context(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<usize> {
        let Some(steering) = self.agent_turn_pending_steering.remove(&turn.turn_id) else {
            return Ok(0);
        };
        let count = steering.len();
        let context = self
            .agent_turn_contexts
            .get_mut(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        for (index, steering) in steering.into_iter().enumerate() {
            context.blocks.push(ContextBlock {
                source: ContextSourceKind::UserInstruction,
                label: format!(
                    "user steering input {} for active turn {}",
                    index + 1,
                    turn.turn_id
                ),
                content: runtime_agent_turn_steering_context_content(&steering),
            });
        }
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!("user_steering applied count={count} reason=provider_context_preparation"),
        )?;
        Ok(count)
    }

    /// Locally compacts active-turn context after a provider rejects the request
    /// as too large.
    ///
    /// This recovery path is intentionally independent of proactive
    /// `agents.auto_compact`: once the provider has rejected the exact request,
    /// the only recoverable continuation is to reduce model-visible active-turn
    /// context and retry with the same durable turn.
    pub(crate) fn recover_agent_provider_context_limit_failure(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        error: &MezError,
        attempt: u32,
    ) -> Result<bool> {
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.agent_id != agent_id.as_str() {
            return Err(MezError::invalid_args(
                "agent provider recovery agent id does not match turn",
            ));
        }
        if turn.state != AgentTurnState::Running {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        let Some(stored_model_profile) = self.agent_turn_model_profiles.get(turn_id).cloned()
        else {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Err(MezError::invalid_state(
                "runtime agent turn has no model profile",
            ));
        };
        let auto_sizing =
            self.runtime_auto_sizing_dispatch_for_turn(&turn, &stored_model_profile)?;
        let model_profile = runtime_auto_sizing_minimum_context_profile(
            &stored_model_profile,
            auto_sizing.as_ref(),
        );
        let context = self
            .agent_turn_contexts
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let budget_words = model_profile.context_window_budget_words();
        let recovery_attempt = attempt.max(1);
        let retained_tail_percent = self.agent_compaction_raw_retention_percent;
        let (compacted_context, report) =
            compact_model_context_for_budget_with_retained_tail_percent(
                context,
                budget_words,
                retained_tail_percent,
            )?;
        if !report.changed() {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                &format!(
                    "auto_compact recovery_skipped reason=provider_context_limit attempt={} budget_words={} retained_tail_percent={} error_kind={} no_compactable_blocks=true",
                    recovery_attempt,
                    budget_words,
                    retained_tail_percent,
                    runtime_mezzanine_error_code(error.kind())
                ),
            )?;
            self.append_agent_status_text_to_terminal_buffer(
                &turn.pane_id,
                "agent: provider rejected context as too large; no compactable active turn context remains",
            )?;
            return Ok(false);
        }
        self.agent_turn_contexts
            .insert(turn_id.to_string(), compacted_context);
        self.append_agent_status_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent: provider rejected context as too large; compacted active turn context budget_words={} retained_tail_percent={} compacted_blocks={} omitted_blocks={}",
                budget_words,
                retained_tail_percent,
                report.compacted_blocks,
                report.omitted_blocks
            ),
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!(
                "auto_compact recovery_applied reason=provider_context_limit attempt={} budget_words={} retained_tail_percent={} compacted_blocks={} omitted_blocks={} omitted_original_words={} error_kind={}",
                recovery_attempt,
                budget_words,
                retained_tail_percent,
                report.compacted_blocks,
                report.omitted_blocks,
                report.omitted_original_words,
                runtime_mezzanine_error_code(error.kind())
            ),
        )?;
        Ok(true)
    }

    /// Adds compact-output retry guidance after a provider cuts generation off
    /// at its output-token limit.
    ///
    /// This recovery path deliberately does not compact context: the provider
    /// accepted the input, but the model spent too much output budget. The
    /// durable active turn is retried with a temporary developer instruction
    /// and an escalated `max_output_tokens` provider option.
    pub(crate) fn recover_agent_provider_output_limit_failure(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        error: &MezError,
        attempt: u32,
    ) -> Result<bool> {
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.agent_id != agent_id.as_str() {
            return Err(MezError::invalid_args(
                "agent provider recovery agent id does not match turn",
            ));
        }
        if turn.state != AgentTurnState::Running {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        let Some(mut model_profile) = self.agent_turn_model_profiles.get(turn_id).cloned() else {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Err(MezError::invalid_state(
                "runtime agent turn has no model profile",
            ));
        };
        let retry_tokens = model_profile.output_limit_retry_tokens();
        model_profile
            .provider_options
            .insert("max_output_tokens".to_string(), retry_tokens.to_string());
        self.agent_turn_model_profiles
            .insert(turn_id.to_string(), model_profile);

        let context = self
            .agent_turn_contexts
            .get_mut(turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        context.blocks.retain(|block| {
            block.source != ContextSourceKind::Configuration
                || block.label != RUNTIME_PROVIDER_OUTPUT_LIMIT_RETRY_LABEL
        });
        context.blocks.push(ContextBlock {
            source: ContextSourceKind::Configuration,
            label: RUNTIME_PROVIDER_OUTPUT_LIMIT_RETRY_LABEL.to_string(),
            content: format!(
                "[ephemeral provider output-limit retry]\n\
                 The previous provider response was incomplete because generation hit max_output_tokens. \
                 Return one complete compact MAAP batch or one short final say. \
                 Do not include progress prose, plans, evidence lists, command logs, or explanations. \
                 Prefer the next focused executable action when work remains. \
                 This retry instruction is not durable transcript or future-turn context.\n\
                 attempt={} max_output_tokens={} error_kind={} error_message={}",
                attempt.max(1),
                retry_tokens,
                runtime_mezzanine_error_code(error.kind()),
                error.message()
            ),
        });
        self.append_agent_status_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent: provider response hit output limit; retrying compactly attempt={} max_output_tokens={}",
                attempt.max(1),
                retry_tokens
            ),
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!(
                "provider_request recovery_applied reason=provider_output_limit attempt={} max_output_tokens={} error_kind={}",
                attempt.max(1),
                retry_tokens,
                runtime_mezzanine_error_code(error.kind())
            ),
        )?;
        Ok(true)
    }

    /// Keeps an active turn alive when user steering arrived mid-request.
    ///
    /// If the model completed while a newer user prompt was waiting to be
    /// incorporated, finishing the turn would silently discard the user's
    /// steering. Instead, the runtime converts that completion into one more
    /// provider continuation so the pending input can be drained into context.
    fn continue_completed_turn_for_pending_steering(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<bool> {
        if execution.terminal_state != AgentTurnState::Completed
            || !self.agent_turn_pending_steering.contains_key(&turn.turn_id)
        {
            return Ok(false);
        }
        execution.terminal_state = AgentTurnState::Running;
        execution.final_turn = false;
        self.append_agent_status_text_to_terminal_buffer(
            &turn.pane_id,
            "agent: steering input arrived during provider work; continuing current turn",
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            "user_steering forced_continuation reason=provider_completed_before_context_applied",
        )?;
        Ok(true)
    }

    /// Returns the pane-local routing preference, falling back to
    /// the configured default when the pane has no explicit override.
    pub(super) fn agent_routing_enabled_for_pane(&self, pane_id: &str) -> bool {
        self.agent_routing_overrides
            .get(pane_id)
            .copied()
            .or_else(|| {
                self.agent_selected_personality_profile(pane_id)
                    .and_then(|profile| profile.routing_enabled)
            })
            .unwrap_or(self.agent_routing)
    }

    /// Builds an automatic sizing dispatch for the first provider request of a
    /// turn.
    fn runtime_auto_sizing_dispatch_for_turn(
        &self,
        turn: &AgentTurnRecord,
        default_profile: &ModelProfile,
    ) -> Result<Option<RuntimeAutoSizingDispatch>> {
        if !self.agent_routing_enabled_for_pane(&turn.pane_id)
            || self.agent_turn_executions.contains_key(&turn.turn_id)
        {
            return Ok(None);
        }
        let config = self.runtime_auto_sizing_config_for_pane(&turn.pane_id);
        let router_profile = self
            .provider_registry
            .resolve_profile(&config.router_model_profile)?;
        let small =
            self.runtime_auto_sizing_target_profile("small", &config.small_model_profile)?;
        let medium =
            self.runtime_auto_sizing_target_profile("medium", &config.medium_model_profile)?;
        let large =
            self.runtime_auto_sizing_target_profile("large", &config.large_model_profile)?;
        Ok(Some(RuntimeAutoSizingDispatch {
            router_profile_name: config.router_model_profile.clone(),
            router_profile,
            default_profile_name: turn.model_profile.clone(),
            default_profile: default_profile.clone(),
            small,
            medium,
            large,
            turn_metadata: self.runtime_auto_sizing_turn_metadata(turn),
            allowed_reasoning_efforts: config.allowed_reasoning_efforts.clone(),
            fallback_policy: config.fallback_policy,
        }))
    }

    /// Builds bounded non-conversation metadata for the internal router.
    fn runtime_auto_sizing_turn_metadata(&self, turn: &AgentTurnRecord) -> Option<String> {
        let mut lines = Vec::new();
        if let Some(parent_turn_id) = turn.parent_turn_id.as_deref() {
            lines.push(format!("parent_turn_id={parent_turn_id}"));
        }
        if let Some(lineage) = self.subagent_lineage.get(&turn.agent_id) {
            lines.push(format!("parent_agent_id={}", lineage.parent_agent_id));
            lines.push(format!("root_agent_id={}", lineage.root_agent_id));
            lines.push(format!("subagent_display_name={}", lineage.display_name));
        }
        if let Some(scope) = self.subagent_scope_declaration_for_turn(turn) {
            lines.push(format!(
                "subagent_cooperation_mode={}",
                runtime_cooperation_mode_name(scope.cooperation_mode)
            ));
            lines.push(format!(
                "subagent_current_directory={}",
                scope.current_directory
            ));
            lines.push(format!(
                "subagent_read_scopes={}",
                scope.read_scopes.join(",")
            ));
            lines.push(format!(
                "subagent_write_scopes={}",
                scope.write_scopes.join(",")
            ));
        }
        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }

    /// Resolves one configured auto-sizing target profile.
    fn runtime_auto_sizing_target_profile(
        &self,
        size: &str,
        profile_name: &str,
    ) -> Result<RuntimeAutoSizingTargetProfile> {
        let profile = self.provider_registry.resolve_profile(profile_name)?;
        let provider_config = self.provider_registry.provider(&profile.provider);
        Ok(RuntimeAutoSizingTargetProfile {
            size: size.to_string(),
            profile_name: profile_name.to_string(),
            supported_reasoning_efforts: runtime_auto_sizing_reasoning_levels_for_profile(
                provider_config,
                &profile,
            ),
            profile,
        })
    }

    /// Logs a bounded auto-sizing decision without placing router
    /// correspondence into model context or transcript content.
    #[cfg(test)]
    fn record_auto_sizing_outcome(
        &mut self,
        turn: &AgentTurnRecord,
        profile: &ModelProfile,
        decision: Option<&super::RuntimeAutoSizingDecision>,
        fallback: Option<&str>,
    ) -> Result<()> {
        if let Some(decision) = decision {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "auto_sizing selected size={} model={} reasoning={} confidence={:.2}",
                    decision.size, profile.model, decision.reasoning_effort, decision.confidence
                ),
            )?;
            self.append_agent_status_text_to_terminal_buffer(
                &turn.pane_id,
                &format!(
                    "agent: routing selected {} reasoning on {}",
                    decision.reasoning_effort, profile.model
                ),
            )?;
        } else if let Some(fallback) = fallback {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "auto_sizing fallback model={} reasoning={} error={}",
                    profile.model,
                    profile.reasoning_profile.as_deref().unwrap_or("none"),
                    fallback
                ),
            )?;
            self.append_agent_status_text_to_terminal_buffer(
                &turn.pane_id,
                &format!("agent: routing fallback to {}: {}", profile.model, fallback),
            )?;
        }
        Ok(())
    }

    /// Runs the execute agent turn with provider operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn execute_agent_turn_with_provider<P: ModelProvider>(
        &mut self,
        turn_id: &str,
        provider: &P,
        mut model_profile: ModelProfile,
    ) -> Result<AgentTurnExecution> {
        self.require_live()?;
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        if turn.state != AgentTurnState::Running {
            return Err(MezError::conflict(
                "only running runtime agent turns can execute through a provider",
            ));
        }
        self.agent_turn_model_profiles
            .insert(turn_id.to_string(), model_profile.clone());
        self.refresh_agent_turn_project_guidance_context(&turn)?;
        self.drain_pending_agent_turn_steering_context(&turn)?;
        let context = self
            .agent_turn_contexts
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let mcp_summary = self.mcp_registry.prompt_summary();
        let context = append_mcp_context(context, &mcp_summary)?;
        self.agent_turn_contexts
            .insert(turn_id.to_string(), context.clone());
        if let Some(auto_sizing) =
            self.runtime_auto_sizing_dispatch_for_turn(&turn, &model_profile)?
        {
            let (selected_profile, decision, fallback) =
                runtime_execute_auto_sizing_with_provider(provider, &auto_sizing, &turn, &context);
            self.record_auto_sizing_outcome(
                &turn,
                &selected_profile,
                decision.as_ref(),
                fallback.as_deref(),
            )?;
            model_profile = selected_profile;
            self.agent_turn_model_profiles
                .insert(turn_id.to_string(), model_profile.clone());
        }
        if let Some(block) = self.run_configured_pre_action_hooks(
            HookEvent::AgentTurnStart,
            &runtime_agent_turn_start_hook_payload(&turn, &model_profile),
        )? {
            self.fail_agent_turn_for_hook_block(&turn, &model_profile, &block)?;
            return Err(MezError::forbidden(format!(
                "agent turn blocked by hook `{}`: {}",
                block.hook_id, block.message
            )));
        }
        let context = self
            .agent_turn_contexts
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let available_mcp_servers = mcp_summary
            .available_tools
            .iter()
            .map(|tool| tool.server_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        self.append_provider_request_audit(
            &turn,
            &model_profile,
            provider.provider_id(),
            "started",
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "provider_request started provider={} model={} context_blocks={}",
                provider.provider_id(),
                model_profile.model,
                context.blocks.len()
            ),
        )?;
        self.record_runtime_provider_request_shape_for_context(
            &model_profile,
            &turn,
            &context,
            &mcp_summary.available_tools,
        );
        if self.agent_debug_enabled(&turn.pane_id) {
            match assemble_model_request_with_retained_tail_percent(
                &model_profile,
                &turn,
                &context,
                self.agent_compaction_raw_retention_percent,
            ) {
                Ok(mut request) => {
                    request.available_mcp_tools = mcp_summary.available_tools.clone();
                    self.append_agent_trace_maap_request(&turn, &request)?;
                }
                Err(error) => {
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "maap request trace unavailable error_kind={} error={}",
                            runtime_mezzanine_error_code(error.kind()),
                            error.message()
                        ),
                    )?;
                }
            }
        }
        self.append_agent_verbose_status_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent: thinking with {} model {}",
                provider.provider_id(),
                model_profile.model
            ),
        )?;
        let subagent_scope = self.subagent_scope_declaration_for_turn(&turn);
        let path_scopes = if subagent_scope.is_some() {
            None
        } else {
            self.path_scopes_for_pane(&turn.pane_id)
        };
        let permission_policy = self.permission_policy_for_turn(&turn);
        let mut provider_context = context;
        let mut context_limit_recovery_attempts = 0u32;
        let mut output_limit_recovery_attempts = 0u32;
        let execution = loop {
            let mut provider_ledger = AgentTurnLedger::new(false);
            let runner = AgentTurnRunner {
                provider,
                model_profile: model_profile.clone(),
                permissions: &permission_policy,
                approvals: &self.session_approvals,
                path_scopes: path_scopes.as_ref(),
                subagent_scope: subagent_scope.as_ref(),
                available_mcp_servers: available_mcp_servers.clone(),
                available_mcp_tools: &mcp_summary.available_tools,
            };
            match runner.run_turn(&mut provider_ledger, turn.clone(), provider_context.clone()) {
                Ok(execution) => break execution,
                Err(error) => {
                    self.append_agent_trace_provider_error(
                        &turn,
                        provider.provider_id(),
                        &model_profile,
                        &error,
                    )?;
                    self.append_provider_request_failure_audit(
                        &turn,
                        &model_profile,
                        provider.provider_id(),
                        &error,
                    )?;
                    if provider_error_is_context_limit_exceeded(
                        error.message(),
                        error.provider_failure_json(),
                    ) && context_limit_recovery_attempts
                        < RUNTIME_PROVIDER_CONTEXT_LIMIT_RETRY_LIMIT
                    {
                        context_limit_recovery_attempts =
                            context_limit_recovery_attempts.saturating_add(1);
                        let agent_id = AgentId::opaque(turn.agent_id.clone()).ok_or_else(|| {
                            MezError::invalid_state("runtime agent turn agent id is invalid")
                        })?;
                        if self.recover_agent_provider_context_limit_failure(
                            &agent_id,
                            turn_id,
                            &error,
                            context_limit_recovery_attempts,
                        )? {
                            provider_context =
                                self.agent_turn_contexts.get(turn_id).cloned().ok_or_else(
                                    || {
                                        MezError::invalid_state(
                                            "runtime agent turn context is unavailable",
                                        )
                                    },
                                )?;
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "provider_request retrying reason=provider_context_limit attempt={context_limit_recovery_attempts}"
                                ),
                            )?;
                            continue;
                        }
                    }
                    if provider_error_is_output_limit_exceeded(
                        error.message(),
                        error.provider_failure_json(),
                    ) && output_limit_recovery_attempts
                        < RUNTIME_PROVIDER_OUTPUT_LIMIT_RETRY_LIMIT
                    {
                        output_limit_recovery_attempts =
                            output_limit_recovery_attempts.saturating_add(1);
                        let agent_id = AgentId::opaque(turn.agent_id.clone()).ok_or_else(|| {
                            MezError::invalid_state("runtime agent turn agent id is invalid")
                        })?;
                        if self.recover_agent_provider_output_limit_failure(
                            &agent_id,
                            turn_id,
                            &error,
                            output_limit_recovery_attempts,
                        )? {
                            provider_context =
                                self.agent_turn_contexts.get(turn_id).cloned().ok_or_else(
                                    || {
                                        MezError::invalid_state(
                                            "runtime agent turn context is unavailable",
                                        )
                                    },
                                )?;
                            model_profile = self
                                .agent_turn_model_profiles
                                .get(turn_id)
                                .cloned()
                                .ok_or_else(|| {
                                    MezError::invalid_state(
                                        "runtime agent turn model profile is unavailable",
                                    )
                                })?;
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "provider_request retrying reason=provider_output_limit attempt={output_limit_recovery_attempts}"
                                ),
                            )?;
                            continue;
                        }
                    }
                    self.runtime_metrics.record_provider_failure();
                    self.fail_agent_turn_for_provider_error(
                        &turn,
                        provider.provider_id(),
                        &model_profile,
                        &error,
                    )?;
                    return Err(error);
                }
            }
        };
        self.apply_agent_provider_execution(
            &turn,
            &model_profile,
            provider.provider_id(),
            execution,
        )
    }

    /// Runs the execute agent turn with provider async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub async fn execute_agent_turn_with_provider_async<P: ModelProvider>(
        &mut self,
        turn_id: &str,
        provider: &P,
        mut model_profile: ModelProfile,
    ) -> Result<AgentTurnExecution> {
        self.require_live()?;
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        if turn.state != AgentTurnState::Running {
            return Err(MezError::conflict(
                "only running runtime agent turns can execute through a provider",
            ));
        }
        self.agent_turn_model_profiles
            .insert(turn_id.to_string(), model_profile.clone());
        let context = self
            .agent_turn_contexts
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let mcp_summary = self.mcp_registry.prompt_summary();
        let context = append_mcp_context(context, &mcp_summary)?;
        self.agent_turn_contexts
            .insert(turn_id.to_string(), context.clone());
        if let Some(auto_sizing) =
            self.runtime_auto_sizing_dispatch_for_turn(&turn, &model_profile)?
        {
            let (selected_profile, decision, fallback) =
                runtime_execute_auto_sizing_with_provider(provider, &auto_sizing, &turn, &context);
            self.record_auto_sizing_outcome(
                &turn,
                &selected_profile,
                decision.as_ref(),
                fallback.as_deref(),
            )?;
            model_profile = selected_profile;
            self.agent_turn_model_profiles
                .insert(turn_id.to_string(), model_profile.clone());
        }
        if let Some(block) = self.run_configured_pre_action_hooks(
            HookEvent::AgentTurnStart,
            &runtime_agent_turn_start_hook_payload(&turn, &model_profile),
        )? {
            self.fail_agent_turn_for_hook_block(&turn, &model_profile, &block)?;
            return Err(MezError::forbidden(format!(
                "agent turn blocked by hook `{}`: {}",
                block.hook_id, block.message
            )));
        }
        let context = self
            .agent_turn_contexts
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let available_mcp_servers = mcp_summary
            .available_tools
            .iter()
            .map(|tool| tool.server_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        self.append_provider_request_audit(
            &turn,
            &model_profile,
            provider.provider_id(),
            "started",
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "provider_request started provider={} model={} context_blocks={}",
                provider.provider_id(),
                model_profile.model,
                context.blocks.len()
            ),
        )?;
        self.record_runtime_provider_request_shape_for_context(
            &model_profile,
            &turn,
            &context,
            &mcp_summary.available_tools,
        );
        let subagent_scope = self.subagent_scope_declaration_for_turn(&turn);
        let path_scopes = if subagent_scope.is_some() {
            None
        } else {
            self.path_scopes_for_pane(&turn.pane_id)
        };
        let permission_policy = self.permission_policy_for_turn(&turn);
        let mut provider_context = context;
        let mut context_limit_recovery_attempts = 0u32;
        let mut output_limit_recovery_attempts = 0u32;
        let execution = loop {
            let mut provider_ledger = AgentTurnLedger::new(false);
            let runner = AgentTurnRunner {
                provider,
                model_profile: model_profile.clone(),
                permissions: &permission_policy,
                approvals: &self.session_approvals,
                path_scopes: path_scopes.as_ref(),
                subagent_scope: subagent_scope.as_ref(),
                available_mcp_servers: available_mcp_servers.clone(),
                available_mcp_tools: &mcp_summary.available_tools,
            };
            match runner.run_turn(&mut provider_ledger, turn.clone(), provider_context.clone()) {
                Ok(execution) => break execution,
                Err(error) => {
                    self.append_agent_trace_provider_error(
                        &turn,
                        provider.provider_id(),
                        &model_profile,
                        &error,
                    )?;
                    self.append_provider_request_failure_audit(
                        &turn,
                        &model_profile,
                        provider.provider_id(),
                        &error,
                    )?;
                    if provider_error_is_context_limit_exceeded(
                        error.message(),
                        error.provider_failure_json(),
                    ) && context_limit_recovery_attempts
                        < RUNTIME_PROVIDER_CONTEXT_LIMIT_RETRY_LIMIT
                    {
                        context_limit_recovery_attempts =
                            context_limit_recovery_attempts.saturating_add(1);
                        let agent_id = AgentId::opaque(turn.agent_id.clone()).ok_or_else(|| {
                            MezError::invalid_state("runtime agent turn agent id is invalid")
                        })?;
                        if self.recover_agent_provider_context_limit_failure(
                            &agent_id,
                            turn_id,
                            &error,
                            context_limit_recovery_attempts,
                        )? {
                            provider_context =
                                self.agent_turn_contexts.get(turn_id).cloned().ok_or_else(
                                    || {
                                        MezError::invalid_state(
                                            "runtime agent turn context is unavailable",
                                        )
                                    },
                                )?;
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "provider_request retrying reason=provider_context_limit attempt={context_limit_recovery_attempts}"
                                ),
                            )?;
                            continue;
                        }
                    }
                    if provider_error_is_output_limit_exceeded(
                        error.message(),
                        error.provider_failure_json(),
                    ) && output_limit_recovery_attempts
                        < RUNTIME_PROVIDER_OUTPUT_LIMIT_RETRY_LIMIT
                    {
                        output_limit_recovery_attempts =
                            output_limit_recovery_attempts.saturating_add(1);
                        let agent_id = AgentId::opaque(turn.agent_id.clone()).ok_or_else(|| {
                            MezError::invalid_state("runtime agent turn agent id is invalid")
                        })?;
                        if self.recover_agent_provider_output_limit_failure(
                            &agent_id,
                            turn_id,
                            &error,
                            output_limit_recovery_attempts,
                        )? {
                            provider_context =
                                self.agent_turn_contexts.get(turn_id).cloned().ok_or_else(
                                    || {
                                        MezError::invalid_state(
                                            "runtime agent turn context is unavailable",
                                        )
                                    },
                                )?;
                            model_profile = self
                                .agent_turn_model_profiles
                                .get(turn_id)
                                .cloned()
                                .ok_or_else(|| {
                                    MezError::invalid_state(
                                        "runtime agent turn model profile is unavailable",
                                    )
                                })?;
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "provider_request retrying reason=provider_output_limit attempt={output_limit_recovery_attempts}"
                                ),
                            )?;
                            continue;
                        }
                    }
                    self.runtime_metrics.record_provider_failure();
                    self.fail_agent_turn_for_provider_error(
                        &turn,
                        provider.provider_id(),
                        &model_profile,
                        &error,
                    )?;
                    return Err(error);
                }
            }
        };
        self.apply_agent_provider_execution_async(
            &turn,
            &model_profile,
            provider.provider_id(),
            execution,
        )
        .await
    }

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
        execution: AgentTurnExecution,
    ) -> Result<bool> {
        self.require_live()?;
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.pending_agent_provider_tasks.remove(turn_id);
            self.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.state != AgentTurnState::Running {
            self.pending_agent_provider_tasks.remove(turn_id);
            self.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        let Some(mut model_profile) = self.agent_turn_model_profiles.get(turn_id).cloned() else {
            let error = MezError::invalid_state("runtime agent turn has no model profile");
            self.fail_agent_turn_after_provider_completion_application_error(
                &turn,
                execution.response.provider.as_str(),
                None,
                &error,
            );
            return Ok(true);
        };
        if let Err(error) =
            runtime_validate_provider_completion_identity(&turn, agent_id, turn_id, &execution)
        {
            let provider_id = execution.response.provider.clone();
            self.fail_agent_turn_after_provider_completion_application_error(
                &turn,
                &provider_id,
                Some(&model_profile),
                &error,
            );
            return Ok(true);
        }
        if let Err(error) = runtime_validate_provider_completion_execution(&turn, &execution) {
            let provider_id = execution.response.provider.clone();
            self.fail_agent_turn_after_provider_completion_application_error(
                &turn,
                &provider_id,
                Some(&model_profile),
                &error,
            );
            return Ok(true);
        }
        let execution_profile =
            runtime_apply_auto_sizing_execution_profile(model_profile.clone(), &execution.request);
        if execution_profile != model_profile {
            self.agent_turn_model_profiles
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
        self.pending_agent_provider_tasks.remove(turn_id);
        self.claimed_agent_provider_tasks.remove(turn_id);
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

    /// Appends the provider response's assistant-visible context to a running
    /// turn before any action results are observed.
    ///
    /// # Parameters
    /// - `turn`: The running agent turn receiving the assistant context block.
    /// - `execution`: The provider execution whose rationale and visible
    ///   assistant text should remain available to later provider requests.
    fn append_agent_execution_assistant_context(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let content = assistant_context_content_for_execution(execution);
        if content.trim().is_empty() {
            return Ok(());
        }
        let context = self
            .agent_turn_contexts
            .get_mut(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let label = format!("assistant response for {}", turn.turn_id);
        if context.blocks.iter().any(|block| {
            block.source == ContextSourceKind::TranscriptAssistant
                && block.label == label
                && block.content == content
        }) {
            return Ok(());
        }
        context.blocks.push(ContextBlock {
            source: ContextSourceKind::TranscriptAssistant,
            label,
            content,
        });
        Ok(())
    }

    /// Appends or updates the active-turn progress `say` ledger.
    ///
    /// # Parameters
    /// - `turn`: The running agent turn receiving the ledger context block.
    /// - `execution`: The provider execution whose progress `say` actions should
    ///   become explicit context for later continuations.
    fn append_agent_execution_progress_say_ledger_context(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let new_entries = runtime_progress_say_entries_for_execution(execution);
        if new_entries.is_empty() {
            return Ok(());
        }
        let context = self
            .agent_turn_contexts
            .get_mut(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let mut previous_entries = Vec::new();
        context.blocks.retain(|block| {
            let is_progress_say_ledger = block.source == ContextSourceKind::LocalMessage
                && block.label == RUNTIME_PROGRESS_SAY_LEDGER_LABEL;
            if is_progress_say_ledger {
                previous_entries.extend(runtime_progress_say_entries_from_ledger(&block.content));
            }
            !is_progress_say_ledger
        });
        let entries = runtime_merge_progress_say_entries(previous_entries, new_entries);
        if entries.is_empty() {
            return Ok(());
        }
        context.blocks.push(ContextBlock {
            source: ContextSourceKind::LocalMessage,
            label: RUNTIME_PROGRESS_SAY_LEDGER_LABEL.to_string(),
            content: runtime_progress_say_ledger_content(&entries),
        });
        Ok(())
    }

    /// Returns progress `say` entries already visible during an active turn.
    ///
    /// # Parameters
    /// - `turn_id`: Active turn whose current progress ledger should be read.
    fn current_turn_progress_say_entries(&self, turn_id: &str) -> Vec<String> {
        let Some(context) = self.agent_turn_contexts.get(turn_id) else {
            return Vec::new();
        };
        context
            .blocks
            .iter()
            .filter(|block| {
                block.source == ContextSourceKind::LocalMessage
                    && block.label == RUNTIME_PROGRESS_SAY_LEDGER_LABEL
            })
            .flat_map(|block| runtime_progress_say_entries_from_ledger(&block.content))
            .collect()
    }

    /// Suppresses progress `say` actions that repeat an already visible update.
    ///
    /// The provider still receives a successful action result explaining the
    /// suppression, but the duplicate text is removed before user display,
    /// assistant context, copy retention, and progress-ledger updates.
    ///
    /// # Parameters
    /// - `turn`: Active turn receiving the provider execution.
    /// - `execution`: Provider execution whose progress actions may be filtered.
    fn suppress_redundant_progress_say_actions(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<usize> {
        let mut visible_entries = self.current_turn_progress_say_entries(&turn.turn_id);
        let Some(batch) = execution.response.action_batch.as_mut() else {
            return Ok(0);
        };
        if let Some(rationale_entry) = runtime_normalize_progress_say_entry(&batch.rationale)
            && runtime_progress_say_entry_repeats_existing(&rationale_entry, &visible_entries)
        {
            batch.rationale.clear();
        }
        let mut suppressed_actions = Vec::new();
        let mut suppressed_action_ids = Vec::new();
        for action in &mut batch.actions {
            let AgentActionPayload::Say {
                status,
                text,
                content_type: _,
            } = &mut action.payload
            else {
                continue;
            };
            if *status != SayStatus::Progress {
                continue;
            }
            let Some(entry) = runtime_normalize_progress_say_entry(text) else {
                continue;
            };
            if runtime_progress_say_entry_repeats_existing(&entry, &visible_entries) {
                text.clear();
                action.rationale.clear();
                suppressed_action_ids.push(action.id.clone());
                suppressed_actions.push(action.clone());
            } else {
                visible_entries.push(entry);
            }
        }
        for action_id in &suppressed_action_ids {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "action {action_id} progress_say suppressed reason=repeated_current_turn_progress"
                ),
            )?;
        }
        for action in &suppressed_actions {
            if let Some(result) = execution
                .action_results
                .iter_mut()
                .find(|result| result.action_id == action.id)
            {
                *result = ActionResult::succeeded(
                    turn,
                    action,
                    vec![
                        "progress say suppressed because it repeated an already visible current-turn update; continue with only materially new progress".to_string(),
                    ],
                    Some(
                        r#"{"kind":"say","status":"progress","display":"suppressed_duplicate_progress","reason":"repeated_current_turn_progress"}"#
                            .to_string(),
                    ),
                );
            }
        }
        if !suppressed_actions.is_empty() {
            execution.terminal_state = runtime_agent_turn_state_from_action_results(
                &execution.action_results,
                execution.final_turn,
            );
        }
        Ok(suppressed_actions.len())
    }

    /// Runs the apply agent provider execution operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub(super) fn apply_agent_provider_execution(
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
        self.runtime_metrics
            .record_provider_response(&execution.response, execution.latest_response_usage);
        self.record_agent_provider_token_usage_with_profile(
            &turn.pane_id,
            execution.response.usage,
            execution.latest_response_usage,
            Some(model_profile),
        );
        self.record_agent_provider_quota_usage(&turn.pane_id, &execution.response.quota_usage);
        self.append_agent_trace_maap_response(turn, &execution.response)?;
        self.suppress_redundant_progress_say_actions(turn, &mut execution)?;
        self.present_agent_response_actions_to_terminal_buffer(&turn.pane_id, &execution)?;
        self.append_agent_execution_assistant_context(turn, &execution)?;
        self.append_agent_execution_progress_say_ledger_context(turn, &execution)?;
        self.record_agent_copy_output(turn, &execution);
        let skill_actions_executed =
            self.execute_running_skill_actions_for_turn(turn, &mut execution)?;
        let message_actions_executed =
            self.execute_running_message_actions_for_turn(turn, &mut execution)?;
        let network_actions_executed = 0usize;
        let mcp_actions_executed =
            self.execute_running_mcp_actions_for_turn(turn, &mut execution)?;
        let spawn_actions_executed =
            self.execute_running_spawn_actions_for_turn(turn, &mut execution)?;
        let config_actions_executed =
            self.execute_running_config_change_actions_for_turn(turn, &mut execution)?;
        let shell_actions_dispatched =
            self.dispatch_running_shell_actions_to_panes(turn, &mut execution)?;
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
        let failure_feedback_queued = self.queue_agent_failure_feedback_for_correction(
            turn,
            &mut execution,
            "provider_execution_failed_action",
        )?;
        let _ = self.continue_completed_turn_for_pending_steering(turn, &mut execution)?;
        self.present_deferred_agent_say_actions_to_terminal_buffer(&turn.pane_id, &execution)?;
        let mut persisted_transcript_entries = 0usize;
        if failure_feedback_queued {
            self.agent_turn_executions.remove(turn_id);
        } else if execution.terminal_state == AgentTurnState::Blocked {
            persisted_transcript_entries =
                self.persist_runtime_agent_turn_execution_transcript(turn, &execution)?;
            self.queue_blocked_approvals_for_execution(turn, &execution)?;
            self.agent_turn_executions
                .insert(turn_id.to_string(), execution.clone());
            let _ = self.agent_scheduler.block_running(turn_id);
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                "scheduler running -> blocked reason=approval_required",
            )?;
            self.pending_agent_provider_tasks.remove(turn_id);
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                "provider_task removed reason=blocked_waiting_approval",
            )?;
            self.agent_turn_ledger
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
            self.complete_running_agent_turn_and_start_ready(
                turn,
                execution.terminal_state,
                "provider_execution_settled",
            )?;
        } else {
            let waiting_for_joined_subagents =
                self.execution_waiting_for_live_joined_subagents(turn_id, &execution);
            if waiting_for_joined_subagents {
                self.agent_turn_executions
                    .insert(turn_id.to_string(), execution.clone());
                let _ = self.agent_scheduler.block_running(turn_id);
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    turn_id,
                    "scheduler running -> blocked reason=waiting_for_subagents",
                )?;
                self.pending_agent_provider_tasks.remove(turn_id);
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    turn_id,
                    "provider_task removed reason=waiting_for_subagents",
                )?;
                self.agent_turn_ledger
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
                self.pending_agent_provider_tasks
                    .insert(turn_id.to_string());
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    turn_id,
                    "provider_task queued reason=ready_for_provider_continuation",
                )?;
            }
            if !waiting_for_joined_subagents {
                self.agent_turn_executions
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
                    .saturating_add(config_actions_executed),
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
                    .saturating_add(config_actions_executed),
                persisted_transcript_entries,
            ),
        )?;
        Ok(execution)
    }

    /// Runs the apply agent provider execution async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn apply_agent_provider_execution_async(
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
        self.runtime_metrics
            .record_provider_response(&execution.response, execution.latest_response_usage);
        self.record_agent_provider_token_usage_with_profile(
            &turn.pane_id,
            execution.response.usage,
            execution.latest_response_usage,
            Some(model_profile),
        );
        self.record_agent_provider_quota_usage(&turn.pane_id, &execution.response.quota_usage);
        self.append_agent_trace_maap_response(turn, &execution.response)?;
        self.suppress_redundant_progress_say_actions(turn, &mut execution)?;
        self.present_agent_response_actions_to_terminal_buffer(&turn.pane_id, &execution)?;
        self.append_agent_execution_assistant_context(turn, &execution)?;
        self.append_agent_execution_progress_say_ledger_context(turn, &execution)?;
        self.record_agent_copy_output(turn, &execution);
        let skill_actions_executed =
            self.execute_running_skill_actions_for_turn(turn, &mut execution)?;
        let message_actions_executed =
            self.execute_running_message_actions_for_turn(turn, &mut execution)?;
        let network_actions_executed = self
            .execute_running_network_actions_for_turn_async(turn, &mut execution)
            .await?;
        let mcp_actions_executed = self
            .execute_running_mcp_actions_for_turn_async(turn, &mut execution)
            .await?;
        let spawn_actions_executed =
            self.execute_running_spawn_actions_for_turn(turn, &mut execution)?;
        let config_actions_executed =
            self.execute_running_config_change_actions_for_turn(turn, &mut execution)?;
        let shell_actions_dispatched =
            self.dispatch_running_shell_actions_to_panes(turn, &mut execution)?;
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
        let failure_feedback_queued = self.queue_agent_failure_feedback_for_correction(
            turn,
            &mut execution,
            "provider_execution_failed_action",
        )?;
        let _ = self.continue_completed_turn_for_pending_steering(turn, &mut execution)?;
        self.present_deferred_agent_say_actions_to_terminal_buffer(&turn.pane_id, &execution)?;
        let mut persisted_transcript_entries = 0usize;
        if failure_feedback_queued {
            self.agent_turn_executions.remove(turn_id);
        } else if execution.terminal_state == AgentTurnState::Blocked {
            persisted_transcript_entries =
                self.persist_runtime_agent_turn_execution_transcript(turn, &execution)?;
            self.queue_blocked_approvals_for_execution(turn, &execution)?;
            self.agent_turn_executions
                .insert(turn_id.to_string(), execution.clone());
            let _ = self.agent_scheduler.block_running(turn_id);
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                "scheduler running -> blocked reason=approval_required",
            )?;
            self.pending_agent_provider_tasks.remove(turn_id);
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                "provider_task removed reason=blocked_waiting_approval",
            )?;
            self.agent_turn_ledger
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
            self.complete_running_agent_turn_and_start_ready(
                turn,
                execution.terminal_state,
                "provider_execution_settled",
            )?;
        } else {
            let waiting_for_joined_subagents =
                self.execution_waiting_for_live_joined_subagents(turn_id, &execution);
            if waiting_for_joined_subagents {
                self.agent_turn_executions
                    .insert(turn_id.to_string(), execution.clone());
                let _ = self.agent_scheduler.block_running(turn_id);
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    turn_id,
                    "scheduler running -> blocked reason=waiting_for_subagents",
                )?;
                self.pending_agent_provider_tasks.remove(turn_id);
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    turn_id,
                    "provider_task removed reason=waiting_for_subagents",
                )?;
                self.agent_turn_ledger
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
                self.pending_agent_provider_tasks
                    .insert(turn_id.to_string());
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    turn_id,
                    "provider_task queued reason=ready_for_provider_continuation",
                )?;
            }
            if !waiting_for_joined_subagents {
                self.agent_turn_executions
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
                    .saturating_add(config_actions_executed),
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
                    .saturating_add(config_actions_executed),
                persisted_transcript_entries,
            ),
        )?;
        Ok(execution)
    }

    /// Runs the execution has pending shell dispatch operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn execution_has_pending_shell_dispatch(
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
    pub(super) fn turn_has_running_readiness_probe(&self, turn_id: &str) -> bool {
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
    fn record_shell_dispatch_history(&mut self, turn_id: &str, command: &str) {
        self.agent_turn_shell_dispatch_history
            .entry(turn_id.to_string())
            .or_default()
            .record(command.to_string());
    }

    /// Records a shell command that exited successfully for loop detection.
    pub(super) fn record_shell_dispatch_success(&mut self, turn_id: &str, command: &str) {
        self.agent_turn_shell_dispatch_history
            .entry(turn_id.to_string())
            .or_default()
            .record_success(command.to_string());
    }

    /// Keeps the network action dispatch boundary symmetrical with shell
    /// actions without enforcing a count-based per-turn cap.
    fn network_action_loop_guard_failure(
        &self,
        _turn: &AgentTurnRecord,
        _action: &AgentAction,
        _request: &str,
    ) -> Result<Option<ActionResult>> {
        Ok(None)
    }

    /// Records a runtime-owned network request for loop detection.
    fn record_network_action_history(&mut self, turn_id: &str, request: &str) {
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
    pub(super) fn dispatch_stored_running_shell_actions(
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
    pub(super) fn fail_pending_shell_action_for_hook_block(
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

    /// Claims one configured provider task for execution outside the runtime
    /// actor.
    ///
    /// The actor remains responsible for validating turn identity, running
    /// pre-request hooks, recording audit/trace state, snapshotting the policy
    /// and MCP context, and constructing the provider from runtime
    /// configuration. The returned dispatch contains only deterministic inputs
    /// needed by a supervised worker to perform the provider request and plan
    /// action results without holding the actor.
    pub fn claim_configured_agent_provider_task(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
    ) -> Result<Option<RuntimeAgentProviderDispatch>> {
        match self.try_claim_configured_agent_provider_task(agent_id, turn_id) {
            Ok(dispatch) => Ok(dispatch),
            Err(error) => {
                self.fail_configured_agent_provider_task(turn_id, &error)?;
                Ok(None)
            }
        }
    }

    /// Runs the try claim configured agent provider task operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn try_claim_configured_agent_provider_task(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
    ) -> Result<Option<RuntimeAgentProviderDispatch>> {
        self.require_live()?;
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(None);
        };
        if turn.agent_id != agent_id.as_str() {
            return Err(MezError::invalid_args(
                "agent provider dispatch agent id does not match turn",
            ));
        }
        if self
            .agent_turn_executions
            .get(turn_id)
            .is_some_and(|execution| self.execution_has_pending_shell_dispatch(turn_id, execution))
        {
            self.pending_agent_provider_tasks.remove(turn_id);
            let _ = self.dispatch_stored_running_shell_actions(turn_id)?;
            return Ok(None);
        }
        if !self.pending_agent_provider_tasks.contains(turn_id) {
            return Ok(None);
        }
        if turn.state != AgentTurnState::Running {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(None);
        }

        let model_profile = self
            .agent_turn_model_profiles
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn has no model profile"))?;
        let provider_config = self
            .provider_registry
            .provider(&model_profile.provider)
            .cloned()
            .ok_or_else(|| {
                MezError::config(format!(
                    "provider `{}` for active model profile is not configured",
                    model_profile.provider
                ))
            })?;
        let provider = match provider_config.kind.as_str() {
            "openai" => {
                self.append_credential_access_audit(
                    "openai",
                    &provider_config.auth_profile,
                    "provider_request",
                    "requested",
                )?;
                let auth_store = self.auth_store.as_ref().ok_or_else(|| {
                    MezError::invalid_state(
                        "OpenAI provider execution requires an attached auth store",
                    )
                })?;
                let endpoint_override = provider_config
                    .base_url
                    .as_deref()
                    .filter(|endpoint| !endpoint.is_empty());
                let provider_result = openai_provider_from_auth_store_with_provider_options(
                    auth_store,
                    endpoint_override,
                    &provider_config.options,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                );
                match provider_result {
                    Ok(provider) => {
                        self.append_credential_access_audit(
                            "openai",
                            &provider_config.auth_profile,
                            "provider_request",
                            "granted",
                        )?;
                        RuntimeAgentProviderDispatchProvider::OpenAi(provider)
                    }
                    Err(error) => {
                        self.append_credential_access_audit(
                            "openai",
                            &provider_config.auth_profile,
                            "provider_request",
                            "denied",
                        )?;
                        return Err(error);
                    }
                }
            }
            "deepseek" => {
                self.append_credential_access_audit(
                    "deepseek",
                    &provider_config.auth_profile,
                    "provider_request",
                    "requested",
                )?;
                let auth_store = self.auth_store.as_ref().ok_or_else(|| {
                    MezError::invalid_state(
                        "DeepSeek provider execution requires an attached auth store",
                    )
                })?;
                let endpoint_override = provider_config
                    .base_url
                    .as_deref()
                    .filter(|endpoint| !endpoint.is_empty());
                let provider_result = deepseek_provider_from_auth_store_with_provider_options(
                    auth_store,
                    endpoint_override,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                );
                match provider_result {
                    Ok(provider) => {
                        self.append_credential_access_audit(
                            "deepseek",
                            &provider_config.auth_profile,
                            "provider_request",
                            "granted",
                        )?;
                        RuntimeAgentProviderDispatchProvider::DeepSeek(provider)
                    }
                    Err(error) => {
                        self.append_credential_access_audit(
                            "deepseek",
                            &provider_config.auth_profile,
                            "provider_request",
                            "denied",
                        )?;
                        return Err(error);
                    }
                }
            }
            other => {
                return Err(MezError::config(format!(
                    "provider kind `{other}` is not supported for runtime execution"
                )));
            }
        };

        self.agent_turn_model_profiles
            .insert(turn_id.to_string(), model_profile.clone());
        self.refresh_agent_turn_project_guidance_context(&turn)?;
        self.drain_pending_agent_turn_steering_context(&turn)?;
        let context = self
            .agent_turn_contexts
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let mcp_summary = self.mcp_registry.prompt_summary();
        let context = append_mcp_context(context, &mcp_summary)?;
        self.agent_turn_contexts
            .insert(turn_id.to_string(), context.clone());
        let auto_sizing = self.runtime_auto_sizing_dispatch_for_turn(&turn, &model_profile)?;
        if let Some(auto_sizing) = auto_sizing.as_ref() {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "auto_sizing queued router_profile={} small={} medium={} large={}",
                    auto_sizing.router_profile_name,
                    auto_sizing.small.profile_name,
                    auto_sizing.medium.profile_name,
                    auto_sizing.large.profile_name
                ),
            )?;
            self.append_agent_verbose_status_text_to_terminal_buffer(
                &turn.pane_id,
                "agent: routing selecting model and reasoning effort",
            )?;
        }
        let auto_sizing_provider = if let Some(auto_sizing) = auto_sizing.as_ref()
            && auto_sizing.router_profile.provider != model_profile.provider
        {
            let router_provider_config = self
                .provider_registry
                .provider(&auto_sizing.router_profile.provider)
                .cloned()
                .ok_or_else(|| {
                    MezError::config(format!(
                        "auto-sizing router provider `{}` is not configured",
                        auto_sizing.router_profile.provider
                    ))
                })?;
            let router_auth_store = self.auth_store.as_ref().ok_or_else(|| {
                MezError::invalid_state(
                    "auto-sizing router provider requires an attached auth store",
                )
            })?;
            let endpoint_override = router_provider_config
                .base_url
                .as_deref()
                .filter(|endpoint| !endpoint.is_empty());
            let result = match router_provider_config.kind.as_str() {
                "openai" => openai_provider_from_auth_store_with_provider_options(
                    router_auth_store,
                    endpoint_override,
                    &router_provider_config.options,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                )
                .map(RuntimeAgentProviderDispatchProvider::OpenAi),
                "deepseek" => deepseek_provider_from_auth_store_with_provider_options(
                    router_auth_store,
                    endpoint_override,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                )
                .map(RuntimeAgentProviderDispatchProvider::DeepSeek),
                _ => Err(MezError::config(format!(
                    "auto-sizing router provider `{}` has unsupported kind `{}`",
                    auto_sizing.router_profile.provider, router_provider_config.kind
                ))),
            };
            match result {
                Ok(provider) => Some(provider),
                Err(error) => {
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "auto_sizing router provider unavailable error_kind={} error={}",
                            runtime_mezzanine_error_code(error.kind()),
                            error.message()
                        ),
                    )?;
                    None
                }
            }
        } else {
            None
        };
        if let Some(block) = self.run_configured_pre_action_hooks(
            HookEvent::AgentTurnStart,
            &runtime_agent_turn_start_hook_payload(&turn, &model_profile),
        )? {
            self.fail_agent_turn_for_hook_block(&turn, &model_profile, &block)?;
            return Err(MezError::forbidden(format!(
                "agent turn blocked by hook `{}`: {}",
                block.hook_id, block.message
            )));
        }
        let context = self
            .agent_turn_contexts
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let available_mcp_servers = mcp_summary
            .available_tools
            .iter()
            .map(|tool| tool.server_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        self.append_provider_request_audit(
            &turn,
            &model_profile,
            provider.provider_id(),
            "started",
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "provider_request started provider={} model={} context_blocks={}",
                provider.provider_id(),
                model_profile.model,
                context.blocks.len()
            ),
        )?;
        self.record_runtime_provider_request_shape_for_context(
            &model_profile,
            &turn,
            &context,
            &mcp_summary.available_tools,
        );
        if self.agent_debug_enabled(&turn.pane_id) {
            match assemble_model_request_with_retained_tail_percent(
                &model_profile,
                &turn,
                &context,
                self.agent_compaction_raw_retention_percent,
            ) {
                Ok(mut request) => {
                    request.available_mcp_tools = mcp_summary.available_tools.clone();
                    self.append_agent_trace_maap_request(&turn, &request)?;
                }
                Err(error) => {
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "maap request trace unavailable error_kind={} error={}",
                            runtime_mezzanine_error_code(error.kind()),
                            error.message()
                        ),
                    )?;
                }
            }
        }
        self.append_agent_verbose_status_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent: thinking with {} model {}",
                provider.provider_id(),
                model_profile.model
            ),
        )?;
        let subagent_scope = self.subagent_scope_declaration_for_turn(&turn);
        let path_scopes = if subagent_scope.is_some() {
            None
        } else {
            self.path_scopes_for_pane(&turn.pane_id)
        };
        let permission_policy = self.permission_policy_for_turn(&turn);
        self.pending_agent_provider_tasks.remove(turn_id);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            "provider_task claimed reason=async_provider_worker",
        )?;
        Ok(Some(RuntimeAgentProviderDispatch {
            turn,
            context,
            model_profile,
            auto_sizing,
            auto_sizing_provider,
            provider,
            permission_policy,
            session_approvals: self.session_approvals.clone(),
            path_scopes,
            subagent_scope,
            available_mcp_servers,
            available_mcp_tools: mcp_summary.available_tools,
        }))
    }

    /// Runs the fail configured agent provider task operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn fail_configured_agent_provider_task(
        &mut self,
        turn_id: &str,
        error: &MezError,
    ) -> Result<()> {
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.pending_agent_provider_tasks.remove(turn_id);
            self.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(());
        };
        if !matches!(
            turn.state,
            AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
        ) {
            self.pending_agent_provider_tasks.remove(turn_id);
            self.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(());
        }
        let Some(model_profile) = self.agent_turn_model_profiles.get(turn_id).cloned() else {
            self.pending_agent_provider_tasks.remove(turn_id);
            self.claimed_agent_provider_tasks.remove(turn_id);
            return Err(MezError::invalid_state(
                "runtime agent turn has no model profile",
            ));
        };
        self.pending_agent_provider_tasks.remove(turn_id);
        self.claimed_agent_provider_tasks.remove(turn_id);
        self.append_provider_request_failure_audit(
            &turn,
            &model_profile,
            &model_profile.provider,
            error,
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!(
                "provider_task failed provider={} error_kind={}",
                model_profile.provider,
                runtime_mezzanine_error_code(error.kind())
            ),
        )?;
        self.append_agent_trace_provider_error(
            &turn,
            &model_profile.provider,
            &model_profile,
            error,
        )?;
        self.runtime_metrics.record_provider_failure();
        self.fail_agent_turn_for_provider_error(
            &turn,
            &model_profile.provider,
            &model_profile,
            error,
        )
    }

    /// Runs the record agent provider retry event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn record_agent_provider_retry_event(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        error: &MezError,
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
    ) -> Result<bool> {
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.agent_id != agent_id.as_str() {
            return Err(MezError::invalid_args(
                "agent provider event agent id does not match turn",
            ));
        }
        if turn.state != AgentTurnState::Running {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        let Some(model_profile) = self.agent_turn_model_profiles.get(turn_id).cloned() else {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Err(MezError::invalid_state(
                "runtime agent turn has no model profile",
            ));
        };
        self.pending_agent_provider_tasks.remove(turn_id);
        self.append_provider_request_failure_audit(
            &turn,
            &model_profile,
            &model_profile.provider,
            error,
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!(
                "provider_task retry_scheduled provider={} error_kind={} attempt={} max_attempts={} delay_ms={}",
                model_profile.provider,
                runtime_mezzanine_error_code(error.kind()),
                attempt,
                max_attempts,
                delay_ms
            ),
        )?;
        self.append_agent_trace_provider_error(
            &turn,
            &model_profile.provider,
            &model_profile,
            error,
        )?;
        self.append_agent_status_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent: provider {} request failed; retrying attempt {attempt}/{max_attempts} in {} ms",
                model_profile.provider, delay_ms
            ),
        )?;
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"running","provider":"{}","provider_retry":"scheduled","attempt":{},"max_attempts":{},"delay_ms":{},"error_kind":"{}"}}"#,
                json_escape(&turn.pane_id),
                json_escape(turn_id),
                json_escape(&model_profile.provider),
                attempt,
                max_attempts,
                delay_ms,
                json_escape(runtime_mezzanine_error_code(error.kind()))
            ),
        )?;
        Ok(true)
    }

    /// Runs the queue agent provider retry task operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn queue_agent_provider_retry_task(
        &mut self,
        turn_id: &str,
        attempt: u64,
    ) -> Result<bool> {
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.state != AgentTurnState::Running {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        if !self.agent_turn_model_profiles.contains_key(turn_id) {
            return Err(MezError::invalid_state(
                "runtime agent turn has no model profile",
            ));
        }
        self.pending_agent_provider_tasks
            .insert(turn_id.to_string());
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!("provider_task queued reason=provider_retry_timer attempt={attempt}"),
        )?;
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"running","provider_retry":"ready","attempt":{}}}"#,
                json_escape(&turn.pane_id),
                json_escape(turn_id),
                attempt
            ),
        )?;
        Ok(true)
    }

    /// Applies an async provider-worker failure event through actor-owned
    /// runtime ingress.
    ///
    /// Provider workers can fail before producing a model response. The event
    /// carries enough identity and error information to fail the active turn
    /// using the same audit, transcript, prompt-display, scheduler, and
    /// lifecycle paths as the configured compatibility poller.
    pub fn apply_agent_provider_failed_event(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        kind: &str,
        message: &str,
        provider_failure_json: Option<&str>,
        provider_raw_text: Option<&str>,
    ) -> Result<bool> {
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.agent_id != agent_id.as_str() {
            return Err(MezError::invalid_args(
                "agent provider event agent id does not match turn",
            ));
        }
        let error =
            runtime_provider_event_error(kind, message, provider_failure_json, provider_raw_text);
        self.fail_configured_agent_provider_task(turn_id, &error)?;
        Ok(true)
    }

    /// Runs the pending agent provider tasks operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn pending_agent_provider_tasks(&self) -> Vec<RuntimeAgentProviderTask> {
        self.pending_agent_provider_tasks
            .iter()
            .filter_map(|turn_id| self.runtime_agent_provider_task(turn_id))
            .collect()
    }

    /// Records that an async provider worker owns a claimed task.
    ///
    /// Claimed provider tasks are no longer visible in the pending queue, so the
    /// runtime keeps this lease record to make worker loss observable and
    /// recoverable through a timer.
    pub(crate) fn record_claimed_agent_provider_task(
        &mut self,
        dispatch: &RuntimeAgentProviderDispatch,
        generation: u64,
        timeout_ms: u64,
    ) -> Result<()> {
        let turn = &dispatch.turn;
        self.claimed_agent_provider_tasks.insert(
            turn.turn_id.clone(),
            RuntimeAgentProviderClaim {
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                generation,
                claimed_at_unix_ms: current_unix_millis(),
                timeout_ms,
            },
        );
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "provider_task claim_lease started generation={generation} timeout_ms={timeout_ms}"
            ),
        )?;
        Ok(())
    }

    /// Clears the provider-worker claim lease for a settled turn.
    pub(crate) fn clear_claimed_agent_provider_task(&mut self, turn_id: &str) {
        self.claimed_agent_provider_tasks.remove(turn_id);
    }

    /// Fails a running turn when its claimed provider worker lease expires.
    ///
    /// Stale timer generations are ignored so a late timer from an older claim
    /// cannot fail a turn whose provider work has already been retried.
    pub(crate) fn fail_expired_claimed_agent_provider_task(
        &mut self,
        turn_id: &str,
        generation: u64,
    ) -> Result<bool> {
        let Some(claim) = self.claimed_agent_provider_tasks.get(turn_id).cloned() else {
            return Ok(false);
        };
        if claim.turn_id != turn_id {
            self.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        if claim.generation != generation {
            return Ok(false);
        }
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.state != AgentTurnState::Running {
            self.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        self.append_agent_status_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent: provider worker timed out after {} ms; failing turn",
                claim.timeout_ms
            ),
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!(
                "provider_task failed reason=provider_claim_timeout generation={} timeout_ms={}",
                claim.generation, claim.timeout_ms
            ),
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!(
                "provider_task claim_lease expired agent_id={} claimed_at_unix_ms={}",
                claim.agent_id, claim.claimed_at_unix_ms
            ),
        )?;
        let error = MezError::invalid_state(format!(
            "provider worker did not settle claimed task within {} ms",
            claim.timeout_ms
        ));
        self.fail_configured_agent_provider_task(turn_id, &error)?;
        Ok(true)
    }

    /// Returns whether the provider worker for a turn should continue.
    ///
    /// `/stop` can finish a turn after the async provider task has already
    /// claimed it from `pending_agent_provider_tasks`. The provider service
    /// polls this predicate while waiting so cancelled turns do not keep
    /// holding memory or network work after the user has stopped them.
    pub fn agent_turn_is_running(&self, turn_id: &str) -> bool {
        self.agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)
    }

    /// Runs the prune stale agent provider tasks operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    fn prune_stale_agent_provider_tasks(&mut self) {
        let stale_turn_ids =
            self.pending_agent_provider_tasks
                .iter()
                .filter(|turn_id| {
                    let turn_id = turn_id.as_str();
                    !self.agent_turn_ledger.turns().iter().any(|turn| {
                        turn.turn_id == turn_id && turn.state == AgentTurnState::Running
                    }) || !self.agent_turn_model_profiles.contains_key(turn_id)
                })
                .cloned()
                .collect::<Vec<_>>();
        for turn_id in stale_turn_ids {
            self.pending_agent_provider_tasks.remove(&turn_id);
        }
    }

    /// Runs the poll agent provider tasks with provider operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn poll_agent_provider_tasks_with_provider<P: ModelProvider>(
        &mut self,
        provider: &P,
        limit: usize,
    ) -> Result<Vec<AgentTurnExecution>> {
        self.require_live()?;
        if limit == 0 {
            return Err(MezError::invalid_args(
                "agent provider task poll limit must be greater than zero",
            ));
        }

        self.prune_stale_agent_provider_tasks();
        let task_ids = self
            .pending_agent_provider_tasks
            .iter()
            .filter(|turn_id| {
                self.agent_turn_model_profiles
                    .get(*turn_id)
                    .is_some_and(|profile| profile.provider == provider.provider_id())
            })
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let mut executions = Vec::with_capacity(task_ids.len());
        for turn_id in task_ids {
            if self
                .agent_turn_executions
                .get(&turn_id)
                .is_some_and(|execution| {
                    self.execution_has_pending_shell_dispatch(&turn_id, execution)
                })
            {
                self.pending_agent_provider_tasks.remove(&turn_id);
                if let Some(execution) = self.dispatch_stored_running_shell_actions(&turn_id)? {
                    executions.push(execution);
                }
                continue;
            }
            let Some(model_profile) = self.agent_turn_model_profiles.get(&turn_id).cloned() else {
                self.pending_agent_provider_tasks.remove(&turn_id);
                continue;
            };
            self.pending_agent_provider_tasks.remove(&turn_id);
            if let Some(turn) = self
                .agent_turn_ledger
                .turns()
                .iter()
                .find(|turn| turn.turn_id == turn_id)
                .cloned()
            {
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn_id,
                    &format!(
                        "provider_task claimed reason=test_provider_poll provider={}",
                        provider.provider_id()
                    ),
                )?;
            }
            executions.push(self.execute_agent_turn_with_provider(
                &turn_id,
                provider,
                model_profile,
            )?);
        }
        Ok(executions)
    }

    /// Runs the runtime agent provider task operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_agent_provider_task(
        &self,
        turn_id: &str,
    ) -> Option<RuntimeAgentProviderTask> {
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)?;
        let model_profile = self.agent_turn_model_profiles.get(turn_id)?.clone();
        Some(RuntimeAgentProviderTask {
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            model_profile,
        })
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
                    continue;
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
    pub(super) fn dispatch_apply_patch_followup_if_needed(
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
        let decoded_output = decode_shell_output_transport(&transaction.observed_output_preview);
        let write_plan = apply_patch_write_plan_from_read_output(patch, &decoded_output)
            .unwrap_or_else(|error| apply_patch_error_plan(error.message()));

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

    /// Runs the execute running mcp actions for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub(super) fn execute_running_mcp_actions_for_turn(
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
        let mut executed = 0usize;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Running
                || execution.action_results[index].action_type != "mcp_call"
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("running MCP result does not match an action")
                })?;
            let permission_policy = self.permission_policy_for_turn(turn);
            let auto_allowed = permission_policy.approval_policy
                == crate::permissions::ApprovalPolicy::AutoAllow
                && runtime_action_supports_auto_allow(&action);
            let policy_allowed =
                permission_policy.approval_policy == crate::permissions::ApprovalPolicy::FullAccess;
            execution.action_results[index] =
                self.execute_mcp_action_for_turn(turn, &action, auto_allowed || policy_allowed)?;
            executed = executed.saturating_add(1);
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        if execution.terminal_state == AgentTurnState::Running
            && runtime_execution_ready_for_provider_continuation(execution)
        {
            for result in execution
                .action_results
                .iter()
                .filter(|result| result.action_type == "mcp_call")
            {
                self.agent_turn_contexts
                    .get_mut(&turn.turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("running agent turn context is unavailable")
                    })?
                    .blocks
                    .push(ContextBlock {
                        source: ContextSourceKind::ActionResult,
                        label: format!("action result {}", result.action_id),
                        content: action_result_context_content(result),
                    });
            }
        }
        Ok(executed)
    }

    /// Runs the execute running mcp actions for turn async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn execute_running_mcp_actions_for_turn_async(
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
        let mut executed = 0usize;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Running
                || execution.action_results[index].action_type != "mcp_call"
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("running MCP result does not match an action")
                })?;
            let permission_policy = self.permission_policy_for_turn(turn);
            let auto_allowed = permission_policy.approval_policy
                == crate::permissions::ApprovalPolicy::AutoAllow
                && runtime_action_supports_auto_allow(&action);
            let policy_allowed =
                permission_policy.approval_policy == crate::permissions::ApprovalPolicy::FullAccess;
            execution.action_results[index] = self
                .execute_mcp_action_for_turn_async(turn, &action, auto_allowed || policy_allowed)
                .await?;
            executed = executed.saturating_add(1);
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        if execution.terminal_state == AgentTurnState::Running
            && runtime_execution_ready_for_provider_continuation(execution)
        {
            for result in execution
                .action_results
                .iter()
                .filter(|result| result.action_type == "mcp_call")
            {
                self.agent_turn_contexts
                    .get_mut(&turn.turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("running agent turn context is unavailable")
                    })?
                    .blocks
                    .push(ContextBlock {
                        source: ContextSourceKind::ActionResult,
                        label: format!("action result {}", result.action_id),
                        content: action_result_context_content(result),
                    });
            }
        }
        Ok(executed)
    }

    pub(super) async fn execute_running_network_actions_for_turn_async(
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
        let mut executed = 0usize;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Running
                || !matches!(
                    execution.action_results[index].action_type,
                    "web_search" | "fetch_url"
                )
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("running network result does not match an action")
                })?;
            let Some(plan) = network_action_plan(&action)? else {
                continue;
            };
            let request_key = plan.policy_command.clone();
            if let Some(result) =
                self.network_action_loop_guard_failure(turn, &action, &request_key)?
            {
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    &format!(
                        "action {} {} reason=network_action_loop_guard",
                        action.id,
                        runtime_action_status_name(result.status)
                    ),
                )?;
                execution.action_results[index] = result;
                continue;
            }
            if !self
                .append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, &action)?
            {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: {}",
                        runtime_agent_action_summary(&action)
                            .unwrap_or_else(|| "network action".to_string())
                    ),
                )?;
            }
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "action {} type={} network_executor=started",
                    action.id,
                    action.action_type()
                ),
            )?;
            let transport = ReqwestProviderHttpTransport;
            self.record_network_action_history(&turn.turn_id, &request_key);
            let result =
                execute_network_action_with_transport_async(turn, &action, &transport).await?;
            if !result.is_error && self.agent_verbose_enabled(&turn.pane_id) {
                self.append_agent_action_result_text_to_terminal_buffer(
                    &turn.pane_id,
                    &action,
                    &result,
                    &result.content_text(),
                )?;
            }
            let outcome = if result.is_error {
                "failed"
            } else {
                "succeeded"
            };
            self.append_agent_network_action_audit(turn, &action, outcome)?;
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "action {} {} reason=runtime_network_action",
                    action.id,
                    runtime_action_status_name(result.status)
                ),
            )?;
            execution.action_results[index] = result;
            executed = executed.saturating_add(1);
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        if execution.terminal_state == AgentTurnState::Running
            && runtime_execution_ready_for_provider_continuation(execution)
        {
            for result in execution
                .action_results
                .iter()
                .filter(|result| matches!(result.action_type, "web_search" | "fetch_url"))
            {
                self.agent_turn_contexts
                    .get_mut(&turn.turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("running agent turn context is unavailable")
                    })?
                    .blocks
                    .push(ContextBlock {
                        source: ContextSourceKind::ActionResult,
                        label: format!("action result {}", result.action_id),
                        content: action_result_context_content(result),
                    });
            }
            self.pending_agent_provider_tasks
                .insert(turn.turn_id.clone());
        }
        Ok(executed)
    }

    /// Executes one approved runtime-owned network action from a legacy
    /// synchronous approval path.
    ///
    /// The control dispatcher is still synchronous, so the network future runs
    /// on a short-lived helper thread with its own Tokio runtime instead of
    /// trying to nest a runtime inside the actor thread.
    pub(super) fn execute_network_action_for_turn_blocking(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
    ) -> Result<ActionResult> {
        let Some(plan) = network_action_plan(action)? else {
            return Err(MezError::invalid_args(
                "network action execution requires a network-backed action",
            ));
        };
        let request_key = plan.policy_command;
        if let Some(result) = self.network_action_loop_guard_failure(turn, action, &request_key)? {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "action {} {} reason=network_action_loop_guard",
                    action.id,
                    runtime_action_status_name(result.status)
                ),
            )?;
            return Ok(result);
        }
        if !self.append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, action)? {
            self.append_agent_status_text_to_terminal_buffer(
                &turn.pane_id,
                &format!(
                    "agent: {}",
                    runtime_agent_action_summary(action)
                        .unwrap_or_else(|| "network action".to_string())
                ),
            )?;
        }
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "action {} type={} network_executor=started",
                action.id,
                action.action_type()
            ),
        )?;
        let turn_for_thread = turn.clone();
        let action_for_thread = action.clone();
        self.record_network_action_history(&turn.turn_id, &request_key);
        let result = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| {
                    MezError::invalid_state(format!("network action runtime setup failed: {error}"))
                })?;
            let transport = ReqwestProviderHttpTransport;
            runtime.block_on(execute_network_action_with_transport_async(
                &turn_for_thread,
                &action_for_thread,
                &transport,
            ))
        })
        .join()
        .map_err(|_| MezError::invalid_state("network action worker panicked"))??;
        if !result.is_error && self.agent_verbose_enabled(&turn.pane_id) {
            self.append_agent_action_result_text_to_terminal_buffer(
                &turn.pane_id,
                action,
                &result,
                &result.content_text(),
            )?;
        }
        let outcome = if result.is_error {
            "failed"
        } else {
            "succeeded"
        };
        self.append_agent_network_action_audit(turn, action, outcome)?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "action {} {} reason=runtime_network_action",
                action.id,
                runtime_action_status_name(result.status)
            ),
        )?;
        Ok(result)
    }

    /// Builds the effective skill catalog for one pane.
    ///
    /// User skills are always read from the configured user root. Project
    /// skills are included only when the pane is inside a trusted project root.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose current working directory scopes project skills.
    pub(super) fn effective_skill_catalog_for_pane(&self, pane_id: &str) -> SkillCatalog {
        let project_root = self.trusted_skill_project_root_for_pane(pane_id);
        discover_skill_catalog(self.config_root.as_deref(), project_root.as_deref())
    }

    /// Returns the trusted project root whose skills may apply to one pane.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose working directory determines project scope.
    fn trusted_skill_project_root_for_pane(&self, pane_id: &str) -> Option<PathBuf> {
        let working_directory = self.pane_current_working_directory(pane_id)?;
        let store = self.project_trust_store.as_ref()?;
        store
            .records()
            .filter(|record| record.state == TrustDecision::Trusted)
            .find(|record| {
                runtime_path_under_project_root(&working_directory, &record.project_root)
            })
            .map(|record| record.project_root.clone())
    }

    /// Builds the currently loaded skill context state for one active turn.
    ///
    /// Explicit `$skill` prompt expansion and successful `call_skill` results
    /// both place full skill text in the model context. The runtime uses that
    /// context as the source of truth for suppressing redundant non-effecting
    /// skill actions before they become unbounded provider continuations.
    fn runtime_skill_action_context_for_turn(
        &self,
        turn_id: &str,
    ) -> Result<RuntimeSkillActionContext> {
        let context = self
            .agent_turn_contexts
            .get(turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        Ok(runtime_skill_action_context_from_blocks(&context.blocks))
    }

    /// Returns a model-correctable failure for redundant skill actions.
    ///
    /// Skill lookup and load actions are non-effecting and normally trigger a
    /// provider continuation. When the requested skill context is already
    /// present, treating the duplicate as another success can produce a
    /// discovery/load loop. This result keeps the attempted action visible while
    /// steering the next provider turn toward real work.
    fn redundant_skill_action_failure(
        turn: &AgentTurnRecord,
        action: &AgentAction,
        code: &'static str,
        message: impl Into<String>,
    ) -> Result<ActionResult> {
        ActionResult::failed(turn, action, ActionStatus::Failed, code, message.into())
    }

    /// Executes a runtime-owned skill lookup or skill-load action.
    ///
    /// # Parameters
    /// - `turn`: Active turn receiving the action result.
    /// - `action`: `request_skills` or `call_skill` action to execute.
    fn execute_skill_action_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        skill_context: &mut RuntimeSkillActionContext,
    ) -> Result<ActionResult> {
        let catalog = self.effective_skill_catalog_for_pane(&turn.pane_id);
        match &action.payload {
            AgentActionPayload::RequestSkills => {
                if !skill_context.loaded_skills.is_empty() {
                    return Self::redundant_skill_action_failure(
                        turn,
                        action,
                        "skill_context_already_loaded",
                        format!(
                            "skill context is already loaded for this turn: {}; use the loaded skill guidance or request the missing action capability instead of discovering skills again",
                            skill_context
                                .loaded_skills
                                .iter()
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(",")
                        ),
                    );
                }
                if skill_context.catalog_requested {
                    return Self::redundant_skill_action_failure(
                        turn,
                        action,
                        "skill_catalog_already_requested",
                        "the effective skill catalog has already been returned for this turn; use an available skill or request the missing action capability instead of requesting the catalog again",
                    );
                }
                skill_context.catalog_requested = true;
                Ok(ActionResult::succeeded(
                    turn,
                    action,
                    vec![catalog.model_catalog_text()],
                    Some(catalog.structured_json()),
                ))
            }
            AgentActionPayload::CallSkill {
                name,
                additional_context,
            } => {
                if !is_valid_skill_name(name) {
                    return ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        "invalid_skill_name",
                        "skill name must contain only lowercase letters, digits, and hyphens",
                    );
                }
                let Some(summary) = catalog.get(name) else {
                    let available = if catalog.skills.is_empty() {
                        "none".to_string()
                    } else {
                        catalog.names().join(",")
                    };
                    return ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        "skill_not_found",
                        format!("skill {name:?} is not available; available skills: {available}"),
                    );
                };
                if skill_context.loaded_skills.contains(name) {
                    return Self::redundant_skill_action_failure(
                        turn,
                        action,
                        "skill_context_already_loaded",
                        format!(
                            "skill {name:?} is already loaded for this turn; use the loaded skill guidance or request the missing action capability instead of loading it again"
                        ),
                    );
                }
                let document = match load_skill_document(summary) {
                    Ok(document) => document,
                    Err(error) => {
                        return ActionResult::failed(
                            turn,
                            action,
                            ActionStatus::Failed,
                            runtime_mezzanine_error_code(error.kind()),
                            error.message().to_string(),
                        );
                    }
                };
                let content = self
                    .runtime_skill_context_text(document.clone(), additional_context.as_deref())?;
                let result = ActionResult::succeeded(
                    turn,
                    action,
                    vec![content],
                    Some(format!(
                        r#"{{"name":"{}","source":"{}","path":"{}","skill_bytes":{},"additional_context_bytes":{}}}"#,
                        json_escape(&document.summary.name),
                        document.summary.source.as_str(),
                        json_escape(&document.summary.path.to_string_lossy()),
                        document.text.len(),
                        additional_context.as_deref().map(str::len).unwrap_or(0)
                    )),
                );
                skill_context.loaded_skills.insert(name.clone());
                Ok(result)
            }
            _ => Err(MezError::invalid_args(
                "skill execution requires request_skills or call_skill action",
            )),
        }
    }

    /// Executes any provider-produced non-effecting skill actions and appends
    /// their results to running turn context for provider continuation.
    ///
    /// # Parameters
    /// - `turn`: Active turn containing the running action results.
    /// - `execution`: Provider execution whose pending skill results are updated.
    pub(super) fn execute_running_skill_actions_for_turn(
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
        let mut executed = 0usize;
        let mut skill_context = self.runtime_skill_action_context_for_turn(&turn.turn_id)?;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Running
                || !matches!(
                    execution.action_results[index].action_type,
                    "request_skills" | "call_skill"
                )
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("running skill result does not match an action")
                })?;
            execution.action_results[index] =
                self.execute_skill_action_for_turn(turn, &action, &mut skill_context)?;
            executed = executed.saturating_add(1);
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        if execution.terminal_state == AgentTurnState::Running
            && runtime_execution_ready_for_provider_continuation(execution)
        {
            for result in execution
                .action_results
                .iter()
                .filter(|result| matches!(result.action_type, "request_skills" | "call_skill"))
            {
                self.agent_turn_contexts
                    .get_mut(&turn.turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("running agent turn context is unavailable")
                    })?
                    .blocks
                    .push(ContextBlock {
                        source: ContextSourceKind::ActionResult,
                        label: format!("action result {}", result.action_id),
                        content: action_result_context_content(result),
                    });
            }
            self.pending_agent_provider_tasks
                .insert(turn.turn_id.clone());
        }
        Ok(executed)
    }

    /// Runs the present agent response actions to terminal buffer operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn present_agent_response_actions_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let Some(batch) = execution.response.action_batch.as_ref() else {
            if execution.terminal_state == AgentTurnState::Completed
                && !execution.response.raw_text.trim().is_empty()
            {
                self.append_agent_assistant_text_to_terminal_buffer(
                    pane_id,
                    &execution.response.raw_text,
                )?;
            }
            return Ok(());
        };

        let visible_action_texts = runtime_agent_batch_visible_action_texts(batch);
        if !batch.rationale.trim().is_empty()
            && !runtime_agent_batch_rationale_repeats_visible_batch_text(
                batch,
                &visible_action_texts,
            )
        {
            self.append_agent_thinking_text_to_terminal_buffer(pane_id, batch.rationale.trim())?;
        }
        if self.agent_verbose_enabled(pane_id)
            && let Some(thought) = batch.thought.as_deref()
            && !thought.trim().is_empty()
        {
            self.append_agent_thinking_text_to_terminal_buffer(pane_id, thought.trim())?;
        }
        let mut emitted_user_visible_action = false;
        let mut pending_runtime_visible_action = false;
        let mut emitted_action_rationale_keys = BTreeSet::new();
        let has_runtime_visible_action = batch
            .actions
            .iter()
            .any(runtime_agent_action_has_runtime_visible_effect);
        for action in &batch.actions {
            let rationale_key = normalize_agent_user_visible_text(&action.rationale);
            if !action.rationale.trim().is_empty()
                && !runtime_agent_action_rationale_repeats_visible_summary(action)
                && !runtime_agent_action_rationale_repeats_visible_batch_text(
                    action,
                    &visible_action_texts,
                )
                && emitted_action_rationale_keys.insert(rationale_key)
            {
                self.append_agent_thinking_text_to_terminal_buffer(
                    pane_id,
                    action.rationale.trim(),
                )?;
            }
            match &action.payload {
                AgentActionPayload::Say {
                    status,
                    text,
                    content_type,
                } => {
                    if text.trim().is_empty() {
                        continue;
                    }
                    if has_runtime_visible_action && *status != SayStatus::Progress {
                        pending_runtime_visible_action = true;
                    } else {
                        emitted_user_visible_action = true;
                        self.append_agent_assistant_content_to_terminal_buffer(
                            pane_id,
                            text,
                            content_type,
                        )?;
                    }
                }
                AgentActionPayload::RequestCapability { .. }
                | AgentActionPayload::RequestSkills
                | AgentActionPayload::CallSkill { .. } => {}
                AgentActionPayload::Abort { reason } => {
                    emitted_user_visible_action = true;
                    self.append_agent_error_text_to_terminal_buffer(
                        pane_id,
                        &format!("agent: aborted: {reason}"),
                    )?;
                }
                AgentActionPayload::ShellCommand { .. }
                | AgentActionPayload::ApplyPatch { .. }
                | AgentActionPayload::WebSearch { .. }
                | AgentActionPayload::FetchUrl { .. } => {
                    pending_runtime_visible_action = true;
                }
                AgentActionPayload::McpCall { .. }
                | AgentActionPayload::SendMessage { .. }
                | AgentActionPayload::SpawnAgent { .. }
                | AgentActionPayload::ConfigChange { .. } => {
                    pending_runtime_visible_action = true;
                }
                AgentActionPayload::Complete => {}
            }
        }
        if execution.terminal_state == AgentTurnState::Completed
            && !emitted_user_visible_action
            && !pending_runtime_visible_action
        {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                "agent: completed without a user-facing response",
            )?;
        }
        Ok(())
    }

    /// Presents deferred `say` actions once a mixed response's runtime-visible
    /// actions have finished and emitted their own logs or diffs.
    pub(super) fn present_deferred_agent_say_actions_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
    ) -> Result<usize> {
        if execution.terminal_state != AgentTurnState::Completed {
            return Ok(0);
        }
        let Some(batch) = execution.response.action_batch.as_ref() else {
            return Ok(0);
        };
        if !batch
            .actions
            .iter()
            .any(runtime_agent_action_has_runtime_visible_effect)
        {
            return Ok(0);
        }

        let mut emitted = 0usize;
        for action in &batch.actions {
            if let AgentActionPayload::Say {
                status,
                text,
                content_type,
            } = &action.payload
            {
                if *status == SayStatus::Progress || text.trim().is_empty() {
                    continue;
                }
                self.append_agent_assistant_content_to_terminal_buffer(
                    pane_id,
                    text,
                    content_type,
                )?;
                emitted = emitted.saturating_add(1);
            }
        }
        Ok(emitted)
    }

    /// Presents runtime-gated action outcomes that otherwise would not have a
    /// natural command, tool, or assistant-output line in the pane buffer.
    pub(super) fn present_agent_action_outcomes_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let Some(batch) = execution.response.action_batch.as_ref() else {
            return Ok(());
        };
        let mut aggregated_result_ids = BTreeSet::new();
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
            let matching_results = execution
                .action_results
                .iter()
                .filter(|result| {
                    result.is_error && runtime_action_result_has_error_code(result, code)
                })
                .collect::<Vec<_>>();
            if matching_results.is_empty() {
                continue;
            }
            let message = matching_results
                .iter()
                .find_map(|result| result.error.as_ref().map(|error| error.message.as_str()))
                .unwrap_or("runtime loop guard suppressed this action batch");
            self.append_agent_error_text_to_terminal_buffer(
                pane_id,
                &runtime_loop_guard_failure_summary_line(label, matching_results.len(), message),
            )?;
            aggregated_result_ids.extend(
                matching_results
                    .iter()
                    .map(|result| result.action_id.clone()),
            );
        }
        for result in &execution.action_results {
            if aggregated_result_ids.contains(&result.action_id) {
                continue;
            }
            let Some(action) = batch
                .actions
                .iter()
                .find(|action| action.id == result.action_id)
            else {
                continue;
            };
            let Some((is_error, line)) = runtime_agent_action_outcome_line(
                action,
                result,
                self.agent_verbose_enabled(pane_id) || self.agent_trace_enabled(pane_id),
            ) else {
                continue;
            };
            if is_error {
                self.append_agent_error_text_to_terminal_buffer(pane_id, &line)?;
            } else {
                self.append_agent_status_text_to_terminal_buffer(pane_id, &line)?;
            }
        }
        Ok(())
    }

    /// Presents bounded failure details when the runtime is ending a failed
    /// turn instead of giving the model another recovery attempt.
    fn present_unrecovered_agent_failure_diagnostics_to_terminal_buffer(
        &mut self,
        pane_id: &str,
        execution: &AgentTurnExecution,
        reason: &str,
    ) -> Result<()> {
        let Some(batch) = execution.response.action_batch.as_ref() else {
            return Ok(());
        };
        let mut aggregated_result_ids = BTreeSet::new();
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
            let matching_results = execution
                .action_results
                .iter()
                .filter(|result| {
                    runtime_action_result_is_terminal_failure(result)
                        && runtime_action_result_has_error_code(result, code)
                })
                .collect::<Vec<_>>();
            if matching_results.is_empty() {
                continue;
            }
            let message = matching_results
                .iter()
                .find_map(|result| result.error.as_ref().map(|error| error.message.as_str()))
                .unwrap_or("runtime loop guard suppressed this action batch");
            self.append_agent_error_text_to_terminal_buffer(
                pane_id,
                &format!(
                    "{}; {reason}",
                    runtime_loop_guard_failure_summary_line(label, matching_results.len(), message)
                ),
            )?;
            aggregated_result_ids.extend(
                matching_results
                    .iter()
                    .map(|result| result.action_id.clone()),
            );
        }
        for result in execution
            .action_results
            .iter()
            .filter(|result| runtime_action_result_is_terminal_failure(result))
        {
            if aggregated_result_ids.contains(&result.action_id) {
                continue;
            }
            let Some(action) = batch
                .actions
                .iter()
                .find(|action| action.id == result.action_id)
            else {
                continue;
            };
            let label = runtime_agent_action_summary(action)
                .unwrap_or_else(|| format!("{} action {}", result.action_type, result.action_id));
            let detail = runtime_agent_action_error_suffix(result);
            let mut message = format!("agent: {label} failed; {reason}{detail}");
            if let Some(output) = runtime_unrecovered_action_failure_output(result) {
                let lines = runtime_unrecovered_failure_output_lines(action, &output);
                if !lines.is_empty() {
                    message.push('\n');
                    message.push_str(&lines.join("\n"));
                }
            }
            self.append_agent_error_text_to_terminal_buffer(pane_id, &message)?;
        }
        Ok(())
    }

    /// Runs the execute running message actions for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn execute_running_message_actions_for_turn(
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
        let mut executed = 0usize;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Running
                || execution.action_results[index].action_type != "send_message"
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("running message result does not match an action")
                })?;
            execution.action_results[index] =
                self.execute_message_action_for_turn(turn, &action)?;
            executed = executed.saturating_add(1);
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        if execution.terminal_state == AgentTurnState::Running
            && runtime_execution_ready_for_provider_continuation(execution)
        {
            for result in execution
                .action_results
                .iter()
                .filter(|result| result.action_type == "send_message")
            {
                self.agent_turn_contexts
                    .get_mut(&turn.turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("running agent turn context is unavailable")
                    })?
                    .blocks
                    .push(ContextBlock {
                        source: ContextSourceKind::ActionResult,
                        label: format!("action result {}", result.action_id),
                        content: action_result_context_content(result),
                    });
            }
            self.pending_agent_provider_tasks
                .insert(turn.turn_id.clone());
        }
        Ok(executed)
    }

    /// Executes any provider-produced MAAP `spawn_agent` actions in a running
    /// turn and rewrites their planned running results to terminal action
    /// results. Successful spawns create the child pane, child turn, MMP task
    /// status, and audit record through the shared runtime spawn helper;
    /// failures are returned as action-level errors so the parent turn can be
    /// transcripted normally.
    pub(super) fn execute_running_spawn_actions_for_turn(
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
        let mut executed = 0usize;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Running
                || execution.action_results[index].action_type != "spawn_agent"
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("running spawn result does not match an action")
                })?;
            if !self
                .append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, &action)?
            {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    "agent: spawn agent",
                )?;
            }
            execution.action_results[index] = match self
                .execute_spawn_action_for_turn(turn, &action)
            {
                Ok(result) => result,
                Err(error) => {
                    let status = if error.kind() == crate::error::MezErrorKind::Forbidden {
                        ActionStatus::Denied
                    } else {
                        ActionStatus::Failed
                    };
                    let mut result = ActionResult::failed(
                        turn,
                        &action,
                        status,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message().to_string(),
                    )?;
                    result.structured_content_json = Some(format!(
                        r#"{{"spawn":null,"delivery_status":"failed","error":{{"code":"{}","message":"{}"}}}}"#,
                        runtime_mezzanine_error_code(error.kind()),
                        json_escape(error.message())
                    ));
                    result
                }
            };
            executed = executed.saturating_add(1);
        }
        if execution.action_results.iter().any(|result| {
            result.action_type == "spawn_agent" && result.status == ActionStatus::Running
        }) {
            execution.final_turn = false;
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        if execution.terminal_state == AgentTurnState::Running
            && runtime_execution_ready_for_provider_continuation(execution)
        {
            for result in execution
                .action_results
                .iter()
                .filter(|result| result.action_type == "spawn_agent")
            {
                self.agent_turn_contexts
                    .get_mut(&turn.turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("running agent turn context is unavailable")
                    })?
                    .blocks
                    .push(ContextBlock {
                        source: ContextSourceKind::ActionResult,
                        label: format!("action result {}", result.action_id),
                        content: action_result_context_content(result),
                    });
            }
            self.pending_agent_provider_tasks
                .insert(turn.turn_id.clone());
        }
        Ok(executed)
    }

    /// Applies one MAAP `config_change` action through the live configuration
    /// control path and maps the control response back into an action result.
    ///
    /// # Parameters
    /// - `turn`: The agent turn that proposed the configuration change.
    /// - `action`: The `config_change` action to apply.
    /// - `caller_client_id`: The primary client identity used for control
    ///   authorization.
    /// - `approval_state`: The structured approval state to report, such as
    ///   `approved` for a routed approval or `full_access` for policy-accepted
    ///   execution.
    pub(super) fn execute_config_change_action_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        caller_client_id: &crate::ids::ClientId,
        approval_state: &str,
    ) -> Result<ActionResult> {
        let AgentActionPayload::ConfigChange {
            setting_path,
            operation,
            value,
        } = &action.payload
        else {
            return Err(MezError::invalid_args(
                "config_change execution requires a config_change action",
            ));
        };
        if setting_path == "theme.active" && runtime_config_change_operation_sets_value(operation) {
            return self.execute_theme_config_change_action_for_turn(
                turn,
                action,
                setting_path,
                operation,
                value.as_deref(),
                approval_state,
            );
        }
        let persist_path = self.ensure_agent_config_change_persist_path()?;
        let persistent_target_json = format!(
            r#"{{"scope":"user","path":"{}"}}"#,
            json_escape(&persist_path.to_string_lossy())
        );
        match runtime_config_change_control_request(
            turn,
            action,
            setting_path,
            operation,
            value.as_deref(),
            &persistent_target_json,
            "persist",
        ) {
            Ok(persistent_request) => {
                let persistent_response =
                    self.dispatch_runtime_control_body(&persistent_request, caller_client_id);
                if persistent_response.contains(r#""error""#) {
                    let mut result = ActionResult::failed(
                        turn,
                        action,
                        ActionStatus::Failed,
                        "config_change_failed",
                        persistent_response.clone(),
                    )?;
                    result.structured_content_json = Some(format!(
                        r#"{{"approval":{{"state":"{}","kind":"config_change","action_id":"{}"}},"persistent_control_response":{}}}"#,
                        json_escape(approval_state),
                        json_escape(&action.id),
                        persistent_response
                    ));
                    Ok(result)
                } else {
                    Ok(ActionResult::succeeded(
                        turn,
                        action,
                        vec![format!(
                            "configuration change persisted and applied: {} {}",
                            operation, setting_path
                        )],
                        Some(format!(
                            r#"{{"approval":{{"state":"{}","kind":"config_change","action_id":"{}"}},"persistent_control_response":{}}}"#,
                            json_escape(approval_state),
                            json_escape(&action.id),
                            persistent_response
                        )),
                    ))
                }
            }
            Err(error) => ActionResult::failed(
                turn,
                action,
                ActionStatus::Failed,
                runtime_mezzanine_error_code(error.kind()),
                error.message().to_string(),
            ),
        }
    }

    /// Applies a `theme.active` config change through the same runtime command
    /// path as `:set-theme`.
    ///
    /// The dedicated command materializes the selected theme aliases and color
    /// slots before persistence. A generic `config/set theme.active` request
    /// would change only the selector and could leave stale materialized colors
    /// in place.
    fn execute_theme_config_change_action_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        setting_path: &str,
        operation: &str,
        value: Option<&str>,
        approval_state: &str,
    ) -> Result<ActionResult> {
        let theme = match runtime_config_change_string_value(setting_path, value) {
            Ok(theme) => theme,
            Err(error) => {
                return ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    runtime_mezzanine_error_code(error.kind()),
                    error.message().to_string(),
                );
            }
        };
        let invocation = CommandInvocation {
            name: "set-theme".to_string(),
            args: vec![theme.clone()],
        };
        match runtime_set_theme_command(self, &invocation) {
            Ok(command_response) => Ok(ActionResult::succeeded(
                turn,
                action,
                vec![format!(
                    "configuration change persisted and applied: {} {}",
                    operation, setting_path
                )],
                Some(format!(
                    r#"{{"approval":{{"state":"{}","kind":"config_change","action_id":"{}"}},"runtime_command":"set-theme","theme":"{}","command_response":"{}"}}"#,
                    json_escape(approval_state),
                    json_escape(&action.id),
                    json_escape(&theme),
                    json_escape(&command_response)
                )),
            )),
            Err(error) => {
                let mut result = ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    runtime_mezzanine_error_code(error.kind()),
                    error.message().to_string(),
                )?;
                result.structured_content_json = Some(format!(
                    r#"{{"approval":{{"state":"{}","kind":"config_change","action_id":"{}"}},"runtime_command":"set-theme","theme":"{}","error":"{}"}}"#,
                    json_escape(approval_state),
                    json_escape(&action.id),
                    json_escape(&theme),
                    json_escape(error.message())
                ));
                Ok(result)
            }
        }
    }

    /// Applies a batch of theme scalar `config_change` actions with one reload.
    ///
    /// # Parameters
    /// - `turn`: The agent turn that proposed the configuration changes.
    /// - `execution`: The running execution whose action results should be
    ///   replaced with terminal results.
    /// - `actions`: Running theme scalar config-change actions and their result
    ///   indexes.
    /// - `approval_state`: The policy approval state to include in each
    ///   action-result payload.
    fn execute_batched_theme_config_change_actions_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
        actions: &[(usize, AgentAction)],
        approval_state: &str,
    ) -> Result<usize> {
        for (_, action) in actions {
            if !self.append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, action)? {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: {}",
                        runtime_agent_action_summary(action)
                            .unwrap_or_else(|| "config change".to_string())
                    ),
                )?;
            }
        }
        let batch_result = (|| {
            let mutations = actions
                .iter()
                .map(|(_, action)| runtime_config_change_mutation_from_action(action))
                .collect::<Result<Vec<_>>>()?;
            let persist_path = self.ensure_agent_config_change_persist_path()?;
            runtime_apply_persisted_config_mutation_batch(
                self,
                persist_path,
                &mutations,
                "agent/config_change:theme-batch",
            )
        })();
        for (index, action) in actions {
            execution.action_results[*index] = match &batch_result {
                Ok(report) => ActionResult::succeeded(
                    turn,
                    action,
                    vec![format!(
                        "configuration change persisted and applied in batch: {} {}",
                        runtime_config_change_operation_name(action),
                        runtime_config_change_setting_path(action).unwrap_or("unknown")
                    )],
                    Some(format!(
                        r#"{{"approval":{{"state":"{}","kind":"config_change","action_id":"{}"}},"persistent_batch":{{"path":"{}","changed":{},"reload_required":{},"mutation_count":{},"deferred":{}}}}}"#,
                        json_escape(approval_state),
                        json_escape(&action.id),
                        json_escape(&report.path.to_string_lossy()),
                        report.changed,
                        report.reload_required,
                        report.mutation_count,
                        report.deferred
                    )),
                ),
                Err(error) => ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    runtime_mezzanine_error_code(error.kind()),
                    error.message().to_string(),
                )?,
            };
        }
        let executed = actions.len();
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        if execution.terminal_state == AgentTurnState::Running
            && runtime_execution_ready_for_provider_continuation(execution)
        {
            for result in execution
                .action_results
                .iter()
                .filter(|result| result.action_type == "config_change")
            {
                self.agent_turn_contexts
                    .get_mut(&turn.turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("running agent turn context is unavailable")
                    })?
                    .blocks
                    .push(ContextBlock {
                        source: ContextSourceKind::ActionResult,
                        label: format!("action result {}", result.action_id),
                        content: action_result_context_content(result),
                    });
            }
            self.pending_agent_provider_tasks
                .insert(turn.turn_id.clone());
        }
        Ok(executed)
    }

    /// Ensures model-authored configuration changes have a persistent user
    /// config file target before they are applied live.
    ///
    /// The model-facing `config_change` action intentionally does not let the
    /// model choose arbitrary files. The runtime selects the active primary
    /// config layer when one exists, or creates the default private config file
    /// under the configured Mezzanine config root.
    fn ensure_agent_config_change_persist_path(&mut self) -> Result<std::path::PathBuf> {
        if let Some(path) = self
            .config_layers
            .iter()
            .find(|layer| layer.scope == ConfigScope::Primary && layer.path.is_some())
            .and_then(|layer| layer.path.clone())
        {
            return Ok(path);
        }
        let root = self.config_root.clone().ok_or_else(|| {
            MezError::config(
                "config_change persistence requires a configured config root or primary config file",
            )
        })?;
        let path = ConfigPaths::from_root(root).ensure_default_config()?;
        let format = ConfigFormat::from_path(&path)?;
        let text = super::fs::read_to_string(&path)?;
        self.config_layers.push(ConfigLayer {
            name: "primary".to_string(),
            path: Some(path.clone()),
            format,
            scope: ConfigScope::Primary,
            trusted: true,
            text,
        });
        let _ = self.apply_runtime_config_layers()?;
        Ok(path)
    }

    /// Executes provider-produced `config_change` actions that were accepted by
    /// the active approval policy instead of entering blocked approval routing.
    ///
    /// Full-access mode resolves the approval prompt at action-planning time,
    /// but live configuration mutation still has to pass through the normal
    /// runtime control path so validation, events, and idempotency remain
    /// identical to approved blocked config changes.
    pub(super) fn execute_running_config_change_actions_for_turn(
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
        let controller =
            self.session.primary_client_id().cloned().ok_or_else(|| {
                MezError::invalid_state("config_change requires an attached primary")
            })?;
        let pending_config_actions = execution
            .action_results
            .iter()
            .enumerate()
            .filter(|(_, result)| {
                result.status == ActionStatus::Running && result.action_type == "config_change"
            })
            .map(|(index, result)| {
                batch
                    .actions
                    .iter()
                    .find(|action| action.id == result.action_id)
                    .cloned()
                    .map(|action| (index, action))
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "running config_change result does not match an action",
                        )
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        if pending_config_actions.len() > 1
            && pending_config_actions
                .iter()
                .all(|(_, action)| runtime_config_change_action_is_theme_scalar_batchable(action))
        {
            let permission_policy = self.permission_policy_for_turn(turn);
            let approval_state = pending_config_actions
                .first()
                .map(|(_, action)| runtime_config_change_approval_state(&permission_policy, action))
                .unwrap_or("full_access");
            return self.execute_batched_theme_config_change_actions_for_turn(
                turn,
                execution,
                &pending_config_actions,
                approval_state,
            );
        }
        let mut executed = 0usize;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Running
                || execution.action_results[index].action_type != "config_change"
            {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("running config_change result does not match an action")
                })?;
            if !self
                .append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, &action)?
            {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: {}",
                        runtime_agent_action_summary(&action)
                            .unwrap_or_else(|| "config change".to_string())
                    ),
                )?;
            }
            let permission_policy = self.permission_policy_for_turn(turn);
            let approval_state = runtime_config_change_approval_state(&permission_policy, &action);
            execution.action_results[index] = self.execute_config_change_action_for_turn(
                turn,
                &action,
                &controller,
                approval_state,
            )?;
            executed = executed.saturating_add(1);
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        if execution.terminal_state == AgentTurnState::Running
            && runtime_execution_ready_for_provider_continuation(execution)
        {
            for result in execution
                .action_results
                .iter()
                .filter(|result| result.action_type == "config_change")
            {
                self.agent_turn_contexts
                    .get_mut(&turn.turn_id)
                    .ok_or_else(|| {
                        MezError::invalid_state("running agent turn context is unavailable")
                    })?
                    .blocks
                    .push(ContextBlock {
                        source: ContextSourceKind::ActionResult,
                        label: format!("action result {}", result.action_id),
                        content: action_result_context_content(result),
                    });
            }
            self.pending_agent_provider_tasks
                .insert(turn.turn_id.clone());
        }
        Ok(executed)
    }

    /// Executes one MAAP `spawn_agent` action through the runtime subagent
    /// creation path.
    ///
    /// The action's simple MAAP placement string is parsed through the same
    /// control schema helper used by `agent/spawn`. Unsupported placements,
    /// invalid cooperation modes, scope inheritance errors, or audit failures are
    /// returned to the caller before child state can leak.
    pub(super) fn execute_spawn_action_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
    ) -> Result<ActionResult> {
        let AgentActionPayload::SpawnAgent {
            role,
            placement,
            cooperation_mode,
            read_scopes,
            write_scopes,
            task_prompt,
        } = &action.payload
        else {
            return Err(MezError::invalid_args(
                "subagent execution requires a spawn_agent action",
            ));
        };
        let controller =
            self.session.primary_client_id().cloned().ok_or_else(|| {
                MezError::invalid_state("spawn_agent requires an attached primary")
            })?;
        let normalized_cooperation_mode = runtime_cooperation_mode(cooperation_mode)?;
        let normalized_role =
            self.maap_spawn_role_for_action(role, normalized_cooperation_mode, write_scopes);
        let prompt = if normalized_role != *role {
            format!(
                "[requested role alias: {}; using built-in profile: {}]\n{}",
                role, normalized_role, task_prompt
            )
        } else {
            task_prompt.clone()
        };
        let normalized_cooperation_mode_name =
            runtime_cooperation_mode_name(normalized_cooperation_mode);
        let params = serde_json::json!({
            "parent_agent": {
                "agent_id": turn.agent_id,
            },
            "placement": placement,
            "role": normalized_role,
            "cooperation_mode": normalized_cooperation_mode_name,
            "read_scopes": read_scopes,
            "write_scopes": write_scopes,
            "prompt": prompt,
        })
        .to_string();
        let spawn = runtime_subagent_spawn_request(&params, false)?;
        let placement_mode = runtime_subagent_placement_mode(&params)?;
        let spawn_json = self.spawn_runtime_subagent(&controller, spawn, placement_mode)?;
        if self.subagent_wait_policy == SubagentWaitPolicy::Join {
            let (child_agent_id, child_display_name, child_turn_id) =
                runtime_spawn_json_agent_and_turn(&spawn_json)?;
            self.joined_subagent_dependencies.insert(
                child_turn_id.clone(),
                JoinedSubagentDependency {
                    parent_turn_id: turn.turn_id.clone(),
                    parent_action_id: action.id.clone(),
                    child_turn_id: child_turn_id.clone(),
                    child_agent_id: child_agent_id.clone(),
                    child_display_name: child_display_name.clone(),
                },
            );
            let child_label =
                runtime_subagent_display_label(&child_agent_id, child_display_name.as_deref());
            let task_summary = runtime_agent_terminal_preview(task_prompt);
            return Ok(ActionResult::running(
                turn,
                action,
                vec![format!(
                    "subagent {child_label} spawn accepted for {placement} placement; waiting for task result: {task_summary}"
                )],
                Some(format!(
                    r#"{{"spawn":{},"placement":"{}","delivery_status":"accepted","join_policy":"join","join_state":"waiting","child_agent_id":"{}","child_display_name":{},"child_turn_id":"{}","error":null}}"#,
                    spawn_json,
                    json_escape(placement),
                    json_escape(&child_agent_id),
                    child_display_name
                        .as_deref()
                        .map(|name| format!(r#""{}""#, json_escape(name)))
                        .unwrap_or_else(|| "null".to_string()),
                    json_escape(&child_turn_id)
                )),
            ));
        }
        Ok(ActionResult::succeeded(
            turn,
            action,
            vec![format!(
                "subagent spawn accepted for {} placement: {}",
                placement,
                runtime_agent_terminal_preview(task_prompt)
            )],
            Some(format!(
                r#"{{"spawn":{},"placement":"{}","delivery_status":"accepted","join_policy":"detach","error":null}}"#,
                spawn_json,
                json_escape(placement)
            )),
        ))
    }

    /// Normalizes common model-produced descriptive read-only roles to the
    /// built-in explorer profile used by the runtime subagent harness.
    ///
    /// Provider models often describe a subagent's job as a role such as
    /// `repo-searcher`. MAAP execution keeps configured custom roles exact, but
    /// maps common read-only aliases onto `explorer` so safe delegation requests
    /// do not fail after the model already emitted an otherwise valid action.
    fn maap_spawn_role_for_action(
        &self,
        role: &str,
        cooperation_mode: crate::subagent::CooperationMode,
        write_scopes: &[String],
    ) -> String {
        if self.subagent_profiles.contains_key(role) {
            return role.to_string();
        }
        if cooperation_mode == crate::subagent::CooperationMode::ExploreOnly
            && write_scopes.is_empty()
            && Self::maap_read_only_subagent_role_alias(role)
        {
            return "explorer".to_string();
        }
        role.to_string()
    }

    /// Reports whether a provider-produced descriptive role name is a safe
    /// read-only alias for the built-in explorer profile.
    fn maap_read_only_subagent_role_alias(role: &str) -> bool {
        matches!(
            role,
            "repo-searcher"
                | "repository-searcher"
                | "searcher"
                | "researcher"
                | "inspector"
                | "reader"
                | "scanner"
                | "finder"
        )
    }

    /// Runs the execute message action for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn execute_message_action_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
    ) -> Result<ActionResult> {
        let AgentActionPayload::SendMessage {
            recipient,
            content_type,
            payload,
        } = &action.payload
        else {
            return Err(MezError::invalid_args(
                "message execution requires a send_message action",
            ));
        };
        let content_type = runtime_maap_message_content_type(content_type);
        if let Err(error) = validate_mmp_payload_metadata("send", &content_type, payload, None) {
            let mut result = ActionResult::failed(
                turn,
                action,
                ActionStatus::Failed,
                "invalid_message_payload",
                error.message().to_string(),
            )?;
            result.structured_content_json = Some(format!(
                r#"{{"recipient":"{}","content_type":"{}","message_id":null,"delivery_status":"rejected","protocol_error":{{"code":"{}","message":"{}"}}}}"#,
                json_escape(recipient),
                json_escape(&content_type),
                runtime_mezzanine_error_code(error.kind()),
                json_escape(error.message())
            ));
            return Ok(result);
        }
        let sender = self.runtime_message_sender_identity(turn)?;
        let recipient_target = runtime_message_recipient(recipient)?;
        let message_id = format!("{}:{}", turn.turn_id, action.id);
        let now_ms = current_unix_seconds().saturating_mul(1000);
        let envelope = Envelope {
            protocol: "mmp/1",
            id: message_id.clone(),
            message_type: "send".to_string(),
            time: format!("runtime:{now_ms}"),
            sender: sender.clone(),
            recipient: recipient_target,
            correlation_id: Some(turn.turn_id.clone()),
            ttl_ms: None,
            content_type: content_type.clone(),
            payload: payload.clone(),
            extension_fields: Vec::new(),
        };
        let delivery = match self
            .message_service
            .accept_at(&sender.agent_id, envelope, now_ms)
        {
            Ok(delivery) => delivery,
            Err(error) => {
                let mut result = ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    "transport_error",
                    error.message().to_string(),
                )?;
                result.structured_content_json = Some(format!(
                    r#"{{"recipient":"{}","message_id":null,"delivery_status":"failed","protocol_error":{{"code":"{}","message":"{}"}}}}"#,
                    json_escape(recipient),
                    runtime_mezzanine_error_code(error.kind()),
                    json_escape(error.message())
                ));
                return Ok(result);
            }
        };
        Ok(ActionResult::succeeded(
            turn,
            action,
            vec![format!(
                "message {} delivered to {} recipient(s)",
                delivery.message_id, delivery.queued_recipients
            )],
            Some(format!(
                r#"{{"recipient":"{}","message_id":"{}","delivery_status":"accepted","queued_recipients":{},"sequence":{},"protocol_error":null}}"#,
                json_escape(recipient),
                json_escape(&delivery.message_id),
                delivery.queued_recipients,
                delivery.sequence
            )),
        ))
    }

    /// Runs the runtime message sender identity operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_message_sender_identity(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<SenderIdentity> {
        let agent_id = AgentId::opaque(turn.agent_id.clone())
            .ok_or_else(|| MezError::invalid_args("turn agent id is invalid for MMP"))?;
        let pane_id = PaneId::parse('%', turn.pane_id.clone());
        let window_id = self
            .find_pane_descriptor(&turn.pane_id)
            .map(|descriptor| descriptor.window_id);
        self.message_service.ensure_agent_identity(
            SenderIdentity {
                agent_id,
                pane_id,
                window_id,
                role: Some("agent".to_string()),
                capabilities: vec!["agent-harness".to_string()],
            },
            current_unix_seconds().saturating_mul(1000),
        )
    }

    /// Runs the execute mcp action for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn execute_mcp_action_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        approved: bool,
    ) -> Result<ActionResult> {
        let AgentActionPayload::McpCall {
            server,
            tool,
            arguments_json,
        } = &action.payload
        else {
            return Err(MezError::invalid_args(
                "MCP execution requires an mcp_call action",
            ));
        };
        if let Some(block) = self.run_configured_pre_action_hooks(
            HookEvent::PreMcpToolUse,
            &runtime_pre_mcp_hook_payload(turn, action, server, tool, arguments_json),
        )? {
            let mut result = ActionResult::failed(
                turn,
                action,
                ActionStatus::Denied,
                "hook_blocked",
                block.message.clone(),
            )?;
            result.structured_content_json = Some(block.structured_json());
            return Ok(result);
        }
        let request = McpToolCallRequest {
            server_id: server.clone(),
            tool_name: tool.clone(),
            arguments_json: arguments_json.clone(),
            timeout_ms: None,
            approval_bypass: self.permission_policy.approval_bypass(),
        };
        let plan = self.mcp_registry.plan_tool_call(&request)?;
        if plan.approval_required && !approved && !self.permission_policy.approval_bypass() {
            return Ok(ActionResult::blocked(
                turn,
                action,
                vec!["approval required before executing MCP tool call".to_string()],
                format!(
                    r#"{{"approval":{{"state":"pending","kind":"mcp_call","action_id":"{}","server":"{}","tool":"{}"}}}}"#,
                    json_escape(&action.id),
                    json_escape(server),
                    json_escape(tool)
                ),
            ));
        }
        let call_id = format!("{}:{}", turn.turn_id, action.id);
        let environment = std::env::vars().collect::<BTreeMap<_, _>>();
        let audit_log = self.audit_log.as_mut();
        let mut executor = RuntimeMcpActionExecutor {
            transports: &mut self.mcp_transports,
            audit_log,
            environment,
            session_id: self.session.id.to_string(),
            actor: AuditActor {
                kind: "agent".to_string(),
                id: turn.agent_id.clone(),
            },
            call_id,
        };
        let result = match execute_mcp_action_through_runtime(turn, action, &plan, &mut executor) {
            Ok(result) => result,
            Err(error) => {
                let _ = self.mcp_registry.mark_unavailable(
                    &plan.server_id,
                    format!("runtime tool call failed: {}", error.message()),
                );
                ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    runtime_mcp_error_code(&error),
                    error.message().to_string(),
                )?
            }
        };
        self.run_configured_completed_hooks(
            HookEvent::PostMcpToolUse,
            &runtime_post_mcp_hook_payload(turn, action, &result),
        )?;
        Ok(result)
    }

    /// Runs the execute mcp action for turn async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn execute_mcp_action_for_turn_async(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        approved: bool,
    ) -> Result<ActionResult> {
        let AgentActionPayload::McpCall {
            server,
            tool,
            arguments_json,
        } = &action.payload
        else {
            return Err(MezError::invalid_args(
                "MCP execution requires an mcp_call action",
            ));
        };
        if let Some(block) = self.run_configured_pre_action_hooks(
            HookEvent::PreMcpToolUse,
            &runtime_pre_mcp_hook_payload(turn, action, server, tool, arguments_json),
        )? {
            let mut result = ActionResult::failed(
                turn,
                action,
                ActionStatus::Denied,
                "hook_blocked",
                block.message.clone(),
            )?;
            result.structured_content_json = Some(block.structured_json());
            return Ok(result);
        }
        let request = McpToolCallRequest {
            server_id: server.clone(),
            tool_name: tool.clone(),
            arguments_json: arguments_json.clone(),
            timeout_ms: None,
            approval_bypass: self.permission_policy.approval_bypass(),
        };
        let plan = self.mcp_registry.plan_tool_call(&request)?;
        if plan.approval_required && !approved && !self.permission_policy.approval_bypass() {
            return Ok(ActionResult::blocked(
                turn,
                action,
                vec!["approval required before executing MCP tool call".to_string()],
                format!(
                    r#"{{"approval":{{"state":"pending","kind":"mcp_call","action_id":"{}","server":"{}","tool":"{}"}}}}"#,
                    json_escape(&action.id),
                    json_escape(server),
                    json_escape(tool)
                ),
            ));
        }
        let call_id = format!("{}:{}", turn.turn_id, action.id);
        let environment = std::env::vars().collect::<BTreeMap<_, _>>();
        let audit_log = self.audit_log.as_mut();
        let mut executor = RuntimeMcpActionExecutor {
            transports: &mut self.mcp_transports,
            audit_log,
            environment,
            session_id: self.session.id.to_string(),
            actor: AuditActor {
                kind: "agent".to_string(),
                id: turn.agent_id.clone(),
            },
            call_id,
        };
        let result = match execute_mcp_action_through_runtime_async(
            turn,
            action,
            &plan,
            &mut executor,
        )
        .await
        {
            Ok(result) => result,
            Err(error) => {
                let _ = self.mcp_registry.mark_unavailable(
                    &plan.server_id,
                    format!("runtime tool call failed: {}", error.message()),
                );
                ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Failed,
                    runtime_mcp_error_code(&error),
                    error.message().to_string(),
                )?
            }
        };
        self.run_configured_completed_hooks(
            HookEvent::PostMcpToolUse,
            &runtime_post_mcp_hook_payload(turn, action, &result),
        )?;
        Ok(result)
    }

    /// Runs the apply permission request hooks for execution operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_permission_request_hooks_for_execution(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<usize> {
        let Some(batch) = execution.response.action_batch.clone() else {
            return Ok(0);
        };
        let mut blocked_by_hooks = 0usize;
        for index in 0..execution.action_results.len() {
            if execution.action_results[index].status != ActionStatus::Blocked {
                continue;
            }
            let action = batch
                .actions
                .iter()
                .find(|action| action.id == execution.action_results[index].action_id)
                .ok_or_else(|| {
                    MezError::invalid_state("blocked result does not match an action")
                })?;
            if let Some(block) = self.run_configured_pre_action_hooks(
                HookEvent::PermissionRequest,
                &runtime_permission_request_hook_payload(
                    turn,
                    action,
                    &execution.action_results[index],
                ),
            )? {
                let mut result = ActionResult::failed(
                    turn,
                    action,
                    ActionStatus::Denied,
                    "hook_blocked",
                    block.message.clone(),
                )?;
                result.structured_content_json = Some(block.structured_json());
                execution.action_results[index] = result;
                blocked_by_hooks = blocked_by_hooks.saturating_add(1);
            }
        }
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        Ok(blocked_by_hooks)
    }

    /// Runs the dispatch shell action to pane operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_shell_action_to_pane(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        command: &str,
        stateful: bool,
        timeout_ms: Option<u64>,
    ) -> Result<()> {
        self.require_pane_ready_for_agent_command(&turn.pane_id)?;
        let previous_readiness = self.pane_readiness_state(&turn.pane_id);
        let marker = runtime_marker_for_action(turn, &action.id)?;
        let marker_id = marker.as_str().to_string();
        let transaction = ShellTransaction::new(
            marker,
            &turn.turn_id,
            &turn.agent_id,
            &turn.pane_id,
            self.session.shell.path(),
            command,
        )?
        .with_output_transport(if stateful {
            ShellTransactionOutputTransport::Raw
        } else {
            ShellTransactionOutputTransport::Base64
        });
        let classification = self.shell_classification_for_pane(&turn.pane_id);
        let transaction_input = if stateful {
            None
        } else {
            Some(transaction.render_for_classification_input(classification))
        };
        let mut wrapper = if stateful {
            transaction.render_stateful_for_classification(classification)
        } else {
            transaction_input
                .as_ref()
                .expect("non-stateful transactions render streamed input")
                .wrapper
                .clone()
        };
        if !wrapper.ends_with('\n') {
            wrapper.push('\n');
        }
        let payload_len = transaction_input
            .as_ref()
            .map(|input| input.payload.len())
            .unwrap_or_default();
        let is_internal_apply_patch_write_phase =
            matches!(action.payload, AgentActionPayload::ApplyPatch { .. })
                && apply_patch_transaction_phase(command)
                    == Some(ApplyPatchTransactionPhase::Write);
        let emitted_action_log = if is_internal_apply_patch_write_phase {
            false
        } else {
            self.append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, action)?
        };
        let is_model_shell_command =
            matches!(action.payload, AgentActionPayload::ShellCommand { .. });
        let should_emit_fallback_action_status = (self.agent_verbose_enabled(&turn.pane_id)
            || !is_model_shell_command)
            && !is_internal_apply_patch_write_phase
            && !emitted_action_log;
        if should_emit_fallback_action_status {
            let emitted_thinking =
                self.append_agent_action_model_thinking_to_terminal_buffer(&turn.pane_id, action)?;
            if !emitted_thinking {
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &runtime_agent_shell_status(action, "shell command"),
                )?;
            }
        }
        if is_model_shell_command
            || (!is_internal_apply_patch_write_phase && !emitted_action_log)
            || self.agent_verbose_enabled(&turn.pane_id)
        {
            self.append_agent_command_preview_to_terminal_buffer(&turn.pane_id, command)?;
        }
        self.remember_mez_wrapper_filter_command(&turn.pane_id, command);
        let wrapper_bytes = wrapper.len().saturating_add(payload_len);
        self.write_runtime_pane_input(&turn.pane_id, wrapper.as_bytes())?;
        self.pane_readiness_overrides.revoke(
            &turn.pane_id,
            ReadinessOverrideRevocation::HarnessOwnedCommand,
        );
        self.set_pane_readiness(&turn.pane_id, PaneReadinessState::Busy);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "pane_readiness {} -> busy reason=shell_dispatch action={} marker={}",
                runtime_pane_readiness_state_name(previous_readiness),
                action.id,
                marker_id
            ),
        )?;
        self.append_agent_shell_command_audit(turn, action, command, "sent")?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "pane_input accepted bytes={} action={} marker={}",
                wrapper_bytes, action.id, marker_id
            ),
        )?;
        self.running_shell_transactions.insert(
            marker_id.clone(),
            RunningShellTransactionRef {
                turn_id: turn.turn_id.clone(),
                kind: RunningShellTransactionKind::AgentAction {
                    action_id: action.id.clone(),
                },
                pane_id: turn.pane_id.clone(),
                command: command.to_string(),
                started_at_unix_ms: current_unix_millis(),
                timeout_ms: Some(runtime_shell_action_timeout_ms(turn, timeout_ms)),
                pending_input_payload: transaction_input.and_then(|input| {
                    (!input.payload.is_empty()).then(|| input.payload.into_bytes())
                }),
                observed_output_bytes: 0,
                observed_output_preview: String::new(),
                observed_output_truncated: false,
            },
        );
        self.shell_transaction_require_start_markers
            .insert(marker_id.clone());
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "shell_transaction inserted marker={} action={} command={}",
                marker_id,
                action.id,
                runtime_agent_terminal_preview(command)
            ),
        )?;
        Ok(())
    }

    /// Runs the require pane ready for agent command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn require_pane_ready_for_agent_command(&self, pane_id: &str) -> Result<()> {
        match self.pane_readiness_state(pane_id) {
            PaneReadinessState::Ready => Ok(()),
            state => Err(MezError::conflict(format!(
                "pane {pane_id} is not ready for agent shell input: {}",
                runtime_pane_readiness_state_name(state)
            ))),
        }
    }

    /// Builds the best-available `PathScopes` for a pane from the last bootstrap
    /// environment signature.
    ///
    /// The current directory comes from the pane-shell-observed working directory
    /// recorded during bootstrap. Canonical path evidence is not yet resolved
    /// through the pane shell, so the resolution status is `Unresolved`, which
    /// fails closed on scoped path decisions.
    pub(super) fn path_scopes_for_pane(&self, pane_id: &str) -> Option<PathScopes> {
        let signature = self.pane_environment_signatures.get(pane_id)?;
        Some(PathScopes::unresolved(
            signature.working_directory.clone(),
            Vec::new(),
            Vec::new(),
        ))
    }

    /// Reports whether a running shell transaction should display a transient
    /// latest-output line in the pane while its output is otherwise hidden.
    pub(super) fn agent_shell_transaction_action_shows_live_output(
        &self,
        turn_id: &str,
        action_id: &str,
    ) -> bool {
        self.agent_turn_executions
            .get(turn_id)
            .and_then(|execution| execution.response.action_batch.as_ref())
            .and_then(|batch| batch.actions.iter().find(|action| action.id == action_id))
            .is_some_and(|action| matches!(action.payload, AgentActionPayload::ShellCommand { .. }))
    }

    /// Runs the subagent scope declaration for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn subagent_scope_declaration_for_turn(
        &self,
        turn: &AgentTurnRecord,
    ) -> Option<SubagentScopeDeclaration> {
        let mut declaration = self
            .subagent_scope_declarations
            .get(&turn.agent_id)
            .cloned()?;
        if let Some(signature) = self.pane_environment_signatures.get(&turn.pane_id) {
            declaration.current_directory = signature.working_directory.clone();
        }
        Some(declaration)
    }

    /// Runs the permission policy for turn operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn permission_policy_for_turn(&self, turn: &AgentTurnRecord) -> PermissionPolicy {
        let mut policy = self.permission_policy.clone();
        if let Some(preset) = self
            .subagent_scope_declaration_for_turn(turn)
            .and_then(|declaration| declaration.permission_preset)
        {
            policy.preset = preset;
        }
        policy
    }

    /// Runs the pane readiness state operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn pane_readiness_state(&self, pane_id: &str) -> PaneReadinessState {
        self.pane_readiness_states
            .get(pane_id)
            .copied()
            .unwrap_or(PaneReadinessState::Unknown)
    }

    /// Runs the set pane readiness operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn set_pane_readiness(&mut self, pane_id: &str, state: PaneReadinessState) {
        self.pane_readiness_states
            .insert(pane_id.to_string(), state);
    }

    /// Queues provider continuation for the running turn in a pane when its
    /// stored execution has no running or blocked action results left.
    ///
    /// Readiness probes already call this continuation path when the probe
    /// completes. Manual readiness overrides use this helper so an operator
    /// can unblock a turn waiting for readiness without waiting for a pending
    /// probe marker to finish.
    pub(super) fn queue_ready_provider_continuation_for_pane(&mut self, pane_id: &str) -> usize {
        if self.pane_readiness_state(pane_id) != PaneReadinessState::Ready
            || self.pane_readiness_overrides.has_pending_probe(pane_id)
        {
            return 0;
        }
        let Some(turn_id) = self
            .agent_shell_store
            .get(pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
        else {
            return 0;
        };
        if self.pending_agent_provider_tasks.contains(turn_id)
            || self.claimed_agent_provider_tasks.contains_key(turn_id)
        {
            return 0;
        }
        let turn_is_running = self
            .agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running);
        if !turn_is_running {
            return 0;
        }
        let Some(execution) = self.agent_turn_executions.get(turn_id) else {
            return 0;
        };
        if !runtime_execution_ready_for_provider_continuation(execution)
            && !self.execution_has_pending_shell_dispatch(turn_id, execution)
        {
            return 0;
        }
        self.pending_agent_provider_tasks
            .insert(turn_id.to_string());
        1
    }

    /// Runs the append agent shell command audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_agent_shell_command_audit(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        command: &str,
        outcome: &str,
    ) -> Result<()> {
        let Some(audit_log) = self.audit_log.as_mut() else {
            return Ok(());
        };
        let mut record = AuditRecord::new(
            self.session.id.to_string(),
            AuditActor {
                kind: "agent".to_string(),
                id: turn.agent_id.clone(),
            },
            "shell_command",
            "send_to_pane",
        )
        .with_pane_id(turn.pane_id.clone())
        .with_agent_id(turn.agent_id.clone())
        .with_metadata("turn_id", turn.turn_id.clone())
        .with_metadata("action_id", action.id.clone())
        .with_metadata(
            "command_sha256",
            exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, command),
        );
        record.policy_mode =
            runtime_permission_preset_name(self.permission_policy.preset).to_string();
        record.approval_state = "not_required_or_preapproved".to_string();
        record.outcome = outcome.to_string();
        let _ = audit_log.append(record.sanitized())?;
        Ok(())
    }

    /// Appends an audit event for a runtime-owned network action.
    ///
    /// URL and query values are hashed rather than stored directly so external
    /// content requests remain diagnosable without leaking sensitive inputs into
    /// the audit log.
    pub(super) fn append_agent_network_action_audit(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        outcome: &str,
    ) -> Result<()> {
        let Some(audit_log) = self.audit_log.as_mut() else {
            return Ok(());
        };
        let mut record = AuditRecord::new(
            self.session.id.to_string(),
            AuditActor {
                kind: "agent".to_string(),
                id: turn.agent_id.clone(),
            },
            "external_integration",
            "runtime_network_action",
        )
        .with_pane_id(turn.pane_id.clone())
        .with_agent_id(turn.agent_id.clone())
        .with_metadata("turn_id", turn.turn_id.clone())
        .with_metadata("action_id", action.id.clone())
        .with_metadata("action_type", action.action_type().to_string());
        match &action.payload {
            AgentActionPayload::FetchUrl { url, .. } => {
                record = record.with_metadata(
                    "url_sha256",
                    exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, url),
                );
            }
            AgentActionPayload::WebSearch { query, domains, .. } => {
                record = record.with_metadata(
                    "query_sha256",
                    exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, query),
                );
                if !domains.is_empty() {
                    record = record.with_metadata("domain_count", domains.len().to_string());
                }
            }
            _ => {}
        }
        record.policy_mode =
            runtime_permission_preset_name(self.permission_policy.preset).to_string();
        record.approval_state = "not_required_or_preapproved".to_string();
        record.outcome = outcome.to_string();
        let _ = audit_log.append(record.sanitized())?;
        Ok(())
    }

    /// Runs the persist runtime agent turn execution transcript operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn persist_runtime_agent_turn_execution_transcript(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<usize> {
        let conversation_id = self
            .agent_shell_store
            .get(&turn.pane_id)
            .map(|session| session.session_id.clone());
        if let Some(conversation_id) = conversation_id.as_deref() {
            self.record_runtime_agent_patch_results(conversation_id, execution);
        }
        let Some(store) = self.agent_transcript_store.clone() else {
            return Ok(0);
        };
        let conversation_id = conversation_id
            .ok_or_else(|| MezError::invalid_state("agent shell session missing for transcript"))?;
        let created_at_unix_seconds = current_unix_seconds().max(1);
        let entries = if self.defer_agent_transcript_writes {
            let first_sequence = self
                .deferred_transcript_next_sequences
                .get(&conversation_id)
                .copied()
                .map(Ok)
                .unwrap_or_else(|| next_transcript_sequence(&store, &conversation_id))?;
            let entries = self.runtime_transcript_entries_for_execution(
                &conversation_id,
                first_sequence,
                created_at_unix_seconds,
                turn,
                execution,
            )?;
            if let Some(next_sequence) =
                entries.last().map(|entry| entry.sequence.saturating_add(1))
            {
                self.deferred_transcript_next_sequences
                    .insert(conversation_id.clone(), next_sequence);
            }
            self.deferred_agent_transcript_writes
                .push(DeferredAgentTranscriptWrite {
                    path: store.transcript_path(&conversation_id)?,
                    store,
                    entries: entries.clone(),
                });
            entries
        } else {
            let first_sequence = next_transcript_sequence(&store, &conversation_id)?;
            let entries = self.runtime_transcript_entries_for_execution(
                &conversation_id,
                first_sequence,
                created_at_unix_seconds,
                turn,
                execution,
            )?;
            store.append_many(&entries)?;
            entries
        };
        self.agent_shell_store
            .record_transcript_entries(&turn.pane_id, entries.len())?;
        self.record_pane_transcript_ref(
            &turn.pane_id,
            format!("transcript:{}:{conversation_id}", turn.pane_id),
        )?;
        Ok(entries.len())
    }

    /// Retains exact `apply_patch` payloads and observed outcomes for export.
    ///
    /// Durable transcript entries intentionally summarize patch actions so
    /// model context stays compact. This separate pane-session ledger preserves
    /// the exact patches for `/copy-patches` without feeding them back into later
    /// model prompts.
    fn record_runtime_agent_patch_results(
        &mut self,
        conversation_id: &str,
        execution: &AgentTurnExecution,
    ) {
        let Some(batch) = execution.response.action_batch.as_ref() else {
            return;
        };
        for action in &batch.actions {
            let AgentActionPayload::ApplyPatch { patch, strip } = &action.payload else {
                continue;
            };
            let Some(result) = execution
                .action_results
                .iter()
                .find(|candidate| candidate.action_id == action.id)
            else {
                continue;
            };
            if result.status == ActionStatus::Running {
                continue;
            }
            let record = RuntimeAgentPatchRecord {
                turn_id: batch.turn_id.clone(),
                action_id: action.id.clone(),
                status: runtime_action_status_name(result.status).to_string(),
                patch: patch.clone(),
                strip: *strip,
                error_code: result.error.as_ref().map(|error| error.code.clone()),
                error_message: Self::runtime_agent_patch_record_error_message(result),
            };
            let records = self
                .agent_session_patch_records
                .entry(conversation_id.to_string())
                .or_default();
            // Running records are per-attempt placeholders. Settled records are
            // immutable so a later retry with the same action id stays visible.
            if let Some(existing) = records.iter_mut().rev().find(|candidate| {
                candidate.turn_id == record.turn_id
                    && candidate.action_id == record.action_id
                    && candidate.patch == record.patch
                    && candidate.status == "running"
            }) {
                *existing = record;
            } else if result.status == ActionStatus::Running
                || !records.iter().any(|candidate| candidate == &record)
            {
                records.push(record);
            }
        }
    }

    /// Retains patch action outcomes for the pane session that owns a turn.
    ///
    /// Recovery paths can remove an in-flight execution before transcript
    /// persistence runs, so action-result boundaries call this helper to keep
    /// `/copy-patches` complete for failed attempts as well as settled turns.
    pub(super) fn record_runtime_agent_patch_results_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) {
        let Some(conversation_id) = self
            .agent_shell_store
            .get(&turn.pane_id)
            .map(|session| session.session_id.clone())
        else {
            return;
        };
        self.record_runtime_agent_patch_results(&conversation_id, execution);
    }

    /// Returns the most useful retained diagnostic for one patch attempt.
    ///
    /// The action error often only says that the shell command exited nonzero.
    /// For `apply_patch` debugging, the captured patcher's stderr/stdout is the
    /// actionable text because it includes the failed hunk, affected path, and
    /// current-file context hints.
    fn runtime_agent_patch_record_error_message(result: &ActionResult) -> Option<String> {
        let generic = result.error.as_ref().map(|error| error.message.clone());
        if !result.is_error {
            return generic;
        }
        runtime_unrecovered_action_failure_output(result)
            .map(|output| output.trim().to_string())
            .filter(|output| !output.is_empty())
            .or(generic)
    }

    /// Builds durable transcript entries for one completed turn, including one
    /// initial environment entry that preserves the session directory.
    ///
    /// # Parameters
    /// - `conversation_id`: The durable transcript conversation id.
    /// - `first_sequence`: The next sequence number in the transcript.
    /// - `created_at_unix_seconds`: The timestamp assigned to appended entries.
    /// - `turn`: The turn whose execution is being persisted.
    /// - `execution`: The completed execution being converted into entries.
    fn runtime_transcript_entries_for_execution(
        &self,
        conversation_id: &str,
        first_sequence: u64,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<Vec<TranscriptEntry>> {
        let mut sequence = first_sequence;
        let mut entries = Vec::new();
        if sequence == 1
            && let Some(entry) = self.runtime_session_directory_transcript_entry(
                conversation_id,
                sequence,
                created_at_unix_seconds,
                turn,
            )
        {
            sequence = sequence.saturating_add(1);
            entries.push(entry);
        }
        entries.extend(transcript_entries_for_execution(
            conversation_id,
            sequence,
            created_at_unix_seconds,
            turn,
            execution,
        )?);
        Ok(entries)
    }

    /// Builds the one-time system transcript entry that makes saved sessions
    /// self-describing in `/list-sessions` and `/resume` flows.
    ///
    /// # Parameters
    /// - `conversation_id`: The durable transcript conversation id.
    /// - `sequence`: The sequence assigned to the context entry.
    /// - `created_at_unix_seconds`: The timestamp assigned to the context entry.
    /// - `turn`: The turn whose pane owns the saved session.
    fn runtime_session_directory_transcript_entry(
        &self,
        conversation_id: &str,
        sequence: u64,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
    ) -> Option<TranscriptEntry> {
        let working_directory = self.pane_current_working_directory(&turn.pane_id)?;
        let project_root = discover_project_root(&working_directory);
        let mut content = format!("cwd={}", working_directory.to_string_lossy());
        if !project_root.as_os_str().is_empty() {
            content.push('\n');
            content.push_str(&format!("project_root={}", project_root.to_string_lossy()));
        }
        Some(TranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence,
            created_at_unix_seconds,
            role: TranscriptRole::System,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            content,
        })
    }

    /// Retains the latest model-authored `say` text for pane-local copy commands.
    pub(super) fn record_agent_copy_output(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) {
        let Some(batch) = execution.response.action_batch.as_ref() else {
            return;
        };
        let Some((output, content_type)) = batch.actions.iter().rev().find_map(|action| {
            if let AgentActionPayload::Say {
                text, content_type, ..
            } = &action.payload
                && !text.trim().is_empty()
            {
                Some((text.clone(), content_type.clone()))
            } else {
                None
            }
        }) else {
            return;
        };
        self.agent_copy_outputs.insert(
            turn.pane_id.clone(),
            RuntimeAgentCopyOutput {
                turn_id: turn.turn_id.clone(),
                output,
                content_type,
            },
        );
    }

    /// Adds provider-reported token usage to the active pane conversation.
    #[cfg(test)]
    pub(super) fn record_agent_provider_token_usage(
        &mut self,
        pane_id: &str,
        usage: ModelTokenUsage,
    ) {
        let agent_id = format!("agent-{pane_id}");
        let profile = self
            .active_model_profile_for_pane(pane_id, &agent_id, None)
            .ok()
            .map(|(_, profile)| profile);
        self.record_agent_provider_token_usage_with_profile(
            pane_id,
            usage,
            usage,
            profile.as_ref(),
        );
    }

    /// Adds provider-reported token usage using the exact selected model profile.
    pub(super) fn record_agent_provider_token_usage_with_profile(
        &mut self,
        pane_id: &str,
        usage: ModelTokenUsage,
        latest_context_usage: ModelTokenUsage,
        profile: Option<&ModelProfile>,
    ) {
        if usage.is_zero() {
            return;
        }
        let conversation_id = self
            .agent_shell_store
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .unwrap_or_else(|| format!("pane:{pane_id}"));
        self.agent_token_usage_by_conversation
            .entry(conversation_id.clone())
            .or_default()
            .add_assign(usage);
        if let Some(display) = profile.and_then(|profile| {
            runtime_agent_provider_context_usage_display(profile, latest_context_usage)
        }) {
            self.agent_context_usage_by_conversation
                .insert(conversation_id, display);
        }
        let _ = self.checkpoint_agent_session_metadata();
    }

    /// Stores the latest provider-reported quota usage for the active pane conversation.
    pub(super) fn record_agent_provider_quota_usage(
        &mut self,
        pane_id: &str,
        quota_usage: &[ProviderQuotaUsage],
    ) {
        if quota_usage.is_empty() {
            return;
        }
        let conversation_id = self
            .agent_shell_store
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .unwrap_or_else(|| format!("pane:{pane_id}"));
        self.agent_quota_usage_by_conversation
            .insert(conversation_id, quota_usage.to_vec());
    }

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
    fn fail_agent_turn_after_provider_completion_application_error(
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
            .agent_turn_ledger
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
        let _ = self.agent_scheduler.complete(&turn.turn_id);
        let _ = self
            .agent_turn_ledger
            .finish_turn(&turn.turn_id, AgentTurnState::Failed);
        let _ = self.append_agent_trace_turn_transition(
            turn,
            current_state.unwrap_or(turn.state),
            AgentTurnState::Failed,
            "completion_application_error_fallback",
        );
        if self
            .agent_shell_store
            .get(&turn.pane_id)
            .and_then(|session| session.running_turn_id.as_deref())
            == Some(turn.turn_id.as_str())
        {
            let _ = self
                .agent_shell_store
                .finish_turn(&turn.pane_id, &turn.turn_id);
        }
        self.agent_turn_contexts.remove(&turn.turn_id);
        self.agent_turn_executions.remove(&turn.turn_id);
        self.agent_turn_pending_steering.remove(&turn.turn_id);
        self.clear_agent_failure_feedback_attempts_for_turn(&turn.turn_id);
        self.agent_turn_shell_dispatch_history.remove(&turn.turn_id);
        self.agent_turn_network_action_history.remove(&turn.turn_id);
        self.clear_joined_subagent_dependencies_for_turn(&turn.turn_id);
        self.clear_agent_pre_shell_hook_completions_for_turn(&turn.turn_id);
        self.agent_turn_model_profiles.remove(&turn.turn_id);
        self.pending_agent_provider_tasks.remove(&turn.turn_id);
        self.claimed_agent_provider_tasks.remove(&turn.turn_id);
        self.blocked_agent_approval_refs
            .retain(|_, approval_ref| approval_ref.turn_id != turn.turn_id);
        let _ = self.start_ready_agent_turns();
    }

    /// Runs the fail agent turn for provider error operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn fail_agent_turn_for_provider_error(
        &mut self,
        turn: &AgentTurnRecord,
        provider_id: &str,
        model_profile: &ModelProfile,
        error: &MezError,
    ) -> Result<()> {
        self.refresh_agent_turn_project_guidance_context(turn)?;
        let context = self
            .agent_turn_contexts
            .get(&turn.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let request = assemble_model_request_with_retained_tail_percent(
            model_profile,
            turn,
            &context,
            self.agent_compaction_raw_retention_percent,
        )?;
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
                quota_usage: Default::default(),
                action_batch: None,
            },
            latest_response_usage: Default::default(),
            action_results: Vec::new(),
            final_turn: true,
            terminal_state: AgentTurnState::Failed,
        };
        self.agent_turn_executions
            .insert(turn.turn_id.clone(), execution.clone());
        let transcript_entries =
            self.persist_runtime_agent_turn_execution_transcript(turn, &execution)?;
        self.emit_subagent_task_result_for_execution(turn, &execution)?;
        self.complete_running_agent_turn_and_start_ready(
            turn,
            AgentTurnState::Failed,
            "provider_error",
        )?;
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
    pub(super) fn fail_running_shell_transaction_action(
        &mut self,
        transaction_ref: &RunningShellTransactionRef,
        marker: &str,
        failure: RuntimeShellTransactionActionFailure,
    ) -> Result<usize> {
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == transaction_ref.turn_id)
            .cloned()
        else {
            return Ok(0);
        };
        let maybe_failure = {
            let execution = self
                .agent_turn_executions
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
                let structured_content = shell_command_structured_content_json(
                    &action,
                    failure.sent_to_pane,
                    serde_json::Value::Null,
                    &[],
                    failure.terminal_observation.clone(),
                )?;
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
            self.agent_turn_executions.remove(&turn.turn_id);
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
    pub(super) fn append_runtime_agent_execution_failure_audit(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let Some(model_profile) = self.agent_turn_model_profiles.get(&turn.turn_id).cloned() else {
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
    pub(super) fn fail_agent_turn_for_hook_block(
        &mut self,
        turn: &AgentTurnRecord,
        model_profile: &ModelProfile,
        block: &RuntimeHookPipelineBlock,
    ) -> Result<()> {
        self.refresh_agent_turn_project_guidance_context(turn)?;
        let context = self
            .agent_turn_contexts
            .get(&turn.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let request = assemble_model_request_with_retained_tail_percent(
            model_profile,
            turn,
            &context,
            self.agent_compaction_raw_retention_percent,
        )?;
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
                quota_usage: Default::default(),
                action_batch: None,
            },
            latest_response_usage: Default::default(),
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

    /// Runs the append subagent spawn audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_subagent_spawn_audit(
        &mut self,
        spawn: &SubagentSpawnRequest,
        child_agent_id: &str,
        pane_id: &str,
    ) -> Result<()> {
        let Some(audit_log) = self.audit_log.as_mut() else {
            return Ok(());
        };
        let record = AuditRecord::subagent_spawn(
            self.session.id.to_string(),
            AuditActor {
                kind: "agent".to_string(),
                id: spawn.parent_agent_id.clone(),
            },
            spawn.parent_agent_id.clone(),
            child_agent_id.to_string(),
            spawn.requested_role.clone(),
            runtime_cooperation_mode_name(spawn.cooperation_mode),
            "accepted",
        )
        .with_pane_id(pane_id.to_string());
        let _ = audit_log.append(record)?;
        Ok(())
    }

    /// Runs the emit subagent task result for execution operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn emit_subagent_task_result_for_execution(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let success = execution.terminal_state == AgentTurnState::Completed;
        let summary = if success {
            "subagent task completed"
        } else {
            "subagent task failed"
        };
        let output = subagent_task_output_for_execution(execution);
        self.emit_subagent_task_result(turn, success, summary, &output)
    }

    /// Runs the emit cancelled subagent task result operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn emit_cancelled_subagent_task_result(
        &mut self,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        self.emit_subagent_task_result(
            turn,
            false,
            "subagent task cancelled",
            "cancelled by runtime request",
        )
    }

    /// Runs the emit subagent task result for state operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn emit_subagent_task_result_for_state(
        &mut self,
        turn: &AgentTurnRecord,
        state: AgentTurnState,
    ) -> Result<()> {
        match state {
            AgentTurnState::Completed => self.emit_subagent_task_result(
                turn,
                true,
                "subagent task completed",
                "completed without provider output",
            ),
            AgentTurnState::Failed => self.emit_subagent_task_result(
                turn,
                false,
                "subagent task failed",
                "failed without provider output",
            ),
            AgentTurnState::Interrupted => self.emit_subagent_task_result(
                turn,
                false,
                "subagent task interrupted",
                "interrupted by snapshot resume",
            ),
            _ => Ok(()),
        }
    }

    /// Emits an intermediate MMP task-status update for a spawned subagent
    /// without closing its task route or releasing its active scope.
    ///
    /// Status delivery is best-effort after spawn setup: an offline parent is
    /// recorded as an undelivered runtime event, but the child turn keeps its
    /// normal lifecycle so approval or provider work can continue.
    pub(super) fn emit_subagent_task_status(
        &mut self,
        turn: &AgentTurnRecord,
        state: TaskState,
        progress_percent: Option<u8>,
        summary: &str,
    ) -> Result<()> {
        let Some(parent_agent_id) = self.subagent_task_routes.get(&turn.turn_id).cloned() else {
            return Ok(());
        };
        let now_ms = current_unix_seconds().saturating_mul(1000);
        let parent_identity = self.message_service.ensure_agent_identity(
            SenderIdentity {
                agent_id: AgentId::opaque(parent_agent_id.clone()).ok_or_else(|| {
                    MezError::invalid_args("subagent parent agent id is invalid for MMP")
                })?,
                pane_id: None,
                window_id: None,
                role: Some("agent".to_string()),
                capabilities: Vec::new(),
            },
            now_ms,
        )?;
        if self
            .message_service
            .subscription(&parent_identity.agent_id)
            .is_none()
        {
            self.message_service.subscribe(&parent_identity.agent_id)?;
        }
        let child_identity = self.runtime_message_sender_identity(turn)?;
        let payload = TaskStatusPayload {
            task_id: turn.turn_id.clone(),
            state,
            progress_percent,
            summary: summary.to_string(),
        };
        let child_display_name = self
            .subagent_lineage
            .get(&turn.agent_id)
            .map(|lineage| lineage.display_name.clone());
        let envelope = Envelope {
            protocol: "mmp/1",
            id: format!(
                "{}:task_status:{}",
                turn.turn_id,
                runtime_task_state_suffix(state)
            ),
            message_type: "task_status".to_string(),
            time: format!("runtime:{now_ms}"),
            sender: child_identity.clone(),
            recipient: Recipient::Agent(parent_identity.agent_id),
            correlation_id: Some(turn.turn_id.clone()),
            ttl_ms: None,
            content_type: "application/json".to_string(),
            payload: payload.to_json(),
            extension_fields: child_display_name
                .as_deref()
                .map(|name| {
                    vec![(
                        "subagent_display_name".to_string(),
                        format!(r#""{}""#, json_escape(name)),
                    )]
                })
                .unwrap_or_default(),
        };
        let delivery = self
            .message_service
            .accept_at(&child_identity.agent_id, envelope, now_ms);
        let child_label =
            runtime_subagent_display_label(&turn.agent_id, child_display_name.as_deref());
        self.append_subagent_parent_status_line(
            &parent_agent_id,
            &format!(
                "subagent {} {}: {}",
                child_label,
                runtime_task_state_suffix(state),
                summary
            ),
        )?;
        if let Err(error) = delivery {
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","subagent_task_status":"undelivered","error_code":"{}","error":"{}"}}"#,
                    json_escape(&turn.pane_id),
                    json_escape(&turn.turn_id),
                    runtime_agent_turn_state_name(turn.state),
                    runtime_mezzanine_error_code(error.kind()),
                    json_escape(error.message())
                ),
            )?;
        }
        Ok(())
    }

    /// Runs the emit subagent task result operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn emit_subagent_task_result(
        &mut self,
        turn: &AgentTurnRecord,
        success: bool,
        summary: &str,
        output: &str,
    ) -> Result<()> {
        let parent_agent_id = self.subagent_task_result_parent_agent_id(turn);
        let has_subagent_runtime_state = parent_agent_id.is_some()
            || self
                .subagent_scope_declarations
                .contains_key(&turn.agent_id)
            || self.subagent_lineage.contains_key(&turn.agent_id)
            || !self
                .subagent_scopes
                .active_write_scopes_for(&turn.agent_id)
                .is_empty();
        if !has_subagent_runtime_state {
            return Ok(());
        }

        let now_ms = current_unix_seconds().saturating_mul(1000);
        let child_display_name = self
            .subagent_lineage
            .get(&turn.agent_id)
            .map(|lineage| lineage.display_name.clone());
        let child_label =
            runtime_subagent_display_label(&turn.agent_id, child_display_name.as_deref());
        let delivery = match parent_agent_id.clone() {
            Some(parent_agent_id) => self.deliver_subagent_task_result_message(
                turn,
                &parent_agent_id,
                success,
                summary,
                output,
                now_ms,
            ),
            None => {
                self.append_lifecycle_event(
                    EventKind::AgentStatus,
                    format!(
                        r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","subagent_task_result":"undelivered","error_code":"not_found","error":"subagent parent route not found"}}"#,
                        json_escape(&turn.pane_id),
                        json_escape(&turn.turn_id),
                        runtime_agent_turn_state_name(turn.state),
                    ),
                )?;
                Ok(())
            }
        };
        if let Some(parent_agent_id) = parent_agent_id.as_deref() {
            self.append_subagent_parent_status_line(
                parent_agent_id,
                &format!(
                    "subagent {} {}: {}",
                    child_label,
                    runtime_subagent_result_status_label(success, summary),
                    summary
                ),
            )?;
        }
        self.subagent_task_routes.remove(&turn.turn_id);
        self.subagent_scopes.unregister(&turn.agent_id);
        self.subagent_scope_declarations.remove(&turn.agent_id);
        self.subagent_lineage.remove(&turn.agent_id);
        self.resolve_joined_subagent_dependency(turn, success, summary, output)?;
        self.pending_terminal_subagent_pane_closes
            .insert(turn.pane_id.clone());
        if let Err(error) = delivery {
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","subagent_task_result":"undelivered","error_code":"{}","error":"{}"}}"#,
                    json_escape(&turn.pane_id),
                    json_escape(&turn.turn_id),
                    runtime_agent_turn_state_name(turn.state),
                    runtime_mezzanine_error_code(error.kind()),
                    json_escape(error.message())
                ),
            )?;
        }
        Ok(())
    }

    /// Returns the parent agent that should receive a terminal child task result.
    ///
    /// Normal subagent delivery uses `subagent_task_routes`, but joined parent
    /// continuations must still be resolved if that route was already cleaned up.
    /// In that case the parent turn recorded in the join dependency is the
    /// fallback source of truth.
    fn subagent_task_result_parent_agent_id(&self, turn: &AgentTurnRecord) -> Option<String> {
        self.subagent_task_routes
            .get(&turn.turn_id)
            .cloned()
            .or_else(|| {
                let dependency = self.joined_subagent_dependencies.get(&turn.turn_id)?;
                self.agent_turn_ledger
                    .turns()
                    .iter()
                    .find(|turn| turn.turn_id == dependency.parent_turn_id)
                    .map(|turn| turn.agent_id.clone())
            })
            .or_else(|| {
                self.subagent_lineage
                    .get(&turn.agent_id)
                    .map(|lineage| lineage.parent_agent_id.clone())
                    .filter(|parent_agent_id| !parent_agent_id.is_empty())
            })
    }

    /// Delivers the terminal `task_result` envelope for a spawned subagent.
    ///
    /// Delivery is best-effort from the caller's perspective: the caller records
    /// and resolves terminal child state even when this function returns an MMP
    /// identity, subscription, or accept error.
    fn deliver_subagent_task_result_message(
        &mut self,
        turn: &AgentTurnRecord,
        parent_agent_id: &str,
        success: bool,
        summary: &str,
        output: &str,
        now_ms: u64,
    ) -> Result<()> {
        let parent_identity = self.message_service.ensure_agent_identity(
            SenderIdentity {
                agent_id: AgentId::opaque(parent_agent_id.to_string()).ok_or_else(|| {
                    MezError::invalid_args("subagent parent agent id is invalid for MMP")
                })?,
                pane_id: None,
                window_id: None,
                role: Some("agent".to_string()),
                capabilities: Vec::new(),
            },
            now_ms,
        )?;
        if self
            .message_service
            .subscription(&parent_identity.agent_id)
            .is_none()
        {
            self.message_service.subscribe(&parent_identity.agent_id)?;
        }
        let child_identity = self.runtime_message_sender_identity(turn)?;
        let child_display_name = self
            .subagent_lineage
            .get(&turn.agent_id)
            .map(|lineage| lineage.display_name.clone());
        let payload = TaskResultPayload {
            task_id: turn.turn_id.clone(),
            success,
            summary: summary.to_string(),
            output: output.to_string(),
        };
        let envelope = Envelope {
            protocol: "mmp/1",
            id: format!("{}:task_result:final", turn.turn_id),
            message_type: "task_result".to_string(),
            time: format!("runtime:{now_ms}"),
            sender: child_identity.clone(),
            recipient: Recipient::Agent(parent_identity.agent_id),
            correlation_id: Some(turn.turn_id.clone()),
            ttl_ms: None,
            content_type: "application/json".to_string(),
            payload: payload.to_json(),
            extension_fields: child_display_name
                .as_deref()
                .map(|name| {
                    vec![(
                        "subagent_display_name".to_string(),
                        format!(r#""{}""#, json_escape(name)),
                    )]
                })
                .unwrap_or_default(),
        };
        self.message_service
            .accept_at(&child_identity.agent_id, envelope, now_ms)
            .map(|_| ())
    }

    /// Resolves a parent `spawn_agent` action that joined a child task result.
    ///
    /// Joined child task results are delivered through MMP for observability and
    /// also converted into the parent turn's MAAP action result so the next
    /// provider request receives the child output as tool context. The parent
    /// stays blocked until every joined child action in its current execution
    /// has settled, then it is resumed and queued for provider continuation.
    fn resolve_joined_subagent_dependency(
        &mut self,
        turn: &AgentTurnRecord,
        success: bool,
        summary: &str,
        output: &str,
    ) -> Result<()> {
        let Some(dependency) = self.joined_subagent_dependencies.remove(&turn.turn_id) else {
            return Ok(());
        };
        let Some(parent_turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|candidate| candidate.turn_id == dependency.parent_turn_id)
            .cloned()
        else {
            return Ok(());
        };
        let parent_previous_state = parent_turn.state;
        let (observed_result, ready_for_continuation) = {
            let Some(execution) = self
                .agent_turn_executions
                .get_mut(&dependency.parent_turn_id)
            else {
                return Ok(());
            };
            let Some(batch) = execution.response.action_batch.as_ref() else {
                return Ok(());
            };
            let Some(action) = batch
                .actions
                .iter()
                .find(|action| action.id == dependency.parent_action_id)
                .cloned()
            else {
                return Ok(());
            };
            let Some(result_index) = execution
                .action_results
                .iter()
                .position(|result| result.action_id == dependency.parent_action_id)
            else {
                return Ok(());
            };
            let child_label = runtime_subagent_display_label(
                &dependency.child_agent_id,
                dependency.child_display_name.as_deref(),
            );
            let result_summary = if success {
                format!("subagent {child_label} completed: {summary}")
            } else {
                format!("subagent {child_label} failed: {summary}")
            };
            let observed_result = ActionResult::succeeded(
                &parent_turn,
                &action,
                vec![result_summary],
                Some(format!(
                    r#"{{"join_policy":"join","join_state":"completed","child_agent_id":"{}","child_display_name":{},"child_turn_id":"{}","task_result":{{"success":{},"summary":"{}","output":"{}"}}}}"#,
                    json_escape(&dependency.child_agent_id),
                    dependency
                        .child_display_name
                        .as_deref()
                        .map(|name| format!(r#""{}""#, json_escape(name)))
                        .unwrap_or_else(|| "null".to_string()),
                    json_escape(&dependency.child_turn_id),
                    success,
                    json_escape(summary),
                    json_escape(output)
                )),
            );
            execution.action_results[result_index] = observed_result.clone();
            execution.final_turn = false;
            execution.terminal_state = runtime_agent_turn_state_from_action_results(
                &execution.action_results,
                execution.final_turn,
            );
            let ready_for_continuation =
                runtime_execution_ready_for_provider_continuation(execution);
            (observed_result, ready_for_continuation)
        };
        if let Some(context) = self.agent_turn_contexts.get_mut(&dependency.parent_turn_id) {
            context.blocks.push(ContextBlock {
                source: ContextSourceKind::ActionResult,
                label: format!("action result {}", observed_result.action_id),
                content: action_result_context_content(&observed_result),
            });
        }
        self.append_agent_trace_turn_event(
            &parent_turn.pane_id,
            &parent_turn.turn_id,
            &format!(
                "joined_subagent_result child_turn={} child_agent={} success={}",
                dependency.child_turn_id, dependency.child_agent_id, success
            ),
        )?;
        if ready_for_continuation {
            let _ = self.agent_scheduler.resume_blocked(&parent_turn.turn_id);
            self.append_agent_trace_turn_event(
                &parent_turn.pane_id,
                &parent_turn.turn_id,
                "scheduler blocked -> running reason=joined_subagent_result_ready",
            )?;
            if parent_previous_state == AgentTurnState::Blocked {
                self.agent_turn_ledger
                    .resume_blocked_turn(&parent_turn.turn_id)?;
                self.append_agent_trace_turn_transition(
                    &parent_turn,
                    AgentTurnState::Blocked,
                    AgentTurnState::Running,
                    "joined_subagent_result_ready",
                )?;
            }
            self.pending_agent_provider_tasks
                .insert(parent_turn.turn_id.clone());
            self.append_agent_trace_turn_event(
                &parent_turn.pane_id,
                &parent_turn.turn_id,
                "provider_task queued reason=joined_subagent_result_ready",
            )?;
            self.append_agent_status_text_to_terminal_buffer(
                &parent_turn.pane_id,
                "agent: subagent results received; continuing",
            )?;
        }
        Ok(())
    }

    /// Appends a subagent status update into the controlling pane buffer.
    ///
    /// MMP remains the structured delivery mechanism; this visible line gives
    /// the user a copyable, in-context event stream in the parent window.
    pub(super) fn append_subagent_parent_status_line(
        &mut self,
        parent_agent_id: &str,
        text: &str,
    ) -> Result<()> {
        let Some(parent_pane_id) = runtime_agent_pane_id(parent_agent_id) else {
            return Ok(());
        };
        if runtime_pane_by_id(&self.session, parent_pane_id.as_str()).is_err() {
            return Ok(());
        }
        self.append_agent_status_text_to_terminal_buffer(parent_pane_id.as_str(), text)
    }

    /// Closes a terminal subagent pane after final turn cleanup has run.
    fn close_terminal_subagent_pane_if_pending(&mut self, turn: &AgentTurnRecord) -> Result<()> {
        if !self
            .pending_terminal_subagent_pane_closes
            .remove(&turn.pane_id)
        {
            return Ok(());
        }
        if self.pane_closing.contains(&turn.pane_id) {
            return Ok(());
        }
        if self.find_pane_descriptor(&turn.pane_id).is_none() {
            return Ok(());
        }
        let Some(primary) = self.session.primary_client_id().cloned() else {
            return Ok(());
        };
        self.dispatch_runtime_pane_close(
            &primary,
            &format!(
                r#"{{"pane_id":"{}","force":true}}"#,
                json_escape(&turn.pane_id)
            ),
        )?;
        let live_windows = self
            .session
            .windows()
            .iter()
            .map(|window| window.id.to_string())
            .collect::<std::collections::BTreeSet<_>>();
        self.subagent_window_ids
            .retain(|window_id| live_windows.contains(window_id));
        self.refresh_subagent_window_names(&primary)?;
        Ok(())
    }

    /// Runs the append credential access audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_credential_access_audit(
        &mut self,
        provider: &str,
        credential_id: &str,
        purpose: &str,
        outcome: &str,
    ) -> Result<()> {
        let Some(audit_log) = self.audit_log.as_mut() else {
            return Ok(());
        };
        let record = AuditRecord::credential_access_attempt(
            self.session.id.to_string(),
            AuditActor {
                kind: "runtime".to_string(),
                id: "provider".to_string(),
            },
            provider.to_string(),
            credential_id.to_string(),
            purpose.to_string(),
            outcome.to_string(),
        );
        let _ = audit_log.append(record)?;
        Ok(())
    }

    /// Runs the append provider request audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_provider_request_audit(
        &mut self,
        turn: &AgentTurnRecord,
        model_profile: &ModelProfile,
        provider_id: &str,
        outcome: &str,
    ) -> Result<()> {
        let Some(audit_log) = self.audit_log.as_mut() else {
            return Ok(());
        };
        let record = AuditRecord::provider_request(
            self.session.id.to_string(),
            AuditActor {
                kind: "agent".to_string(),
                id: turn.agent_id.clone(),
            },
            provider_id.to_string(),
            model_profile.model.clone(),
            turn.turn_id.clone(),
            outcome.to_string(),
        )
        .with_agent_id(turn.agent_id.clone())
        .with_pane_id(turn.pane_id.clone());
        let _ = audit_log.append(record.sanitized())?;
        Ok(())
    }

    /// Runs the append provider request failure audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_provider_request_failure_audit(
        &mut self,
        turn: &AgentTurnRecord,
        model_profile: &ModelProfile,
        provider_id: &str,
        error: &MezError,
    ) -> Result<()> {
        let Some(audit_log) = self.audit_log.as_mut() else {
            return Ok(());
        };
        let mut record = AuditRecord::provider_request(
            self.session.id.to_string(),
            AuditActor {
                kind: "agent".to_string(),
                id: turn.agent_id.clone(),
            },
            provider_id.to_string(),
            model_profile.model.clone(),
            turn.turn_id.clone(),
            "failed",
        )
        .with_agent_id(turn.agent_id.clone())
        .with_pane_id(turn.pane_id.clone())
        .with_metadata("error_kind", runtime_mezzanine_error_code(error.kind()))
        .with_metadata(
            "error_message",
            runtime_provider_audit_error_message(error.message()),
        );
        if let Some(raw_text) = error.provider_raw_text() {
            record = record
                .with_metadata("provider_raw_text_bytes", raw_text.len().to_string())
                .with_metadata(
                    "provider_raw_text_sha256",
                    exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, raw_text),
                );
        }
        if let Some(failure_json) = error.provider_failure_json() {
            record = record
                .with_metadata("provider_failure_json", failure_json.to_string())
                .with_metadata(
                    "provider_failure_json_bytes",
                    failure_json.len().to_string(),
                )
                .with_metadata(
                    "provider_failure_json_sha256",
                    exact_command_sha256(DEFAULT_COMMAND_SHELL_CLASSIFICATION, failure_json),
                );
        }
        let _ = audit_log.append(record.sanitized())?;
        Ok(())
    }

    /// Runs the queue blocked approvals for execution operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn queue_blocked_approvals_for_execution(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<Vec<String>> {
        let mut approval_ids = Vec::new();
        let subagent_scope = self.subagent_scope_declaration_for_turn(turn);
        for result in execution
            .action_results
            .iter()
            .filter(|result| result.status == ActionStatus::Blocked)
        {
            let approval_id = self.queue_blocked_approval(runtime_blocked_approval_request(
                turn,
                result,
                subagent_scope.as_ref(),
            ))?;
            self.blocked_agent_approval_refs.insert(
                approval_id.clone(),
                BlockedAgentApprovalRef {
                    turn_id: turn.turn_id.clone(),
                    action_id: result.action_id.clone(),
                },
            );
            if let Some(approval) = self.blocked_approvals.get(&approval_id).cloned() {
                let log_line = runtime_agent_pending_approval_log_line(&approval);
                self.append_agent_status_text_to_terminal_buffer(&turn.pane_id, &log_line)?;
            }
            approval_ids.push(approval_id);
        }
        if approval_ids.is_empty() {
            return Err(MezError::invalid_state(
                "blocked agent turn did not include blocked action results",
            ));
        }
        Ok(approval_ids)
    }

    /// Reconciles pending blocked agent approvals after permission policy changes.
    ///
    /// # Parameters
    /// - `caller_client_id`: Client that caused the policy update, when known.
    /// - `previous`: Permission policy before the update.
    /// - `source`: Human-readable source of the policy update for lifecycle events.
    pub(super) fn reconcile_pending_agent_approvals_after_permission_change(
        &mut self,
        caller_client_id: Option<&crate::ids::ClientId>,
        previous: &PermissionPolicy,
        source: &str,
    ) -> Result<usize> {
        if previous.preset == self.permission_policy.preset
            && previous.approval_policy == self.permission_policy.approval_policy
            && previous.approval_bypass() == self.permission_policy.approval_bypass()
            && previous.rules() == self.permission_policy.rules()
        {
            return Ok(0);
        }
        let pending_ids = self
            .blocked_approvals
            .pending()
            .into_iter()
            .map(|approval| approval.id.clone())
            .collect::<Vec<_>>();
        let mut resumed = 0usize;
        for approval_id in pending_ids {
            let Some(approval) = self.blocked_approvals.get(&approval_id).cloned() else {
                continue;
            };
            if !self
                .pending_agent_approval_is_satisfied_by_current_policy(&approval_id, &approval)?
            {
                continue;
            }
            let controller = caller_client_id
                .cloned()
                .or_else(|| self.session.primary_client_id().cloned())
                .ok_or_else(|| {
                    MezError::invalid_state(
                        "policy-resolved blocked approval requires an attached primary client",
                    )
                })?;
            let decided = self
                .blocked_approvals
                .decide_with_client(
                    &approval_id,
                    crate::permissions::ApprovalDecision::Approve,
                    None,
                    Some(controller.to_string()),
                )?
                .clone();
            let count =
                self.resume_approved_blocked_agent_action(&approval_id, &decided, &controller)?;
            let count = count.unwrap_or(0);
            resumed = resumed.saturating_add(count);
            self.append_primary_lifecycle_event(
                EventKind::ApprovalChanged,
                format!(
                    r#"{{"approval_id":"{}","decision":"approve","state":"decided","source":"{}","agent_actions_resumed":{}}}"#,
                    json_escape(&approval_id),
                    json_escape(source),
                    count
                ),
            )?;
        }
        Ok(resumed)
    }

    /// Reports whether a pending blocked approval is now satisfied by policy.
    ///
    /// # Parameters
    /// - `approval_id`: Identifier of the pending blocked approval.
    /// - `approval`: Pending blocked approval metadata.
    fn pending_agent_approval_is_satisfied_by_current_policy(
        &self,
        approval_id: &str,
        approval: &BlockedApprovalRequest,
    ) -> Result<bool> {
        if approval.state != crate::permissions::BlockedApprovalState::Pending {
            return Ok(false);
        }
        let Some(approval_ref) = self.blocked_agent_approval_refs.get(approval_id) else {
            return Ok(false);
        };
        let execution = self
            .agent_turn_executions
            .get(&approval_ref.turn_id)
            .ok_or_else(|| MezError::invalid_state("blocked agent execution is unavailable"))?;
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == approval_ref.turn_id)
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        let batch = execution
            .response
            .action_batch
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("blocked execution has no action batch"))?;
        let action = batch
            .actions
            .iter()
            .find(|action| action.id == approval_ref.action_id)
            .ok_or_else(|| {
                MezError::invalid_state("blocked approval does not match an agent action")
            })?;
        let permission_policy = self.permission_policy_for_turn(turn);
        if permission_policy.approval_bypass()
            || permission_policy.approval_policy == crate::permissions::ApprovalPolicy::FullAccess
        {
            return Ok(true);
        }
        match &action.payload {
            _ if local_action_plan(action)?.is_some() => {
                let Some(plan) = local_action_plan(action)? else {
                    return Ok(false);
                };
                let subagent_scope = self.subagent_scope_declaration_for_turn(turn);
                if let Some(scope) = subagent_scope.as_ref()
                    && let Some(_message) = scope.shell_command_violation(&plan.policy_command)?
                {
                    return Ok(false);
                }
                let path_scopes = if subagent_scope.is_some() {
                    None
                } else {
                    self.path_scopes_for_pane(&turn.pane_id)
                };
                Ok(matches!(
                    permission_policy.evaluate_shell_command_with_approvals_scoped(
                        &plan.policy_command,
                        &self.session_approvals,
                        path_scopes.as_ref(),
                    ),
                    RuleDecision::Allow
                ) || (permission_policy.approval_policy
                    == crate::permissions::ApprovalPolicy::AutoAllow
                    && runtime_action_supports_auto_allow(action)))
            }
            _ if network_action_plan(action)?.is_some() => {
                let Some(plan) = network_action_plan(action)? else {
                    return Ok(false);
                };
                Ok(matches!(
                    permission_policy.evaluate_shell_command_with_approvals_scoped(
                        &plan.policy_command,
                        &self.session_approvals,
                        None,
                    ),
                    RuleDecision::Allow
                ) || (permission_policy.approval_policy
                    == crate::permissions::ApprovalPolicy::AutoAllow
                    && runtime_action_supports_auto_allow(action)))
            }
            AgentActionPayload::McpCall { .. } => Ok(permission_policy.approval_policy
                == crate::permissions::ApprovalPolicy::AutoAllow
                && runtime_action_supports_auto_allow(action)),
            AgentActionPayload::ConfigChange { .. } => Ok(permission_policy.approval_policy
                == crate::permissions::ApprovalPolicy::AutoAllow
                && runtime_action_supports_auto_allow(action)),
            _ => Ok(false),
        }
    }

    /// Runs the resume approved blocked agent action operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn resume_approved_blocked_agent_action(
        &mut self,
        approval_id: &str,
        approval: &BlockedApprovalRequest,
        caller_client_id: &crate::ids::ClientId,
    ) -> Result<Option<usize>> {
        if !matches!(
            approval.action_kind.as_str(),
            "shell_command"
                | "apply_patch"
                | "mcp_call"
                | "config_change"
                | "web_search"
                | "fetch_url"
        ) {
            return Ok(None);
        }
        let Some(approval_ref) = self.blocked_agent_approval_refs.get(approval_id).cloned() else {
            return Ok(None);
        };
        let mut execution = self
            .agent_turn_executions
            .get(&approval_ref.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("blocked agent execution is unavailable"))?;
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == approval_ref.turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        let batch = execution
            .response
            .action_batch
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("blocked execution has no action batch"))?;
        let action = batch
            .actions
            .iter()
            .find(|action| action.id == approval_ref.action_id)
            .cloned()
            .ok_or_else(|| {
                MezError::invalid_state("blocked approval does not match an agent action")
            })?;
        let result_index = execution
            .action_results
            .iter()
            .position(|result| result.action_id == approval_ref.action_id)
            .ok_or_else(|| {
                MezError::invalid_state("blocked approval does not match an action result")
            })?;
        if execution.action_results[result_index].status != ActionStatus::Blocked {
            return Ok(None);
        }
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "approval {} decision=approved action={} kind={}",
                approval_id, action.id, approval.action_kind
            ),
        )?;
        let _ = self.agent_scheduler.resume_blocked(&turn.turn_id);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            "scheduler blocked -> running reason=approval_approved",
        )?;
        if turn.state == AgentTurnState::Blocked {
            self.agent_turn_ledger.resume_blocked_turn(&turn.turn_id)?;
            self.append_agent_trace_turn_transition(
                &turn,
                AgentTurnState::Blocked,
                AgentTurnState::Running,
                "approval_approved",
            )?;
        }
        match &action.payload {
            _ if local_action_plan(&action)?.is_some() => {
                let Some(plan) = local_action_plan(&action)? else {
                    return Err(MezError::invalid_state(
                        "approved shell-backed action has no local action plan",
                    ));
                };
                let command = plan.command.as_str();
                let subagent_scope = self.subagent_scope_declaration_for_turn(&turn);
                let permission_policy = self.permission_policy_for_turn(&turn);
                if permission_policy.approval_policy
                    != crate::permissions::ApprovalPolicy::FullAccess
                    && let Some(scope) = subagent_scope.as_ref()
                    && let Some(message) = scope.shell_command_violation(&plan.policy_command)?
                {
                    return Err(MezError::forbidden(message));
                }
                let path_scopes = if subagent_scope.is_some() {
                    None
                } else {
                    self.path_scopes_for_pane(&turn.pane_id)
                };
                match permission_policy.evaluate_shell_command_with_approvals_scoped(
                    &plan.policy_command,
                    &self.session_approvals,
                    path_scopes.as_ref(),
                ) {
                    RuleDecision::Allow => {}
                    RuleDecision::Prompt
                        if approval.state == crate::permissions::BlockedApprovalState::Approved
                            && approval.decision
                                == Some(crate::permissions::ApprovalDecision::Approve) => {}
                    RuleDecision::Prompt => {
                        return Err(MezError::conflict(
                            "approved shell action still requires approval",
                        ));
                    }
                    RuleDecision::Forbid => {
                        return Err(MezError::forbidden(
                            "approved shell action is forbidden by current permission policy",
                        ));
                    }
                }
                execution.action_results[result_index] = ActionResult::running(
                    &turn,
                    &action,
                    vec!["approved local action accepted for pane execution".to_string()],
                    Some(shell_command_structured_content_json(
                        &action,
                        false,
                        serde_json::json!({
                            "state": "approved",
                            "kind": action.action_type(),
                            "action_id": action.id.as_str(),
                            "command": runtime_agent_context_command(&action, command)
                        }),
                        &[],
                        serde_json::json!({"state":"pending_dispatch"}),
                    )?),
                );
                execution.terminal_state = AgentTurnState::Running;
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    &format!(
                        "action {} blocked -> running reason=approval_approved",
                        action.id
                    ),
                )?;
                self.dispatch_running_shell_actions_to_panes(&turn, &mut execution)?;
            }
            _ if network_action_plan(&action)?.is_some() => {
                let Some(plan) = network_action_plan(&action)? else {
                    return Err(MezError::invalid_state(
                        "approved network action has no network action plan",
                    ));
                };
                let permission_policy = self.permission_policy_for_turn(&turn);
                match permission_policy.evaluate_shell_command_with_approvals_scoped(
                    &plan.policy_command,
                    &self.session_approvals,
                    None,
                ) {
                    RuleDecision::Allow => {}
                    RuleDecision::Prompt
                        if approval.state == crate::permissions::BlockedApprovalState::Approved
                            && approval.decision
                                == Some(crate::permissions::ApprovalDecision::Approve) => {}
                    RuleDecision::Prompt => {
                        return Err(MezError::conflict(
                            "approved network action still requires approval",
                        ));
                    }
                    RuleDecision::Forbid => {
                        return Err(MezError::forbidden(
                            "approved network action is forbidden by current permission policy",
                        ));
                    }
                }
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    &format!(
                        "action {} blocked -> running reason=approval_approved_network",
                        action.id
                    ),
                )?;
                execution.action_results[result_index] =
                    self.execute_network_action_for_turn_blocking(&turn, &action)?;
            }
            AgentActionPayload::McpCall { .. } => {
                execution.action_results[result_index] =
                    self.execute_mcp_action_for_turn(&turn, &action, true)?;
            }
            AgentActionPayload::ConfigChange { .. } => {
                if !self
                    .append_agent_action_execution_text_to_terminal_buffer(&turn.pane_id, &action)?
                {
                    self.append_agent_status_text_to_terminal_buffer(
                        &turn.pane_id,
                        &format!(
                            "agent: {}",
                            runtime_agent_action_summary(&action)
                                .unwrap_or_else(|| "config change".to_string())
                        ),
                    )?;
                }
                let result = self.execute_config_change_action_for_turn(
                    &turn,
                    &action,
                    caller_client_id,
                    "approved",
                )?;
                if result.is_error {
                    self.append_agent_error_text_to_terminal_buffer(
                        &turn.pane_id,
                        &format!(
                            "agent: configuration change failed: {}",
                            result.content_text()
                        ),
                    )?;
                }
                execution.action_results[result_index] = result;
            }
            _ => return Ok(None),
        }
        self.blocked_agent_approval_refs.remove(approval_id);
        execution.terminal_state = runtime_agent_turn_state_from_action_results(
            &execution.action_results,
            execution.final_turn,
        );
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "approval_resume recalculated terminal_state={}",
                runtime_agent_turn_state_name(execution.terminal_state)
            ),
        )?;
        if execution.terminal_state == AgentTurnState::Running
            && runtime_execution_ready_for_provider_continuation(&execution)
        {
            let observed_result = execution.action_results[result_index].clone();
            self.agent_turn_contexts
                .get_mut(&turn.turn_id)
                .ok_or_else(|| {
                    MezError::invalid_state("running agent turn context is unavailable")
                })?
                .blocks
                .push(ContextBlock {
                    source: ContextSourceKind::ActionResult,
                    label: format!("action result {}", observed_result.action_id),
                    content: action_result_context_content(&observed_result),
                });
            self.pending_agent_provider_tasks
                .insert(turn.turn_id.clone());
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                "provider_task queued reason=approval_resume_ready_for_provider_continuation",
            )?;
        }
        if matches!(
            execution.terminal_state,
            AgentTurnState::Completed | AgentTurnState::Failed | AgentTurnState::Interrupted
        ) {
            let transcript_execution = execution.clone();
            let _ =
                self.persist_runtime_agent_turn_execution_transcript(&turn, &transcript_execution)?;
            self.emit_subagent_task_result_for_execution(&turn, &execution)?;
            self.complete_running_agent_turn_and_start_ready(
                &turn,
                execution.terminal_state,
                "approval_resume_settled",
            )?;
            return Ok(Some(1));
        }
        self.agent_turn_executions
            .insert(turn.turn_id.clone(), execution);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            "execution stored reason=approval_resume",
        )?;
        Ok(Some(1))
    }

    /// Runs the settle decided blocked agent action operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn settle_decided_blocked_agent_action(
        &mut self,
        approval_id: &str,
        approval: &BlockedApprovalRequest,
    ) -> Result<Option<usize>> {
        let Some(decision) = approval.decision else {
            return Ok(None);
        };
        if !matches!(
            decision,
            crate::permissions::ApprovalDecision::Disapprove
                | crate::permissions::ApprovalDecision::Redirect
        ) {
            return Ok(None);
        }
        let Some(approval_ref) = self.blocked_agent_approval_refs.get(approval_id).cloned() else {
            return Ok(None);
        };
        let mut execution = self
            .agent_turn_executions
            .get(&approval_ref.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("blocked agent execution is unavailable"))?;
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == approval_ref.turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        let batch = execution
            .response
            .action_batch
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("blocked execution has no action batch"))?;
        let action = batch
            .actions
            .iter()
            .find(|action| action.id == approval_ref.action_id)
            .cloned()
            .ok_or_else(|| {
                MezError::invalid_state("blocked approval does not match an agent action")
            })?;
        let result_index = execution
            .action_results
            .iter()
            .position(|result| result.action_id == approval_ref.action_id)
            .ok_or_else(|| {
                MezError::invalid_state("blocked approval does not match an action result")
            })?;
        if execution.action_results[result_index].status != ActionStatus::Blocked {
            return Ok(None);
        }

        match decision {
            crate::permissions::ApprovalDecision::Disapprove => {
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    &format!(
                        "approval {} decision=disapprove action={} kind={}",
                        approval_id, action.id, approval.action_kind
                    ),
                )?;
                let mut result = ActionResult::failed(
                    &turn,
                    &action,
                    ActionStatus::Denied,
                    "approval_disapproved",
                    format!(
                        "user denied {} {}",
                        approval.action_kind,
                        runtime_agent_terminal_preview(&approval.action_summary)
                    ),
                )?;
                result.structured_content_json = Some(format!(
                    r#"{{"approval":{{"state":"disapproved","kind":"{}","approval_id":"{}","action_id":"{}"}}}}"#,
                    json_escape(&approval.action_kind),
                    json_escape(approval_id),
                    json_escape(&action.id)
                ));
                execution.action_results[result_index] = result;
                execution.terminal_state = runtime_agent_turn_state_from_action_results(
                    &execution.action_results,
                    execution.final_turn,
                );
                let transcript_entries =
                    self.persist_runtime_agent_turn_execution_transcript(&turn, &execution)?;
                self.emit_subagent_task_result_for_execution(&turn, &execution)?;
                let _ = self.agent_scheduler.cancel(&turn.turn_id);
                self.blocked_agent_approval_refs.remove(approval_id);
                self.append_agent_error_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: approval {} denied: {} {}",
                        approval_id,
                        approval.action_kind,
                        runtime_agent_terminal_preview(&approval.action_summary)
                    ),
                )?;
                if self
                    .agent_shell_store
                    .get(&turn.pane_id)
                    .and_then(|session| session.running_turn_id.as_deref())
                    == Some(turn.turn_id.as_str())
                {
                    self.finish_agent_turn(&turn.pane_id, &turn.turn_id, AgentTurnState::Failed)?;
                } else {
                    self.finish_agent_turn_without_shell_session(&turn, AgentTurnState::Failed)?;
                }
                self.append_lifecycle_event(
                    EventKind::AgentStatus,
                    format!(
                        r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"failed","approval_id":"{}","decision":"disapprove","transcript_entries":{}}}"#,
                        json_escape(&turn.pane_id),
                        json_escape(&turn.turn_id),
                        json_escape(approval_id),
                        transcript_entries
                    ),
                )?;
                Ok(Some(1))
            }
            crate::permissions::ApprovalDecision::Redirect => {
                let instruction = approval.redirect_instruction.as_deref().ok_or_else(|| {
                    MezError::invalid_state("redirect approval has no instruction")
                })?;
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    &format!(
                        "approval {} decision=redirect action={} kind={}",
                        approval_id, action.id, approval.action_kind
                    ),
                )?;
                let _ = self.agent_scheduler.resume_blocked(&turn.turn_id);
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    "scheduler blocked -> running reason=approval_redirected",
                )?;
                if turn.state == AgentTurnState::Blocked {
                    self.agent_turn_ledger.resume_blocked_turn(&turn.turn_id)?;
                    self.append_agent_trace_turn_transition(
                        &turn,
                        AgentTurnState::Blocked,
                        AgentTurnState::Running,
                        "approval_redirected",
                    )?;
                }
                execution.action_results[result_index] = ActionResult::succeeded(
                    &turn,
                    &action,
                    vec![format!("user redirected action: {instruction}")],
                    Some(format!(
                        r#"{{"approval":{{"state":"redirected","kind":"{}","approval_id":"{}","action_id":"{}"}},"redirect_instruction":"{}"}}"#,
                        json_escape(&approval.action_kind),
                        json_escape(approval_id),
                        json_escape(&action.id),
                        json_escape(instruction)
                    )),
                );
                execution.terminal_state = runtime_agent_turn_state_from_action_results(
                    &execution.action_results,
                    execution.final_turn,
                );
                self.blocked_agent_approval_refs.remove(approval_id);
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!(
                        "agent: approval {} redirected: {}",
                        approval_id,
                        runtime_agent_terminal_preview(instruction)
                    ),
                )?;
                if execution.terminal_state == AgentTurnState::Running
                    && runtime_execution_ready_for_provider_continuation(&execution)
                {
                    let observed_result = execution.action_results[result_index].clone();
                    self.agent_turn_contexts
                        .get_mut(&turn.turn_id)
                        .ok_or_else(|| {
                            MezError::invalid_state("running agent turn context is unavailable")
                        })?
                        .blocks
                        .push(ContextBlock {
                            source: ContextSourceKind::ActionResult,
                            label: format!("action result {}", observed_result.action_id),
                            content: action_result_context_content(&observed_result),
                        });
                    self.pending_agent_provider_tasks
                        .insert(turn.turn_id.clone());
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        "provider_task queued reason=approval_redirect_ready_for_provider_continuation",
                    )?;
                }
                if matches!(
                    execution.terminal_state,
                    AgentTurnState::Completed
                        | AgentTurnState::Failed
                        | AgentTurnState::Interrupted
                ) {
                    let transcript_execution = execution.clone();
                    let _ = self.persist_runtime_agent_turn_execution_transcript(
                        &turn,
                        &transcript_execution,
                    )?;
                    self.emit_subagent_task_result_for_execution(&turn, &execution)?;
                    self.complete_running_agent_turn_and_start_ready(
                        &turn,
                        execution.terminal_state,
                        "approval_redirect_settled",
                    )?;
                    return Ok(Some(1));
                }
                self.agent_turn_executions
                    .insert(turn.turn_id.clone(), execution);
                Ok(Some(1))
            }
            crate::permissions::ApprovalDecision::Approve => Ok(None),
        }
    }
}

/// Builds the message-delivered final output for a subagent task result.
///
/// Provider raw text often contains the MAAP JSON envelope rather than useful
/// user-facing text. Parent agents should receive conversational `say` text on
/// success, concrete action diagnostics on action failure, and provider error
/// text when the failure happened before any action result existed.
fn subagent_task_output_for_execution(execution: &AgentTurnExecution) -> String {
    let mut lines = Vec::new();
    for result in &execution.action_results {
        if let Some(error) = &result.error {
            lines.push(format!(
                "{} {} {}: {}",
                result.action_type,
                result.action_id,
                runtime_action_status_name(result.status),
                error.message
            ));
            lines.extend(subagent_failed_action_diagnostic_lines(result));
            continue;
        }
        if result.action_type == "say" {
            lines.extend(
                result
                    .content_texts()
                    .into_iter()
                    .filter(|text| !text.trim().is_empty()),
            );
        }
    }

    if !lines.is_empty() {
        return lines.join("\n");
    }
    if execution.terminal_state == AgentTurnState::Completed {
        "completed without user-facing response".to_string()
    } else if !execution.response.raw_text.trim().is_empty() {
        execution.response.raw_text.trim().to_string()
    } else {
        "failed without action diagnostics".to_string()
    }
}

/// Returns bounded diagnostic lines for a failed subagent action result.
///
/// Parent agents rely on final `task_result` payloads to understand why a child
/// failed. Shell-backed semantic actions often store their useful stderr/stdout
/// preview in structured content rather than in plain content, so this extracts
/// both locations without exposing unbounded terminal output.
fn subagent_failed_action_diagnostic_lines(result: &ActionResult) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(command) = subagent_action_result_structured_string(result, &["command"]) {
        lines.push(format!(
            "{} {} command: {}",
            result.action_type,
            result.action_id,
            subagent_bounded_diagnostic_text(command.trim())
        ));
    }
    let output = result
        .content_texts()
        .into_iter()
        .find(|text| !text.trim().is_empty())
        .or_else(|| {
            subagent_action_result_structured_string(
                result,
                &["terminal_observation", "combined_output_preview"],
            )
            .filter(|text| !text.trim().is_empty())
        });
    if let Some(output) = output {
        lines.push(format!(
            "{} {} output:\n{}",
            result.action_type,
            result.action_id,
            subagent_bounded_diagnostic_text(output.trim())
        ));
    }
    lines
}

/// Extracts a string from nested action-result structured content.
fn subagent_action_result_structured_string(
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

/// Bounds diagnostic text included in subagent task results.
fn subagent_bounded_diagnostic_text(value: &str) -> String {
    const MAX_SUBAGENT_DIAGNOSTIC_CHARS: usize = 4_000;
    let mut output = value
        .chars()
        .take(MAX_SUBAGENT_DIAGNOSTIC_CHARS)
        .collect::<String>();
    if value.chars().count() > MAX_SUBAGENT_DIAGNOSTIC_CHARS {
        output.push_str("\n[truncated]");
    }
    output
}

/// Runs the runtime provider event error kind operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_provider_event_error_kind(kind: &str) -> crate::error::MezErrorKind {
    match kind {
        "invalid_args" | "InvalidArgs" => crate::error::MezErrorKind::InvalidArgs,
        "config" | "Config" => crate::error::MezErrorKind::Config,
        "io" | "Io" => crate::error::MezErrorKind::Io,
        "conflict" | "Conflict" => crate::error::MezErrorKind::Conflict,
        "not_found" | "NotFound" => crate::error::MezErrorKind::NotFound,
        "forbidden" | "Forbidden" => crate::error::MezErrorKind::Forbidden,
        "not_implemented" | "NotImplemented" => crate::error::MezErrorKind::NotImplemented,
        _ => crate::error::MezErrorKind::InvalidState,
    }
}

/// Runs the runtime provider event error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_provider_event_error(
    kind: &str,
    message: &str,
    provider_failure_json: Option<&str>,
    provider_raw_text: Option<&str>,
) -> MezError {
    let mut error = MezError::new(runtime_provider_event_error_kind(kind), message);
    if let Some(raw_text) = provider_raw_text {
        error = error.with_provider_raw_text(raw_text.to_string());
    }
    if let Some(failure_json) = provider_failure_json {
        error = error.with_provider_failure_json(failure_json.to_string());
    }
    error
}

/// Runs the runtime task state suffix operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn runtime_task_state_suffix(state: TaskState) -> &'static str {
    match state {
        TaskState::Queued => "queued",
        TaskState::Running => "running",
        TaskState::Blocked => "blocked",
        TaskState::Succeeded => "succeeded",
        TaskState::Failed => "failed",
        TaskState::Cancelled => "cancelled",
    }
}

/// Derives the pane identity encoded by runtime-created agent ids.
fn runtime_agent_pane_id(agent_id: &str) -> Option<PaneId> {
    agent_id
        .strip_prefix("agent-")
        .and_then(|pane_id| PaneId::parse('%', pane_id.to_string()))
}
