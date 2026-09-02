//! Cli Control Client implementation.
//!
//! This module owns the cli control client boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use iroh::EndpointAddr;
use secrecy::{ExposeSecret, SecretString};
use tokio::io::AsyncWriteExt;
use zeroize::Zeroizing;

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
    /// Selects the existing default or creates one when none exists.
    ResolveOrCreate { idempotency_key: String },
    /// Attaches one existing lease by stable lease id, session id, or exact name.
    Attach { target: String },
    /// Selects one existing default and never creates.
    Default,
}

impl IrohSessionRouting {
    fn intent(&self) -> &'static str {
        match self {
            Self::Create { .. } => "create",
            Self::ResolveOrCreate { .. } => "resolve_or_create",
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
            Self::Create { .. } | Self::ResolveOrCreate { .. } | Self::Default => None,
        }
    }

    fn idempotency_key(&self) -> Option<&str> {
        match self {
            Self::Create {
                idempotency_key, ..
            }
            | Self::ResolveOrCreate { idempotency_key } => Some(idempotency_key),
            Self::Attach { .. } | Self::Default => None,
        }
    }

    fn session_name(&self) -> Option<&str> {
        match self {
            Self::Create { name, .. } => name.as_deref(),
            Self::ResolveOrCreate { .. } | Self::Attach { .. } | Self::Default => None,
        }
    }
}

/// One initialized, long-lived Iroh control stream for interactive attach.
pub(super) struct PersistentIrohControlChannel {
    _identity: RemoteClientIdentity,
    endpoint: iroh::Endpoint,
    connection: iroh::endpoint::Connection,
    bridge: IrohCompressionBridge,
    attached_client_id: mez_core::ids::ClientId,
    x11_client: Option<super::x11::PreparedX11Client>,
    x11_route: Option<crate::runtime::x11::X11ForwardingResult>,
    x11_task: Option<AbortOnDropTask<()>>,
    event_receiver:
        Option<tokio::sync::mpsc::Receiver<Result<super::attach::IrohAttachRenderWakeup>>>,
    event_task: AbortOnDropTask<()>,
    pushed_render_owner: bool,
    setup_timeout: std::time::Duration,
}

/// Tokio task ownership that aborts background work whenever its owner drops.
struct AbortOnDropTask<T> {
    task: Option<tokio::task::JoinHandle<T>>,
}

impl<T> AbortOnDropTask<T> {
    /// Retains one task behind cancellation-safe ownership.
    fn new(task: tokio::task::JoinHandle<T>) -> Self {
        Self { task: Some(task) }
    }

