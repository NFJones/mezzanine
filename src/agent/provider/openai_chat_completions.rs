//! Generic OpenAI-compatible Chat Completions dialect.
//!
//! This module implements the conservative OpenAI-style Chat Completions wire
//! shape used for local and third-party compatible backends. It deliberately
//! avoids DeepSeek thinking fields, DeepSeek shim function names, hidden
//! reasoning transcript replay, and DeepSeek retry policy.

use super::chat_completions::{ChatCompletionsDialect, parse_chat_completions_response_envelope};
use super::errors::provider_maap_parse_error;
use super::{
    MezError, ModelInteractionKind, ModelRequest, ModelResponse, ModelTokenUsage,
    OPENAI_MAAP_FUNCTION_TOOL_NAME, ProviderHttpRequest, ProviderHttpResponse, Result,
    openai_models_endpoint_for_responses_endpoint, parse_fenced_maap_action_batch_for_turn,
    parse_maap_action_batch_json_for_turn, provider_quota_usage_from_headers, validate_non_empty,
};
use mez_agent::{OpenAiChatCompletionsOptions, openai_chat_completions_request_body};
use std::collections::BTreeMap;

/// Chat Completions dialect for generic OpenAI-compatible providers.
#[derive(Debug, Clone, Default)]
pub struct OpenAiChatCompletionsDialect {
    options: OpenAiChatCompletionsOptions,
}

