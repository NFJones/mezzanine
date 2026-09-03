//! Provider-neutral Chat Completions transport shell.
//!
//! This module owns the shared Chat Completions provider mechanics: bearer
//! credential storage, provider identity guards, endpoint/timeout/stream
//! settings, HTTP dispatch, status-code wrapping, model-list execution, and
//! retry accounting. Provider-specific wire shape and policy are delegated to a
//! `ChatCompletionsDialect` implementation so DeepSeek behavior does not become
//! the default for every OpenAI-compatible backend.

use super::{
    AsyncModelProvider, AsyncProviderHttpTransport, DEFAULT_PROVIDER_TIMEOUT_MS, ExposeSecret,
    MezError, ModelRequest, ModelResponse, ProviderHttpRequest, ProviderHttpResponse,
    ProviderModelCatalog, ProviderWireObservationContext, Result, SecretString,
    bounded_streaming_say_events, parse_openai_models_http_body, provider_maap_stream_fragment,
    provider_quota_usage_from_headers, validate_non_empty,
};
#[cfg(test)]
use super::{ModelProvider, ProviderHttpTransport};
use mez_agent::{
    SseEvent, provider_catalog_reasoning_levels,
    provider_error_detail as openai_provider_error_detail,
    provider_failure_json as openai_provider_failure_json,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

/// Incremental provider-specific state for one Chat Completions SSE response.
pub trait ChatCompletionsStreamDecoder: Send {
    /// Applies one complete SSE event and returns display-safe text progress.
    fn push_event(&mut self, event: &SseEvent) -> Result<Option<String>>;

    /// Finalizes the accumulated stream into the existing response projection.
    fn finish(
        self: Box<Self>,
        headers: BTreeMap<String, String>,
        request: &ModelRequest,
        provider_id: &str,
    ) -> Result<ModelResponse>;
}

/// Provider-specific behavior required by the Chat Completions transport shell.
pub trait ChatCompletionsDialect: Clone + Send + Sync + Default + 'static {
    /// Returns the provider id used before configuration overrides are applied.
    fn default_provider_id(&self) -> &'static str;

    /// Returns the default Chat Completions endpoint for this dialect.
    fn default_chat_endpoint(&self) -> &'static str;

    /// Returns a human-readable provider label for diagnostics.
    fn provider_label(&self) -> &'static str;

    /// Returns the diagnostic label used when validating a bearer credential.
    fn credential_label(&self) -> &'static str;

    /// Extracts one provider-owned error detail string from a failed HTTP body.
    fn provider_error_detail(&self, body: &str) -> String {
        openai_provider_error_detail(body)
    }

    /// Shapes one sanitized provider failure JSON payload from a failed HTTP body.
    fn provider_failure_json(&self, status_code: Option<u16>, body: &str) -> String {
        openai_provider_failure_json(status_code, body)
    }

    /// Derives the chat-completions endpoint from a configured base URL.
    fn chat_endpoint_for_base_url(&self, base_url: &str) -> Result<String>;

    /// Builds one provider-specific chat-completions HTTP request.
    fn build_chat_request(
        &self,
        request: &ModelRequest,
        api_key: Option<&str>,
        endpoint: &str,
        stream: bool,
        timeout_ms: u64,
    ) -> Result<ProviderHttpRequest>;

    /// Parses one successful provider-specific chat-completions HTTP response.
    fn parse_chat_response(
        &self,
        response: ProviderHttpResponse,
        request: &ModelRequest,
        provider_id: &str,
        stream: bool,
    ) -> Result<ModelResponse>;

    /// Returns the stream parser mode that should be used for a request.
    fn effective_stream(&self, _request: &ModelRequest, stream: bool) -> bool {
        stream
    }

    /// Builds provider-specific incremental state for one streaming request.
    fn stream_decoder(
        &self,
        _request: &ModelRequest,
    ) -> Result<Option<Box<dyn ChatCompletionsStreamDecoder>>> {
        Ok(None)
    }

    /// Builds an optional retry request after a parseable but inadequate reply.
    fn build_retry_chat_request(
        &self,
        _request: &ModelRequest,
        _api_key: Option<&str>,
        _endpoint: &str,
        _stream: bool,
        _timeout_ms: u64,
        _previous_response: &ModelResponse,
    ) -> Result<Option<ChatCompletionsRetry>> {
        Ok(None)
    }

    /// Builds the provider-specific model catalog HTTP request.
    fn build_models_request(
        &self,
        api_key: Option<&str>,
        chat_endpoint: &str,
        timeout_ms: u64,
    ) -> Result<ProviderHttpRequest>;

    /// Applies provider-specific post-parse requirements to a response.
    fn require_action_batch(
        &self,
        response: ModelResponse,
        _request: &ModelRequest,
    ) -> Result<ModelResponse> {
        Ok(response)
    }
}

