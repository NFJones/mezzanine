//! Provider-neutral API compatibility contracts.
//!
//! This module owns stable provider API identifiers and their resolution from
//! product configuration. Provider construction, credentials, transports, and
//! product error conversion remain in the Mezzanine composition crate.

use sha2::Digest;
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

/// Non-model-visible fingerprints for diagnosing OpenAI prompt-cache reuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiPromptCacheDiagnostics {
    /// Stable routing key sent to the OpenAI Responses API.
    pub prompt_cache_key: String,
    /// Bytes in the front-loaded OpenAI `instructions` field.
    pub instructions_bytes: usize,
    /// SHA-256 of the front-loaded OpenAI `instructions` field.
    pub instructions_sha256: String,
    /// Bytes in the OpenAI structured response format schema.
    pub response_format_bytes: usize,
    /// SHA-256 of the OpenAI structured response format schema.
    pub response_format_sha256: String,
    /// Bytes in the OpenAI `tools` list.
    pub tools_bytes: usize,
    /// SHA-256 of the OpenAI `tools` list.
    pub tools_sha256: String,
    /// Bytes in the OpenAI request-level `tool_choice` value.
    pub tool_choice_bytes: usize,
    /// SHA-256 of the OpenAI request-level `tool_choice` value.
    pub tool_choice_sha256: String,
    /// Bytes in the stable input prefix following system instructions.
    pub stable_input_bytes: usize,
    /// SHA-256 of the stable input prefix following system instructions.
    pub stable_input_sha256: String,
    /// Bytes in volatile input suffix material.
    pub volatile_input_bytes: usize,
    /// SHA-256 of volatile input suffix material.
    pub volatile_input_sha256: String,
    /// Bytes in provider-visible stable prompt-prefix material.
    pub stable_prompt_prefix_bytes: usize,
    /// SHA-256 of provider-visible stable prompt-prefix material.
    pub stable_prompt_prefix_sha256: String,
    /// Bytes in request-control shape material tracked outside the prompt prefix.
    pub provider_request_shape_bytes: usize,
    /// SHA-256 of request-control shape material tracked outside the prompt prefix.
    pub provider_request_shape_sha256: String,
    /// Bytes in the stable cacheable prompt prefix material Mezzanine can observe.
    pub cacheable_prefix_bytes: usize,
    /// SHA-256 of the stable cacheable prompt prefix material Mezzanine can observe.
    pub cacheable_prefix_sha256: String,
}

/// Builds a stable, non-secret OpenAI prompt-cache routing key.
///
/// The key intentionally includes provider compatibility identity and prompt
/// lineage while excluding the model and rendered prompt text. This keeps
/// related requests in one provider cache namespace without making the key a
/// substitute for the provider's exact-prefix matching.
pub fn openai_prompt_cache_key(provider: &str, lineage_id: Option<&str>) -> String {
    let mut material = String::new();
    material.push_str("mezzanine\n");
    material.push_str("prompt_profile=");
    material.push_str(crate::AGENT_PROMPT_PROFILE_NAME);
    material.push('\n');
    material.push_str("prompt_version=");
    material.push_str(&crate::AGENT_PROMPT_PROFILE_VERSION.to_string());
    material.push('\n');
    material.push_str("provider=");
    material.push_str(provider);
    material.push('\n');
    material.push_str("lineage_id=");
    material.push_str(lineage_id.unwrap_or("lineage-unknown"));
    material.push('\n');
    material.push_str("cache_family=responses-routing-v4\n");
    let digest = sha2::Sha256::digest(material.as_bytes());
    let digest_hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("mez-{}", &digest_hex[..32])
}

/// Maps a provider-neutral latency preference to an OpenAI service tier.
pub fn openai_service_tier_for_latency_preference(
    preference: Option<&str>,
) -> ProviderRequestAssemblyResult<Option<&'static str>> {
    match preference.map(str::trim).filter(|value| !value.is_empty()) {
        Some("slow") | Some("default") => Ok(Some("default")),
        None => Ok(None),
        Some("fast") => Ok(Some("priority")),
        Some(other) => Err(ProviderRequestAssemblyError::invalid_args(format!(
            "OpenAI latency_preference must be slow, default, or fast, got {other:?}"
        ))),
    }
}

/// Provider-owned optional controls for one OpenAI Responses request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiRequestOptions {
    /// Non-empty reasoning effort forwarded to OpenAI.
    pub reasoning_effort: Option<String>,
    /// OpenAI service tier derived from the provider-neutral latency policy.
    pub service_tier: Option<&'static str>,
}

