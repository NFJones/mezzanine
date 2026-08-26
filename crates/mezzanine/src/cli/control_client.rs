//! Cli Control Client implementation.
//!
//! This module owns the cli control client boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use iroh::EndpointAddr;
use secrecy::{ExposeSecret, SecretString};

use super::{
    CliOutputFormat, MezError, Read, Result, SocketSelection, UnixStream, Write,
    decode_control_frame, encode_control_body, json_escape, selected_socket_path,
    write_control_response,
};
use crate::runtime::{
    IrohCompressionBridge, IrohCompressionPolicy, RuntimeIrohTransportPolicy,
    bind_runtime_iroh_client_endpoint,
};
use crate::security::remote::{
    RemoteClientIdentity, RemoteClientProfile, RemoteClientProfileScope, RemoteClientProfileStore,
    RemoteRoleCeiling, read_remote_invitation_file,
};

// Direct control request framing and response handling.

/// Maximum control response body accepted by direct CLI requests.
const CLI_CONTROL_MAX_CONTENT_LENGTH: usize = 1024 * 1024;

/// Runs the run control request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn run_control_request<W: Write>(
    socket_selection: &SocketSelection,
    method: &str,
    params: &str,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let body = request_control_body(socket_selection, method, params)?;
    write_control_response(stdout, output_format, &body)?;
    Ok(())
}

/// Exchanges one initialized request over the selected local control socket.
pub(super) fn request_control_body(
    socket_selection: &SocketSelection,
    method: &str,
    params: &str,
) -> Result<String> {
    let socket_path = selected_socket_path(socket_selection);
    let mut stream = UnixStream::connect(socket_path)?;
    exchange_control_request(&mut stream, method, params)
}

/// Exchanges initialization and one request over an established byte stream.
///
/// Connection selection and transport authentication remain the concrete
/// connector's responsibility. This function owns only ordered control framing
/// and response decoding, so later connectors can reuse it without changing the
/// Unix-socket default.
pub(super) fn exchange_control_request<S: Read + Write>(
    stream: &mut S,
    method: &str,
    params: &str,
) -> Result<String> {
    let initialize = r#"{"jsonrpc":"2.0","id":"cli-init","method":"control/initialize","params":{"client_name":"primary","requested_version":2,"requested_role":"primary","detach_primary_on_disconnect":true,"client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#;
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":"cli","method":"{}","params":{}}}"#,
        json_escape(method),
        params
    );
    stream.write_all(&encode_control_body(initialize))?;
    stream.flush()?;
    let initialize_response =
        read_control_response_frames(stream, CLI_CONTROL_MAX_CONTENT_LENGTH, 1)?;
    let (initialize_body, _) =
        decode_control_frame(&initialize_response, CLI_CONTROL_MAX_CONTENT_LENGTH)?;
    ensure_one_shot_initialize_success(&initialize_body)?;
    stream.write_all(&encode_control_body(&request))?;
    stream.flush()?;
    let response = read_control_response_frames(stream, CLI_CONTROL_MAX_CONTENT_LENGTH, 1)?;
    let (body, _) = decode_control_frame(&response, CLI_CONTROL_MAX_CONTENT_LENGTH)?;
    Ok(body)
}

/// Rejects a failed one-shot initialization before a follow-up request can
/// replace the causal JSON-RPC error with an uninitialized-connection error.
fn ensure_one_shot_initialize_success(body: &str) -> Result<()> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("control initialize response is not valid JSON"))?;
    let Some(error) = value.get("error") else {
        return Ok(());
    };
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("control initialization was rejected");
    Err(MezError::invalid_state(format!(
        "control initialize failed: {message}"
    )))
}

const MAX_IROH_INVITATION_FILE_BYTES: u64 = 64 * 1024;

/// Runs one direct request through the explicitly selected transport.
pub(super) async fn run_control_request_for_target<W: Write>(
    control_target: &super::ControlTargetSelection,
    socket_selection: &SocketSelection,
    env: &super::CliEnv,
    method: &str,
    params: &str,
    output_format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    if control_target.is_unix() {
        return run_control_request(socket_selection, method, params, output_format, stdout);
    }

    let paths = env.config_paths()?;
    let layers = super::load_runtime_config_layers(&paths)?;
    let structured = crate::runtime::runtime_effective_config_value(&layers)?;
    let configured_policy = crate::runtime::runtime_iroh_transport_policy_from_config(&structured)?;
    let target = match control_target {
        super::ControlTargetSelection::Unix => unreachable!("Unix target returned above"),
        super::ControlTargetSelection::IrohProfile(name) => {
            let profile = RemoteClientProfileStore::under_config_root(paths.root())
                .load(name)?
                .ok_or_else(|| {
                    MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "Iroh client profile not found",
                    )
                })?;
            IrohControlTarget::Profile(profile)
        }
        super::ControlTargetSelection::IrohInvitation { path, save_as } => {
            let target = parse_iroh_invitation_file(path, save_as.as_deref())?;
            preflight_iroh_invitation_profile(paths.root(), &target)?;
            target
        }
    };
    let policy = explicit_iroh_client_policy(&configured_policy, &target)?;
    let body =
        exchange_iroh_control_request(paths.root(), &policy, &target, method, params).await?;
    write_control_response(stdout, output_format, &body)
}

fn parse_iroh_invitation_file(path: &Path, save_as: Option<&str>) -> Result<IrohControlTarget> {
    let bytes = read_remote_invitation_file(path, MAX_IROH_INVITATION_FILE_BYTES)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| MezError::invalid_args("invalid Iroh invitation JSON"))?;
    let invitation = value.get("result").unwrap_or(&value);
    let object = invitation
        .as_object()
        .ok_or_else(|| MezError::invalid_args("Iroh invitation must be a JSON object"))?;
    if object
        .get("format_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err(MezError::invalid_args(
            "Iroh invitation format_version must be 1",
        ));
    }
    let server_addr: EndpointAddr = serde_json::from_value(
        object
            .get("server_addr")
            .cloned()
            .ok_or_else(|| MezError::invalid_args("Iroh invitation omitted server_addr"))?,
    )
    .map_err(|_| MezError::invalid_args("Iroh invitation contains an invalid server_addr"))?;
    if let Some(server_endpoint_id) = object
        .get("server_endpoint_id")
        .and_then(serde_json::Value::as_str)
        && server_addr.id.to_string() != server_endpoint_id
    {
        return Err(MezError::forbidden(
            "Iroh invitation server identity does not match its address",
        ));
    }
    let profile_name = save_as
        .map(str::to_string)
        .unwrap_or(invitation_string(object, "profile_name")?);
    let token = invitation_string(object, "token")?;
    let role = match invitation_string(object, "role")?.as_str() {
        "observer" => RemoteRoleCeiling::Observer,
        "primary" => RemoteRoleCeiling::Primary,
        _ => {
            return Err(MezError::invalid_args(
                "Iroh invitation role is unsupported",
            ));
        }
    };
    let expires_at_unix_seconds = object
        .get("expires_at_unix_seconds")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| MezError::invalid_args("Iroh invitation omitted expiration"))?;
    let scope = match object
        .get("profile_scope")
        .and_then(serde_json::Value::as_str)
    {
        Some("host") => RemoteClientProfileScope::Host,
        None | Some("legacy_session") => RemoteClientProfileScope::LegacySession,
        Some(_) => {
            return Err(MezError::invalid_args(
                "Iroh invitation profile_scope must be host or legacy_session",
            ));
        }
    };
    Ok(IrohControlTarget::Invitation {
        profile_name,
        server_addr,
        token: SecretString::from(token),
        role,
        scope,
        expires_at_unix_seconds,
    })
}

/// Secret-free metadata parsed from one validated invitation file.
#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct IrohInvitationSummary {
    /// Version of the invitation envelope.
    pub format_version: u64,
    /// Server-suggested client-local profile name.
    pub profile_name: String,
    /// Abbreviated pinned server endpoint identity.
    pub server_fingerprint: String,
    /// Maximum role carried by the invitation.
    pub role: RemoteRoleCeiling,
    /// Whether the invitation names a persistent host or one legacy session.
    pub scope: RemoteClientProfileScope,
    /// Invitation expiration as Unix seconds.
    pub expires_at_unix_seconds: u64,
    /// Whether the invitation is already expired according to the local clock.
    pub expired: bool,
    /// Number of pinned direct IP routes.
    pub direct_route_count: usize,
    /// Number of pinned relay routes.
    pub relay_route_count: usize,
}

/// Parses one invitation file and returns only safe display metadata.
pub(super) fn inspect_iroh_invitation_file(path: &Path) -> Result<IrohInvitationSummary> {
    let target = parse_iroh_invitation_file(path, None)?;
    let IrohControlTarget::Invitation {
        profile_name,
        server_addr,
        role,
        scope,
        expires_at_unix_seconds,
        ..
    } = target
    else {
        unreachable!("invitation parsing always returns an invitation target")
    };
    let direct_route_count = server_addr
        .addrs
        .iter()
        .filter(|addr| matches!(addr, iroh::TransportAddr::Ip(_)))
        .count();
    let relay_route_count = server_addr
        .addrs
        .iter()
        .filter(|addr| matches!(addr, iroh::TransportAddr::Relay(_)))
        .count();
    Ok(IrohInvitationSummary {
        format_version: 1,
        profile_name,
        server_fingerprint: crate::security::remote::abbreviated_endpoint_fingerprint(
            server_addr.id,
        ),
        role,
        scope,
        expires_at_unix_seconds,
        expired: current_unix_seconds_for_iroh_client()? > expires_at_unix_seconds,
        direct_route_count,
        relay_route_count,
    })
}

