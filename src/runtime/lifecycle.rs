//! Runtime Lifecycle implementation.
//!
//! This module owns the runtime lifecycle boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    EventKind, EventLog, HookEvent, Path, Result, RuntimeLifecycleState, RuntimeSessionService,
    Size, json_escape,
};
use crate::runtime::{
    ClientEvent, RenderInvalidationReason, RuntimeSideEffect, RuntimeTransition, ShutdownEvent,
};
use mez_mux::session::ClientTerminalDescriptor;

// Session lifecycle, primary attachment, and kill handling.

impl RuntimeSessionService {
    /// Runs the event log operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn event_log(&self) -> Option<&EventLog> {
        self.event_log.as_ref()
    }

    /// Runs the lifecycle state operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn lifecycle_state(&self) -> RuntimeLifecycleState {
        self.lifecycle_state
    }

    /// Runs the socket path operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Runs the last attach at unix seconds operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn last_attach_at_unix_seconds(&self) -> Option<u64> {
        self.last_attach_at_unix_seconds
    }

    /// Runs the attach primary operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn attach_primary(
        &mut self,
        name: impl Into<String>,
        interactive: bool,
        terminal_size: Size,
        now_unix_seconds: u64,
    ) -> Result<mez_core::ids::ClientId> {
        self.require_attachable()?;
        let terminal = ClientTerminalDescriptor {
            columns: terminal_size.columns,
            rows: terminal_size.rows,
            term: self.terminal_term.clone(),
            features: Vec::new(),
        };
        let client_id =
            self.session
                .attach_primary_with_terminal(name, interactive, Some(terminal))?;
        self.session.state = mez_mux::session::SessionState::Running;
        self.lifecycle_state = RuntimeLifecycleState::Running;
        self.session
            .resize_authoritative_terminal(&client_id, terminal_size)?;
        self.sync_tracked_pty_sizes()?;
        self.resume_detached_config_change_actions()?;
        self.last_attach_at_unix_seconds = Some(now_unix_seconds);
        self.append_lifecycle_event(
            EventKind::ClientAttached,
            format!(
                r#"{{"client_id":"{}","role":"primary","columns":{},"rows":{}}}"#,
                json_escape(client_id.as_str()),
                terminal_size.columns,
                terminal_size.rows
            ),
        )?;
        self.persist_or_defer_registry_update()?;
        Ok(client_id)
    }

    /// Runs the detach primary operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn detach_primary(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        terminal_size: Size,
    ) -> Result<()> {
        self.require_live()?;
        self.session.authoritative_size = terminal_size;
        self.session.detach_primary(primary_client_id)?;
        self.lifecycle_state = RuntimeLifecycleState::Detached;
        self.run_configured_completed_hooks(
            HookEvent::SessionDetach,
            &format!(
                r#"{{"client_id":"{}","role":"primary","columns":{},"rows":{}}}"#,
                json_escape(primary_client_id.as_str()),
                terminal_size.columns,
                terminal_size.rows
            ),
        )?;
        self.append_lifecycle_event(
            EventKind::ClientDetached,
            format!(
                r#"{{"client_id":"{}","role":"primary","columns":{},"rows":{}}}"#,
                json_escape(primary_client_id.as_str()),
                terminal_size.columns,
                terminal_size.rows
            ),
        )?;
        self.persist_or_defer_registry_update()?;
        Ok(())
    }

    /// Applies a primary-client terminal resize delivered through async runtime
    /// event ingress.
    ///
    /// Events for stale or non-primary clients are ignored because they may
    /// arrive after a detach, observer disconnect, or primary handoff. The
    /// live primary path reuses the established terminal-resize behavior so
    /// pane geometry, tracked PTY sizes, registry state, and lifecycle events
    /// remain identical to the compatibility request path.
    pub fn apply_primary_client_resize_event(
        &mut self,
        client_id: &mez_core::ids::ClientId,
        size: Size,
    ) -> Result<bool> {
        if self.session.primary_client_id() != Some(client_id) {
            return Ok(false);
        }
        self.resize_attached_primary_terminal(client_id, size)?;
        Ok(true)
    }

    /// Applies one state-changing client event through the transport-neutral
    /// runtime transition contract.
    pub(crate) fn apply_client_lifecycle_transition(
        &mut self,
        event: ClientEvent,
    ) -> Result<RuntimeTransition> {
        let (applied, reason) = match event {
            ClientEvent::Resize { client_id, size } => (
                self.apply_primary_client_resize_event(&client_id, size)?,
                RenderInvalidationReason::Layout,
            ),
            ClientEvent::Disconnected { client_id, reason } => (
                self.apply_primary_client_disconnect_event(&client_id, reason)?,
                RenderInvalidationReason::FullRedraw,
            ),
            ClientEvent::Input { .. }
            | ClientEvent::ResizeSignal { .. }
            | ClientEvent::OutputReady { .. } => {
                return Err(crate::error::MezError::invalid_state(
                    "client I/O and render signals require an async adapter transition",
                ));
            }
        };
        let side_effects = if applied {
            self.session
                .clients()
                .iter()
                .filter(|client| client.state == mez_mux::session::ClientState::Attached)
                .map(|client| RuntimeSideEffect::RenderClient {
                    client_id: client.id.clone(),
                    reason,
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut side_effects = side_effects;
        if applied {
            side_effects.extend(self.registry_persistence_transition().side_effects);
        }
        Ok(RuntimeTransition {
            applied,
            side_effects,
        })
    }

    /// Applies a primary-client disconnect delivered through async runtime
    /// event ingress.
    ///
    /// Non-primary disconnects are accepted as stale or observer-local events
    /// for now. Primary disconnects reuse the normal detach path and append an
    /// additional diagnostic carrying the async I/O reason so event consumers
    /// can distinguish user-initiated detach from fd hangup or service exit.
    pub fn apply_primary_client_disconnect_event(
        &mut self,
        client_id: &mez_core::ids::ClientId,
        reason: impl Into<String>,
    ) -> Result<bool> {
        if self.session.primary_client_id() != Some(client_id) {
            return Ok(false);
        }
        let reason = reason.into();
        let terminal_size = self.session.authoritative_size;
        self.detach_primary(client_id, terminal_size)?;
        self.append_lifecycle_event(
            EventKind::Diagnostic,
            format!(
                r#"{{"client_id":"{}","client_disconnect":"primary","reason":"{}"}}"#,
                json_escape(client_id.as_str()),
                json_escape(&reason)
            ),
        )?;
        self.persist_or_defer_registry_update()?;
        Ok(true)
    }

    /// Runs the kill session operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn kill_session(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        force: bool,
    ) -> Result<()> {
        self.require_live()?;
        let previous_state = self.lifecycle_state;
        if self.session.primary_client_id() != Some(primary_client_id) {
            return Err(crate::error::MezError::forbidden(
                "operation requires the primary client",
            ));
        }
        let panes_have_live_process = self
            .session
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .any(|pane| pane.live);
        let pane_ids = self
            .session
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .map(|pane| pane.id.to_string())
            .collect::<Vec<_>>();
        if force || !panes_have_live_process {
            self.fail_agent_turns_for_pane_shutdown(&pane_ids, "session killed")?;
        }
        self.lifecycle_state = RuntimeLifecycleState::Stopping;
        if let Err(error) = self.session.kill_session(primary_client_id, force) {
            self.lifecycle_state = previous_state;
            return Err(error.into());
        }
        self.stop_all_active_pane_pipes();
        let terminated = self.terminate_all_runtime_pane_processes(force)?;
        let terminated_mcp_servers = self.clear_runtime_mcp_transports();
        self.persist_session_memory_to_disk();
        let cleared_memory = self.session_memory.clear_all();
        self.lifecycle_state = RuntimeLifecycleState::Killed;
        self.run_configured_completed_hooks(
            HookEvent::SessionStop,
            &format!(
                r#"{{"client_id":"{}","lifecycle":"killed","terminated_panes":{},"terminated_mcp_servers":{},"cleared_session_memory":{}}}"#,
                json_escape(primary_client_id.as_str()),
                terminated,
                terminated_mcp_servers,
                cleared_memory
            ),
        )?;
        self.append_lifecycle_event(
            EventKind::Diagnostic,
            format!(
                r#"{{"client_id":"{}","lifecycle":"killed","terminated_panes":{},"terminated_mcp_servers":{},"cleared_session_memory":{}}}"#,
                json_escape(primary_client_id.as_str()),
                terminated,
                terminated_mcp_servers,
                cleared_memory
            ),
        )?;
        self.persist_or_defer_registry_update()?;
        Ok(())
    }

    /// Applies supervisor shutdown through the transport-neutral transition contract.
    pub(crate) fn apply_shutdown_transition(
        &mut self,
        shutdown: ShutdownEvent,
    ) -> Result<RuntimeTransition> {
        let applied = if shutdown.failed {
            self.apply_supervisor_failure_event(shutdown.reason, shutdown.force)?
        } else {
            self.apply_supervisor_shutdown_event(shutdown.reason, shutdown.force)?
        };
        let side_effects = if applied {
            self.session
                .clients()
                .iter()
                .filter(|client| client.state == mez_mux::session::ClientState::Attached)
                .map(|client| RuntimeSideEffect::RenderClient {
                    client_id: client.id.clone(),
                    reason: RenderInvalidationReason::FullRedraw,
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut transition = RuntimeTransition {
            applied,
            side_effects,
        };
        if applied {
            transition
                .side_effects
                .extend(self.registry_persistence_transition().side_effects);
        }
        Ok(transition)
    }

    /// Applies a supervisor-originated shutdown event delivered through async
    /// runtime event ingress.
    ///
    /// Unlike `kill_session`, this path does not require a live primary client:
    /// it is used by supervisor, signal, or failed-service paths where the
    /// primary may already be gone. Non-forced shutdown transitions the runtime
    /// into stopping state and leaves live panes alone. Forced shutdown
    /// interrupts panes, clears live session shape, and removes the session from
    /// the registry.
    pub fn apply_supervisor_shutdown_event(
        &mut self,
        reason: impl Into<String>,
        force: bool,
    ) -> Result<bool> {
        match self.lifecycle_state {
            RuntimeLifecycleState::Killed | RuntimeLifecycleState::Failed => return Ok(false),
            RuntimeLifecycleState::Stopping if !force => return Ok(false),
            RuntimeLifecycleState::Stopping
            | RuntimeLifecycleState::Running
            | RuntimeLifecycleState::Detached => {}
        }
        let reason = reason.into();
        let pane_ids = self
            .session
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .map(|pane| pane.id.to_string())
            .collect::<Vec<_>>();
        if force {
            self.fail_agent_turns_for_pane_shutdown(&pane_ids, "runtime supervisor shutdown")?;
        }
        self.lifecycle_state = RuntimeLifecycleState::Stopping;
        self.session.state = mez_mux::session::SessionState::Stopping;

        if !force {
            self.append_lifecycle_event(
                EventKind::Diagnostic,
                format!(
                    r#"{{"lifecycle":"stopping","shutdown_reason":"{}","force":false}}"#,
                    json_escape(&reason)
                ),
            )?;
            self.persist_or_defer_registry_update()?;
            return Ok(true);
        }

        self.stop_all_active_pane_pipes();
        let terminated = self.terminate_all_runtime_pane_processes(force)?;
        let terminated_mcp_servers = self.clear_runtime_mcp_transports();
        self.persist_session_memory_to_disk();
        let cleared_memory = self.session_memory.clear_all();
        self.session.force_supervisor_shutdown();
        self.lifecycle_state = RuntimeLifecycleState::Killed;
        self.run_configured_completed_hooks(
            HookEvent::SessionStop,
            &format!(
                r#"{{"lifecycle":"shutdown","shutdown_reason":"{}","force":true,"terminated_panes":{},"terminated_mcp_servers":{},"cleared_session_memory":{}}}"#,
                json_escape(&reason),
                terminated,
                terminated_mcp_servers,
                cleared_memory
            ),
        )?;
        self.append_lifecycle_event(
            EventKind::Diagnostic,
            format!(
                r#"{{"lifecycle":"shutdown","shutdown_reason":"{}","force":true,"terminated_panes":{},"terminated_mcp_servers":{},"cleared_session_memory":{}}}"#,
                json_escape(&reason),
                terminated,
                terminated_mcp_servers,
                cleared_memory
            ),
        )?;
        Ok(true)
    }

    /// Applies a supervisor-originated failure event delivered through async
    /// runtime event ingress.
    ///
    /// Failed critical async services need a distinct state from graceful
    /// stopping and forced kill. This path records a failed lifecycle
    /// diagnostic, optionally interrupts live panes, fails active pane agent
    /// turns, and persists the registry update without requiring a primary
    /// client to still be attached.
    pub fn apply_supervisor_failure_event(
        &mut self,
        reason: impl Into<String>,
        force: bool,
    ) -> Result<bool> {
        match self.lifecycle_state {
            RuntimeLifecycleState::Killed | RuntimeLifecycleState::Failed => return Ok(false),
            RuntimeLifecycleState::Stopping
            | RuntimeLifecycleState::Running
            | RuntimeLifecycleState::Detached => {}
        }
        let reason = reason.into();
        let pane_ids = self
            .session
            .windows()
            .iter()
            .flat_map(|window| window.panes())
            .map(|pane| pane.id.to_string())
            .collect::<Vec<_>>();
        self.fail_agent_turns_for_pane_shutdown(&pane_ids, "runtime supervisor failure")?;
        self.stop_all_active_pane_pipes();
        let terminated_panes = if force {
            self.terminate_all_runtime_pane_processes(force)?
        } else {
            0
        };
        let terminated_mcp_servers = self.clear_runtime_mcp_transports();
        self.lifecycle_state = RuntimeLifecycleState::Failed;
        self.session.state = mez_mux::session::SessionState::Failed;
        self.append_lifecycle_event(
            EventKind::Diagnostic,
            format!(
                r#"{{"lifecycle":"failed","shutdown_reason":"{}","force":{},"terminated_panes":{},"terminated_mcp_servers":{}}}"#,
                json_escape(&reason),
                force,
                terminated_panes,
                terminated_mcp_servers
            ),
        )?;
        Ok(true)
    }
}
