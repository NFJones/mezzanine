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
        resolve_provider_api,
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
}