/// Rejects an invitation alias conflict before dialing or redeeming its token.
fn preflight_iroh_invitation_profile(config_root: &Path, target: &IrohControlTarget) -> Result<()> {
    let IrohControlTarget::Invitation {
        profile_name,
        server_addr,
        scope,
        ..
    } = target
    else {
        return Ok(());
    };
    RemoteClientProfileStore::under_config_root(config_root).preflight_name_for_server(
        profile_name,
        server_addr.id,
        *scope,
    )
}

fn invitation_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<String> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| MezError::invalid_args(format!("Iroh invitation omitted {field}")))
}

/// Explicit Iroh destination and Mezzanine authentication material.
#[derive(Clone)]
pub(crate) enum IrohControlTarget {
    /// First-use pairing against a pinned server address.
    Invitation {
        profile_name: String,
        server_addr: EndpointAddr,
        token: SecretString,
        role: RemoteRoleCeiling,
        scope: RemoteClientProfileScope,
        expires_at_unix_seconds: u64,
    },
    /// Durable reconnect through one protected profile.
    Profile(RemoteClientProfile),
}

impl std::fmt::Debug for IrohControlTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Invitation {
                profile_name,
                server_addr,
                role,
                scope,
                expires_at_unix_seconds,
                ..
            } => formatter
                .debug_struct("IrohControlTarget::Invitation")
                .field("profile_name", profile_name)
                .field("server_address_count", &server_addr.addrs.len())
                .field("role", role)
                .field("scope", scope)
                .field("expires_at_unix_seconds", expires_at_unix_seconds)
                .field("token", &"[REDACTED]")
                .finish(),
            Self::Profile(profile) => formatter
                .debug_struct("IrohControlTarget::Profile")
                .field("profile_name", &profile.name)
                .field("server_address_count", &profile.server_addr.addrs.len())
                .field("role", &profile.role)
                .field("device_credential", &"[REDACTED]")
                .finish(),
        }
    }
}

impl IrohControlTarget {
    fn server_addr(&self) -> &EndpointAddr {
        match self {
            Self::Invitation { server_addr, .. } => server_addr,
            Self::Profile(profile) => &profile.server_addr,
        }
    }

    fn profile_name(&self) -> &str {
        match self {
            Self::Invitation { profile_name, .. } => profile_name,
            Self::Profile(profile) => &profile.name,
        }
    }

    fn route_counts(&self) -> (usize, usize) {
        let direct = self
            .server_addr()
            .addrs
            .iter()
            .filter(|addr| matches!(addr, iroh::TransportAddr::Ip(_)))
            .count();
        let relay = self
            .server_addr()
            .addrs
            .iter()
            .filter(|addr| matches!(addr, iroh::TransportAddr::Relay(_)))
            .count();
        (direct, relay)
    }

    fn role(&self) -> RemoteRoleCeiling {
        match self {
            Self::Invitation { role, .. } => *role,
            Self::Profile(profile) => profile.role,
        }
    }

    fn scope(&self) -> RemoteClientProfileScope {
        match self {
            Self::Invitation { scope, .. } => *scope,
            Self::Profile(profile) => profile.scope,
        }
    }

    fn authentication(&self) -> (&str, &SecretString) {
        match self {
            Self::Invitation { token, .. } => ("extension:iroh_invitation", token),
            Self::Profile(profile) => ("extension:iroh_device", &profile.device_credential),
        }
    }
}

/// Derives a client-only network policy from one explicit pinned target.
fn explicit_iroh_client_policy(
    configured: &RuntimeIrohTransportPolicy,
    target: &IrohControlTarget,
) -> Result<RuntimeIrohTransportPolicy> {
    if !configured.outbound_enabled {
        return Err(MezError::config(
            "outbound Iroh connections are disabled by transport.iroh.outbound_enabled",
        ));
    }
    let server_addr = target.server_addr();
    let direct_connections = server_addr.ip_addrs().next().is_some();
    let relay_urls = server_addr
        .relay_urls()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let profile_lookup = matches!(target, IrohControlTarget::Profile(_));
    let address_lookup = if profile_lookup {
        configured.address_lookup.clone()
    } else {
        crate::runtime::RuntimeIrohAddressLookupPolicy::Disabled
    };
    let lookup_enabled = !matches!(
        address_lookup,
        crate::runtime::RuntimeIrohAddressLookupPolicy::Disabled
            | crate::runtime::RuntimeIrohAddressLookupPolicy::Local
    );
    let direct_connections =
        configured.direct_connections && (direct_connections || profile_lookup && lookup_enabled);
    let relay = if !relay_urls.is_empty() {
        crate::runtime::RuntimeIrohRelayPolicy::Custom { urls: relay_urls }
    } else if profile_lookup && lookup_enabled {
        configured.relay.clone()
    } else {
        crate::runtime::RuntimeIrohRelayPolicy::Disabled
    };
    if !direct_connections
        && matches!(relay, crate::runtime::RuntimeIrohRelayPolicy::Disabled)
        && !lookup_enabled
    {
        return Err(MezError::invalid_args(
            "explicit Iroh target has no supported pinned IP or relay route; address lookup is disabled for explicit targets",
        ));
    }
    Ok(RuntimeIrohTransportPolicy {
        enabled: false,
        address_lookup,
        relay,
        direct_connections,
        port_mapping: false,
        ..configured.clone()
    })
}

/// Returns active route information learned during an authenticated connection.
async fn authenticated_remote_addr(
    endpoint: &iroh::Endpoint,
    endpoint_id: iroh::EndpointId,
) -> Option<EndpointAddr> {
    let info = endpoint.remote_info(endpoint_id).await?;
    let addr = EndpointAddr::from_parts(
        info.id(),
        info.into_addrs()
            .filter(|addr| matches!(addr.usage(), iroh::endpoint::TransportAddrUsage::Active))
            .map(iroh::endpoint::TransportAddrInfo::into_addr),
    );
    (!addr.is_empty()).then_some(addr)
}

/// Refreshes a paired profile only after transport and device authentication succeed.
async fn refresh_authenticated_profile_route(
    config_root: &Path,
    endpoint: &iroh::Endpoint,
    target: &IrohControlTarget,
) -> Result<()> {
    let IrohControlTarget::Profile(profile) = target else {
        return Ok(());
    };
    let Some(server_addr) = authenticated_remote_addr(endpoint, profile.server_addr.id).await
    else {
        return Ok(());
    };
    RemoteClientProfileStore::under_config_root(config_root).save(&RemoteClientProfile {
        name: profile.name.clone(),
        server_addr,
        role: profile.role,
        scope: profile.scope,
        device_credential: profile.device_credential.clone(),
    })
}

/// Session selection carried by one host-scoped protocol-v3 initialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum IrohSessionRouting {
    /// Creates one fresh lease-backed session.
    Create {
        name: Option<String>,
        idempotency_key: String,
    },
    /// Attaches one existing lease by stable lease id, session id, or exact name.
    Attach { target: String },
    /// Selects one existing default and never creates.
    Default,
}

impl IrohSessionRouting {
    fn intent(&self) -> &'static str {
        match self {
            Self::Create { .. } => "create",
            Self::Attach { .. } => "attach",
            Self::Default => "default",
        }
    }

    fn session_target(&self) -> Option<serde_json::Value> {
        match self {
            Self::Attach { target } if target.starts_with("lease-") => {
                Some(serde_json::json!({"lease_id": target}))
            }
            Self::Attach { target } if target.starts_with('$') => {
                Some(serde_json::json!({"session_id": target}))
            }
            Self::Attach { target } => Some(serde_json::json!({"name": target})),
            Self::Create { .. } | Self::Default => None,
        }
    }

    fn idempotency_key(&self) -> Option<&str> {
        match self {
            Self::Create {
                idempotency_key, ..
            } => Some(idempotency_key),
            Self::Attach { .. } | Self::Default => None,
        }
    }

    fn session_name(&self) -> Option<&str> {
        match self {
            Self::Create { name, .. } => name.as_deref(),
            Self::Attach { .. } | Self::Default => None,
        }
    }
}

/// One initialized, long-lived Iroh control stream for interactive attach.
pub(super) struct PersistentIrohControlChannel {
    _identity: RemoteClientIdentity,
    endpoint: iroh::Endpoint,
    connection: iroh::endpoint::Connection,
    bridge: IrohCompressionBridge,
    event_receiver: Option<tokio::sync::mpsc::Receiver<Result<super::attach::AttachRenderAction>>>,
    event_task: tokio::task::JoinHandle<()>,
    setup_timeout: std::time::Duration,
}

impl PersistentIrohControlChannel {
    /// Returns the initialized byte stream used by the shared attach protocol.
    pub(super) fn stream_mut(&mut self) -> &mut tokio::io::DuplexStream {
        self.bridge.stream_mut()
    }

    /// Returns a clone of the retained connection for local attach health sampling.
    pub(super) fn connection(&self) -> iroh::endpoint::Connection {
        self.connection.clone()
    }

