//! Control Initialize implementation.
//!
//! This module owns the control initialize boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AuthenticationMaterial, AuthenticationMechanism, Capabilities, ClientDescriptor, GrantedRole,
    InitializeContext, InitializeParams, InitializeResult, MezError, ObserverRequestSummary,
    RequestedRole, Result, ServerIdentity, TerminalDescriptor, granted_role_name, json_escape,
    json_optional_string, json_string_field, reject_unknown_json_fields,
};

// Control initialization parsing and serialization.

/// Runs the initialize operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn initialize(
    params: InitializeParams,
    context: InitializeContext,
) -> Result<InitializeResult> {
    let selected_version = negotiate_protocol_version(params.requested_version)?;
    let authentication = params
        .authentication
        .as_ref()
        .unwrap_or(&AuthenticationMaterial {
            mechanism: AuthenticationMechanism::None,
            token: None,
        });
    let authenticated = context.outer_authenticated || authentication.is_payload_authenticated();

    if !authenticated {
        return Ok(InitializeResult {
            selected_version,
            server: ServerIdentity::current(),
            session: None,
            granted_role: match params.requested_role {
                RequestedRole::Observer => GrantedRole::PendingObserver,
                RequestedRole::Primary => GrantedRole::Automation,
                RequestedRole::Agent => GrantedRole::Agent,
                RequestedRole::Automation => GrantedRole::Automation,
            },
            capabilities: Capabilities::unauthenticated(),
            approval_pending: params.requested_role == RequestedRole::Observer,
            observer_request: None,
        });
    }

    match params.requested_role {
        RequestedRole::Primary => {
            let client = params.client.as_ref().ok_or_else(|| {
                MezError::invalid_args("primary initialization requires a client descriptor")
            })?;
            if !client.identifies_interactive_terminal(context.trusted_interactive_assertion) {
                return Err(MezError::forbidden(
                    "primary initialization requires a verified interactive terminal",
                ));
            }
            Ok(InitializeResult {
                selected_version,
                server: ServerIdentity::current(),
                session: Some(default_initialize_session_summary_json()),
                granted_role: GrantedRole::Primary,
                capabilities: Capabilities::primary(),
                approval_pending: false,
                observer_request: None,
            })
        }
        RequestedRole::Observer => Ok(InitializeResult {
            selected_version,
            server: ServerIdentity::current(),
            session: None,
            granted_role: GrantedRole::PendingObserver,
            capabilities: Capabilities::pending_observer(),
            approval_pending: true,
            observer_request: Some(ObserverRequestSummary {
                request_id: "pending".to_string(),
                state: "pending",
                state_json: None,
            }),
        }),
        RequestedRole::Agent => Ok(InitializeResult {
            selected_version,
            server: ServerIdentity::current(),
            session: Some(default_initialize_session_summary_json()),
            granted_role: GrantedRole::Agent,
            capabilities: Capabilities::agent(),
            approval_pending: false,
            observer_request: None,
        }),
        RequestedRole::Automation => Ok(InitializeResult {
            selected_version,
            server: ServerIdentity::current(),
            session: Some(default_initialize_session_summary_json()),
            granted_role: GrantedRole::Automation,
            capabilities: Capabilities::automation(),
            approval_pending: false,
            observer_request: None,
        }),
    }
}

