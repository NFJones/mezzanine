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
    MEZZANINE_IROH_ALPN, RuntimeIrohTransportPolicy, bind_runtime_iroh_client_endpoint,
};
use crate::security::remote::{
    RemoteClientIdentity, RemoteClientProfile, RemoteClientProfileStore, RemoteRoleCeiling,
    read_remote_invitation_file,
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
    let socket_path = selected_socket_path(socket_selection);
    let mut stream = UnixStream::connect(socket_path)?;
    let body = exchange_control_request(&mut stream, method, params)?;
    write_control_response(stdout, output_format, &body)?;
    Ok(())
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
    let initialize = r#"{"jsonrpc":"2.0","id":"cli-init","method":"control/initialize","params":{"client_name":"primary","requested_version":1,"requested_role":"primary","client":{"name":"primary","interactive":true,"terminal":{"columns":80,"rows":24,"term":"xterm-256color"}}}}"#;
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":"cli","method":"{}","params":{}}}"#,
        json_escape(method),
        params
    );
    stream.write_all(&encode_control_body(initialize))?;
    stream.flush()?;
    let initialize_response =
        read_control_response_frames(stream, CLI_CONTROL_MAX_CONTENT_LENGTH, 1)?;
    let _ = decode_control_frame(&initialize_response, CLI_CONTROL_MAX_CONTENT_LENGTH)?;
    stream.write_all(&encode_control_body(&request))?;
    stream.flush()?;
    let response = read_control_response_frames(stream, CLI_CONTROL_MAX_CONTENT_LENGTH, 1)?;
    let (body, _) = decode_control_frame(&response, CLI_CONTROL_MAX_CONTENT_LENGTH)?;
    Ok(body)
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
    let policy = crate::runtime::runtime_iroh_transport_policy_from_config(&structured)?;
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
        super::ControlTargetSelection::IrohInvitation(path) => parse_iroh_invitation_file(path)?,
    };
    let body =
        exchange_iroh_control_request(paths.root(), &policy, &target, method, params).await?;
    write_control_response(stdout, output_format, &body)
}

fn parse_iroh_invitation_file(path: &Path) -> Result<IrohControlTarget> {
    let bytes = read_remote_invitation_file(path, MAX_IROH_INVITATION_FILE_BYTES)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| MezError::invalid_args("invalid Iroh invitation JSON"))?;
    let invitation = value.get("result").unwrap_or(&value);
    let object = invitation
        .as_object()
        .ok_or_else(|| MezError::invalid_args("Iroh invitation must be a JSON object"))?;
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
    let profile_name = invitation_string(object, "profile_name")?;
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
    Ok(IrohControlTarget::Invitation {
        profile_name,
        server_addr,
        token: SecretString::from(token),
        role,
        expires_at_unix_seconds,
    })
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
                expires_at_unix_seconds,
                ..
            } => formatter
                .debug_struct("IrohControlTarget::Invitation")
                .field("profile_name", profile_name)
                .field("server_address_count", &server_addr.addrs.len())
                .field("role", role)
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

    fn role(&self) -> RemoteRoleCeiling {
        match self {
            Self::Invitation { role, .. } => *role,
            Self::Profile(profile) => profile.role,
        }
    }

    fn authentication(&self) -> (&str, &SecretString) {
        match self {
            Self::Invitation { token, .. } => ("extension:iroh_invitation", token),
            Self::Profile(profile) => ("extension:iroh_device", &profile.device_credential),
        }
    }
}

/// Exchanges initialization and one request over one client-opened Iroh stream.
pub(crate) async fn exchange_iroh_control_request(
    config_root: &Path,
    policy: &RuntimeIrohTransportPolicy,
    target: &IrohControlTarget,
    method: &str,
    params: &str,
) -> Result<String> {
    if !policy.enabled {
        return Err(MezError::config(
            "Iroh client transport is disabled; enable transport.iroh explicitly",
        ));
    }
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
    let endpoint = bind_runtime_iroh_client_endpoint(policy, identity.secret_key().clone()).await?;
    let exchange = exchange_bound_iroh_control_request(
        config_root,
        policy,
        target,
        method,
        request_params,
        &endpoint,
    )
    .await;
    let _ = tokio::time::timeout(policy.setup_timeout, endpoint.close()).await;
    exchange
}

