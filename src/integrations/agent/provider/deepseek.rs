//! DeepSeek Chat Completions product transport and response adapter.
//!
//! This module attaches credentials, HTTP metadata, transport, quota headers,
//! provider identity, and product error projection around lower-owned DeepSeek
//! endpoint, request policy, and response parsing.

use super::chat_completions::{ChatCompletionsDialect, ChatCompletionsRetry};
use super::errors::provider_maap_parse_error;
use super::{
    MezError, ModelRequest, ModelResponse, ProviderHttpRequest, ProviderHttpResponse, Result,
    provider_quota_usage_from_headers, validate_non_empty,
};
use mez_agent::{
    DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME, DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME,
    DEEPSEEK_CHAT_COMPLETIONS_ENDPOINT, DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME,
    DeepSeekMaapRequestStrategy, DeepSeekResponse, deepseek_chat_completions_endpoint_for_base_url,
    deepseek_chat_completions_request_body_with_strategy, deepseek_effective_stream,
    deepseek_maap_request_strategy, deepseek_models_endpoint_for_base_url,
    deepseek_request_requires_maap, deepseek_should_retry_with_forced_maap,
    parse_deepseek_chat_completions_provider_body,
};
use std::collections::BTreeMap;

/// Chat Completions dialect implementation for DeepSeek's native API shape.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeepSeekChatCompletionsDialect;

impl ChatCompletionsDialect for DeepSeekChatCompletionsDialect {
    fn default_provider_id(&self) -> &'static str {
        "deepseek"
    }

    fn default_chat_endpoint(&self) -> &'static str {
        DEEPSEEK_CHAT_COMPLETIONS_ENDPOINT
    }

    fn provider_label(&self) -> &'static str {
        "DeepSeek"
    }

    fn credential_label(&self) -> &'static str {
        "DeepSeek API key"
    }

    fn chat_endpoint_for_base_url(&self, base_url: &str) -> Result<String> {
        Ok(deepseek_chat_completions_endpoint_for_base_url(base_url)?)
    }

    fn build_chat_request(
        &self,
        request: &ModelRequest,
        api_key: Option<&str>,
        endpoint: &str,
        stream: bool,
        timeout_ms: u64,
    ) -> Result<ProviderHttpRequest> {
        build_deepseek_chat_completions_http_request_with_strategy(
            request,
            api_key,
            endpoint,
            stream,
            timeout_ms,
            deepseek_maap_request_strategy(request),
        )
    }

    fn parse_chat_response(
        &self,
        response: ProviderHttpResponse,
        request: &ModelRequest,
        provider_id: &str,
        stream: bool,
    ) -> Result<ModelResponse> {
        parse_deepseek_chat_completions_http_response(response, request, provider_id, stream)
    }

    fn effective_stream(&self, request: &ModelRequest, stream: bool) -> bool {
        deepseek_effective_stream(stream, deepseek_maap_request_strategy(request))
    }

    fn build_retry_chat_request(
        &self,
        request: &ModelRequest,
        api_key: Option<&str>,
        endpoint: &str,
        stream: bool,
        timeout_ms: u64,
        previous_response: &ModelResponse,
    ) -> Result<Option<ChatCompletionsRetry>> {
        let strategy = deepseek_maap_request_strategy(request);
        if !deepseek_should_retry_with_forced_maap(
            request,
            strategy,
            previous_response.action_batch.is_some(),
        ) {
            return Ok(None);
        }
        let request = build_deepseek_chat_completions_http_request_with_strategy(
            request,
            api_key,
            endpoint,
            stream,
            timeout_ms,
            DeepSeekMaapRequestStrategy::ForcedToolNonThinking,
        )?;
        Ok(Some(ChatCompletionsRetry {
            request,
            stream: false,
        }))
    }

    fn build_models_request(
        &self,
        api_key: Option<&str>,
        chat_endpoint: &str,
        timeout_ms: u64,
    ) -> Result<ProviderHttpRequest> {
        build_deepseek_models_http_request(api_key, chat_endpoint, timeout_ms)
    }

    fn require_action_batch(
        &self,
        response: ModelResponse,
        request: &ModelRequest,
    ) -> Result<ModelResponse> {
        deepseek_required_maap_response(response, request)
    }
}

