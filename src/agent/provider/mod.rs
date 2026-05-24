//! Agent Provider implementation.
//!
//! This module owns the agent provider boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AGENT_PROMPT_PROFILE_NAME, AGENT_PROMPT_PROFILE_VERSION, AgentCapability, AllowedAction,
    AllowedActionSet, AuthStore, BTreeMap, ContextSourceKind, ExposeSecret, MaapBatch,
    McpPromptTool, MezError, ModelInteractionKind, ModelMessage, ModelMessageRole, ModelRequest,
    Result, SecretString, parse_fenced_maap_action_batch_for_turn,
    parse_maap_action_batch_json_for_turn, validate_non_empty,
};
use crate::auth::{AuthCredentialKind, AuthMetadata};
use sha2::Digest;
use std::future::Future;
use std::pin::Pin;

// Model provider traits and OpenAI Responses adapter.

mod catalog;
mod errors;
mod http;
mod schema;
use catalog::provider_catalog_reasoning_levels;
pub use catalog::{
    ProviderModelCatalog, ProviderModelInfo, openai_default_reasoning_levels_for_model,
    openai_models_endpoint_for_responses_endpoint, openai_responses_endpoint_for_base_url,
    parse_openai_models_http_body,
};
use errors::{
    openai_provider_error_detail, openai_provider_failure_event_json, openai_provider_failure_json,
    provider_maap_parse_error,
};
pub(crate) use errors::{
    provider_error_invites_retry, provider_error_is_context_limit_exceeded,
    provider_error_is_output_limit_exceeded,
};
#[cfg(test)]
pub use http::ProviderHttpTransport;
pub use http::{
    AsyncProviderHttpTransport, DEFAULT_PROVIDER_MAX_RESPONSE_BYTES, DEFAULT_PROVIDER_TIMEOUT_MS,
    ProviderHttpRequest, ProviderHttpResponse, ReqwestProviderHttpTransport,
};
use schema::{
    OpenAiMaapToolSurface, maap_action_batch_schema, openai_maap_action_batch_tools,
    openai_maap_tool_surface_for_request,
};
#[cfg(test)]
use schema::{maap_mcp_call_action_schema_for_tool, normalize_openai_strict_schema};

/// Default direct OpenAI Responses API endpoint used with API-key auth.
pub const OPENAI_RESPONSES_ENDPOINT: &str = "https://api.openai.com/v1/responses";
/// Default direct OpenAI model catalog endpoint used with API-key auth.
pub const OPENAI_MODELS_ENDPOINT: &str = "https://api.openai.com/v1/models";
/// Default DeepSeek Chat Completions API endpoint.
pub const DEEPSEEK_CHAT_COMPLETIONS_ENDPOINT: &str = "https://api.deepseek.com/v1/chat/completions";
/// Default DeepSeek models listing endpoint.
#[allow(dead_code)]
pub const DEEPSEEK_MODELS_ENDPOINT: &str = "https://api.deepseek.com/v1/models";
/// OpenAI organization routing header for multi-organization API keys.
pub const OPENAI_ORGANIZATION_HEADER: &str = "OpenAI-Organization";
/// OpenAI project routing header for project-scoped API accounting.
pub const OPENAI_PROJECT_HEADER: &str = "OpenAI-Project";
/// Default ChatGPT browser-auth backend endpoint used with device credentials.
pub const CHATGPT_RESPONSES_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
/// ChatGPT account selection header required by ChatGPT-backed requests.
pub const CHATGPT_ACCOUNT_ID_HEADER: &str = "ChatGPT-Account-ID";
/// OpenAI function tool name used to carry one validated MAAP action batch.
pub const OPENAI_MAAP_FUNCTION_TOOL_NAME: &str = "submit_maap_action_batch";
/// Prefix used by local provider-context compaction summaries.
const OPENAI_CONTEXT_COMPACTED_PREFIX: &str = "[context compacted]";
/// Maximum native function-call argument bytes accepted from OpenAI responses.
const OPENAI_FUNCTION_CALL_ARGUMENT_LIMIT_BYTES: usize = DEFAULT_PROVIDER_MAX_RESPONSE_BYTES;

/// DeepSeek request strategy for provider-native MAAP transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeepSeekMaapRequestStrategy {
    /// No MAAP tool is needed for this provider request.
    NoTool,
    /// Use DeepSeek thinking mode and let the model choose the MAAP tool.
    AutoToolThinking,
    /// Disable thinking and force the MAAP tool with `tool_choice`.
    ForcedToolNonThinking,
}

/// Provider-reported token usage for one or more model requests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelTokenUsage {
    /// Raw input tokens reported by the provider before prompt-cache adjustment.
    pub input_tokens: u64,
    /// Output tokens charged or counted by the provider.
    pub output_tokens: u64,
    /// Output tokens attributed to reasoning by the provider.
    pub reasoning_tokens: u64,
    /// Input tokens served from the provider prompt cache, when reported.
    pub cached_input_tokens: Option<u64>,
}

impl ModelTokenUsage {
    /// Adds provider usage counters with saturating arithmetic.
    pub fn add_assign(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(other.reasoning_tokens);
        self.cached_input_tokens = match (self.cached_input_tokens, other.cached_input_tokens) {
            (Some(current), Some(next)) => Some(current.saturating_add(next)),
            (None, Some(next)) => Some(next),
            (Some(current), None) => Some(current),
            (None, None) => None,
        };
    }

    /// Returns true when the provider did not report any token usage.
    pub fn is_zero(self) -> bool {
        self.input_tokens == 0
            && self.output_tokens == 0
            && self.reasoning_tokens == 0
            && self.cached_input_tokens.unwrap_or(0) == 0
    }

    /// Returns provider-visible total tokens when input and output are known.
    pub fn total_tokens(self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    /// Returns input tokens that were not reported as prompt-cache hits.
    pub fn billed_input_tokens(self) -> u64 {
        self.input_tokens
            .saturating_sub(self.cached_input_tokens.unwrap_or(0))
    }

    /// Returns the display value for provider prompt-cache hits.
    pub fn cached_input_tokens_display(self) -> String {
        self.cached_input_tokens
            .map(|tokens| tokens.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Returns the provider prompt-cache hit ratio when it is known.
    pub fn cached_input_hit_ratio_basis_points(self) -> Option<u32> {
        let cached = self.cached_input_tokens?;
        if self.input_tokens == 0 {
            return Some(0);
        }
        let basis_points = cached
            .saturating_mul(10_000)
            .saturating_add(self.input_tokens / 2)
            / self.input_tokens;
        Some(basis_points.min(10_000) as u32)
    }

    /// Returns a human-readable provider prompt-cache hit ratio.
    pub fn cached_input_hit_ratio_display(self) -> String {
        self.cached_input_hit_ratio_basis_points()
            .map(|basis_points| format!("{}.{:02}%", basis_points / 100, basis_points % 100))
            .unwrap_or_else(|| "unknown".to_string())
    }
}

/// Provider-reported quota usage for one rate-limit bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderQuotaUsage {
    /// Provider quota bucket name, such as `requests` or `tokens`.
    pub name: String,
    /// Usage percentage in basis points, where `10000` is exactly 100%.
    pub used_basis_points: u32,
    /// Provider-reported quota limit for this bucket.
    pub limit: u64,
    /// Provider-reported quota remaining for this bucket.
    pub remaining: u64,
    /// Provider-reported reset value for this bucket, if supplied.
    pub reset: Option<String>,
}

impl ProviderQuotaUsage {
    /// Returns a human-readable percentage with two decimal places.
    pub fn used_percent_display(&self) -> String {
        format!(
            "{}.{:02}%",
            self.used_basis_points / 100,
            self.used_basis_points % 100
        )
    }
}

/// Carries Model Response state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelResponse {
    /// Stores the provider value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub provider: String,
    /// Stores the model value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub model: String,
    /// Stores the raw text value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub raw_text: String,
    /// Provider-reported token usage for the request or accumulated exchange.
    pub usage: ModelTokenUsage,
    /// Provider-reported quota usage percentages for the request.
    pub quota_usage: Vec<ProviderQuotaUsage>,
    /// Stores the action batch value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub action_batch: Option<MaapBatch>,
}

/// Non-model-visible fingerprints for diagnosing provider prompt-cache reuse.
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
    /// Bytes in the stable input prefix following instructions/tools/schema.
    pub stable_input_bytes: usize,
    /// SHA-256 of the stable input prefix following instructions/tools/schema.
    pub stable_input_sha256: String,
    /// Bytes in volatile input suffix material.
    pub volatile_input_bytes: usize,
    /// SHA-256 of volatile input suffix material.
    pub volatile_input_sha256: String,
    /// Bytes in the complete cacheable prefix material Mezzanine can observe.
    pub cacheable_prefix_bytes: usize,
    /// SHA-256 of the complete cacheable prefix material Mezzanine can observe.
    pub cacheable_prefix_sha256: String,
}

/// Defines the Model Provider behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary
/// consumed by higher-level orchestration code.
#[cfg(test)]
pub trait ModelProvider {
    /// Runs the provider id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn provider_id(&self) -> &str;
    /// Runs the send request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_request(&self, request: &ModelRequest) -> Result<ModelResponse>;

    /// Runs the list models operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn list_models(&self) -> Result<ProviderModelCatalog> {
        Err(MezError::invalid_state(format!(
            "provider `{}` does not expose a model catalog",
            self.provider_id()
        )))
    }
}

/// Defines the Async Model Provider behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary
/// consumed by higher-level orchestration code.
pub trait AsyncModelProvider: Send + Sync {
    /// Runs the provider id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn provider_id(&self) -> &str;
    /// Runs the send request async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_request_async<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>>;

    /// Runs the list models async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn list_models_async<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderModelCatalog>> + Send + 'a>> {
        Box::pin(async move {
            Err(MezError::invalid_state(format!(
                "provider `{}` does not expose a model catalog",
                self.provider_id()
            )))
        })
    }
}

/// Declares which request fields and features a provider supports.
///
/// Capability flags drive request construction, retry mutation, and fallback
/// selection: Mezzanine skips fields the provider does not advertise, and
/// fail-fast rules treat unsupported-parameter rejections as permanent errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    /// Whether the provider accepts the OpenAI Responses API body shape.
    pub supports_responses_api: bool,
    /// Whether max_output_tokens is accepted by the provider.
    pub supports_max_output_tokens: bool,
    /// Whether reasoning effort controls are accepted.
    pub supports_reasoning_controls: bool,
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
    /// Returns the capabilities for a known provider kind string.
    pub fn for_kind(kind: &str) -> Self {
        match kind {
            "openai" => Self {
                supports_responses_api: true,
                supports_max_output_tokens: true,
                supports_reasoning_controls: true,
                supports_service_tier: true,
                supports_prompt_cache_retention: true,
                supports_streaming: true,
                supports_tool_calls: true,
                supports_parallel_tool_calls: true,
            },
            "deepseek" => Self {
                supports_responses_api: false,
                supports_max_output_tokens: true,
                supports_reasoning_controls: true,
                supports_service_tier: false,
                supports_prompt_cache_retention: false,
                supports_streaming: true,
                supports_tool_calls: true,
                supports_parallel_tool_calls: false,
            },
            _ => Self {
                supports_responses_api: false,
                supports_max_output_tokens: false,
                supports_reasoning_controls: false,
                supports_service_tier: false,
                supports_prompt_cache_retention: false,
                supports_streaming: false,
                supports_tool_calls: false,
                supports_parallel_tool_calls: false,
            },
        }
    }
}

