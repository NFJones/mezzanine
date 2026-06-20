//! Generic OpenAI-compatible Chat Completions dialect.
//!
//! This module implements the conservative OpenAI-style Chat Completions wire
//! shape used for local and third-party compatible backends. It deliberately
//! avoids DeepSeek thinking fields, DeepSeek shim function names, hidden
//! reasoning transcript replay, and DeepSeek retry policy.

use super::chat_completions::{ChatCompletionsDialect, parse_chat_completions_response_envelope};
use super::errors::provider_maap_parse_error;
use super::schema::{maap_action_batch_schema, mcp_tool_manifest_for_description};
use super::{
    MezError, ModelInteractionKind, ModelMessageRole, ModelRequest, ModelResponse, ModelTokenUsage,
    OPENAI_MAAP_FUNCTION_TOOL_NAME, ProviderHttpRequest, ProviderHttpResponse, Result,
    parse_fenced_maap_action_batch_for_turn, parse_maap_action_batch_json_for_turn,
    provider_quota_usage_from_headers, validate_non_empty,
};
use std::collections::BTreeMap;

/// Chat Completions dialect for generic OpenAI-compatible providers.
#[derive(Debug, Clone, Default)]
pub struct OpenAiChatCompletionsDialect {
    options: OpenAiChatCompletionsOptions,
}

/// Provider-level compatibility options for generic OpenAI-style chat servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OpenAiChatCompletionsOptions {
    maap_output: OpenAiMaapOutputMode,
    tool_calls: OpenAiCompatibilitySwitch,
    tool_choice: OpenAiToolChoiceMode,
    parallel_tool_calls: OpenAiCompatibilitySwitch,
    structured_output: OpenAiStructuredOutputMode,
    output_token_field: OpenAiOutputTokenField,
    maap_surface: OpenAiMaapSurfaceMode,
}

impl Default for OpenAiChatCompletionsOptions {
    fn default() -> Self {
        Self {
            maap_output: OpenAiMaapOutputMode::Auto,
            tool_calls: OpenAiCompatibilitySwitch::Auto,
            tool_choice: OpenAiToolChoiceMode::Required,
            parallel_tool_calls: OpenAiCompatibilitySwitch::Disabled,
            structured_output: OpenAiStructuredOutputMode::Auto,
            output_token_field: OpenAiOutputTokenField::MaxTokens,
            maap_surface: OpenAiMaapSurfaceMode::CanonicalBatch,
        }
    }
}

impl OpenAiChatCompletionsOptions {
    /// Parses provider-level compatibility options for local OpenAI-style APIs.
    fn from_provider_options(provider_options: &BTreeMap<String, String>) -> Result<Self> {
        let mut options = Self::default();
        if let Some(value) =
            openai_chat_provider_option(provider_options, &["maap_output", "maap_output_mode"])
        {
            options.maap_output = OpenAiMaapOutputMode::parse(&value)?;
        }
        if let Some(value) =
            openai_chat_provider_option(provider_options, &["tool_calls", "supports_tool_calls"])
        {
            options.tool_calls = OpenAiCompatibilitySwitch::parse("tool_calls", &value)?;
        }
        if let Some(value) =
            openai_chat_provider_option(provider_options, &["tool_choice", "maap_tool_choice"])
        {
            options.tool_choice = OpenAiToolChoiceMode::parse(&value)?;
        }
        if let Some(value) = openai_chat_provider_option(
            provider_options,
            &["parallel_tool_calls", "supports_parallel_tool_calls"],
        ) {
            options.parallel_tool_calls =
                OpenAiCompatibilitySwitch::parse("parallel_tool_calls", &value)?;
        }
        if let Some(value) =
            openai_chat_provider_option(provider_options, &["structured_output", "response_format"])
        {
            options.structured_output = OpenAiStructuredOutputMode::parse(&value)?;
        }
        if let Some(value) = openai_chat_provider_option(provider_options, &["output_token_field"])
        {
            options.output_token_field = OpenAiOutputTokenField::parse(&value)?;
        }
        if let Some(value) = openai_chat_provider_option(provider_options, &["maap_surface"]) {
            options.maap_surface = OpenAiMaapSurfaceMode::parse(&value)?;
        }
        Ok(options)
    }
}