/// Converts a successful DeepSeek response without required MAAP into a
/// repairable malformed-output provider error.
fn deepseek_required_maap_response(
    response: ModelResponse,
    request: &ModelRequest,
) -> Result<ModelResponse> {
    if response.action_batch.is_some() || !deepseek_request_requires_maap(request) {
        return Ok(response);
    }
    Err(provider_maap_parse_error(
        MezError::invalid_args(format!(
            "DeepSeek response did not call a Mezzanine DeepSeek shim tool ({DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME}, {DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME}, or {DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME}) or return a MAAP JSON object"
        )),
        &response.raw_text,
    ))
}

/// Builds a DeepSeek Chat Completions HTTP request.
#[cfg(test)]
pub fn build_deepseek_chat_completions_http_request(
    request: &ModelRequest,
    api_key: &str,
    endpoint: &str,
    stream: bool,
    timeout_ms: u64,
) -> Result<ProviderHttpRequest> {
    build_deepseek_chat_completions_http_request_with_strategy(
        request,
        Some(api_key),
        endpoint,
        stream,
        timeout_ms,
        deepseek_maap_request_strategy(request),
    )
}

/// Builds a DeepSeek Chat Completions HTTP request with an explicit MAAP strategy.
pub(super) fn build_deepseek_chat_completions_http_request_with_strategy(
    request: &ModelRequest,
    api_key: Option<&str>,
    endpoint: &str,
    stream: bool,
    timeout_ms: u64,
    strategy: DeepSeekMaapRequestStrategy,
) -> Result<ProviderHttpRequest> {
    if let Some(api_key) = api_key {
        validate_non_empty("DeepSeek provider bearer credential", api_key)?;
    }
    validate_non_empty("DeepSeek Chat Completions endpoint", endpoint)?;
    if timeout_ms == 0 {
        return Err(MezError::invalid_args(
            "DeepSeek provider timeout must be greater than zero",
        ));
    }
    let stream = deepseek_effective_stream(stream, strategy);
    let body = deepseek_chat_completions_request_body_with_strategy(request, stream, strategy)?;
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
    if let Some(api_key) = api_key {
        headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
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

/// Parses one successful DeepSeek HTTP response into a model response.
pub(super) fn parse_deepseek_chat_completions_http_response(
    response: ProviderHttpResponse,
    request: &ModelRequest,
    provider_id: &str,
    stream: bool,
) -> Result<ModelResponse> {
    let ProviderHttpResponse { headers, body, .. } = response;
    let DeepSeekResponse {
        model,
        raw_text,
        usage,
        action_batch,
        provider_transcript_events,
    } = parse_deepseek_chat_completions_provider_body(&body, request, stream)?;
    Ok(ModelResponse {
        provider: provider_id.to_string(),
        model,
        raw_text,
        usage,
        latest_request_usage: None,
        quota_usage: provider_quota_usage_from_headers(&headers),
        action_batch,
        provider_transcript_events,
    })
}

/// Builds a DeepSeek models listing HTTP request.
pub(super) fn build_deepseek_models_http_request(
    api_key: Option<&str>,
    chat_endpoint: &str,
    timeout_ms: u64,
) -> Result<ProviderHttpRequest> {
    if let Some(api_key) = api_key {
        validate_non_empty("DeepSeek model listing credential", api_key)?;
    }
    let models_endpoint = deepseek_models_endpoint_for_base_url(chat_endpoint)?;
    let mut headers = BTreeMap::new();
    headers.insert("Accept".to_string(), "application/json".to_string());
    if let Some(api_key) = api_key {
        headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
    }
    Ok(ProviderHttpRequest {
        method: "GET".to_string(),
        url: models_endpoint,
        headers,
        body: String::new(),
        timeout_ms,
        max_response_bytes: None,
    })
}
