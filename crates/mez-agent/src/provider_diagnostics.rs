//! Secret-safe provider failure diagnostic shaping.
//!
//! This module normalizes provider error bodies into bounded diagnostics that
//! are safe to persist or pass across worker boundaries. It intentionally owns
//! no transport, credential storage, or product error types.

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
    use super::{provider_error_detail, provider_failure_event_json, provider_failure_json};

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
}
