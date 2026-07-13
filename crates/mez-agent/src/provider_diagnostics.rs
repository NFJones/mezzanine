//! Secret-safe provider failure diagnostic shaping.
//!
//! This module normalizes provider error bodies into bounded diagnostics that
//! are safe to persist or pass across worker boundaries. It intentionally owns
//! no transport, credential storage, or product error types.

use crate::ProviderErrorKind;
use serde_json::{Map, Value};

const MAX_PROVIDER_FAILURE_TEXT_CHARS: usize = 4096;
const MAX_PROVIDER_FAILURE_ARRAY_ITEMS: usize = 32;

/// Extracts a bounded, secret-safe human-readable detail from a provider body.
pub fn provider_error_detail(body: &str) -> String {
    if body.trim().is_empty() {
        return "empty provider response".to_string();
    }
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .or_else(|| value.get("error_description"))
                .or_else(|| value.get("message"))
                .or_else(|| value.get("error"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(240).collect());
    redact_or_truncate_provider_failure_text(&detail)
}

/// Builds bounded, secret-safe structured diagnostics from an HTTP failure.
pub fn provider_failure_json(status_code: Option<u16>, body: &str) -> String {
    let trimmed = body.trim();
    let mut object = Map::new();
    if let Some(status_code) = status_code {
        object.insert(
            "status_code".to_string(),
            Value::Number(serde_json::Number::from(u64::from(status_code))),
        );
    }
    if trimmed.is_empty() {
        object.insert("body_text".to_string(), Value::String(String::new()));
    } else if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        insert_provider_failure_value(&mut object, value);
    } else {
        object.insert(
            "body_text".to_string(),
            Value::String(truncate_provider_failure_text(trimmed)),
        );
    }
    Value::Object(object).to_string()
}

/// Builds bounded, secret-safe structured diagnostics from a provider event.
pub fn provider_failure_event_json(value: &Value) -> String {
    let mut object = Map::new();
    insert_provider_failure_value(&mut object, value.clone());
    Value::Object(object).to_string()
}

/// Returns a focused correction for common malformed model-output shapes.
pub fn provider_malformed_output_hint(raw_text: &str) -> Option<&'static str> {
    let value = serde_json::from_str::<Value>(raw_text).ok()?;
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

/// A typed, bounded diagnostic for malformed model output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderMalformedOutputError {
    kind: ProviderErrorKind,
    message: String,
    raw_text: String,
    provider_failure_json: String,
}

impl ProviderMalformedOutputError {
    /// Returns the stable provider failure category.
    pub fn kind(&self) -> ProviderErrorKind {
        self.kind
    }

    /// Returns the corrective diagnostic shown at the product boundary.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the original provider output retained for diagnostics.
    pub fn raw_text(&self) -> &str {
        &self.raw_text
    }

    /// Returns the bounded structured failure payload.
    pub fn provider_failure_json(&self) -> &str {
        &self.provider_failure_json
    }
}

impl std::fmt::Display for ProviderMalformedOutputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProviderMalformedOutputError {}

/// Shapes one malformed provider result without depending on product errors.
pub fn provider_malformed_output_error(
    error_kind: ProviderErrorKind,
    error_message: &str,
    raw_text: &str,
) -> ProviderMalformedOutputError {
    let mut message = format!("provider MAAP output is malformed: {error_message}");
    if let Some(hint) = provider_malformed_output_hint(raw_text) {
        message.push_str("; ");
        message.push_str(hint);
    }
    ProviderMalformedOutputError {
        kind: error_kind,
        message,
        raw_text: raw_text.to_string(),
        provider_failure_json: provider_malformed_output_failure_json(
            error_kind,
            error_message,
            raw_text,
        ),
    }
}

/// Builds a bounded diagnostic payload for malformed model output.
pub fn provider_malformed_output_failure_json(
    error_kind: ProviderErrorKind,
    error_message: &str,
    raw_text: &str,
) -> String {
    let parsed = serde_json::from_str::<Value>(raw_text).ok();
    let mut output = serde_json::json!({
        "format": if parsed.is_some() { "json" } else { "text" },
        "bytes": raw_text.len()
    });
    if let Some(Value::Object(object)) = parsed {
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
            "kind": error_kind.as_str(),
            "message": error_message
        },
        "output": output
    })
    .to_string()
}

fn provider_output_contains_bare_command_actions(object: &Map<String, Value>) -> bool {
    object
        .get("actions")
        .and_then(Value::as_array)
        .is_some_and(|actions| {
            actions.iter().any(|action| {
                action.as_object().is_some_and(|action_object| {
                    action_object.contains_key("command") && !action_object.contains_key("type")
                })
            })
        })
}

