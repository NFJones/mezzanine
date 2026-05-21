//! JSON-RPC notification encoding for visible events.
//!
//! The encoder preserves object payloads when possible and wraps plain payload
//! text in an object so event notifications always carry object params.

use super::types::{EventKind, VisibleEvent};

/// Returns the JSON-RPC notification method for an event kind.
pub fn event_method_name(kind: EventKind) -> &'static str {
    match kind {
        EventKind::ClientAttached => "event/client_attached",
        EventKind::ClientDetached => "event/client_detached",
        EventKind::ObserverRequested => "event/observer_requested",
        EventKind::ObserverDecided => "event/observer_decided",
        EventKind::WindowChanged => "event/window_changed",
        EventKind::PaneChanged => "event/pane_changed",
        EventKind::AgentStatus => "event/agent_status",
        EventKind::Message => "event/message",
        EventKind::ConfigChanged => "event/config_changed",
        EventKind::SnapshotChanged => "event/snapshot_changed",
        EventKind::ApprovalChanged => "event/approval_changed",
        EventKind::McpServerChanged => "event/mcp_server_changed",
        EventKind::HookFailed => "event/hook_failed",
        EventKind::Diagnostic => "event/diagnostic",
    }
}

/// Returns the compact event type name for an event kind.
pub fn event_type_name(kind: EventKind) -> &'static str {
    match kind {
        EventKind::ClientAttached => "client_attached",
        EventKind::ClientDetached => "client_detached",
        EventKind::ObserverRequested => "observer_requested",
        EventKind::ObserverDecided => "observer_decided",
        EventKind::WindowChanged => "window_changed",
        EventKind::PaneChanged => "pane_changed",
        EventKind::AgentStatus => "agent_status",
        EventKind::Message => "message",
        EventKind::ConfigChanged => "config_changed",
        EventKind::SnapshotChanged => "snapshot_changed",
        EventKind::ApprovalChanged => "approval_changed",
        EventKind::McpServerChanged => "mcp_server_changed",
        EventKind::HookFailed => "hook_failed",
        EventKind::Diagnostic => "diagnostic",
    }
}

/// Encodes a visible event as a JSON-RPC event notification string.
pub fn encode_event_notification(event: &VisibleEvent) -> String {
    let object = event_object_json(&event.payload);
    format!(
        r#"{{"jsonrpc":"2.0","method":"{}","params":{{"event_id":{},"time":"{}","event_type":"{}","session_id":{},"object":{}}}}}"#,
        event_method_name(event.kind),
        event.id,
        json_escape(&event.time),
        event_type_name(event.kind),
        optional_string_json(event.session_id.as_deref()),
        object
    )
}

/// Runs the optional string json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn optional_string_json(value: Option<&str>) -> String {
    value
        .map(|value| format!(r#""{}""#, json_escape(value)))
        .unwrap_or_else(|| "null".to_string())
}

/// Runs the event object json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn event_object_json(payload: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(serde_json::Value::Object(_)) => payload.to_string(),
        _ => format!(r#"{{"payload":"{}"}}"#, json_escape(payload)),
    }
}

/// Runs the json escape operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn json_escape(value: &str) -> String {
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