    /// Takes the negotiated event receiver exactly once for the attach loop.
    pub(super) fn take_event_receiver(
        &mut self,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<super::attach::AttachRenderAction>>> {
        self.event_receiver
            .take()
            .ok_or_else(|| MezError::invalid_state("Iroh event receiver was already taken"))
    }

    /// Finishes the control stream and closes the connection and endpoint boundedly.
    pub(super) async fn close(self) {
        let Self {
            _identity,
            endpoint,
            connection,
            bridge,
            event_receiver: _,
            mut event_task,
            setup_timeout,
        } = self;
        let _ = bridge.shutdown(setup_timeout).await;
        connection.close(iroh::endpoint::VarInt::from_u32(0), b"attach complete");
        if tokio::time::timeout(setup_timeout, &mut event_task)
            .await
            .is_err()
        {
            event_task.abort();
            let _ = event_task.await;
        }
        let _ = tokio::time::timeout(setup_timeout, endpoint.close()).await;
    }
}

/// Opens and initializes one persistent Iroh control stream for interactive attach.
pub(super) async fn open_persistent_iroh_control_channel(
    control_target: &super::ControlTargetSelection,
    env: &super::CliEnv,
    requested_role: &str,
    routing: Option<&IrohSessionRouting>,
    columns: u16,
    rows: u16,
    term: &str,
) -> Result<(PersistentIrohControlChannel, String)> {
    let paths = env.config_paths()?;
    let layers = super::load_runtime_config_layers(&paths)?;
    let structured = crate::runtime::runtime_effective_config_value(&layers)?;
    let configured_policy = crate::runtime::runtime_iroh_transport_policy_from_config(&structured)?;
    let client_clipboard = crate::runtime::runtime_client_host_clipboard_from_config(&structured)?;
    let mut target = resolve_iroh_control_target(control_target, paths.root())?;
    ensure_iroh_attach_role_allowed(target.role(), requested_role)?;
    if let IrohControlTarget::Invitation {
        expires_at_unix_seconds,
        ..
    } = &target
        && current_unix_seconds_for_iroh_client()? > *expires_at_unix_seconds
    {
        return Err(MezError::forbidden(
            "Iroh pairing invitation expired before connection setup",
        ));
    }

    if matches!(
        &target,
        IrohControlTarget::Invitation {
            scope: RemoteClientProfileScope::Host,
            ..
        }
    ) {
        let profile_name = target.profile_name().to_string();
        exchange_iroh_host_only_initialize(paths.root(), &configured_policy, &target).await?;
        let profile = RemoteClientProfileStore::under_config_root(paths.root())
            .load(&profile_name)?
            .ok_or_else(|| {
                MezError::invalid_state(
                    "successful host pairing did not persist a reconnect profile",
                )
            })?;
        target = IrohControlTarget::Profile(profile);
    }

    let policy = explicit_iroh_client_policy(&configured_policy, &target)?;

    let identity = RemoteClientIdentity::load_or_create(paths.root())?;
    let endpoint =
        bind_runtime_iroh_client_endpoint(&policy, identity.secret_key().clone()).await?;
    let (connection, compression) =
        connect_iroh_with_compression(&endpoint, &policy, &target).await?;
    if connection.remote_id() != target.server_addr().id {
        return Err(MezError::forbidden(
            "Iroh connection authenticated an unexpected server identity",
        ));
    }
    let (send, recv) = tokio::time::timeout(policy.setup_timeout, connection.open_bi())
        .await
        .map_err(|_| MezError::invalid_state("Iroh control stream setup timed out"))?
        .map_err(|_| MezError::invalid_state("failed to open Iroh control stream"))?;
    let mut bridge =
        IrohCompressionBridge::spawn(recv, send, compression, CLI_CONTROL_MAX_CONTENT_LENGTH)?;
    let (mechanism, credential) = target.authentication();
    let host_scoped = target.scope() == RemoteClientProfileScope::Host;
    let routing = if host_scoped {
        Some(routing.ok_or_else(|| {
            MezError::invalid_args("host-scoped Iroh control requires explicit session routing")
        })?)
    } else {
        None
    };
    let mut client = serde_json::json!({
        "name": "remote-cli",
        "interactive": true,
        "terminal": {
            "columns": columns,
            "rows": rows,
            "term": term
        }
    });
    if let Some(session_name) = routing.and_then(IrohSessionRouting::session_name) {
        client["metadata"] = serde_json::json!({"session_name": session_name});
    }
    let mut requested_event_stream_version = if requested_role == "primary" { 2 } else { 1 };
    let mut params = serde_json::json!({
        "client_name": "remote-cli",
        "requested_version": if host_scoped { 3 } else { 2 },
        "requested_role": requested_role,
        "detach_primary_on_disconnect": requested_role == "primary",
        "event_stream_version": requested_event_stream_version,
        "client": client,
        "authentication": {
            "mechanism": mechanism,
            "token": credential.expose_secret()
        }
    });
    if let Some(routing) = routing {
        params["session_intent"] = serde_json::Value::String(routing.intent().to_string());
        if let Some(target) = routing.session_target() {
            params["session_target"] = target;
        }
        if let Some(idempotency_key) = routing.idempotency_key() {
            params["idempotency_key"] = serde_json::Value::String(idempotency_key.to_string());
        }
    }
    let mut initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "cli-init",
        "method": "control/initialize",
        "params": params
    })
    .to_string();
    write_iroh_control_frame(bridge.stream_mut(), &initialize, policy.idle_timeout).await?;
    let mut response =
        read_persistent_iroh_control_frame(bridge.stream_mut(), policy.idle_timeout).await?;
    if requested_event_stream_version == 2 && iroh_initialize_rejected_event_stream_v2(&response) {
        requested_event_stream_version = 1;
        let mut initialize_value: serde_json::Value = serde_json::from_str(&initialize)
            .map_err(|_| MezError::invalid_state("invalid local Iroh initialize request"))?;
        initialize_value["params"]["event_stream_version"] = serde_json::Value::from(1);
        initialize = initialize_value.to_string();
        write_iroh_control_frame(bridge.stream_mut(), &initialize, policy.idle_timeout).await?;
        response =
            read_persistent_iroh_control_frame(bridge.stream_mut(), policy.idle_timeout).await?;
    }
    let issued_credential = validate_iroh_initialize_response(&response, requested_role)?;
    let client_clipboard_negotiated = iroh_client_clipboard_negotiated(
        &response,
        requested_role,
        requested_event_stream_version,
    )?;
    if let IrohControlTarget::Invitation {
        profile_name,
        server_addr,
        role,
        ..
    } = &target
    {
        let issued_credential = issued_credential.ok_or_else(|| {
            MezError::invalid_state("successful Iroh pairing response omitted device credential")
        })?;
        let server_addr = authenticated_remote_addr(&endpoint, server_addr.id)
            .await
            .unwrap_or_else(|| server_addr.clone());
        RemoteClientProfileStore::under_config_root(paths.root()).save(&RemoteClientProfile {
            name: profile_name.clone(),
            server_addr,
            role: *role,
            scope: target.scope(),
            device_credential: issued_credential,
        })?;
    } else {
        refresh_authenticated_profile_route(paths.root(), &endpoint, &target).await?;
    }
    let (event_receiver, event_task) = super::attach::spawn_iroh_runtime_event_receiver(
        connection.clone(),
        compression,
        policy.setup_timeout,
        requested_event_stream_version,
        client_clipboard_negotiated.then_some(client_clipboard),
    );
    Ok((
        PersistentIrohControlChannel {
            _identity: identity,
            endpoint,
            connection,
            bridge,
            event_receiver: Some(event_receiver),
            event_task,
            setup_timeout: policy.setup_timeout,
        },
        response,
    ))
}

fn resolve_iroh_control_target(
    control_target: &super::ControlTargetSelection,
    config_root: &Path,
) -> Result<IrohControlTarget> {
    match control_target {
        super::ControlTargetSelection::Unix => Err(MezError::invalid_args(
            "persistent Iroh control requires an explicit remote target",
        )),
        super::ControlTargetSelection::IrohProfile(name) => {
            RemoteClientProfileStore::under_config_root(config_root)
                .load(name)?
                .map(IrohControlTarget::Profile)
                .ok_or_else(|| {
                    MezError::new(
                        crate::error::MezErrorKind::NotFound,
                        "Iroh client profile not found",
                    )
                })
        }
        super::ControlTargetSelection::IrohInvitation { path, save_as } => {
            let target = parse_iroh_invitation_file(path, save_as.as_deref())?;
            preflight_iroh_invitation_profile(config_root, &target)?;
            Ok(target)
        }
    }
}

async fn read_persistent_iroh_control_frame<S>(
    stream: &mut S,
    timeout: std::time::Duration,
) -> Result<String>
where
    S: tokio::io::AsyncRead + Unpin,
{
    let response = tokio::time::timeout(
        timeout,
        super::attach::read_async_control_response_frames(
            stream,
            CLI_CONTROL_MAX_CONTENT_LENGTH,
            1,
        ),
    )
    .await
    .map_err(|_| MezError::invalid_state("Iroh attach response timed out"))??;
    decode_control_frame(&response, CLI_CONTROL_MAX_CONTENT_LENGTH).map(|(body, _)| body)
}

/// Exchanges initialization and one request over one client-opened Iroh stream.
pub(crate) async fn exchange_iroh_control_request(
    config_root: &Path,
    configured_policy: &RuntimeIrohTransportPolicy,
    target: &IrohControlTarget,
    method: &str,
    params: &str,
) -> Result<String> {
    exchange_iroh_control_request_as(
        config_root,
        configured_policy,
        target,
        target.role().as_str(),
        true,
        method,
        params,
    )
    .await
}

/// Pairs from one invitation without entering an interactive terminal session.
pub(super) async fn pair_iroh_invitation(
    env: &super::CliEnv,
    path: &Path,
    save_as: Option<&str>,
) -> Result<crate::security::remote::RemoteClientProfileSummary> {
    let paths = env.config_paths()?;
    let layers = super::load_runtime_config_layers(&paths)?;
    let structured = crate::runtime::runtime_effective_config_value(&layers)?;
    let configured_policy = crate::runtime::runtime_iroh_transport_policy_from_config(&structured)?;
    let target = parse_iroh_invitation_file(path, save_as)?;
    preflight_iroh_invitation_profile(paths.root(), &target)?;
    let profile_name = target.profile_name().to_string();
    if target.scope() == RemoteClientProfileScope::Host {
        exchange_iroh_host_only_initialize(paths.root(), &configured_policy, &target).await?;
    } else {
        let params = format!(
            r#"{{"idempotency_key":"{}"}}"#,
            super::cli_idempotency_key("remote-pair-detach")
        );
        let body = exchange_iroh_control_request_as(
            paths.root(),
            &configured_policy,
            &target,
            "observer",
            false,
            "client/detach",
            &params,
        )
        .await?;
        ensure_iroh_follow_up_success(&body, "pairing cleanup")?;
    }
    RemoteClientProfileStore::under_config_root(paths.root())
        .summary(&profile_name)?
        .ok_or_else(|| MezError::invalid_state("successful Iroh pairing did not persist a profile"))
}