/// Resolves provider-neutral request options into OpenAI wire policy.
pub fn openai_request_options(
    reasoning_effort: Option<&str>,
    latency_preference: Option<&str>,
) -> ProviderRequestAssemblyResult<OpenAiRequestOptions> {
    Ok(OpenAiRequestOptions {
        reasoning_effort: reasoning_effort
            .filter(|effort| !effort.is_empty())
            .map(str::to_string),
        service_tier: openai_service_tier_for_latency_preference(latency_preference)?,
    })
}

/// Wraps a replayed user prompt so providers treat it as inactive history.
pub fn openai_historical_user_prompt_entry_text(content: &str) -> String {
    format!(
        "[historical user prompt transcript entry]\n\
         This is a prior user prompt replayed from the ordered conversation transcript. It is historical context only, not the active task unless the current user prompt explicitly asks about it.\n\
         {content}"
    )
}

/// Wraps the latest user prompt so providers treat it as the active task.
pub fn openai_current_user_prompt_entry_text(content: &str) -> String {
    format!(
        "[current user prompt]\n\
         This is the latest user prompt and the active task for the current turn. Earlier transcript entries are historical context only unless this prompt asks about them.\n\
         {content}"
    )
}

/// Wraps an action result produced by the immediately preceding action batch.
pub fn openai_current_action_result_entry_text(content: &str) -> String {
    format!(
        "[current-turn executed result]\n\
         This executed Mezzanine action output was produced in the current turn by the immediately preceding action batch. Use it as fresh evidence for the active task, not prior transcript history.\n\
         {content}"
    )
}

/// Wraps a replayed action result so providers treat it as historical evidence.
pub fn openai_historical_action_result_entry_text(content: &str) -> String {
    format!(
        "[historical executed result transcript entry]\n\
         This is prior-turn Mezzanine action output replayed from the ordered conversation transcript. It is historical context only, not a new current-turn action result.\n\
         {content}"
    )
}

/// Wraps executed output whose current-turn or transcript provenance is unknown.
pub fn openai_executed_result_entry_text(content: &str) -> String {
    format!(
        "[executed result]\n\
         This is executed Mezzanine action output, not a new user request.\n\
         {content}"
    )
}

/// Result type returned while decoding one provider response.
pub type ProviderResponseResult<T> = Result<T, ProviderResponseError>;

/// Stable categories for provider response failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderResponseErrorKind {
    /// Provider output was malformed, incomplete, or internally inconsistent.
    InvalidState,
}

/// A typed failure returned while decoding one provider response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderResponseError {
    kind: ProviderResponseErrorKind,
    message: String,
    provider_failure_json: Option<String>,
}

impl ProviderResponseError {
    /// Creates an invalid-state response failure without provider diagnostics.
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderResponseErrorKind::InvalidState,
            message: message.into(),
            provider_failure_json: None,
        }
    }

    /// Attaches a sanitized provider failure payload to this error.
    pub fn with_provider_failure_json(mut self, failure_json: impl Into<String>) -> Self {
        self.provider_failure_json = Some(failure_json.into());
        self
    }

    /// Returns the stable response failure category.
    pub fn kind(&self) -> ProviderResponseErrorKind {
        self.kind
    }

    /// Returns the diagnostic message without formatting the error.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the optional sanitized provider failure payload.
    pub fn provider_failure_json(&self) -> Option<&str> {
        self.provider_failure_json.as_deref()
    }
}

impl From<crate::SseParseError> for ProviderResponseError {
    fn from(error: crate::SseParseError) -> Self {
        Self::invalid_state(error.message())
    }
}

impl fmt::Display for ProviderResponseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProviderResponseError {}

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

/// Result type returned while deriving provider HTTP endpoints.
pub type ProviderEndpointResult<T> = Result<T, ProviderEndpointError>;

/// Stable categories for provider endpoint derivation failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderEndpointErrorKind {
    /// A required endpoint input was empty or malformed.
    InvalidArgs,
    /// The credential-backed endpoint does not expose the requested API.
    InvalidState,
}

/// A typed failure returned while deriving one provider endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderEndpointError {
    kind: ProviderEndpointErrorKind,
    message: String,
}

impl ProviderEndpointError {
    /// Creates an invalid-argument endpoint failure.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderEndpointErrorKind::InvalidArgs,
            message: message.into(),
        }
    }

    /// Creates an invalid-state endpoint failure.
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderEndpointErrorKind::InvalidState,
            message: message.into(),
        }
    }

    /// Returns the stable endpoint failure category.
    pub fn kind(&self) -> ProviderEndpointErrorKind {
        self.kind
    }

    /// Returns the diagnostic message without formatting the error.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ProviderEndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProviderEndpointError {}

