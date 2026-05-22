//! Agent Provider implementation.
//!
//! This module owns the agent provider boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    AGENT_PROMPT_PROFILE_NAME, AGENT_PROMPT_PROFILE_VERSION, AgentCapability, AllowedAction,
    AllowedActionSet, AuthStore, BTreeMap, ContextSourceKind, Duration, ExposeSecret, MaapBatch,
    McpPromptTool, MezError, ModelInteractionKind, ModelMessage, ModelMessageRole, ModelRequest,
    Result, SecretString, parse_fenced_maap_action_batch_for_turn,
    parse_maap_action_batch_json_for_turn, validate_non_empty,
};
use crate::auth::{AuthCredentialKind, AuthMetadata};
use crate::config::{
    CONFIG_CHANGE_OPERATION_NAMES, CONFIG_CHANGE_VALUE_DESCRIPTION,
    config_change_setting_path_description,
};
use sha2::Digest;
use std::error::Error as StdError;
use std::future::Future;
use std::pin::Pin;

// Model provider traits and OpenAI Responses adapter.

/// Default direct OpenAI Responses API endpoint used with API-key auth.
pub const OPENAI_RESPONSES_ENDPOINT: &str = "https://api.openai.com/v1/responses";
/// Default direct OpenAI model catalog endpoint used with API-key auth.
pub const OPENAI_MODELS_ENDPOINT: &str = "https://api.openai.com/v1/models";
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

/// Cache-stable OpenAI MAAP function-tool surfaces.
///
/// OpenAI can cache the complete tool list, while `tool_choice` can force the
/// one surface that is valid for the current turn. Keeping the action subset at
/// the function boundary lets strict schema generation remove disallowed action
/// variants instead of relying on prose inside the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAiMaapToolSurface {
    /// Initial capability selection surface.
    CapabilityDecision,
    /// Response-only continuation surface.
    RespondOnly,
    /// Shell and patch execution surface.
    Shell,
    /// Network search execution surface.
    NetworkSearch,
    /// Network fetch execution surface.
    NetworkFetch,
    /// MCP call execution surface.
    Mcp,
    /// Local messaging and subagent execution surface.
    Subagent,
    /// Configuration mutation request surface.
    ConfigChange,
    /// Narrow fallback for uncommon composite capability grants.
    CurrentRequest,
}

impl OpenAiMaapToolSurface {
    /// Returns cache-stable surfaces that are always advertised to OpenAI.
    fn stable_surfaces() -> &'static [Self] {
        &[
            Self::CapabilityDecision,
            Self::RespondOnly,
            Self::Shell,
            Self::NetworkSearch,
            Self::NetworkFetch,
            Self::Mcp,
            Self::Subagent,
            Self::ConfigChange,
        ]
    }

    /// Returns the function-tool name for this surface.
    fn tool_name(self) -> &'static str {
        match self {
            Self::CapabilityDecision => "submit_maap_capability_decision",
            Self::RespondOnly => "submit_maap_respond_only_actions",
            Self::Shell => "submit_maap_shell_actions",
            Self::NetworkSearch => "submit_maap_network_search_actions",
            Self::NetworkFetch => "submit_maap_network_fetch_actions",
            Self::Mcp => "submit_maap_mcp_actions",
            Self::Subagent => "submit_maap_subagent_actions",
            Self::ConfigChange => "submit_maap_config_change_actions",
            Self::CurrentRequest => "submit_maap_current_actions",
        }
    }

    /// Returns provider-facing guidance for this function tool.
    fn description(self) -> &'static str {
        match self {
            Self::CapabilityDecision => {
                "Submit one MAAP batch for deciding the next coarse capability. Only say and request_capability are valid. Model-selected skill lookup/loading is disabled."
            }
            Self::RespondOnly => {
                "Submit one MAAP batch for response-only progress or completion. Model-selected skill lookup/loading is disabled. Only non-executing say actions are valid."
            }
            Self::Shell => {
                "Submit one MAAP batch for local shell work or Mezzanine patch mutations. Shell and apply_patch are the only executable actions in this surface."
            }
            Self::NetworkSearch => {
                "Submit one MAAP batch for external network search work. Web search is the only network action in this surface."
            }
            Self::NetworkFetch => {
                "Submit one MAAP batch for external URL fetch work. Fetch URL is the only network action in this surface."
            }
            Self::Mcp => {
                "Submit one MAAP batch for MCP tool work. MCP calls are limited to the tools listed in this function schema."
            }
            Self::Subagent => {
                "Submit one MAAP batch for local agent messaging or spawning subagents."
            }
            Self::ConfigChange => {
                "Submit one MAAP batch for proposing Mezzanine configuration changes."
            }
            Self::CurrentRequest => {
                "Submit one MAAP batch for this request's current composite action surface."
            }
        }
    }

    /// Returns the canonical action set for a cache-stable surface.
    fn allowed_actions(self) -> AllowedActionSet {
        match self {
            Self::CapabilityDecision => AllowedActionSet::capability_decision(),
            Self::RespondOnly => AllowedActionSet::for_capability(AgentCapability::RespondOnly),
            Self::Shell => AllowedActionSet::for_capability(AgentCapability::Shell),
            Self::NetworkSearch => AllowedActionSet::for_capability(AgentCapability::NetworkSearch),
            Self::NetworkFetch => AllowedActionSet::for_capability(AgentCapability::NetworkFetch),
            Self::Mcp => AllowedActionSet::for_capability(AgentCapability::Mcp),
            Self::Subagent => AllowedActionSet::for_capability(AgentCapability::Subagent),
            Self::ConfigChange => AllowedActionSet::for_capability(AgentCapability::ConfigChange),
            Self::CurrentRequest => AllowedActionSet::capability_decision(),
        }
    }
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

/// Carries Provider Model Info state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelInfo {
    /// Stores the id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: String,
    /// Stores the display name value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub display_name: Option<String>,
    /// Stores the reasoning levels value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub reasoning_levels: Vec<String>,
}

/// Carries Provider Model Catalog state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModelCatalog {
    /// Stores the provider value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub provider: String,
    /// Stores the source value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub source: String,
    /// Stores the models value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub models: Vec<ProviderModelInfo>,
    /// Stores the reasoning levels value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub reasoning_levels: Vec<String>,
    /// Provider-reported quota usage percentages for the catalog request.
    pub quota_usage: Vec<ProviderQuotaUsage>,
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

/// Carries Provider Http Request state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderHttpRequest {
    /// Stores the method value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub method: String,
    /// Stores the url value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub url: String,
    /// Stores the headers value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub headers: BTreeMap<String, String>,
    /// Stores the body value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub body: String,
    /// Stores the timeout ms value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub timeout_ms: u64,
    /// Optional maximum response-body bytes retained by the shared HTTP
    /// transport before returning a bounded partial body.
    pub max_response_bytes: Option<usize>,
}

/// Carries Provider Http Response state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderHttpResponse {
    /// Stores the status code value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub status_code: u16,
    /// Stores non-secret response headers returned by the provider transport.
    pub headers: BTreeMap<String, String>,
    /// Stores the body value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub body: String,
}

/// Defines the Provider Http Transport behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary
/// consumed by higher-level orchestration code.
#[cfg(test)]
pub trait ProviderHttpTransport {
    /// Runs the send operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send(&self, request: &ProviderHttpRequest) -> Result<ProviderHttpResponse>;
}

