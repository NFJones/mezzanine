//! Runtime control read-only state serialization helpers.
//!
//! This module owns read-only state request dispatch and the JSON serialization
//! helpers that adapt live runtime session state into control protocol response
//! bodies. Keeping these helpers separate keeps the main control adapter focused
//! on method routing and mutation orchestration.

use super::super::{
    ClientState, ConfigScope, MezError, PaneProcessStart, PaneResizeUpdate, Result,
    RuntimeSessionService, TrustDecision, dispatch_event_list_request,
    frame_read_json_with_context, json_escape, layout_state_json, observers_json,
    runtime_approval_policy_name, runtime_json_string_field, runtime_pane_by_id,
    runtime_pane_readiness_state_name, runtime_permission_preset_name, runtime_string_array_json,
    session_state_name, state_request_pane_list_window_ids, state_request_session_target_matches,
};
use super::protocol::{
    runtime_client_requested_role_name, runtime_client_role_name, runtime_client_state_name,
    runtime_client_terminal_descriptor_json, runtime_optional_string,
    runtime_optional_timestamp_json, runtime_size_object_json, runtime_timestamp_json,
    runtime_validate_state_request_params,
};
use crate::control::control_event_audience;

impl RuntimeSessionService {
    /// Mints one short-lived event-socket binding for an initialized Unix client.
    pub(crate) fn mint_unix_event_binding(
        &mut self,
        client_id: mez_core::ids::ClientId,
        peer_uid: u32,
    ) -> (String, u64) {
        self.control
            .mint_unix_event_binding(client_id, peer_uid, super::current_unix_seconds())
    }

    /// Consumes one Unix event-socket binding without disclosing credential state.
    pub(crate) fn consume_unix_event_binding(
        &mut self,
        token: &str,
        peer_uid: u32,
    ) -> Result<mez_core::ids::ClientId> {
        self.control
            .consume_unix_event_binding(token, peer_uid, super::current_unix_seconds())
            .ok_or_else(|| MezError::forbidden("invalid or expired event binding"))
    }

