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
        timeouts: mez_agent::ProviderHttpTimeouts::from_total(timeout_ms),
        max_response_bytes: None,
    })
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
        parse_sse_events_with(
            &body,
            "OpenAI-compatible Chat Completions stream contained no SSE events",
            |name, data| {
                decoder.push_event(&SseEvent {
                    name: name.map(str::to_string),
                    data: data.to_string(),
                })?;
                Ok::<(), mez_agent::OpenAiChatCompletionsResponseError>(())
            },
        )?;
        decoder.finish(request)?
    } else {
        parse_openai_chat_completions_response_body(&body, request)?
    };
    openai_chat_completions_model_response(parsed, headers, provider_id)
}

impl ChatCompletionsStreamDecoder for OpenAiChatCompletionsStreamDecoder {
    fn push_event(&mut self, event: &SseEvent) -> Result<Option<String>> {
        Ok(OpenAiChatCompletionsStreamDecoder::push_event(self, event)?)
    }

    fn finish(
        self: Box<Self>,
        headers: BTreeMap<String, String>,
        request: &ModelRequest,
        provider_id: &str,
    ) -> Result<ModelResponse> {
        let parsed = OpenAiChatCompletionsStreamDecoder::finish(*self, request)?;
        openai_chat_completions_model_response(parsed, headers, provider_id)
    }
}

/// Attaches product-owned provider identity and quota metadata to decoded output.
fn openai_chat_completions_model_response(
    parsed: OpenAiChatCompletionsResponse,
    headers: BTreeMap<String, String>,
    provider_id: &str,
) -> Result<ModelResponse> {
    Ok(ModelResponse {
        provider: provider_id.to_string(),
        model: parsed.model,
        raw_text: parsed.raw_text,
        usage: parsed.usage,
        latest_request_usage: None,
        quota_usage: provider_quota_usage_from_headers(&headers),
        action_batch: parsed.action_batch,
        provider_transcript_events: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::build_openai_chat_completions_models_http_request;

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
