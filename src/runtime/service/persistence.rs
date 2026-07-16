//! Session access, effect adapters, timers, registry persistence, and metrics.

use super::{
    RenderInvalidationReason, Result, RuntimeRegistryUpdatePlan, RuntimeSessionService,
    RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind, RuntimeTransition, Session,
    SessionRegistry, TerminalClientLoopConfig, apply_registry_update,
    runtime_status_refresh_interval_ms_for_config, runtime_status_refresh_required_by_config,
};

impl RuntimeSessionService {
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
        self.persistence.set_session_registry(registry);
    }

    /// Assigns audit persistence to the external effect adapter.
    ///
    /// Actor owners call this explicitly so writers installed by later config
    /// reloads retain adapter ownership without consulting the global mode.
    pub(crate) fn use_audit_effect_adapter(&mut self) {
        self.persistence.enable_audit_adapter();
        if let Some(audit_log) = self.persistence.audit_log_mut() {
            audit_log.set_defer_writes(true);
        }
    }

    /// Assigns pane-pipe process and persistence work to external adapters.
    pub(crate) fn use_pane_pipe_effect_adapter(&mut self) {
        self.persistence.enable_pane_pipe_adapter();
    }

    /// Assigns agent transcript persistence to the external effect adapter.
    pub(crate) fn use_transcript_effect_adapter(&mut self) {
        self.persistence.enable_transcript_adapter();
    }

    /// Assigns session-registry persistence to the external effect adapter.
    pub(crate) fn use_registry_effect_adapter(&mut self) {
        self.persistence.enable_registry_adapter();
    }

    /// Assigns configuration persistence to the external effect adapter.
    pub(crate) fn use_config_effect_adapter(&mut self) {
        self.persistence.enable_config_adapter();
    }

    /// Assigns non-blocking program-hook execution to the external effect adapter.
    pub(crate) fn use_hook_effect_adapter(&mut self) {
        self.persistence.enable_hook_adapter();
    }

    /// Applies a resize-debounce timer after the adapter validates its key.
    ///
    /// Timer-key tracking remains adapter state, while the runtime core owns
    /// the resulting render transition for every attached terminal client.
    pub(crate) fn apply_resize_debounce_timer_transition(&self, active: bool) -> RuntimeTransition {
        if !active {
            return RuntimeTransition::default();
        }
        let side_effects = self
            .session
            .clients()
            .iter()
            .filter(|client| client.state == crate::runtime::ClientState::Attached)
            .map(|client| RuntimeSideEffect::RenderClient {
                client_id: client.id.clone(),
                reason: RenderInvalidationReason::Resize,
            })
            .collect::<Vec<_>>();
        RuntimeTransition {
            applied: !side_effects.is_empty(),
            side_effects,
        }
    }

    /// Reconciles the cursor-blink timer for one attached terminal client.
    ///
    /// The runtime core owns eligibility and timer generation while the caller
    /// supplies the adapter's currently scheduled key. This keeps Tokio timer
    /// tracking out of domain policy without creating a second timer owner.
    pub(crate) fn client_cursor_blink_timer_transition(
        &self,
        client_id: &str,
        active_key: Option<RuntimeTimerKey>,
        generation_base_ms: u64,
    ) -> Result<RuntimeTransition> {
        let config = self.terminal_client_loop_config(TerminalClientLoopConfig::default())?;
        let client_attached = self.session.clients().iter().any(|client| {
            client.id.as_str() == client_id && client.state == crate::runtime::ClientState::Attached
        });
        if !client_attached || !config.cursor_blink || config.cursor_blink_interval_ms == 0 {
            return Ok(RuntimeTransition {
                applied: false,
                side_effects: active_key
                    .map(|key| RuntimeSideEffect::CancelTimer { key })
                    .into_iter()
                    .collect(),
            });
        }
        if active_key.is_some() {
            return Ok(RuntimeTransition::default());
        }
        let delay_ms = (config.cursor_blink_interval_ms / 2).max(1);
        Ok(RuntimeTransition {
            applied: false,
            side_effects: vec![RuntimeSideEffect::ScheduleTimer {
                key: RuntimeTimerKey::new(
                    RuntimeTimerKind::CursorBlink,
                    client_id,
                    generation_base_ms.saturating_add(delay_ms),
                ),
                delay_ms,
            }],
        })
    }

    /// Reconciles the status-refresh timer for one attached terminal client.
    ///
    /// This transition retains the actor's timer key as adapter state while
    /// centralizing the client/configuration policy in the runtime core.
    pub(crate) fn client_status_refresh_timer_transition(
        &self,
        client_id: &str,
        active_key: Option<RuntimeTimerKey>,
        generation_base_ms: u64,
    ) -> Result<RuntimeTransition> {
        let config = self.terminal_client_loop_config(TerminalClientLoopConfig::default())?;
        let client_attached = self.session.clients().iter().any(|client| {
            client.id.as_str() == client_id && client.state == crate::runtime::ClientState::Attached
        });
        if !client_attached || !runtime_status_refresh_required_by_config(&config) {
            return Ok(RuntimeTransition {
                applied: false,
                side_effects: active_key
                    .map(|key| RuntimeSideEffect::CancelTimer { key })
                    .into_iter()
                    .collect(),
            });
        }
        let delay_ms = runtime_status_refresh_interval_ms_for_config(&config);
        let next_key = RuntimeTimerKey::new(
            RuntimeTimerKind::StatusRefresh,
            client_id,
            generation_base_ms.saturating_add(delay_ms),
        );
        let side_effects = match active_key {
            Some(existing_key) if existing_key.generation <= next_key.generation => Vec::new(),
            Some(existing_key) => vec![
                RuntimeSideEffect::CancelTimer { key: existing_key },
                RuntimeSideEffect::ScheduleTimer {
                    key: next_key,
                    delay_ms,
                },
            ],
            None => vec![RuntimeSideEffect::ScheduleTimer {
                key: next_key,
                delay_ms,
            }],
        };
        Ok(RuntimeTransition {
            applied: false,
            side_effects,
        })
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
        if self.persistence.session_registry().is_none() {
            return Ok(false);
        }
        if self.persistence.registry_uses_adapter() {
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
        let registry = self.persistence.cloned_session_registry()?;
        Some((registry, self.registry_update_plan()))
    }

    /// Emits current registry persistence directly through the transition contract.
    pub(crate) fn registry_persistence_transition(&self) -> crate::runtime::RuntimeTransition {
        let Some((registry, update)) = self.registry_update_for_async_persistence() else {
            return crate::runtime::RuntimeTransition::default();
        };
        crate::runtime::RuntimeTransition {
            applied: false,
            side_effects: vec![crate::runtime::RuntimeSideEffect::PersistRegistry {
                registry,
                update,
            }],
        }
    }

    /// Persists a precomputed registry update plan when a registry is attached.
    pub(in crate::runtime) fn persist_registry_update_plan(
        &self,
        update: &RuntimeRegistryUpdatePlan,
    ) -> Result<bool> {
        let Some(registry) = self.persistence.session_registry() else {
            return Ok(false);
        };
        apply_registry_update(registry, update)
    }
    /// Replaces the cached async runtime metrics snapshot used by display commands.
    pub(crate) fn set_async_runtime_metrics(
        &mut self,
        metrics: crate::async_runtime::AsyncRuntimeActorMetrics,
    ) {
        self.integration.set_async_runtime_metrics(Some(metrics));
    }
    /// Returns the cached async runtime metrics snapshot when the actor provided one.
    pub(in crate::runtime) fn async_runtime_metrics(
        &self,
    ) -> Option<&crate::async_runtime::AsyncRuntimeActorMetrics> {
        self.integration.async_runtime_metrics()
    }
    /// Returns runtime-owned agent, provider, prompt-cache, and shell metrics.
    pub(in crate::runtime) fn runtime_metrics(
        &self,
    ) -> &crate::runtime::service_state::RuntimeMetricsSnapshot {
        self.integration.runtime_metrics()
    }
}