/// Defines the Async Provider Http Transport behavior contract for this subsystem.
///
/// Implementors provide the concrete I/O or state transition boundary
/// consumed by higher-level orchestration code.
pub trait AsyncProviderHttpTransport: Send + Sync {
    /// Runs the send async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_async<'a>(
        &'a self,
        request: &'a ProviderHttpRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderHttpResponse>> + Send + 'a>>;
}

/// Defines the DEFAULT PROVIDER MAX RESPONSE BYTES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
pub const DEFAULT_PROVIDER_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// Default provider response timeout for long-running model calls.
///
/// This timeout is used as a per-read stall timeout, not as a whole-request
/// deadline, because model reasoning and streaming responses can legitimately
/// take several minutes before the final body is complete.
pub const DEFAULT_PROVIDER_TIMEOUT_MS: u64 = 30 * 60 * 1000;
/// Default provider TCP/TLS connection timeout.
const DEFAULT_PROVIDER_CONNECT_TIMEOUT_MS: u64 = 30 * 1000;
/// Maximum native function-call argument bytes accepted from OpenAI responses.
const OPENAI_FUNCTION_CALL_ARGUMENT_LIMIT_BYTES: usize = DEFAULT_PROVIDER_MAX_RESPONSE_BYTES;

/// Carries Reqwest Provider Http Transport state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReqwestProviderHttpTransport;

/// Builds the reqwest client used for provider calls.
///
/// Provider responses are expected to be UTF-8 JSON or event-stream text.
/// Compression adds an extra body-decoding failure path before Mezzanine can
/// inspect provider diagnostics, so this transport explicitly avoids automatic
/// decompression. The client also avoids reqwest's whole-request timeout
/// because that deadline includes reading the entire model response body.
fn provider_http_client_builder(timeout_ms: u64) -> reqwest::ClientBuilder {
    let timeout = Duration::from_millis(timeout_ms);
    let connect_timeout =
        Duration::from_millis(timeout_ms.clamp(1, DEFAULT_PROVIDER_CONNECT_TIMEOUT_MS));

    reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .read_timeout(timeout)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
}

/// Adds provider transport headers that keep response handling deterministic.
///
/// Callers may still set an explicit `Accept-Encoding` header for tests or
/// specialized transports. The default path asks providers for identity bytes
/// so body reads do not fail in reqwest's decompression layer.
fn apply_provider_transport_default_headers(headers: &mut reqwest::header::HeaderMap) {
    if !headers.contains_key(reqwest::header::ACCEPT_ENCODING) {
        headers.insert(
            reqwest::header::ACCEPT_ENCODING,
            reqwest::header::HeaderValue::from_static("identity"),
        );
    }
}

/// Returns a header value from a string-keyed provider header map.
fn provider_header_value<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Reports whether request or response headers identify an SSE provider body.
fn provider_http_expects_event_stream(
    request_headers: &BTreeMap<String, String>,
    response_headers: &BTreeMap<String, String>,
) -> bool {
    provider_header_value(request_headers, "accept")
        .or_else(|| provider_header_value(response_headers, "content-type"))
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"))
}

/// Reports whether buffered SSE text already contains a terminal provider event.
fn provider_http_body_has_terminal_sse_event(body: &[u8]) -> bool {
    let Ok(body) = std::str::from_utf8(body) else {
        return false;
    };
    let body = body.replace("\r\n", "\n");
    let mut remaining = body.as_str();
    while let Some(separator_index) = remaining.find("\n\n") {
        let block = &remaining[..separator_index];
        if provider_sse_block_is_terminal(block) {
            return true;
        }
        remaining = &remaining[separator_index + 2..];
    }
    false
}

/// Reports whether one complete SSE event block is terminal.
fn provider_sse_block_is_terminal(block: &str) -> bool {
    let mut event_name = None;
    let mut data_lines = Vec::new();
    for line in block.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event_name = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start());
        }
    }
    if data_lines.is_empty() {
        return false;
    }
    let data = data_lines.join("\n");
    let data = data.trim();
    if data == "[DONE]" {
        return true;
    }
    let event_name_is_terminal = matches!(
        event_name,
        Some("response.completed" | "response.failed" | "response.incomplete")
    );
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return false;
    };
    event_name_is_terminal
        || matches!(
            value.get("type").and_then(serde_json::Value::as_str),
            Some("response.completed" | "response.failed" | "response.incomplete")
        )
}

/// Formats a reqwest response-body read error with useful transport details.
fn provider_http_response_read_error(
    status_code: u16,
    content_encoding: &str,
    error: reqwest::Error,
) -> MezError {
    let source_chain = provider_http_error_source_chain(&error);
    MezError::invalid_state(format!(
        "provider HTTP response read failed (status {status_code}, \
         content-encoding {content_encoding}, timeout {}, decode {}, source {source_chain}): \
         {error}",
        error.is_timeout(),
        error.is_decode(),
    ))
}

/// Returns the lower-level reqwest source chain for provider diagnostics.
fn provider_http_error_source_chain(error: &reqwest::Error) -> String {
    let mut sources = Vec::new();
    let mut source = StdError::source(error);
    while let Some(current) = source {
        sources.push(current.to_string());
        source = current.source();
    }
    if sources.is_empty() {
        "none".to_string()
    } else {
        sources.join(" -> ")
    }
}

impl AsyncProviderHttpTransport for ReqwestProviderHttpTransport {
    /// Runs the send async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_async<'a>(
        &'a self,
        request: &'a ProviderHttpRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderHttpResponse>> + Send + 'a>> {
        Box::pin(async move {
            let method = request.method.parse::<reqwest::Method>().map_err(|_| {
                MezError::invalid_args(format!(
                    "unsupported provider HTTP method {}",
                    request.method
                ))
            })?;
            let mut headers = reqwest::header::HeaderMap::new();
            for (name, value) in &request.headers {
                let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| MezError::invalid_args("provider HTTP header name is invalid"))?;
                let value = reqwest::header::HeaderValue::from_str(value)
                    .map_err(|_| MezError::invalid_args("provider HTTP header value is invalid"))?;
                headers.insert(name, value);
            }
            apply_provider_transport_default_headers(&mut headers);

            let client = provider_http_client_builder(request.timeout_ms)
                .build()
                .map_err(|error| {
                    MezError::invalid_state(format!("provider HTTP client setup failed: {error}"))
                })?;
            let mut response = client
                .request(method, &request.url)
                .headers(headers)
                .body(request.body.clone())
                .send()
                .await
                .map_err(|error| {
                    MezError::invalid_state(format!("provider HTTP request failed: {error}"))
                })?;
            let status_code = response.status().as_u16();
            let mut response_headers = response
                .headers()
                .iter()
                .filter_map(|(name, value)| {
                    value
                        .to_str()
                        .ok()
                        .map(|value| (name.as_str().to_string(), value.to_string()))
                })
                .collect::<BTreeMap<_, _>>();
            let content_encoding = response_headers
                .get("content-encoding")
                .map(String::as_str)
                .unwrap_or("absent");
            let expects_event_stream =
                provider_http_expects_event_stream(&request.headers, &response_headers);
            let response_limit = request
                .max_response_bytes
                .unwrap_or(DEFAULT_PROVIDER_MAX_RESPONSE_BYTES)
                .min(DEFAULT_PROVIDER_MAX_RESPONSE_BYTES);
            let mut body_truncated = false;
            let mut body = Vec::new();
            loop {
                let chunk = match response.chunk().await {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break,
                    Err(error) => {
                        if expects_event_stream && provider_http_body_has_terminal_sse_event(&body)
                        {
                            break;
                        }
                        return Err(provider_http_response_read_error(
                            status_code,
                            content_encoding,
                            error,
                        ));
                    }
                };
                if body.len().saturating_add(chunk.len()) > response_limit {
                    if request.max_response_bytes.is_none() {
                        return Err(MezError::invalid_state(
                            "provider HTTP response exceeds configured limit",
                        ));
                    }
                    let remaining = response_limit.saturating_sub(body.len());
                    if remaining > 0 {
                        body.extend_from_slice(&chunk[..remaining]);
                    }
                    body_truncated = true;
                    break;
                }
                body.extend_from_slice(&chunk);
                if expects_event_stream && provider_http_body_has_terminal_sse_event(&body) {
                    break;
                }
            }
            if body_truncated {
                response_headers.insert("x-mez-body-truncated".to_string(), "true".to_string());
            }
            let body = if body_truncated && request.max_response_bytes.is_some() {
                String::from_utf8_lossy(&body).into_owned()
            } else {
                String::from_utf8(body).map_err(|_| {
                    MezError::invalid_state("provider HTTP response body is not UTF-8")
                })?
            };
            Ok(ProviderHttpResponse {
                status_code,
                headers: response_headers,
                body,
            })
        })
    }
}

