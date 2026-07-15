//! Provider-independent OpenAI-compatible Chat Completions request shaping.
//!
//! This module owns compatibility-option parsing and deterministic JSON request
//! construction for local and third-party OpenAI-style backends. Credentials,
//! endpoints, HTTP headers, timeouts, transport, and product error projection
//! remain in the root provider adapter.

use crate::{
    MAAP_ACTION_BATCH_TOOL_NAME as OPENAI_MAAP_FUNCTION_TOOL_NAME, MaapBatch, ModelInteractionKind,
    ModelMessageRole, ModelRequest, ModelTokenUsage, ProviderErrorKind,
    ProviderMalformedOutputError, ProviderRequestAssemblyError, ProviderRequestAssemblyResult,
    ProviderResponseError, maap_action_batch_schema, openai_maap_current_action_batch_description,
    parse_fenced_maap_action_batch_for_turn, parse_maap_action_batch_json_for_turn,
    provider_malformed_output_error,
};
use std::collections::BTreeMap;

/// Parsed non-secret compatibility policy for an OpenAI-style chat backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenAiChatCompletionsOptions {
    maap_output: OpenAiMaapOutputMode,
    tool_calls: OpenAiCompatibilitySwitch,
    tool_choice: OpenAiToolChoiceMode,
    parallel_tool_calls: OpenAiCompatibilitySwitch,
    structured_output: OpenAiStructuredOutputMode,
    output_token_field: OpenAiOutputTokenField,
    maap_surface: OpenAiMaapSurfaceMode,
    developer_role: OpenAiDeveloperRole,
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
            developer_role: OpenAiDeveloperRole::System,
        }
    }
}

impl OpenAiChatCompletionsOptions {
    /// Parses provider-level compatibility options for local OpenAI-style APIs.
    pub fn from_provider_options(
        provider_options: &BTreeMap<String, String>,
    ) -> ProviderRequestAssemblyResult<Self> {
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
        if let Some(value) = openai_chat_provider_option(provider_options, &["developer_role"]) {
            options.developer_role = OpenAiDeveloperRole::parse(&value)?;
        }
        Ok(options)
    }
}

/// Three-state compatibility switch used by provider options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAiCompatibilitySwitch {
    Auto,
    Enabled,
    Disabled,
}

/// MAAP output strategy for generic Chat Completions.
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

/// Output-token field accepted by a compatible backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAiOutputTokenField {
    MaxTokens,
    MaxCompletionTokens,
}

/// MAAP schema surface mode for generic Chat Completions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAiMaapSurfaceMode {
    CanonicalBatch,
    ContentJson,
}

/// Wire role used for canonical developer messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAiDeveloperRole {
    System,
    Developer,
}

impl OpenAiCompatibilitySwitch {
    fn parse(option: &str, value: &str) -> ProviderRequestAssemblyResult<Self> {
        match openai_chat_normalized_option(value).as_str() {
            "auto" => Ok(Self::Auto),
            "enabled" | "enable" | "true" | "yes" | "on" => Ok(Self::Enabled),
            "disabled" | "disable" | "false" | "no" | "off" => Ok(Self::Disabled),
            _ => Err(ProviderRequestAssemblyError::invalid_args(format!(
                "OpenAI-compatible provider option `{option}` must be auto, enabled, or disabled"
            ))),
        }
    }

    fn is_disabled(self) -> bool {
        matches!(self, Self::Disabled)
    }

    fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

impl OpenAiMaapOutputMode {
    fn parse(value: &str) -> ProviderRequestAssemblyResult<Self> {
        match openai_chat_normalized_option(value).as_str() {
            "auto" => Ok(Self::Auto),
            "tools" | "tool" | "tool_calls" | "function_tools" => Ok(Self::Tools),
            "structured_json" | "structured" | "json_schema" | "response_format" => {
                Ok(Self::StructuredJson)
            }
            _ => Err(ProviderRequestAssemblyError::invalid_args(
                "OpenAI-compatible provider option `maap_output` must be auto, tools, or structured_json",
            )),
        }
    }
}

impl OpenAiToolChoiceMode {
    fn parse(value: &str) -> ProviderRequestAssemblyResult<Self> {
        match openai_chat_normalized_option(value).as_str() {
            "named" | "forced" | "force" | "function" | "function_name" => Ok(Self::Named),
            "required" => Ok(Self::Required),
            "auto" => Ok(Self::Auto),
            "disabled" | "disable" | "none" | "omit" | "off" => Ok(Self::Disabled),
            _ => Err(ProviderRequestAssemblyError::invalid_args(
                "OpenAI-compatible provider option `tool_choice` must be named, required, auto, or disabled",
            )),
        }
    }

