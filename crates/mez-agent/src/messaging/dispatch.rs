//! MMP request dispatch and fanout flushing.
//!
//! Dispatch translates JSON protocol bodies into message-service operations and
//! emits JSON responses suitable for framed transport replies.

use mez_core::ids::{AgentId, StableId};

use super::error::{MessageError, Result};
use super::json::{
    delivery_batch_json, json_escape, json_number_field, json_object_field, json_optional,
    json_string_field, mmp_delivery_status, mmp_error, mmp_error_code, sender_identity_json,
    top_level_json_string_field,
};
use super::types::{
    AgentPresenceStatus, DeliveryStatus, Envelope, MMP_PROTOCOL, MMP_UNSUPPORTED_PROTOCOL_MESSAGE,
    MessageConnection, MessageScope, MessageSequence, MessageService, Recipient, SenderIdentity,
};
use super::validation::{
    normalize_objective, validate_message_type, validate_mmp_payload_metadata, validate_protocol,
};

/// Runs the dispatch mmp body operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn dispatch_mmp_body(
    body: &str,
    service: &mut MessageService,
    connection: &mut MessageConnection,
    now_ms: u64,
) -> String {
    if let Some(protocol) = top_level_json_string_field(body, "protocol")
        && protocol != MMP_PROTOCOL
    {
        return mmp_error(
            "unsupported_protocol",
            MMP_UNSUPPORTED_PROTOCOL_MESSAGE,
            false,
            "rejected",
        );
    }
    match dispatch_mmp_body_result(body, service, connection, now_ms) {
        Ok(response) => response,
        Err(error) => mmp_error(
            mmp_error_code(&error),
            error.message(),
            false,
            mmp_delivery_status(&error),
        ),
    }
}

