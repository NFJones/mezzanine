//! Provider HTTP transport boundary.
//!
//! This module owns reqwest-backed I/O and transport-specific tests.
//! Provider-neutral request/response values and response bounds live in
//! `mez-agent`; provider-specific construction and parsing remain in the
//! parent module.

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use mez_agent::{
    DEFAULT_PROVIDER_MAX_RESPONSE_BYTES, IncrementalSseDecoder, ProviderHttpError,
    ProviderHttpRequest, ProviderHttpResponse, ProviderHttpResult, ProviderHttpTimeoutPhase,
    ProviderHttpTimeouts, ProviderSseTerminalDetector, SseEvent, SseParseError, parse_sse_events,
    provider_sse_event_is_terminal,
};

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
    fn send(&self, request: &ProviderHttpRequest) -> ProviderHttpResult<ProviderHttpResponse>;
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
    ) -> Pin<Box<dyn Future<Output = ProviderHttpResult<ProviderHttpResponse>> + Send + 'a>>;

    /// Sends one request and reports parsed SSE events as they become available.
    ///
    /// Compatibility transports may retain a complete body and use this default
    /// implementation. Streaming transports override it so callers can update
    /// provider-specific decoder state before the terminal event arrives.
    fn send_async_with_sse_events<'a>(
        &'a self,
        request: &'a ProviderHttpRequest,
        on_event: &'a mut (dyn FnMut(SseEvent) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send),
    ) -> Pin<Box<dyn Future<Output = ProviderHttpResult<ProviderHttpResponse>> + Send + 'a>> {
        Box::pin(async move {
            let response = self.send_async(request).await?;
            if provider_http_expects_event_stream(&request.headers, &response.headers) {
                let events = parse_sse_events(
                    &response.body,
                    "provider stream response did not contain SSE data events",
                )
                .map_err(|error| ProviderHttpError::invalid_state(error.message()))?;
                for event in events {
                    on_event(event).await;
                }
            }
            Ok(response)
        })
    }
}

/// Carries Reqwest Provider Http Transport state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReqwestProviderHttpTransport;

/// Maximum distinct reqwest connection policies retained process-wide.
const MAX_PROVIDER_HTTP_CLIENT_POLICIES: usize = 16;

/// Maximum idle provider connections retained for one origin.
const MAX_PROVIDER_HTTP_IDLE_CONNECTIONS_PER_HOST: usize = 4;

/// Process-wide provider client cache shared by zero-sized transport adapters.
static PROVIDER_HTTP_CLIENTS: OnceLock<Mutex<ProviderHttpClientCache>> = OnceLock::new();

/// Connection policy that affects reqwest client construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ProviderHttpClientPolicy {
    plaintext_http: bool,
    connect_timeout_ms: u64,
}

/// Bounded reusable reqwest clients keyed by construction policy.
#[derive(Debug, Default)]
struct ProviderHttpClientCache {
    clients: BTreeMap<ProviderHttpClientPolicy, reqwest::Client>,
}

/// Builds the reqwest client used for provider calls.
///
/// Provider responses are expected to be UTF-8 JSON or event-stream text.
/// Compression adds an extra body-decoding failure path before Mezzanine can
/// inspect provider diagnostics, so this transport explicitly avoids automatic
/// decompression. Plain HTTP does not need certificate roots, so disabling
/// their discovery for that scheme keeps loopback and private plaintext
/// providers usable on hosts without a CA bundle. HTTPS retains reqwest's
/// verified platform-root behavior. Mezzanine owns explicit first-byte,
/// inter-chunk, and total deadlines around reqwest operations so timeout
/// failures retain a stable phase classification.
fn provider_http_client_builder(
    timeouts: ProviderHttpTimeouts,
    scheme: &str,
) -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_millis(timeouts.connect_timeout_ms))
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(MAX_PROVIDER_HTTP_IDLE_CONNECTIONS_PER_HOST)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd();
    if scheme == "http" {
        builder.tls_certs_only(std::iter::empty::<reqwest::Certificate>())
    } else {
        builder
    }
}

