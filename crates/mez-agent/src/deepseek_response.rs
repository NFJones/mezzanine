//! Provider-independent DeepSeek Chat Completions response parsing.
//!
//! This module owns deterministic JSON and SSE response parsing, native
//! reasoning cleanup, shim-tool MAAP translation, finish-reason recovery,
//! token accounting, and provider transcript-event capture. Product adapters
//! retain HTTP metadata, quota attachment, provider identity, and error
//! projection.

use crate::deepseek::{DeepSeekMaapShimKind, deepseek_thinking_enabled_for_request};
use crate::{
    MaapBatch, ModelRequest, ModelTokenUsage, ProviderErrorKind, ProviderMalformedOutputError,
    ProviderResponseError, ProviderTranscriptEvent, parse_chat_completions_response_envelope,
    parse_fenced_maap_action_batch_for_turn, parse_maap_action_batch_json_for_turn,
    parse_sse_events, provider_malformed_output_error,
};
use std::collections::BTreeMap;

/// Deterministic fields decoded from one DeepSeek Chat Completions body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepSeekResponse {
    /// Provider-reported model id or the request fallback model.
    pub model: String,
    /// Visible assistant content retained for transcript and diagnostics.
    pub raw_text: String,
    /// Provider-reported token accounting.
    pub usage: ModelTokenUsage,
    /// Parsed MAAP batch when the request expected provider actions.
    pub action_batch: Option<MaapBatch>,
    /// Provider-native events retained for later transcript replay.
    pub provider_transcript_events: Vec<ProviderTranscriptEvent>,
}

/// Failure returned while parsing a DeepSeek response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepSeekResponseError {
    /// The provider returned an invalid envelope or incomplete response.
    Provider(ProviderResponseError),
    /// Provider-authored MAAP output was malformed.
    MalformedOutput(ProviderMalformedOutputError),
}

impl DeepSeekResponseError {
    /// Returns the stable provider error category.
    pub fn kind(&self) -> ProviderErrorKind {
        match self {
            Self::Provider(_) => ProviderErrorKind::InvalidState,
            Self::MalformedOutput(error) => error.kind(),
        }
    }

    /// Returns the human-readable parser diagnostic.
    pub fn message(&self) -> &str {
        match self {
            Self::Provider(error) => error.message(),
            Self::MalformedOutput(error) => error.message(),
        }
    }

    /// Returns retained raw provider output when available.
    pub fn provider_raw_text(&self) -> Option<&str> {
        match self {
            Self::Provider(error) => error.provider_raw_text(),
            Self::MalformedOutput(error) => Some(error.raw_text()),
        }
    }

    /// Returns the sanitized provider failure payload when available.
    pub fn provider_failure_json(&self) -> Option<&str> {
        match self {
            Self::Provider(error) => error.provider_failure_json(),
            Self::MalformedOutput(error) => Some(error.provider_failure_json()),
        }
    }
}

