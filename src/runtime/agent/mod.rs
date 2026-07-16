//! Runtime Agent implementation.
//!
//! This module owns the runtime agent boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::agent_state::RuntimeAgentProviderClaim;
#[cfg(test)]
use super::runtime_execute_auto_sizing_with_provider;
use super::service_state::{
    RuntimeAgentPatchRecord, RuntimeAgentTurnSteering, RuntimeApplyPatchBatchState,
};
use super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentId, AgentShellSession,
    AgentShellVisibility, AgentTurnExecution, AgentTurnRecord, AgentTurnState, AuditActor,
    AuditRecord, BTreeMap, BTreeSet, BlockedAgentApprovalRef, BlockedApprovalRequest, ContextBlock,
    ContextSourceKind, DEFAULT_COMMAND_SHELL_CLASSIFICATION, Envelope, EventKind, HookEvent,
    JoinedSubagentDependency, McpToolCallRequest, MezError, ModelProfile, ModelResponse, PaneId,
    PaneReadinessState, PathBuf, PathScopes, PendingFocusedShellHookContinuation, PermissionPolicy,
    ReadinessOverrideRevocation, Recipient, ReqwestProviderHttpTransport, Result, RuleDecision,
    RunningShellTransactionKind, RunningShellTransactionRef, RuntimeAgentCopyOutput,
    RuntimeAgentLoopTurnKind, RuntimeAgentProviderDispatch, RuntimeAgentProviderDispatchProvider,
    RuntimeAgentProviderTask, RuntimeAutoSizingDispatch, RuntimeAutoSizingTargetProfile,
    RuntimeHookPipelineBlock, RuntimeHookPipelineDecision, RuntimeMcpActionExecutor,
    RuntimeProviderConfig, RuntimeSessionService, RuntimeShellTransactionActionFailure,
    RuntimeSideEffect, SenderIdentity, ShellTransaction, ShellTransactionOutputTransport,
    SubagentScopeDeclaration, SubagentSpawnRequest, SubagentWaitPolicy, TaskResultPayload,
    TaskState, TaskStatusPayload, TranscriptEntry, TranscriptRole, action_result_context_content,
    assemble_model_request, compact_model_context_for_budget_with_retained_tail_percent,
    current_unix_millis, current_unix_seconds, decode_shell_output_transport_with_diagnostics,
    discover_project_root, exact_command_sha256, execute_mcp_action_through_runtime,
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
    runtime_subagent_placement_mode, runtime_subagent_spawn_request,
    shell_command_structured_content_json, transcript_entries_for_execution,
    validate_mmp_payload_metadata,
};
#[cfg(test)]
use crate::agent::actions::AgentTurnRunner;
#[cfg(test)]
use crate::agent::provider::ModelProvider;
#[cfg(test)]
use crate::agent::provider::provider_error_retry_class;
use crate::agent::provider::{
    deepseek_chat_completions_provider_from_auth_store_with_provider_options,
    openai_compatible_provider_from_auth_store_with_provider_options,
    openai_responses_provider_from_auth_store_with_provider_options,
};
use crate::config::{
    ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation, ConfigMutationValue,
    ConfigPaths, ConfigScope,
};
#[cfg(test)]
use mez_agent::AgentTurnLedger;
use mez_agent::resolve_provider_api;
use mez_agent::semantic_patch_planning::{
    ApplyPatchTransactionPhase, apply_patch_error_plan, apply_patch_read_plan_for_paths,
    apply_patch_touched_paths, apply_patch_transaction_phase,
    apply_patch_write_plan_from_read_output, apply_patch_write_plan_from_read_outputs,
};
use mez_agent::{
    DEFAULT_PROVIDER_TIMEOUT_MS, MaapBatch, ModelTokenUsage, ModelTokenUsageKey,
    ProviderApiCompatibility, ProviderQuotaUsage, SayStatus, append_mcp_context,
    assistant_context_content_for_execution, invoked_mcp_tools_for_context,
    set_project_guidance_context,
};
use mez_mux::command::CommandInvocation;