/// Returns one reusable reqwest client for the request connection policy.
fn provider_http_client(
    timeouts: ProviderHttpTimeouts,
    scheme: &str,
) -> ProviderHttpResult<reqwest::Client> {
    let policy = ProviderHttpClientPolicy {
        plaintext_http: scheme == "http",
        connect_timeout_ms: timeouts.connect_timeout_ms,
    };
    let cache =
        PROVIDER_HTTP_CLIENTS.get_or_init(|| Mutex::new(ProviderHttpClientCache::default()));
    if let Some(client) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clients
        .get(&policy)
        .cloned()
    {
        return Ok(client);
    }

    let client = provider_http_client_builder(timeouts, scheme)
        .build()
        .map_err(|error| {
            ProviderHttpError::invalid_state(format!("provider HTTP client setup failed: {error}"))
        })?;
    let mut cache = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = cache.clients.get(&policy) {
        return Ok(existing.clone());
    }
    if cache.clients.len() < MAX_PROVIDER_HTTP_CLIENT_POLICIES {
        cache.clients.insert(policy, client.clone());
    }
    Ok(client)
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

/// Formats a reqwest response-body read error with useful transport details.
fn provider_http_response_read_error(
    status_code: u16,
    content_encoding: &str,
    error: reqwest::Error,
) -> ProviderHttpError {
    let source_chain = provider_http_error_source_chain(&error);
    ProviderHttpError::invalid_state(format!(
        "provider HTTP response read failed (status {status_code}, \
         content-encoding {content_encoding}, timeout {}, decode {}, source {source_chain}): \
         {error}",
        error.is_timeout(),
        error.is_decode(),
    ))
}

/// Returns the earlier of one phase deadline and the whole-request deadline.
fn provider_http_bounded_deadline(
    phase_deadline: tokio::time::Instant,
    total_deadline: tokio::time::Instant,
) -> (tokio::time::Instant, bool) {
    if total_deadline <= phase_deadline {
        (total_deadline, true)
    } else {
        (phase_deadline, false)
    }
}

/// Builds a typed phase or total timeout after one bounded wait expires.
fn provider_http_deadline_error(
    phase: ProviderHttpTimeoutPhase,
    phase_timeout_ms: u64,
    total_timeout_ms: u64,
    total_limited: bool,
    operation: &str,
) -> ProviderHttpError {
    if total_limited {
        ProviderHttpError::timeout(ProviderHttpTimeoutPhase::Total, total_timeout_ms, operation)
    } else {
        ProviderHttpError::timeout(phase, phase_timeout_ms, operation)
    }
}

/// Classifies reqwest send failures without losing connect timeout identity.
fn provider_http_request_error(
    error: reqwest::Error,
    timeouts: ProviderHttpTimeouts,
) -> ProviderHttpError {
    if error.is_timeout() && error.is_connect() {
        return ProviderHttpError::timeout(
            ProviderHttpTimeoutPhase::Connect,
            timeouts.connect_timeout_ms,
            "establishing the provider connection",
        );
    }
    if error.is_timeout() {
        return ProviderHttpError::timeout(
            ProviderHttpTimeoutPhase::FirstByte,
            timeouts.first_byte_timeout_ms,
            "sending the request and waiting for response headers",
        );
    }
    ProviderHttpError::invalid_state(format!("provider HTTP request failed: {error}"))
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
    ) -> Pin<Box<dyn Future<Output = ProviderHttpResult<ProviderHttpResponse>> + Send + 'a>> {
        Box::pin(async move {
            let mut event_body = String::new();
            let mut response = {
                let mut collect_event = |event: SseEvent| {
                    if let Some(name) = event.name {
                        event_body.push_str("event: ");
                        event_body.push_str(&name);
                        event_body.push('\n');
                    }
                    for line in event.data.lines() {
                        event_body.push_str("data: ");
                        event_body.push_str(line);
                        event_body.push('\n');
                    }
                    event_body.push('\n');
                    Box::pin(async {}) as Pin<Box<dyn Future<Output = ()> + Send>>
                };
                self.send_async_with_sse_events(request, &mut collect_event)
                    .await?
            };
            if response.body.is_empty() && !event_body.is_empty() {
                response.body = event_body;
            }
            Ok(response)
        })
    }

    /// Streams complete SSE events to the provider decoder during body reads.
    fn send_async_with_sse_events<'a>(
        &'a self,
        request: &'a ProviderHttpRequest,
        on_event: &'a mut (dyn FnMut(SseEvent) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send),
    ) -> Pin<Box<dyn Future<Output = ProviderHttpResult<ProviderHttpResponse>> + Send + 'a>> {
        Box::pin(async move {
            request.timeouts.validate()?;
            let started_at = tokio::time::Instant::now();
            let first_byte_deadline =
                started_at + Duration::from_millis(request.timeouts.first_byte_timeout_ms);
            let total_deadline =
                started_at + Duration::from_millis(request.timeouts.total_timeout_ms);
            let method = request.method.parse::<reqwest::Method>().map_err(|_| {
                ProviderHttpError::invalid_args(format!(
                    "unsupported provider HTTP method {}",
                    request.method
                ))
            })?;
            let url = request
                .url
                .parse::<reqwest::Url>()
                .map_err(|_| ProviderHttpError::invalid_args("provider HTTP URL is invalid"))?;
            let mut headers = reqwest::header::HeaderMap::new();
            for (name, value) in &request.headers {
                let name =
                    reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                        ProviderHttpError::invalid_args("provider HTTP header name is invalid")
                    })?;
                let value = reqwest::header::HeaderValue::from_str(value).map_err(|_| {
                    ProviderHttpError::invalid_args("provider HTTP header value is invalid")
                })?;
                headers.insert(name, value);
            }
            apply_provider_transport_default_headers(&mut headers);

            let client = provider_http_client(request.timeouts, url.scheme())?;
            let (send_deadline, total_limited) =
                provider_http_bounded_deadline(first_byte_deadline, total_deadline);
            let mut response = tokio::time::timeout_at(
                send_deadline,
                client
                    .request(method, url)
                    .headers(headers)
                    .body(request.body.clone())
                    .send(),
            )
            .await
            .map_err(|_| {
                provider_http_deadline_error(
                    ProviderHttpTimeoutPhase::FirstByte,
                    request.timeouts.first_byte_timeout_ms,
                    request.timeouts.total_timeout_ms,
                    total_limited,
                    "sending the request and waiting for response headers",
                )
            })?
            .map_err(|error| provider_http_request_error(error, request.timeouts))?;
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
            let retain_body = !expects_event_stream || !(200..300).contains(&status_code);
            let mut response_bytes = 0usize;
            let mut terminal_detector = ProviderSseTerminalDetector::default();
            let mut event_decoder = expects_event_stream.then(IncrementalSseDecoder::default);
            let mut terminal_event_seen = false;
            let mut progress_phase = ProviderHttpTimeoutPhase::FirstByte;
            let mut progress_timeout_ms = request.timeouts.first_byte_timeout_ms;
            let mut progress_deadline = first_byte_deadline;
            loop {
                let (read_deadline, total_limited) =
                    provider_http_bounded_deadline(progress_deadline, total_deadline);
                let chunk = match tokio::time::timeout_at(read_deadline, response.chunk())
                    .await
                    .map_err(|_| {
                        provider_http_deadline_error(
                            progress_phase,
                            progress_timeout_ms,
                            request.timeouts.total_timeout_ms,
                            total_limited,
                            "waiting for provider response body progress",
                        )
                    })? {
                    Ok(Some(chunk)) => chunk,
                    Ok(None) => break,
                    Err(error) => {
                        if expects_event_stream && terminal_detector.has_terminal_event(&body) {
                            break;
                        }
                        if error.is_timeout() {
                            return Err(ProviderHttpError::timeout(
                                progress_phase,
                                progress_timeout_ms,
                                "waiting for provider response body progress",
                            ));
                        }
                        return Err(provider_http_response_read_error(
                            status_code,
                            content_encoding,
                            error,
                        ));
                    }
                };
                if chunk.is_empty() {
                    continue;
                }
                if response_bytes.saturating_add(chunk.len()) > response_limit {
                    if request.max_response_bytes.is_none() {
                        return Err(ProviderHttpError::invalid_state(
                            "provider HTTP response exceeds configured limit",
                        ));
                    }
                    let remaining = response_limit.saturating_sub(response_bytes);
                    if retain_body && remaining > 0 {
                        body.extend_from_slice(&chunk[..remaining]);
                    }
                    body_truncated = true;
                    break;
                }
                response_bytes = response_bytes.saturating_add(chunk.len());
                if retain_body {
                    body.extend_from_slice(&chunk);
                }
                if let Some(decoder) = event_decoder.as_mut() {
                    let mut decoded_events = Vec::new();
                    decoder
                        .push::<SseParseError, _>(&chunk, |event| {
                            terminal_event_seen =
                                terminal_event_seen || provider_sse_event_is_terminal(&event);
                            decoded_events.push(event);
                            Ok(())
                        })
                        .map_err(|error| ProviderHttpError::invalid_state(error.message()))?;
                    for event in decoded_events {
                        on_event(event).await;
                    }
                }
                if expects_event_stream
                    && (terminal_event_seen
                        || (retain_body && terminal_detector.has_terminal_event(&body)))
                {
                    break;
                }
                progress_phase = ProviderHttpTimeoutPhase::InterChunk;
                progress_timeout_ms = request.timeouts.inter_chunk_timeout_ms;
                progress_deadline = tokio::time::Instant::now()
                    + Duration::from_millis(request.timeouts.inter_chunk_timeout_ms);
            }
            if let Some(decoder) = event_decoder.as_mut() {
                let mut decoded_events = Vec::new();
                decoder
                    .finish::<SseParseError, _>(
                        "provider stream response did not contain SSE data events",
                        |event| {
                            decoded_events.push(event);
                            Ok(())
                        },
                    )
                    .map_err(|error| ProviderHttpError::invalid_state(error.message()))?;
                for event in decoded_events {
                    on_event(event).await;
                }
            }
            if body_truncated {
                response_headers.insert("x-mez-body-truncated".to_string(), "true".to_string());
            }
            if expects_event_stream && !retain_body {
                response_headers.insert(
                    "x-mez-stream-decoded".to_string(),
                    "incremental".to_string(),
                );
            }
            let body = if body_truncated && request.max_response_bytes.is_some() {
                String::from_utf8_lossy(&body).into_owned()
            } else {
                String::from_utf8(body).map_err(|_| {
                    ProviderHttpError::invalid_state("provider HTTP response body is not UTF-8")
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
    use super::{
        AsyncProviderHttpTransport, ProviderHttpRequest, ProviderHttpTimeouts,
        ProviderSseTerminalDetector, ReqwestProviderHttpTransport,
        apply_provider_transport_default_headers,
    };
    use std::collections::BTreeMap;
    use std::time::Duration;

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

    /// Verifies sequential provider requests reuse one HTTP/1.1 connection.
    ///
    /// The server accepts exactly one TCP stream and serves two complete
    /// requests on it. A transport that constructs a fresh reqwest client for
    /// the second request cannot complete because no second accept is driven.
    #[tokio::test]
    async fn provider_transport_reuses_keep_alive_connections() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            for _ in 0..2 {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let read = stream.read(&mut buffer).await.unwrap();
                    assert!(read > 0, "provider connection closed before both requests");
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok",
                    )
                    .await
                    .unwrap();
                stream.flush().await.unwrap();
            }
        });
        let request = ProviderHttpRequest {
            method: "GET".to_string(),
            url: format!("http://{address}/reuse"),
            headers: BTreeMap::new(),
            body: String::new(),
            timeouts: ProviderHttpTimeouts::from_total(1_000),
            max_response_bytes: None,
        };

        let first = ReqwestProviderHttpTransport
            .send_async(&request)
            .await
            .unwrap();
        let second = ReqwestProviderHttpTransport
            .send_async(&request)
            .await
            .unwrap();
        server.await.unwrap();

        assert_eq!(first.body, "ok");
        assert_eq!(second.body, "ok");
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
            timeouts: ProviderHttpTimeouts::from_total(1_000),
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

    /// Verifies observer-based provider streaming exposes visible deltas before
    /// terminal completion and does not retain the complete successful SSE body.
    ///
    /// The server gates its terminal event until the test observes the first
    /// decoded delta. This makes a full-body implementation deadlock or time out,
    /// while the incremental path returns an empty compatibility body after the
    /// final event because provider-specific state already owns the response.
    #[tokio::test]
    async fn provider_transport_streams_events_before_terminal_without_retaining_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n",
                )
                .await
                .unwrap();
            let delta = concat!(
                "event: response.output_text.delta\n",
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"early\"}\n\n"
            );
            stream
                .write_all(format!("{:x}\r\n{}\r\n", delta.len(), delta).as_bytes())
                .await
                .unwrap();
            stream.flush().await.unwrap();
            release_rx.await.unwrap();
            let completed = concat!(
                "event: response.completed\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"model\":\"gpt-test\"}}\n\n"
            );
            stream
                .write_all(format!("{:x}\r\n{}\r\n", completed.len(), completed).as_bytes())
                .await
                .unwrap();
            stream.flush().await.unwrap();
        });
        let request = ProviderHttpRequest {
            method: "POST".to_string(),
            url: format!("http://{address}/responses"),
            headers: BTreeMap::from([("Accept".to_string(), "text/event-stream".to_string())]),
            body: "{}".to_string(),
            timeouts: ProviderHttpTimeouts::from_total(1_000),
            max_response_bytes: None,
        };
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel(4);
        let client = tokio::spawn(async move {
            let mut on_event = move |event| {
                let _ = progress_tx.try_send(event);
                Box::pin(async {}) as std::pin::Pin<Box<dyn Future<Output = ()> + Send>>
            };
            ReqwestProviderHttpTransport
                .send_async_with_sse_events(&request, &mut on_event)
                .await
        });

        let progress = tokio::time::timeout(Duration::from_millis(250), progress_rx.recv())
            .await
            .expect("first provider delta should arrive before terminal completion")
            .expect("provider progress channel should remain open");
        assert_eq!(progress.name.as_deref(), Some("response.output_text.delta"));
        assert!(progress.data.contains("early"));
        assert!(!client.is_finished());
        release_tx.send(()).unwrap();

        let response = client.await.unwrap().unwrap();
        server.await.unwrap();
        assert_eq!(response.status_code, 200);
        assert!(response.body.is_empty());
        assert_eq!(
            response
                .headers
                .get("x-mez-stream-decoded")
                .map(String::as_str),
            Some("incremental")
        );
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
            timeouts: ProviderHttpTimeouts::from_total(1_000),
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

    /// Verifies provider body reads fail with a Mezzanine timeout when no body
    /// chunk arrives inside the per-read inactivity window.
    ///
    /// Some provider or proxy failures can send headers and then leave the body
    /// stream open forever. The transport must classify that condition itself
    /// instead of relying only on the lower-level HTTP client's read timeout.
    #[tokio::test]
    async fn provider_transport_times_out_stalled_body_reads() {
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
            let response = "HTTP/1.1 200 OK\r\n\
                            Content-Type: text/plain; charset=utf-8\r\n\
                            Content-Length: 5\r\n\
                            Connection: keep-alive\r\n\
                            \r\n";
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let request = ProviderHttpRequest {
            method: "GET".to_string(),
            url: format!("http://{address}/stall.txt"),
            headers: BTreeMap::new(),
            body: String::new(),
            timeouts: ProviderHttpTimeouts {
                connect_timeout_ms: 50,
                first_byte_timeout_ms: 50,
                inter_chunk_timeout_ms: 50,
                total_timeout_ms: 200,
            },
            max_response_bytes: None,
        };

        let error = ReqwestProviderHttpTransport
            .send_async(&request)
            .await
            .unwrap_err();
        server.abort();

        assert_eq!(
            error.kind(),
            mez_agent::ProviderHttpErrorKind::Timeout(
                mez_agent::ProviderHttpTimeoutPhase::FirstByte
            )
        );
    }

    /// Verifies a response that emits one body chunk and then stalls is
    /// classified as an inter-chunk timeout rather than a first-byte timeout.
    #[tokio::test]
    async fn provider_transport_classifies_inter_chunk_stalls() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                request.extend_from_slice(&buffer[..read]);
                if read == 0 || request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\na",
                )
                .await
                .unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let request = ProviderHttpRequest {
            method: "GET".to_string(),
            url: format!("http://{address}/inter-chunk"),
            headers: BTreeMap::new(),
            body: String::new(),
            timeouts: ProviderHttpTimeouts {
                connect_timeout_ms: 100,
                first_byte_timeout_ms: 200,
                inter_chunk_timeout_ms: 40,
                total_timeout_ms: 500,
            },
            max_response_bytes: None,
        };

        let error = ReqwestProviderHttpTransport
            .send_async(&request)
            .await
            .unwrap_err();
        server.abort();

        assert_eq!(
            error.kind(),
            mez_agent::ProviderHttpErrorKind::Timeout(
                mez_agent::ProviderHttpTimeoutPhase::InterChunk
            )
        );
    }

    /// Verifies frequent body progress cannot extend the monotonic total
    /// deadline for a slow-drip response.
    #[tokio::test]
    async fn provider_transport_total_deadline_bounds_slow_drip_responses() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                request.extend_from_slice(&buffer[..read]);
                if read == 0 || request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: keep-alive\r\n\r\n",
                )
                .await
                .unwrap();
            for byte in b"abcdefgh" {
                if stream.write_all(std::slice::from_ref(byte)).await.is_err() {
                    break;
                }
                let _ = stream.flush().await;
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        });
        let request = ProviderHttpRequest {
            method: "GET".to_string(),
            url: format!("http://{address}/slow-drip"),
            headers: BTreeMap::new(),
            body: String::new(),
            timeouts: ProviderHttpTimeouts {
                connect_timeout_ms: 50,
                first_byte_timeout_ms: 80,
                inter_chunk_timeout_ms: 60,
                total_timeout_ms: 110,
            },
            max_response_bytes: None,
        };

        let error = ReqwestProviderHttpTransport
            .send_async(&request)
            .await
            .unwrap_err();
        server.abort();

        assert_eq!(
            error.kind(),
            mez_agent::ProviderHttpErrorKind::Timeout(mez_agent::ProviderHttpTimeoutPhase::Total)
        );
    }

    /// Verifies a multi-chunk response completes when each phase and the total
    /// exchange stay within their configured budgets.
    #[tokio::test]
    async fn provider_transport_accepts_healthy_multi_chunk_progress() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                request.extend_from_slice(&buffer[..read]);
                if read == 0 || request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nab")
                .await
                .unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
            stream.write_all(b"cd").await.unwrap();
        });
        let request = ProviderHttpRequest {
            method: "GET".to_string(),
            url: format!("http://{address}/healthy"),
            headers: BTreeMap::new(),
            body: String::new(),
            timeouts: ProviderHttpTimeouts {
                connect_timeout_ms: 100,
                first_byte_timeout_ms: 200,
                inter_chunk_timeout_ms: 100,
                total_timeout_ms: 500,
            },
            max_response_bytes: None,
        };

        let response = ReqwestProviderHttpTransport
            .send_async(&request)
            .await
            .unwrap();
        server.abort();

        assert_eq!(response.body, "abcd");
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
        let mut detector = ProviderSseTerminalDetector::default();

        assert!(detector.has_terminal_event(body.as_bytes()));
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

        let mut detector = ProviderSseTerminalDetector::default();
        assert!(!detector.has_terminal_event(body.as_bytes()));

        let mut detector = ProviderSseTerminalDetector::default();
        assert!(!detector.has_terminal_event(delimited_but_invalid.as_bytes()));
    }

    /// Verifies terminal SSE detection keeps incremental progress across
    /// provider chunks.
    ///
    /// Large agent responses can contain many small SSE delta events before a
    /// terminal response event. The transport detector must not revisit already
    /// completed event blocks after each chunk, because that makes long streams
    /// quadratic and duplicates JSON parsing/allocation work.
    #[test]
    fn provider_transport_terminal_sse_detector_accumulates_completed_blocks() {
        let mut detector = ProviderSseTerminalDetector::default();
        let mut body = b"event: response.output_text.delta\n\
            data: {\"type\":\"response.output_text.delta\",\"delta\":\"a\"}\n\n"
            .to_vec();

        assert!(!detector.has_terminal_event(&body));

        body.extend_from_slice(b"event: response.completed\n");
        assert!(!detector.has_terminal_event(&body));

        body.extend_from_slice(b"data: {\"type\":\"response.completed\"}\n\n");
        assert!(detector.has_terminal_event(&body));
    }

    /// Verifies terminal SSE detection accepts CRLF-delimited event blocks
    /// without allocating a newline-normalized copy of the body.
    ///
    /// Some providers emit spec-compliant CRLF separators. The transport must
    /// still detect terminal events while scanning the buffered response in
    /// place so per-chunk SSE detection stays allocation-free.
    #[test]
    fn provider_transport_detects_terminal_failure_sse_events_with_crlf_blocks() {
        let body = format!(
            "event: response.failed\r\ndata: {}\r\n\r\n",
            serde_json::json!({
                "type": "response.failed",
                "response": {"error": {"message": "bad token"}}
            })
        );
        let mut detector = ProviderSseTerminalDetector::default();

        assert!(detector.has_terminal_event(body.as_bytes()));
    }
}
