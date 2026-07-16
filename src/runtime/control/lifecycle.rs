//! Runtime control helpers for lifecycle initialization and snapshot resume.
//!
//! This module owns runtime-visible side effects for successful control
//! initialization, observer request publication, snapshot operation auditing,
//! and snapshot resume application. The parent control module keeps request
//! routing while this module keeps lifecycle mutation and rollback details out
//! of the main control facade.

use super::*;

impl RuntimeSessionService {
    /// Runs the apply runtime initialize side effects operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_runtime_initialize_side_effects(
        &mut self,
        request: &crate::control::JsonRpcRequest,
        primary_before: Option<&mez_core::ids::ClientId>,
        observer_count_before: usize,
    ) -> Result<()> {
        if runtime_initialize_requested_observer(request) {
            self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
            return self.apply_runtime_observer_initialize_side_effects(observer_count_before);
        }
        if !runtime_initialize_requested_primary(request) {
            self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
            return Ok(());
        }
        let Some(primary_after) = self.session.primary_client_id().cloned() else {
            self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
            return Ok(());
        };
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
        if let Some(size) = runtime_initialize_terminal_size(request) {
            self.session
                .resize_authoritative_terminal(&primary_after, size)?;
            self.sync_tracked_pty_sizes()?;
        }
        if primary_before == Some(&primary_after) {
            return Ok(());
        }
        self.last_attach_at_unix_seconds = Some(current_unix_seconds());
        self.append_lifecycle_event(
            EventKind::ClientAttached,
            format!(
                r#"{{"client_id":"{}","role":"primary","columns":{},"rows":{}}}"#,
                json_escape(primary_after.as_str()),
                self.session.authoritative_size.columns,
                self.session.authoritative_size.rows
            ),
        )
    }

    /// Publishes runtime-visible side effects for a successful observer request.
    fn apply_runtime_observer_initialize_side_effects(
        &mut self,
        observer_count_before: usize,
    ) -> Result<()> {
        let Some(observer) = self.session.observers().get(observer_count_before).cloned() else {
            return Ok(());
        };
        let observer_id = observer.id.to_string();
        let payload = format!(
            r#"{{"observer_id":"{}","client_id":"{}","state":"pending","descriptor":"{}","interactive":{},"terminal":"{}"}}"#,
            json_escape(&observer_id),
            json_escape(observer.client_id.as_str()),
            json_escape(&observer.descriptor_name),
            observer.descriptor_interactive,
            json_escape(
                &observer
                    .descriptor_terminal
                    .as_ref()
                    .map(|terminal| format!(
                        "{}x{} {}",
                        terminal.columns, terminal.rows, terminal.term
                    ))
                    .unwrap_or_else(|| "none".to_string())
            )
        );
        self.append_observer_requested_lifecycle_event(observer_id.as_str(), payload)?;
        let active_pane_id = self.active_pane_id()?;
        self.append_agent_status_text_to_terminal_buffer(
            &active_pane_id,
            &format!(
                "observer request {} from {} is pending",
                observer.id, observer.descriptor_name
            ),
        )
    }

    /// Appends an observer-request event with pending-observer visibility.
    fn append_observer_requested_lifecycle_event(
        &mut self,
        observer_id: &str,
        payload: String,
    ) -> Result<()> {
        if let Some(event_log) = &mut self.event_log {
            event_log.append(
                EventKind::ObserverRequested,
                Some(self.session.id.to_string()),
                EventVisibility::PendingObserverRequest(observer_id.to_string()),
                payload.clone(),
            )?;
        }
        if let Some(hook_event) =
            runtime_hook_event_for_lifecycle(EventKind::ObserverRequested, &payload)
        {
            self.run_configured_completed_hooks(hook_event, &payload)?;
        }
        Ok(())
    }

    /// Runs the append runtime snapshot audit operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn append_runtime_snapshot_audit(
        &mut self,
        request: &crate::control::JsonRpcRequest,
        caller_client_id: &mez_core::ids::ClientId,
        outcome: &str,
    ) -> Result<()> {
        let Some(operation) = request.method.strip_prefix("snapshot/") else {
            return Ok(());
        };
        if !matches!(operation, "create" | "resume" | "delete") {
            return Ok(());
        }
        let Some(audit_log) = self.audit_log.as_mut() else {
            return Ok(());
        };
        let params = request.params.as_deref().unwrap_or("{}");
        let snapshot_id = match operation {
            "create" => runtime_json_string_field(params, "idempotency_key")
                .map(|key| snapshot_id_for_idempotency_key(&self.session, &key))
                .unwrap_or_else(|| "unknown".to_string()),
            _ => runtime_json_string_field(params, "snapshot_id")
                .unwrap_or_else(|| "unknown".to_string()),
        };
        let mut record = AuditRecord::snapshot_operation(
            self.session.id.to_string(),
            AuditActor {
                kind: "client".to_string(),
                id: caller_client_id.to_string(),
            },
            snapshot_id,
            operation,
            outcome,
        );
        if let Some(name) = runtime_json_string_field(params, "name") {
            record = record.with_metadata("name", name);
        }
        let _ = audit_log.append(record)?;
        Ok(())
    }

    /// Runs the dispatch runtime snapshot resume for connection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn dispatch_runtime_snapshot_resume_for_connection(
        &mut self,
        request: &crate::control::JsonRpcRequest,
        snapshots: &SnapshotRepository,
        connection: &mut ControlConnectionState,
        caller_client_id: &mez_core::ids::ClientId,
    ) -> Result<String> {
        let params = request
            .params
            .as_deref()
            .ok_or_else(|| MezError::invalid_args("snapshot/resume requires a params object"))?;
        let _idempotency_key = runtime_json_string_field(params, "idempotency_key")
            .ok_or_else(|| MezError::invalid_args("snapshot/resume requires idempotency_key"))?;
        let snapshot_id = runtime_json_string_field(params, "snapshot_id")
            .ok_or_else(|| MezError::invalid_args("snapshot/resume requires snapshot_id"))?;
        let payload = snapshots.inspect_payload(&snapshot_id)?;
        self.require_snapshot_resume_hooks_allow(&payload)?;
        let _ = connection;
        let resume_plan = payload.resume_plan();
        self.apply_runtime_snapshot_resume_for_connection(
            &snapshot_id,
            payload,
            resume_plan,
            caller_client_id,
        )
    }

    /// Runs the dispatch runtime snapshot resume for connection async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) async fn dispatch_runtime_snapshot_resume_for_connection_async(
        &mut self,
        request: &crate::control::JsonRpcRequest,
        snapshots: &SnapshotRepository,
        connection: &mut ControlConnectionState,
        caller_client_id: &mez_core::ids::ClientId,
    ) -> Result<String> {
        let _ = connection;
        let params = request
            .params
            .as_deref()
            .ok_or_else(|| MezError::invalid_args("snapshot/resume requires a params object"))?;
        let _idempotency_key = runtime_json_string_field(params, "idempotency_key")
            .ok_or_else(|| MezError::invalid_args("snapshot/resume requires idempotency_key"))?;
        let snapshot_id = runtime_json_string_field(params, "snapshot_id")
            .ok_or_else(|| MezError::invalid_args("snapshot/resume requires snapshot_id"))?;
        let payload = snapshots.inspect_payload_async(&snapshot_id).await?;
        self.require_snapshot_resume_hooks_allow(&payload)?;
        let resume_plan = payload.resume_plan();
        self.apply_runtime_snapshot_resume_for_connection(
            &snapshot_id,
            payload,
            resume_plan,
            caller_client_id,
        )
    }

    /// Runs the apply runtime snapshot resume for connection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn apply_runtime_snapshot_resume_for_connection(
        &mut self,
        snapshot_id: &str,
        payload: crate::snapshot::SessionSnapshotPayload,
        resume_plan: crate::snapshot::LayoutLoadPlan,
        caller_client_id: &mez_core::ids::ClientId,
    ) -> Result<String> {
        let previous_session = self.session.clone();
        let previous_window_created_at_unix_seconds = self.window_created_at_unix_seconds.clone();
        let mut prepared_session = self.session.clone();
        prepared_session
            .replace_layout_from_restore_input(crate::snapshot::session_restore_input(&payload)?)?;
        let replaced_pane_ids = self
            .session
            .windows()
            .iter()
            .flat_map(|window| window.panes().iter().map(|pane| pane.id.to_string()))
            .collect::<Vec<_>>();
        let replaced_pane_id_refs = replaced_pane_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        self.stop_active_pane_pipes_for(replaced_pane_id_refs.as_slice());
        let terminated_panes =
            self.terminate_runtime_pane_processes(replaced_pane_id_refs.iter().copied(), true)?;
        for pane_id in &replaced_pane_ids {
            self.cleanup_removed_pane_runtime_state(pane_id);
        }
        self.active_copy_modes_mut().clear();
        self.clear_pane_screens();
        self.clear_pane_transaction_parsers();
        self.clear_pane_process_lifecycle_tracking();
        self.pane_transcript_refs.clear();
        self.agent_shell_store = AgentShellStore::default();
        self.agent_turn_ledger = AgentTurnLedger::new(false);
        self.agent_turn_contexts.clear();
        self.agent_turn_executions.clear();
        self.agent_turn_pending_steering.clear();
        self.agent_turn_failure_feedback_attempts.clear();
        self.agent_turn_shell_dispatch_history.clear();
        self.agent_turn_network_action_history.clear();
        self.clear_agent_session_artifacts();
        self.clear_agent_prompt_inputs();
        self.agent_turn_model_profiles.clear();
        self.pending_agent_provider_tasks.clear();
        self.claimed_agent_provider_tasks.clear();
        self.agent_scheduler = AgentScheduler::with_default_limit();
        self.subagent_task_routes.clear();
        self.joined_subagent_dependencies.clear();
        self.subagent_lineage.clear();
        self.subagent_window_ids.clear();
        self.pending_terminal_subagent_pane_closes.clear();
        self.subagent_scope_declarations.clear();
        self.subagent_scopes = ScopeRegistry::default();
        self.blocked_agent_approval_refs.clear();
        self.clear_all_shell_transaction_state();
        self.clear_pane_readiness_state_and_overrides();
        self.blocked_approvals = Default::default();
        self.session_approvals = Default::default();

        self.session = prepared_session;
        self.session.state = mez_mux::session::SessionState::Running;
        let restored_at = current_unix_seconds();
        self.window_created_at_unix_seconds = self
            .session
            .windows()
            .iter()
            .map(|window| {
                (
                    window.id.to_string(),
                    window.created_at_unix_seconds.unwrap_or(restored_at),
                )
            })
            .collect();
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
        let restarted_panes = match self.restart_restored_pane_processes(None) {
            Ok(starts) => {
                self.sync_tracked_pty_sizes()?;
                starts.len()
            }
            Err(error) => {
                let restored_pane_ids = self.tracked_runtime_pane_process_ids();
                let restored_pane_id_refs = restored_pane_ids
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                let _ = self
                    .terminate_runtime_pane_processes(restored_pane_id_refs.iter().copied(), true);
                for pane_id in &restored_pane_ids {
                    self.cleanup_removed_pane_runtime_state(pane_id);
                }
                self.session = previous_session;
                self.window_created_at_unix_seconds = previous_window_created_at_unix_seconds;
                for pane_id in &replaced_pane_ids {
                    let _ = self.session.set_pane_live_state(pane_id, false);
                }
                self.lifecycle_state =
                    RuntimeLifecycleState::from_session_state(self.session.state);
                let _ = self.restart_restored_pane_processes(None);
                return Err(MezError::invalid_state(format!(
                    "load-layout failed to start restored pane shells and rolled back to the previous layout: {error}"
                )));
            }
        };
        self.append_lifecycle_event(
            EventKind::SnapshotChanged,
            format!(
                r#"{{"method":"snapshot/resume","snapshot_id":"{}","resumed":true,"terminated_panes":{},"restarted_panes":{},"seeded_terminal_screens":0,"interrupted_agent_turns":0}}"#,
                json_escape(snapshot_id),
                terminated_panes,
                restarted_panes
            ),
        )?;
        Ok(format!(
            r#"{{"session":{},"resumed":true,"resume_plan":{},"limitations":{},"terminated_panes":{},"restarted_panes":{},"seeded_terminal_screens":0,"interrupted_agent_turns":0,"primary_client_id":"{}"}}"#,
            self.runtime_session_state_json(),
            runtime_snapshot_resume_plan_json(&resume_plan),
            runtime_string_array_json(&resume_plan.limitations),
            terminated_panes,
            restarted_panes,
            json_escape(caller_client_id.as_str())
        ))
    }
}
