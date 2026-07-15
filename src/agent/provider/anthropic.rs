//! Anthropic Messages provider shell and endpoint helpers.
//!
//! This module owns the Anthropic-specific provider boundary built on top of
//! the shared Chat Completions transport shell. It defines the Anthropic
//! endpoint derivation rules, provider identity defaults, and current
//! first-pass skeleton behavior for request, response, and model-catalog
//! handling while keeping the parent module responsible for facade exports and
//! auth-store construction.

use super::chat_completions::ChatCompletionsDialect;
use super::errors::provider_maap_parse_error;
use super::schema::{maap_action_batch_schema, maap_current_action_batch_description};
use super::{
    ANTHROPIC_MESSAGES_ENDPOINT, MezError, ModelRequest, ModelResponse, ModelTokenUsage,
    OPENAI_MAAP_FUNCTION_TOOL_NAME, ProviderHttpRequest, ProviderHttpResponse, Result,
    parse_fenced_maap_action_batch_for_turn, parse_maap_action_batch_json_for_turn,
    provider_quota_usage_from_headers, validate_non_empty,
};
use mez_agent::{
    parse_sse_events_with, provider_failure_event_json as openai_provider_failure_event_json,
    provider_failure_json as openai_provider_failure_json,
};
use std::collections::BTreeMap;

/// Default Anthropic Messages API version used when provider options omit one.
const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";
/// Conservative fallback output cap required by Anthropic Messages requests.
const DEFAULT_ANTHROPIC_MAX_TOKENS: usize = 4096;
/// Anthropic prompt caching is enabled by default because cache-control markers
/// only establish provider-side cache breakpoints for otherwise identical
/// request content.
const DEFAULT_ANTHROPIC_PROMPT_CACHING: bool = true;

/// Provider-level options for Anthropic Messages requests.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AnthropicMessagesOptions {
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

/// Parses a boolean-like provider option.
fn parse_bool_option(label: &str, value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" | "enabled" => Ok(true),
        "false" | "0" | "no" | "off" | "disabled" => Ok(false),
        _ => Err(MezError::invalid_args(format!(
            "{label} must be true or false"
        ))),
    }
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
        let (model, raw_text, action_batch, usage) = parse_anthropic_messages_provider_body(
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
                if !message.content.is_empty() {
                    system_parts.push(message.content.clone());
                }
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
        MezError::invalid_state(format!(
            "Anthropic Messages request encoding failed: {error}"
        ))
    })
}

/// Builds the Anthropic-native MAAP carrier tool for action turns.
fn anthropic_maap_tool(request: &ModelRequest) -> serde_json::Value {
    serde_json::json!({
        "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
        "description": maap_current_action_batch_description(request),
        "input_schema": maap_action_batch_schema(
            &request.allowed_actions,
            &request.available_mcp_tools,
        )
    })
}

/// Forces Anthropic action turns through the canonical MAAP carrier tool.
fn anthropic_maap_tool_choice() -> serde_json::Value {
    serde_json::json!({
        "type": "tool",
        "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
        "disable_parallel_tool_use": true,
    })
}

/// Returns whether this Anthropic request must produce a MAAP action batch.
fn anthropic_request_requires_maap(request: &ModelRequest) -> bool {
    request.interaction_kind.expects_maap_batch() && !request.allowed_actions.actions.is_empty()
}

/// Parses one Anthropic provider body using the transport mode selected for
/// the request.
fn parse_anthropic_messages_provider_body(
    body: &str,
    fallback_model: &str,
    stream: bool,
    turn_id: &str,
    agent_id: &str,
    requires_maap: bool,
) -> Result<(String, String, Option<super::MaapBatch>, ModelTokenUsage)> {
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
) -> Result<(String, String, Option<super::MaapBatch>, ModelTokenUsage)> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|error| {
        MezError::invalid_state(format!("Anthropic response was not JSON: {error}"))
    })?;
    if let Some(error) = anthropic_provider_error_from_value(&value, "Anthropic response") {
        return Err(error);
    }
    let model = value
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(fallback_model)
        .to_string();
    let content = value
        .get("content")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| MezError::invalid_state("Anthropic response has no content array"))?;
    let (raw_text, action_batch) =
        anthropic_content_to_output(content, turn_id, agent_id, requires_maap)?;
    if let Some(error) = anthropic_stop_reason_error(
        value.get("stop_reason").and_then(serde_json::Value::as_str),
        &raw_text,
        requires_maap,
    ) {
        return Err(error);
    }
    Ok((
        model,
        raw_text,
        action_batch,
        anthropic_usage_from_value(value.get("usage")),
    ))
}

