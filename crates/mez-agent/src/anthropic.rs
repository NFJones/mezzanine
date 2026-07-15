//! Provider-independent Anthropic Messages request shaping.
//!
//! This module owns non-secret option parsing and deterministic Messages API
//! JSON construction. Product adapters retain endpoint derivation, credentials,
//! HTTP headers, timeouts, transport, response parsing, and error projection.

use crate::{
    MAAP_ACTION_BATCH_TOOL_NAME, ModelMessageRole, ModelRequest, ModelTokenUsage,
    ProviderEndpointError, ProviderEndpointResult, ProviderRequestAssemblyError,
    ProviderRequestAssemblyResult, maap_action_batch_schema,
    openai_maap_current_action_batch_description, provider_failure_event_json,
    provider_failure_json,
};
use std::collections::BTreeMap;

/// Default Anthropic Messages API version used when options omit one.
pub const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";
/// Conservative fallback output cap required by Anthropic Messages requests.
pub const DEFAULT_ANTHROPIC_MAX_TOKENS: usize = 4096;
/// Default prompt-caching policy for stable system prompt blocks.
pub const DEFAULT_ANTHROPIC_PROMPT_CACHING: bool = true;

/// Parsed non-secret request policy for the Anthropic Messages API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicMessagesOptions {
    anthropic_version: String,
    default_max_tokens: usize,
    prompt_caching: bool,
}

impl Default for AnthropicMessagesOptions {
    fn default() -> Self {
        Self {
            anthropic_version: DEFAULT_ANTHROPIC_VERSION.to_string(),
            default_max_tokens: DEFAULT_ANTHROPIC_MAX_TOKENS,
            prompt_caching: DEFAULT_ANTHROPIC_PROMPT_CACHING,
        }
    }
}

impl AnthropicMessagesOptions {
    /// Parses Anthropic options and rejects settings from incompatible APIs.
    pub fn from_provider_options(
        provider_options: &BTreeMap<String, String>,
    ) -> ProviderRequestAssemblyResult<Self> {
        let mut options = Self::default();
        for (key, value) in provider_options {
            match key.as_str() {
                "anthropic_version" | "anthropic-version" => {
                    validate_non_empty("Anthropic provider option `anthropic_version`", value)?;
                    options.anthropic_version = value.trim().to_string();
                }
                "max_tokens" | "default_max_tokens" => {
                    options.default_max_tokens = parse_positive_usize(
                        "Anthropic provider option `default_max_tokens`",
                        value,
                    )?;
                }
                "prompt_caching" | "prompt-caching" => {
                    options.prompt_caching =
                        parse_bool_option("Anthropic provider option `prompt_caching`", value)?;
                }
                "max_output_tokens"
                | "context_window_tokens"
                | "context_limit_tokens"
                | "privacy_tier"
                | "residency"
                | "approval_policy"
                | "reasoning_effort" => {}
                "maap_output"
                | "maap_output_mode"
                | "structured_output"
                | "response_format"
                | "tool_choice"
                | "maap_tool_choice"
                | "parallel_tool_calls"
                | "supports_parallel_tool_calls"
                | "tool_calls"
                | "supports_tool_calls"
                | "output_token_field"
                | "maap_surface"
                | "thinking" => {
                    return Err(ProviderRequestAssemblyError::invalid_args(format!(
                        "Anthropic provider option `{key}` is not supported by the Anthropic Messages API"
                    )));
                }
                _ => {}
            }
        }
        Ok(options)
    }

    /// Returns the API version value required by the Anthropic HTTP header.
    pub fn anthropic_version(&self) -> &str {
        &self.anthropic_version
    }
}

/// Derives an Anthropic Messages endpoint from a configured base URL.
pub fn anthropic_messages_endpoint_for_base_url(base_url: &str) -> ProviderEndpointResult<String> {
    if base_url.trim().is_empty() {
        return Err(ProviderEndpointError::invalid_args(
            "Anthropic provider base URL must not be empty",
        ));
    }
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.ends_with("/v1/messages") || base_url.ends_with("/messages") {
        return Ok(base_url.to_string());
    }
    if base_url.ends_with("/v1") {
        return Ok(format!("{base_url}/messages"));
    }
    Ok(format!("{base_url}/v1/messages"))
}

fn validate_non_empty(label: &str, value: &str) -> ProviderRequestAssemblyResult<()> {
    if value.trim().is_empty() {
        return Err(ProviderRequestAssemblyError::invalid_args(format!(
            "{label} must not be empty"
        )));
    }
    Ok(())
}

fn parse_positive_usize(label: &str, value: &str) -> ProviderRequestAssemblyResult<usize> {
    let parsed = value.trim().parse::<usize>().map_err(|_| {
        ProviderRequestAssemblyError::invalid_args(format!("{label} must be a positive integer"))
    })?;
    if parsed == 0 {
        return Err(ProviderRequestAssemblyError::invalid_args(format!(
            "{label} must be a positive integer"
        )));
    }
    Ok(parsed)
}

