//! Runtime Agent implementation.
//!
//! This module owns the runtime agent boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::agent_state::RuntimeAgentProviderClaim;
use super::commands::RuntimeModelCatalog;
#[cfg(test)]
use super::runtime_execute_auto_sizing_with_provider;
use super::service_state::{RuntimeAgentPatchRecord, RuntimeApplyPatchBatchState};
use super::{
    ActionResult, ActionStatus, AgentAction, AgentActionPayload, AgentId, AgentScheduler,
    AgentShellSession, AgentShellVisibility, AgentTurnExecution, AgentTurnRecord, AgentTurnState,
    AuditActor, AuditRecord, BTreeMap, BTreeSet, BlockedAgentApprovalRef, BlockedApprovalRequest,
    ContextBlock, ContextSourceKind, DEFAULT_COMMAND_SHELL_CLASSIFICATION,
    DEFAULT_MAX_ROOT_SUBAGENTS, DEFAULT_MAX_SUBAGENT_DEPTH, DEFAULT_MAX_SUBAGENT_PANES_PER_WINDOW,
    DEFAULT_MAX_SUBAGENTS_PER_SUBAGENT, DEFAULT_SUBAGENT_WAIT_POLICY, Envelope, EventKind,
    HookEvent, JoinedSubagentDependency, McpToolCallRequest, MezError, ModelProfile, ModelResponse,
    PaneId, PaneReadinessState, PathBuf, PathScopes, PendingFocusedShellHookContinuation,
    PermissionPolicy, ReadinessOverrideRevocation, Recipient, ReqwestProviderHttpTransport, Result,
    RuleDecision, RunningShellTransactionKind, RunningShellTransactionRef,
    RuntimeAgentCompactionTask, RuntimeAgentCopyOutput, RuntimeAgentLoopState,
    RuntimeAgentLoopTurn, RuntimeAgentLoopTurnKind, RuntimeAgentModifiedFileSummary,
    RuntimeAgentPreShellHookCompletion, RuntimeAgentProviderDispatch,
    RuntimeAgentProviderDispatchProvider, RuntimeAgentProviderTask, RuntimeAgentRememberTask,
    RuntimeAutoSizingConfig, RuntimeAutoSizingDispatch, RuntimeAutoSizingTargetProfile,
    RuntimeHookPipelineBlock, RuntimeHookPipelineDecision, RuntimeMcpActionExecutor,
    RuntimeProviderConfig, RuntimeSessionService, RuntimeShellTransactionActionFailure,
    RuntimeSideEffect, ScheduledWork, SenderIdentity, ShellTransaction,
    ShellTransactionOutputTransport, SubagentScopeDeclaration, SubagentSpawnRequest,
    SubagentWaitPolicy, TaskResultPayload, TaskState, TaskStatusPayload, TranscriptEntry,
    TranscriptRole, action_result_context_content, assemble_model_request,
    compact_model_context_for_budget_with_retained_tail_percent, current_unix_millis,
    current_unix_seconds, decode_shell_output_transport_with_diagnostics, discover_project_root,
    exact_command_sha256, execute_mcp_action_through_runtime,
    execute_mcp_action_through_runtime_async, execute_network_action_with_transport_async,
    json_escape, local_action_plan, network_action_plan, next_transcript_sequence,
    runtime_agent_turn_duration_display, runtime_agent_turn_start_hook_payload,
    runtime_agent_turn_state_from_action_results, runtime_agent_turn_state_name,
    runtime_apply_auto_sizing_execution_profile, runtime_apply_persisted_config_mutation_batch,
    runtime_auto_sizing_reasoning_levels_for_profile, runtime_blocked_approval_request,
    runtime_cooperation_mode, runtime_cooperation_mode_name,
    runtime_execution_ready_for_provider_continuation, runtime_hook_event_name,
    runtime_marker_for_action, runtime_mcp_error_code, runtime_message_recipient,
    runtime_mezzanine_error_code, runtime_pane_by_id, runtime_pane_readiness_state_name,
    runtime_path_under_project_root, runtime_permission_preset_name,
    runtime_permission_request_hook_payload, runtime_post_mcp_hook_payload,
    runtime_pre_mcp_hook_payload, runtime_pre_shell_hook_payload, runtime_set_theme_command,
    runtime_subagent_placement_mode, runtime_subagent_spawn_request,
    transcript_entries_for_execution, validate_mmp_payload_metadata,
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
    AgentNetworkActionHistory, AgentShellDispatchHistory, AgentTurnSteering,
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

use mez_agent::messaging::task_state_name as runtime_task_state_suffix;

/// Owns application-side agent execution state and lifecycle invariants.
///
/// The component begins with visible agent-subshell lifecycle state and grows
/// by coherent turn, provider, scheduling, and subagent slices. Its fields are
/// private so neighboring runtime subsystems use typed agent operations.
#[derive(Debug, Default)]
pub(in crate::runtime) struct RuntimeAgentComponent {
    /// Fair scheduling state for queued, running, and blocked agent turns.
    agent_scheduler: AgentScheduler,
    /// Panes whose visible agent session is scoped to a child shell.
    agent_subshell_panes: BTreeSet<String>,
    /// Interrupted subshells that must exit with a line-oriented command.
    agent_subshell_command_exit_panes: BTreeSet<String>,
    /// Bounded hidden diagnostic lines retained by pane.
    agent_pane_trace_logs: BTreeMap<String, Vec<String>>,
    /// Exact apply-patch attempts retained by agent session id.
    agent_session_patch_records: BTreeMap<String, Vec<RuntimeAgentPatchRecord>>,
    /// Latest model-authored copy output retained by pane.
    agent_copy_outputs: BTreeMap<String, RuntimeAgentCopyOutput>,
    /// File modification summaries retained by pane and display path.
    agent_modified_files: BTreeMap<String, BTreeMap<String, RuntimeAgentModifiedFileSummary>>,
    /// Panes with explicit planning-mode presentation enabled.
    agent_planning_modes: BTreeSet<String>,
    /// Pane-local response style selections.
    agent_response_styles: BTreeMap<String, String>,
    /// Configured default provider-routing state.
    agent_routing: bool,
    /// Explicit pane-local provider-routing overrides.
    agent_routing_overrides: BTreeMap<String, bool>,
    /// Percent of raw context retained after compaction.
    agent_compaction_raw_retention_percent: usize,
    /// Default model and reasoning auto-sizing policy.
    agent_auto_sizing: RuntimeAutoSizingConfig,
    /// Pane-local auto-sizing policy overrides.
    agent_auto_sizing_overrides: BTreeMap<String, RuntimeAutoSizingConfig>,
    /// Maximum iterations accepted by one loop controller.
    agent_loop_limit: usize,
    /// Active loop controller state keyed by pane id.
    agent_loops_by_pane: BTreeMap<String, RuntimeAgentLoopState>,
    /// Loop-owned turn metadata keyed by turn id.
    agent_loop_turns: BTreeMap<String, RuntimeAgentLoopTurn>,
    /// Per-signature correction retry limit for failed model actions.
    agent_action_failure_retry_limit: usize,
    /// Successful shell streak that activates implementation-pressure hints.
    agent_implementation_pressure_after_shell_actions: usize,
    /// Per-failure-signature correction attempts keyed by turn/signature.
    agent_turn_failure_feedback_attempts: BTreeMap<String, usize>,
    /// Per-turn successful shell dispatch history.
    agent_turn_shell_dispatch_history: BTreeMap<String, AgentShellDispatchHistory>,
    /// Per-turn network action history.
    agent_turn_network_action_history: BTreeMap<String, AgentNetworkActionHistory>,
    /// Pre-shell hooks already completed for an action.
    agent_pre_shell_hook_completions: BTreeSet<RuntimeAgentPreShellHookCompletion>,
    /// Effective provider model profile retained for each active turn.
    agent_turn_model_profiles: BTreeMap<String, ModelProfile>,
    /// Provider retry attempt number retained by turn id.
    agent_provider_retry_attempts: BTreeMap<String, u32>,
    /// Provider turns queued for worker dispatch.
    pending_agent_provider_tasks: BTreeSet<String>,
    /// Provider turns claimed by workers but not yet settled.
    claimed_agent_provider_tasks: BTreeMap<String, RuntimeAgentProviderClaim>,
    /// User steering prompts waiting for the next provider action boundary.
    agent_turn_pending_steering: BTreeMap<String, Vec<AgentTurnSteering>>,
    /// Panes currently running model-backed context compaction.
    agent_compacting_panes: BTreeMap<String, u64>,
    /// Model-backed compaction tasks waiting for provider dispatch.
    pending_agent_compaction_tasks: BTreeMap<String, RuntimeAgentCompactionTask>,
    /// Model-backed compaction tasks claimed by provider workers.
    claimed_agent_compaction_tasks: BTreeMap<String, RuntimeAgentCompactionTask>,
    /// Panes currently running model-backed durable-memory generation.
    agent_remembering_panes: BTreeMap<String, u64>,
    /// Durable-memory generation tasks waiting for provider dispatch.
    pending_agent_remember_tasks: BTreeMap<String, RuntimeAgentRememberTask>,
    /// Durable-memory generation tasks claimed by provider workers.
    claimed_agent_remember_tasks: BTreeMap<String, RuntimeAgentRememberTask>,
    /// Cumulative provider token usage keyed by conversation and model.
    agent_token_usage_by_conversation:
        BTreeMap<String, BTreeMap<ModelTokenUsageKey, ModelTokenUsage>>,
    /// Cumulative provider token usage keyed by pane and model.
    agent_token_usage_by_pane: BTreeMap<String, BTreeMap<ModelTokenUsageKey, ModelTokenUsage>>,
    /// Latest display-ready context usage keyed by conversation.
    agent_context_usage_by_conversation: BTreeMap<String, String>,
    /// Latest structured context usage keyed by conversation.
    agent_context_usage_snapshot_by_conversation:
        BTreeMap<String, mez_agent::AgentContextUsageSnapshot>,
    /// Latest provider quota usage keyed by conversation.
    agent_quota_usage_by_conversation: BTreeMap<String, Vec<ProviderQuotaUsage>>,
    /// Latest live model catalog keyed by provider id.
    provider_model_catalog_cache: BTreeMap<String, RuntimeModelCatalog>,
    /// Maximum subagent panes assigned to one background window.
    max_subagent_panes_per_window: usize,
    /// Maximum direct subagents available to a root pane agent.
    max_root_subagents: usize,
    /// Maximum direct subagents available to a child agent.
    max_subagents_per_subagent: usize,
    /// Maximum nested subagent delegation depth.
    max_subagent_depth: usize,
    /// Whether parent turns join or detach spawned subagents.
    subagent_wait_policy: SubagentWaitPolicy,
    /// Parent agent route keyed by spawned child turn id.
    subagent_task_routes: BTreeMap<String, String>,
    /// Windows reserved for spawned subagent panes.
    subagent_window_ids: BTreeSet<String>,
    /// Subagent panes awaiting close after terminal turn cleanup.
    pending_terminal_subagent_pane_closes: BTreeSet<String>,
}

/// State removed when a compaction worker reports failure.
#[derive(Debug, Default)]
pub(in crate::runtime) struct RuntimeAgentCompactionFailureState {
    /// Whether pending, claimed, or active compaction state existed.
    had_task: bool,
    /// Running provider turn that must fail when recovery compaction failed.
    resume_turn_id: Option<String>,
}

impl RuntimeAgentCompactionFailureState {
    /// Reports whether any compaction state was removed.
    pub(in crate::runtime) fn had_task(&self) -> bool {
        self.had_task
    }

    /// Takes the running turn awaiting failed recovery compaction.
    pub(in crate::runtime) fn take_resume_turn_id(&mut self) -> Option<String> {
        self.resume_turn_id.take()
    }
}

impl RuntimeAgentComponent {
    /// Builds agent ownership with configured provider-selection defaults.
    pub(in crate::runtime) fn with_settings(
        agent_routing: bool,
        agent_auto_sizing: RuntimeAutoSizingConfig,
        agent_compaction_raw_retention_percent: usize,
        agent_loop_limit: usize,
        agent_action_failure_retry_limit: usize,
        agent_implementation_pressure_after_shell_actions: usize,
    ) -> Self {
        Self {
            agent_routing,
            agent_auto_sizing,
            agent_compaction_raw_retention_percent,
            agent_loop_limit,
            agent_action_failure_retry_limit,
            agent_implementation_pressure_after_shell_actions,
            max_subagent_panes_per_window: DEFAULT_MAX_SUBAGENT_PANES_PER_WINDOW,
            max_root_subagents: DEFAULT_MAX_ROOT_SUBAGENTS,
            max_subagents_per_subagent: DEFAULT_MAX_SUBAGENTS_PER_SUBAGENT,
            max_subagent_depth: DEFAULT_MAX_SUBAGENT_DEPTH,
            subagent_wait_policy: DEFAULT_SUBAGENT_WAIT_POLICY,
            ..Self::default()
        }
    }
}

impl RuntimeSessionService {
    /// Returns the parent-agent route for one spawned child turn.
    pub(in crate::runtime) fn subagent_task_parent(&self, turn_id: &str) -> Option<String> {
        self.agent.subagent_task_routes.get(turn_id).cloned()
    }

    /// Records the parent-agent route for one spawned child turn.
    pub(in crate::runtime) fn set_subagent_task_parent(
        &mut self,
        turn_id: impl Into<String>,
        parent_agent_id: impl Into<String>,
    ) {
        self.agent
            .subagent_task_routes
            .insert(turn_id.into(), parent_agent_id.into());
    }

    /// Removes the parent-agent route for one spawned child turn.
    pub(in crate::runtime) fn remove_subagent_task_parent(&mut self, turn_id: &str) {
        self.agent.subagent_task_routes.remove(turn_id);
    }

    /// Removes every child-turn route owned by one parent agent.
    pub(in crate::runtime) fn remove_subagent_task_routes_for_parent(
        &mut self,
        parent_agent_id: &str,
    ) {
        self.agent
            .subagent_task_routes
            .retain(|_, parent| parent != parent_agent_id);
    }

    /// Records a window as reserved for subagent panes.
    pub(in crate::runtime) fn mark_subagent_window(&mut self, window_id: impl Into<String>) {
        self.agent.subagent_window_ids.insert(window_id.into());
    }

    /// Reports whether a window is reserved for subagent panes.
    pub(in crate::runtime) fn is_subagent_window(&self, window_id: &str) -> bool {
        self.agent.subagent_window_ids.contains(window_id)
    }

    /// Returns all currently reserved subagent window ids.
    pub(in crate::runtime) fn subagent_window_ids(&self) -> Vec<String> {
        self.agent.subagent_window_ids.iter().cloned().collect()
    }

    /// Retains only subagent windows still present in the mux session.
    pub(in crate::runtime) fn retain_live_subagent_windows(
        &mut self,
        live_window_ids: &BTreeSet<String>,
    ) {
        self.agent
            .subagent_window_ids
            .retain(|window_id| live_window_ids.contains(window_id));
    }

    /// Removes one deferred terminal-pane close marker.
    pub(in crate::runtime) fn clear_terminal_subagent_pane_close(&mut self, pane_id: &str) -> bool {
        self.agent
            .pending_terminal_subagent_pane_closes
            .remove(pane_id)
    }

    /// Clears all subagent routing and placement state on session replacement.
    pub(in crate::runtime) fn clear_subagent_placement_state(&mut self) {
        self.agent.subagent_task_routes.clear();
        self.agent.subagent_window_ids.clear();
        self.agent.pending_terminal_subagent_pane_closes.clear();
    }

    /// Replaces all configured subagent placement and delegation limits.
    pub(in crate::runtime) fn configure_subagent_policy(
        &mut self,
        max_subagent_panes_per_window: usize,
        max_root_subagents: usize,
        max_subagents_per_subagent: usize,
        max_subagent_depth: usize,
        subagent_wait_policy: SubagentWaitPolicy,
    ) {
        self.agent.max_subagent_panes_per_window = max_subagent_panes_per_window;
        self.agent.max_root_subagents = max_root_subagents;
        self.agent.max_subagents_per_subagent = max_subagents_per_subagent;
        self.agent.max_subagent_depth = max_subagent_depth;
        self.agent.subagent_wait_policy = subagent_wait_policy;
    }

    /// Returns the configured subagent pane capacity per window.
    pub(in crate::runtime) fn max_subagent_panes_per_window(&self) -> usize {
        self.agent.max_subagent_panes_per_window
    }

    /// Returns the direct-subagent limit for root agents.
    pub(crate) fn max_root_subagents(&self) -> usize {
        self.agent.max_root_subagents
    }

    /// Returns the direct-subagent limit for child agents.
    pub(crate) fn max_subagents_per_subagent(&self) -> usize {
        self.agent.max_subagents_per_subagent
    }

    /// Returns the maximum nested subagent depth.
    pub(crate) fn max_subagent_depth(&self) -> usize {
        self.agent.max_subagent_depth
    }

    /// Returns whether parent turns join or detach spawned subagents.
    #[cfg(test)]
    pub(crate) fn subagent_wait_policy(&self) -> SubagentWaitPolicy {
        self.agent.subagent_wait_policy
    }

    /// Returns a cached live model catalog for one provider.
    pub(in crate::runtime) fn cached_provider_model_catalog(
        &self,
        provider_id: &str,
    ) -> Option<RuntimeModelCatalog> {
        self.agent
            .provider_model_catalog_cache
            .get(provider_id)
            .cloned()
    }

    /// Replaces the cached live model catalog for one provider.
    pub(in crate::runtime) fn cache_provider_model_catalog(
        &mut self,
        provider_id: impl Into<String>,
        catalog: RuntimeModelCatalog,
    ) {
        self.agent
            .provider_model_catalog_cache
            .insert(provider_id.into(), catalog);
    }

    /// Invalidates one provider's cached live model catalog.
    pub(in crate::runtime) fn remove_cached_provider_model_catalog(&mut self, provider_id: &str) {
        self.agent.provider_model_catalog_cache.remove(provider_id);
    }

    /// Invalidates all live model catalogs after configuration changes.
    pub(in crate::runtime) fn clear_provider_model_catalog_cache(&mut self) {
        self.agent.provider_model_catalog_cache.clear();
    }

    /// Reports whether a catalog is cached in crate-local regression tests.
    #[cfg(test)]
    pub(crate) fn has_cached_provider_model_catalog(&self, provider_id: &str) -> bool {
        self.agent
            .provider_model_catalog_cache
            .contains_key(provider_id)
    }

    /// Returns cumulative token usage for one pane.
    pub(in crate::runtime) fn agent_token_usage_for_pane(
        &self,
        pane_id: &str,
    ) -> BTreeMap<ModelTokenUsageKey, ModelTokenUsage> {
        self.agent
            .agent_token_usage_by_pane
            .get(pane_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Returns cumulative token usage for one conversation.
    pub(in crate::runtime) fn agent_token_usage_for_conversation(
        &self,
        conversation_id: &str,
    ) -> BTreeMap<ModelTokenUsageKey, ModelTokenUsage> {
        self.agent
            .agent_token_usage_by_conversation
            .get(conversation_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Aggregates non-zero token usage across all agent conversations.
    pub(in crate::runtime) fn total_agent_token_usage_by_model(
        &self,
    ) -> BTreeMap<ModelTokenUsageKey, ModelTokenUsage> {
        let mut total: BTreeMap<ModelTokenUsageKey, ModelTokenUsage> = BTreeMap::new();
        for session_usage in self.agent.agent_token_usage_by_conversation.values() {
            for (key, usage) in session_usage {
                if usage.is_zero() {
                    continue;
                }
                total.entry(key.clone()).or_default().add_assign(*usage);
            }
        }
        total
    }

    /// Replaces restored token usage for one conversation and its pane.
    pub(in crate::runtime) fn replace_restored_agent_token_usage(
        &mut self,
        conversation_id: &str,
        pane_id: &str,
        usage: BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    ) {
        if usage.is_empty() {
            self.agent
                .agent_token_usage_by_conversation
                .remove(conversation_id);
            self.agent.agent_token_usage_by_pane.remove(pane_id);
        } else {
            self.agent
                .agent_token_usage_by_conversation
                .insert(conversation_id.to_string(), usage.clone());
            self.agent
                .agent_token_usage_by_pane
                .insert(pane_id.to_string(), usage);
        }
    }

    /// Merges conversation metadata usage into the pane aggregate.
    pub(in crate::runtime) fn merge_restored_agent_token_usage(
        &mut self,
        conversation_id: &str,
        pane_id: &str,
        usage: BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    ) {
        if usage.is_empty() {
            self.agent
                .agent_token_usage_by_conversation
                .remove(conversation_id);
            return;
        }
        self.agent
            .agent_token_usage_by_conversation
            .insert(conversation_id.to_string(), usage.clone());
        let pane_usage = self
            .agent
            .agent_token_usage_by_pane
            .entry(pane_id.to_string())
            .or_default();
        for (key, value) in usage {
            pane_usage.entry(key).or_default().add_assign(value);
        }
    }

    /// Restores legacy and structured provider context usage together.
    pub(in crate::runtime) fn restore_agent_context_usage(
        &mut self,
        conversation_id: &str,
        display: Option<String>,
        snapshot: Option<mez_agent::AgentContextUsageSnapshot>,
    ) {
        if let Some(display) = display {
            self.agent
                .agent_context_usage_by_conversation
                .insert(conversation_id.to_string(), display);
        } else {
            self.agent
                .agent_context_usage_by_conversation
                .remove(conversation_id);
        }
        if let Some(snapshot) = snapshot {
            self.agent
                .agent_context_usage_snapshot_by_conversation
                .insert(conversation_id.to_string(), snapshot);
            if let Some(display) = runtime_agent_provider_context_usage_display(snapshot) {
                self.agent
                    .agent_context_usage_by_conversation
                    .insert(conversation_id.to_string(), display);
            }
        } else {
            self.agent
                .agent_context_usage_snapshot_by_conversation
                .remove(conversation_id);
        }
    }

    /// Returns the display-ready context usage for one conversation.
    pub(in crate::runtime) fn agent_context_usage_display(
        &self,
        conversation_id: &str,
    ) -> Option<String> {
        self.agent
            .agent_context_usage_by_conversation
            .get(conversation_id)
            .cloned()
    }

    /// Returns the structured context usage for one conversation.
    pub(in crate::runtime) fn agent_context_usage_snapshot(
        &self,
        conversation_id: &str,
    ) -> Option<mez_agent::AgentContextUsageSnapshot> {
        self.agent
            .agent_context_usage_snapshot_by_conversation
            .get(conversation_id)
            .copied()
    }

    /// Reports when model-backed compaction started for one pane.
    pub(in crate::runtime) fn agent_compaction_started_at(&self, pane_id: &str) -> Option<u64> {
        self.agent.agent_compacting_panes.get(pane_id).copied()
    }

    /// Reports when durable-memory generation started for one pane.
    pub(in crate::runtime) fn agent_remember_started_at(&self, pane_id: &str) -> Option<u64> {
        self.agent.agent_remembering_panes.get(pane_id).copied()
    }

    /// Reports whether one pane is compacting its model context.
    pub(in crate::runtime) fn agent_is_compacting(&self, pane_id: &str) -> bool {
        self.agent.agent_compacting_panes.contains_key(pane_id)
    }

    /// Reports whether one pane is generating durable memories.
    pub(in crate::runtime) fn agent_is_remembering(&self, pane_id: &str) -> bool {
        self.agent.agent_remembering_panes.contains_key(pane_id)
    }

    /// Counts background model operations attached to the provided panes.
    pub(in crate::runtime) fn active_agent_background_work_count(
        &self,
        pane_ids: &[String],
    ) -> usize {
        self.agent
            .agent_compacting_panes
            .keys()
            .filter(|pane_id| pane_ids.contains(pane_id))
            .count()
            .saturating_add(
                self.agent
                    .agent_remembering_panes
                    .keys()
                    .filter(|pane_id| pane_ids.contains(pane_id))
                    .count(),
            )
    }

    /// Queues one compaction task and marks its pane active.
    pub(in crate::runtime) fn queue_agent_compaction_task(
        &mut self,
        task: RuntimeAgentCompactionTask,
    ) {
        let pane_id = task.pane_id.clone();
        self.agent
            .agent_compacting_panes
            .insert(pane_id.clone(), current_unix_seconds().max(1));
        self.agent
            .pending_agent_compaction_tasks
            .insert(pane_id, task);
    }

    /// Returns pane ids with queued model-backed compaction work.
    pub(in crate::runtime) fn pending_agent_compaction_task_ids(&self) -> Vec<String> {
        self.agent
            .pending_agent_compaction_tasks
            .keys()
            .cloned()
            .collect()
    }

    /// Returns turns waiting for output-limit recovery compaction.
    pub(in crate::runtime) fn agent_compaction_resume_ids(&self) -> Vec<String> {
        self.agent
            .pending_agent_compaction_tasks
            .values()
            .chain(self.agent.claimed_agent_compaction_tasks.values())
            .filter_map(|task| task.resume_turn_id.clone())
            .collect()
    }

    /// Removes one pending compaction task for provider construction.
    pub(in crate::runtime) fn take_pending_agent_compaction_task(
        &mut self,
        pane_id: &str,
    ) -> Option<RuntimeAgentCompactionTask> {
        self.agent.pending_agent_compaction_tasks.remove(pane_id)
    }

    /// Records that a provider worker owns one compaction task.
    pub(in crate::runtime) fn claim_agent_compaction_task_state(
        &mut self,
        pane_id: impl Into<String>,
        task: RuntimeAgentCompactionTask,
    ) {
        self.agent
            .claimed_agent_compaction_tasks
            .insert(pane_id.into(), task);
    }

    /// Finishes claimed compaction state and clears its pane activity marker.
    pub(in crate::runtime) fn finish_agent_compaction_task(
        &mut self,
        pane_id: &str,
    ) -> Option<RuntimeAgentCompactionTask> {
        let task = self.agent.claimed_agent_compaction_tasks.remove(pane_id);
        self.agent.agent_compacting_panes.remove(pane_id);
        task
    }

    /// Removes all compaction state after provider failure.
    pub(in crate::runtime) fn fail_agent_compaction_task(
        &mut self,
        pane_id: &str,
    ) -> RuntimeAgentCompactionFailureState {
        let pending = self.agent.pending_agent_compaction_tasks.remove(pane_id);
        let claimed = self.agent.claimed_agent_compaction_tasks.remove(pane_id);
        let resume_turn_id = claimed
            .as_ref()
            .or(pending.as_ref())
            .and_then(|task| task.resume_turn_id.clone());
        let had_task = pending.is_some()
            || claimed.is_some()
            || self.agent.agent_compacting_panes.remove(pane_id).is_some();
        RuntimeAgentCompactionFailureState {
            had_task,
            resume_turn_id,
        }
    }

    /// Queues one durable-memory task and marks its pane active.
    pub(in crate::runtime) fn queue_agent_remember_task(&mut self, task: RuntimeAgentRememberTask) {
        let pane_id = task.pane_id.clone();
        self.agent
            .agent_remembering_panes
            .insert(pane_id.clone(), current_unix_seconds().max(1));
        self.agent
            .pending_agent_remember_tasks
            .insert(pane_id, task);
    }

    /// Returns pane ids with queued durable-memory generation work.
    pub(in crate::runtime) fn pending_agent_remember_task_ids(&self) -> Vec<String> {
        self.agent
            .pending_agent_remember_tasks
            .keys()
            .cloned()
            .collect()
    }

    /// Removes one pending durable-memory task for provider construction.
    pub(in crate::runtime) fn take_pending_agent_remember_task(
        &mut self,
        pane_id: &str,
    ) -> Option<RuntimeAgentRememberTask> {
        self.agent.pending_agent_remember_tasks.remove(pane_id)
    }

    /// Records that a provider worker owns one durable-memory task.
    pub(in crate::runtime) fn claim_agent_remember_task_state(
        &mut self,
        pane_id: impl Into<String>,
        task: RuntimeAgentRememberTask,
    ) {
        self.agent
            .claimed_agent_remember_tasks
            .insert(pane_id.into(), task);
    }

    /// Finishes claimed durable-memory state and clears pane activity.
    pub(in crate::runtime) fn finish_agent_remember_task(
        &mut self,
        pane_id: &str,
    ) -> Option<RuntimeAgentRememberTask> {
        let task = self.agent.claimed_agent_remember_tasks.remove(pane_id);
        self.agent.agent_remembering_panes.remove(pane_id);
        task
    }

    /// Removes all durable-memory generation state after provider failure.
    pub(in crate::runtime) fn fail_agent_remember_task(&mut self, pane_id: &str) -> bool {
        let pending = self
            .agent
            .pending_agent_remember_tasks
            .remove(pane_id)
            .is_some();
        let claimed = self
            .agent
            .claimed_agent_remember_tasks
            .remove(pane_id)
            .is_some();
        let active = self.agent.agent_remembering_panes.remove(pane_id).is_some();
        pending || claimed || active
    }

    /// Marks one pane as compacting in rendering regression tests.
    #[cfg(test)]
    pub(crate) fn mark_agent_compacting_for_tests(&mut self, pane_id: impl Into<String>, at: u64) {
        self.agent.agent_compacting_panes.insert(pane_id.into(), at);
    }

    /// Returns one queued compaction task to crate-local regression tests.
    #[cfg(test)]
    pub(crate) fn pending_agent_compaction_task_for_tests(
        &self,
        pane_id: &str,
    ) -> Option<&RuntimeAgentCompactionTask> {
        self.agent.pending_agent_compaction_tasks.get(pane_id)
    }

    /// Returns the agent scheduler for read-only diagnostics and prompt context.
    pub(crate) fn agent_scheduler(&self) -> &AgentScheduler {
        &self.agent.agent_scheduler
    }

    /// Returns mutable scheduler access to crate-local regression tests.
    #[cfg(test)]
    pub(crate) fn agent_scheduler_mut(&mut self) -> &mut AgentScheduler {
        &mut self.agent.agent_scheduler
    }

    /// Applies the configured global agent concurrency limit.
    pub(in crate::runtime) fn configure_agent_scheduler_limit(
        &mut self,
        max_concurrent_agents: usize,
    ) -> Result<()> {
        self.agent
            .agent_scheduler
            .set_max_concurrent_agents(max_concurrent_agents)?;
        Ok(())
    }

    /// Enqueues one validated unit of agent work.
    pub(in crate::runtime) fn enqueue_agent_work(&mut self, work: ScheduledWork) -> Result<()> {
        self.agent.agent_scheduler.enqueue(work)?;
        Ok(())
    }

    /// Cancels queued, running, or blocked scheduler work when it exists.
    pub(in crate::runtime) fn cancel_agent_work(&mut self, turn_id: &str) -> bool {
        self.agent.agent_scheduler.cancel(turn_id).is_ok()
    }

    /// Restores an empty scheduler with the repository default limit.
    pub(in crate::runtime) fn reset_agent_scheduler(&mut self) {
        self.agent.agent_scheduler = AgentScheduler::with_default_limit();
    }

    /// Appends one steering prompt to an active turn.
    pub(in crate::runtime) fn push_agent_turn_steering(
        &mut self,
        turn_id: impl Into<String>,
        steering: AgentTurnSteering,
    ) {
        self.agent
            .agent_turn_pending_steering
            .entry(turn_id.into())
            .or_default()
            .push(steering);
    }

    /// Takes all steering prompts waiting for one turn.
    pub(in crate::runtime) fn take_agent_turn_steering(
        &mut self,
        turn_id: &str,
    ) -> Option<Vec<AgentTurnSteering>> {
        self.agent.agent_turn_pending_steering.remove(turn_id)
    }

    /// Reports whether one turn has pending user steering.
    pub(in crate::runtime) fn agent_turn_has_pending_steering(&self, turn_id: &str) -> bool {
        self.agent.agent_turn_pending_steering.contains_key(turn_id)
    }

    /// Removes pending steering for one completed turn.
    pub(in crate::runtime) fn clear_agent_turn_steering(&mut self, turn_id: &str) {
        self.agent.agent_turn_pending_steering.remove(turn_id);
    }

    /// Clears all pending steering for session replacement.
    pub(in crate::runtime) fn clear_all_agent_turn_steering(&mut self) {
        self.agent.agent_turn_pending_steering.clear();
    }

    /// Reports whether one provider turn is queued for dispatch.
    pub(in crate::runtime) fn agent_provider_task_is_pending(&self, turn_id: &str) -> bool {
        self.agent.pending_agent_provider_tasks.contains(turn_id)
    }

    /// Reports whether one provider turn is claimed by a worker.
    pub(in crate::runtime) fn agent_provider_task_is_claimed(&self, turn_id: &str) -> bool {
        self.agent
            .claimed_agent_provider_tasks
            .contains_key(turn_id)
    }

    /// Reports whether a provider turn is queued or claimed.
    pub(in crate::runtime) fn agent_provider_task_is_owned(&self, turn_id: &str) -> bool {
        self.agent_provider_task_is_pending(turn_id) || self.agent_provider_task_is_claimed(turn_id)
    }

    /// Queues one provider turn when it is not already pending.
    pub(in crate::runtime) fn queue_agent_provider_task(
        &mut self,
        turn_id: impl Into<String>,
    ) -> bool {
        self.agent
            .pending_agent_provider_tasks
            .insert(turn_id.into())
    }

    /// Removes one pending provider turn.
    pub(in crate::runtime) fn remove_pending_agent_provider_task(&mut self, turn_id: &str) -> bool {
        self.agent.pending_agent_provider_tasks.remove(turn_id)
    }

    /// Removes one claimed provider turn.
    pub(in crate::runtime) fn remove_claimed_agent_provider_task(
        &mut self,
        turn_id: &str,
    ) -> Option<RuntimeAgentProviderClaim> {
        self.agent.claimed_agent_provider_tasks.remove(turn_id)
    }

    /// Clears all queued and claimed provider work for session replacement.
    pub(in crate::runtime) fn clear_agent_provider_task_ownership(&mut self) {
        self.agent.pending_agent_provider_tasks.clear();
        self.agent.claimed_agent_provider_tasks.clear();
    }

    /// Returns the effective model profile retained for one turn.
    pub(in crate::runtime) fn agent_turn_model_profile(
        &self,
        turn_id: &str,
    ) -> Option<&ModelProfile> {
        self.agent.agent_turn_model_profiles.get(turn_id)
    }

    /// Replaces the effective model profile retained for one turn.
    pub(in crate::runtime) fn set_agent_turn_model_profile(
        &mut self,
        turn_id: impl Into<String>,
        profile: ModelProfile,
    ) {
        self.agent
            .agent_turn_model_profiles
            .insert(turn_id.into(), profile);
    }

    /// Removes the effective model profile retained for one turn.
    pub(in crate::runtime) fn remove_agent_turn_model_profile(
        &mut self,
        turn_id: &str,
    ) -> Option<ModelProfile> {
        self.agent.agent_turn_model_profiles.remove(turn_id)
    }

    /// Clears all retained turn model profiles for session replacement.
    pub(in crate::runtime) fn clear_agent_turn_model_profiles(&mut self) {
        self.agent.agent_turn_model_profiles.clear();
    }

    /// Clears correction attempts and action histories for one completed turn.
    pub(in crate::runtime) fn clear_agent_action_bookkeeping_for_turn(&mut self, turn_id: &str) {
        self.clear_agent_failure_feedback_attempts_for_turn(turn_id);
        self.agent.agent_turn_shell_dispatch_history.remove(turn_id);
        self.agent.agent_turn_network_action_history.remove(turn_id);
        self.agent
            .agent_pre_shell_hook_completions
            .retain(|completion| completion.turn_id != turn_id);
    }

    /// Clears all action bookkeeping when the live session is replaced.
    pub(in crate::runtime) fn clear_all_agent_action_bookkeeping(&mut self) {
        self.agent.agent_turn_failure_feedback_attempts.clear();
        self.agent.agent_turn_shell_dispatch_history.clear();
        self.agent.agent_turn_network_action_history.clear();
        self.agent.agent_pre_shell_hook_completions.clear();
    }

    /// Reports whether one pre-shell hook already completed for an action.
    pub(in crate::runtime) fn agent_pre_shell_hook_completed(
        &self,
        continuation: &PendingFocusedShellHookContinuation,
        hook_id: &str,
    ) -> bool {
        self.agent
            .agent_pre_shell_hook_completions
            .contains(&RuntimeAgentPreShellHookCompletion {
                turn_id: continuation.turn_id.clone(),
                action_id: continuation.action_id.clone(),
                hook_id: hook_id.to_string(),
            })
    }

    /// Records one completed pre-shell hook for an action.
    pub(in crate::runtime) fn record_agent_pre_shell_hook_completed(
        &mut self,
        continuation: &PendingFocusedShellHookContinuation,
        hook_id: &str,
    ) {
        self.agent
            .agent_pre_shell_hook_completions
            .insert(RuntimeAgentPreShellHookCompletion {
                turn_id: continuation.turn_id.clone(),
                action_id: continuation.action_id.clone(),
                hook_id: hook_id.to_string(),
            });
    }

    /// Clears completed pre-shell hook records for one turn.
    pub(in crate::runtime) fn clear_agent_pre_shell_hook_completions_for_turn(
        &mut self,
        turn_id: &str,
    ) {
        self.agent
            .agent_pre_shell_hook_completions
            .retain(|completion| completion.turn_id != turn_id);
    }

    /// Returns the bounded model-correction retry limit.
    pub(in crate::runtime) fn agent_action_failure_retry_limit(&self) -> usize {
        self.agent.agent_action_failure_retry_limit.max(1)
    }

    /// Replaces the bounded model-correction retry limit.
    pub(in crate::runtime) fn set_agent_action_failure_retry_limit(&mut self, limit: usize) {
        self.agent.agent_action_failure_retry_limit = limit;
    }

    /// Returns the shell streak that activates implementation-pressure hints.
    pub(in crate::runtime) fn agent_implementation_pressure_after_shell_actions(&self) -> usize {
        self.agent
            .agent_implementation_pressure_after_shell_actions
            .max(1)
    }

    /// Replaces the implementation-pressure shell streak.
    pub(in crate::runtime) fn set_agent_implementation_pressure_after_shell_actions(
        &mut self,
        threshold: usize,
    ) {
        self.agent.agent_implementation_pressure_after_shell_actions = threshold;
    }

    /// Returns the configured loop iteration limit.
    pub(in crate::runtime) fn agent_loop_limit(&self) -> usize {
        self.agent.agent_loop_limit.max(1)
    }

    /// Replaces the configured loop iteration limit.
    pub(in crate::runtime) fn set_agent_loop_limit(&mut self, limit: usize) {
        self.agent.agent_loop_limit = limit;
    }

    /// Returns loop controller state for one pane.
    pub(in crate::runtime) fn agent_loop_state(
        &self,
        pane_id: &str,
    ) -> Option<&RuntimeAgentLoopState> {
        self.agent.agent_loops_by_pane.get(pane_id)
    }

    /// Reports whether a pane has loop controller state.
    pub(in crate::runtime) fn agent_loop_is_active(&self, pane_id: &str) -> bool {
        self.agent.agent_loops_by_pane.contains_key(pane_id)
    }

    /// Replaces loop controller state for one pane.
    pub(in crate::runtime) fn insert_agent_loop_state(&mut self, state: RuntimeAgentLoopState) {
        self.agent
            .agent_loops_by_pane
            .insert(state.pane_id.clone(), state);
    }

    /// Removes loop controller state for one pane.
    pub(in crate::runtime) fn remove_agent_loop_state(
        &mut self,
        pane_id: &str,
    ) -> Option<RuntimeAgentLoopState> {
        self.agent.agent_loops_by_pane.remove(pane_id)
    }

    /// Returns loop-owned metadata for one turn.
    pub(in crate::runtime) fn agent_loop_turn(
        &self,
        turn_id: &str,
    ) -> Option<&RuntimeAgentLoopTurn> {
        self.agent.agent_loop_turns.get(turn_id)
    }

    /// Records one loop-owned turn.
    pub(in crate::runtime) fn insert_agent_loop_turn(
        &mut self,
        turn_id: String,
        loop_turn: RuntimeAgentLoopTurn,
    ) {
        self.agent.agent_loop_turns.insert(turn_id, loop_turn);
    }

    /// Removes loop-owned metadata for one turn.
    pub(in crate::runtime) fn remove_agent_loop_turn(
        &mut self,
        turn_id: &str,
    ) -> Option<RuntimeAgentLoopTurn> {
        self.agent.agent_loop_turns.remove(turn_id)
    }

    /// Removes stale loop-owned turns for one pane.
    pub(in crate::runtime) fn clear_agent_loop_turns_for_pane(&mut self, pane_id: &str) {
        self.agent
            .agent_loop_turns
            .retain(|_, loop_turn| loop_turn.pane_id != pane_id);
    }

    /// Returns the raw-context percentage retained after compaction.
    pub(in crate::runtime) fn agent_compaction_raw_retention_percent(&self) -> usize {
        self.agent.agent_compaction_raw_retention_percent
    }

    /// Replaces the raw-context percentage retained after compaction.
    pub(in crate::runtime) fn set_agent_compaction_raw_retention_percent(
        &mut self,
        percent: usize,
    ) {
        self.agent.agent_compaction_raw_retention_percent = percent;
    }

    /// Returns the configured default auto-sizing policy.
    pub(in crate::runtime) fn agent_auto_sizing(&self) -> &RuntimeAutoSizingConfig {
        &self.agent.agent_auto_sizing
    }

    /// Replaces the configured default auto-sizing policy.
    pub(in crate::runtime) fn set_agent_auto_sizing(&mut self, config: RuntimeAutoSizingConfig) {
        self.agent.agent_auto_sizing = config;
    }

    /// Replaces the router model profile in the default auto-sizing policy.
    pub(in crate::runtime) fn set_agent_router_model_profile(&mut self, profile_name: &str) {
        self.agent.agent_auto_sizing.router_model_profile = profile_name.to_string();
    }

    /// Returns an explicit pane-local auto-sizing override.
    pub(in crate::runtime) fn agent_auto_sizing_override(
        &self,
        pane_id: &str,
    ) -> Option<&RuntimeAutoSizingConfig> {
        self.agent.agent_auto_sizing_overrides.get(pane_id)
    }

    /// Replaces or clears one pane-local auto-sizing override.
    pub(in crate::runtime) fn set_agent_auto_sizing_override(
        &mut self,
        pane_id: &str,
        config: Option<RuntimeAutoSizingConfig>,
    ) {
        if let Some(config) = config {
            self.agent
                .agent_auto_sizing_overrides
                .insert(pane_id.to_string(), config);
        } else {
            self.agent.agent_auto_sizing_overrides.remove(pane_id);
        }
    }

    /// Returns the effective auto-sizing policy for one pane.
    pub(in crate::runtime) fn agent_auto_sizing_for_pane(
        &self,
        pane_id: &str,
    ) -> &RuntimeAutoSizingConfig {
        self.agent_auto_sizing_override(pane_id)
            .unwrap_or_else(|| self.agent_auto_sizing())
    }

    /// Returns the configured default provider-routing state.
    pub(in crate::runtime) fn agent_default_routing(&self) -> bool {
        self.agent.agent_routing
    }

    /// Replaces the configured default provider-routing state.
    pub(in crate::runtime) fn set_agent_default_routing(&mut self, enabled: bool) {
        self.agent.agent_routing = enabled;
    }

    /// Returns an explicit pane-local routing override.
    pub(in crate::runtime) fn agent_routing_override(&self, pane_id: &str) -> Option<bool> {
        self.agent.agent_routing_overrides.get(pane_id).copied()
    }

    /// Replaces or clears one pane-local routing override.
    pub(in crate::runtime) fn set_agent_routing_override(
        &mut self,
        pane_id: &str,
        enabled: Option<bool>,
    ) {
        if let Some(enabled) = enabled {
            self.agent
                .agent_routing_overrides
                .insert(pane_id.to_string(), enabled);
        } else {
            self.agent.agent_routing_overrides.remove(pane_id);
        }
    }

    /// Clears one pane-local routing override during pane teardown.
    pub(in crate::runtime) fn clear_agent_routing_override(&mut self, pane_id: &str) {
        self.agent.agent_routing_overrides.remove(pane_id);
    }

    /// Reports whether planning presentation is enabled for one pane.
    pub(in crate::runtime) fn agent_planning_enabled(&self, pane_id: &str) -> bool {
        self.agent.agent_planning_modes.contains(pane_id)
    }

    /// Sets pane-local planning presentation state.
    pub(in crate::runtime) fn set_agent_planning_enabled(&mut self, pane_id: &str, enabled: bool) {
        if enabled {
            self.agent.agent_planning_modes.insert(pane_id.to_string());
        } else {
            self.agent.agent_planning_modes.remove(pane_id);
        }
    }

    /// Returns the pane-local response style selection.
    pub(in crate::runtime) fn agent_response_style(&self, pane_id: &str) -> Option<&str> {
        self.agent
            .agent_response_styles
            .get(pane_id)
            .map(String::as_str)
    }

    /// Replaces or clears one pane-local response style selection.
    pub(in crate::runtime) fn set_agent_response_style(
        &mut self,
        pane_id: &str,
        style: Option<String>,
    ) {
        if let Some(style) = style {
            self.agent
                .agent_response_styles
                .insert(pane_id.to_string(), style);
        } else {
            self.agent.agent_response_styles.remove(pane_id);
        }
    }

    /// Clears transcript-persisted pane presentation preferences.
    pub(in crate::runtime) fn clear_agent_pane_presentation_preferences(&mut self, pane_id: &str) {
        self.agent.agent_planning_modes.remove(pane_id);
        self.agent.agent_response_styles.remove(pane_id);
    }

    /// Returns retained patch attempts for one agent session.
    pub(in crate::runtime) fn retained_agent_patch_records(
        &self,
        session_id: &str,
    ) -> Option<&[RuntimeAgentPatchRecord]> {
        self.agent
            .agent_session_patch_records
            .get(session_id)
            .map(Vec::as_slice)
    }

    /// Returns the latest retained copy output for one pane.
    pub(in crate::runtime) fn retained_agent_copy_output(
        &self,
        pane_id: &str,
    ) -> Option<&RuntimeAgentCopyOutput> {
        self.agent.agent_copy_outputs.get(pane_id)
    }

    /// Returns modified-file summaries retained for one pane.
    pub(in crate::runtime) fn retained_agent_modified_files(
        &self,
        pane_id: &str,
    ) -> Option<&BTreeMap<String, RuntimeAgentModifiedFileSummary>> {
        self.agent.agent_modified_files.get(pane_id)
    }

    /// Adds one observed modification delta to a pane-local file summary.
    pub(in crate::runtime) fn record_agent_modified_file_delta(
        &mut self,
        pane_id: &str,
        path: String,
        added: usize,
        removed: usize,
    ) {
        let entry = self
            .agent
            .agent_modified_files
            .entry(pane_id.to_string())
            .or_default()
            .entry(path.clone())
            .or_insert_with(|| RuntimeAgentModifiedFileSummary {
                path,
                added: 0,
                removed: 0,
            });
        entry.added = entry.added.saturating_add(added);
        entry.removed = entry.removed.saturating_add(removed);
    }

    /// Clears session-scoped copy and modified-file artifacts.
    pub(in crate::runtime) fn clear_agent_session_artifacts(&mut self) {
        self.agent.agent_copy_outputs.clear();
        self.agent.agent_modified_files.clear();
    }

    /// Clears pane-scoped copy and modified-file artifacts.
    pub(in crate::runtime) fn clear_agent_pane_artifacts(&mut self, pane_id: &str) {
        self.agent.agent_copy_outputs.remove(pane_id);
        self.agent.agent_modified_files.remove(pane_id);
    }

    /// Clears modified-file summaries when a pane starts a fresh conversation.
    pub(in crate::runtime) fn clear_agent_modified_files(&mut self, pane_id: &str) {
        self.agent.agent_modified_files.remove(pane_id);
    }

    /// Reports whether one pane currently owns an agent child shell.
    pub(in crate::runtime) fn agent_subshell_is_active(&self, pane_id: &str) -> bool {
        self.agent.agent_subshell_panes.contains(pane_id)
    }

    /// Marks one pane as owning an agent child shell.
    pub(in crate::runtime) fn enter_agent_subshell(&mut self, pane_id: impl Into<String>) {
        self.agent.agent_subshell_panes.insert(pane_id.into());
    }

    /// Removes one pane from active agent child-shell ownership.
    pub(in crate::runtime) fn leave_agent_subshell(&mut self, pane_id: &str) -> bool {
        self.agent.agent_subshell_panes.remove(pane_id)
    }

    /// Marks an interrupted child shell for line-oriented exit.
    pub(in crate::runtime) fn mark_agent_subshell_command_exit(
        &mut self,
        pane_id: impl Into<String>,
    ) {
        self.agent
            .agent_subshell_command_exit_panes
            .insert(pane_id.into());
    }

    /// Consumes a line-oriented child-shell exit marker.
    pub(in crate::runtime) fn take_agent_subshell_command_exit(&mut self, pane_id: &str) -> bool {
        self.agent.agent_subshell_command_exit_panes.remove(pane_id)
    }

    /// Clears all agent child-shell state for a removed pane.
    pub(in crate::runtime) fn clear_agent_subshell_state(&mut self, pane_id: &str) {
        self.agent.agent_subshell_panes.remove(pane_id);
        self.agent.agent_subshell_command_exit_panes.remove(pane_id);
    }
}

#[cfg(test)]
impl RuntimeSessionService {
    /// Returns failure-feedback attempts for integration-test observation.
    pub(in crate::runtime) fn agent_failure_feedback_attempts_for_tests(
        &self,
    ) -> &BTreeMap<String, usize> {
        &self.agent.agent_turn_failure_feedback_attempts
    }

    /// Returns failure-feedback attempts for fixture setup.
    pub(in crate::runtime) fn agent_failure_feedback_attempts_mut_for_tests(
        &mut self,
    ) -> &mut BTreeMap<String, usize> {
        &mut self.agent.agent_turn_failure_feedback_attempts
    }

    /// Returns network action history for integration-test observation.
    pub(in crate::runtime) fn agent_network_action_history_for_tests(
        &self,
    ) -> &BTreeMap<String, AgentNetworkActionHistory> {
        &self.agent.agent_turn_network_action_history
    }

    /// Returns loop-owned turn metadata for integration-test observation.
    pub(in crate::runtime) fn agent_loop_turns_for_tests(
        &self,
    ) -> &BTreeMap<String, RuntimeAgentLoopTurn> {
        &self.agent.agent_loop_turns
    }

    /// Reports whether a process fixture still has a command-exit marker.
    pub(in crate::runtime) fn agent_subshell_command_exit_is_pending_for_tests(
        &self,
        pane_id: &str,
    ) -> bool {
        self.agent
            .agent_subshell_command_exit_panes
            .contains(pane_id)
    }
}
use mez_agent::outcome::{
    ActionPresentationInput, action_error_suffix as runtime_agent_action_error_suffix,
    action_has_runtime_visible_effect as runtime_agent_action_has_runtime_visible_effect,
    action_outcome_line,
    action_rationale_repeats_visible_batch_text as runtime_agent_action_rationale_repeats_visible_batch_text,
    action_rationale_repeats_visible_summary,
    action_rejects_duplicate_success as runtime_agent_action_rejects_duplicate_success,
    action_result_is_suppressed_duplicate_file_mutation as runtime_action_result_is_suppressed_duplicate_file_mutation,
    action_summary, action_terminal_preview as runtime_agent_terminal_preview,
    batch_rationale_repeats_visible_text as runtime_agent_batch_rationale_repeats_visible_batch_text,
    batch_visible_action_texts as runtime_agent_batch_visible_action_texts,
    normalize_user_visible_text as normalize_agent_user_visible_text,
    runtime_action_result_error_code, runtime_action_result_has_error_code,
    runtime_action_result_is_aggregated_loop_guard_failure,
    runtime_action_result_is_feedback_candidate, runtime_action_result_is_terminal_failure,
    runtime_action_status_name, runtime_action_type_is_shell_backed,
    runtime_execution_can_feed_failure_to_model,
    runtime_execution_uses_unbounded_apply_patch_recovery, runtime_failure_feedback_attempt_keys,
    runtime_failure_feedback_evidence_guidance, runtime_failure_feedback_loop_guard_aggregate_note,
    runtime_failure_feedback_repeat_guidance, runtime_failure_feedback_specific_guidance,
    runtime_failure_feedback_status_line, runtime_loop_guard_failure_label,
    runtime_loop_guard_failure_summary_line, runtime_provider_audit_error_message,
    runtime_unrecovered_action_failure_output, runtime_unrecovered_failure_output_lines,
    runtime_unrecovered_failure_reason, runtime_validate_provider_completion_execution,
    runtime_validate_provider_completion_identity,
};
use mez_agent::progress::{
    PROGRESS_SAY_LEDGER_LABEL as RUNTIME_PROGRESS_SAY_LEDGER_LABEL,
    RATIONALE_LEDGER_LABEL as RUNTIME_RATIONALE_LEDGER_LABEL,
    merge_progress_say_entries as runtime_merge_progress_say_entries,
    merge_rationale_entries as runtime_merge_rationale_entries,
    progress_say_entries_for_execution as runtime_progress_say_entries_for_execution,
    progress_say_entries_from_ledger as runtime_progress_say_entries_from_ledger,
    progress_say_ledger_content as runtime_progress_say_ledger_content,
    rationale_entries_for_execution as runtime_rationale_entries_for_execution,
    rationale_entries_from_context_blocks as runtime_rationale_entries_from_context_blocks,
    rationale_entries_from_ledger as runtime_rationale_entries_from_ledger,
    rationale_ledger_content as runtime_rationale_ledger_content,
    suppress_redundant_batch_rationale as runtime_suppress_redundant_batch_rationale,
};
use mez_agent::subagent_task_output_for_execution;
use outcome::{
    runtime_agent_action_outcome_line, runtime_agent_action_rationale_repeats_visible_summary,
    runtime_agent_action_summary, runtime_agent_context_command,
    runtime_agent_execution_failure_error, runtime_agent_finished_footer_line,
    runtime_agent_pending_approval_log_line, runtime_agent_shell_status,
};
use provider_events::runtime_provider_event_error;
use subagents::runtime_agent_pane_id;
use trace::{
    runtime_maap_message_content_type, runtime_spawn_json_agent_and_turn,
    runtime_subagent_display_label, runtime_subagent_result_status_label,
};

// Agent turn execution, provider polling, action dispatch, and approvals.

/// Maximum in-process provider context-limit retries for test providers.
#[cfg(test)]
const RUNTIME_PROVIDER_CONTEXT_LIMIT_RETRY_LIMIT: u32 = 3;
/// Maximum in-process provider output-limit retries for test providers.
#[cfg(test)]
const RUNTIME_PROVIDER_OUTPUT_LIMIT_RETRY_LIMIT: u32 = 2;
/// Label for ephemeral active-turn context that guides output-limit retries.
const RUNTIME_PROVIDER_OUTPUT_LIMIT_RETRY_LABEL: &str = "provider output-limit retry guidance";
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
