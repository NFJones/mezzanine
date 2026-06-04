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

/// Tracks incremental terminal-event detection for one buffered SSE body.
#[derive(Debug, Default)]
struct ProviderSseTerminalDetector {
    scanned_bytes: usize,
    terminal_seen: bool,
}

impl ProviderSseTerminalDetector {
    /// Reports whether buffered SSE text already contains a terminal provider
    /// event while scanning each completed event block at most once.
    fn has_terminal_event(&mut self, body: &[u8]) -> bool {
        if self.terminal_seen {
            return true;
        }
        if self.scanned_bytes > body.len() {
            self.scanned_bytes = 0;
        }
        while self.scanned_bytes < body.len() {
            let remaining = &body[self.scanned_bytes..];
            let Some((separator_index, separator_len)) =
                provider_http_find_sse_block_separator(remaining)
            else {
                break;
            };
            let block_end = self.scanned_bytes + separator_index;
            let Ok(block) = std::str::from_utf8(&body[self.scanned_bytes..block_end]) else {
                self.scanned_bytes = block_end + separator_len;
                continue;
            };
            self.scanned_bytes = block_end + separator_len;
            if provider_sse_block_is_terminal(block) {
                self.terminal_seen = true;
                return true;
            }
        }
        false
    }
}

/// Reports whether buffered SSE text already contains a terminal provider event.
fn provider_http_body_has_terminal_sse_event(body: &[u8]) -> bool {
    ProviderSseTerminalDetector::default().has_terminal_event(body)
}

/// Locates the next complete SSE block separator without allocating a
/// normalized copy of the buffered provider body.
fn provider_http_find_sse_block_separator(body: &[u8]) -> Option<(usize, usize)> {
    let bytes = body;
    let mut index = 0;
    while index + 1 < bytes.len() {
        match bytes[index] {
            b'\n' if bytes[index + 1] == b'\n' => return Some((index, 2)),
            b'\r'
                if index + 3 < bytes.len()
                    && bytes[index + 1] == b'\n'
                    && bytes[index + 2] == b'\r'
                    && bytes[index + 3] == b'\n' =>
            {
                return Some((index, 4));
            }
            _ => index += 1,
        }
    }
    None
}

/// Reports whether one complete SSE event block is terminal.
fn provider_sse_block_is_terminal(block: &str) -> bool {
    let mut event_name = None;
    let mut data_start = None;
    let mut data_end = None;
    let mut data_line_count = 0usize;
    for line in block.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event_name = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("data:") {
            let data = value.trim_start();
            let offset = data.as_ptr() as usize - block.as_ptr() as usize;
            data_start.get_or_insert(offset);
            data_end = Some(offset.saturating_add(data.len()));
            data_line_count += 1;
        }
    }
    let (Some(data_start), Some(data_end)) = (data_start, data_end) else {
        return false;
    };
    let data = block[data_start..data_end].trim();
    if data_line_count == 1 && data == "[DONE]" {
        return true;
    }
    if data_line_count > 1 && provider_sse_data_lines_equal(block, "[DONE]") {
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

/// Reports whether the logical joined SSE data field equals the target
/// without allocating the joined field contents.
fn provider_sse_data_lines_equal(block: &str, target: &str) -> bool {
    let mut target_remaining = target;
    let mut first = true;
    for line in block.lines() {
        let Some(value) = line.strip_prefix("data:") else {
            continue;
        };
        let data = value.trim_start();
        if !first {
            let Some(remaining) = target_remaining.strip_prefix('\n') else {
                return false;
            };
            target_remaining = remaining;
        }
        let Some(remaining) = target_remaining.strip_prefix(data) else {
            return false;
        };
        target_remaining = remaining;
        first = false;
    }
    !first && target_remaining.is_empty()
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

/// Reads one provider response chunk with an explicit inactivity deadline.
///
/// Reqwest's client-level read timeout is advisory at the transport boundary,
/// while this wrapper gives Mezzanine one deterministic stall classification for
/// every streamed body read. The deadline is restarted for each successful
/// chunk, so long responses can continue as long as bytes keep arriving.
async fn provider_http_read_chunk_with_timeout(
    response: &mut reqwest::Response,
    timeout_ms: u64,
) -> Result<std::result::Result<Option<Vec<u8>>, reqwest::Error>> {
    let timeout = Duration::from_millis(timeout_ms.max(1));
    let chunk = tokio::time::timeout(timeout, response.chunk())
        .await
        .map_err(|_| provider_http_response_stalled_error(timeout_ms))?;
    Ok(chunk.map(|chunk| chunk.map(|chunk| chunk.to_vec())))
}

/// Builds the deterministic error used when provider response progress stalls.
fn provider_http_response_stalled_error(timeout_ms: u64) -> MezError {
    MezError::invalid_state(format!(
        "provider HTTP response read stalled for {timeout_ms}ms while waiting for body chunk"
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
                let chunk =
                    match provider_http_read_chunk_with_timeout(&mut response, request.timeout_ms)
                        .await?
                    {
                        Ok(Some(chunk)) => chunk,
                        Ok(None) => break,
                        Err(error) => {
                            if expects_event_stream
                                && provider_http_body_has_terminal_sse_event(&body)
                            {
                                break;
                            }
                            if error.is_timeout() {
                                return Err(provider_http_response_stalled_error(
                                    request.timeout_ms,
                                ));
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
            timeout_ms: 50,
            max_response_bytes: None,
        };

        let error = ReqwestProviderHttpTransport
            .send_async(&request)
            .await
            .unwrap_err();
        server.abort();

        assert!(
            error
                .to_string()
                .contains("provider HTTP response read stalled")
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

    /// Verifies terminal SSE detection keeps incremental progress across
    /// provider chunks.
    ///
    /// Large agent responses can contain many small SSE delta events before a
    /// terminal response event. The transport detector must not revisit already
    /// completed event blocks after each chunk, because that makes long streams
    /// quadratic and duplicates JSON parsing/allocation work.
    #[test]
    fn provider_transport_terminal_sse_detector_scans_completed_blocks_once() {
        let mut detector = ProviderSseTerminalDetector::default();
        let mut body = b"event: response.output_text.delta\n\
            data: {\"type\":\"response.output_text.delta\",\"delta\":\"a\"}\n\n"
            .to_vec();

        assert!(!detector.has_terminal_event(&body));
        let scanned_after_first_event = detector.scanned_bytes;
        assert_eq!(scanned_after_first_event, body.len());

        body.extend_from_slice(b"event: response.completed\n");
        assert!(!detector.has_terminal_event(&body));
        assert_eq!(detector.scanned_bytes, scanned_after_first_event);

        body.extend_from_slice(b"data: {\"type\":\"response.completed\"}\n\n");
        assert!(detector.has_terminal_event(&body));
        assert_eq!(detector.scanned_bytes, body.len());
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

        assert!(provider_http_body_has_terminal_sse_event(body.as_bytes()));
    }
}