/// Authenticates one paired profile and cleans up its temporary observer client.
pub(super) async fn check_iroh_profile(
    env: &super::CliEnv,
    profile_name: &str,
) -> Result<crate::security::remote::RemoteClientProfileSummary> {
    let paths = env.config_paths()?;
    let layers = super::load_runtime_config_layers(&paths)?;
    let structured = crate::runtime::runtime_effective_config_value(&layers)?;
    let configured_policy = crate::runtime::runtime_iroh_transport_policy_from_config(&structured)?;
    let profile = RemoteClientProfileStore::under_config_root(paths.root())
        .load(profile_name)?
        .ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                format!("remote client profile `{profile_name}` was not found"),
            )
        })?;
    let target = IrohControlTarget::Profile(profile);
    if target.scope() == RemoteClientProfileScope::Host {
        exchange_iroh_host_only_initialize(paths.root(), &configured_policy, &target).await?;
    } else {
        let params = format!(
            r#"{{"idempotency_key":"{}"}}"#,
            super::cli_idempotency_key("remote-profile-check-detach")
        );
        let body = exchange_iroh_control_request_as(
            paths.root(),
            &configured_policy,
            &target,
            "observer",
            false,
            "client/detach",
            &params,
        )
        .await?;
        ensure_iroh_follow_up_success(&body, "profile check cleanup")?;
    }
    RemoteClientProfileStore::under_config_root(paths.root())
        .summary(profile_name)?
        .ok_or_else(|| MezError::invalid_state("authenticated Iroh profile disappeared"))
}

/// Lists sessions visible to one paired host profile without selecting a session.
pub(super) async fn list_iroh_host_sessions(
    control_target: &super::ControlTargetSelection,
    env: &super::CliEnv,
) -> Result<String> {
    exchange_iroh_host_request(
        control_target,
        env,
        "host/session/list",
        serde_json::json!({}),
        "session-list",
        "session list",
    )
    .await
}

/// Force-kills one visible hosted session through a separately authorized
/// host-only operation without attaching a terminal client.
pub(super) async fn force_kill_iroh_host_session(
    control_target: &super::ControlTargetSelection,
    env: &super::CliEnv,
    target: &str,
) -> Result<String> {
    exchange_iroh_host_request(
        control_target,
        env,
        "host/session/kill",
        serde_json::json!({
            "target": target,
            "force": true,
            "idempotency_key": super::cli_idempotency_key("remote-session-kill"),
        }),
        "session-kill",
        "session kill",
    )
    .await
}

async fn exchange_iroh_host_request(
    control_target: &super::ControlTargetSelection,
    env: &super::CliEnv,
    method: &str,
    params: serde_json::Value,
    purpose: &str,
    operation: &str,
) -> Result<String> {
    let paths = env.config_paths()?;
    let layers = super::load_runtime_config_layers(&paths)?;
    let structured = crate::runtime::runtime_effective_config_value(&layers)?;
    let configured_policy = crate::runtime::runtime_iroh_transport_policy_from_config(&structured)?;
    let target = resolve_iroh_control_target(control_target, paths.root())?;
    if target.scope() != RemoteClientProfileScope::Host {
        return Err(MezError::invalid_args(
            "remote session listing requires a host-scoped Iroh profile",
        ));
    }
    let policy = explicit_iroh_client_policy(&configured_policy, &target)?;
    if let IrohControlTarget::Invitation {
        expires_at_unix_seconds,
        ..
    } = &target
        && current_unix_seconds_for_iroh_client()? > *expires_at_unix_seconds
    {
        return Err(MezError::forbidden(
            "Iroh pairing invitation expired before connection setup",
        ));
    }
    let identity = RemoteClientIdentity::load_or_create(paths.root())?;
    let endpoint =
        bind_runtime_iroh_client_endpoint(&policy, identity.secret_key().clone()).await?;
    let result = exchange_bound_iroh_host_request(
        paths.root(),
        &policy,
        &target,
        &endpoint,
        method,
        params,
        purpose,
        operation,
    )
    .await;
    let _ = tokio::time::timeout(policy.setup_timeout, endpoint.close()).await;
    result
}

#[allow(
    clippy::too_many_arguments,
    reason = "host request transport, method payload, client purpose, and diagnostics are independent inputs"
)]
async fn exchange_bound_iroh_host_request(
    config_root: &Path,
    policy: &RuntimeIrohTransportPolicy,
    target: &IrohControlTarget,
    endpoint: &iroh::Endpoint,
    method: &str,
    params: serde_json::Value,
    purpose: &str,
    operation: &str,
) -> Result<String> {
    let (connection, compression) = connect_iroh_with_compression(endpoint, policy, target).await?;
    if connection.remote_id() != target.server_addr().id {
        return Err(MezError::forbidden(
            "Iroh connection authenticated an unexpected server identity",
        ));
    }
    let (send, recv) = tokio::time::timeout(policy.setup_timeout, connection.open_bi())
        .await
        .map_err(|_| MezError::invalid_state("Iroh host list stream setup timed out"))?
        .map_err(|_| MezError::invalid_state("failed to open Iroh host list stream"))?;
    let mut bridge =
        IrohCompressionBridge::spawn(recv, send, compression, CLI_CONTROL_MAX_CONTENT_LENGTH)?;
    let (mechanism, credential) = target.authentication();
    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "cli-init",
        "method": "control/initialize",
        "params": {
            "client_name": "remote-cli",
            "requested_version": 3,
            "requested_role": "observer",
            "session_intent": "host_only",
            "client": {
                "name": "remote-cli",
                "interactive": false,
                "purpose": purpose
            },
            "authentication": {
                "mechanism": mechanism,
                "token": credential.expose_secret()
            }
        }
    })
    .to_string();
    write_iroh_control_frame(bridge.stream_mut(), &initialize, policy.idle_timeout).await?;
    let initialize_body = read_iroh_control_frame(bridge.stream_mut(), policy.idle_timeout).await?;
    let issued_credential = validate_iroh_host_only_initialize_response(&initialize_body)?;
    if let IrohControlTarget::Invitation {
        profile_name,
        server_addr,
        role,
        ..
    } = target
    {
        let issued_credential = issued_credential.ok_or_else(|| {
            MezError::invalid_state("successful host pairing response omitted device credential")
        })?;
        let server_addr = authenticated_remote_addr(endpoint, server_addr.id)
            .await
            .unwrap_or_else(|| server_addr.clone());
        RemoteClientProfileStore::under_config_root(config_root).save(&RemoteClientProfile {
            name: profile_name.clone(),
            server_addr,
            role: *role,
            scope: RemoteClientProfileScope::Host,
            device_credential: issued_credential,
        })?;
    } else {
        refresh_authenticated_profile_route(config_root, endpoint, target).await?;
    }
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "cli",
        "method": method,
        "params": params
    })
    .to_string();
    write_iroh_control_frame(bridge.stream_mut(), &request, policy.idle_timeout).await?;
    tokio::io::AsyncWriteExt::shutdown(bridge.stream_mut())
        .await
        .map_err(|_| MezError::invalid_state("failed to finish Iroh host request"))?;
    let body = read_iroh_control_frame(bridge.stream_mut(), policy.idle_timeout).await?;
    ensure_iroh_follow_up_success(&body, operation)?;
    bridge.shutdown(policy.setup_timeout).await?;
    connection.close(
        iroh::endpoint::VarInt::from_u32(0),
        b"host request complete",
    );
    Ok(body)
}

/// Authenticates one host-scoped target without selecting or creating a session.
async fn exchange_iroh_host_only_initialize(
    config_root: &Path,
    configured_policy: &RuntimeIrohTransportPolicy,
    target: &IrohControlTarget,
) -> Result<()> {
    if target.scope() != RemoteClientProfileScope::Host {
        return Err(MezError::invalid_args(
            "host-only initialization requires a host-scoped Iroh target",
        ));
    }
    let policy = explicit_iroh_client_policy(configured_policy, target)?;
    if let IrohControlTarget::Invitation {
        expires_at_unix_seconds,
        ..
    } = target
        && current_unix_seconds_for_iroh_client()? > *expires_at_unix_seconds
    {
        return Err(MezError::forbidden(
            "Iroh pairing invitation expired before connection setup",
        ));
    }
    let identity = RemoteClientIdentity::load_or_create(config_root)?;
    let endpoint =
        bind_runtime_iroh_client_endpoint(&policy, identity.secret_key().clone()).await?;
    let result =
        exchange_bound_iroh_host_only_initialize(config_root, &policy, target, &endpoint).await;
    let _ = tokio::time::timeout(policy.setup_timeout, endpoint.close()).await;
    result
}

