//! Long-lived runtime session-service aggregate and owned subsystem stores.

use super::*;
use crate::runtime::RuntimePresentationComponent;

/// Carries Runtime Session Service state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
pub struct RuntimeSessionService {
    /// Private state owner for terminal presentation and client interaction.
    pub(in crate::runtime) presentation: RuntimePresentationComponent,
    /// Stores the session value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) session: Session,
    /// Stores the window created at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) window_created_at_unix_seconds: BTreeMap<String, u64>,
    /// Stores the config layers value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) config_layers: Vec<ConfigLayer>,
    /// Stores the config root value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) config_root: Option<PathBuf>,
    /// Stores the snapshot repository used by live terminal snapshot commands.
    ///
    /// The field is optional so tests and embedded runtimes that do not provide
    /// persistent snapshot storage continue to report an explicit runtime
    /// repository requirement instead of silently writing to an implicit path.
    pub(in crate::runtime) snapshot_repository: Option<SnapshotRepository>,
    /// Stores the control idempotency value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) control_idempotency: ControlIdempotencyCache,
    /// Stores the message service value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) message_service: MessageService,
    /// Stores the pane processes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_processes: PaneProcessManager,
    /// Stores the async owned pane processes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) detached_pane_primary_pids: BTreeMap<String, u32>,
    /// Stores the latest async runtime actor metrics snapshot when available.
    ///
    /// The actor-owned command path updates this snapshot before rendering
    /// `show-metrics` so runtime display helpers can present metrics without
    /// taking a direct dependency on actor internals.
    pub(in crate::runtime) async_runtime_metrics:
        Option<crate::async_runtime::AsyncRuntimeActorMetrics>,
    /// Stores runtime-owned agent, provider, and shell diagnostics.
    ///
    /// These counters and histograms are updated from the serialized runtime
    /// service path so `show-metrics` can expose prompt-cache shape, provider
    /// usage, turn lifecycle, and shell-transaction behavior without parsing
    /// trace logs.
    pub(in crate::runtime) runtime_metrics: RuntimeMetricsSnapshot,
    /// Stores the pane current working directories value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_current_working_directories: BTreeMap<String, PathBuf>,
    /// Last foreground process group reported by the async pane worker.
    ///
    /// The synchronous PTY metadata path is best-effort and can be unavailable
    /// immediately after new pane creation or layout restoration. This cache
    /// lets readiness recovery use the actor-owned foreground observation when
    /// it is newer than no host metadata at all.
    pub(in crate::runtime) pane_foreground_process_groups: BTreeMap<String, u32>,
    /// Program-owned pane titles keyed by pane id.
    ///
    /// The map stores the pane title mode that was active before a foreground
    /// program emitted an OSC title so process metadata refreshes can leave that
    /// title sticky until the owning foreground process changes or exits.
    pub(in crate::runtime) program_owned_pane_titles: BTreeMap<String, ProgramOwnedPaneTitle>,
    /// Stores the deferred pane inputs value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) queued_pane_input_effects: Vec<RuntimeSideEffect>,
    /// Stores the deferred pane resizes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) queued_pane_resize_effects: BTreeMap<String, RuntimeSideEffect>,
    /// Stores the deferred pane terminations value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) queued_pane_termination_effects: BTreeMap<String, RuntimeSideEffect>,
    /// Stores the deferred pane pipe writes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) queued_pane_pipe_effects: Vec<(String, RuntimeSideEffect)>,
    /// Stores audit persistence effects awaiting adapter execution.
    ///
    /// The runtime retains canonical effects rather than audit-specific
    /// compatibility records after the audit writer encodes each record.
    pub(in crate::runtime) queued_audit_effects: Vec<RuntimeSideEffect>,
    /// Stores transcript and prompt-history effects awaiting adapter execution.
    ///
    /// Producers retain canonical persistence effects rather than
    /// transcript-specific compatibility records.
    pub(in crate::runtime) queued_transcript_effects: Vec<RuntimeSideEffect>,
    /// Stores the deferred config file writes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    /// Stores the deferred project instruction writes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) queued_config_effects: Vec<RuntimeSideEffect>,
    /// Stores the deferred transcript next sequences value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) deferred_transcript_next_sequences: BTreeMap<String, u64>,
    /// Stores the pane screens value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_screens: BTreeMap<String, TerminalScreen>,
    /// Stores the pane transaction osc screens value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_transaction_osc_screens: BTreeMap<String, TerminalScreen>,
    /// Stores hidden agent-shell OSC parser fragments for each pane.
    ///
    /// Hidden agent-shell output is command data, not user-visible terminal
    /// traffic. The runtime keeps only bounded fragments that may contain a
    /// split Mezzanine transaction marker so large file-read bodies never have
    /// to pass through the full terminal-screen parser.
    pub(in crate::runtime) pane_transaction_osc_pending: BTreeMap<String, Vec<u8>>,
    /// Stores the pane mez wrapper filter pending value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_mez_wrapper_filter_pending: BTreeMap<String, Vec<u8>>,
    /// Stores the pane mez wrapper filter recent commands value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_mez_wrapper_filter_recent_commands: BTreeMap<String, Vec<String>>,
    /// Stores the pane mez wrapper filter recent polls value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_mez_wrapper_filter_recent_polls: BTreeMap<String, usize>,
    /// Stores the pane hidden shell render recent polls value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_hidden_shell_render_recent_polls: BTreeMap<String, usize>,
    /// Stores the foreground title idle sync polls value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) foreground_title_idle_sync_polls: usize,
    /// Stores the pane exit records value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_exit_records: BTreeMap<String, PaneExitRecord>,
    /// Stores the active pane pipes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) active_pane_pipes: BTreeMap<String, ActivePanePipe>,
    /// Whether audit writes are emitted for an adapter instead of written inline.
    ///
    /// This ownership is explicit because config reloads may replace the writer
    /// after the async actor has started.
    pub(in crate::runtime) audit_effects_use_adapter: bool,
    /// Whether pane-pipe process and persistence work is owned by adapters.
    pub(in crate::runtime) pane_pipe_effects_use_adapter: bool,
    /// Whether agent transcript entries are persisted by an adapter.
    pub(in crate::runtime) transcript_effects_use_adapter: bool,
    /// Whether session-registry updates are persisted by an adapter.
    pub(in crate::runtime) registry_effects_use_adapter: bool,
    /// Whether configuration writes are persisted by an adapter.
    pub(in crate::runtime) config_effects_use_adapter: bool,
    /// Whether non-blocking program hooks execute through an adapter.
    pub(in crate::runtime) hook_effects_use_adapter: bool,
    /// Stores the pane transcript refs value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_transcript_refs: BTreeMap<String, Vec<String>>,
    /// Stores the terminal history limit value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) terminal_history_limit: usize,
    /// Stores the terminal history rotation line count value for this data
    /// structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) terminal_history_rotate_lines: usize,
    /// Stores the terminal term value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) terminal_term: String,
    /// Stores the terminal emoji status-glyph width policy value.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) terminal_emoji_width: TerminalEmojiWidth,
    /// Stores the hidden shell-output preview tail line count for this data
    /// structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) terminal_shell_output_preview_lines: usize,
    /// Stores the permission policy value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) permission_policy: PermissionPolicy,
    /// Stores an explicit live approval-bypass override selected by the user.
    ///
    /// Configuration is intentionally unable to enable approval bypass, so
    /// explicit runtime activation must survive unrelated configuration
    /// reloads without being encoded into normal config layers.
    pub(in crate::runtime) live_approval_bypass_override: Option<bool>,
    /// Stores an explicit live approval-policy override selected by the user.
    ///
    /// Runtime approval changes are session choices. They must survive unrelated
    /// configuration reloads without being erased by persistent config changes.
    pub(in crate::runtime) live_approval_policy_override: Option<mez_agent::ApprovalPolicy>,
    /// Stores the blocked approvals value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) blocked_approvals: BlockedApprovalQueue,
    /// Stores the session approvals value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) session_approvals: SessionApprovalStore,
    /// Stores the session memory value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) session_memory: SessionMemoryStore,
    /// Stores the mcp registry value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) mcp_registry: McpRegistry,
    /// Stores the mcp transports value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) mcp_transports: RuntimeMcpTransportSet,
    /// Stores the provider registry value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) provider_registry: RuntimeProviderRegistry,
    /// Stores the preset registry value for this data structure.
    pub(in crate::runtime) preset_registry: RuntimePresetRegistry,
    /// Stores the subagent profiles value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) subagent_profiles: BTreeMap<String, SubagentProfile>,
    /// User-defined pane personality profiles.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_personality_profiles:
        BTreeMap<String, RuntimeAgentPersonalityProfile>,
    /// Configured default personality profile id, when one exists.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) default_agent_personality: Option<String>,
    /// User-configured system prompt text appended after the built-in prompt.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) custom_agent_system_prompt: Option<String>,
    /// Pane-local selected personality profile ids.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_personality_selections: BTreeMap<String, String>,
    /// Stores the model profile overrides value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) model_profile_overrides: RuntimeModelProfileOverrideStore,
    /// Stores the auth store value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) auth_store: Option<AuthStore>,
    /// Seconds before provider access-token expiry that triggers proactive refresh.
    ///
    /// The field is part of structured runtime state so startup and provider
    /// turn preflight checks use the same configured threshold.
    pub(in crate::runtime) provider_auth_refresh_leeway_seconds: u64,
    /// Stores the audit log value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) audit_log: Option<AuditLog>,
    /// Stores the agent scheduler value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_scheduler: AgentScheduler,
    /// Stores the agent shell store value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_shell_store: AgentShellStore,
    /// Retains bounded per-pane agent trace lines independent of visible log level.
    ///
    /// The runtime uses this ring as a diagnostics escape hatch so normal-mode
    /// sessions can later dump recent trace context without enabling noisy
    /// trace logging up front.
    pub(in crate::runtime) agent_pane_trace_logs: BTreeMap<String, Vec<String>>,
    /// Retained patch payloads keyed by pane-local agent session id.
    ///
    /// This lets `/copy-patches` export exact patch attempts and outcomes from
    /// the current session without depending on rendered pane text or compact
    /// transcript summaries.
    pub(in crate::runtime) agent_session_patch_records:
        BTreeMap<String, Vec<RuntimeAgentPatchRecord>>,
    /// Tracks panes whose visible agent shell is scoped to a child shell.
    ///
    /// The runtime uses this ephemeral set to exit the child shell cleanly when
    /// agent mode hides, while keeping prompt and environment mutations away
    /// from the user's original interactive shell.
    pub(in crate::runtime) agent_subshell_panes: BTreeSet<String>,
    /// Tracks agent subshells that should exit with a command line after an
    /// interrupted shell transaction.
    ///
    /// EOF can be consumed by a transaction wrapper that is still unwinding
    /// after Ctrl+C. These panes use a line-oriented `exit` command instead so
    /// the parent shell is restored when the interrupted wrapper returns.
    pub(in crate::runtime) agent_subshell_command_exit_panes: BTreeSet<String>,
    /// Stores the agent turn ledger value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_turn_ledger: AgentTurnLedger,
    /// Stores the agent turn contexts value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_turn_contexts: BTreeMap<String, AgentContext>,
    /// Stores the agent turn executions value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_turn_executions: BTreeMap<String, AgentTurnExecution>,
    /// Tracks shell-backed `apply_patch` actions that are collecting batched read snapshots.
    ///
    /// The key is `turn_id/action_id`, keeping the accumulator scoped to one
    /// running semantic action while successive read transactions complete.
    pub(in crate::runtime) apply_patch_batch_states: BTreeMap<String, RuntimeApplyPatchBatchState>,
    /// User steering prompts waiting to be incorporated into an active turn.
    ///
    /// Input submitted while a turn is already running cannot alter a provider
    /// request that has already been dispatched. These entries are drained into
    /// the next provider-bound context so the same turn can incorporate the new
    /// instruction before taking further action.
    pub(in crate::runtime) agent_turn_pending_steering: BTreeMap<String, Vec<AgentTurnSteering>>,
    /// Counts bounded model self-correction attempts after real action failures.
    ///
    /// Failure feedback is scoped to a turn so provider continuations can give
    /// the model one chance to recover from command/tool failures without
    /// creating an unbounded retry loop.
    pub(in crate::runtime) agent_turn_failure_feedback_attempts: BTreeMap<String, usize>,
    /// Stores the configured retry budget for model-correctable action failures.
    ///
    /// The value is applied per stable failed-action signature so identical
    /// failures cannot loop forever while distinct failures in the same turn
    /// still receive bounded correction opportunities.
    pub(in crate::runtime) agent_action_failure_retry_limit: usize,
    /// Stores the configured successful shell-command streak that triggers a
    /// soft action-pressure hint during one active turn.
    ///
    /// The runtime uses this as advisory context only; it must not block shell
    /// execution because legitimate audits can require long inspection runs.
    pub(in crate::runtime) agent_implementation_pressure_after_shell_actions: usize,
    /// Maximum number of work iterations for one `/loop` command.
    pub(in crate::runtime) agent_loop_limit: usize,
    /// Active `/loop` controller state keyed by pane id.
    pub(in crate::runtime) agent_loops_by_pane: BTreeMap<String, RuntimeAgentLoopState>,
    /// Loop metadata keyed by runtime agent turn id.
    pub(in crate::runtime) agent_loop_turns: BTreeMap<String, RuntimeAgentLoopTurn>,
    /// Stores the agent turn shell dispatch history value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_turn_shell_dispatch_history:
        BTreeMap<String, AgentShellDispatchHistory>,
    /// Stores the agent turn network action history value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_turn_network_action_history:
        BTreeMap<String, AgentNetworkActionHistory>,
    /// Stores the agent pre shell hook completions value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_pre_shell_hook_completions:
        BTreeSet<RuntimeAgentPreShellHookCompletion>,
    /// Stores the latest agent say output retained for copy commands.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_copy_outputs: BTreeMap<String, RuntimeAgentCopyOutput>,
    /// File modifications observed from successful agent patch actions.
    ///
    /// The outer key is the pane id, and the inner map is keyed by repository
    /// relative display path so `/list-modified-files` can summarize the
    /// current conversation without re-reading the working tree.
    pub(in crate::runtime) agent_modified_files:
        BTreeMap<String, BTreeMap<String, RuntimeAgentModifiedFileSummary>>,
    /// Stores the agent turn model profiles value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_turn_model_profiles: BTreeMap<String, ModelProfile>,
    /// Stores the agent planning modes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_planning_modes: BTreeSet<String>,
    /// Stores the agent response styles value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_response_styles: BTreeMap<String, String>,
    /// Percent of the active model context retained as uncompacted raw tail.
    pub(in crate::runtime) agent_compaction_raw_retention_percent: usize,
    /// Panes currently running model-backed context compaction, keyed by start
    /// time for timer rendering.
    pub(in crate::runtime) agent_compacting_panes: BTreeMap<String, u64>,
    /// Model-backed compaction tasks waiting for async provider dispatch.
    pub(in crate::runtime) pending_agent_compaction_tasks:
        BTreeMap<String, RuntimeAgentCompactionTask>,
    /// Model-backed compaction tasks claimed by async provider workers.
    pub(in crate::runtime) claimed_agent_compaction_tasks:
        BTreeMap<String, RuntimeAgentCompactionTask>,
    /// Panes currently running model-backed durable memory generation, keyed by
    /// start time for timer rendering.
    pub(in crate::runtime) agent_remembering_panes: BTreeMap<String, u64>,
    /// Model-backed memory-generation tasks waiting for async provider dispatch.
    pub(in crate::runtime) pending_agent_remember_tasks: BTreeMap<String, RuntimeAgentRememberTask>,
    /// Model-backed memory-generation tasks claimed by async provider workers.
    pub(in crate::runtime) claimed_agent_remember_tasks: BTreeMap<String, RuntimeAgentRememberTask>,
    /// Whether new agent turns use routing model and reasoning sizing by default.
    pub(in crate::runtime) agent_routing: bool,
    /// Pane-local routing overrides. Missing entries inherit the
    /// configured default.
    pub(in crate::runtime) agent_routing_overrides: BTreeMap<String, bool>,
    /// Automatic sizing profile and fallback configuration.
    pub(in crate::runtime) agent_auto_sizing: RuntimeAutoSizingConfig,
    /// Pane-local automatic sizing profile overrides selected through model
    /// presets. Missing entries inherit the configured default.
    pub(in crate::runtime) agent_auto_sizing_overrides: BTreeMap<String, RuntimeAutoSizingConfig>,
    /// Cumulative provider token usage keyed by conversation and provider/model.
    pub(in crate::runtime) agent_token_usage_by_conversation:
        BTreeMap<String, BTreeMap<ModelTokenUsageKey, ModelTokenUsage>>,
    /// Cumulative provider token usage keyed by pane and provider/model.
    pub(in crate::runtime) agent_token_usage_by_pane:
        BTreeMap<String, BTreeMap<ModelTokenUsageKey, ModelTokenUsage>>,
    /// Latest provider-response input context usage percentage keyed by
    /// conversation id for terminal rendering and legacy persistence.
    pub(in crate::runtime) agent_context_usage_by_conversation: BTreeMap<String, String>,
    /// Latest provider-response request-context snapshots keyed by
    /// conversation id.
    pub(in crate::runtime) agent_context_usage_snapshot_by_conversation:
        BTreeMap<String, mez_agent::AgentContextUsageSnapshot>,
    /// Latest provider quota usage percentages keyed by agent conversation id.
    pub(in crate::runtime) agent_quota_usage_by_conversation:
        BTreeMap<String, Vec<ProviderQuotaUsage>>,
    /// Latest live provider model catalogs keyed by provider id.
    pub(in crate::runtime) provider_model_catalog_cache:
        BTreeMap<String, crate::runtime::commands::RuntimeModelCatalog>,
    /// Stores the pending agent provider tasks value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pending_agent_provider_tasks: BTreeSet<String>,
    /// Retry attempts keyed by provider turn id.
    pub(in crate::runtime) agent_provider_retry_attempts: BTreeMap<String, u32>,
    /// Stores provider tasks claimed by async workers but not yet settled.
    ///
    /// Claimed tasks are removed from `pending_agent_provider_tasks`, so this
    /// map keeps running turns observable and gives the actor a timeout path if
    /// a worker fails to deliver a provider event.
    pub(in crate::runtime) claimed_agent_provider_tasks:
        BTreeMap<String, RuntimeAgentProviderClaim>,
    /// Stores the subagent task routes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) subagent_task_routes: BTreeMap<String, String>,
    /// Tracks windows reserved for subagent panes.
    ///
    /// Subagent windows are runtime-only placement buckets. They remain in the
    /// same user-visible group as the controlling pane, use an even layout, and
    /// are pruned from this set whenever the backing window disappears.
    pub(in crate::runtime) subagent_window_ids: BTreeSet<String>,
    /// Panes that should close once their terminal subagent turn fully
    /// finishes its normal terminal cleanup.
    pub(in crate::runtime) pending_terminal_subagent_pane_closes: BTreeSet<String>,
    /// Maximum number of subagent panes to place in one subagent window.
    ///
    /// This bound keeps helper panes readable by forcing a fresh background
    /// window once the configured bucket size is reached.
    pub(in crate::runtime) max_subagent_panes_per_window: usize,
    /// Maximum number of direct subagents a root pane agent may spawn.
    ///
    /// This caps the delegation width available to the user-facing root agent
    /// independently from scheduler concurrency and subagent window bucket
    /// capacity.
    pub(in crate::runtime) max_root_subagents: usize,
    /// Maximum number of direct subagents a child subagent may spawn.
    ///
    /// Child delegation uses a narrower branching factor so nested helper
    /// trees remain bounded even when the parent task is allowed to delegate.
    pub(in crate::runtime) max_subagents_per_subagent: usize,
    /// Maximum depth at which spawned subagents may create more children.
    ///
    /// Root pane agents are depth zero. A subagent at this depth may continue
    /// its own work but cannot spawn another generation.
    pub(in crate::runtime) max_subagent_depth: usize,
    /// Policy controlling whether parent turns wait for spawned subagents.
    ///
    /// Joined parents move to blocked scheduler state until all spawned child
    /// task results are available, preventing scheduler deadlocks while keeping
    /// provider continuation ordered after child output.
    pub(in crate::runtime) subagent_wait_policy: SubagentWaitPolicy,
    /// Child turns currently joined by parent `spawn_agent` actions.
    ///
    /// The map is keyed by child turn id so task-result delivery can resolve
    /// the exact parent action result that was waiting.
    pub(in crate::runtime) joined_subagent_dependencies: BTreeMap<String, JoinedSubagentDependency>,
    /// Subagents whose parent messages should become queued agent-shell steps.
    ///
    /// Agent macros keep one child session alive across multiple prompts. Those
    /// prompts still travel through MMP `send_message`, but the runtime must
    /// bridge each accepted message back into the child's normal agent-shell
    /// turn path so slash commands and step results behave like ordinary
    /// subagent prompt submissions. Entries are scoped to the owning macro
    /// parent turn so stale child recipients cannot be reused by later turns.
    pub(in crate::runtime) macro_managed_subagent_agents: BTreeMap<String, MacroManagedSubagent>,
    /// Active macro runs keyed by their parent orchestration turn id.
    pub(in crate::runtime) macro_runs_by_parent_turn: BTreeMap<String, MacroRunState>,
    /// Reverse lookup from child step turn id to parent macro run id.
    pub(in crate::runtime) macro_run_by_child_turn: BTreeMap<String, String>,
    /// Stores the subagent scope declarations value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) subagent_scope_declarations: BTreeMap<String, SubagentScopeDeclaration>,
    /// Runtime-only delegation lineage for active spawned subagents.
    ///
    /// Entries are keyed by child agent id. Root pane agents are inferred when
    /// absent from this map, which keeps limits scoped to active child work.
    pub(in crate::runtime) subagent_lineage: BTreeMap<String, RuntimeSubagentLineage>,
    /// Stores the blocked agent approval refs value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) blocked_agent_approval_refs: BTreeMap<String, BlockedAgentApprovalRef>,
    /// Stores the running shell transactions value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) running_shell_transactions: BTreeMap<String, RunningShellTransactionRef>,
    /// Tracks live shell transactions whose wrapper start marker is mandatory.
    ///
    /// Runtime-dispatched transactions are sequenced by explicit wrapper start
    /// and end markers. Tests and migration fixtures may still construct
    /// transactions directly, so this set defines which live marker entries are
    /// subject to strict start-before-end validation.
    pub(in crate::runtime) shell_transaction_require_start_markers: BTreeSet<String>,
    /// Tracks live shell transactions whose wrapper start marker was observed.
    ///
    /// This state is intentionally separate from pending command payloads:
    /// stateful commands have no deferred payload but still must emit exactly
    /// one wrapper start marker before they can complete.
    pub(in crate::runtime) shell_transaction_started_markers: BTreeSet<String>,
    /// Tracks pane-local transient shell-output status rows for hidden agent
    /// shell commands.
    ///
    /// The rows are display-only progress feedback: each output tail update
    /// replaces the prior preview block, and the next durable agent transcript
    /// line clears it before writing its own content.
    pub(in crate::runtime) agent_shell_output_status_lines: BTreeMap<String, Vec<String>>,
    /// Panes currently replaying durable agent presentation entries.
    ///
    /// Replay writes use the same terminal append primitives as live agent
    /// output, so this set prevents restored rows from being persisted again.
    pub(in crate::runtime) agent_presentation_replay_panes: BTreeSet<String>,
    /// Stores the pane readiness states value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_readiness_states: BTreeMap<String, PaneReadinessState>,
    /// Stores the pane readiness overrides value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_readiness_overrides: PaneReadinessOverrideStore,
    /// Stores the pane environment signatures value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_environment_signatures: BTreeMap<String, EnvironmentSignature>,
    /// Stores the pane bootstrap pending value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_bootstrap_pending: BTreeSet<String>,
    /// Stores the tool discovery cache value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) tool_discovery_cache: ToolDiscoveryCache,
    /// Stores the pane instruction files value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_instruction_files: BTreeMap<String, Vec<DiscoveredInstructionFile>>,
    /// Stores the pane closing value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) pane_closing: BTreeSet<String>,
    /// Stores the agent transcript store value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) agent_transcript_store: Option<AgentTranscriptStore>,
    /// Stores the subagent scopes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) subagent_scopes: ScopeRegistry,
    /// Stores the project trust store value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) project_trust_store: Option<ProjectTrustStore>,
    /// Stores the project trust database path value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) project_trust_database_path: Option<PathBuf>,
    /// Stores the announced project trust roots value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) announced_project_trust_roots: BTreeSet<PathBuf>,
    /// Stores the hook definitions value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) hook_definitions: Vec<HookDefinition>,
    /// Stores program-hook side effects awaiting adapter execution.
    ///
    /// Runtime transitions queue the canonical effect directly so the async
    /// actor does not need a hook-specific compatibility record.
    pub(in crate::runtime) queued_program_hook_effects: Vec<RuntimeSideEffect>,
    /// Stores the focused shell hooks value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) focused_shell_hooks: FocusedShellHookQueue,
    /// Stores the next focused shell hook marker value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) next_focused_shell_hook_marker: u64,
    /// Stores the focused shell hook transactions value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) focused_shell_hook_transactions:
        BTreeMap<String, PendingFocusedShellHookTransaction>,
    /// Stores the focused shell hook results value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) focused_shell_hook_results: Vec<HookExecutionResult>,
    /// Stores the event log value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) event_log: Option<EventLog>,
    /// Stores the lifecycle state value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) lifecycle_state: RuntimeLifecycleState,
    /// Stores the session registry value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) session_registry: Option<SessionRegistry>,
    /// Stores the socket path value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) socket_path: PathBuf,
    /// Stores the created at unix seconds value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) created_at_unix_seconds: u64,
    /// Stores the last attach at unix seconds value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(in crate::runtime) last_attach_at_unix_seconds: Option<u64>,
}
