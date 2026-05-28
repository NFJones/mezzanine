//! Runtime Service implementation.
//!
//! This module owns the runtime service boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AgentLogLevel, AgentScheduler, AgentSessionMetadata, AgentShellStore, AgentShellVisibility,
    AgentTranscriptStore, AgentTurnLedger, AgentTurnRecord, AgentTurnState, AgentTurnTrigger,
    AuditActor, AuditDeferredWrite, AuditLog, AuditRecord, AuthStore, BTreeMap, BTreeSet,
    BlockedApprovalQueue, BlockedApprovalRequest, ConfigFormat, ConfigLayer, ConfigScope,
    ControlIdempotencyCache, DEFAULT_AGENT_ACTION_FAILURE_RETRY_LIMIT,
    DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT,
    DEFAULT_AGENT_IMPLEMENTATION_PRESSURE_AFTER_SHELL_ACTIONS, DEFAULT_AGENT_ROUTING,
    DEFAULT_HISTORY_LIMIT, DEFAULT_HISTORY_ROTATE_LINES, DEFAULT_MAX_ROOT_SUBAGENTS,
    DEFAULT_MAX_SUBAGENT_DEPTH, DEFAULT_MAX_SUBAGENT_PANES_PER_WINDOW,
    DEFAULT_MAX_SUBAGENTS_PER_SUBAGENT, DEFAULT_PANE_TERM, DEFAULT_SUBAGENT_WAIT_POLICY,
    DeferredAgentPromptHistoryWrite, DeferredAgentTranscriptWrite,
    DeferredCommandPromptHistoryWrite, DeferredConfigFileWrite, DeferredProjectConfigWrite,
    DeferredProjectInstructionWrite, EventKind, EventLog, FocusedShellHookQueue, HostClipboard,
    KeyBindings, MEZ_ENV_FIELD_SEPARATOR, McpRegistry, McpServerStatus, McpStartupTransportPlan,
    MemoryRecord, MessageService, MezError, ModelProfile, ModelTokenUsage, ModelTokenUsageKey,
    PaneProcessManager, PaneReadinessOverrideStore, PasteBuffers, Path, PathBuf,
    PermissionAuthorityChange, PermissionPolicy, ProjectTrustStore, Result,
    RuntimeConfigApplyReport, RuntimeHttpMcpTransportState, RuntimeLifecycleState,
    RuntimeMcpRetryReport, RuntimeMcpTransportSet, RuntimeModelProfileOverrideStore,
    RuntimePresetRegistry, RuntimeProviderConfig, RuntimeProviderRegistry,
    RuntimeRegistryUpdatePlan, RuntimeSessionService, ScopeRegistry, Session, SessionApprovalStore,
    SessionMemoryStore, SessionRegistry, TerminalScreen, ToolDiscoveryCache, TrustDecision, Value,
    agent_shell_visibility_json_name, apply_registry_update, builtin_subagent_profiles,
    compare_approval_policy_authority, compose_effective_config, current_unix_seconds,
    discover_existing_overlays, discover_project_root, discover_streamable_http_mcp_server,
    ensure_absolute, ensure_no_mez_separator, fs, json_escape,
    runtime_agent_action_failure_retry_limit_from_config, runtime_agent_auto_sizing_from_config,
    runtime_agent_compaction_raw_retention_percent_from_config,
    runtime_agent_custom_system_prompt_from_config,
    runtime_agent_implementation_pressure_after_shell_actions_from_config,
    runtime_agent_personality_profiles_from_config, runtime_agent_routing_from_config,
    runtime_approval_policy_name, runtime_audit_config_present, runtime_audit_log_from_config,
    runtime_command_bindings_from_effective, runtime_default_agent_personality_from_config,
    runtime_default_models_for_provider, runtime_effective_config_value,
    runtime_history_limit_from_config, runtime_history_rotate_lines_from_config,
    runtime_hook_definitions_from_config, runtime_host_clipboard_from_config,
    runtime_key_bindings_from_config, runtime_max_concurrent_agents_from_config,
    runtime_max_root_subagents_from_config, runtime_max_subagent_depth_from_config,
    runtime_max_subagent_panes_per_window_from_config,
    runtime_max_subagents_per_subagent_from_config, runtime_mcp_registry_from_config,
    runtime_pane_by_id, runtime_pane_frame_position_from_config,
    runtime_pane_frame_style_from_config, runtime_pane_frame_template_from_config,
    runtime_pane_frame_visible_fields_from_config, runtime_pane_frames_enabled_from_config,
    runtime_parse_approval_policy, runtime_permission_policy_from_config,
    runtime_preset_registry_from_config, runtime_provider_registry_from_config,
    runtime_subagent_profiles_from_config, runtime_subagent_wait_policy_from_config,
    runtime_terminal_clipboard_from_config, runtime_terminal_cursor_blink_from_config,
    runtime_terminal_cursor_blink_interval_ms_from_config,
    runtime_terminal_cursor_style_from_config, runtime_terminal_reduced_motion_from_config,
    runtime_terminal_render_rate_limit_fps_from_config,
    runtime_terminal_resize_debounce_ms_from_config, runtime_terminal_term_from_config,
    runtime_ui_theme_from_config, runtime_window_frame_position_from_config,
    runtime_window_frame_right_status_template_from_config, runtime_window_frame_style_from_config,
    runtime_window_frame_template_from_config, runtime_window_frame_visible_fields_from_config,
    runtime_window_frames_enabled_from_config, spawn_stdio_mcp_connection,
};

// RuntimeSessionService construction, accessors, and live config application.

/// Returns persisted per-model token accounting with legacy aggregate fallback.
fn runtime_agent_token_usage_by_model_from_metadata(
    metadata: &AgentSessionMetadata,
) -> BTreeMap<ModelTokenUsageKey, ModelTokenUsage> {
    if !metadata.token_usage_by_model.is_empty() {
        return metadata.token_usage_by_model.clone();
    }
    if metadata.token_usage.is_zero() {
        return BTreeMap::new();
    }
    BTreeMap::from([(ModelTokenUsageKey::unknown(), metadata.token_usage)])
}

