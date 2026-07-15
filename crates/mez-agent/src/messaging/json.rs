//! JSON formatting and lightweight JSON field extraction for MMP.
//!
//! Message dispatch currently uses deterministic small-format JSON helpers for
//! protocol responses and targeted field extraction from inbound envelopes.

use super::error::{MessageError, MessageErrorKind};
use super::types::{
    DeliveryBatch, DeliveryCursor, Envelope, MMP_DUPLICATE_MESSAGE_ID_MESSAGE, MMP_EXPIRED_MESSAGE,
    MMP_PAYLOAD_TOO_LARGE_MESSAGE, MMP_UNDELIVERABLE_MESSAGE, MMP_UNSUPPORTED_PROTOCOL_MESSAGE,
    MessageSequence, Recipient, SenderIdentity, SequencedEnvelope, TaskResultPayload,
    TaskStatusPayload,
};
use super::validation::task_state_name;

impl TaskStatusPayload {
    /// Runs the to json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"task_id":"{}","state":"{}","progress_percent":{},"summary":"{}"}}"#,
            json_escape(&self.task_id),
            task_state_name(self.state),
            self.progress_percent
                .map(|progress| progress.to_string())
                .unwrap_or_else(|| "null".to_string()),
            json_escape(&self.summary)
        )
    }
}

impl TaskResultPayload {
    /// Runs the to json operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"task_id":"{}","success":{},"summary":"{}","output":"{}"}}"#,
            json_escape(&self.task_id),
            self.success,
            json_escape(&self.summary),
            json_escape(&self.output)
        )
    }
}

