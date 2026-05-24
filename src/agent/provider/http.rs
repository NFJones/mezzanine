//! Provider HTTP transport boundary.
//!
//! This module owns request/response transport types, reqwest-backed I/O,
//! response-size bounds, and transport-specific tests. Provider-specific
//! request construction and response parsing remain in the parent module.

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::error::{MezError, Result};

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
