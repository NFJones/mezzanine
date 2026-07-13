//! Provider-neutral API compatibility contracts.
//!
//! This module owns stable provider API identifiers and their resolution from
//! product configuration. Provider construction, credentials, transports, and
//! product error conversion remain in the Mezzanine composition crate.

use std::fmt;

/// Result type returned while assembling one provider request.
pub type ProviderRequestAssemblyResult<T> = Result<T, ProviderRequestAssemblyError>;

/// Stable categories for provider request-assembly failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRequestAssemblyErrorKind {
    /// A required provider request input was malformed.
    InvalidArgs,
    /// Provider request encoding or diagnostic construction failed.
    InvalidState,
}

/// A typed failure returned while assembling one provider request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequestAssemblyError {
    kind: ProviderRequestAssemblyErrorKind,
    message: String,
}

impl ProviderRequestAssemblyError {
    /// Creates an invalid-argument request assembly failure.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderRequestAssemblyErrorKind::InvalidArgs,
            message: message.into(),
        }
    }

    /// Creates an invalid-state request assembly failure.
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderRequestAssemblyErrorKind::InvalidState,
            message: message.into(),
        }
    }

    /// Returns the stable request-assembly failure category.
    pub fn kind(&self) -> ProviderRequestAssemblyErrorKind {
        self.kind
    }

    /// Returns the diagnostic message without formatting the error.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ProviderRequestAssemblyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProviderRequestAssemblyError {}

/// Validates one required provider request field.
pub fn validate_provider_request_required(
    field: &str,
    value: &str,
) -> ProviderRequestAssemblyResult<()> {
    if value.trim().is_empty() {
        return Err(ProviderRequestAssemblyError::invalid_args(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

/// API compatibility id for providers that speak the OpenAI Responses API.
pub const OPENAI_RESPONSES_API: &str = "openai-responses";
/// API compatibility id for providers that speak OpenAI-style Chat Completions.
pub const OPENAI_CHAT_COMPLETIONS_API: &str = "openai-chat-completions";
/// API compatibility id for the DeepSeek Chat Completions dialect.
pub const DEEPSEEK_CHAT_COMPLETIONS_API: &str = "deepseek-chat-completions";
/// API compatibility id for the Anthropic Messages API.
pub const ANTHROPIC_MESSAGES_API: &str = "anthropic-messages";
/// API compatibility id for the Claude Code subprocess adapter.
pub const CLAUDE_CODE_API: &str = "claude-code";

/// Wire API compatibility selected for one configured provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderApiCompatibility {
    /// OpenAI Responses request, response, model-catalog, and MAAP tool shape.
    OpenAiResponses,
    /// OpenAI-compatible Chat Completions request and response shape.
    OpenAiChatCompletions,
    /// DeepSeek Chat Completions dialect with native thinking and shim tools.
    DeepSeekChatCompletions,
    /// Anthropic Messages request, response, and tool-use shape.
    AnthropicMessages,
    /// Claude Code subprocess request and response shape.
    ClaudeCode,
}

impl ProviderApiCompatibility {
    /// Returns the stable configuration identifier for this compatibility.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiResponses => OPENAI_RESPONSES_API,
            Self::OpenAiChatCompletions => OPENAI_CHAT_COMPLETIONS_API,
            Self::DeepSeekChatCompletions => DEEPSEEK_CHAT_COMPLETIONS_API,
            Self::AnthropicMessages => ANTHROPIC_MESSAGES_API,
            Self::ClaudeCode => CLAUDE_CODE_API,
        }
    }

    /// Parses a stable API compatibility identifier.
    pub fn from_id(api: &str) -> Option<Self> {
        match api {
            OPENAI_RESPONSES_API => Some(Self::OpenAiResponses),
            OPENAI_CHAT_COMPLETIONS_API => Some(Self::OpenAiChatCompletions),
            DEEPSEEK_CHAT_COMPLETIONS_API => Some(Self::DeepSeekChatCompletions),
            ANTHROPIC_MESSAGES_API => Some(Self::AnthropicMessages),
            CLAUDE_CODE_API => Some(Self::ClaudeCode),
            _ => None,
        }
    }

    /// Returns the compatibility historically implied by one provider kind.
    pub fn default_for_kind(kind: &str) -> Option<Self> {
        match kind {
            "openai" => Some(Self::OpenAiResponses),
            "openai-compatible" => Some(Self::OpenAiChatCompletions),
            "deepseek" => Some(Self::DeepSeekChatCompletions),
            "anthropic" => Some(Self::AnthropicMessages),
            "claude-code" => Some(Self::ClaudeCode),
            _ => None,
        }
    }
}

/// Declares which request fields and features a provider supports.
///
/// Capability flags drive request construction, retry mutation, and fallback
/// selection without depending on product configuration or transport types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    /// Whether the provider accepts the OpenAI Responses API body shape.
    pub supports_responses_api: bool,
    /// Whether max_output_tokens is accepted by the provider.
    pub supports_max_output_tokens: bool,
    /// Whether reasoning effort controls are accepted.
    pub supports_reasoning_controls: bool,
    /// Whether provider thinking mode can be explicitly enabled or disabled.
    pub supports_thinking_toggle: bool,
    /// Whether the service_tier field is accepted.
    pub supports_service_tier: bool,
    /// Whether prompt cache retention is supported.
    pub supports_prompt_cache_retention: bool,
    /// Whether streaming (SSE) is supported.
    pub supports_streaming: bool,
    /// Whether function tool calling is supported.
    pub supports_tool_calls: bool,
    /// Whether the provider supports parallel tool calls.
    pub supports_parallel_tool_calls: bool,
}

