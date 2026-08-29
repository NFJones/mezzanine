//! Runtime Agent implementation.
//!
//! This module owns the runtime agent boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::agent_state::{
    RuntimeAgentLoopCompletion, RuntimeAgentLoopSettlement, RuntimeAgentProviderClaim,
    RuntimeNativeShellDispatch,
};
use super::commands::RuntimeModelCatalog;
#[cfg(test)]
use super::execute_mcp_action_through_runtime;
#[cfg(test)]
use super::runtime_execute_auto_sizing_with_provider;
use super::service_state::{
    RuntimeAgentPatchRecord, RuntimeApplyPatchBatchState, RuntimePendingApplyPatchPhase,
};
use super::{
    ActionResult, ActionStatus, ActiveTurnSleepInhibition, AgentAction, AgentActionPayload,
    AgentId, AgentScheduler, AgentShellSession, AgentShellVisibility, AgentTurnExecution,
    AgentTurnRecord, AgentTurnState, AuditActor, AuditRecord, BTreeMap, BTreeSet,
    BlockedAgentApprovalRef, BlockedApprovalRequest, ContextBlock, ContextSourceKind,
    DEFAULT_COMMAND_SHELL_CLASSIFICATION, DEFAULT_MAX_ROOT_SUBAGENTS, DEFAULT_MAX_SUBAGENT_DEPTH,
    DEFAULT_MAX_SUBAGENT_PANES_PER_WINDOW, DEFAULT_MAX_SUBAGENTS_PER_SUBAGENT,
    DEFAULT_SUBAGENT_WAIT_POLICY, Envelope, EventKind, HookEvent, JoinedSubagentDependency,
    McpToolCallRequest, MezError, ModelProfile, ModelResponse, PaneId, PaneReadinessState, PathBuf,
    PathScopes, PendingFocusedShellHookContinuation, PermissionPolicy, ReadinessOverrideRevocation,
    Recipient, ReqwestProviderHttpTransport, Result, RuleDecision, RunningShellTransactionKind,
    RunningShellTransactionRef, RuntimeAgentCompactionTask, RuntimeAgentCopyOutput,
    RuntimeAgentLoopState, RuntimeAgentLoopTurn, RuntimeAgentLoopTurnKind,
    RuntimeAgentModifiedFileSummary, RuntimeAgentPreShellHookCompletion,
    RuntimeAgentProviderDispatch, RuntimeAgentProviderDispatchProvider, RuntimeAgentProviderTask,
    RuntimeAgentRememberTask, RuntimeApprovedExternalActionDispatch,
    RuntimeApprovedExternalActionOutcome, RuntimeApprovedMcpActionDispatch,
    RuntimeAutoSizingConfig, RuntimeAutoSizingDispatch, RuntimeAutoSizingTargetProfile,
    RuntimeHookPipelineBlock, RuntimeHookPipelineDecision, RuntimeMcpActionExecutor,
    RuntimeProviderConfig, RuntimeSandboxFailureAssessment, RuntimeSandboxFallbackAudit,
    RuntimeSessionService, RuntimeShellTransactionActionFailure, RuntimeSideEffect,
    RuntimeSubagentLineage, ScheduledWork, SenderIdentity, ShellTransaction,
    ShellTransactionOutputTransport, SubagentScopeDeclaration, SubagentSpawnRequest,
    SubagentWaitPolicy, TaskResultPayload, TaskState, TaskStatusPayload, TranscriptEntry,
    TranscriptRole, assemble_model_request, current_unix_millis, current_unix_seconds,
    decode_shell_output_transport_with_diagnostics, discover_project_root, exact_command_sha256,
    execute_mcp_action_through_runtime_async, execute_network_action_with_transport_async,
    json_escape, local_action_plan, network_action_plan, next_transcript_sequence,
    runtime_agent_turn_duration_display, runtime_agent_turn_start_hook_payload,
    runtime_agent_turn_state_from_action_results, runtime_agent_turn_state_name,
    runtime_apply_persisted_config_mutation_batch, runtime_blocked_approval_request,
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
use crate::config::{
    ConfigFormat, ConfigLayer, ConfigMutation, ConfigMutationOperation, ConfigMutationValue,
    ConfigPaths, ConfigScope,
};
#[cfg(test)]
use crate::host::power_inhibition::PowerInhibitionState;
use crate::host::power_inhibition::{
    PowerInhibitionController, PowerInhibitionMode, production_power_inhibition_controller,
};
use crate::integrations::agent::provider::{
    deepseek_chat_completions_provider_from_auth_store_with_provider_options,
    openai_compatible_provider_from_auth_store_with_provider_options,
    openai_responses_provider_from_auth_store_with_provider_options,
};
use crate::runtime::config::ShellMode;
#[cfg(test)]
use mez_agent::CooperationMode;
use mez_agent::resolve_provider_api;
use mez_agent::routed_workflow::RoutedWorkflowState;
use mez_agent::semantic_patch_planning::{
    ApplyPatchPathBoundary, ApplyPatchTransactionPhase, apply_patch_error_plan,
    apply_patch_read_plan_for_paths_with_boundary, apply_patch_touched_paths,
    apply_patch_transaction_phase, apply_patch_write_plan_from_read_outputs_with_boundary,
};
use mez_agent::{
    ActiveWriteScope, AgentContext, AgentNetworkActionHistory, AgentShellDispatchHistory,
    AgentShellStore, AgentTurnLedger, AutoSizingRoutingPolicy, AutoSizingRoutingSelection,
    DEFAULT_PROVIDER_TIMEOUT_MS, EnvironmentSignature, MaapBatch, MacroManagedSubagent,
    MacroRunState, ModelInteractionKind, ModelTokenUsage, ModelTokenUsageKey, PreparedModelContext,
    ProviderApiCompatibility, ProviderQuotaUsage, SayStatus, ToolDiscoveryCache, ToolInventory,
    append_mcp_context_for_api_with_configured, append_mcp_context_for_provider_with_configured,
    assistant_context_content_for_execution, invoked_mcp_tools_for_context_with_configured,
    set_project_guidance_context,
};
use mez_mux::command::CommandInvocation;

/// Exactly-once ownership outcome for one execution-aware terminal settlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalResultDisposition {
    /// An ordinary child result was delivered directly to its parent.
    Delivered,
    /// A continued loop retained ownership of the logical result.
    RetainedByLoop,
    /// A routed workflow retained ownership until parent presentation.
    RetainedByRoutedWorkflow,
    /// Routed parent presentation delivered the joined result.
    PresentationDelivered,
}

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
pub(crate) use provider_execution::execute_agent_provider_persistence_work;
mod provider_tasks;
mod routed_workflow;
mod sandbox_assessment;
mod scheduler_state;
mod shell_dispatch;
mod shell_state;
mod skills;
mod startup;
mod subagents;
mod trace;
mod turn_state;

#[cfg(test)]
pub(crate) use shell_state::shell_transaction_output_max_raw_bytes;

/// Fresh, stable foreign foreground-process observations retained while one
/// shell action waits for fail-closed dispatch recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeShellDispatchBlockedRecoveryState {
    /// Unix timestamp when the current stable foreign-observation run began.
    pub(crate) started_at_unix_ms: u64,
    /// Primary process identity that the observations are fenced to.
    pub(crate) primary_process_id: u32,
    /// Shell interaction generation that the observations are fenced to.
    pub(crate) interaction_generation: u64,
    /// Foreign foreground process group observed by the first confirmation.
    pub(crate) foreign_process_group_id: u32,
    /// Consecutive fresh observations of the same foreign process group.
    pub(crate) confirmations: usize,
}

use mez_agent::ProviderRetryScheduler;
use mez_agent::messaging::task_state_name as runtime_task_state_suffix;
use presentation::{
    runtime_agent_execution_prompt_display_lines, runtime_agent_provider_context_usage_display,
};

