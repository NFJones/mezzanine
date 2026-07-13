//! Provider-neutral API compatibility contracts.
//!
//! This module owns stable provider API identifiers and their resolution from
//! product configuration. Provider construction, credentials, transports, and
//! product error conversion remain in the Mezzanine composition crate.

use std::fmt;

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
}