    fn request_value(self) -> Option<serde_json::Value> {
        match self {
            Self::Named => Some(serde_json::json!({
                "type": "function",
                "function": { "name": OPENAI_MAAP_FUNCTION_TOOL_NAME }
            })),
            Self::Required => Some(serde_json::json!("required")),
            Self::Auto => Some(serde_json::json!("auto")),
            Self::Disabled => None,
        }
    }
}

impl OpenAiStructuredOutputMode {
    fn parse(value: &str) -> ProviderRequestAssemblyResult<Self> {
        match openai_chat_normalized_option(value).as_str() {
            "auto" => Ok(Self::Auto),
            "json_object" | "object" => Ok(Self::JsonObject),
            "json_schema" | "schema" => Ok(Self::JsonSchema),
            "disabled" | "disable" | "none" | "off" => Ok(Self::Disabled),
            _ => Err(ProviderRequestAssemblyError::invalid_args(
                "OpenAI-compatible provider option `structured_output` must be auto, json_object, json_schema, or disabled",
            )),
        }
    }
}

impl OpenAiOutputTokenField {
    fn parse(value: &str) -> ProviderRequestAssemblyResult<Self> {
        match openai_chat_normalized_option(value).as_str() {
            "max_tokens" | "tokens" => Ok(Self::MaxTokens),
            "max_completion_tokens" | "completion_tokens" => Ok(Self::MaxCompletionTokens),
            _ => Err(ProviderRequestAssemblyError::invalid_args(
                "OpenAI-compatible provider option `output_token_field` must be max_tokens or max_completion_tokens",
            )),
        }
    }
}

impl OpenAiMaapSurfaceMode {
    fn parse(value: &str) -> ProviderRequestAssemblyResult<Self> {
        match openai_chat_normalized_option(value).as_str() {
            "canonical_batch" | "canonical" | "batch" => Ok(Self::CanonicalBatch),
            "content_json" | "content_json_only" | "json" => Ok(Self::ContentJson),
            _ => Err(ProviderRequestAssemblyError::invalid_args(
                "OpenAI-compatible provider option `maap_surface` must be canonical_batch or content_json",
            )),
        }
    }
}

impl OpenAiDeveloperRole {
    fn parse(value: &str) -> ProviderRequestAssemblyResult<Self> {
        match openai_chat_normalized_option(value).as_str() {
            "system" => Ok(Self::System),
            "developer" => Ok(Self::Developer),
            _ => Err(ProviderRequestAssemblyError::invalid_args(
                "OpenAI-compatible provider option `developer_role` must be developer or system",
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Developer => "developer",
        }
    }
}

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

fn openai_chat_normalized_option(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

/// Encodes one canonical request as an OpenAI-compatible Chat Completions body.
pub fn openai_chat_completions_request_body(
    request: &ModelRequest,
    options: OpenAiChatCompletionsOptions,
) -> ProviderRequestAssemblyResult<String> {
    let mut body = serde_json::json!({
        "model": request.model,
        "messages": openai_chat_completions_messages(request, options.developer_role),
        "stream": false,
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
    serde_json::to_string(&body).map_err(|error| {
        ProviderRequestAssemblyError::invalid_state(format!(
            "OpenAI-compatible Chat Completions request encoding failed: {error}"
        ))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAiMaapRequestMode {
    Tools,
    StructuredJson,
}

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

fn openai_chat_completions_messages(
    request: &ModelRequest,
    developer_role: OpenAiDeveloperRole,
) -> Vec<serde_json::Value> {
    request
        .messages
        .iter()
        .map(|message| {
            let role = match message.role {
                ModelMessageRole::System => "system",
                ModelMessageRole::Developer => developer_role.as_str(),
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

fn openai_chat_completions_maap_tool(request: &ModelRequest) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
            "description": openai_maap_current_action_batch_description(request),
            "parameters": maap_action_batch_schema(
                &request.allowed_actions,
                &request.available_mcp_tools,
            )
        }
    })
}

/// Shared first-choice fields decoded from a Chat Completions response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatCompletionsResponseEnvelope {
    /// Complete parsed response root used for dialect-specific accounting.
    pub root: serde_json::Value,
    /// Response model id, falling back to the requested model when absent.
    pub model: String,
    /// First assistant message from the first response choice.
    pub message: serde_json::Value,
    /// First-choice finish reason when supplied by the provider.
    pub finish_reason: Option<String>,
}

/// Parses the shared Chat Completions JSON envelope.
///
/// Dialect-specific code remains responsible for content, tool-call, usage,
/// transcript, and finish-reason policy.
pub fn parse_chat_completions_response_envelope(
    body: &str,
    fallback_model: &str,
    provider_label: &str,
) -> Result<ChatCompletionsResponseEnvelope, ProviderResponseError> {
    let root: serde_json::Value = serde_json::from_str(body).map_err(|error| {
        ProviderResponseError::invalid_state(format!(
            "{provider_label} Chat Completions response body is invalid JSON: {error}"
        ))
    })?;
    let model = root
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(fallback_model)
        .to_string();
    let choices = root
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            ProviderResponseError::invalid_state(format!(
                "{provider_label} Chat Completions response has no choices array"
            ))
        })?;
    let first_choice = choices.first().ok_or_else(|| {
        ProviderResponseError::invalid_state(format!(
            "{provider_label} Chat Completions response has empty choices array"
        ))
    })?;
    let finish_reason = first_choice
        .get("finish_reason")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let message = first_choice.get("message").cloned().ok_or_else(|| {
        ProviderResponseError::invalid_state(format!(
            "{provider_label} Chat Completions choice has no message"
        ))
    })?;
    Ok(ChatCompletionsResponseEnvelope {
        root,
        model,
        message,
        finish_reason,
    })
}

/// Deterministic fields decoded from one compatible Chat Completions body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiChatCompletionsResponse {
    /// Provider-reported model id or the request fallback model.
    pub model: String,
    /// Visible assistant content retained for transcript and diagnostics.
    pub raw_text: String,
    /// Provider-reported token accounting.
    pub usage: ModelTokenUsage,
    /// Parsed MAAP batch when the request expected provider actions.
    pub action_batch: Option<MaapBatch>,
}

/// Failure returned while decoding a compatible Chat Completions body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenAiChatCompletionsResponseError {
    /// The response envelope or finish state was invalid.
    Provider(ProviderResponseError),
    /// Provider-authored MAAP output was malformed.
    MalformedOutput(ProviderMalformedOutputError),
}

impl std::fmt::Display for OpenAiChatCompletionsResponseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(error) => error.fmt(formatter),
            Self::MalformedOutput(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for OpenAiChatCompletionsResponseError {}

impl From<ProviderResponseError> for OpenAiChatCompletionsResponseError {
    fn from(error: ProviderResponseError) -> Self {
        Self::Provider(error)
    }
}

impl From<ProviderMalformedOutputError> for OpenAiChatCompletionsResponseError {
    fn from(error: ProviderMalformedOutputError) -> Self {
        Self::MalformedOutput(error)
    }
}

/// Parses one non-streaming generic OpenAI-compatible response body.
pub fn parse_openai_chat_completions_response_body(
    body: &str,
    request: &ModelRequest,
) -> Result<OpenAiChatCompletionsResponse, OpenAiChatCompletionsResponseError> {
    let envelope =
        parse_chat_completions_response_envelope(body, &request.model, "OpenAI-compatible")?;
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
                    return Err(error.into());
                }
                return Err(error.into());
            }
        }
    };
    if let Some(error) = openai_chat_completions_finish_reason_error(
        envelope.finish_reason.as_deref(),
        &raw_text,
        None,
        request,
    ) {
        return Err(error.into());
    }
    Ok(OpenAiChatCompletionsResponse {
        model: envelope.model,
        raw_text,
        usage: openai_chat_completions_usage(&envelope.root),
        action_batch,
    })
}

fn openai_chat_completions_request_requires_maap(request: &ModelRequest) -> bool {
    request.interaction_kind.expects_maap_batch() && !request.allowed_actions.actions.is_empty()
}

fn openai_chat_completions_finish_reason_error(
    finish_reason: Option<&str>,
    raw_text: &str,
    parse_error: Option<&ProviderMalformedOutputError>,
    request: &ModelRequest,
) -> Option<ProviderResponseError> {
    if !openai_chat_completions_request_requires_maap(request) || finish_reason != Some("length") {
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

fn parse_openai_chat_completions_maap_action_batch(
    message: &serde_json::Value,
    raw_text: &str,
    request: &ModelRequest,
) -> Result<Option<MaapBatch>, ProviderMalformedOutputError> {
    if let Some(arguments) = openai_chat_completions_maap_tool_arguments(message)? {
        return parse_maap_action_batch_json_for_turn(
            &arguments,
            &request.turn_id,
            &request.agent_id,
        )
        .map(Some)
        .map_err(|error| openai_chat_malformed_output(error.message(), raw_text));
    }
    let trimmed = raw_text.trim();
    if trimmed.starts_with('{') {
        return parse_maap_action_batch_json_for_turn(trimmed, &request.turn_id, &request.agent_id)
            .map(Some)
            .map_err(|error| openai_chat_malformed_output(error.message(), raw_text));
    }
    if let Some(batch) =
        parse_fenced_maap_action_batch_for_turn(raw_text, &request.turn_id, &request.agent_id)
            .map_err(|error| openai_chat_malformed_output(error.message(), raw_text))?
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
            .map_err(|error| openai_chat_malformed_output(error.message(), reasoning_content));
        }
        return parse_fenced_maap_action_batch_for_turn(
            reasoning_content,
            &request.turn_id,
            &request.agent_id,
        )
        .map_err(|error| openai_chat_malformed_output(error.message(), reasoning_content));
    }
    Ok(None)
}

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

fn openai_chat_completions_maap_tool_arguments(
    message: &serde_json::Value,
) -> Result<Option<String>, ProviderMalformedOutputError> {
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
        0 => Err(provider_malformed_output_error(
            ProviderErrorKind::InvalidState,
            "OpenAI-compatible Chat Completions response returned non-MAAP tool calls without a MAAP action batch",
            &serde_json::Value::Array(tool_calls.clone()).to_string(),
        )),
        1 => Ok(matches.pop()),
        _ => Err(provider_malformed_output_error(
            ProviderErrorKind::InvalidState,
            "OpenAI-compatible Chat Completions response returned multiple MAAP tool calls",
            &serde_json::Value::Array(tool_calls.clone()).to_string(),
        )),
    }
}

