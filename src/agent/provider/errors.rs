//! Provider error classification and diagnostic shaping.
//!
//! This module owns provider-facing error text extraction, sanitized failure
//! payload construction, retry/context/output-limit classification, and
//! malformed model-output diagnostics.

use crate::error::MezError;
pub(crate) use mez_agent::ProviderErrorRetryClass;
use mez_agent::{ProviderErrorKind, classify_provider_error_retry};

/// Runs the openai provider error detail operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn openai_provider_error_detail(body: &str) -> String {
    if body.trim().is_empty() {
        return "empty provider response".to_string();
    }
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .or_else(|| value.get("error_description"))
                .or_else(|| value.get("message"))
                .or_else(|| value.get("error"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(240).collect());
    redact_or_truncate_provider_failure_text(&detail)
}

/// Runs the openai provider failure json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn openai_provider_failure_json(status_code: Option<u16>, body: &str) -> String {
    let trimmed = body.trim();
    let mut object = serde_json::Map::new();
    if let Some(status_code) = status_code {
        object.insert(
            "status_code".to_string(),
            serde_json::Value::Number(serde_json::Number::from(u64::from(status_code))),
        );
    }
    if trimmed.is_empty() {
        object.insert(
            "body_text".to_string(),
            serde_json::Value::String(String::new()),
        );
    } else if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        insert_provider_failure_value(&mut object, value);
    } else {
        object.insert(
            "body_text".to_string(),
            serde_json::Value::String(truncate_provider_failure_text(trimmed)),
        );
    }
    serde_json::Value::Object(object).to_string()
}

/// Runs the openai provider failure event json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn openai_provider_failure_event_json(value: &serde_json::Value) -> String {
    let mut object = serde_json::Map::new();
    insert_provider_failure_value(&mut object, value.clone());
    serde_json::Value::Object(object).to_string()
}

/// Classifies one provider failure for runtime recovery and retry handling.
///
/// The classifier preserves existing precedence: context-limit and
/// output-limit recovery win over generic transport retry, unsupported 400s do
/// not retry, and provider-authored retry invitations remain visible.
pub(crate) fn provider_error_retry_class(error: &MezError) -> ProviderErrorRetryClass {
    provider_error_retry_class_from_parts(
        error.kind(),
        error.message(),
        error.provider_failure_json(),
    )
}

/// Classifies provider failure fields after an error crosses an async boundary.
///
/// Provider worker events carry the stable error kind, message, and sanitized
/// provider failure payload separately. This helper keeps their retry policy in
/// sync with in-process `MezError` classification.
pub(crate) fn provider_error_retry_class_from_parts(
    kind: crate::error::MezErrorKind,
    message: &str,
    provider_failure_json: Option<&str>,
) -> ProviderErrorRetryClass {
    classify_provider_error_retry(provider_error_kind(kind), message, provider_failure_json)
}

fn provider_error_kind(kind: crate::error::MezErrorKind) -> ProviderErrorKind {
    match kind {
        crate::error::MezErrorKind::InvalidArgs => ProviderErrorKind::InvalidArgs,
        crate::error::MezErrorKind::InvalidState => ProviderErrorKind::InvalidState,
        crate::error::MezErrorKind::Config => ProviderErrorKind::Config,
        crate::error::MezErrorKind::Io => ProviderErrorKind::Io,
        crate::error::MezErrorKind::Conflict => ProviderErrorKind::Conflict,
        crate::error::MezErrorKind::NotFound => ProviderErrorKind::NotFound,
        crate::error::MezErrorKind::Forbidden => ProviderErrorKind::Forbidden,
        crate::error::MezErrorKind::NotImplemented => ProviderErrorKind::NotImplemented,
    }
}

/// Converts a serialized provider-event error kind into a Mezzanine error kind.
///
/// Async provider workers carry error kinds as strings across actor channels.
/// Keeping this parser beside provider retry classification and error-envelope
/// construction prevents runtime and async-runtime copies from drifting.
pub(crate) fn provider_event_error_kind(kind: &str) -> crate::error::MezErrorKind {
    match kind {
        "invalid_args" | "InvalidArgs" => crate::error::MezErrorKind::InvalidArgs,
        "config" | "Config" => crate::error::MezErrorKind::Config,
        "io" | "Io" => crate::error::MezErrorKind::Io,
        "conflict" | "Conflict" => crate::error::MezErrorKind::Conflict,
        "not_found" | "NotFound" => crate::error::MezErrorKind::NotFound,
        "forbidden" | "Forbidden" => crate::error::MezErrorKind::Forbidden,
        "not_implemented" | "NotImplemented" => crate::error::MezErrorKind::NotImplemented,
        _ => crate::error::MezErrorKind::InvalidState,
    }
}