/// Carries Open Ai Responses Provider state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct OpenAiResponsesProvider<T> {
    /// Stores the api key value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) api_key: SecretString,
    /// Stores the endpoint value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) endpoint: String,
    /// Stores the extra headers value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) extra_headers: BTreeMap<String, String>,
    /// Stores the stream value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) stream: bool,
    /// Stores the timeout ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) timeout_ms: u64,
    /// Stores the transport value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) transport: T,
}

impl<T> OpenAiResponsesProvider<T> {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new(api_key: impl Into<String>, transport: T) -> Result<Self> {
        let api_key = SecretString::from(api_key.into());
        Self::new_secret(api_key, transport)
    }

    /// Runs the new secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn new_secret(api_key: SecretString, transport: T) -> Result<Self> {
        Self::with_endpoint(
            api_key,
            OPENAI_RESPONSES_ENDPOINT,
            DEFAULT_PROVIDER_TIMEOUT_MS,
            transport,
        )
    }

    /// Creates a provider that uses a ChatGPT OAuth access token.
    ///
    /// The account id is sent as a provider header and must come from non-secret
    /// auth metadata parsed during browser or device-code login.
    pub fn new_chatgpt_secret(
        access_token: SecretString,
        account_id: impl Into<String>,
        transport: T,
    ) -> Result<Self> {
        let account_id = account_id.into();
        let mut extra_headers = BTreeMap::new();
        extra_headers.insert(CHATGPT_ACCOUNT_ID_HEADER.to_string(), account_id);
        Self::with_endpoint_headers_and_stream(
            access_token,
            CHATGPT_RESPONSES_ENDPOINT,
            DEFAULT_PROVIDER_TIMEOUT_MS,
            extra_headers,
            true,
            transport,
        )
    }

    /// Runs the with endpoint operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn with_endpoint(
        api_key: impl Into<SecretString>,
        endpoint: impl Into<String>,
        timeout_ms: u64,
        transport: T,
    ) -> Result<Self> {
        Self::with_endpoint_and_headers(api_key, endpoint, timeout_ms, BTreeMap::new(), transport)
    }

    /// Creates a provider with an explicit endpoint and additional headers.
    ///
    /// Additional headers are intended for provider-owned auth routing metadata,
    /// such as the ChatGPT account id. The bearer credential remains stored in
    /// the dedicated `Authorization` header.
    pub fn with_endpoint_and_headers(
        api_key: impl Into<SecretString>,
        endpoint: impl Into<String>,
        timeout_ms: u64,
        extra_headers: BTreeMap<String, String>,
        transport: T,
    ) -> Result<Self> {
        Self::with_endpoint_headers_and_stream(
            api_key,
            endpoint,
            timeout_ms,
            extra_headers,
            false,
            transport,
        )
    }

    /// Creates a provider with an explicit endpoint, extra headers, and stream mode.
    ///
    /// Direct API-key providers default to unary JSON responses. ChatGPT-backed
    /// providers require streaming, but the adapter still normalizes the final
    /// completed stream into one `ModelResponse`.
    pub fn with_endpoint_headers_and_stream(
        api_key: impl Into<SecretString>,
        endpoint: impl Into<String>,
        timeout_ms: u64,
        extra_headers: BTreeMap<String, String>,
        stream: bool,
        transport: T,
    ) -> Result<Self> {
        let api_key = api_key.into();
        let endpoint = endpoint.into();
        validate_non_empty("OpenAI provider bearer credential", api_key.expose_secret())?;
        validate_non_empty("OpenAI Responses endpoint", &endpoint)?;
        for (name, value) in &extra_headers {
            validate_non_empty("OpenAI provider extra header name", name)?;
            validate_non_empty("OpenAI provider extra header value", value)?;
        }
        if timeout_ms == 0 {
            return Err(MezError::invalid_args(
                "OpenAI provider timeout must be greater than zero",
            ));
        }
        Ok(Self {
            api_key,
            endpoint,
            extra_headers,
            stream,
            timeout_ms,
            transport,
        })
    }
}

/// Runs the openai provider from auth store with transport operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
pub fn openai_provider_from_auth_store_with_transport<T>(
    auth_store: &AuthStore,
    transport: T,
) -> Result<OpenAiResponsesProvider<T>> {
    openai_provider_from_auth_store_with_options(
        auth_store,
        None,
        DEFAULT_PROVIDER_TIMEOUT_MS,
        transport,
    )
}

/// Builds an OpenAI provider from persisted auth metadata and credentials.
///
/// API-key credentials use the direct OpenAI Responses endpoint derived from
/// the configured provider base URL. ChatGPT browser/device credentials use the
/// ChatGPT backend and carry the persisted ChatGPT account id header.
pub fn openai_provider_from_auth_store_with_options<T>(
    auth_store: &AuthStore,
    base_url_override: Option<&str>,
    timeout_ms: u64,
    transport: T,
) -> Result<OpenAiResponsesProvider<T>> {
    openai_provider_from_auth_store_with_provider_options(
        auth_store,
        base_url_override,
        &BTreeMap::new(),
        timeout_ms,
        transport,
    )
}

/// Carries Deep Seek Chat Completions Provider state.
#[derive(Debug, Clone)]
pub struct DeepSeekChatCompletionsProvider<T> {
    pub(super) api_key: SecretString,
    pub(super) endpoint: String,
    pub(super) stream: bool,
    pub(super) timeout_ms: u64,
    pub(super) transport: T,
}

impl<T> DeepSeekChatCompletionsProvider<T> {
    /// Creates a new DeepSeek Chat Completions provider with the given API key.
    pub fn new(api_key: impl Into<SecretString>, transport: T) -> Result<Self> {
        let api_key = api_key.into();
        validate_non_empty("DeepSeek API key", api_key.expose_secret())?;
        Ok(Self {
            api_key,
            endpoint: DEEPSEEK_CHAT_COMPLETIONS_ENDPOINT.to_string(),
            stream: false,
            timeout_ms: DEFAULT_PROVIDER_TIMEOUT_MS,
            transport,
        })
    }

    /// Enables or disables streaming for this provider.
    pub fn with_stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    /// Overrides the default endpoint URL for this provider.
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Sets the request timeout in milliseconds.
    pub fn with_timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        self
    }
}

#[cfg(test)]
impl<T: ProviderHttpTransport> ModelProvider for DeepSeekChatCompletionsProvider<T> {
    fn provider_id(&self) -> &str {
        "deepseek"
    }

    fn list_models(&self) -> Result<ProviderModelCatalog> {
        let http_request = build_deepseek_models_http_request(
            self.api_key.expose_secret(),
            &self.endpoint,
            self.timeout_ms,
        )?;
        let response = self.transport.send(&http_request)?;
        if !(200..300).contains(&response.status_code) {
            return Err(MezError::invalid_state(format!(
                "DeepSeek Models API returned status {}: {}",
                response.status_code,
                openai_provider_error_detail(&response.body)
            ))
            .with_provider_failure_json(openai_provider_failure_json(
                Some(response.status_code),
                &response.body,
            )));
        }
        let models = parse_openai_models_http_body(&response.body)?;
        let reasoning_levels = provider_catalog_reasoning_levels(&models);
        let quota_usage = provider_quota_usage_from_headers(&response.headers);
        Ok(ProviderModelCatalog {
            provider: ModelProvider::provider_id(self).to_string(),
            source: "provider".to_string(),
            models,
            reasoning_levels,
            quota_usage,
        })
    }

    fn send_request(&self, request: &ModelRequest) -> Result<ModelResponse> {
        if request.provider != ModelProvider::provider_id(self) {
            return Err(MezError::invalid_args(
                "DeepSeek provider received a request for a different provider",
            ));
        }
        let strategy = deepseek_maap_request_strategy(request);
        let http_request = build_deepseek_chat_completions_http_request_with_strategy(
            request,
            self.api_key.expose_secret(),
            &self.endpoint,
            self.stream,
            self.timeout_ms,
            strategy,
        )?;
        let response = self.transport.send(&http_request)?;
        if !(200..300).contains(&response.status_code) {
            return Err(MezError::invalid_state(format!(
                "DeepSeek Chat Completions API returned status {}: {}",
                response.status_code,
                openai_provider_error_detail(&response.body)
            ))
            .with_provider_failure_json(openai_provider_failure_json(
                Some(response.status_code),
                &response.body,
            )));
        }
        let mut parsed = parse_deepseek_chat_completions_http_response(
            response,
            request,
            ModelProvider::provider_id(self),
            deepseek_effective_stream(self.stream, strategy),
        )?;
        if deepseek_should_retry_with_forced_maap(request, strategy, &parsed) {
            let fallback_request = build_deepseek_chat_completions_http_request_with_strategy(
                request,
                self.api_key.expose_secret(),
                &self.endpoint,
                self.stream,
                self.timeout_ms,
                DeepSeekMaapRequestStrategy::ForcedToolNonThinking,
            )?;
            let fallback_response = self.transport.send(&fallback_request)?;
            if !(200..300).contains(&fallback_response.status_code) {
                return Err(MezError::invalid_state(format!(
                    "DeepSeek Chat Completions API returned status {}: {}",
                    fallback_response.status_code,
                    openai_provider_error_detail(&fallback_response.body)
                ))
                .with_provider_failure_json(openai_provider_failure_json(
                    Some(fallback_response.status_code),
                    &fallback_response.body,
                )));
            }
            let mut fallback = parse_deepseek_chat_completions_http_response(
                fallback_response,
                request,
                ModelProvider::provider_id(self),
                false,
            )?;
            fallback.usage.add_assign(parsed.usage);
            if fallback.quota_usage.is_empty() {
                fallback.quota_usage = parsed.quota_usage;
            }
            parsed = fallback;
        }
        Ok(parsed)
    }
}

impl<T: AsyncProviderHttpTransport> AsyncModelProvider for DeepSeekChatCompletionsProvider<T> {
    fn provider_id(&self) -> &str {
        "deepseek"
    }