/// Runs the sender identity json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn sender_identity_json(identity: &SenderIdentity) -> String {
    format!(
        r#"{{"agent_id":"{}","pane_id":{},"window_id":{},"role":{},"capabilities":[{}]}}"#,
        json_escape(identity.agent_id.as_str()),
        json_optional(identity.pane_id.as_ref().map(|id| id.as_str())),
        json_optional(identity.window_id.as_ref().map(|id| id.as_str())),
        json_optional(identity.role.as_deref()),
        identity
            .capabilities
            .iter()
            .map(|capability| format!(r#""{}""#, json_escape(capability)))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Runs the delivery batch json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn delivery_batch_json(batch: &DeliveryBatch) -> String {
    format!(
        r#"{{"protocol":"mmp/1","type":"deliver","cursor":{},"messages":[{}]}}"#,
        delivery_cursor_json(&batch.cursor),
        batch
            .messages
            .iter()
            .map(sequenced_envelope_json)
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Runs the delivery cursor json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn delivery_cursor_json(cursor: &DeliveryCursor) -> String {
    format!(
        r#"{{"recipient":"{}","last_sequence":{}}}"#,
        json_escape(cursor.recipient.as_str()),
        cursor.last_sequence
    )
}

/// Runs the sequenced envelope json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn sequenced_envelope_json(delivery: &SequencedEnvelope) -> String {
    format!(
        r#"{{"sequence":{},"envelope":{}}}"#,
        delivery.sequence,
        envelope_json(&delivery.envelope, delivery.sequence)
    )
}

/// Runs the envelope json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn envelope_json(envelope: &Envelope, sequence: MessageSequence) -> String {
    format!(
        r#"{{"protocol":"{}","id":"{}","type":"{}","sequence":{},"time":"{}","sender":{},"recipient":{},"correlation_id":{},"ttl_ms":{},"content_type":"{}","payload":"{}"{}}}"#,
        json_escape(envelope.protocol),
        json_escape(&envelope.id),
        json_escape(&envelope.message_type),
        sequence,
        json_escape(&envelope.time),
        sender_identity_json(&envelope.sender),
        recipient_json(&envelope.recipient),
        json_optional(envelope.correlation_id.as_deref()),
        envelope
            .ttl_ms
            .map(|ttl| ttl.to_string())
            .unwrap_or_else(|| "null".to_string()),
        json_escape(&envelope.content_type),
        json_escape(&envelope.payload),
        extension_fields_json(&envelope.extension_fields)
    )
}

/// Runs the extension fields json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn extension_fields_json(fields: &[(String, String)]) -> String {
    fields
        .iter()
        .map(|(field, value)| format!(r#","{}":{}"#, json_escape(field), value))
        .collect::<Vec<_>>()
        .join("")
}

/// Runs the recipient json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn recipient_json(recipient: &Recipient) -> String {
    match recipient {
        Recipient::Agent(agent_id) => {
            format!(r#"{{"agent_id":"{}"}}"#, json_escape(agent_id.as_str()))
        }
        Recipient::Pane(pane_id) => {
            format!(r#"{{"pane_id":"{}"}}"#, json_escape(pane_id.as_str()))
        }
        Recipient::Window(window_id) => {
            format!(r#"{{"window_id":"{}"}}"#, json_escape(window_id.as_str()))
        }
        Recipient::Session => r#"{"session":true}"#.to_string(),
        Recipient::Role(role) => format!(r#"{{"role":"{}"}}"#, json_escape(role)),
        Recipient::Capability(capability) => {
            format!(r#"{{"capability":"{}"}}"#, json_escape(capability))
        }
        Recipient::Group(group) => format!(r#"{{"group":"{}"}}"#, json_escape(group)),
    }
}

/// Runs the mmp error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mmp_error(
    code: &str,
    message: &str,
    retryable: bool,
    delivery_status: &str,
) -> String {
    format!(
        r#"{{"protocol":"mmp/1","type":"error","error":{{"code":"{}","message":"{}","retryable":{},"delivery_status":"{}"}}}}"#,
        json_escape(code),
        json_escape(message),
        retryable,
        json_escape(delivery_status)
    )
}

/// Runs the mmp error code operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn mmp_error_code(error: &MessageError) -> &'static str {
    if error.kind() == MessageErrorKind::InvalidArgs
        && error.message() == MMP_UNSUPPORTED_PROTOCOL_MESSAGE
    {
        "unsupported_protocol"
    } else if error.kind() == MessageErrorKind::InvalidArgs
        && error.message() == MMP_PAYLOAD_TOO_LARGE_MESSAGE
    {
        "payload_too_large"
    } else if error.kind() == MessageErrorKind::NotFound
        && error.message() == MMP_UNDELIVERABLE_MESSAGE
    {
        "undeliverable"
    } else if error.kind() == MessageErrorKind::InvalidState
        && error.message() == MMP_EXPIRED_MESSAGE
    {
        "expired"
    } else if error.kind() == MessageErrorKind::Conflict
        && error.message() == MMP_DUPLICATE_MESSAGE_ID_MESSAGE
    {
        "invalid_envelope"
    } else {
        error_code(error.kind())
    }
}

/// Runs the mmp delivery status operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn mmp_delivery_status(error: &MessageError) -> &'static str {
    if error.kind() == MessageErrorKind::NotFound && error.message() == MMP_UNDELIVERABLE_MESSAGE {
        "undeliverable"
    } else if error.kind() == MessageErrorKind::InvalidState
        && error.message() == MMP_EXPIRED_MESSAGE
    {
        "expired"
    } else {
        "rejected"
    }
}

/// Runs the error code operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn error_code(kind: MessageErrorKind) -> &'static str {
    match kind {
        MessageErrorKind::InvalidArgs => "invalid_envelope",
        MessageErrorKind::Forbidden => "unauthorized",
        MessageErrorKind::NotFound => "not_found",
        _ => "internal_error",
    }
}

/// Runs the json escape operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push(' '),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Runs the top level json string field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn top_level_json_string_field(body: &str, field: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .as_object()?
        .get(field)?
        .as_str()
        .map(str::to_string)
}

/// Runs the json string field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_string_field(body: &str, field: &str) -> Option<String> {
    let pattern = format!(r#""{field}""#);
    let start = body.find(&pattern)?;
    let after_name = body[start + pattern.len()..].trim_start();
    let after_colon = after_name.strip_prefix(':')?.trim_start();
    parse_json_string(after_colon)
}

/// Runs the json number field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_number_field(body: &str, field: &str) -> Option<String> {
    let pattern = format!(r#""{field}""#);
    let start = body.find(&pattern)?;
    let after_name = body[start + pattern.len()..].trim_start();
    let mut chars = after_name
        .strip_prefix(':')?
        .trim_start()
        .chars()
        .peekable();
    let mut value = String::new();
    while let Some(ch) = chars.peek().copied() {
        if ch.is_ascii_digit() {
            value.push(ch);
            let _ = chars.next();
        } else {
            break;
        }
    }
    (!value.is_empty()).then_some(value)
}

/// Runs the json object field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_object_field(body: &str, field: &str) -> Option<String> {
    let pattern = format!(r#""{field}""#);
    let start = body.find(&pattern)?;
    let after_name = body[start + pattern.len()..].trim_start();
    let value = after_name.strip_prefix(':')?.trim_start();
    take_json_balanced(value, '{', '}')
}

/// Runs the parse json string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_json_string(input: &str) -> Option<String> {
    let mut chars = input.chars();
    if chars.next()? != '"' {
        return None;
    }
    let mut value = String::new();
    let mut escaped = false;
    for ch in chars {
        if escaped {
            value.push(match ch {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                other => other,
            });
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(value),
            _ => value.push(ch),
        }
    }
    None
}

/// Runs the take json balanced operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn take_json_balanced(input: &str, open: char, close: char) -> Option<String> {
    let mut output = String::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for ch in input.chars() {
        output.push(ch);
        if escaped {
            escaped = false;
            continue;
        }
        if in_string {
            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            ch if ch == open => depth += 1,
            ch if ch == close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(output);
                }
            }
            _ => {}
        }
    }
    None
}

/// Runs the json optional operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_optional(value: Option<&str>) -> String {
    value
        .map(|value| format!(r#""{}""#, json_escape(value)))
        .unwrap_or_else(|| "null".to_string())
}