/// Runs the dispatch mmp body result operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn dispatch_mmp_body_result(
    body: &str,
    service: &mut MessageService,
    connection: &mut MessageConnection,
    now_ms: u64,
) -> Result<String> {
    let protocol = json_string_field(body, "protocol")
        .ok_or_else(|| MessageError::invalid_args("MMP envelope requires protocol"))?;
    validate_protocol(&protocol)?;
    let message_type = json_string_field(body, "type")
        .or_else(|| json_string_field(body, "message_type"))
        .ok_or_else(|| MessageError::invalid_args("MMP envelope requires type"))?;
    validate_message_type(&message_type)?;

    match message_type.as_str() {
        "hello" => {
            let role = optional_message_label_field(body, "role")?
                .unwrap_or_else(|| "default".to_string());
            let capabilities = optional_message_string_array_field(body, "capabilities")?;
            let objective = optional_message_objective_field(body)?;
            let identity = service.register_agent_with_objective(
                None,
                None,
                role,
                capabilities,
                objective.as_deref(),
            )?;
            let cursor = service.subscribe(&identity.agent_id)?;
            connection.agent_id = Some(identity.agent_id.clone());
            connection.delivery_cursor = Some(cursor);
            Ok(format!(
                r#"{{"protocol":"mmp/1","type":"welcome","identity":{}}}"#,
                sender_identity_json(&identity)
            ))
        }
        "discover" => {
            require_registered_connection(connection)?;
            let requester = connection
                .agent_id
                .as_ref()
                .ok_or_else(|| MessageError::forbidden("unregistered agent connection"))?;
            let agent_id = json_string_field(body, "agent_id");
            let pane_id = json_string_field(body, "pane_id");
            let window_id = json_string_field(body, "window_id");
            let role = optional_message_label_field(body, "role")?;
            let status = json_string_field(body, "status")
                .as_deref()
                .map(parse_presence_status)
                .transpose()?;
            let capabilities = optional_message_string_array_field(body, "capabilities")?;
            let scope = parse_transport_message_scope(body)?;
            let scope_name = match scope {
                MessageScope::Project => "project",
                MessageScope::Session => "session",
            };
            Ok(format!(
                r#"{{"protocol":"mmp/1","type":"discover_result","scope":"{}","agents":[{}]}}"#,
                scope_name,
                service
                    .discover_agents_filtered_for_requester(
                        requester,
                        scope,
                        agent_id.as_deref(),
                        pane_id.as_deref(),
                        window_id.as_deref(),
                        role.as_deref(),
                        status,
                        &capabilities
                    )
                    .iter()
                    .map(sender_identity_json)
                    .collect::<Vec<_>>()
                    .join(",")
            ))
        }
        "presence" => {
            let agent_id = require_registered_connection(connection)?.clone();
            let status = json_string_field(body, "status")
                .as_deref()
                .map(parse_presence_status)
                .transpose()?
                .unwrap_or(AgentPresenceStatus::Available);
            service.update_presence(&agent_id, status, now_ms)?;
            if let Some(objective) = optional_message_objective_field(body)? {
                service.update_agent_objective(&agent_id, Some(&objective), now_ms)?;
            }
            Ok(format!(
                r#"{{"protocol":"mmp/1","type":"ack","message_id":{},"queued_recipients":0}}"#,
                json_optional(json_string_field(body, "id").as_deref())
            ))
        }
        "heartbeat" => {
            let agent_id = require_registered_connection(connection)?.clone();
            service.record_heartbeat(&agent_id, now_ms)?;
            Ok(format!(
                r#"{{"protocol":"mmp/1","type":"ack","message_id":{},"queued_recipients":0}}"#,
                json_optional(json_string_field(body, "id").as_deref())
            ))
        }
        "send" | "task_status" | "task_result" => {
            let agent_id = require_registered_connection(connection)?.clone();
            let sender = service
                .registered_identity(&agent_id)
                .cloned()
                .ok_or_else(|| MessageError::forbidden("unregistered agent connection"))?;
            let scope = parse_transport_message_scope(body)?;
            let envelope = parse_transport_envelope(body, message_type, sender)?;
            let delivery = service.accept_at_with_scope(&agent_id, envelope, scope, now_ms)?;
            Ok(format!(
                r#"{{"protocol":"mmp/1","type":"ack","message_id":"{}","queued_recipients":{},"status":"{}"}}"#,
                json_escape(&delivery.message_id),
                delivery.queued_recipients,
                delivery_status_name(delivery.status)
            ))
        }
        "transport/receive" | "mmp.receive" => {
            let agent_id = require_registered_connection(connection)?.clone();
            if service.subscription(&agent_id).is_none() {
                connection.delivery_cursor = Some(service.subscribe(&agent_id)?);
            }
            let limit = optional_message_usize_field(body, "limit")?.unwrap_or(100);
            let batch = service.receive_subscribed(&agent_id, now_ms, limit)?;
            Ok(delivery_batch_json(&batch))
        }
        "ack" => {
            let agent_id = require_registered_connection(connection)?.clone();
            let sequence = json_number_field(body, "sequence")
                .or_else(|| json_number_field(body, "last_sequence"))
                .ok_or_else(|| MessageError::invalid_args("ack requires sequence"))?
                .parse::<MessageSequence>()
                .map_err(|_| MessageError::invalid_args("ack sequence is invalid"))?;
            let cursor = service.advance_subscription(&agent_id, sequence)?;
            connection.delivery_cursor = Some(cursor.clone());
            Ok(format!(
                r#"{{"protocol":"mmp/1","type":"ack","last_sequence":{}}}"#,
                cursor.last_sequence
            ))
        }
        _ => Err(MessageError::invalid_args(
            "message type is not accepted on this endpoint",
        )),
    }
}

/// Runs the parse transport envelope operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_transport_envelope(
    body: &str,
    message_type: String,
    sender: SenderIdentity,
) -> Result<Envelope> {
    let value = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|_| MessageError::invalid_args("MMP envelope must be a JSON object"))?;
    let object = value
        .as_object()
        .ok_or_else(|| MessageError::invalid_args("MMP envelope must be a JSON object"))?;
    validate_transport_sender(object, &sender)?;
    let id = required_transport_string(object, "id", "MMP envelope requires id")?;
    let time = required_transport_string(object, "time", "MMP envelope requires time")?;
    let recipient_body = json_object_field(body, "recipient")
        .ok_or_else(|| MessageError::invalid_args("MMP envelope requires recipient"))?;
    let recipient = parse_recipient(&recipient_body)?;
    let content_type =
        required_transport_string(object, "content_type", "MMP envelope requires content_type")?;
    let payload = required_transport_payload(object)?;
    validate_transport_payload(object, &message_type, &content_type, &payload)?;
    let ttl_ms = required_nullable_transport_u64(object, "ttl_ms")?;
    let correlation_id = required_nullable_transport_string(object, "correlation_id")?;
    let extension_fields = preserved_transport_extension_fields(object);

    Ok(Envelope {
        protocol: "mmp/1",
        id,
        message_type,
        time,
        sender,
        recipient,
        correlation_id,
        ttl_ms,
        content_type,
        payload,
        extension_fields,
    })
}

