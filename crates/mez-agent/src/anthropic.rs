//! Provider-independent Anthropic Messages request and response policy.
//!
//! This module owns non-secret option parsing, deterministic Messages API JSON
//! construction, JSON/SSE response parsing, content interpretation,
//! diagnostics, token usage, and stop-reason recovery. Product adapters retain
//! credentials, HTTP metadata, transport, quota attachment, and error
//! projection.

use crate::context::ContextPlacement;
use crate::{
    MAAP_ACTION_BATCH_TOOL_NAME, MAAP_ACTION_BATCH_TOOL_NAME as OPENAI_MAAP_FUNCTION_TOOL_NAME,
    MaapBatch, ModelMessageRole, ModelRequest, ModelTokenUsage, ProviderEndpointError,
    ProviderEndpointResult, ProviderErrorKind, ProviderMalformedOutputError,
    ProviderRequestAssemblyError, ProviderRequestAssemblyResult, ProviderResponseError,
    maap_action_batch_schema, openai_maap_current_action_batch_description,
    parse_fenced_maap_action_batch_for_turn, parse_maap_action_batch_json_for_turn,
    provider_failure_event_json, provider_failure_json, provider_malformed_output_error,
};
use std::collections::BTreeMap;

/// Default Anthropic Messages API version used when options omit one.
pub const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";
/// Default Anthropic Messages API endpoint.
pub const ANTHROPIC_MESSAGES_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
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

/// One Anthropic message retained with its cache lifecycle during serialization.
struct AnthropicRenderedMessage {
    role: &'static str,
    content: String,
    placement: ContextPlacement,
}