    /// Runs the dispatch runtime read only state request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_read_only_state_request(
        &self,
        request: &crate::control::JsonRpcRequest,
        caller_client_id: &mez_core::ids::ClientId,
    ) -> Result<Option<String>> {
        match request.method.as_str() {
            "session/list" => {
                runtime_validate_state_request_params(
                    request.params.as_deref(),
                    "session/list",
                    &[],
                )?;
                Ok(Some(format!(
                    r#"{{"sessions":[{}]}}"#,
                    self.runtime_session_summary_json()
                )))
            }
            "session/get" => {
                runtime_validate_state_request_params(
                    request.params.as_deref(),
                    "session/get",
                    &["target"],
                )?;
                state_request_session_target_matches(
                    &self.session,
                    request.params.as_deref(),
                    "session/get params",
                )?;
                Ok(Some(format!(
                    r#"{{"session":{}}}"#,
                    self.runtime_session_state_json(caller_client_id)?
                )))
            }
            "client/list" => {
                runtime_validate_state_request_params(
                    request.params.as_deref(),
                    "client/list",
                    &["target"],
                )?;
                state_request_session_target_matches(
                    &self.session,
                    request.params.as_deref(),
                    "client/list params",
                )?;
                Ok(Some(format!(
                    r#"{{"clients":{}}}"#,
                    self.runtime_clients_json()
                )))
            }
            "window/list" => {
                runtime_validate_state_request_params(
                    request.params.as_deref(),
                    "window/list",
                    &["target"],
                )?;
                state_request_session_target_matches(
                    &self.session,
                    request.params.as_deref(),
                    "window/list params",
                )?;
                Ok(Some(format!(
                    r#"{{"windows":{}}}"#,
                    self.runtime_windows_state_json()
                )))
            }
            "pane/list" => {
                runtime_validate_state_request_params(
                    request.params.as_deref(),
                    "pane/list",
                    &["target"],
                )?;
                let window_ids = state_request_pane_list_window_ids(
                    &self.session,
                    request.params.as_deref(),
                    "pane/list params",
                )?;
                Ok(Some(format!(
                    r#"{{"panes":{}}}"#,
                    match window_ids {
                        Some(window_ids) =>
                            self.runtime_panes_state_json_for_window_ids(&window_ids)?,
                        None => self.runtime_panes_state_json(),
                    }
                )))
            }
            "buffer/list" => {
                runtime_validate_state_request_params(
                    request.params.as_deref(),
                    "buffer/list",
                    &[],
                )?;
                let buffers = self
                    .paste_buffers()
                    .list()
                    .into_iter()
                    .map(|buffer| {
                        let origin = buffer
                            .origin
                            .as_deref()
                            .map(|origin| format!(r#""{}""#, json_escape(origin)))
                            .unwrap_or_else(|| "null".to_string());
                        format!(
                            r#"{{"name":"{}","bytes":{},"created_at":{},"origin":{},"preview":"{}"}}"#,
                            json_escape(&buffer.name),
                            buffer.bytes,
                            buffer.created_at_unix_seconds,
                            origin,
                            json_escape(&buffer.preview)
                        )
                    })
                    .collect::<Vec<_>>();
                Ok(Some(format!(r#"{{"buffers":[{}]}}"#, buffers.join(","))))
            }
            "buffer/read" => {
                runtime_validate_state_request_params(
                    request.params.as_deref(),
                    "buffer/read",
                    &["name"],
                )?;
                let params = request.params.as_deref().ok_or_else(|| {
                    MezError::invalid_args("buffer/read requires a params object")
                })?;
                let name = runtime_json_string_field(params, "name")
                    .ok_or_else(|| MezError::invalid_args("buffer/read requires name"))?;
                let content = self.paste_buffers().get(&name).ok_or_else(|| {
                    MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "paste buffer not found",
                    )
                })?;
                Ok(Some(format!(
                    r#"{{"name":"{}","content":"{}","bytes":{}}}"#,
                    json_escape(&name),
                    json_escape(content),
                    content.len()
                )))
            }
            "frame/read" => Ok(Some(frame_read_json_with_context(
                &self.session,
                request.params.as_deref(),
                &self.terminal_frame_context(),
            )?)),
            _ => Ok(None),
        }
    }

    /// Runs the dispatch runtime event list request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_event_list_request(
        &self,
        request: &crate::control::JsonRpcRequest,
        caller_client_id: &mez_core::ids::ClientId,
    ) -> Result<String> {
        let event_log = self
            .control
            .event_log()
            .ok_or_else(|| MezError::invalid_state("runtime event log is not configured"))?;
        dispatch_event_list_request(request, &self.session, caller_client_id, event_log)
    }

    /// Resolves the caller current event audience and returns one bounded batch.
    pub(crate) fn authorized_event_wakeups(
        &self,
        caller_client_id: &mez_core::ids::ClientId,
        connection_id: &str,
        last_delivered_event_id: u64,
        limit_per_connection: usize,
    ) -> Result<Vec<crate::runtime::RuntimeEventWakeup>> {
        let audience = control_event_audience(&self.session, caller_client_id)?;
        let mut connections = crate::runtime::RuntimeEventConnectionTable::default();
        connections.attach(connection_id, audience, true, last_delivered_event_id)?;
        Ok(connections.wakeups(self.control.event_log(), limit_per_connection))
    }

    /// Runs the runtime session summary json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_session_summary_json(&self) -> String {
        let session = &self.session;
        let attached_client_count = session
            .clients()
            .iter()
            .filter(|client| client.state == ClientState::Attached)
            .count();
        let attached_primary_count = session.attached_primaries().count();
        let layout_owner_client_id = session.layout_owner_client_id().map(ToString::to_string);
        format!(
            r#"{{"id":"{}","version":2,"name":"{}","state":"{}","created_at":{},"last_attached_at":{},"window_count":{},"attached_client_count":{},"attached_primary_count":{},"max_attached_primaries":{},"accepts_primary":{},"layout_owner_client_id":{},"authoritative_size":{{"columns":{},"rows":{}}}}}"#,
            json_escape(session.id.as_str()),
            json_escape(&session.name),
            session_state_name(session.state),
            runtime_timestamp_json(self.session.created_at_unix_seconds()),
            runtime_optional_timestamp_json(self.session.last_attach_at_unix_seconds()),
            session.windows().len(),
            attached_client_count,
            attached_primary_count,
            mez_mux::session::MAX_ATTACHED_PRIMARY_CLIENTS,
            attached_primary_count < mez_mux::session::MAX_ATTACHED_PRIMARY_CLIENTS,
            runtime_optional_string(layout_owner_client_id.as_deref()),
            session.authoritative_size.columns,
            session.authoritative_size.rows
        )
    }

    /// Runs the runtime session state json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_session_state_json(
        &self,
        caller_client_id: &mez_core::ids::ClientId,
    ) -> Result<String> {
        let session = &self.session;
        let primary_client_ids = session
            .attached_primaries()
            .map(|client| format!(r#""{}""#, json_escape(client.id.as_str())))
            .collect::<Vec<_>>()
            .join(",");
        let attached_primary_count = session.attached_primaries().count();
        let layout_owner_client_id = session.layout_owner_client_id().map(ToString::to_string);
        let navigation = session.navigation(caller_client_id)?;
        let active_group_id = navigation.groups.active.as_ref().map(ToString::to_string);
        let active_window_id = navigation
            .groups
            .active
            .as_ref()
            .and_then(|group_id| navigation.windows_by_group.get(group_id))
            .and_then(|cursor| cursor.active.as_ref())
            .map(ToString::to_string);
        let active_pane_id = active_window_id
            .as_deref()
            .and_then(|window_id| {
                navigation
                    .panes_by_window
                    .iter()
                    .find(|(id, _)| id.as_str() == window_id)
            })
            .and_then(|(_, cursor)| cursor.active.as_ref())
            .map(ToString::to_string);
        let updated_at = self
            .session
            .last_attach_at_unix_seconds()
            .unwrap_or(self.session.created_at_unix_seconds());
        Ok(format!(
            r#"{{"id":"{}","version":2,"session_id":"{}","name":"{}","state":"{}","created_at":{},"updated_at":{},"primary_client_ids":[{}],"attached_primary_count":{},"max_attached_primaries":{},"layout_owner_client_id":{},"authoritative_size":{{"columns":{},"rows":{}}},"navigation":{{"active_group_id":{},"active_window_id":{},"active_pane_id":{},"revision":{}}},"windows":{},"window_count":{},"clients":{},"observers":{},"config_generation":{},"permission_summary":{}}}"#,
            json_escape(session.id.as_str()),
            json_escape(session.id.as_str()),
            json_escape(&session.name),
            session_state_name(session.state),
            runtime_timestamp_json(self.session.created_at_unix_seconds()),
            runtime_timestamp_json(updated_at),
            primary_client_ids,
            attached_primary_count,
            mez_mux::session::MAX_ATTACHED_PRIMARY_CLIENTS,
            runtime_optional_string(layout_owner_client_id.as_deref()),
            session.authoritative_size.columns,
            session.authoritative_size.rows,
            runtime_optional_string(active_group_id.as_deref()),
            runtime_optional_string(active_window_id.as_deref()),
            runtime_optional_string(active_pane_id.as_deref()),
            navigation.revision,
            self.runtime_windows_state_json(),
            session.windows().len(),
            self.runtime_clients_json(),
            observers_json(session),
            session.config_generation,
            self.runtime_permission_summary_json()
        ))
    }

    /// Runs the runtime permission summary json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_permission_summary_json(&self) -> String {
        let trusted_project = self
            .integration
            .config_layers()
            .iter()
            .any(|layer| layer.scope == ConfigScope::ProjectOverlay && layer.trusted);
        let trusted_directories = self
            .project_trust_store()
            .as_ref()
            .map(|store| {
                store
                    .records()
                    .filter(|record| record.state == TrustDecision::Trusted)
                    .map(|record| record.project_root.to_string_lossy().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let configured = self.configured_permissions();
        let effective = self
            .session
            .active_window()
            .map(|window| self.primary_path_scope_status(window.active_pane().id.as_str()));
        let effective_read_scopes = effective
            .as_ref()
            .map(|status| status.read_scopes.as_slice())
            .unwrap_or_default();
        let effective_write_scopes = effective
            .as_ref()
            .map(|status| status.write_scopes.as_slice())
            .unwrap_or_default();
        let effective_scope_provenance = effective
            .as_ref()
            .map_or("none", |status| status.provenance);
        let trusted_project_root = effective
            .as_ref()
            .and_then(|status| status.trusted_project_root.as_deref());
        let sandbox_restrictions = if matches!(
            configured.sandbox,
            crate::runtime::SandboxConfig::Bubblewrap(_)
        ) {
            crate::security::sandbox::BUBBLEWRAP_RESTRICTION_IDS
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let effective_sandbox = crate::security::sandbox::effective_sandbox_boundary(
            &configured.sandbox,
            self.permission_policy().approval_policy,
        );
        format!(
            r#"{{"preset":"{}","approval_policy":"{}","bypass_active":{},"sandbox":"{}","sandbox_effective":"{}","network_policy":"{}","trusted_project":{},"trusted_directories":{},"read_scopes":{},"write_scopes":{},"effective_scope_provenance":"{}","effective_read_scopes":{},"effective_write_scopes":{},"trusted_project_root":{},"sandbox_restrictions":{},"command_rule_generation":{}}}"#,
            runtime_permission_preset_name(self.permission_policy().preset),
            runtime_approval_policy_name(self.permission_policy().approval_policy),
            self.permission_policy().approval_bypass(),
            configured.sandbox.as_str(),
            effective_sandbox,
            configured.resources.network_policy.as_str(),
            trusted_project,
            runtime_string_array_json(&trusted_directories),
            runtime_string_array_json(&configured.resources.read_scopes),
            runtime_string_array_json(&configured.resources.write_scopes),
            effective_scope_provenance,
            runtime_string_array_json(effective_read_scopes),
            runtime_string_array_json(effective_write_scopes),
            runtime_optional_string(trusted_project_root),
            runtime_string_array_json(&sandbox_restrictions),
            self.permission_policy().rules().len()
        )
    }

    /// Runs the runtime clients json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_clients_json(&self) -> String {
        let clients = self
            .session
            .clients()
            .iter()
            .map(|client| self.runtime_client_state_json(client))
            .collect::<Vec<_>>();
        format!("[{}]", clients.join(","))
    }

    /// Runs the runtime client state json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_client_state_json(&self, client: &mez_mux::session::Client) -> String {
        let is_primary = self.session.is_attached_primary(&client.id);
        let attached_at = if is_primary {
            self.session
                .last_attach_at_unix_seconds()
                .or(client.attached_at_unix_seconds)
        } else {
            client.attached_at_unix_seconds
        };
        let last_seen_at = if is_primary {
            self.session
                .last_attach_at_unix_seconds()
                .or(client.last_seen_at_unix_seconds)
        } else {
            client.last_seen_at_unix_seconds
        };
        let terminal_size = client
            .terminal
            .as_ref()
            .map(|terminal| mez_mux::layout::Size {
                columns: terminal.columns,
                rows: terminal.rows,
            })
            .or_else(|| {
                (is_primary && client.interactive).then_some(self.session.authoritative_size)
            });
        let navigation_revision = client
            .navigation
            .as_ref()
            .map(|navigation| navigation.revision.to_string())
            .unwrap_or_else(|| "null".to_string());
        format!(
            r#"{{"id":"{}","version":2,"client_id":"{}","name":"{}","role":"{}","requested_role":"{}","state":"{}","attached_at":{},"last_seen_at":{},"descriptor":{{"name":"{}","interactive":{},"terminal":{}}},"terminal_size":{},"interactive":{},"navigation_revision":{}}}"#,
            json_escape(client.id.as_str()),
            json_escape(client.id.as_str()),
            json_escape(&client.name),
            runtime_client_role_name(client.role),
            runtime_client_requested_role_name(client.role),
            runtime_client_state_name(client.state),
            runtime_optional_timestamp_json(attached_at),
            runtime_optional_timestamp_json(last_seen_at),
            json_escape(&client.name),
            client.interactive,
            runtime_client_terminal_descriptor_json(terminal_size, self.terminal_term()),
            runtime_size_object_json(terminal_size),
            client.interactive,
            navigation_revision
        )
    }

    /// Runs the runtime windows state json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_windows_state_json(&self) -> String {
        let windows = self
            .session
            .windows()
            .iter()
            .map(|window| self.runtime_window_state_json(window))
            .collect::<Vec<_>>();
        format!("[{}]", windows.join(","))
    }

    /// Runs the runtime panes state json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_panes_state_json(&self) -> String {
        let panes = self
            .session
            .active_window()
            .map(|window| {
                window
                    .panes()
                    .iter()
                    .map(|pane| self.runtime_control_pane_state_json(window, pane))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        format!("[{}]", panes.join(","))
    }

    /// Runs the runtime panes state json for window ids operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_panes_state_json_for_window_ids(
        &self,
        window_ids: &[String],
    ) -> Result<String> {
        let panes = window_ids
            .iter()
            .map(|window_id| {
                self.session
                    .windows()
                    .iter()
                    .find(|window| window.id.as_str() == window_id)
                    .ok_or_else(|| {
                        MezError::new(crate::error::MezErrorKind::NotFound, "window not found")
                    })
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flat_map(|window| {
                window
                    .panes()
                    .iter()
                    .map(|pane| self.runtime_control_pane_state_json(window, pane))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        Ok(format!("[{}]", panes.join(",")))
    }

    /// Runs the runtime window state json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_window_state_json(&self, window: &mez_mux::layout::Window) -> String {
        let created_at = self
            .session
            .window_created_at_unix_seconds()
            .get(window.id.as_str())
            .copied()
            .unwrap_or(self.session.created_at_unix_seconds());
        let panes = window
            .panes()
            .iter()
            .map(|pane| self.runtime_control_pane_state_json(window, pane))
            .collect::<Vec<_>>();
        format!(
            r#"{{"id":"{}","version":1,"session_id":"{}","window_id":"{}","index":{},"name":"{}","active":{},"created_at":{},"size":{{"columns":{},"rows":{}}},"active_pane_id":{},"panes":[{}],"pane_count":{},"layout":{}}}"#,
            json_escape(window.id.as_str()),
            json_escape(self.session.id.as_str()),
            json_escape(window.id.as_str()),
            window.index,
            json_escape(&window.name),
            self.session
                .active_window()
                .is_some_and(|active| active.id == window.id),
            runtime_timestamp_json(created_at),
            window.size.columns,
            window.size.rows,
            runtime_optional_string(Some(window.active_pane().id.as_str())),
            panes.join(","),
            window.panes().len(),
            layout_state_json(window)
        )
    }

    /// Runs the runtime control pane state json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn runtime_control_pane_state_json(
        &self,
        window: &mez_mux::layout::Window,
        pane: &mez_mux::layout::Pane,
    ) -> String {
        let primary_pid = self.primary_pid_for_live_pane_process(pane.id.as_str());
        let exit_status = self
            .pane_exit_status(pane.id.as_str())
            .map(|status| status.to_json())
            .unwrap_or_else(|| "null".to_string());
        let process_state = if self.pane_is_closing(pane.id.as_str()) {
            "closing"
        } else if primary_pid.is_some() {
            "running"
        } else if pane.live {
            "starting"
        } else {
            "exited"
        };
        let alternate_screen_active = self
            .process_pane_screen(pane.id.as_str())
            .is_some_and(|screen| screen.alternate_screen_active());
        let current_working_directory = self
            .pane_current_working_directory(pane.id.as_str())
            .map(|path| path.to_string_lossy().to_string());
        let agent_id = self
            .agent_shell_store()
            .get(pane.id.as_str())
            .map(|_| format!("agent-{}", pane.id));
        let pty_size = self
            .pane_process_size_for(window, pane.id.as_str())
            .unwrap_or(pane.size);
        format!(
            r#"{{"id":"{}","version":1,"session_id":"{}","window_id":"{}","pane_id":"{}","index":{},"title":"{}","active":{},"size":{{"columns":{},"rows":{}}},"columns":{},"rows":{},"layout_size":{{"columns":{},"rows":{}}},"primary_pid":{},"process_state":"{}","exit_status":{},"current_working_directory":{},"terminal_profile":"{}","history_limit":{},"alternate_screen_active":{},"readiness_state":"{}","agent_id":{},"live":{}}}"#,
            json_escape(pane.id.as_str()),
            json_escape(self.session.id.as_str()),
            json_escape(window.id.as_str()),
            json_escape(pane.id.as_str()),
            pane.index,
            json_escape(&pane.title),
            pane.active,
            pty_size.columns,
            pty_size.rows,
            pty_size.columns,
            pty_size.rows,
            pane.size.columns,
            pane.size.rows,
            primary_pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "null".to_string()),
            process_state,
            exit_status,
            runtime_optional_string(current_working_directory.as_deref()),
            json_escape(self.terminal_term()),
            self.terminal_history_limit(),
            alternate_screen_active,
            runtime_pane_readiness_state_name(self.pane_readiness_state(pane.id.as_str())),
            runtime_optional_string(agent_id.as_deref()),
            pane.live
        )
    }

    /// Runs the runtime started pane result json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_started_pane_result_json(
        &self,
        started: &PaneProcessStart,
        include_window: bool,
    ) -> Result<String> {
        let (window, pane) = runtime_pane_by_id(&self.session, &started.pane_id)?;
        let pane_json = self.runtime_control_pane_state_json(window, pane);
        let layout_json = layout_state_json(window);
        if include_window {
            let window_json = self.runtime_window_state_json(window);
            Ok(format!(
                r#"{{"window":{window_json},"pane":{pane_json},"layout":{layout_json}}}"#
            ))
        } else {
            Ok(format!(r#"{{"pane":{pane_json},"layout":{layout_json}}}"#))
        }
    }

    /// Runs the runtime pane resize result json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_pane_resize_result_json(
        &self,
        update: &PaneResizeUpdate,
    ) -> Result<String> {
        let (window, pane) = runtime_pane_by_id(&self.session, &update.pane_id)?;
        Ok(format!(
            r#"{{"pane":{},"layout":{}}}"#,
            self.runtime_control_pane_state_json(window, pane),
            layout_state_json(window)
        ))
    }

    /// Runs the runtime active layout state json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_active_layout_state_json(&self) -> Result<String> {
        let window = self
            .session
            .active_window()
            .ok_or_else(|| MezError::invalid_state("session has no active window"))?;
        Ok(layout_state_json(window))
    }

    /// Builds the live pane-to-model-profile view used by runtime `agent/list`.
    ///
    /// The latest turn model profile is authoritative when a turn exists for a
    /// pane. Otherwise the currently selected runtime override/default profile
    /// is used when it can be resolved, with the generic serializer's `default`
    /// fallback preserved only for non-runtime or unconfigured contexts.
    pub(super) fn runtime_agent_model_profiles_by_pane(
        &self,
    ) -> std::collections::BTreeMap<String, String> {
        let mut profiles = std::collections::BTreeMap::new();
        for window in self.session.windows() {
            for pane in window.panes() {
                let pane_id = pane.id.to_string();
                let latest_turn_profile = self
                    .agent_turn_ledger()
                    .turns()
                    .iter()
                    .rev()
                    .find(|turn| turn.pane_id == pane_id)
                    .map(|turn| turn.model_profile.clone());
                let profile = latest_turn_profile.or_else(|| {
                    let agent_id = format!("agent-{pane_id}");
                    self.active_model_profile_for_pane(&pane_id, &agent_id, None)
                        .ok()
                        .map(|(profile_name, _profile)| profile_name)
                });
                if let Some(profile) = profile {
                    profiles.insert(pane_id, profile);
                }
            }
        }
        profiles
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol::event::{EventAudience, EventKind};
    use crate::security::audit::{AuditConfig, AuditLog};
    use crate::test_support::runtime::RuntimeServiceFixture;
    use mez_mux::layout::Size;
    use mez_mux::session::{ClientRole, ClientState, ObserverDecisionState};
    use std::path::PathBuf;

    #[test]
    fn exact_primary_event_wakeups_keep_private_visibility_and_independent_cursors() {
        let mut service = RuntimeServiceFixture::new().build();
        let first = service
            .attach_primary("first", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        let second = service
            .attach_primary("second", true, Size::new(100, 30).unwrap(), 121)
            .unwrap();
        service
            .append_primary_lifecycle_event(
                EventKind::Diagnostic,
                r#"{"scope":"shared-first"}"#.to_string(),
            )
            .unwrap();
        let first_cursor = service.event_log().unwrap().latest_event_id();
        service
            .append_primary_client_event(
                &first,
                EventKind::Diagnostic,
                r#"{"scope":"private-first"}"#.to_string(),
            )
            .unwrap();
        service
            .append_primary_lifecycle_event(
                EventKind::Diagnostic,
                r#"{"scope":"shared-second"}"#.to_string(),
            )
            .unwrap();

        let first_payloads = service
            .authorized_event_wakeups(&first, "first-events", 0, 8)
            .unwrap()
            .into_iter()
            .flat_map(|wakeup| wakeup.events)
            .map(|event| event.payload)
            .collect::<Vec<_>>();
        let second_payloads = service
            .authorized_event_wakeups(&second, "second-events", first_cursor, 8)
            .unwrap()
            .into_iter()
            .flat_map(|wakeup| wakeup.events)
            .map(|event| event.payload)
            .collect::<Vec<_>>();

        assert!(
            first_payloads
                .iter()
                .any(|payload| payload.contains("shared-first"))
        );
        assert!(
            first_payloads
                .iter()
                .any(|payload| payload.contains("private-first"))
        );
        assert!(
            first_payloads
                .iter()
                .any(|payload| payload.contains("shared-second"))
        );
        assert!(
            !second_payloads
                .iter()
                .any(|payload| payload.contains("shared-first"))
        );
        assert!(
            !second_payloads
                .iter()
                .any(|payload| payload.contains("private-first"))
        );
        assert!(
            second_payloads
                .iter()
                .any(|payload| payload.contains("shared-second"))
        );
    }

    #[test]
    fn authorized_event_wakeups_revalidates_observer_approval_and_revocation() {
        let mut service = RuntimeServiceFixture::new().build();
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        let (observer_client, observer_request) =
            service.session.request_observer("remote-observer");
        service
            .append_lifecycle_event(
                EventKind::PaneChanged,
                r#"{"phase":"before-approval"}"#.to_string(),
            )
            .unwrap();

        let pending = service
            .authorized_event_wakeups(&observer_client, "remote-events", 0, 8)
            .expect_err("pending observers must not receive a remote event stream");
        assert!(pending.message().contains("pending observer event streams"));

        service
            .approve_observer_with_runtime_cutoff(&primary, observer_request.as_str())
            .unwrap();
        service
            .append_lifecycle_event(
                EventKind::PaneChanged,
                r#"{"phase":"after-approval"}"#.to_string(),
            )
            .unwrap();
        let wakeups = service
            .authorized_event_wakeups(&observer_client, "remote-events", 0, 8)
            .unwrap();
        let payloads = wakeups
            .iter()
            .flat_map(|wakeup| wakeup.events.iter())
            .map(|event| event.payload.as_str())
            .collect::<Vec<_>>();
        assert!(
            payloads
                .iter()
                .any(|payload| payload.contains("after-approval"))
        );
        assert!(
            !payloads
                .iter()
                .any(|payload| payload.contains("before-approval"))
        );

        service
            .session
            .revoke_observer_client(&primary, observer_client.as_str())
            .unwrap();
        let revoked = service
            .authorized_event_wakeups(&observer_client, "remote-events", 0, 8)
            .expect_err("revoked observers must stop receiving remote events");
        assert!(revoked.message().contains("detached or revoked"));
    }

    /// Verifies observer-management decisions remain visible only to primaries.
    ///
    /// An approved observer may receive later session-view events, but must not
    /// learn another observer's request or client identifiers through approve,
    /// reject, or revoke notifications emitted after its visibility marker.
    #[test]
    fn observer_decisions_do_not_leak_to_other_approved_observers() {
        let mut service = RuntimeServiceFixture::new().build();
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        let (first_client, first_request) = service.session.request_observer("first-observer");
        let first_approval = service.dispatch_runtime_control_body(
            &format!(
                r#"{{"jsonrpc":"2.0","id":"approve-first","method":"observer/approve","params":{{"observer_request_id":"{first_request}","idempotency_key":"approve-first"}}}}"#
            ),
            &primary,
        );
        assert!(
            first_approval.contains(r#""state":"approved""#),
            "{first_approval}"
        );

        let (second_client, second_request) = service.session.request_observer("second-observer");
        let second_approval = service.dispatch_runtime_control_body(
            &format!(
                r#"{{"jsonrpc":"2.0","id":"approve-second","method":"observer/approve","params":{{"observer_request_id":"{second_request}","idempotency_key":"approve-second"}}}}"#
            ),
            &primary,
        );
        assert!(
            second_approval.contains(r#""state":"approved""#),
            "{second_approval}"
        );
        let second_revocation = service.dispatch_runtime_control_body(
            &format!(
                r#"{{"jsonrpc":"2.0","id":"revoke-second","method":"observer/revoke","params":{{"client_id":"{second_client}","reason":"done","idempotency_key":"revoke-second"}}}}"#
            ),
            &primary,
        );
        assert!(
            second_revocation.contains(r#""revoked":true"#),
            "{second_revocation}"
        );

        let (_third_client, third_request) = service.session.request_observer("third-observer");
        let third_rejection = service.dispatch_runtime_control_body(
            &format!(
                r#"{{"jsonrpc":"2.0","id":"reject-third","method":"observer/reject","params":{{"observer_request_id":"{third_request}","reason":"denied","idempotency_key":"reject-third"}}}}"#
            ),
            &primary,
        );
        assert!(
            third_rejection.contains(r#""state":"rejected""#),
            "{third_rejection}"
        );

        let observer_events = service
            .authorized_event_wakeups(&first_client, "first-observer-events", 0, 64)
            .unwrap()
            .into_iter()
            .flat_map(|wakeup| wakeup.events)
            .collect::<Vec<_>>();
        assert!(!observer_events.iter().any(|event| {
            event.kind == EventKind::ObserverDecided
                && (event.payload.contains(second_request.as_str())
                    || event.payload.contains(second_client.as_str())
                    || event.payload.contains(third_request.as_str()))
        }));

        let primary_events = service
            .event_log()
            .unwrap()
            .replay_for(&EventAudience::AllPrimaries);
        assert!(primary_events.iter().any(|event| {
            event.kind == EventKind::ObserverDecided
                && event.payload.contains(second_request.as_str())
                && event.payload.contains(r#""decision":"approved""#)
        }));
        assert!(primary_events.iter().any(|event| {
            event.kind == EventKind::ObserverDecided
                && event.payload.contains(second_client.as_str())
                && event.payload.contains(r#""decision":"revoked""#)
        }));
        assert!(primary_events.iter().any(|event| {
            event.kind == EventKind::ObserverDecided
                && event.payload.contains(third_request.as_str())
                && event.payload.contains(r#""decision":"rejected""#)
        }));
    }

    /// Verifies mandatory audit failures roll back every observer decision.
    ///
    /// Authority, visibility markers, client state, event retention, and the
    /// cached retry response must agree when required audit publication fails.
    /// Approve, reject, and revoke are exercised independently so no terminal
    /// transition can survive behind an RPC error.
    #[test]
    fn observer_decision_audit_failures_leave_authority_and_events_unchanged() {
        let mut service = RuntimeServiceFixture::new().build();
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        let (approve_client, approve_request) = service.session.request_observer("approve");
        let (reject_client, reject_request) = service.session.request_observer("reject");
        let (revoke_client, revoke_request) = service.session.request_observer("revoke");
        service
            .approve_observer_with_runtime_cutoff(&primary, revoke_request.as_str())
            .unwrap();
        service.set_audit_log(AuditLog::new(AuditConfig {
            enabled: false,
            path: PathBuf::from("/tmp/unused-observer-decision-audit.jsonl"),
            hash_chain: false,
            required: true,
        }));
        let event_count_before = service.event_log().unwrap().len();

        let approve = format!(
            r#"{{"jsonrpc":"2.0","id":"approve","method":"observer/approve","params":{{"observer_request_id":"{approve_request}","idempotency_key":"audit-fail-approve"}}}}"#
        );
        let approve_response = service.dispatch_runtime_control_body(&approve, &primary);
        assert!(approve_response.contains(r#""mezzanine_code":"forbidden""#));
        assert_eq!(
            service.dispatch_runtime_control_body(&approve, &primary),
            approve_response
        );

        let reject = format!(
            r#"{{"jsonrpc":"2.0","id":"reject","method":"observer/reject","params":{{"observer_request_id":"{reject_request}","reason":"denied","idempotency_key":"audit-fail-reject"}}}}"#
        );
        let reject_response = service.dispatch_runtime_control_body(&reject, &primary);
        assert!(reject_response.contains(r#""mezzanine_code":"forbidden""#));
        assert_eq!(
            service.dispatch_runtime_control_body(&reject, &primary),
            reject_response
        );

        let revoke = format!(
            r#"{{"jsonrpc":"2.0","id":"revoke","method":"observer/revoke","params":{{"client_id":"{revoke_client}","reason":"done","idempotency_key":"audit-fail-revoke"}}}}"#
        );
        let revoke_response = service.dispatch_runtime_control_body(&revoke, &primary);
        assert!(revoke_response.contains(r#""mezzanine_code":"forbidden""#));
        assert_eq!(
            service.dispatch_runtime_control_body(&revoke, &primary),
            revoke_response
        );

        assert_eq!(service.event_log().unwrap().len(), event_count_before);
        for request_id in [&approve_request, &reject_request] {
            let observer = service
                .session()
                .observers()
                .iter()
                .find(|observer| observer.id == *request_id)
                .unwrap();
            assert_eq!(observer.state, ObserverDecisionState::Pending);
            assert_eq!(observer.decided_at_unix_seconds, None);
            assert_eq!(observer.decided_by_client_id, None);
            assert_eq!(observer.visible_from_event_id, None);
        }
        for client_id in [&approve_client, &reject_client] {
            let client = service
                .session()
                .clients()
                .iter()
                .find(|client| client.id == *client_id)
                .unwrap();
            assert_eq!(client.role, ClientRole::PendingObserver);
            assert_eq!(client.state, ClientState::Pending);
        }
        let revoked_observer = service
            .session()
            .observers()
            .iter()
            .find(|observer| observer.id == revoke_request)
            .unwrap();
        assert_eq!(revoked_observer.state, ObserverDecisionState::Approved);
        let revoked_client = service
            .session()
            .clients()
            .iter()
            .find(|client| client.id == revoke_client)
            .unwrap();
        assert_eq!(revoked_client.role, ClientRole::Observer);
        assert_eq!(revoked_client.state, ClientState::Attached);
    }
}
