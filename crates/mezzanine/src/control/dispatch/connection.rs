//! Per-connection initialization, authentication, and idempotency dispatch.

use super::entry::client_terminal_descriptor_from_control;
use super::method_dispatch::dispatch_parsed_to_response;
use super::{
    AuthenticationMaterial, AuthenticationMechanism, Capabilities, ClientId, ClientRole,
    ControlIdempotencyCache, GrantedRole, InitializeContext, InitializeResult, JsonRpcRequest,
    MezError, ObserverRequestSummary, RequestedRole, Result, ServerIdentity, Session,
    authorize_control_request, error_code, initialize, initialize_params_from_json,
    initialize_result_json, json_rpc_error, json_rpc_success, json_string_field,
    mezzanine_error_code, negotiate_protocol_version, observer_json, parse_json_rpc_request,
    require_session_target_matches_value, session_summary_json,
};
#[cfg(test)]
use super::{decode_control_frame, encode_control_body};
use crate::control::AuthenticatedPeer;
use crate::security::remote::RemotePrincipal;
/// Carries Control Connection State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlConnectionState {
    /// Authenticated identity supplied by the concrete transport adapter.
    ///
    /// This identity is deliberately separate from the initialized client and
    /// its application role.
    pub(super) authenticated_peer: Option<AuthenticatedPeer>,
    /// Application authority resolved from pairing or durable device proof.
    pub(super) remote_principal: Option<RemotePrincipal>,
    /// Stores the initialized value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) initialized: bool,
    /// Stores the outer authenticated value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) outer_authenticated: bool,
    /// Stores the trusted interactive assertion value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) trusted_interactive_assertion: bool,
    /// Stores the caller client id value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) caller_client_id: Option<ClientId>,
    /// Stores whether EOF on this connection should detach the primary client.
    ///
    /// This is opt-in for foreground attach sockets so request-scoped control
    /// clients can close without mutating primary ownership.
    pub(super) detach_primary_on_disconnect: bool,
    /// Whether this connection has already emitted its primary disconnect.
    pub(super) disconnect_submitted: bool,
}

impl ControlConnectionState {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(outer_authenticated: bool, trusted_interactive_assertion: bool) -> Self {
        Self {
            authenticated_peer: None,
            remote_principal: None,
            initialized: false,
            outer_authenticated,
            trusted_interactive_assertion,
            caller_client_id: None,
            detach_primary_on_disconnect: false,
            disconnect_submitted: false,
        }
    }