/// Runs the negotiate protocol version operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn negotiate_protocol_version(requested_version: u32) -> Result<u32> {
    if requested_version == 1 {
        Ok(1)
    } else {
        Err(MezError::invalid_args(format!(
            "unsupported control protocol version: {requested_version}"
        )))
    }
}
/// Runs the initialize params from json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn initialize_params_from_json(params: &str) -> Result<InitializeParams> {
    reject_unknown_json_fields(
        params,
        "control/initialize params",
        &[
            "client_name",
            "requested_version",
            "requested_role",
            "client_version",
            "session_target",
            "detach_primary_on_disconnect",
            "client",
            "authentication",
        ],
    )?;
    let object = parse_object(params, "control/initialize params")?;
    let requested_role = parse_requested_role(&required_string_member(
        &object,
        "requested_role",
        "control/initialize",
    )?)?;
    let client = optional_object_member_json(&object, "client", "control/initialize")?
        .as_deref()
        .map(client_descriptor_from_json)
        .transpose()?;
    if let Some(client) = client.as_ref() {
        ensure_client_descriptor_role_matches(
            client,
            requested_role,
            "control/initialize client descriptor",
        )?;
    }
    Ok(InitializeParams {
        client_name: required_string_member(&object, "client_name", "control/initialize")?,
        requested_version: required_u32_member(&object, "requested_version", "control/initialize")?,
        requested_role,
        client_version: optional_string_member(&object, "client_version", "control/initialize")?,
        session_target_json: optional_session_target_member_json(&object)?,
        detach_primary_on_disconnect: optional_bool_member(
            &object,
            "detach_primary_on_disconnect",
            "control/initialize",
        )?
        .unwrap_or(false),
        client,
        authentication: optional_object_member_json(
            &object,
            "authentication",
            "control/initialize",
        )?
        .as_deref()
        .map(authentication_from_json)
        .transpose()?,
    })
}

/// Runs the optional session target member json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_session_target_member_json(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<String>> {
    match object.get("session_target") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => {
            validate_session_target_value(value)?;
            Ok(Some(value.to_string()))
        }
    }
}

/// Runs the validate session target value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_session_target_value(value: &serde_json::Value) -> Result<()> {
    let object = value.as_object().ok_or_else(|| {
        MezError::invalid_args("control/initialize session_target must be an object or null")
    })?;
    for (key, value) in object {
        if key == "extensions" {
            if !value.is_object() {
                return Err(MezError::invalid_args(
                    "control/initialize session_target extensions must be an object",
                ));
            }
            continue;
        }
        if !matches!(key.as_str(), "session_id" | "name" | "default") {
            return Err(MezError::invalid_args(format!(
                "control/initialize session_target contains unknown field `{key}`"
            )));
        }
    }
    let mut selector_count = 0usize;
    if object
        .get("session_id")
        .is_some_and(|value| !value.is_null())
    {
        selector_count += 1;
        required_string_member(object, "session_id", "control/initialize session_target")?;
    }
    if object.get("name").is_some_and(|value| !value.is_null()) {
        selector_count += 1;
        required_string_member(object, "name", "control/initialize session_target")?;
    }
    if object.get("default").is_some_and(|value| !value.is_null()) {
        let default = optional_bool_member(object, "default", "control/initialize session_target")?
            .unwrap_or(false);
        if default {
            selector_count += 1;
        }
    }
    if selector_count != 1 {
        return Err(MezError::invalid_args(
            "SessionTarget must use exactly one of session_id, name, or default=true",
        ));
    }
    Ok(())
}

/// Runs the parse requested role operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_requested_role(value: &str) -> Result<RequestedRole> {
    match value {
        "primary" => Ok(RequestedRole::Primary),
        "observer" => Ok(RequestedRole::Observer),
        "agent" => Ok(RequestedRole::Agent),
        "automation" => Ok(RequestedRole::Automation),
        _ => Err(MezError::invalid_args("unsupported requested_role")),
    }
}