/// Aggregates per-model token accounting for legacy metadata readers.
fn runtime_agent_total_token_usage_by_model(
    usage_by_model: &BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
) -> ModelTokenUsage {
    let mut total = ModelTokenUsage::default();
    for usage in usage_by_model.values() {
        total.add_assign(*usage);
    }
    total
}

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
        Ok(Self {
            session,
            window_created_at_unix_seconds,
            config_layers: Vec::new(),
            config_root: None,
            control_idempotency,
            message_service,
            pane_processes,
            async_owned_pane_processes: BTreeMap::new(),
            async_runtime_metrics: None,
            runtime_metrics: Default::default(),
            pane_current_working_directories: BTreeMap::new(),
            deferred_pane_inputs: Vec::new(),
            deferred_pane_resizes: BTreeMap::new(),
            deferred_pane_terminations: BTreeMap::new(),
            deferred_pane_pipe_writes: Vec::new(),
            deferred_audit_writes: Vec::new(),
            deferred_agent_transcript_writes: Vec::new(),
            deferred_agent_prompt_history_writes: Vec::new(),
            deferred_command_prompt_history_writes: Vec::new(),
            deferred_config_file_writes: Vec::new(),
            deferred_project_config_writes: Vec::new(),
            deferred_project_instruction_writes: Vec::new(),
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
            defer_file_pane_pipe_writes: false,
            defer_command_pane_pipe_startup: false,
            paste_buffers: PasteBuffers::default_limit(),
            active_paste_buffer: None,
            host_clipboard: HostClipboard::system(),
            active_copy_modes: BTreeMap::new(),
            scrollback_copy_mode_panes: BTreeSet::new(),
            mouse_resize_drag_state: None,
            mouse_selection_drag_state: None,
            pressed_window_action: None,
            pane_transcript_refs: BTreeMap::new(),
            terminal_history_limit: DEFAULT_HISTORY_LIMIT,
            terminal_history_rotate_lines: DEFAULT_HISTORY_ROTATE_LINES,
            terminal_term: DEFAULT_PANE_TERM.to_string(),
            window_frames_enabled: true,
            window_frame_template: crate::terminal::DEFAULT_WINDOW_FRAME_TEMPLATE.to_string(),
            window_frame_right_status_template:
                crate::terminal::DEFAULT_WINDOW_FRAME_RIGHT_STATUS_TEMPLATE.to_string(),
            window_frame_position: crate::terminal::TerminalFramePosition::Bottom,
            window_frame_style: crate::terminal::TerminalFrameStyle::Default,
            window_frame_visible_fields: crate::terminal::DEFAULT_WINDOW_FRAME_VISIBLE_FIELDS
                .iter()
                .map(|field| (*field).to_string())
                .collect(),
            pane_frames_enabled: true,
            pane_frame_template: crate::terminal::DEFAULT_PANE_FRAME_TEMPLATE.to_string(),
            pane_frame_position: crate::terminal::TerminalFramePosition::Top,
            pane_frame_style: crate::terminal::TerminalFrameStyle::Default,
            pane_frame_visible_fields: crate::terminal::DEFAULT_PANE_FRAME_VISIBLE_FIELDS
                .iter()
                .map(|field| (*field).to_string())
                .collect(),
            terminal_cursor_style: crate::terminal::TerminalCursorStyle::Block,
            terminal_cursor_blink: false,
            terminal_cursor_blink_interval_ms: 500,
            terminal_resize_debounce_ms: 200,
            terminal_render_rate_limit_fps: 5,
            terminal_reduced_motion: false,
            terminal_clipboard: "external".to_string(),
            ui_theme: crate::terminal::UiTheme::default(),
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
            audit_log: None,
            defer_audit_writes: false,
            agent_scheduler: AgentScheduler::with_default_limit(),
            agent_shell_store: AgentShellStore::default(),
            agent_pane_trace_logs: BTreeMap::new(),
            agent_session_patch_records: BTreeMap::new(),
            agent_subshell_panes: BTreeSet::new(),
            agent_subshell_command_exit_panes: BTreeSet::new(),
            agent_turn_ledger: AgentTurnLedger::new(false),
            agent_turn_contexts: BTreeMap::new(),
            agent_turn_executions: BTreeMap::new(),
            agent_turn_pending_steering: BTreeMap::new(),
            agent_turn_failure_feedback_attempts: BTreeMap::new(),
            agent_action_failure_retry_limit: DEFAULT_AGENT_ACTION_FAILURE_RETRY_LIMIT,
            agent_implementation_pressure_after_shell_actions:
                DEFAULT_AGENT_IMPLEMENTATION_PRESSURE_AFTER_SHELL_ACTIONS,
            agent_turn_shell_dispatch_history: BTreeMap::new(),
            agent_turn_network_action_history: BTreeMap::new(),
            agent_pre_shell_hook_completions: BTreeSet::new(),
            agent_copy_outputs: BTreeMap::new(),
            agent_modified_files: BTreeMap::new(),
            primary_command_prompt_history: Vec::new(),
            primary_prompt_input: None,
            primary_prefix_key_pending: false,
            agent_prompt_inputs: BTreeMap::new(),
            primary_display_overlay: None,
            primary_error_status_overlay: None,
            pane_agent_status_selector: None,
            agent_turn_model_profiles: BTreeMap::new(),
            agent_planning_modes: BTreeSet::new(),
            agent_response_styles: BTreeMap::new(),
            agent_compaction_raw_retention_percent: DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT,
            agent_compacting_panes: BTreeMap::new(),
            pending_agent_compaction_tasks: BTreeMap::new(),
            claimed_agent_compaction_tasks: BTreeMap::new(),
            agent_routing: DEFAULT_AGENT_ROUTING,
            agent_routing_overrides: BTreeMap::new(),
            agent_auto_sizing: Default::default(),
            agent_auto_sizing_overrides: BTreeMap::new(),
            agent_token_usage_by_conversation: BTreeMap::new(),
            agent_context_usage_by_conversation: BTreeMap::new(),
            agent_context_usage_snapshot_by_conversation: BTreeMap::new(),
            agent_quota_usage_by_conversation: BTreeMap::new(),
            provider_model_catalog_cache: BTreeMap::new(),
            pending_agent_provider_tasks: BTreeSet::new(),
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
            defer_agent_transcript_writes: false,
            defer_config_file_writes: false,
            defer_project_config_writes: false,
            defer_project_instruction_writes: false,
            subagent_scopes: ScopeRegistry::default(),
            project_trust_store: None,
            project_trust_database_path: None,
            announced_project_trust_roots: BTreeSet::new(),
            hook_definitions: Vec::new(),
            defer_program_hooks: false,
            deferred_program_hooks: Vec::new(),
            focused_shell_hooks: FocusedShellHookQueue::default(),
            next_focused_shell_hook_marker: 1,
            focused_shell_hook_transactions: BTreeMap::new(),
            focused_shell_hook_results: Vec::new(),
            event_log,
            lifecycle_state,
            session_registry: None,
            defer_registry_updates: false,
            deferred_registry_update: None,
            socket_path,
            created_at_unix_seconds,
            last_attach_at_unix_seconds: None,
        })
    }

    /// Runs the session operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Attaches the session registry used to publish live session metadata.
    ///
    /// Foreground daemons call this after constructing the service so attach,
    /// detach, process-exit, and control mutations can refresh `mez list`
    /// without depending on an outer listener loop. Test services leave the
    /// registry unset unless the test needs to verify persisted discovery
    /// state.
    pub fn set_session_registry(&mut self, registry: SessionRegistry) {
        self.session_registry = Some(registry);
    }

    /// Enables or disables deferred registry persistence for async actor owners.
    pub(crate) fn set_defer_registry_updates(&mut self, defer: bool) {
        self.defer_registry_updates = defer;
    }

    /// Persists the current registry update plan when this service owns a
    /// registry handle.
    ///
    /// A service without an attached registry treats persistence as a no-op so
    /// unit tests and in-memory control helpers can continue using the runtime
    /// without a filesystem-backed session registry.
    pub(crate) fn persist_registry_update(&self) -> Result<bool> {
        let update = self.registry_update_plan();
        self.persist_registry_update_plan(&update)
    }

    /// Defers or persists a registry update from a mutable service path.
    pub(crate) fn persist_or_defer_registry_update(&mut self) -> Result<bool> {
        let update = self.registry_update_plan();
        self.persist_or_defer_registry_update_plan(update)
    }

    /// Defers or persists a precomputed registry update from a mutable service path.
    pub(crate) fn persist_or_defer_registry_update_plan(
        &mut self,
        update: RuntimeRegistryUpdatePlan,
    ) -> Result<bool> {
        if self.session_registry.is_none() {
            return Ok(false);
        }
        if self.defer_registry_updates {
            self.deferred_registry_update = Some(update);
            return Ok(true);
        }
        self.persist_registry_update_plan(&update)
    }

    /// Returns the registry handle and current update plan for async persistence.
    ///
    /// The async runtime actor uses this to move filesystem-backed registry
    /// mutation onto the persistence worker while keeping the update derived
    /// from actor-owned state.
    pub(crate) fn registry_update_for_async_persistence(
        &self,
    ) -> Option<(SessionRegistry, RuntimeRegistryUpdatePlan)> {
        let registry = self.session_registry.clone()?;
        Some((registry, self.registry_update_plan()))
    }

    /// Drains a registry update queued by actor-owned compatibility service paths.
    pub(crate) fn drain_deferred_registry_update_for_async_persistence(
        &mut self,
    ) -> Option<(SessionRegistry, RuntimeRegistryUpdatePlan)> {
        let registry = self.session_registry.clone()?;
        let update = self.deferred_registry_update.take()?;
        Some((registry, update))
    }

    /// Persists a precomputed registry update plan when a registry is attached.
    pub(super) fn persist_registry_update_plan(
        &self,
        update: &RuntimeRegistryUpdatePlan,
    ) -> Result<bool> {
        let Some(registry) = self.session_registry.as_ref() else {
            return Ok(false);
        };
        apply_registry_update(registry, update)
    }
    /// Replaces the cached async runtime metrics snapshot used by display commands.
    pub(crate) fn set_async_runtime_metrics(
        &mut self,
        metrics: crate::async_runtime::AsyncRuntimeActorMetrics,
    ) {
        self.async_runtime_metrics = Some(metrics);
    }
    /// Returns the cached async runtime metrics snapshot when the actor provided one.
    pub(super) fn async_runtime_metrics(
        &self,
    ) -> Option<&crate::async_runtime::AsyncRuntimeActorMetrics> {
        self.async_runtime_metrics.as_ref()
    }
    /// Returns runtime-owned agent, provider, prompt-cache, and shell metrics.
    pub(super) fn runtime_metrics(&self) -> &super::types::RuntimeMetricsSnapshot {
        &self.runtime_metrics
    }

    /// Runs the config layers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn config_layers(&self) -> &[ConfigLayer] {
        &self.config_layers
    }

    /// Runs the set config root operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_config_root(&mut self, root: PathBuf) {
        self.config_root = Some(root);
    }

    /// Runs the replace config layers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn replace_config_layers(
        &mut self,
        layers: Vec<ConfigLayer>,
    ) -> Result<RuntimeConfigApplyReport> {
        self.config_layers = layers;
        self.apply_runtime_config_layers()
    }

    /// Runs the replace config layers async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn replace_config_layers_async(
        &mut self,
        layers: Vec<ConfigLayer>,
    ) -> Result<RuntimeConfigApplyReport> {
        self.config_layers = layers;
        self.apply_runtime_config_layers_async().await
    }

    /// Refreshes trusted or pending project overlay layers for a pane cwd.
    ///
    /// The daemon can outlive shell-directory changes, so project `.mezzanine`
    /// overlays cannot be discovered only at startup. This refresh keeps the
    /// active layer list aligned with the pane's current repository before
    /// agent work or explicit skill display relies on project-scoped config.
    ///
    /// # Parameters
    /// - `pane_id`: Pane whose current working directory determines the
    ///   effective project root and overlay files.
    pub(super) fn refresh_project_config_layers_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Result<usize> {
        let Some(current_dir) = self.pane_current_working_directory(pane_id) else {
            return Ok(0);
        };
        let project_root = discover_project_root(&current_dir);
        let overlay_files = discover_existing_overlays(&project_root, &current_dir)?;
        if overlay_files.is_empty() {
            return self.remove_project_config_layers_for_root(&project_root);
        }

        let trusted = self
            .project_trust_store
            .as_ref()
            .and_then(|store| store.get(&project_root))
            .is_some_and(|record| record.state == TrustDecision::Trusted);
        let selected = overlay_files.iter().cloned().collect::<BTreeSet<_>>();
        let before = self.config_layers.clone();
        self.config_layers.retain(|layer| {
            layer.scope != ConfigScope::ProjectOverlay
                || layer
                    .path
                    .as_ref()
                    .is_some_and(|path| selected.contains(path))
        });

        let overlay_count = overlay_files.len();
        for (index, overlay_path) in overlay_files.into_iter().enumerate() {
            let name = if overlay_count == 1 {
                "project".to_string()
            } else {
                format!("project:{}", index + 1)
            };
            let refreshed = ConfigLayer {
                name,
                path: Some(overlay_path.clone()),
                format: ConfigFormat::from_path(&overlay_path)?,
                scope: ConfigScope::ProjectOverlay,
                trusted,
                text: fs::read_to_string(&overlay_path)?,
            };
            if let Some(existing) = self.config_layers.iter_mut().find(|layer| {
                layer.scope == ConfigScope::ProjectOverlay
                    && layer.path.as_ref() == Some(&overlay_path)
            }) {
                *existing = refreshed;
            } else {
                self.config_layers.push(refreshed);
            }
        }

        if self.config_layers == before {
            return Ok(0);
        }
        let report = self.apply_runtime_config_layers()?;
        Ok(report.applied_layers.len() + report.skipped_layers.len())
    }

    /// Removes stale project overlay layers when the active pane has no
    /// discoverable overlay files.
    ///
    /// # Parameters
    /// - `_project_root`: Current project root, retained for call-site clarity.
    fn remove_project_config_layers_for_root(&mut self, _project_root: &Path) -> Result<usize> {
        let before_len = self.config_layers.len();
        self.config_layers
            .retain(|layer| layer.scope != ConfigScope::ProjectOverlay);
        let removed = before_len.saturating_sub(self.config_layers.len());
        if removed > 0 {
            self.apply_runtime_config_layers()?;
        }
        Ok(removed)
    }

    /// Runs the reload config layers from disk operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn reload_config_layers_from_disk(&mut self) -> Result<RuntimeConfigApplyReport> {
        for layer in &mut self.config_layers {
            let Some(path) = layer.path.as_ref() else {
                continue;
            };
            layer.format = ConfigFormat::from_path(path)?;
            layer.text = fs::read_to_string(path)?;
        }
        self.apply_runtime_config_layers()
    }

    /// Runs the reload config layers from disk async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn reload_config_layers_from_disk_async(
        &mut self,
    ) -> Result<RuntimeConfigApplyReport> {
        for layer in &mut self.config_layers {
            let Some(path) = layer.path.as_ref() else {
                continue;
            };
            layer.format = ConfigFormat::from_path(path)?;
            layer.text = fs::read_to_string(path)?;
        }
        self.apply_runtime_config_layers_async().await
    }

    /// Captures live generated model profiles referenced by override state.
    fn preserved_model_override_profiles(&self) -> BTreeMap<String, ModelProfile> {
        let mut names = BTreeSet::new();
        if let Some(profile) = self.model_profile_overrides.session_profile.as_ref() {
            names.insert(profile.clone());
        }
        names.extend(
            self.model_profile_overrides
                .window_profiles
                .values()
                .cloned(),
        );
        names.extend(self.model_profile_overrides.pane_profiles.values().cloned());
        names.extend(
            self.model_profile_overrides
                .agent_profiles
                .values()
                .cloned(),
        );
        names.extend(
            self.model_profile_overrides
                .subagent_profiles
                .values()
                .cloned(),
        );
        names
            .into_iter()
            .filter_map(|name| {
                self.provider_registry
                    .profile(&name)
                    .cloned()
                    .map(|profile| (name, profile))
            })
            .collect()
    }

    /// Runs the apply runtime config layers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn apply_runtime_config_layers(&mut self) -> Result<RuntimeConfigApplyReport> {
        let effective = compose_effective_config(&self.config_layers)?;
        let structured = runtime_effective_config_value(&self.config_layers)?;
        let terminal_history_limit = runtime_history_limit_from_config(&structured)?;
        let terminal_history_rotate_lines = runtime_history_rotate_lines_from_config(&structured)?;
        let terminal_term = runtime_terminal_term_from_config(&structured)?;
        let window_frames_enabled = runtime_window_frames_enabled_from_config(&structured)?;
        let window_frame_template = runtime_window_frame_template_from_config(&structured)?;
        let window_frame_right_status_template =
            runtime_window_frame_right_status_template_from_config(&structured)?;
        let window_frame_position = runtime_window_frame_position_from_config(&structured)?;
        let window_frame_style = runtime_window_frame_style_from_config(&structured)?;
        let window_frame_visible_fields =
            runtime_window_frame_visible_fields_from_config(&structured)?;
        let pane_frames_enabled = runtime_pane_frames_enabled_from_config(&structured)?;
        let pane_frame_template = runtime_pane_frame_template_from_config(&structured)?;
        let pane_frame_position = runtime_pane_frame_position_from_config(&structured)?;
        let pane_frame_style = runtime_pane_frame_style_from_config(&structured)?;
        let pane_frame_visible_fields = runtime_pane_frame_visible_fields_from_config(&structured)?;
        let terminal_cursor_style = runtime_terminal_cursor_style_from_config(&structured)?;
        let terminal_cursor_blink = runtime_terminal_cursor_blink_from_config(&structured)?;
        let terminal_cursor_blink_interval_ms =
            runtime_terminal_cursor_blink_interval_ms_from_config(&structured)?;
        let terminal_resize_debounce_ms =
            runtime_terminal_resize_debounce_ms_from_config(&structured)?;
        let terminal_render_rate_limit_fps =
            runtime_terminal_render_rate_limit_fps_from_config(&structured)?;
        let terminal_reduced_motion = runtime_terminal_reduced_motion_from_config(&structured)?;
        let terminal_clipboard = runtime_terminal_clipboard_from_config(&structured)?;
        let host_clipboard = runtime_host_clipboard_from_config(&structured)?;
        let ui_theme = runtime_ui_theme_from_config(&structured)?;
        let key_bindings = runtime_key_bindings_from_config(&structured)?;
        let command_bindings = runtime_command_bindings_from_effective(&effective)?;
        let audit_log = if runtime_audit_config_present(&structured) {
            Some(runtime_audit_log_from_config(
                &structured,
                self.config_root.as_deref(),
            )?)
        } else {
            None
        };
        self.terminal_history_limit = terminal_history_limit;
        self.terminal_history_rotate_lines = terminal_history_rotate_lines;
        self.terminal_term = terminal_term;
        self.window_frames_enabled = window_frames_enabled;
        self.window_frame_template = window_frame_template;
        self.window_frame_right_status_template = window_frame_right_status_template;
        self.window_frame_position = window_frame_position;
        self.window_frame_style = window_frame_style;
        self.window_frame_visible_fields = window_frame_visible_fields;
        self.pane_frames_enabled = pane_frames_enabled;
        self.pane_frame_template = pane_frame_template;
        self.pane_frame_position = pane_frame_position;
        self.pane_frame_style = pane_frame_style;
        self.pane_frame_visible_fields = pane_frame_visible_fields;
        self.terminal_cursor_style = terminal_cursor_style;
        self.terminal_cursor_blink = terminal_cursor_blink;
        self.terminal_cursor_blink_interval_ms = terminal_cursor_blink_interval_ms;
        self.terminal_resize_debounce_ms = terminal_resize_debounce_ms;
        self.terminal_render_rate_limit_fps = terminal_render_rate_limit_fps;
        self.terminal_reduced_motion = terminal_reduced_motion;
        self.terminal_clipboard = terminal_clipboard;
        self.host_clipboard = host_clipboard;
        self.ui_theme = ui_theme;
        self.key_bindings = key_bindings;
        self.command_bindings = command_bindings;
        match audit_log {
            Some(Some(audit_log)) => self.set_audit_log(audit_log),
            Some(None) => self.clear_audit_log(),
            None => {}
        }
        for screen in self.pane_screens.values_mut() {
            screen.set_history_limit(self.terminal_history_limit)?;
            screen.set_history_rotate_lines(self.terminal_history_rotate_lines)?;
        }
        let max_concurrent_agents = runtime_max_concurrent_agents_from_config(&structured)?;
        self.max_subagent_panes_per_window =
            runtime_max_subagent_panes_per_window_from_config(&structured)?;
        self.max_root_subagents = runtime_max_root_subagents_from_config(&structured)?;
        self.max_subagents_per_subagent =
            runtime_max_subagents_per_subagent_from_config(&structured)?;
        self.max_subagent_depth = runtime_max_subagent_depth_from_config(&structured)?;
        self.subagent_wait_policy = runtime_subagent_wait_policy_from_config(&structured)?;
        self.agent_compaction_raw_retention_percent =
            runtime_agent_compaction_raw_retention_percent_from_config(&structured)?;
        self.agent_routing = runtime_agent_routing_from_config(&structured)?;
        self.agent_action_failure_retry_limit =
            runtime_agent_action_failure_retry_limit_from_config(&structured)?;
        self.agent_implementation_pressure_after_shell_actions =
            runtime_agent_implementation_pressure_after_shell_actions_from_config(&structured)?;
        self.agent_auto_sizing = runtime_agent_auto_sizing_from_config(&structured)?;
        self.agent_scheduler
            .set_max_concurrent_agents(max_concurrent_agents)?;
        self.start_ready_agent_turns()?;
        let mut permission_policy = runtime_permission_policy_from_config(&structured)?;
        if let Some(approval_policy) = self.live_approval_policy_override {
            permission_policy.approval_policy = approval_policy;
        }
        if let Some(active) = self.live_approval_bypass_override {
            permission_policy.set_approval_bypass(active);
        }
        self.permission_policy = permission_policy;
        let preserved_model_profiles = self.preserved_model_override_profiles();
        let mut provider_registry = runtime_provider_registry_from_config(&structured)?;
        for (name, profile) in preserved_model_profiles {
            if provider_registry.provider(&profile.provider).is_some() {
                provider_registry.profiles.entry(name).or_insert(profile);
            }
        }
        self.provider_registry = provider_registry;
        self.preset_registry =
            runtime_preset_registry_from_config(&structured, &self.provider_registry.profiles)?;
        // Synthesize provider entries for authenticated providers not in config.
        if let Some(auth_store) = self.auth_store.as_ref() {
            let all_metadata = auth_store.read_all_metadata().unwrap_or_default();
            for auth_provider in all_metadata.keys() {
                if !self.provider_registry.providers.contains_key(auth_provider)
                    && let Ok(default_models) = runtime_default_models_for_provider(auth_provider)
                {
                    self.provider_registry.providers.insert(
                        auth_provider.clone(),
                        RuntimeProviderConfig {
                            provider_id: auth_provider.clone(),
                            kind: auth_provider.clone(),
                            auth_profile: "default".to_string(),
                            base_url: None,
                            models: default_models.iter().map(|m| (*m).to_string()).collect(),
                            default_model: Some(
                                default_models
                                    .first()
                                    .map(|m| (*m).to_string())
                                    .unwrap_or_default(),
                            ),
                            options: BTreeMap::new(),
                        },
                    );
                }
            }
        }
        self.provider_model_catalog_cache.clear();
        self.subagent_profiles = runtime_subagent_profiles_from_config(&structured)?;
        self.agent_personality_profiles =
            runtime_agent_personality_profiles_from_config(&structured)?;
        self.default_agent_personality =
            runtime_default_agent_personality_from_config(&structured)?;
        if let Some(default_personality) = self.default_agent_personality.as_ref()
            && !self
                .agent_personality_profiles
                .contains_key(default_personality)
        {
            return Err(MezError::config(format!(
                "agents.default_personality `{default_personality}` is not defined in personalities"
            )));
        }
        self.custom_agent_system_prompt =
            runtime_agent_custom_system_prompt_from_config(&structured)?;
        self.hook_definitions = runtime_hook_definitions_from_config(&structured)?;
        let mut registry = runtime_mcp_registry_from_config(&structured)?;
        let environment = std::env::vars().collect::<BTreeMap<_, _>>();
        let blacklisted = registry.blacklist_servers_with_missing_environment(&environment)?;
        self.mcp_transports.clear();
        let configured = registry.list_servers().len();
        self.mcp_registry = registry;
        let trust_prompts_announced =
            self.append_project_trust_prompt_events_for_pending_layers()?;
        let _ = self.load_persistent_memory_into_session();
        Ok(RuntimeConfigApplyReport {
            applied_layers: effective.applied_layers().to_vec(),
            skipped_layers: effective.skipped_layers().to_vec(),
            terminal_history_limit: self.terminal_history_limit,
            terminal_history_rotate_lines: self.terminal_history_rotate_lines,
            terminal_term: self.terminal_term.clone(),
            window_frames_enabled: self.window_frames_enabled,
            pane_frames_enabled: self.pane_frames_enabled,
            max_concurrent_agents,
            permission_policy_applied: true,
            mcp_servers_configured: configured,
            mcp_servers_blacklisted: blacklisted,
            providers_configured: self.provider_registry.providers.len(),
            model_profiles_configured: self.provider_registry.profiles.len(),
            default_model_profile: self.provider_registry.default_profile.clone(),
            hooks_configured: self.hook_definitions.len(),
            project_trust_prompts_announced: trust_prompts_announced,
            ui_theme: self.ui_theme.name.clone(),
        })
    }

    /// Runs the apply runtime config layers async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn apply_runtime_config_layers_async(&mut self) -> Result<RuntimeConfigApplyReport> {
        let mut report = self.apply_runtime_config_layers()?;
        let environment = std::env::vars().collect::<BTreeMap<_, _>>();
        let mut registry = std::mem::take(&mut self.mcp_registry);
        let discovery_blacklisted = self
            .discover_runtime_mcp_transports_async(&mut registry, &environment)
            .await?;
        report.mcp_servers_blacklisted.extend(discovery_blacklisted);
        self.mcp_registry = registry;
        Ok(report)
    }

    /// Discovers configured MCP transports that are not already available.
    ///
    /// MCP transports are runtime-owned resources shared across agent turns.
    /// This method is intentionally lazy and preserves existing transports so
    /// an agent prompt or `/list-mcp` does not disconnect working servers.
    pub(crate) async fn ensure_runtime_mcp_transports_discovered_async(
        &mut self,
    ) -> Result<Vec<String>> {
        let environment = std::env::vars().collect::<BTreeMap<_, _>>();
        let mut registry = std::mem::take(&mut self.mcp_registry);
        let blacklisted = self
            .discover_runtime_mcp_transports_async(&mut registry, &environment)
            .await?;
        self.mcp_registry = registry;
        let _ = self.persist_registry_update_plan(&self.registry_update_plan());
        Ok(blacklisted)
    }

    /// Loads global and project-scoped persistent memory records into the
    /// session memory store so agents can benefit from user-stored context
    /// loaded through the CLI.
    pub(super) fn load_persistent_memory_into_session(&mut self) -> Result<()> {
        let Some(ref config_root) = self.config_root else {
            return Ok(());
        };
        let store = crate::memory::PersistentMemoryStore::under_config_root(config_root);
        let Ok(records) = store.list() else {
            return Ok(());
        };
        for record in &records {
            match &record.scope {
                crate::memory::MemoryScope::Global | crate::memory::MemoryScope::Project { .. }
                    if record.validate_for_session().is_ok() =>
                {
                    let _ = self.session_memory.upsert(record.clone());
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Persists global and project-scoped session memory records to the
    /// persistent store so they survive beyond this session.
    pub(super) fn persist_session_memory_to_disk(&mut self) {
        let Some(ref config_root) = self.config_root else {
            return;
        };
        let store = crate::memory::PersistentMemoryStore::under_config_root(config_root);
        for record in self.session_memory.export() {
            match &record.scope {
                crate::memory::MemoryScope::Global | crate::memory::MemoryScope::Project { .. }
                    if record.validate_for_persistence().is_ok() =>
                {
                    let _ = store.upsert(record);
                }
                _ => {}
            }
        }
    }

    /// Runs the append project trust prompt events for pending layers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_project_trust_prompt_events_for_pending_layers(
        &mut self,
    ) -> Result<usize> {
        let mut overlays_by_root: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
        for layer in &self.config_layers {
            if layer.scope != ConfigScope::ProjectOverlay || layer.trusted {
                continue;
            }
            let Some(path) = layer.path.as_ref() else {
                continue;
            };
            let root = path
                .parent()
                .map(discover_project_root)
                .unwrap_or_else(|| discover_project_root(path));
            let pending = self
                .project_trust_store
                .as_ref()
                .and_then(|store| store.get(&root))
                .map(|record| record.state == TrustDecision::Pending)
                .unwrap_or(true);
            if pending {
                overlays_by_root.entry(root).or_default().push(path.clone());
            }
        }

        let mut announced = 0usize;
        for (root, overlays) in overlays_by_root {
            if !self.announced_project_trust_roots.insert(root.clone()) {
                continue;
            }
            let overlay_json = overlays
                .iter()
                .map(|path| format!(r#""{}""#, json_escape(&path.to_string_lossy())))
                .collect::<Vec<_>>()
                .join(",");
            self.append_primary_lifecycle_event(
                EventKind::ConfigChanged,
                format!(
                    r#"{{"project_root":"{}","state":"pending","blocks_until_primary_decision":true,"overlay_files":[{}],"prompt":"project trust decision required","approve_method":"project/trust/decide","reject_method":"project/trust/decide","trust_command":"/trust {}"}}"#,
                    json_escape(&root.to_string_lossy()),
                    overlay_json,
                    json_escape(&root.to_string_lossy())
                ),
            )?;
            announced = announced.saturating_add(1);
        }
        Ok(announced)
    }

    /// Runs the control idempotency operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn control_idempotency(&self) -> &ControlIdempotencyCache {
        &self.control_idempotency
    }

    /// Appends a runtime diagnostic event for async worker status that has
    /// re-entered the single-owner actor path.
    pub(crate) fn append_runtime_diagnostic_event(&mut self, payload: String) -> Result<()> {
        self.append_lifecycle_event(EventKind::Diagnostic, payload)
    }

    /// Runs the message service operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn message_service(&self) -> &MessageService {
        &self.message_service
    }

    /// Runs the message service mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn message_service_mut(&mut self) -> &mut MessageService {
        &mut self.message_service
    }

    /// Runs the pane processes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn pane_processes(&self) -> &PaneProcessManager {
        &self.pane_processes
    }

    /// Runs the pane processes mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn pane_processes_mut(&mut self) -> &mut PaneProcessManager {
        &mut self.pane_processes
    }

    /// Runs the pane screens operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn pane_screens(&self) -> &BTreeMap<String, TerminalScreen> {
        &self.pane_screens
    }

    /// Runs the pane screen operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn pane_screen(&self, pane_id: &str) -> Option<&TerminalScreen> {
        self.pane_screens.get(pane_id)
    }

    /// Runs the terminal history limit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn terminal_history_limit(&self) -> usize {
        self.terminal_history_limit
    }

    /// Returns the configured history overflow rotation batch size.
    pub fn terminal_history_rotate_lines(&self) -> usize {
        self.terminal_history_rotate_lines
    }

    /// Runs the terminal term operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn terminal_term(&self) -> &str {
        &self.terminal_term
    }

    /// Runs the paste buffers operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn paste_buffers(&self) -> &PasteBuffers {
        &self.paste_buffers
    }

    /// Runs the record pane transcript ref operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn record_pane_transcript_ref(
        &mut self,
        pane_id: impl Into<String>,
        transcript_ref: impl Into<String>,
    ) -> Result<()> {
        let pane_id = pane_id.into();
        let transcript_ref = transcript_ref.into();
        if self.find_pane_descriptor(&pane_id).is_none() {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotFound,
                "pane not found for transcript reference",
            ));
        }
        if transcript_ref.trim().is_empty() {
            return Err(MezError::invalid_args(
                "pane transcript reference must not be empty",
            ));
        }
        if transcript_ref.contains(MEZ_ENV_FIELD_SEPARATOR) {
            return Err(MezError::invalid_args(
                "pane transcript reference contains reserved separator",
            ));
        }
        let refs = self.pane_transcript_refs.entry(pane_id).or_default();
        if !refs.iter().any(|existing| existing == &transcript_ref) {
            refs.push(transcript_ref);
        }
        Ok(())
    }

    /// Runs the permission policy operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn permission_policy(&self) -> &PermissionPolicy {
        &self.permission_policy
    }

    /// Runs the permission policy mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn permission_policy_mut(&mut self) -> &mut PermissionPolicy {
        &mut self.permission_policy
    }

    /// Applies an explicit user-selected approval-bypass state.
    ///
    /// # Parameters
    /// - `active`: Whether approval bypass should be active after the change.
    pub fn set_live_approval_bypass_override(&mut self, active: bool) {
        self.live_approval_bypass_override = Some(active);
        self.permission_policy.set_approval_bypass(active);
    }

    /// Applies an explicit user-selected approval policy override.
    ///
    /// # Parameters
    /// - `policy`: Approval policy that should survive unrelated config reloads.
    pub fn set_live_approval_policy_override(
        &mut self,
        policy: crate::permissions::ApprovalPolicy,
    ) {
        self.live_approval_policy_override = Some(policy);
        self.permission_policy.approval_policy = policy;
    }

    /// Runs the blocked approvals operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn blocked_approvals(&self) -> &BlockedApprovalQueue {
        &self.blocked_approvals
    }

    /// Runs the session approvals operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn session_approvals(&self) -> &SessionApprovalStore {
        &self.session_approvals
    }

    /// Runs the session approvals mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn session_approvals_mut(&mut self) -> &mut SessionApprovalStore {
        &mut self.session_approvals
    }

    /// Runs the queue blocked approval operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn queue_blocked_approval(&mut self, request: BlockedApprovalRequest) -> Result<String> {
        let approval_id = self.blocked_approvals.create(request)?;
        let approval = self
            .blocked_approvals
            .get(&approval_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("blocked approval was not retained"))?;
        self.append_blocked_approval_prompt_audit(&approval)?;
        self.append_primary_lifecycle_event(
            EventKind::ApprovalChanged,
            format!(
                r#"{{"approval_id":"{}","state":"pending"}}"#,
                json_escape(&approval_id)
            ),
        )?;
        Ok(approval_id)
    }

    /// Runs the append blocked approval prompt audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn append_blocked_approval_prompt_audit(
        &mut self,
        approval: &BlockedApprovalRequest,
    ) -> Result<()> {
        let Some(audit_log) = self.audit_log.as_mut() else {
            return Ok(());
        };
        let scope = if approval.read_scopes.is_empty() && approval.write_scopes.is_empty() {
            "none".to_string()
        } else {
            format!(
                "read=[{}];write=[{}]",
                approval.read_scopes.join(","),
                approval.write_scopes.join(",")
            )
        };
        let record = AuditRecord::approval_prompt(
            self.session.id.to_string(),
            AuditActor {
                kind: "agent".to_string(),
                id: approval.requesting_agent_id.clone(),
            },
            approval.id.clone(),
            approval.requesting_agent_id.clone(),
            approval.action_kind.clone(),
            scope,
            "prompted",
        );
        let _ = audit_log.append(record)?;
        Ok(())
    }

    /// Runs the session memory operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn session_memory(&self) -> &SessionMemoryStore {
        &self.session_memory
    }

    /// Runs the session memory mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn session_memory_mut(&mut self) -> &mut SessionMemoryStore {
        &mut self.session_memory
    }

    /// Runs the memory records operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn memory_records(&self) -> Vec<MemoryRecord> {
        self.session_memory.export()
    }

    /// Runs the upsert session memory operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn upsert_session_memory(&mut self, record: MemoryRecord) -> Result<()> {
        self.require_live()?;
        self.session_memory.upsert(record)?;
        Ok(())
    }

    /// Runs the delete session memory operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn delete_session_memory(&mut self, id: &str) -> Result<bool> {
        self.require_live()?;
        Ok(self.session_memory.delete(id))
    }

    /// Runs the mcp registry operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn mcp_registry(&self) -> &McpRegistry {
        &self.mcp_registry
    }

    /// Runs the mcp registry mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn mcp_registry_mut(&mut self) -> &mut McpRegistry {
        &mut self.mcp_registry
    }

    /// Clears all runtime-owned MCP transports and returns the number dropped.
    pub(crate) fn clear_runtime_mcp_transports(&mut self) -> usize {
        self.mcp_transports.clear_counted()
    }

    /// Runs the retry runtime mcp server operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn retry_runtime_mcp_server(
        &mut self,
        server_id: &str,
    ) -> Result<RuntimeMcpRetryReport> {
        let previous = self
            .mcp_registry
            .list_servers()
            .into_iter()
            .find(|server| server.configured.id == server_id)
            .cloned()
            .ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "MCP server not found")
            })?;
        if !previous.configured.enabled {
            return Err(MezError::forbidden(
                "MCP server is disabled; enable it before retrying",
            ));
        }
        let retryable_before_retry = matches!(
            previous.status,
            McpServerStatus::Unavailable | McpServerStatus::Blacklisted | McpServerStatus::Failed
        );

        let mut registry = std::mem::take(&mut self.mcp_registry);
        let result = self.retry_runtime_mcp_server_with_registry(
            &mut registry,
            server_id,
            previous.status,
            retryable_before_retry,
        );
        self.mcp_registry = registry;
        result
    }

    /// Runs the retry runtime mcp server async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn retry_runtime_mcp_server_async(
        &mut self,
        server_id: &str,
    ) -> Result<RuntimeMcpRetryReport> {
        let previous = self
            .mcp_registry
            .list_servers()
            .into_iter()
            .find(|server| server.configured.id == server_id)
            .cloned()
            .ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "MCP server not found")
            })?;
        if !previous.configured.enabled {
            return Err(MezError::forbidden(
                "MCP server is disabled; enable it before retrying",
            ));
        }
        let retryable_before_retry = matches!(
            previous.status,
            McpServerStatus::Unavailable | McpServerStatus::Blacklisted | McpServerStatus::Failed
        );

        let mut registry = std::mem::take(&mut self.mcp_registry);
        let result = self
            .retry_runtime_mcp_server_with_registry_async(
                &mut registry,
                server_id,
                previous.status,
                retryable_before_retry,
            )
            .await;
        if result.is_ok() {
            let _ = self.persist_registry_update_plan(&self.registry_update_plan());
        }
        self.mcp_registry = registry;
        result
    }

    /// Runs the retry runtime mcp server with registry operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn retry_runtime_mcp_server_with_registry(
        &mut self,
        registry: &mut McpRegistry,
        server_id: &str,
        previous_status: McpServerStatus,
        retryable_before_retry: bool,
    ) -> Result<RuntimeMcpRetryReport> {
        registry.retry_server(server_id)?;
        self.mcp_transports.remove(server_id);
        let environment = std::env::vars().collect::<BTreeMap<_, _>>();
        let mut rediscovered = true;
        let mut reason = None;
        if let Err(error) = self.discover_runtime_mcp_transport(registry, server_id, &environment) {
            rediscovered = false;
            let message = error.message().to_string();
            let _ = registry.blacklist_for_session(server_id, message.clone());
            self.mcp_transports.remove(server_id);
            reason = Some(message);
        }

        let current = registry
            .list_servers()
            .into_iter()
            .find(|server| server.configured.id == server_id)
            .ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "MCP server not found")
            })?;
        Ok(RuntimeMcpRetryReport {
            server_id: server_id.to_string(),
            previous_status,
            status: current.status,
            retryable_before_retry,
            rediscovered,
            tools: current.tools.len(),
            reason,
        })
    }

    /// Runs the retry runtime mcp server with registry async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn retry_runtime_mcp_server_with_registry_async(
        &mut self,
        registry: &mut McpRegistry,
        server_id: &str,
        previous_status: McpServerStatus,
        retryable_before_retry: bool,
    ) -> Result<RuntimeMcpRetryReport> {
        registry.retry_server(server_id)?;
        self.mcp_transports.remove(server_id);
        let environment = std::env::vars().collect::<BTreeMap<_, _>>();
        let mut rediscovered = true;
        let mut reason = None;
        if let Err(error) = self
            .discover_runtime_mcp_transport_async(registry, server_id, &environment)
            .await
        {
            rediscovered = false;
            let message = error.message().to_string();
            let _ = registry.blacklist_for_session(server_id, message.clone());
            self.mcp_transports.remove(server_id);
            reason = Some(message);
        }

        let current = registry
            .list_servers()
            .into_iter()
            .find(|server| server.configured.id == server_id)
            .ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "MCP server not found")
            })?;
        Ok(RuntimeMcpRetryReport {
            server_id: server_id.to_string(),
            previous_status,
            status: current.status,
            retryable_before_retry,
            rediscovered,
            tools: current.tools.len(),
            reason,
        })
    }

    /// Runs the discover runtime mcp transports async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn discover_runtime_mcp_transports_async(
        &mut self,
        registry: &mut McpRegistry,
        environment: &BTreeMap<String, String>,
    ) -> Result<Vec<String>> {
        let server_ids = registry
            .list_servers()
            .into_iter()
            .filter(|server| {
                server.configured.enabled && server.status == McpServerStatus::Configured
            })
            .map(|server| server.configured.id.clone())
            .collect::<Vec<_>>();
        let mut blacklisted = Vec::new();
        for server_id in server_ids {
            match self
                .discover_runtime_mcp_transport_async(registry, &server_id, environment)
                .await
            {
                Ok(()) => {
                    if let Some(server) = registry
                        .list_servers()
                        .into_iter()
                        .find(|server| server.configured.id == server_id)
                    {
                        self.append_runtime_mcp_discovery_event(
                            &server_id,
                            server.status,
                            server.tools.len(),
                            None,
                        )?;
                    }
                }
                Err(error) => {
                    let reason = error.message().to_string();
                    let _ = registry.blacklist_for_session(&server_id, reason.clone());
                    self.mcp_transports.remove(&server_id);
                    self.append_runtime_mcp_discovery_event(
                        &server_id,
                        McpServerStatus::Blacklisted,
                        0,
                        Some(&reason),
                    )?;
                    blacklisted.push(server_id);
                }
            }
        }
        Ok(blacklisted)
    }

    /// Runs the discover runtime mcp transport operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn discover_runtime_mcp_transport(
        &mut self,
        registry: &mut McpRegistry,
        server_id: &str,
        environment: &BTreeMap<String, String>,
    ) -> Result<()> {
        let _ = (registry, environment);
        Err(MezError::invalid_state(format!(
            "MCP server `{server_id}` requires async runtime discovery"
        )))
    }

    /// Runs the discover runtime mcp transport async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn discover_runtime_mcp_transport_async(
        &mut self,
        registry: &mut McpRegistry,
        server_id: &str,
        environment: &BTreeMap<String, String>,
    ) -> Result<()> {
        let plan = registry.startup_plan(server_id, environment)?;
        match &plan.transport {
            McpStartupTransportPlan::Stdio { .. } => {
                let mut connection = spawn_stdio_mcp_connection(&plan, environment).await?;
                let initialize = connection
                    .initialize("mezzanine", env!("CARGO_PKG_VERSION"), plan.timeout_ms)
                    .await?;
                connection.send_initialized_notification().await?;
                let mut tools = Vec::new();
                if initialize.supports_tools {
                    let mut cursor = None;
                    let mut pagination = crate::mcp::McpToolListPagination::default();
                    loop {
                        let response = connection
                            .list_tools(cursor.as_deref(), plan.timeout_ms)
                            .await?;
                        tools.extend(response.tools);
                        let Some(next_cursor) =
                            pagination.advance(&plan.server_id, response.next_cursor)?
                        else {
                            break;
                        };
                        cursor = Some(next_cursor);
                    }
                }
                registry.mark_available_from_discovered_tools(server_id, tools)?;
                self.mcp_transports
                    .insert_stdio(server_id.to_string(), connection);
            }
            McpStartupTransportPlan::StreamableHttp { .. } => {
                let discovery = discover_streamable_http_mcp_server(
                    &plan,
                    environment,
                    "mezzanine",
                    env!("CARGO_PKG_VERSION"),
                )
                .await?;
                registry
                    .mark_available_from_discovered_tools(server_id, discovery.tools.clone())?;
                self.mcp_transports.insert_streamable_http(
                    server_id.to_string(),
                    RuntimeHttpMcpTransportState {
                        startup_plan: plan,
                        session_id: discovery.session_id,
                        next_request_id: 1000,
                    },
                );
            }
        }
        Ok(())
    }

    /// Appends a lifecycle event for one MCP server discovery result.
    fn append_runtime_mcp_discovery_event(
        &mut self,
        server_id: &str,
        status: McpServerStatus,
        tools: usize,
        reason: Option<&str>,
    ) -> Result<()> {
        let reason_json = reason
            .map(|reason| format!(r#""{}""#, json_escape(reason)))
            .unwrap_or_else(|| "null".to_string());
        self.append_lifecycle_event(
            EventKind::McpServerChanged,
            format!(
                r#"{{"server_id":"{}","status":"{}","tools":{},"reason":{},"source":"runtime-mcp-discovery"}}"#,
                json_escape(server_id),
                runtime_mcp_service_status_name(status),
                tools,
                reason_json
            ),
        )
    }

    /// Runs the provider registry operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn provider_registry(&self) -> &RuntimeProviderRegistry {
        &self.provider_registry
    }

    /// Runs the auth store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn auth_store(&self) -> Option<&AuthStore> {
        self.auth_store.as_ref()
    }

    /// Runs the set auth store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_auth_store(&mut self, store: AuthStore) {
        self.auth_store = Some(store);
    }

    /// Runs the set audit log operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_audit_log(&mut self, mut audit_log: AuditLog) {
        if let Some(existing) = self.audit_log.as_mut() {
            let pending = existing.drain_deferred_writes();
            self.deferred_audit_writes.extend(pending);
        }
        audit_log.set_defer_writes(self.defer_audit_writes);
        self.audit_log = Some(audit_log);
    }

    /// Clears the active audit writer while preserving deferred writes.
    fn clear_audit_log(&mut self) {
        if let Some(existing) = self.audit_log.as_mut() {
            let pending = existing.drain_deferred_writes();
            self.deferred_audit_writes.extend(pending);
        }
        self.audit_log = None;
    }

    /// Runs the audit log operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn audit_log(&self) -> Option<&AuditLog> {
        self.audit_log.as_ref()
    }

    /// Enables or disables deferred audit persistence for async actor owners.
    pub(crate) fn set_defer_audit_writes(&mut self, defer: bool) {
        self.defer_audit_writes = defer;
        if let Some(audit_log) = self.audit_log.as_mut() {
            audit_log.set_defer_writes(defer);
        }
    }

    /// Drains audit JSONL payloads queued for async persistence.
    pub(crate) fn drain_deferred_audit_writes(&mut self) -> Vec<AuditDeferredWrite> {
        if let Some(audit_log) = self.audit_log.as_mut() {
            let pending = audit_log.drain_deferred_writes();
            self.deferred_audit_writes.extend(pending);
        }
        std::mem::take(&mut self.deferred_audit_writes)
    }

    /// Runs the agent shell store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn agent_shell_store(&self) -> &AgentShellStore {
        &self.agent_shell_store
    }

    /// Runs the agent shell store mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn agent_shell_store_mut(&mut self) -> &mut AgentShellStore {
        &mut self.agent_shell_store
    }

    /// Runs the agent transcript store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn agent_transcript_store(&self) -> Option<&AgentTranscriptStore> {
        self.agent_transcript_store.as_ref()
    }

    /// Runs the set agent transcript store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_agent_transcript_store(&mut self, store: AgentTranscriptStore) {
        self.agent_transcript_store = Some(store);
    }

    /// Restores pane-scoped active agent shell metadata for this live session.
    ///
    /// Transcript files hold the durable conversation content, but the runtime
    /// also needs a small pane binding record to recover which pane owned which
    /// conversation after a daemon restart. Records are scoped by Mezzanine
    /// session id so a fresh daemon or different pane cannot inherit another
    /// session's context automatically.
    pub fn restore_agent_sessions_from_transcript_store(&mut self) -> Result<usize> {
        let Some(store) = self.agent_transcript_store.clone() else {
            return Ok(0);
        };
        let session_id = self.session.id.as_str().to_string();
        let records = store.load_agent_session_metadata(&session_id)?;
        let restored_at = current_unix_seconds();
        let mut restored = 0usize;
        let mut interrupted = 0usize;
        for metadata in records {
            if runtime_pane_by_id(&self.session, &metadata.pane_id).is_err() {
                continue;
            }
            let visibility = runtime_agent_session_metadata_visibility(&metadata.visibility)?;
            let log_level = AgentLogLevel::parse(&metadata.log_level).ok_or_else(|| {
                MezError::invalid_args("agent session metadata log level is invalid")
            })?;
            let running_turn_id = metadata.running_turn_id.clone();
            let session = self
                .agent_shell_store
                .ensure_session(metadata.pane_id.clone())?;
            session.session_id = metadata.conversation_id.clone();
            session.prompt_cache_lineage_id = metadata.prompt_cache_lineage_id.clone();
            session.visibility = visibility;
            session.running_turn_id = None;
            session.transcript_entries = metadata.transcript_entries;
            session.log_level = log_level;
            if let Some(profile) = metadata.pane_model_profile.as_ref() {
                self.model_profile_overrides
                    .pane_profiles
                    .insert(metadata.pane_id.clone(), profile.clone());
            } else {
                self.model_profile_overrides
                    .pane_profiles
                    .remove(&metadata.pane_id);
            }
            if metadata.planning_enabled {
                self.agent_planning_modes.insert(metadata.pane_id.clone());
            } else {
                self.agent_planning_modes.remove(&metadata.pane_id);
            }
            if let Some(style) = metadata.response_style.as_ref() {
                self.agent_response_styles
                    .insert(metadata.pane_id.clone(), style.clone());
            } else {
                self.agent_response_styles.remove(&metadata.pane_id);
            }
            if let Some(enabled) = metadata.routing_enabled {
                self.agent_routing_overrides
                    .insert(metadata.pane_id.clone(), enabled);
            } else {
                self.agent_routing_overrides.remove(&metadata.pane_id);
            }
            self.restore_agent_approval_policy_from_metadata(
                metadata.approval_policy.as_deref(),
                "agent-session-restore",
            )?;
            if let Some(working_directory) = metadata.working_directory.as_ref() {
                self.pane_current_working_directories
                    .insert(metadata.pane_id.clone(), PathBuf::from(working_directory));
            }
            let token_usage_by_model = runtime_agent_token_usage_by_model_from_metadata(&metadata);
            if token_usage_by_model.is_empty() {
                self.agent_token_usage_by_conversation
                    .remove(&metadata.conversation_id);
            } else {
                self.agent_token_usage_by_conversation
                    .insert(metadata.conversation_id.clone(), token_usage_by_model);
            }
            self.record_pane_transcript_ref(
                &metadata.pane_id,
                format!(
                    "transcript:{}:{}",
                    metadata.pane_id, metadata.conversation_id
                ),
            )?;
            self.reload_agent_prompt_history_for_pane(&metadata.pane_id)?;
            if let Some(turn_id) = running_turn_id {
                self.agent_turn_ledger.start_turn(AgentTurnRecord {
                    turn_id: turn_id.clone(),
                    agent_id: format!("agent-{}", metadata.pane_id),
                    pane_id: metadata.pane_id.clone(),
                    trigger: AgentTurnTrigger::ScheduledTask,
                    started_at_unix_seconds: restored_at,
                    policy_profile: "agent-session-restore".to_string(),
                    model_profile: "default".to_string(),
                    parent_turn_id: None,
                    state: AgentTurnState::Queued,
                    cooperation_mode: None,
                })?;
                self.agent_turn_ledger
                    .finish_turn(&turn_id, AgentTurnState::Interrupted)?;
                interrupted = interrupted.saturating_add(1);
            }
            restored = restored.saturating_add(1);
        }
        if restored > 0 || interrupted > 0 {
            self.append_lifecycle_event(
                EventKind::AgentStatus,
                format!(
                    r#"{{"source":"agent-session-restore","restored_agent_sessions":{},"interrupted_agent_turns":{},"retry_requires_confirmation":{}}}"#,
                    restored,
                    interrupted,
                    interrupted > 0
                ),
            )?;
        }
        if restored > 0 {
            self.checkpoint_agent_session_metadata()?;
        }
        Ok(restored)
    }

    /// Persists the active pane-to-agent-session bindings for crash recovery.
    ///
    /// The checkpoint is intentionally metadata-only. Conversation content
    /// remains in per-conversation transcript files, while this file records
    /// which pane should point at which conversation when the same Mezzanine
    /// session is restored.
    pub(super) fn checkpoint_agent_session_metadata(&mut self) -> Result<usize> {
        let Some(store) = self.agent_transcript_store.clone() else {
            return Ok(0);
        };
        let mezzanine_session_id = self.session.id.as_str().to_string();
        let records = self
            .agent_shell_store
            .sessions()
            .filter(|session| runtime_pane_by_id(&self.session, &session.pane_id).is_ok())
            .map(|session| {
                let working_directory = self
                    .pane_current_working_directory(&session.pane_id)
                    .map(|path| path.to_string_lossy().into_owned());
                let project_root = working_directory
                    .as_deref()
                    .map(PathBuf::from)
                    .map(|path| discover_project_root(&path).to_string_lossy().into_owned());
                let token_usage_by_model = self
                    .agent_token_usage_by_conversation
                    .get(&session.session_id)
                    .cloned()
                    .unwrap_or_default();
                AgentSessionMetadata {
                    mezzanine_session_id: mezzanine_session_id.clone(),
                    pane_id: session.pane_id.clone(),
                    conversation_id: session.session_id.clone(),
                    prompt_cache_lineage_id: session.prompt_cache_lineage_id.clone(),
                    visibility: agent_shell_visibility_json_name(session.visibility).to_string(),
                    running_turn_id: session.running_turn_id.clone(),
                    transcript_entries: session.transcript_entries,
                    log_level: session.log_level.as_str().to_string(),
                    pane_model_profile: self
                        .model_profile_overrides
                        .pane_profiles
                        .get(&session.pane_id)
                        .cloned(),
                    planning_enabled: self.agent_planning_modes.contains(&session.pane_id),
                    response_style: self.agent_response_styles.get(&session.pane_id).cloned(),
                    routing_enabled: self.agent_routing_overrides.get(&session.pane_id).copied(),
                    approval_policy: self
                        .live_approval_policy_override
                        .map(runtime_approval_policy_name)
                        .map(ToOwned::to_owned),
                    working_directory,
                    project_root,
                    token_usage: runtime_agent_total_token_usage_by_model(&token_usage_by_model),
                    token_usage_by_model,
                    context_usage: self
                        .agent_context_usage_by_conversation
                        .get(&session.session_id)
                        .cloned(),
                    context_usage_snapshot: self
                        .agent_context_usage_snapshot_by_conversation
                        .get(&session.session_id)
                        .copied(),
                }
            })
            .collect::<Vec<_>>();
        store.save_agent_session_metadata(&mezzanine_session_id, &records)
    }

    /// Restores persisted pane-local agent settings for one rebound conversation.
    ///
    /// `/resume` can bind a saved conversation without going through daemon
    /// startup recovery. This helper reloads the matching metadata row so
    /// explicit session choices such as routing, approval policy, and
    /// provider token accounting continue from saved state instead of falling
    /// back to current defaults.
    pub(super) fn restore_agent_resume_state_for_conversation(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
    ) -> Result<()> {
        let Some(store) = self.agent_transcript_store.clone() else {
            return Ok(());
        };
        let mezzanine_session_id = self.session.id.as_str().to_string();
        for metadata in store.load_agent_session_metadata(&mezzanine_session_id)? {
            if metadata.conversation_id != conversation_id {
                continue;
            }
            let session = self.agent_shell_store.ensure_session(pane_id.to_string())?;
            session.prompt_cache_lineage_id = metadata.prompt_cache_lineage_id.clone();
            if let Some(profile) = metadata.pane_model_profile.as_ref() {
                self.model_profile_overrides
                    .pane_profiles
                    .insert(pane_id.to_string(), profile.clone());
            } else {
                self.model_profile_overrides.pane_profiles.remove(pane_id);
            }
            if metadata.planning_enabled {
                self.agent_planning_modes.insert(pane_id.to_string());
            } else {
                self.agent_planning_modes.remove(pane_id);
            }
            if let Some(style) = metadata.response_style.as_ref() {
                self.agent_response_styles
                    .insert(pane_id.to_string(), style.clone());
            } else {
                self.agent_response_styles.remove(pane_id);
            }
            if let Some(enabled) = metadata.routing_enabled {
                self.agent_routing_overrides
                    .insert(pane_id.to_string(), enabled);
            } else {
                self.agent_routing_overrides.remove(pane_id);
            }
            self.restore_agent_approval_policy_from_metadata(
                metadata.approval_policy.as_deref(),
                "agent-session-resume",
            )?;
            let token_usage_by_model = runtime_agent_token_usage_by_model_from_metadata(&metadata);
            if token_usage_by_model.is_empty() {
                self.agent_token_usage_by_conversation
                    .remove(conversation_id);
            } else {
                self.agent_token_usage_by_conversation
                    .insert(conversation_id.to_string(), token_usage_by_model);
            }
            if let Some(context_usage) = metadata.context_usage {
                self.agent_context_usage_by_conversation
                    .insert(conversation_id.to_string(), context_usage);
            } else {
                self.agent_context_usage_by_conversation
                    .remove(conversation_id);
            }
            if let Some(snapshot) = metadata.context_usage_snapshot {
                self.agent_context_usage_snapshot_by_conversation
                    .insert(conversation_id.to_string(), snapshot);
                if let Some(display) =
                    crate::runtime::agent::runtime_agent_provider_context_usage_display(snapshot)
                {
                    self.agent_context_usage_by_conversation
                        .insert(conversation_id.to_string(), display);
                }
            } else {
                self.agent_context_usage_snapshot_by_conversation
                    .remove(conversation_id);
            }
            let _ = self.checkpoint_agent_session_metadata();
            break;
        }
        Ok(())
    }

    /// Applies a saved approval-policy value directly from session metadata.
    ///
    /// New checkpoints only persist this field for explicit live approval
    /// choices. Older checkpoints stored the effective policy, so restore must
    /// avoid letting legacy inherited values narrow a stronger configured
    /// default.
    fn restore_agent_approval_policy_from_metadata(
        &mut self,
        approval_policy: Option<&str>,
        source: &str,
    ) -> Result<()> {
        let Some(approval_policy) =
            approval_policy.filter(|approval_policy| !approval_policy.trim().is_empty())
        else {
            return Ok(());
        };
        let requested = runtime_parse_approval_policy(approval_policy).map_err(|_| {
            MezError::invalid_args("agent session metadata approval policy is invalid")
        })?;
        if matches!(
            compare_approval_policy_authority(self.permission_policy.approval_policy, requested),
            PermissionAuthorityChange::Narrowing
        ) {
            return Ok(());
        }
        let previous_permission_policy = self.permission_policy.clone();
        self.set_live_approval_policy_override(requested);
        self.reconcile_pending_agent_approvals_after_permission_change(
            None,
            &previous_permission_policy,
            source,
        )?;
        Ok(())
    }

    /// Enables or disables deferred agent transcript writes for async actors.
    pub(crate) fn set_defer_agent_transcript_writes(&mut self, defer: bool) {
        self.defer_agent_transcript_writes = defer;
    }

    /// Drains agent transcript writes queued for the async persistence worker.
    pub(crate) fn drain_deferred_agent_transcript_writes(
        &mut self,
    ) -> Vec<DeferredAgentTranscriptWrite> {
        std::mem::take(&mut self.deferred_agent_transcript_writes)
    }

    /// Drains shared prompt-history writes queued for async persistence.
    pub(crate) fn drain_deferred_agent_prompt_history_writes(
        &mut self,
    ) -> Vec<DeferredAgentPromptHistoryWrite> {
        std::mem::take(&mut self.deferred_agent_prompt_history_writes)
    }

    /// Drains command prompt history writes queued for async persistence.
    pub(crate) fn drain_deferred_command_prompt_history_writes(
        &mut self,
    ) -> Vec<DeferredCommandPromptHistoryWrite> {
        std::mem::take(&mut self.deferred_command_prompt_history_writes)
    }

    /// Enables or disables deferred user/project config writes for async actors.
    pub(crate) fn set_defer_config_file_writes(&mut self, defer: bool) {
        self.defer_config_file_writes = defer;
    }

    /// Drains user/project config writes queued for async persistence.
    pub(crate) fn drain_deferred_config_file_writes(&mut self) -> Vec<DeferredConfigFileWrite> {
        std::mem::take(&mut self.deferred_config_file_writes)
    }

    /// Enables or disables deferred project config writes for async actors.
    pub(crate) fn set_defer_project_config_writes(&mut self, defer: bool) {
        self.defer_project_config_writes = defer;
    }

    /// Drains project config writes queued for async persistence.
    pub(crate) fn drain_deferred_project_config_writes(
        &mut self,
    ) -> Vec<DeferredProjectConfigWrite> {
        std::mem::take(&mut self.deferred_project_config_writes)
    }

    /// Enables or disables deferred project instruction writes for async actors.
    pub(crate) fn set_defer_project_instruction_writes(&mut self, defer: bool) {
        self.defer_project_instruction_writes = defer;
    }

    /// Drains project instruction scaffold writes queued for async persistence.
    pub(crate) fn drain_deferred_project_instruction_writes(
        &mut self,
    ) -> Vec<DeferredProjectInstructionWrite> {
        std::mem::take(&mut self.deferred_project_instruction_writes)
    }

    /// Runs the project trust store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn project_trust_store(&self) -> Option<&ProjectTrustStore> {
        self.project_trust_store.as_ref()
    }

    /// Runs the set project trust store operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn set_project_trust_store(
        &mut self,
        store: ProjectTrustStore,
        database_path: Option<PathBuf>,
    ) {
        self.project_trust_store = Some(store);
        self.project_trust_database_path = database_path;
    }

    /// Runs the agent scheduler operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn agent_scheduler(&self) -> &AgentScheduler {
        &self.agent_scheduler
    }

    /// Runs the agent scheduler mut operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn agent_scheduler_mut(&mut self) -> &mut AgentScheduler {
        &mut self.agent_scheduler
    }
}

/// Returns the normalized MCP status name used in runtime discovery events.
fn runtime_mcp_service_status_name(status: McpServerStatus) -> &'static str {
    match status {
        McpServerStatus::Configured => "configured",
        McpServerStatus::Starting => "starting",
        McpServerStatus::Available => "available",
        McpServerStatus::Unavailable => "unavailable",
        McpServerStatus::Blacklisted => "blacklisted",
        McpServerStatus::Failed => "failed",
    }
}

/// Parses persisted agent shell visibility metadata.
fn runtime_agent_session_metadata_visibility(value: &str) -> Result<AgentShellVisibility> {
    match value {
        "hidden" => Ok(AgentShellVisibility::Hidden),
        "visible" => Ok(AgentShellVisibility::Visible),
        "hide-pending-task-completion" => Ok(AgentShellVisibility::HidePendingTaskCompletion),
        _ => Err(MezError::invalid_args(
            "agent session metadata visibility is invalid",
        )),
    }
}