/// Builds one Anthropic-compliant Messages API JSON body.
pub fn anthropic_messages_request_body(
    request: &ModelRequest,
    stream: bool,
    options: &AnthropicMessagesOptions,
) -> ProviderRequestAssemblyResult<String> {
    let mut system_parts = Vec::new();
    let mut rendered_messages = Vec::<AnthropicRenderedMessage>::new();
    for message in &request.messages {
        let role = match message.role {
            ModelMessageRole::System | ModelMessageRole::Developer => {
                if !message.content.is_empty() {
                    system_parts.push((message.content.clone(), message.cache_disposition()));
                }
                continue;
            }
            ModelMessageRole::Assistant => "assistant",
            ModelMessageRole::User | ModelMessageRole::Tool => "user",
        };
        if message.content.is_empty() {
            continue;
        }
        let cache_disposition = message.cache_disposition();
        if let Some(last) = rendered_messages.last_mut()
            && last.role == role
            && last.placement == cache_disposition
        {
            last.content.push_str("\n\n");
            last.content.push_str(&message.content);
            continue;
        }
        rendered_messages.push(AnthropicRenderedMessage {
            role,
            content: message.content.clone(),
            placement: cache_disposition,
        });
    }
    if rendered_messages.is_empty() {
        return Err(ProviderRequestAssemblyError::invalid_args(
            "Anthropic Messages request requires at least one user or assistant message",
        ));
    }
    let latest_immutable_message = options.prompt_caching.then(|| {
        rendered_messages
            .iter()
            .rposition(|message| message.placement == ContextPlacement::ConversationAppend)
    });
    let latest_immutable_message = latest_immutable_message.flatten();
    let messages = rendered_messages
        .into_iter()
        .enumerate()
        .map(|(index, message)| {
            let content = if Some(index) == latest_immutable_message {
                serde_json::json!([{
                    "type": "text",
                    "text": message.content,
                    "cache_control": { "type": "ephemeral" },
                }])
            } else {
                serde_json::json!(message.content)
            };
            serde_json::json!({
                "role": message.role,
                "content": content,
            })
        })
        .collect::<Vec<_>>();
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
        body["system"] = if options.prompt_caching {
            let final_static_part = system_parts
                .iter()
                .rposition(|(_, disposition)| *disposition == ContextPlacement::StablePrefix);
            serde_json::Value::Array(
                system_parts
                    .into_iter()
                    .enumerate()
                    .map(|(index, (text, _))| {
                        if Some(index) == final_static_part {
                            serde_json::json!({
                                "type": "text",
                                "text": text,
                                "cache_control": { "type": "ephemeral" },
                            })
                        } else {
                            serde_json::json!({
                                "type": "text",
                                "text": text,
                            })
                        }
                    })
                    .collect(),
            )
        } else {
            serde_json::json!(
                system_parts
                    .into_iter()
                    .map(|(text, _)| text)
                    .collect::<Vec<_>>()
                    .join("\n\n")
            )
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
fn anthropic_provider_failure_event_json(value: &serde_json::Value) -> String {
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
fn anthropic_usage_from_value(value: Option<&serde_json::Value>) -> ModelTokenUsage {
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
fn anthropic_overlay_usage(current: &mut ModelTokenUsage, value: Option<&serde_json::Value>) {
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

/// Failure returned while parsing an Anthropic response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnthropicResponseError {
    /// The provider returned an unsupported response content shape.
    Provider(ProviderResponseError),
    /// Provider-authored MAAP output was malformed.
    MalformedOutput(ProviderMalformedOutputError),
}

impl std::fmt::Display for AnthropicResponseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(error) => error.fmt(formatter),
            Self::MalformedOutput(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AnthropicResponseError {}

impl From<ProviderResponseError> for AnthropicResponseError {
    fn from(error: ProviderResponseError) -> Self {
        Self::Provider(error)
    }
}

impl From<ProviderMalformedOutputError> for AnthropicResponseError {
    fn from(error: ProviderMalformedOutputError) -> Self {
        Self::MalformedOutput(error)
    }
}

impl From<crate::SseParseError> for AnthropicResponseError {
    fn from(error: crate::SseParseError) -> Self {
        Self::Provider(error.into())
    }
}

/// Converts Anthropic content blocks into response text and an optional MAAP
/// action batch.
fn anthropic_content_to_output(
    content: &[serde_json::Value],
    turn_id: &str,
    agent_id: &str,
    requires_maap: bool,
) -> Result<(String, Option<MaapBatch>), AnthropicResponseError> {
    let mut raw_text = String::new();
    let mut maap_inputs = Vec::new();
    let mut saw_tool_use = false;

    for block in content {
        let block_type = block
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        match block_type {
            "text" => {
                raw_text.push_str(
                    block
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                );
            }
            "tool_use" => {
                saw_tool_use = true;
                if block.get("name").and_then(serde_json::Value::as_str)
                    == Some(OPENAI_MAAP_FUNCTION_TOOL_NAME)
                {
                    let input = block
                        .get("input")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    maap_inputs.push(serde_json::to_string(&input).map_err(|error| {
                        ProviderResponseError::invalid_state(format!(
                            "Anthropic tool_use input encoding failed: {error}"
                        ))
                    })?);
                }
            }
            "thinking" | "redacted_thinking" => {}
            "server_tool_use" | "server_tool_result" => {
                return Err(anthropic_unsupported_content_block_error(block).into());
            }
            _ => return Err(anthropic_unsupported_content_block_error(block).into()),
        }
    }

    let action_batch = if requires_maap {
        if maap_inputs.len() > 1 {
            return Err(anthropic_malformed_output(
                "Anthropic returned extra MAAP tool_use blocks; pack the complete MAAP batch into exactly one submit_maap_action_batch tool_use",
                &serde_json::Value::Array(content.to_vec()).to_string(),
            )
            .into());
        }
        if let Some(arguments) = maap_inputs.first() {
            Some(
                parse_maap_action_batch_json_for_turn(arguments, turn_id, agent_id)
                    .map_err(|error| anthropic_malformed_output(error.message(), arguments))?,
            )
        } else if saw_tool_use {
            return Err(anthropic_malformed_output(
                "Anthropic response did not include submit_maap_action_batch input in its tool_use block",
                &serde_json::Value::Array(content.to_vec()).to_string(),
            )
            .into());
        } else {
            anthropic_text_maap_action_batch(&raw_text, turn_id, agent_id)?
        }
    } else {
        None
    };

    let raw_text = if action_batch.is_some() && raw_text.trim().is_empty() {
        "executing".to_string()
    } else {
        raw_text
    };
    Ok((raw_text, action_batch))
}

/// Parses fallback MAAP output from Anthropic text when no native tool block
/// was returned.
fn anthropic_text_maap_action_batch(
    raw_text: &str,
    turn_id: &str,
    agent_id: &str,
) -> Result<Option<MaapBatch>, ProviderMalformedOutputError> {
    let trimmed = raw_text.trim();
    if trimmed.starts_with('{') {
        return parse_maap_action_batch_json_for_turn(trimmed, turn_id, agent_id)
            .map(Some)
            .map_err(|error| anthropic_malformed_output(error.message(), raw_text));
    }
    parse_fenced_maap_action_batch_for_turn(raw_text, turn_id, agent_id)
        .map_err(|error| anthropic_malformed_output(error.message(), raw_text))
}

/// Shapes malformed Anthropic MAAP output with the shared corrective
/// diagnostics.
fn anthropic_malformed_output(error_message: &str, raw_text: &str) -> ProviderMalformedOutputError {
    provider_malformed_output_error(ProviderErrorKind::InvalidArgs, error_message, raw_text)
}

/// Builds a structured provider failure for one unsupported Anthropic content
/// block.
fn anthropic_unsupported_content_block_error(block: &serde_json::Value) -> ProviderResponseError {
    let block_type = block
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    ProviderResponseError::invalid_state(format!(
        "Anthropic response contained unsupported content block type `{block_type}`"
    ))
    .with_provider_failure_json(anthropic_provider_failure_event_json(&serde_json::json!({
        "content_block": block
    })))
    .with_provider_raw_text(block.to_string())
}

/// Deterministic fields decoded from one Anthropic Messages response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicMessagesResponse {
    /// Provider-reported model id or the request fallback model.
    pub model: String,
    /// Visible assistant content retained for transcript and diagnostics.
    pub raw_text: String,
    /// Parsed MAAP batch when the request expected provider actions.
    pub action_batch: Option<MaapBatch>,
    /// Provider-reported token accounting.
    pub usage: ModelTokenUsage,
}

/// Selects the Anthropic response parser matching the transport mode.
pub fn parse_anthropic_messages_provider_body(
    body: &str,
    fallback_model: &str,
    stream: bool,
    turn_id: &str,
    agent_id: &str,
    requires_maap: bool,
) -> Result<AnthropicMessagesResponse, AnthropicResponseError> {
    if stream {
        parse_anthropic_messages_stream_body(body, fallback_model, turn_id, agent_id, requires_maap)
    } else {
        parse_anthropic_messages_http_body(body, fallback_model, turn_id, agent_id, requires_maap)
    }
}

/// Parses one non-streaming Anthropic Messages API body.
fn parse_anthropic_messages_http_body(
    body: &str,
    fallback_model: &str,
    turn_id: &str,
    agent_id: &str,
    requires_maap: bool,
) -> Result<AnthropicMessagesResponse, AnthropicResponseError> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|error| {
        ProviderResponseError::invalid_state(format!("Anthropic response was not JSON: {error}"))
    })?;
    if let Some(error) = anthropic_provider_error_from_value(&value, "Anthropic response") {
        return Err(error.into());
    }
    let model = value
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(fallback_model)
        .to_string();
    let content = value
        .get("content")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            ProviderResponseError::invalid_state("Anthropic response has no content array")
        })?;
    let (raw_text, action_batch) =
        anthropic_content_to_output(content, turn_id, agent_id, requires_maap)?;
    if let Some(error) = anthropic_stop_reason_response_error(
        value.get("stop_reason").and_then(serde_json::Value::as_str),
        &raw_text,
        requires_maap,
    ) {
        return Err(error.into());
    }
    Ok(AnthropicMessagesResponse {
        model,
        raw_text,
        action_batch,
        usage: anthropic_usage_from_value(value.get("usage")),
    })
}

