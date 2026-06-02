//! Shared server-sent event parsing primitives.
//!
//! The module owns only syntax-level SSE parsing: line-ending normalization,
//! comment skipping, event names, and multi-line `data:` assembly. Provider and
//! MCP callers remain responsible for endpoint-specific completion, `[DONE]`,
//! malformed-event, and JSON-RPC policy.

use crate::error::{MezError, Result};

/// One parsed server-sent event block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseEvent {
    /// Optional event name from the last `event:` field in the block.
    pub(crate) name: Option<String>,
    /// Joined `data:` field payload with embedded newlines between lines.
    pub(crate) data: String,
}

/// Parses SSE event blocks from one complete response body.
///
/// Empty blocks, comments, and blocks without `data:` fields are ignored. The
/// parser accepts LF and CRLF input and leaves field-specific validation to the
/// caller so provider and MCP policies can differ intentionally.
pub(crate) fn parse_sse_events(body: &str, missing_message: &'static str) -> Result<Vec<SseEvent>> {
    let mut events = Vec::new();
    for block in body.replace("\r\n", "\n").split("\n\n") {
        let mut name = None;
        let mut data_lines = Vec::new();
        for line in block.lines() {
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if let Some(value) = line.strip_prefix("event:") {
                name = Some(value.trim().to_string());
            } else if let Some(value) = line.strip_prefix("data:") {
                data_lines.push(value.trim_start().to_string());
            }
        }
        if !data_lines.is_empty() {
            events.push(SseEvent {
                name,
                data: data_lines.join("\n"),
            });
        }
    }
    if events.is_empty() {
        return Err(MezError::invalid_state(missing_message));
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::parse_sse_events;

    /// Verifies shared SSE parsing preserves event names and multi-line data.
    ///
    /// Provider and MCP parsers both consume SSE bodies. This regression keeps
    /// the common syntax parser aligned while leaving endpoint policy outside
    /// the helper.
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
}
