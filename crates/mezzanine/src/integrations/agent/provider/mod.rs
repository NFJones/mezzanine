//! Agent Provider implementation.
//!
//! This module owns the agent provider boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{BTreeMap, ExposeSecret, MaapBatch, MezError, Result, SecretString};
use sha2::{Digest, Sha256};
use std::future::Future;
use std::pin::Pin;

/// Validates one required concrete provider-adapter field.
fn validate_non_empty(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        Err(MezError::invalid_args(format!("{field} must not be empty")))
    } else {
        Ok(())
    }
}

// Model provider traits and OpenAI Responses adapter.

mod anthropic;
mod chat_completions;
mod deepseek;
mod errors;
mod http;
mod openai_chat_completions;
use anthropic::AnthropicMessagesDialect;
pub use chat_completions::ChatCompletionsProvider;
use deepseek::DeepSeekChatCompletionsDialect;
#[cfg(test)]
pub use deepseek::build_deepseek_chat_completions_http_request;
use errors::provider_maap_parse_error;
pub(crate) use errors::{
    provider_error_retry_class, provider_error_retry_class_from_parts,
    provider_event_error_from_parts, provider_event_error_kind,
};
#[cfg(test)]
pub use http::ProviderHttpTransport;
pub use http::{AsyncProviderHttpTransport, ReqwestProviderHttpTransport};
use mez_agent::OpenAiResponsesStreamDecoder;
use mez_agent::parse_openai_models_http_body;
use mez_agent::provider_quota_usage_from_headers;
use mez_agent::{
    DEFAULT_PROVIDER_TIMEOUT_MS, ModelRequest, ModelResponse, ProviderAuthMetadata,
    ProviderCredentialKind, ProviderCredentialSource, ProviderHttpRequest, ProviderHttpResponse,
    ProviderModelCatalog,
};
use mez_agent::{
    openai_models_endpoint_for_responses_endpoint, openai_responses_endpoint_for_base_url,
    provider_catalog_reasoning_levels,
};
use mez_agent::{openai_responses_request_body_with_stream, parse_openai_responses_provider_body};
use mez_agent::{parse_fenced_maap_action_batch_for_turn, parse_maap_action_batch_json_for_turn};
use mez_agent::{
    provider_error_detail as openai_provider_error_detail,
    provider_failure_json_with_retry_headers as openai_provider_failure_json_with_retry_headers,
};
use openai_chat_completions::OpenAiChatCompletionsDialect;

use mez_agent::{CHATGPT_RESPONSES_ENDPOINT, OPENAI_RESPONSES_ENDPOINT};

/// Maximum decoded `say.text` bytes delivered in one progress event.
pub(crate) const STREAMING_SAY_TEXT_CHUNK_LIMIT_BYTES: usize = 16 * 1024;

/// Classifies one concrete provider request for cache and cost diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderRequestPurpose {
    /// User-visible agent execution, including capability and repair rounds.
    Execution,
    /// Automatic model or reasoning-profile selection.
    Routing,
    /// Internal macro, sandbox, or failure-assessment work.
    Auxiliary,
    /// Model-backed conversation compaction.
    Compaction,
    /// Model-backed durable-memory generation.
    Memory,
}

impl ProviderRequestPurpose {
    /// Returns the stable diagnostic name for this request class.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Execution => "execution",
            Self::Routing => "routing",
            Self::Auxiliary => "auxiliary",
            Self::Compaction => "compaction",
            Self::Memory => "memory",
        }
    }
}

