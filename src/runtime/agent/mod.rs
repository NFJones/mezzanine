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
    McpToolCallRequest, MezError, ModelProfile, ModelResponse, ModelTokenUsage, PaneId,
    PaneReadinessState, PathBuf, PathScopes, PendingFocusedShellHookContinuation, PermissionPolicy,
    ProviderQuotaUsage, ReadinessOverrideRevocation, Recipient, ReqwestProviderHttpTransport,
    Result, RuleDecision, RunningShellTransactionKind, RunningShellTransactionRef,
    RuntimeAgentCopyOutput, RuntimeAgentProviderDispatch, RuntimeAgentProviderDispatchProvider,
    RuntimeAgentProviderTask, RuntimeAutoSizingDispatch, RuntimeAutoSizingTargetProfile,
    RuntimeHookPipelineBlock, RuntimeHookPipelineDecision, RuntimeMcpActionExecutor,
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
    ApplyPatchTransactionPhase, MaapBatch, apply_patch_error_plan, apply_patch_transaction_phase,
    apply_patch_write_plan_from_read_output,
    deepseek_provider_from_auth_store_with_provider_options,
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

mod config_change;
mod lifecycle;
mod outcome;
mod progress;
mod provider_context;
mod provider_events;
mod subagent_output;
mod trace;

use outcome::*;
pub(in crate::runtime) use outcome::{
    runtime_unrecovered_failure_output_lines, runtime_validate_provider_completion_execution,
};
use progress::*;
use provider_events::*;
use subagent_output::*;
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

impl RuntimeSessionService {
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

/// Derives the pane identity encoded by runtime-created agent ids.
fn runtime_agent_pane_id(agent_id: &str) -> Option<PaneId> {
    agent_id
        .strip_prefix("agent-")
        .and_then(|pane_id| PaneId::parse('%', pane_id.to_string()))
}