/// Runs the preserved transport extension fields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn preserved_transport_extension_fields(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Vec<(String, String)> {
    object
        .iter()
        .filter(|(field, _)| !is_reserved_envelope_field(field))
        .map(|(field, value)| (field.clone(), value.to_string()))
        .collect()
}

/// Runs the is reserved envelope field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn is_reserved_envelope_field(field: &str) -> bool {
    matches!(
        field,
        "protocol"
            | "id"
            | "type"
            | "message_type"
            | "sequence"
            | "time"
            | "sender"
            | "recipient"
            | "scope"
            | "correlation_id"
            | "ttl_ms"
            | "content_type"
            | "payload"
    )
}

/// Runs the required transport string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_transport_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    missing_message: &str,
) -> Result<String> {
    let value = object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| MessageError::invalid_args(missing_message))?;
    Ok(value.to_string())
}

/// Runs the required transport payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_transport_payload(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<String> {
    let value = object
        .get("payload")
        .ok_or_else(|| MessageError::invalid_args("MMP envelope requires payload"))?;
    Ok(match value {
        serde_json::Value::String(text) => text.clone(),
        value => value.to_string(),
    })
}

/// Validates transport payload metadata that depends on the declared media type.
fn validate_transport_payload(
    object: &serde_json::Map<String, serde_json::Value>,
    message_type: &str,
    content_type: &str,
    payload: &str,
) -> Result<()> {
    let payload_encoding = optional_transport_string(object, "payload_encoding")?;
    validate_mmp_payload_metadata(
        message_type,
        content_type,
        payload,
        payload_encoding.as_deref(),
    )
}

/// Returns an optional string field from a transport envelope extension.
fn optional_transport_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<String>> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    value
        .as_str()
        .map(|value| Some(value.to_string()))
        .ok_or_else(|| MessageError::invalid_args(format!("MMP envelope {field} must be a string")))
}

/// Parses an optional raw MMP delivery audience, defaulting to project.
fn parse_transport_message_scope(body: &str) -> Result<MessageScope> {
    let body = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|_| MessageError::invalid_args("MMP envelope scope must be project or session"))?;
    let object = body.as_object().ok_or_else(|| {
        MessageError::invalid_args("MMP envelope scope must be project or session")
    })?;
    let scope = match object.get("scope") {
        None | Some(serde_json::Value::Null) => "project",
        Some(serde_json::Value::String(scope)) => scope,
        Some(_) => {
            return Err(MessageError::invalid_args(
                "MMP envelope scope must be project or session",
            ));
        }
    };
    match scope {
        "project" => Ok(MessageScope::Project),
        "session" => Ok(MessageScope::Session),
        _ => Err(MessageError::invalid_args(
            "MMP envelope scope must be project or session",
        )),
    }
}

/// Runs the required nullable transport string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_nullable_transport_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<String>> {
    let value = object
        .get(field)
        .ok_or_else(|| MessageError::invalid_args(format!("MMP envelope requires {field}")))?;
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_str()
        .map(|value| Some(value.to_string()))
        .ok_or_else(|| MessageError::invalid_args(format!("MMP envelope {field} must be a string")))
}

/// Runs the required nullable transport u64 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_nullable_transport_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<u64>> {
    let value = object
        .get(field)
        .ok_or_else(|| MessageError::invalid_args(format!("MMP envelope requires {field}")))?;
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_u64()
        .map(Some)
        .ok_or_else(|| MessageError::invalid_args(format!("MMP envelope {field} must be a number")))
}