/// Content-free observation of one concrete provider request and its outcome.
///
/// Prompt and tool-result bytes never enter this record. OpenAI-specific data
/// consists only of sizes, hashes, routing-key identity, and message digests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderWireRequestObservation {
    /// Monotonic identity within the owning worker dispatch.
    pub request_id: String,
    /// One-based wire-attempt ordinal within the logical provider call.
    pub attempt_index: usize,
    /// Content-free reason for an adapter-internal retry, when applicable.
    pub retry_reason: Option<String>,
    /// Conversation that owns the request.
    pub conversation_id: String,
    /// Runtime turn that owns the request.
    pub turn_id: String,
    /// Agent identity that owns the request.
    pub agent_id: String,
    /// Pane associated with the conversation.
    pub pane_id: String,
    /// Configured provider identity.
    pub provider: String,
    /// Hashed provider endpoint/account routing namespace.
    pub cache_namespace: String,
    /// Concrete model identity.
    pub model: String,
    /// Prompt-cache lineage identity, when available.
    pub prompt_cache_lineage_id: Option<String>,
    /// Provider interaction mode.
    pub interaction_kind: String,
    /// Current concrete action surface.
    pub allowed_actions: String,
    /// Provider output-token budget sent with this exact request.
    pub max_output_tokens: Option<usize>,
    /// Temporary output-limit retry override carried by this exact request.
    pub output_limit_retry_override_tokens: Option<usize>,
    /// Execution, routing, or other auxiliary request class.
    pub purpose: ProviderRequestPurpose,
    /// Number of provider-neutral request messages.
    pub message_count: usize,
    /// Total provider-neutral message content bytes.
    pub message_bytes: usize,
    /// Request-local MCP manifest and availability bytes.
    pub mcp_live_state_bytes: usize,
    /// Same-turn expanded action-result detail bytes.
    pub action_detail_bytes: usize,
    /// OpenAI Responses cache diagnostics, when applicable.
    pub openai_diagnostics: Option<mez_agent::OpenAiPromptCacheDiagnostics>,
    /// Whether OpenAI diagnostic construction failed independently of send.
    pub diagnostics_failed: bool,
    /// Exact usage returned by this request, when provider accounting exists.
    pub usage: Option<mez_agent::ModelTokenUsage>,
    /// Whether the provider request returned a usable response.
    pub succeeded: bool,
    /// Content-free failure classification when the request failed.
    pub failure_kind: Option<String>,
}

/// Actor-bound observation sink shared by every provider call in one worker.
pub struct ProviderWireRequestObserver {
    conversation_id: String,
    pane_id: String,
    sender: tokio::sync::mpsc::Sender<ProviderWireRequestObservation>,
}

/// Process-local monotonic identity for concrete provider requests.
static NEXT_PROVIDER_WIRE_REQUEST_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

impl ProviderWireRequestObserver {
    /// Creates one sequential observation owner for a provider worker.
    pub fn new(
        conversation_id: impl Into<String>,
        pane_id: impl Into<String>,
        sender: tokio::sync::mpsc::Sender<ProviderWireRequestObservation>,
    ) -> Self {
        Self {
            conversation_id: conversation_id.into(),
            pane_id: pane_id.into(),
            sender,
        }
    }

    /// Allocates the next process-local request identity.
    fn next_request_id(&self) -> String {
        let sequence = NEXT_PROVIDER_WIRE_REQUEST_ID
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .max(1);
        format!("wire-{sequence}")
    }
}

/// Observation state passed through one logical provider call.
pub struct ProviderWireObservationContext<'a> {
    observer: &'a ProviderWireRequestObserver,
    purpose: ProviderRequestPurpose,
    cache_namespace: String,
    openai_diagnostics: Option<mez_agent::OpenAiPromptCacheDiagnostics>,
    diagnostics_failed: bool,
}

impl<'a> ProviderWireObservationContext<'a> {
    /// Creates context for every concrete send made by one provider call.
    fn new(
        observer: &'a ProviderWireRequestObserver,
        purpose: ProviderRequestPurpose,
        cache_namespace: String,
        diagnostics: Result<Option<mez_agent::OpenAiPromptCacheDiagnostics>>,
    ) -> Self {
        let (openai_diagnostics, diagnostics_failed) = match diagnostics {
            Ok(diagnostics) => (diagnostics, false),
            Err(_) => (None, true),
        };
        Self {
            observer,
            purpose,
            cache_namespace,
            openai_diagnostics,
            diagnostics_failed,
        }
    }