/// Runs the client descriptor from json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn client_descriptor_from_json(body: &str) -> Result<ClientDescriptor> {
    reject_unknown_json_fields(
        body,
        "client descriptor",
        &[
            "name",
            "version",
            "pid",
            "host",
            "user",
            "purpose",
            "requested_role",
            "interactive",
            "stdio",
            "metadata",
            "terminal",
        ],
    )?;
    let object = parse_object(body, "client descriptor")?;
    let terminal = optional_object_member_json(&object, "terminal", "client descriptor")?
        .as_deref()
        .map(terminal_descriptor_from_json)
        .transpose()?;
    let requested_role = optional_string_member(&object, "requested_role", "client descriptor")?
        .map(|role| parse_requested_role(&role))
        .transpose()?;
    Ok(ClientDescriptor {
        name: required_string_member(&object, "name", "client descriptor")?,
        version: optional_string_member(&object, "version", "client descriptor")?,
        pid: optional_u32_member(&object, "pid", "client descriptor")?,
        host: optional_string_member(&object, "host", "client descriptor")?,
        user: optional_string_member(&object, "user", "client descriptor")?,
        purpose: optional_string_member(&object, "purpose", "client descriptor")?,
        requested_role,
        interactive: optional_bool_member(&object, "interactive", "client descriptor")?
            .unwrap_or(false),
        stdio: match object.get("stdio") {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => Some(stdio_descriptor_from_value(value)?),
        },
        metadata_json: optional_object_json_member(&object, "metadata", "client descriptor")?,
        terminal,
    })
}

/// Runs the terminal descriptor from json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn terminal_descriptor_from_json(body: &str) -> Result<TerminalDescriptor> {
    reject_unknown_json_fields(
        body,
        "terminal descriptor",
        &["columns", "rows", "term", "features"],
    )?;
    let object = parse_object(body, "terminal descriptor")?;
    let columns = required_u16_member(&object, "columns", "terminal descriptor")?;
    let rows = required_u16_member(&object, "rows", "terminal descriptor")?;
    if columns == 0 || rows == 0 {
        return Err(MezError::invalid_args(
            "terminal descriptor dimensions must be non-zero",
        ));
    }
    Ok(TerminalDescriptor {
        columns,
        rows,
        term: required_string_member(&object, "term", "terminal descriptor")?,
        features: optional_string_array_member(&object, "features", "terminal descriptor")?
            .unwrap_or_default(),
    })
}

/// Runs the ensure client descriptor role matches operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn ensure_client_descriptor_role_matches(
    client: &ClientDescriptor,
    requested_role: RequestedRole,
    context: &str,
) -> Result<()> {
    if client
        .requested_role
        .is_some_and(|descriptor_role| descriptor_role != requested_role)
    {
        return Err(MezError::invalid_args(format!(
            "{context} requested_role must match enclosing role"
        )));
    }
    Ok(())
}

/// Runs the parse object operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_object(body: &str, context: &str) -> Result<serde_json::Map<String, serde_json::Value>> {
    let value = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|_| MezError::invalid_args(format!("{context} must be a JSON object")))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| MezError::invalid_args(format!("{context} must be a JSON object")))
}

/// Runs the required string member operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_string_member(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<String> {
    match object.get(field) {
        Some(serde_json::Value::String(value)) if !value.trim().is_empty() => Ok(value.clone()),
        Some(_) => Err(MezError::invalid_args(format!(
            "{context} requires non-empty string {field}"
        ))),
        None => Err(MezError::invalid_args(format!(
            "{context} requires {field}"
        ))),
    }
}

/// Runs the optional string member operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_string_member(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<Option<String>> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) if !value.trim().is_empty() => {
            Ok(Some(value.clone()))
        }
        Some(_) => Err(MezError::invalid_args(format!(
            "{context} {field} must be a non-empty string"
        ))),
    }
}

/// Runs the optional bool member operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_bool_member(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<Option<bool>> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(MezError::invalid_args(format!(
            "{context} {field} must be a boolean"
        ))),
    }
}

/// Runs the optional u32 member operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_u32_member(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<Option<u32>> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(number)) => {
            let value = number.as_u64().ok_or_else(|| {
                MezError::invalid_args(format!("{context} {field} must be a non-negative integer"))
            })?;
            u32::try_from(value)
                .map(Some)
                .map_err(|_| MezError::invalid_args(format!("{context} {field} is too large")))
        }
        Some(_) => Err(MezError::invalid_args(format!(
            "{context} {field} must be a non-negative integer"
        ))),
    }
}

