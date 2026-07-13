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

mod anthropic;
mod cache;
mod catalog;
mod chat_completions;
mod claude_code;
mod deepseek;
mod errors;
mod http;
mod openai_chat_completions;
mod openai_request;
mod response;
mod schema;
use anthropic::AnthropicMessagesDialect;
#[cfg(test)]
pub(crate) use cache::openai_stable_prefix_material_for_request;
pub use cache::{OpenAiPromptCacheDiagnostics, openai_prompt_cache_diagnostics_for_request};
use catalog::provider_catalog_reasoning_levels;
pub use catalog::{
    ProviderModelCatalog, ProviderModelInfo, openai_default_reasoning_levels_for_model,
    openai_models_endpoint_for_responses_endpoint, openai_responses_endpoint_for_base_url,
    parse_openai_models_http_body,
};
pub use chat_completions::ChatCompletionsProvider;
pub use claude_code::ClaudeCodeProvider;
use deepseek::DeepSeekChatCompletionsDialect;
pub use deepseek::build_deepseek_chat_completions_http_request;
pub(crate) use errors::{
    ProviderErrorRetryClass, provider_error_retry_class, provider_error_retry_class_from_parts,
    provider_event_error_from_parts, provider_event_error_kind,
};
use errors::{
    openai_provider_error_detail, openai_provider_failure_json, provider_maap_parse_error,
};
#[cfg(test)]
pub(crate) use errors::{
    provider_error_is_context_limit_exceeded, provider_error_is_output_limit_exceeded,
};
#[cfg(test)]
pub use http::ProviderHttpTransport;
pub use http::{
    AsyncProviderHttpTransport, DEFAULT_PROVIDER_MAX_RESPONSE_BYTES, DEFAULT_PROVIDER_TIMEOUT_MS,
    ProviderHttpRequest, ProviderHttpResponse, ReqwestProviderHttpTransport,
};
#[cfg(test)]
use mez_agent::ANTHROPIC_MESSAGES_API;
use mez_agent::resolve_provider_api;
pub use mez_agent::{
    AgentContextUsageSnapshot, CLAUDE_CODE_API, DEEPSEEK_CHAT_COMPLETIONS_API, ModelTokenUsage,
    ModelTokenUsageKey, OPENAI_CHAT_COMPLETIONS_API, OPENAI_RESPONSES_API,
    ProviderApiCompatibility,
};
pub use mez_agent::{ProviderQuotaUsage, provider_quota_usage_from_headers};
use openai_chat_completions::OpenAiChatCompletionsDialect;
pub use openai_request::openai_responses_request_body;
use openai_request::openai_responses_request_body_with_stream;
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
/// Default Anthropic Messages API endpoint.
#[allow(dead_code)]
pub const ANTHROPIC_MESSAGES_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
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
/// DeepSeek shim function tool name used for capability routing turns.
pub const DEEPSEEK_CAPABILITY_MAAP_FUNCTION_TOOL_NAME: &str = "mez_decide_capability";
/// DeepSeek shim function tool name used for response-only turns.
pub const DEEPSEEK_RESPOND_MAAP_FUNCTION_TOOL_NAME: &str = "mez_respond";
/// DeepSeek shim function tool name used for executable action turns.
pub const DEEPSEEK_ACTIONS_MAAP_FUNCTION_TOOL_NAME: &str = "mez_take_actions";