#[cfg(test)]
mod provider_transport_tests {
    use super::*;

    /// Verifies provider HTTP calls ask for identity response bytes unless a
    /// caller explicitly chooses a different content encoding.
    ///
    /// The OpenAI transport consumes UTF-8 JSON or event-stream text. Asking
    /// for identity encoding prevents transient body decompression failures
    /// from hiding provider diagnostics before the response parser can run.
    #[test]
    fn provider_transport_requests_identity_encoding_by_default() {
        let mut headers = reqwest::header::HeaderMap::new();

        apply_provider_transport_default_headers(&mut headers);

        assert_eq!(
            headers.get(reqwest::header::ACCEPT_ENCODING).unwrap(),
            "identity"
        );
    }

    /// Verifies provider HTTP calls preserve an explicitly supplied
    /// `Accept-Encoding` value.
    ///
    /// The default runtime path avoids compressed responses, but tests and
    /// specialized callers may need to assert exact header pass-through
    /// behavior. The defaulting helper must not overwrite that intent.
    #[test]
    fn provider_transport_preserves_explicit_accept_encoding() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::ACCEPT_ENCODING,
            reqwest::header::HeaderValue::from_static("gzip"),
        );

        apply_provider_transport_default_headers(&mut headers);

        assert_eq!(
            headers.get(reqwest::header::ACCEPT_ENCODING).unwrap(),
            "gzip"
        );
    }

    /// Verifies event-stream provider responses complete when a terminal SSE
    /// event is received instead of waiting for the HTTP stream to close.
    ///
    /// ChatGPT-backed provider calls use SSE. Some servers and intermediaries
    /// can keep the stream open after `response.completed`, so the transport
    /// must return the complete provider body as soon as the terminal event is
    /// buffered.
    #[tokio::test]
    async fn provider_transport_returns_after_terminal_sse_event_without_eof() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let body = format!(
                "event: response.completed\ndata: {}\n\n",
                serde_json::json!({
                    "type": "response.completed",
                    "response": {"id": "resp_1", "model": "gpt-test"}
                })
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: text/event-stream\r\n\
                 Transfer-Encoding: chunked\r\n\
                 Connection: keep-alive\r\n\
                 \r\n\
                 {:x}\r\n\
                 {}\r\n",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let request = ProviderHttpRequest {
            method: "POST".to_string(),
            url: format!("http://{address}/responses"),
            headers: BTreeMap::from([("Accept".to_string(), "text/event-stream".to_string())]),
            body: "{}".to_string(),
            timeout_ms: 1_000,
            max_response_bytes: None,
        };

        let response = tokio::time::timeout(
            Duration::from_secs(1),
            ReqwestProviderHttpTransport.send_async(&request),
        )
        .await
        .expect("event-stream response should return before EOF")
        .unwrap();
        server.abort();

        assert_eq!(response.status_code, 200);
        assert!(response.body.contains("response.completed"));
    }

    /// Verifies callers can request a lower retained response-body cap than
    /// the provider default.
    ///
    /// Runtime-owned web actions may fetch arbitrary pages. They should not
    /// retain provider-scale response bodies before their own action-level
    /// truncation logic runs.
    #[tokio::test]
    async fn provider_transport_bounds_response_body_for_callers() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let body = "abcdef";
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: text/plain; charset=utf-8\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\
                 \r\n\
                 {}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
        });
        let request = ProviderHttpRequest {
            method: "GET".to_string(),
            url: format!("http://{address}/large.txt"),
            headers: BTreeMap::new(),
            body: String::new(),
            timeout_ms: 1_000,
            max_response_bytes: Some(3),
        };

        let response = ReqwestProviderHttpTransport
            .send_async(&request)
            .await
            .unwrap();
        server.abort();

        assert_eq!(response.status_code, 200);
        assert_eq!(response.body, "abc");
        assert_eq!(
            response
                .headers
                .get("x-mez-body-truncated")
                .map(String::as_str),
            Some("true")
        );
    }

    /// Verifies terminal SSE detection also lets buffered failure events survive
    /// a later body read failure.
    ///
    /// Provider failures inside an SSE stream contain structured diagnostics.
    /// The transport should preserve a complete `response.failed` event for the
    /// provider parser instead of replacing it with a lower-level stream error.
    #[test]
    fn provider_transport_detects_terminal_failure_sse_events() {
        let body = format!(
            "event: response.failed\ndata: {}\n\n",
            serde_json::json!({
                "type": "response.failed",
                "response": {"error": {"message": "bad token"}}
            })
        );

        assert!(provider_http_body_has_terminal_sse_event(body.as_bytes()));
    }

    /// Verifies terminal SSE detection does not stop on a partial JSON event.
    ///
    /// Provider streaming chunks can split inside a large JSON string. The
    /// transport must keep reading until the complete SSE block arrives rather
    /// than returning a body that the OpenAI stream parser later reports as
    /// `EOF while parsing a string`.
    #[test]
    fn provider_transport_does_not_stop_on_partial_terminal_sse_json() {
        let body = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"unterminated"
        );
        let delimited_but_invalid = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"unterminated\n\n"
        );

        assert!(!provider_http_body_has_terminal_sse_event(body.as_bytes()));
        assert!(!provider_http_body_has_terminal_sse_event(
            delimited_but_invalid.as_bytes()
        ));
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
        .read_metadata()?
        .ok_or_else(|| MezError::invalid_state("OpenAI provider is not authenticated"))?;
    if metadata.provider != "openai" {
        return Err(MezError::invalid_state(format!(
            "auth metadata is for provider `{}`",
            metadata.provider
        )));
    }
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