    fn list_models_async<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderModelCatalog>> + Send + 'a>> {
        Box::pin(async move {
            let http_request = build_deepseek_models_http_request(
                self.api_key.expose_secret(),
                &self.endpoint,
                self.timeout_ms,
            )?;
            let response = self.transport.send_async(&http_request).await?;
            if !(200..300).contains(&response.status_code) {
                return Err(MezError::invalid_state(format!(
                    "DeepSeek Models API returned status {}: {}",
                    response.status_code,
                    openai_provider_error_detail(&response.body)
                ))
                .with_provider_failure_json(openai_provider_failure_json(
                    Some(response.status_code),
                    &response.body,
                )));
            }
            let models = parse_openai_models_http_body(&response.body)?;
            let reasoning_levels = provider_catalog_reasoning_levels(&models);
            let quota_usage = provider_quota_usage_from_headers(&response.headers);
            Ok(ProviderModelCatalog {
                provider: AsyncModelProvider::provider_id(self).to_string(),
                source: "provider".to_string(),
                models,
                reasoning_levels,
                quota_usage,
            })
        })
    }

    fn send_request_async<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>> {
        Box::pin(async move {
            if request.provider != AsyncModelProvider::provider_id(self) {
                return Err(MezError::invalid_args(
                    "DeepSeek provider received a request for a different provider",
                ));
            }
            let strategy = deepseek_maap_request_strategy(request);
            let http_request = build_deepseek_chat_completions_http_request_with_strategy(
                request,
                self.api_key.expose_secret(),
                &self.endpoint,
                self.stream,
                self.timeout_ms,
                strategy,
            )?;
            let response = self.transport.send_async(&http_request).await?;
            if !(200..300).contains(&response.status_code) {
                return Err(MezError::invalid_state(format!(
                    "DeepSeek Chat Completions API returned status {}: {}",
                    response.status_code,
                    openai_provider_error_detail(&response.body)
                ))
                .with_provider_failure_json(openai_provider_failure_json(
                    Some(response.status_code),
                    &response.body,
                )));
            }
            let mut parsed = parse_deepseek_chat_completions_http_response(
                response,
                request,
                AsyncModelProvider::provider_id(self),
                deepseek_effective_stream(self.stream, strategy),
            )?;
            if deepseek_should_retry_with_forced_maap(request, strategy, &parsed) {
                let fallback_request = build_deepseek_chat_completions_http_request_with_strategy(
                    request,
                    self.api_key.expose_secret(),
                    &self.endpoint,
                    self.stream,
                    self.timeout_ms,
                    DeepSeekMaapRequestStrategy::ForcedToolNonThinking,
                )?;
                let fallback_response = self.transport.send_async(&fallback_request).await?;
                if !(200..300).contains(&fallback_response.status_code) {
                    return Err(MezError::invalid_state(format!(
                        "DeepSeek Chat Completions API returned status {}: {}",
                        fallback_response.status_code,
                        openai_provider_error_detail(&fallback_response.body)
                    ))
                    .with_provider_failure_json(openai_provider_failure_json(
                        Some(fallback_response.status_code),
                        &fallback_response.body,
                    )));
                }
                let mut fallback = parse_deepseek_chat_completions_http_response(
                    fallback_response,
                    request,
                    AsyncModelProvider::provider_id(self),
                    false,
                )?;
                fallback.usage.add_assign(parsed.usage);
                if fallback.quota_usage.is_empty() {
                    fallback.quota_usage = parsed.quota_usage;
                }
                parsed = fallback;
            }
            Ok(parsed)
        })
    }
}

/// Builds an OpenAI provider from auth metadata plus non-secret provider options.
///
/// Direct API-key requests use the documented OpenAI REST endpoints and may
/// include documented organization/project routing headers from provider
/// options. Browser/device credentials continue to target the ChatGPT backend
/// and do not expose the OpenAI-compatible model catalog.
pub fn openai_provider_from_auth_store_with_provider_options<T>(
    auth_store: &AuthStore,
    base_url_override: Option<&str>,
    provider_options: &BTreeMap<String, String>,
    timeout_ms: u64,
    transport: T,
) -> Result<OpenAiResponsesProvider<T>> {
    let metadata = auth_store
        .read_metadata_for_provider("openai")?
        .ok_or_else(|| MezError::invalid_state("OpenAI provider is not authenticated"))?;
    let credential = auth_store.provider_secret("openai")?;
    match metadata.credential_kind {
        AuthCredentialKind::ApiKey => {
            let endpoint = base_url_override
                .filter(|endpoint| !endpoint.trim().is_empty())
                .map(openai_responses_endpoint_for_base_url)
                .transpose()?
                .unwrap_or_else(|| OPENAI_RESPONSES_ENDPOINT.to_string());
            OpenAiResponsesProvider::with_endpoint_and_headers(
                credential,
                endpoint,
                timeout_ms,
                openai_direct_api_extra_headers(&metadata, provider_options),
                transport,
            )
        }
        AuthCredentialKind::ChatGpt => {
            let account_id = metadata.account_id.ok_or_else(|| {
                MezError::invalid_state(
                    "OpenAI ChatGPT login is missing a ChatGPT account id; run `mez auth login` again",
                )
            })?;
            let endpoint = base_url_override
                .filter(|endpoint| !endpoint.trim().is_empty())
                .unwrap_or(CHATGPT_RESPONSES_ENDPOINT);
            let mut extra_headers = BTreeMap::new();
            extra_headers.insert(CHATGPT_ACCOUNT_ID_HEADER.to_string(), account_id);
            OpenAiResponsesProvider::with_endpoint_headers_and_stream(
                credential,
                endpoint,
                timeout_ms,
                extra_headers,
                true,
                transport,
            )
        }
    }
}

/// Builds a DeepSeek Chat Completions provider from auth metadata.
///
/// DeepSeek only supports direct API-key authentication. Endpoint overrides
/// are passed through to the provider.
pub fn deepseek_provider_from_auth_store_with_provider_options<T>(
    auth_store: &AuthStore,
    base_url_override: Option<&str>,
    timeout_ms: u64,
    transport: T,
) -> Result<DeepSeekChatCompletionsProvider<T>> {
    let _metadata = auth_store
        .read_metadata_for_provider("deepseek")?
        .ok_or_else(|| MezError::invalid_state("DeepSeek provider is not authenticated"))?;
    let credential = auth_store.provider_secret("deepseek")?;
    let mut provider = DeepSeekChatCompletionsProvider::new(credential, transport)?;
    if let Some(endpoint) = base_url_override.filter(|e| !e.trim().is_empty()) {
        provider = provider.with_endpoint(endpoint);
    }
    provider = provider.with_timeout(timeout_ms);
    Ok(provider)
}

/// Builds documented OpenAI REST routing headers for direct API-key requests.
fn openai_direct_api_extra_headers(
    metadata: &AuthMetadata,
    provider_options: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    let organization_id = openai_provider_option(
        provider_options,
        &[
            "organization_id",
            "organization",
            "openai_organization_id",
            "openai_organization",
        ],
    )
    .or_else(|| metadata.organization_id.clone());
    if let Some(organization_id) = organization_id {
        headers.insert(OPENAI_ORGANIZATION_HEADER.to_string(), organization_id);
    }
    if let Some(project_id) = openai_provider_option(
        provider_options,
        &[
            "project_id",
            "project",
            "openai_project_id",
            "openai_project",
        ],
    ) {
        headers.insert(OPENAI_PROJECT_HEADER.to_string(), project_id);
    }
    headers
}

/// Returns a non-empty provider option value from the first supported key.
fn openai_provider_option(
    provider_options: &BTreeMap<String, String>,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .find_map(|key| provider_options.get(*key))
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
impl<T: ProviderHttpTransport> ModelProvider for OpenAiResponsesProvider<T> {
    /// Runs the provider id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn provider_id(&self) -> &str {
        "openai"
    }

    /// Runs the list models operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn list_models(&self) -> Result<ProviderModelCatalog> {
        let http_request = build_openai_models_http_request_with_headers(
            self.api_key.expose_secret(),
            &self.endpoint,
            &self.extra_headers,
            self.timeout_ms,
        )?;
        let response = self.transport.send(&http_request)?;
        if !(200..300).contains(&response.status_code) {
            return Err(MezError::invalid_state(format!(
                "OpenAI Models API returned status {}: {}",
                response.status_code,
                openai_provider_error_detail(&response.body)
            ))
            .with_provider_failure_json(openai_provider_failure_json(
                Some(response.status_code),
                &response.body,
            )));
        }
        let models = parse_openai_models_http_body(&response.body)?;
        let reasoning_levels = provider_catalog_reasoning_levels(&models);
        let quota_usage = provider_quota_usage_from_headers(&response.headers);
        Ok(ProviderModelCatalog {
            provider: ModelProvider::provider_id(self).to_string(),
            source: "provider".to_string(),
            models,
            reasoning_levels,
            quota_usage,
        })
    }

    /// Runs the send request operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_request(&self, request: &ModelRequest) -> Result<ModelResponse> {
        if request.provider != ModelProvider::provider_id(self) {
            return Err(MezError::invalid_args(
                "OpenAI provider received a request for a different provider",
            ));
        }
        let http_request = build_openai_responses_http_request_with_headers(
            request,
            self.api_key.expose_secret(),
            &self.endpoint,
            &self.extra_headers,
            self.stream,
            self.timeout_ms,
        )?;
        let response = self.transport.send(&http_request)?;
        if !(200..300).contains(&response.status_code) {
            return Err(MezError::invalid_state(format!(
                "OpenAI Responses API returned status {}: {}",
                response.status_code,
                openai_provider_error_detail(&response.body)
            ))
            .with_provider_failure_json(openai_provider_failure_json(
                Some(response.status_code),
                &response.body,
            )));
        }
        let (model, raw_text, usage) =
            parse_openai_responses_provider_body(&response.body, &request.model, self.stream)?;
        let quota_usage = provider_quota_usage_from_headers(&response.headers);
        let action_batch = if request.interaction_kind == ModelInteractionKind::AutoSizing {
            None
        } else {
            match parse_provider_native_maap_action_batch(&raw_text, request)? {
                Some(batch) => Some(batch),
                None => parse_fenced_maap_action_batch_for_turn(
                    &raw_text,
                    &request.turn_id,
                    &request.agent_id,
                )
                .map_err(|error| provider_maap_parse_error(error, &raw_text))?,
            }
        };
        Ok(ModelResponse {
            provider: ModelProvider::provider_id(self).to_string(),
            model,
            raw_text,
            usage,
            quota_usage,
            action_batch,
        })
    }
}

impl<T: AsyncProviderHttpTransport> AsyncModelProvider for OpenAiResponsesProvider<T> {
    /// Runs the provider id operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn provider_id(&self) -> &str {
        "openai"
    }

    /// Runs the list models async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn list_models_async<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderModelCatalog>> + Send + 'a>> {
        Box::pin(async move {
            let http_request = build_openai_models_http_request_with_headers(
                self.api_key.expose_secret(),
                &self.endpoint,
                &self.extra_headers,
                self.timeout_ms,
            )?;
            let response = self.transport.send_async(&http_request).await?;
            if !(200..300).contains(&response.status_code) {
                return Err(MezError::invalid_state(format!(
                    "OpenAI Models API returned status {}: {}",
                    response.status_code,
                    openai_provider_error_detail(&response.body)
                ))
                .with_provider_failure_json(openai_provider_failure_json(
                    Some(response.status_code),
                    &response.body,
                )));
            }
            let models = parse_openai_models_http_body(&response.body)?;
            let reasoning_levels = provider_catalog_reasoning_levels(&models);
            let quota_usage = provider_quota_usage_from_headers(&response.headers);
            Ok(ProviderModelCatalog {
                provider: AsyncModelProvider::provider_id(self).to_string(),
                source: "provider".to_string(),
                models,
                reasoning_levels,
                quota_usage,
            })
        })
    }

    /// Runs the send request async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_request_async<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>> {
        Box::pin(async move {
            if request.provider != AsyncModelProvider::provider_id(self) {
                return Err(MezError::invalid_args(
                    "OpenAI provider received a request for a different provider",
                ));
            }
            let http_request = build_openai_responses_http_request_with_headers(
                request,
                self.api_key.expose_secret(),
                &self.endpoint,
                &self.extra_headers,
                self.stream,
                self.timeout_ms,
            )?;
            let response = self.transport.send_async(&http_request).await?;
            if !(200..300).contains(&response.status_code) {
                return Err(MezError::invalid_state(format!(
                    "OpenAI Responses API returned status {}: {}",
                    response.status_code,
                    openai_provider_error_detail(&response.body)
                ))
                .with_provider_failure_json(openai_provider_failure_json(
                    Some(response.status_code),
                    &response.body,
                )));
            }
            let (model, raw_text, usage) =
                parse_openai_responses_provider_body(&response.body, &request.model, self.stream)?;
            let quota_usage = provider_quota_usage_from_headers(&response.headers);
            let action_batch = if request.interaction_kind == ModelInteractionKind::AutoSizing {
                None
            } else {
                match parse_provider_native_maap_action_batch(&raw_text, request)? {
                    Some(batch) => Some(batch),
                    None => parse_fenced_maap_action_batch_for_turn(
                        &raw_text,
                        &request.turn_id,
                        &request.agent_id,
                    )
                    .map_err(|error| provider_maap_parse_error(error, &raw_text))?,
                }
            };
            Ok(ModelResponse {
                provider: AsyncModelProvider::provider_id(self).to_string(),
                model,
                raw_text,
                usage,
                quota_usage,
                action_batch,
            })
        })
    }
}