async fn exchange_bound_iroh_host_only_initialize(
    config_root: &Path,
    policy: &RuntimeIrohTransportPolicy,
    target: &IrohControlTarget,
    endpoint: &iroh::Endpoint,
) -> Result<()> {
    let (connection, compression) = connect_iroh_with_compression(endpoint, policy, target).await?;
    if connection.remote_id() != target.server_addr().id {
        return Err(MezError::forbidden(
            "Iroh connection authenticated an unexpected server identity",
        ));
    }
    let (send, recv) = tokio::time::timeout(policy.setup_timeout, connection.open_bi())
        .await
        .map_err(|_| MezError::invalid_state("Iroh host-only stream setup timed out"))?
        .map_err(|_| MezError::invalid_state("failed to open Iroh host-only stream"))?;
    let mut bridge =
        IrohCompressionBridge::spawn(recv, send, compression, CLI_CONTROL_MAX_CONTENT_LENGTH)?;
    let (mechanism, credential) = target.authentication();
    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "cli-init",
        "method": "control/initialize",
        "params": {
            "client_name": "remote-cli",
            "requested_version": 3,
            "requested_role": "observer",
            "session_intent": "host_only",
            "client": {
                "name": "remote-cli",
                "interactive": false,
                "purpose": "pairing-or-connectivity-check"
            },
            "authentication": {
                "mechanism": mechanism,
                "token": credential.expose_secret()
            }
        }
    })
    .to_string();
    write_iroh_control_frame(bridge.stream_mut(), &initialize, policy.idle_timeout).await?;
    tokio::io::AsyncWriteExt::shutdown(bridge.stream_mut())
        .await
        .map_err(|_| MezError::invalid_state("failed to finish Iroh host-only request"))?;
    let response = read_iroh_control_frame(bridge.stream_mut(), policy.idle_timeout).await?;
    let issued_credential = validate_iroh_host_only_initialize_response(&response)?;
    if let IrohControlTarget::Invitation {
        profile_name,
        server_addr,
        role,
        ..
    } = target
    {
        let issued_credential = issued_credential.ok_or_else(|| {
            MezError::invalid_state("successful host pairing response omitted device credential")
        })?;
        let server_addr = authenticated_remote_addr(endpoint, server_addr.id)
            .await
            .unwrap_or_else(|| server_addr.clone());
        RemoteClientProfileStore::under_config_root(config_root).save(&RemoteClientProfile {
            name: profile_name.clone(),
            server_addr,
            role: *role,
            scope: RemoteClientProfileScope::Host,
            device_credential: issued_credential,
        })?;
    } else {
        refresh_authenticated_profile_route(config_root, endpoint, target).await?;
    }
    bridge.shutdown(policy.setup_timeout).await?;
    connection.close(iroh::endpoint::VarInt::from_u32(0), b"host-only complete");
    Ok(())
}

/// Rejects a JSON-RPC follow-up error after successful Iroh authentication.
fn ensure_iroh_follow_up_success(body: &str, operation: &str) -> Result<()> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| {
        MezError::invalid_state(format!("Iroh {operation} response is invalid JSON"))
    })?;
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("remote control request was rejected");
        return Err(MezError::invalid_state(format!(
            "Iroh {operation} failed: {message}"
        )));
    }
    Ok(())
}

/// Exchanges initialization at one permitted role and one follow-up request.
async fn exchange_iroh_control_request_as(
    config_root: &Path,
    configured_policy: &RuntimeIrohTransportPolicy,
    target: &IrohControlTarget,
    requested_role: &str,
    client_interactive: bool,
    method: &str,
    params: &str,
) -> Result<String> {
    let policy = explicit_iroh_client_policy(configured_policy, target)?;
    ensure_iroh_attach_role_allowed(target.role(), requested_role)?;
    if let IrohControlTarget::Invitation {
        expires_at_unix_seconds,
        ..
    } = target
        && current_unix_seconds_for_iroh_client()? > *expires_at_unix_seconds
    {
        return Err(MezError::forbidden(
            "Iroh pairing invitation expired before connection setup",
        ));
    }
    let request_params: serde_json::Value = serde_json::from_str(params).map_err(|error| {
        MezError::invalid_args(format!("invalid control request params: {error}"))
    })?;
    let identity = RemoteClientIdentity::load_or_create(config_root)?;
    let endpoint =
        bind_runtime_iroh_client_endpoint(&policy, identity.secret_key().clone()).await?;
    let exchange = exchange_bound_iroh_control_request(
        config_root,
        &policy,
        target,
        IrohClientInitialization {
            requested_role,
            interactive: client_interactive,
        },
        method,
        request_params,
        &endpoint,
    )
    .await;
    let _ = tokio::time::timeout(policy.setup_timeout, endpoint.close()).await;
    exchange
}

