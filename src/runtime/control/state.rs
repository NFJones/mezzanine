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
    runtime_approval_policy_name, runtime_pane_by_id, runtime_pane_readiness_state_name,
    runtime_permission_preset_name, runtime_string_array_json, session_state_name,
    state_request_pane_list_window_ids, state_request_session_target_matches,
};
use super::protocol::{
    runtime_client_requested_role_name, runtime_client_role_name, runtime_client_state_name,
    runtime_client_terminal_descriptor_json, runtime_optional_string,
    runtime_optional_timestamp_json, runtime_size_object_json, runtime_timestamp_json,
    runtime_validate_state_request_params,
};

impl RuntimeSessionService {
    /// Runs the dispatch runtime read only state request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_read_only_state_request(
        &self,
        request: &crate::control::JsonRpcRequest,
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
                    self.runtime_session_state_json()
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

    /// Runs the runtime session summary json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_session_summary_json(&self) -> String {
        let session = &self.session;
        let active_window_id = session.active_window().map(|window| window.id.to_string());
        let attached_client_count = session
            .clients()
            .iter()
            .filter(|client| client.state == ClientState::Attached)
            .count();
        format!(
            r#"{{"id":"{}","version":1,"name":"{}","state":"{}","created_at":{},"last_attached_at":{},"window_count":{},"attached_client_count":{},"has_primary":{},"active_window_id":{}}}"#,
            json_escape(session.id.as_str()),
            json_escape(&session.name),
            session_state_name(session.state),
            runtime_timestamp_json(self.session.created_at_unix_seconds()),
            runtime_optional_timestamp_json(self.session.last_attach_at_unix_seconds()),
            session.windows().len(),
            attached_client_count,
            session.primary_client_id().is_some(),
            runtime_optional_string(active_window_id.as_deref())
        )
    }

    /// Runs the runtime session state json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_session_state_json(&self) -> String {
        let session = &self.session;
        let primary_client_id = session
            .primary_client_id()
            .map(|client_id| client_id.to_string());
        let active_window_id = session.active_window().map(|window| window.id.to_string());
        let updated_at = self
            .session
            .last_attach_at_unix_seconds()
            .unwrap_or(self.session.created_at_unix_seconds());
        format!(
            r#"{{"id":"{}","version":1,"session_id":"{}","name":"{}","state":"{}","created_at":{},"updated_at":{},"primary_client_id":{},"authoritative_size":{{"columns":{},"rows":{}}},"active_window_id":{},"windows":{},"window_count":{},"clients":{},"observers":{},"config_generation":{},"permission_summary":{}}}"#,
            json_escape(session.id.as_str()),
            json_escape(session.id.as_str()),
            json_escape(&session.name),
            session_state_name(session.state),
            runtime_timestamp_json(self.session.created_at_unix_seconds()),
            runtime_timestamp_json(updated_at),
            runtime_optional_string(primary_client_id.as_deref()),
            session.authoritative_size.columns,
            session.authoritative_size.rows,
            runtime_optional_string(active_window_id.as_deref()),
            self.runtime_windows_state_json(),
            session.windows().len(),
            self.runtime_clients_json(),
            observers_json(session),
            session.config_generation,
            self.runtime_permission_summary_json()
        )
    }

    /// Runs the runtime permission summary json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_permission_summary_json(&self) -> String {
        let trusted_project = self
            .config_layers
            .iter()
            .any(|layer| layer.scope == ConfigScope::ProjectOverlay && layer.trusted);
        let trusted_directories = self
            .project_trust_store
            .as_ref()
            .map(|store| {
                store
                    .records()
                    .filter(|record| record.state == TrustDecision::Trusted)
                    .map(|record| record.project_root.to_string_lossy().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        format!(
            r#"{{"preset":"{}","approval_policy":"{}","bypass_active":{},"trusted_project":{},"trusted_directories":{},"read_scopes":[],"write_scopes":[],"command_rule_generation":{}}}"#,
            runtime_permission_preset_name(self.permission_policy.preset),
            runtime_approval_policy_name(self.permission_policy.approval_policy),
            self.permission_policy.approval_bypass(),
            trusted_project,
            runtime_string_array_json(&trusted_directories),
            self.permission_policy.rules().len()
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
        let is_primary = self
            .session
            .primary_client_id()
            .is_some_and(|primary| primary == &client.id);
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
        let terminal_size =
            (is_primary && client.interactive).then_some(self.session.authoritative_size);
        format!(
            r#"{{"id":"{}","version":1,"client_id":"{}","name":"{}","role":"{}","requested_role":"{}","state":"{}","attached_at":{},"last_seen_at":{},"descriptor":{{"name":"{}","interactive":{},"terminal":{}}},"terminal_size":{},"interactive":{}}}"#,
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
            client.interactive
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
    pub(in crate::runtime) fn runtime_control_pane_state_json(
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
            .pane_screen(pane.id.as_str())
            .is_some_and(|screen| screen.alternate_screen_active());
        let current_working_directory = self
            .pane_current_working_directory(pane.id.as_str())
            .map(|path| path.to_string_lossy().to_string());
        let agent_id = self
            .agent_shell_store()
            .get(pane.id.as_str())
            .map(|_| format!("agent-{}", pane.id));
        format!(
            r#"{{"id":"{}","version":1,"session_id":"{}","window_id":"{}","pane_id":"{}","index":{},"title":"{}","active":{},"size":{{"columns":{},"rows":{}}},"columns":{},"rows":{},"primary_pid":{},"process_state":"{}","exit_status":{},"current_working_directory":{},"terminal_profile":"{}","history_limit":{},"alternate_screen_active":{},"readiness_state":"{}","agent_id":{},"live":{}}}"#,
            json_escape(pane.id.as_str()),
            json_escape(self.session.id.as_str()),
            json_escape(window.id.as_str()),
            json_escape(pane.id.as_str()),
            pane.index,
            json_escape(&pane.title),
            pane.active,
            pane.size.columns,
            pane.size.rows,
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