/// Three-state compatibility switch used by OpenAI-compatible provider options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAiCompatibilitySwitch {
    Auto,
    Enabled,
    Disabled,
}

/// Provider-neutral MAAP output strategy for generic Chat Completions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAiMaapOutputMode {
    Auto,
    Tools,
    StructuredJson,
}

/// Tool-choice request shape for the generic MAAP tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAiToolChoiceMode {
    Named,
    Required,
    Auto,
    Disabled,
}

/// Structured-output request behavior for generic Chat Completions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAiStructuredOutputMode {
    Auto,
    JsonObject,
    JsonSchema,
    Disabled,
}

/// Output token field accepted by one compatible backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAiOutputTokenField {
    MaxTokens,
    MaxCompletionTokens,
}

/// Provider-neutral MAAP schema surface mode for generic Chat Completions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAiMaapSurfaceMode {
    CanonicalBatch,
    ContentJson,
}

impl OpenAiCompatibilitySwitch {
    /// Parses a generic three-state compatibility switch.
    fn parse(option: &str, value: &str) -> Result<Self> {
        match openai_chat_normalized_option(value).as_str() {
            "auto" => Ok(Self::Auto),
            "enabled" | "enable" | "true" | "yes" | "on" => Ok(Self::Enabled),
            "disabled" | "disable" | "false" | "no" | "off" => Ok(Self::Disabled),
            _ => Err(MezError::invalid_args(format!(
                "OpenAI-compatible provider option `{option}` must be auto, enabled, or disabled"
            ))),
        }
    }

    /// Returns true when the switch explicitly disables a feature.
    fn is_disabled(self) -> bool {
        matches!(self, Self::Disabled)
    }

    /// Returns true when the switch explicitly enables a feature.
    fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

impl OpenAiMaapOutputMode {
    /// Parses the MAAP output strategy for generic Chat Completions.
    fn parse(value: &str) -> Result<Self> {
        match openai_chat_normalized_option(value).as_str() {
            "auto" => Ok(Self::Auto),
            "tools" | "tool" | "tool_calls" | "function_tools" => Ok(Self::Tools),
            "structured_json" | "structured" | "json_schema" | "response_format" => {
                Ok(Self::StructuredJson)
            }
            _ => Err(MezError::invalid_args(
                "OpenAI-compatible provider option `maap_output` must be auto, tools, or structured_json",
            )),
        }
    }
}

impl OpenAiToolChoiceMode {
    /// Parses the generic MAAP tool-choice mode.
    fn parse(value: &str) -> Result<Self> {
        match openai_chat_normalized_option(value).as_str() {
            "named" | "forced" | "force" | "function" | "function_name" => Ok(Self::Named),
            "required" => Ok(Self::Required),
            "auto" => Ok(Self::Auto),
            "disabled" | "disable" | "none" | "omit" | "off" => Ok(Self::Disabled),
            _ => Err(MezError::invalid_args(
                "OpenAI-compatible provider option `tool_choice` must be named, required, auto, or disabled",
            )),
        }
    }

