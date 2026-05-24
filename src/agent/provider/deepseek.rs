//! DeepSeek Chat Completions request and response helpers.
//!
//! This module owns DeepSeek-specific request body construction, MAAP tool
//! strategy handling, response parsing, and model-list request construction.
//! Provider dispatch remains in the parent module so shared trait wiring stays
//! centralized.

use super::schema::maap_action_batch_schema;
use super::{
    AllowedActionSet, DEEPSEEK_CHAT_COMPLETIONS_ENDPOINT, DeepSeekMaapRequestStrategy, MezError,
    ModelInteractionKind, ModelMessageRole, ModelRequest, ModelResponse, ModelTokenUsage,
    OPENAI_MAAP_FUNCTION_TOOL_NAME, ProviderCapabilities, ProviderHttpRequest,
    ProviderHttpResponse, Result, parse_maap_action_batch_json_for_turn,
    provider_quota_usage_from_headers, validate_non_empty,
};
use std::collections::BTreeMap;

/// Builds a DeepSeek Chat Completions HTTP request.
pub fn build_deepseek_chat_completions_http_request(
    request: &ModelRequest,
    api_key: &str,
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

/// Builds a DeepSeek Chat Completions HTTP request with an explicit MAAP strategy.
pub(super) fn build_deepseek_chat_completions_http_request_with_strategy(
    request: &ModelRequest,
    api_key: &str,
    endpoint: &str,
    stream: bool,
    timeout_ms: u64,
    strategy: DeepSeekMaapRequestStrategy,
) -> Result<ProviderHttpRequest> {
    validate_non_empty("DeepSeek provider bearer credential", api_key)?;
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
    headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
    Ok(ProviderHttpRequest {
        method: "POST".to_string(),
        url: endpoint.to_string(),
        headers,
        body,
        timeout_ms,
        max_response_bytes: None,
    })
}

/// Builds the JSON body for a DeepSeek request with an explicit MAAP strategy.
fn deepseek_chat_completions_request_body_with_strategy(
    request: &ModelRequest,
    stream: bool,
    strategy: DeepSeekMaapRequestStrategy,
) -> Result<String> {
    let capabilities = ProviderCapabilities::for_kind("deepseek");
    let messages: Vec<serde_json::Value> = request
        .messages
        .iter()
        .map(|message| {
            let role = match message.role {
                ModelMessageRole::System => "system",
                ModelMessageRole::User => "user",
                ModelMessageRole::Assistant => "assistant",
                _ => "user",
            };
            serde_json::json!({
                "role": role,
                "content": message.content
            })
        })
        .collect();
    let mut body = serde_json::json!({
        "model": request.model,
        "messages": messages,
        "stream": stream,
    });
    if let Some(max_output_tokens) = request
        .max_output_tokens
        .filter(|tokens| *tokens > 0)
        .filter(|_| capabilities.supports_max_output_tokens)
    {
        body["max_tokens"] = serde_json::json!(max_output_tokens);
    }
    if strategy == DeepSeekMaapRequestStrategy::ForcedToolNonThinking {
        body["thinking"] = serde_json::json!({"type": "disabled"});
    } else if let Some(reasoning_effort) = request
        .reasoning_effort
        .as_deref()
        .filter(|effort| !effort.is_empty())
    {
        let deepseek_effort = deepseek_reasoning_effort(reasoning_effort);
        body["reasoning_effort"] = serde_json::json!(deepseek_effort);
        body["thinking"] = serde_json::json!({"type": "enabled"});
    }
    if capabilities.supports_tool_calls && strategy != DeepSeekMaapRequestStrategy::NoTool {
        if strategy == DeepSeekMaapRequestStrategy::ForcedToolNonThinking {
            body["tool_choice"] = deepseek_maap_tool_choice();
        }
        let maap_tool = serde_json::json!({
            "type": "function",
            "function": {
                "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                "description": deepseek_maap_tool_description(&request.allowed_actions),
                "parameters": maap_action_batch_schema(
                    &request.allowed_actions,
                    &request.available_mcp_tools
                ),
                "strict": false
            }
        });
        body["tools"] = serde_json::json!([maap_tool]);
    }
    serde_json::to_string(&body).map_err(|error| {
        MezError::invalid_state(format!(
            "DeepSeek Chat Completions request encoding failed: {error}"
        ))
    })
}

/// Returns the DeepSeek MAAP strategy for one model request.
///
/// Thinking mode can use function tools only when DeepSeek chooses the tool
/// itself. When reasoning is configured, Mezzanine therefore exposes the MAAP
/// tool without forcing `tool_choice` and falls back to strict non-thinking
/// mode only if DeepSeek returns prose instead of a MAAP batch.
pub(super) fn deepseek_maap_request_strategy(
    request: &ModelRequest,
) -> DeepSeekMaapRequestStrategy {
    if request.interaction_kind == ModelInteractionKind::AutoSizing
        || request.allowed_actions.actions.is_empty()
    {
        return DeepSeekMaapRequestStrategy::NoTool;
    }
    if request
        .reasoning_effort
        .as_deref()
        .is_some_and(|effort| !effort.is_empty())
    {
        DeepSeekMaapRequestStrategy::AutoToolThinking
    } else {
        DeepSeekMaapRequestStrategy::ForcedToolNonThinking
    }
}

/// Returns the DeepSeek stream flag after accounting for MAAP tool strategy.
///
/// DeepSeek streaming tool-call parsing is not implemented in this adapter, so
/// MAAP tool requests use unary JSON even when the provider object was created
/// with streaming enabled.
pub(super) fn deepseek_effective_stream(
    stream: bool,
    strategy: DeepSeekMaapRequestStrategy,
) -> bool {
    stream && strategy == DeepSeekMaapRequestStrategy::NoTool
}

/// Reports whether a DeepSeek thinking request should retry strict MAAP.
pub(super) fn deepseek_should_retry_with_forced_maap(
    request: &ModelRequest,
    strategy: DeepSeekMaapRequestStrategy,
    response: &ModelResponse,
) -> bool {
    strategy == DeepSeekMaapRequestStrategy::AutoToolThinking
        && request.interaction_kind != ModelInteractionKind::AutoSizing
        && !request.allowed_actions.actions.is_empty()
        && response.action_batch.is_none()
}

/// Returns the DeepSeek tool choice that forces the MAAP function call.
///
/// # Behavior
/// DeepSeek's Chat Completions API defaults `tool_choice` to `auto` when tools
/// are present, which allows a prose answer instead of a MAAP action batch.
/// Mezzanine requires a structured action batch for every non-auto-sizing
/// provider turn. This helper is therefore reserved for strict fallback
/// requests with thinking disabled; thinking-mode MAAP requests omit
/// `tool_choice` and let DeepSeek choose the advertised MAAP tool.
fn deepseek_maap_tool_choice() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": OPENAI_MAAP_FUNCTION_TOOL_NAME
        }
    })
}

