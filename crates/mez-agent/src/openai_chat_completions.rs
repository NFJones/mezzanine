//! Provider-independent OpenAI-compatible Chat Completions request shaping.
//!
//! This module owns compatibility-option parsing and deterministic JSON request
//! construction for local and third-party OpenAI-style backends. Credentials,
//! endpoints, HTTP headers, timeouts, transport, and product error projection
//! remain in the root provider adapter.

use crate::{
    MAAP_ACTION_BATCH_TOOL_NAME as OPENAI_MAAP_FUNCTION_TOOL_NAME, ModelInteractionKind,
    ModelMessageRole, ModelRequest, ProviderRequestAssemblyError, ProviderRequestAssemblyResult,
    maap_action_batch_schema, openai_maap_current_action_batch_description,
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
}
