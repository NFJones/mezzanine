//! Message sender, protocol, and message-type validation.
//!
//! Validation centralizes spoofing-sensitive sender checks, supported baseline
//! message types, extension namespace rules, and task-state names.

use super::error::{MessageError, Result};
use super::types::{MMP_PROTOCOL, MMP_UNSUPPORTED_PROTOCOL_MESSAGE, SenderIdentity, TaskState};

/// Runs the validate sender identity operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_sender_identity(identity: &SenderIdentity) -> Result<()> {
    if identity.agent_id.as_str().is_empty() {
        return Err(MessageError::invalid_args(
            "message sender agent id is invalid",
        ));
    }
    if identity
        .role
        .as_deref()
        .is_some_and(|role| role.is_empty() || role.chars().any(char::is_control))
    {
        return Err(MessageError::invalid_args("message sender role is invalid"));
    }
    if identity
        .capabilities
        .iter()
        .any(|capability| capability.is_empty() || capability.chars().any(char::is_control))
    {
        return Err(MessageError::invalid_args(
            "message sender capability is invalid",
        ));
    }
    Ok(())
}

/// Runs the validate message type operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn validate_message_type(message_type: &str) -> Result<()> {
    let baseline = [
        "hello",
        "welcome",
        "discover",
        "discover_result",
        "send",
        "transport/receive",
        "mmp.receive",
        "deliver",
        "ack",
        "error",
        "presence",
        "heartbeat",
        "task_status",
        "task_result",
    ];
    if baseline.contains(&message_type) || has_extension_namespace(message_type) {
        Ok(())
    } else {
        Err(MessageError::invalid_args("unsupported message type"))
    }
}

/// Runs the validate protocol operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_protocol(protocol: &str) -> Result<()> {
    if protocol == MMP_PROTOCOL {
        Ok(())
    } else {
        Err(MessageError::invalid_args(MMP_UNSUPPORTED_PROTOCOL_MESSAGE))
    }
}

/// Runs the has extension namespace operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn has_extension_namespace(message_type: &str) -> bool {
    is_reverse_dns_message_type(message_type) || is_uri_like_message_type(message_type)
}

/// Runs the is reverse dns message type operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn is_reverse_dns_message_type(message_type: &str) -> bool {
    let (namespace, name) = if let Some((namespace, name)) = message_type.split_once('/') {
        (namespace, name)
    } else if let Some((namespace, name)) = message_type.rsplit_once('.') {
        (namespace, name)
    } else {
        return false;
    };
    if namespace.is_empty() || name.is_empty() {
        return false;
    }
    let mut labels = namespace.split('.');
    let has_multiple_labels = labels.clone().nth(1).is_some();
    has_multiple_labels && labels.all(is_dns_label) && is_message_name(name)
}

/// Runs the is dns label operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn is_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}

/// Runs the is message name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn is_message_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/'))
}

/// Runs the is uri like message type operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn is_uri_like_message_type(message_type: &str) -> bool {
    let Some((scheme, rest)) = message_type.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && !rest.is_empty()
        && scheme
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

/// Runs the validate task payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_task_payload(
    message_type: &str,
    content_type: &str,
    payload: &str,
) -> Result<()> {
    if !matches!(message_type, "task_status" | "task_result") {
        return Ok(());
    }
    if content_type != "application/json" {
        return Err(MessageError::invalid_args(
            "MMP task messages require content_type application/json",
        ));
    }
    let value = serde_json::from_str::<serde_json::Value>(payload)
        .map_err(|_| MessageError::invalid_args("MMP task payload must be a valid JSON object"))?;
    let object = value.as_object().ok_or_else(|| {
        MessageError::invalid_args("MMP task payload must be a valid JSON object")
    })?;

    match message_type {
        "task_status" => validate_task_status_payload(object),
        "task_result" => validate_task_result_payload(object),
        _ => Ok(()),
    }
}