/// Resolves an optional configured API id against one provider kind.
pub fn effective_provider_api(kind: &str, api: Option<&str>) -> Result<ProviderApiCompatibility> {
    resolve_provider_api(kind, api).map_err(|error| MezError::config(error.to_string()))
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
    /// Provider-reported usage for the last concrete request that produced this
    /// response when `usage` carries an accumulated total.
    pub latest_request_usage: Option<ModelTokenUsage>,
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

pub use mez_agent::ProviderCapabilities;

/// Carries Open Ai Responses Provider state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone)]
pub struct OpenAiResponsesProvider<T> {
    /// Stores the configured provider id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) provider_id: String,
    /// Stores the optional bearer credential for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) api_key: Option<SecretString>,
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
    /// such as the ChatGPT account id. When present, the bearer credential
    /// remains stored in the dedicated `Authorization` header.
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
        Self::with_optional_endpoint_headers_and_stream(
            Some(api_key.into()),
            endpoint,
            timeout_ms,
            extra_headers,
            stream,
            transport,
        )
    }

    /// Creates a provider with no bearer credential for compatible local APIs.
    pub fn without_auth(
        endpoint: impl Into<String>,
        timeout_ms: u64,
        extra_headers: BTreeMap<String, String>,
        stream: bool,
        transport: T,
    ) -> Result<Self> {
        Self::with_optional_endpoint_headers_and_stream(
            None,
            endpoint,
            timeout_ms,
            extra_headers,
            stream,
            transport,
        )
    }

    /// Creates a provider with optional bearer authentication.
    pub fn with_optional_endpoint_headers_and_stream(
        api_key: Option<SecretString>,
        endpoint: impl Into<String>,
        timeout_ms: u64,
        extra_headers: BTreeMap<String, String>,
        stream: bool,
        transport: T,
    ) -> Result<Self> {
        let endpoint = endpoint.into();
        if let Some(api_key) = api_key.as_ref() {
            validate_non_empty("OpenAI provider bearer credential", api_key.expose_secret())?;
        }
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
            provider_id: "openai".to_string(),
            api_key,
            endpoint,
            extra_headers,
            stream,
            timeout_ms,
            transport,
        })
    }

    /// Returns the configured provider id guarded by this provider instance.
    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    /// Overrides the runtime provider identity accepted by request guards.
    pub fn with_provider_id(mut self, provider_id: impl Into<String>) -> Result<Self> {
        let provider_id = provider_id.into();
        validate_non_empty("provider id", &provider_id)?;
        self.provider_id = provider_id;
        Ok(self)
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

/// Alias for the shared Chat Completions provider when used for DeepSeek.
pub type DeepSeekChatCompletionsProvider<T> =
    ChatCompletionsProvider<T, DeepSeekChatCompletionsDialect>;
/// Alias for the shared transport shell when used for Anthropic Messages.
pub type AnthropicMessagesProvider<T> = ChatCompletionsProvider<T, AnthropicMessagesDialect>;
/// Alias for the shared Chat Completions provider when used for named
/// OpenAI-compatible backends.
pub type OpenAiCompatibleChatCompletionsProvider<T> =
    ChatCompletionsProvider<T, OpenAiChatCompletionsDialect>;

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
    openai_responses_provider_from_auth_store_with_provider_options(
        auth_store,
        "openai",
        base_url_override,
        provider_options,
        timeout_ms,
        transport,
    )
}