/// Derives the OpenAI Responses endpoint from a configured provider base URL.
///
/// Configuration names this value `base_url`, so a value such as
/// `https://api.openai.com/v1` is expanded to the documented
/// `https://api.openai.com/v1/responses` request endpoint. Existing endpoint
/// values ending in `/responses` are preserved.
pub fn openai_responses_endpoint_for_base_url(base_url: &str) -> Result<String> {
    validate_non_empty("OpenAI provider base URL", base_url)?;
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url == CHATGPT_RESPONSES_ENDPOINT
        || base_url.starts_with("https://chatgpt.com/backend-api/codex/")
    {
        return Err(MezError::invalid_state(
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

/// Runs the openai models endpoint for responses endpoint operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn openai_models_endpoint_for_responses_endpoint(endpoint: &str) -> Result<String> {
    validate_non_empty("OpenAI Responses endpoint", endpoint)?;
    let endpoint = endpoint.trim_end_matches('/');
    if endpoint == CHATGPT_RESPONSES_ENDPOINT
        || endpoint.starts_with("https://chatgpt.com/backend-api/codex/")
    {
        return Err(MezError::invalid_state(
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

/// Runs the parse openai models http body operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn parse_openai_models_http_body(body: &str) -> Result<Vec<ProviderModelInfo>> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|error| {
        MezError::invalid_state(format!(
            "OpenAI Models response was not valid JSON: {error}"
        ))
    })?;
    let models = openai_models_array(&value)
        .ok_or_else(|| MezError::invalid_state("OpenAI Models response did not contain models"))?;
    let mut parsed = Vec::new();
    for model in models {
        if let Some(info) = openai_model_info_from_value(model) {
            parsed.push(info);
        }
    }
    parsed.sort_by(|left, right| left.id.cmp(&right.id));
    parsed.dedup_by(|left, right| left.id == right.id);
    Ok(parsed)
}

/// Runs the openai models array operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn openai_models_array(value: &serde_json::Value) -> Option<&[serde_json::Value]> {
    value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .or_else(|| value.get("models").and_then(serde_json::Value::as_array))
        .or_else(|| value.as_array())
        .map(Vec::as_slice)
}

/// Runs the openai model info from value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn openai_model_info_from_value(value: &serde_json::Value) -> Option<ProviderModelInfo> {
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
        id,
        display_name,
        reasoning_levels,
    })
}

/// Runs the provider reasoning levels from value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
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

/// Runs the openai default reasoning levels for model operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
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

/// Runs the provider catalog reasoning levels operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_catalog_reasoning_levels(models: &[ProviderModelInfo]) -> Vec<String> {
    dedupe_provider_strings(
        models
            .iter()
            .flat_map(|model| model.reasoning_levels.iter().cloned())
            .collect(),
    )
}

/// Runs the dedupe provider strings operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn dedupe_provider_strings(values: Vec<String>) -> Vec<String> {
    let mut deduped = Vec::new();
    for value in values {
        if !deduped.iter().any(|existing| existing == &value) {
            deduped.push(value);
        }
    }
    deduped
}

/// Runs the openai provider error detail operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn openai_provider_error_detail(body: &str) -> String {
    if body.trim().is_empty() {
        return "empty provider response".to_string();
    }
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .or_else(|| value.get("error_description"))
                .or_else(|| value.get("message"))
                .or_else(|| value.get("error"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(240).collect())
}

/// Runs the openai provider failure json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn openai_provider_failure_json(status_code: Option<u16>, body: &str) -> String {
    let trimmed = body.trim();
    let mut object = serde_json::Map::new();
    if let Some(status_code) = status_code {
        object.insert(
            "status_code".to_string(),
            serde_json::Value::Number(serde_json::Number::from(u64::from(status_code))),
        );
    }
    if trimmed.is_empty() {
        object.insert(
            "body_text".to_string(),
            serde_json::Value::String(String::new()),
        );
    } else if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        insert_provider_failure_value(&mut object, value);
    } else {
        object.insert(
            "body_text".to_string(),
            serde_json::Value::String(truncate_provider_failure_text(trimmed)),
        );
    }
    serde_json::Value::Object(object).to_string()
}

/// Runs the openai provider failure event json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn openai_provider_failure_event_json(value: &serde_json::Value) -> String {
    let mut object = serde_json::Map::new();
    insert_provider_failure_value(&mut object, value.clone());
    serde_json::Value::Object(object).to_string()
}

/// Reports whether provider error text explicitly says the same request can be
/// retried.
///
/// # Parameters
/// - `message`: Primary provider error message attached to the runtime error.
/// - `provider_failure_json`: Optional sanitized provider failure payload.
pub(crate) fn provider_error_invites_retry(
    message: &str,
    provider_failure_json: Option<&str>,
) -> bool {
    if provider_error_text_invites_retry(message) {
        return true;
    }
    let Some(provider_failure_json) = provider_failure_json else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(provider_failure_json) else {
        return false;
    };
    [
        "/error/message",
        "/message",
        "/body/error/message",
        "/body/message",
        "/response/error/message",
    ]
    .into_iter()
    .filter_map(|pointer| value.pointer(pointer))
    .filter_map(serde_json::Value::as_str)
    .any(provider_error_text_invites_retry)
}

/// Reports whether provider diagnostics indicate the request exceeded the
/// model's input context limit.
///
/// # Parameters
/// - `message`: Primary provider error message attached to the runtime error.
/// - `provider_failure_json`: Optional sanitized provider failure payload.
pub(crate) fn provider_error_is_context_limit_exceeded(
    message: &str,
    provider_failure_json: Option<&str>,
) -> bool {
    if provider_error_text_is_context_limit_exceeded(message) {
        return true;
    }
    let Some(provider_failure_json) = provider_failure_json else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(provider_failure_json) else {
        return false;
    };
    [
        "/error/code",
        "/error/type",
        "/error/message",
        "/message",
        "/body/error/code",
        "/body/error/type",
        "/body/error/message",
        "/body/message",
        "/response/error/code",
        "/response/error/type",
        "/response/error/message",
        "/response/incomplete_details/reason",
    ]
    .into_iter()
    .filter_map(|pointer| value.pointer(pointer))
    .filter_map(serde_json::Value::as_str)
    .any(provider_error_text_is_context_limit_exceeded)
}

/// Reports whether provider diagnostics indicate output generation exhausted
/// the configured provider output-token budget.
///
/// # Parameters
/// - `message`: Primary provider error message attached to the runtime error.
/// - `provider_failure_json`: Optional sanitized provider failure payload.
pub(crate) fn provider_error_is_output_limit_exceeded(
    message: &str,
    provider_failure_json: Option<&str>,
) -> bool {
    if provider_error_text_is_output_limit_exceeded(message) {
        return true;
    }
    let Some(provider_failure_json) = provider_failure_json else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(provider_failure_json) else {
        return false;
    };
    [
        "/incomplete_details/reason",
        "/response/incomplete_details/reason",
        "/body/incomplete_details/reason",
        "/body/response/incomplete_details/reason",
        "/error/code",
        "/error/message",
        "/message",
        "/body/error/code",
        "/body/error/message",
        "/body/message",
        "/response/error/code",
        "/response/error/message",
    ]
    .into_iter()
    .filter_map(|pointer| value.pointer(pointer))
    .filter_map(serde_json::Value::as_str)
    .any(provider_error_text_is_output_limit_exceeded)
}

/// Reports whether one provider error message contains a retry invitation.
///
/// # Parameters
/// - `text`: Provider error text to classify.
fn provider_error_text_invites_retry(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("you can retry your request")
        || lower.contains("you can retry the request")
        || (lower.contains("an error occurred while processing your request")
            && lower.contains("retry"))
}

/// Reports whether one provider error field indicates an input context limit.
///
/// # Parameters
/// - `text`: Provider diagnostic text or code to classify.
fn provider_error_text_is_context_limit_exceeded(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("context_length_exceeded")
        || lower.contains("context length exceeded")
        || lower.contains("context_window_exceeded")
        || lower.contains("exceeds the context window")
        || lower.contains("maximum context length")
        || lower.contains("max context length")
        || lower.contains("context window")
        || lower.contains("prompt is too long")
        || lower.contains("input is too large")
        || lower.contains("input too large")
        || lower.contains("too many input tokens")
        || lower.contains("too many tokens")
        || lower.contains("reduce the length of the messages")
        || lower.contains("reduce the length of your input")
        || lower.contains("request too large for the model")
}