    /// Emits one content-free observation for one concrete wire attempt.
    pub async fn observe(
        &self,
        request: &ModelRequest,
        attempt_index: usize,
        retry_reason: Option<&str>,
        result: &Result<ModelResponse>,
    ) {
        let usage = result.as_ref().ok().and_then(|response| {
            response.latest_request_usage.or_else(|| {
                let usage = response.usage;
                (!usage.is_zero()
                    || usage.cached_input_tokens.is_some()
                    || usage.cache_write_input_tokens.is_some())
                .then_some(usage)
            })
        });
        let failure_kind = result
            .as_ref()
            .err()
            .map(|error| provider_wire_failure_kind(error).to_string());
        let observation = ProviderWireRequestObservation {
            request_id: self.observer.next_request_id(),
            attempt_index,
            retry_reason: retry_reason.map(str::to_string),
            conversation_id: self.observer.conversation_id.clone(),
            turn_id: request.turn_id.clone(),
            agent_id: request.agent_id.clone(),
            pane_id: self.observer.pane_id.clone(),
            provider: request.provider.clone(),
            cache_namespace: self.cache_namespace.clone(),
            model: request.model.clone(),
            prompt_cache_lineage_id: request.prompt_cache_lineage_id.clone(),
            interaction_kind: request.interaction_kind.as_str().to_string(),
            allowed_actions: request.allowed_actions.action_type_names().join(","),
            max_output_tokens: request.max_output_tokens,
            output_limit_retry_override_tokens: request.max_output_tokens.filter(|_| {
                request.interaction_kind == mez_agent::ModelInteractionKind::OutputLimitRetry
            }),
            purpose: self.purpose,
            message_count: request.messages.len(),
            message_bytes: request.messages.iter().fold(0usize, |total, message| {
                total.saturating_add(message.content.len())
            }),
            mcp_live_state_bytes: request
                .messages
                .iter()
                .filter(|message| {
                    message.placement == mez_agent::ContextPlacement::EphemeralTail
                        && message.source == mez_agent::ContextSourceKind::RuntimeHint
                        && message.content.starts_with("[mcp integrations]\n")
                })
                .fold(0usize, |total, message| {
                    total.saturating_add(message.content.len())
                }),
            action_detail_bytes: request
                .messages
                .iter()
                .filter(|message| {
                    message.placement == mez_agent::ContextPlacement::EphemeralTail
                        && message.source == mez_agent::ContextSourceKind::ActionDetail
                })
                .fold(0usize, |total, message| {
                    total.saturating_add(message.content.len())
                }),
            openai_diagnostics: self.openai_diagnostics.clone(),
            diagnostics_failed: self.diagnostics_failed,
            usage,
            succeeded: result.is_ok(),
            failure_kind,
        };
        let _ = self.observer.sender.send(observation).await;
    }
}

/// Returns a stable content-free provider failure classification.
fn provider_wire_failure_kind(error: &MezError) -> &'static str {
    match error.kind() {
        crate::error::MezErrorKind::InvalidArgs => "invalid_args",
        crate::error::MezErrorKind::InvalidState => "invalid_state",
        crate::error::MezErrorKind::Config => "config",
        crate::error::MezErrorKind::Io => "io",
        crate::error::MezErrorKind::Conflict => "conflict",
        crate::error::MezErrorKind::NotFound => "not_found",
        crate::error::MezErrorKind::Forbidden => "forbidden",
        crate::error::MezErrorKind::RateLimited => "rate_limited",
        crate::error::MezErrorKind::NotImplemented => "not_implemented",
    }
}

/// Provider adapter that records every concrete request without retaining prompt bytes.
pub struct ObservedAsyncModelProvider<'a, P> {
    provider: &'a P,
    observer: &'a ProviderWireRequestObserver,
    purpose: ProviderRequestPurpose,
}

impl<'a, P> ObservedAsyncModelProvider<'a, P> {
    /// Wraps one concrete provider with a worker-owned diagnostic observer.
    pub fn new(
        provider: &'a P,
        observer: &'a ProviderWireRequestObserver,
        purpose: ProviderRequestPurpose,
    ) -> Self {
        Self {
            provider,
            observer,
            purpose,
        }
    }
}

/// Builds a stable, non-secret provider cache namespace from endpoint routing.
pub(super) fn provider_cache_namespace(provider_id: &str, endpoint: &str) -> String {
    provider_cache_namespace_with_headers(provider_id, endpoint, &BTreeMap::new())
}

