//! Provider-neutral HTTP request and response contracts.
//!
//! This module owns the data exchanged between provider request builders and
//! product-owned HTTP transports. Concrete clients, credentials, retries, and
//! transport error conversion remain in the Mezzanine composition crate.

use std::collections::BTreeMap;

/// Stable failure categories exposed by provider HTTP transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderHttpErrorKind {
    /// The assembled request cannot be represented by the concrete transport.
    InvalidArgs,
    /// The transport could not complete or decode the provider exchange.
    InvalidState,
    /// The transport exceeded one explicit request or response phase deadline.
    Timeout(ProviderHttpTimeoutPhase),
}

/// Stable provider HTTP phases that can expire independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderHttpTimeoutPhase {
    /// TCP/TLS connection establishment.
    Connect,
    /// Request send and first response-body byte.
    FirstByte,
    /// Progress between non-empty response-body chunks.
    InterChunk,
    /// Complete request and response exchange.
    Total,
}

impl ProviderHttpTimeoutPhase {
    /// Returns the stable diagnostic name for this timeout phase.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::FirstByte => "first_byte",
            Self::InterChunk => "inter_chunk",
            Self::Total => "total",
        }
    }
}

/// Provider-neutral failure returned by an HTTP transport adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderHttpError {
    kind: ProviderHttpErrorKind,
    message: String,
}

impl ProviderHttpError {
    /// Builds a request-validation failure.
    pub fn invalid_args(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderHttpErrorKind::InvalidArgs,
            message: message.into(),
        }
    }

    /// Builds a transport or response-decoding failure.
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderHttpErrorKind::InvalidState,
            message: message.into(),
        }
    }

    /// Builds a typed provider HTTP phase-timeout failure.
    pub fn timeout(phase: ProviderHttpTimeoutPhase, timeout_ms: u64, operation: &str) -> Self {
        Self {
            kind: ProviderHttpErrorKind::Timeout(phase),
            message: format!(
                "provider HTTP timeout phase={} limit_ms={timeout_ms} while {operation}",
                phase.as_str()
            ),
        }
    }

    /// Returns the stable failure category.
    pub fn kind(&self) -> ProviderHttpErrorKind {
        self.kind
    }

    /// Returns the bounded caller-facing diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ProviderHttpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProviderHttpError {}

/// Result returned by provider-neutral HTTP transport boundaries.
pub type ProviderHttpResult<T> = Result<T, ProviderHttpError>;

/// Maximum provider response body retained by the shared transport.
pub const DEFAULT_PROVIDER_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Default per-read stall timeout for long-running provider responses.
pub const DEFAULT_PROVIDER_TIMEOUT_MS: u64 = 30 * 60 * 1000;

/// Default upper bound for provider TCP/TLS connection establishment.
pub const DEFAULT_PROVIDER_CONNECT_TIMEOUT_MS: u64 = 30 * 1000;

/// Default upper bound for request send and the first response-body byte.
pub const DEFAULT_PROVIDER_FIRST_BYTE_TIMEOUT_MS: u64 = 5 * 60 * 1000;

/// Default upper bound between non-empty provider response chunks.
pub const DEFAULT_PROVIDER_INTER_CHUNK_TIMEOUT_MS: u64 = 2 * 60 * 1000;

/// Explicit timeout policy for one provider HTTP exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderHttpTimeouts {
    /// TCP/TLS connection timeout.
    pub connect_timeout_ms: u64,
    /// Request-send and first-response-byte timeout.
    pub first_byte_timeout_ms: u64,
    /// Maximum inactivity between non-empty body chunks.
    pub inter_chunk_timeout_ms: u64,
    /// Total request and response deadline.
    pub total_timeout_ms: u64,
}

