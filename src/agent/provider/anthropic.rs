//! Anthropic Messages product transport adapter.
//!
//! This module attaches credentials, HTTP metadata, transport, quota headers,
//! and product response/error projection around provider-independent Anthropic
//! policy and response parsing in `mez-agent`.

use super::chat_completions::ChatCompletionsDialect;
use super::{
    MezError, ModelRequest, ModelResponse, ProviderHttpRequest, ProviderHttpResponse, Result,
    provider_quota_usage_from_headers, validate_non_empty,
};
#[cfg(test)]
use mez_agent::MAAP_ACTION_BATCH_TOOL_NAME as OPENAI_MAAP_FUNCTION_TOOL_NAME;
use mez_agent::{
    ANTHROPIC_MESSAGES_ENDPOINT, AnthropicMessagesOptions, AnthropicMessagesResponse,
    anthropic_messages_endpoint_for_base_url, anthropic_messages_request_body,
    anthropic_provider_failure_json, anthropic_request_requires_maap,
    parse_anthropic_messages_provider_body,
};
use std::collections::BTreeMap;

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
        Ok(anthropic_messages_endpoint_for_base_url(base_url)?)
    }

    /// Shapes sanitized Anthropic failure JSON while retaining the provider
    /// request id when the API supplied one.
    fn provider_failure_json(&self, status_code: Option<u16>, body: &str) -> String {
        anthropic_provider_failure_json(status_code, body)
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
        response: ProviderHttpResponse,
        request: &ModelRequest,
        provider_id: &str,
        stream: bool,
    ) -> Result<ModelResponse> {
        let ProviderHttpResponse { headers, body, .. } = response;
        let AnthropicMessagesResponse {
            model,
            raw_text,
            action_batch,
            usage,
        } = parse_anthropic_messages_provider_body(
            &body,
            &request.model,
            stream,
            &request.turn_id,
            &request.agent_id,
            anthropic_request_requires_maap(request),
        )?;
        Ok(ModelResponse {
            provider: provider_id.to_string(),
            model,
            raw_text,
            usage,
            latest_request_usage: None,
            quota_usage: provider_quota_usage_from_headers(&headers),
            action_batch,
            provider_transcript_events: Vec::new(),
        })
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
        options.anthropic_version().to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::provider::{
        AnthropicMessagesProvider, ReqwestProviderHttpTransport, provider_error_retry_class,
    };
    use crate::auth::{AuthPaths, AuthStore};
    use crate::error::MezErrorKind;
    use mez_agent::{DEFAULT_PROVIDER_TIMEOUT_MS, ProviderErrorRetryClass};
    use std::fs;

    /// Parses one synthetic Anthropic response with the requested stop reason
    /// and projects its lower error into the product error contract.
    fn anthropic_stop_reason_error(
        stop_reason: Option<&str>,
        raw_text: &str,
        requires_maap: bool,
    ) -> Option<MezError> {
        let body = serde_json::json!({
            "model": "claude-test",
            "content": [{ "type": "text", "text": raw_text }],
            "stop_reason": stop_reason,
        })
        .to_string();
        parse_anthropic_messages_provider_body(
            &body,
            "fallback-model",
            false,
            "turn-1",
            "agent-1",
            requires_maap,
        )
        .err()
        .map(Into::into)
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

    /// Verifies Anthropic `max_tokens` stop reasons surface the same
    /// output-limit recovery signal used by the runtime compaction path.
    #[test]
    fn anthropic_stop_reason_max_tokens_maps_to_output_limit() {
        let error = anthropic_stop_reason_error(Some("max_tokens"), "partial", true).unwrap();

        let failure_json = error.provider_failure_json().unwrap();
        assert!(failure_json.contains("max_output_tokens"), "{failure_json}");
        assert_eq!(error.provider_raw_text(), Some("partial"));
    }

    /// Verifies Anthropic stop reasons map context-window exhaustion into the
    /// shared runtime context-limit recovery path.
    #[test]
    fn anthropic_stop_reason_context_window_maps_to_context_limit() {
        let error =
            anthropic_stop_reason_error(Some("model_context_window_exceeded"), "partial", true)
                .unwrap();

        assert_eq!(
            provider_error_retry_class(&error),
            ProviderErrorRetryClass::ContextLimit
        );
    }

    /// Verifies Anthropic refusals stay terminal instead of entering retry or
    /// compaction recovery.
    #[test]
    fn anthropic_stop_reason_refusal_is_terminal() {
        let error = anthropic_stop_reason_error(Some("refusal"), "partial", true).unwrap();

        assert_eq!(
            provider_error_retry_class(&error),
            ProviderErrorRetryClass::NonRetryable
        );
    }

    /// Verifies unsupported Anthropic pause-turn responses fail closed as
    /// terminal provider errors instead of looping inside retry recovery.
    #[test]
    fn anthropic_stop_reason_pause_turn_is_terminal() {
        let error = anthropic_stop_reason_error(Some("pause_turn"), "partial", true).unwrap();

        assert_eq!(
            provider_error_retry_class(&error),
            ProviderErrorRetryClass::NonRetryable
        );
    }

    /// Verifies newly introduced or vendor-specific Anthropic stop reasons are
    /// surfaced as provider diagnostics instead of silently converting a
    /// potentially incomplete response into a successful turn.
    #[test]
    fn anthropic_unknown_stop_reason_is_terminal() {
        let error = anthropic_stop_reason_error(Some("future_reason"), "partial", true).unwrap();

        assert!(
            error
                .message()
                .contains("unrecognized stop_reason `future_reason`"),
            "{}",
            error.message()
        );
        assert_eq!(
            provider_error_retry_class(&error),
            ProviderErrorRetryClass::NonRetryable
        );
        assert_eq!(error.provider_raw_text(), Some("partial"));
        let failure_json = error.provider_failure_json().unwrap();
        assert!(failure_json.contains("future_reason"), "{failure_json}");
    }

    /// Verifies Anthropic prompt caching marks the stable system prompt as an
    /// ephemeral cache breakpoint by default.
    ///
    /// Anthropic only performs prompt caching when request content blocks carry
    /// `cache_control`, so the default request shape must establish a cache
    /// point on the long-lived system prompt while preserving ordinary user
    /// message serialization.
    #[test]
    fn anthropic_request_body_marks_system_prompt_cache_control_by_default() {
        let request = ModelRequest {
            provider: "anthropic".to_string(),
            model: "claude-3-7-sonnet".to_string(),
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: Some(512),
            temperature: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: false,
            interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
            allowed_actions: mez_agent::AllowedActionSet::say_only(),
            stop: None,
            messages: vec![
                mez_agent::ModelMessage {
                    role: mez_agent::ModelMessageRole::System,
                    source: mez_agent::ContextSourceKind::System,
                    content: "stable system prompt".to_string(),
                },
                mez_agent::ModelMessage {
                    role: mez_agent::ModelMessageRole::User,
                    source: mez_agent::ContextSourceKind::UserInstruction,
                    content: "summarize this conversation".to_string(),
                },
            ],
        };

        let body =
            anthropic_messages_request_body(&request, false, &AnthropicMessagesOptions::default())
                .unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(value["system"][0]["type"], "text");
        assert_eq!(value["system"][0]["text"], "stable system prompt");
        assert_eq!(
            value["system"][0]["cache_control"],
            serde_json::json!({ "type": "ephemeral" })
        );
        assert_eq!(
            value["messages"][0]["content"],
            "summarize this conversation"
        );
    }

    /// Verifies Anthropic request bodies serialize the provider-native effort
    /// control through `output_config.effort`.
    ///
    /// Anthropic documents `output_config.effort` as the Messages API control
    /// for response thoroughness and token efficiency. This regression keeps
    /// Mezzanine model profile reasoning selections wired to that native field
    /// without enabling the separate DeepSeek thinking toggle.
    #[test]
    fn anthropic_request_body_serializes_reasoning_effort() {
        let request = ModelRequest {
            provider: "anthropic".to_string(),
            model: "claude-fable-5".to_string(),
            reasoning_effort: Some("medium".to_string()),
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: Some(512),
            temperature: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: false,
            interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
            allowed_actions: mez_agent::AllowedActionSet::say_only(),
            stop: None,
            messages: vec![mez_agent::ModelMessage {
                role: mez_agent::ModelMessageRole::User,
                source: mez_agent::ContextSourceKind::UserInstruction,
                content: "summarize this conversation".to_string(),
            }],
        };

        let body =
            anthropic_messages_request_body(&request, false, &AnthropicMessagesOptions::default())
                .unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(value["output_config"]["effort"], "medium");
        assert!(value.get("thinking").is_none(), "{value}");
    }

    /// Verifies empty system and developer messages do not create cached
    /// Anthropic system blocks.
    ///
    /// The Anthropic request builder skips empty user-facing messages before
    /// serializing them. System and developer messages must follow the same
    /// empty-content rule so prompt caching does not emit an empty text block
    /// with cache-control metadata.
    #[test]
    fn anthropic_request_body_omits_empty_cached_system_blocks() {
        let request = ModelRequest {
            provider: "anthropic".to_string(),
            model: "claude-3-7-sonnet".to_string(),
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: Some(512),
            temperature: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: false,
            interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
            allowed_actions: mez_agent::AllowedActionSet::say_only(),
            stop: None,
            messages: vec![
                mez_agent::ModelMessage {
                    role: mez_agent::ModelMessageRole::System,
                    source: mez_agent::ContextSourceKind::System,
                    content: String::new(),
                },
                mez_agent::ModelMessage {
                    role: mez_agent::ModelMessageRole::Developer,
                    source: mez_agent::ContextSourceKind::DeveloperInstruction,
                    content: String::new(),
                },
                mez_agent::ModelMessage {
                    role: mez_agent::ModelMessageRole::User,
                    source: mez_agent::ContextSourceKind::UserInstruction,
                    content: "summarize this conversation".to_string(),
                },
            ],
        };

        let body =
            anthropic_messages_request_body(&request, false, &AnthropicMessagesOptions::default())
                .unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert!(value.get("system").is_none(), "{value}");
        assert_eq!(
            value["messages"][0]["content"],
            "summarize this conversation"
        );
    }

    /// Verifies Anthropic prompt caching can be disabled through provider
    /// options for callers that need the legacy plain-string system shape.
    #[test]
    fn anthropic_request_body_allows_prompt_caching_to_be_disabled() {
        let mut provider_options = BTreeMap::new();
        provider_options.insert("prompt_caching".to_string(), "false".to_string());
        let options = AnthropicMessagesOptions::from_provider_options(&provider_options).unwrap();
        let request = ModelRequest {
            provider: "anthropic".to_string(),
            model: "claude-3-7-sonnet".to_string(),
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: Some(512),
            temperature: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: false,
            interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
            allowed_actions: mez_agent::AllowedActionSet::say_only(),
            stop: None,
            messages: vec![
                mez_agent::ModelMessage {
                    role: mez_agent::ModelMessageRole::System,
                    source: mez_agent::ContextSourceKind::System,
                    content: "stable system prompt".to_string(),
                },
                mez_agent::ModelMessage {
                    role: mez_agent::ModelMessageRole::User,
                    source: mez_agent::ContextSourceKind::UserInstruction,
                    content: "summarize this conversation".to_string(),
                },
            ],
        };

        let body = anthropic_messages_request_body(&request, false, &options).unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(value["system"], "stable system prompt");
    }

    /// Verifies Anthropic action-execution requests advertise one provider-native
    /// MAAP tool and force that tool even for say-only surfaces such as
    /// compaction and remember flows.
    #[test]
    fn anthropic_request_body_forces_maap_tool_for_say_only_action_execution() {
        let request = ModelRequest {
            provider: "anthropic".to_string(),
            model: "claude-3-7-sonnet".to_string(),
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: Some(512),
            temperature: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: false,
            interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
            allowed_actions: mez_agent::AllowedActionSet::say_only(),
            stop: None,
            messages: vec![mez_agent::ModelMessage {
                role: mez_agent::ModelMessageRole::User,
                source: mez_agent::ContextSourceKind::UserInstruction,
                content: "summarize this conversation".to_string(),
            }],
        };

        let body =
            anthropic_messages_request_body(&request, false, &AnthropicMessagesOptions::default())
                .unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(value["tool_choice"]["type"], "tool");
        assert_eq!(value["tool_choice"]["name"], OPENAI_MAAP_FUNCTION_TOOL_NAME);
        assert_eq!(
            value["tool_choice"]["disable_parallel_tool_use"],
            serde_json::json!(true)
        );
        assert_eq!(value["tools"][0]["name"], OPENAI_MAAP_FUNCTION_TOOL_NAME);
        assert_eq!(
            value["tools"][0]["input_schema"]["required"],
            serde_json::json!(["rationale", "thought", "actions"])
        );
        let description = value["tools"][0]["description"].as_str().unwrap();
        assert!(description.contains("Return a function call, not prose"));
    }

    /// Verifies AutoSizing requests stay tool-free so Anthropic routing turns do
    /// not advertise or force the MAAP carrier.
    #[test]
    fn anthropic_request_body_omits_tools_for_auto_sizing() {
        let request = ModelRequest {
            provider: "anthropic".to_string(),
            model: "claude-3-7-sonnet".to_string(),
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: Some(512),
            temperature: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: false,
            interaction_kind: mez_agent::ModelInteractionKind::AutoSizing,
            allowed_actions: mez_agent::AllowedActionSet::say_only(),
            stop: None,
            messages: vec![mez_agent::ModelMessage {
                role: mez_agent::ModelMessageRole::User,
                source: mez_agent::ContextSourceKind::UserInstruction,
                content: "pick the best provider".to_string(),
            }],
        };

        let body =
            anthropic_messages_request_body(&request, false, &AnthropicMessagesOptions::default())
                .unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert!(value.get("tools").is_none());
        assert!(value.get("tool_choice").is_none());
    }

    /// Verifies Anthropic HTTP-200 stream error events classify by structured
    /// error type and preserve the provider request id for diagnostics.
    #[test]
    fn anthropic_stream_error_event_is_retryable_and_preserves_request_id() {
        let body = concat!(
            "event: error\n",
            "data: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"too many requests\"},\"request_id\":\"req_123\"}\n\n"
        );

        let error: MezError = parse_anthropic_messages_provider_body(
            body,
            "fallback-model",
            true,
            "turn-1",
            "agent-1",
            true,
        )
        .unwrap_err()
        .into();

        assert_eq!(
            provider_error_retry_class(&error),
            ProviderErrorRetryClass::RetryableTransport
        );
        let failure_json = error.provider_failure_json().unwrap();
        assert!(
            failure_json.contains(r#""request_id":"req_123""#),
            "{failure_json}"
        );
    }
}
