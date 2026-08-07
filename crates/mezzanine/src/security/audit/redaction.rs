//! Secret-like value redaction for audit records.
//!
//! Redaction is intentionally conservative and string-marker based because audit
//! callers can provide arbitrary metadata keys and values.

use serde_json::Value;

/// Sanitizes one MCP argument object while preserving non-sensitive structure.
///
/// Sensitive-key values are always replaced, including nested objects and
/// arrays. String values are independently checked for secret-like markers.
/// Malformed or non-object payloads fail closed because MCP arguments are
/// required to be JSON objects at the protocol boundary.
pub(super) fn sanitize_mcp_arguments_json(arguments_json: &str) -> (String, bool) {
    let Ok(mut arguments) = serde_json::from_str::<Value>(arguments_json) else {
        return ("[REDACTED]".to_string(), true);
    };
    if !arguments.is_object() {
        return ("[REDACTED]".to_string(), true);
    }
    let changed = sanitize_json_value(&mut arguments, None);
    (
        serde_json::to_string(&arguments).unwrap_or_else(|_| "[REDACTED]".to_string()),
        changed,
    )
}

/// Recursively sanitizes one JSON value and reports whether it changed.
fn sanitize_json_value(value: &mut Value, owning_key: Option<&str>) -> bool {
    if owning_key.is_some_and(sensitive_json_key) {
        *value = Value::String("[REDACTED]".to_string());
        return true;
    }
    match value {
        Value::Object(object) => {
            let mut changed = false;
            for (key, value) in object {
                changed |= sanitize_json_value(value, Some(key));
            }
            changed
        }
        Value::Array(values) => {
            let mut changed = false;
            for value in values {
                changed |= sanitize_json_value(value, None);
            }
            changed
        }
        Value::String(text) => {
            let (redacted, changed) = redact_secret_like(text);
            if changed {
                *text = redacted;
            }
            changed
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

/// Reports whether one normalized JSON key conventionally carries secrets.
fn sensitive_json_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    [
        "authorization",
        "apikey",
        "accesstoken",
        "refreshtoken",
        "bearertoken",
        "clientsecret",
        "credential",
        "credentials",
        "password",
        "passwd",
        "privatekey",
        "secret",
        "token",
    ]
    .iter()
    .any(|marker| normalized == *marker || normalized.ends_with(marker))
}

/// Runs the redact secret like operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn redact_secret_like(value: &str) -> (String, bool) {
    let markers = [
        "sk-",
        "Bearer ",
        "Authorization:",
        "-----BEGIN",
        "api_key=",
        "access_token=",
        "refresh_token=",
        "secret=",
        "token=",
        "password=",
    ];
    if markers.iter().any(|marker| value.contains(marker)) {
        ("[REDACTED]".to_string(), true)
    } else {
        (value.to_string(), false)
    }
}

/// Runs the redact record field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn redact_record_field(
    value: &mut String,
    redactions: &mut Vec<String>,
    field: &'static str,
) {
    let (redacted, changed) = redact_secret_like(value);
    if changed {
        *value = redacted;
        redactions.push(field.to_string());
    }
}

/// Runs the redact optional record field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn redact_optional_record_field(
    value: &mut Option<String>,
    redactions: &mut Vec<String>,
    field: &'static str,
) {
    if let Some(value) = value.as_mut() {
        redact_record_field(value, redactions, field);
    }
}