/// Provider-specific retry request plus the response parser mode for that retry.
#[derive(Debug, Clone)]
pub struct ChatCompletionsRetry {
    /// HTTP request to send for the retry.
    pub request: ProviderHttpRequest,
    /// Whether the retry response should be parsed as a stream.
    pub stream: bool,
}

/// Carries shared Chat Completions provider state.
#[derive(Debug, Clone)]
pub struct ChatCompletionsProvider<T, D> {
    pub(in crate::integrations::agent) api_key: Option<SecretString>,
    pub(in crate::integrations::agent) provider_id: String,
    pub(in crate::integrations::agent) endpoint: String,
    pub(in crate::integrations::agent) stream: bool,
    pub(in crate::integrations::agent) timeout_ms: u64,
    pub(in crate::integrations::agent) transport: T,
    dialect: D,
}

impl<T, D> ChatCompletionsProvider<T, D>
where
    D: ChatCompletionsDialect,
{
    /// Creates a new Chat Completions provider with the given API key.
    pub fn new(api_key: impl Into<SecretString>, transport: T) -> Result<Self> {
        let api_key = api_key.into();
        let dialect = D::default();
        validate_non_empty(dialect.credential_label(), api_key.expose_secret())?;
        Self::with_optional_auth_and_dialect(Some(api_key), transport, dialect)
    }

    /// Creates a Chat Completions provider without bearer authentication.
    pub fn without_auth(transport: T) -> Result<Self> {
        Self::with_optional_auth(None, transport)
    }

    /// Creates a Chat Completions provider with optional bearer authentication.
    pub fn with_optional_auth(api_key: Option<SecretString>, transport: T) -> Result<Self> {
        Self::with_optional_auth_and_dialect(api_key, transport, D::default())
    }

    /// Creates a Chat Completions provider with an explicit dialect instance.
    pub(in crate::integrations::agent) fn with_optional_auth_and_dialect(
        api_key: Option<SecretString>,
        transport: T,
        dialect: D,
    ) -> Result<Self> {
        if let Some(api_key) = api_key.as_ref() {
            validate_non_empty(dialect.credential_label(), api_key.expose_secret())?;
        }
        Ok(Self {
            api_key,
            provider_id: dialect.default_provider_id().to_string(),
            endpoint: dialect.default_chat_endpoint().to_string(),
            stream: false,
            timeout_ms: DEFAULT_PROVIDER_TIMEOUT_MS,
            transport,
            dialect,
        })
    }

    /// Returns the configured provider id guarded by this provider instance.
    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    /// Returns whether this provider will stream one concrete request.
    pub(crate) fn streams_request(&self, request: &ModelRequest) -> bool {
        self.dialect.effective_stream(request, self.stream)
    }

    /// Overrides the runtime provider identity accepted by request guards.
    pub fn with_provider_id(mut self, provider_id: impl Into<String>) -> Result<Self> {
        let provider_id = provider_id.into();
        validate_non_empty("provider id", &provider_id)?;
        self.provider_id = provider_id;
        Ok(self)
    }

    /// Enables or disables streaming for this provider.
    #[cfg(test)]
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

    /// Derives this provider's chat endpoint from a configured base URL.
    pub fn chat_endpoint_for_base_url(&self, base_url: &str) -> Result<String> {
        self.dialect.chat_endpoint_for_base_url(base_url)
    }

    /// Returns the bearer credential as a borrowed secret string.
    fn api_key_secret(&self) -> Option<&str> {
        self.api_key.as_ref().map(|api_key| api_key.expose_secret())
    }

    /// Builds a provider-identity mismatch error for this dialect.
    fn provider_mismatch_error(&self) -> MezError {
        MezError::invalid_args(format!(
            "{} provider received a request for a different provider",
            self.dialect.provider_label()
        ))
    }

    /// Builds a provider status-code error with sanitized failure metadata.
    fn provider_status_error(&self, surface: &str, response: &ProviderHttpResponse) -> MezError {
        let mut failure = serde_json::from_str::<Value>(
            &self
                .dialect
                .provider_failure_json(Some(response.status_code), &response.body),
        )
        .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
        if let Some(retry_after) = response
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
            .map(|(_, value)| value.trim())
            .filter(|value| !value.is_empty())
            && let Some(object) = failure.as_object_mut()
        {
            object.insert(
                "retry_after".to_string(),
                Value::String(retry_after.chars().take(128).collect()),
            );
        }
        MezError::invalid_state(format!(
            "{} {surface} API returned status {}: {}",
            self.dialect.provider_label(),
            response.status_code,
            self.dialect.provider_error_detail(&response.body)
        ))
        .with_provider_failure_json(failure.to_string())
    }

    /// Parses a successful model catalog response into shared catalog metadata.
    fn model_catalog_from_response(
        &self,
        response: ProviderHttpResponse,
    ) -> Result<ProviderModelCatalog> {
        let models = parse_openai_models_http_body(&response.body)?;
        let reasoning_levels = provider_catalog_reasoning_levels(&models);
        let quota_usage = provider_quota_usage_from_headers(&response.headers);
        Ok(ProviderModelCatalog {
            provider: self.provider_id().to_string(),
            source: "provider".to_string(),
            models,
            reasoning_levels,
            quota_usage,
        })
    }

    /// Merges first-request usage into a retry response that produced the result.
    fn merge_retry_usage(
        &self,
        mut retry: ModelResponse,
        previous: ModelResponse,
    ) -> ModelResponse {
        retry.latest_request_usage = Some(retry.usage);
        retry.usage.add_assign(previous.usage);
        if retry.quota_usage.is_empty() {
            retry.quota_usage = previous.quota_usage;
        }
        retry
    }
}