fn parse_bool_option(label: &str, value: &str) -> ProviderRequestAssemblyResult<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" | "enabled" => Ok(true),
        "false" | "0" | "no" | "off" | "disabled" => Ok(false),
        _ => Err(ProviderRequestAssemblyError::invalid_args(format!(
            "{label} must be true or false"
        ))),
    }
}

/// Builds one Anthropic-compliant Messages API JSON body.
pub fn anthropic_messages_request_body(
    request: &ModelRequest,
    stream: bool,
    options: &AnthropicMessagesOptions,
) -> ProviderRequestAssemblyResult<String> {
    let mut system_parts = Vec::new();
    let mut messages = Vec::<serde_json::Value>::new();
    for message in &request.messages {
        let role = match message.role {
            ModelMessageRole::System | ModelMessageRole::Developer => {
                if !message.content.is_empty() {
                    system_parts.push(message.content.clone());
                }
                continue;
            }
            ModelMessageRole::Assistant => "assistant",
            ModelMessageRole::User | ModelMessageRole::Tool => "user",
        };
        if message.content.is_empty() {
            continue;
        }
        if let Some(last) = messages.last_mut()
            && last.get("role").and_then(serde_json::Value::as_str) == Some(role)
        {
            let previous = last
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            last["content"] = serde_json::json!(format!("{previous}\n\n{}", message.content));
            continue;
        }
        messages.push(serde_json::json!({
            "role": role,
            "content": message.content,
        }));
    }
    if messages.is_empty() {
        return Err(ProviderRequestAssemblyError::invalid_args(
            "Anthropic Messages request requires at least one user or assistant message",
        ));
    }
    let max_tokens = request
        .max_output_tokens
        .filter(|tokens| *tokens > 0)
        .unwrap_or(options.default_max_tokens);
    let mut body = serde_json::json!({
        "model": request.model,
        "max_tokens": max_tokens,
        "messages": messages,
        "stream": stream,
    });
    if let Some(effort) = request
        .reasoning_effort
        .as_deref()
        .filter(|effort| !effort.is_empty())
    {
        body["output_config"] = serde_json::json!({ "effort": effort });
    }
    if !system_parts.is_empty() {
        let system_text = system_parts.join("\n\n");
        body["system"] = if options.prompt_caching {
            serde_json::json!([{
                "type": "text",
                "text": system_text,
                "cache_control": { "type": "ephemeral" },
            }])
        } else {
            serde_json::json!(system_text)
        };
    }
    if anthropic_request_requires_maap(request) {
        body["tools"] = serde_json::json!([anthropic_maap_tool(request)]);
        body["tool_choice"] = anthropic_maap_tool_choice();
    }
    if let Some(temperature) = request
        .temperature
        .as_deref()
        .and_then(|temperature| temperature.parse::<f64>().ok())
        .filter(|temperature| temperature.is_finite())
    {
        body["temperature"] = serde_json::json!(temperature);
    }
    if let Some(stop) = request.stop.as_ref().filter(|stop| !stop.is_empty()) {
        body["stop_sequences"] = serde_json::json!(stop);
    }
    serde_json::to_string(&body).map_err(|error| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "Anthropic Messages request encoding failed: {error}"
        ))
    })
}

fn anthropic_maap_tool(request: &ModelRequest) -> serde_json::Value {
    serde_json::json!({
        "name": MAAP_ACTION_BATCH_TOOL_NAME,
        "description": openai_maap_current_action_batch_description(request),
        "input_schema": maap_action_batch_schema(
            &request.allowed_actions,
            &request.available_mcp_tools,
        )
    })
}

fn anthropic_maap_tool_choice() -> serde_json::Value {
    serde_json::json!({
        "type": "tool",
        "name": MAAP_ACTION_BATCH_TOOL_NAME,
        "disable_parallel_tool_use": true,
    })
}

/// Returns whether an Anthropic request must produce a MAAP action batch.
pub fn anthropic_request_requires_maap(request: &ModelRequest) -> bool {
    request.interaction_kind.expects_maap_batch() && !request.allowed_actions.actions.is_empty()
}

/// Builds sanitized Anthropic failure JSON while preserving a provider request id.
pub fn anthropic_provider_failure_json(status_code: Option<u16>, body: &str) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    anthropic_failure_json_with_request_id(
        provider_failure_json(status_code, body),
        parsed.as_ref(),
    )
}

/// Builds sanitized Anthropic event diagnostics while preserving a request id.
pub fn anthropic_provider_failure_event_json(value: &serde_json::Value) -> String {
    anthropic_failure_json_with_request_id(provider_failure_event_json(value), Some(value))
}

fn anthropic_failure_json_with_request_id(
    base_json: String,
    value: Option<&serde_json::Value>,
) -> String {
    let Some(request_id) = anthropic_request_id(value) else {
        return base_json;
    };
    let Ok(mut object) =
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&base_json)
    else {
        return base_json;
    };
    object.insert(
        "request_id".to_string(),
        serde_json::Value::String(request_id.to_string()),
    );
    serde_json::Value::Object(object).to_string()
}

