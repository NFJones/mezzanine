//! Runtime session-service construction and initial aggregate assembly.

use super::{
    BTreeMap, ControlIdempotencyCache, DEFAULT_AGENT_ACTION_FAILURE_RETRY_LIMIT,
    DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT, DEFAULT_AGENT_LOOP_LIMIT,
    DEFAULT_AGENT_ROUTING, EventLog, MessageService, MezError, PaneProcessManager, PathBuf, Result,
    RuntimeLifecycleState, RuntimeSessionService, Session, Value, builtin_subagent_profiles,
    ensure_absolute, ensure_no_mez_separator, runtime_provider_registry_from_config,
};
#[cfg(test)]
use crate::host::terminal::HostClipboard;
use crate::runtime::{
    RuntimeAgentComponent, RuntimeAutoSizingConfig, RuntimeControlComponent,
    RuntimeExternalEditorComponent, RuntimeIntegrationComponent, RuntimePersistenceComponent,
    RuntimePresentationComponent, RuntimeProcessComponent, RuntimeSessionComponent,
};

/// Optional owners and process-global policy applied during aggregate construction.
struct RuntimeSessionConstructionOptions {
    event_log: Option<EventLog>,
    apply_global_terminal_policy: bool,
}

impl RuntimeSessionService {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
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

    /// Builds an isolated service used only for blocking presentation replay.
    ///
    /// The projection service owns no live processes or external-effect
    /// adapters. It receives immutable layout and rendering inputs captured by
    /// the serialized runtime, then reuses the canonical presentation methods
    /// without touching actor-owned state.
    pub(crate) fn for_agent_presentation_projection(
        session: Session,
        socket_path: PathBuf,
        created_at_unix_seconds: u64,
        presentation_settings: crate::runtime::RuntimePresentationSettings,
        history_limit: usize,
        history_rotate_lines: usize,
    ) -> Result<Self> {
        let mut service = Self::from_parts_with_processes_policy(
            session,
            socket_path,
            created_at_unix_seconds,
            ControlIdempotencyCache::default(),
            MessageService::default(),
            PaneProcessManager::new(),
            RuntimeSessionConstructionOptions {
                event_log: None,
                apply_global_terminal_policy: false,
            },
        )?;
        service
            .presentation
            .apply_projection_settings(presentation_settings);
        service.configure_agent_projection_history_policy(history_limit, history_rotate_lines)?;
        Ok(service)
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
        Self::from_parts_with_processes_policy(
            session,
            socket_path,
            created_at_unix_seconds,
            control_idempotency,
            message_service,
            pane_processes,
            RuntimeSessionConstructionOptions {
                event_log,
                apply_global_terminal_policy: true,
            },
        )
    }

    /// Assembles live or isolated runtime state with explicit global-policy ownership.
    fn from_parts_with_processes_policy(
        session: Session,
        socket_path: PathBuf,
        created_at_unix_seconds: u64,
        control_idempotency: ControlIdempotencyCache,
        message_service: MessageService,
        pane_processes: PaneProcessManager,
        options: RuntimeSessionConstructionOptions,
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
        if options.apply_global_terminal_policy {
            mez_terminal::set_terminal_emoji_width(terminal_emoji_width);
        }
        let external_editor = if options.apply_global_terminal_policy {
            let runtime_root = socket_path
                .parent()
                .ok_or_else(|| MezError::invalid_state("runtime socket has no parent directory"))?;
            RuntimeExternalEditorComponent::discover(runtime_root, session.id.as_str())?
        } else {
            RuntimeExternalEditorComponent::default()
        };
        Ok(Self {
            presentation: RuntimePresentationComponent::default(),
            process: RuntimeProcessComponent::with_pane_processes(pane_processes),
            agent: RuntimeAgentComponent::with_settings(
                DEFAULT_AGENT_ROUTING,
                RuntimeAutoSizingConfig::default(),
                DEFAULT_AGENT_COMPACTION_RAW_RETENTION_PERCENT,
                DEFAULT_AGENT_LOOP_LIMIT,
                DEFAULT_AGENT_ACTION_FAILURE_RETRY_LIMIT,
                mez_agent::DEFAULT_AGENT_TURN_TIMEOUT_MS,
            ),
            persistence: RuntimePersistenceComponent::default(),
            control: RuntimeControlComponent::new(
                control_idempotency,
                message_service,
                options.event_log,
            ),
            integration: RuntimeIntegrationComponent::new(
                runtime_provider_registry_from_config(&Value::Object(serde_json::Map::new()))?,
                builtin_subagent_profiles(),
            ),
            external_editor,
            session: RuntimeSessionComponent::new(
                session,
                window_created_at_unix_seconds,
                lifecycle_state,
                socket_path,
                created_at_unix_seconds,
            ),
        })
    }
}