    /// Returns the OpenAI Chat Completions `tool_choice` value for this mode.
    fn request_value(self) -> Option<serde_json::Value> {
        match self {
            Self::Named => Some(serde_json::json!({
                "type": "function",
                "function": {
                    "name": OPENAI_MAAP_FUNCTION_TOOL_NAME
                }
            })),
            Self::Required => Some(serde_json::json!("required")),
            Self::Auto => Some(serde_json::json!("auto")),
            Self::Disabled => None,
        }
    }
}

impl OpenAiStructuredOutputMode {
    /// Parses the structured-output compatibility mode.
    fn parse(value: &str) -> Result<Self> {
        match openai_chat_normalized_option(value).as_str() {
            "auto" => Ok(Self::Auto),
            "json_object" | "object" => Ok(Self::JsonObject),
            "json_schema" | "schema" => Ok(Self::JsonSchema),
            "disabled" | "disable" | "none" | "off" => Ok(Self::Disabled),
            _ => Err(MezError::invalid_args(
                "OpenAI-compatible provider option `structured_output` must be auto, json_object, json_schema, or disabled",
            )),
        }
    }
}

impl OpenAiOutputTokenField {
    /// Parses the output token field accepted by the backend.
    fn parse(value: &str) -> Result<Self> {
        match openai_chat_normalized_option(value).as_str() {
            "max_tokens" | "tokens" => Ok(Self::MaxTokens),
            "max_completion_tokens" | "completion_tokens" => Ok(Self::MaxCompletionTokens),
            _ => Err(MezError::invalid_args(
                "OpenAI-compatible provider option `output_token_field` must be max_tokens or max_completion_tokens",
            )),
        }
    }
}

impl OpenAiMaapSurfaceMode {
    /// Parses the MAAP surface mode for generic Chat Completions.
    fn parse(value: &str) -> Result<Self> {
        match openai_chat_normalized_option(value).as_str() {
            "canonical_batch" | "canonical" | "batch" => Ok(Self::CanonicalBatch),
            "content_json" | "content_json_only" | "json" => Ok(Self::ContentJson),
            _ => Err(MezError::invalid_args(
                "OpenAI-compatible provider option `maap_surface` must be canonical_batch or content_json",
            )),
        }
    }
}

/// Returns a trimmed provider option value from the first supported key.
fn openai_chat_provider_option(
    provider_options: &BTreeMap<String, String>,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .find_map(|key| provider_options.get(*key))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Normalizes provider option tokens for enum parsing.
fn openai_chat_normalized_option(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
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
    stream: bool,
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
    let mut body = serde_json::json!({
        "model": request.model,
        "messages": openai_chat_completions_messages(request),
        "stream": stream,
    });
    if let Some(max_output_tokens) = request.max_output_tokens.filter(|tokens| *tokens > 0) {
        let field = match options.output_token_field {
            OpenAiOutputTokenField::MaxTokens => "max_tokens",
            OpenAiOutputTokenField::MaxCompletionTokens => "max_completion_tokens",
        };
        body[field] = serde_json::json!(max_output_tokens);
    }
    if request.interaction_kind == ModelInteractionKind::AutoSizing {
        openai_chat_apply_response_format(&mut body, request, options, false);
    } else if !request.allowed_actions.actions.is_empty() {
        match openai_chat_maap_request_mode(options) {
            OpenAiMaapRequestMode::Tools => {
                body["tools"] = serde_json::json!([openai_chat_completions_maap_tool(request)]);
                body["parallel_tool_calls"] =
                    serde_json::json!(options.parallel_tool_calls.is_enabled());
                if let Some(tool_choice) = options.tool_choice.request_value() {
                    body["tool_choice"] = tool_choice;
                }
            }
            OpenAiMaapRequestMode::StructuredJson => {
                openai_chat_apply_response_format(&mut body, request, options, true);
            }
        }
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
        body["stop"] = serde_json::json!(stop);
    }
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
    let body = serde_json::to_string(&body).map_err(|error| {
        MezError::invalid_state(format!(
            "OpenAI-compatible Chat Completions request encoding failed: {error}"
        ))
    })?;
    Ok(ProviderHttpRequest {
        method: "POST".to_string(),
        url: endpoint.to_string(),
        headers,
        body,
        timeout_ms,
        max_response_bytes: None,
    })
}

/// Request encoding selected for the current MAAP turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAiMaapRequestMode {
    Tools,
    StructuredJson,
}

/// Selects the provider-neutral MAAP request encoding for generic backends.
fn openai_chat_maap_request_mode(options: OpenAiChatCompletionsOptions) -> OpenAiMaapRequestMode {
    match options.maap_output {
        OpenAiMaapOutputMode::StructuredJson => OpenAiMaapRequestMode::StructuredJson,
        OpenAiMaapOutputMode::Tools => OpenAiMaapRequestMode::Tools,
        OpenAiMaapOutputMode::Auto => {
            if options.tool_calls.is_disabled()
                || options.maap_surface == OpenAiMaapSurfaceMode::ContentJson
            {
                OpenAiMaapRequestMode::StructuredJson
            } else {
                OpenAiMaapRequestMode::Tools
            }
        }
    }
}

/// Applies Chat Completions structured-output controls when the backend allows them.
fn openai_chat_apply_response_format(
    body: &mut serde_json::Value,
    request: &ModelRequest,
    options: OpenAiChatCompletionsOptions,
    maap_json: bool,
) {
    match options.structured_output {
        OpenAiStructuredOutputMode::Disabled => {}
        OpenAiStructuredOutputMode::Auto | OpenAiStructuredOutputMode::JsonObject => {
            body["response_format"] = serde_json::json!({"type": "json_object"});
        }
        OpenAiStructuredOutputMode::JsonSchema => {
            let schema = if maap_json {
                maap_action_batch_schema(&request.allowed_actions, &request.available_mcp_tools)
            } else {
                serde_json::json!({"type": "object"})
            };
            body["response_format"] = serde_json::json!({
                "type": "json_schema",
                "json_schema": {
                    "name": if maap_json { OPENAI_MAAP_FUNCTION_TOOL_NAME } else { "mezzanine_json" },
                    "strict": true,
                    "schema": schema
                }
            });
        }
    }
}

/// Renders Mezzanine model messages into Chat Completions message objects.
fn openai_chat_completions_messages(request: &ModelRequest) -> Vec<serde_json::Value> {
    request
        .messages
        .iter()
        .map(|message| {
            let role = match message.role {
                ModelMessageRole::System => "system",
                ModelMessageRole::Developer => "system",
                ModelMessageRole::User => "user",
                ModelMessageRole::Assistant => "assistant",
                ModelMessageRole::Tool => "tool",
            };
            serde_json::json!({
                "role": role,
                "content": message.content
            })
        })
        .collect()
}

/// Builds the canonical MAAP function tool for generic Chat Completions.
fn openai_chat_completions_maap_tool(request: &ModelRequest) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
            "description": format!(
                "Submit one validated Mezzanine MAAP action batch. Return a function call, not prose. The function call is only the transport envelope for the action batch, not a prerequisite task step; do not emit a say-only or progress batch claiming that an initial or schema-valid batch is needed before the executable action, and do not put required-function-call compliance language in rationale or thought fields. If an executable action is available and useful, put that action in this function call now. If missing information, parameters, or identifiers can be safely gathered from current context, action results, local artifacts, web results, MCP results, or another available or requestable action, request or use the relevant capability instead of asking the user. If this schema includes mcp_call and the task matches visible MCP metadata, treat mcp_call as a likely useful action in the same batch schema as other currently allowed actions. Do not use memory_search, memory_store, shell preflight, or request_capability for shell/network merely to set up a useful MCP call. Do not use memory_search or memory_store to rehydrate, preserve, or look up facts already present in current action results. {}",
                mcp_tool_manifest_for_description(&request.available_mcp_tools)
            ),
            "parameters": maap_action_batch_schema(&request.allowed_actions, &request.available_mcp_tools)
        }
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
    let models_endpoint = chat_endpoint.replace("/chat/completions", "/models");
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
        parse_openai_chat_completions_maap_action_batch(message, &raw_text, request)?
    };
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
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ => Err(MezError::invalid_state(
            "OpenAI-compatible Chat Completions response returned multiple MAAP tool calls",
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
    }
}