#[cfg(test)]
mod outbound_policy_tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::time::Duration;

    use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
    use crate::host::iroh::HostIrohRuntime;
    use crate::host::router::{
        HostDefaultSessionPolicy, HostRecoveryPolicy, HostSessionRouter, HostSessionRouterConfig,
    };
    use crate::host::shell::{ResolvedShell, ShellSource};
    use crate::security::remote::{
        RemoteHostRoutingAuthority, RemoteSessionAttachScope, RemoteTrustStore,
    };

    use super::*;

    /// Allows local Iroh endpoint creation to tolerate parallel workspace load
    /// without weakening the production transport setup deadline.
    const COMPRESSION_CONNECTOR_TEST_SETUP_TIMEOUT: Duration = Duration::from_secs(10);

    async fn v1_only_server() -> (iroh::Endpoint, EndpointAddr) {
        let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(iroh::SecretKey::generate())
            .alpns(vec![crate::runtime::MEZZANINE_IROH_ALPN.to_vec()])
            .relay_mode(iroh::RelayMode::Disabled)
            .clear_address_lookup()
            .bind()
            .await
            .unwrap();
        let addr = server.addr();
        (server, addr)
    }

    fn invitation_target(server_addr: EndpointAddr) -> IrohControlTarget {
        IrohControlTarget::Invitation {
            profile_name: "remote".to_string(),
            server_addr,
            token: SecretString::from("secret".to_string()),
            role: RemoteRoleCeiling::Observer,
            scope: RemoteClientProfileScope::LegacySession,
            expires_at_unix_seconds: u64::MAX,
        }
    }

    /// A fresh host invitation must redeem without session authority, persist
    /// device proof, and reconnect before either create or attach is routed.
    #[tokio::test(flavor = "current_thread")]
    async fn host_invitation_pairs_then_reconnects_for_create_and_attach() {
        let root = std::env::temp_dir().join(format!(
            "mez-cli-two-step-pairing-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let host_config_root = root.join("host-config");
        fs::create_dir_all(&host_config_root).unwrap();
        fs::set_permissions(&host_config_root, fs::Permissions::from_mode(0o700)).unwrap();
        let policy = RuntimeIrohTransportPolicy {
            enabled: true,
            identity: crate::runtime::RuntimeIrohIdentityPolicy::Host,
            compression_codecs: vec![crate::runtime::RuntimeIrohCompressionCodec::None],
            setup_timeout: std::time::Duration::from_secs(3),
            idle_timeout: std::time::Duration::from_secs(3),
            ..RuntimeIrohTransportPolicy::default()
        };
        let host = HostIrohRuntime::bind(&host_config_root, policy)
            .await
            .unwrap()
            .unwrap();
        let router = HostSessionRouter::new(HostSessionRouterConfig {
            runtime_root: root.join("runtime"),
            owner_uid: crate::runtime::current_effective_uid(),
            config_root: host_config_root.clone(),
            config_layers: vec![ConfigLayer {
                name: "two-step-pairing-test".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: "[agents]\nshell_mode = \"pane\"\n[permissions]\nsandbox = \"policy-only\"\n"
                    .to_string(),
            }],
            shell: ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
            max_sessions: 8,
            max_live_sessions: 8,
            recovery_policy: HostRecoveryPolicy::Lazy,
            default_session_policy: HostDefaultSessionPolicy::MostRecentAttachable,
            default_lease_lifetime_seconds: 0,
        });
        let trust = RemoteTrustStore::under_host_config_root(&host_config_root).unwrap();
        let now = super::current_unix_seconds_for_iroh_client().unwrap();
        let create_invitation = trust
            .create_host_invitation(
                host.endpoint_id(),
                RemoteRoleCeiling::Primary,
                RemoteHostRoutingAuthority {
                    session_create: true,
                    session_kill: false,
                    session_list: true,
                    session_attach_scope: RemoteSessionAttachScope::Own,
                    max_active_leases: 2,
                    max_live_sessions: 2,
                    lease_lifetime_ceiling_seconds: None,
                },
                600,
                now,
            )
            .unwrap();
        let attach_invitation = trust
            .create_host_invitation(
                host.endpoint_id(),
                RemoteRoleCeiling::Observer,
                RemoteHostRoutingAuthority {
                    session_create: false,
                    session_kill: false,
                    session_list: true,
                    session_attach_scope: RemoteSessionAttachScope::All,
                    max_active_leases: 0,
                    max_live_sessions: 0,
                    lease_lifetime_ceiling_seconds: None,
                },
                600,
                now,
            )
            .unwrap();
        let server_addr = host.endpoint_addr().unwrap();
        let stop = std::sync::Arc::new(tokio::sync::Notify::new());
        let server_stop = stop.clone();
        let server_router = router.clone();
        let server = host.serve_routed(server_router, async move { server_stop.notified().await });

        let client_work = async {
            let (create_env, create_target) =
                invitation_client_fixture(&root, "creator", &server_addr, &create_invitation);
            let create_routing = IrohSessionRouting::Create {
                name: Some("from-invitation".to_string()),
                idempotency_key: "two-step-create".to_string(),
            };
            let (mut create_channel, create_response) = open_persistent_iroh_control_channel(
                &create_target,
                &create_env,
                "primary",
                Some(&create_routing),
                80,
                24,
                "xterm-256color",
            )
            .await
            .unwrap();
            let create_response: serde_json::Value =
                serde_json::from_str(&create_response).unwrap();
            let session_id = create_response["result"]["lease"]["session_id"]
                .as_str()
                .unwrap()
                .to_string();
            let lease_id = create_response["result"]["lease"]["lease_id"]
                .as_str()
                .unwrap()
                .to_string();
            assert_eq!(
                RemoteClientProfileStore::under_config_root(
                    create_env.config_paths().unwrap().root()
                )
                .load("creator")
                .unwrap()
                .unwrap()
                .scope,
                RemoteClientProfileScope::Host
            );
            let view_request = r#"{"jsonrpc":"2.0","id":"host-routed-view","method":"terminal/view","params":{"client_size":{"columns":80,"rows":24}}}"#;
            tokio::io::AsyncWriteExt::write_all(
                create_channel.stream_mut(),
                &encode_control_body(view_request),
            )
            .await
            .unwrap();
            tokio::io::AsyncWriteExt::flush(create_channel.stream_mut())
                .await
                .unwrap();
            let create_view = read_persistent_iroh_control_frame(
                create_channel.stream_mut(),
                std::time::Duration::from_secs(3),
            )
            .await
            .unwrap();
            let create_view: serde_json::Value = serde_json::from_str(&create_view).unwrap();
            assert_eq!(
                create_view["result"]["view"]["iroh_status_slot"]["width"],
                crate::host::terminal::TERMINAL_IROH_STATUS_SLOT_WIDTH,
                "{create_view}"
            );

            let (attach_env, attach_target) =
                invitation_client_fixture(&root, "attacher", &server_addr, &attach_invitation);
            let attach_routing = IrohSessionRouting::Attach { target: session_id };
            let (mut attach_channel, attach_response) = open_persistent_iroh_control_channel(
                &attach_target,
                &attach_env,
                "observer",
                Some(&attach_routing),
                80,
                24,
                "xterm-256color",
            )
            .await
            .unwrap();
            let attach_response: serde_json::Value =
                serde_json::from_str(&attach_response).unwrap();
            assert_eq!(attach_response["result"]["lease"]["lease_id"], lease_id);
            let view_request = r#"{"jsonrpc":"2.0","id":"host-routed-view","method":"terminal/view","params":{"client_size":{"columns":80,"rows":24}}}"#;
            tokio::io::AsyncWriteExt::write_all(
                attach_channel.stream_mut(),
                &encode_control_body(view_request),
            )
            .await
            .unwrap();
            tokio::io::AsyncWriteExt::flush(attach_channel.stream_mut())
                .await
                .unwrap();
            let attach_view = read_persistent_iroh_control_frame(
                attach_channel.stream_mut(),
                std::time::Duration::from_secs(3),
            )
            .await
            .unwrap();
            let attach_view: serde_json::Value = serde_json::from_str(&attach_view).unwrap();
            assert_eq!(
                attach_view["result"]["view"]["iroh_status_slot"]["width"],
                crate::host::terminal::TERMINAL_IROH_STATUS_SLOT_WIDTH,
                "{attach_view}"
            );
            attach_channel.close().await;
            create_channel.close().await;
            stop.notify_one();
        };

        let (served, ()) = tokio::join!(server, client_work);
        assert!(
            served.unwrap() >= 4,
            "pairing and routed create/attach require at least four connections"
        );
        assert_eq!(trust.list_records().unwrap().len(), 2);
        assert_eq!(router.snapshots().await.unwrap().len(), 1);
        router
            .shutdown_all(true, std::time::Duration::from_secs(2))
            .await
            .unwrap();
        drop(host);
        let _ = fs::remove_dir_all(root);
    }

    fn invitation_client_fixture(
        root: &Path,
        name: &str,
        server_addr: &EndpointAddr,
        invitation: &crate::security::remote::RemotePairingInvitation,
    ) -> (crate::cli::CliEnv, crate::cli::ControlTargetSelection) {
        let home = root.join(name);
        let runtime = home.join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let path = home.join("invitation.json");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "format_version": 1,
                "profile_scope": "host",
                "profile_name": name,
                "server_endpoint_id": invitation.server_endpoint_id,
                "server_addr": server_addr,
                "token": invitation.token.expose_secret(),
                "role": invitation.role_ceiling.as_str(),
                "expires_at_unix_seconds": invitation.expires_at_unix_seconds,
            }))
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        (
            crate::cli::CliEnv {
                home: Some(home),
                shell: Some(std::ffi::OsString::from("/bin/sh")),
                mez: None,
                runtime: crate::runtime::RuntimeEnv {
                    mez_tmpdir: Some(runtime.into_os_string()),
                    xdg_runtime_dir: None,
                    tmpdir: None,
                    uid: crate::runtime::effective_uid_for_tests(),
                },
            },
            crate::cli::ControlTargetSelection::IrohInvitation {
                path,
                save_as: None,
            },
        )
    }

    /// Verifies a new client may try compressed ALPNs and then select the
    /// explicitly configured v1 `none` route before any stream is opened.
    #[tokio::test(flavor = "current_thread")]
    async fn compression_connector_falls_back_to_v1_before_stream_open() {
        let (server, server_addr) = v1_only_server().await;
        let target = invitation_target(server_addr);
        let policy = RuntimeIrohTransportPolicy {
            setup_timeout: COMPRESSION_CONNECTOR_TEST_SETUP_TIMEOUT,
            ..RuntimeIrohTransportPolicy::default()
        };
        let client = bind_runtime_iroh_client_endpoint(&policy, iroh::SecretKey::generate())
            .await
            .unwrap();
        let server_task = tokio::spawn({
            let server = server.clone();
            async move {
                loop {
                    let incoming = server.accept().await.unwrap();
                    let Ok(accepting) = incoming.accept() else {
                        continue;
                    };
                    if let Ok(connection) = accepting.await {
                        return connection;
                    }
                }
            }
        });

        let (connection, compression) = connect_iroh_with_compression(&client, &policy, &target)
            .await
            .unwrap();

        assert_eq!(
            compression.codec(),
            crate::runtime::RuntimeIrohCompressionCodec::None
        );
        connection.close(iroh::endpoint::VarInt::from_u32(0), b"test complete");
        let server_connection =
            tokio::time::timeout(std::time::Duration::from_secs(2), server_task)
                .await
                .unwrap()
                .unwrap();
        server_connection.close(iroh::endpoint::VarInt::from_u32(0), b"test complete");
        client.close().await;
        server.close().await;
    }

    /// Verifies a client without `none` fails closed against a v1-only peer
    /// instead of inventing an unconfigured downgrade route.
    #[tokio::test(flavor = "current_thread")]
    async fn compression_connector_rejects_v1_without_none() {
        let (server, server_addr) = v1_only_server().await;
        let target = invitation_target(server_addr);
        let policy = RuntimeIrohTransportPolicy {
            compression_codecs: vec![crate::runtime::RuntimeIrohCompressionCodec::Zstd],
            setup_timeout: COMPRESSION_CONNECTOR_TEST_SETUP_TIMEOUT,
            ..RuntimeIrohTransportPolicy::default()
        };
        let client = bind_runtime_iroh_client_endpoint(&policy, iroh::SecretKey::generate())
            .await
            .unwrap();
        let server_task = tokio::spawn({
            let server = server.clone();
            async move {
                let incoming = server.accept().await.unwrap();
                match incoming.accept() {
                    Ok(accepting) => accepting.await.is_err(),
                    Err(_) => true,
                }
            }
        });

        let error = connect_iroh_with_compression(&client, &policy, &target)
            .await
            .unwrap_err();

        assert!(error.message().contains("ALPN"), "{error:?}");
        assert!(server_task.await.unwrap());
        client.close().await;
        server.close().await;
    }

    /// Verifies an explicit direct target enables only its pinned IP route even
    /// when the local listener remains disabled by default.
    #[test]
    fn explicit_direct_target_derives_client_only_policy() {
        let configured = RuntimeIrohTransportPolicy::default();
        let target = invitation_target(
            EndpointAddr::new(iroh::SecretKey::generate().public())
                .with_ip_addr("127.0.0.1:47000".parse().unwrap()),
        );

        let policy = explicit_iroh_client_policy(&configured, &target).unwrap();

        assert!(!configured.enabled);
        assert!(!policy.enabled);
        assert!(policy.direct_connections);
        assert!(matches!(
            policy.relay,
            crate::runtime::RuntimeIrohRelayPolicy::Disabled
        ));
        assert!(matches!(
            policy.address_lookup,
            crate::runtime::RuntimeIrohAddressLookupPolicy::Disabled
        ));
        assert!(!policy.port_mapping);
    }

    /// Verifies an explicit relay-only target enables exactly its pinned relay
    /// without enabling direct IP paths or implicit endpoint lookup.
    #[test]
    fn explicit_relay_target_derives_relay_only_policy() {
        let configured = RuntimeIrohTransportPolicy::default();
        let target = invitation_target(
            EndpointAddr::new(iroh::SecretKey::generate().public())
                .with_relay_url("https://relay.example".parse().unwrap()),
        );

        let policy = explicit_iroh_client_policy(&configured, &target).unwrap();

        assert!(!policy.direct_connections);
        assert_eq!(
            policy.relay,
            crate::runtime::RuntimeIrohRelayPolicy::Custom {
                urls: vec!["https://relay.example/".to_string()]
            }
        );
        assert!(matches!(
            policy.address_lookup,
            crate::runtime::RuntimeIrohAddressLookupPolicy::Disabled
        ));
        assert!(!policy.port_mapping);
    }

    /// Verifies administrators can disable every explicit outbound Iroh target
    /// independently of listener policy.
    #[test]
    fn explicit_target_respects_outbound_opt_out() {
        let configured = RuntimeIrohTransportPolicy {
            outbound_enabled: false,
            ..RuntimeIrohTransportPolicy::default()
        };
        let target = invitation_target(
            EndpointAddr::new(iroh::SecretKey::generate().public())
                .with_ip_addr("127.0.0.1:47000".parse().unwrap()),
        );

        let error = explicit_iroh_client_policy(&configured, &target).unwrap_err();

        assert_eq!(error.kind(), crate::error::MezErrorKind::Config);
        assert!(error.message().contains("outbound_enabled"));
    }

    /// Verifies an explicitly configured lookup service remains available for
    /// a paired profile so stale route hints can be refreshed by endpoint ID.
    #[test]
    fn explicit_profile_preserves_configured_address_lookup() {
        let configured = RuntimeIrohTransportPolicy {
            address_lookup: crate::runtime::RuntimeIrohAddressLookupPolicy::N0Dns,
            relay: crate::runtime::RuntimeIrohRelayPolicy::Public,
            ..RuntimeIrohTransportPolicy::default()
        };
        let profile = RemoteClientProfile {
            name: "remote".to_string(),
            server_addr: EndpointAddr::new(iroh::SecretKey::generate().public())
                .with_ip_addr("192.0.2.10:4242".parse().unwrap()),
            role: RemoteRoleCeiling::Observer,
            scope: RemoteClientProfileScope::LegacySession,
            device_credential: SecretString::from("credential".to_string()),
        };

        let policy =
            explicit_iroh_client_policy(&configured, &IrohControlTarget::Profile(profile)).unwrap();

        assert!(matches!(
            policy.address_lookup,
            crate::runtime::RuntimeIrohAddressLookupPolicy::N0Dns
        ));
        assert!(matches!(
            policy.relay,
            crate::runtime::RuntimeIrohRelayPolicy::Public
        ));
    }
}