#[cfg(test)]
impl<T, D> ModelProvider for ChatCompletionsProvider<T, D>
where
    T: ProviderHttpTransport,
    D: ChatCompletionsDialect,
{
    fn provider_id(&self) -> &str {
        self.provider_id()
    }

    fn list_models(&self) -> Result<ProviderModelCatalog> {
        let http_request = self.dialect.build_models_request(
            self.api_key_secret(),
            &self.endpoint,
            self.timeout_ms,
        )?;
        let response = self.transport.send(&http_request)?;
        if !(200..300).contains(&response.status_code) {
            return Err(self.provider_status_error("Models", &response));
        }
        self.model_catalog_from_response(response)
    }

    fn send_request(&self, request: &ModelRequest) -> Result<ModelResponse> {
        if request.provider != ModelProvider::provider_id(self) {
            return Err(self.provider_mismatch_error());
        }
        let http_request = self.dialect.build_chat_request(
            request,
            self.api_key_secret(),
            &self.endpoint,
            self.stream,
            self.timeout_ms,
        )?;
        let response = self.transport.send(&http_request)?;
        if !(200..300).contains(&response.status_code) {
            return Err(self.provider_status_error("Chat Completions", &response));
        }
        let mut parsed = self.dialect.parse_chat_response(
            response,
            request,
            ModelProvider::provider_id(self),
            self.dialect.effective_stream(request, self.stream),
        )?;
        if let Some(retry) = self.dialect.build_retry_chat_request(
            request,
            self.api_key_secret(),
            &self.endpoint,
            self.stream,
            self.timeout_ms,
            &parsed,
        )? {
            let retry_response = self.transport.send(&retry.request)?;
            if !(200..300).contains(&retry_response.status_code) {
                return Err(self.provider_status_error("Chat Completions", &retry_response));
            }
            let retry_parsed = self.dialect.parse_chat_response(
                retry_response,
                request,
                ModelProvider::provider_id(self),
                retry.stream,
            )?;
            parsed = self.merge_retry_usage(retry_parsed, parsed);
        }
        self.dialect.require_action_batch(parsed, request)
    }
}