/// Runs the required u32 member operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_u32_member(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<u32> {
    match optional_u32_member(object, field, context)? {
        Some(value) => Ok(value),
        None => Err(MezError::invalid_args(format!(
            "{context} requires {field}"
        ))),
    }
}

/// Runs the required u16 member operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_u16_member(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<u16> {
    let value = object
        .get(field)
        .ok_or_else(|| MezError::invalid_args(format!("{context} requires {field}")))?;
    let Some(value) = value.as_u64() else {
        return Err(MezError::invalid_args(format!(
            "{context} {field} must be a non-negative integer"
        )));
    };
    u16::try_from(value)
        .map_err(|_| MezError::invalid_args(format!("{context} {field} is invalid")))
}

/// Runs the optional object member json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_object_member_json(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<Option<String>> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Object(_)) => serde_json::to_string(&object[field])
            .map(Some)
            .map_err(|_| MezError::invalid_args(format!("{context} {field} is invalid"))),
        Some(_) => Err(MezError::invalid_args(format!(
            "{context} {field} must be an object or null"
        ))),
    }
}

/// Runs the optional string array member operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_string_array_member(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<Option<Vec<String>>> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .map(|value| match value {
                serde_json::Value::String(value) if !value.trim().is_empty() => Ok(value.clone()),
                _ => Err(MezError::invalid_args(format!(
                    "{context} {field} must contain only non-empty strings"
                ))),
            })
            .collect::<Result<Vec<_>>>()
            .map(Some),
        Some(_) => Err(MezError::invalid_args(format!(
            "{context} {field} must be an array of strings"
        ))),
    }
}

/// Runs the optional object json member operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_object_json_member(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    context: &str,
) -> Result<Option<String>> {
    match object.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Object(_)) => serde_json::to_string(&object[field])
            .map(Some)
            .map_err(|_| MezError::invalid_args(format!("{context} {field} is invalid"))),
        Some(_) => Err(MezError::invalid_args(format!(
            "{context} {field} must be an object"
        ))),
    }
}

/// Runs the stdio descriptor from value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn stdio_descriptor_from_value(value: &serde_json::Value) -> Result<super::ClientStdioDescriptor> {
    let Some(object) = value.as_object() else {
        return Err(MezError::invalid_args(
            "client descriptor stdio must be an object",
        ));
    };
    for (field, value) in object {
        if field == "extensions" {
            if !value.is_object() {
                return Err(MezError::invalid_args(
                    "client descriptor stdio extensions must be an object",
                ));
            }
            continue;
        }
        if ![
            "stdin_is_tty",
            "stdout_is_tty",
            "stderr_is_tty",
            "controlling_tty",
            "tty_device",
        ]
        .contains(&field.as_str())
        {
            return Err(MezError::invalid_args(format!(
                "client descriptor stdio contains unknown field `{field}`"
            )));
        }
    }
    Ok(super::ClientStdioDescriptor {
        stdin_is_tty: optional_bool_member(object, "stdin_is_tty", "client descriptor stdio")?,
        stdout_is_tty: optional_bool_member(object, "stdout_is_tty", "client descriptor stdio")?,
        stderr_is_tty: optional_bool_member(object, "stderr_is_tty", "client descriptor stdio")?,
        controlling_tty: optional_string_member(
            object,
            "controlling_tty",
            "client descriptor stdio",
        )?,
        tty_device: optional_string_member(object, "tty_device", "client descriptor stdio")?,
    })
}

/// Runs the authentication from json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn authentication_from_json(body: &str) -> Result<AuthenticationMaterial> {
    reject_unknown_json_fields(body, "authentication material", &["mechanism", "token"])?;
    let mechanism = match json_string_field(body, "mechanism")
        .unwrap_or_else(|| "none".to_string())
        .as_str()
    {
        "peer_credentials" => AuthenticationMechanism::PeerCredentials,
        "bearer_token" => AuthenticationMechanism::BearerToken,
        "none" => AuthenticationMechanism::None,
        other if other.starts_with("extension:") => {
            AuthenticationMechanism::Extension(other["extension:".len()..].to_string())
        }
        _ => {
            return Err(MezError::invalid_args(
                "unsupported authentication mechanism",
            ));
        }
    };
    let token = json_string_field(body, "token");
    if mechanism == AuthenticationMechanism::BearerToken
        && token.as_ref().is_none_or(|token| token.trim().is_empty())
    {
        return Err(MezError::invalid_args(
            "bearer_token authentication requires token",
        ));
    }
    Ok(AuthenticationMaterial { mechanism, token })
}

