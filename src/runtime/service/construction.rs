//! Runtime session-service construction and initial aggregate assembly.

use super::*;
use crate::runtime::RuntimePresentationComponent;

impl RuntimeSessionService {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(
        session: Session,
        socket_path: PathBuf,
        created_at_unix_seconds: u64,
    ) -> Result<Self> {
        Self::from_parts(
            session,
            socket_path,
            created_at_unix_seconds,
            ControlIdempotencyCache::default(),
            MessageService::default(),
            None,
        )
    }

    /// Runs the with event log operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn with_event_log(
        session: Session,
        socket_path: PathBuf,
        created_at_unix_seconds: u64,
        max_events: usize,
        max_payload_bytes: usize,
    ) -> Result<Self> {
        if max_payload_bytes < 128 {
            return Err(MezError::invalid_args(
                "runtime lifecycle event payload limit must be at least 128 bytes",
            ));
        }
        Self::from_parts(
            session,
            socket_path,
            created_at_unix_seconds,
            ControlIdempotencyCache::default(),
            MessageService::default(),
            Some(EventLog::new(max_events, max_payload_bytes)?),
        )
    }

    /// Replaces host clipboard access for tests that must avoid touching the
    /// parent desktop/session clipboard.
    #[cfg(test)]
    pub(crate) fn set_host_clipboard_for_tests(&mut self, host_clipboard: HostClipboard) {
        self.host_clipboard = host_clipboard;
    }

    /// Runs the from parts operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_parts(
        session: Session,
        socket_path: PathBuf,
        created_at_unix_seconds: u64,
        control_idempotency: ControlIdempotencyCache,
        message_service: MessageService,
        event_log: Option<EventLog>,
    ) -> Result<Self> {
        Self::from_parts_with_processes(
            session,
            socket_path,
            created_at_unix_seconds,
            control_idempotency,
            message_service,
            PaneProcessManager::new(),
            event_log,
        )
    }

    /// Runs the from parts with processes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn from_parts_with_processes(
        session: Session,
        socket_path: PathBuf,
        created_at_unix_seconds: u64,
        control_idempotency: ControlIdempotencyCache,
        message_service: MessageService,
        pane_processes: PaneProcessManager,
        event_log: Option<EventLog>,
    ) -> Result<Self> {
        ensure_absolute(&socket_path)?;
        ensure_no_mez_separator(&socket_path)?;
        let lifecycle_state = RuntimeLifecycleState::from_session_state(session.state);
        let window_created_at_unix_seconds = session
            .windows()
            .iter()
            .map(|window| (window.id.to_string(), created_at_unix_seconds))
            .collect::<BTreeMap<_, _>>();
        let terminal_emoji_width = mez_terminal::TerminalEmojiWidth::Wide;
        mez_terminal::set_terminal_emoji_width(terminal_emoji_width);
        crate::terminal::set_agent_wrap_column_cap(crate::terminal::DEFAULT_AGENT_WRAP_COLUMN_CAP);
        Ok(Self {
            presentation: RuntimePresentationComponent::default(),
            session,
            window_created_at_unix_seconds,
            config_layers: Vec::new(),
            config_root: None,
            snapshot_repository: None,
            control_idempotency,
            message_service,
            pane_processes,
            detached_pane_primary_pids: BTreeMap::new(),
            async_runtime_metrics: None,
            runtime_metrics: Default::default(),
            pane_current_working_directories: BTreeMap::new(),
            queued_pane_input_effects: Vec::new(),
            queued_pane_resize_effects: BTreeMap::new(),
            queued_pane_termination_effects: BTreeMap::new(),
            queued_pane_pipe_effects: Vec::new(),
            queued_audit_effects: Vec::new(),
            queued_transcript_effects: Vec::new(),
            queued_config_effects: Vec::new(),
            deferred_transcript_next_sequences: BTreeMap::new(),
            pane_screens: BTreeMap::new(),
            pane_transaction_osc_screens: BTreeMap::new(),
            pane_transaction_osc_pending: BTreeMap::new(),
            pane_mez_wrapper_filter_pending: BTreeMap::new(),
            pane_mez_wrapper_filter_recent_commands: BTreeMap::new(),
            pane_mez_wrapper_filter_recent_polls: BTreeMap::new(),
            pane_hidden_shell_render_recent_polls: BTreeMap::new(),
            foreground_title_idle_sync_polls: 0,
            pane_exit_records: BTreeMap::new(),
            active_pane_pipes: BTreeMap::new(),
            audit_effects_use_adapter: false,
            pane_pipe_effects_use_adapter: false,
            transcript_effects_use_adapter: false,
            registry_effects_use_adapter: false,
            config_effects_use_adapter: false,
            hook_effects_use_adapter: false,
            paste_buffers: PasteBuffers::default_limit(),
            active_paste_buffer: None,
            host_clipboard: HostClipboard::system(),
            active_copy_modes: BTreeMap::new(),
            scrollback_copy_mode_panes: BTreeSet::new(),
            pane_transcript_refs: BTreeMap::new(),
            terminal_history_limit: DEFAULT_HISTORY_LIMIT,
            terminal_history_rotate_lines: DEFAULT_HISTORY_ROTATE_LINES,
            terminal_term: DEFAULT_PANE_TERM.to_string(),
            terminal_emoji_width,
            terminal_shell_output_preview_lines: 5,
            terminal_clipboard: "external".to_string(),
            ui_theme: mez_mux::theme::UiTheme::default(),
            key_bindings: KeyBindings::default(),
            command_bindings: BTreeMap::new(),
            permission_policy: PermissionPolicy::default(),
            live_approval_bypass_override: None,
            live_approval_policy_override: None,
            blocked_approvals: BlockedApprovalQueue::default(),
            session_approvals: SessionApprovalStore::default(),
            session_memory: SessionMemoryStore::default(),
            mcp_registry: McpRegistry::default(),
            mcp_transports: RuntimeMcpTransportSet::default(),
            provider_registry: runtime_provider_registry_from_config(&Value::Object(
                serde_json::Map::new(),
            ))?,
            preset_registry: RuntimePresetRegistry::default(),
            subagent_profiles: builtin_subagent_profiles(),
            agent_personality_profiles: BTreeMap::new(),
            default_agent_personality: None,
            custom_agent_system_prompt: None,
            agent_personality_selections: BTreeMap::new(),
            model_profile_overrides: RuntimeModelProfileOverrideStore::default(),
            auth_store: None,
            provider_auth_refresh_leeway_seconds:
                crate::auth::DEFAULT_PROVIDER_AUTH_REFRESH_LEEWAY_SECONDS,
            audit_log: None,
            agent_scheduler: AgentScheduler::with_default_limit(),
            agent_shell_store: AgentShellStore::default(),
            agent_pane_trace_logs: BTreeMap::new(),
            agent_session_patch_records: BTreeMap::new(),
            agent_subshell_panes: BTreeSet::new(),
            agent_subshell_command_exit_panes: BTreeSet::new(),
            agent_turn_ledger: AgentTurnLedger::new(false),
            agent_turn_contexts: BTreeMap::new(),
            agent_turn_executions: BTreeMap::new(),
            apply_patch_batch_states: BTreeMap::new(),
            agent_turn_pending_steering: BTreeMap::new(),
            agent_turn_failure_feedback_attempts: BTreeMap::new(),
            agent_action_failure_retry_limit: DEFAULT_AGENT_ACTION_FAILURE_RETRY_LIMIT,
            agent_implementation_pressure_after_shell_actions:
                DEFAULT_AGENT_IMPLEMENTATION_PRESSURE_AFTER_SHELL_ACTIONS,
            agent_loop_limit: DEFAULT_AGENT_LOOP_LIMIT,
            agent_loops_by_pane: BTreeMap::new(),
            agent_loop_turns: BTreeMap::new(),
            agent_turn_shell_dispatch_history: BTreeMap::new(),
            agent_turn_network_action_history: BTreeMap::new(),
            agent_pre_shell_hook_completions: BTreeSet::new(),
            agent_copy_outputs: BTreeMap::new(),
            agent_modified_files: BTreeMap::new(),
            agent_prompt_inputs: BTreeMap::new(),
            agent_turn_model_profiles: BTreeMap::new(),
            agent_planning_modes: BTreeSet::new(),
            agent_response_styles: BTreeMap::new(),
            agent_compaction_raw_retention_percent: DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT,
            agent_compacting_panes: BTreeMap::new(),
            pending_agent_compaction_tasks: BTreeMap::new(),
            claimed_agent_compaction_tasks: BTreeMap::new(),
            agent_remembering_panes: BTreeMap::new(),
            pending_agent_remember_tasks: BTreeMap::new(),
            claimed_agent_remember_tasks: BTreeMap::new(),
            agent_routing: DEFAULT_AGENT_ROUTING,
            agent_routing_overrides: BTreeMap::new(),
            agent_auto_sizing: Default::default(),
            agent_auto_sizing_overrides: BTreeMap::new(),
            agent_token_usage_by_conversation: BTreeMap::new(),
            agent_token_usage_by_pane: BTreeMap::new(),
            agent_context_usage_by_conversation: BTreeMap::new(),
            agent_context_usage_snapshot_by_conversation: BTreeMap::new(),
            agent_quota_usage_by_conversation: BTreeMap::new(),
            provider_model_catalog_cache: BTreeMap::new(),
            pane_foreground_process_groups: BTreeMap::new(),
            program_owned_pane_titles: BTreeMap::new(),
            pending_agent_provider_tasks: BTreeSet::new(),
            agent_provider_retry_attempts: BTreeMap::new(),
            claimed_agent_provider_tasks: BTreeMap::new(),
            subagent_task_routes: BTreeMap::new(),
            subagent_window_ids: BTreeSet::new(),
            pending_terminal_subagent_pane_closes: BTreeSet::new(),
            max_subagent_panes_per_window: DEFAULT_MAX_SUBAGENT_PANES_PER_WINDOW,
            max_root_subagents: DEFAULT_MAX_ROOT_SUBAGENTS,
            max_subagents_per_subagent: DEFAULT_MAX_SUBAGENTS_PER_SUBAGENT,
            max_subagent_depth: DEFAULT_MAX_SUBAGENT_DEPTH,
            subagent_wait_policy: DEFAULT_SUBAGENT_WAIT_POLICY,
            joined_subagent_dependencies: BTreeMap::new(),
            macro_managed_subagent_agents: BTreeMap::new(),
            macro_runs_by_parent_turn: BTreeMap::new(),
            macro_run_by_child_turn: BTreeMap::new(),
            subagent_scope_declarations: BTreeMap::new(),
            subagent_lineage: BTreeMap::new(),
            blocked_agent_approval_refs: BTreeMap::new(),
            running_shell_transactions: BTreeMap::new(),
            shell_transaction_require_start_markers: BTreeSet::new(),
            shell_transaction_started_markers: BTreeSet::new(),
            agent_shell_output_status_lines: BTreeMap::new(),
            agent_presentation_replay_panes: BTreeSet::new(),
            pane_readiness_states: BTreeMap::new(),
            pane_readiness_overrides: PaneReadinessOverrideStore::default(),
            pane_environment_signatures: BTreeMap::new(),
            pane_bootstrap_pending: BTreeSet::new(),
            tool_discovery_cache: ToolDiscoveryCache::default(),
            pane_instruction_files: BTreeMap::new(),
            pane_closing: BTreeSet::new(),
            agent_transcript_store: None,
            subagent_scopes: ScopeRegistry::default(),
            project_trust_store: None,
            project_trust_database_path: None,
            announced_project_trust_roots: BTreeSet::new(),
            hook_definitions: Vec::new(),
            queued_program_hook_effects: Vec::new(),
            focused_shell_hooks: FocusedShellHookQueue::default(),
            next_focused_shell_hook_marker: 1,
            focused_shell_hook_transactions: BTreeMap::new(),
            focused_shell_hook_results: Vec::new(),
            event_log,
            lifecycle_state,
            session_registry: None,
            socket_path,
            created_at_unix_seconds,
            last_attach_at_unix_seconds: None,
        })
    }
}