impl ProviderCapabilities {
    /// Returns the capabilities for one API compatibility implementation.
    pub fn for_api(api: ProviderApiCompatibility) -> Self {
        match api {
            ProviderApiCompatibility::OpenAiResponses => Self {
                supports_responses_api: true,
                supports_max_output_tokens: false,
                supports_reasoning_controls: true,
                supports_thinking_toggle: false,
                supports_service_tier: true,
                supports_prompt_cache_retention: false,
                supports_streaming: true,
                supports_tool_calls: true,
                supports_parallel_tool_calls: true,
            },
            ProviderApiCompatibility::DeepSeekChatCompletions => Self {
                supports_responses_api: false,
                supports_max_output_tokens: true,
                supports_reasoning_controls: true,
                supports_thinking_toggle: true,
                supports_service_tier: false,
                supports_prompt_cache_retention: false,
                supports_streaming: true,
                supports_tool_calls: true,
                supports_parallel_tool_calls: false,
            },
            ProviderApiCompatibility::OpenAiChatCompletions => Self {
                supports_responses_api: false,
                supports_max_output_tokens: true,
                supports_reasoning_controls: false,
                supports_thinking_toggle: false,
                supports_service_tier: false,
                supports_prompt_cache_retention: false,
                supports_streaming: false,
                supports_tool_calls: true,
                supports_parallel_tool_calls: false,
            },
            ProviderApiCompatibility::AnthropicMessages => Self {
                supports_responses_api: false,
                supports_max_output_tokens: true,
                supports_reasoning_controls: true,
                supports_thinking_toggle: false,
                supports_service_tier: false,
                supports_prompt_cache_retention: false,
                supports_streaming: true,
                supports_tool_calls: true,
                supports_parallel_tool_calls: false,
            },
            ProviderApiCompatibility::ClaudeCode => Self {
                supports_responses_api: false,
                supports_max_output_tokens: false,
                supports_reasoning_controls: true,
                supports_thinking_toggle: false,
                supports_service_tier: false,
                supports_prompt_cache_retention: false,
                supports_streaming: false,
                supports_tool_calls: false,
                supports_parallel_tool_calls: false,
            },
        }
    }

    /// Returns the capabilities historically implied by one provider kind.
    pub fn for_kind(kind: &str) -> Self {
        ProviderApiCompatibility::default_for_kind(kind)
            .map(Self::for_api)
            .unwrap_or_else(Self::unsupported)
    }

