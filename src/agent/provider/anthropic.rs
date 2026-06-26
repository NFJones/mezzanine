//! Anthropic Messages provider shell and endpoint helpers.
//!
//! This module owns the Anthropic-specific provider boundary built on top of
//! the shared Chat Completions transport shell. It defines the Anthropic
//! endpoint derivation rules, provider identity defaults, and current
//! first-pass skeleton behavior for request, response, and model-catalog
//! handling while keeping the parent module responsible for facade exports and
//! auth-store construction.

use super::chat_completions::ChatCompletionsDialect;
use super::{
    ANTHROPIC_MESSAGES_ENDPOINT, MezError, ModelRequest, ModelResponse, ProviderHttpRequest,
    ProviderHttpResponse, Result, validate_non_empty,
};
use std::collections::BTreeMap;

/// Default Anthropic Messages API version used when provider options omit one.
const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";
/// Conservative fallback output cap required by Anthropic Messages requests.
const DEFAULT_ANTHROPIC_MAX_TOKENS: usize = 4096;

/// Provider-level options for Anthropic Messages requests.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AnthropicMessagesOptions {
    anthropic_version: String,
    default_max_tokens: usize,
}

impl Default for AnthropicMessagesOptions {
    fn default() -> Self {
        Self {
            anthropic_version: DEFAULT_ANTHROPIC_VERSION.to_string(),
            default_max_tokens: DEFAULT_ANTHROPIC_MAX_TOKENS,
        }
    }
}

impl AnthropicMessagesOptions {
    /// Parses Anthropic-specific provider options while rejecting option names
    /// that only make sense for OpenAI-compatible or DeepSeek request shapes.
    fn from_provider_options(provider_options: &BTreeMap<String, String>) -> Result<Self> {
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
                    return Err(MezError::invalid_args(format!(
                        "Anthropic provider option `{key}` is not supported by the Anthropic Messages API"
                    )));
                }
                _ => {}
            }
        }
        Ok(options)
    }
}

/// Parses a positive integer provider option.
fn parse_positive_usize(label: &str, value: &str) -> Result<usize> {
    let parsed = value
        .trim()
        .parse::<usize>()
        .map_err(|_| MezError::invalid_args(format!("{label} must be a positive integer")))?;
    if parsed == 0 {
        return Err(MezError::invalid_args(format!(
            "{label} must be a positive integer"
        )));
    }
    Ok(parsed)
}

/// Chat Completions transport dialect implementation for Anthropic Messages.
#[derive(Debug, Clone, Default)]
pub struct AnthropicMessagesDialect {
    options: AnthropicMessagesOptions,
}

impl AnthropicMessagesDialect {
    /// Builds an Anthropic dialect from configured non-secret provider options.
    pub(in crate::agent) fn from_provider_options(
        provider_options: &BTreeMap<String, String>,
    ) -> Result<Self> {
        Ok(Self {
            options: AnthropicMessagesOptions::from_provider_options(provider_options)?,
        })
    }
}

impl ChatCompletionsDialect for AnthropicMessagesDialect {
    /// Returns the default provider id used before configuration overrides are applied.
    fn default_provider_id(&self) -> &'static str {
        "anthropic"
    }

    /// Returns the default Anthropic Messages endpoint.
    fn default_chat_endpoint(&self) -> &'static str {
        ANTHROPIC_MESSAGES_ENDPOINT
    }

    /// Returns the human-readable provider label used in diagnostics.
    fn provider_label(&self) -> &'static str {
        "Anthropic"
    }

    /// Returns the diagnostic label used when validating a credential.
    fn credential_label(&self) -> &'static str {
        "Anthropic API key"
    }

    /// Derives the Anthropic Messages endpoint from a configured base URL.
    fn chat_endpoint_for_base_url(&self, base_url: &str) -> Result<String> {
        anthropic_messages_endpoint_for_base_url(base_url)
    }

    /// Builds one provider-specific Messages API request.
    fn build_chat_request(
        &self,
        request: &ModelRequest,
        api_key: Option<&str>,
        endpoint: &str,
        stream: bool,
        timeout_ms: u64,
    ) -> Result<ProviderHttpRequest> {
        build_anthropic_messages_http_request(
            request,
            api_key,
            endpoint,
            stream,
            timeout_ms,
            &self.options,
        )
    }

    /// Parses one successful provider-specific Messages API response.
    fn parse_chat_response(
        &self,
        _response: ProviderHttpResponse,
        _request: &ModelRequest,
        _provider_id: &str,
        _stream: bool,
    ) -> Result<ModelResponse> {
        Err(MezError::invalid_state(
            "Anthropic provider response parsing is not implemented yet",
        ))
    }

    /// Builds the provider-specific model catalog HTTP request.
    fn build_models_request(
        &self,
        _api_key: Option<&str>,
        _chat_endpoint: &str,
        _timeout_ms: u64,
    ) -> Result<ProviderHttpRequest> {
        Err(MezError::invalid_state(
            "Anthropic provider model listing is not implemented yet",
        ))
    }
}

/// Derives the Anthropic Messages endpoint from a configured base URL.
pub(super) fn anthropic_messages_endpoint_for_base_url(base_url: &str) -> Result<String> {
    validate_non_empty("Anthropic provider base URL", base_url)?;
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.ends_with("/v1/messages") || base_url.ends_with("/messages") {
        return Ok(base_url.to_string());
    }
    if base_url.ends_with("/v1") {
        return Ok(format!("{base_url}/messages"));
    }
    Ok(format!("{base_url}/v1/messages"))
}