/// Reports whether one provider error field indicates output-token exhaustion.
///
/// # Parameters
/// - `text`: Provider diagnostic text or code to classify.
fn provider_error_text_is_output_limit_exceeded(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("max_output_tokens")
        || lower.contains("maximum output tokens")
        || lower.contains("output token limit")
        || lower.contains("output tokens limit")
        || lower.contains("response output limit")
}

/// Runs the insert provider failure value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn insert_provider_failure_value(
    object: &mut serde_json::Map<String, serde_json::Value>,
    value: serde_json::Value,
) {
    let value = sanitize_provider_failure_value(value);
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        object.insert("error".to_string(), error.clone());
    } else if let Some(response_error) = value
        .get("response")
        .and_then(|response| response.get("error"))
        .filter(|error| !error.is_null())
    {
        object.insert("error".to_string(), response_error.clone());
        if let Some(response_id) = value
            .get("response")
            .and_then(|response| response.get("id"))
            .and_then(serde_json::Value::as_str)
        {
            object.insert(
                "response_id".to_string(),
                serde_json::Value::String(response_id.to_string()),
            );
        }
    } else if let Some(incomplete_details) = value
        .get("response")
        .and_then(|response| response.get("incomplete_details"))
        .filter(|details| !details.is_null())
    {
        object.insert("incomplete_details".to_string(), incomplete_details.clone());
    } else {
        object.insert("body".to_string(), value);
    }
}

/// Runs the sanitize provider failure value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn sanitize_provider_failure_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let value = if provider_failure_key_is_secret_like(&key) {
                        serde_json::Value::String("[REDACTED]".to_string())
                    } else {
                        sanitize_provider_failure_value(value)
                    };
                    (key, value)
                })
                .collect(),
        ),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .take(32)
                .map(sanitize_provider_failure_value)
                .collect(),
        ),
        serde_json::Value::String(value) => {
            serde_json::Value::String(truncate_provider_failure_text(&value))
        }
        other => other,
    }
}

/// Runs the provider failure key is secret like operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_failure_key_is_secret_like(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("authorization")
        || key.contains("api_key")
        || key.contains("access_token")
        || key.contains("refresh_token")
        || key.contains("secret")
        || key.contains("password")
}

/// Runs the truncate provider failure text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn truncate_provider_failure_text(value: &str) -> String {
    /// Defines the MAX PROVIDER FAILURE TEXT CHARS const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    const MAX_PROVIDER_FAILURE_TEXT_CHARS: usize = 4096;
    let mut output = value
        .chars()
        .take(MAX_PROVIDER_FAILURE_TEXT_CHARS)
        .collect::<String>();
    if value.chars().count() > MAX_PROVIDER_FAILURE_TEXT_CHARS {
        output.push_str("...");
    }
    output
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

/// Runs the provider maap parse error operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_maap_parse_error(error: MezError, raw_text: &str) -> MezError {
    MezError::new(
        error.kind(),
        provider_maap_parse_error_message(&error, raw_text),
    )
    .with_provider_raw_text(raw_text.to_string())
    .with_provider_failure_json(provider_malformed_output_failure_json(&error, raw_text))
}

/// Runs the provider maap parse error message operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_maap_parse_error_message(error: &MezError, raw_text: &str) -> String {
    let mut message = format!("provider MAAP output is malformed: {}", error.message());
    if let Some(hint) = provider_malformed_output_hint(raw_text) {
        message.push_str("; ");
        message.push_str(hint);
    }
    message
}

/// Runs the provider malformed output hint operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_malformed_output_hint(raw_text: &str) -> Option<&'static str> {
    let value = serde_json::from_str::<serde_json::Value>(raw_text).ok()?;
    let object = value.as_object()?;
    if provider_output_contains_bare_command_actions(object) {
        return Some(
            "provider returned bare command objects inside actions; expected each action to include type and required action-specific fields such as shell_command summary inside a MAAP action batch",
        );
    }
    if object.contains_key("command") {
        return Some(
            "provider returned a bare command object; expected a MAAP action batch with an actions array",
        );
    }
    if object.contains_key("type") && !object.contains_key("actions") {
        return Some(
            "provider returned a bare action object; expected a MAAP action batch envelope",
        );
    }
    None
}

/// Runs the provider output contains bare command actions operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_output_contains_bare_command_actions(
    object: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    object
        .get("actions")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|actions| {
            actions.iter().any(|action| {
                action.as_object().is_some_and(|action_object| {
                    action_object.contains_key("command") && !action_object.contains_key("type")
                })
            })
        })
}

/// Runs the provider malformed output failure json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_malformed_output_failure_json(error: &MezError, raw_text: &str) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(raw_text).ok();
    let mut output = serde_json::json!({
        "format": if parsed.is_some() { "json" } else { "text" },
        "bytes": raw_text.len()
    });
    if let Some(serde_json::Value::Object(object)) = parsed {
        let top_level_keys = object.keys().take(32).cloned().collect::<Vec<_>>();
        output["top_level_keys"] = serde_json::json!(top_level_keys);
        output["bare_command_object"] = serde_json::json!(object.contains_key("command"));
        output["bare_action_object"] =
            serde_json::json!(object.contains_key("type") && !object.contains_key("actions"));
        output["bare_command_actions"] =
            serde_json::json!(provider_output_contains_bare_command_actions(&object));
    }
    serde_json::json!({
        "type": "malformed_model_output",
        "error": {
            "kind": provider_error_kind_name(error.kind()),
            "message": error.message()
        },
        "output": output
    })
    .to_string()
}

/// Runs the provider error kind name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn provider_error_kind_name(kind: crate::error::MezErrorKind) -> &'static str {
    match kind {
        crate::error::MezErrorKind::InvalidArgs => "invalid_args",
        crate::error::MezErrorKind::InvalidState => "invalid_state",
        crate::error::MezErrorKind::Config => "config",
        crate::error::MezErrorKind::Io => "io",
        crate::error::MezErrorKind::Conflict => "conflict",
        crate::error::MezErrorKind::NotFound => "not_found",
        crate::error::MezErrorKind::Forbidden => "forbidden",
        crate::error::MezErrorKind::NotImplemented => "not_implemented",
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

/// Returns the OpenAI MAAP tool surface that matches one request.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn openai_maap_tool_surface_for_request(request: &ModelRequest) -> OpenAiMaapToolSurface {
    let allowed_actions = &request.allowed_actions;
    if *allowed_actions == AllowedActionSet::capability_decision() {
        return if request.interaction_kind == ModelInteractionKind::CapabilityDecision {
            OpenAiMaapToolSurface::CapabilityDecision
        } else {
            OpenAiMaapToolSurface::RespondOnly
        };
    }
    if *allowed_actions == AllowedActionSet::say_only() {
        return OpenAiMaapToolSurface::RespondOnly;
    }
    for (capability, surface) in [
        (AgentCapability::Shell, OpenAiMaapToolSurface::Shell),
        (
            AgentCapability::NetworkSearch,
            OpenAiMaapToolSurface::NetworkSearch,
        ),
        (
            AgentCapability::NetworkFetch,
            OpenAiMaapToolSurface::NetworkFetch,
        ),
        (AgentCapability::Mcp, OpenAiMaapToolSurface::Mcp),
        (AgentCapability::Subagent, OpenAiMaapToolSurface::Subagent),
        (
            AgentCapability::ConfigChange,
            OpenAiMaapToolSurface::ConfigChange,
        ),
    ] {
        if *allowed_actions == AllowedActionSet::for_capability(capability) {
            return surface;
        }
    }
    OpenAiMaapToolSurface::CurrentRequest
}