    /// Returns capabilities for a provider kind plus optional API id.
    pub fn for_provider_config(
        kind: &str,
        api: Option<&str>,
    ) -> Result<Self, ProviderApiCompatibilityError> {
        resolve_provider_api(kind, api).map(Self::for_api)
    }

    /// Returns a capability set that advertises no provider features.
    fn unsupported() -> Self {
        Self {
            supports_responses_api: false,
            supports_max_output_tokens: false,
            supports_reasoning_controls: false,
            supports_thinking_toggle: false,
            supports_service_tier: false,
            supports_prompt_cache_retention: false,
            supports_streaming: false,
            supports_tool_calls: false,
            supports_parallel_tool_calls: false,
        }
    }
}

/// Describes one model returned by a provider catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelInfo {
    /// Stable provider model identifier.
    pub id: String,
    /// Optional provider display label.
    pub display_name: Option<String>,
    /// Provider-supported reasoning levels.
    pub reasoning_levels: Vec<String>,
    /// Provider-reported or locally documented context-window size in tokens.
    pub context_window_tokens: Option<usize>,
    /// Provider-reported capability tags such as `tool_use`.
    pub capabilities: Vec<String>,
}

/// Describes a normalized provider model-catalog response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelCatalog {
    /// Configured provider identifier.
    pub provider: String,
    /// Secret-safe catalog source description.
    pub source: String,
    /// Models returned by the provider or product adapter.
    pub models: Vec<ProviderModelInfo>,
    /// Reasoning levels supported across the catalog.
    pub reasoning_levels: Vec<String>,
    /// Provider-reported quota usage for the catalog request.
    pub quota_usage: Vec<crate::ProviderQuotaUsage>,
}

/// Failure to parse one provider model-catalog response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelCatalogParseError {
    message: String,
}

impl ProviderModelCatalogParseError {
    /// Returns the stable diagnostic for the malformed catalog response.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ProviderModelCatalogParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProviderModelCatalogParseError {}

#[cfg(test)]
mod request_assembly_tests {
    use super::{
        ProviderRequestAssemblyError, ProviderRequestAssemblyErrorKind,
        validate_provider_request_required,
    };

    /// Provider request validation preserves invalid-argument diagnostics for
    /// required fields and accepts substantive values.
    #[test]
    fn provider_request_validation_rejects_empty_required_fields() {
        assert!(validate_provider_request_required("OpenAI model", "gpt-5").is_ok());
        let error = validate_provider_request_required("OpenAI model", " \t ").unwrap_err();
        assert_eq!(error.kind(), ProviderRequestAssemblyErrorKind::InvalidArgs);
        assert_eq!(error.message(), "OpenAI model must not be empty");
    }

    /// Provider request encoding failures retain their invalid-state category
    /// for conversion by the product composition boundary.
    #[test]
    fn provider_request_encoding_errors_are_invalid_state() {
        let error = ProviderRequestAssemblyError::invalid_state("encoding failed");
        assert_eq!(error.kind(), ProviderRequestAssemblyErrorKind::InvalidState);
        assert_eq!(error.to_string(), "encoding failed");
    }
}

/// Parses an OpenAI-compatible model-catalog response.
///
/// The caller supplies locally documented context-window sizes so product
/// model knowledge remains outside the provider-neutral parser.
pub fn parse_openai_models_http_body_with<F>(
    body: &str,
    known_context_window_tokens: F,
) -> Result<Vec<ProviderModelInfo>, ProviderModelCatalogParseError>
where
    F: Fn(&str) -> Option<usize>,
{
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|error| ProviderModelCatalogParseError {
            message: format!("OpenAI Models response was not valid JSON: {error}"),
        })?;
    let models = openai_models_array(&value).ok_or_else(|| ProviderModelCatalogParseError {
        message: "OpenAI Models response did not contain models".to_string(),
    })?;
    let mut parsed = Vec::new();
    for model in models {
        if let Some(info) = openai_model_info_from_value(model, &known_context_window_tokens) {
            parsed.push(info);
        }
    }
    parsed.sort_by(|left, right| left.id.cmp(&right.id));
    parsed.dedup_by(|left, right| left.id == right.id);
    Ok(parsed)
}