/// Client role and presentation metadata sent during one-shot Iroh initialization.
#[derive(Debug, Clone, Copy)]
struct IrohClientInitialization<'a> {
    requested_role: &'a str,
    interactive: bool,
}

async fn exchange_bound_iroh_control_request(
    config_root: &Path,
    policy: &RuntimeIrohTransportPolicy,
    target: &IrohControlTarget,
    initialization: IrohClientInitialization<'_>,
    method: &str,
    params: serde_json::Value,
    endpoint: &iroh::Endpoint,
) -> Result<String> {
    let (connection, compression) = connect_iroh_with_compression(endpoint, policy, target).await?;
    if connection.remote_id() != target.server_addr().id {
        return Err(MezError::forbidden(
            "Iroh connection authenticated an unexpected server identity",
        ));
    }
    let (send, recv) = tokio::time::timeout(policy.setup_timeout, connection.open_bi())
        .await
        .map_err(|_| MezError::invalid_state("Iroh control stream setup timed out"))?
        .map_err(|_| MezError::invalid_state("failed to open Iroh control stream"))?;
    let mut bridge =
        IrohCompressionBridge::spawn(recv, send, compression, CLI_CONTROL_MAX_CONTENT_LENGTH)?;

    let (mechanism, credential) = target.authentication();
    let client = if initialization.interactive {
        serde_json::json!({
            "name": "remote-cli",
            "interactive": true,
            "terminal": {
                "columns": 80,
                "rows": 24,
                "term": "xterm-256color"
            }
        })
    } else {
        serde_json::json!({
            "name": "remote-cli",
            "interactive": false,
            "purpose": "pairing-or-connectivity-check"
        })
    };
    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "cli-init",
        "method": "control/initialize",
        "params": {
            "client_name": "remote-cli",
            "requested_version": 2,
            "requested_role": initialization.requested_role,
            "detach_primary_on_disconnect": initialization.requested_role == "primary",
            "client": client,
            "authentication": {
                "mechanism": mechanism,
                "token": credential.expose_secret()
            }
        }
    })
    .to_string();
    write_iroh_control_frame(bridge.stream_mut(), &initialize, policy.idle_timeout).await?;
    let initialize_body = read_iroh_control_frame(bridge.stream_mut(), policy.idle_timeout).await?;
    let issued_credential =
        validate_iroh_initialize_response(&initialize_body, initialization.requested_role)?;

    if let IrohControlTarget::Invitation {
        profile_name,
        server_addr,
        role,
        ..
    } = target
    {
        let issued_credential = issued_credential.ok_or_else(|| {
            MezError::invalid_state("successful Iroh pairing response omitted device credential")
        })?;
        let server_addr = authenticated_remote_addr(endpoint, server_addr.id)
            .await
            .unwrap_or_else(|| server_addr.clone());
        RemoteClientProfileStore::under_config_root(config_root).save(&RemoteClientProfile {
            name: profile_name.clone(),
            server_addr,
            role: *role,
            scope: RemoteClientProfileScope::LegacySession,
            device_credential: issued_credential,
        })?;
    } else {
        refresh_authenticated_profile_route(config_root, endpoint, target).await?;
    }

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "cli",
        "method": method,
        "params": params,
    })
    .to_string();
    write_iroh_control_frame(bridge.stream_mut(), &request, policy.idle_timeout).await?;
    tokio::io::AsyncWriteExt::shutdown(bridge.stream_mut())
        .await
        .map_err(|_| MezError::invalid_state("failed to finish Iroh control request stream"))?;
    let body = read_iroh_control_frame(bridge.stream_mut(), policy.idle_timeout).await?;
    let trailing = tokio::time::timeout(
        policy.setup_timeout,
        tokio::io::AsyncReadExt::read_to_end(
            bridge.stream_mut(),
            &mut Vec::with_capacity(CLI_CONTROL_MAX_CONTENT_LENGTH),
        ),
    )
    .await
    .map_err(|_| MezError::invalid_state("Iroh final response acknowledgement timed out"))?
    .map_err(|_| MezError::invalid_state("failed to drain Iroh control response stream"))?;
    if trailing != 0 {
        return Err(MezError::invalid_state(
            "Iroh server sent unexpected data after the final control response",
        ));
    }
    bridge.shutdown(policy.setup_timeout).await?;
    connection.close(iroh::endpoint::VarInt::from_u32(0), b"control complete");
    Ok(body)
}

async fn write_iroh_control_frame<S>(
    send: &mut S,
    body: &str,
    timeout: std::time::Duration,
) -> Result<()>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(
        timeout,
        tokio::io::AsyncWriteExt::write_all(send, &encode_control_body(body)),
    )
    .await
    .map_err(|_| MezError::invalid_state("Iroh control write timed out"))?
    .map_err(|_| MezError::invalid_state("Iroh control write failed"))
}

async fn read_iroh_control_frame<S>(recv: &mut S, timeout: std::time::Duration) -> Result<String>
where
    S: tokio::io::AsyncRead + Unpin,
{
    tokio::time::timeout(timeout, async {
        let mut response = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let read = tokio::io::AsyncReadExt::read(recv, &mut buffer)
                .await
                .map_err(|_| MezError::invalid_state("Iroh control read failed"))?;
            if read == 0 {
                return Err(incomplete_control_response_error(
                    &response,
                    CLI_CONTROL_MAX_CONTENT_LENGTH,
                    1,
                ));
            }
            response.extend_from_slice(&buffer[..read]);
            if response.len() > CLI_CONTROL_MAX_CONTENT_LENGTH + 8192 {
                return Err(MezError::invalid_state("control response exceeds limit"));
            }
            if let Ok((body, consumed)) =
                decode_control_frame(&response, CLI_CONTROL_MAX_CONTENT_LENGTH)
            {
                if consumed != response.len() {
                    return Err(MezError::invalid_state(
                        "Iroh server sent more than one response frame at once",
                    ));
                }
                return Ok(body);
            }
        }
    })
    .await
    .map_err(|_| MezError::invalid_state("Iroh control read timed out"))?
}

/// Connects with configured codec preference before any application stream is opened.
///
/// A failed connection or ALPN attempt may advance to the next configured
/// codec. Once this function returns a connection, callers must not downgrade
/// because opening a stream or writing initialization data makes the outcome
/// potentially ambiguous.
async fn connect_iroh_with_compression(
    endpoint: &iroh::Endpoint,
    policy: &RuntimeIrohTransportPolicy,
    target: &IrohControlTarget,
) -> Result<(iroh::endpoint::Connection, IrohCompressionPolicy)> {
    let max_decoded_bytes = CLI_CONTROL_MAX_CONTENT_LENGTH
        .checked_add(1024)
        .ok_or_else(|| MezError::invalid_state("Iroh control frame limit overflow"))?;
    let mut last_error = None;
    let mut timed_out = false;
    for codec in &policy.compression_codecs {
        match tokio::time::timeout(
            policy.setup_timeout,
            endpoint.connect(target.server_addr().clone(), codec.alpn()),
        )
        .await
        {
            Ok(Ok(connection)) => {
                let compression = IrohCompressionPolicy::new(
                    *codec,
                    policy.compression_min_bytes,
                    policy.compression_zstd_level,
                    max_decoded_bytes,
                )?;
                return Ok((connection, compression));
            }
            Ok(Err(error)) => last_error = Some(error),
            Err(_) => timed_out = true,
        }
    }
    if timed_out {
        return Err(iroh_setup_timeout_error(
            policy,
            target,
            "codec negotiation and connection setup",
        ));
    }
    Err(last_error.map(iroh_connect_error).unwrap_or_else(|| {
        MezError::invalid_state("Iroh compression policy contains no usable codec")
    }))
}