fn openai_chat_malformed_output(
    error_message: &str,
    raw_text: &str,
) -> ProviderMalformedOutputError {
    provider_malformed_output_error(ProviderErrorKind::InvalidArgs, error_message, raw_text)
}

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
    use crate::{AllowedActionSet, ContextSourceKind, ModelMessage};

    /// Verifies OpenAI-compatible Chat Completions can preserve the canonical
    /// developer role on modern APIs while retaining the default system-role
    /// fallback required by older compatible backends.
    #[test]
    fn openai_chat_completions_messages_support_configurable_developer_role() {
        let request = ModelRequest {
            provider: "local-openai-chat".to_string(),
            model: "local-chat-model".to_string(),
            reasoning_effort: None,
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
            issue_actions_enabled: false,
            interaction_kind: ModelInteractionKind::ActionExecution,
            allowed_actions: AllowedActionSet::say_only(),
            stop: None,
            messages: vec![ModelMessage {
                role: ModelMessageRole::Developer,
                source: ContextSourceKind::DeveloperInstruction,
                content: "Follow developer authority.".to_string(),
            }],
        };

        let default_messages =
            openai_chat_completions_messages(&request, OpenAiDeveloperRole::System);
        assert_eq!(default_messages[0]["role"], "system");

        let developer_messages =
            openai_chat_completions_messages(&request, OpenAiDeveloperRole::Developer);
        assert_eq!(developer_messages[0]["role"], "developer");
    }

    /// Verifies invalid compatibility values fail at the lower request-policy
    /// boundary with the stable invalid-argument category used by root error
    /// projection.
    #[test]
    fn openai_chat_completions_options_reject_invalid_values() {
        let options = BTreeMap::from([("developer_role".to_string(), "operator".to_string())]);

        let error = OpenAiChatCompletionsOptions::from_provider_options(&options).unwrap_err();

        assert_eq!(
            error.kind(),
            crate::ProviderRequestAssemblyErrorKind::InvalidArgs
        );
        assert!(error.message().contains("developer or system"));
    }

    /// Verifies hallucinated or unsupported compatible tool calls fail as
    /// malformed provider output and retain the raw call for product-level
    /// diagnostics.
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

        assert!(error.message().contains("non-MAAP tool calls"));
        assert!(error.raw_text().contains("unexpected_tool"));
    }
}