/// Parses one streaming Anthropic Messages API SSE body.
fn parse_anthropic_messages_stream_body(
    body: &str,
    fallback_model: &str,
    turn_id: &str,
    agent_id: &str,
    requires_maap: bool,
) -> Result<AnthropicMessagesResponse, AnthropicResponseError> {
    let mut model = None;
    let mut usage = ModelTokenUsage::default();
    let mut stop_reason = None::<String>;
    let mut completed = false;
    let mut blocks = BTreeMap::<u64, AnthropicStreamContentBlock>::new();

    crate::parse_sse_events_with::<AnthropicResponseError, _>(
        body,
        "Anthropic stream response did not contain SSE data events",
        |event_name, data| {
            let data = data.trim();
            if data.is_empty() {
                return Ok(());
            }
            let value: serde_json::Value = serde_json::from_str(data).map_err(|error| {
                ProviderResponseError::invalid_state(format!(
                    "Anthropic stream event was not JSON: {error}"
                ))
            })?;
            if event_name == Some("error")
                || value.get("type").and_then(serde_json::Value::as_str) == Some("error")
            {
                return Err(
                    anthropic_provider_error_from_value(&value, "Anthropic stream error")
                        .unwrap_or_else(|| {
                            ProviderResponseError::invalid_state(
                                "Anthropic stream returned an error event",
                            )
                            .with_provider_failure_json(
                                anthropic_provider_failure_event_json(&value),
                            )
                        })
                        .into(),
                );
            }
            match value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .or(event_name)
            {
                Some("message_start") => {
                    if model.is_none() {
                        model = value
                            .pointer("/message/model")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string);
                    }
                    anthropic_overlay_usage(&mut usage, value.pointer("/message/usage"));
                }
                Some("content_block_start") => {
                    let index = value
                        .get("index")
                        .and_then(serde_json::Value::as_u64)
                        .ok_or_else(|| {
                            ProviderResponseError::invalid_state(
                                "Anthropic stream content_block_start event is missing an index",
                            )
                        })?;
                    let block = value.get("content_block").ok_or_else(|| {
                        ProviderResponseError::invalid_state(
                            "Anthropic stream content_block_start event is missing content_block",
                        )
                    })?;
                    blocks.insert(index, AnthropicStreamContentBlock::from_start(block)?);
                }
                Some("content_block_delta") => {
                    let index = value
                        .get("index")
                        .and_then(serde_json::Value::as_u64)
                        .ok_or_else(|| {
                            ProviderResponseError::invalid_state(
                                "Anthropic stream content_block_delta event is missing an index",
                            )
                        })?;
                    let delta = value.get("delta").ok_or_else(|| {
                        ProviderResponseError::invalid_state(
                            "Anthropic stream content_block_delta event is missing delta",
                        )
                    })?;
                    blocks
                        .get_mut(&index)
                        .ok_or_else(|| {
                            ProviderResponseError::invalid_state(format!(
                                "Anthropic stream delta referenced unknown content block index {index}"
                            ))
                        })?
                        .apply_delta(delta)?;
                }
                Some("content_block_stop") => {
                    let index = value
                        .get("index")
                        .and_then(serde_json::Value::as_u64)
                        .ok_or_else(|| {
                            ProviderResponseError::invalid_state(
                                "Anthropic stream content_block_stop event is missing an index",
                            )
                        })?;
                    let block = blocks.get_mut(&index).ok_or_else(|| {
                        ProviderResponseError::invalid_state(format!(
                            "Anthropic stream stop referenced unknown content block index {index}"
                        ))
                    })?;
                    block.stopped = true;
                }
                Some("message_delta") => {
                    if let Some(reason) = value
                        .pointer("/delta/stop_reason")
                        .and_then(serde_json::Value::as_str)
                    {
                        stop_reason = Some(reason.to_string());
                    }
                    anthropic_overlay_usage(&mut usage, value.get("usage"));
                }
                Some("message_stop") => completed = true,
                Some("ping") => {}
                Some(_) | None => {}
            }
            Ok(())
        },
    )?;

    let content = blocks
        .into_values()
        .map(AnthropicStreamContentBlock::finish)
        .collect::<Result<Vec<_>, _>>()?;
    let (raw_text, action_batch) =
        anthropic_content_to_output(&content, turn_id, agent_id, requires_maap)?;
    if let Some(error) =
        anthropic_stop_reason_response_error(stop_reason.as_deref(), &raw_text, requires_maap)
    {
        return Err(error.into());
    }
    if raw_text.is_empty() && action_batch.is_none() {
        return Err(ProviderResponseError::invalid_state(
            "Anthropic stream did not contain text or MAAP tool_use output",
        )
        .into());
    }
    if !completed && action_batch.is_none() {
        return Err(ProviderResponseError::invalid_state(
            "Anthropic stream closed before message_stop",
        )
        .into());
    }
    Ok(AnthropicMessagesResponse {
        model: model.unwrap_or_else(|| fallback_model.to_string()),
        raw_text,
        action_batch,
        usage,
    })
}