mod approvals;
mod audit;
mod bookkeeping;
mod config_change;
mod failures;
mod issues;
mod lifecycle;
mod macros;
mod mcp_network;
mod memory;
mod messages;
mod outcome;
mod presentation;
mod provider_context;
mod provider_events;
mod provider_execution;
mod provider_tasks;
mod shell_dispatch;
mod shell_state;
mod skills;
mod subagents;
mod trace;

use mez_agent::outcome::{
    RuntimeSkillActionContext, runtime_action_result_error_code,
    runtime_action_result_has_error_code, runtime_action_result_is_aggregated_loop_guard_failure,
    runtime_action_result_is_feedback_candidate, runtime_action_result_is_terminal_failure,
    runtime_action_status_name, runtime_action_type_is_shell_backed,
    runtime_execution_can_feed_failure_to_model,
    runtime_execution_uses_unbounded_apply_patch_recovery, runtime_failure_feedback_attempt_keys,
    runtime_failure_feedback_evidence_guidance, runtime_failure_feedback_loop_guard_aggregate_note,
    runtime_failure_feedback_repeat_guidance, runtime_failure_feedback_specific_guidance,
    runtime_failure_feedback_status_line, runtime_loop_guard_failure_label,
    runtime_loop_guard_failure_summary_line, runtime_provider_audit_error_message,
    runtime_skill_action_context_from_blocks, runtime_unrecovered_action_failure_output,
    runtime_unrecovered_failure_output_lines, runtime_unrecovered_failure_reason,
    runtime_validate_provider_completion_execution, runtime_validate_provider_completion_identity,
};
use mez_agent::progress::{
    PROGRESS_SAY_LEDGER_LABEL as RUNTIME_PROGRESS_SAY_LEDGER_LABEL,
    RATIONALE_LEDGER_LABEL as RUNTIME_RATIONALE_LEDGER_LABEL,
    merge_progress_say_entries as runtime_merge_progress_say_entries,
    merge_rationale_entries as runtime_merge_rationale_entries,
    normalize_rationale_entry as runtime_normalize_rationale_entry,
    progress_say_entries_for_execution as runtime_progress_say_entries_for_execution,
    progress_say_entries_from_ledger as runtime_progress_say_entries_from_ledger,
    progress_say_ledger_content as runtime_progress_say_ledger_content,
    rationale_entries_for_execution as runtime_rationale_entries_for_execution,
    rationale_entries_from_ledger as runtime_rationale_entries_from_ledger,
    rationale_entry_repeats_existing as runtime_rationale_entry_repeats_existing,
    rationale_ledger_content as runtime_rationale_ledger_content,
};
use mez_agent::subagent_task_output_for_execution;
use outcome::{
    normalize_agent_user_visible_text, runtime_action_result_is_suppressed_duplicate_file_mutation,
    runtime_action_supports_auto_allow, runtime_agent_action_error_suffix,
    runtime_agent_action_has_runtime_visible_effect, runtime_agent_action_outcome_line,
    runtime_agent_action_rationale_repeats_visible_batch_text,
    runtime_agent_action_rationale_repeats_visible_summary,
    runtime_agent_action_rejects_duplicate_success, runtime_agent_action_summary,
    runtime_agent_batch_rationale_repeats_visible_batch_text,
    runtime_agent_batch_visible_action_texts, runtime_agent_context_command,
    runtime_agent_execution_failure_error, runtime_agent_finished_footer_line,
    runtime_agent_pending_approval_log_line, runtime_agent_shell_status,
    runtime_agent_terminal_preview, runtime_agent_turn_steering_context_content,
};
use provider_events::{runtime_provider_event_error, runtime_task_state_suffix};
use subagents::runtime_agent_pane_id;
use trace::{
    runtime_maap_message_content_type, runtime_spawn_json_agent_and_turn,
    runtime_subagent_display_label, runtime_subagent_result_status_label,
};

