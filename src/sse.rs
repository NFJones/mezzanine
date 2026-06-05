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
pub(crate) fn parse_sse_events_with<F>(
    body: &str,
    missing_message: &'static str,
    mut on_event: F,
) -> Result<()>
where
    F: FnMut(Option<&str>, &str) -> Result<()>,
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
        return Err(MezError::invalid_state(missing_message));
    }
    Ok(())
}

/// Parses SSE event blocks from one complete response body.
///
/// Empty blocks, comments, and blocks without `data:` fields are ignored. The
/// parser accepts LF and CRLF input and leaves field-specific validation to the
/// caller so provider and MCP policies can differ intentionally.
pub(crate) fn parse_sse_events(body: &str, missing_message: &'static str) -> Result<Vec<SseEvent>> {
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

    /// Verifies the shared parser also flushes a final event without a blank-line terminator.
    ///
    /// Streaming HTTP peers can end the body immediately after the last `data:`
    /// line. The shared syntax parser still needs to surface that event without
    /// requiring an extra separator block.
    #[test]
    fn parses_unterminated_final_sse_event() {
        let events = parse_sse_events("event: message\ndata: {\"done\":true}", "missing events")
            .expect("SSE event should parse");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name.as_deref(), Some("message"));
        assert_eq!(events[0].data, "{\"done\":true}");
    }
}