impl ProviderHttpTimeouts {
    /// Derives safe phase deadlines from one configured total timeout.
    pub const fn from_total(total_timeout_ms: u64) -> Self {
        Self {
            connect_timeout_ms: min_u64(total_timeout_ms, DEFAULT_PROVIDER_CONNECT_TIMEOUT_MS),
            first_byte_timeout_ms: min_u64(
                total_timeout_ms,
                DEFAULT_PROVIDER_FIRST_BYTE_TIMEOUT_MS,
            ),
            inter_chunk_timeout_ms: min_u64(
                total_timeout_ms,
                DEFAULT_PROVIDER_INTER_CHUNK_TIMEOUT_MS,
            ),
            total_timeout_ms,
        }
    }

    /// Validates that every phase is finite and bounded by the total deadline.
    pub fn validate(self) -> ProviderHttpResult<()> {
        if self.connect_timeout_ms == 0
            || self.first_byte_timeout_ms == 0
            || self.inter_chunk_timeout_ms == 0
            || self.total_timeout_ms == 0
        {
            return Err(ProviderHttpError::invalid_args(
                "provider HTTP timeout phases must be greater than zero",
            ));
        }
        if self.connect_timeout_ms > self.first_byte_timeout_ms
            || self.first_byte_timeout_ms > self.total_timeout_ms
            || self.inter_chunk_timeout_ms > self.total_timeout_ms
        {
            return Err(ProviderHttpError::invalid_args(
                "provider HTTP timeout phases must fit within the total deadline",
            ));
        }
        Ok(())
    }
}

/// Const-compatible minimum helper for timeout derivation.
const fn min_u64(left: u64, right: u64) -> u64 {
    if left < right { left } else { right }
}

/// Provider-neutral HTTP request assembled by a model-provider adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderHttpRequest {
    /// HTTP method.
    pub method: String,
    /// Absolute provider endpoint.
    pub url: String,
    /// Request headers, including any credentials supplied by the product.
    pub headers: BTreeMap<String, String>,
    /// UTF-8 request body.
    pub body: String,
    /// Connect, first-byte, inter-chunk, and total request deadlines.
    pub timeouts: ProviderHttpTimeouts,
    /// Optional response-body bound requested by the caller.
    pub max_response_bytes: Option<usize>,
}

/// Provider-neutral HTTP response returned by a product-owned transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderHttpResponse {
    /// HTTP status code.
    pub status_code: u16,
    /// Non-secret response headers retained for provider parsing.
    pub headers: BTreeMap<String, String>,
    /// UTF-8 response body, possibly bounded by the request limit.
    pub body: String,
}

/// One parsed server-sent event block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    /// Optional event name from the last `event:` field in the block.
    pub name: Option<String>,
    /// Joined `data:` field payload with embedded newlines between lines.
    pub data: String,
}

/// Syntax-level failure produced while parsing server-sent events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseParseError {
    message: String,
}

impl SseParseError {
    /// Returns the caller-supplied diagnostic for a body without data events.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for SseParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SseParseError {}

/// Parses SSE event blocks and dispatches each data-bearing event to a caller.
///
/// Empty blocks, comments, and blocks without `data:` fields are ignored. LF
/// and CRLF input are accepted, while endpoint-specific completion and JSON
/// policy remain with the caller.
pub fn parse_sse_events_with<E, F>(
    body: &str,
    missing_message: &'static str,
    mut on_event: F,
) -> Result<(), E>
where
    E: From<SseParseError>,
    F: FnMut(Option<&str>, &str) -> Result<(), E>,
{
    let mut saw_event = false;
    let mut name = None;
    let mut data = String::new();
    let mut has_data = false;

    for raw_line in body.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() {
            if has_data {
                saw_event = true;
                on_event(name.as_deref(), data.as_str())?;
            }
            name = None;
            data.clear();
            has_data = false;
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            if has_data {
                data.push('\n');
            }
            data.push_str(value.trim_start());
            has_data = true;
        }
    }

    if has_data {
        saw_event = true;
        on_event(name.as_deref(), data.as_str())?;
    }
    if !saw_event {
        return Err(SseParseError {
            message: missing_message.to_string(),
        }
        .into());
    }
    Ok(())
}