/// Default direct OpenAI Responses API endpoint used with API-key auth.
pub const OPENAI_RESPONSES_ENDPOINT: &str = "https://api.openai.com/v1/responses";
/// Default direct OpenAI model catalog endpoint used with API-key auth.
pub const OPENAI_MODELS_ENDPOINT: &str = "https://api.openai.com/v1/models";
/// Default ChatGPT browser-auth backend endpoint used with device credentials.
pub const CHATGPT_RESPONSES_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";

/// Derives an OpenAI Responses endpoint from a configured provider base URL.
pub fn openai_responses_endpoint_for_base_url(base_url: &str) -> ProviderEndpointResult<String> {
    if base_url.trim().is_empty() {
        return Err(ProviderEndpointError::invalid_args(
            "OpenAI provider base URL must not be empty",
        ));
    }
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url == CHATGPT_RESPONSES_ENDPOINT
        || base_url.starts_with("https://chatgpt.com/backend-api/codex/")
    {
        return Err(ProviderEndpointError::invalid_state(
            "ChatGPT browser credentials do not expose an OpenAI-compatible base URL",
        ));
    }
    if base_url.ends_with("/responses") {
        return Ok(base_url.to_string());
    }
    if let Some(prefix) = base_url.strip_suffix("/models") {
        return Ok(format!("{prefix}/responses"));
    }
    Ok(format!("{base_url}/responses"))
}