// Agent turn execution, provider polling, action dispatch, and approvals.

/// Defines the RUNTIME AGENT DEFAULT SHELL ACTION TIMEOUT MS const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const RUNTIME_AGENT_TURN_TIMEOUT_MS: u64 = 30 * 60 * 1000;
/// Maximum in-process provider context-limit retries for test providers.
#[cfg(test)]
const RUNTIME_PROVIDER_CONTEXT_LIMIT_RETRY_LIMIT: u32 = 3;
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

/// Returns a last-request context snapshot when the denominator is known.
fn runtime_agent_provider_context_usage_snapshot(
    profile: &ModelProfile,
    usage: ModelTokenUsage,
) -> Option<mez_agent::AgentContextUsageSnapshot> {
    let context_window_tokens = profile
        .known_context_window_tokens()
        .and_then(|tokens| u64::try_from(tokens).ok())
        .filter(|tokens| *tokens > 0)?;
    if usage.input_tokens == 0 {
        return None;
    }
    Some(mez_agent::AgentContextUsageSnapshot {
        input_tokens: usage.input_tokens,
        context_window_tokens,
        cached_input_tokens: usage.cached_input_tokens,
    })
}

/// Formats one last-request context snapshot for pane status.
///
/// The display is a bounded status indicator, so accepted provider responses
/// whose token count exceeds the configured profile window saturate at `100%`
/// instead of rendering impossible percentages above the full window.
pub(crate) fn runtime_agent_provider_context_usage_display(
    snapshot: mez_agent::AgentContextUsageSnapshot,
) -> Option<String> {
    if snapshot.input_tokens == 0 || snapshot.context_window_tokens == 0 {
        return None;
    }
    let budget_tokens = snapshot.context_window_tokens;
    let percentage = snapshot
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
            lines.extend(runtime_agent_failed_execution_prompt_display_lines(
                execution,
            ));
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

/// Returns prompt display lines for a failed provider execution.
fn runtime_agent_failed_execution_prompt_display_lines(
    execution: &AgentTurnExecution,
) -> Vec<String> {
    let failure = runtime_agent_execution_failure_error(execution);
    let mut lines = vec![format!("agent: failure: {}", failure.message())];
    lines.extend(
        execution
            .response
            .raw_text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter(|line| !runtime_agent_failed_execution_raw_text_is_placeholder(execution, line))
            .take(200)
            .map(ToOwned::to_owned),
    );
    lines
}

/// Returns true when provider raw text is only an internal execution marker.
fn runtime_agent_failed_execution_raw_text_is_placeholder(
    execution: &AgentTurnExecution,
    line: &str,
) -> bool {
    line == "executing"
        && (execution.response.action_batch.is_some()
            || !execution.response.provider_transcript_events.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies failed DeepSeek tool-call turns display the action failure
    /// diagnostic instead of the provider's `executing` placeholder.
    ///
    /// DeepSeek responses with only tool calls use `executing` as local
    /// fallback raw text. If a later action result fails, the prompt footer must
    /// show the failed action diagnostic so users can see why the turn stopped.
    #[test]
    fn failed_deepseek_execution_prompt_shows_action_error_not_executing_placeholder() {
        let execution = AgentTurnExecution {
            request: mez_agent::ModelRequest {
                provider: "deepseek".to_string(),
                model: "deepseek-v4-pro".to_string(),
                reasoning_effort: Some("high".to_string()),
                thinking_enabled: None,
                latency_preference: None,
                prompt_cache_retention: None,
                max_output_tokens: None,
                temperature: None,
                stop: None,
                prompt_cache_session_id: None,
                prompt_cache_lineage_id: None,
                turn_id: "turn-2".to_string(),
                agent_id: "agent-1".to_string(),
                available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
                issue_actions_enabled: true,
                interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
                allowed_actions: mez_agent::AllowedActionSet::action_execution_base(),
                messages: Vec::new(),
            },
            response: ModelResponse {
                provider: "deepseek".to_string(),
                model: "deepseek-v4-pro".to_string(),
                raw_text: "executing".to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch: Some(MaapBatch {
                    protocol: "maap/1".to_string(),
                    rationale: "inspect the target files".to_string(),
                    thought: None,
                    turn_id: "turn-2".to_string(),
                    agent_id: "agent-1".to_string(),
                    actions: Vec::new(),
                    final_turn: false,
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: vec![ActionResult {
                protocol: "maap/1".to_string(),
                turn_id: "turn-2".to_string(),
                agent_id: "agent-1".to_string(),
                action_id: "a1".to_string(),
                action_type: "shell_command",
                status: ActionStatus::Failed,
                content: vec![mez_agent::ActionContentBlock::text("shell command failed")],
                structured_content_json: None,
                is_error: true,
                error: Some(mez_agent::ActionError {
                    code: "shell_failed".to_string(),
                    message: "command exited with status 1".to_string(),
                    data_json: None,
                }),
            }],
            final_turn: false,
            terminal_state: AgentTurnState::Failed,
        };

        let lines =
            runtime_agent_execution_prompt_display_lines("turn-2", "deepseek", &execution, 0, 5);

        assert!(lines.contains(&"agent: turn turn-2 failed".to_string()));
        assert!(lines.contains(&"agent: provider deepseek responded".to_string()));
        assert!(lines.contains(&"agent: recorded 5 transcript entries".to_string()));
        assert!(lines.contains(
            &"agent: failure: agent action shell_failed: command exited with status 1".to_string(),
        ));
        assert!(!lines.iter().any(|line| line == "executing"));
    }

    /// Verifies failed macro-judge completions display the structured runtime
    /// application error instead of the generic missing-MAAP diagnostic.
    ///
    /// Macro-judge provider responses are intentionally JSON-only and do not
    /// contain MAAP batches. When applying that JSON fails, the failed-turn
    /// prompt must show the embedded provider error so users see the judge
    /// validation problem that actually stopped the macro.
    #[test]
    fn failed_macro_judge_execution_prompt_shows_provider_error_not_missing_batch() {
        let execution = AgentTurnExecution {
            request: mez_agent::ModelRequest {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                reasoning_effort: None,
                thinking_enabled: None,
                latency_preference: None,
                prompt_cache_retention: None,
                max_output_tokens: None,
                temperature: None,
                stop: None,
                prompt_cache_session_id: None,
                prompt_cache_lineage_id: None,
                turn_id: "turn-1".to_string(),
                agent_id: "agent-%1".to_string(),
                available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
                issue_actions_enabled: false,
                interaction_kind: mez_agent::ModelInteractionKind::MacroJudge,
                allowed_actions: mez_agent::AllowedActionSet::for_capability(
                    mez_agent::AgentCapability::RespondOnly,
                ),
                messages: Vec::new(),
            },
            response: ModelResponse {
                provider: "runtime-batch".to_string(),
                model: "test".to_string(),
                raw_text: "{\"outcome\":\"finish_success\",\"step_success\":true,\"rationale\":\"done\",\"adapted_prompt\":null,\"user_message\":null}\nprovider_error: InvalidArgs: macro judge cannot finish before the final step"
                    .to_string(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Vec::new(),
                action_batch: None,
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: std::collections::BTreeMap::new(),
            action_results: Vec::new(),
            final_turn: true,
            terminal_state: AgentTurnState::Failed,
        };

        let lines = runtime_agent_execution_prompt_display_lines(
            "turn-1",
            "runtime-batch",
            &execution,
            0,
            3,
        );

        assert!(
            lines.contains(
                &"agent: failure: InvalidArgs: macro judge cannot finish before the final step"
                    .to_string(),
            )
        );
        assert!(
            lines.iter().all(|line| {
                !line.contains("model response did not contain a MAAP action batch")
            })
        );
    }
}