/// Parses all data-bearing SSE event blocks from one complete response body.
pub fn parse_sse_events(
    body: &str,
    missing_message: &'static str,
) -> Result<Vec<SseEvent>, SseParseError> {
    let mut events = Vec::new();
    parse_sse_events_with(body, missing_message, |name, data| {
        events.push(SseEvent {
            name: name.map(str::to_string),
            data: data.to_string(),
        });
        Ok(())
    })?;
    Ok(events)
}

/// Default maximum number of data-bearing events accepted from one provider stream.
pub const DEFAULT_PROVIDER_MAX_SSE_EVENTS: usize = 65_536;

/// Incrementally decodes complete SSE blocks while retaining only the unfinished suffix.
///
/// Chunk boundaries may split UTF-8 scalars, lines, or block separators. Completed
/// events are emitted exactly once and removed from the retained buffer. The event
/// count bound prevents a peer from replacing the response-byte limit with an
/// unbounded stream of tiny events.
#[derive(Debug)]
pub struct IncrementalSseDecoder {
    pending: Vec<u8>,
    event_count: usize,
    max_events: usize,
}

impl Default for IncrementalSseDecoder {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            event_count: 0,
            max_events: DEFAULT_PROVIDER_MAX_SSE_EVENTS,
        }
    }
}

impl IncrementalSseDecoder {
    /// Builds a decoder with an explicit cumulative data-event limit.
    pub fn with_event_limit(max_events: usize) -> Self {
        Self {
            max_events,
            ..Self::default()
        }
    }

    /// Appends one transport chunk and emits every newly completed data event.
    pub fn push<E, F>(&mut self, chunk: &[u8], mut on_event: F) -> Result<(), E>
    where
        E: From<SseParseError>,
        F: FnMut(SseEvent) -> Result<(), E>,
    {
        self.pending.extend_from_slice(chunk);
        let mut consumed = 0usize;
        while consumed < self.pending.len() {
            let Some((separator_index, separator_len)) =
                find_sse_block_separator(&self.pending[consumed..])
            else {
                break;
            };
            let block_end = consumed.saturating_add(separator_index);
            let block = std::str::from_utf8(&self.pending[consumed..block_end]).map_err(|_| {
                SseParseError {
                    message: "provider SSE event is not valid UTF-8".to_string(),
                }
            })?;
            if let Some(event) = parse_sse_event_block(block) {
                self.emit_event(event, &mut on_event)?;
            }
            consumed = block_end.saturating_add(separator_len);
        }
        if consumed > 0 {
            self.pending.drain(..consumed);
        }
        Ok(())
    }

    /// Flushes one final unterminated event after the transport reaches EOF.
    pub fn finish<E, F>(&mut self, missing_message: &'static str, mut on_event: F) -> Result<(), E>
    where
        E: From<SseParseError>,
        F: FnMut(SseEvent) -> Result<(), E>,
    {
        if !self.pending.is_empty() {
            let block = std::str::from_utf8(&self.pending).map_err(|_| SseParseError {
                message: "provider SSE event is not valid UTF-8".to_string(),
            })?;
            if let Some(event) = parse_sse_event_block(block) {
                self.event_count = self.event_count.saturating_add(1);
                if self.event_count > self.max_events {
                    return Err(SseParseError {
                        message: "provider SSE event count exceeds configured limit".to_string(),
                    }
                    .into());
                }
                on_event(event)?;
            }
            self.pending.clear();
        }
        if self.event_count == 0 {
            return Err(SseParseError {
                message: missing_message.to_string(),
            }
            .into());
        }
        Ok(())
    }

    /// Returns bytes retained solely because their event block is incomplete.
    pub fn buffered_bytes(&self) -> usize {
        self.pending.len()
    }

    /// Returns the cumulative number of emitted data-bearing events.
    pub fn event_count(&self) -> usize {
        self.event_count
    }

    fn emit_event<E, F>(&mut self, event: SseEvent, on_event: &mut F) -> Result<(), E>
    where
        E: From<SseParseError>,
        F: FnMut(SseEvent) -> Result<(), E>,
    {
        self.event_count = self.event_count.saturating_add(1);
        if self.event_count > self.max_events {
            return Err(SseParseError {
                message: "provider SSE event count exceeds configured limit".to_string(),
            }
            .into());
        }
        on_event(event)
    }
}

/// Reports whether one parsed provider SSE event terminates the response stream.
pub fn provider_sse_event_is_terminal(event: &SseEvent) -> bool {
    let data = event.data.trim();
    if data == "[DONE]" {
        return true;
    }
    let event_name_is_terminal = matches!(
        event.name.as_deref(),
        Some("response.completed" | "response.failed" | "response.incomplete" | "message_stop")
    );
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return false;
    };
    event_name_is_terminal
        || matches!(
            value.get("type").and_then(serde_json::Value::as_str),
            Some("response.completed" | "response.failed" | "response.incomplete" | "message_stop")
        )
}