impl OpenAiChatCompletionsDialect {
    /// Builds a dialect from non-secret provider compatibility options.
    pub(in crate::agent) fn from_provider_options(
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
        false
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
    let stream = false;
    let body = openai_chat_completions_request_body(request, options)?;
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
        timeout_ms,
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
    if stream {
        return Err(MezError::invalid_state(
            "OpenAI-compatible Chat Completions streaming responses are not yet supported",
        ));
    }
    let envelope =
        parse_chat_completions_response_envelope(&body, &request.model, "OpenAI-compatible")?;
    let message = &envelope.message;
    let raw_text = message
        .get("content")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_default();
    let action_batch = if request.interaction_kind == ModelInteractionKind::AutoSizing {
        None
    } else {
        match parse_openai_chat_completions_maap_action_batch(message, &raw_text, request) {
            Ok(action_batch) => action_batch,
            Err(error) => {
                if let Some(error) = openai_chat_completions_finish_reason_error(
                    envelope.finish_reason.as_deref(),
                    &raw_text,
                    Some(&error),
                    request,
                ) {
                    return Err(error);
                }
                return Err(error);
            }
        }
    };
    if let Some(error) = openai_chat_completions_finish_reason_error(
        envelope.finish_reason.as_deref(),
        &raw_text,
        None,
        request,
    ) {
        return Err(error);
    }
    Ok(ModelResponse {
        provider: provider_id.to_string(),
        model: envelope.model,
        raw_text,
        usage: openai_chat_completions_usage(&envelope.root),
        latest_request_usage: None,
        quota_usage: provider_quota_usage_from_headers(&headers),
        action_batch,
        provider_transcript_events: Vec::new(),
    })
}

/// Returns whether this OpenAI-compatible request must produce a provider action batch.
fn openai_chat_completions_request_requires_maap(request: &ModelRequest) -> bool {
    request.interaction_kind.expects_maap_batch() && !request.allowed_actions.actions.is_empty()
}

/// Converts terminal OpenAI-compatible finish reasons into runtime-recoverable errors.
fn openai_chat_completions_finish_reason_error(
    finish_reason: Option<&str>,
    raw_text: &str,
    parse_error: Option<&MezError>,
    request: &ModelRequest,
) -> Option<MezError> {
    if !openai_chat_completions_request_requires_maap(request) {
        return None;
    }
    if finish_reason != Some("length") {
        return None;
    }
    let detail = parse_error
        .map(|error| format!(": {}", error.message()))
        .unwrap_or_default();
    let provider_raw_text = parse_error
        .and_then(MezError::provider_raw_text)
        .unwrap_or(raw_text)
        .to_string();
    Some(
        MezError::invalid_state(format!(
            "OpenAI-compatible Chat Completions response hit max_output_tokens before completing MAAP output{detail}"
        ))
        .with_provider_failure_json(
            serde_json::json!({
                "provider": "openai_chat_completions",
                "finish_reason": "length",
                "incomplete_details": {
                    "reason": "max_output_tokens"
                },
                "raw_text_bytes": provider_raw_text.len()
            })
            .to_string(),
        )
        .with_provider_raw_text(provider_raw_text),
    )
}

/// Parses a MAAP batch from native tool calls or fallback text content.
fn parse_openai_chat_completions_maap_action_batch(
    message: &serde_json::Value,
    raw_text: &str,
    request: &ModelRequest,
) -> Result<Option<super::MaapBatch>> {
    if let Some(arguments) = openai_chat_completions_maap_tool_arguments(message)? {
        return parse_maap_action_batch_json_for_turn(
            &arguments,
            &request.turn_id,
            &request.agent_id,
        )
        .map(Some)
        .map_err(|error| provider_maap_parse_error(error, raw_text));
    }
    let trimmed = raw_text.trim();
    if trimmed.starts_with('{') {
        return parse_maap_action_batch_json_for_turn(trimmed, &request.turn_id, &request.agent_id)
            .map(Some)
            .map_err(|error| provider_maap_parse_error(error, raw_text));
    }
    if let Some(batch) =
        parse_fenced_maap_action_batch_for_turn(raw_text, &request.turn_id, &request.agent_id)
            .map_err(|error| provider_maap_parse_error(error, raw_text))?
    {
        return Ok(Some(batch));
    }
    if trimmed.is_empty()
        && let Some(reasoning_content) = openai_chat_completions_reasoning_content(message)
    {
        let trimmed_reasoning = reasoning_content.trim();
        if trimmed_reasoning.starts_with('{') {
            return parse_maap_action_batch_json_for_turn(
                trimmed_reasoning,
                &request.turn_id,
                &request.agent_id,
            )
            .map(Some)
            .map_err(|error| provider_maap_parse_error(error, reasoning_content));
        }
        return parse_fenced_maap_action_batch_for_turn(
            reasoning_content,
            &request.turn_id,
            &request.agent_id,
        )
        .map_err(|error| provider_maap_parse_error(error, reasoning_content));
    }
    Ok(None)
}

/// Returns provider-supplied reasoning content only for empty visible-content
/// responses so structured-output recovery stays narrowly scoped.
fn openai_chat_completions_reasoning_content(message: &serde_json::Value) -> Option<&str> {
    let content = message
        .get("content")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if !content.is_empty() {
        return None;
    }
    message
        .get("reasoning_content")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// Extracts canonical MAAP function arguments from OpenAI-style tool calls.
fn openai_chat_completions_maap_tool_arguments(
    message: &serde_json::Value,
) -> Result<Option<String>> {
    let Some(tool_calls) = message
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
    else {
        return Ok(None);
    };
    let mut matches = Vec::new();
    for tool_call in tool_calls {
        let Some(function) = tool_call.get("function") else {
            continue;
        };
        let Some(name) = function.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if name != OPENAI_MAAP_FUNCTION_TOOL_NAME {
            continue;
        }
        let arguments = match function.get("arguments") {
            Some(serde_json::Value::String(arguments)) => arguments.clone(),
            Some(arguments) => arguments.to_string(),
            None => String::new(),
        };
        matches.push(arguments);
    }
    match matches.len() {
        0 if tool_calls.is_empty() => Ok(None),
        0 => Err(provider_maap_parse_error(
            MezError::invalid_state(
                "OpenAI-compatible Chat Completions response returned non-MAAP tool calls without a MAAP action batch",
            ),
            &serde_json::Value::Array(tool_calls.clone()).to_string(),
        )),
        1 => Ok(matches.pop()),
        _ => Err(provider_maap_parse_error(
            MezError::invalid_state(
                "OpenAI-compatible Chat Completions response returned multiple MAAP tool calls",
            ),
            &serde_json::Value::Array(tool_calls.clone()).to_string(),
        )),
    }
}

/// Extracts OpenAI-compatible token usage fields from a response body.
fn openai_chat_completions_usage(root: &serde_json::Value) -> ModelTokenUsage {
    let usage = root.get("usage").unwrap_or(&serde_json::Value::Null);
    ModelTokenUsage {
        input_tokens: usage
            .get("prompt_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        output_tokens: usage
            .get("completion_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        reasoning_tokens: usage
            .get("completion_tokens_details")
            .and_then(|details| details.get("reasoning_tokens"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        cached_input_tokens: usage
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(serde_json::Value::as_u64),
        cache_write_input_tokens: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies hallucinated or otherwise unsupported OpenAI-compatible tool
    /// calls fail visibly instead of falling through to content parsing, so the
    /// runtime can report model protocol drift rather than silently accepting an
    /// empty or unrelated response body.
    #[test]
    fn openai_chat_completions_non_maap_tool_calls_are_malformed_output() {
        let message = serde_json::json!({
            "tool_calls": [
                {
                    "type": "function",
                    "function": {
                        "name": "unexpected_tool",
                        "arguments": {"value": true}
                    }
                }
            ]
        });

        let error = openai_chat_completions_maap_tool_arguments(&message).unwrap_err();

        assert!(
            error.message().contains("non-MAAP tool calls"),
            "{}",
            error.message()
        );
        assert!(
            error
                .provider_raw_text()
                .is_some_and(|raw| raw.contains("unexpected_tool")),
            "{error:?}"
        );
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