    /// Waits boundedly for graceful completion and aborts a remaining task.
    async fn join_bounded(mut self, timeout: std::time::Duration) {
        let Some(mut task) = self.task.take() else {
            return;
        };
        if tokio::time::timeout(timeout, &mut task).await.is_err() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl<T> Drop for AbortOnDropTask<T> {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
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

    /// Returns the client identity validated before background workers started.
    pub(super) fn attached_client_id(&self) -> &mez_core::ids::ClientId {
        &self.attached_client_id
    }

    /// Reports whether negotiated v3 owns this primary's rendered state.
    pub(super) fn pushed_render_owner(&self) -> bool {
        self.pushed_render_owner
    }

    /// Returns the negotiated X11 route metadata retained for the relay worker.
    pub(super) fn x11_route(&self) -> Option<&crate::runtime::x11::X11ForwardingResult> {
        self.x11_route.as_ref()
    }

    /// Takes the negotiated event receiver exactly once for the attach loop.
    pub(super) fn take_event_receiver(
        &mut self,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<super::attach::IrohAttachRenderWakeup>>> {
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
            attached_client_id: _,
            mut x11_client,
            x11_route: _,
            x11_task,
            event_receiver,
            event_task,
            pushed_render_owner: _,
            setup_timeout,
        } = self;
        drop(event_receiver);
        connection.close(iroh::endpoint::VarInt::from_u32(0), b"attach complete");
        let _ = bridge.shutdown(setup_timeout).await;
        if let Some(task) = x11_task {
            task.join_bounded(setup_timeout).await;
        }
        event_task.join_bounded(setup_timeout).await;
        let _ = tokio::time::timeout(setup_timeout, endpoint.close()).await;
        if let Some(client) = x11_client.take() {
            let _ = client.close().await;
        }
    }
}

/// Opens and initializes one persistent Iroh control stream for interactive attach.
#[allow(
    clippy::too_many_arguments,
    reason = "remote target, environment, role, routing, terminal geometry, terminal identity, and optional X11 negotiation are independent attach inputs"
)]
pub(super) async fn open_persistent_iroh_control_channel(
    control_target: &super::ControlTargetSelection,
    env: &super::CliEnv,
    requested_role: &str,
    routing: Option<&IrohSessionRouting>,
    columns: u16,
    rows: u16,
    term: &str,
    x11_request: Option<(crate::runtime::x11::X11ForwardingMode, bool)>,
) -> Result<(PersistentIrohControlChannel, String)> {
    let paths = env.config_paths()?;
    let layers = super::load_runtime_config_layers(&paths)?;
    let structured = crate::runtime::runtime_effective_config_value(&layers)?;
    let configured_policy = crate::runtime::runtime_iroh_transport_policy_from_config(&structured)?;
    let client_clipboard = crate::runtime::runtime_client_host_clipboard_from_config(&structured)?;
    if x11_request.is_some() && requested_role != "primary" {
        return Err(MezError::forbidden(
            "X11 forwarding requires a primary attachment",
        ));
    }
    let x11_client = match x11_request {
        Some((mode, _takeover)) => Some(super::x11::prepare_x11_client(mode).await?),
        None => None,
    };
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

    let mut retained_identity = None;
    if matches!(
        &target,
        IrohControlTarget::Invitation {
            scope: RemoteClientProfileScope::Host,
            ..
        }
    ) {
        let identity = RemoteClientIdentity::load_or_create(paths.root())?;
        let profile_name = target.profile_name().to_string();
        exchange_iroh_host_only_initialize_with_identity(
            paths.root(),
            &configured_policy,
            &target,
            &identity,
        )
        .await?;
        let profile = RemoteClientProfileStore::under_config_root(paths.root())
            .load(&profile_name)?
            .ok_or_else(|| {
                MezError::invalid_state(
                    "successful host pairing did not persist a reconnect profile",
                )
            })?;
        target = IrohControlTarget::Profile(profile);
        retained_identity = Some(identity);
    }

    let policy = explicit_iroh_client_policy(&configured_policy, &target)?;

    let identity = match retained_identity {
        Some(identity) => identity,
        None => RemoteClientIdentity::load_or_create(paths.root())?,
    };
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
    if requested_role == "observer" {
        if client.get("metadata").is_none() {
            client["metadata"] = serde_json::json!({});
        }
        client["metadata"]["pushed_render_updates"] = serde_json::Value::Bool(true);
    }
    let event_stream_version_candidates = iroh_event_stream_version_candidates(requested_role)?;
    let mut requested_event_stream_version = event_stream_version_candidates[0];
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
    if let Some(prepared) = x11_client.as_ref() {
        let offer = prepared.offer(x11_request.is_some_and(|(_mode, takeover)| takeover));
        params["x11_forwarding"] = serde_json::json!({
            "version": offer.version,
            "mode": offer.mode.as_str(),
            "auth_protocol": offer.auth_protocol.as_str(),
            "fake_cookie_base64": base64::engine::general_purpose::STANDARD
                .encode(offer.fake_cookie.as_bytes()),
            "takeover": offer.takeover,
        });
    }
    let mut initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "cli-init",
        "method": "control/initialize",
        "params": params
    })
    .to_string();
    let mut response = String::new();
    for (index, candidate) in event_stream_version_candidates.iter().copied().enumerate() {
        requested_event_stream_version = candidate;
        let mut initialize_value: serde_json::Value = serde_json::from_str(&initialize)
            .map_err(|_| MezError::invalid_state("invalid local Iroh initialize request"))?;
        initialize_value["params"]["event_stream_version"] = serde_json::Value::from(candidate);
        initialize = initialize_value.to_string();
        write_iroh_control_frame(bridge.stream_mut(), &initialize, policy.idle_timeout).await?;
        response =
            read_persistent_iroh_control_frame(bridge.stream_mut(), policy.idle_timeout).await?;
        if !iroh_initialize_rejected_event_stream_version(&response) {
            break;
        }
        if index + 1 == event_stream_version_candidates.len() {
            break;
        }
    }
    let validated = validate_persistent_iroh_initialize_response(
        &response,
        requested_role,
        requested_event_stream_version,
        x11_request.map(|(mode, _)| mode),
    )?;
    if let IrohControlTarget::Invitation {
        profile_name,
        server_addr,
        role,
        ..
    } = &target
    {
        let issued_credential = validated.issued_credential.as_ref().ok_or_else(|| {
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
            device_credential: issued_credential.clone(),
        })?;
    } else {
        refresh_authenticated_profile_route(paths.root(), &endpoint, &target).await?;
    }

    let x11_worker = match (x11_client.as_ref(), validated.x11_route.as_ref()) {
        (Some(client), Some(route)) => {
            let incoming_streams = u32::try_from(policy.x11.max_connections_per_route)
                .map_err(|_| MezError::invalid_state("X11 stream credit exceeds Iroh limits"))?;
            Some((
                incoming_streams,
                route.clone(),
                client.forwarder(),
                compression,
                policy.x11.setup_timeout,
                paths.root().join("x11-client.diagnostics.log"),
            ))
        }
        (None, None) => None,
        _ => {
            return Err(MezError::invalid_state(
                "Iroh X11 negotiation and local preparation diverged",
            ));
        }
    };
    let (event_receiver, event_task) = super::attach::spawn_iroh_runtime_event_receiver(
        connection.clone(),
        compression,
        policy.setup_timeout,
        requested_event_stream_version,
        validated.pushed_render_owner,
        validated
            .pushed_render_owner
            .then(|| requested_role.to_string()),
        validated
            .client_clipboard_negotiated
            .then_some(client_clipboard),
    );
    let event_task = AbortOnDropTask::new(event_task);
    let x11_task = x11_worker.map(
        |(incoming_streams, route, forwarder, compression, setup_timeout, diagnostic_path)| {
            connection
                .set_max_concurrent_bi_streams(iroh::endpoint::VarInt::from_u32(incoming_streams));
            let connection = connection.clone();
            AbortOnDropTask::new(tokio::spawn(async move {
                serve_client_x11_streams(
                    connection,
                    route,
                    forwarder,
                    compression,
                    setup_timeout,
                    diagnostic_path,
                )
                .await;
            }))
        },
    );
    Ok((
        PersistentIrohControlChannel {
            _identity: identity,
            endpoint,
            connection,
            bridge,
            attached_client_id: validated.attached_client_id,
            x11_client,
            x11_route: validated.x11_route,
            x11_task,
            event_receiver: Some(event_receiver),
            event_task,
            pushed_render_owner: validated.pushed_render_owner,
            setup_timeout: policy.setup_timeout,
        },
        response,
    ))
}

/// Accepts server-opened X11 streams independently from control and event framing.
async fn serve_client_x11_streams(
    connection: iroh::endpoint::Connection,
    route: crate::runtime::x11::X11ForwardingResult,
    forwarder: super::x11::X11ClientForwarder,
    compression: IrohCompressionPolicy,
    setup_timeout: std::time::Duration,
    diagnostic_path: PathBuf,
) {
    let mut workers = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = connection.accept_bi() => match accepted {
                Ok((send, recv)) => {
                    let route = route.clone();
                    let forwarder = forwarder.clone();
                    let diagnostic_path = diagnostic_path.clone();
                    workers.spawn(async move {
                        if let Err(failure) = relay_client_x11_stream(
                            send,
                            recv,
                            route,
                            forwarder,
                            compression,
                            setup_timeout,
                        )
                        .await
                        {
                            append_client_x11_diagnostic(&diagnostic_path, &failure);
                        }
                    });
                }
                Err(_) => break,
            },
            joined = workers.join_next(), if !workers.is_empty() => {
                if let Some(Err(error)) = joined
                    && !error.is_cancelled()
                {
                    break;
                }
            }
        }
    }
    workers.abort_all();
    while workers.join_next().await.is_some() {}
}