/// Runs the build openai responses http request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn build_openai_responses_http_request(
    request: &ModelRequest,
    api_key: &str,
    endpoint: &str,
    timeout_ms: u64,
) -> Result<ProviderHttpRequest> {
    build_openai_responses_http_request_with_headers(
        request,
        api_key,
        endpoint,
        &BTreeMap::new(),
        false,
        timeout_ms,
    )
}

/// Builds an OpenAI Responses request with provider-specific extra headers.
///
/// The caller supplies non-secret routing headers only. The bearer credential is
/// always placed in the `Authorization` header by this function.
pub fn build_openai_responses_http_request_with_headers(
    request: &ModelRequest,
    api_key: &str,
    endpoint: &str,
    extra_headers: &BTreeMap<String, String>,
    stream: bool,
    timeout_ms: u64,
) -> Result<ProviderHttpRequest> {
    validate_non_empty("OpenAI provider bearer credential", api_key)?;
    validate_non_empty("OpenAI Responses endpoint", endpoint)?;
    for (name, value) in extra_headers {
        validate_non_empty("OpenAI provider extra header name", name)?;
        validate_non_empty("OpenAI provider extra header value", value)?;
    }
    if timeout_ms == 0 {
        return Err(MezError::invalid_args(
            "OpenAI provider timeout must be greater than zero",
        ));
    }
    let body = openai_responses_request_body_with_stream(request, stream)?;
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
    headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
    headers.extend(
        extra_headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
    Ok(ProviderHttpRequest {
        method: "POST".to_string(),
        url: endpoint.to_string(),
        headers,
        body,
        timeout_ms,
        max_response_bytes: None,
    })
}

/// Runs the build openai models http request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn build_openai_models_http_request(
    api_key: &str,
    responses_endpoint: &str,
    timeout_ms: u64,
) -> Result<ProviderHttpRequest> {
    build_openai_models_http_request_with_headers(
        api_key,
        responses_endpoint,
        &BTreeMap::new(),
        timeout_ms,
    )
}

/// Runs the build openai models http request with headers operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn build_openai_models_http_request_with_headers(
    api_key: &str,
    responses_endpoint: &str,
    extra_headers: &BTreeMap<String, String>,
    timeout_ms: u64,
) -> Result<ProviderHttpRequest> {
    validate_non_empty("OpenAI provider bearer credential", api_key)?;
    validate_non_empty("OpenAI Responses endpoint", responses_endpoint)?;
    for (name, value) in extra_headers {
        validate_non_empty("OpenAI provider extra header name", name)?;
        validate_non_empty("OpenAI provider extra header value", value)?;
    }
    if timeout_ms == 0 {
        return Err(MezError::invalid_args(
            "OpenAI provider timeout must be greater than zero",
        ));
    }
    let mut headers = BTreeMap::new();
    headers.insert("Accept".to_string(), "application/json".to_string());
    headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
    headers.extend(
        extra_headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
    Ok(ProviderHttpRequest {
        method: "GET".to_string(),
        url: openai_models_endpoint_for_responses_endpoint(responses_endpoint)?,
        headers,
        body: String::new(),
        timeout_ms,
        max_response_bytes: None,
    })
}

/// Builds a DeepSeek Chat Completions HTTP request.
pub fn build_deepseek_chat_completions_http_request(
    request: &ModelRequest,
    api_key: &str,
    endpoint: &str,
    stream: bool,
    timeout_ms: u64,
) -> Result<ProviderHttpRequest> {
    build_deepseek_chat_completions_http_request_with_strategy(
        request,
        api_key,
        endpoint,
        stream,
        timeout_ms,
        deepseek_maap_request_strategy(request),
    )
}

/// Builds a DeepSeek Chat Completions HTTP request with an explicit MAAP strategy.
fn build_deepseek_chat_completions_http_request_with_strategy(
    request: &ModelRequest,
    api_key: &str,
    endpoint: &str,
    stream: bool,
    timeout_ms: u64,
    strategy: DeepSeekMaapRequestStrategy,
) -> Result<ProviderHttpRequest> {
    validate_non_empty("DeepSeek provider bearer credential", api_key)?;
    validate_non_empty("DeepSeek Chat Completions endpoint", endpoint)?;
    if timeout_ms == 0 {
        return Err(MezError::invalid_args(
            "DeepSeek provider timeout must be greater than zero",
        ));
    }
    let stream = deepseek_effective_stream(stream, strategy);
    let body = deepseek_chat_completions_request_body_with_strategy(request, stream, strategy)?;
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
    headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
    Ok(ProviderHttpRequest {
        method: "POST".to_string(),
        url: endpoint.to_string(),
        headers,
        body,
        timeout_ms,
        max_response_bytes: None,
    })
}

/// Builds the JSON body for a DeepSeek request with an explicit MAAP strategy.
fn deepseek_chat_completions_request_body_with_strategy(
    request: &ModelRequest,
    stream: bool,
    strategy: DeepSeekMaapRequestStrategy,
) -> Result<String> {
    let capabilities = ProviderCapabilities::for_kind("deepseek");
    let messages: Vec<serde_json::Value> = request
        .messages
        .iter()
        .map(|message| {
            let role = match message.role {
                ModelMessageRole::System => "system",
                ModelMessageRole::User => "user",
                ModelMessageRole::Assistant => "assistant",
                _ => "user",
            };
            serde_json::json!({
                "role": role,
                "content": message.content
            })
        })
        .collect();
    let mut body = serde_json::json!({
        "model": request.model,
        "messages": messages,
        "stream": stream,
    });
    if let Some(max_output_tokens) = request
        .max_output_tokens
        .filter(|tokens| *tokens > 0)
        .filter(|_| capabilities.supports_max_output_tokens)
    {
        body["max_tokens"] = serde_json::json!(max_output_tokens);
    }
    if strategy == DeepSeekMaapRequestStrategy::ForcedToolNonThinking {
        body["thinking"] = serde_json::json!({"type": "disabled"});
    } else if let Some(reasoning_effort) = request
        .reasoning_effort
        .as_deref()
        .filter(|effort| !effort.is_empty())
    {
        let deepseek_effort = deepseek_reasoning_effort(reasoning_effort);
        body["reasoning_effort"] = serde_json::json!(deepseek_effort);
        body["thinking"] = serde_json::json!({"type": "enabled"});
    }
    if capabilities.supports_tool_calls && strategy != DeepSeekMaapRequestStrategy::NoTool {
        if strategy == DeepSeekMaapRequestStrategy::ForcedToolNonThinking {
            body["tool_choice"] = deepseek_maap_tool_choice();
        }
        let maap_tool = serde_json::json!({
            "type": "function",
            "function": {
                "name": OPENAI_MAAP_FUNCTION_TOOL_NAME,
                "description": deepseek_maap_tool_description(&request.allowed_actions),
                "parameters": maap_action_batch_schema(
                    &request.allowed_actions,
                    &request.available_mcp_tools
                ),
                "strict": false
            }
        });
        body["tools"] = serde_json::json!([maap_tool]);
    }
    serde_json::to_string(&body).map_err(|error| {
        MezError::invalid_state(format!(
            "DeepSeek Chat Completions request encoding failed: {error}"
        ))
    })
}

/// Returns the DeepSeek MAAP strategy for one model request.
///
/// Thinking mode can use function tools only when DeepSeek chooses the tool
/// itself. When reasoning is configured, Mezzanine therefore exposes the MAAP
/// tool without forcing `tool_choice` and falls back to strict non-thinking
/// mode only if DeepSeek returns prose instead of a MAAP batch.
fn deepseek_maap_request_strategy(request: &ModelRequest) -> DeepSeekMaapRequestStrategy {
    if request.interaction_kind == ModelInteractionKind::AutoSizing
        || request.allowed_actions.actions.is_empty()
    {
        return DeepSeekMaapRequestStrategy::NoTool;
    }
    if request
        .reasoning_effort
        .as_deref()
        .is_some_and(|effort| !effort.is_empty())
    {
        DeepSeekMaapRequestStrategy::AutoToolThinking
    } else {
        DeepSeekMaapRequestStrategy::ForcedToolNonThinking
    }
}

/// Returns the DeepSeek stream flag after accounting for MAAP tool strategy.
///
/// DeepSeek streaming tool-call parsing is not implemented in this adapter, so
/// MAAP tool requests use unary JSON even when the provider object was created
/// with streaming enabled.
fn deepseek_effective_stream(stream: bool, strategy: DeepSeekMaapRequestStrategy) -> bool {
    stream && strategy == DeepSeekMaapRequestStrategy::NoTool
}

/// Reports whether a DeepSeek thinking request should retry strict MAAP.
fn deepseek_should_retry_with_forced_maap(
    request: &ModelRequest,
    strategy: DeepSeekMaapRequestStrategy,
    response: &ModelResponse,
) -> bool {
    strategy == DeepSeekMaapRequestStrategy::AutoToolThinking
        && request.interaction_kind != ModelInteractionKind::AutoSizing
        && !request.allowed_actions.actions.is_empty()
        && response.action_batch.is_none()
}

/// Returns the DeepSeek tool choice that forces the MAAP function call.
///
/// # Behavior
/// DeepSeek's Chat Completions API defaults `tool_choice` to `auto` when tools
/// are present, which allows a prose answer instead of a MAAP action batch.
/// Mezzanine requires a structured action batch for every non-auto-sizing
/// provider turn. This helper is therefore reserved for strict fallback
/// requests with thinking disabled; thinking-mode MAAP requests omit
/// `tool_choice` and let DeepSeek choose the advertised MAAP tool.
fn deepseek_maap_tool_choice() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": OPENAI_MAAP_FUNCTION_TOOL_NAME
        }
    })
}

/// Builds concise provider-facing guidance for DeepSeek's single MAAP tool.
///
/// # Parameters
/// - `allowed_actions`: The current controller-approved action surface.
fn deepseek_maap_tool_description(allowed_actions: &AllowedActionSet) -> String {
    format!(
        "Submit exactly one MAAP/1 action batch through this function. Current allowed action types: {}. Use only the action objects in this function schema. Do not answer this Mezzanine agent turn in prose outside the function call. If the needed action is absent and request_capability is available, request that capability instead of answering in prose.",
        allowed_actions.action_type_names().join(",")
    )
}

/// Maps Mezzanine reasoning effort levels to DeepSeek-supported values.
fn deepseek_reasoning_effort(effort: &str) -> &'static str {
    match effort {
        "low" | "medium" | "high" => "high",
        "xhigh" | "max" => "max",
        _ => "high",
    }
}