/// Owns application-side agent execution state and lifecycle invariants.
///
/// The component begins with visible agent-subshell lifecycle state and grows
/// by coherent turn, provider, scheduling, and subagent slices. Its fields are
/// private so neighboring runtime subsystems use typed agent operations.
#[derive(Debug, Default)]
pub(crate) struct RuntimeAgentComponent {
    /// Fair scheduling state for queued, running, and blocked agent turns.
    agent_scheduler: AgentScheduler,
    /// Provider retry attempts and effect-boundary phase owned by `mez-agent`.
    provider_retry_scheduler: ProviderRetryScheduler,
    /// Panes whose visible agent session is scoped to a child shell.
    agent_subshell_panes: BTreeSet<String>,
    /// Visible panes waiting for parent bootstrap before entering a child shell.
    deferred_agent_subshell_entry_panes: BTreeSet<String>,
    /// Live panes with user input that has not reached a shell submission boundary.
    panes_with_unsubmitted_process_input: BTreeSet<String>,
    /// Live non-native shell panes awaiting an interrupt-confirmed prompt before entry.
    pending_agent_subshell_input_clear_panes: BTreeSet<String>,
    /// Panes whose child entry followed a live non-native input clear.
    completed_agent_subshell_input_clear_panes: BTreeSet<String>,
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
    /// Configured default shell execution mode for agent actions.
    agent_shell_mode: ShellMode,
    /// Explicit pane-local shell execution mode overrides.
    agent_shell_mode_overrides: BTreeMap<String, ShellMode>,
    /// Mode-specific startup state for runtime-created agent panes.
    agent_surface_startups: BTreeMap<String, startup::RuntimeAgentSurfaceStartup>,
    /// User-controlled host power policy for active agent turns.
    active_turn_sleep_inhibition: ActiveTurnSleepInhibition,
    /// Daemon-wide host power lease retained while canonical turns are running.
    active_turn_power_inhibition: Option<
        PowerInhibitionController<Box<dyn crate::host::power_inhibition::PowerInhibitionBackend>>,
    >,
    /// Percent of raw context retained after compaction.
    agent_compaction_raw_retention_percent: usize,
    /// Default model and reasoning auto-sizing policy.
    agent_auto_sizing: RuntimeAutoSizingConfig,
    /// Pane-local auto-sizing policy overrides.
    agent_auto_sizing_overrides: BTreeMap<String, RuntimeAutoSizingConfig>,
    /// Configured application policy for root-turn routing decisions.
    agent_root_routing_policy: AutoSizingRoutingPolicy,
    /// Explicit pane-local application policies for root-turn routing decisions.
    agent_root_routing_policy_overrides: BTreeMap<String, AutoSizingRoutingPolicy>,
    /// Maximum iterations accepted by one loop controller.
    agent_loop_limit: usize,
    /// Active loop controller state keyed by stable logical loop id.
    agent_loops_by_id: BTreeMap<String, RuntimeAgentLoopState>,
    /// Active logical loop id indexed by invoking or execution pane id.
    agent_loop_by_pane: BTreeMap<String, String>,
    /// Loop-owned turn metadata keyed by turn id.
    agent_loop_turns: BTreeMap<String, RuntimeAgentLoopTurn>,
    /// Per-signature correction retry limit for failed model actions.
    agent_action_failure_retry_limit: usize,
    /// Total wall-clock budget snapshotted when each new agent turn is created.
    agent_turn_timeout_ms: u64,
    /// Per-failure-signature correction attempts keyed by turn/signature.
    agent_turn_failure_feedback_attempts: BTreeMap<String, usize>,
    /// Output-limit recovery attempt currently shaping each active request.
    ///
    /// This controller state is intentionally not stored in model-visible
    /// chronology. Provider preparation projects it into a concise live-state
    /// flag only while the corresponding retry remains active.
    agent_turn_output_limit_recovery_attempts: BTreeMap<String, u32>,
    /// Bounded safe partial provider state retained only for output-limit continuation.
    ///
    /// This request-local state is removed with the owning turn and never enters
    /// durable chronology or action execution.
    agent_turn_output_limit_states: BTreeMap<String, mez_agent::ProviderOutputLimitState>,
    /// Exceptional provider interaction selected for each active turn.
    agent_turn_interaction_kinds: BTreeMap<String, ModelInteractionKind>,
    /// Causal execution group keyed by turn and execution-scoped action id.
    ///
    /// Provider response-local action ordinals receive a stable execution
    /// suffix before registration. Late results use this ownership map instead
    /// of attaching to whichever assistant execution happens to be newest when
    /// they settle.
    agent_execution_groups_by_turn:
        BTreeMap<String, BTreeMap<String, mez_agent::ContextExecutionGroupId>>,
    /// Provider-native tool-call ids keyed by turn and causal execution group.
    ///
    /// Once every action in the group settles, the runtime appends the matching
    /// native tool-result events without replacing provider-neutral chronology.
    agent_provider_tool_calls_by_turn: BTreeMap<
        String,
        BTreeMap<
            mez_agent::ContextExecutionGroupId,
            Vec<(mez_agent::ProviderContinuityOwner, String)>,
        >,
    >,
    /// Successful normalized issue queries keyed by turn and freshness key.
    ///
    /// Values retain the originating action id so a redundant query can point
    /// the model back to the exact committed result without reading SQLite.
    agent_turn_issue_query_freshness: BTreeMap<String, BTreeMap<String, String>>,
    /// Per-turn successful shell dispatch history.
    agent_turn_shell_dispatch_history: BTreeMap<String, AgentShellDispatchHistory>,
    /// Fresh, stable foreign foreground observations of blocked shell actions,
    /// keyed by turn and action identity.
    pending_shell_dispatch_blocked_recovery_attempts:
        BTreeMap<(String, String), RuntimeShellDispatchBlockedRecoveryState>,
    /// Per-turn network action history.
    agent_turn_network_action_history: BTreeMap<String, AgentNetworkActionHistory>,
    /// Successful semantic configuration mutations keyed by turn and signature.
    ///
    /// This controller-owned ledger spans provider continuations within one
    /// logical turn and is cleared with the rest of turn action bookkeeping.
    agent_turn_config_change_successes: BTreeMap<String, BTreeMap<String, ActionResult>>,
    /// Pre-shell hooks already completed for an action.
    agent_pre_shell_hook_completions: BTreeSet<RuntimeAgentPreShellHookCompletion>,
    /// Effective provider model profile retained for each active turn.
    agent_turn_model_profiles: BTreeMap<String, ModelProfile>,
    /// Configured profile identities retained separately from display labels.
    agent_turn_configured_model_profiles: BTreeMap<String, String>,
    /// Number of proactive configured-input-limit compaction passes per turn.
    agent_turn_configured_input_compaction_passes: BTreeMap<String, u32>,
    /// Complete request estimate that preceded the latest proactive pass.
    agent_turn_configured_input_previous_tokens: BTreeMap<String, usize>,
    /// Turns whose automatic routing decision has already been applied.
    agent_turn_routing_applied: BTreeSet<String>,
    /// Provider turns queued for worker dispatch.
    pending_agent_provider_tasks: BTreeSet<String>,
    /// Provider turns claimed by workers but not yet settled.
    claimed_agent_provider_tasks: BTreeMap<String, RuntimeAgentProviderClaim>,
    /// Provider turns whose actor-validated memory or issue actions are being
    /// settled by the bounded persistence worker.
    pending_agent_provider_persistence: BTreeSet<String>,
    /// Approved network and MCP actions waiting for external worker dispatch.
    pending_approved_external_actions: BTreeSet<(String, String)>,
    /// Approved external actions currently owned by async workers.
    claimed_approved_external_actions: BTreeSet<(String, String)>,
    /// Authorized native shell actions waiting for external worker dispatch.
    pending_native_shell_dispatches: BTreeMap<(String, String), RuntimeNativeShellDispatch>,
    /// Exact native shell marker owned by an external worker per turn/action.
    claimed_native_shell_dispatches: BTreeMap<(String, String), String>,
    /// Ambiguous Bubblewrap failures awaiting one bounded internal model
    /// assessment, keyed by the owning turn.
    sandbox_failure_assessments: BTreeMap<String, RuntimeSandboxFailureAssessment>,
    /// Redacted classification and approval facts retained across one exact
    /// approved unsandboxed fallback retry.
    sandbox_fallback_audits: BTreeMap<(String, String), RuntimeSandboxFallbackAudit>,
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
    /// Latest concrete execution-model request usage keyed by conversation.
    agent_latest_request_usage_by_conversation:
        BTreeMap<String, mez_agent::LatestModelRequestUsage>,
    /// Previous provider-bound context snapshot keyed by conversation.
    agent_context_continuity_snapshot_by_conversation:
        BTreeMap<String, mez_agent::ContextContinuitySnapshot>,
    /// Latest provider-bound context comparison keyed by conversation.
    agent_context_continuity_by_conversation:
        BTreeMap<String, mez_agent::ContextContinuityDiagnostics>,
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
    /// Persistent macro-managed child agents keyed by child agent id.
    macro_managed_subagent_agents: BTreeMap<String, MacroManagedSubagent>,
    /// Active macro runs keyed by parent orchestration turn id.
    macro_runs_by_parent_turn: BTreeMap<String, MacroRunState>,
    /// Parent macro run keyed by child step turn id.
    macro_run_by_child_turn: BTreeMap<String, String>,
    /// Active routed-worker workflows keyed by parent presentation turn id.
    routed_workflows_by_parent_turn: BTreeMap<String, RoutedWorkflowState>,
    /// Parent routed workflow keyed by managed child turn id.
    routed_workflow_by_child_turn: BTreeMap<String, String>,
    /// Durable managed-child context snapshots keyed by routed parent turn id.
    routed_child_contexts_by_parent_turn: BTreeMap<String, AgentContext>,
    /// Durable managed-child model profiles keyed by routed parent turn id.
    routed_child_profiles_by_parent_turn: BTreeMap<String, ModelProfile>,
    /// Configured profile identities for durable routed-child profile snapshots.
    routed_child_profile_names_by_parent_turn: BTreeMap<String, String>,
    /// Parent turns whose next provider request is respond-only presentation.
    routed_presentation_turns: BTreeSet<String>,
    /// Macro-loop completions retained until routed parent presentation settles.
    routed_loop_completions_by_parent_turn: BTreeMap<String, RuntimeAgentLoopCompletion>,
    /// Routed parent task results already emitted through terminal presentation.
    settled_routed_parent_result_turns: BTreeSet<String>,
    /// Subagent terminal results already committed through their parent handoff.
    settled_subagent_result_turns: BTreeSet<String>,
    /// Execution-aware terminal settlements claimed before lifecycle cleanup.
    terminal_result_dispositions: BTreeMap<String, TerminalResultDisposition>,
    /// Test-only one-shot failure injected after a routed worker spawn succeeds.
    #[cfg(test)]
    fail_routed_worker_after_spawn: bool,
    /// Test-only one-shot failure injected after a routed child is enqueued.
    #[cfg(test)]
    fail_routed_child_enqueue_trace: bool,
    /// Test-only one-shot failure injected after a durable `/fork` is created.
    #[cfg(test)]
    fail_agent_fork_after_persistence: bool,
    /// Test-only one-shot failure injected before a routed loop continuation queues.
    #[cfg(test)]
    fail_routed_loop_continuation_queue: bool,
    /// Test-only one-shot failure injected while tracing a routed parent continuation.
    #[cfg(test)]
    fail_routed_parent_continuation_trace: bool,
    /// Approval continuation metadata keyed by blocked approval id.
    blocked_agent_approval_refs: BTreeMap<String, BlockedAgentApprovalRef>,
    /// Exact turn/action identities granted one unsandboxed retry after a
    /// Bubblewrap fallback approval. Dispatch consumes each identity once.
    sandbox_bypass_after_approval: BTreeSet<(String, String)>,
    /// Approved bypasses that have started dispatch and remain active across
    /// every internal transaction phase of that exact action.
    active_sandbox_bypasses: BTreeSet<(String, String)>,
    /// Spawned child turns currently joined by parent agent actions.
    joined_subagent_dependencies: BTreeMap<String, JoinedSubagentDependency>,
    /// Declared scope and permission inheritance keyed by child agent id.
    subagent_scope_declarations: BTreeMap<String, SubagentScopeDeclaration>,
    /// Runtime delegation lineage keyed by child agent id.
    subagent_lineage: BTreeMap<String, RuntimeSubagentLineage>,
    /// Canonical active write-scope ownership registry.
    subagent_scopes: mez_agent::ScopeRegistry,
    /// Tool inventory cache keyed by pane environment signature.
    tool_discovery_cache: ToolDiscoveryCache,
    /// Project instruction files discovered for each pane.
    pane_instruction_files:
        BTreeMap<String, Vec<mez_agent::instructions::DiscoveredInstructionFile>>,
    /// Batched semantic apply-patch read state keyed by turn/action.
    apply_patch_batch_states: BTreeMap<String, RuntimeApplyPatchBatchState>,
    /// Generated apply-patch phases retained while their pre-shell hooks run.
    pending_apply_patch_phases: BTreeMap<String, RuntimePendingApplyPatchPhase>,
    /// Pane-scoped agent shell sessions and conversation bindings.
    agent_shell_store: AgentShellStore,
    /// Canonical queued, running, blocked, and terminal agent turns.
    agent_turn_ledger: AgentTurnLedger,
    /// Assembled provider context keyed by turn id.
    agent_turn_contexts: BTreeMap<String, AgentContext>,
    /// Terminal execution transcript groups already accepted for persistence.
    agent_persisted_execution_transcripts: BTreeSet<(String, String)>,
    /// Action execution state keyed by turn id.
    agent_turn_executions: BTreeMap<String, AgentTurnExecution>,
}

/// Test-only backend that records no host-side power state while preserving
/// controller transition semantics for runtime lifecycle regressions.
#[cfg(test)]
#[derive(Debug, Default)]
struct TestPowerInhibitionBackend {
    next_lease_id: u32,
}

#[cfg(test)]
impl crate::host::power_inhibition::PowerInhibitionBackend for TestPowerInhibitionBackend {
    fn acquire(
        &mut self,
        _: crate::host::power_inhibition::PowerInhibitionResource,
    ) -> std::result::Result<u32, String> {
        self.next_lease_id = self.next_lease_id.saturating_add(1);
        Ok(self.next_lease_id)
    }

    fn release(&mut self, _: u32) -> std::result::Result<(), String> {
        Ok(())
    }
}

/// State removed when a compaction worker reports failure.
#[derive(Debug, Default)]
pub(crate) struct RuntimeAgentCompactionFailureState {
    /// Whether pending, claimed, or active compaction state existed.
    had_task: bool,
    /// Running provider turn that must fail when recovery compaction failed.
    resume_turn_id: Option<String>,
}