/// Parses one already-delimited SSE block into an owned data event.
fn parse_sse_event_block(block: &str) -> Option<SseEvent> {
    let mut name = None;
    let mut data = String::new();
    let mut has_data = false;
    for raw_line in block.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            if has_data {
                data.push('\n');
            }
            data.push_str(value.trim_start());
            has_data = true;
        }
    }
    has_data.then_some(SseEvent { name, data })
}

/// Incrementally detects terminal events in a buffered provider SSE body.
///
/// The detector scans each completed event block at most once. Product-owned
/// transports can therefore decide whether a later read failure or stalled
/// connection occurred after a complete provider response without coupling
/// this protocol rule to a concrete HTTP client.
#[derive(Debug, Default)]
pub struct ProviderSseTerminalDetector {
    scanned_bytes: usize,
    separator_search_bytes: usize,
    terminal_seen: bool,
    #[cfg(test)]
    searched_bytes: usize,
}

impl ProviderSseTerminalDetector {
    /// Reports whether buffered SSE text contains a complete terminal event.
    pub fn has_terminal_event(&mut self, body: &[u8]) -> bool {
        if self.terminal_seen {
            return true;
        }
        if self.scanned_bytes > body.len() || self.separator_search_bytes > body.len() {
            self.scanned_bytes = 0;
            self.separator_search_bytes = 0;
        }
        while self.scanned_bytes < body.len() {
            let search_start = self.separator_search_bytes.max(self.scanned_bytes);
            let remaining = &body[search_start..];
            #[cfg(test)]
            {
                self.searched_bytes = self.searched_bytes.saturating_add(remaining.len());
            }
            let Some((separator_index, separator_len)) = find_sse_block_separator(remaining) else {
                self.separator_search_bytes = body.len().saturating_sub(3).max(self.scanned_bytes);
                break;
            };
            let block_end = search_start + separator_index;
            let Ok(block) = std::str::from_utf8(&body[self.scanned_bytes..block_end]) else {
                self.scanned_bytes = block_end + separator_len;
                self.separator_search_bytes = self.scanned_bytes;
                continue;
            };
            self.scanned_bytes = block_end + separator_len;
            self.separator_search_bytes = self.scanned_bytes;
            if sse_block_is_terminal(block) {
                self.terminal_seen = true;
                return true;
            }
        }
        false
    }
}

/// Locates the next complete SSE block separator without allocating.
fn find_sse_block_separator(body: &[u8]) -> Option<(usize, usize)> {
    let mut index = 0;
    while index + 1 < body.len() {
        match body[index] {
            b'\n' if body[index + 1] == b'\n' => return Some((index, 2)),
            b'\r'
                if index + 3 < body.len()
                    && body[index + 1] == b'\n'
                    && body[index + 2] == b'\r'
                    && body[index + 3] == b'\n' =>
            {
                return Some((index, 4));
            }
            _ => index += 1,
        }
    }
    None
}

/// Reports whether one complete SSE event block is terminal.
fn sse_block_is_terminal(block: &str) -> bool {
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
    if data_line_count > 1 && sse_data_lines_equal(block, "[DONE]") {
        return true;
    }
    let event_name_is_terminal = matches!(
        event_name,
        Some("response.completed" | "response.failed" | "response.incomplete" | "message_stop")
    );
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return false;
    };
    event_name_is_terminal
        || matches!(
            value.get("type").and_then(serde_json::Value::as_str),
            Some("response.completed" | "response.failed" | "response.incomplete" | "message_stop")
        )
}

