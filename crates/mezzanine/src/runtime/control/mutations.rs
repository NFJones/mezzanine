//! Runtime control helpers for mutating session and terminal requests.
//!
//! This module owns runtime-backed mutating control operations for session,
//! window, pane, observer, terminal, agent-shell, MCP retry, and pane capture
//! requests. The parent control module keeps request routing and shared context
//! helpers while this module keeps live session mutations out of the main
//! control facade.

use super::{
    AgentId, AttachedTerminalClientStepPlan, ClientRole, ClientState, ClientViewRole, EventKind,
    MezError, PaneCaptureSource, PaneId, Result, RuntimeLifecycleState, RuntimeSessionService,
    RuntimeSideEffect, SenderIdentity, SplitDirection, TerminalClientLoopAction,
    TerminalClientLoopConfig, destination_target_checked_resolved,
    dispatch_control_request_for_client_with_agent_state, dispatch_control_request_with_captures,
    json_escape, layout_state_json, pane_id_from_runtime_agent_id, pane_target_checked_resolved,
    rendered_client_view_json, route_client_input_actions, runtime_json_bool_field,
    runtime_json_creation_command, runtime_json_input_bytes, runtime_json_optional_client_size,
    runtime_json_optional_size_field, runtime_json_optional_view_offset, runtime_json_rpc_error,
    runtime_json_size, runtime_json_start_directory, runtime_json_string_field,
    runtime_json_terminal_step_render_if_changed, runtime_mcp_retry_event_payload,
    runtime_mcp_retry_result_json, runtime_mutating_response_is_cacheable, runtime_pane_by_id,
    runtime_pane_readiness_state_name, runtime_split_direction, runtime_terminal_step_result_json,
    source_pane_target_checked_resolved, window_target_checked_resolved,
};
use crate::runtime::RenderInvalidationReason;