/// Runs the validate mmp payload metadata operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn validate_mmp_payload_metadata(
    message_type: &str,
    content_type: &str,
    payload: &str,
    payload_encoding: Option<&str>,
) -> Result<()> {
    if content_type.starts_with("text/") && content_type != "text/plain; charset=utf-8" {
        return Err(MessageError::invalid_args(
            "MMP text payloads require content_type text/plain; charset=utf-8",
        ));
    }
    if content_type == "application/json"
        && serde_json::from_str::<serde_json::Value>(payload).is_err()
    {
        return Err(MessageError::invalid_args(
            "MMP JSON payload must be valid JSON",
        ));
    }
    if content_type == "application/octet-stream" && payload_encoding != Some("base64") {
        return Err(MessageError::invalid_args(
            "MMP binary payloads require payload_encoding base64",
        ));
    }
    if payload_encoding == Some("base64") && !is_valid_base64_payload(payload) {
        return Err(MessageError::invalid_args(
            "MMP base64 payload must be valid base64",
        ));
    }
    if payload_encoding.is_some_and(|encoding| encoding != "base64") {
        return Err(MessageError::invalid_args(
            "MMP payload_encoding must be base64 when present",
        ));
    }
    validate_task_payload(message_type, content_type, payload)?;
    Ok(())
}

/// Checks standard padded base64 syntax without allocating decoded bytes.
pub(super) fn is_valid_base64_payload(payload: &str) -> bool {
    let bytes = payload.as_bytes();
    if bytes.is_empty() {
        return true;
    }
    if !bytes.len().is_multiple_of(4) {
        return false;
    }
    let mut padding_started = false;
    for (index, byte) in bytes.iter().enumerate() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' if !padding_started => {}
            b'=' => {
                padding_started = true;
                let remaining = bytes.len().saturating_sub(index);
                if remaining > 2 {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

/// Runs the validate task status payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_task_status_payload(object: &serde_json::Map<String, serde_json::Value>) -> Result<()> {
    let task_id = required_task_string(object, "task_id")?;
    if task_id.trim().is_empty() {
        return Err(MessageError::invalid_args(
            "MMP task payload task_id is invalid",
        ));
    }
    let state = required_task_string(object, "state")?;
    parse_task_state_name(state)?;
    let _summary = required_task_string(object, "summary")?;
    if let Some(progress) = object.get("progress_percent")
        && !progress.is_null()
    {
        let progress = progress.as_u64().ok_or_else(|| {
            MessageError::invalid_args("MMP task_status progress_percent must be 0 through 100")
        })?;
        if progress > 100 {
            return Err(MessageError::invalid_args(
                "MMP task_status progress_percent must be 0 through 100",
            ));
        }
    }
    Ok(())
}

/// Runs the validate task result payload operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_task_result_payload(object: &serde_json::Map<String, serde_json::Value>) -> Result<()> {
    let task_id = required_task_string(object, "task_id")?;
    if task_id.trim().is_empty() {
        return Err(MessageError::invalid_args(
            "MMP task payload task_id is invalid",
        ));
    }
    object
        .get("success")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| MessageError::invalid_args("MMP task_result requires boolean success"))?;
    let _summary = required_task_string(object, "summary")?;
    let _output = required_task_string(object, "output")?;
    Ok(())
}

/// Runs the required task string operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_task_string<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<&'a str> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MessageError::invalid_args(format!("MMP task payload requires {field}")))
}

/// Runs the parse task state name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_task_state_name(state: &str) -> Result<TaskState> {
    match state {
        "queued" => Ok(TaskState::Queued),
        "running" => Ok(TaskState::Running),
        "blocked" => Ok(TaskState::Blocked),
        "succeeded" => Ok(TaskState::Succeeded),
        "failed" => Ok(TaskState::Failed),
        "cancelled" => Ok(TaskState::Cancelled),
        _ => Err(MessageError::invalid_args("unsupported task state")),
    }
}

/// Returns the canonical wire and presentation name for a task state.
pub fn task_state_name(state: TaskState) -> &'static str {
    match state {
        TaskState::Queued => "queued",
        TaskState::Running => "running",
        TaskState::Blocked => "blocked",
        TaskState::Succeeded => "succeeded",
        TaskState::Failed => "failed",
        TaskState::Cancelled => "cancelled",
    }
}