/// Reports whether logically joined SSE data lines equal the target.
fn sse_data_lines_equal(block: &str, target: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_PROVIDER_MAX_RESPONSE_BYTES, DEFAULT_PROVIDER_TIMEOUT_MS, IncrementalSseDecoder,
        ProviderHttpError, ProviderHttpErrorKind, ProviderHttpRequest, ProviderHttpResponse,
        ProviderHttpTimeoutPhase, ProviderHttpTimeouts, ProviderSseTerminalDetector, SseParseError,
        parse_sse_events,
    };
    use std::collections::BTreeMap;

    #[test]
    /// Verifies provider HTTP values preserve the complete dependency-neutral
    /// transport envelope and stable response bounds.
    fn provider_http_contracts_preserve_transport_values() {
        let request = ProviderHttpRequest {
            method: "POST".to_string(),
            url: "https://provider.invalid/v1/responses".to_string(),
            headers: BTreeMap::from([("content-type".to_string(), "application/json".to_string())]),
            body: "{}".to_string(),
            timeouts: ProviderHttpTimeouts::from_total(DEFAULT_PROVIDER_TIMEOUT_MS),
            max_response_bytes: Some(DEFAULT_PROVIDER_MAX_RESPONSE_BYTES),
        };
        assert_eq!(request.timeouts.total_timeout_ms, 30 * 60 * 1000);
        assert_eq!(request.timeouts.connect_timeout_ms, 30 * 1000);
        assert_eq!(request.timeouts.first_byte_timeout_ms, 5 * 60 * 1000);
        assert_eq!(request.timeouts.inter_chunk_timeout_ms, 2 * 60 * 1000);
        request.timeouts.validate().unwrap();
        assert_eq!(request.max_response_bytes, Some(16 * 1024 * 1024));

        let response = ProviderHttpResponse {
            status_code: 200,
            headers: BTreeMap::new(),
            body: "ok".to_string(),
        };
        assert_eq!(response.status_code, 200);
        assert_eq!(response.body, "ok");
    }

    #[test]
    /// Verifies provider HTTP failures preserve request-validation and
    /// transport-state categories without depending on product errors.
    fn provider_http_errors_preserve_stable_categories() {
        let invalid_args = ProviderHttpError::invalid_args("bad method");
        let invalid_state = ProviderHttpError::invalid_state("read stalled");
        let timeout = ProviderHttpError::timeout(
            ProviderHttpTimeoutPhase::FirstByte,
            50,
            "waiting for the first response byte",
        );

        assert_eq!(invalid_args.kind(), ProviderHttpErrorKind::InvalidArgs);
        assert_eq!(invalid_args.message(), "bad method");
        assert_eq!(invalid_state.kind(), ProviderHttpErrorKind::InvalidState);
        assert_eq!(invalid_state.message(), "read stalled");
        assert_eq!(
            timeout.kind(),
            ProviderHttpErrorKind::Timeout(ProviderHttpTimeoutPhase::FirstByte)
        );
        assert!(timeout.message().contains("phase=first_byte"));
    }

    /// Verifies syntax-level parsing preserves event names and joins multiple
    /// `data:` fields while accepting CRLF framing and ignoring comments.
    #[test]
    fn parses_named_multiline_sse_events() {
        let events = parse_sse_events(
            ": comment\r\nevent: message\r\ndata: {\"a\":1}\r\ndata: {\"b\":2}\r\n\r\n",
            "missing events",
        )
        .expect("SSE event should parse");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name.as_deref(), Some("message"));
        assert_eq!(events[0].data, "{\"a\":1}\n{\"b\":2}");
    }

    /// Verifies complete-body parsing flushes a final data event even when the
    /// peer closes immediately without a trailing blank-line separator.
    #[test]
    fn parses_unterminated_final_sse_event() {
        let events = parse_sse_events("event: message\ndata: {\"done\":true}", "missing events")
            .expect("SSE event should parse");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name.as_deref(), Some("message"));
        assert_eq!(events[0].data, "{\"done\":true}");
    }

    /// Every possible byte boundary, including inside UTF-8 and CRLF framing,
    /// produces the same events while completed blocks leave the retained
    /// suffix immediately.
    #[test]
    fn incremental_sse_decoder_accepts_single_byte_chunks() {
        let body = "event: delta\r\ndata: hé\r\n\r\nevent: done\ndata: [DONE]\n\n";
        let mut decoder = IncrementalSseDecoder::default();
        let mut events = Vec::new();
        for byte in body.as_bytes() {
            decoder
                .push::<SseParseError, _>(std::slice::from_ref(byte), |event| {
                    events.push(event);
                    Ok(())
                })
                .unwrap();
            if events.len() == 1 {
                assert!(decoder.buffered_bytes() < body.len());
            }
        }
        decoder
            .finish::<SseParseError, _>("missing events", |event| {
                events.push(event);
                Ok(())
            })
            .unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].name.as_deref(), Some("delta"));
        assert_eq!(events[0].data, "hé");
        assert_eq!(events[1].data, "[DONE]");
        assert_eq!(decoder.event_count(), 2);
        assert_eq!(decoder.buffered_bytes(), 0);
    }

    /// Tiny-event floods are rejected independently of the cumulative byte cap.
    #[test]
    fn incremental_sse_decoder_enforces_event_limit() {
        let mut decoder = IncrementalSseDecoder::with_event_limit(1);
        let error = decoder
            .push::<SseParseError, _>(b"data: one\n\ndata: two\n\n", |_| Ok(()))
            .unwrap_err();

        assert!(error.message().contains("event count exceeds"));
    }

    /// Complete terminal failures are recognized so transports can preserve
    /// provider diagnostics when a later body read fails.
    #[test]
    fn provider_sse_detects_terminal_failure_events() {
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

    /// Partial or invalid terminal JSON is not treated as a complete event.
    #[test]
    fn provider_sse_rejects_partial_terminal_json() {
        let body = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"unterminated"
        );
        let delimited_but_invalid = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"unterminated\n\n"
        );

        assert!(!ProviderSseTerminalDetector::default().has_terminal_event(body.as_bytes()));
        assert!(
            !ProviderSseTerminalDetector::default()
                .has_terminal_event(delimited_but_invalid.as_bytes())
        );
    }

    /// Incremental scans retain progress and accept CRLF-delimited blocks.
    #[test]
    fn provider_sse_scans_incrementally_and_accepts_crlf() {
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

        let crlf = b"event: response.failed\r\ndata: {\"type\":\"response.failed\"}\r\n\r\n";
        assert!(ProviderSseTerminalDetector::default().has_terminal_event(crlf));
    }

    /// A large incomplete event delivered one byte at a time advances the
    /// separator search cursor while retaining only enough overlap for a split
    /// CRLF delimiter. Total inspected bytes therefore remain linear, and the
    /// completed terminal event is still recognized when its delimiter lands.
    #[test]
    fn provider_sse_incomplete_event_scan_work_is_linear() {
        let complete = format!(
            "event: response.completed\ndata: {}\n\n",
            serde_json::json!({
                "type": "response.completed",
                "padding": "x".repeat(64 * 1024)
            })
        );
        let incomplete_len = complete.len().saturating_sub(2);
        let mut detector = ProviderSseTerminalDetector::default();
        let mut body = Vec::with_capacity(complete.len());

        for byte in &complete.as_bytes()[..incomplete_len] {
            body.push(*byte);
            assert!(!detector.has_terminal_event(&body));
        }

        assert!(
            detector.searched_bytes <= body.len().saturating_mul(4),
            "searched {} bytes for an {} byte incomplete event",
            detector.searched_bytes,
            body.len()
        );

        body.extend_from_slice(&complete.as_bytes()[incomplete_len..]);
        assert!(detector.has_terminal_event(&body));
    }
}
