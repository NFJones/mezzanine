//! Generic OpenAI-compatible Chat Completions dialect.
//!
//! This module implements the conservative OpenAI-style Chat Completions wire
//! shape used for local and third-party compatible backends. It deliberately
//! avoids DeepSeek thinking fields, DeepSeek shim function names, hidden
//! reasoning transcript replay, and DeepSeek retry policy.

use super::chat_completions::{ChatCompletionsDialect, ChatCompletionsStreamDecoder};
use super::{
    MezError, ModelRequest, ModelResponse, ProviderHttpRequest, ProviderHttpResponse, Result,
    openai_models_endpoint_for_responses_endpoint, provider_quota_usage_from_headers,
    validate_non_empty,
};
use mez_agent::{
    OpenAiChatCompletionsOptions, OpenAiChatCompletionsResponse,
    OpenAiChatCompletionsStreamDecoder, SseEvent, openai_chat_completions_request_body_with_stream,
    parse_openai_chat_completions_response_body, parse_sse_events_with,
    prepare_openai_chat_completions_request_prefix_extension,
};
use std::collections::BTreeMap;

/// Chat Completions dialect for generic OpenAI-compatible providers.
#[derive(Debug, Clone, Default)]
pub struct OpenAiChatCompletionsDialect {
    options: OpenAiChatCompletionsOptions,
}

impl OpenAiChatCompletionsDialect {
    /// Builds a dialect from non-secret provider compatibility options.
    pub(in crate::integrations::agent) fn from_provider_options(
        provider_options: &BTreeMap<String, String>,
    ) -> Result<Self> {
        Ok(Self {
            options: OpenAiChatCompletionsOptions::from_provider_options(provider_options)?,
        })
    }
}

impl ChatCompletionsDialect for OpenAiChatCompletionsDialect {
    fn default_provider_id(&self) -> &'static str {
        "openai-compatible"
    }

    fn api_compatibility(&self) -> mez_agent::ProviderApiCompatibility {
        mez_agent::ProviderApiCompatibility::OpenAiChatCompletions
    }

    fn default_chat_endpoint(&self) -> &'static str {
        "http://localhost:1234/v1/chat/completions"
    }

    fn provider_label(&self) -> &'static str {
        "OpenAI-compatible Chat Completions"
    }

    fn credential_label(&self) -> &'static str {
        "OpenAI-compatible provider bearer credential"
    }

    fn chat_endpoint_for_base_url(&self, base_url: &str) -> Result<String> {
        openai_chat_completions_endpoint_for_base_url(base_url)
    }

    fn build_chat_request(
        &self,
        request: &ModelRequest,
        api_key: Option<&str>,
        endpoint: &str,
        stream: bool,
        timeout_ms: u64,
    ) -> Result<ProviderHttpRequest> {
        build_openai_chat_completions_http_request(
            request,
            api_key,
            endpoint,
            stream,
            timeout_ms,
            self.options,
        )
    }

    fn prepare_request_prefix_extension(
        &self,
        request: &mut ModelRequest,
        previous: Option<&ModelRequest>,
        cache_namespace: &str,
        _stream: bool,
    ) -> Result<()> {
        Ok(prepare_openai_chat_completions_request_prefix_extension(
            request,
            previous,
            cache_namespace,
            self.options,
        )?)
    }

    fn parse_chat_response(
        &self,
        response: ProviderHttpResponse,
        request: &ModelRequest,
        provider_id: &str,
        stream: bool,
    ) -> Result<ModelResponse> {
        parse_openai_chat_completions_http_response(response, request, provider_id, stream)
    }

    fn effective_stream(&self, _request: &ModelRequest, _stream: bool) -> bool {
        self.options.streaming_enabled()
    }

    fn stream_decoder(
        &self,
        _request: &ModelRequest,
    ) -> Result<Option<Box<dyn ChatCompletionsStreamDecoder>>> {
        Ok(Some(
            Box::new(OpenAiChatCompletionsStreamDecoder::default()),
        ))
    }

    fn build_models_request(
        &self,
        api_key: Option<&str>,
        chat_endpoint: &str,
        timeout_ms: u64,
    ) -> Result<ProviderHttpRequest> {
        build_openai_chat_completions_models_http_request(api_key, chat_endpoint, timeout_ms)
    }
}

/// Derives a generic OpenAI-compatible Chat Completions endpoint from a base URL.
pub(super) fn openai_chat_completions_endpoint_for_base_url(base_url: &str) -> Result<String> {
    validate_non_empty("OpenAI-compatible provider base URL", base_url)?;
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.ends_with("/chat/completions") {
        return Ok(base_url.to_string());
    }
    if let Some(prefix) = base_url.strip_suffix("/models") {
        return Ok(format!("{prefix}/chat/completions"));
    }
    Ok(format!("{base_url}/chat/completions"))
}