/// Builds a cache namespace that includes non-secret account-routing headers.
fn provider_cache_namespace_with_headers(
    provider_id: &str,
    endpoint: &str,
    routing_headers: &BTreeMap<String, String>,
) -> String {
    let mut material = endpoint.to_string();
    for (name, value) in routing_headers.iter().filter(|(name, _)| {
        name.eq_ignore_ascii_case(OPENAI_ORGANIZATION_HEADER)
            || name.eq_ignore_ascii_case(OPENAI_PROJECT_HEADER)
            || name.eq_ignore_ascii_case(CHATGPT_ACCOUNT_ID_HEADER)
    }) {
        material.push('\0');
        material.push_str(&name.to_ascii_lowercase());
        material.push('=');
        material.push_str(value);
    }
    let digest = Sha256::digest(material.as_bytes());
    let endpoint_digest = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{provider_id}:{endpoint_digest}")
}

#[cfg(test)]
mod provider_wire_namespace_tests {
    use super::*;

    /// Verifies cache namespaces include only non-secret provider routing
    /// identity and ignore credentials and unrelated request headers.
    #[test]
    fn cache_namespace_hashes_only_allowlisted_routing_headers() {
        let endpoint = "https://api.openai.test/v1/responses";
        let base = provider_cache_namespace_with_headers("openai", endpoint, &BTreeMap::new());
        let mut unrelated = BTreeMap::new();
        unrelated.insert(
            "Authorization".to_string(),
            "Bearer private-token".to_string(),
        );
        unrelated.insert("X-Diagnostic".to_string(), "changed".to_string());
        assert_eq!(
            provider_cache_namespace_with_headers("openai", endpoint, &unrelated),
            base
        );

        for header in [
            OPENAI_ORGANIZATION_HEADER,
            OPENAI_PROJECT_HEADER,
            CHATGPT_ACCOUNT_ID_HEADER,
        ] {
            let mut routed = unrelated.clone();
            routed.insert(header.to_ascii_lowercase(), "route-a".to_string());
            let first = provider_cache_namespace_with_headers("openai", endpoint, &routed);
            routed.insert(header.to_ascii_uppercase(), "route-b".to_string());
            let second = provider_cache_namespace_with_headers("openai", endpoint, &routed);
            assert_ne!(first, base, "{header} must affect cache routing scope");
            assert_ne!(second, first, "{header} value changes must isolate scope");
        }
    }
}

/// Returns one MAAP-bearing text or native-argument fragment from an SSE event.
pub(super) fn provider_maap_stream_fragment(event: &mez_agent::SseEvent) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(event.data.trim()).ok()?;
    value
        .get("delta")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            value
                .pointer("/delta/content")
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            value
                .pointer("/delta/partial_json")
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            value
                .pointer("/choices/0/delta/content")
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            value
                .pointer("/choices/0/delta/tool_calls/0/function/arguments")
                .and_then(serde_json::Value::as_str)
        })
        .filter(|fragment| !fragment.is_empty())
        .map(str::to_string)
}

/// Splits oversized text deltas at UTF-8 boundaries while preserving barriers.
fn bounded_streaming_say_events(
    events: Vec<mez_agent::StreamingSayEvent>,
) -> Vec<mez_agent::StreamingSayEvent> {
    let mut bounded = Vec::new();
    for event in events {
        let (text, rebuild): (String, Box<dyn Fn(String) -> mez_agent::StreamingSayEvent>) =
            match event {
                mez_agent::StreamingSayEvent::RationaleTextDelta { text } => (
                    text,
                    Box::new(|text| mez_agent::StreamingSayEvent::RationaleTextDelta { text }),
                ),
                mez_agent::StreamingSayEvent::TextDelta { action_index, text } => (
                    text,
                    Box::new(move |text| mez_agent::StreamingSayEvent::TextDelta {
                        action_index,
                        text,
                    }),
                ),
                mez_agent::StreamingSayEvent::ShellCommandTextDelta { action_index, text } => (
                    text,
                    Box::new(
                        move |text| mez_agent::StreamingSayEvent::ShellCommandTextDelta {
                            action_index,
                            text,
                        },
                    ),
                ),
                event => {
                    bounded.push(event);
                    continue;
                }
            };
        if text.is_empty() {
            continue;
        };
        let mut start = 0usize;
        while start < text.len() {
            let mut end = start
                .saturating_add(STREAMING_SAY_TEXT_CHUNK_LIMIT_BYTES)
                .min(text.len());
            while end > start && !text.is_char_boundary(end) {
                end -= 1;
            }
            if end == start {
                let Some(character) = text[start..].chars().next() else {
                    break;
                };
                end = start.saturating_add(character.len_utf8());
            }
            bounded.push(rebuild(text[start..end].to_string()));
            start = end;
        }
    }
    bounded
}