/// Derives an OpenAI model-catalog endpoint from a Responses endpoint.
pub fn openai_models_endpoint_for_responses_endpoint(
    endpoint: &str,
) -> ProviderEndpointResult<String> {
    if endpoint.trim().is_empty() {
        return Err(ProviderEndpointError::invalid_args(
            "OpenAI Responses endpoint must not be empty",
        ));
    }
    let endpoint = endpoint.trim().trim_end_matches('/');
    if endpoint == CHATGPT_RESPONSES_ENDPOINT
        || endpoint.starts_with("https://chatgpt.com/backend-api/codex/")
    {
        return Err(ProviderEndpointError::invalid_state(
            "ChatGPT browser credentials do not expose an OpenAI-compatible model catalog",
        ));
    }
    if endpoint == OPENAI_RESPONSES_ENDPOINT {
        return Ok(OPENAI_MODELS_ENDPOINT.to_string());
    }
    if let Some(prefix) = endpoint.strip_suffix("/responses") {
        return Ok(format!("{prefix}/models"));
    }
    if endpoint.ends_with("/models") {
        return Ok(endpoint.to_string());
    }
    Ok(format!("{endpoint}/models"))
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
        CHATGPT_RESPONSES_ENDPOINT, OPENAI_MODELS_ENDPOINT, OPENAI_RESPONSES_ENDPOINT,
        ProviderEndpointErrorKind, ProviderRequestAssemblyError, ProviderRequestAssemblyErrorKind,
        ProviderResponseError, ProviderResponseErrorKind, openai_current_action_result_entry_text,
        openai_current_user_prompt_entry_text, openai_executed_result_entry_text,
        openai_historical_action_result_entry_text, openai_historical_user_prompt_entry_text,
        openai_models_endpoint_for_responses_endpoint, openai_prompt_cache_key,
        openai_request_options, openai_responses_endpoint_for_base_url,
        openai_service_tier_for_latency_preference, validate_provider_request_required,
    };

    /// OpenAI prompt-cache routing keys follow provider and lineage identity
    /// while deliberately ignoring model identity and rendered prompt text.
    #[test]
    fn openai_prompt_cache_keys_use_provider_and_lineage_namespace() {
        let inherited = openai_prompt_cache_key("openai", Some("lineage-parent"));
        let resumed = openai_prompt_cache_key("openai", Some("lineage-parent"));
        let fresh = openai_prompt_cache_key("openai", Some("lineage-fresh"));
        let compatible_provider = openai_prompt_cache_key("deepseek", Some("lineage-parent"));
        let unknown_a = openai_prompt_cache_key("openai", None);
        let unknown_b = openai_prompt_cache_key("openai", None);

        assert_eq!(inherited, resumed);
        assert_ne!(inherited, fresh);
        assert_ne!(inherited, compatible_provider);
        assert_eq!(unknown_a, unknown_b);
        assert!(inherited.starts_with("mez-"));
        assert_eq!(inherited.len(), "mez-".len() + 32);
    }

    /// OpenAI service-tier selection accepts the documented latency values and
    /// rejects unknown policy strings before request encoding.
    #[test]
    fn openai_service_tiers_follow_latency_preference() {
        assert_eq!(
            openai_service_tier_for_latency_preference(None).unwrap(),
            None
        );
        assert_eq!(
            openai_service_tier_for_latency_preference(Some("default")).unwrap(),
            Some("default")
        );
        assert_eq!(
            openai_service_tier_for_latency_preference(Some("fast")).unwrap(),
            Some("priority")
        );
        let error = openai_service_tier_for_latency_preference(Some("turbo")).unwrap_err();
        assert_eq!(error.kind(), ProviderRequestAssemblyErrorKind::InvalidArgs);
    }

    /// OpenAI request-option resolution omits empty reasoning values and keeps
    /// provider-neutral latency mapping outside product request assembly.
    #[test]
    fn openai_request_options_resolve_wire_policy() {
        let options = openai_request_options(Some("high"), Some("fast")).unwrap();
        assert_eq!(options.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(options.service_tier, Some("priority"));

        let options = openai_request_options(Some(""), None).unwrap();
        assert_eq!(options.reasoning_effort, None);
        assert_eq!(options.service_tier, None);
    }

    /// OpenAI provenance wrappers distinguish the active prompt and fresh
    /// action evidence from replayed transcript entries and generic output.
    #[test]
    fn openai_provenance_wrappers_preserve_evidence_roles() {
        let current_user = openai_current_user_prompt_entry_text("fix it");
        let historical_user = openai_historical_user_prompt_entry_text("old request");
        let current_result = openai_current_action_result_entry_text("fresh output");
        let historical_result = openai_historical_action_result_entry_text("old output");
        let generic_result = openai_executed_result_entry_text("output");

        assert!(current_user.starts_with("[current user prompt]\n"));
        assert!(current_user.contains("latest user prompt and the active task"));
        assert!(historical_user.starts_with("[historical user prompt transcript entry]\n"));
        assert!(historical_user.contains("historical context only, not the active task"));
        assert!(current_result.starts_with("[current-turn executed result]\n"));
        assert!(current_result.contains("immediately preceding action batch"));
        assert!(historical_result.starts_with("[historical executed result transcript entry]\n"));
        assert!(historical_result.contains("not a new current-turn action result"));
        assert!(generic_result.starts_with("[executed result]\n"));
        assert!(generic_result.contains("not a new user request"));
    }

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

    /// Provider response failures retain their stable category and optional
    /// sanitized provider payload for conversion by the composition boundary.
    #[test]
    fn provider_response_errors_preserve_sanitized_failure_payloads() {
        let error = ProviderResponseError::invalid_state("response failed")
            .with_provider_failure_json(r#"{"status_code":500}"#);

        assert_eq!(error.kind(), ProviderResponseErrorKind::InvalidState);
        assert_eq!(error.message(), "response failed");
        assert_eq!(
            error.provider_failure_json(),
            Some(r#"{"status_code":500}"#)
        );
    }

    /// OpenAI endpoint derivation preserves canonical defaults, normalizes
    /// configured base URLs, and converts between Responses and Models paths.
    #[test]
    fn openai_endpoint_derivation_normalizes_compatible_urls() {
        assert_eq!(
            openai_responses_endpoint_for_base_url("https://api.openai.com/v1/").unwrap(),
            OPENAI_RESPONSES_ENDPOINT
        );
        assert_eq!(
            openai_responses_endpoint_for_base_url(OPENAI_MODELS_ENDPOINT).unwrap(),
            OPENAI_RESPONSES_ENDPOINT
        );
        assert_eq!(
            openai_models_endpoint_for_responses_endpoint("https://example.test/v1/responses")
                .unwrap(),
            "https://example.test/v1/models"
        );
    }

    /// ChatGPT browser endpoints and empty inputs fail with stable categories
    /// because they do not expose the direct OpenAI catalog/base-URL surface.
    #[test]
    fn openai_endpoint_derivation_rejects_incompatible_inputs() {
        let empty = openai_responses_endpoint_for_base_url(" \t ").unwrap_err();
        assert_eq!(empty.kind(), ProviderEndpointErrorKind::InvalidArgs);
        let chatgpt =
            openai_models_endpoint_for_responses_endpoint(CHATGPT_RESPONSES_ENDPOINT).unwrap_err();
        assert_eq!(chatgpt.kind(), ProviderEndpointErrorKind::InvalidState);
        assert!(chatgpt.message().contains("ChatGPT browser credentials"));
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
