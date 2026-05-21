//! Minimal JSON formatting helpers for audit records.
//!
//! Audit output is append-only JSON Lines. These helpers keep serialization
//! deterministic for tests and hash-chain inputs without introducing parser state.

use std::collections::BTreeMap;

use super::types::AuditRecord;

/// Runs the record json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn record_json(record: &AuditRecord) -> String {
    format!(
        r#"{{"version":{},"event_id":{},"timestamp":"{}","session_id":"{}","window_id":{},"pane_id":{},"agent_id":{},"actor":{{"kind":"{}","id":"{}"}},"event_type":"{}","action":"{}","policy_mode":"{}","approval_state":"{}","outcome":"{}","redactions":{},"metadata":{}}}"#,
        record.version,
        record.event_id,
        json_escape(&record.timestamp),
        json_escape(&record.session_id),
        json_optional(record.window_id.as_deref()),
        json_optional(record.pane_id.as_deref()),
        json_optional(record.agent_id.as_deref()),
        json_escape(&record.actor.kind),
        json_escape(&record.actor.id),
        json_escape(&record.event_type),
        json_escape(&record.action),
        json_escape(&record.policy_mode),
        json_escape(&record.approval_state),
        json_escape(&record.outcome),
        string_array_json(&record.redactions),
        string_map_json(&record.metadata)
    )
}

/// Runs the insert hash field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn insert_hash_field(mut line: String, hash: &str) -> String {
    let suffix = format!(r#","hash":"{}"}}"#, json_escape(hash));
    line.pop();
    line.push_str(&suffix);
    line
}

/// Runs the string array json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn string_array_json(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!(r#""{}""#, json_escape(value)))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Runs the string map json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn string_map_json(values: &BTreeMap<String, String>) -> String {
    format!(
        "{{{}}}",
        values
            .iter()
            .map(|(key, value)| format!(r#""{}":"{}""#, json_escape(key), json_escape(value)))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Runs the json optional operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn json_optional(value: Option<&str>) -> String {
    value
        .map(|value| format!(r#""{}""#, json_escape(value)))
        .unwrap_or_else(|| "null".to_string())
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