/// Parses one successful DeepSeek HTTP response into a model response.
///
/// # Parameters
/// - `response`: The HTTP response returned by the provider transport.
/// - `request`: The model request that produced the response.
/// - `provider_id`: The provider identity to report on the normalized result.
/// - `stream`: Whether the HTTP body uses DeepSeek SSE streaming format.
fn parse_deepseek_chat_completions_http_response(
    response: ProviderHttpResponse,
    request: &ModelRequest,
    provider_id: &str,
    stream: bool,
) -> Result<ModelResponse> {
    let ProviderHttpResponse { headers, body, .. } = response;
    if stream {
        let actions = parse_deepseek_chat_completions_stream_body(&body, request)?;
        return Ok(ModelResponse {
            provider: provider_id.to_string(),
            model: request.model.clone(),
            raw_text: actions,
            usage: Default::default(),
            quota_usage: provider_quota_usage_from_headers(&headers),
            action_batch: None,
        });
    }
    let mut parsed = parse_deepseek_chat_completions_response_body(&body, request)?;
    parsed.provider = provider_id.to_string();
    parsed.quota_usage = provider_quota_usage_from_headers(&headers);
    Ok(parsed)
}

/// Parses a DeepSeek Chat Completions non-streaming response body.
fn parse_deepseek_chat_completions_response_body(
    body: &str,
    request: &ModelRequest,
) -> Result<ModelResponse> {
    let root: serde_json::Value = serde_json::from_str(body).map_err(|error| {
        MezError::invalid_state(format!(
            "DeepSeek Chat Completions response body is invalid JSON: {error}"
        ))
    })?;
    let model = root
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&request.model)
        .to_string();
    let choices = root
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            MezError::invalid_state("DeepSeek Chat Completions response has no choices array")
        })?;
    let first_choice = choices.first().ok_or_else(|| {
        MezError::invalid_state("DeepSeek Chat Completions response has empty choices array")
    })?;
    let message = first_choice.get("message").ok_or_else(|| {
        MezError::invalid_state("DeepSeek Chat Completions choice has no message")
    })?;
    let raw_text = message
        .get("content")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_default();
    let reasoning_content = message
        .get("reasoning_content")
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let raw_text = if raw_text.is_empty() {
        reasoning_content.clone().unwrap_or_else(|| {
            if message.get("tool_calls").is_some() {
                "executing".to_string()
            } else {
                "(empty)".to_string()
            }
        })
    } else {
        raw_text
    };
    let action_batch = if let Some(parsed) = message
        .get("tool_calls")
        .and_then(|tool_calls| tool_calls.as_array())
        .filter(|tool_calls| !tool_calls.is_empty())
    {
        let maap_json = parsed
            .iter()
            .find_map(|call| {
                let name = call.get("function")?.get("name")?.as_str()?;
                if name == OPENAI_MAAP_FUNCTION_TOOL_NAME {
                    call.get("function")?.get("arguments")?.as_str()
                } else {
                    None
                }
            })
            .unwrap_or("");
        if maap_json.is_empty() {
            None
        } else {
            parse_maap_action_batch_json_for_turn(maap_json, &request.turn_id, &request.agent_id)
                .ok()
        }
    } else {
        None
    };
    let usage = root
        .get("usage")
        .map(parse_deepseek_usage)
        .unwrap_or_default();
    Ok(ModelResponse {
        provider: request.provider.clone(),
        model,
        raw_text,
        usage,
        quota_usage: Vec::new(),
        action_batch,
    })
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
        reasoning_tokens: usage
            .get("reasoning_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        cached_input_tokens: usage
            .get("prompt_cache_hit_tokens")
            .and_then(serde_json::Value::as_u64),
    }
}

/// Parses a DeepSeek Chat Completions streaming (SSE) response body.
fn parse_deepseek_chat_completions_stream_body(
    body: &str,
    _request: &ModelRequest,
) -> Result<String> {
    let mut text_content = String::new();
    for line in body.lines() {
        let data = line.strip_prefix("data: ").unwrap_or(line);
        if data == "[DONE]" || data.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        if let Some(choices) = event.get("choices").and_then(serde_json::Value::as_array) {
            for choice in choices {
                if let Some(delta) = choice.get("delta")
                    && let Some(content) = delta.get("content").and_then(serde_json::Value::as_str)
                {
                    text_content.push_str(content);
                }
            }
        }
    }
    Ok(text_content)
}

/// Builds a DeepSeek models listing HTTP request.
pub fn build_deepseek_models_http_request(
    api_key: &str,
    chat_endpoint: &str,
    timeout_ms: u64,
) -> Result<ProviderHttpRequest> {
    validate_non_empty("DeepSeek model listing credential", api_key)?;
    let models_endpoint = chat_endpoint.replace("/chat/completions", "/models");
    let mut headers = BTreeMap::new();
    headers.insert("Accept".to_string(), "application/json".to_string());
    headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
    Ok(ProviderHttpRequest {
        method: "GET".to_string(),
        url: models_endpoint,
        headers,
        body: String::new(),
        timeout_ms,
        max_response_bytes: None,
    })
}

/// Runs the openai responses request body operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn openai_responses_request_body(request: &ModelRequest) -> Result<String> {
    openai_responses_request_body_with_stream(request, false)
}

/// Runs the openai responses request body with stream operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn openai_responses_request_body_with_stream(
    request: &ModelRequest,
    stream: bool,
) -> Result<String> {
    validate_non_empty("OpenAI model", &request.model)?;
    let rendered = openai_render_request_messages(request)?;
    let mut body = serde_json::json!({
        "model": request.model,
        "instructions": rendered.instructions,
        "input": rendered.input,
        "prompt_cache_key": openai_prompt_cache_key(request),
        "parallel_tool_calls": false,
        "store": false,
        "stream": stream
    });
    if let Some(response_format) = openai_response_format(request) {
        body["text"] = serde_json::json!({
            "format": response_format
        });
    }
    if let Some(effort) = request
        .reasoning_effort
        .as_deref()
        .filter(|effort| !effort.is_empty())
    {
        body["reasoning"] = serde_json::json!({ "effort": effort });
    }
    if let Some(service_tier) =
        openai_service_tier_for_latency_preference(request.latency_preference.as_deref())?
    {
        body["service_tier"] = serde_json::json!(service_tier);
    }
    if let Some(max_output_tokens) = request
        .max_output_tokens
        .filter(|max_output_tokens| *max_output_tokens > 0)
    {
        body["max_output_tokens"] = serde_json::json!(max_output_tokens);
    }
    if let Some(retention) = request
        .prompt_cache_retention
        .as_deref()
        .filter(|retention| !retention.is_empty())
    {
        match retention {
            "in_memory" => {
                if openai_model_defaults_to_extended_prompt_cache_retention(&request.model) {
                    return Err(MezError::invalid_args(format!(
                        "OpenAI prompt_cache_retention \"in_memory\" is not supported for model {}; omit the option or use 24h",
                        request.model
                    )));
                }
            }
            "24h" => {
                if openai_model_supports_extended_prompt_cache_retention(&request.model) {
                    body["prompt_cache_retention"] = serde_json::json!(retention);
                } else {
                    return Err(MezError::invalid_args(format!(
                        "OpenAI prompt_cache_retention \"24h\" is not supported for model {}; omit the option or use in_memory",
                        request.model
                    )));
                }
            }
            other => {
                return Err(MezError::invalid_args(format!(
                    "OpenAI prompt_cache_retention must be in_memory or 24h, got {other:?}"
                )));
            }
        }
    }
    if request.interaction_kind == ModelInteractionKind::AutoSizing {
        body["tool_choice"] = serde_json::json!("none");
    } else {
        let surface = openai_maap_tool_surface_for_request(request);
        body["tools"] = serde_json::json!(openai_maap_action_batch_tools(request));
        body["tool_choice"] = serde_json::json!({
            "type": "function",
            "name": surface.tool_name()
        });
    }
    serde_json::to_string(&body).map_err(|error| {
        MezError::invalid_state(format!("OpenAI request encoding failed: {error}"))
    })
}
/// Maps Mezzanine latency preferences to OpenAI Responses service tiers.
fn openai_service_tier_for_latency_preference(
    preference: Option<&str>,
) -> Result<Option<&'static str>> {
    match preference.map(str::trim).filter(|value| !value.is_empty()) {
        Some("slow") | Some("default") => Ok(Some("default")),
        None => Ok(None),
        Some("fast") => Ok(Some("priority")),
        Some(other) => Err(MezError::invalid_args(format!(
            "OpenAI latency_preference must be slow, default, or fast, got {other:?}"
        ))),
    }
}

/// Reports whether one OpenAI model id is known to support extended prompt
/// cache retention.
///
/// Most supported model families still default to `in_memory`, but current
/// `gpt-5.5` and future GPT-family models default to extended retention and do
/// not accept an explicit `in_memory` request. The explicit `24h` request field
/// is accepted only for documented model families to avoid provider-side
/// unsupported-parameter failures on models that still support automatic prompt
/// caching without extended retention.
fn openai_model_supports_extended_prompt_cache_retention(model: &str) -> bool {
    let model = model.trim();
    openai_model_defaults_to_extended_prompt_cache_retention(model)
        || openai_model_matches_snapshot_family(model, "gpt-4.1")
        || openai_model_matches_snapshot_family(model, "gpt-5")
        || openai_model_matches_snapshot_family(model, "gpt-5-codex")
        || openai_model_matches_snapshot_family(model, "gpt-5.1")
        || openai_model_matches_snapshot_family(model, "gpt-5.1-codex")
        || openai_model_matches_snapshot_family(model, "gpt-5.1-codex-max")
        || openai_model_matches_snapshot_family(model, "gpt-5.1-codex-mini")
        || openai_model_matches_snapshot_family(model, "gpt-5.1-chat-latest")
        || openai_model_matches_snapshot_family(model, "gpt-5.2")
        || openai_model_matches_snapshot_family(model, "gpt-5.4")
}

/// Returns true when OpenAI documents extended retention as the default policy.
fn openai_model_defaults_to_extended_prompt_cache_retention(model: &str) -> bool {
    openai_gpt_model_version_at_least(model.trim(), 5, 5)
}

/// Matches an OpenAI model family exactly or by dated snapshot suffix.
///
/// This deliberately does not treat arbitrary named variants as members of a
/// documented family. For example, `gpt-5.4-2026-01-01` matches `gpt-5.4`, but
/// `gpt-5.4-mini` must be listed separately before Mezzanine sends `24h`.
fn openai_model_matches_snapshot_family(model: &str, family: &str) -> bool {
    model == family
        || model
            .strip_prefix(family)
            .and_then(|suffix| suffix.strip_prefix('-'))
            .and_then(|suffix| suffix.chars().next())
            .is_some_and(|first| first.is_ascii_digit())
}

/// Parses GPT-family versions and compares them with a minimum version.
fn openai_gpt_model_version_at_least(model: &str, min_major: u16, min_minor: u16) -> bool {
    let Some(rest) = model.strip_prefix("gpt-") else {
        return false;
    };
    let version = rest.split('-').next().unwrap_or_default();
    let mut parts = version.split('.');
    let Some(major) = parts.next().and_then(|part| part.parse::<u16>().ok()) else {
        return false;
    };
    let minor = parts
        .next()
        .and_then(|part| part.parse::<u16>().ok())
        .unwrap_or(0);
    major > min_major || (major == min_major && minor >= min_minor)
}