impl RuntimeSessionService {
    /// Runs the dispatch runtime mutating request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_mutating_request(
        &mut self,
        request: crate::control::JsonRpcRequest,
        caller_client_id: &mez_core::ids::ClientId,
    ) -> String {
        let params = request.params.clone().unwrap_or_else(|| "{}".to_string());
        let idempotency_key = match runtime_json_string_field(&params, "idempotency_key") {
            Some(value) => value,
            None => {
                return runtime_json_rpc_error(
                    &request.id,
                    crate::error::MezErrorKind::InvalidArgs,
                    "mutating control request requires idempotency_key",
                );
            }
        };
        let cache_key = format!("{caller_client_id}:{idempotency_key}");
        let cacheable_response = runtime_mutating_response_is_cacheable(&request.method);
        if cacheable_response {
            match self.control.idempotency_mut().cached_response(
                &cache_key,
                &request.method,
                &request.params,
            ) {
                Ok(Some(response)) => return response,
                Ok(None) => {}
                Err(error) => {
                    return runtime_json_rpc_error(&request.id, error.kind(), error.message());
                }
            }
        }

        let result = self.dispatch_runtime_mutating_result(
            request.method.as_str(),
            caller_client_id,
            &params,
        );
        let response = match result {
            Ok(result) => format!(
                r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                request.id
            ),
            Err(error) => runtime_json_rpc_error(&request.id, error.kind(), error.message()),
        };
        if cacheable_response {
            self.control.idempotency_mut().remember_response(
                cache_key,
                request.method,
                request.params,
                response.clone(),
            );
        }
        response
    }

    /// Runs the dispatch runtime mutating result operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_mutating_result(
        &mut self,
        method: &str,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        match method {
            "window/create" => self.dispatch_runtime_window_create(primary_client_id, params),
            "window/layout" => self.dispatch_runtime_window_layout(primary_client_id, params),
            "window/rebalance" => self.dispatch_runtime_window_rebalance(primary_client_id, params),
            "pane/create" => self.dispatch_runtime_pane_create(primary_client_id, params),
            "pane/resize" => self.dispatch_runtime_pane_resize(primary_client_id, params),
            "pane/swap" => self.dispatch_runtime_pane_swap(primary_client_id, params),
            "pane/break" => self.dispatch_runtime_pane_break(primary_client_id, params),
            "pane/join" | "pane/move" => self.dispatch_runtime_pane_join(primary_client_id, params),
            "pane/close" => self.dispatch_runtime_pane_close(primary_client_id, params),
            "pane/zoom" => self.dispatch_runtime_pane_zoom(primary_client_id, params),
            "pane/attention" => self.dispatch_runtime_pane_attention(params),
            "pane/status" => self.dispatch_runtime_pane_status(primary_client_id, params),
            "pane/notice" => self.dispatch_runtime_pane_notice(params),
            "buffer/create" => self.dispatch_runtime_buffer_create(params),
            "buffer/delete" => self.dispatch_runtime_buffer_delete(params),
            "client/detach" => self.dispatch_runtime_client_detach(primary_client_id, params),
            "client/set_layout_owner" => {
                self.dispatch_runtime_layout_owner_transfer(primary_client_id, params)
            }
            "window/close" => self.dispatch_runtime_window_close(primary_client_id, params),
            "session/kill" => self.dispatch_runtime_session_kill(primary_client_id, params),
            "terminal/resize" => self.dispatch_runtime_observer_resize(primary_client_id, params),
            "terminal/step" => self.dispatch_runtime_terminal_step(primary_client_id, params),
            "terminal/command" => self.dispatch_runtime_terminal_command(primary_client_id, params),
            "agent/shell/command" => {
                self.dispatch_runtime_agent_shell_command(primary_client_id, params)
            }
            "agent/spawn" => self.dispatch_runtime_agent_spawn(primary_client_id, params),
            "project/trust/decide" | "project/trust/revoke" => {
                self.dispatch_runtime_project_trust_mutation(method, primary_client_id, params)
            }
            "mcp/retry" => self.dispatch_runtime_mcp_retry(params),
            _ => Err(MezError::invalid_state(
                "runtime control method was filtered incorrectly",
            )),
        }
    }

    /// Updates only the authenticated observer's retained terminal geometry.
    pub(super) fn dispatch_runtime_observer_resize(
        &mut self,
        observer_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        self.require_live()?;
        let size = runtime_json_optional_client_size(params)?
            .ok_or_else(|| MezError::invalid_args("terminal/resize requires client_size"))?;
        self.session
            .resize_observer_terminal(observer_client_id, size)?;
        self.presentation
            .defer_render_effects([RuntimeSideEffect::RenderClient {
                client_id: observer_client_id.clone(),
                reason: RenderInvalidationReason::Resize,
            }]);
        Ok(format!(
            r#"{{"resized":true,"client_size":{{"columns":{},"rows":{}}}}}"#,
            size.columns, size.rows
        ))
    }

    /// Detaches one exact client through runtime lifecycle and presentation owners.
    pub(super) fn dispatch_runtime_client_detach(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        self.require_live()?;
        self.session.require_primary(primary_client_id)?;
        let target = runtime_json_string_field(params, "client_id")
            .unwrap_or_else(|| primary_client_id.to_string());
        let target_client = self
            .session
            .clients()
            .iter()
            .find(|client| client.id.as_str() == target)
            .cloned()
            .ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "client not found")
            })?;
        if target_client.role == ClientRole::Primary {
            let terminal_size = target_client
                .terminal
                .as_ref()
                .map(|terminal| mez_mux::layout::Size::new(terminal.columns, terminal.rows))
                .transpose()?
                .unwrap_or(self.session.authoritative_size);
            self.detach_primary(&target_client.id, terminal_size)?;
        } else if target_client.id == *primary_client_id {
            self.session.detach_client_self(primary_client_id)?;
            self.presentation.remove_client_state(primary_client_id);
            self.control
                .remove_unix_event_bindings_for_client(primary_client_id);
        } else {
            self.session
                .detach_client_target(primary_client_id, target_client.id.as_str())?;
            self.presentation.remove_client_state(&target_client.id);
            self.control
                .remove_unix_event_bindings_for_client(&target_client.id);
        }
        let protected_clients = self.presentation.pending_client_references();
        self.session
            .prune_detached_client_summaries(&protected_clients);
        Ok(format!(
            r#"{{"detached":true,"client_id":"{}"}}"#,
            json_escape(target_client.id.as_str())
        ))
    }

    /// Transfers canonical layout ownership and synchronizes resulting pane geometry.
    pub(super) fn dispatch_runtime_layout_owner_transfer(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        self.require_live()?;
        let target = runtime_json_string_field(params, "client_id")
            .ok_or_else(|| MezError::invalid_args("client/set_layout_owner requires client_id"))?;
        let transition = self
            .session
            .select_layout_owner_transition(Some(primary_client_id), &target)?;
        let updates = self.sync_pane_resize_effects(&transition.resize_effects)?;
        self.presentation.clear_mouse_resize_drag_state();
        self.reflow_primary_record_browser_overlay();
        Ok(format!(
            r#"{{"layout_owner_client_id":"{}","layout_revision":{},"resized_panes":{}}}"#,
            json_escape(transition.client_id.as_str()),
            self.session.layout_revision(),
            updates.len()
        ))
    }

    /// Runs the dispatch runtime mcp retry operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_mcp_retry(&mut self, params: &str) -> Result<String> {
        let server_id = runtime_json_string_field(params, "server_id")
            .or_else(|| runtime_json_string_field(params, "id"))
            .ok_or_else(|| MezError::invalid_args("mcp/retry requires server_id"))?;
        let report = self.retry_runtime_mcp_server(&server_id)?;
        self.append_lifecycle_event(
            EventKind::McpServerChanged,
            runtime_mcp_retry_event_payload("control:mcp/retry", &report),
        )?;
        Ok(runtime_mcp_retry_result_json(&report))
    }

    /// Runs the dispatch runtime window create operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_window_create(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let name = runtime_json_string_field(params, "name").unwrap_or_else(|| "shell".to_string());
        let select = runtime_json_bool_field(params, "select").unwrap_or(true);
        let command = runtime_json_creation_command(params)?;
        let start_directory = runtime_json_start_directory(params)?;
        let started = self.create_window_with_pane_process_with_options(
            primary_client_id,
            name,
            select,
            command.as_deref(),
            start_directory.as_deref(),
            None,
        )?;
        self.runtime_started_pane_result_json(&started, true)
    }

    /// Selects a layout policy for a target window and synchronizes live PTYs.
    pub(super) fn dispatch_runtime_window_layout(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let target = window_target_checked_resolved(&self.session, params)?;
        let layout = runtime_json_string_field(params, "layout")
            .ok_or_else(|| MezError::invalid_args("window/layout requires layout"))?;
        let (policy, effects) = self.session.select_window_layout_transition(
            primary_client_id,
            target.as_deref(),
            &layout,
        )?;
        self.sync_pane_resize_effects(&effects)?;
        let window = target
            .as_deref()
            .and_then(|id| {
                self.session
                    .windows()
                    .iter()
                    .find(|window| window.id.as_str() == id)
            })
            .or_else(|| self.session.active_window())
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        Ok(format!(
            r#"{{"window_id":"{}","layout":"{}","state":{}}}"#,
            json_escape(window.id.as_str()),
            policy.name(),
            layout_state_json(window)
        ))
    }

    /// Reapplies a target window's selected layout and synchronizes live PTYs.
    pub(super) fn dispatch_runtime_window_rebalance(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let target = window_target_checked_resolved(&self.session, params)?;
        let (policy, effects) = self
            .session
            .rebalance_target_window_transition(primary_client_id, target.as_deref())?;
        self.sync_pane_resize_effects(&effects)?;
        let window = target
            .as_deref()
            .and_then(|id| {
                self.session
                    .windows()
                    .iter()
                    .find(|window| window.id.as_str() == id)
            })
            .or_else(|| self.session.active_window())
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        Ok(format!(
            r#"{{"window_id":"{}","layout":"{}","state":{}}}"#,
            json_escape(window.id.as_str()),
            policy.name(),
            layout_state_json(window)
        ))
    }

    /// Runs the dispatch runtime pane create operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_pane_create(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        if let Some(target) = pane_target_checked_resolved(&self.session, params)? {
            self.session.select_pane(primary_client_id, &target)?;
            self.acknowledge_focused_pane_completion();
        }
        let split =
            runtime_json_string_field(params, "split").unwrap_or_else(|| "vertical".to_string());
        let select = runtime_json_bool_field(params, "select").unwrap_or(true);
        let command = runtime_json_creation_command(params)?;
        let start_directory = runtime_json_start_directory(params)?;
        let requested_size = runtime_json_optional_size_field(params, "size")?;
        if split == "window" {
            let started = self.create_window_with_pane_process_with_options(
                primary_client_id,
                "shell",
                select,
                command.as_deref(),
                start_directory.as_deref(),
                requested_size,
            )?;
            return self.runtime_started_pane_result_json(&started, false);
        }
        let direction = runtime_split_direction(&split)?;
        let started = self.split_pane_with_process_with_options(
            primary_client_id,
            direction,
            select,
            command.as_deref(),
            start_directory.as_deref(),
            requested_size,
        )?;
        self.runtime_started_pane_result_json(&started, false)
    }

    /// Runs the dispatch runtime pane resize operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_pane_resize(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let target = pane_target_checked_resolved(&self.session, params)?;
        let spec = runtime_json_size(params)?;
        let update = self.resize_pane_pty_with_spec(primary_client_id, target.as_deref(), spec)?;
        self.runtime_pane_resize_result_json(&update)
    }

    /// Runs the dispatch runtime pane swap operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_pane_swap(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let source = source_pane_target_checked_resolved(&self.session, params)?;
        let destination = destination_target_checked_resolved(&self.session, params)?
            .ok_or_else(|| MezError::invalid_args("pane/swap requires destination"))?;
        let updates =
            self.swap_panes_and_sync_pty_sizes(primary_client_id, source.as_deref(), &destination)?;
        let layout = self.runtime_active_layout_state_json()?;
        Ok(format!(
            r#"{{"layout":{layout},"synced_panes":{}}}"#,
            updates.len()
        ))
    }

    /// Runs the dispatch runtime pane break operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_pane_break(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let target = pane_target_checked_resolved(&self.session, params)?;
        let name = runtime_json_string_field(params, "name");
        let (window_id, updates) =
            self.break_pane_and_sync_pty_sizes(primary_client_id, target.as_deref(), name, true)?;
        let window = self
            .session
            .windows()
            .iter()
            .find(|window| window.id.as_str() == window_id.as_str())
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "created window not found",
                )
            })?;
        let pane = window.active_pane();
        Ok(format!(
            r#"{{"window":{},"pane":{},"synced_panes":{}}}"#,
            self.runtime_window_state_json(window),
            self.runtime_control_pane_state_json(window, pane),
            updates.len()
        ))
    }

    /// Runs the dispatch runtime pane join operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_pane_join(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let source = source_pane_target_checked_resolved(&self.session, params)?;
        let destination = destination_target_checked_resolved(&self.session, params)?
            .ok_or_else(|| MezError::invalid_args("pane/join requires destination"))?;
        let direction = runtime_json_string_field(params, "position")
            .as_deref()
            .map(runtime_split_direction)
            .transpose()?
            .unwrap_or(SplitDirection::Vertical);
        let (pane_id, updates) = self.join_pane_and_sync_pty_sizes(
            primary_client_id,
            source.as_deref(),
            &destination,
            direction,
            true,
        )?;
        let (window, pane) = runtime_pane_by_id(&self.session, pane_id.as_str())?;
        Ok(format!(
            r#"{{"pane":{},"layout":{},"synced_panes":{}}}"#,
            self.runtime_control_pane_state_json(window, pane),
            layout_state_json(window),
            updates.len()
        ))
    }

    /// Runs the dispatch runtime pane close operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn dispatch_runtime_pane_close(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        self.require_live()?;
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "operation requires an attached primary client",
            ));
        }
        let force = runtime_json_bool_field(params, "force").unwrap_or(false);
        let target = pane_target_checked_resolved(&self.session, params)?;
        let descriptor = match target.as_deref() {
            Some(target) => self.find_pane_descriptor(target).ok_or_else(|| {
                MezError::new(crate::error::MezErrorKind::NotFound, "pane not found")
            })?,
            None => self.active_window_pane_descriptor(None)?,
        };
        let render_effects = self.render_effects_for_clients_projecting_windows(
            &[descriptor.window_id.to_string()],
            RenderInvalidationReason::Layout,
        );
        let (_, pane) = runtime_pane_by_id(&self.session, descriptor.pane_id.as_str())?;
        if force || !pane.live {
            self.fail_agent_turns_for_pane_shutdown(
                &[descriptor.pane_id.to_string()],
                "pane closed",
            )?;
        }
        let transition =
            self.session
                .kill_pane_with_effects(primary_client_id, target.as_deref(), force)?;
        let terminated = if let Some(pane) = transition.pane {
            let pane_id = pane.id.to_string();
            let _ = self.stop_active_pane_pipe(pane.id.as_str());
            let terminated = usize::from(self.terminate_runtime_pane_process(&pane_id, force)?);
            self.cleanup_removed_pane_runtime_state(&pane_id)?;
            terminated
        } else {
            0
        };
        self.sync_pane_resize_effects(&transition.effects)?;
        self.presentation.defer_render_effects(render_effects);
        self.session
            .set_lifecycle_state(RuntimeLifecycleState::from_session_state(
                self.session.state,
            ));
        self.append_pane_close_event(
            descriptor.pane_id.as_str(),
            descriptor.window_id.as_str(),
            terminated,
            self.session.windows().is_empty(),
        )?;
        Ok(format!(
            r#"{{"closed":true,"terminated_panes":{},"session_empty":{}}}"#,
            terminated,
            self.session.windows().is_empty()
        ))
    }

    /// Runs the dispatch runtime window close operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_window_close(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        self.require_live()?;
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "operation requires an attached primary client",
            ));
        }
        let force = runtime_json_bool_field(params, "force").unwrap_or(false);
        let target = window_target_checked_resolved(&self.session, params)?;
        let window = if let Some(target) = target.as_deref() {
            self.session
                .windows()
                .iter()
                .find(|window| window.id.as_str() == target)
                .ok_or_else(|| {
                    MezError::new(crate::error::MezErrorKind::NotFound, "window not found")
                })?
        } else {
            self.session
                .active_window()
                .ok_or_else(|| MezError::invalid_state("session has no active window"))?
        };
        let panes_have_live_process = window.panes().iter().any(|pane| pane.live);
        let pane_ids = window
            .panes()
            .iter()
            .map(|pane| pane.id.to_string())
            .collect::<Vec<_>>();
        if force || !panes_have_live_process {
            self.fail_agent_turns_for_pane_shutdown(&pane_ids, "window closed")?;
        }
        let transition =
            self.session
                .kill_window_transition(primary_client_id, target.as_deref(), force)?;
        let pane_ids = transition
            .window
            .panes()
            .iter()
            .map(|pane| pane.id.to_string())
            .collect::<Vec<_>>();
        let pane_id_refs = pane_ids.iter().map(String::as_str).collect::<Vec<_>>();
        self.stop_active_pane_pipes_for(pane_id_refs.as_slice());
        let terminated = self.terminate_runtime_pane_processes(pane_id_refs, force)?;
        for pane_id in &pane_ids {
            self.cleanup_removed_pane_runtime_state(pane_id)?;
        }
        self.sync_pane_resize_effects(&transition.effects)?;
        self.session
            .set_lifecycle_state(RuntimeLifecycleState::from_session_state(
                self.session.state,
            ));
        self.append_window_close_event(
            transition.window.id.as_str(),
            terminated,
            self.session.windows().is_empty(),
        )?;
        Ok(format!(
            r#"{{"closed":true,"terminated_panes":{},"session_empty":{}}}"#,
            terminated,
            self.session.windows().is_empty()
        ))
    }

    /// Runs the dispatch runtime session kill operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_session_kill(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let force = runtime_json_bool_field(params, "force").unwrap_or(false);
        self.kill_session(primary_client_id, force)?;
        Ok(format!(
            r#"{{"killed":true,"session_id":"{}"}}"#,
            json_escape(self.session.id.as_str())
        ))
    }

    /// Runs the dispatch runtime terminal step operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_terminal_step(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        self.require_live()?;
        if !self.session.is_attached_primary(primary_client_id) {
            return Err(MezError::forbidden(
                "operation requires an attached primary client",
            ));
        }
        let input = runtime_json_input_bytes(params)?;
        let render = runtime_json_bool_field(params, "render").unwrap_or(true);
        let render_if_changed = runtime_json_terminal_step_render_if_changed(params)?;
        let client_size =
            runtime_json_optional_client_size(params)?.unwrap_or(self.session.authoritative_size);
        if client_size != self.session.authoritative_size {
            self.resize_attached_primary_terminal(primary_client_id, client_size)?;
        }
        let terminal_config =
            self.terminal_client_loop_config(TerminalClientLoopConfig::default())?;
        let prompt_active = if input.is_empty() {
            false
        } else {
            self.render_client_view_with_resolved_config(
                ClientViewRole::Primary,
                client_size,
                &terminal_config,
            )?
            .is_some_and(|view| view.primary_prompt_active)
        };
        let actions = if prompt_active {
            vec![TerminalClientLoopAction::ForwardToPane(input.clone())]
        } else {
            route_client_input_actions(&input, &terminal_config)?
        };
        let step = AttachedTerminalClientStepPlan {
            actions,
            output_lines: Vec::new(),
            output_line_style_spans: Vec::new(),
            input_hangup: false,
            output_hangup: false,
            error_roles: Vec::new(),
        };
        let application = self.apply_attached_terminal_step_plan(primary_client_id, &step)?;
        let render_reason = self.attached_terminal_step_render_reason(&application, &step);
        let view = if render
            || (render_if_changed
                && (application.view_refresh_required || application.full_redraw_required))
        {
            self.render_client_view_with_resolved_config(
                ClientViewRole::Primary,
                client_size,
                &terminal_config,
            )?
        } else {
            None
        };
        if view.is_none()
            && let Some(reason) = render_reason
        {
            self.presentation.defer_render_effects(
                self.render_effects_for_primary_projection(primary_client_id, reason),
            );
        }
        let iroh_status_slot = view
            .as_ref()
            .and_then(|view| self.terminal_iroh_status_slot(view, &terminal_config));
        let event_cutoff = view.as_ref().map(|_| {
            self.event_log()
                .map(|event_log| event_log.latest_event_id())
                .unwrap_or(0)
        });
        let session_terminated = matches!(
            self.lifecycle_state(),
            RuntimeLifecycleState::Killed | RuntimeLifecycleState::Failed
        );
        let client_detached = !self.session.is_attached_primary(primary_client_id);
        Ok(runtime_terminal_step_result_json(
            input.len(),
            &application,
            view.as_ref(),
            iroh_status_slot.as_ref(),
            event_cutoff,
            client_detached,
            session_terminated,
        ))
    }

    /// Runs the dispatch runtime terminal view operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_terminal_view(
        &mut self,
        caller_client_id: &mez_core::ids::ClientId,
        params: Option<&str>,
    ) -> Result<String> {
        let client = self
            .session
            .clients()
            .iter()
            .find(|client| client.id == *caller_client_id)
            .ok_or_else(|| MezError::forbidden("unknown control client"))?;
        if client.state != ClientState::Attached {
            return Err(MezError::forbidden("control client is not attached"));
        }
        let role = match client.role {
            ClientRole::Primary => ClientViewRole::Primary,
            ClientRole::Observer => ClientViewRole::Observer,
            ClientRole::Agent | ClientRole::Automation => {
                return Err(MezError::forbidden(
                    "client role is not authorized for terminal view",
                ));
            }
        };
        let client_size = match params {
            Some(params) => runtime_json_optional_client_size(params)?
                .unwrap_or(self.session.authoritative_size),
            None => self.session.authoritative_size,
        };
        self.prepare_client_render(caller_client_id, role)?;
        let terminal_config =
            self.terminal_client_loop_config(TerminalClientLoopConfig::default())?;
        let mut view =
            self.render_client_view_with_resolved_config(role, client_size, &terminal_config)?;
        if let (Some(params), Some(view)) = (params, view.as_mut())
            && let Some((row, column)) = runtime_json_optional_view_offset(params)?
        {
            mez_mux::presentation::apply_client_view_offset(view, row, column);
        }
        let iroh_status_slot = view
            .as_ref()
            .and_then(|view| self.terminal_iroh_status_slot(view, &terminal_config));
        let view_json = view
            .as_ref()
            .map(|view| rendered_client_view_json(view, iroh_status_slot.as_ref()))
            .unwrap_or_else(|| "null".to_string());
        let event_cutoff = self
            .event_log()
            .map(|event_log| event_log.latest_event_id())
            .unwrap_or(0);
        Ok(format!(
            r#"{{"view":{view_json},"event_cutoff":{event_cutoff}}}"#
        ))
    }

    /// Dispatches runtime-owned agent shell visibility changes.
    ///
    /// The shared control layer mutates persisted agent shell state. The live
    /// runtime layers pane-local side effects on top of that state change so
    /// showing agent mode enters the scoped child shell and hiding agent mode
    /// leaves it when no turn still needs it.
    pub(super) fn dispatch_runtime_agent_shell_visibility_request(
        &mut self,
        body: &str,
        request: &crate::control::JsonRpcRequest,
        caller_client_id: &mez_core::ids::ClientId,
    ) -> String {
        let pane_id = match self.runtime_agent_shell_visibility_target_pane_id(request) {
            Ok(pane_id) => pane_id,
            Err(error) => {
                return runtime_json_rpc_error(&request.id, error.kind(), error.message());
            }
        };
        let (agent_shell_store, agent_turn_ledger) = self.agent.control_turn_state();
        let response = dispatch_control_request_for_client_with_agent_state(
            body,
            &mut self.session,
            caller_client_id,
            None,
            agent_shell_store,
            agent_turn_ledger,
        );
        if response.contains(r#""error""#) {
            return response;
        }
        let side_effect = if request.method == "agent/shell/show" {
            self.enter_agent_mode_for_pane(&pane_id).map(|_| ())
        } else {
            self.request_agent_shell_exit_for_pane(&pane_id).map(|_| ())
        }
        .and_then(|()| self.sync_tracked_pty_sizes().map(|_| ()));
        match side_effect {
            Ok(()) => response,
            Err(error) => runtime_json_rpc_error(&request.id, error.kind(), error.message()),
        }
    }

    /// Resolves the pane affected by an `agent/shell/show` or
    /// `agent/shell/hide` request before live side effects are applied.
    fn runtime_agent_shell_visibility_target_pane_id(
        &self,
        request: &crate::control::JsonRpcRequest,
    ) -> Result<String> {
        let params = request.params.as_deref().ok_or_else(|| {
            MezError::invalid_args(format!("{} requires a params object", request.method))
        })?;
        pane_target_checked_resolved(&self.session, params)?
            .map(Ok)
            .unwrap_or_else(|| self.active_pane_id())
    }

    /// Runs the dispatch runtime terminal command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_terminal_command(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let input = runtime_json_string_field(params, "input")
            .ok_or_else(|| MezError::invalid_args("terminal/command requires input"))?;
        self.execute_terminal_command(primary_client_id, &input)
    }

    /// Sets or clears the completion-attention indicator for one pane.
    ///
    /// An omitted target resolves to the active pane. The pane must exist, and
    /// `attention` must be an explicit boolean so hook integrations cannot
    /// accidentally toggle presentation state with a malformed request.
    pub(super) fn dispatch_runtime_pane_attention(&mut self, params: &str) -> Result<String> {
        let attention = runtime_json_bool_field(params, "attention").ok_or_else(|| {
            MezError::invalid_args("pane/attention requires attention to be a boolean")
        })?;
        let pane_id = pane_target_checked_resolved(&self.session, params)?
            .map(Ok)
            .unwrap_or_else(|| self.active_pane_id())?;
        runtime_pane_by_id(&self.session, &pane_id)?;
        if attention {
            self.presentation
                .register_completion_attention(&pane_id, None);
        } else {
            self.presentation.acknowledge_completion_attention(&pane_id);
        }
        Ok(format!(
            r#"{{"pane_id":"{}","attention":{attention}}}"#,
            json_escape(&pane_id)
        ))
    }

    /// Explicitly sets pane zoom and synchronizes the affected live PTYs.
    pub(super) fn dispatch_runtime_pane_zoom(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let zoomed = runtime_json_bool_field(params, "zoomed")
            .ok_or_else(|| MezError::invalid_args("pane/zoom requires zoomed to be a boolean"))?;
        let target = pane_target_checked_resolved(&self.session, params)?;
        let (pane_id, zoomed, effects) =
            self.session
                .set_pane_zoom_transition(primary_client_id, target.as_deref(), zoomed)?;
        self.sync_pane_resize_effects(&effects)?;
        let (window, _) = runtime_pane_by_id(&self.session, pane_id.as_str())?;
        Ok(format!(
            r#"{{"pane_id":"{}","zoomed":{zoomed},"layout":{}}}"#,
            json_escape(pane_id.as_str()),
            layout_state_json(window)
        ))
    }

    /// Runs the dispatch runtime agent shell command operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_agent_shell_command(
        &mut self,
        primary_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let input = runtime_json_string_field(params, "input")
            .ok_or_else(|| MezError::invalid_args("agent/shell/command requires input"))?;
        self.execute_agent_shell_control_command(primary_client_id, &input)
    }

    /// Ensures a runtime-created agent identity exists in the local MMP service.
    ///
    /// Agent ids are opaque MMP identities. When the id follows the runtime
    /// `agent-%pane` convention, the identity is enriched with pane and window
    /// metadata so discovery can connect the agent to its terminal surface.
    pub(crate) fn ensure_runtime_message_identity(
        &mut self,
        agent_id: &str,
        pane_id: Option<PaneId>,
        role: &str,
        capabilities: &[&str],
        now_ms: u64,
    ) -> Result<SenderIdentity> {
        let agent_id = AgentId::opaque(agent_id.to_string())
            .ok_or_else(|| MezError::invalid_args("agent id is invalid for MMP"))?;
        let pane_id = pane_id.or_else(|| pane_id_from_runtime_agent_id(agent_id.as_str()));
        let window_id = pane_id
            .as_ref()
            .and_then(|pane_id| self.find_pane_descriptor(pane_id.as_str()))
            .map(|descriptor| descriptor.window_id);
        Ok(self.control.message_service_mut().ensure_agent_identity(
            SenderIdentity {
                agent_id,
                pane_id,
                window_id,
                role: Some(role.to_string()),
                capabilities: capabilities
                    .iter()
                    .map(|capability| (*capability).to_string())
                    .collect(),
            },
            now_ms,
        )?)
    }

    /// Runs the dispatch runtime pane capture operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_pane_capture(
        &mut self,
        body: &str,
        request_id: &str,
        caller_client_id: &mez_core::ids::ClientId,
    ) -> String {
        if !self.session.is_attached_primary(caller_client_id) {
            return runtime_json_rpc_error(
                request_id,
                crate::error::MezErrorKind::Forbidden,
                "operation requires the primary client",
            );
        }
        let captures = self.pane_capture_sources();
        dispatch_control_request_with_captures(body, &mut self.session, caller_client_id, &captures)
    }

    /// Runs the pane capture sources operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn pane_capture_sources(&self) -> Vec<PaneCaptureSource> {
        self.process_pane_screens()
            .keys()
            .filter_map(|pane_id| {
                let screen = self.presented_pane_screen(pane_id)?;
                let history_styled_lines = screen.history().styled_lines().collect::<Vec<_>>();
                let primary_pid = self.primary_pid_for_live_pane_process(pane_id);
                let process_state = primary_pid.map(|_| "running").unwrap_or_else(|| {
                    match runtime_pane_by_id(&self.session, pane_id) {
                        Ok((_, pane)) if pane.live => "starting",
                        Ok((_, _)) => "exited",
                        Err(_) => "unknown",
                    }
                });
                Some(PaneCaptureSource {
                    pane_id: pane_id.clone(),
                    visible_lines: screen.visible_lines(),
                    visible_line_style_spans: screen
                        .visible_styled_lines()
                        .into_iter()
                        .map(|line| line.style_spans)
                        .collect(),
                    history_lines: history_styled_lines
                        .iter()
                        .map(|line| line.text.clone())
                        .collect(),
                    history_line_style_spans: history_styled_lines
                        .into_iter()
                        .map(|line| line.style_spans)
                        .collect(),
                    alternate_screen_active: screen.alternate_screen_active(),
                    truncated: false,
                    primary_pid,
                    process_state: Some(process_state.to_string()),
                    readiness_state: Some(
                        runtime_pane_readiness_state_name(self.pane_readiness_state(pane_id))
                            .to_string(),
                    ),
                    exit_status: self.pane_exit_status(pane_id),
                })
            })
            .collect()
    }
    /// Runs the require attachable operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub(crate) fn require_attachable(&self) -> Result<()> {
        match self.session.lifecycle_state() {
            RuntimeLifecycleState::Running | RuntimeLifecycleState::Detached => Ok(()),
            RuntimeLifecycleState::Stopping => {
                Err(MezError::invalid_state("runtime service is stopping"))
            }
            RuntimeLifecycleState::Killed => Err(MezError::invalid_state(
                "runtime service has already been killed",
            )),
            RuntimeLifecycleState::Failed => Err(MezError::invalid_state(
                "runtime service is in a failed lifecycle state",
            )),
        }
    }

    /// Runs the require live operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn require_live(&self) -> Result<()> {
        match self.session.lifecycle_state() {
            RuntimeLifecycleState::Running | RuntimeLifecycleState::Detached => Ok(()),
            RuntimeLifecycleState::Stopping => {
                Err(MezError::invalid_state("runtime service is stopping"))
            }
            RuntimeLifecycleState::Killed => Err(MezError::invalid_state(
                "runtime service has already been killed",
            )),
            RuntimeLifecycleState::Failed => Err(MezError::invalid_state(
                "runtime service is in a failed lifecycle state",
            )),
        }
    }
}