/// Runs the initialize result json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn initialize_result_json(result: &InitializeResult) -> String {
    format!(
        r#"{{"selected_version":{},"server":{},"session":{},"granted_role":"{}","capabilities":{},"approval_pending":{},"observer_request":{}}}"#,
        result.selected_version,
        server_identity_json(&result.server),
        result.session.as_deref().unwrap_or("null"),
        granted_role_name(result.granted_role),
        capabilities_json(&result.capabilities),
        result.approval_pending,
        result
            .observer_request
            .as_ref()
            .map(|observer| {
                observer.state_json.clone().unwrap_or_else(|| {
                    format!(
                        r#"{{"request_id":"{}","state":"{}"}}"#,
                        json_escape(&observer.request_id),
                        json_escape(observer.state)
                    )
                })
            })
            .unwrap_or_else(|| "null".to_string())
    )
}

/// Runs the default initialize session summary json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn default_initialize_session_summary_json() -> String {
    r#"{"id":"default","version":1,"name":"default","state":"running","created_at":null,"last_attached_at":null,"window_count":0,"attached_client_count":0,"has_primary":false,"active_window_id":null}"#
        .to_string()
}

/// Runs the server identity json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn server_identity_json(server: &ServerIdentity) -> String {
    format!(
        r#"{{"id":"{}","implementation_name":"{}","version":"{}","protocol_versions":{},"started_at":"{}","user_id":{},"host":{},"pid":{}}}"#,
        json_escape(&server.id),
        json_escape(server.implementation_name),
        json_escape(server.version),
        u32_array_json(&server.protocol_versions),
        json_escape(&server.started_at),
        server
            .user_id
            .map(|user_id| user_id.to_string())
            .unwrap_or_else(|| "null".to_string()),
        json_optional_string(server.host.as_deref()),
        server
            .pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "null".to_string())
    )
}

/// Runs the capabilities json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn capabilities_json(capabilities: &Capabilities) -> String {
    format!(
        r#"{{"protocol_version":{},"methods":{},"event_types":{},"roles":{},"transports":{},"limits":{{"max_frame_size":{},"max_request_size":{},"max_event_replay_retention":{},"max_capture_payload_size":{}}},"features":{{"tcp":{},"event_replay":{},"observers":{},"mcp":{},"snapshots":{},"audit":{},"approval_bypass":{}}}}}"#,
        capabilities.protocol_version,
        static_str_array_json(&capabilities.methods),
        static_str_array_json(&capabilities.event_types),
        static_str_array_json(&capabilities.roles),
        static_str_array_json(&capabilities.transports),
        capabilities.limits.max_frame_size,
        capabilities.limits.max_request_size,
        capabilities.limits.max_event_replay_retention,
        capabilities.limits.max_capture_payload_size,
        capabilities.features.tcp,
        capabilities.features.event_replay,
        capabilities.features.observers,
        capabilities.features.mcp,
        capabilities.features.snapshots,
        capabilities.features.audit,
        capabilities.features.approval_bypass
    )
}

/// Runs the u32 array json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn u32_array_json(values: &[u32]) -> String {
    let encoded = values
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    format!("[{}]", encoded.join(","))
}

/// Runs the static str array json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn static_str_array_json(values: &[&'static str]) -> String {
    let encoded = values
        .iter()
        .map(|value| format!(r#""{}""#, json_escape(value)))
        .collect::<Vec<_>>();
    format!("[{}]", encoded.join(","))
}