/// Builds an OpenAI Responses-compatible provider from auth metadata.
///
/// The configured provider name scopes credentials and request guards, while
/// the compatibility layer reuses the OpenAI Responses wire implementation.
pub fn openai_responses_provider_from_auth_store_with_provider_options<T>(
    auth_store: &AuthStore,
    provider_name: &str,
    base_url_override: Option<&str>,
    provider_options: &BTreeMap<String, String>,
    timeout_ms: u64,
    transport: T,
) -> Result<OpenAiResponsesProvider<T>> {
    let endpoint = base_url_override
        .filter(|endpoint| !endpoint.trim().is_empty())
        .map(openai_responses_endpoint_for_base_url)
        .transpose()?
        .unwrap_or_else(|| OPENAI_RESPONSES_ENDPOINT.to_string());
    let Some(metadata) = auth_store.read_metadata_for_provider(provider_name)? else {
        return OpenAiResponsesProvider::without_auth(
            endpoint,
            timeout_ms,
            BTreeMap::new(),
            false,
            transport,
        )
        .and_then(|provider| provider.with_provider_id(provider_name));
    };
    match metadata.credential_kind {
        AuthCredentialKind::ApiKey => {
            let credential = auth_store.provider_secret(provider_name)?;
            OpenAiResponsesProvider::with_endpoint_and_headers(
                credential,
                endpoint,
                timeout_ms,
                openai_direct_api_extra_headers(&metadata, provider_options),
                transport,
            )
            .and_then(|provider| provider.with_provider_id(provider_name))
        }
        AuthCredentialKind::ChatGpt => {
            if provider_name != "openai" {
                return Err(MezError::invalid_state(format!(
                    "OpenAI Responses-compatible provider `{provider_name}` cannot use ChatGPT browser credentials"
                )));
            }
            let credential = auth_store.provider_secret(provider_name)?;
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
            .and_then(|provider| provider.with_provider_id(provider_name))
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
    deepseek_chat_completions_provider_from_auth_store_with_provider_options(
        auth_store,
        "deepseek",
        base_url_override,
        timeout_ms,
        transport,
    )
}

/// Builds a DeepSeek Chat Completions-compatible provider from auth metadata.
///
/// The configured provider name scopes credentials and request guards, while
/// the compatibility layer reuses the DeepSeek Chat Completions wire dialect.
pub fn deepseek_chat_completions_provider_from_auth_store_with_provider_options<T>(
    auth_store: &AuthStore,
    provider_name: &str,
    base_url_override: Option<&str>,
    timeout_ms: u64,
    transport: T,
) -> Result<DeepSeekChatCompletionsProvider<T>> {
    let mut provider = if auth_store
        .read_metadata_for_provider(provider_name)?
        .is_some()
    {
        let credential = auth_store.provider_secret(provider_name)?;
        DeepSeekChatCompletionsProvider::new(credential, transport)?
    } else {
        DeepSeekChatCompletionsProvider::without_auth(transport)?
    }
    .with_provider_id(provider_name)?;
    if let Some(base_url) = base_url_override.filter(|e| !e.trim().is_empty()) {
        let endpoint = provider.chat_endpoint_for_base_url(base_url)?;
        provider = provider.with_endpoint(endpoint);
    }
    provider = provider.with_timeout(timeout_ms);
    Ok(provider)
}

/// Builds an Anthropic Messages provider from auth metadata.
///
/// Anthropic only supports direct API-key authentication in Mez. The configured
/// provider name scopes credential lookup and request guards so multiple named
/// Claude providers can coexist without falling back to the literal
/// `anthropic` provider id.
pub fn anthropic_provider_from_auth_store_with_provider_options<T>(
    auth_store: &AuthStore,
    provider_name: &str,
    base_url_override: Option<&str>,
    provider_options: &BTreeMap<String, String>,
    timeout_ms: u64,
    transport: T,
) -> Result<AnthropicMessagesProvider<T>> {
    let dialect = AnthropicMessagesDialect::from_provider_options(provider_options)?;
    let Some(metadata) = auth_store.read_metadata_for_provider(provider_name)? else {
        return Err(MezError::invalid_state(format!(
            "Anthropic provider `{provider_name}` requires an authenticated API key"
        )));
    };
    if metadata.credential_kind != AuthCredentialKind::ApiKey {
        return Err(MezError::invalid_state(format!(
            "Anthropic provider `{provider_name}` requires API-key credentials"
        )));
    }
    let credential = auth_store.provider_secret(provider_name)?;
    let mut provider = AnthropicMessagesProvider::with_optional_auth_and_dialect(
        Some(credential),
        transport,
        dialect,
    )?
    .with_provider_id(provider_name)?;
    if let Some(base_url) = base_url_override.filter(|e| !e.trim().is_empty()) {
        let endpoint = provider.chat_endpoint_for_base_url(base_url)?;
        provider = provider.with_endpoint(endpoint);
    }
    provider = provider.with_timeout(timeout_ms);
    Ok(provider)
}

/// Builds an OpenAI-compatible Chat Completions provider from auth metadata.
///
/// The provider is scoped by its configured provider name so multiple named
/// compatible backends can coexist while sharing the Chat Completions wire
/// contract. Endpoint overrides are expanded to `/chat/completions` using the
/// same compatibility rules as the DeepSeek adapter.
pub fn openai_compatible_provider_from_auth_store_with_provider_options<T>(
    auth_store: &AuthStore,
    provider_name: &str,
    base_url_override: Option<&str>,
    provider_options: &BTreeMap<String, String>,
    timeout_ms: u64,
    transport: T,
) -> Result<OpenAiCompatibleChatCompletionsProvider<T>> {
    let dialect = OpenAiChatCompletionsDialect::from_provider_options(provider_options)?;
    let api_key = if auth_store
        .read_metadata_for_provider(provider_name)?
        .is_some()
    {
        Some(auth_store.provider_secret(provider_name)?)
    } else {
        None
    };
    let mut provider = OpenAiCompatibleChatCompletionsProvider::with_optional_auth_and_dialect(
        api_key, transport, dialect,
    )?
    .with_provider_id(provider_name)?;
    if let Some(base_url) = base_url_override.filter(|e| !e.trim().is_empty()) {
        let endpoint = provider.chat_endpoint_for_base_url(base_url)?;
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
        self.provider_id()
    }

    /// Runs the list models operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn list_models(&self) -> Result<ProviderModelCatalog> {
        let http_request = build_openai_models_http_request_with_headers(
            self.api_key.as_ref().map(|api_key| api_key.expose_secret()),
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
            self.api_key.as_ref().map(|api_key| api_key.expose_secret()),
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
        let action_batch = if !request.interaction_kind.expects_maap_batch() {
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
            latest_request_usage: None,
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
        self.provider_id()
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
                self.api_key.as_ref().map(|api_key| api_key.expose_secret()),
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
                self.api_key.as_ref().map(|api_key| api_key.expose_secret()),
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
            let action_batch = if !request.interaction_kind.expects_maap_batch() {
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
                latest_request_usage: None,
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
        Some(api_key),
        endpoint,
        &BTreeMap::new(),
        false,
        timeout_ms,
    )
}

/// Builds an OpenAI Responses request with provider-specific extra headers.
///
/// The caller supplies non-secret routing headers only. When a bearer
/// credential is supplied, it is placed in the `Authorization` header.
pub fn build_openai_responses_http_request_with_headers(
    request: &ModelRequest,
    api_key: Option<&str>,
    endpoint: &str,
    extra_headers: &BTreeMap<String, String>,
    stream: bool,
    timeout_ms: u64,
) -> Result<ProviderHttpRequest> {
    if let Some(api_key) = api_key {
        validate_non_empty("OpenAI provider bearer credential", api_key)?;
    }
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
    if let Some(api_key) = api_key {
        headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
    }
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
        Some(api_key),
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
    api_key: Option<&str>,
    responses_endpoint: &str,
    extra_headers: &BTreeMap<String, String>,
    timeout_ms: u64,
) -> Result<ProviderHttpRequest> {
    if let Some(api_key) = api_key {
        validate_non_empty("OpenAI provider bearer credential", api_key)?;
    }
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
    if let Some(api_key) = api_key {
        headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
    }
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
    /// the OpenAI MCP action schema without leaking the rejected annotation.
    ///
    /// This covers the provider error path where OpenAI rejected
    /// `submit_maap_action_batch` because a configured MCP server advertised a
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

    /// Verifies the Anthropic API compatibility identifier round-trips through
    /// the stable parser and formatter used by provider configuration.
    ///
    /// This guards the new first-party Anthropic compatibility surface against
    /// regressions where config accepts the constant but the runtime no longer
    /// resolves it back to the same enum variant.
    #[test]
    fn anthropic_messages_api_id_round_trips() {
        assert_eq!(
            ProviderApiCompatibility::from_id(ANTHROPIC_MESSAGES_API),
            Some(ProviderApiCompatibility::AnthropicMessages)
        );
        assert_eq!(
            ProviderApiCompatibility::AnthropicMessages.as_str(),
            ANTHROPIC_MESSAGES_API
        );
    }

    /// Verifies mixed known and unknown prompt-cache counters remain unknown
    /// after aggregation instead of rendering a misleading partial hit ratio.
    ///
    /// Runtime retry and pane totals use `ModelTokenUsage::add_assign` to
    /// combine multiple provider responses. When any response with token usage
    /// omits cache counters, the aggregate no longer has complete denominator
    /// information for a precise prompt-cache hit or write total.
    #[test]
    fn token_usage_aggregation_preserves_unknown_cache_counters() {
        let mut usage = ModelTokenUsage::default();
        usage.add_assign(ModelTokenUsage {
            input_tokens: 100,
            output_tokens: 10,
            reasoning_tokens: 0,
            cached_input_tokens: Some(60),
            cache_write_input_tokens: Some(20),
        });
        assert_eq!(usage.cache_write_input_tokens, Some(20));
        assert_eq!(usage.billed_input_tokens(), 60);
        assert_eq!(usage.total_tokens(), 130);
        usage.add_assign(ModelTokenUsage {
            input_tokens: 50,
            output_tokens: 5,
            reasoning_tokens: 0,
            cached_input_tokens: None,
            cache_write_input_tokens: None,
        });

        assert_eq!(usage.input_tokens, 150);
        assert_eq!(usage.output_tokens, 15);
        assert_eq!(usage.cached_input_tokens, None);
        assert_eq!(usage.cache_write_input_tokens, None);
        assert_eq!(usage.cached_input_tokens_display(), "unknown");
        assert_eq!(usage.cached_input_hit_ratio_basis_points(), None);
        assert_eq!(usage.cached_input_hit_ratio_display(), "unknown");
    }

    /// Verifies prompt-cache helpers handle providers that report cache hits
    /// outside their ordinary input-token counter.
    ///
    /// Claude Code reports `cache_read_input_tokens` separately from
    /// `input_tokens`, unlike providers whose input total already includes
    /// cache hits. The shared usage helpers must preserve the raw input counter
    /// while still deriving useful billed, total, and cache-hit values.
    #[test]
    fn token_usage_accounts_for_separately_reported_cache_hits() {
        let usage = ModelTokenUsage {
            input_tokens: 2,
            output_tokens: 12,
            reasoning_tokens: 0,
            cached_input_tokens: Some(10_496),
            cache_write_input_tokens: Some(6_112),
        };

        assert_eq!(usage.input_tokens, 2);
        assert_eq!(usage.billed_input_tokens(), 6_114);
        assert_eq!(usage.total_tokens(), 16_622);
        assert_eq!(usage.cached_input_hit_ratio_display(), "63.19%");
    }

    /// Verifies Anthropic provider kinds default to the Anthropic Messages
    /// compatibility layer when no explicit API id is configured.
    ///
    /// This keeps provider-kind resolution aligned with the new built-in
    /// compatibility contract and exercises the user-facing helper that emits
    /// configuration errors for unsupported API ids.
    #[test]
    fn effective_provider_api_defaults_anthropic_kind_to_messages() {
        assert_eq!(
            effective_provider_api("anthropic", None).unwrap(),
            ProviderApiCompatibility::AnthropicMessages
        );
    }

    /// Verifies OpenAI Responses capability metadata only advertises accepted
    /// provider request fields.
    ///
    /// OpenAI prompt caching is automatic for eligible input prefixes. The
    /// adapter must keep cache-retention controls disabled so runtime code does
    /// not treat `prompt_cache_retention` as a supported wire field.
    #[test]
    fn openai_responses_capabilities_omit_prompt_cache_retention() {
        let capabilities = ProviderCapabilities::for_api(ProviderApiCompatibility::OpenAiResponses);

        assert!(capabilities.supports_responses_api);
        assert!(capabilities.supports_reasoning_controls);
        assert!(capabilities.supports_service_tier);
        assert!(!capabilities.supports_prompt_cache_retention);
        assert!(capabilities.supports_streaming);
        assert!(capabilities.supports_tool_calls);
    }

    /// Verifies Anthropic advertises only the conservative capabilities needed
    /// for native Messages integration.
    ///
    /// The Anthropic adapter should allow provider-neutral output caps,
    /// streaming, tool use, and the native `output_config.effort` reasoning
    /// control without implying OpenAI Responses support, service tiers,
    /// prompt-cache retention, thinking toggles, or parallel tool-call
    /// semantics.
    #[test]
    fn anthropic_messages_capabilities_are_conservative() {
        let capabilities =
            ProviderCapabilities::for_api(ProviderApiCompatibility::AnthropicMessages);

        assert!(!capabilities.supports_responses_api);
        assert!(capabilities.supports_max_output_tokens);
        assert!(capabilities.supports_reasoning_controls);
        assert!(!capabilities.supports_thinking_toggle);
        assert!(!capabilities.supports_service_tier);
        assert!(!capabilities.supports_prompt_cache_retention);
        assert!(capabilities.supports_streaming);
        assert!(capabilities.supports_tool_calls);
        assert!(!capabilities.supports_parallel_tool_calls);
    }

    /// Verifies Claude Code advertises only the local CLI reasoning control it
    /// can actually map to subprocess arguments.
    ///
    /// Claude Code does not expose native tool execution, streaming, service
    /// tiers, or a documented model catalog through Mez. It does accept
    /// `--effort`, so the capability metadata must allow reasoning controls
    /// without implying unrelated provider features.
    #[test]
    fn claude_code_capabilities_expose_cli_reasoning_only() {
        let capabilities = ProviderCapabilities::for_api(ProviderApiCompatibility::ClaudeCode);

        assert!(!capabilities.supports_responses_api);
        assert!(!capabilities.supports_max_output_tokens);
        assert!(capabilities.supports_reasoning_controls);
        assert!(!capabilities.supports_thinking_toggle);
        assert!(!capabilities.supports_service_tier);
        assert!(!capabilities.supports_prompt_cache_retention);
        assert!(!capabilities.supports_streaming);
        assert!(!capabilities.supports_tool_calls);
        assert!(!capabilities.supports_parallel_tool_calls);
    }
}