    /// Runs the trusted existing client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn trusted_existing_client(caller_client_id: ClientId) -> Self {
        Self {
            authenticated_peer: None,
            remote_principal: None,
            initialized: true,
            outer_authenticated: true,
            trusted_interactive_assertion: true,
            caller_client_id: Some(caller_client_id),
            detach_primary_on_disconnect: false,
            disconnect_submitted: false,
        }
    }

    /// Binds the transport-authenticated peer to this connection.
    ///
    /// Rebinding to a different identity is rejected so reconnect or adapter
    /// bugs cannot silently change the principal associated with live state.
    pub fn bind_authenticated_peer(&mut self, peer: AuthenticatedPeer) -> Result<()> {
        match &self.authenticated_peer {
            Some(existing) if existing != &peer => Err(MezError::invalid_state(
                "control connection authenticated peer cannot change",
            )),
            Some(_) => Ok(()),
            None => {
                if matches!(peer, AuthenticatedPeer::IrohEndpoint { .. }) {
                    self.outer_authenticated = false;
                    self.trusted_interactive_assertion = false;
                    self.remote_principal = None;
                }
                self.authenticated_peer = Some(peer);
                Ok(())
            }
        }
    }

    /// Returns the identity established by the concrete transport adapter.
    pub fn authenticated_peer(&self) -> Option<&AuthenticatedPeer> {
        self.authenticated_peer.as_ref()
    }

    /// Binds application authority resolved from remote pairing or device proof.
    pub fn bind_remote_principal(&mut self, principal: RemotePrincipal) -> Result<()> {
        let endpoint_id = match self.authenticated_peer.as_ref() {
            Some(AuthenticatedPeer::IrohEndpoint { endpoint_id }) => endpoint_id,
            _ => {
                return Err(MezError::invalid_state(
                    "remote principal requires an authenticated Iroh peer",
                ));
            }
        };
        if endpoint_id != &principal.endpoint_id {
            return Err(MezError::forbidden(
                "remote principal endpoint does not match the transport peer",
            ));
        }
        if let Some(existing) = self.remote_principal.as_ref()
            && existing != &principal
        {
            return Err(MezError::invalid_state(
                "control connection remote principal cannot change",
            ));
        }
        self.outer_authenticated = true;
        self.trusted_interactive_assertion = principal.requested_role == RequestedRole::Primary;
        self.remote_principal = Some(principal);
        Ok(())
    }

    /// Returns resolved remote application authority, when present.
    pub fn remote_principal(&self) -> Option<&RemotePrincipal> {
        self.remote_principal.as_ref()
    }

    /// Runs the caller client id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn caller_client_id(&self) -> Option<&ClientId> {
        self.caller_client_id.as_ref()
    }

    /// Runs the rebind caller client operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
    pub fn rebind_caller_client(&mut self, caller_client_id: ClientId) {
        self.initialized = true;
        self.caller_client_id = Some(caller_client_id);
    }

    /// Runs the initialized operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn initialized(&self) -> bool {
        self.initialized
    }

    /// Takes the primary client that should receive an EOF disconnect event.
    ///
    /// At most one call returns a client ID, making duplicate EOF, reset, and
    /// shutdown notifications harmless at this boundary.
    pub fn take_disconnect_client_id(&mut self) -> Option<ClientId> {
        if self.disconnect_submitted || !self.detach_primary_on_disconnect {
            return None;
        }
        let client_id = self.caller_client_id.clone()?;
        self.disconnect_submitted = true;
        Some(client_id)
    }
}

/// Runs the handle control frames for connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub fn handle_control_frames_for_connection(
    input: &[u8],
    max_content_length: usize,
    session: &mut Session,
    connection: &mut ControlConnectionState,
    idempotency: &mut ControlIdempotencyCache,
) -> Result<(Vec<u8>, usize)> {
    let mut offset = 0usize;
    let mut output = Vec::new();
    while offset < input.len() {
        let (body, consumed) = decode_control_frame(&input[offset..], max_content_length)?;
        let response =
            dispatch_control_request_for_connection(&body, session, connection, idempotency);
        output.extend_from_slice(&encode_control_body(&response));
        offset += consumed;
    }
    Ok((output, offset))
}

/// Runs the dispatch control request for connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_for_connection(
    body: &str,
    session: &mut Session,
    connection: &mut ControlConnectionState,
    idempotency: &mut ControlIdempotencyCache,
) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };

    if !connection.initialized {
        if request.method != "control/initialize" {
            return json_rpc_error(
                &request.id,
                error_code(crate::error::MezErrorKind::Forbidden),
                "first control request must be control/initialize",
                mezzanine_error_code(crate::error::MezErrorKind::Forbidden),
            );
        }
        return match initialize_control_connection(&request, session, connection) {
            Ok(result) => json_rpc_success(&request.id, &initialize_result_json(&result)),
            Err(error) => json_rpc_error(
                &request.id,
                error_code(error.kind()),
                error.message(),
                mezzanine_error_code(error.kind()),
            ),
        };
    }

    if request.method == "control/initialize" {
        return json_rpc_error(
            &request.id,
            error_code(crate::error::MezErrorKind::InvalidState),
            "control connection is already initialized",
            mezzanine_error_code(crate::error::MezErrorKind::InvalidState),
        );
    }

    let caller_client_id = match connection.caller_client_id.clone() {
        Some(client_id) => client_id,
        None => {
            return json_rpc_error(
                &request.id,
                error_code(crate::error::MezErrorKind::Forbidden),
                "control connection has no authenticated session client",
                mezzanine_error_code(crate::error::MezErrorKind::Forbidden),
            );
        }
    };
    dispatch_control_request_cached_for_client(&request, session, &caller_client_id, idempotency)
}

