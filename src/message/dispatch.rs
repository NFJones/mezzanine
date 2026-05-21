//! MMP request dispatch and fanout flushing.
//!
//! Dispatch translates JSON protocol bodies into message-service operations and
//! emits JSON responses suitable for framed transport replies.

use crate::error::{MezError, Result};
use crate::ids::{AgentId, StableId};

use super::framing::encode_mmp_body;
use super::json::{
    delivery_batch_json, json_escape, json_number_field, json_object_field, json_optional,
    json_string_field, mmp_delivery_status, mmp_error, mmp_error_code, sender_identity_json,
    top_level_json_string_field,
};
use super::types::{
    AgentPresenceStatus, DeliveryStatus, Envelope, MMP_PROTOCOL, MMP_UNSUPPORTED_PROTOCOL_MESSAGE,
    MessageConnection, MessageSequence, MessageService, Recipient, SenderIdentity,
};
use super::validation::{validate_message_type, validate_mmp_payload_metadata, validate_protocol};

/// Defines the Message Fanout Sink behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary
/// consumed by higher-level orchestration code.
pub trait MessageFanoutSink {
    /// Runs the send frame operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_frame(&mut self, recipient: &AgentId, frame: &[u8]) -> Result<()>;
}

/// Runs the flush message fanout operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn flush_message_fanout(
    service: &mut MessageService,
    now_ms: u64,
    limit_per_recipient: usize,
    sink: &mut impl MessageFanoutSink,
) -> Result<usize> {
    let batches = service.fanout_ready(now_ms, limit_per_recipient);
    let mut sent = 0usize;
    for batch in batches {
        let body = delivery_batch_json(&batch.batch);
        let frame = encode_mmp_body(&body);
        sink.send_frame(&batch.recipient, &frame)?;
        service.acknowledge_fanout_batch(&batch)?;
        sent += batch.batch.messages.len();
    }
    Ok(sent)
}