#[cfg(test)]
mod streaming_say_progress_tests {
    use super::*;

    /// Verifies oversized deltas are split losslessly without cutting a
    /// multi-byte scalar or moving lifecycle barriers around the text.
    #[test]
    fn bounded_streaming_say_events_preserve_utf8_and_order() {
        let source = format!(
            "{}😀{}",
            "a".repeat(STREAMING_SAY_TEXT_CHUNK_LIMIT_BYTES - 1),
            "b".repeat(STREAMING_SAY_TEXT_CHUNK_LIMIT_BYTES)
        );
        let events = bounded_streaming_say_events(vec![
            mez_agent::StreamingSayEvent::TextDelta {
                action_index: 3,
                text: source.clone(),
            },
            mez_agent::StreamingSayEvent::TextComplete { action_index: 3 },
        ]);

        assert!(events.len() >= 3, "events={events:?}");
        assert!(events[..events.len() - 1].iter().all(|event| matches!(
            event,
            mez_agent::StreamingSayEvent::TextDelta { text, .. }
                if text.len() <= STREAMING_SAY_TEXT_CHUNK_LIMIT_BYTES
        )));
        assert!(matches!(
            events.last(),
            Some(mez_agent::StreamingSayEvent::TextComplete { action_index: 3 })
        ));
        let reconstructed = events
            .iter()
            .filter_map(|event| match event {
                mez_agent::StreamingSayEvent::TextDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(reconstructed, source);
    }
}
/// OpenAI organization routing header for multi-organization API keys.
pub const OPENAI_ORGANIZATION_HEADER: &str = "OpenAI-Organization";
/// OpenAI project routing header for project-scoped API accounting.
pub const OPENAI_PROJECT_HEADER: &str = "OpenAI-Project";
/// ChatGPT account selection header required by ChatGPT-backed requests.
pub const CHATGPT_ACCOUNT_ID_HEADER: &str = "ChatGPT-Account-ID";

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

    /// Returns a non-secret identity for the provider's cache-routing namespace.
    fn cache_namespace(&self) -> String {
        self.provider_id().to_string()
    }

    /// Builds provider-native prompt-cache diagnostics for one concrete send.
    fn prompt_cache_diagnostics(
        &self,
        _request: &ModelRequest,
    ) -> Result<Option<mez_agent::OpenAiPromptCacheDiagnostics>> {
        Ok(None)
    }
    /// Runs the send request async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn send_request_async<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>>;

    /// Sends one request while reporting ordered visible streaming say events.
    fn send_request_async_with_progress<'a>(
        &'a self,
        request: &'a ModelRequest,
        _progress: Option<tokio::sync::mpsc::Sender<mez_agent::StreamingSayEvent>>,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>> {
        self.send_request_async(request)
    }

    /// Sends one logical request while observing each concrete wire attempt.
    fn send_request_async_with_progress_and_observation<'a>(
        &'a self,
        request: &'a ModelRequest,
        progress: Option<tokio::sync::mpsc::Sender<mez_agent::StreamingSayEvent>>,
        observation: Option<ProviderWireObservationContext<'a>>,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>> {
        Box::pin(async move {
            let result = self
                .send_request_async_with_progress(request, progress)
                .await;
            if let Some(observation) = observation.as_ref() {
                observation.observe(request, 1, None, &result).await;
            }
            result
        })
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
            Err(MezError::invalid_state(format!(
                "provider `{}` does not expose a model catalog",
                self.provider_id()
            )))
        })
    }
}