/// Builds one Anthropic Messages API HTTP request.
fn build_anthropic_messages_http_request(
    request: &ModelRequest,
    api_key: Option<&str>,
    endpoint: &str,
    stream: bool,
    timeout_ms: u64,
    options: &AnthropicMessagesOptions,
) -> Result<ProviderHttpRequest> {
    if let Some(api_key) = api_key {
        validate_non_empty("Anthropic provider API key", api_key)?;
    }
    validate_non_empty("Anthropic Messages endpoint", endpoint)?;
    if timeout_ms == 0 {
        return Err(MezError::invalid_args(
            "Anthropic provider timeout must be greater than zero",
        ));
    }
    let body = anthropic_messages_request_body(request, stream, options)?;
    let mut headers = BTreeMap::new();
    headers.insert(
        "Accept".to_string(),
        if stream {
            "text/event-stream".to_string()
        } else {
            "application/json".to_string()
        },
    );
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers.insert(
        "anthropic-version".to_string(),
        options.anthropic_version.clone(),
    );
    if let Some(api_key) = api_key {
        headers.insert("x-api-key".to_string(), api_key.to_string());
    }
    Ok(ProviderHttpRequest {
        method: "POST".to_string(),
        url: endpoint.to_string(),
        headers,
        body,
        timeout_ms,
        max_response_bytes: None,
    })
}

/// Builds an Anthropic-compliant Messages API JSON body.
fn anthropic_messages_request_body(
    request: &ModelRequest,
    stream: bool,
    options: &AnthropicMessagesOptions,
) -> Result<String> {
    let mut system_parts = Vec::new();
    let mut messages = Vec::<serde_json::Value>::new();
    for message in &request.messages {
        let role = match message.role {
            super::ModelMessageRole::System | super::ModelMessageRole::Developer => {
                system_parts.push(message.content.clone());
                continue;
            }
            super::ModelMessageRole::Assistant => "assistant",
            super::ModelMessageRole::User | super::ModelMessageRole::Tool => "user",
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
        return Err(MezError::invalid_args(
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
    if !system_parts.is_empty() {
        body["system"] = serde_json::json!(system_parts.join("\n\n"));
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
        MezError::invalid_state(format!(
            "Anthropic Messages request encoding failed: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        AnthropicMessagesProvider, DEFAULT_PROVIDER_TIMEOUT_MS, ReqwestProviderHttpTransport,
    };
    use crate::auth::{AuthPaths, AuthStore};
    use crate::error::MezErrorKind;
    use std::fs;

    /// Verifies Anthropic base URL normalization accepts the documented root,
    /// versioned root, and full Messages endpoint forms without producing an
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

    /// Verifies Anthropic provider construction scopes credentials to the
    /// configured provider id rather than only the literal `anthropic` name.
    #[test]
    fn anthropic_provider_from_auth_store_uses_configured_provider_id() {
        let root = std::env::temp_dir().join(format!(
            "mez-auth-anthropic-provider-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let store = AuthStore::new(AuthPaths::under_config_root(&root));
        let credential_store = store.file_credential_store("claude-prod").unwrap();
        store
            .login_with_api_key(
                "claude-prod",
                "default",
                "anthropic-test-key",
                &credential_store,
            )
            .unwrap();

        let provider = super::super::anthropic_provider_from_auth_store_with_provider_options(
            &store,
            "claude-prod",
            Some("https://api.anthropic.com/v1"),
            &BTreeMap::new(),
            DEFAULT_PROVIDER_TIMEOUT_MS,
            ReqwestProviderHttpTransport,
        )
        .unwrap();

        assert_eq!(provider.provider_id(), "claude-prod");
        assert_eq!(provider.endpoint, "https://api.anthropic.com/v1/messages");

        let _ = fs::remove_dir_all(root);
    }

    /// Verifies direct Anthropic provider construction fails clearly when the
    /// configured provider id has no stored credential.
    #[test]
    fn anthropic_provider_from_auth_store_reports_missing_provider_credential() {
        let root = std::env::temp_dir().join(format!(
            "mez-auth-anthropic-missing-provider-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let store = AuthStore::new(AuthPaths::under_config_root(&root));

        let error = super::super::anthropic_provider_from_auth_store_with_provider_options(
            &store,
            "claude-prod",
            None,
            &BTreeMap::new(),
            DEFAULT_PROVIDER_TIMEOUT_MS,
            ReqwestProviderHttpTransport,
        )
        .unwrap_err();

        assert_eq!(error.kind(), MezErrorKind::InvalidState);
        assert_eq!(
            error.message(),
            "Anthropic provider `claude-prod` requires an authenticated API key"
        );

        let _ = fs::remove_dir_all(root);
    }

    /// Verifies Anthropic providers can still be constructed without auth for
    /// test or proxy scenarios when callers explicitly use the shell type.
    #[test]
    fn anthropic_provider_without_auth_uses_default_endpoint() {
        let provider =
            AnthropicMessagesProvider::without_auth(ReqwestProviderHttpTransport).unwrap();

        assert_eq!(provider.provider_id(), "anthropic");
        assert_eq!(provider.endpoint, ANTHROPIC_MESSAGES_ENDPOINT);
    }
}