/// Runs the validate transport sender operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_transport_sender(
    object: &serde_json::Map<String, serde_json::Value>,
    expected: &SenderIdentity,
) -> Result<()> {
    let sender_value = object
        .get("sender")
        .ok_or_else(|| MessageError::invalid_args("MMP envelope requires sender"))?;
    let sender = sender_value
        .as_object()
        .ok_or_else(|| MessageError::invalid_args("MMP sender must be an object"))?;
    let agent_id = sender
        .get("agent_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MessageError::invalid_args("MMP sender requires agent_id"))?;
    if agent_id != expected.agent_id.as_str() {
        return Err(MessageError::forbidden(
            "message sender does not match authenticated agent connection",
        ));
    }
    validate_optional_sender_id(
        sender,
        "pane_id",
        expected.pane_id.as_ref().map(|id| id.as_str()),
    )?;
    validate_optional_sender_id(
        sender,
        "window_id",
        expected.window_id.as_ref().map(|id| id.as_str()),
    )?;
    validate_optional_sender_id(sender, "role", expected.role.as_deref())?;
    if let Some(capabilities) = sender.get("capabilities") {
        let Some(capabilities) = capabilities.as_array() else {
            return Err(MessageError::invalid_args(
                "MMP sender capabilities must be an array",
            ));
        };
        let parsed = capabilities
            .iter()
            .map(|capability| {
                capability
                    .as_str()
                    .ok_or_else(|| MessageError::invalid_args("MMP sender capability is invalid"))
                    .map(str::to_string)
            })
            .collect::<Result<Vec<_>>>()?;
        if parsed != expected.capabilities {
            return Err(MessageError::forbidden(
                "message sender does not match authenticated agent connection",
            ));
        }
    }
    Ok(())
}

/// Runs the validate optional sender id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_optional_sender_id(
    sender: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    expected: Option<&str>,
) -> Result<()> {
    let Some(value) = sender.get(field) else {
        return Ok(());
    };
    if value.is_null() {
        if expected.is_none() {
            return Ok(());
        }
        return Err(MessageError::forbidden(
            "message sender does not match authenticated agent connection",
        ));
    }
    let actual = value
        .as_str()
        .ok_or_else(|| MessageError::invalid_args("MMP sender field is invalid"))?;
    if Some(actual) == expected {
        Ok(())
    } else {
        Err(MessageError::forbidden(
            "message sender does not match authenticated agent connection",
        ))
    }
}

/// Runs the require registered connection operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn require_registered_connection(connection: &MessageConnection) -> Result<&AgentId> {
    connection
        .agent_id
        .as_ref()
        .ok_or_else(|| MessageError::forbidden("message connection has not sent hello"))
}

/// Runs the parse presence status operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_presence_status(value: &str) -> Result<AgentPresenceStatus> {
    match value {
        "available" => Ok(AgentPresenceStatus::Available),
        "busy" => Ok(AgentPresenceStatus::Busy),
        "blocked" => Ok(AgentPresenceStatus::Blocked),
        "offline" => Ok(AgentPresenceStatus::Offline),
        _ => Err(MessageError::invalid_args("unsupported presence status")),
    }
}

/// Runs the delivery status name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn delivery_status_name(status: DeliveryStatus) -> &'static str {
    match status {
        DeliveryStatus::Accepted => "accepted",
        DeliveryStatus::Undeliverable => "undeliverable",
        DeliveryStatus::Expired => "expired",
    }
}