/// Maximum retained attaching-side X11 diagnostic log size before rotation.
const X11_CLIENT_DIAGNOSTIC_MAX_BYTES: u64 = 256 * 1024;

/// One stage-classified client X11 relay failure with local-only detail.
#[derive(Debug)]
struct ClientX11RelayFailure {
    stage: crate::runtime::x11::X11StreamFailureStage,
    error: MezError,
}

impl ClientX11RelayFailure {
    /// Associates one detailed local error with a privacy-safe stream stage.
    fn new(stage: crate::runtime::x11::X11StreamFailureStage, error: MezError) -> Self {
        Self { stage, error }
    }

    /// Returns the QUIC reset code carrying only the fixed failure stage.
    fn application_code(&self) -> iroh::endpoint::VarInt {
        iroh::endpoint::VarInt::from_u32(self.stage.application_code())
    }
}

/// Appends one sanitized client-local failure to an owner-private bounded log.
fn append_client_x11_diagnostic(path: &Path, failure: &ClientX11RelayFailure) {
    if std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.len() >= X11_CLIENT_DIAGNOSTIC_MAX_BYTES)
    {
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path);
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
    else {
        return;
    };
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let message = failure.error.message().replace(['\r', '\n'], " ");
    let _ = writeln!(
        file,
        "timestamp_unix={timestamp} stage={} kind={:?} io_kind={:?} error={message}",
        failure.stage.as_str(),
        failure.error.kind(),
        failure.error.io_kind(),
    );
}

/// Authenticates one route preface, rewrites setup locally, and relays X11 records.
async fn relay_client_x11_stream(
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    route: crate::runtime::x11::X11ForwardingResult,
    forwarder: super::x11::X11ClientForwarder,
    compression: IrohCompressionPolicy,
    setup_timeout: std::time::Duration,
) -> std::result::Result<(), ClientX11RelayFailure> {
    use crate::runtime::x11::X11StreamFailureStage as Stage;

    let setup_deadline = tokio::time::Instant::now() + setup_timeout;
    let prepared = async {
        let mut encoded = Zeroizing::new([0u8; crate::runtime::x11::X11_STREAM_PREFACE_BYTES]);
        tokio::time::timeout_at(setup_deadline, recv.read_exact(&mut *encoded))
            .await
            .map_err(|_| {
                ClientX11RelayFailure::new(
                    Stage::ClientPreface,
                    MezError::invalid_state("X11 client stream preface timed out"),
                )
            })?
            .map_err(|error| {
                ClientX11RelayFailure::new(
                    Stage::ClientPreface,
                    MezError::forbidden(format!("incomplete X11 stream preface: {error}")),
                )
            })?;
        let preface =
            crate::runtime::x11::X11StreamPreface::decode(&*encoded).map_err(|error| {
                ClientX11RelayFailure::new(
                    Stage::ClientPreface,
                    MezError::forbidden(error.to_string()),
                )
            })?;
        if preface.generation != route.generation || preface.route_token != route.route_token {
            return Err(ClientX11RelayFailure::new(
                Stage::ClientRouteAuthentication,
                MezError::forbidden("X11 stream does not authenticate the negotiated route"),
            ));
        }
        let mut decoder = crate::runtime::x11::X11IrohDecoder::new(compression)
            .map_err(|error| ClientX11RelayFailure::new(Stage::ClientSetupDecode, error))?;
        let setup_read = async {
            if decoder.is_raw() {
                read_client_x11_setup(&mut recv).await
            } else {
                decoder.read_setup(&mut recv).await
            }
        };
        let mut setup = tokio::time::timeout_at(setup_deadline, setup_read)
            .await
            .map_err(|_| {
                ClientX11RelayFailure::new(
                    Stage::ClientSetupDecode,
                    MezError::invalid_state("X11 client setup decode timed out"),
                )
            })?
            .map_err(|error| ClientX11RelayFailure::new(Stage::ClientSetupDecode, error))?;
        forwarder
            .rewrite_setup(&mut setup)
            .map_err(|error| ClientX11RelayFailure::new(Stage::ClientCredentialRewrite, error))?;
        let remaining = setup_deadline.saturating_duration_since(tokio::time::Instant::now());
        let mut local = forwarder
            .connect(remaining)
            .await
            .map_err(|error| ClientX11RelayFailure::new(Stage::ClientLocalConnect, error))?;
        tokio::time::timeout_at(setup_deadline, async {
            local.write_all(&setup).await?;
            local.flush().await
        })
        .await
        .map_err(|_| {
            ClientX11RelayFailure::new(
                Stage::ClientLocalSetupWrite,
                MezError::invalid_state("local X11 setup write timed out"),
            )
        })?
        .map_err(|error| ClientX11RelayFailure::new(Stage::ClientLocalSetupWrite, error.into()))?;
        Ok((local, decoder))
    }
    .await;
    let (local, mut downstream_decoder) = match prepared {
        Ok(prepared) => prepared,
        Err(failure) => {
            let code = failure.application_code();
            let _ = send.reset(code);
            let _ = recv.stop(code);
            return Err(failure);
        }
    };

    let (mut local_read, mut local_write) = tokio::io::split(local);
    let mut upstream_encoder = crate::runtime::x11::X11IrohEncoder::new(compression)
        .map_err(|error| ClientX11RelayFailure::new(Stage::ClientUpstreamRelay, error))?;
    let upstream = async {
        let result = upstream_encoder
            .relay(&mut local_read, &mut send, None)
            .await;
        if let Err(error) = result {
            return Err(ClientX11RelayFailure::new(
                Stage::ClientUpstreamRelay,
                error,
            ));
        }
        let _ = send.finish();
        Ok::<(), ClientX11RelayFailure>(())
    };
    let downstream = async {
        let result = downstream_decoder
            .relay(&mut recv, &mut local_write, None)
            .await;
        if let Err(error) = result {
            return Err(ClientX11RelayFailure::new(
                Stage::ClientDownstreamRelay,
                error,
            ));
        }
        Ok::<(), ClientX11RelayFailure>(())
    };
    if let Err(failure) = tokio::try_join!(upstream, downstream) {
        let code = failure.application_code();
        let _ = send.reset(code);
        let _ = recv.stop(code);
        return Err(failure);
    }
    Ok(())
}