/// Runs the dispatch control request cached for client operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn dispatch_control_request_cached_for_client(
    request: &JsonRpcRequest,
    session: &mut Session,
    caller_client_id: &ClientId,
    idempotency: &mut ControlIdempotencyCache,
) -> String {
    if let Err(error) = authorize_control_request(session, caller_client_id, request) {
        return json_rpc_error(
            &request.id,
            error_code(error.kind()),
            error.message(),
            mezzanine_error_code(error.kind()),
        );
    }
    let cache_key = request
        .params
        .as_deref()
        .and_then(|params| json_string_field(params, "idempotency_key"))
        .map(|key| format!("{caller_client_id}:{key}"));
    if let Some(cache_key) = &cache_key {
        match idempotency.cached_response(cache_key, &request.method, &request.params) {
            Ok(Some(response)) => return response,
            Ok(None) => {}
            Err(error) => {
                return json_rpc_error(
                    &request.id,
                    error_code(error.kind()),
                    error.message(),
                    mezzanine_error_code(error.kind()),
                );
            }
        }
    }
    let response = dispatch_parsed_to_response(request, session, caller_client_id, None);
    if let Some(cache_key) = cache_key {
        idempotency.remember_response(
            cache_key,
            request.method.clone(),
            request.params.clone(),
            response.clone(),
        );
    }
    response
}

