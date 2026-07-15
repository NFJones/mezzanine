//! Provider-independent Anthropic Messages request shaping.
//!
//! This module owns non-secret option parsing and deterministic Messages API
//! JSON construction. Product adapters retain endpoint derivation, credentials,
//! HTTP headers, timeouts, transport, response parsing, and error projection.

use crate::{
    MAAP_ACTION_BATCH_TOOL_NAME, ModelMessageRole, ModelRequest, ProviderRequestAssemblyError,
    ProviderRequestAssemblyResult, maap_action_batch_schema,
    openai_maap_current_action_batch_description,
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
}