/// Returns default reasoning levels for OpenAI reasoning-model families.
pub fn openai_default_reasoning_levels_for_model(model_id: &str) -> Vec<String> {
    let lower = model_id.to_ascii_lowercase();
    if lower.starts_with("gpt-5")
        || lower.starts_with("o1")
        || lower.starts_with("o3")
        || lower.starts_with("o4")
    {
        vec![
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
            "xhigh".to_string(),
        ]
    } else {
        Vec::new()
    }
}

/// Returns the ordered union of reasoning levels advertised by a catalog.
pub fn provider_catalog_reasoning_levels(models: &[ProviderModelInfo]) -> Vec<String> {
    dedupe_provider_strings(
        models
            .iter()
            .flat_map(|model| model.reasoning_levels.iter().cloned())
            .collect(),
    )
}

fn openai_models_array(value: &serde_json::Value) -> Option<&[serde_json::Value]> {
    value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .or_else(|| value.get("models").and_then(serde_json::Value::as_array))
        .or_else(|| value.as_array())
        .map(Vec::as_slice)
}

fn openai_model_info_from_value<F>(
    value: &serde_json::Value,
    known_context_window_tokens: &F,
) -> Option<ProviderModelInfo>
where
    F: Fn(&str) -> Option<usize>,
{
    let (id, display_name) = match value {
        serde_json::Value::String(model_id) => (model_id.to_string(), None),
        serde_json::Value::Object(object) => {
            let id = object
                .get("id")
                .or_else(|| object.get("name"))
                .or_else(|| object.get("slug"))
                .and_then(serde_json::Value::as_str)?
                .to_string();
            let display_name = object
                .get("display_name")
                .or_else(|| object.get("label"))
                .and_then(serde_json::Value::as_str)
                .filter(|name| *name != id)
                .map(str::to_string);
            (id, display_name)
        }
        _ => return None,
    };
    let mut reasoning_levels = provider_reasoning_levels_from_value(value);
    if reasoning_levels.is_empty() {
        reasoning_levels = openai_default_reasoning_levels_for_model(&id);
    }
    Some(ProviderModelInfo {
        id: id.clone(),
        display_name,
        reasoning_levels,
        context_window_tokens: provider_context_window_tokens_from_value(value)
            .or_else(|| known_context_window_tokens(&id)),
        capabilities: provider_capabilities_from_value(value),
    })
}

fn provider_capabilities_from_value(value: &serde_json::Value) -> Vec<String> {
    let mut capabilities = Vec::new();
    if let Some(values) = value
        .get("capabilities")
        .and_then(serde_json::Value::as_array)
    {
        capabilities.extend(
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|capability| !capability.is_empty())
                .map(str::to_string),
        );
    }
    if let Some(object) = value
        .get("capabilities")
        .and_then(serde_json::Value::as_object)
    {
        capabilities.extend(
            object
                .iter()
                .filter(|(_, value)| value.as_bool().unwrap_or(false))
                .map(|(capability, _)| capability.trim())
                .filter(|capability| !capability.is_empty())
                .map(str::to_string),
        );
    }
    for field in ["tool_use", "tools", "function_calling", "structured_output"] {
        if value
            .get(field)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            capabilities.push(field.to_string());
        }
    }
    dedupe_provider_strings(capabilities)
}