/// Provider-specific rendering of Mezzanine model messages for OpenAI Responses.
#[derive(Debug, Clone)]
struct OpenAiRenderedMessages {
    /// Joined Responses `instructions` value.
    instructions: String,
    /// Responses `input` messages.
    input: Vec<serde_json::Value>,
    /// Input messages included in the stable reusable prefix.
    stable_input: Vec<serde_json::Value>,
    /// Input messages that belong to the volatile suffix.
    volatile_input: Vec<serde_json::Value>,
}

/// Renders request messages and captures canonical stable-prefix material.
fn openai_render_request_messages(request: &ModelRequest) -> Result<OpenAiRenderedMessages> {
    let mut instructions = Vec::new();
    let mut input = Vec::new();
    let mut stable_input = Vec::new();
    let mut volatile_input = Vec::new();
    let mut stable_input_open = true;
    for message in &request.messages {
        if openai_message_belongs_in_instructions(message) {
            instructions.push(message.content.clone());
            continue;
        }

        openai_push_input_message(
            message,
            &mut input,
            &mut stable_input,
            &mut volatile_input,
            &mut stable_input_open,
        );
    }
    if let Some(message) = openai_allowed_action_surface_message(request) {
        openai_push_input_message(
            &message,
            &mut input,
            &mut stable_input,
            &mut volatile_input,
            &mut stable_input_open,
        );
    }
    if input.is_empty() {
        return Err(MezError::invalid_args(
            "OpenAI Responses request requires at least one user or tool input message",
        ));
    }
    let instructions = instructions.join("\n\n");
    Ok(OpenAiRenderedMessages {
        instructions,
        input,
        stable_input,
        volatile_input,
    })
}

/// Adds one rendered input message to both provider input and cache diagnostics.
fn openai_push_input_message(
    message: &ModelMessage,
    input: &mut Vec<serde_json::Value>,
    stable_input: &mut Vec<serde_json::Value>,
    volatile_input: &mut Vec<serde_json::Value>,
    stable_input_open: &mut bool,
) {
    let value = openai_input_message_value(message);
    if *stable_input_open && openai_message_stable_prefix_eligible(message) {
        stable_input.push(value.clone());
    } else {
        *stable_input_open = false;
        volatile_input.push(value.clone());
    }
    input.push(value);
}

/// Returns true when a message should be rendered into OpenAI `instructions`.
fn openai_message_belongs_in_instructions(message: &ModelMessage) -> bool {
    message.role == ModelMessageRole::System
}

/// Renders one non-instruction message into OpenAI Responses input shape.
fn openai_input_message_value(message: &ModelMessage) -> serde_json::Value {
    match message.role {
        ModelMessageRole::Assistant => serde_json::json!({
            "role": "assistant",
            "content": [
                {
                    "type": "output_text",
                    "text": message.content
                }
            ]
        }),
        ModelMessageRole::Developer => serde_json::json!({
            "role": "developer",
            "content": [
                {
                    "type": "input_text",
                    "text": message.content
                }
            ]
        }),
        ModelMessageRole::System => serde_json::json!({
            "role": "system",
            "content": [
                {
                    "type": "input_text",
                    "text": message.content
                }
            ]
        }),
        ModelMessageRole::User => serde_json::json!({
            "role": "user",
            "content": [
                {
                    "type": "input_text",
                    "text": message.content
                }
            ]
        }),
        ModelMessageRole::Tool => serde_json::json!({
            "role": "user",
            "content": [
                {
                    "type": "input_text",
                    "text": openai_tool_result_input_text(message)
                }
            ]
        }),
    }
}

/// Renders Mezzanine tool/action evidence through an OpenAI-supported message
/// role while preserving its provenance in-band.
fn openai_tool_result_input_text(message: &ModelMessage) -> String {
    let marker = match message.source {
        ContextSourceKind::ActionResult => "[current action result]",
        ContextSourceKind::TranscriptTool => "[historical tool result]",
        _ => "[tool result]",
    };
    format!(
        "{marker}\n\
         This is executed Mezzanine action output, not a new user request.\n\
         {}",
        message.content
    )
}

/// Returns whether a rendered input message belongs in the reusable prefix.
fn openai_message_stable_prefix_eligible(message: &ModelMessage) -> bool {
    if openai_message_is_volatile_controller_state(message) {
        return false;
    }
    match message.source {
        ContextSourceKind::System
        | ContextSourceKind::DeveloperInstruction
        | ContextSourceKind::Configuration
        | ContextSourceKind::ProjectGuidance
        | ContextSourceKind::Memory
        | ContextSourceKind::Transcript
        | ContextSourceKind::TranscriptUser
        | ContextSourceKind::TranscriptAssistant => true,
        ContextSourceKind::Policy => !message.content.starts_with("[scheduler state]\n"),
        ContextSourceKind::UserInstruction
        | ContextSourceKind::LocalMessage
        | ContextSourceKind::TranscriptTool
        | ContextSourceKind::EvidenceLedger
        | ContextSourceKind::ActionResult => false,
    }
}

/// Builds the late controller instruction that makes the current executable
/// surface visible in model context.
fn openai_allowed_action_surface_message(request: &ModelRequest) -> Option<ModelMessage> {
    if request.interaction_kind == ModelInteractionKind::AutoSizing {
        return None;
    }
    let allowed_actions = request.allowed_actions.action_type_names().join(",");
    let selected_tool = openai_maap_tool_surface_for_request(request).tool_name();
    Some(ModelMessage {
        role: ModelMessageRole::Developer,
        source: ContextSourceKind::LocalMessage,
        content: format!(
            "[allowed action surface]\n\
             interaction_kind={}\n\
             allowed_actions={allowed_actions}\n\
             active_function_tool={selected_tool}\n\
             This controller state is authoritative for action eligibility. \
             OpenAI may receive a cache-stable list of inactive MAAP tools, but tool_choice selects only active_function_tool for this request. \
             Emit only action objects whose type appears in allowed_actions and is present in the selected function schema. \
             Treat [current action result] and [action_result ...] messages as current execution evidence. If they already satisfy the task, emit say with status final instead of requesting capability or rerunning actions to reconfirm them. \
             Model-selected skill lookup/loading is disabled; do not emit request_skills or call_skill. Users can still invoke skills explicitly with $<skill-name> syntax before this request is built. \
             If the needed action type is absent and request_capability appears in allowed_actions, emit request_capability immediately for the needed coarse capability; do not spend the response on a plan or progress message. \
             If no listed action can make progress, emit say with status blocked or final. \
             Disallowed action types are rejected by Mezzanine and waste a recovery attempt.",
            request.interaction_kind.as_str(),
        ),
    })
}

/// Returns true for late controller state that should never enter the stable prefix.
fn openai_message_is_volatile_controller_state(message: &ModelMessage) -> bool {
    let content = message.content.trim_start();
    content.starts_with("[capability ")
        || content.starts_with("[capability decisions]")
        || content.starts_with("[controller failure summary]")
        || content.starts_with(OPENAI_CONTEXT_COMPACTED_PREFIX)
}

/// Builds canonical provider-visible stable-prefix material.
#[cfg(test)]
fn openai_stable_prefix_material(
    instructions: &str,
    stable_input: &[serde_json::Value],
) -> serde_json::Result<String> {
    serde_json::to_string(&serde_json::json!({
        "cache_family": "responses-prefix-v2",
        "instructions": instructions,
        "stable_input": stable_input,
    }))
}

/// Runs the parse provider native maap action batch operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_provider_native_maap_action_batch(
    raw_text: &str,
    request: &ModelRequest,
) -> Result<Option<MaapBatch>> {
    let trimmed = raw_text.trim();
    if trimmed.starts_with('{') {
        parse_maap_action_batch_json_for_turn(trimmed, &request.turn_id, &request.agent_id)
            .map(Some)
            .map_err(|error| provider_maap_parse_error(error, raw_text))
    } else {
        Ok(None)
    }
}

/// Runs the openai maap response format operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn openai_response_format(request: &ModelRequest) -> Option<serde_json::Value> {
    if request.interaction_kind == ModelInteractionKind::AutoSizing {
        return Some(openai_auto_sizing_response_format());
    }
    None
}

/// Builds the OpenAI structured-output schema for internal auto-sizing
/// decisions.
fn openai_auto_sizing_response_format() -> serde_json::Value {
    serde_json::json!({
        "type": "json_schema",
        "name": "mezzanine_auto_sizing_decision",
        "description": "Internal Mezzanine turn model and reasoning sizing decision.",
        "strict": true,
        "schema": {
            "type": "object",
            "properties": {
                "version": { "type": "integer", "enum": [1] },
                "size": { "type": "string", "enum": ["small", "medium", "large"] },
                "reasoning_effort": {
                    "type": "string",
                    "enum": ["low", "medium", "high", "xhigh"]
                },
                "confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                "rationale": {
                    "type": "string",
                    "description": "Short non-secret explanation suitable for an agent status log."
                }
            },
            "required": ["version", "size", "reasoning_effort", "confidence", "rationale"],
            "additionalProperties": false
        }
    })
}

/// Runs the parse openai responses provider body operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_openai_responses_provider_body(
    body: &str,
    fallback_model: &str,
    stream: bool,
) -> Result<(String, String, ModelTokenUsage)> {
    if stream {
        parse_openai_responses_stream_body(body, fallback_model)
    } else {
        parse_openai_responses_http_body(body, fallback_model)
    }
}

/// Runs the parse openai responses http body operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn parse_openai_responses_http_body(
    body: &str,
    fallback_model: &str,
) -> Result<(String, String, ModelTokenUsage)> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|error| {
        MezError::invalid_state(format!("OpenAI response was not JSON: {error}"))
    })?;
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("OpenAI response contained an error");
        return Err(MezError::invalid_state(message)
            .with_provider_failure_json(openai_provider_failure_event_json(&value)));
    }
    let model = value
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(fallback_model)
        .to_string();
    let raw_text = collect_openai_maap_function_call_arguments(&value)?
        .or_else(|| {
            value
                .get("output_text")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| collect_openai_output_text(&value));
    let Some(raw_text) = raw_text else {
        return Err(MezError::invalid_state(
            "OpenAI response did not contain text or MAAP function-call output",
        ));
    };
    let usage = openai_token_usage_from_response_value(&value);
    Ok((model, raw_text, usage))
}