impl std::fmt::Display for DeepSeekResponseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(error) => error.fmt(formatter),
            Self::MalformedOutput(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for DeepSeekResponseError {}

impl From<ProviderResponseError> for DeepSeekResponseError {
    fn from(error: ProviderResponseError) -> Self {
        Self::Provider(error)
    }
}

impl From<ProviderMalformedOutputError> for DeepSeekResponseError {
    fn from(error: ProviderMalformedOutputError) -> Self {
        Self::MalformedOutput(error)
    }
}

impl From<crate::SseParseError> for DeepSeekResponseError {
    fn from(error: crate::SseParseError) -> Self {
        Self::Provider(error.into())
    }
}

/// Result returned by DeepSeek response parsing.
pub type DeepSeekResponseResult<T> = std::result::Result<T, DeepSeekResponseError>;

/// Selects the DeepSeek response parser matching the transport mode.
pub fn parse_deepseek_chat_completions_provider_body(
    body: &str,
    request: &ModelRequest,
    stream: bool,
) -> DeepSeekResponseResult<DeepSeekResponse> {
    if stream {
        parse_deepseek_chat_completions_stream_body(body, request)
    } else {
        parse_deepseek_chat_completions_response_body(body, request)
    }
}

#[derive(Debug)]
struct DeepSeekParseError {
    message: String,
}

impl DeepSeekParseError {
    fn invalid_args(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn message(&self) -> &str {
        &self.message
    }
}

fn deepseek_malformed_output(error_message: &str, raw_text: &str) -> ProviderMalformedOutputError {
    provider_malformed_output_error(ProviderErrorKind::InvalidArgs, error_message, raw_text)
}

/// Parses a DeepSeek Chat Completions non-streaming response body.
fn parse_deepseek_chat_completions_response_body(
    body: &str,
    request: &ModelRequest,
) -> DeepSeekResponseResult<DeepSeekResponse> {
    let envelope = parse_chat_completions_response_envelope(body, &request.model, "DeepSeek")?;
    let finish_reason = envelope.finish_reason.as_deref();
    let message = &envelope.message;
    let raw_text = message
        .get("content")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_default();
    let raw_text = if deepseek_thinking_enabled_for_request(request)
        || request.model.to_ascii_lowercase().contains("r1")
    {
        strip_think_tags(&raw_text)
    } else {
        raw_text
    };
    let reasoning_content = message
        .get("reasoning_content")
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let raw_text = if raw_text.is_empty() {
        if message.get("tool_calls").is_some() {
            "executing".to_string()
        } else {
            "(empty)".to_string()
        }
    } else {
        raw_text
    };
    let provider_transcript_events =
        deepseek_provider_transcript_events_for_message(message, reasoning_content);
    let action_batch = if deepseek_request_requires_maap(request) {
        match parse_deepseek_maap_action_batch(message, &raw_text, request) {
            Ok(action_batch) => action_batch,
            Err(error) => {
                return Err(deepseek_completion_finish_reason_error(
                    finish_reason,
                    &raw_text,
                    Some(&error),
                    request,
                )
                .map(Into::into)
                .unwrap_or_else(|| error.into()));
            }
        }
    } else {
        None
    };
    if action_batch.is_none()
        && let Some(error) =
            deepseek_completion_finish_reason_error(finish_reason, &raw_text, None, request)
    {
        return Err(error.into());
    }
    let usage = envelope
        .root
        .get("usage")
        .map(parse_deepseek_usage)
        .unwrap_or_default();
    Ok(DeepSeekResponse {
        model: envelope.model,
        raw_text,
        usage,
        action_batch,
        provider_transcript_events,
    })
}

/// Parses a DeepSeek MAAP action batch from either function-call arguments or
/// a content fallback.
///
/// DeepSeek should normally return the negotiated MAAP tool call. The content
/// fallbacks keep the adapter compatible with proxies or model variants that
/// return compact JSON or a fenced MAAP block despite being asked for a tool.
fn parse_deepseek_maap_action_batch(
    message: &serde_json::Value,
    raw_text: &str,
    request: &ModelRequest,
) -> std::result::Result<Option<MaapBatch>, ProviderMalformedOutputError> {
    if let Some(tool_calls) = message
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
        .filter(|tool_calls| !tool_calls.is_empty())
    {
        return parse_deepseek_maap_tool_calls(tool_calls, request);
    }
    parse_deepseek_content_maap_action_batch(raw_text, request)
}

/// Parses a DeepSeek MAAP batch from provider-native function calls.
fn parse_deepseek_maap_tool_calls(
    tool_calls: &[serde_json::Value],
    request: &ModelRequest,
) -> std::result::Result<Option<MaapBatch>, ProviderMalformedOutputError> {
    let recognized_calls = tool_calls
        .iter()
        .filter_map(|call| {
            let tool_name = call
                .pointer("/function/name")
                .and_then(serde_json::Value::as_str)?;
            Some((
                call,
                DeepSeekMaapShimKind::from_tool_name(tool_name)?,
                tool_name,
            ))
        })
        .collect::<Vec<_>>();
    let Some((maap_call, shim_kind, tool_name)) = recognized_calls.first().copied() else {
        return Ok(None);
    };
    if recognized_calls.len() != 1 || tool_calls.len() != 1 {
        return Err(deepseek_malformed_output(
            "DeepSeek returned extra tool calls; pack the complete MAAP batch into exactly one active function call",
            &serde_json::Value::Array(tool_calls.to_vec()).to_string(),
        ));
    }
    let missing_arguments_raw_text = maap_call.to_string();
    let arguments = maap_call
        .pointer("/function/arguments")
        .and_then(serde_json::Value::as_str)
        .filter(|arguments| !arguments.trim().is_empty())
        .ok_or_else(|| {
            deepseek_malformed_output(
                &format!("DeepSeek tool call {tool_name} did not include JSON arguments"),
                &missing_arguments_raw_text,
            )
        })?;
    let batch_json = deepseek_shim_arguments_to_maap_json(arguments, shim_kind)
        .map_err(|error| deepseek_malformed_output(error.message(), arguments))?;
    parse_maap_action_batch_json_for_turn(&batch_json, &request.turn_id, &request.agent_id)
        .map(Some)
        .map_err(|error| deepseek_malformed_output(error.message(), &batch_json))
}

/// Translates DeepSeek shim arguments into canonical compact MAAP batch JSON.
fn deepseek_shim_arguments_to_maap_json(
    arguments: &str,
    shim_kind: DeepSeekMaapShimKind,
) -> std::result::Result<String, DeepSeekParseError> {
    if shim_kind == DeepSeekMaapShimKind::ActionDispatch {
        return Ok(arguments.to_string());
    }
    let value = serde_json::from_str::<serde_json::Value>(arguments).map_err(|error| {
        DeepSeekParseError::invalid_args(format!(
            "DeepSeek shim arguments are invalid JSON: {error}"
        ))
    })?;
    let object = value.as_object().ok_or_else(|| {
        DeepSeekParseError::invalid_args("DeepSeek shim arguments must be a JSON object")
    })?;
    let rationale = required_deepseek_shim_string(object, "rationale")?;
    let action = match shim_kind {
        DeepSeekMaapShimKind::CapabilityDecision => serde_json::json!({
            "type": "request_capability",
            "capability": required_deepseek_shim_string(object, "capability")?,
            "reason": required_deepseek_shim_string(object, "reason")?
        }),
        DeepSeekMaapShimKind::RespondOnly => serde_json::json!({
            "type": "say",
            "status": required_deepseek_shim_string(object, "status")?,
            "content_type": object
                .get("content_type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("text/plain; charset=utf-8"),
            "text": required_deepseek_shim_string(object, "text")?
        }),
        DeepSeekMaapShimKind::ActionDispatch => unreachable!("handled above"),
    };
    Ok(serde_json::json!({
        "rationale": rationale,
        "actions": [action]
    })
    .to_string())
}

/// Returns one required DeepSeek shim string argument.
fn required_deepseek_shim_string<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> std::result::Result<&'a str, DeepSeekParseError> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            DeepSeekParseError::invalid_args(format!("DeepSeek shim field {field} is required"))
        })
}