fn ensure_iroh_attach_role_allowed(
    role_ceiling: RemoteRoleCeiling,
    requested_role: &str,
) -> Result<()> {
    match (role_ceiling, requested_role) {
        (RemoteRoleCeiling::Observer, "observer")
        | (RemoteRoleCeiling::Primary, "observer" | "primary") => Ok(()),
        _ => Err(MezError::forbidden(
            "Iroh profile role ceiling does not permit the requested attach role",
        )),
    }
}

fn validate_iroh_initialize_response(
    body: &str,
    requested_role: &str,
) -> Result<Option<SecretString>> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("invalid Iroh initialize response"))?;
    if value.get("error").is_some() {
        return Err(MezError::forbidden(
            "Iroh transport connected, but Mezzanine trust initialization was rejected",
        ));
    }
    let result = value
        .get("result")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_state("Iroh initialize response omitted result"))?;
    let expected_grant = match requested_role {
        "primary" => "primary",
        "observer" => "observer",
        _ => return Err(MezError::invalid_args("unsupported Iroh requested role")),
    };
    if result
        .get("granted_role")
        .and_then(serde_json::Value::as_str)
        != Some(expected_grant)
    {
        return Err(MezError::forbidden(
            "Iroh initialization granted an unexpected remote role",
        ));
    }
    Ok(result
        .get("device_credential")
        .and_then(serde_json::Value::as_str)
        .map(|credential| SecretString::from(credential.to_string())))
}

/// Returns whether a failed initialize response permits a same-connection v1 retry.
fn iroh_initialize_rejected_event_stream_v2(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("error").cloned())
        .and_then(|error| error.get("message").cloned())
        .and_then(|message| message.as_str().map(str::to_string))
        .is_some_and(|message| message == "unsupported event stream version")
}

/// Returns whether client clipboard effects were explicitly negotiated.
fn iroh_client_clipboard_negotiated(
    body: &str,
    requested_role: &str,
    event_stream_version: u32,
) -> Result<bool> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("invalid Iroh initialize response"))?;
    let result = value
        .get("result")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_state("Iroh initialize response omitted result"))?;
    let granted_role = result
        .get("granted_role")
        .and_then(serde_json::Value::as_str);
    let clipboard_capable = result
        .get("capabilities")
        .and_then(|capabilities| capabilities.get("features"))
        .and_then(|features| features.get("client_clipboard_write"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Ok(event_stream_version == 2
        && requested_role == "primary"
        && granted_role == Some("primary")
        && clipboard_capable)
}

fn validate_iroh_host_only_initialize_response(body: &str) -> Result<Option<SecretString>> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("invalid host-only Iroh initialize response"))?;
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("host trust initialization was rejected");
        return Err(MezError::forbidden(format!(
            "Iroh transport connected, but {message}",
        )));
    }
    let result = value
        .get("result")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_state("host-only response omitted result"))?;
    if result
        .get("granted_role")
        .and_then(serde_json::Value::as_str)
        != Some("observer")
        || !result
            .get("session")
            .is_some_and(serde_json::Value::is_null)
        || !result.get("lease").is_some_and(serde_json::Value::is_null)
        || !result.get("host").is_some_and(serde_json::Value::is_object)
    {
        return Err(MezError::forbidden(
            "host-only initialization returned session authority or an unexpected role",
        ));
    }
    Ok(result
        .get("device_credential")
        .and_then(serde_json::Value::as_str)
        .map(|credential| SecretString::from(credential.to_string())))
}

fn iroh_connect_error(error: iroh::endpoint::ConnectError) -> MezError {
    let diagnostic = error.to_string().to_ascii_lowercase();
    if diagnostic.contains("alpn") || diagnostic.contains("application protocol") {
        MezError::invalid_state(
            "Iroh ALPN negotiation failed; the peer does not serve mezzanine/transport/1",
        )
    } else if diagnostic.contains("address") || diagnostic.contains("route") {
        MezError::invalid_state(
            "Iroh endpoint is unreachable under the configured direct, relay, and lookup policy",
        )
    } else {
        MezError::invalid_state("Iroh connection failed during address, relay, or ALPN negotiation")
    }
}

fn iroh_setup_timeout_error(
    policy: &RuntimeIrohTransportPolicy,
    target: &IrohControlTarget,
    stage: &str,
) -> MezError {
    let (direct_routes, relay_routes) = target.route_counts();
    MezError::invalid_state(format!(
        "Iroh {stage} timed out after {} ms before Mezzanine authentication; profile `{}` has {direct_routes} pinned direct route(s) and {relay_routes} pinned relay route(s)",
        policy.setup_timeout.as_millis(),
        target.profile_name()
    ))
}

fn current_unix_seconds_for_iroh_client() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| MezError::invalid_state("system clock is before Unix epoch"))
}

/// Runs the read control response frames operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn read_control_response_frames<R: Read>(
    stream: &mut R,
    max_content_length: usize,
    expected_frames: usize,
) -> Result<Vec<u8>> {
    let mut response = Vec::new();
    let mut buffer = vec![0; 8192];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        response.extend_from_slice(&buffer[..read]);
        if response.len() > max_content_length {
            return Err(MezError::invalid_state("control response exceeds limit"));
        }
        if count_complete_control_frames(&response, max_content_length) >= expected_frames {
            return Ok(response);
        }
    }
    Err(incomplete_control_response_error(
        &response,
        max_content_length,
        expected_frames,
    ))
}

/// Runs the count complete control frames operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn count_complete_control_frames(input: &[u8], max_content_length: usize) -> usize {
    let mut count = 0;
    let mut consumed = 0;
    while consumed < input.len() {
        let Ok((_, next)) = decode_control_frame(&input[consumed..], max_content_length) else {
            break;
        };
        if next == 0 {
            break;
        }
        count += 1;
        consumed += next;
    }
    count
}

/// Returns a diagnostic for a closed control socket with missing response frames.
///
/// # Parameters
/// - `input`: The bytes received before the socket closed.
/// - `max_content_length`: The maximum control frame body length.
/// - `expected_frames`: The number of frames the caller was waiting for.
pub(super) fn incomplete_control_response_error(
    input: &[u8],
    max_content_length: usize,
    expected_frames: usize,
) -> MezError {
    let complete_frames = count_complete_control_frames(input, max_content_length);
    MezError::invalid_state(format!(
        "control socket closed before complete response frame ({complete_frames}/{expected_frames})"
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        RemoteRoleCeiling, ensure_iroh_attach_role_allowed, iroh_client_clipboard_negotiated,
        iroh_initialize_rejected_event_stream_v2, validate_iroh_host_only_initialize_response,
        validate_iroh_initialize_response,
    };

    #[test]
    fn iroh_attach_role_ceiling_allows_observer_and_blocks_primary_escalation() {
        ensure_iroh_attach_role_allowed(RemoteRoleCeiling::Observer, "observer").unwrap();
        ensure_iroh_attach_role_allowed(RemoteRoleCeiling::Primary, "observer").unwrap();
        ensure_iroh_attach_role_allowed(RemoteRoleCeiling::Primary, "primary").unwrap();

        let error = ensure_iroh_attach_role_allowed(RemoteRoleCeiling::Observer, "primary")
            .expect_err("observer authority must not attach as primary");
        assert!(error.message().contains("role ceiling"), "{error:?}");
    }

    #[test]
    fn iroh_initialize_requires_the_requested_attach_grant() {
        validate_iroh_initialize_response(
            r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"primary"}}"#,
            "primary",
        )
        .unwrap();
        validate_iroh_initialize_response(
            r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"observer"}}"#,
            "observer",
        )
        .unwrap();

        let error = validate_iroh_initialize_response(
            r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"observer"}}"#,
            "primary",
        )
        .expect_err("primary attach must reject a downgraded grant");
        assert!(
            error.message().contains("unexpected remote role"),
            "{error:?}"
        );
    }

    /// Verifies host-scoped pairing retains the server's trust diagnostic so a
    /// rejected fresh invitation can be distinguished from a transport failure.
    #[test]
    fn iroh_host_only_initialize_reports_the_server_trust_rejection() {
        let error = validate_iroh_host_only_initialize_response(
            r#"{"jsonrpc":"2.0","id":"cli-init","error":{"code":-32002,"message":"Iroh invitation token is invalid"}}"#,
        )
        .expect_err("a rejected host invitation must fail initialization");

        assert!(
            error.message().contains("Iroh invitation token is invalid"),
            "{error:?}"
        );
    }

    /// Verifies only an explicitly capability-confirmed primary enables the
    /// client-effect event stream while observers and legacy peers remain v1.
    #[test]
    fn iroh_initialize_negotiates_clipboard_event_stream_only_for_capable_primary() {
        let capable_primary = r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"primary","capabilities":{"features":{"client_clipboard_write":true}}}}"#;
        let legacy_primary = r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"primary","capabilities":{"features":{}}}}"#;
        let observer = r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"observer","capabilities":{"features":{"client_clipboard_write":true}}}}"#;

        assert!(iroh_client_clipboard_negotiated(capable_primary, "primary", 2).unwrap());
        assert!(!iroh_client_clipboard_negotiated(legacy_primary, "primary", 2).unwrap());
        assert!(!iroh_client_clipboard_negotiated(observer, "observer", 1).unwrap());
    }

    /// Verifies fallback is limited to the exact legacy event-version rejection.
    #[test]
    fn iroh_initialize_v2_fallback_rejects_unrelated_failures() {
        assert!(iroh_initialize_rejected_event_stream_v2(
            r#"{"jsonrpc":"2.0","id":"cli-init","error":{"code":-32602,"message":"unsupported event stream version"}}"#,
        ));
        assert!(!iroh_initialize_rejected_event_stream_v2(
            r#"{"jsonrpc":"2.0","id":"cli-init","error":{"code":-32001,"message":"authentication failed"}}"#,
        ));
    }
}