/// Builds a provider/runtime error envelope from serialized provider-event fields.
///
/// The resulting `MezError` preserves the structured provider failure payload
/// and raw provider text while sharing the event-kind parser used by retry
/// classification.
pub(crate) fn provider_event_error_from_parts(
    kind: &str,
    message: &str,
    provider_failure_json: Option<&str>,
    provider_raw_text: Option<&str>,
) -> MezError {
    let mut error = MezError::new(provider_event_error_kind(kind), message);
    if let Some(raw_text) = provider_raw_text {
        error = error.with_provider_raw_text(raw_text.to_string());
    }
    if let Some(failure_json) = provider_failure_json {
        error = error.with_provider_failure_json(failure_json.to_string());
    }
    error
}

/// Runs the insert provider failure value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn insert_provider_failure_value(
    object: &mut serde_json::Map<String, serde_json::Value>,
    value: serde_json::Value,
) {
    let value = sanitize_provider_failure_value(value);
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        object.insert("error".to_string(), error.clone());
    } else if let Some(response_error) = value
        .get("response")
        .and_then(|response| response.get("error"))
        .filter(|error| !error.is_null())
    {
        object.insert("error".to_string(), response_error.clone());
        if let Some(response_id) = value
            .get("response")
            .and_then(|response| response.get("id"))
            .and_then(serde_json::Value::as_str)
        {
            object.insert(
                "response_id".to_string(),
                serde_json::Value::String(response_id.to_string()),
            );
        }
    } else if let Some(incomplete_details) = value
        .get("response")
        .and_then(|response| response.get("incomplete_details"))
        .filter(|details| !details.is_null())
    {
        object.insert("incomplete_details".to_string(), incomplete_details.clone());
    } else {
        object.insert("body".to_string(), value);
    }
}

/// Runs the sanitize provider failure value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn sanitize_provider_failure_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let value = if provider_failure_key_is_secret_like(&key) {
                        serde_json::Value::String("[REDACTED]".to_string())
                    } else {
                        sanitize_provider_failure_value(value)
                    };
                    (key, value)
                })
                .collect(),
        ),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .take(32)
                .map(sanitize_provider_failure_value)
                .collect(),
        ),
        serde_json::Value::String(value) => {
            serde_json::Value::String(redact_or_truncate_provider_failure_text(&value))
        }
        other => other,
    }
}

/// Runs the redact or truncate provider failure text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn redact_or_truncate_provider_failure_text(value: &str) -> String {
    if provider_failure_text_contains_secret_like(value) {
        "[REDACTED]".to_string()
    } else {
        truncate_provider_failure_text(value)
    }
}

/// Runs the provider failure key is secret like operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_failure_key_is_secret_like(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("authorization")
        || key.contains("api_key")
        || key.contains("access_token")
        || key.contains("refresh_token")
        || key.contains("secret")
        || key.contains("password")
}

/// Runs the provider failure text contains secret like operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_failure_text_contains_secret_like(value: &str) -> bool {
    value.contains("-----BEGIN")
        || value
            .split_whitespace()
            .any(provider_failure_token_is_secret_like)
}

/// Runs the provider failure token is secret like operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_failure_token_is_secret_like(token: &str) -> bool {
    let token = token.trim_matches(|character: char| {
        matches!(
            character,
            ',' | ';' | ':' | '.' | '!' | '?' | ')' | '(' | '[' | ']' | '{' | '}' | '"' | '\''
        )
    });
    let lower = token.to_ascii_lowercase();
    lower == "bearer"
        || lower.starts_with("bearer=")
        || lower.starts_with("sk-")
        || lower.starts_with("sk_")
        || lower.starts_with("sk-proj-")
        || lower.starts_with("sk-ant-")
        || lower.starts_with("xoxb-")
        || lower.starts_with("ghp_")
        || provider_failure_token_is_jwt_like(token)
}

/// Runs the provider failure token is jwt like operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_failure_token_is_jwt_like(token: &str) -> bool {
    let mut segments = token.split('.');
    let Some(header) = segments.next() else {
        return false;
    };
    let Some(payload) = segments.next() else {
        return false;
    };
    let Some(signature) = segments.next() else {
        return false;
    };
    segments.next().is_none()
        && [header, payload, signature]
            .into_iter()
            .all(|segment| segment.len() >= 8 && segment.chars().all(is_base64_url_character))
}