/// Parses a DeepSeek content fallback when no MAAP tool call is present.
fn parse_deepseek_content_maap_action_batch(
    raw_text: &str,
    request: &ModelRequest,
) -> std::result::Result<Option<MaapBatch>, ProviderMalformedOutputError> {
    let trimmed = raw_text.trim();
    if trimmed.starts_with('{') {
        return parse_maap_action_batch_json_for_turn(trimmed, &request.turn_id, &request.agent_id)
            .map(Some)
            .map_err(|error| deepseek_malformed_output(error.message(), raw_text));
    }
    parse_fenced_maap_action_batch_for_turn(raw_text, &request.turn_id, &request.agent_id)
        .map_err(|error| deepseek_malformed_output(error.message(), raw_text))
}

/// Returns whether this DeepSeek request must produce a provider action batch.
pub fn deepseek_request_requires_maap(request: &ModelRequest) -> bool {
    request.interaction_kind.expects_maap_batch() && !request.allowed_actions.actions.is_empty()
}

/// Converts terminal DeepSeek finish reasons into runtime-recoverable errors.
fn deepseek_completion_finish_reason_error(
    finish_reason: Option<&str>,
    raw_text: &str,
    parse_error: Option<&ProviderMalformedOutputError>,
    request: &ModelRequest,
) -> Option<ProviderResponseError> {
    if !deepseek_request_requires_maap(request) {
        return None;
    }
    if finish_reason != Some("length") {
        return None;
    }
    let detail = parse_error
        .map(|error| format!(": {}", error.message()))
        .unwrap_or_default();
    let provider_raw_text = parse_error
        .map(ProviderMalformedOutputError::raw_text)
        .unwrap_or(raw_text)
        .to_string();
    Some(
        ProviderResponseError::invalid_state(format!(
            "DeepSeek Chat Completions response hit max_output_tokens before completing MAAP output{detail}"
        ))
        .with_provider_failure_json(
            serde_json::json!({
                "provider": "deepseek",
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

/// Captures DeepSeek-native assistant tool-call metadata for transcript replay.
fn deepseek_provider_transcript_events_for_message(
    message: &serde_json::Value,
    reasoning_content: Option<String>,
) -> Vec<ProviderTranscriptEvent> {
    let Some(tool_calls) = message
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
        .filter(|tool_calls| !tool_calls.is_empty())
    else {
        return Vec::new();
    };
    vec![ProviderTranscriptEvent::DeepSeekAssistantToolCall {
        content: message
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        reasoning_content,
        tool_calls: tool_calls.clone(),
    }]
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
        reasoning_tokens: deepseek_reasoning_tokens_from_usage(usage),
        cached_input_tokens: usage
            .get("prompt_cache_hit_tokens")
            .and_then(serde_json::Value::as_u64),
        cache_write_input_tokens: None,
    }
}

/// Extracts DeepSeek reasoning token usage from the documented nested shape.
fn deepseek_reasoning_tokens_from_usage(usage: &serde_json::Value) -> u64 {
    usage
        .pointer("/completion_tokens_details/reasoning_tokens")
        .or_else(|| usage.get("reasoning_tokens"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

/// Accumulates one DeepSeek streaming tool-call delta across SSE events.
#[derive(Debug, Default)]
struct DeepSeekStreamToolCall {
    id: String,
    function_name: String,
    arguments: String,
}

/// Parses a DeepSeek Chat Completions streaming (SSE) response body.
///
/// Accumulates content text, reasoning content, and tool-call argument
/// deltas across SSE events. When the stream includes a MAAP function
/// call the accumulated arguments are parsed into an action batch.
fn parse_deepseek_chat_completions_stream_body(
    body: &str,
    request: &ModelRequest,
) -> DeepSeekResponseResult<DeepSeekResponse> {
    let strip_think = deepseek_thinking_enabled_for_request(request)
        || request.model.to_ascii_lowercase().contains("r1");
    let mut text_content = String::new();
    let mut reasoning_content = String::new();
    let mut tool_calls: BTreeMap<u64, DeepSeekStreamToolCall> = BTreeMap::new();
    let mut model: Option<String> = None;
    let mut usage = ModelTokenUsage::default();
    let mut finish_reason: Option<String> = None;

    let events = parse_sse_events(
        body,
        "DeepSeek stream response did not contain SSE data events",
    )?;
    for event in events {
        let data = event.data.trim();
        if data == "[DONE]" || data.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };

        if model.is_none() {
            model = event
                .get("model")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
        }
        if let Some(u) = event.get("usage") {
            usage = parse_deepseek_usage(u);
        }

        let Some(choices) = event.get("choices").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for choice in choices {
            if let Some(reason) = choice
                .get("finish_reason")
                .and_then(serde_json::Value::as_str)
            {
                finish_reason = Some(reason.to_string());
            }
            let Some(delta) = choice.get("delta") else {
                continue;
            };
            if let Some(content) = delta.get("content").and_then(serde_json::Value::as_str) {
                text_content.push_str(content);
            }
            if let Some(reasoning) = delta
                .get("reasoning_content")
                .and_then(serde_json::Value::as_str)
            {
                reasoning_content.push_str(reasoning);
            }
            if let Some(tool_deltas) = delta
                .get("tool_calls")
                .and_then(serde_json::Value::as_array)
            {
                for tool_delta in tool_deltas {
                    let index = tool_delta
                        .get("index")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    let acc = tool_calls.entry(index).or_default();
                    if let Some(id) = tool_delta.get("id").and_then(serde_json::Value::as_str) {
                        acc.id = id.to_string();
                    }
                    if let Some(func) = tool_delta.get("function") {
                        if let Some(name) = func.get("name").and_then(serde_json::Value::as_str) {
                            acc.function_name = name.to_string();
                        }
                        if let Some(args) =
                            func.get("arguments").and_then(serde_json::Value::as_str)
                        {
                            acc.arguments.push_str(args);
                        }
                    }
                }
            }
        }
    }

    if strip_think {
        text_content = strip_think_tags(&text_content);
    }

    let model = model.unwrap_or_else(|| request.model.clone());

    let raw_text = if text_content.is_empty() {
        if !tool_calls.is_empty() {
            "executing".to_string()
        } else {
            "(empty)".to_string()
        }
    } else {
        text_content.clone()
    };

    let tool_calls_json: Vec<serde_json::Value> = tool_calls
        .values()
        .map(|tc| {
            serde_json::json!({
                "id": tc.id,
                "type": "function",
                "function": {
                    "name": tc.function_name,
                    "arguments": tc.arguments
                }
            })
        })
        .collect();

    let message = serde_json::json!({
        "content": text_content,
        "tool_calls": tool_calls_json
    });

    let reasoning = if reasoning_content.is_empty() {
        None
    } else {
        Some(reasoning_content)
    };
    let provider_transcript_events =
        deepseek_provider_transcript_events_for_message(&message, reasoning);

    let action_batch = if deepseek_request_requires_maap(request) {
        match parse_deepseek_maap_action_batch(&message, &raw_text, request) {
            Ok(action_batch) => action_batch,
            Err(error) => {
                return Err(deepseek_completion_finish_reason_error(
                    finish_reason.as_deref(),
                    &raw_text,
                    Some(&error),
                    request,
                )
                .map(Into::into)
                .unwrap_or_else(|| error.into()));
            }
        }
    } else {
        None
    };

    if action_batch.is_none()
        && let Some(error) = deepseek_completion_finish_reason_error(
            finish_reason.as_deref(),
            &raw_text,
            None,
            request,
        )
    {
        return Err(error.into());
    }

    Ok(DeepSeekResponse {
        model,
        raw_text,
        usage,
        action_batch,
        provider_transcript_events,
    })
}

/// Strips `<think>...</think>` tags and their content from a response string.
///
/// R1 reasoning variants wrap internal chain-of-thought in these tags. The
/// content between them is useful for verbose-mode logging but must not
/// appear in raw_text that feeds MAAP parsing or auto-sizing routing.
fn strip_think_tags(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut depth = 0u32;
    let mut tag_buf = String::new();
    for ch in text.chars() {
        tag_buf.push(ch);
        if depth > 0 {
            if tag_buf.ends_with("</think>") {
                depth = depth.saturating_sub(1);
                tag_buf.clear();
            }
        } else {
            if tag_buf.ends_with("<think>") {
                result.push_str(&tag_buf[..tag_buf.len() - "<think>".len()]);
                tag_buf.clear();
                depth = 1;
            } else if tag_buf.len() > 32 {
                result.push_str(&tag_buf);
                tag_buf.clear();
            }
        }
    }
    if depth == 0 {
        result.push_str(&tag_buf);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AllowedActionSet, MAAP_ACTION_BATCH_TOOL_NAME as OPENAI_MAAP_FUNCTION_TOOL_NAME,
        ModelInteractionKind, ModelMessage,
    };

    /// Builds a minimal DeepSeek model request for provider-shape tests.
    fn deepseek_test_request(messages: Vec<ModelMessage>) -> ModelRequest {
        ModelRequest {
            provider: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            reasoning_effort: Some("high".to_string()),
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: None,
            temperature: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: true,
            interaction_kind: ModelInteractionKind::ActionExecution,
            allowed_actions: AllowedActionSet::action_execution_base(),
            stop: None,
            messages,
        }
    }

    /// Verifies DeepSeek usage parsing follows the documented nested reasoning
    /// token shape while retaining compatibility with older flat responses.
    ///
    /// DeepSeek reports prompt cache accounting directly in `usage` and
    /// reasoning tokens under `completion_tokens_details.reasoning_tokens`.
    /// Capturing both fields keeps runtime cost and cache metrics accurate for
    /// thinking-mode sessions.
    #[test]
    fn deepseek_usage_parses_nested_reasoning_and_prompt_cache_hits() {
        let usage = parse_deepseek_usage(&serde_json::json!({
            "prompt_tokens": 100,
            "completion_tokens": 30,
            "prompt_cache_hit_tokens": 75,
            "prompt_cache_miss_tokens": 25,
            "completion_tokens_details": {
                "reasoning_tokens": 12
            }
        }));

        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 30);
        assert_eq!(usage.reasoning_tokens, 12);
        assert_eq!(usage.cached_input_tokens, Some(75));

        let flat = parse_deepseek_usage(&serde_json::json!({
            "prompt_tokens": 10,
            "completion_tokens": 4,
            "reasoning_tokens": 3
        }));
        assert_eq!(flat.reasoning_tokens, 3);
    }

    /// Verifies DeepSeek content fallbacks can still produce a valid MAAP
    /// batch when a proxy or model variant ignores the advertised function
    /// tool but returns the compact JSON object in assistant content.
    #[test]
    fn deepseek_response_parses_content_json_maap_fallback() {
        let request = deepseek_test_request(Vec::new());
        let body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": serde_json::json!({
                        "rationale": "content fallback still produced structured output",
                        "actions": [{
                            "type": "say",
                            "status": "final",
                            "text": "hello"
                        }]
                    }).to_string()
                }
            }]
        })
        .to_string();

        let response = parse_deepseek_chat_completions_response_body(&body, &request).unwrap();

        let batch = response.action_batch.unwrap();
        assert_eq!(
            batch.rationale,
            "content fallback still produced structured output"
        );
        assert!(batch.final_turn);
    }

    /// Verifies DeepSeek auto-sizing responses preserve raw JSON instead of
    /// entering MAAP fallback parsing.
    ///
    /// Auto-sizing requests deliberately have no MAAP tool surface; DeepSeek is
    /// asked for one JSON router decision in assistant content. A leading `{`
    /// in that response must not be treated as malformed MAAP because the
    /// runtime auto-sizing router parses the raw provider text itself.
    #[test]
    fn deepseek_auto_sizing_response_preserves_json_content_without_maap_parse() {
        let mut request = deepseek_test_request(Vec::new());
        request.interaction_kind = ModelInteractionKind::AutoSizing;
        request.allowed_actions = AllowedActionSet::from_actions([]);
        let router_json = serde_json::json!({
            "version": 1,
            "size": "medium",
            "reasoning_effort": "high",
            "confidence": 0.82,
            "rationale": "coding task needs a medium model"
        })
        .to_string();
        let body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": router_json
                }
            }]
        })
        .to_string();

        let response = parse_deepseek_chat_completions_response_body(&body, &request).unwrap();

        assert_eq!(response.raw_text, router_json);
        assert!(response.action_batch.is_none());
    }

    /// Verifies DeepSeek `finish_reason=length` uses output-limit recovery
    /// instead of malformed-MAAP repair.
    ///
    /// DeepSeek can cut assistant content in the middle of JSON when the
    /// completion hits `max_tokens`. Retrying with the MAAP repair prompt just
    /// asks the model to reinterpret a truncated object; the runtime already
    /// has a better output-limit recovery path that raises `max_output_tokens`
    /// and asks for one compact complete batch.
    #[test]
    fn deepseek_length_finish_reason_is_output_limit_error_for_partial_maap() {
        let request = deepseek_test_request(Vec::new());
        let partial_json = r#"{"actions":[{"type":"say","status":"blocked","text":"Need shell"}],"rationale":"need capability","thought":"partial"#;
        let body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "choices": [{
                "finish_reason": "length",
                "message": {
                    "role": "assistant",
                    "content": partial_json
                }
            }]
        })
        .to_string();

        let error = parse_deepseek_chat_completions_response_body(&body, &request).unwrap_err();

        assert_eq!(error.kind(), ProviderErrorKind::InvalidState);
        assert_eq!(
            crate::classify_provider_error_retry(
                error.kind(),
                error.message(),
                error.provider_failure_json(),
            ),
            crate::ProviderErrorRetryClass::OutputLimit
        );
        assert_eq!(error.provider_raw_text(), Some(partial_json));
        let failure_json: serde_json::Value =
            serde_json::from_str(error.provider_failure_json().unwrap()).unwrap();
        assert_eq!(failure_json["provider"], "deepseek");
        assert_eq!(failure_json["finish_reason"], "length");
        assert_eq!(
            failure_json["incomplete_details"]["reason"],
            "max_output_tokens"
        );
    }

    /// Verifies malformed DeepSeek MAAP tool arguments are rejected as
    /// repairable malformed provider output.
    ///
    /// The prior parser converted failed argument parsing into `None`, which
    /// let the runner surface a generic missing-batch failure and discarded the
    /// actual malformed arguments needed for a repair retry.
    #[test]
    fn deepseek_response_rejects_malformed_maap_tool_arguments() {
        let request = deepseek_test_request(Vec::new());
        let malformed_arguments = serde_json::json!({
            "rationale": "missing action content",
            "actions": []
        })
        .to_string();
        let body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                            "arguments": malformed_arguments
                        }
                    }]
                }
            }]
        })
        .to_string();

        let error = parse_deepseek_chat_completions_response_body(&body, &request).unwrap_err();

        assert!(
            error
                .message()
                .contains("provider MAAP output is malformed"),
            "{}",
            error.message()
        );
        assert_eq!(
            error.provider_raw_text(),
            Some(malformed_arguments.as_str())
        );
    }

    /// Verifies DeepSeek responses cannot smuggle extra tool calls.
    ///
    /// Mezzanine exposes exactly one active provider function per turn. If the
    /// provider returns two calls, accepting the first would silently discard the
    /// second and leave the transcript inconsistent with the model output.
    #[test]
    fn deepseek_response_rejects_extra_maap_tool_calls() {
        let request = deepseek_test_request(Vec::new());
        let arguments = serde_json::json!({
            "rationale": "single batch",
            "actions": [{
                "type": "say",
                "status": "final",
                "text": "done"
            }]
        })
        .to_string();
        let body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [
                        {
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                                "arguments": arguments
                            }
                        },
                        {
                            "id": "call_2",
                            "type": "function",
                            "function": {
                                "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                                "arguments": arguments
                            }
                        }
                    ]
                }
            }]
        })
        .to_string();

        let error = parse_deepseek_chat_completions_response_body(&body, &request).unwrap_err();

        assert!(
            error.message().contains("extra tool calls"),
            "{}",
            error.message()
        );
        assert!(
            error
                .provider_raw_text()
                .is_some_and(|raw| raw.contains("call_2")),
            "{:?}",
            error.provider_raw_text()
        );
    }

    /// Verifies DeepSeek thinking-mode tool-call responses retain provider
    /// native replay metadata.
    ///
    /// DeepSeek requires the assistant `reasoning_content` and `tool_calls`
    /// from thinking-mode tool-call turns to be sent again on later requests.
    /// The provider parser therefore captures that native assistant envelope
    /// alongside the MAAP batch instead of flattening it into visible text.
    #[test]
    fn deepseek_response_captures_thinking_tool_call_transcript_event() {
        let request = deepseek_test_request(Vec::new());
        let body = serde_json::json!({
            "model": "deepseek-v4-pro",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "I need to inspect the workspace first.",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                            "arguments": serde_json::json!({
                                "rationale": "inspect before editing",
                                "actions": [{
                                    "id": "a1",
                                    "type": "shell_command",
                                    "summary": "list files",
                                    "command": "ls",
                                    "rationale": "find project files"
                                }],
                                "final_turn": false
                            }).to_string()
                        }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 4
            }
        })
        .to_string();

        let response = parse_deepseek_chat_completions_response_body(&body, &request).unwrap();

        assert!(response.action_batch.is_some());
        assert_eq!(response.provider_transcript_events.len(), 1);
        let ProviderTranscriptEvent::DeepSeekAssistantToolCall {
            content,
            reasoning_content,
            tool_calls,
        } = &response.provider_transcript_events[0]
        else {
            panic!("expected DeepSeek assistant tool-call event");
        };
        assert_eq!(content, "");
        assert_eq!(
            reasoning_content.as_deref(),
            Some("I need to inspect the workspace first.")
        );
        assert_eq!(tool_calls[0]["id"], "call_1");
        assert_eq!(
            tool_calls[0]["function"]["name"],
            OPENAI_MAAP_FUNCTION_TOOL_NAME
        );
    }
}