fn anthropic_provider_error_from_value(
    value: &serde_json::Value,
    fallback_message: &str,
) -> Option<ProviderResponseError> {
    let error = value.get("error").filter(|error| !error.is_null())?;
    let message = error
        .get("message")
        .or_else(|| value.get("message"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(fallback_message);
    Some(
        ProviderResponseError::invalid_state(message)
            .with_provider_failure_json(anthropic_provider_failure_event_json(value)),
    )
}

/// Accumulates one streaming Anthropic content block until it stops.
#[derive(Debug, Clone)]
struct AnthropicStreamContentBlock {
    kind: AnthropicStreamContentBlockKind,
    stopped: bool,
}

/// Distinguishes the supported Anthropic streaming content block shapes.
#[derive(Debug, Clone)]
enum AnthropicStreamContentBlockKind {
    Text { text: String },
    ToolUse { name: String, input_json: String },
}

impl AnthropicStreamContentBlock {
    /// Builds an accumulator from one `content_block_start` event payload.
    fn from_start(block: &serde_json::Value) -> Result<Self, AnthropicResponseError> {
        let kind = match block
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
        {
            "text" => AnthropicStreamContentBlockKind::Text {
                text: block
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            },
            "tool_use" => AnthropicStreamContentBlockKind::ToolUse {
                name: block
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input_json: block
                    .get("input")
                    .filter(|input| !input.is_null())
                    .and_then(|input| {
                        if input == &serde_json::json!({}) {
                            None
                        } else {
                            serde_json::to_string(input).ok()
                        }
                    })
                    .unwrap_or_default(),
            },
            _ => return Err(anthropic_unsupported_content_block_error(block).into()),
        };
        Ok(Self {
            kind,
            stopped: false,
        })
    }

    /// Applies one `content_block_delta` payload to the current accumulator.
    fn apply_delta(&mut self, delta: &serde_json::Value) -> Result<(), AnthropicResponseError> {
        let delta_type = delta
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        match (&mut self.kind, delta_type) {
            (AnthropicStreamContentBlockKind::Text { text }, "text_delta") => {
                text.push_str(
                    delta
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                );
                Ok(())
            }
            (AnthropicStreamContentBlockKind::ToolUse { input_json, .. }, "input_json_delta") => {
                input_json.push_str(
                    delta
                        .get("partial_json")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                );
                Ok(())
            }
            _ => Err(ProviderResponseError::invalid_state(format!(
                "Anthropic stream returned unsupported content block delta type `{delta_type}`"
            ))
            .with_provider_failure_json(anthropic_provider_failure_event_json(
                &serde_json::json!({ "delta": delta }),
            ))
            .with_provider_raw_text(delta.to_string())
            .into()),
        }
    }

    /// Finalizes one content block after stream completion.
    fn finish(self) -> Result<serde_json::Value, AnthropicResponseError> {
        match self.kind {
            AnthropicStreamContentBlockKind::Text { text } => Ok(serde_json::json!({
                "type": "text",
                "text": text,
            })),
            AnthropicStreamContentBlockKind::ToolUse { name, input_json } => {
                if !self.stopped {
                    return Err(ProviderResponseError::invalid_state(
                        "Anthropic stream closed before a tool_use block finished",
                    )
                    .into());
                }
                let input = if input_json.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str::<serde_json::Value>(&input_json).map_err(|error| {
                        anthropic_malformed_output(
                            &format!("Anthropic tool_use input JSON is malformed: {error}"),
                            &input_json,
                        )
                    })?
                };
                Ok(serde_json::json!({
                    "type": "tool_use",
                    "name": name,
                    "input": input,
                }))
            }
        }
    }
}

/// Converts an Anthropic stop reason into a typed recovery failure.
fn anthropic_stop_reason_response_error(
    stop_reason: Option<&str>,
    raw_text: &str,
    requires_maap: bool,
) -> Option<ProviderResponseError> {
    let stop_reason = stop_reason?;
    let base = serde_json::json!({
        "provider": "anthropic",
        "stop_reason": stop_reason,
        "raw_text_bytes": raw_text.len(),
    });
    match stop_reason {
        "stop_sequence" | "tool_use" => None,
        "end_turn" => requires_maap.then(|| {
            ProviderResponseError::invalid_state(
                "Anthropic Messages response ended turn before producing MAAP output",
            )
            .with_provider_failure_json(base.to_string())
            .with_provider_raw_text(raw_text)
        }),
        "max_tokens" => Some(
            ProviderResponseError::invalid_state(if requires_maap {
                "Anthropic Messages response hit max_tokens before completing MAAP output"
            } else {
                "Anthropic Messages response hit max_tokens before completing output"
            })
            .with_provider_failure_json(
                serde_json::json!({
                    "provider": "anthropic",
                    "stop_reason": "max_tokens",
                    "incomplete_details": { "reason": "max_output_tokens" },
                    "raw_text_bytes": raw_text.len()
                })
                .to_string(),
            )
            .with_provider_raw_text(raw_text),
        ),
        "model_context_window_exceeded" => Some(
            ProviderResponseError::invalid_state(
                "Anthropic Messages response exceeded the model context window",
            )
            .with_provider_failure_json(
                serde_json::json!({
                    "provider": "anthropic",
                    "stop_reason": "model_context_window_exceeded",
                    "incomplete_details": { "reason": "model_context_window_exceeded" },
                    "raw_text_bytes": raw_text.len()
                })
                .to_string(),
            )
            .with_provider_raw_text(raw_text),
        ),
        "refusal" => Some(
            ProviderResponseError::invalid_state("Anthropic Messages response ended with refusal")
                .with_provider_failure_json(base.to_string())
                .with_provider_raw_text(raw_text),
        ),
        "pause_turn" => Some(
            ProviderResponseError::invalid_state(
                "Anthropic Messages response paused the turn before completion",
            )
            .with_provider_failure_json(base.to_string())
            .with_provider_raw_text(raw_text),
        ),
        _ => Some(
            ProviderResponseError::invalid_state(format!(
                "Anthropic Messages response ended with unrecognized stop_reason `{stop_reason}`"
            ))
            .with_provider_failure_json(base.to_string())
            .with_provider_raw_text(raw_text),
        ),
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

    /// Builds a minimal Anthropic request for cache-segmentation tests.
    fn anthropic_cache_test_request(messages: Vec<crate::ModelMessage>) -> ModelRequest {
        ModelRequest {
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
            interaction_kind: crate::ModelInteractionKind::ActionExecution,
            allowed_actions: crate::AllowedActionSet::say_only(),
            stop: None,
            messages,
        }
    }

    /// Verifies volatile developer guidance remains after the static Anthropic cache breakpoint.
    ///
    /// Continuation ledgers change between provider calls. They must remain
    /// model-visible without changing the preceding cache-marked system block.
    #[test]
    fn anthropic_cache_breakpoint_precedes_volatile_developer_guidance() {
        let request = |ledger: &str| {
            anthropic_cache_test_request(vec![
                crate::ModelMessage {
                    role: ModelMessageRole::System,
                    source: crate::ContextSourceKind::System,
                    placement: crate::ContextPlacement::StablePrefix,
                    content: "stable system prompt".to_string(),
                },
                crate::ModelMessage {
                    role: ModelMessageRole::Developer,
                    source: crate::ContextSourceKind::RuntimeHint,
                    placement: crate::ContextPlacement::EphemeralTail,
                    content: format!("[progress ledger]\n{ledger}"),
                },
                crate::ModelMessage {
                    role: ModelMessageRole::User,
                    source: crate::ContextSourceKind::UserInstruction,
                    placement: crate::ContextPlacement::EphemeralTail,
                    content: "continue".to_string(),
                },
            ])
        };
        let first: serde_json::Value = serde_json::from_str(
            &anthropic_messages_request_body(
                &request("first update"),
                false,
                &AnthropicMessagesOptions::default(),
            )
            .unwrap(),
        )
        .unwrap();
        let second: serde_json::Value = serde_json::from_str(
            &anthropic_messages_request_body(
                &request("second update"),
                false,
                &AnthropicMessagesOptions::default(),
            )
            .unwrap(),
        )
        .unwrap();

        assert_eq!(first["system"][0], second["system"][0]);
        assert_eq!(first["system"][0]["text"], "stable system prompt");
        assert_eq!(
            first["system"][0]["cache_control"],
            serde_json::json!({ "type": "ephemeral" })
        );
        assert_eq!(
            first["system"][1]["text"],
            "[progress ledger]\nfirst update"
        );
        assert!(first["system"][1].get("cache_control").is_none());
        assert_eq!(
            second["system"][1]["text"],
            "[progress ledger]\nsecond update"
        );
    }

    /// Verifies Anthropic marks the latest immutable transcript turn as a second cache boundary.
    ///
    /// Historical user and assistant content should remain reusable while the
    /// current user request stays after the breakpoint as a volatile suffix.
    #[test]
    fn anthropic_cache_breakpoint_marks_latest_immutable_transcript_message() {
        let request = anthropic_cache_test_request(vec![
            crate::ModelMessage {
                role: ModelMessageRole::System,
                source: crate::ContextSourceKind::System,
                placement: crate::ContextPlacement::StablePrefix,
                content: "stable system prompt".to_string(),
            },
            crate::ModelMessage {
                role: ModelMessageRole::User,
                source: crate::ContextSourceKind::TranscriptUser,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "historical request".to_string(),
            },
            crate::ModelMessage {
                role: ModelMessageRole::Assistant,
                source: crate::ContextSourceKind::TranscriptAssistant,
                placement: crate::ContextPlacement::ConversationAppend,
                content: "historical answer".to_string(),
            },
            crate::ModelMessage {
                role: ModelMessageRole::User,
                source: crate::ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::EphemeralTail,
                content: "current request".to_string(),
            },
        ]);

        let value: serde_json::Value = serde_json::from_str(
            &anthropic_messages_request_body(&request, false, &AnthropicMessagesOptions::default())
                .unwrap(),
        )
        .unwrap();

        assert_eq!(value["messages"][0]["content"], "historical request");
        assert_eq!(
            value["messages"][1]["content"][0]["text"],
            "historical answer"
        );
        assert_eq!(
            value["messages"][1]["content"][0]["cache_control"],
            serde_json::json!({ "type": "ephemeral" })
        );
        assert_eq!(value["messages"][2]["content"], "current request");
    }

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

    /// Verifies Anthropic native `tool_use` blocks are translated into one
    /// canonical MAAP action batch and preserve execution-mode placeholder
    /// text when the provider omits visible assistant text.
    #[test]
    fn anthropic_content_blocks_parse_single_maap_tool_use_batch() {
        let content = vec![serde_json::json!({
            "type": "tool_use",
            "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
            "input": {
                "rationale": "Return the final answer now.",
                "actions": [{
                    "type": "say",
                    "status": "final",
                    "content_type": "text/plain; charset=utf-8",
                    "text": "done"
                }]
            }
        })];

        let (raw_text, action_batch) =
            anthropic_content_to_output(&content, "turn-1", "agent-1", true).unwrap();

        assert_eq!(raw_text, "executing");
        let batch = action_batch.unwrap();
        assert_eq!(batch.rationale, "Return the final answer now.");
        assert_eq!(batch.actions.len(), 1);
    }

    /// Verifies Anthropic thinking content blocks are ignored because they are
    /// model-private reasoning artifacts and do not carry MAAP-relevant text or
    /// tool input.
    #[test]
    fn anthropic_content_blocks_skip_thinking_blocks() {
        let content = vec![
            serde_json::json!({
                "type": "thinking",
                "thinking": "private chain of thought"
            }),
            serde_json::json!({
                "type": "redacted_thinking",
                "data": "opaque-redacted-payload"
            }),
            serde_json::json!({
                "type": "tool_use",
                "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                "input": {
                    "rationale": "Return the final answer now.",
                    "actions": [{
                        "type": "say",
                        "status": "final",
                        "content_type": "text/plain; charset=utf-8",
                        "text": "done"
                    }]
                }
            }),
        ];

        let (raw_text, action_batch) =
            anthropic_content_to_output(&content, "turn-1", "agent-1", true).unwrap();

        assert_eq!(raw_text, "executing");
        let batch = action_batch.unwrap();
        assert_eq!(batch.rationale, "Return the final answer now.");
        assert_eq!(batch.actions.len(), 1);
    }

    /// Verifies Anthropic server-side tool blocks remain rejected because they
    /// represent provider tool activity outside Mezzanine's MAAP action
    /// contract.
    #[test]
    fn anthropic_content_blocks_reject_server_tool_blocks() {
        let content = vec![serde_json::json!({
            "type": "server_tool_use",
            "name": "web_search"
        })];

        let error = anthropic_content_to_output(&content, "turn-1", "agent-1", true).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("unsupported content block type `server_tool_use`"),
            "{error}"
        );
    }

    /// Verifies Anthropic rejects multiple MAAP carrier `tool_use` blocks so
    /// one turn cannot smuggle extra carrier calls past the action-batch
    /// contract.
    #[test]
    fn anthropic_content_blocks_reject_extra_maap_tool_use_blocks() {
        let content = vec![
            serde_json::json!({
                "type": "tool_use",
                "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                "input": {
                    "rationale": "First.",
                    "actions": [{
                        "type": "say",
                        "status": "progress",
                        "content_type": "text/plain; charset=utf-8",
                        "text": "one"
                    }]
                }
            }),
            serde_json::json!({
                "type": "tool_use",
                "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                "input": {
                    "rationale": "Second.",
                    "actions": [{
                        "type": "say",
                        "status": "final",
                        "content_type": "text/plain; charset=utf-8",
                        "text": "two"
                    }]
                }
            }),
        ];

        let error = anthropic_content_to_output(&content, "turn-1", "agent-1", true).unwrap_err();

        assert!(error.to_string().contains("extra MAAP tool_use blocks"));
        assert!(matches!(
            error,
            AnthropicResponseError::MalformedOutput(ref error) if !error.raw_text().is_empty()
        ));
    }

    /// Verifies Anthropic streaming tool-use input JSON is accumulated across
    /// `input_json_delta` events and finalized only after the block stops.
    #[test]
    fn anthropic_stream_parses_tool_use_partial_json() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"model\":\"claude-3-7-sonnet\",\"usage\":{\"input_tokens\":12}}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"name\":\"submit_maap_action_batch\",\"input\":{}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"rationale\\\":\\\"Stream batch\\\",\\\"actions\\\":[{\\\"type\\\":\\\"say\\\",\\\"status\\\":\\\"final\\\",\\\"content_type\\\":\\\"text/plain; charset=utf-8\\\",\\\"text\\\":\\\"ok\\\"}]}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":5}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );

        let response = parse_anthropic_messages_provider_body(
            body,
            "fallback-model",
            true,
            "turn-1",
            "agent-1",
            true,
        )
        .unwrap();

        assert_eq!(response.model, "claude-3-7-sonnet");
        assert_eq!(response.raw_text, "executing");
        assert!(response.action_batch.is_some());
        assert_eq!(response.usage.input_tokens, 12);
        assert_eq!(response.usage.output_tokens, 5);
    }

    /// Verifies Anthropic output exhaustion retains the structured recovery
    /// reason and raw partial output required by the product retry classifier.
    #[test]
    fn anthropic_stop_reason_max_tokens_preserves_recovery_diagnostics() {
        let error =
            anthropic_stop_reason_response_error(Some("max_tokens"), "partial output", true)
                .unwrap();

        assert!(error.message().contains("before completing MAAP output"));
        assert_eq!(error.provider_raw_text(), Some("partial output"));
        assert!(
            error
                .provider_failure_json()
                .is_some_and(|failure| failure.contains("max_output_tokens"))
        );
    }
}