fn anthropic_request_id(value: Option<&serde_json::Value>) -> Option<&str> {
    let value = value?;
    value
        .pointer("/request_id")
        .or_else(|| value.pointer("/error/request_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|request_id| !request_id.trim().is_empty())
}

/// Extracts Anthropic token-usage and prompt-cache counters.
pub fn anthropic_usage_from_value(value: Option<&serde_json::Value>) -> ModelTokenUsage {
    let Some(value) = value else {
        return ModelTokenUsage::default();
    };
    let cached_input_tokens = value
        .get("cache_read_input_tokens")
        .and_then(serde_json::Value::as_u64);
    ModelTokenUsage {
        input_tokens: anthropic_usage_u64(value, "input_tokens")
            .saturating_add(cached_input_tokens.unwrap_or(0)),
        output_tokens: anthropic_usage_u64(value, "output_tokens"),
        reasoning_tokens: 0,
        cached_input_tokens,
        cache_write_input_tokens: value
            .get("cache_creation_input_tokens")
            .and_then(serde_json::Value::as_u64),
    }
}

/// Overlays cumulative Anthropic usage fields from one stream event.
pub fn anthropic_overlay_usage(current: &mut ModelTokenUsage, value: Option<&serde_json::Value>) {
    let next = anthropic_usage_from_value(value);
    if next.input_tokens > 0 {
        current.input_tokens = next.input_tokens;
    }
    if next.output_tokens > 0 {
        current.output_tokens = next.output_tokens;
    }
    if next.reasoning_tokens > 0 {
        current.reasoning_tokens = next.reasoning_tokens;
    }
    if next.cached_input_tokens.is_some() {
        current.cached_input_tokens = next.cached_input_tokens;
    }
    if next.cache_write_input_tokens.is_some() {
        current.cache_write_input_tokens = next.cache_write_input_tokens;
    }
}

fn anthropic_usage_u64(value: &serde_json::Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies options from incompatible provider APIs fail at the lower
    /// Anthropic request-policy boundary.
    #[test]
    fn anthropic_options_reject_incompatible_request_controls() {
        let options = BTreeMap::from([("tool_choice".to_string(), "required".to_string())]);

        let error = AnthropicMessagesOptions::from_provider_options(&options).unwrap_err();

        assert_eq!(
            error.kind(),
            crate::ProviderRequestAssemblyErrorKind::InvalidArgs
        );
        assert!(error.message().contains("not supported"));
    }

    /// Verifies Anthropic base URL normalization accepts documented root,
    /// versioned-root, and full Messages endpoint forms without producing an
    /// OpenAI-compatible path.
    #[test]
    fn anthropic_base_url_derives_documented_messages_endpoints() {
        assert_eq!(
            anthropic_messages_endpoint_for_base_url("https://api.anthropic.com").unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            anthropic_messages_endpoint_for_base_url("https://api.anthropic.com/v1").unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            anthropic_messages_endpoint_for_base_url("https://api.anthropic.com/messages").unwrap(),
            "https://api.anthropic.com/messages"
        );
        assert_eq!(
            anthropic_messages_endpoint_for_base_url("https://api.anthropic.com/v1/messages")
                .unwrap(),
            "https://api.anthropic.com/v1/messages"
        );
    }

    /// Verifies Anthropic status failure shaping retains the provider request
    /// id inside sanitized diagnostics.
    #[test]
    fn anthropic_failure_json_preserves_request_id() {
        let failure_json = anthropic_provider_failure_json(
            Some(429),
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"},"request_id":"req_123"}"#,
        );

        assert!(
            failure_json.contains(r#""request_id":"req_123""#),
            "{failure_json}"
        );
    }

    /// Verifies Anthropic usage parsing keeps prompt-cache read and write
    /// counters distinct so billing and hit-ratio accounting remain stable.
    #[test]
    fn anthropic_usage_parses_cache_creation_tokens() {
        let usage = anthropic_usage_from_value(Some(&serde_json::json!({
            "input_tokens": 42,
            "output_tokens": 9,
            "cache_read_input_tokens": 7,
            "cache_creation_input_tokens": 11
        })));

        assert_eq!(usage.input_tokens, 49);
        assert_eq!(usage.output_tokens, 9);
        assert_eq!(usage.cached_input_tokens, Some(7));
        assert_eq!(usage.cache_write_input_tokens, Some(11));
        assert_eq!(usage.billed_input_tokens(), 53);
        assert_eq!(usage.total_tokens(), 69);
        assert_eq!(usage.cached_input_hit_ratio_display(), "11.67%");

        let mut overlaid = ModelTokenUsage::default();
        anthropic_overlay_usage(
            &mut overlaid,
            Some(&serde_json::json!({
                "cache_creation_input_tokens": 13
            })),
        );

        assert_eq!(overlaid.cache_write_input_tokens, Some(13));
    }
}
