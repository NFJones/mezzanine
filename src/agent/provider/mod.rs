//! Agent Provider implementation.
//!
//! This module owns the agent provider boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AgentCapability, AllowedAction, AllowedActionSet, AuthStore, BTreeMap, ExposeSecret, MaapBatch,
    McpPromptTool, MezError, ModelInteractionKind, ModelMessageRole, ModelRequest,
    ProviderTranscriptEvent, Result, SecretString, parse_fenced_maap_action_batch_for_turn,
    parse_maap_action_batch_json_for_turn, validate_non_empty,
};
use crate::auth::{AuthCredentialKind, AuthMetadata};
use std::future::Future;
use std::pin::Pin;

// Model provider traits and OpenAI Responses adapter.

mod cache;
mod catalog;
mod deepseek;
mod errors;
mod http;
mod openai_request;
mod quota;
mod response;
mod schema;
#[cfg(test)]
pub(crate) use cache::openai_stable_prefix_material_for_request;
pub use cache::{OpenAiPromptCacheDiagnostics, openai_prompt_cache_diagnostics_for_request};
use catalog::provider_catalog_reasoning_levels;
pub use catalog::{
    ProviderModelCatalog, ProviderModelInfo, openai_default_reasoning_levels_for_model,
    openai_models_endpoint_for_responses_endpoint, openai_responses_endpoint_for_base_url,
    parse_openai_models_http_body,
};
pub use deepseek::build_deepseek_chat_completions_http_request;
use deepseek::{
    build_deepseek_chat_completions_http_request_with_strategy, build_deepseek_models_http_request,
    deepseek_chat_completions_endpoint_for_base_url, deepseek_effective_stream,
    deepseek_maap_request_strategy, deepseek_should_retry_with_forced_maap,
    parse_deepseek_chat_completions_http_response,
};
use errors::{
    openai_provider_error_detail, openai_provider_failure_json, provider_maap_parse_error,
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
pub use openai_request::openai_responses_request_body;
use openai_request::openai_responses_request_body_with_stream;
pub use quota::{ProviderQuotaUsage, provider_quota_usage_from_headers};
pub use response::parse_openai_responses_http_body;
use response::parse_openai_responses_provider_body;
#[cfg(test)]
pub(crate) use response::parse_openai_responses_stream_body;
#[cfg(test)]
use schema::{maap_mcp_call_action_schema_for_tool, normalize_openai_strict_schema};

/// Default direct OpenAI Responses API endpoint used with API-key auth.
pub const OPENAI_RESPONSES_ENDPOINT: &str = "https://api.openai.com/v1/responses";
/// Default direct OpenAI model catalog endpoint used with API-key auth.
pub const OPENAI_MODELS_ENDPOINT: &str = "https://api.openai.com/v1/models";
/// Default DeepSeek Chat Completions API endpoint.
pub const DEEPSEEK_CHAT_COMPLETIONS_ENDPOINT: &str = "https://api.deepseek.com/chat/completions";
/// Default DeepSeek models listing endpoint.
#[allow(dead_code)]
pub const DEEPSEEK_MODELS_ENDPOINT: &str = "https://api.deepseek.com/models";
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
    /// Hidden provider-native transcript events required for future requests.
    ///
    /// Provider adapters populate this only when the provider API requires
    /// non-neutral message fields to be replayed for multi-turn correctness.
    pub provider_transcript_events: Vec<ProviderTranscriptEvent>,
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

/// Returns whether a DeepSeek request must produce a structured MAAP batch.
fn deepseek_response_requires_maap(request: &ModelRequest) -> bool {
    request.interaction_kind != ModelInteractionKind::AutoSizing
        && !request.allowed_actions.actions.is_empty()
}

/// Converts a successful DeepSeek response without required MAAP into a
/// repairable malformed-output provider error.
fn deepseek_required_maap_response(
    response: ModelResponse,
    request: &ModelRequest,
) -> Result<ModelResponse> {
    if response.action_batch.is_some() || !deepseek_response_requires_maap(request) {
        return Ok(response);
    }
    Err(provider_maap_parse_error(
        MezError::invalid_args(format!(
            "DeepSeek response did not call the {OPENAI_MAAP_FUNCTION_TOOL_NAME} tool or return a MAAP JSON object"
        )),
        &response.raw_text,
    ))
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
        deepseek_required_maap_response(parsed, request)
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
            deepseek_required_maap_response(parsed, request)
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
/// are expanded to the provider's documented Chat Completions endpoint.
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
    if let Some(base_url) = base_url_override.filter(|e| !e.trim().is_empty()) {
        provider =
            provider.with_endpoint(deepseek_chat_completions_endpoint_for_base_url(base_url)?);
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
            provider_transcript_events: Vec::new(),
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
                provider_transcript_events: Vec::new(),
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