/// Builds concise provider-facing guidance for DeepSeek's single MAAP tool.
fn deepseek_maap_tool_description(allowed_actions: &AllowedActionSet) -> String {
    format!(
        "Submit exactly one MAAP/1 action batch through this function. Current allowed action types: {}. Use only the action objects in this function schema. Do not answer this Mezzanine agent turn in prose outside the function call. If the needed action is absent and request_capability is available, request that capability instead of answering in prose.",
        allowed_actions.action_type_names().join(",")
    )
}

/// Maps Mezzanine reasoning effort levels to DeepSeek-supported values.
fn deepseek_reasoning_effort(effort: &str) -> &'static str {
    match effort {
        "low" | "medium" | "high" => "high",
        "xhigh" | "max" => "max",
        _ => "high",
    }
}

/// Parses one successful DeepSeek HTTP response into a model response.
pub(super) fn parse_deepseek_chat_completions_http_response(
    response: ProviderHttpResponse,
    request: &ModelRequest,
    provider_id: &str,
    stream: bool,
) -> Result<ModelResponse> {
    let ProviderHttpResponse { headers, body, .. } = response;
    if stream {
        let actions = parse_deepseek_chat_completions_stream_body(&body, request)?;
        return Ok(ModelResponse {
            provider: provider_id.to_string(),
            model: request.model.clone(),
            raw_text: actions,
            usage: Default::default(),
            quota_usage: provider_quota_usage_from_headers(&headers),
            action_batch: None,
        });
    }
    let mut parsed = parse_deepseek_chat_completions_response_body(&body, request)?;
    parsed.provider = provider_id.to_string();
    parsed.quota_usage = provider_quota_usage_from_headers(&headers);
    Ok(parsed)
}