/// Runs the is base64 url character operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn is_base64_url_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '-' || character == '_'
}

/// Runs the truncate provider failure text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn truncate_provider_failure_text(value: &str) -> String {
    /// Defines the MAX PROVIDER FAILURE TEXT CHARS const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    const MAX_PROVIDER_FAILURE_TEXT_CHARS: usize = 4096;
    let mut output = value
        .chars()
        .take(MAX_PROVIDER_FAILURE_TEXT_CHARS)
        .collect::<String>();
    if value.chars().count() > MAX_PROVIDER_FAILURE_TEXT_CHARS {
        output.push_str("...");
    }
    output
}

/// Runs the provider maap parse error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn provider_maap_parse_error(error: MezError, raw_text: &str) -> MezError {
    MezError::new(
        error.kind(),
        provider_maap_parse_error_message(&error, raw_text),
    )
    .with_provider_raw_text(raw_text.to_string())
    .with_provider_failure_json(provider_malformed_output_failure_json(&error, raw_text))
}

/// Runs the provider maap parse error message operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn provider_maap_parse_error_message(error: &MezError, raw_text: &str) -> String {
    let mut message = format!("provider MAAP output is malformed: {}", error.message());
    if let Some(hint) = provider_malformed_output_hint(raw_text) {
        message.push_str("; ");
        message.push_str(hint);
    }
    message
}

/// Runs the provider malformed output hint operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_malformed_output_hint(raw_text: &str) -> Option<&'static str> {
    let value = serde_json::from_str::<serde_json::Value>(raw_text).ok()?;
    let object = value.as_object()?;
    if provider_output_contains_bare_command_actions(object) {
        return Some(
            "provider returned bare command objects inside actions; expected each action to include type and required action-specific fields such as shell_command summary inside a MAAP action batch",
        );
    }
    if object.contains_key("command") {
        return Some(
            "provider returned a bare command object; expected a MAAP action batch with an actions array",
        );
    }
    if object.contains_key("type") && !object.contains_key("actions") {
        return Some(
            "provider returned a bare action object; expected a MAAP action batch envelope",
        );
    }
    None
}

/// Runs the provider output contains bare command actions operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_output_contains_bare_command_actions(
    object: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    object
        .get("actions")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|actions| {
            actions.iter().any(|action| {
                action.as_object().is_some_and(|action_object| {
                    action_object.contains_key("command") && !action_object.contains_key("type")
                })
            })
        })
}

/// Runs the provider malformed output failure json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_malformed_output_failure_json(error: &MezError, raw_text: &str) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(raw_text).ok();
    let mut output = serde_json::json!({
        "format": if parsed.is_some() { "json" } else { "text" },
        "bytes": raw_text.len()
    });
    if let Some(serde_json::Value::Object(object)) = parsed {
        let top_level_keys = object.keys().take(32).cloned().collect::<Vec<_>>();
        output["top_level_keys"] = serde_json::json!(top_level_keys);
        output["bare_command_object"] = serde_json::json!(object.contains_key("command"));
        output["bare_action_object"] =
            serde_json::json!(object.contains_key("type") && !object.contains_key("actions"));
        output["bare_command_actions"] =
            serde_json::json!(provider_output_contains_bare_command_actions(&object));
    }
    serde_json::json!({
        "type": "malformed_model_output",
        "error": {
            "kind": provider_error_kind_name(error.kind()),
            "message": error.message()
        },
        "output": output
    })
    .to_string()
}

/// Runs the provider error kind name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_error_kind_name(kind: crate::error::MezErrorKind) -> &'static str {
    match kind {
        crate::error::MezErrorKind::InvalidArgs => "invalid_args",
        crate::error::MezErrorKind::InvalidState => "invalid_state",
        crate::error::MezErrorKind::Config => "config",
        crate::error::MezErrorKind::Io => "io",
        crate::error::MezErrorKind::Conflict => "conflict",
        crate::error::MezErrorKind::NotFound => "not_found",
        crate::error::MezErrorKind::Forbidden => "forbidden",
        crate::error::MezErrorKind::NotImplemented => "not_implemented",
    }
}

#[cfg(test)]
mod tests {
    use super::{ProviderErrorRetryClass, provider_error_retry_class_from_parts};