/// Parses one streaming Anthropic Messages API SSE body.
fn parse_anthropic_messages_stream_body(
    body: &str,
    fallback_model: &str,
    turn_id: &str,
    agent_id: &str,
    requires_maap: bool,
) -> Result<(String, String, Option<super::MaapBatch>, ModelTokenUsage)> {
    let mut model = None;
    let mut usage = ModelTokenUsage::default();
    let mut stop_reason = None::<String>;
    let mut completed = false;
    let mut blocks = BTreeMap::<u64, AnthropicStreamContentBlock>::new();

    parse_sse_events_with(
        body,
        "Anthropic stream response did not contain SSE data events",
        |event_name, data| {
            let data = data.trim();
            if data.is_empty() {
                return Ok(());
            }
            let value: serde_json::Value = serde_json::from_str(data).map_err(|error| {
                MezError::invalid_state(format!("Anthropic stream event was not JSON: {error}"))
            })?;
            if event_name == Some("error")
                || value.get("type").and_then(serde_json::Value::as_str) == Some("error")
            {
                return Err(
                    anthropic_provider_error_from_value(&value, "Anthropic stream error")
                        .unwrap_or_else(|| {
                            MezError::invalid_state("Anthropic stream returned an error event")
                                .with_provider_failure_json(openai_provider_failure_event_json(
                                    &value,
                                ))
                        }),
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
                            MezError::invalid_state(
                                "Anthropic stream content_block_start event is missing an index",
                            )
                        })?;
                    let block = value.get("content_block").ok_or_else(|| {
                        MezError::invalid_state(
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
                            MezError::invalid_state(
                                "Anthropic stream content_block_delta event is missing an index",
                            )
                        })?;
                    let delta = value.get("delta").ok_or_else(|| {
                        MezError::invalid_state(
                            "Anthropic stream content_block_delta event is missing delta",
                        )
                    })?;
                    blocks
                        .get_mut(&index)
                        .ok_or_else(|| {
                            MezError::invalid_state(format!(
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
                            MezError::invalid_state(
                                "Anthropic stream content_block_stop event is missing an index",
                            )
                        })?;
                    let block = blocks.get_mut(&index).ok_or_else(|| {
                        MezError::invalid_state(format!(
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
                Some("message_stop") => {
                    completed = true;
                }
                Some("ping") => {}
                Some(_) | None => {}
            }
            Ok(())
        },
    )?;

    let content = blocks
        .into_values()
        .map(AnthropicStreamContentBlock::finish)
        .collect::<Result<Vec<_>>>()?;
    let (raw_text, action_batch) =
        anthropic_content_to_output(&content, turn_id, agent_id, requires_maap)?;
    if let Some(error) =
        anthropic_stop_reason_error(stop_reason.as_deref(), &raw_text, requires_maap)
    {
        return Err(error);
    }
    if raw_text.is_empty() && action_batch.is_none() {
        return Err(MezError::invalid_state(
            "Anthropic stream did not contain text or MAAP tool_use output",
        ));
    }
    if !completed && action_batch.is_none() {
        return Err(MezError::invalid_state(
            "Anthropic stream closed before message_stop",
        ));
    }
    Ok((
        model.unwrap_or_else(|| fallback_model.to_string()),
        raw_text,
        action_batch,
        usage,
    ))
}

/// Converts Anthropic content blocks into response text and an optional MAAP
/// action batch.
fn anthropic_content_to_output(
    content: &[serde_json::Value],
    turn_id: &str,
    agent_id: &str,
    requires_maap: bool,
) -> Result<(String, Option<super::MaapBatch>)> {
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
                        MezError::invalid_state(format!(
                            "Anthropic tool_use input encoding failed: {error}"
                        ))
                    })?);
                }
            }
            "thinking" | "redacted_thinking" => {}
            "server_tool_use" | "server_tool_result" => {
                return Err(anthropic_unsupported_content_block_error(block));
            }
            _ => return Err(anthropic_unsupported_content_block_error(block)),
        }
    }

    let action_batch = if requires_maap {
        if maap_inputs.len() > 1 {
            return Err(provider_maap_parse_error(
                MezError::invalid_args(
                    "Anthropic returned extra MAAP tool_use blocks; pack the complete MAAP batch into exactly one submit_maap_action_batch tool_use",
                ),
                &serde_json::Value::Array(content.to_vec()).to_string(),
            ));
        }
        if let Some(arguments) = maap_inputs.first() {
            Some(
                parse_maap_action_batch_json_for_turn(arguments, turn_id, agent_id)
                    .map_err(|error| provider_maap_parse_error(error, arguments))?,
            )
        } else if saw_tool_use {
            return Err(provider_maap_parse_error(
                MezError::invalid_args(
                    "Anthropic response did not include submit_maap_action_batch input in its tool_use block",
                ),
                &serde_json::Value::Array(content.to_vec()).to_string(),
            ));
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

/// Parses a fallback MAAP batch from Anthropic text content when no tool_use
/// block is present.
fn anthropic_text_maap_action_batch(
    raw_text: &str,
    turn_id: &str,
    agent_id: &str,
) -> Result<Option<super::MaapBatch>> {
    let trimmed = raw_text.trim();
    if trimmed.starts_with('{') {
        return parse_maap_action_batch_json_for_turn(trimmed, turn_id, agent_id)
            .map(Some)
            .map_err(|error| provider_maap_parse_error(error, raw_text));
    }
    parse_fenced_maap_action_batch_for_turn(raw_text, turn_id, agent_id)
        .map_err(|error| provider_maap_parse_error(error, raw_text))
}

/// Converts Anthropic stop reasons into runtime-visible incomplete or terminal
/// errors.
fn anthropic_stop_reason_error(
    stop_reason: Option<&str>,
    raw_text: &str,
    requires_maap: bool,
) -> Option<MezError> {
    let stop_reason = stop_reason?;
    let base = serde_json::json!({
        "provider": "anthropic",
        "stop_reason": stop_reason,
        "raw_text_bytes": raw_text.len(),
    });
    match stop_reason {
        "stop_sequence" | "tool_use" => None,
        "end_turn" => {
            if requires_maap {
                Some(
                    MezError::invalid_state(
                        "Anthropic Messages response ended turn before producing MAAP output",
                    )
                    .with_provider_failure_json(base.to_string())
                    .with_provider_raw_text(raw_text.to_string()),
                )
            } else {
                None
            }
        }
        "max_tokens" => Some(
            MezError::invalid_state(if requires_maap {
                "Anthropic Messages response hit max_tokens before completing MAAP output"
            } else {
                "Anthropic Messages response hit max_tokens before completing output"
            })
            .with_provider_failure_json(
                serde_json::json!({
                    "provider": "anthropic",
                    "stop_reason": "max_tokens",
                    "incomplete_details": {
                        "reason": "max_output_tokens"
                    },
                    "raw_text_bytes": raw_text.len()
                })
                .to_string(),
            )
            .with_provider_raw_text(raw_text.to_string()),
        ),
        "model_context_window_exceeded" => Some(
            MezError::invalid_state(
                "Anthropic Messages response exceeded the model context window",
            )
            .with_provider_failure_json(
                serde_json::json!({
                    "provider": "anthropic",
                    "stop_reason": "model_context_window_exceeded",
                    "incomplete_details": {
                        "reason": "model_context_window_exceeded"
                    },
                    "raw_text_bytes": raw_text.len()
                })
                .to_string(),
            )
            .with_provider_raw_text(raw_text.to_string()),
        ),
        "refusal" => Some(
            MezError::invalid_state("Anthropic Messages response ended with refusal")
                .with_provider_failure_json(base.to_string())
                .with_provider_raw_text(raw_text.to_string()),
        ),
        "pause_turn" => Some(
            MezError::invalid_state(
                "Anthropic Messages response paused the turn before completion",
            )
            .with_provider_failure_json(base.to_string())
            .with_provider_raw_text(raw_text.to_string()),
        ),
        _ => Some(
            MezError::invalid_state(format!(
                "Anthropic Messages response ended with unrecognized stop_reason `{stop_reason}`"
            ))
            .with_provider_failure_json(base.to_string())
            .with_provider_raw_text(raw_text.to_string()),
        ),
    }
}

/// Builds a sanitized error from an Anthropic response or stream event.
fn anthropic_provider_error_from_value(
    value: &serde_json::Value,
    fallback_message: &str,
) -> Option<MezError> {
    let error = value.get("error").filter(|error| !error.is_null())?;
    let message = error
        .get("message")
        .or_else(|| value.get("message"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(fallback_message);
    Some(
        MezError::invalid_state(message)
            .with_provider_failure_json(anthropic_provider_failure_event_json(value)),
    )
}

/// Builds a sanitized Anthropic failure JSON payload from one failed HTTP body.
fn anthropic_provider_failure_json(status_code: Option<u16>, body: &str) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    anthropic_failure_json_with_request_id(
        openai_provider_failure_json(status_code, body),
        parsed.as_ref(),
    )
}

/// Builds a sanitized Anthropic failure JSON payload from one parsed response
/// or stream event object.
fn anthropic_provider_failure_event_json(value: &serde_json::Value) -> String {
    anthropic_failure_json_with_request_id(openai_provider_failure_event_json(value), Some(value))
}

/// Adds an Anthropic request id to one already-sanitized failure JSON payload
/// when the provider supplied that identifier.
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

/// Returns the Anthropic request id from one parsed failure payload when present.
fn anthropic_request_id(value: Option<&serde_json::Value>) -> Option<&str> {
    let value = value?;
    value
        .pointer("/request_id")
        .or_else(|| value.pointer("/error/request_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|request_id| !request_id.trim().is_empty())
}

/// Extracts Anthropic token-usage counters from a response or stream usage
/// object.
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

/// Returns one Anthropic usage counter when present.
fn anthropic_usage_u64(value: &serde_json::Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

/// Builds a structured unsupported-content diagnostic for Anthropic blocks.
fn anthropic_unsupported_content_block_error(block: &serde_json::Value) -> MezError {
    let block_type = block
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    MezError::invalid_state(format!(
        "Anthropic response contained unsupported content block type `{block_type}`"
    ))
    .with_provider_failure_json(openai_provider_failure_event_json(&serde_json::json!({
        "content_block": block
    })))
    .with_provider_raw_text(block.to_string())
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
    fn from_start(block: &serde_json::Value) -> Result<Self> {
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
            _ => return Err(anthropic_unsupported_content_block_error(block)),
        };
        Ok(Self {
            kind,
            stopped: false,
        })
    }

    /// Applies one `content_block_delta` payload to the current accumulator.
    fn apply_delta(&mut self, delta: &serde_json::Value) -> Result<()> {
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
            _ => Err(MezError::invalid_state(format!(
                "Anthropic stream returned unsupported content block delta type `{delta_type}`"
            ))
            .with_provider_failure_json(openai_provider_failure_event_json(&serde_json::json!({
                "delta": delta
            })))
            .with_provider_raw_text(delta.to_string())),
        }
    }

    /// Finalizes one content block after stream completion.
    fn finish(self) -> Result<serde_json::Value> {
        match self.kind {
            AnthropicStreamContentBlockKind::Text { text } => Ok(serde_json::json!({
                "type": "text",
                "text": text,
            })),
            AnthropicStreamContentBlockKind::ToolUse { name, input_json } => {
                if !self.stopped {
                    return Err(MezError::invalid_state(
                        "Anthropic stream closed before a tool_use block finished",
                    ));
                }
                let input = if input_json.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str::<serde_json::Value>(&input_json).map_err(|error| {
                        provider_maap_parse_error(
                            MezError::invalid_args(format!(
                                "Anthropic tool_use input JSON is malformed: {error}"
                            )),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        AnthropicMessagesProvider, DEFAULT_PROVIDER_TIMEOUT_MS, ProviderErrorRetryClass,
        ReqwestProviderHttpTransport, provider_error_retry_class,
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

    /// Verifies Anthropic usage parsing keeps prompt-cache read and write
    /// counters distinct.
    ///
    /// Anthropic reports prompt-cache hits as `cache_read_input_tokens` and
    /// prompt-cache writes as `cache_creation_input_tokens`. The provider must
    /// preserve both counters so downstream accounting can distinguish cached
    /// reads from newly written cache tokens.
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
                .message()
                .contains("unsupported content block type `server_tool_use`"),
            "{}",
            error.message()
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

        assert!(
            error.message().contains("extra MAAP tool_use blocks"),
            "{}",
            error.message()
        );
        assert!(error.provider_raw_text().is_some());
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

        let (model, raw_text, action_batch, usage) = parse_anthropic_messages_provider_body(
            body,
            "fallback-model",
            true,
            "turn-1",
            "agent-1",
            true,
        )
        .unwrap();

        assert_eq!(model, "claude-3-7-sonnet");
        assert_eq!(raw_text, "executing");
        assert!(action_batch.is_some());
        assert_eq!(usage.input_tokens, 12);
        assert_eq!(usage.output_tokens, 5);
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

    /// Verifies Anthropic HTTP-200 stream error events classify by structured
    /// error type and preserve the provider request id for diagnostics.
    #[test]
    fn anthropic_stream_error_event_is_retryable_and_preserves_request_id() {
        let body = concat!(
            "event: error\n",
            "data: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"too many requests\"},\"request_id\":\"req_123\"}\n\n"
        );

        let error = parse_anthropic_messages_provider_body(
            body,
            "fallback-model",
            true,
            "turn-1",
            "agent-1",
            true,
        )
        .unwrap_err();

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