/// Runs the initialize control connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn initialize_control_connection(
    request: &JsonRpcRequest,
    session: &mut Session,
    connection: &mut ControlConnectionState,
) -> Result<InitializeResult> {
    let params = request
        .params
        .as_deref()
        .ok_or_else(|| MezError::invalid_args("control/initialize requires a params object"))?;
    let init = initialize_params_from_json(params)?;
    let authentication = init
        .authentication
        .as_ref()
        .unwrap_or(&AuthenticationMaterial {
            mechanism: AuthenticationMechanism::None,
            token: None,
        });
    let authenticated = connection.outer_authenticated || authentication.is_payload_authenticated();

    if !authenticated {
        connection.initialized = true;
        return initialize(
            init,
            InitializeContext {
                outer_authenticated: false,
                trusted_interactive_assertion: connection.trusted_interactive_assertion,
            },
        );
    }

    let selected_version = negotiate_protocol_version(init.requested_version)?;
    if let Some(session_target) = init.session_target_json.as_deref() {
        let session_target =
            serde_json::from_str::<serde_json::Value>(session_target).map_err(|error| {
                MezError::invalid_args(format!("session_target is invalid: {error}"))
            })?;
        require_session_target_matches_value(session, &session_target)?;
    }
    match init.requested_role {
        RequestedRole::Primary => {
            let client = init.client.as_ref().ok_or_else(|| {
                MezError::invalid_args("primary initialization requires a client descriptor")
            })?;
            if !client.identifies_interactive_terminal(connection.trusted_interactive_assertion) {
                return Err(MezError::forbidden(
                    "primary initialization requires a verified interactive terminal",
                ));
            }
            let client_id = match session.primary_client_id().cloned() {
                Some(existing_primary) => {
                    let existing = session
                        .clients()
                        .iter()
                        .find(|client| client.id == existing_primary)
                        .ok_or_else(|| {
                            MezError::invalid_state("primary client is missing from client list")
                        })?;
                    if existing.name != init.client_name {
                        return Err(MezError::conflict(
                            "session already has an attached primary client",
                        ));
                    }
                    existing_primary
                }
                None => session.attach_primary_with_terminal(
                    init.client_name.clone(),
                    client.interactive,
                    client_terminal_descriptor_from_control(client.terminal.as_ref()),
                )?,
            };
            connection.initialized = true;
            connection.caller_client_id = Some(client_id);
            connection.detach_primary_on_disconnect = init.detach_primary_on_disconnect;
            Ok(InitializeResult {
                selected_version,
                server: ServerIdentity::current(),
                session: Some(session_summary_json(session)),
                granted_role: GrantedRole::Primary,
                capabilities: Capabilities::primary(),
                approval_pending: false,
                observer_request: None,
            })
        }
        RequestedRole::Observer => {
            let terminal = init.client.as_ref().and_then(|client| {
                client_terminal_descriptor_from_control(client.terminal.as_ref())
            });
            let (client_id, observer_id) =
                session.request_observer_with_terminal(init.client_name, terminal);
            let observer_state = observer_json(session, observer_id.as_str())?;
            connection.initialized = true;
            connection.caller_client_id = Some(client_id);
            connection.detach_primary_on_disconnect = false;
            Ok(InitializeResult {
                selected_version,
                server: ServerIdentity::current(),
                session: None,
                granted_role: GrantedRole::PendingObserver,
                capabilities: Capabilities::pending_observer(),
                approval_pending: true,
                observer_request: Some(ObserverRequestSummary {
                    request_id: observer_id.to_string(),
                    state: "pending",
                    state_json: Some(observer_state),
                }),
            })
        }
        RequestedRole::Agent => {
            let client_id = session.attach_control_client(
                init.client_name,
                ClientRole::Agent,
                init.client
                    .as_ref()
                    .is_some_and(|client| client.interactive),
            )?;
            connection.initialized = true;
            connection.caller_client_id = Some(client_id);
            Ok(InitializeResult {
                selected_version,
                server: ServerIdentity::current(),
                session: Some(session_summary_json(session)),
                granted_role: GrantedRole::Agent,
                capabilities: Capabilities::agent(),
                approval_pending: false,
                observer_request: None,
            })
        }
        RequestedRole::Automation => {
            let client_id = session.attach_control_client(
                init.client_name,
                ClientRole::Automation,
                init.client
                    .as_ref()
                    .is_some_and(|client| client.interactive),
            )?;
            connection.initialized = true;
            connection.caller_client_id = Some(client_id);
            Ok(InitializeResult {
                selected_version,
                server: ServerIdentity::current(),
                session: Some(session_summary_json(session)),
                granted_role: GrantedRole::Automation,
                capabilities: Capabilities::automation(),
                approval_pending: false,
                observer_request: None,
            })
        }
    }
}

/// Runs the dispatch control request cached operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_control_request_cached(
    body: &str,
    session: &mut Session,
    primary_client_id: &ClientId,
    idempotency: &mut ControlIdempotencyCache,
) -> String {
    let request = match parse_json_rpc_request(body) {
        Ok(request) => request,
        Err(error) => {
            return json_rpc_error("null", -32600, error.message(), "invalid_request");
        }
    };
    let cache_key = request
        .params
        .as_deref()
        .and_then(|params| json_string_field(params, "idempotency_key"))
        .map(|key| format!("{primary_client_id}:{key}"));
    if let Some(cache_key) = &cache_key {
        match idempotency.cached_response(cache_key, &request.method, &request.params) {
            Ok(Some(response)) => return response,
            Ok(None) => {}
            Err(error) => {
                return json_rpc_error(
                    &request.id,
                    error_code(error.kind()),
                    error.message(),
                    mezzanine_error_code(error.kind()),
                );
            }
        }
    }

    let response = dispatch_parsed_to_response(&request, session, primary_client_id, None);
    if let Some(cache_key) = cache_key {
        idempotency.remember_response(cache_key, request.method, request.params, response.clone());
    }
    response
}