impl<T, D> AsyncModelProvider for ChatCompletionsProvider<T, D>
where
    T: AsyncProviderHttpTransport,
    D: ChatCompletionsDialect,
{
    fn provider_id(&self) -> &str {
        self.provider_id()
    }

    fn cache_namespace(&self) -> String {
        super::provider_cache_namespace(self.provider_id(), &self.endpoint)
    }

    fn list_models_async<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<ProviderModelCatalog>> + Send + 'a>> {
        Box::pin(async move {
            let http_request = self.dialect.build_models_request(
                self.api_key_secret(),
                &self.endpoint,
                self.timeout_ms,
            )?;
            let response = self.transport.send_async(&http_request).await?;
            if !(200..300).contains(&response.status_code) {
                return Err(self.provider_status_error("Models", &response));
            }
            self.model_catalog_from_response(response)
        })
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
        self.send_request_async_with_progress_and_observation(request, progress, None)
    }

    fn send_request_async_with_progress_and_observation<'a>(
        &'a self,
        request: &'a ModelRequest,
        progress: Option<tokio::sync::mpsc::Sender<mez_agent::StreamingSayEvent>>,
        observation: Option<ProviderWireObservationContext<'a>>,
    ) -> Pin<Box<dyn Future<Output = Result<ModelResponse>> + Send + 'a>> {
        Box::pin(async move {
            if request.provider != AsyncModelProvider::provider_id(self) {
                return Err(self.provider_mismatch_error());
            }
            let http_request = self.dialect.build_chat_request(
                request,
                self.api_key_secret(),
                &self.endpoint,
                self.stream,
                self.timeout_ms,
            )?;
            let effective_stream = self.dialect.effective_stream(request, self.stream);
            let mut stream_decoder = if effective_stream {
                self.dialect.stream_decoder(request)?
            } else {
                None
            };
            let mut streaming_say_extractor = mez_agent::StreamingSayExtractor::default();
            let mut stream_error = None;
            let response_result = if let Some(decoder) = stream_decoder.as_mut() {
                let mut on_event = |event| {
                    let mut progress_events = Vec::new();
                    if stream_error.is_none() {
                        match decoder.push_event(&event) {
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
                    .await
            } else {
                self.transport.send_async(&http_request).await
            };
            let response = match response_result {
                Ok(response) => response,
                Err(error) => {
                    let result = Err(error.into());
                    if let Some(observation) = observation.as_ref() {
                        observation.observe(request, 1, None, &result).await;
                    }
                    return result;
                }
            };
            if !(200..300).contains(&response.status_code) {
                let result = Err(self.provider_status_error("Chat Completions", &response));
                if let Some(observation) = observation.as_ref() {
                    observation.observe(request, 1, None, &result).await;
                }
                return result;
            }
            if let Some(error) = stream_error {
                let result = Err(error);
                if let Some(observation) = observation.as_ref() {
                    observation.observe(request, 1, None, &result).await;
                }
                return result;
            }
            let parsed_result = if let Some(decoder) = stream_decoder {
                decoder.finish(
                    response.headers,
                    request,
                    AsyncModelProvider::provider_id(self),
                )
            } else {
                self.dialect.parse_chat_response(
                    response,
                    request,
                    AsyncModelProvider::provider_id(self),
                    effective_stream,
                )
            };
            let mut parsed = match parsed_result {
                Ok(parsed) => parsed,
                Err(error) => {
                    let result = Err(error);
                    if let Some(observation) = observation.as_ref() {
                        observation.observe(request, 1, None, &result).await;
                    }
                    return result;
                }
            };
            let retry = match self.dialect.build_retry_chat_request(
                request,
                self.api_key_secret(),
                &self.endpoint,
                self.stream,
                self.timeout_ms,
                &parsed,
            ) {
                Ok(retry) => retry,
                Err(error) => {
                    if let Some(observation) = observation.as_ref() {
                        let first_result = Ok(parsed);
                        observation.observe(request, 1, None, &first_result).await;
                    }
                    return Err(error);
                }
            };
            if let Some(retry) = retry {
                if let Some(observation) = observation.as_ref() {
                    let first_result = Ok(parsed.clone());
                    observation
                        .observe(request, 1, Some("provider_forced_maap"), &first_result)
                        .await;
                }
                let retry_response = match self.transport.send_async(&retry.request).await {
                    Ok(response) => response,
                    Err(error) => {
                        let result = Err(error.into());
                        if let Some(observation) = observation.as_ref() {
                            observation
                                .observe(request, 2, Some("provider_forced_maap"), &result)
                                .await;
                        }
                        return result;
                    }
                };
                if !(200..300).contains(&retry_response.status_code) {
                    let result =
                        Err(self.provider_status_error("Chat Completions", &retry_response));
                    if let Some(observation) = observation.as_ref() {
                        observation
                            .observe(request, 2, Some("provider_forced_maap"), &result)
                            .await;
                    }
                    return result;
                }
                let retry_result = self.dialect.parse_chat_response(
                    retry_response,
                    request,
                    AsyncModelProvider::provider_id(self),
                    retry.stream,
                );
                let retry_parsed = match retry_result {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        let result = Err(error);
                        if let Some(observation) = observation.as_ref() {
                            observation
                                .observe(request, 2, Some("provider_forced_maap"), &result)
                                .await;
                        }
                        return result;
                    }
                };
                parsed = self.merge_retry_usage(retry_parsed, parsed);
                let result = self.dialect.require_action_batch(parsed, request);
                if let Some(observation) = observation.as_ref() {
                    observation
                        .observe(request, 2, Some("provider_forced_maap"), &result)
                        .await;
                }
                result
            } else {
                let result = self.dialect.require_action_batch(parsed, request);
                if let Some(observation) = observation.as_ref() {
                    observation.observe(request, 1, None, &result).await;
                }
                result
            }
        })
    }
}