/// Runs the flush message fanout for operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn flush_message_fanout_for(
    service: &mut MessageService,
    recipient: &AgentId,
    now_ms: u64,
    limit: usize,
    sink: &mut impl MessageFanoutSink,
) -> Result<usize> {
    let Some(batch) = service.fanout_ready_for(recipient, now_ms, limit)? else {
        return Ok(0);
    };
    let body = delivery_batch_json(&batch.batch);
    let frame = encode_mmp_body(&body);
    sink.send_frame(&batch.recipient, &frame)?;
    service.acknowledge_fanout_batch(&batch)?;
    Ok(batch.batch.messages.len())
}

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
        .ok_or_else(|| MezError::invalid_args("MMP envelope requires protocol"))?;
    validate_protocol(&protocol)?;
    let message_type = json_string_field(body, "type")
        .or_else(|| json_string_field(body, "message_type"))
        .ok_or_else(|| MezError::invalid_args("MMP envelope requires type"))?;
    validate_message_type(&message_type)?;

    match message_type.as_str() {
        "hello" => {
            let role = optional_message_label_field(body, "role")?
                .unwrap_or_else(|| "default".to_string());
            let capabilities = optional_message_string_array_field(body, "capabilities")?;
            let identity = service.register_agent(None, None, role, capabilities);
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
            let agent_id = json_string_field(body, "agent_id");
            let pane_id = json_string_field(body, "pane_id");
            let window_id = json_string_field(body, "window_id");
            let role = optional_message_label_field(body, "role")?;
            let status = json_string_field(body, "status")
                .as_deref()
                .map(parse_presence_status)
                .transpose()?;
            let capabilities = optional_message_string_array_field(body, "capabilities")?;
            Ok(format!(
                r#"{{"protocol":"mmp/1","type":"discover_result","agents":[{}]}}"#,
                service
                    .discover_agents_filtered(
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
                .ok_or_else(|| MezError::forbidden("unregistered agent connection"))?;
            let envelope = parse_transport_envelope(body, message_type, sender)?;
            let delivery = service.accept_at(&agent_id, envelope, now_ms)?;
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
                .ok_or_else(|| MezError::invalid_args("ack requires sequence"))?
                .parse::<MessageSequence>()
                .map_err(|_| MezError::invalid_args("ack sequence is invalid"))?;
            let cursor = service.advance_subscription(&agent_id, sequence)?;
            connection.delivery_cursor = Some(cursor.clone());
            Ok(format!(
                r#"{{"protocol":"mmp/1","type":"ack","last_sequence":{}}}"#,
                cursor.last_sequence
            ))
        }
        _ => Err(MezError::invalid_args(
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
        .map_err(|_| MezError::invalid_args("MMP envelope must be a JSON object"))?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("MMP envelope must be a JSON object"))?;
    validate_transport_sender(object, &sender)?;
    let id = required_transport_string(object, "id", "MMP envelope requires id")?;
    let time = required_transport_string(object, "time", "MMP envelope requires time")?;
    let recipient_body = json_object_field(body, "recipient")
        .ok_or_else(|| MezError::invalid_args("MMP envelope requires recipient"))?;
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
        .ok_or_else(|| MezError::invalid_args(missing_message))?;
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
        .ok_or_else(|| MezError::invalid_args("MMP envelope requires payload"))?;
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
        .ok_or_else(|| MezError::invalid_args(format!("MMP envelope {field} must be a string")))
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
        .ok_or_else(|| MezError::invalid_args(format!("MMP envelope requires {field}")))?;
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_str()
        .map(|value| Some(value.to_string()))
        .ok_or_else(|| MezError::invalid_args(format!("MMP envelope {field} must be a string")))
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
        .ok_or_else(|| MezError::invalid_args(format!("MMP envelope requires {field}")))?;
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_u64()
        .map(Some)
        .ok_or_else(|| MezError::invalid_args(format!("MMP envelope {field} must be a number")))
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
        .ok_or_else(|| MezError::invalid_args("MMP envelope requires sender"))?;
    let sender = sender_value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("MMP sender must be an object"))?;
    let agent_id = sender
        .get("agent_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_args("MMP sender requires agent_id"))?;
    if agent_id != expected.agent_id.as_str() {
        return Err(MezError::forbidden(
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
            return Err(MezError::invalid_args(
                "MMP sender capabilities must be an array",
            ));
        };
        let parsed = capabilities
            .iter()
            .map(|capability| {
                capability
                    .as_str()
                    .ok_or_else(|| MezError::invalid_args("MMP sender capability is invalid"))
                    .map(str::to_string)
            })
            .collect::<Result<Vec<_>>>()?;
        if parsed != expected.capabilities {
            return Err(MezError::forbidden(
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
        return Err(MezError::forbidden(
            "message sender does not match authenticated agent connection",
        ));
    }
    let actual = value
        .as_str()
        .ok_or_else(|| MezError::invalid_args("MMP sender field is invalid"))?;
    if Some(actual) == expected {
        Ok(())
    } else {
        Err(MezError::forbidden(
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
        .ok_or_else(|| MezError::forbidden("message connection has not sent hello"))
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
        _ => Err(MezError::invalid_args("unsupported presence status")),
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
        .map_err(|_| MezError::invalid_args("recipient must be a JSON object"))?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("recipient must be a JSON object"))?;
    let mut selectors = Vec::new();

    if let Some(session) = object.get("session") {
        let session = session
            .as_bool()
            .ok_or_else(|| MezError::invalid_args("recipient session must be a boolean"))?;
        if session {
            selectors.push(Recipient::Session);
        }
    }
    if let Some(agent_id) = recipient_string_field(object, "agent_id")? {
        selectors.push(
            StableId::parse('a', agent_id)
                .map(Recipient::Agent)
                .ok_or_else(|| MezError::invalid_args("recipient agent_id is invalid"))?,
        );
    }
    if let Some(pane_id) = recipient_string_field(object, "pane_id")? {
        selectors.push(
            StableId::parse('%', pane_id)
                .map(Recipient::Pane)
                .ok_or_else(|| MezError::invalid_args("recipient pane_id is invalid"))?,
        );
    }
    if let Some(window_id) = recipient_string_field(object, "window_id")? {
        selectors.push(
            StableId::parse('@', window_id)
                .map(Recipient::Window)
                .ok_or_else(|| MezError::invalid_args("recipient window_id is invalid"))?,
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
        0 => Err(MezError::invalid_args(
            "recipient object has no supported target",
        )),
        1 => Ok(selectors.remove(0)),
        _ => Err(MezError::invalid_args(
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
        .map_err(|_| MezError::invalid_args("MMP envelope must be a JSON object"))?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("MMP envelope must be a JSON object"))?;
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| MezError::invalid_args(format!("MMP {field} must be a string")))?;
    validate_message_label(field, value)?;
    Ok(Some(value.to_string()))
}

/// Runs the optional message string array field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_message_string_array_field(body: &str, field: &str) -> Result<Vec<String>> {
    let value = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|_| MezError::invalid_args("MMP envelope must be a JSON object"))?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("MMP envelope must be a JSON object"))?;
    let Some(value) = object.get(field) else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| MezError::invalid_args(format!("MMP {field} must be an array")))?;
    values
        .iter()
        .map(|value| {
            let value = value
                .as_str()
                .ok_or_else(|| MezError::invalid_args(format!("MMP {field} entry is invalid")))?;
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
        .map_err(|_| MezError::invalid_args("MMP envelope must be a JSON object"))?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("MMP envelope must be a JSON object"))?;
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    let value = value.as_u64().ok_or_else(|| {
        MezError::invalid_args(format!("MMP {field} must be a non-negative integer"))
    })?;
    let value = usize::try_from(value)
        .map_err(|_| MezError::invalid_args(format!("MMP {field} is too large")))?;
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
        .ok_or_else(|| MezError::invalid_args(format!("recipient {field} must be a string")))?;
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
        return Err(MezError::invalid_args(format!("{field} is invalid")));
    }
    Ok(())
}