/// Runs the parse recipient operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_recipient(body: &str) -> Result<Recipient> {
    let value = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|_| MessageError::invalid_args("recipient must be a JSON object"))?;
    let object = value
        .as_object()
        .ok_or_else(|| MessageError::invalid_args("recipient must be a JSON object"))?;
    let mut selectors = Vec::new();

    if let Some(session) = object.get("session") {
        let session = session
            .as_bool()
            .ok_or_else(|| MessageError::invalid_args("recipient session must be a boolean"))?;
        if session {
            selectors.push(Recipient::Session);
        }
    }
    if let Some(agent_id) = recipient_string_field(object, "agent_id")? {
        selectors.push(
            StableId::parse('a', agent_id)
                .map(Recipient::Agent)
                .ok_or_else(|| MessageError::invalid_args("recipient agent_id is invalid"))?,
        );
    }
    if let Some(pane_id) = recipient_string_field(object, "pane_id")? {
        selectors.push(
            StableId::parse('%', pane_id)
                .map(Recipient::Pane)
                .ok_or_else(|| MessageError::invalid_args("recipient pane_id is invalid"))?,
        );
    }
    if let Some(window_id) = recipient_string_field(object, "window_id")? {
        selectors.push(
            StableId::parse('@', window_id)
                .map(Recipient::Window)
                .ok_or_else(|| MessageError::invalid_args("recipient window_id is invalid"))?,
        );
    }
    if let Some(role) = recipient_string_field(object, "role")? {
        selectors.push(Recipient::Role(role.to_string()));
    }
    if let Some(capability) = recipient_string_field(object, "capability")? {
        selectors.push(Recipient::Capability(capability.to_string()));
    }
    if let Some(group) = recipient_string_field(object, "group")? {
        selectors.push(Recipient::Group(group.to_string()));
    }

    match selectors.len() {
        0 => Err(MessageError::invalid_args(
            "recipient object has no supported target",
        )),
        1 => Ok(selectors.remove(0)),
        _ => Err(MessageError::invalid_args(
            "recipient object contains multiple independent selectors",
        )),
    }
}

/// Runs the optional message label field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_message_label_field(body: &str, field: &str) -> Result<Option<String>> {
    let value = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|_| MessageError::invalid_args("MMP envelope must be a JSON object"))?;
    let object = value
        .as_object()
        .ok_or_else(|| MessageError::invalid_args("MMP envelope must be a JSON object"))?;
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| MessageError::invalid_args(format!("MMP {field} must be a string")))?;
    validate_message_label(field, value)?;
    Ok(Some(value.to_string()))
}

/// Runs the optional message objective field operation for this subsystem.
///
/// The objective is additive to mmp/1: it is omitted when absent and is
/// normalized with the shared objective bounds when present, so no unbounded or
/// control-character bearing value can be published to discovery.
fn optional_message_objective_field(body: &str) -> Result<Option<String>> {
    let value = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|_| MessageError::invalid_args("MMP envelope must be a JSON object"))?;
    let object = value
        .as_object()
        .ok_or_else(|| MessageError::invalid_args("MMP envelope must be a JSON object"))?;
    let Some(value) = object.get("objective") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .as_str()
        .ok_or_else(|| MessageError::invalid_args("MMP objective must be a string"))?;
    normalize_objective(value).map(Some)
}

/// Runs the optional message string array field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_message_string_array_field(body: &str, field: &str) -> Result<Vec<String>> {
    let value = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|_| MessageError::invalid_args("MMP envelope must be a JSON object"))?;
    let object = value
        .as_object()
        .ok_or_else(|| MessageError::invalid_args("MMP envelope must be a JSON object"))?;
    let Some(value) = object.get(field) else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| MessageError::invalid_args(format!("MMP {field} must be an array")))?;
    values
        .iter()
        .map(|value| {
            let value = value.as_str().ok_or_else(|| {
                MessageError::invalid_args(format!("MMP {field} entry is invalid"))
            })?;
            validate_message_label(field, value)?;
            Ok(value.to_string())
        })
        .collect()
}

/// Runs the optional message usize field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_message_usize_field(body: &str, field: &str) -> Result<Option<usize>> {
    let value = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|_| MessageError::invalid_args("MMP envelope must be a JSON object"))?;
    let object = value
        .as_object()
        .ok_or_else(|| MessageError::invalid_args("MMP envelope must be a JSON object"))?;
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    let value = value.as_u64().ok_or_else(|| {
        MessageError::invalid_args(format!("MMP {field} must be a non-negative integer"))
    })?;
    let value = usize::try_from(value)
        .map_err(|_| MessageError::invalid_args(format!("MMP {field} is too large")))?;
    Ok(Some(value))
}

/// Runs the recipient string field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn recipient_string_field<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<&'a str>> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| MessageError::invalid_args(format!("recipient {field} must be a string")))?;
    validate_message_label(&format!("recipient {field}"), value)?;
    Ok(Some(value))
}

