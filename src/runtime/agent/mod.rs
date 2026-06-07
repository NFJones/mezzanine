//! Runtime Agent implementation.
//!
//! This module owns the runtime agent boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

#[cfg(test)]
use super::runtime_execute_auto_sizing_with_provider;
use super::types::{RuntimeAgentPatchRecord, RuntimeAgentProviderClaim, RuntimeAgentTurnSteering};
use super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentId, AgentShellSession,
    AgentShellVisibility, AgentTurnExecution, AgentTurnRecord, AgentTurnState, AuditActor,
    AuditRecord, BTreeMap, BTreeSet, BlockedAgentApprovalRef, BlockedApprovalRequest, ContextBlock,
    ContextSourceKind, DEFAULT_COMMAND_SHELL_CLASSIFICATION, DEFAULT_PROVIDER_TIMEOUT_MS,
    DeferredAgentTranscriptWrite, Envelope, EventKind, HookEvent, JoinedSubagentDependency,
    McpToolCallRequest, MezError, ModelProfile, ModelResponse, ModelTokenUsage, ModelTokenUsageKey,
    PaneId, PaneReadinessState, PathBuf, PathScopes, PendingFocusedShellHookContinuation,
    PermissionPolicy, ProviderQuotaUsage, ReadinessOverrideRevocation, Recipient,
    ReqwestProviderHttpTransport, Result, RuleDecision, RunningShellTransactionKind,
    RunningShellTransactionRef, RuntimeAgentCopyOutput, RuntimeAgentLoopTurnKind,
    RuntimeAgentProviderDispatch, RuntimeAgentProviderDispatchProvider, RuntimeAgentProviderTask,
    RuntimeAutoSizingDispatch, RuntimeAutoSizingTargetProfile, RuntimeHookPipelineBlock,
    RuntimeHookPipelineDecision, RuntimeMcpActionExecutor, RuntimeProviderConfig,
    RuntimeSessionService, RuntimeShellTransactionActionFailure, SenderIdentity, ShellTransaction,
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
    ApplyPatchTransactionPhase, MaapBatch, ProviderApiCompatibility, apply_patch_error_plan,
    apply_patch_transaction_phase, apply_patch_write_plan_from_read_output,
    deepseek_chat_completions_provider_from_auth_store_with_provider_options,
    effective_provider_api, openai_compatible_provider_from_auth_store_with_provider_options,
    openai_responses_provider_from_auth_store_with_provider_options,
};
#[cfg(test)]
use crate::agent::{ProviderErrorRetryClass, provider_error_retry_class};
use crate::agent::{SayStatus, assistant_context_content_for_execution};
use crate::command::CommandInvocation;
use crate::config::{
    ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation, ConfigMutationValue,
    ConfigPaths, ConfigScope,
};

mod approvals;
mod audit;
mod bookkeeping;
mod config_change;
mod failures;
mod lifecycle;
mod mcp_network;
mod memory;
mod messages;
mod outcome;
mod presentation;
mod progress;
mod provider_context;
mod provider_events;
mod provider_execution;
mod provider_tasks;
mod shell_dispatch;
mod shell_state;
mod skills;
mod subagent_output;
mod subagents;
mod trace;

use outcome::*;
pub(in crate::runtime) use outcome::{
    runtime_unrecovered_failure_output_lines, runtime_validate_provider_completion_execution,
};
use progress::*;
use provider_events::*;
use subagent_output::*;
use subagents::*;
use trace::*;

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
) -> Option<crate::agent::AgentContextUsageSnapshot> {
    let context_window_tokens = profile
        .known_context_window_tokens()
        .and_then(|tokens| u64::try_from(tokens).ok())
        .filter(|tokens| *tokens > 0)?;
    if usage.input_tokens == 0 {
        return None;
    }
    Some(crate::agent::AgentContextUsageSnapshot {
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
    snapshot: crate::agent::AgentContextUsageSnapshot,
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
            request: crate::agent::ModelRequest {
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
                interaction_kind: crate::agent::ModelInteractionKind::ActionExecution,
                allowed_actions: crate::agent::AllowedActionSet::action_execution_base(),
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
                content: vec![crate::agent::ActionContentBlock::text(
                    "shell command failed",
                )],
                structured_content_json: None,
                is_error: true,
                error: Some(crate::agent::ActionError {
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
}