/// Builds a generic OpenAI-compatible Chat Completions HTTP request.
fn build_openai_chat_completions_http_request(
    request: &ModelRequest,
    api_key: Option<&str>,
    endpoint: &str,
    _stream: bool,
    timeout_ms: u64,
    options: OpenAiChatCompletionsOptions,
) -> Result<ProviderHttpRequest> {
    if let Some(api_key) = api_key {
        validate_non_empty("OpenAI-compatible provider bearer credential", api_key)?;
    }
    validate_non_empty("OpenAI-compatible Chat Completions endpoint", endpoint)?;
    validate_non_empty("OpenAI-compatible Chat Completions model", &request.model)?;
    if timeout_ms == 0 {
        return Err(MezError::invalid_args(
            "OpenAI-compatible provider timeout must be greater than zero",
        ));
    }
    let stream = options.streaming_enabled();
    let body = openai_chat_completions_request_body_with_stream(request, options, stream)?;
    let mut headers = BTreeMap::new();
    if let Some(api_key) = api_key {
        headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
    }
    Ok(super::provider_json_post_request(
        endpoint, body, stream, timeout_ms, headers,
    ))
}

/// Builds a generic OpenAI-compatible model-list HTTP request.
fn build_openai_chat_completions_models_http_request(
    api_key: Option<&str>,
    chat_endpoint: &str,
    timeout_ms: u64,
) -> Result<ProviderHttpRequest> {
    if let Some(api_key) = api_key {
        validate_non_empty("OpenAI-compatible model listing credential", api_key)?;
    }
    let chat_endpoint = openai_chat_completions_endpoint_for_base_url(chat_endpoint)?;
    let responses_endpoint = chat_endpoint
        .strip_suffix("/chat/completions")
        .map(|prefix| format!("{prefix}/responses"))
        .ok_or_else(|| {
            MezError::invalid_state(format!(
                "OpenAI-compatible Chat Completions endpoint must end with /chat/completions: {chat_endpoint}"
            ))
        })?;
    let models_endpoint = openai_models_endpoint_for_responses_endpoint(&responses_endpoint)?;
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
        timeouts: mez_agent::ProviderHttpTimeouts::from_total(timeout_ms),
        max_response_bytes: None,
    })
}

/// Parses a successful generic OpenAI-compatible Chat Completions response.
fn parse_openai_chat_completions_http_response(
    response: ProviderHttpResponse,
    request: &ModelRequest,
    provider_id: &str,
    stream: bool,
) -> Result<ModelResponse> {
    let ProviderHttpResponse { headers, body, .. } = response;
    let parsed = if stream {
        let mut decoder = OpenAiChatCompletionsStreamDecoder::default();
        let events = parse_sse_events_with(
            &body,
            "OpenAI-compatible Chat Completions stream contained no SSE events",
            |name, data| {
                decoder
                    .push_event(&SseEvent {
                        name: name.map(str::to_string),
                        data: data.to_string(),
                    })
                    .map_err(MezError::from)?;
                Ok::<(), MezError>(())
            },
        );
        events.map_err(|error| error.with_provider_incurred_usage(decoder.reported_usage()))?;
        let usage = decoder.reported_usage();
        decoder
            .finish(request)
            .map_err(|error| MezError::from(error).with_provider_incurred_usage(usage))?
    } else {
        let usage = serde_json::from_str::<serde_json::Value>(&body)
            .map(|root| mez_agent::openai_chat_completions_usage(&root))
            .unwrap_or_default();
        parse_openai_chat_completions_response_body(&body, request)
            .map_err(|error| MezError::from(error).with_provider_incurred_usage(usage))?
    };
    openai_chat_completions_model_response(parsed, headers, provider_id)
}

impl ChatCompletionsStreamDecoder for OpenAiChatCompletionsStreamDecoder {
    fn push_event(&mut self, event: &SseEvent) -> Result<Option<String>> {
        OpenAiChatCompletionsStreamDecoder::push_event(self, event).map_err(|error| {
            MezError::from(error).with_provider_incurred_usage(self.reported_usage())
        })
    }

    fn finish(
        self: Box<Self>,
        headers: BTreeMap<String, String>,
        request: &ModelRequest,
        provider_id: &str,
    ) -> Result<ModelResponse> {
        let usage = self.reported_usage();
        let parsed = OpenAiChatCompletionsStreamDecoder::finish(*self, request)
            .map_err(|error| MezError::from(error).with_provider_incurred_usage(usage))?;
        openai_chat_completions_model_response(parsed, headers, provider_id)
    }
}