/// Returns the current request's action set for an OpenAI MAAP tool surface.
fn openai_maap_allowed_actions_for_surface(
    surface: OpenAiMaapToolSurface,
    request: &ModelRequest,
) -> AllowedActionSet {
    if surface == OpenAiMaapToolSurface::CurrentRequest {
        request.allowed_actions.clone()
    } else {
        surface.allowed_actions()
    }
}

/// Builds the cache-stable OpenAI MAAP function-tool list.
fn openai_maap_action_batch_tools(request: &ModelRequest) -> Vec<serde_json::Value> {
    let selected_surface = openai_maap_tool_surface_for_request(request);
    let mut tools = OpenAiMaapToolSurface::stable_surfaces()
        .iter()
        .copied()
        .map(|surface| openai_maap_action_batch_tool(surface, request))
        .collect::<Vec<_>>();
    if selected_surface == OpenAiMaapToolSurface::CurrentRequest {
        tools.push(openai_maap_action_batch_tool(selected_surface, request));
    }
    tools
}

/// Runs the openai maap action batch tool operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn openai_maap_action_batch_tool(
    surface: OpenAiMaapToolSurface,
    request: &ModelRequest,
) -> serde_json::Value {
    let allowed_actions = openai_maap_allowed_actions_for_surface(surface, request);
    serde_json::json!({
        "type": "function",
        "name": surface.tool_name(),
        "description": surface.description(),
        "strict": true,
        "parameters": maap_action_batch_schema(&allowed_actions, &request.available_mcp_tools)
    })
}

/// Runs the maap action batch schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_action_batch_schema(
    allowed_actions: &AllowedActionSet,
    available_mcp_tools: &[McpPromptTool],
) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "rationale": {
                "type": "string",
                "minLength": 1,
                "description": "Very terse model-authored rationale for this complete action batch. Mezzanine renders this once as a thinking log before the listed actions and persists it as future context. Write it as an additive delta: only the new reason these actions are next, not a restatement of the user request, global goal, previous rationale, prior say, loaded context, or action summaries. Compare against recent thinking lines, action results, and any progress say in the same response; if there is no new user-visible update, keep the rationale minimal and do not add a progress say. If progress say records durable learning, this rationale should only name the next executable reason. Omit optional action rationales that duplicate this batch rationale, progress say, or the action summary. Progress say is for sequence-point updates: include it when a non-trivial task reaches a meaningful boundary such as changed diagnosis, chosen implementation strategy, phase transition, blocker state, validation outcome, or user-requested narration."
            },
            "actions": {
                "type": "array",
                "minItems": 1,
                "description": "At least one visible or executable action from this function tool's currently active MAAP action surface.",
                "items": maap_action_schema(allowed_actions, available_mcp_tools)
            }
        },
        "required": ["rationale", "actions"],
        "additionalProperties": false
    })
}

/// Runs the maap action schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_action_schema(
    allowed_actions: &super::AllowedActionSet,
    available_mcp_tools: &[McpPromptTool],
) -> serde_json::Value {
    let mut action_schemas = Vec::new();
    for action in &allowed_actions.actions {
        match action {
            AllowedAction::Say => action_schemas.push(maap_say_action_schema()),
            AllowedAction::RequestCapability => {
                action_schemas.push(maap_request_capability_action_schema())
            }
            AllowedAction::RequestSkills => {
                action_schemas.push(maap_request_skills_action_schema())
            }
            AllowedAction::CallSkill => action_schemas.push(maap_call_skill_action_schema()),
            AllowedAction::ShellCommand => action_schemas.push(maap_shell_command_action_schema()),
            AllowedAction::ApplyPatch => action_schemas.push(maap_apply_patch_action_schema()),
            AllowedAction::WebSearch => action_schemas.push(maap_web_search_action_schema()),
            AllowedAction::FetchUrl => action_schemas.push(maap_fetch_url_action_schema()),
            AllowedAction::SendMessage => action_schemas.push(maap_send_message_action_schema()),
            AllowedAction::SpawnAgent => action_schemas.push(maap_spawn_agent_action_schema()),
            AllowedAction::ConfigChange => action_schemas.push(maap_config_change_action_schema()),
            AllowedAction::McpCall => action_schemas.extend(
                sorted_mcp_prompt_tools(available_mcp_tools)
                    .into_iter()
                    .map(maap_mcp_call_action_schema_for_tool),
            ),
            AllowedAction::Abort => action_schemas.push(maap_abort_action_schema()),
        }
    }
    if action_schemas.is_empty() {
        action_schemas.push(maap_say_action_schema());
    }
    serde_json::json!({
        "anyOf": action_schemas
    })
}

/// Returns MCP prompt tools in deterministic provider-visible order.
fn sorted_mcp_prompt_tools(tools: &[McpPromptTool]) -> Vec<&McpPromptTool> {
    let mut tools = tools.iter().collect::<Vec<_>>();
    tools.sort_by(|left, right| {
        left.server_id
            .cmp(&right.server_id)
            .then_with(|| left.tool_name.cmp(&right.tool_name))
    });
    tools
}

/// Runs the maap common action properties operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_common_action_properties(action_type: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "type".to_string(),
        serde_json::json!({
            "type": "string",
            "enum": [action_type]
        }),
    );
    properties
}

/// Runs the maap action object schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_action_object_schema(
    action_type: &str,
    extra_properties: impl IntoIterator<Item = (&'static str, serde_json::Value)>,
    extra_required: &[&str],
) -> serde_json::Value {
    let mut properties = maap_common_action_properties(action_type);
    for (name, schema) in extra_properties {
        properties.insert(name.to_string(), schema);
    }

    let mut required = vec!["type"];
    required.extend(extra_required);

    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

/// Returns a compact required string property schema with provider-facing usage
/// guidance for fields whose action semantics are otherwise easy to overuse.
fn described_string_property(
    name: &'static str,
    description: &'static str,
) -> (&'static str, serde_json::Value) {
    (
        name,
        serde_json::json!({
            "type": "string",
            "minLength": 1,
            "description": description
        }),
    )
}

/// Runs the maap say action schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_say_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "say",
        [
            (
                "status",
                serde_json::json!({
                    "type": "string",
                    "enum": ["progress", "final", "blocked"],
                    "description": "Required terminal intent. Use progress for nonterminal sequence-point updates when the turn should continue and the user should know what was learned, which direction was chosen, what phase is starting, or what blocker/validation result changed the task state. For non-trivial multi-step work, include at most one progress say at meaningful task boundaries: after the first evidence pass identifies the real owner or diagnosis, when choosing an implementation or report direction, when moving from inspection to editing, when moving from editing to validation, when validation changes the plan, or when a blocker or uncertainty changes the next step. Do not use progress for future-tense plans, intended-work checklists, routine inspection, owner localization, anchor lookup, test lookup, command-wrapper lookup, \"now patching\" updates, routine action continuity, or headings such as Plan:, Steps:, Next:, Executed:, or Evidence: when executable actions are requested in the same response. Do not use progress just to announce or justify executable actions, duplicate the batch rationale/action summaries, or restate prior progress in the same turn, and do not emit progress in every action batch. Use final when the user goal is complete, and blocked when user input or an external condition is required before progress can continue. Do not pair final or blocked say actions with executable actions; wait for results first."
                }),
            ),
            (
                "content_type",
                serde_json::json!({
                    "type": "string",
                    "enum": ["text/plain; charset=utf-8", "text/markdown; charset=utf-8", "text/x-diff; charset=utf-8"],
                    "description": "HTTP-style media type for text. Use text/markdown; charset=utf-8 when the text uses Markdown presentation syntax, text/x-diff; charset=utf-8 when the text is a unified diff, otherwise use text/plain; charset=utf-8."
                }),
            ),
            (
                "text",
                serde_json::json!({
                    "type": "string",
                    "minLength": 1,
                    "description": "Non-empty conversational text for the user. Content in say is display-only: shell commands and Mezzanine patch blocks here do not execute. Use shell_command when terminal work should be executed and apply_patch for executable *** Begin Patch blocks. Only include commands or patches here when the user explicitly asked to see examples or text. Do not use say to duplicate the batch rationale, action summaries, recent thinking lines, action results, or prior progress say. When using progress for a sequence-point update, write 1-2 compact sentences naming the important fact, decision, phase transition, blocker, or validation outcome. The text must state durable learning or a decision, not intended work. If there is no new sequence-point update, omit progress say. If progress say is included, keep the batch rationale to the next executable reason instead of restating the same finding. Do not format ordinary progress or final text with Plan:, Executed:, or Evidence: headings unless the user explicitly requested that report format. For markdown content, this remains the raw markdown copied to buffers and clipboards."
                }),
            ),
        ],
        &["status", "content_type", "text"],
    )
}