/// Runs the validate message label operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_message_label(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(MessageError::invalid_args(format!("{field} is invalid")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MessageConnection, MessageService, dispatch_mmp_body};

    /// Discover defaults to the authenticated requester's project membership
    /// and widens only when the caller explicitly requests session scope.
    #[test]
    fn discover_defaults_to_project_and_session_scope_widens() {
        let mut service = MessageService::default();
        let mut connection = MessageConnection::default();
        dispatch_mmp_body(
            r#"{"protocol":"mmp/1","type":"hello","role":"agent"}"#,
            &mut service,
            &mut connection,
            10,
        );
        let requester = connection.agent_id.clone().unwrap();
        let alpha = super::super::ProjectScopeId::from_canonical_root_bytes(b"/workspace/alpha");
        let beta = super::super::ProjectScopeId::from_canonical_root_bytes(b"/workspace/beta");
        service
            .rebind_agent_project_scope(&requester, Some(alpha.clone()))
            .unwrap();
        let same_project = service.register_agent(None, None, "agent", Vec::new());
        service
            .rebind_agent_project_scope(&same_project.agent_id, Some(alpha))
            .unwrap();
        let other_project = service.register_agent(None, None, "agent", Vec::new());
        service
            .rebind_agent_project_scope(&other_project.agent_id, Some(beta))
            .unwrap();

        let project = dispatch_mmp_body(
            r#"{"protocol":"mmp/1","type":"discover"}"#,
            &mut service,
            &mut connection,
            20,
        );
        assert!(project.contains(r#""scope":"project""#), "{project}");
        assert!(
            project.contains(same_project.agent_id.as_str()),
            "{project}"
        );
        assert!(
            !project.contains(other_project.agent_id.as_str()),
            "{project}"
        );

        let null_scope = dispatch_mmp_body(
            r#"{"protocol":"mmp/1","type":"discover","scope":null}"#,
            &mut service,
            &mut connection,
            20,
        );
        assert!(null_scope.contains(r#""scope":"project""#), "{null_scope}");
        assert!(
            !null_scope.contains(other_project.agent_id.as_str()),
            "{null_scope}"
        );

        let session = dispatch_mmp_body(
            r#"{"protocol":"mmp/1","type":"discover","scope":"session"}"#,
            &mut service,
            &mut connection,
            20,
        );
        assert!(session.contains(r#""scope":"session""#), "{session}");
        assert!(
            session.contains(other_project.agent_id.as_str()),
            "{session}"
        );

        let service_snapshot = service.snapshot_state();
        let invalid_scope = dispatch_mmp_body(
            r#"{"protocol":"mmp/1","type":"discover","scope":"workspace"}"#,
            &mut service,
            &mut connection,
            20,
        );
        assert!(
            invalid_scope.contains("MMP envelope scope must be project or session"),
            "{invalid_scope}"
        );
        assert_eq!(service.snapshot_state(), service_snapshot);
        for invalid_scope in ["true", "1", "[]", "{}"] {
            let response = dispatch_mmp_body(
                &format!(r#"{{"protocol":"mmp/1","type":"discover","scope":{invalid_scope}}}"#),
                &mut service,
                &mut connection,
                20,
            );
            assert!(
                response.contains("MMP envelope scope must be project or session"),
                "{response}"
            );
            assert!(!response.contains(requester.as_str()), "{response}");
            assert!(
                !response.contains(same_project.agent_id.as_str()),
                "{response}"
            );
            assert!(
                !response.contains(other_project.agent_id.as_str()),
                "{response}"
            );
            assert_eq!(service.snapshot_state(), service_snapshot);
        }
        for malformed_body in [
            r#"{"protocol":"mmp/1","type":"discover""#,
            r#"{"protocol":"mmp/1","type":"discover","scope":"project""#,
        ] {
            let response = dispatch_mmp_body(malformed_body, &mut service, &mut connection, 20);
            assert!(response.contains(r#""type":"error""#), "{response}");
            assert!(!response.contains(requester.as_str()), "{response}");
            assert!(
                !response.contains(same_project.agent_id.as_str()),
                "{response}"
            );
            assert!(
                !response.contains(other_project.agent_id.as_str()),
                "{response}"
            );
            assert_eq!(service.snapshot_state(), service_snapshot);
        }
    }

    /// Hello carries the additive objective into welcome, discovery results, and
    /// envelope senders while leaving role and capability filters unchanged.
    #[test]
    fn hello_objective_round_trips_through_welcome_and_discovery() {
        let mut service = MessageService::default();
        let mut connection = MessageConnection::default();
        let welcome = dispatch_mmp_body(
            r#"{"protocol":"mmp/1","type":"hello","role":"reviewer","capabilities":["rust"],"objective":"Inspect the MMP discovery contract"}"#,
            &mut service,
            &mut connection,
            10,
        );
        assert!(
            welcome.contains(r#""objective":"Inspect the MMP discovery contract""#),
            "{welcome}"
        );

        let discover = dispatch_mmp_body(
            r#"{"protocol":"mmp/1","type":"discover","role":"reviewer","capabilities":["rust"]}"#,
            &mut service,
            &mut connection,
            20,
        );
        assert!(
            discover.contains(r#""type":"discover_result""#),
            "{discover}"
        );
        assert!(
            discover.contains(r#""objective":"Inspect the MMP discovery contract""#),
            "{discover}"
        );

        let unmatched = dispatch_mmp_body(
            r#"{"protocol":"mmp/1","type":"discover","role":"writer","capabilities":["docs"]}"#,
            &mut service,
            &mut connection,
            20,
        );
        assert!(unmatched.ends_with(r#""agents":[]}"#), "{unmatched}");
    }

    /// A hello without an objective omits the additive field, so legacy
    /// payloads deserialize to no objective.
    #[test]
    fn hello_without_objective_omits_the_additive_field() {
        let mut service = MessageService::default();
        let mut connection = MessageConnection::default();
        let welcome = dispatch_mmp_body(
            r#"{"protocol":"mmp/1","type":"hello"}"#,
            &mut service,
            &mut connection,
            10,
        );
        assert!(welcome.contains(r#""type":"welcome""#), "{welcome}");
        assert!(!welcome.contains("objective"), "{welcome}");
        assert!(service.presence()[0].identity.objective.is_none());
    }

    /// An unbounded or control-character bearing hello objective is rejected
    /// instead of being published to discovery.
    #[test]
    fn hello_rejects_invalid_objective() {
        let mut service = MessageService::default();
        let mut connection = MessageConnection::default();
        let rejected = dispatch_mmp_body(
            r#"{"protocol":"mmp/1","type":"hello","objective":"inspect\u0007the pane"}"#,
            &mut service,
            &mut connection,
            10,
        );
        assert!(rejected.contains(r#""type":"error""#), "{rejected}");
        assert!(rejected.contains("control characters"), "{rejected}");
        assert!(service.presence().is_empty());
    }

    /// Presence publishes a changed objective and keeps an unchanged one, with
    /// no additional objective write for the identical value.
    #[test]
    fn presence_objective_refresh_applies_changes_only() {
        let mut service = MessageService::default();
        let mut connection = MessageConnection::default();
        dispatch_mmp_body(
            r#"{"protocol":"mmp/1","type":"hello","objective":"Inspect the backlog"}"#,
            &mut service,
            &mut connection,
            10,
        );
        dispatch_mmp_body(
            r#"{"protocol":"mmp/1","type":"presence","status":"busy","objective":"Inspect the backlog"}"#,
            &mut service,
            &mut connection,
            20,
        );
        assert_eq!(
            service
                .registered_identity(&connection.agent_id.clone().unwrap())
                .and_then(|identity| identity.objective.as_deref()),
            Some("Inspect the backlog")
        );
        dispatch_mmp_body(
            r#"{"protocol":"mmp/1","type":"presence","status":"available","objective":"Review the backlog"}"#,
            &mut service,
            &mut connection,
            30,
        );
        assert_eq!(
            service
                .registered_identity(&connection.agent_id.clone().unwrap())
                .and_then(|identity| identity.objective.as_deref()),
            Some("Review the backlog")
        );
    }
}