/// Attaches product-owned provider identity and quota metadata to decoded output.
fn openai_chat_completions_model_response(
    parsed: OpenAiChatCompletionsResponse,
    headers: BTreeMap<String, String>,
    provider_id: &str,
) -> Result<ModelResponse> {
    let provider_transcript_events = parsed
        .native_tool_calls
        .map(|tool_calls| {
            mez_agent::ProviderTranscriptEvent::validated_openai_chat_completions_assistant_tool_call(
                provider_id.to_string(),
                parsed.raw_text.clone(),
                tool_calls,
            )
            .ok_or_else(|| {
                MezError::invalid_state(
                    "validated Chat Completions tool calls could not be bound to the configured provider owner",
                )
            })
        })
        .transpose()?
        .into_iter()
        .collect();
    Ok(ModelResponse {
        provider: provider_id.to_string(),
        model: parsed.model,
        raw_text: parsed.raw_text,
        usage: parsed.usage,
        latest_request_usage: None,
        quota_usage: provider_quota_usage_from_headers(&headers),
        action_batch: parsed.action_batch,
        provider_transcript_events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal request for pure response parsing; no provider or action executes.
    fn parsing_request() -> ModelRequest {
        ModelRequest {
            provider: "compatible".into(),
            model: "test".into(),
            model_capabilities: Default::default(),
            max_input_tokens: None,
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: None,
            temperature: None,
            stop: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: "turn-parser".into(),
            agent_id: "agent-%1".into(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: false,
            interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
            allowed_actions: mez_agent::AllowedActionSet::for_capability(
                mez_agent::AgentCapability::Shell,
            ),
            messages: Vec::new().into(),
        }
    }

    /// Parsed provider counters survive malformed MAAP content in both buffered
    /// JSON and SSE adapters. Repeated stream observations are cumulative, so
    /// only the latest snapshot is retained; no successful output is invented.
    /// Cache-write absence and reported cache/reasoning subsets remain distinct.
    #[test]
    fn openai_chat_parse_errors_retain_reported_expense_without_accepting_output() {
        let usage = serde_json::json!({"prompt_tokens":7,"completion_tokens":5,
            "prompt_tokens_details":{"cached_tokens":3},"completion_tokens_details":{"reasoning_tokens":2}});
        let body = serde_json::json!({"model":"test", "choices":[{"message":{"role":"assistant","content":"{"}}],"usage":usage}).to_string();
        let stream = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            serde_json::json!({"model":"test","choices":[{"delta":{"content":"{"}}],"usage":{"prompt_tokens":4,"completion_tokens":3}}),
            serde_json::json!({"model":"test","choices":[{"delta":{},"finish_reason":"stop"}],"usage":usage})
        );
        for (body, streaming) in [(body, false), (stream, true)] {
            let error = parse_openai_chat_completions_http_response(
                ProviderHttpResponse {
                    status_code: 200,
                    headers: Default::default(),
                    body,
                },
                &parsing_request(),
                "compatible",
                streaming,
            )
            .unwrap_err();
            let observed = error.provider_incurred_usage();
            assert_eq!(observed.input_tokens, 7);
            assert_eq!(observed.output_tokens, 5);
            assert_eq!(observed.reasoning_tokens, 2);
            assert_eq!(observed.cached_input_tokens, Some(3));
            assert_eq!(observed.cache_write_input_tokens, None);
        }
    }

    /// The incremental trait adapter retains an earlier usage observation when
    /// a later SSE event is malformed. Error feedback stays an error and is not
    /// normalized into a fake accepted batch or a zero-expense success.
    #[test]
    fn openai_chat_incremental_error_keeps_prior_counter_observation() {
        let mut decoder = OpenAiChatCompletionsStreamDecoder::default();
        ChatCompletionsStreamDecoder::push_event(&mut decoder, &SseEvent {
            name:None, data:serde_json::json!({"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":5}}).to_string(),
        }).unwrap();
        let error = ChatCompletionsStreamDecoder::push_event(
            &mut decoder,
            &SseEvent {
                name: None,
                data: "invalid-json".into(),
            },
        )
        .unwrap_err();
        assert_eq!(error.provider_incurred_usage().input_tokens, 7);
        assert_eq!(error.provider_incurred_usage().output_tokens, 5);
    }

    /// Verifies model listing derives the sibling `/models` endpoint only from
    /// the normalized trailing Chat Completions suffix, so compatible proxy
    /// base URLs that contain `/chat/completions` earlier in the path are not
    /// corrupted by a global string replacement.
    #[test]
    fn openai_chat_completions_models_request_only_rewrites_trailing_suffix() {
        let request = build_openai_chat_completions_models_http_request(
            Some("test-key"),
            "https://proxy.example/custom/chat/completions-proxy/v1",
            30_000,
        )
        .unwrap();

        assert_eq!(request.method, "GET");
        assert_eq!(
            request.url,
            "https://proxy.example/custom/chat/completions-proxy/v1/models"
        );
        assert_eq!(
            request.headers.get("Authorization").map(String::as_str),
            Some("Bearer test-key")
        );
    }
}