/// Runs the parse openai responses stream body operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn parse_openai_responses_stream_body(
    body: &str,
    fallback_model: &str,
) -> Result<(String, String, ModelTokenUsage)> {
    let events = parse_sse_data_events(body)?;
    let mut model = None;
    let mut completed = false;
    let mut usage = ModelTokenUsage::default();
    let mut function_calls = BTreeMap::<u64, OpenAiFunctionCallAccumulator>::new();
    let mut output_item_chunks = Vec::new();
    let mut delta_chunks = Vec::new();

    for (event_name, data) in events {
        let data = data.trim();
        if data == "[DONE]" {
            completed = true;
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(data).map_err(|error| {
            MezError::invalid_state(format!("OpenAI stream event was not JSON: {error}"))
        })?;
        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
            let message = error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("OpenAI stream contained an error");
            return Err(MezError::invalid_state(message)
                .with_provider_failure_json(openai_provider_failure_event_json(&value)));
        }
        let event_usage = openai_token_usage_from_response_value(&value);
        if !event_usage.is_zero() {
            usage = event_usage;
        }

        let event_type = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .or(event_name.as_deref())
            .unwrap_or_default();
        if model.is_none() {
            model = value
                .get("response")
                .and_then(|response| response.get("model"))
                .or_else(|| value.get("model"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
        }

        match event_type {
            "response.output_item.done" | "response.output_item.added" => {
                if let Some(item) = value.get("item") {
                    collect_openai_maap_function_call_event_item(
                        &mut function_calls,
                        &value,
                        item,
                    )?;
                    if let Some(text) = collect_openai_response_item_text(item) {
                        output_item_chunks.push(text);
                    }
                }
            }
            "response.output_text.delta" => {
                if let Some(delta) = value.get("delta").and_then(serde_json::Value::as_str) {
                    delta_chunks.push(delta.to_string());
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(delta) = value.get("delta").and_then(serde_json::Value::as_str) {
                    let output_index = openai_output_index(&value).unwrap_or_default();
                    push_openai_function_call_argument_delta(
                        function_calls.entry(output_index).or_default(),
                        delta,
                    )?;
                }
            }
            "response.function_call_arguments.done" => {
                let output_index = openai_output_index(&value).unwrap_or_default();
                if let Some(item) = value.get("item") {
                    collect_openai_maap_function_call_event_item(
                        &mut function_calls,
                        &value,
                        item,
                    )?;
                }
                if let Some(arguments) = value
                    .get("arguments")
                    .and_then(serde_json::Value::as_str)
                    .or_else(|| value.get("item").and_then(openai_function_call_arguments))
                {
                    set_openai_function_call_complete_arguments(
                        function_calls.entry(output_index).or_default(),
                        arguments,
                    )?;
                }
            }
            "response.completed" => {
                completed = true;
            }
            "response.failed" => {
                return Err(MezError::invalid_state(openai_stream_event_error_detail(
                    &value,
                    "OpenAI stream failed",
                ))
                .with_provider_failure_json(openai_provider_failure_event_json(&value)));
            }
            "response.incomplete" => {
                return Err(MezError::invalid_state(openai_stream_event_error_detail(
                    &value,
                    "OpenAI stream returned an incomplete response",
                ))
                .with_provider_failure_json(openai_provider_failure_event_json(&value)));
            }
            "message" | "" => {
                if let Some(text) = value
                    .get("output_text")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .or_else(|| collect_openai_output_text(&value))
                {
                    output_item_chunks.push(text);
                }
            }
            _ => {}
        }
    }

    let raw_text = if let Some(arguments) =
        collect_openai_maap_function_call_arguments_from_accumulators(&function_calls)?
    {
        arguments
    } else if output_item_chunks.is_empty() {
        delta_chunks.join("")
    } else {
        output_item_chunks.join("")
    };
    if raw_text.is_empty() {
        return Err(MezError::invalid_state(
            "OpenAI stream did not contain text or MAAP function-call output",
        ));
    }
    if !completed && output_item_chunks.is_empty() && function_calls.is_empty() {
        return Err(MezError::invalid_state(
            "OpenAI stream closed before response.completed",
        ));
    }
    Ok((
        model.unwrap_or_else(|| fallback_model.to_string()),
        raw_text,
        usage,
    ))
}

/// Extracts OpenAI-style token usage from a response or stream event object.
fn openai_token_usage_from_response_value(value: &serde_json::Value) -> ModelTokenUsage {
    let Some(usage) = value
        .get("usage")
        .or_else(|| value.pointer("/response/usage"))
    else {
        return ModelTokenUsage::default();
    };
    ModelTokenUsage {
        input_tokens: openai_usage_u64(usage, &["/input_tokens", "/prompt_tokens"]),
        output_tokens: openai_usage_u64(usage, &["/output_tokens", "/completion_tokens"]),
        reasoning_tokens: openai_usage_u64(
            usage,
            &[
                "/output_tokens_details/reasoning_tokens",
                "/completion_tokens_details/reasoning_tokens",
                "/reasoning_tokens",
            ],
        ),
        cached_input_tokens: openai_cached_input_tokens(usage),
    }
}

/// Returns the first unsigned integer found at one of the supplied JSON paths.
fn openai_usage_u64(value: &serde_json::Value, pointers: &[&str]) -> u64 {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(serde_json::Value::as_u64))
        .unwrap_or(0)
}

/// Returns cached input token accounting across OpenAI-compatible usage shapes.
fn openai_cached_input_tokens(value: &serde_json::Value) -> Option<u64> {
    let mut found = false;
    let total = [
        "/input_tokens_details/cached_tokens",
        "/prompt_tokens_details/cached_tokens",
        "/input_token_details/cached_tokens",
        "/prompt_token_details/cached_tokens",
        "/cached_input_tokens",
        "/cached_prompt_tokens",
        "/cached_tokens",
    ]
    .iter()
    .filter_map(|pointer| value.pointer(pointer).and_then(serde_json::Value::as_u64))
    .fold(0_u64, |total, tokens| {
        found = true;
        total.saturating_add(tokens)
    });
    found.then_some(total)
}

/// Builds a stable, non-secret OpenAI prompt-cache routing key for a request.
fn openai_prompt_cache_key(request: &ModelRequest) -> String {
    let mut material = String::new();
    material.push_str("mezzanine\n");
    material.push_str("prompt_profile=");
    material.push_str(AGENT_PROMPT_PROFILE_NAME);
    material.push('\n');
    material.push_str("prompt_version=");
    material.push_str(&AGENT_PROMPT_PROFILE_VERSION.to_string());
    material.push('\n');
    material.push_str("session_id=");
    material.push_str(
        request
            .prompt_cache_session_id
            .as_deref()
            .unwrap_or("session-unknown"),
    );
    material.push('\n');
    material.push_str("cache_family=responses-routing-v4\n");
    format!("mez-{}", &sha256_hex(material.as_bytes())[..32])
}

/// Returns non-model-visible OpenAI prompt-cache diagnostics for one request.
pub fn openai_prompt_cache_diagnostics_for_request(
    request: &ModelRequest,
) -> Result<OpenAiPromptCacheDiagnostics> {
    validate_non_empty("OpenAI model", &request.model)?;
    let rendered = openai_render_request_messages(request)?;
    let response_format = openai_response_format(request).unwrap_or(serde_json::Value::Null);
    let response_format_text = serde_json::to_string(&response_format).map_err(|error| {
        MezError::invalid_state(format!(
            "OpenAI response-format diagnostics failed: {error}"
        ))
    })?;
    let tools = if request.interaction_kind == ModelInteractionKind::AutoSizing {
        serde_json::json!([])
    } else {
        serde_json::json!(openai_maap_action_batch_tools(request))
    };
    let tools_text = serde_json::to_string(&tools).map_err(|error| {
        MezError::invalid_state(format!("OpenAI tools diagnostics failed: {error}"))
    })?;
    let tool_choice = if request.interaction_kind == ModelInteractionKind::AutoSizing {
        serde_json::json!("none")
    } else {
        let surface = openai_maap_tool_surface_for_request(request);
        serde_json::json!({
            "type": "function",
            "name": surface.tool_name()
        })
    };
    let tool_choice_text = serde_json::to_string(&tool_choice).map_err(|error| {
        MezError::invalid_state(format!("OpenAI tool-choice diagnostics failed: {error}"))
    })?;
    let stable_input_text = serde_json::to_string(&rendered.stable_input).map_err(|error| {
        MezError::invalid_state(format!("OpenAI stable-input diagnostics failed: {error}"))
    })?;
    let volatile_input_text = serde_json::to_string(&rendered.volatile_input).map_err(|error| {
        MezError::invalid_state(format!("OpenAI volatile-input diagnostics failed: {error}"))
    })?;
    let cacheable_prefix = serde_json::to_string(&serde_json::json!({
        "cache_family": "responses-routing-v4",
        "instructions": rendered.instructions,
        "response_format": response_format,
        "tools": tools,
        "tool_choice": tool_choice,
        "stable_input": rendered.stable_input,
    }))
    .map_err(|error| {
        MezError::invalid_state(format!("OpenAI cache-prefix diagnostics failed: {error}"))
    })?;

    Ok(OpenAiPromptCacheDiagnostics {
        prompt_cache_key: openai_prompt_cache_key(request),
        instructions_bytes: rendered.instructions.len(),
        instructions_sha256: sha256_hex(rendered.instructions.as_bytes()),
        response_format_bytes: response_format_text.len(),
        response_format_sha256: sha256_hex(response_format_text.as_bytes()),
        tools_bytes: tools_text.len(),
        tools_sha256: sha256_hex(tools_text.as_bytes()),
        tool_choice_bytes: tool_choice_text.len(),
        tool_choice_sha256: sha256_hex(tool_choice_text.as_bytes()),
        stable_input_bytes: stable_input_text.len(),
        stable_input_sha256: sha256_hex(stable_input_text.as_bytes()),
        volatile_input_bytes: volatile_input_text.len(),
        volatile_input_sha256: sha256_hex(volatile_input_text.as_bytes()),
        cacheable_prefix_bytes: cacheable_prefix.len(),
        cacheable_prefix_sha256: sha256_hex(cacheable_prefix.as_bytes()),
    })
}

/// Returns canonical OpenAI stable-prefix material for tests and diagnostics.
#[cfg(test)]
pub(super) fn openai_stable_prefix_material_for_request(request: &ModelRequest) -> Result<String> {
    let rendered = openai_render_request_messages(request)?;
    openai_stable_prefix_material(&rendered.instructions, &rendered.stable_input).map_err(|error| {
        MezError::invalid_state(format!(
            "OpenAI stable prefix material encoding failed: {error}"
        ))
    })
}

/// Encodes bytes as lower-case SHA-256 hexadecimal text.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Extracts quota usage percentages from provider rate-limit headers.
pub fn provider_quota_usage_from_headers(
    headers: &BTreeMap<String, String>,
) -> Vec<ProviderQuotaUsage> {
    let normalized = headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_string()))
        .collect::<BTreeMap<_, _>>();
    let mut quotas = Vec::new();
    for (header, value) in &normalized {
        let Some(name) = header.strip_prefix("x-ratelimit-limit-") else {
            continue;
        };
        let Some(limit) = provider_header_u64(value) else {
            continue;
        };
        let remaining_header = format!("x-ratelimit-remaining-{name}");
        let Some(remaining) = normalized
            .get(&remaining_header)
            .and_then(|remaining| provider_header_u64(remaining))
        else {
            continue;
        };
        let used = limit.saturating_sub(remaining.min(limit));
        let used_basis_points = if limit == 0 {
            0
        } else {
            ((u128::from(used) * 10_000 + u128::from(limit / 2)) / u128::from(limit))
                .min(u128::from(u32::MAX)) as u32
        };
        quotas.push(ProviderQuotaUsage {
            name: name.to_string(),
            used_basis_points,
            limit,
            remaining,
            reset: normalized
                .get(&format!("x-ratelimit-reset-{name}"))
                .cloned(),
        });
    }
    quotas.sort_by(|left, right| left.name.cmp(&right.name));
    quotas.dedup_by(|left, right| left.name == right.name);
    quotas
}

/// Parses the leading unsigned integer from a provider quota header.
fn provider_header_u64(value: &str) -> Option<u64> {
    let value = value.trim();
    if let Ok(parsed) = value.parse::<u64>() {
        return Some(parsed);
    }
    let normalized = value
        .chars()
        .filter(|character| *character != ',' && *character != '_')
        .collect::<String>();
    if normalized
        .chars()
        .all(|character| character.is_ascii_digit())
    {
        normalized.parse::<u64>().ok()
    } else {
        None
    }
}