/// Parses a DeepSeek Chat Completions non-streaming response body.
fn parse_deepseek_chat_completions_response_body(
    body: &str,
    request: &ModelRequest,
) -> Result<ModelResponse> {
    let root: serde_json::Value = serde_json::from_str(body).map_err(|error| {
        MezError::invalid_state(format!(
            "DeepSeek Chat Completions response body is invalid JSON: {error}"
        ))
    })?;
    let model = root
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&request.model)
        .to_string();
    let choices = root
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            MezError::invalid_state("DeepSeek Chat Completions response has no choices array")
        })?;
    let first_choice = choices.first().ok_or_else(|| {
        MezError::invalid_state("DeepSeek Chat Completions response has empty choices array")
    })?;
    let message = first_choice.get("message").ok_or_else(|| {
        MezError::invalid_state("DeepSeek Chat Completions choice has no message")
    })?;
    let raw_text = message
        .get("content")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_default();
    let reasoning_content = message
        .get("reasoning_content")
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let raw_text = if raw_text.is_empty() {
        reasoning_content.clone().unwrap_or_else(|| {
            if message.get("tool_calls").is_some() {
                "executing".to_string()
            } else {
                "(empty)".to_string()
            }
        })
    } else {
        raw_text
    };
    let action_batch = if let Some(parsed) = message
        .get("tool_calls")
        .and_then(|tool_calls| tool_calls.as_array())
        .filter(|tool_calls| !tool_calls.is_empty())
    {
        let maap_json = parsed
            .iter()
            .find_map(|call| {
                let name = call.get("function")?.get("name")?.as_str()?;
                if name == OPENAI_MAAP_FUNCTION_TOOL_NAME {
                    call.get("function")?.get("arguments")?.as_str()
                } else {
                    None
                }
            })
            .unwrap_or("");
        if maap_json.is_empty() {
            None
        } else {
            parse_maap_action_batch_json_for_turn(maap_json, &request.turn_id, &request.agent_id)
                .ok()
        }
    } else {
        None
    };
    let usage = root
        .get("usage")
        .map(parse_deepseek_usage)
        .unwrap_or_default();
    Ok(ModelResponse {
        provider: request.provider.clone(),
        model,
        raw_text,
        usage,
        quota_usage: Vec::new(),
        action_batch,
    })
}

/// Parses usage statistics from a DeepSeek Chat Completions response.
fn parse_deepseek_usage(usage: &serde_json::Value) -> ModelTokenUsage {
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
            .get("reasoning_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        cached_input_tokens: usage
            .get("prompt_cache_hit_tokens")
            .and_then(serde_json::Value::as_u64),
    }
}

/// Parses a DeepSeek Chat Completions streaming (SSE) response body.
fn parse_deepseek_chat_completions_stream_body(
    body: &str,
    _request: &ModelRequest,
) -> Result<String> {
    let mut text_content = String::new();
    for line in body.lines() {
        let data = line.strip_prefix("data: ").unwrap_or(line);
        if data == "[DONE]" || data.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        if let Some(choices) = event.get("choices").and_then(serde_json::Value::as_array) {
            for choice in choices {
                if let Some(delta) = choice.get("delta")
                    && let Some(content) = delta.get("content").and_then(serde_json::Value::as_str)
                {
                    text_content.push_str(content);
                }
            }
        }
    }
    Ok(text_content)
}

/// Builds a DeepSeek models listing HTTP request.
pub(super) fn build_deepseek_models_http_request(
    api_key: &str,
    chat_endpoint: &str,
    timeout_ms: u64,
) -> Result<ProviderHttpRequest> {
    validate_non_empty("DeepSeek model listing credential", api_key)?;
    let models_endpoint = chat_endpoint.replace("/chat/completions", "/models");
    let mut headers = BTreeMap::new();
    headers.insert("Accept".to_string(), "application/json".to_string());
    headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
    Ok(ProviderHttpRequest {
        method: "GET".to_string(),
        url: if models_endpoint == chat_endpoint {
            DEEPSEEK_CHAT_COMPLETIONS_ENDPOINT.replace("/chat/completions", "/models")
        } else {
            models_endpoint
        },
        headers,
        body: String::new(),
        timeout_ms,
        max_response_bytes: None,
    })
}