async fn exchange_bound_iroh_control_request(
    config_root: &Path,
    policy: &RuntimeIrohTransportPolicy,
    target: &IrohControlTarget,
    method: &str,
    params: serde_json::Value,
    endpoint: &iroh::Endpoint,
) -> Result<String> {
    let connection = tokio::time::timeout(
        policy.setup_timeout,
        endpoint.connect(target.server_addr().clone(), MEZZANINE_IROH_ALPN),
    )
    .await
    .map_err(|_| MezError::invalid_state("Iroh connection setup timed out"))?
    .map_err(iroh_connect_error)?;
    if connection.remote_id() != target.server_addr().id {
        return Err(MezError::forbidden(
            "Iroh connection authenticated an unexpected server identity",
        ));
    }
    let (mut send, mut recv) = tokio::time::timeout(policy.setup_timeout, connection.open_bi())
        .await
        .map_err(|_| MezError::invalid_state("Iroh control stream setup timed out"))?
        .map_err(|_| MezError::invalid_state("failed to open Iroh control stream"))?;

    let (mechanism, credential) = target.authentication();
    let role = target.role().as_str();
    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "cli-init",
        "method": "control/initialize",
        "params": {
            "client_name": "remote-cli",
            "requested_version": 1,
            "requested_role": role,
            "detach_primary_on_disconnect": true,
            "client": {
                "name": "remote-cli",
                "interactive": true,
                "terminal": {
                    "columns": 80,
                    "rows": 24,
                    "term": "xterm-256color"
                }
            },
            "authentication": {
                "mechanism": mechanism,
                "token": credential.expose_secret()
            }
        }
    })
    .to_string();
    write_iroh_control_frame(&mut send, &initialize, policy.idle_timeout).await?;
    let initialize_body = read_iroh_control_frame(&mut recv, policy.idle_timeout).await?;
    let issued_credential = validate_iroh_initialize_response(&initialize_body)?;

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
        RemoteClientProfileStore::under_config_root(config_root).save(&RemoteClientProfile {
            name: profile_name.clone(),
            server_addr: server_addr.clone(),
            role: *role,
            device_credential: issued_credential,
        })?;
    }

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "cli",
        "method": method,
        "params": params,
    })
    .to_string();
    write_iroh_control_frame(&mut send, &request, policy.idle_timeout).await?;
    send.finish()
        .map_err(|_| MezError::invalid_state("failed to finish Iroh control request stream"))?;
    let body = read_iroh_control_frame(&mut recv, policy.idle_timeout).await?;
    let trailing = tokio::time::timeout(
        policy.setup_timeout,
        recv.read_to_end(CLI_CONTROL_MAX_CONTENT_LENGTH),
    )
    .await
    .map_err(|_| MezError::invalid_state("Iroh final response acknowledgement timed out"))?
    .map_err(|_| MezError::invalid_state("failed to drain Iroh control response stream"))?;
    if !trailing.is_empty() {
        return Err(MezError::invalid_state(
            "Iroh server sent unexpected data after the final control response",
        ));
    }
    let _ = tokio::time::timeout(policy.setup_timeout, send.stopped()).await;
    connection.close(iroh::endpoint::VarInt::from_u32(0), b"control complete");
    Ok(body)
}

async fn write_iroh_control_frame(
    send: &mut iroh::endpoint::SendStream,
    body: &str,
    timeout: std::time::Duration,
) -> Result<()> {
    tokio::time::timeout(
        timeout,
        tokio::io::AsyncWriteExt::write_all(send, &encode_control_body(body)),
    )
    .await
    .map_err(|_| MezError::invalid_state("Iroh control write timed out"))?
    .map_err(|_| MezError::invalid_state("Iroh control write failed"))
}

async fn read_iroh_control_frame(
    recv: &mut iroh::endpoint::RecvStream,
    timeout: std::time::Duration,
) -> Result<String> {
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

fn validate_iroh_initialize_response(body: &str) -> Result<Option<SecretString>> {
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