/// Reads exactly one bounded X11 setup request without consuming application bytes.
async fn read_client_x11_setup(
    recv: &mut iroh::endpoint::RecvStream,
) -> Result<Zeroizing<Vec<u8>>> {
    let mut setup = Zeroizing::new(Vec::new());
    loop {
        match crate::runtime::x11::parse_x11_setup(&setup)
            .map_err(|error| MezError::forbidden(error.to_string()))?
        {
            crate::runtime::x11::X11SetupProgress::Complete(_) => return Ok(setup),
            crate::runtime::x11::X11SetupProgress::Incomplete { required_len } => {
                if required_len > crate::runtime::x11::X11_MAX_SETUP_BYTES
                    || required_len <= setup.len()
                {
                    return Err(MezError::forbidden("invalid X11 setup length"));
                }
                let start = setup.len();
                setup.resize(required_len, 0);
                recv.read_exact(&mut setup[start..])
                    .await
                    .map_err(|_| MezError::forbidden("incomplete X11 setup packet"))?;
            }
        }
    }
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
    let identity = RemoteClientIdentity::load_or_create(config_root)?;
    exchange_iroh_host_only_initialize_with_identity(
        config_root,
        configured_policy,
        target,
        &identity,
    )
    .await
}

async fn exchange_iroh_host_only_initialize_with_identity(
    config_root: &Path,
    configured_policy: &RuntimeIrohTransportPolicy,
    target: &IrohControlTarget,
    identity: &RemoteClientIdentity,
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
                None,
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
                None,
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
                sandbox_platform_availability: None,
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
    for codec in IrohCompressionPolicy::negotiation_codecs(&policy.compression_codecs) {
        match tokio::time::timeout(
            policy.setup_timeout,
            endpoint.connect(target.server_addr().clone(), codec.alpn()),
        )
        .await
        {
            Ok(Ok(connection)) => {
                let compression = IrohCompressionPolicy::new(
                    codec,
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

/// Fully validated initialize metadata required before attach workers may start.
struct ValidatedPersistentIrohInitialize {
    issued_credential: Option<SecretString>,
    attached_client_id: mez_core::ids::ClientId,
    x11_route: Option<crate::runtime::x11::X11ForwardingResult>,
    client_clipboard_negotiated: bool,
    pushed_render_owner: bool,
}

/// Validates every fallible initialize field consumed by a persistent attach.
fn validate_persistent_iroh_initialize_response(
    body: &str,
    requested_role: &str,
    requested_event_stream_version: u32,
    requested_x11_mode: Option<crate::runtime::x11::X11ForwardingMode>,
) -> Result<ValidatedPersistentIrohInitialize> {
    Ok(ValidatedPersistentIrohInitialize {
        issued_credential: validate_iroh_initialize_response(body, requested_role)?,
        attached_client_id: super::attach::attached_client_id_from_initialize_response(body)?,
        x11_route: validate_iroh_x11_initialize_response(body, requested_x11_mode)?,
        client_clipboard_negotiated: iroh_client_clipboard_negotiated(
            body,
            requested_role,
            requested_event_stream_version,
        )?,
        pushed_render_owner: iroh_pushed_render_negotiated(
            body,
            requested_role,
            requested_event_stream_version,
        )?,
    })
}

/// Validates exact X11 capability and route metadata when forwarding was requested.
fn validate_iroh_x11_initialize_response(
    body: &str,
    requested_mode: Option<crate::runtime::x11::X11ForwardingMode>,
) -> Result<Option<crate::runtime::x11::X11ForwardingResult>> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("invalid Iroh initialize response"))?;
    let result = value
        .get("result")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_state("Iroh initialize response omitted result"))?;
    let capable = result
        .get("capabilities")
        .and_then(|capabilities| capabilities.get("features"))
        .and_then(|features| features.get("x11_forwarding"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let negotiated = result.get("x11_forwarding");
    let Some(requested_mode) = requested_mode else {
        if capable || negotiated.is_some_and(|value| !value.is_null()) {
            return Err(MezError::invalid_state(
                "Iroh server returned unrequested X11 forwarding authority",
            ));
        }
        return Ok(None);
    };
    if !capable {
        return Err(MezError::not_implemented(
            "Iroh server does not support requested X11 forwarding",
        ));
    }
    let negotiated = negotiated
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_state("Iroh X11 negotiation omitted route metadata"))?;
    let version = negotiated
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
        .filter(|version| *version == crate::runtime::x11::X11_FORWARDING_VERSION)
        .ok_or_else(|| {
            MezError::invalid_state("Iroh X11 negotiation returned an unsupported version")
        })?;
    let mode = match negotiated.get("mode").and_then(serde_json::Value::as_str) {
        Some("untrusted") => crate::runtime::x11::X11ForwardingMode::Untrusted,
        Some("trusted") => crate::runtime::x11::X11ForwardingMode::Trusted,
        _ => {
            return Err(MezError::invalid_state(
                "Iroh X11 negotiation returned an invalid mode",
            ));
        }
    };
    if mode != requested_mode {
        return Err(MezError::forbidden(
            "Iroh X11 negotiation changed the requested trust mode",
        ));
    }
    let generation = negotiated
        .get("generation")
        .and_then(serde_json::Value::as_u64)
        .filter(|generation| *generation > 0)
        .ok_or_else(|| {
            MezError::invalid_state("Iroh X11 negotiation returned an invalid generation")
        })?;
    let token = negotiated
        .get("route_token_base64")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_state("Iroh X11 negotiation omitted its route token"))?;
    let decoded_token = Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(token)
            .map_err(|_| {
                MezError::invalid_state("Iroh X11 negotiation returned an invalid route token")
            })?,
    );
    let token: Zeroizing<[u8; crate::runtime::x11::X11_ROUTE_TOKEN_BYTES]> =
        Zeroizing::new(decoded_token.as_slice().try_into().map_err(|_| {
            MezError::invalid_state("Iroh X11 negotiation returned an invalid route token")
        })?);
    Ok(Some(crate::runtime::x11::X11ForwardingResult {
        version,
        mode,
        generation,
        route_token: crate::runtime::x11::X11RouteToken::new(*token),
    }))
}

/// Returns the role-limited event-stream versions attempted by Iroh attach.
fn iroh_event_stream_version_candidates(requested_role: &str) -> Result<Vec<u32>> {
    match requested_role {
        "primary" => Ok(vec![3, 2, 1]),
        "observer" => Ok(vec![3, 1]),
        _ => Err(MezError::invalid_args("unsupported Iroh requested role")),
    }
}

/// Returns whether an initialize response permits a same-connection retry.
fn iroh_initialize_rejected_event_stream_version(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    let Some(error) = value.get("error") else {
        return false;
    };
    let code = error.get("code").and_then(serde_json::Value::as_i64);
    let message = error.get("message").and_then(serde_json::Value::as_str);
    let mezzanine_code = error
        .get("data")
        .and_then(|data| data.get("mezzanine_code"))
        .and_then(serde_json::Value::as_str);
    matches!(
        (code, message, mezzanine_code),
        (
            Some(-32003),
            Some("unsupported event stream version"),
            Some("unsupported_event_stream_version")
        ) | (
            Some(-32602),
            Some("unsupported event stream version"),
            Some("invalid_params")
        )
    )
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
    Ok(matches!(event_stream_version, 2 | 3)
        && requested_role == "primary"
        && granted_role == Some("primary")
        && clipboard_capable)
}

/// Returns whether the negotiated event stream owns rendered state.
fn iroh_pushed_render_negotiated(
    body: &str,
    requested_role: &str,
    event_stream_version: u32,
) -> Result<bool> {
    if event_stream_version != 3 {
        return Ok(false);
    }
    if requested_role == "primary" {
        return Ok(true);
    }
    if requested_role != "observer" {
        return Err(MezError::invalid_args("unsupported Iroh requested role"));
    }
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("invalid Iroh initialize response"))?;
    let result = value
        .get("result")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| MezError::invalid_state("Iroh initialize response omitted result"))?;
    let observer_granted = result
        .get("granted_role")
        .and_then(serde_json::Value::as_str)
        == Some("observer");
    let capable = result
        .get("capabilities")
        .and_then(|capabilities| capabilities.get("features"))
        .and_then(|features| features.get("pushed_render_updates"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Ok(observer_granted && capable)
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
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::{
        AbortOnDropTask, ClientX11RelayFailure, RemoteRoleCeiling, append_client_x11_diagnostic,
        ensure_iroh_attach_role_allowed, iroh_client_clipboard_negotiated,
        iroh_event_stream_version_candidates, iroh_initialize_rejected_event_stream_version,
        iroh_pushed_render_negotiated, relay_client_x11_stream,
        validate_iroh_host_only_initialize_response, validate_iroh_initialize_response,
        validate_iroh_x11_initialize_response, validate_persistent_iroh_initialize_response,
    };
    use iroh::endpoint::{PortmapperConfig, QuicTransportConfig, VarInt, presets};
    use iroh::{Endpoint, RelayMode};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

    /// Persistent attach validation must reject a missing or malformed client
    /// identity at the same pre-worker boundary that validates role, event,
    /// clipboard, and X11 negotiation metadata.
    #[test]
    fn persistent_iroh_initialize_validates_attached_client_before_workers() {
        let valid = r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"primary","client":{"id":"c7"},"capabilities":{"features":{"pushed_render_updates":true,"client_clipboard_write":true,"x11_forwarding":true}},"x11_forwarding":{"version":2,"mode":"untrusted","generation":7,"route_token_base64":"MzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzM="}}}"#;
        let validated = validate_persistent_iroh_initialize_response(
            valid,
            "primary",
            3,
            Some(crate::runtime::x11::X11ForwardingMode::Untrusted),
        )
        .unwrap();
        assert_eq!(validated.attached_client_id.as_str(), "c7");
        assert!(validated.pushed_render_owner);
        assert!(validated.client_clipboard_negotiated);
        assert_eq!(validated.x11_route.unwrap().generation, 7);

        for malformed in [
            r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"primary","capabilities":{"features":{}}}}"#,
            r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"primary","client":{"id":"observer-7"},"capabilities":{"features":{}}}}"#,
        ] {
            let Err(error) =
                validate_persistent_iroh_initialize_response(malformed, "primary", 1, None)
            else {
                panic!("persistent attach must reject invalid client identity before workers");
            };
            assert!(error.message().contains("client id"), "{error:?}");
        }
    }

    /// Dropping a background-task owner must abort pending work instead of
    /// detaching the Tokio task after an attach setup error.
    #[tokio::test]
    async fn abort_on_drop_task_cancels_pending_worker() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);
            impl Drop for DropSignal {
                fn drop(&mut self) {
                    if let Some(sender) = self.0.take() {
                        let _ = sender.send(());
                    }
                }
            }

            let _drop_signal = DropSignal(Some(dropped_tx));
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        });
        let owner = AbortOnDropTask::new(task);
        started_rx.await.unwrap();
        drop(owner);
        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .expect("dropping the task owner should abort pending work")
            .unwrap();
    }

    /// Graceful channel cleanup must remain bounded and abort a worker that
    /// does not stop after connection closure.
    #[tokio::test]
    async fn abort_on_drop_task_bounded_join_aborts_pending_worker() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);
            impl Drop for DropSignal {
                fn drop(&mut self) {
                    if let Some(sender) = self.0.take() {
                        let _ = sender.send(());
                    }
                }
            }

            let _drop_signal = DropSignal(Some(dropped_tx));
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        });
        let owner = AbortOnDropTask::new(task);
        started_rx.await.unwrap();
        owner
            .join_bounded(std::time::Duration::from_millis(10))
            .await;
        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .expect("bounded join should abort a worker that remains pending")
            .unwrap();
    }

    /// Client X11 failures must retain useful local detail in an owner-private
    /// log while replacing line breaks that could forge additional records.
    #[test]
    fn x11_client_diagnostic_log_is_private_and_line_bounded() {
        let root = x11_test_root("client-diagnostic-log");
        let path = root.join("x11-client.diagnostics.log");
        let failure = ClientX11RelayFailure::new(
            crate::runtime::x11::X11StreamFailureStage::ClientLocalConnect,
            crate::error::MezError::invalid_state("connection refused\nforged record"),
        );

        append_client_x11_diagnostic(&path, &failure);

        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 1, "{text:?}");
        assert!(text.contains("stage=client_local_connect"), "{text:?}");
        assert!(
            text.contains("connection refused forged record"),
            "{text:?}"
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Explicit X11 negotiation must retain exact version, mode, generation,
    /// and token metadata while missing support and unrequested authority fail.
    #[test]
    fn iroh_x11_initialize_requires_exact_explicit_capability() {
        let capable = r#"{"jsonrpc":"2.0","id":"cli-init","result":{"capabilities":{"features":{"x11_forwarding":true}},"x11_forwarding":{"version":2,"mode":"untrusted","generation":7,"route_token_base64":"MzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzM="}}}"#;
        let route = validate_iroh_x11_initialize_response(
            capable,
            Some(crate::runtime::x11::X11ForwardingMode::Untrusted),
        )
        .unwrap()
        .unwrap();
        assert_eq!(route.version, 2);
        assert_eq!(
            route.mode,
            crate::runtime::x11::X11ForwardingMode::Untrusted
        );
        assert_eq!(route.generation, 7);

        let legacy = r#"{"jsonrpc":"2.0","id":"cli-init","result":{"capabilities":{"features":{"x11_forwarding":true}},"x11_forwarding":{"version":1,"mode":"untrusted","generation":7,"route_token_base64":"MzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzM="}}}"#;
        let error = validate_iroh_x11_initialize_response(
            legacy,
            Some(crate::runtime::x11::X11ForwardingMode::Untrusted),
        )
        .expect_err("X11 v1 must be rejected before a stream worker starts");
        assert!(error.message().contains("unsupported version"), "{error:?}");

        let unsupported = r#"{"jsonrpc":"2.0","id":"cli-init","result":{"capabilities":{"features":{}},"x11_forwarding":null}}"#;
        assert_eq!(
            validate_iroh_x11_initialize_response(
                unsupported,
                Some(crate::runtime::x11::X11ForwardingMode::Untrusted),
            )
            .unwrap_err()
            .kind(),
            crate::error::MezErrorKind::NotImplemented
        );
        assert!(validate_iroh_x11_initialize_response(capable, None).is_err());
        assert!(
            validate_iroh_x11_initialize_response(
                capable,
                Some(crate::runtime::x11::X11ForwardingMode::Trusted),
            )
            .is_err()
        );
    }

    /// One authenticated server-opened stream must rewrite only the setup
    /// cookie, dial the frozen local target, and preserve later raw bytes.
    #[tokio::test]
    async fn iroh_x11_client_rewrites_setup_and_relays_raw_bytes() {
        const TEST_ALPN: &[u8] = b"mezzanine/x11-client-test/1";
        let local_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_port = local_listener.local_addr().unwrap().port();
        let display = crate::cli::x11::resolve_local_x11_display(&format!(
            "127.0.0.1:{}",
            local_port.checked_sub(6000).unwrap()
        ))
        .unwrap();
        let forwarder = crate::cli::x11::X11ClientForwarder::new_for_test(
            display,
            crate::runtime::x11::X11Cookie::new([0x41; 16]),
            crate::runtime::x11::X11Cookie::new([0x52; 16]),
        );
        let route = crate::runtime::x11::X11ForwardingResult {
            version: crate::runtime::x11::X11_FORWARDING_VERSION,
            mode: crate::runtime::x11::X11ForwardingMode::Trusted,
            generation: 9,
            route_token: crate::runtime::x11::X11RouteToken::new([0x63; 32]),
        };

        let server_endpoint = Endpoint::builder(presets::Minimal)
            .alpns(vec![TEST_ALPN.to_vec()])
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let client_endpoint = Endpoint::builder(presets::Minimal)
            .transport_config(
                QuicTransportConfig::builder()
                    .max_concurrent_bidi_streams(VarInt::from_u32(1))
                    .build(),
            )
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let server_addr = server_endpoint.addr();
        let client_side = async {
            client_endpoint
                .connect(server_addr, TEST_ALPN)
                .await
                .unwrap()
        };
        let server_side = async {
            let incoming = server_endpoint.accept().await.unwrap();
            incoming.accept().unwrap().await.unwrap()
        };
        let (client_connection, server_connection) = tokio::join!(client_side, server_side);

        let server_route = route.clone();
        let server = async move {
            let (mut send, mut recv) = server_connection.open_bi().await.unwrap();
            let preface = crate::runtime::x11::X11StreamPreface {
                generation: server_route.generation,
                route_token: server_route.route_token,
            }
            .encode();
            send.write_all(&preface).await.unwrap();
            send.write_all(&x11_setup_packet(b'B', &[0x41; 16]))
                .await
                .unwrap();
            send.write_all(b"ping").await.unwrap();
            send.finish().unwrap();
            let mut reply = [0u8; 4];
            recv.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply, b"pong");
        };
        let relay_connection = client_connection.clone();
        let client = async move {
            let (send, recv) = relay_connection.accept_bi().await.unwrap();
            relay_client_x11_stream(
                send,
                recv,
                route,
                forwarder,
                test_x11_compression(crate::runtime::RuntimeIrohCompressionCodec::None),
                std::time::Duration::from_secs(2),
            )
            .await
            .unwrap();
        };
        let local = async {
            let (mut stream, _) = local_listener.accept().await.unwrap();
            let mut setup = [0u8; 48];
            stream.read_exact(&mut setup).await.unwrap();
            crate::runtime::x11::validate_x11_setup_cookie(
                &setup,
                &crate::runtime::x11::X11Cookie::new([0x52; 16]),
            )
            .unwrap();
            let mut payload = [0u8; 4];
            stream.read_exact(&mut payload).await.unwrap();
            assert_eq!(&payload, b"ping");
            stream.write_all(b"pong").await.unwrap();
            stream.shutdown().await.unwrap();
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(server, client, local);
        })
        .await
        .unwrap();

        drop(client_connection);
        client_endpoint.close().await;
        server_endpoint.close().await;
    }

    /// The complete data path must carry setup and repetitive application data
    /// through every negotiated codec while accounting both X11 directions.
    #[tokio::test]
    async fn x11_proxy_iroh_client_and_fake_server_round_trip_every_codec() {
        for codec in [
            crate::runtime::RuntimeIrohCompressionCodec::None,
            crate::runtime::RuntimeIrohCompressionCodec::Zstd,
            crate::runtime::RuntimeIrohCompressionCodec::Lz4,
            crate::runtime::RuntimeIrohCompressionCodec::ZstdStream,
            crate::runtime::RuntimeIrohCompressionCodec::Lz4Stream,
        ] {
            let local_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let local_port = local_listener.local_addr().unwrap().port();
            let display = crate::cli::x11::resolve_local_x11_display(&format!(
                "127.0.0.1:{}",
                local_port.checked_sub(6000).unwrap()
            ))
            .unwrap();
            let forwarder = crate::cli::x11::X11ClientForwarder::new_for_test(
                display,
                crate::runtime::x11::X11Cookie::new([0x41; 16]),
                crate::runtime::x11::X11Cookie::new([0x52; 16]),
            );
            let offer = crate::runtime::x11::X11ForwardingOffer {
                version: crate::runtime::x11::X11_FORWARDING_VERSION,
                mode: crate::runtime::x11::X11ForwardingMode::Trusted,
                auth_protocol: crate::runtime::x11::X11AuthProtocol::MitMagicCookie1,
                fake_cookie: crate::runtime::x11::X11Cookie::new([0x41; 16]),
                takeover: false,
            };
            let redacted = format!("{forwarder:?} {offer:?}");
            assert!(!redacted.contains("41414141"), "{redacted}");
            assert!(!redacted.contains("52525252"), "{redacted}");

            let request = vec![0x70; 16 * 1024];
            let response = vec![0x71; 12 * 1024];
            let expected_request = request.clone();
            let expected_response = response.clone();
            let fake_server = tokio::spawn(async move {
                let (mut stream, _) = local_listener.accept().await.unwrap();
                let mut setup = [0u8; 48];
                stream.read_exact(&mut setup).await.unwrap();
                crate::runtime::x11::validate_x11_setup_cookie(
                    &setup,
                    &crate::runtime::x11::X11Cookie::new([0x52; 16]),
                )
                .unwrap();
                let mut payload = vec![0u8; expected_request.len()];
                stream.read_exact(&mut payload).await.unwrap();
                assert_eq!(payload, expected_request);
                stream.write_all(&expected_response).await.unwrap();
                stream.shutdown().await.unwrap();
            });
            let (reply, metrics) =
                run_complete_x11_round_trip(forwarder, offer, codec, &request, response.len())
                    .await;
            let metrics = metrics.snapshot();

            assert_eq!(reply, response, "{codec:?}");
            assert!(metrics.identity_frames >= 1, "{codec:?}: {metrics:?}");
            if codec == crate::runtime::RuntimeIrohCompressionCodec::None {
                assert_eq!(metrics.compressed_frames, 0, "{metrics:?}");
            } else {
                assert!(metrics.compressed_frames >= 2, "{codec:?}: {metrics:?}");
                assert!(metrics.wire_bytes < metrics.decoded_bytes, "{metrics:?}");
            }
            fake_server.await.unwrap();
        }
    }

    /// Runs the complete forwarding path against a real Xvfb display in both
    /// trusted and X SECURITY untrusted modes. The CI wrapper supplies DISPLAY,
    /// XAUTHORITY, Xvfb, and xauth and never downgrades a failed untrusted run.
    #[tokio::test]
    #[ignore = "requires scripts/test-x11-forwarding.sh with Linux Xvfb and xauth"]
    async fn x11_xvfb_trusted_and_untrusted_setup_round_trip() {
        assert_eq!(std::env::var("MEZ_X11_XVFB_TEST").as_deref(), Ok("1"));
        for mode in [
            crate::runtime::x11::X11ForwardingMode::Trusted,
            crate::runtime::x11::X11ForwardingMode::Untrusted,
        ] {
            let prepared = crate::cli::x11::prepare_x11_client(mode).await.unwrap();
            let offer = prepared.offer(false);
            for codec in [
                crate::runtime::RuntimeIrohCompressionCodec::None,
                crate::runtime::RuntimeIrohCompressionCodec::Zstd,
                crate::runtime::RuntimeIrohCompressionCodec::ZstdStream,
            ] {
                let (reply, _) =
                    run_complete_x11_round_trip(prepared.forwarder(), offer.clone(), codec, &[], 8)
                        .await;
                assert_eq!(
                    reply[0], 1,
                    "X11 setup failed for {mode:?} with {codec:?}: {reply:?}"
                );
            }
            prepared.close().await.unwrap();
        }
    }

    /// Composes the stable proxy, exact route, host-opened Iroh stream, client
    /// forwarder, frozen local target, and selected compression policy.
    async fn run_complete_x11_round_trip(
        forwarder: crate::cli::x11::X11ClientForwarder,
        offer: crate::runtime::x11::X11ForwardingOffer,
        codec: crate::runtime::RuntimeIrohCompressionCodec,
        application_bytes: &[u8],
        reply_len: usize,
    ) -> (Vec<u8>, crate::runtime::IrohCompressionMetrics) {
        const TEST_ALPN: &[u8] = b"mezzanine/x11-end-to-end-test/1";
        let server_endpoint = Endpoint::builder(presets::Minimal)
            .alpns(vec![TEST_ALPN.to_vec()])
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let client_endpoint = Endpoint::builder(presets::Minimal)
            .transport_config(
                QuicTransportConfig::builder()
                    .max_concurrent_bidi_streams(VarInt::from_u32(1))
                    .build(),
            )
            .relay_mode(RelayMode::Disabled)
            .clear_address_lookup()
            .portmapper_config(PortmapperConfig::Disabled)
            .bind()
            .await
            .unwrap();
        let server_addr = server_endpoint.addr();
        let client_side = async {
            client_endpoint
                .connect(server_addr, TEST_ALPN)
                .await
                .unwrap()
        };
        let server_side = async {
            let incoming = server_endpoint.accept().await.unwrap();
            incoming.accept().unwrap().await.unwrap()
        };
        let (client_connection, server_connection) = tokio::join!(client_side, server_side);

        let root = x11_test_root("complete-round-trip");
        let proxy = crate::runtime::x11::RuntimeX11Proxy::prepare_with_policy(
            &root,
            crate::runtime::RuntimeIrohX11Policy {
                enabled: true,
                allow_trusted: true,
                max_connections_per_route: 1,
                setup_timeout: std::time::Duration::from_secs(5),
            },
        )
        .unwrap();
        let handle = proxy.handle();
        let setup = x11_setup_packet(b'B', offer.fake_cookie.as_bytes());
        let owner = crate::runtime::x11::RuntimeX11RouteOwner {
            session_id: "$x11-e2e".to_string(),
            client_id: "client-x11-e2e".to_string(),
            endpoint_id: "endpoint-x11-e2e".to_string(),
            principal_id: Some("principal-x11-e2e".to_string()),
            connection_id: format!("iroh-{}", server_connection.stable_id()),
        };
        let (route, lease) = handle.reserve_route(owner, offer).unwrap();
        let compression = test_x11_compression(codec);
        let compression_metrics = crate::runtime::IrohCompressionMetrics::new(compression.codec());
        lease
            .activate(server_connection, compression, compression_metrics.clone())
            .unwrap();
        let proxy_task = tokio::spawn(proxy.serve());

        let relay_connection = client_connection.clone();
        let relay_route = route.clone();
        let mut relay_task = tokio::spawn(async move {
            let (send, recv) = relay_connection.accept_bi().await.unwrap();
            relay_client_x11_stream(
                send,
                recv,
                relay_route,
                forwarder,
                compression,
                std::time::Duration::from_secs(5),
            )
            .await
        });
        let mut remote = tokio::net::TcpStream::connect((
            std::net::Ipv4Addr::LOCALHOST,
            6000 + handle.display_number(),
        ))
        .await
        .unwrap();
        remote.write_all(&setup).await.unwrap();
        remote.write_all(application_bytes).await.unwrap();
        remote.flush().await.unwrap();
        let mut reply = vec![0u8; reply_len];
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            remote.read_exact(&mut reply),
        )
        .await
        .unwrap()
        .unwrap();
        drop(remote);

        assert!(lease.deactivate().unwrap());
        client_connection.close(iroh::endpoint::VarInt::from_u32(0), b"test complete");
        if tokio::time::timeout(std::time::Duration::from_secs(2), &mut relay_task)
            .await
            .is_err()
        {
            relay_task.abort();
            let _ = relay_task.await;
        }
        proxy_task.abort();
        let _ = proxy_task.await;
        client_endpoint.close().await;
        server_endpoint.close().await;
        let _ = fs::remove_dir_all(root);
        (reply, compression_metrics)
    }

    /// Builds one exact little- or big-endian MIT setup request.
    fn x11_setup_packet(byte_order: u8, cookie: &[u8; 16]) -> Vec<u8> {
        let mut setup = vec![0u8; 48];
        setup[0] = byte_order;
        let encode = |value: u16| {
            if byte_order == b'l' {
                value.to_le_bytes()
            } else {
                value.to_be_bytes()
            }
        };
        setup[2..4].copy_from_slice(&encode(11));
        setup[4..6].copy_from_slice(&encode(0));
        setup[6..8].copy_from_slice(&encode(18));
        setup[8..10].copy_from_slice(&encode(16));
        setup[12..30].copy_from_slice(b"MIT-MAGIC-COOKIE-1");
        setup[32..48].copy_from_slice(cookie);
        setup
    }

    /// Builds the uncompressed negotiated transport used by legacy X11 fixtures.
    fn test_x11_compression(
        codec: crate::runtime::RuntimeIrohCompressionCodec,
    ) -> crate::runtime::IrohCompressionPolicy {
        crate::runtime::IrohCompressionPolicy::new(codec, 1, 3, 64 * 1024).unwrap()
    }

    /// Allocates one owner-private root for composed X11 tests.
    fn x11_test_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mez-cli-x11-{name}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        root
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

    /// Verifies observer v3 takes render ownership only when the server
    /// explicitly advertises pushed updates; older observer-v3 servers retain
    /// notification-plus-fetch behavior.
    #[test]
    fn iroh_observer_pushed_render_requires_explicit_capability() {
        let capable_observer = r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"observer","capabilities":{"features":{"pushed_render_updates":true}}}}"#;
        let legacy_observer = r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"observer","capabilities":{"features":{}}}}"#;

        assert!(iroh_pushed_render_negotiated(capable_observer, "observer", 3).unwrap());
        assert!(!iroh_pushed_render_negotiated(legacy_observer, "observer", 3).unwrap());
        assert!(!iroh_pushed_render_negotiated(capable_observer, "observer", 1).unwrap());
        assert!(iroh_pushed_render_negotiated(legacy_observer, "primary", 3).unwrap());
    }

    /// Verifies primary and observer attach use their role-specific event-stream
    /// negotiation order without enabling primary-only v2 for observers.
    #[test]
    fn iroh_initialize_uses_role_specific_event_stream_candidates() {
        assert_eq!(
            iroh_event_stream_version_candidates("primary").unwrap(),
            [3, 2, 1]
        );
        assert_eq!(
            iroh_event_stream_version_candidates("observer").unwrap(),
            [3, 1]
        );
        assert!(iroh_event_stream_version_candidates("agent").is_err());
    }

    /// Verifies fallback is limited to structured current or exact legacy
    /// unsupported-version results and never hides unrelated failures.
    #[test]
    fn iroh_initialize_event_stream_fallback_rejects_unrelated_failures() {
        assert!(iroh_initialize_rejected_event_stream_version(
            r#"{"jsonrpc":"2.0","id":"cli-init","error":{"code":-32003,"message":"unsupported event stream version","data":{"mezzanine_code":"unsupported_event_stream_version"}}}"#,
        ));
        assert!(iroh_initialize_rejected_event_stream_version(
            r#"{"jsonrpc":"2.0","id":"cli-init","error":{"code":-32602,"message":"unsupported event stream version","data":{"mezzanine_code":"invalid_params"}}}"#,
        ));
        assert!(!iroh_initialize_rejected_event_stream_version(
            r#"{"jsonrpc":"2.0","id":"cli-init","error":{"code":-32001,"message":"unsupported event stream version","data":{"mezzanine_code":"forbidden"}}}"#,
        ));
        assert!(!iroh_initialize_rejected_event_stream_version(
            r#"{"jsonrpc":"2.0","id":"cli-init","error":{"code":-32001,"message":"authentication failed","data":{"mezzanine_code":"forbidden"}}}"#,
        ));
        assert!(!iroh_initialize_rejected_event_stream_version("not-json"));
    }
}