    /// Verifies provider HTTP body-read inactivity is treated as a retryable
    /// transport failure rather than a terminal provider failure.
    ///
    /// The HTTP transport reports stalled chunk reads as `InvalidState` with a
    /// stable diagnostic prefix. That condition is transient in the same way as
    /// request and response-read transport failures, so the retry classifier
    /// must preserve it for runtime retry scheduling.
    #[test]
    fn provider_retry_classifier_treats_response_read_stalls_as_retryable_transport() {
        let retry_class = provider_error_retry_class_from_parts(
            crate::error::MezErrorKind::InvalidState,
            "provider HTTP response read stalled for 50ms while waiting for body chunk",
            None,
        );

        assert_eq!(retry_class, ProviderErrorRetryClass::RetryableTransport);
    }

    /// Verifies status-bearing provider errors still honor explicit retry
    /// invitations before falling back to terminal non-retryable handling.
    ///
    /// Some providers use non-429/5xx status codes for transient failures while
    /// including a clear retry instruction in the response body. The classifier
    /// must preserve those retries without weakening the unsupported-400 guard.
    #[test]
    fn provider_retry_classifier_honors_retry_invitations_with_status_codes() {
        let retry_class = provider_error_retry_class_from_parts(
            crate::error::MezErrorKind::InvalidState,
            "Chat Completions API returned status 409: you can retry your request",
            Some(
                r#"{"status_code":409,"error":{"message":"An error occurred while processing your request; you can retry your request"}}"#,
            ),
        );

        assert_eq!(retry_class, ProviderErrorRetryClass::RetryableTransport);
    }

    /// Verifies unsupported OpenAI-style status 400 errors remain non-retryable
    /// even when the status-code branch also honors retry invitation language.
    #[test]
    fn provider_retry_classifier_keeps_unsupported_400_non_retryable() {
        let retry_class = provider_error_retry_class_from_parts(
            crate::error::MezErrorKind::InvalidState,
            "Chat Completions API returned status 400: Unsupported parameter",
            Some(r#"{"status_code":400,"error":{"message":"Unsupported parameter: temperature"}}"#),
        );

        assert_eq!(retry_class, ProviderErrorRetryClass::NonRetryable);
    }

    /// Verifies Anthropic status-bearing errors map into the same retry and
    /// recovery classes used by the runtime after structured failure JSON is
    /// attached to provider errors.
    #[test]
    fn provider_retry_classifier_maps_anthropic_status_and_error_types() {
        let cases = [
            (
                401,
                "authentication_error",
                "invalid api key",
                ProviderErrorRetryClass::NonRetryable,
            ),
            (
                402,
                "billing_error",
                "billing failure",
                ProviderErrorRetryClass::NonRetryable,
            ),
            (
                403,
                "permission_error",
                "permission denied",
                ProviderErrorRetryClass::NonRetryable,
            ),
            (
                404,
                "not_found_error",
                "model not found",
                ProviderErrorRetryClass::NonRetryable,
            ),
            (
                408,
                "timeout_error",
                "request timed out",
                ProviderErrorRetryClass::RetryableTransport,
            ),
            (
                413,
                "request_too_large",
                "request_too_large",
                ProviderErrorRetryClass::ContextLimit,
            ),
            (
                429,
                "rate_limit_error",
                "rate limit exceeded",
                ProviderErrorRetryClass::RetryableTransport,
            ),
            (
                500,
                "api_error",
                "internal api error",
                ProviderErrorRetryClass::RetryableTransport,
            ),
            (
                529,
                "overloaded_error",
                "server overloaded",
                ProviderErrorRetryClass::RetryableTransport,
            ),
            (
                400,
                "invalid_request_error",
                "request schema is invalid",
                ProviderErrorRetryClass::NonRetryable,
            ),
        ];

        for (status_code, error_type, message, expected) in cases {
            let failure_json = format!(
                r#"{{"status_code":{status_code},"error":{{"type":"{error_type}","message":"{message}"}}}}"#
            );
            let retry_class = provider_error_retry_class_from_parts(
                crate::error::MezErrorKind::InvalidState,
                message,
                Some(&failure_json),
            );

            assert_eq!(retry_class, expected, "{status_code} {error_type}");
        }
    }

    /// Verifies structured provider error types can drive retry behavior even
    /// when the HTTP transport already succeeded and no status code is present.
    #[test]
    fn provider_retry_classifier_uses_structured_error_types_without_status_codes() {
        let retry_class = provider_error_retry_class_from_parts(
            crate::error::MezErrorKind::InvalidState,
            "Anthropic stream error",
            Some(
                r#"{"error":{"type":"rate_limit_error","message":"too many requests"},"request_id":"req_123"}"#,
            ),
        );

        assert_eq!(retry_class, ProviderErrorRetryClass::RetryableTransport);
    }
}