impl RuntimeAgentCompactionFailureState {
    /// Reports whether any compaction state was removed.
    pub(crate) fn had_task(&self) -> bool {
        self.had_task
    }

    /// Takes the running turn awaiting failed recovery compaction.
    pub(crate) fn take_resume_turn_id(&mut self) -> Option<String> {
        self.resume_turn_id.take()
    }
}

impl RuntimeAgentComponent {
    /// Builds agent ownership with configured provider-selection defaults.
    pub(crate) fn with_settings(
        agent_routing: bool,
        agent_auto_sizing: RuntimeAutoSizingConfig,
        agent_compaction_raw_retention_percent: usize,
        agent_loop_limit: usize,
        agent_action_failure_retry_limit: usize,
        agent_turn_timeout_ms: u64,
    ) -> Self {
        Self {
            agent_routing,
            agent_auto_sizing,
            agent_compaction_raw_retention_percent,
            agent_loop_limit,
            agent_action_failure_retry_limit,
            agent_turn_timeout_ms,
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
    /// Returns the discovered tool inventory for one environment signature.
    pub(crate) fn agent_tool_inventory(
        &self,
        signature: &EnvironmentSignature,
    ) -> Option<&ToolInventory> {
        self.agent.tool_discovery_cache.get(signature)
    }

    /// Records a discovered tool inventory for one environment signature.
    pub(crate) fn record_agent_tool_inventory(
        &mut self,
        signature: EnvironmentSignature,
        inventory: ToolInventory,
    ) {
        self.agent.tool_discovery_cache.record(signature, inventory);
    }

    /// Returns project instruction files discovered for one pane.
    pub(crate) fn pane_agent_instruction_files(
        &self,
        pane_id: &str,
    ) -> Option<&[mez_agent::instructions::DiscoveredInstructionFile]> {
        self.agent
            .pane_instruction_files
            .get(pane_id)
            .map(Vec::as_slice)
    }

    /// Replaces project instruction files discovered for one pane.
    pub(crate) fn set_pane_agent_instruction_files(
        &mut self,
        pane_id: impl Into<String>,
        files: Vec<mez_agent::instructions::DiscoveredInstructionFile>,
    ) {
        let pane_id = pane_id.into();
        if files.is_empty() {
            self.agent.pane_instruction_files.remove(&pane_id);
        } else {
            self.agent.pane_instruction_files.insert(pane_id, files);
        }
    }

    /// Clears pane-scoped instruction discovery during pane teardown.
    pub(crate) fn clear_pane_agent_instruction_files(&mut self, pane_id: &str) {
        self.agent.pane_instruction_files.remove(pane_id);
    }

    /// Returns runtime lineage metadata for one child agent.
    pub(crate) fn subagent_lineage(&self, agent_id: &str) -> Option<&RuntimeSubagentLineage> {
        self.agent.subagent_lineage.get(agent_id)
    }

    /// Records runtime lineage metadata for one child agent.
    pub(crate) fn set_subagent_lineage(
        &mut self,
        agent_id: impl Into<String>,
        lineage: RuntimeSubagentLineage,
    ) {
        self.agent.subagent_lineage.insert(agent_id.into(), lineage);
    }

    /// Counts direct active children of one parent agent.
    pub(crate) fn active_direct_subagent_count_for(&self, parent_agent_id: &str) -> usize {
        self.agent
            .subagent_lineage
            .values()
            .filter(|lineage| lineage.parent_agent_id == parent_agent_id)
            .count()
    }

    /// Returns non-empty active subagent display names.
    pub(crate) fn active_subagent_display_names(&self) -> Vec<String> {
        self.agent
            .subagent_lineage
            .values()
            .filter(|lineage| !lineage.display_name.trim().is_empty())
            .map(|lineage| lineage.display_name.clone())
            .collect()
    }

    /// Returns one inherited subagent scope declaration.
    pub(crate) fn subagent_scope_declaration(
        &self,
        agent_id: &str,
    ) -> Option<SubagentScopeDeclaration> {
        self.agent
            .subagent_scope_declarations
            .get(agent_id)
            .cloned()
    }

    /// Records one inherited subagent scope declaration.
    pub(crate) fn set_subagent_scope_declaration(
        &mut self,
        agent_id: impl Into<String>,
        declaration: SubagentScopeDeclaration,
    ) {
        self.agent
            .subagent_scope_declarations
            .insert(agent_id.into(), declaration);
    }

    /// Reports whether an agent has lineage, declarations, or active scopes.
    pub(crate) fn has_subagent_authority_state(&self, agent_id: &str) -> bool {
        self.agent.subagent_lineage.contains_key(agent_id)
            || self
                .agent
                .subagent_scope_declarations
                .contains_key(agent_id)
            || !self
                .agent
                .subagent_scopes
                .active_write_scopes_for(agent_id)
                .is_empty()
    }

    /// Removes lineage, declarations, and active scopes for one agent.
    pub(crate) fn remove_subagent_authority_state(&mut self, agent_id: &str) {
        self.agent.subagent_lineage.remove(agent_id);
        self.agent.subagent_scope_declarations.remove(agent_id);
        self.agent.subagent_scopes.unregister(agent_id);
    }

    /// Returns active write scopes for one agent.
    pub(crate) fn active_subagent_write_scopes_for(&self, agent_id: &str) -> Vec<ActiveWriteScope> {
        self.agent.subagent_scopes.active_write_scopes_for(agent_id)
    }

    /// Returns the number of active subagent write scopes.
    pub(crate) fn active_subagent_write_scope_count(&self) -> usize {
        self.agent.subagent_scopes.active_write_scope_count()
    }

    /// Clears all runtime subagent authority state on session replacement.
    pub(crate) fn clear_all_subagent_authority_state(&mut self) {
        self.agent.subagent_lineage.clear();
        self.agent.subagent_scope_declarations.clear();
        self.agent.subagent_scopes = mez_agent::ScopeRegistry::default();
    }

    /// Registers active write scopes in crate-local regression tests.
    #[cfg(test)]
    pub(crate) fn register_subagent_write_scopes_for_tests(
        &mut self,
        agent_id: &str,
        mode: CooperationMode,
        write_scopes: &[String],
        serial_lock: Option<String>,
    ) -> Result<()> {
        self.agent
            .subagent_scopes
            .register(agent_id, mode, write_scopes, serial_lock)?;
        Ok(())
    }

    /// Reports whether one child agent retains lineage in regression tests.
    #[cfg(test)]
    pub(crate) fn has_subagent_lineage(&self, agent_id: &str) -> bool {
        self.agent.subagent_lineage.contains_key(agent_id)
    }

    /// Reports whether one child agent retains a scope declaration in tests.
    #[cfg(test)]
    pub(crate) fn has_subagent_scope_declaration(&self, agent_id: &str) -> bool {
        self.agent
            .subagent_scope_declarations
            .contains_key(agent_id)
    }

    /// Returns the joined dependency for one child turn.
    pub(crate) fn joined_subagent_dependency(
        &self,
        child_turn_id: &str,
    ) -> Option<&JoinedSubagentDependency> {
        self.agent.joined_subagent_dependencies.get(child_turn_id)
    }

    /// Reports whether one child turn is joined to a parent action.
    #[cfg(test)]
    pub(crate) fn has_joined_subagent_dependency(&self, child_turn_id: &str) -> bool {
        self.agent
            .joined_subagent_dependencies
            .contains_key(child_turn_id)
    }

    /// Records one child-to-parent join dependency.
    #[cfg(test)]
    pub(crate) fn insert_joined_subagent_dependency(
        &mut self,
        child_turn_id: impl Into<String>,
        dependency: JoinedSubagentDependency,
    ) {
        self.agent
            .joined_subagent_dependencies
            .insert(child_turn_id.into(), dependency);
    }

    /// Removes joined dependencies owned by one child agent.
    pub(crate) fn remove_joined_subagent_dependencies_for_agent(&mut self, child_agent_id: &str) {
        self.agent
            .joined_subagent_dependencies
            .retain(|_, dependency| dependency.child_agent_id != child_agent_id);
    }

    /// Clears all joined child dependencies on session replacement.
    pub(crate) fn clear_all_joined_subagent_dependencies(&mut self) {
        self.agent.joined_subagent_dependencies.clear();
    }

    /// Returns the joined child count to crate-local regression tests.
    #[cfg(test)]
    pub(crate) fn joined_subagent_dependency_count(&self) -> usize {
        self.agent.joined_subagent_dependencies.len()
    }

    /// Marks a terminal child as already claimed by execution-aware settlement.
    #[cfg(test)]
    pub(crate) fn mark_terminal_result_claimed_for_tests(&mut self, turn_id: &str) {
        self.agent.terminal_result_dispositions.insert(
            turn_id.to_string(),
            TerminalResultDisposition::RetainedByLoop,
        );
    }

    /// Reports whether one turn is waiting for an approval decision.
    pub(crate) fn agent_turn_has_blocked_approval(&self, turn_id: &str) -> bool {
        self.agent
            .blocked_agent_approval_refs
            .values()
            .any(|approval_ref| approval_ref.turn_id == turn_id)
    }

    /// Returns pending approval ids grouped by their owning turn.
    pub(crate) fn blocked_agent_approval_ids_by_turn(&self) -> BTreeMap<String, Vec<String>> {
        let mut approval_ids_by_turn = BTreeMap::<String, Vec<String>>::new();
        for (approval_id, approval_ref) in &self.agent.blocked_agent_approval_refs {
            approval_ids_by_turn
                .entry(approval_ref.turn_id.clone())
                .or_default()
                .push(approval_id.clone());
        }
        approval_ids_by_turn
    }

    /// Reports whether one blocked approval grants an exact sandbox bypass.
    #[cfg(test)]
    pub(crate) fn blocked_approval_grants_sandbox_bypass_for_tests(
        &self,
        approval_id: &str,
    ) -> bool {
        self.agent
            .blocked_agent_approval_refs
            .get(approval_id)
            .is_some_and(|approval_ref| approval_ref.sandbox_bypass_after_approval)
    }

    /// Removes every blocked approval continuation owned by one turn.
    pub(crate) fn clear_blocked_agent_approvals_for_turn(&mut self, turn_id: &str) {
        self.agent
            .blocked_agent_approval_refs
            .retain(|_, approval_ref| approval_ref.turn_id != turn_id);
    }

    /// Clears all blocked approval continuations on session replacement.
    pub(crate) fn clear_all_blocked_agent_approval_refs(&mut self) {
        self.agent.blocked_agent_approval_refs.clear();
    }

    /// Grants one exact action a single unsandboxed dispatch after approval.
    pub(crate) fn grant_sandbox_bypass_after_approval(
        &mut self,
        turn_id: impl Into<String>,
        action_id: impl Into<String>,
    ) {
        self.agent
            .sandbox_bypass_after_approval
            .insert((turn_id.into(), action_id.into()));
    }

    /// Consumes an approved sandbox bypass so it cannot authorize a replay.
    pub(crate) fn take_sandbox_bypass_after_approval(
        &mut self,
        turn_id: &str,
        action_id: &str,
    ) -> bool {
        self.agent
            .sandbox_bypass_after_approval
            .remove(&(turn_id.to_string(), action_id.to_string()))
    }

    /// Activates or reuses the exact approved bypass for one action.
    pub(crate) fn activate_sandbox_bypass_after_approval(
        &mut self,
        turn_id: &str,
        action_id: &str,
    ) -> bool {
        let identity = (turn_id.to_string(), action_id.to_string());
        if self.agent.active_sandbox_bypasses.contains(&identity) {
            return true;
        }
        if !self.take_sandbox_bypass_after_approval(turn_id, action_id) {
            return false;
        }
        self.agent.active_sandbox_bypasses.insert(identity);
        true
    }

    /// Reports whether one exact approved bypass is active during dispatch.
    pub(crate) fn sandbox_bypass_active_for_action(&self, turn_id: &str, action_id: &str) -> bool {
        self.agent
            .active_sandbox_bypasses
            .contains(&(turn_id.to_string(), action_id.to_string()))
    }

    /// Clears any pending or active bypass for one settled action.
    pub(crate) fn clear_sandbox_bypass_for_action(&mut self, turn_id: &str, action_id: &str) {
        let identity = (turn_id.to_string(), action_id.to_string());
        self.agent.sandbox_bypass_after_approval.remove(&identity);
        self.agent.active_sandbox_bypasses.remove(&identity);
    }

    /// Reports whether one managed macro child is registered.
    #[cfg(test)]
    pub(crate) fn has_macro_managed_subagent(&self, agent_id: &str) -> bool {
        self.agent
            .macro_managed_subagent_agents
            .contains_key(agent_id)
    }

    /// Returns managed macro child ids to crate-local regression tests.
    #[cfg(test)]
    pub(crate) fn macro_managed_subagent_ids(&self) -> Vec<String> {
        self.agent
            .macro_managed_subagent_agents
            .keys()
            .cloned()
            .collect()
    }

    /// Returns one active macro run to crate-local regression tests.
    #[cfg(test)]
    pub(crate) fn macro_run_for_tests(&self, parent_turn_id: &str) -> Option<&MacroRunState> {
        self.agent.macro_runs_by_parent_turn.get(parent_turn_id)
    }

    /// Reports whether one parent macro run remains active.
    #[cfg(test)]
    pub(crate) fn has_macro_run(&self, parent_turn_id: &str) -> bool {
        self.agent
            .macro_runs_by_parent_turn
            .contains_key(parent_turn_id)
    }

    /// Reports whether one parent turn owns an active routed workflow.
    pub(crate) fn has_active_routed_workflow(&self, parent_turn_id: &str) -> bool {
        self.agent
            .routed_workflows_by_parent_turn
            .get(parent_turn_id)
            .is_some_and(|workflow| !workflow.phase.is_terminal())
    }

    /// Reports whether one routed parent is still waiting for its worker result.
    pub(crate) fn routed_workflow_waits_for_worker_result(&self, parent_turn_id: &str) -> bool {
        self.agent
            .routed_workflows_by_parent_turn
            .get(parent_turn_id)
            .is_some_and(|workflow| {
                workflow.phase
                    == mez_agent::routed_workflow::RoutedWorkflowPhase::WaitingForWorkerResult
            })
    }

    /// Returns one active routed workflow to crate-local regression tests.
    #[cfg(test)]
    pub(crate) fn routed_workflow_for_tests(
        &self,
        parent_turn_id: &str,
    ) -> Option<&RoutedWorkflowState> {
        self.agent
            .routed_workflows_by_parent_turn
            .get(parent_turn_id)
    }

    /// Injects one routed setup failure immediately after worker spawn.
    #[cfg(test)]
    pub(crate) fn fail_next_routed_worker_after_spawn_for_tests(&mut self) {
        self.agent.fail_routed_worker_after_spawn = true;
    }

    /// Injects one routed child trace failure after scheduler publication.
    #[cfg(test)]
    pub(crate) fn fail_next_routed_child_enqueue_trace_for_tests(&mut self) {
        self.agent.fail_routed_child_enqueue_trace = true;
    }

    /// Injects one `/fork` setup failure after durable persistence succeeds.
    #[cfg(test)]
    pub(crate) fn fail_next_agent_fork_after_persistence_for_tests(&mut self) {
        self.agent.fail_agent_fork_after_persistence = true;
    }

    /// Consumes the test-only post-persistence `/fork` failure injection.
    #[cfg(test)]
    pub(crate) fn take_agent_fork_after_persistence_failure_for_tests(&mut self) -> bool {
        std::mem::take(&mut self.agent.fail_agent_fork_after_persistence)
    }

    /// Injects one routed loop continuation queue failure.
    #[cfg(test)]
    pub(crate) fn fail_next_routed_loop_continuation_queue_for_tests(&mut self) {
        self.agent.fail_routed_loop_continuation_queue = true;
    }

    /// Consumes the test-only routed continuation queue failure injection.
    #[cfg(test)]
    pub(crate) fn take_routed_loop_continuation_queue_failure_for_tests(&mut self) -> bool {
        std::mem::take(&mut self.agent.fail_routed_loop_continuation_queue)
    }

    /// Injects one routed parent continuation trace failure.
    #[cfg(test)]
    pub(crate) fn fail_next_routed_parent_continuation_trace_for_tests(&mut self) {
        self.agent.fail_routed_parent_continuation_trace = true;
    }

    /// Returns the parent macro turn for one child step turn.
    #[cfg(test)]
    pub(crate) fn macro_parent_turn_for_child(&self, child_turn_id: &str) -> Option<&String> {
        self.agent.macro_run_by_child_turn.get(child_turn_id)
    }

    /// Returns the parent-agent route for one spawned child turn.
    pub(crate) fn subagent_task_parent(&self, turn_id: &str) -> Option<String> {
        self.agent.subagent_task_routes.get(turn_id).cloned()
    }

    /// Records the parent-agent route for one spawned child turn.
    pub(crate) fn set_subagent_task_parent(
        &mut self,
        turn_id: impl Into<String>,
        parent_agent_id: impl Into<String>,
    ) {
        self.agent
            .subagent_task_routes
            .insert(turn_id.into(), parent_agent_id.into());
    }

    /// Removes the parent-agent route for one spawned child turn.
    pub(crate) fn remove_subagent_task_parent(&mut self, turn_id: &str) {
        self.agent.subagent_task_routes.remove(turn_id);
    }

    /// Removes every child-turn route owned by one parent agent.
    pub(crate) fn remove_subagent_task_routes_for_parent(&mut self, parent_agent_id: &str) {
        self.agent
            .subagent_task_routes
            .retain(|_, parent| parent != parent_agent_id);
    }

    /// Records a window as reserved for subagent panes.
    pub(crate) fn mark_subagent_window(&mut self, window_id: impl Into<String>) {
        self.agent.subagent_window_ids.insert(window_id.into());
    }

    /// Reports whether a window is reserved for subagent panes.
    pub(crate) fn is_subagent_window(&self, window_id: &str) -> bool {
        self.agent.subagent_window_ids.contains(window_id)
    }

    /// Returns all currently reserved subagent window ids.
    pub(crate) fn subagent_window_ids(&self) -> Vec<String> {
        self.agent.subagent_window_ids.iter().cloned().collect()
    }

    /// Retains only subagent windows still present in the mux session.
    pub(crate) fn retain_live_subagent_windows(&mut self, live_window_ids: &BTreeSet<String>) {
        self.agent
            .subagent_window_ids
            .retain(|window_id| live_window_ids.contains(window_id));
    }

    /// Removes one deferred terminal-pane close marker.
    pub(crate) fn clear_terminal_subagent_pane_close(&mut self, pane_id: &str) -> bool {
        self.agent
            .pending_terminal_subagent_pane_closes
            .remove(pane_id)
    }

    /// Marks one terminal subagent pane for deferred close in focused tests.
    #[cfg(test)]
    pub(crate) fn mark_terminal_subagent_pane_close_for_tests(&mut self, pane_id: &str) {
        self.agent
            .pending_terminal_subagent_pane_closes
            .insert(pane_id.to_string());
    }

    /// Reports whether one terminal subagent pane is awaiting deferred close.
    #[cfg(test)]
    pub(crate) fn has_pending_terminal_subagent_pane_close(&self, pane_id: &str) -> bool {
        self.agent
            .pending_terminal_subagent_pane_closes
            .contains(pane_id)
    }

    /// Clears all subagent routing and placement state on session replacement.
    pub(crate) fn clear_subagent_placement_state(&mut self) {
        self.agent.subagent_task_routes.clear();
        self.agent.subagent_window_ids.clear();
        self.agent.pending_terminal_subagent_pane_closes.clear();
    }

    /// Replaces all configured subagent placement and delegation limits.
    pub(crate) fn configure_subagent_policy(
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
    pub(crate) fn max_subagent_panes_per_window(&self) -> usize {
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
    pub(crate) fn cached_provider_model_catalog(
        &self,
        provider_id: &str,
    ) -> Option<RuntimeModelCatalog> {
        self.agent
            .provider_model_catalog_cache
            .get(provider_id)
            .cloned()
    }

    /// Replaces the cached live model catalog for one provider.
    pub(crate) fn cache_provider_model_catalog(
        &mut self,
        provider_id: impl Into<String>,
        catalog: RuntimeModelCatalog,
    ) -> Result<()> {
        let provider_id = provider_id.into();
        self.integration
            .provider_registry_mut()
            .rematerialize_profiles_for_provider(&provider_id, Some(catalog.catalog()))
            .map_err(|error| MezError::config(error.to_string()))?;
        self.agent
            .provider_model_catalog_cache
            .insert(provider_id, catalog);
        Ok(())
    }

    /// Invalidates one provider's cached live model catalog.
    pub(crate) fn remove_cached_provider_model_catalog(&mut self, provider_id: &str) -> Result<()> {
        self.agent.provider_model_catalog_cache.remove(provider_id);
        self.integration
            .provider_registry_mut()
            .rematerialize_profiles_for_provider(provider_id, None)
            .map_err(|error| MezError::config(error.to_string()))
    }

    /// Invalidates all live model catalogs after configuration changes.
    pub(crate) fn clear_provider_model_catalog_cache(&mut self) {
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
    pub(crate) fn agent_token_usage_for_pane(
        &self,
        pane_id: &str,
    ) -> BTreeMap<ModelTokenUsageKey, ModelTokenUsage> {
        self.agent
            .agent_token_usage_by_pane
            .get(pane_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Clears cumulative token usage for one pane without changing conversation totals.
    pub(crate) fn reset_agent_token_usage_for_pane(&mut self, pane_id: &str) -> bool {
        self.agent
            .agent_token_usage_by_pane
            .remove(pane_id)
            .is_some()
    }

    /// Returns cumulative token usage for one conversation.
    pub(crate) fn agent_token_usage_for_conversation(
        &self,
        conversation_id: &str,
    ) -> BTreeMap<ModelTokenUsageKey, ModelTokenUsage> {
        self.agent
            .agent_token_usage_by_conversation
            .get(conversation_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Returns the latest concrete execution-model request sample.
    pub(crate) fn agent_latest_request_usage(
        &self,
        conversation_id: &str,
    ) -> Option<&mez_agent::LatestModelRequestUsage> {
        self.agent
            .agent_latest_request_usage_by_conversation
            .get(conversation_id)
    }

    /// Replaces the latest concrete execution-model request sample on restore.
    pub(crate) fn restore_agent_latest_request_usage(
        &mut self,
        conversation_id: &str,
        usage: Option<mez_agent::LatestModelRequestUsage>,
    ) {
        if let Some(usage) = usage {
            self.agent
                .agent_latest_request_usage_by_conversation
                .insert(conversation_id.to_string(), usage);
        } else {
            self.agent
                .agent_latest_request_usage_by_conversation
                .remove(conversation_id);
        }
    }

    /// Returns the latest immutable-context continuity comparison.
    pub(crate) fn agent_context_continuity(
        &self,
        conversation_id: &str,
    ) -> Option<&mez_agent::ContextContinuityDiagnostics> {
        self.agent
            .agent_context_continuity_by_conversation
            .get(conversation_id)
    }

    /// Records one finalized provider-bound context comparison.
    pub(crate) fn record_agent_context_continuity(
        &mut self,
        conversation_id: &str,
        diagnostics: mez_agent::ContextContinuityDiagnostics,
    ) {
        self.agent
            .agent_context_continuity_snapshot_by_conversation
            .insert(conversation_id.to_string(), diagnostics.snapshot.clone());
        self.agent
            .agent_context_continuity_by_conversation
            .insert(conversation_id.to_string(), diagnostics);
    }

    /// Aggregates non-zero token usage across all agent conversations.
    pub(crate) fn total_agent_token_usage_by_model(
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
    pub(crate) fn replace_restored_agent_token_usage(
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
    pub(crate) fn merge_restored_agent_token_usage(
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
    pub(crate) fn restore_agent_context_usage(
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
    pub(crate) fn agent_context_usage_display(&self, conversation_id: &str) -> Option<String> {
        self.agent
            .agent_context_usage_by_conversation
            .get(conversation_id)
            .cloned()
    }

    /// Returns the structured context usage for one conversation.
    pub(crate) fn agent_context_usage_snapshot(
        &self,
        conversation_id: &str,
    ) -> Option<mez_agent::AgentContextUsageSnapshot> {
        self.agent
            .agent_context_usage_snapshot_by_conversation
            .get(conversation_id)
            .copied()
    }

    /// Reports when model-backed compaction started for one pane.
    pub(crate) fn agent_compaction_started_at(&self, pane_id: &str) -> Option<u64> {
        self.agent.agent_compacting_panes.get(pane_id).copied()
    }

    /// Reports when durable-memory generation started for one pane.
    pub(crate) fn agent_remember_started_at(&self, pane_id: &str) -> Option<u64> {
        self.agent.agent_remembering_panes.get(pane_id).copied()
    }

    /// Reports whether one pane is compacting its model context.
    pub(crate) fn agent_is_compacting(&self, pane_id: &str) -> bool {
        self.agent.agent_compacting_panes.contains_key(pane_id)
    }

    /// Reports whether one pane is generating durable memories.
    pub(crate) fn agent_is_remembering(&self, pane_id: &str) -> bool {
        self.agent.agent_remembering_panes.contains_key(pane_id)
    }

    /// Counts background model operations attached to the provided panes.
    pub(crate) fn active_agent_background_work_count(&self, pane_ids: &[String]) -> usize {
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
    pub(crate) fn queue_agent_compaction_task(&mut self, task: RuntimeAgentCompactionTask) {
        let pane_id = task.pane_id.clone();
        self.agent
            .agent_compacting_panes
            .insert(pane_id.clone(), current_unix_seconds().max(1));
        self.agent
            .pending_agent_compaction_tasks
            .insert(pane_id, task);
    }

    /// Returns pane ids with queued model-backed compaction work.
    pub(crate) fn pending_agent_compaction_task_ids(&self) -> Vec<String> {
        self.agent
            .pending_agent_compaction_tasks
            .keys()
            .cloned()
            .collect()
    }

    /// Returns turns waiting for output-limit recovery compaction.
    pub(crate) fn agent_compaction_resume_ids(&self) -> Vec<String> {
        self.agent
            .pending_agent_compaction_tasks
            .values()
            .chain(self.agent.claimed_agent_compaction_tasks.values())
            .filter_map(|task| task.resume_turn_id.clone())
            .collect()
    }

    /// Removes one pending compaction task for provider construction.
    pub(crate) fn take_pending_agent_compaction_task(
        &mut self,
        pane_id: &str,
    ) -> Option<RuntimeAgentCompactionTask> {
        self.agent.pending_agent_compaction_tasks.remove(pane_id)
    }

    /// Records that a provider worker owns one compaction task.
    pub(crate) fn claim_agent_compaction_task_state(
        &mut self,
        pane_id: impl Into<String>,
        task: RuntimeAgentCompactionTask,
    ) {
        self.agent
            .claimed_agent_compaction_tasks
            .insert(pane_id.into(), task);
    }

    /// Finishes claimed compaction state and clears its pane activity marker.
    pub(crate) fn finish_agent_compaction_task(
        &mut self,
        pane_id: &str,
    ) -> Option<RuntimeAgentCompactionTask> {
        let task = self.agent.claimed_agent_compaction_tasks.remove(pane_id);
        self.agent.agent_compacting_panes.remove(pane_id);
        task
    }

    /// Removes all compaction state after provider failure.
    pub(crate) fn fail_agent_compaction_task(
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
    pub(crate) fn queue_agent_remember_task(&mut self, task: RuntimeAgentRememberTask) {
        let pane_id = task.pane_id.clone();
        self.agent
            .agent_remembering_panes
            .insert(pane_id.clone(), current_unix_seconds().max(1));
        self.agent
            .pending_agent_remember_tasks
            .insert(pane_id, task);
    }

    /// Returns pane ids with queued durable-memory generation work.
    pub(crate) fn pending_agent_remember_task_ids(&self) -> Vec<String> {
        self.agent
            .pending_agent_remember_tasks
            .keys()
            .cloned()
            .collect()
    }

    /// Removes one pending durable-memory task for provider construction.
    pub(crate) fn take_pending_agent_remember_task(
        &mut self,
        pane_id: &str,
    ) -> Option<RuntimeAgentRememberTask> {
        self.agent.pending_agent_remember_tasks.remove(pane_id)
    }

    /// Records that a provider worker owns one durable-memory task.
    pub(crate) fn claim_agent_remember_task_state(
        &mut self,
        pane_id: impl Into<String>,
        task: RuntimeAgentRememberTask,
    ) {
        self.agent
            .claimed_agent_remember_tasks
            .insert(pane_id.into(), task);
    }

    /// Finishes claimed durable-memory state and clears pane activity.
    pub(crate) fn finish_agent_remember_task(
        &mut self,
        pane_id: &str,
    ) -> Option<RuntimeAgentRememberTask> {
        let task = self.agent.claimed_agent_remember_tasks.remove(pane_id);
        self.agent.agent_remembering_panes.remove(pane_id);
        task
    }

    /// Removes all durable-memory generation state after provider failure.
    pub(crate) fn fail_agent_remember_task(&mut self, pane_id: &str) -> bool {
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

    /// Returns one queued compaction task for focused state-boundary tests.
    #[cfg(test)]
    pub(crate) fn pending_agent_compaction_task_mut_for_tests(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut RuntimeAgentCompactionTask> {
        self.agent.pending_agent_compaction_tasks.get_mut(pane_id)
    }

    /// Reports whether one provider turn is queued for dispatch.
    pub(crate) fn agent_provider_task_is_pending(&self, turn_id: &str) -> bool {
        self.agent.pending_agent_provider_tasks.contains(turn_id)
    }

    /// Reports whether one provider turn is claimed by a worker.
    pub(crate) fn agent_provider_task_is_claimed(&self, turn_id: &str) -> bool {
        self.agent
            .claimed_agent_provider_tasks
            .contains_key(turn_id)
    }

    /// Reports whether a provider turn is queued or claimed.
    pub(crate) fn agent_provider_task_is_owned(&self, turn_id: &str) -> bool {
        self.agent_provider_task_is_pending(turn_id) || self.agent_provider_task_is_claimed(turn_id)
    }

    /// Queues one provider turn when it is not already pending.
    pub(crate) fn queue_agent_provider_task(&mut self, turn_id: impl Into<String>) -> bool {
        self.agent
            .pending_agent_provider_tasks
            .insert(turn_id.into())
    }

    /// Removes one pending provider turn.
    pub(crate) fn remove_pending_agent_provider_task(&mut self, turn_id: &str) -> bool {
        self.agent.pending_agent_provider_tasks.remove(turn_id)
    }

    /// Removes one claimed provider turn.
    pub(crate) fn remove_claimed_agent_provider_task(
        &mut self,
        turn_id: &str,
    ) -> Option<RuntimeAgentProviderClaim> {
        self.agent.claimed_agent_provider_tasks.remove(turn_id)
    }

    /// Marks one validated provider turn as waiting on persistence settlement.
    pub(crate) fn mark_agent_provider_persistence_pending(&mut self, turn_id: &str) -> bool {
        self.agent
            .pending_agent_provider_persistence
            .insert(turn_id.to_string())
    }

    /// Clears one completed or cancelled provider persistence settlement.
    pub(crate) fn clear_agent_provider_persistence_pending(&mut self, turn_id: &str) -> bool {
        self.agent
            .pending_agent_provider_persistence
            .remove(turn_id)
    }

    /// Iterates over turns whose progress is owned by the persistence worker.
    pub(crate) fn agent_provider_persistence_progress_turn_ids(
        &self,
    ) -> impl Iterator<Item = &String> {
        self.agent.pending_agent_provider_persistence.iter()
    }

    /// Returns the complete rejected OpenAI request size owned by one claim.
    pub(crate) fn claimed_agent_provider_openai_request_bytes(
        &self,
        turn_id: &str,
    ) -> Option<usize> {
        self.agent
            .claimed_agent_provider_tasks
            .get(turn_id)
            .and_then(|claim| claim.openai_request_bytes)
    }

    /// Returns the OpenAI Responses streaming mode owned by one claimed request.
    pub(crate) fn claimed_agent_provider_openai_request_stream(
        &self,
        turn_id: &str,
    ) -> Option<bool> {
        self.agent
            .claimed_agent_provider_tasks
            .get(turn_id)
            .and_then(|claim| claim.openai_request_stream)
    }

    /// Clears all queued and claimed provider work for session replacement.
    pub(crate) fn clear_agent_provider_task_ownership(&mut self) {
        let pane_ids = self
            .agent
            .claimed_agent_provider_tasks
            .keys()
            .filter_map(|turn_id| {
                self.agent_turn_ledger()
                    .turns()
                    .iter()
                    .find(|turn| &turn.turn_id == turn_id)
                    .map(|turn| turn.pane_id.clone())
            })
            .collect::<std::collections::BTreeSet<_>>();
        self.agent.pending_agent_provider_tasks.clear();
        self.agent.claimed_agent_provider_tasks.clear();
        for pane_id in pane_ids {
            let _ = self.discard_agent_streaming_say_presentation(&pane_id, None);
            let _ = self.clear_agent_shell_output_status_line(&pane_id);
        }
    }

    /// Returns the effective model profile retained for one turn.
    pub(crate) fn agent_turn_model_profile(&self, turn_id: &str) -> Option<&ModelProfile> {
        self.agent.agent_turn_model_profiles.get(turn_id)
    }

    /// Replaces the effective model profile retained for one turn.
    pub(crate) fn set_agent_turn_model_profile(
        &mut self,
        turn_id: impl Into<String>,
        profile: ModelProfile,
    ) {
        self.agent
            .agent_turn_model_profiles
            .insert(turn_id.into(), profile);
    }

    /// Removes the effective model profile retained for one turn.
    pub(crate) fn remove_agent_turn_model_profile(
        &mut self,
        turn_id: &str,
    ) -> Option<ModelProfile> {
        self.agent.agent_turn_routing_applied.remove(turn_id);
        self.agent
            .agent_turn_configured_model_profiles
            .remove(turn_id);
        self.agent.agent_turn_model_profiles.remove(turn_id)
    }

    /// Clears all retained turn model profiles for session replacement.
    pub(crate) fn clear_agent_turn_model_profiles(&mut self) {
        self.agent.agent_turn_model_profiles.clear();
        self.agent.agent_turn_configured_model_profiles.clear();
        self.agent.agent_turn_routing_applied.clear();
    }

    /// Reports whether automatic routing has already been applied to a turn.
    pub(crate) fn agent_turn_routing_applied(&self, turn_id: &str) -> bool {
        self.agent.agent_turn_routing_applied.contains(turn_id)
    }

    /// Marks a turn's automatic routing decision as applied.
    pub(crate) fn mark_agent_turn_routing_applied(&mut self, turn_id: impl Into<String>) -> bool {
        self.agent.agent_turn_routing_applied.insert(turn_id.into())
    }

    /// Selects how a routing decision applies to the owning runtime turn.
    pub(crate) fn auto_sizing_routing_policy_for_turn(
        &self,
        turn: &AgentTurnRecord,
    ) -> AutoSizingRoutingPolicy {
        if self.subagent_lineage(&turn.agent_id).is_some() {
            AutoSizingRoutingPolicy::InPlace
        } else {
            self.agent_root_routing_policy_for_pane(&turn.pane_id)
        }
    }

    /// Clears correction attempts and action histories for one completed turn.
    pub(crate) fn clear_agent_action_bookkeeping_for_turn(&mut self, turn_id: &str) {
        self.clear_agent_failure_feedback_attempts_for_turn(turn_id);
        self.clear_agent_execution_group_ownership_for_turn(turn_id);
        self.clear_agent_issue_query_freshness_for_turn(turn_id);
        self.agent.sandbox_failure_assessments.remove(turn_id);
        self.agent
            .sandbox_fallback_audits
            .retain(|(owner_turn_id, _), _| owner_turn_id != turn_id);
        self.agent
            .sandbox_bypass_after_approval
            .retain(|(owner_turn_id, _)| owner_turn_id != turn_id);
        self.agent
            .active_sandbox_bypasses
            .retain(|(owner_turn_id, _)| owner_turn_id != turn_id);
        self.agent.agent_turn_shell_dispatch_history.remove(turn_id);
        self.clear_pending_shell_dispatch_blocked_recovery_attempts_for_turn(turn_id);
        self.agent.agent_turn_network_action_history.remove(turn_id);
        self.agent
            .agent_turn_config_change_successes
            .remove(turn_id);
        self.agent
            .agent_pre_shell_hook_completions
            .retain(|completion| completion.turn_id != turn_id);
        self.agent
            .pending_apply_patch_phases
            .retain(|state_key, _| !state_key.starts_with(&format!("{turn_id}/")));
    }

    /// Clears all action bookkeeping when the live session is replaced.
    pub(crate) fn clear_all_agent_action_bookkeeping(&mut self) {
        self.agent.agent_turn_failure_feedback_attempts.clear();
        self.agent.agent_turn_output_limit_recovery_attempts.clear();
        self.agent.agent_turn_interaction_kinds.clear();
        self.agent.sandbox_failure_assessments.clear();
        self.agent.sandbox_fallback_audits.clear();
        self.agent.agent_execution_groups_by_turn.clear();
        self.agent.agent_provider_tool_calls_by_turn.clear();
        self.agent.agent_turn_issue_query_freshness.clear();
        self.agent.agent_turn_shell_dispatch_history.clear();
        self.agent
            .pending_shell_dispatch_blocked_recovery_attempts
            .clear();
        self.agent.agent_turn_network_action_history.clear();
        self.agent.agent_turn_config_change_successes.clear();
        self.agent.agent_pre_shell_hook_completions.clear();
        self.agent.pending_apply_patch_phases.clear();
    }

    /// Records one fresh foreign foreground observation and returns its stable
    /// confirmation count. A changed process, interaction epoch, or foreign
    /// process group starts a new stabilization run.
    pub(crate) fn record_pending_shell_dispatch_blocked_recovery_observation(
        &mut self,
        turn_id: &str,
        action_id: &str,
        primary_process_id: u32,
        interaction_generation: u64,
        foreign_process_group_id: u32,
    ) -> usize {
        let state = self
            .agent
            .pending_shell_dispatch_blocked_recovery_attempts
            .entry((turn_id.to_string(), action_id.to_string()))
            .or_insert_with(|| RuntimeShellDispatchBlockedRecoveryState {
                started_at_unix_ms: current_unix_millis(),
                primary_process_id,
                interaction_generation,
                foreign_process_group_id,
                confirmations: 0,
            });
        if state.primary_process_id != primary_process_id
            || state.interaction_generation != interaction_generation
            || state.foreign_process_group_id != foreign_process_group_id
        {
            *state = RuntimeShellDispatchBlockedRecoveryState {
                started_at_unix_ms: current_unix_millis(),
                primary_process_id,
                interaction_generation,
                foreign_process_group_id,
                confirmations: 0,
            };
        }
        state.confirmations = state.confirmations.saturating_add(1);
        state.confirmations
    }

    /// Returns recorded foreground-process recovery observations for one action.
    pub(crate) fn pending_shell_dispatch_blocked_recovery_attempts(
        &self,
        turn_id: &str,
        action_id: &str,
    ) -> usize {
        self.agent
            .pending_shell_dispatch_blocked_recovery_attempts
            .get(&(turn_id.to_string(), action_id.to_string()))
            .map(|state| state.confirmations)
            .unwrap_or_default()
    }

    /// Reports whether fresh foreign foreground observations have exceeded the
    /// bounded fail-closed recovery deadline for one pending shell action.
    pub(crate) fn pending_shell_dispatch_blocked_recovery_deadline_exhausted(
        &self,
        turn_id: &str,
        action_id: &str,
    ) -> bool {
        const RECOVERY_DEADLINE_MS: u64 = 1_500;
        self.agent
            .pending_shell_dispatch_blocked_recovery_attempts
            .get(&(turn_id.to_string(), action_id.to_string()))
            .is_some_and(|state| {
                current_unix_millis().saturating_sub(state.started_at_unix_ms)
                    >= RECOVERY_DEADLINE_MS
            })
    }

    /// Clears foreground-process recovery observations for one action.
    pub(crate) fn clear_pending_shell_dispatch_blocked_recovery_attempt(
        &mut self,
        turn_id: &str,
        action_id: &str,
    ) {
        self.agent
            .pending_shell_dispatch_blocked_recovery_attempts
            .remove(&(turn_id.to_string(), action_id.to_string()));
        self.clear_shell_dispatch_recovery_observations_for_action(turn_id, action_id);
    }

    /// Clears foreground-process recovery observations for one completed turn.
    pub(crate) fn clear_pending_shell_dispatch_blocked_recovery_attempts_for_turn(
        &mut self,
        turn_id: &str,
    ) {
        self.agent
            .pending_shell_dispatch_blocked_recovery_attempts
            .retain(|(owner_turn_id, _), _| owner_turn_id != turn_id);
        self.clear_shell_dispatch_recovery_observations_for_turn(turn_id);
    }

    /// Clears provider-execution identities and action ownership for one turn.
    pub(crate) fn clear_agent_execution_group_ownership_for_turn(&mut self, turn_id: &str) {
        self.agent.agent_execution_groups_by_turn.remove(turn_id);
        self.agent.agent_provider_tool_calls_by_turn.remove(turn_id);
    }

    /// Clears successful issue-query freshness state for one logical turn.
    pub(crate) fn clear_agent_issue_query_freshness_for_turn(&mut self, turn_id: &str) {
        self.agent.agent_turn_issue_query_freshness.remove(turn_id);
    }

    /// Reports whether one pre-shell hook already completed for an action.
    pub(crate) fn agent_pre_shell_hook_completed(
        &self,
        continuation: &PendingFocusedShellHookContinuation,
        hook_id: &str,
    ) -> bool {
        self.agent
            .agent_pre_shell_hook_completions
            .contains(&RuntimeAgentPreShellHookCompletion {
                turn_id: continuation.turn_id.clone(),
                action_id: continuation.action_id.clone(),
                phase_command_sha256: continuation.phase_command_sha256.clone(),
                hook_id: hook_id.to_string(),
            })
    }

    /// Records one completed pre-shell hook for an action.
    pub(crate) fn record_agent_pre_shell_hook_completed(
        &mut self,
        continuation: &PendingFocusedShellHookContinuation,
        hook_id: &str,
    ) {
        self.agent
            .agent_pre_shell_hook_completions
            .insert(RuntimeAgentPreShellHookCompletion {
                turn_id: continuation.turn_id.clone(),
                action_id: continuation.action_id.clone(),
                phase_command_sha256: continuation.phase_command_sha256.clone(),
                hook_id: hook_id.to_string(),
            });
    }

    /// Clears completed pre-shell hook records for one turn.
    pub(crate) fn clear_agent_pre_shell_hook_completions_for_turn(&mut self, turn_id: &str) {
        self.agent
            .agent_pre_shell_hook_completions
            .retain(|completion| completion.turn_id != turn_id);
    }

    /// Returns the bounded model-correction retry limit.
    pub(crate) fn agent_action_failure_retry_limit(&self) -> usize {
        self.agent.agent_action_failure_retry_limit.max(1)
    }

    /// Replaces the bounded model-correction retry limit.
    pub(crate) fn set_agent_action_failure_retry_limit(&mut self, limit: usize) {
        self.agent.agent_action_failure_retry_limit = limit;
    }

    /// Returns the total wall-clock budget snapshotted for each new turn.
    pub(crate) fn agent_turn_timeout_ms(&self) -> u64 {
        self.agent.agent_turn_timeout_ms.max(1)
    }

    /// Replaces the budget used when subsequently creating agent turns.
    pub(crate) fn set_agent_turn_timeout_ms(&mut self, timeout_ms: u64) {
        self.agent.agent_turn_timeout_ms = timeout_ms;
    }

    /// Returns the configured loop iteration limit.
    pub(crate) fn agent_loop_limit(&self) -> usize {
        self.agent.agent_loop_limit.max(1)
    }

    /// Replaces the configured loop iteration limit.
    pub(crate) fn set_agent_loop_limit(&mut self, limit: usize) {
        self.agent.agent_loop_limit = limit;
    }

    /// Returns loop controller state for one pane.
    pub(crate) fn agent_loop_state(&self, pane_id: &str) -> Option<&RuntimeAgentLoopState> {
        let loop_id = self.agent.agent_loop_by_pane.get(pane_id)?;
        self.agent.agent_loops_by_id.get(loop_id)
    }

    /// Returns loop controller state for one stable logical loop id.
    pub(crate) fn agent_loop_state_by_id(&self, loop_id: &str) -> Option<&RuntimeAgentLoopState> {
        self.agent.agent_loops_by_id.get(loop_id)
    }

    /// Returns mutable loop controller state indexed by an invoking or execution pane.
    pub(crate) fn agent_loop_state_mut(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut RuntimeAgentLoopState> {
        let loop_id = self.agent.agent_loop_by_pane.get(pane_id)?.clone();
        self.agent.agent_loops_by_id.get_mut(&loop_id)
    }

    /// Returns mutable loop controller state for one stable logical loop id.
    pub(crate) fn agent_loop_state_mut_by_id(
        &mut self,
        loop_id: &str,
    ) -> Option<&mut RuntimeAgentLoopState> {
        self.agent.agent_loops_by_id.get_mut(loop_id)
    }

    /// Reports whether a pane has loop controller state.
    pub(crate) fn agent_loop_is_active(&self, pane_id: &str) -> bool {
        self.agent.agent_loop_by_pane.contains_key(pane_id)
    }

    /// Replaces loop controller state for one pane.
    pub(crate) fn insert_agent_loop_state(&mut self, state: RuntimeAgentLoopState) {
        self.agent
            .agent_loop_by_pane
            .insert(state.invoking_pane_id.clone(), state.loop_id.clone());
        self.agent
            .agent_loop_by_pane
            .insert(state.execution_pane_id.clone(), state.loop_id.clone());
        self.agent
            .agent_loops_by_id
            .insert(state.loop_id.clone(), state);
    }

    /// Removes loop controller state for one pane.
    pub(crate) fn remove_agent_loop_state(
        &mut self,
        pane_id: &str,
    ) -> Option<RuntimeAgentLoopState> {
        let loop_id = self.agent.agent_loop_by_pane.get(pane_id)?.clone();
        self.remove_agent_loop_state_by_id(&loop_id)
    }

    /// Removes loop controller state and every pane index for one logical loop.
    pub(crate) fn remove_agent_loop_state_by_id(
        &mut self,
        loop_id: &str,
    ) -> Option<RuntimeAgentLoopState> {
        let state = self.agent.agent_loops_by_id.remove(loop_id)?;
        self.agent
            .agent_loop_by_pane
            .retain(|_, indexed_loop_id| indexed_loop_id != loop_id);
        self.agent
            .agent_loop_turns
            .retain(|_, loop_turn| loop_turn.loop_id != loop_id);
        Some(state)
    }

    /// Returns loop-owned metadata for one turn.
    pub(crate) fn agent_loop_turn(&self, turn_id: &str) -> Option<&RuntimeAgentLoopTurn> {
        self.agent.agent_loop_turns.get(turn_id)
    }

    /// Records one loop-owned turn.
    pub(crate) fn insert_agent_loop_turn(
        &mut self,
        turn_id: String,
        loop_turn: RuntimeAgentLoopTurn,
    ) {
        self.agent.agent_loop_turns.insert(turn_id, loop_turn);
    }

    /// Removes loop-owned metadata for one turn.
    pub(crate) fn remove_agent_loop_turn(&mut self, turn_id: &str) -> Option<RuntimeAgentLoopTurn> {
        self.agent.agent_loop_turns.remove(turn_id)
    }

    /// Removes stale loop-owned turns for one pane.
    pub(crate) fn clear_agent_loop_turns_for_pane(&mut self, pane_id: &str) {
        self.agent
            .agent_loop_turns
            .retain(|_, loop_turn| loop_turn.pane_id != pane_id);
    }

    /// Returns the raw-context percentage retained after compaction.
    pub(crate) fn agent_compaction_raw_retention_percent(&self) -> usize {
        self.agent.agent_compaction_raw_retention_percent
    }

    /// Returns the configured host power policy for active agent turns.
    #[cfg(test)]
    pub(crate) fn active_turn_sleep_inhibition(&self) -> ActiveTurnSleepInhibition {
        self.agent.active_turn_sleep_inhibition
    }

    /// Replaces the configured host power policy for active agent turns.
    pub(crate) fn set_active_turn_sleep_inhibition(&mut self, policy: ActiveTurnSleepInhibition) {
        self.agent.active_turn_sleep_inhibition = policy;
        self.reconcile_active_turn_sleep_inhibition();
    }

    /// Reconciles the one daemon-wide host power lease from canonical Running
    /// ledger state rather than pane-local shell-session bookkeeping.
    pub(crate) fn reconcile_active_turn_sleep_inhibition(&mut self) {
        let runtime_is_live = matches!(
            self.session.lifecycle_state(),
            crate::runtime::RuntimeLifecycleState::Running
                | crate::runtime::RuntimeLifecycleState::Detached
        );
        let running = runtime_is_live
            && self
                .agent_turn_ledger()
                .turns()
                .iter()
                .any(|turn| turn.state == AgentTurnState::Running);
        let mode = if running {
            match self.agent.active_turn_sleep_inhibition {
                ActiveTurnSleepInhibition::Disabled => PowerInhibitionMode::Disabled,
                ActiveTurnSleepInhibition::System => PowerInhibitionMode::System,
                ActiveTurnSleepInhibition::SystemAndDisplay => {
                    PowerInhibitionMode::SystemAndDisplay
                }
            }
        } else {
            PowerInhibitionMode::Disabled
        };
        if self.agent.active_turn_power_inhibition.is_none()
            && mode != PowerInhibitionMode::Disabled
        {
            self.agent.active_turn_power_inhibition =
                Some(production_power_inhibition_controller());
        }
        if let Some(controller) = self.agent.active_turn_power_inhibition.as_mut() {
            controller.reconcile(mode);
        }
    }

    /// Returns the effective host power-inhibition state for focused tests.
    #[cfg(test)]
    pub(crate) fn active_turn_power_inhibition_state_for_tests(&self) -> PowerInhibitionState {
        self.agent
            .active_turn_power_inhibition
            .as_ref()
            .map(PowerInhibitionController::state)
            .unwrap_or(PowerInhibitionState::Inactive)
    }

    /// Installs a deterministic backend before exercising runtime lifecycle
    /// reconciliation in tests.
    #[cfg(test)]
    pub(crate) fn install_test_active_turn_power_inhibition_backend(&mut self) {
        self.agent.active_turn_power_inhibition = Some(PowerInhibitionController::new(Box::new(
            TestPowerInhibitionBackend::default(),
        )));
    }

    /// Replaces the raw-context percentage retained after compaction.
    pub(crate) fn set_agent_compaction_raw_retention_percent(&mut self, percent: usize) {
        self.agent.agent_compaction_raw_retention_percent = percent;
    }

    /// Returns the configured default auto-sizing policy.
    pub(crate) fn agent_auto_sizing(&self) -> &RuntimeAutoSizingConfig {
        &self.agent.agent_auto_sizing
    }

    /// Replaces the configured default auto-sizing policy.
    pub(crate) fn set_agent_auto_sizing(&mut self, config: RuntimeAutoSizingConfig) {
        self.agent.agent_auto_sizing = config;
    }

    /// Replaces the configured root-turn routing application policy.
    pub(crate) fn set_agent_root_routing_policy(&mut self, policy: AutoSizingRoutingPolicy) {
        self.agent.agent_root_routing_policy = policy;
    }

    /// Returns an explicit pane-local root-turn routing application policy.
    pub(crate) fn agent_root_routing_policy_override(
        &self,
        pane_id: &str,
    ) -> Option<AutoSizingRoutingPolicy> {
        self.agent
            .agent_root_routing_policy_overrides
            .get(pane_id)
            .copied()
    }

    /// Replaces or clears one pane-local root-turn routing application policy.
    pub(crate) fn set_agent_root_routing_policy_override(
        &mut self,
        pane_id: &str,
        policy: Option<AutoSizingRoutingPolicy>,
    ) {
        if let Some(policy) = policy {
            self.agent
                .agent_root_routing_policy_overrides
                .insert(pane_id.to_string(), policy);
        } else {
            self.agent
                .agent_root_routing_policy_overrides
                .remove(pane_id);
        }
    }

    /// Returns the effective root-turn routing application policy for one pane.
    pub(crate) fn agent_root_routing_policy_for_pane(
        &self,
        pane_id: &str,
    ) -> AutoSizingRoutingPolicy {
        self.agent_root_routing_policy_override(pane_id)
            .unwrap_or(self.agent.agent_root_routing_policy)
    }

    /// Replaces the router model profile in the default auto-sizing policy.
    pub(crate) fn set_agent_router_model_profile(&mut self, profile_name: &str) {
        self.agent.agent_auto_sizing.router_model_profile = profile_name.to_string();
    }

    /// Returns an explicit pane-local auto-sizing override.
    pub(crate) fn agent_auto_sizing_override(
        &self,
        pane_id: &str,
    ) -> Option<&RuntimeAutoSizingConfig> {
        self.agent.agent_auto_sizing_overrides.get(pane_id)
    }

    /// Replaces or clears one pane-local auto-sizing override.
    pub(crate) fn set_agent_auto_sizing_override(
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
    pub(crate) fn agent_auto_sizing_for_pane(&self, pane_id: &str) -> &RuntimeAutoSizingConfig {
        self.agent_auto_sizing_override(pane_id)
            .unwrap_or_else(|| self.agent_auto_sizing())
    }

    /// Returns the configured default provider-routing state.
    pub(crate) fn agent_default_routing(&self) -> bool {
        self.agent.agent_routing
    }

    /// Replaces the configured default provider-routing state.
    pub(crate) fn set_agent_default_routing(&mut self, enabled: bool) {
        self.agent.agent_routing = enabled;
    }

    /// Returns an explicit pane-local routing override.
    pub(crate) fn agent_routing_override(&self, pane_id: &str) -> Option<bool> {
        self.agent.agent_routing_overrides.get(pane_id).copied()
    }

    /// Replaces or clears one pane-local routing override.
    pub(crate) fn set_agent_routing_override(&mut self, pane_id: &str, enabled: Option<bool>) {
        if let Some(enabled) = enabled {
            self.agent
                .agent_routing_overrides
                .insert(pane_id.to_string(), enabled);
        } else {
            self.agent.agent_routing_overrides.remove(pane_id);
        }
    }

    /// Clears one pane-local routing override during pane teardown.
    pub(crate) fn clear_agent_routing_override(&mut self, pane_id: &str) {
        self.agent.agent_routing_overrides.remove(pane_id);
    }

    /// Returns the configured default agent shell execution mode.
    pub(crate) fn agent_default_shell_mode(&self) -> ShellMode {
        self.agent.agent_shell_mode
    }

    /// Replaces the configured default agent shell execution mode.
    pub(crate) fn set_agent_default_shell_mode(&mut self, mode: ShellMode) {
        self.agent.agent_shell_mode = mode;
    }

    /// Returns an explicit pane-local shell mode override.
    pub(crate) fn agent_shell_mode_override(&self, pane_id: &str) -> Option<ShellMode> {
        self.agent.agent_shell_mode_overrides.get(pane_id).copied()
    }

    /// Replaces or clears one pane-local shell mode override.
    pub(crate) fn set_agent_shell_mode_override(&mut self, pane_id: &str, mode: Option<ShellMode>) {
        if let Some(mode) = mode {
            self.agent
                .agent_shell_mode_overrides
                .insert(pane_id.to_string(), mode);
            if mode == ShellMode::Native {
                self.clear_pane_bootstrap_pending(pane_id);
            }
        } else {
            self.agent.agent_shell_mode_overrides.remove(pane_id);
        }
    }

    /// Selects native shell mode without exposing the private config enum to tests.
    #[cfg(test)]
    pub(crate) fn set_agent_native_shell_mode_for_tests(&mut self, pane_id: &str) {
        self.set_agent_shell_mode_override(pane_id, Some(ShellMode::Native));
    }

    /// Clears one pane-local shell mode override during pane teardown.
    pub(crate) fn clear_agent_shell_mode_override(&mut self, pane_id: &str) {
        self.agent.agent_shell_mode_overrides.remove(pane_id);
    }

    /// Resolves the effective shell execution mode for one pane.
    pub(crate) fn effective_agent_shell_mode_for_pane(&self, pane_id: &str) -> ShellMode {
        self.agent_shell_mode_override(pane_id)
            .unwrap_or_else(|| self.agent_default_shell_mode())
    }

    /// Reports whether planning presentation is enabled for one pane.
    pub(crate) fn agent_planning_enabled(&self, pane_id: &str) -> bool {
        self.agent.agent_planning_modes.contains(pane_id)
    }

    /// Sets pane-local planning presentation state.
    pub(crate) fn set_agent_planning_enabled(&mut self, pane_id: &str, enabled: bool) {
        if enabled {
            self.agent.agent_planning_modes.insert(pane_id.to_string());
        } else {
            self.agent.agent_planning_modes.remove(pane_id);
        }
    }

    /// Returns the pane-local response style selection.
    pub(crate) fn agent_response_style(&self, pane_id: &str) -> Option<&str> {
        self.agent
            .agent_response_styles
            .get(pane_id)
            .map(String::as_str)
    }

    /// Replaces or clears one pane-local response style selection.
    pub(crate) fn set_agent_response_style(&mut self, pane_id: &str, style: Option<String>) {
        if let Some(style) = style {
            self.agent
                .agent_response_styles
                .insert(pane_id.to_string(), style);
        } else {
            self.agent.agent_response_styles.remove(pane_id);
        }
    }

    /// Clears transcript-persisted pane presentation preferences.
    pub(crate) fn clear_agent_pane_presentation_preferences(&mut self, pane_id: &str) {
        self.agent.agent_planning_modes.remove(pane_id);
        self.agent.agent_response_styles.remove(pane_id);
    }

    /// Returns retained patch attempts for one agent session.
    pub(crate) fn retained_agent_patch_records(
        &self,
        session_id: &str,
    ) -> Option<&[RuntimeAgentPatchRecord]> {
        self.agent
            .agent_session_patch_records
            .get(session_id)
            .map(Vec::as_slice)
    }

    /// Returns the latest retained copy output for one pane.
    pub(crate) fn retained_agent_copy_output(
        &self,
        pane_id: &str,
    ) -> Option<&RuntimeAgentCopyOutput> {
        self.agent.agent_copy_outputs.get(pane_id)
    }

    /// Returns modified-file summaries retained for one pane.
    pub(crate) fn retained_agent_modified_files(
        &self,
        pane_id: &str,
    ) -> Option<&BTreeMap<String, RuntimeAgentModifiedFileSummary>> {
        self.agent.agent_modified_files.get(pane_id)
    }

    /// Adds one observed modification delta to a pane-local file summary.
    pub(crate) fn record_agent_modified_file_delta(
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
    pub(crate) fn clear_agent_session_artifacts(&mut self) {
        self.agent.agent_copy_outputs.clear();
        self.agent.agent_modified_files.clear();
    }

    /// Clears pane-scoped copy and modified-file artifacts.
    pub(crate) fn clear_agent_pane_artifacts(&mut self, pane_id: &str) {
        self.agent.agent_copy_outputs.remove(pane_id);
        self.agent.agent_modified_files.remove(pane_id);
    }

    /// Clears modified-file summaries when a pane starts a fresh conversation.
    pub(crate) fn clear_agent_modified_files(&mut self, pane_id: &str) {
        self.agent.agent_modified_files.remove(pane_id);
    }

    /// Reports whether one pane currently owns an agent child shell.
    pub(crate) fn agent_subshell_is_active(&self, pane_id: &str) -> bool {
        self.agent.agent_subshell_panes.contains(pane_id)
    }

    /// Marks one pane as owning an agent child shell.
    pub(crate) fn enter_agent_subshell(&mut self, pane_id: impl Into<String>) {
        let pane_id = pane_id.into();
        self.agent
            .deferred_agent_subshell_entry_panes
            .remove(&pane_id);
        self.agent.agent_subshell_panes.insert(pane_id);
    }

    /// Records that child-shell entry must resume after parent bootstrap.
    pub(crate) fn defer_agent_subshell_entry(&mut self, pane_id: impl Into<String>) {
        self.agent
            .deferred_agent_subshell_entry_panes
            .insert(pane_id.into());
    }

    /// Records user-originated process input for safe non-native shell admission.
    ///
    /// A line terminator or terminal interrupt ends the current shell edit;
    /// otherwise the pane is treated as having a potentially dirty prompt.
    pub(crate) fn record_user_process_input(&mut self, pane_id: &str, input: &[u8]) {
        if input
            .iter()
            .any(|byte| matches!(*byte, b'\r' | b'\n' | 0x03))
        {
            self.agent
                .panes_with_unsubmitted_process_input
                .remove(pane_id);
        } else {
            self.agent
                .panes_with_unsubmitted_process_input
                .insert(pane_id.to_string());
        }
    }

    /// Reports whether user process input may still be an unsubmitted shell draft.
    pub(crate) fn pane_has_unsubmitted_process_input(&self, pane_id: &str) -> bool {
        self.agent
            .panes_with_unsubmitted_process_input
            .contains(pane_id)
    }

    /// Forgets an unfinished editable command after a managed shell adapter
    /// has authenticated that it discarded the native editor buffer.
    pub(crate) fn discard_unsubmitted_process_input(&mut self, pane_id: &str) -> bool {
        self.agent
            .panes_with_unsubmitted_process_input
            .remove(pane_id)
    }

    /// Records that a live non-native shell must publish a fresh prompt after
    /// an interrupt before the runtime may write an agent child-shell handoff.
    pub(crate) fn begin_agent_subshell_input_clear(&mut self, pane_id: impl Into<String>) {
        let pane_id = pane_id.into();
        self.agent
            .panes_with_unsubmitted_process_input
            .remove(&pane_id);
        self.agent
            .pending_agent_subshell_input_clear_panes
            .insert(pane_id);
    }

    /// Reports whether a live non-native shell is waiting for an
    /// interrupt-confirmed parent prompt before child entry.
    pub(crate) fn agent_subshell_input_clear_is_pending(&self, pane_id: &str) -> bool {
        self.agent
            .pending_agent_subshell_input_clear_panes
            .contains(pane_id)
    }

    /// Consumes the live non-native shell input-clear boundary.
    pub(crate) fn finish_agent_subshell_input_clear(&mut self, pane_id: &str) -> bool {
        let cleared = self
            .agent
            .pending_agent_subshell_input_clear_panes
            .remove(pane_id);
        if cleared {
            self.agent
                .completed_agent_subshell_input_clear_panes
                .insert(pane_id.to_string());
        }
        cleared
    }

    /// Reports whether child entry followed an interrupt that may still echo
    /// after the child exits and must remain hidden from retained parent content.
    pub(crate) fn agent_subshell_input_clear_was_completed(&self, pane_id: &str) -> bool {
        self.agent
            .completed_agent_subshell_input_clear_panes
            .contains(pane_id)
    }

    /// Clears the completed non-native input-clear marker once ordinary process
    /// input resumes and later output must be retained normally.
    pub(crate) fn clear_completed_agent_subshell_input_clear(&mut self, pane_id: &str) -> bool {
        self.agent
            .completed_agent_subshell_input_clear_panes
            .remove(pane_id)
    }

    /// Reports whether one pane has an explicit deferred child-shell entry.
    pub(crate) fn agent_subshell_entry_is_deferred(&self, pane_id: &str) -> bool {
        self.agent
            .deferred_agent_subshell_entry_panes
            .contains(pane_id)
    }

    /// Cancels a deferred child-shell entry for one pane.
    pub(crate) fn clear_deferred_agent_subshell_entry(&mut self, pane_id: &str) -> bool {
        self.agent
            .deferred_agent_subshell_entry_panes
            .remove(pane_id)
    }

    /// Removes one pane from active agent child-shell ownership.
    pub(crate) fn leave_agent_subshell(&mut self, pane_id: &str) -> bool {
        self.agent.agent_subshell_panes.remove(pane_id)
    }

    /// Marks an interrupted child shell for line-oriented exit.
    pub(crate) fn mark_agent_subshell_command_exit(&mut self, pane_id: impl Into<String>) {
        self.agent
            .agent_subshell_command_exit_panes
            .insert(pane_id.into());
    }

    /// Consumes a line-oriented child-shell exit marker.
    pub(crate) fn take_agent_subshell_command_exit(&mut self, pane_id: &str) -> bool {
        self.agent.agent_subshell_command_exit_panes.remove(pane_id)
    }

    /// Clears all agent child-shell state for a removed pane.
    pub(crate) fn clear_agent_subshell_state(&mut self, pane_id: &str) {
        self.agent.agent_subshell_panes.remove(pane_id);
        self.agent
            .deferred_agent_subshell_entry_panes
            .remove(pane_id);
        self.agent
            .panes_with_unsubmitted_process_input
            .remove(pane_id);
        self.agent
            .pending_agent_subshell_input_clear_panes
            .remove(pane_id);
        self.agent
            .completed_agent_subshell_input_clear_panes
            .remove(pane_id);
        self.agent.agent_subshell_command_exit_panes.remove(pane_id);
    }
}

#[cfg(test)]
impl RuntimeSessionService {
    /// Returns failure-feedback attempts for integration-test observation.
    pub(crate) fn agent_failure_feedback_attempts_for_tests(&self) -> &BTreeMap<String, usize> {
        &self.agent.agent_turn_failure_feedback_attempts
    }

    /// Returns failure-feedback attempts for fixture setup.
    pub(crate) fn agent_failure_feedback_attempts_mut_for_tests(
        &mut self,
    ) -> &mut BTreeMap<String, usize> {
        &mut self.agent.agent_turn_failure_feedback_attempts
    }

    /// Returns network action history for integration-test observation.
    pub(crate) fn agent_network_action_history_for_tests(
        &self,
    ) -> &BTreeMap<String, AgentNetworkActionHistory> {
        &self.agent.agent_turn_network_action_history
    }

    /// Returns loop-owned turn metadata for integration-test observation.
    pub(crate) fn agent_loop_turns_for_tests(&self) -> &BTreeMap<String, RuntimeAgentLoopTurn> {
        &self.agent.agent_loop_turns
    }

    /// Reports whether a process fixture still has a command-exit marker.
    pub(crate) fn agent_subshell_command_exit_is_pending_for_tests(&self, pane_id: &str) -> bool {
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
    runtime_action_result_has_error_code, runtime_action_result_is_feedback_candidate,
    runtime_action_result_is_terminal_failure, runtime_action_status_name,
    runtime_action_type_is_shell_backed, runtime_execution_can_feed_failure_to_model,
    runtime_execution_uses_unbounded_apply_patch_recovery, runtime_failure_feedback_attempt_keys,
    runtime_failure_feedback_status_line, runtime_loop_guard_failure_label,
    runtime_loop_guard_failure_summary_line, runtime_provider_audit_error_message,
    runtime_unrecovered_action_failure_output, runtime_unrecovered_failure_output_lines,
    runtime_unrecovered_failure_reason, runtime_validate_provider_completion_execution,
    runtime_validate_provider_completion_identity,
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