/// Runs the parse sse data events operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_sse_data_events(body: &str) -> Result<Vec<(Option<String>, String)>> {
    let mut events = Vec::new();
    for block in body.replace("\r\n", "\n").split("\n\n") {
        let mut event_name = None;
        let mut data_lines = Vec::new();
        for line in block.lines() {
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if let Some(value) = line.strip_prefix("event:") {
                event_name = Some(value.trim().to_string());
            } else if let Some(value) = line.strip_prefix("data:") {
                data_lines.push(value.trim_start().to_string());
            }
        }
        if !data_lines.is_empty() {
            events.push((event_name, data_lines.join("\n")));
        }
    }
    if events.is_empty() {
        return Err(MezError::invalid_state(
            "OpenAI stream response did not contain SSE data events",
        ));
    }
    Ok(events)
}

/// Runs the openai stream event error detail operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn openai_stream_event_error_detail(value: &serde_json::Value, fallback: &str) -> String {
    value
        .pointer("/response/error/message")
        .or_else(|| value.pointer("/response/incomplete_details/reason"))
        .or_else(|| value.pointer("/error/message"))
        .or_else(|| value.get("message"))
        .and_then(serde_json::Value::as_str)
        .map(|message| format!("{fallback}: {message}"))
        .unwrap_or_else(|| fallback.to_string())
}

/// Runs the collect openai output text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn collect_openai_output_text(value: &serde_json::Value) -> Option<String> {
    let mut chunks = Vec::new();
    for item in value.get("output")?.as_array()? {
        if let Some(text) = collect_openai_response_item_text(item) {
            chunks.push(text);
        }
    }
    if chunks.is_empty() {
        None
    } else {
        Some(chunks.join(""))
    }
}

/// Carries Open Ai Function Call Accumulator state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Default)]
struct OpenAiFunctionCallAccumulator {
    /// Stores the name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    name: Option<String>,
    /// Stores the arguments value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    arguments: String,
    /// Stores the complete arguments value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    complete_arguments: Option<String>,
}

/// Runs the collect openai maap function call arguments operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn collect_openai_maap_function_call_arguments(
    value: &serde_json::Value,
) -> Result<Option<String>> {
    let Some(output) = value.get("output").and_then(serde_json::Value::as_array) else {
        return Ok(None);
    };
    let arguments = output
        .iter()
        .filter(|item| {
            item.get("type").and_then(serde_json::Value::as_str) == Some("function_call")
        })
        .filter(|item| {
            openai_function_call_name(item).is_some_and(openai_function_call_name_is_maap)
        })
        .map(|item| {
            let arguments = openai_function_call_arguments(item).ok_or_else(|| {
                MezError::invalid_state("OpenAI MAAP function call did not contain arguments")
            })?;
            openai_function_call_arguments_string(arguments)
        })
        .collect::<Result<Vec<_>>>()?;
    one_openai_maap_function_call_arguments(arguments)
}

/// Runs the collect openai maap function call event item operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn collect_openai_maap_function_call_event_item(
    function_calls: &mut BTreeMap<u64, OpenAiFunctionCallAccumulator>,
    event: &serde_json::Value,
    item: &serde_json::Value,
) -> Result<()> {
    if item.get("type").and_then(serde_json::Value::as_str) != Some("function_call") {
        return Ok(());
    }
    let output_index = openai_output_index(event).unwrap_or_default();
    let entry = function_calls.entry(output_index).or_default();
    if let Some(name) = openai_function_call_name(item) {
        entry.name = Some(name.to_string());
    }
    if let Some(arguments) = openai_function_call_arguments(item)
        && !arguments.is_empty()
    {
        set_openai_function_call_complete_arguments(entry, arguments)?;
    }
    Ok(())
}

/// Runs the collect openai maap function call arguments from accumulators operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn collect_openai_maap_function_call_arguments_from_accumulators(
    function_calls: &BTreeMap<u64, OpenAiFunctionCallAccumulator>,
) -> Result<Option<String>> {
    let arguments = function_calls
        .values()
        .filter(|call| {
            call.name
                .as_deref()
                .is_none_or(openai_function_call_name_is_maap)
        })
        .filter_map(|call| {
            let delta_arguments = if call.arguments.is_empty() {
                None
            } else {
                Some(&call.arguments)
            };
            call.complete_arguments
                .as_ref()
                .filter(|arguments| !arguments.is_empty())
                .or(delta_arguments)
                .cloned()
        })
        .collect::<Vec<_>>();
    one_openai_maap_function_call_arguments(arguments)
}

/// Reports whether an OpenAI function call name is a Mezzanine MAAP carrier.
fn openai_function_call_name_is_maap(name: &str) -> bool {
    name == OPENAI_MAAP_FUNCTION_TOOL_NAME
        || name == OpenAiMaapToolSurface::CurrentRequest.tool_name()
        || OpenAiMaapToolSurface::stable_surfaces()
            .iter()
            .any(|surface| name == surface.tool_name())
}

/// Appends or replaces streaming function-call arguments without unbounded growth.
///
/// Some Responses streaming paths send true deltas, while others send
/// cumulative snapshots in the `delta` field. Replacing when the new value
/// contains the previous buffer as a prefix keeps both shapes correct and
/// prevents repeated snapshots from growing memory without bound.
fn push_openai_function_call_argument_delta(
    call: &mut OpenAiFunctionCallAccumulator,
    delta: &str,
) -> Result<()> {
    if delta.is_empty() {
        return Ok(());
    }
    if !call.arguments.is_empty() && delta.starts_with(&call.arguments) {
        call.arguments.clear();
        call.arguments.push_str(delta);
    } else {
        call.arguments.push_str(delta);
    }
    validate_openai_function_call_argument_size(&call.arguments)
}

/// Stores complete function-call arguments after enforcing the provider cap.
fn set_openai_function_call_complete_arguments(
    call: &mut OpenAiFunctionCallAccumulator,
    arguments: &str,
) -> Result<()> {
    validate_openai_function_call_argument_size(arguments)?;
    call.complete_arguments = Some(arguments.to_string());
    Ok(())
}

/// Copies function-call arguments only after enforcing the provider cap.
fn openai_function_call_arguments_string(arguments: &str) -> Result<String> {
    validate_openai_function_call_argument_size(arguments)?;
    Ok(arguments.to_string())
}

/// Rejects oversized native MAAP argument buffers before they can dominate memory.
fn validate_openai_function_call_argument_size(arguments: &str) -> Result<()> {
    if arguments.len() > OPENAI_FUNCTION_CALL_ARGUMENT_LIMIT_BYTES {
        return Err(MezError::invalid_state(format!(
            "OpenAI MAAP function call arguments exceeded {} bytes",
            OPENAI_FUNCTION_CALL_ARGUMENT_LIMIT_BYTES
        )));
    }
    Ok(())
}

/// Runs the one openai maap function call arguments operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn one_openai_maap_function_call_arguments(arguments: Vec<String>) -> Result<Option<String>> {
    match arguments.len() {
        0 => Ok(None),
        1 => Ok(arguments.into_iter().next()),
        _ => Err(MezError::invalid_state(
            "OpenAI response contained multiple MAAP function calls in one turn",
        )),
    }
}

/// Runs the openai function call name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn openai_function_call_name(item: &serde_json::Value) -> Option<&str> {
    item.get("name")
        .or_else(|| item.pointer("/function/name"))
        .and_then(serde_json::Value::as_str)
}

/// Runs the openai function call arguments operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn openai_function_call_arguments(item: &serde_json::Value) -> Option<&str> {
    item.get("arguments")
        .or_else(|| item.pointer("/function/arguments"))
        .and_then(serde_json::Value::as_str)
}

/// Runs the openai output index operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn openai_output_index(value: &serde_json::Value) -> Option<u64> {
    value
        .get("output_index")
        .and_then(serde_json::Value::as_u64)
}

/// Runs the collect openai response item text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn collect_openai_response_item_text(item: &serde_json::Value) -> Option<String> {
    let content = item.get("content").and_then(serde_json::Value::as_array)?;
    let mut chunks = Vec::new();
    for content_item in content {
        let item_type = content_item
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if matches!(item_type, "output_text" | "text")
            && let Some(text) = content_item.get("text").and_then(serde_json::Value::as_str)
        {
            chunks.push(text.to_string());
        }
    }
    if chunks.is_empty() {
        None
    } else {
        Some(chunks.join(""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies third-party MCP input schemas are normalized into the OpenAI
    /// strict-schema subset before they are embedded in MAAP function tools.
    ///
    /// Some MCP servers advertise ordinary JSON Schema `format` annotations
    /// such as `uri`. The OpenAI Responses function-tool validator rejects at
    /// least some of those values, so the provider adapter must strip the
    /// annotation recursively while preserving the structural object shape and
    /// required-field expansion used by strict tools.
    #[test]
    fn normalize_openai_strict_schema_strips_nested_format_annotations() {
        let normalized = normalize_openai_strict_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "data": {
                    "type": "object",
                    "properties": {
                        "uri": {
                            "type": "string",
                            "format": "uri"
                        }
                    }
                },
                "items": {
                    "type": "array",
                    "items": {
                        "type": "string",
                        "format": "uri-reference"
                    }
                },
                "choice": {
                    "anyOf": [
                        {
                            "type": "string",
                            "format": "email"
                        },
                        {
                            "type": "null"
                        }
                    ]
                }
            }
        }));

        assert_eq!(
            normalized.pointer("/properties/data/properties/uri/format"),
            None
        );
        assert_eq!(normalized.pointer("/properties/items/items/format"), None);
        assert_eq!(
            normalized.pointer("/properties/choice/anyOf/0/format"),
            None
        );
        assert_eq!(
            normalized.pointer("/properties/data/required"),
            Some(&serde_json::json!(["uri"]))
        );
        assert_eq!(
            normalized.pointer("/required"),
            Some(&serde_json::json!(["choice", "data", "items"]))
        );
        assert_eq!(
            normalized.pointer("/properties/data/additionalProperties"),
            Some(&serde_json::json!(false))
        );
    }

    /// Verifies an MCP tool schema containing `format: uri` can be embedded in
    /// the OpenAI MCP action surface without leaking the rejected annotation.
    ///
    /// This covers the provider error path where OpenAI rejected
    /// `submit_maap_mcp_actions` because a configured MCP server advertised a
    /// nested `arguments.data.uri` field with a URI format annotation.
    #[test]
    fn openai_mcp_action_tool_schema_omits_rejected_uri_format() {
        let tool = McpPromptTool {
            server_id: "everything".to_string(),
            tool_name: "echo".to_string(),
            description: "Echo test input".to_string(),
            approval_required: false,
            input_schema_json: serde_json::json!({
                            "type": "object",
                            "properties": {
                                "data": {
                                    "type": "object",
                                    "properties": {
                                        "uri": {
                                            "type": "string",
                                            "format": "uri"
                }
            }

                                }
                            }
                        })
            .to_string(),
        };
        let schema = maap_mcp_call_action_schema_for_tool(&tool);

        assert_eq!(
            schema.pointer("/properties/arguments/properties/data/properties/uri/format"),
            None
        );
        assert_eq!(
            schema.pointer("/properties/arguments/properties/data/required"),
            Some(&serde_json::json!(["uri"]))
        );
        assert_eq!(
            schema.pointer("/properties/arguments/required"),
            Some(&serde_json::json!(["data"]))
        );
    }
}
