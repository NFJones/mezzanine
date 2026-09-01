//! Per-connection initialization, authentication, and idempotency dispatch.

use super::entry::client_terminal_descriptor_from_control;
use super::method_dispatch::dispatch_parsed_to_response;
use super::{
    AuthenticationMaterial, AuthenticationMechanism, Capabilities, ClientId, ClientRole,
    ControlIdempotencyCache, GrantedRole, InitializeContext, InitializeResult, JsonRpcRequest,
    MezError, RequestedRole, Result, ServerIdentity, Session, authorize_control_request,
    client_json, error_code, initialize, initialize_params_from_json, initialize_result_json,
    json_rpc_error, json_rpc_success, json_string_field, mezzanine_error_code,
    negotiate_protocol_version, parse_json_rpc_request, require_session_target_matches_value,
    session_summary_json,
};
#[cfg(test)]
use super::{decode_control_frame, encode_control_body};
use crate::control::AuthenticatedPeer;
use crate::security::remote::{RemotePrincipal, RemoteRoleCeiling};
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
    /// Stores whether EOF on this connection should detach its owned client.
    ///
    /// Newly created foreground primary and observer attachments are owned by
    /// their connection. Request-scoped clients reusing an existing primary
    /// remain unowned so their EOF cannot detach another connection's client.
    pub(super) detach_client_on_disconnect: bool,
    /// Negotiated server-opened event stream version for this connection.
    pub(super) event_stream_version: Option<u32>,
    /// Whether the negotiated stream may carry client-local clipboard effects.
    pub(super) event_stream_client_clipboard_write: bool,
    /// Whether v3 rendering is owned by pushed snapshots for this connection.
    pub(super) event_stream_push_render: bool,
    /// Random identity assigned by the concrete Iroh connection adapter.
    x11_connection_id: Option<String>,
    /// Session identity associated with the bound X11 proxy.
    x11_route_session_id: Option<String>,
    /// Session-local proxy available to this connection.
    x11_route_proxy: Option<crate::runtime::x11::RuntimeX11ProxyHandle>,
    /// Reserved or active route lease owned by this exact connection.
    x11_route_lease: Option<crate::runtime::x11::RuntimeX11RouteLease>,
    /// Whether post-flush activation was already taken.
    x11_route_start_taken: bool,
    /// First runtime event visible to an observer initialized on this connection.
    pub(super) observer_visible_from_event_id: Option<u64>,
    /// Whether the negotiated event stream start was already consumed.
    pub(super) event_stream_started: bool,
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
            detach_client_on_disconnect: false,
            event_stream_version: None,
            event_stream_client_clipboard_write: false,
            event_stream_push_render: false,
            x11_connection_id: None,
            x11_route_session_id: None,
            x11_route_proxy: None,
            x11_route_lease: None,
            x11_route_start_taken: false,
            observer_visible_from_event_id: None,
            event_stream_started: false,
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
            detach_client_on_disconnect: false,
            event_stream_version: None,
            event_stream_client_clipboard_write: false,
            event_stream_push_render: false,
            x11_connection_id: None,
            x11_route_session_id: None,
            x11_route_proxy: None,
            x11_route_lease: None,
            x11_route_start_taken: false,
            observer_visible_from_event_id: None,
            event_stream_started: false,
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
        if !principal.role_ceiling.permits(principal.requested_role) {
            return Err(MezError::forbidden(
                "remote principal requested a role above its durable trust ceiling",
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
        self.trusted_interactive_assertion = principal.role_ceiling == RemoteRoleCeiling::Primary;
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

    /// Binds one non-empty connection identity assigned by an Iroh adapter.
    pub(crate) fn bind_x11_connection_id(&mut self, connection_id: String) -> Result<()> {
        if connection_id.trim().is_empty() {
            return Err(MezError::invalid_args(
                "X11 route connection identity must not be empty",
            ));
        }
        match self.x11_connection_id.as_ref() {
            Some(existing) if existing != &connection_id => Err(MezError::invalid_state(
                "X11 route connection identity cannot change",
            )),
            Some(_) => Ok(()),
            None => {
                self.x11_connection_id = Some(connection_id);
                Ok(())
            }
        }
    }

    /// Makes one actor-owned session proxy available to this connection.
    pub(crate) fn bind_runtime_x11_proxy(
        &mut self,
        session_id: String,
        proxy: crate::runtime::x11::RuntimeX11ProxyHandle,
    ) -> Result<()> {
        if session_id.trim().is_empty() {
            return Err(MezError::invalid_args(
                "X11 route session identity is empty",
            ));
        }
        if let Some(existing) = self.x11_route_session_id.as_ref()
            && existing != &session_id
        {
            return Err(MezError::invalid_state(
                "X11 route session identity cannot change",
            ));
        }
        if let Some(existing) = self.x11_route_proxy.as_ref()
            && existing != &proxy
        {
            return Err(MezError::invalid_state("X11 route proxy cannot change"));
        }
        self.x11_route_session_id = Some(session_id);
        self.x11_route_proxy = Some(proxy);
        Ok(())
    }

    /// Builds exact route ownership from authenticated transport and client state.
    fn x11_route_owner(
        &self,
        client_id: &ClientId,
    ) -> Result<crate::runtime::x11::RuntimeX11RouteOwner> {
        let endpoint_id = match self.authenticated_peer.as_ref() {
            Some(AuthenticatedPeer::IrohEndpoint { endpoint_id }) => endpoint_id.clone(),
            _ => {
                return Err(MezError::forbidden(
                    "X11 forwarding requires an authenticated Iroh connection",
                ));
            }
        };
        Ok(crate::runtime::x11::RuntimeX11RouteOwner {
            session_id: self
                .x11_route_session_id
                .clone()
                .ok_or_else(|| MezError::forbidden("X11 forwarding is disabled by host policy"))?,
            client_id: client_id.to_string(),
            endpoint_id,
            principal_id: self
                .remote_principal
                .as_ref()
                .map(|principal| principal.trust_record_id.clone()),
            connection_id: self.x11_connection_id.clone().ok_or_else(|| {
                MezError::invalid_state("Iroh connection omitted its X11 route identity")
            })?,
        })
    }

    /// Reserves one generation and retains its cleanup lease on this connection.
    fn reserve_x11_route(
        &mut self,
        client_id: &ClientId,
        offer: crate::runtime::x11::X11ForwardingOffer,
    ) -> Result<crate::runtime::x11::X11ForwardingResult> {
        let owner = self.x11_route_owner(client_id)?;
        let proxy = self
            .x11_route_proxy
            .clone()
            .ok_or_else(|| MezError::forbidden("X11 forwarding is disabled by host policy"))?;
        let (result, lease) = proxy.reserve_route(owner, offer)?;
        self.x11_route_lease = Some(lease);
        self.x11_route_start_taken = false;
        Ok(result)
    }

    /// Takes post-flush activation exactly once while retaining cleanup ownership.
    pub(crate) fn take_x11_route_start(
        &mut self,
    ) -> Option<crate::runtime::x11::RuntimeX11RouteLease> {
        if self.x11_route_start_taken || !self.initialized {
            return None;
        }
        let lease = self.x11_route_lease.clone()?;
        self.x11_route_start_taken = true;
        Some(lease)
    }

    /// Immediately invalidates and releases this connection's exact X11 route.
    pub(crate) fn deactivate_x11_route(&mut self) -> Result<bool> {
        let Some(lease) = self.x11_route_lease.take() else {
            return Ok(false);
        };
        self.x11_route_start_taken = true;
        lease.deactivate()
    }

    /// Sets the first runtime event that a newly attached observer may receive.
    pub(crate) fn set_observer_visible_from_event_id(&mut self, event_id: Option<u64>) {
        self.observer_visible_from_event_id = event_id;
    }

    /// Takes the negotiated event-stream start exactly once after initialization.
    pub fn take_event_stream_start(&mut self) -> Option<(ClientId, u32, bool, bool)> {
        if self.event_stream_started || !self.initialized {
            return None;
        }
        let version = self.event_stream_version?;
        let client_id = self.caller_client_id.clone()?;
        self.event_stream_started = true;
        Some((
            client_id,
            version,
            self.event_stream_client_clipboard_write,
            self.event_stream_push_render,
        ))
    }

    /// Takes the connection-owned client that should receive a disconnect event.
    ///
    /// At most one call returns a client ID, making duplicate EOF, reset, and
    /// shutdown notifications harmless at this boundary.
    pub fn take_disconnect_client_id(&mut self) -> Option<ClientId> {
        if self.disconnect_submitted || !self.detach_client_on_disconnect {
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
            Err(error)
                if error
                    .message()
                    .starts_with("unsupported control protocol version:") =>
            {
                json_rpc_error(&request.id, -32003, error.message(), "unsupported_version")
            }
            Err(error) if error.message() == "unsupported event stream version" => json_rpc_error(
                &request.id,
                -32003,
                error.message(),
                "unsupported_event_stream_version",
            ),
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
    if let Some(version) = init.event_stream_version {
        if !matches!(version, 1..=3) {
            return Err(MezError::invalid_args("unsupported event stream version"));
        }
        if !matches!(
            connection.authenticated_peer(),
            Some(AuthenticatedPeer::IrohEndpoint { .. } | AuthenticatedPeer::UnixUser { .. })
        ) {
            return Err(MezError::forbidden(
                "event streams require an authenticated transport connection",
            ));
        }
        if version == 2
            && (!matches!(
                connection.authenticated_peer(),
                Some(AuthenticatedPeer::IrohEndpoint { .. })
            ) || init.requested_role != RequestedRole::Primary)
        {
            return Err(MezError::forbidden(
                "event stream version 2 requires an authenticated Iroh primary",
            ));
        }
        if version == 3
            && !matches!(
                connection.authenticated_peer(),
                Some(AuthenticatedPeer::IrohEndpoint { .. })
            )
        {
            return Err(MezError::forbidden(
                "event stream version 3 requires an authenticated Iroh client",
            ));
        }
    }
    if init.x11_forwarding.is_some() {
        if init.requested_role != RequestedRole::Primary {
            return Err(MezError::forbidden(
                "X11 forwarding requires an authenticated Iroh primary",
            ));
        }
        if !matches!(
            connection.authenticated_peer(),
            Some(AuthenticatedPeer::IrohEndpoint { .. })
        ) {
            return Err(MezError::forbidden(
                "X11 forwarding requires an authenticated Iroh primary",
            ));
        }
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
            let client_id = session.attach_primary_with_terminal(
                init.client_name.clone(),
                client.interactive,
                client_terminal_descriptor_from_control(client.terminal.as_ref()),
            )?;
            let client_json = session
                .clients()
                .iter()
                .find(|client| client.id == client_id)
                .map(|client| client_json(session, client))
                .ok_or_else(|| MezError::invalid_state("attached primary client is missing"))?;
            let mut capabilities = Capabilities::primary();
            capabilities.features.client_clipboard_write =
                matches!(init.event_stream_version, Some(2 | 3));
            capabilities.features.pushed_render_updates = init.event_stream_version == Some(3);
            let x11_forwarding = if let Some(offer) = init.x11_forwarding {
                match connection.reserve_x11_route(&client_id, offer) {
                    Ok(result) => {
                        capabilities.features.x11_forwarding = true;
                        Some(result)
                    }
                    Err(error) => {
                        let _ = session.detach_client_self(&client_id);
                        return Err(error);
                    }
                }
            } else {
                None
            };
            connection.initialized = true;
            connection.caller_client_id = Some(client_id.clone());
            connection.detach_client_on_disconnect = init.detach_primary_on_disconnect;
            connection.event_stream_version = init.event_stream_version;
            connection.event_stream_client_clipboard_write =
                capabilities.features.client_clipboard_write;
            connection.event_stream_push_render = init.event_stream_version == Some(3);
            Ok(InitializeResult {
                selected_version,
                server: ServerIdentity::current(),
                session: Some(session_summary_json(session)),
                client: Some(client_json),
                granted_role: GrantedRole::Primary,
                capabilities,
                x11_forwarding,
            })
        }
        RequestedRole::Observer => {
            let pushed_render_opt_in = init
                .client
                .as_ref()
                .and_then(|client| client.metadata_json.as_deref())
                .and_then(|metadata| serde_json::from_str::<serde_json::Value>(metadata).ok())
                .and_then(|metadata| {
                    metadata
                        .get("pushed_render_updates")
                        .and_then(serde_json::Value::as_bool)
                })
                .unwrap_or(false);
            let terminal = init.client.as_ref().and_then(|client| {
                client_terminal_descriptor_from_control(client.terminal.as_ref())
            });
            let visible_from_event_id = connection
                .observer_visible_from_event_id
                .unwrap_or_else(|| session.mutation_revision().saturating_add(1));
            let client_id = session.attach_observer_with_terminal(
                init.client_name,
                terminal,
                visible_from_event_id,
            )?;
            connection.initialized = true;
            connection.caller_client_id = Some(client_id.clone());
            connection.detach_client_on_disconnect = true;
            connection.event_stream_version = init.event_stream_version;
            connection.event_stream_client_clipboard_write = false;
            connection.event_stream_push_render =
                init.event_stream_version == Some(3) && pushed_render_opt_in;
            let mut capabilities = Capabilities::observer();
            capabilities.features.pushed_render_updates = connection.event_stream_push_render;
            Ok(InitializeResult {
                selected_version,
                server: ServerIdentity::current(),
                session: Some(session_summary_json(session)),
                client: session
                    .clients()
                    .iter()
                    .find(|client| client.id == client_id)
                    .map(|client| client_json(session, client)),
                granted_role: GrantedRole::Observer,
                capabilities,
                x11_forwarding: None,
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
            connection.caller_client_id = Some(client_id.clone());
            connection.detach_client_on_disconnect = true;
            Ok(InitializeResult {
                selected_version,
                server: ServerIdentity::current(),
                session: Some(session_summary_json(session)),
                client: session
                    .clients()
                    .iter()
                    .find(|client| client.id == client_id)
                    .map(|client| client_json(session, client)),
                granted_role: GrantedRole::Agent,
                capabilities: Capabilities::agent(),
                x11_forwarding: None,
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
            connection.caller_client_id = Some(client_id.clone());
            connection.detach_client_on_disconnect = true;
            Ok(InitializeResult {
                selected_version,
                server: ServerIdentity::current(),
                session: Some(session_summary_json(session)),
                client: session
                    .clients()
                    .iter()
                    .find(|client| client.id == client_id)
                    .map(|client| client_json(session, client)),
                granted_role: GrantedRole::Automation,
                capabilities: Capabilities::automation(),
                x11_forwarding: None,
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
