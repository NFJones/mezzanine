//! Runtime session-service construction and initial aggregate assembly.

use super::*;
use crate::runtime::{
    RuntimeAgentComponent, RuntimeAutoSizingConfig, RuntimePresentationComponent,
    RuntimeProcessComponent,
};
#[cfg(test)]
use crate::terminal::HostClipboard;

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
        self.set_host_clipboard(host_clipboard);
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
            process: RuntimeProcessComponent::with_pane_processes(pane_processes),
            agent: RuntimeAgentComponent::with_settings(
                DEFAULT_AGENT_ROUTING,
                RuntimeAutoSizingConfig::default(),
                DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT,
                DEFAULT_AGENT_LOOP_LIMIT,
                DEFAULT_AGENT_ACTION_FAILURE_RETRY_LIMIT,
                DEFAULT_AGENT_IMPLEMENTATION_PRESSURE_AFTER_SHELL_ACTIONS,
            ),
            session,
            window_created_at_unix_seconds,
            config_layers: Vec::new(),
            config_root: None,
            snapshot_repository: None,
            control_idempotency,
            message_service,
            async_runtime_metrics: None,
            runtime_metrics: Default::default(),
            queued_pane_input_effects: Vec::new(),
            queued_pane_resize_effects: BTreeMap::new(),
            queued_pane_termination_effects: BTreeMap::new(),
            queued_pane_pipe_effects: Vec::new(),
            queued_audit_effects: Vec::new(),
            queued_transcript_effects: Vec::new(),
            queued_config_effects: Vec::new(),
            deferred_transcript_next_sequences: BTreeMap::new(),
            audit_effects_use_adapter: false,
            pane_pipe_effects_use_adapter: false,
            transcript_effects_use_adapter: false,
            registry_effects_use_adapter: false,
            config_effects_use_adapter: false,
            hook_effects_use_adapter: false,
            pane_transcript_refs: BTreeMap::new(),
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
            agent_transcript_store: None,
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
