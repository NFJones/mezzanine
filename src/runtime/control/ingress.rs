//! Runtime control ingress and snapshot async dispatch helpers.
//!
//! This module owns the frame-decoding loops used by the runtime control
//! adapter plus the small snapshot request handoff API used when repository I/O
//! must run outside the actor turn. Keeping these entry points separate from
//! the main dispatcher keeps `control::mod` focused on method dispatch and live
//! state shaping.

use super::super::{
    ControlConnectionState, EventKind, MezError, Result, RuntimeLifecycleState,
    RuntimeSessionService, RuntimeSnapshotControlAsyncOutcome, RuntimeSnapshotControlAsyncWork,
    RuntimeSnapshotControlAsyncWorkKind, RuntimeSnapshotOwnedCreationContext, RuntimeTransition,
    SnapshotRepository, decode_control_frame, encode_control_body, parse_json_rpc_request,
    runtime_json_rpc_error,
};
use super::protocol::runtime_snapshot_id_from_request;
use crate::control::{authorize_control_request, validate_control_method_params_schema};

impl RuntimeSessionService {
    /// Runs the handle control input operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn handle_control_input(
        &mut self,
        input: &[u8],
        max_content_length: usize,
    ) -> Result<(Vec<u8>, usize)> {
        self.require_live()?;
        let primary_client_id = self.session.primary_client_id().cloned().ok_or_else(|| {
            MezError::invalid_state("control service requires an attached primary")
        })?;
        let mut offset = 0usize;
        let mut output = Vec::new();
        while offset < input.len() {
            let (body, consumed) = decode_control_frame(&input[offset..], max_content_length)?;
            let response = self.dispatch_runtime_control_body(&body, &primary_client_id);
            output.extend_from_slice(&encode_control_body(&response));
            offset += consumed;
        }
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
        if !self.external_effects_use_adapter() {
            self.persist_or_defer_registry_update()?;
        }
        Ok((output, offset))
    }

    /// Runs the handle control input for connection operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn handle_control_input_for_connection(
        &mut self,
        input: &[u8],
        max_content_length: usize,
        connection: &mut ControlConnectionState,
    ) -> Result<(Vec<u8>, usize)> {
        self.require_live()?;
        let mut offset = 0usize;
        let mut output = Vec::new();
        while offset < input.len() {
            let (body, consumed) = decode_control_frame(&input[offset..], max_content_length)?;
            let response = self.dispatch_runtime_control_body_for_connection(&body, connection);
            output.extend_from_slice(&encode_control_body(&response));
            offset += consumed;
        }
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
        if !self.external_effects_use_adapter() {
            self.persist_or_defer_registry_update()?;
        }
        Ok((output, offset))
    }

    /// Runs the handle control input for connection with snapshots operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn handle_control_input_for_connection_with_snapshots(
        &mut self,
        input: &[u8],
        max_content_length: usize,
        connection: &mut ControlConnectionState,
        snapshots: &SnapshotRepository,
    ) -> Result<(Vec<u8>, usize)> {
        self.require_live()?;
        let mut offset = 0usize;
        let mut output = Vec::new();
        while offset < input.len() {
            let (body, consumed) = decode_control_frame(&input[offset..], max_content_length)?;
            let response = self.dispatch_runtime_control_body_for_connection_with_snapshots(
                &body, connection, snapshots,
            );
            output.extend_from_slice(&encode_control_body(&response));
            offset += consumed;
        }
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
        if !self.external_effects_use_adapter() {
            self.persist_or_defer_registry_update()?;
        }
        Ok((output, offset))
    }

    /// Runs the handle control input for connection with snapshots async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub async fn handle_control_input_for_connection_with_snapshots_async(
        &mut self,
        input: &[u8],
        max_content_length: usize,
        connection: &mut ControlConnectionState,
        snapshots: &SnapshotRepository,
    ) -> Result<(Vec<u8>, usize)> {
        self.require_live()?;
        let mut offset = 0usize;
        let mut output = Vec::new();
        while offset < input.len() {
            let (body, consumed) = decode_control_frame(&input[offset..], max_content_length)?;
            let response = self
                .dispatch_runtime_control_body_for_connection_with_snapshots_async(
                    &body, connection, snapshots,
                )
                .await;
            output.extend_from_slice(&encode_control_body(&response));
            offset += consumed;
        }
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
        if !self.external_effects_use_adapter() {
            self.persist_or_defer_registry_update()?;
        }
        Ok((output, offset))
    }

    /// Handles actor-owned control input and emits its registry persistence
    /// through the runtime transition contract.
    pub(crate) async fn handle_control_input_for_connection_with_snapshots_transition(
        &mut self,
        input: &[u8],
        max_content_length: usize,
        connection: &mut ControlConnectionState,
        snapshots: &SnapshotRepository,
    ) -> Result<(Vec<u8>, usize, RuntimeTransition)> {
        let (output, consumed) = self
            .handle_control_input_for_connection_with_snapshots_async(
                input,
                max_content_length,
                connection,
                snapshots,
            )
            .await?;
        Ok((output, consumed, self.registry_persistence_transition()))
    }

    /// Prepares a single snapshot control request for repository I/O outside
    /// the actor turn.
    ///
    /// Non-snapshot requests, initialization requests, and unauthenticated
    /// connections return `None` so the caller can use the ordinary control
    /// dispatch path. Snapshot request validation errors are converted to a
    /// JSON-RPC response body because they are successful protocol handling,
    /// not actor transport failures.
    pub(crate) fn prepare_runtime_snapshot_control_async_work(
        &self,
        body: &str,
        connection: &ControlConnectionState,
    ) -> Option<std::result::Result<RuntimeSnapshotControlAsyncWork, String>> {
        let request = match parse_json_rpc_request(body) {
            Ok(request) => request,
            Err(error) => {
                return Some(Err(runtime_json_rpc_error(
                    "null",
                    error.kind(),
                    error.message(),
                )));
            }
        };
        if !connection.initialized()
            || request.method == "control/initialize"
            || !request.method.starts_with("snapshot/")
        {
            return None;
        }
        let Some(caller_client_id) = connection.caller_client_id().cloned() else {
            return Some(Err(runtime_json_rpc_error(
                &request.id,
                crate::error::MezErrorKind::Forbidden,
                "control connection has no authenticated session client",
            )));
        };
        if let Err(error) = authorize_control_request(&self.session, &caller_client_id, &request) {
            return Some(Err(runtime_json_rpc_error(
                &request.id,
                error.kind(),
                error.message(),
            )));
        }
        if let Err(error) = validate_control_method_params_schema(&request) {
            return Some(Err(runtime_json_rpc_error(
                &request.id,
                error.kind(),
                error.message(),
            )));
        }
        let kind = if request.method == "snapshot/resume" {
            RuntimeSnapshotControlAsyncWorkKind::Resume {
                shell: self.session.shell.clone(),
            }
        } else {
            RuntimeSnapshotControlAsyncWorkKind::Dispatch {
                session: Box::new(self.session.clone()),
                context: Box::new(RuntimeSnapshotOwnedCreationContext {
                    pane_captures: self.live_snapshot_pane_captures(),
                    active_config_layers: self.live_snapshot_config_layers(),
                    frame_state: self.live_snapshot_frame_state(),
                    agent_sessions: self.live_snapshot_agent_sessions(),
                    approval_grants: self.live_snapshot_approval_grants(),
                    approval_requests: self.live_snapshot_approval_requests(),
                    message_state: self.live_snapshot_message_state(),
                    mcp_servers: self.live_snapshot_mcp_servers(),
                }),
            }
        };
        Some(Ok(RuntimeSnapshotControlAsyncWork {
            request,
            caller_client_id,
            kind,
        }))
    }

    /// Completes a snapshot control request after repository I/O finished
    /// outside the actor turn.
    pub(crate) fn complete_runtime_snapshot_control_async_work(
        &mut self,
        work: RuntimeSnapshotControlAsyncWork,
        outcome: RuntimeSnapshotControlAsyncOutcome,
        connection: &mut ControlConnectionState,
    ) -> String {
        let _ = connection;
        let result = match outcome {
            RuntimeSnapshotControlAsyncOutcome::Dispatch(result) => result,
            RuntimeSnapshotControlAsyncOutcome::Resume(result) => {
                result.and_then(|(payload, _restored)| {
                    self.require_snapshot_resume_hooks_allow(&payload)?;
                    let snapshot_id = runtime_snapshot_id_from_request(&work.request);
                    let resume_plan = payload.resume_plan();
                    self.apply_runtime_snapshot_resume_for_connection(
                        snapshot_id.as_str(),
                        payload,
                        resume_plan,
                        &work.caller_client_id,
                    )
                })
            }
        };
        let response_succeeded = result.is_ok();
        if let Err(error) = self.append_runtime_snapshot_audit(
            &work.request,
            &work.caller_client_id,
            if response_succeeded {
                "applied"
            } else {
                "failed"
            },
        ) {
            return runtime_json_rpc_error(&work.request.id, error.kind(), error.message());
        }
        if response_succeeded && work.request.method == "snapshot/create" {
            let _ = self.append_lifecycle_event(
                EventKind::SnapshotChanged,
                format!(
                    r#"{{"method":"{}","live_capture":true}}"#,
                    work.request.method
                ),
            );
        }
        let body = match result {
            Ok(result) => format!(
                r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                work.request.id
            ),
            Err(error) => runtime_json_rpc_error(&work.request.id, error.kind(), error.message()),
        };
        self.lifecycle_state = RuntimeLifecycleState::from_session_state(self.session.state);
        body
    }

    /// Completes actor-owned snapshot work and emits registry persistence as a
    /// runtime transition rather than deriving it in the async actor.
    pub(crate) fn complete_runtime_snapshot_control_async_work_transition(
        &mut self,
        work: RuntimeSnapshotControlAsyncWork,
        outcome: RuntimeSnapshotControlAsyncOutcome,
        connection: &mut ControlConnectionState,
    ) -> (String, RuntimeTransition) {
        let body = self.complete_runtime_snapshot_control_async_work(work, outcome, connection);
        let transition = self.registry_persistence_transition();
        (body, transition)
    }
}