impl<P: AsyncModelProvider> AsyncModelProvider for ObservedAsyncModelProvider<'_, P> {
    fn provider_id(&self) -> &str {
        self.provider.provider_id()
    }

    fn cache_namespace(&self) -> String {
        self.provider.cache_namespace()
    }

    fn prompt_cache_diagnostics(
        &self,
        request: &ModelRequest,
    ) -> Result<Option<mez_agent::OpenAiPromptCacheDiagnostics>> {
        self.provider.prompt_cache_diagnostics(request)
    }

    fn send_request_async<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>> {
        self.send_request_async_with_progress(request, None)
    }

    fn send_request_async_with_progress<'a>(
        &'a self,
        request: &'a ModelRequest,
        progress: Option<tokio::sync::mpsc::Sender<mez_agent::StreamingSayEvent>>,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>> {
        let observation = ProviderWireObservationContext::new(
            self.observer,
            self.purpose,
            self.provider.cache_namespace(),
            self.provider.prompt_cache_diagnostics(request),
        );
        self.provider
            .send_request_async_with_progress_and_observation(request, progress, Some(observation))
    }
}

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
    /// Reports whether this Responses provider emits server-sent events.
    pub fn streams_responses(&self) -> bool {
        self.stream
    }

    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn new(api_key: impl Into<String>, transport: T) -> Result<Self> {
        let api_key = SecretString::from(api_key.into());
        Self::new_secret(api_key, transport)
    }

    /// Runs the new secret operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
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
    #[cfg(test)]
    #[allow(
        dead_code,
        reason = "test-only adapter retained for focused boundary coverage"
    )]
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
    #[cfg(test)]
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
    auth_store: &dyn ProviderCredentialSource<Error = MezError, Credential = SecretString>,
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
#[cfg(test)]
pub fn openai_provider_from_auth_store_with_options<T>(
    auth_store: &dyn ProviderCredentialSource<Error = MezError, Credential = SecretString>,
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
#[cfg(test)]
pub fn openai_provider_from_auth_store_with_provider_options<T>(
    auth_store: &dyn ProviderCredentialSource<Error = MezError, Credential = SecretString>,
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
    auth_store: &dyn ProviderCredentialSource<Error = MezError, Credential = SecretString>,
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
    let Some(metadata) = auth_store.provider_auth_metadata(provider_name)? else {
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
        ProviderCredentialKind::ApiKey => {
            let credential = auth_store.provider_credential(provider_name)?;
            OpenAiResponsesProvider::with_endpoint_and_headers(
                credential,
                endpoint,
                timeout_ms,
                openai_direct_api_extra_headers(&metadata, provider_options),
                transport,
            )
            .and_then(|provider| provider.with_provider_id(provider_name))
        }
        ProviderCredentialKind::ChatGpt => {
            if provider_name != "openai" {
                return Err(MezError::invalid_state(format!(
                    "OpenAI Responses-compatible provider `{provider_name}` cannot use ChatGPT browser credentials"
                )));
            }
            let credential = auth_store.provider_credential(provider_name)?;
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
#[cfg(test)]
#[allow(
    dead_code,
    reason = "test-only adapter retained for focused boundary coverage"
)]
pub fn deepseek_provider_from_auth_store_with_provider_options<T>(
    auth_store: &dyn ProviderCredentialSource<Error = MezError, Credential = SecretString>,
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
    auth_store: &dyn ProviderCredentialSource<Error = MezError, Credential = SecretString>,
    provider_name: &str,
    base_url_override: Option<&str>,
    timeout_ms: u64,
    transport: T,
) -> Result<DeepSeekChatCompletionsProvider<T>> {
    let mut provider = if auth_store.provider_auth_metadata(provider_name)?.is_some() {
        let credential = auth_store.provider_credential(provider_name)?;
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
    auth_store: &dyn ProviderCredentialSource<Error = MezError, Credential = SecretString>,
    provider_name: &str,
    base_url_override: Option<&str>,
    provider_options: &BTreeMap<String, String>,
    timeout_ms: u64,
    transport: T,
) -> Result<AnthropicMessagesProvider<T>> {
    let dialect = AnthropicMessagesDialect::from_provider_options(provider_options)?;
    let Some(metadata) = auth_store.provider_auth_metadata(provider_name)? else {
        return Err(MezError::invalid_state(format!(
            "Anthropic provider `{provider_name}` requires an authenticated API key"
        )));
    };
    if metadata.credential_kind != ProviderCredentialKind::ApiKey {
        return Err(MezError::invalid_state(format!(
            "Anthropic provider `{provider_name}` requires API-key credentials"
        )));
    }
    let credential = auth_store.provider_credential(provider_name)?;
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
    auth_store: &dyn ProviderCredentialSource<Error = MezError, Credential = SecretString>,
    provider_name: &str,
    base_url_override: Option<&str>,
    provider_options: &BTreeMap<String, String>,
    timeout_ms: u64,
    transport: T,
) -> Result<OpenAiCompatibleChatCompletionsProvider<T>> {
    let dialect = OpenAiChatCompletionsDialect::from_provider_options(provider_options)?;
    let api_key = if auth_store.provider_auth_metadata(provider_name)?.is_some() {
        Some(auth_store.provider_credential(provider_name)?)
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
    metadata: &ProviderAuthMetadata,
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
            .with_provider_failure_json(
                openai_provider_failure_json_with_retry_headers(
                    Some(response.status_code),
                    &response.body,
                    &response.headers,
                ),
            ));
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
            .with_provider_failure_json(
                openai_provider_failure_json_with_retry_headers(
                    Some(response.status_code),
                    &response.body,
                    &response.headers,
                ),
            ));
        }
        let (model, raw_text, usage, provider_transcript_events) =
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
            provider_transcript_events,
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

    fn cache_namespace(&self) -> String {
        provider_cache_namespace_with_headers(
            self.provider_id(),
            &self.endpoint,
            &self.extra_headers,
        )
    }

    fn prompt_cache_diagnostics(
        &self,
        request: &ModelRequest,
    ) -> Result<Option<mez_agent::OpenAiPromptCacheDiagnostics>> {
        mez_agent::openai_prompt_cache_diagnostics_for_request_with_stream(request, self.stream)
            .map(Some)
            .map_err(MezError::from)
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
                .with_provider_failure_json(
                    openai_provider_failure_json_with_retry_headers(
                        Some(response.status_code),
                        &response.body,
                        &response.headers,
                    ),
                ));
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
        self.send_request_async_with_progress(request, None)
    }

    fn send_request_async_with_progress<'a>(
        &'a self,
        request: &'a ModelRequest,
        progress: Option<tokio::sync::mpsc::Sender<mez_agent::StreamingSayEvent>>,
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
            let mut stream_decoder = OpenAiResponsesStreamDecoder::default();
            let mut streaming_say_extractor = mez_agent::StreamingSayExtractor::default();
            let mut stream_error = None;
            let response = if self.stream {
                let mut on_event = |event| {
                    let mut progress_events = Vec::new();
                    if stream_error.is_none() {
                        match stream_decoder.push_event(&event) {
                            Ok(Some(_)) => {}
                            Ok(None) => {}
                            Err(error) => stream_error = Some(error),
                        }
                        if let Some(fragment) = provider_maap_stream_fragment(&event) {
                            progress_events = bounded_streaming_say_events(
                                streaming_say_extractor.push_delta(&fragment),
                            );
                        }
                    }
                    let progress = progress.clone();
                    Box::pin(async move {
                        let Some(progress) = progress else {
                            return;
                        };
                        for event in progress_events {
                            if progress.send(event).await.is_err() {
                                break;
                            }
                        }
                    }) as Pin<Box<dyn Future<Output = ()> + Send>>
                };
                self.transport
                    .send_async_with_sse_events(&http_request, &mut on_event)
                    .await?
            } else {
                self.transport.send_async(&http_request).await?
            };
            if !(200..300).contains(&response.status_code) {
                return Err(MezError::invalid_state(format!(
                    "OpenAI Responses API returned status {}: {}",
                    response.status_code,
                    openai_provider_error_detail(&response.body)
                ))
                .with_provider_failure_json(
                    openai_provider_failure_json_with_retry_headers(
                        Some(response.status_code),
                        &response.body,
                        &response.headers,
                    ),
                ));
            }
            if let Some(error) = stream_error {
                return Err(error.into());
            }
            let (model, raw_text, usage, provider_transcript_events) = if self.stream {
                stream_decoder.finish(&request.model)?
            } else {
                parse_openai_responses_provider_body(&response.body, &request.model, false)?
            };
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
                provider_transcript_events,
            })
        })
    }
}

/// Runs the build openai responses http request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "test-only adapter retained for focused boundary coverage"
)]
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
        timeouts: mez_agent::ProviderHttpTimeouts::from_total(timeout_ms),
        max_response_bytes: None,
    })
}

/// Runs the build openai models http request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "test-only adapter retained for focused boundary coverage"
)]
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
        timeouts: mez_agent::ProviderHttpTimeouts::from_total(timeout_ms),
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