fn provider_context_window_tokens_from_value(value: &serde_json::Value) -> Option<usize> {
    let object = value.as_object()?;
    for field in [
        "context_window_tokens",
        "context_limit_tokens",
        "context_window",
        "context_length",
        "max_context_length",
        "input_token_limit",
        "max_input_tokens",
    ] {
        if let Some(tokens) = object
            .get(field)
            .and_then(serde_json::Value::as_u64)
            .and_then(|tokens| usize::try_from(tokens).ok())
            .filter(|tokens| *tokens > 0)
        {
            return Some(tokens);
        }
    }
    for pointer in [
        "/limits/context_window_tokens",
        "/limits/context_limit_tokens",
        "/limits/context_window",
        "/limits/context_length",
        "/limits/max_context_length",
        "/capabilities/context_window_tokens",
        "/capabilities/context_limit_tokens",
        "/capabilities/context_window",
        "/capabilities/context_length",
        "/capabilities/max_context_length",
    ] {
        if let Some(tokens) = value
            .pointer(pointer)
            .and_then(serde_json::Value::as_u64)
            .and_then(|tokens| usize::try_from(tokens).ok())
            .filter(|tokens| *tokens > 0)
        {
            return Some(tokens);
        }
    }
    None
}

fn provider_reasoning_levels_from_value(value: &serde_json::Value) -> Vec<String> {
    for pointer in [
        "/reasoning/efforts",
        "/reasoning/levels",
        "/reasoning_efforts",
        "/reasoning_levels",
        "/supported_reasoning_efforts",
        "/supported_reasoning_levels",
        "/capabilities/reasoning_efforts",
        "/capabilities/reasoning_levels",
    ] {
        if let Some(levels) = value.pointer(pointer).and_then(serde_json::Value::as_array) {
            let levels = levels
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|level| !level.trim().is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            if !levels.is_empty() {
                return dedupe_provider_strings(levels);
            }
        }
    }
    Vec::new()
}

fn dedupe_provider_strings(values: Vec<String>) -> Vec<String> {
    let mut deduped = Vec::new();
    for value in values {
        if !deduped.iter().any(|existing| existing == &value) {
            deduped.push(value);
        }
    }
    deduped
}

/// Failure to resolve a configured provider API compatibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderApiCompatibilityError {
    /// An explicit API compatibility identifier is unsupported.
    UnsupportedApi(String),
    /// The provider kind has no implicit API compatibility.
    MissingApiForKind(String),
}

impl fmt::Display for ProviderApiCompatibilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedApi(api) => write!(
                formatter,
                "unsupported provider API compatibility `{api}`; use {OPENAI_RESPONSES_API}, {OPENAI_CHAT_COMPLETIONS_API}, {DEEPSEEK_CHAT_COMPLETIONS_API}, {ANTHROPIC_MESSAGES_API}, or {CLAUDE_CODE_API}"
            ),
            Self::MissingApiForKind(kind) => write!(
                formatter,
                "providers using kind `{kind}` must configure an api compatibility id"
            ),
        }
    }
}

impl std::error::Error for ProviderApiCompatibilityError {}

