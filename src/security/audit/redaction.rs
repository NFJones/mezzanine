//! Secret-like value redaction for audit records.
//!
//! Redaction is intentionally conservative and string-marker based because audit
//! callers can provide arbitrary metadata keys and values.

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