/// Runs the maap request capability action schema operation for this subsystem.
fn maap_request_capability_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "request_capability",
        [
            (
                "capability",
                serde_json::json!({
                    "type": "string",
                    "enum": AgentCapability::all_names(),
                    "description": "Coarse action family to expose through the controller when the current schema lacks actions needed for the task. This is not a user permission request."
                }),
            ),
            (
                "reason",
                serde_json::json!({
                    "type": "string",
                    "minLength": 1,
                    "description": "Brief task-specific explanation of why this capability is needed."
                }),
            ),
        ],
        &["capability", "reason"],
    )
}

/// Runs the maap request skills action schema operation for this subsystem.
fn maap_request_skills_action_schema() -> serde_json::Value {
    let mut schema = maap_action_object_schema(
        "request_skills",
        std::iter::empty::<(&'static str, serde_json::Value)>(),
        &[],
    );
    schema["description"] = serde_json::json!(
        "Exceptional workflow discovery action. Do not use as a default preflight, merely because skills exist, or before ordinary repository inspection, implementation, validation, or reporting. Incorrect for tasks that name a concrete file, path, symbol, command, failing test, issue backlog, documentation page, or repo-state plan/review target; request or use shell capability instead. Use only when the user asks for skills/workflows, names a skill, or the task clearly needs a specialized reusable workflow that would materially change the next action."
    );
    schema
}

/// Runs the maap call skill action schema operation for this subsystem.
fn maap_call_skill_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "call_skill",
        [
            (
                "name",
                serde_json::json!({
                    "type": "string",
                    "minLength": 1,
                    "description": "Skill name returned by request_skills. Use this only after an appropriate skill discovery result identifies a workflow that materially changes the next action; skills add context only and do not grant permissions or capabilities."
                }),
            ),
            (
                "additional_context",
                serde_json::json!({
                    "type": ["string", "null"],
                    "description": "Optional task-specific context to append under an Additional context heading in the loaded skill context."
                }),
            ),
        ],
        &["name", "additional_context"],
    )
}

/// Runs the maap shell command action schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_shell_command_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "shell_command",
        [
            (
                "summary",
                serde_json::json!({
                    "type": "string",
                    "minLength": 1,
                    "description": "Concise user-facing progress summary to display before the command runs. Do not include the raw shell command; describe what will happen or what output will be used."
                }),
            ),
            (
                "command",
                serde_json::json!({
                    "type": "string",
                    "minLength": 1,
                    "description": "Exact shell input to execute in the pane. Use this for one logical local inspection, build, test, git, package, process, formatting, validation, bounded generation, directory creation, path move, path deletion, or terminal operation that is not a structured patch. Prefer one focused command or compact pipeline with one purpose; avoid long &&, ;, or newline chains. When shell work is independent, emit separate shell_command actions in the same MAAP action batch instead of joining commands inside one shell string. Split across provider turns when later commands depend on earlier output. Use shell-level chaining only for tightly coupled fail-fast steps that should share one outcome and one output stream. Keep commands bounded and noninteractive. Discover command/tool invocation details only when needed, then reuse the discovered command form during the same work cycle instead of repeating equivalent discovery branches before every command. Never invoke the MAAP action name apply_patch as a shell program; emit apply_patch as an action instead. Agent-authored heredoc and here-string redirections (<<, <<-, <<<) are disabled; use apply_patch for ordinary file content changes. Non-zero shell exit status is ordinary model-visible command evidence."
                }),
            ),
        ],
        &["summary", "command"],
    )
}

/// Runs the maap apply patch action schema operation for this subsystem.
fn maap_apply_patch_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "apply_patch",
        [described_string_property(
            "patch",
            "Mezzanine patch block for one or more file operations. Emit the patch string directly: do not include Markdown fences, heredoc wrappers, or apply_patch <<... shell text. The most reliable update shape is one small file operation with a copied @@ anchor from the current file and 1-6 exact old/context lines; prefer several small anchored hunks over one large brittle hunk. Copy blank separator lines as explicit context when they sit between old/context blocks; recovery may tolerate omitted blank-only separators between adjacent old hunk lines, including add-only insertion boundaries and replacement blocks, but do not rely on that fallback. Canonical grammar: first nonblank line must be *** Begin Patch; last nonblank line must be *** End Patch. Between them emit one or more file operations. Add file: *** Add File: <relative-path> followed by zero or more content lines, each beginning with +. Update file: *** Update File: <relative-path>, optionally immediately followed by *** Move to: <relative-path>, then one or more hunks. Each canonical hunk starts with a line beginning @@, optionally followed by distinctive anchor text from the current file such as @@ fn name or @@ impl Type @@ fn method. For recovery compatibility, parsing also accepts whitespace around patch markers/directives, uniformly indented patch blocks, Markdown-fenced or heredoc-wrapped patch text in this field including accidental apply_patch <<... wrappers, blank hunk-body lines as empty context lines, safe ./ or git-diff a/ or b/ header path prefixes, and an omitted @@ header for the first update hunk only. Unified-diff hunk range metadata is also accepted inside Mezzanine update hunks, e.g. @@ -10,7 +10,8 @@ or @@ -10,7 +10,8 @@ fn name; the old-line number is only a conservative tie-breaker when the old/context body has multiple valid matches, and it is rejected for ties, near-ties, distant candidates, or conflicts with header anchors. Hunk body lines begin with exactly one prefix character: space for context, - for removed text, + for added text; an optional *** End of File line inside a hunk means the final file has no trailing newline. Header anchors constrain old-context placement; they do not replace the required hunk body context. Rust-like header anchors may bound matching to a conservatively resolved structural scope, but unresolved scopes fall back and internal ambiguity still fails. Hunk placement tries exact old-context matching first, may use the unified old-line range as a conservative disambiguating hint, and may tolerate trailing whitespace, surrounding whitespace, common Unicode punctuation drift, or omitted blank-only separator lines between adjacent old hunk lines when that still identifies one deterministic location; copied context lines are preserved from the current file and are not rewritten from the patch, blanks omitted before copied context are preserved, and blanks omitted before removed lines are deleted with the removed block. Unanchored pure-addition update hunks append by default; use a distinctive @@ anchor when inserting elsewhere. Delete file: *** Delete File: <relative-path> with no body. This is the only semantic file-content mutation action. Multi-file patches are accepted when the edits are related; use separate apply_patch actions when independent files would be easier to recover separately. File paths in *** Add File, *** Update File, *** Delete File, and *** Move to headers must be relative to the pane current working directory and must not be absolute, empty, contain empty segments, or contain .. traversal; canonical output should omit ./, a/, and b/ prefixes. Raw unified diffs are not accepted; use shell_command with git apply only when a raw unified diff is truly required. Do not pipe this patch to an apply_patch shell command; apply_patch is this MAAP action, not a pane executable. After a hunk/context mismatch or ambiguity, classify the failure, reuse fresh current-file evidence already present in recent action results, otherwise re-read only missing or stale candidate/owner ranges, compare the intended change with current code, skip already-applied or equivalent behavior, and emit a smaller fresh patch with distinctive @@ header anchors instead of replaying substantially the same patch. Ambiguous context means inspect candidate regions; missing or stale context under an anchor means inspect the current owner body; replacement_hint diagnostics mean reconcile the current file before retrying.",
        )],
        &["patch"],
    )
}