fn insert_provider_failure_value(object: &mut Map<String, Value>, value: Value) {
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
            .and_then(Value::as_str)
        {
            object.insert(
                "response_id".to_string(),
                Value::String(response_id.to_string()),
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

fn sanitize_provider_failure_value(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let value = if provider_failure_key_is_secret_like(&key) {
                        Value::String("[REDACTED]".to_string())
                    } else {
                        sanitize_provider_failure_value(value)
                    };
                    (key, value)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .take(MAX_PROVIDER_FAILURE_ARRAY_ITEMS)
                .map(sanitize_provider_failure_value)
                .collect(),
        ),
        Value::String(value) => Value::String(redact_or_truncate_provider_failure_text(&value)),
        other => other,
    }
}

fn redact_or_truncate_provider_failure_text(value: &str) -> String {
    if provider_failure_text_contains_secret_like(value) {
        "[REDACTED]".to_string()
    } else {
        truncate_provider_failure_text(value)
    }
}

fn provider_failure_key_is_secret_like(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("authorization")
        || key.contains("api_key")
        || key.contains("access_token")
        || key.contains("refresh_token")
        || key.contains("secret")
        || key.contains("password")
}

fn provider_failure_text_contains_secret_like(value: &str) -> bool {
    value.contains("-----BEGIN")
        || value
            .split_whitespace()
            .any(provider_failure_token_is_secret_like)
}

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

fn is_base64_url_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '-' || character == '_'
}

fn truncate_provider_failure_text(value: &str) -> String {
    let mut output = value
        .chars()
        .take(MAX_PROVIDER_FAILURE_TEXT_CHARS)
        .collect::<String>();
    if value.chars().count() > MAX_PROVIDER_FAILURE_TEXT_CHARS {
        output.push_str("...");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{
        provider_error_detail, provider_failure_event_json, provider_failure_json,
        provider_malformed_output_error, provider_malformed_output_failure_json,
        provider_malformed_output_hint,
    };
    use crate::ProviderErrorKind;

    #[test]
    /// Verifies provider diagnostics preserve useful structured fields while
    /// redacting nested credentials before crossing an agent boundary.
    fn provider_failure_json_redacts_secret_fields() {
        let output = provider_failure_json(
            Some(401),
            r#"{"error":{"message":"denied","api_key":"sk-secret"}}"#,
        );
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(value["status_code"], 401);
        assert_eq!(value["error"]["message"], "denied");
        assert_eq!(value["error"]["api_key"], "[REDACTED]");
    }

    #[test]
    /// Verifies human-readable and event diagnostics redact token-shaped text
    /// and bound provider-authored arrays without depending on product errors.
    fn provider_diagnostics_redact_text_and_bound_arrays() {
        assert_eq!(
            provider_error_detail(r#"{"error":{"message":"Bearer sk-secret"}}"#),
            "[REDACTED]"
        );
        let output = provider_failure_event_json(&serde_json::json!({
            "items": (0..40).collect::<Vec<_>>()
        }));
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(value["body"]["items"].as_array().unwrap().len(), 32);
    }

    #[test]
    /// Verifies malformed MAAP diagnostics identify common missing-envelope
    /// shapes without depending on product error or action types.
    fn provider_malformed_output_diagnostics_classify_bare_actions() {
        let raw_text = r#"{"actions":[{"command":"cargo test"}]}"#;

        assert!(
            provider_malformed_output_hint(raw_text)
                .unwrap()
                .contains("bare command objects inside actions")
        );
        let output = provider_malformed_output_failure_json(
            ProviderErrorKind::InvalidArgs,
            "actions[0].type is required",
            raw_text,
        );
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(value["error"]["kind"], "invalid_args");
        assert_eq!(value["output"]["format"], "json");
        assert_eq!(value["output"]["bare_command_actions"], true);
    }

    #[test]
    /// Verifies the typed malformed-output contract preserves corrective text,
    /// raw provider output, and bounded structured diagnostics for adapters.
    fn provider_malformed_output_error_preserves_adapter_diagnostics() {
        let raw_text = r#"{"command":"cargo test"}"#;
        let error = provider_malformed_output_error(
            ProviderErrorKind::InvalidArgs,
            "actions is required",
            raw_text,
        );

        assert_eq!(error.kind(), ProviderErrorKind::InvalidArgs);
        assert!(
            error
                .message()
                .contains("provider MAAP output is malformed")
        );
        assert!(error.message().contains("expected a MAAP action batch"));
        assert_eq!(error.raw_text(), raw_text);
        let failure: serde_json::Value =
            serde_json::from_str(error.provider_failure_json()).unwrap();
        assert_eq!(failure["error"]["message"], "actions is required");
        assert_eq!(failure["output"]["bare_command_object"], true);
    }
}