/// Resolves an optional configured API id against one provider kind.
pub fn resolve_provider_api(
    kind: &str,
    api: Option<&str>,
) -> Result<ProviderApiCompatibility, ProviderApiCompatibilityError> {
    match api.map(str::trim).filter(|api| !api.is_empty()) {
        Some(api) => ProviderApiCompatibility::from_id(api)
            .ok_or_else(|| ProviderApiCompatibilityError::UnsupportedApi(api.to_string())),
        None => ProviderApiCompatibility::default_for_kind(kind)
            .ok_or_else(|| ProviderApiCompatibilityError::MissingApiForKind(kind.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ANTHROPIC_MESSAGES_API, ProviderApiCompatibility, ProviderApiCompatibilityError,
        ProviderCapabilities, ProviderModelCatalog, ProviderModelInfo, resolve_provider_api,
    };

    #[test]
    /// Verifies stable provider API identifiers parse and format through the
    /// dependency-neutral compatibility contract.
    fn provider_api_compatibility_ids_round_trip() {
        assert_eq!(
            ProviderApiCompatibility::from_id(ANTHROPIC_MESSAGES_API),
            Some(ProviderApiCompatibility::AnthropicMessages)
        );
        assert_eq!(
            ProviderApiCompatibility::AnthropicMessages.as_str(),
            ANTHROPIC_MESSAGES_API
        );
    }

    #[test]
    /// Verifies provider kinds select stable defaults while unsupported
    /// explicit and implicit configurations retain distinct typed failures.
    fn provider_api_resolution_preserves_defaults_and_errors() {
        assert_eq!(
            resolve_provider_api("anthropic", None),
            Ok(ProviderApiCompatibility::AnthropicMessages)
        );
        assert_eq!(
            resolve_provider_api("openai", Some("unknown")),
            Err(ProviderApiCompatibilityError::UnsupportedApi(
                "unknown".to_string()
            ))
        );
        assert_eq!(
            resolve_provider_api("custom", None),
            Err(ProviderApiCompatibilityError::MissingApiForKind(
                "custom".to_string()
            ))
        );
    }

    #[test]
    /// Verifies provider feature classification follows the selected wire API
    /// and rejects unsupported explicit configuration at the agent boundary.
    fn provider_capabilities_follow_api_compatibility() {
        let responses = ProviderCapabilities::for_api(ProviderApiCompatibility::OpenAiResponses);
        assert!(responses.supports_responses_api);
        assert!(responses.supports_service_tier);
        assert!(responses.supports_parallel_tool_calls);

        let deepseek = ProviderCapabilities::for_provider_config("deepseek", None).unwrap();
        assert!(deepseek.supports_thinking_toggle);
        assert!(!deepseek.supports_parallel_tool_calls);

        assert_eq!(
            ProviderCapabilities::for_provider_config("openai", Some("unknown")),
            Err(ProviderApiCompatibilityError::UnsupportedApi(
                "unknown".to_string()
            ))
        );
        assert_eq!(
            ProviderCapabilities::for_kind("custom"),
            ProviderCapabilities::unsupported()
        );
    }

    #[test]
    /// Verifies normalized model-catalog contracts preserve provider identity,
    /// model capabilities, context limits, reasoning levels, and quota data.
    fn provider_model_catalog_preserves_normalized_metadata() {
        let catalog = ProviderModelCatalog {
            provider: "provider".to_string(),
            source: "remote".to_string(),
            models: vec![ProviderModelInfo {
                id: "model".to_string(),
                display_name: Some("Model".to_string()),
                reasoning_levels: vec!["high".to_string()],
                context_window_tokens: Some(128_000),
                capabilities: vec!["tool_use".to_string()],
            }],
            reasoning_levels: vec!["high".to_string()],
            quota_usage: Vec::new(),
        };

        assert_eq!(catalog.provider, "provider");
        assert_eq!(catalog.models[0].context_window_tokens, Some(128_000));
        assert_eq!(catalog.models[0].capabilities, ["tool_use"]);
    }

    #[test]
    /// Verifies OpenAI-compatible model catalogs preserve provider metadata,
    /// apply agent-owned reasoning defaults, and use caller-supplied context
    /// knowledge only when the response omits an explicit limit.
    fn openai_models_catalog_parser_extracts_models_and_reasoning_levels() {
        let models = super::parse_openai_models_http_body_with(
            r#"{"object":"list","data":[{"id":"gpt-5.5"},{"id":"gpt-custom","display_name":"Custom","reasoning":{"efforts":["tiny","large"]},"context_length":262144},{"id":"lmstudio-local","capabilities":["tool_use"],"structured_output":true}]}"#,
            |model| (model == "gpt-5.5").then_some(1_050_000),
        )
        .unwrap();

        assert_eq!(models.len(), 3);
        let custom = models
            .iter()
            .find(|model| model.id == "gpt-custom")
            .unwrap();
        assert_eq!(custom.display_name.as_deref(), Some("Custom"));
        assert_eq!(custom.reasoning_levels, ["tiny", "large"]);
        assert_eq!(custom.context_window_tokens, Some(262_144));
        let local = models
            .iter()
            .find(|model| model.id == "lmstudio-local")
            .unwrap();
        assert_eq!(local.capabilities, ["tool_use", "structured_output"]);
        let defaulted = models.iter().find(|model| model.id == "gpt-5.5").unwrap();
        assert_eq!(
            defaulted.reasoning_levels,
            ["low", "medium", "high", "xhigh"]
        );
        assert_eq!(defaulted.context_window_tokens, Some(1_050_000));
    }
}