/// Runs the maap web search action schema operation for this subsystem.
fn maap_web_search_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "web_search",
        [described_string_property(
            "query",
            "Use only when the user asks for web search or current external information; not for local filesystem work or random/test/generated local content.",
        )],
        &["query"],
    )
}

/// Runs the maap fetch url action schema operation for this subsystem.
fn maap_fetch_url_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "fetch_url",
        [described_string_property(
            "url",
            "Use only for explicit http:// or https:// external URLs. For file://, local paths, or created outputs use shell_command; not for random/test/generated local data or replacing apply_patch/shell_command.",
        )],
        &["url"],
    )
}

/// Runs the maap send message action schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_send_message_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "send_message",
        [
            (
                "recipient",
                serde_json::json!({
                    "type": "string"
                }),
            ),
            (
                "content_type",
                serde_json::json!({
                    "type": "string",
                    "enum": ["text/plain; charset=utf-8", "application/json"],
                    "description": "Use text/plain; charset=utf-8 for plain-text coordination messages and application/json for compact JSON-string payloads."
                }),
            ),
            (
                "payload",
                serde_json::json!({
                    "type": "string",
                    "description": "Model-readable payload, with JSON payloads encoded as a compact JSON string."
                }),
            ),
        ],
        &["recipient", "content_type", "payload"],
    )
}

/// Runs the maap spawn agent action schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_spawn_agent_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "spawn_agent",
        [
            (
                "role",
                serde_json::json!({
                    "type": "string",
                    "description": "Subagent role/profile. Use explorer for read-only search and inspection, worker for implementation, or a configured custom role."
                }),
            ),
            (
                "task_prompt",
                serde_json::json!({
                    "type": "string"
                }),
            ),
        ],
        &["role", "task_prompt"],
    )
}

/// Runs the maap config change action schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_config_change_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "config_change",
        [
            (
                "setting_path",
                serde_json::json!({
                    "type": "string",
                    "minLength": 1,
                    "description": config_change_setting_path_description()
                }),
            ),
            (
                "operation",
                serde_json::json!({
                    "type": "string",
                    "enum": CONFIG_CHANGE_OPERATION_NAMES,
                    "description": "Configuration mutation operation. Use this action, not prose or config-file edits, for explicit requests such as changing the mez theme, approval mode, model, reasoning, or other supported settings. Config changes follow the active approval policy like other privileged actions. Once approved or policy-allowed, the runtime persists the change to the user config target and applies it immediately. A theme.active set uses set-theme behavior, including materialized theme aliases/colors. Use set to assign a scalar/string-array value, unset to remove a scalar override, and reset when the user's intent is to return a field to its lower-precedence or default value."
                }),
            ),
            (
                "value",
                serde_json::json!({
                    "type": ["string", "null"],
                    "description": CONFIG_CHANGE_VALUE_DESCRIPTION
                }),
            ),
        ],
        &["setting_path", "operation", "value"],
    )
}

/// Runs the maap mcp call action schema for tool operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_mcp_call_action_schema_for_tool(tool: &McpPromptTool) -> serde_json::Value {
    maap_action_object_schema(
        "mcp_call",
        [
            (
                "server",
                serde_json::json!({
                    "type": "string",
                    "enum": [tool.server_id]
                }),
            ),
            (
                "tool",
                serde_json::json!({
                    "type": "string",
                    "enum": [tool.tool_name]
                }),
            ),
            ("arguments", mcp_tool_arguments_schema(tool)),
        ],
        &["server", "tool", "arguments"],
    )
}

/// Runs the mcp tool arguments schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn mcp_tool_arguments_schema(tool: &McpPromptTool) -> serde_json::Value {
    match serde_json::from_str::<serde_json::Value>(&tool.input_schema_json) {
        Ok(serde_json::Value::Object(schema)) => {
            normalize_openai_strict_schema(serde_json::Value::Object(schema))
        }
        _ => serde_json::json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        }),
    }
}

/// Runs the normalize openai strict schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn normalize_openai_strict_schema(mut value: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Object(schema) = &mut value else {
        return value;
    };

    schema.remove("format");

    if let Some(serde_json::Value::Object(properties)) = schema.get_mut("properties") {
        let required = properties
            .keys()
            .cloned()
            .map(serde_json::Value::String)
            .collect::<Vec<_>>();
        for property_schema in properties.values_mut() {
            *property_schema = normalize_openai_strict_schema(std::mem::take(property_schema));
        }
        schema
            .entry("type")
            .or_insert_with(|| serde_json::json!("object"));
        schema.insert("required".to_string(), serde_json::Value::Array(required));
        schema.insert(
            "additionalProperties".to_string(),
            serde_json::Value::Bool(false),
        );
    } else if schema
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| kind == "object")
    {
        schema.insert(
            "properties".to_string(),
            serde_json::Value::Object(serde_json::Map::new()),
        );
        schema.insert("required".to_string(), serde_json::Value::Array(Vec::new()));
        schema.insert(
            "additionalProperties".to_string(),
            serde_json::Value::Bool(false),
        );
    }

    if let Some(items) = schema.get_mut("items") {
        *items = normalize_openai_strict_schema(std::mem::take(items));
    }
    if let Some(serde_json::Value::Array(variants)) = schema.get_mut("anyOf") {
        for variant in variants {
            *variant = normalize_openai_strict_schema(std::mem::take(variant));
        }
    }

    value
}

/// Runs the maap abort action schema operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn maap_abort_action_schema() -> serde_json::Value {
    maap_action_object_schema(
        "abort",
        [(
            "reason",
            serde_json::json!({
                "type": "string"
            }),
        )],
        &["reason"],
    )
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
    material.push_str("provider=");
    material.push_str(&request.provider);
    material.push('\n');
    material.push_str("model=");
    material.push_str(&request.model);
    material.push('\n');
    material.push_str("cache_family=responses-routing-v3\n");
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
    let stable_input_text = serde_json::to_string(&rendered.stable_input).map_err(|error| {
        MezError::invalid_state(format!("OpenAI stable-input diagnostics failed: {error}"))
    })?;
    let volatile_input_text = serde_json::to_string(&rendered.volatile_input).map_err(|error| {
        MezError::invalid_state(format!("OpenAI volatile-input diagnostics failed: {error}"))
    })?;
    let cacheable_prefix = serde_json::to_string(&serde_json::json!({
        "cache_family": "responses-routing-v3",
        "instructions": rendered.instructions,
        "response_format": response_format,
        "tools": tools,
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
