//! Typed model-context events stored inside durable transcripts.
//!
//! Ordinary transcript roles describe conversational messages, while some
//! provider-independent context must survive between turns without pretending
//! to be user or assistant speech. This module gives those records a reserved,
//! versioned system-entry encoding. Decoders reject malformed, unknown, and
//! unsupported payloads so durable audit records cannot become model context by
//! accident.

use serde_json::Value;

/// Marker prefix for provider-independent transcript context events.
pub const TRANSCRIPT_CONTEXT_EVENT_MARKER: &str = "[mez-transcript-context-event/v1]\n";

/// Wire-format version for transcript context events.
const TRANSCRIPT_CONTEXT_EVENT_VERSION: &str = "mez-transcript-context-event/v1";
/// Event kind for a summarized routed-worker handoff.
const ROUTED_HANDOFF_KIND: &str = "routed_handoff";

/// Provider-independent context that is durable across conversation turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptContextEvent {
    /// Validated routed-worker summary presented through the parent model.
    RoutedHandoff {
        /// Serialized summarized handoff content.
        content: String,
    },
}

impl TranscriptContextEvent {
    /// Encodes one event as a reserved system transcript entry.
    pub fn to_transcript_content(&self) -> String {
        let payload = match self {
            Self::RoutedHandoff { content } => serde_json::json!({
                "version": TRANSCRIPT_CONTEXT_EVENT_VERSION,
                "kind": ROUTED_HANDOFF_KIND,
                "content": content,
            }),
        };
        format!(
            "{}{}",
            TRANSCRIPT_CONTEXT_EVENT_MARKER,
            serde_json::to_string(&payload)
                .expect("transcript context event payload contains only JSON values")
        )
    }

    /// Decodes a supported reserved transcript context event.
    ///
    /// Malformed payloads, unknown kinds, unsupported versions, and empty
    /// routed handoffs return `None` so callers never inject them into model
    /// context.
    pub fn from_transcript_content(content: &str) -> Option<Self> {
        let payload = content.strip_prefix(TRANSCRIPT_CONTEXT_EVENT_MARKER)?;
        let value: Value = serde_json::from_str(payload.trim()).ok()?;
        if value.get("version")?.as_str()? != TRANSCRIPT_CONTEXT_EVENT_VERSION {
            return None;
        }
        match value.get("kind")?.as_str()? {
            ROUTED_HANDOFF_KIND => {
                let content = value.get("content")?.as_str()?.trim();
                if content.is_empty() {
                    return None;
                }
                Some(Self::RoutedHandoff {
                    content: content.to_string(),
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies routed-handoff context survives an exact typed transcript
    /// round trip without changing its serialized summary.
    #[test]
    fn routed_handoff_transcript_context_event_round_trips() {
        let event = TranscriptContextEvent::RoutedHandoff {
            content: r#"{"version":1,"result_summary":"done"}"#.to_string(),
        };

        let encoded = event.to_transcript_content();

        assert!(encoded.starts_with(TRANSCRIPT_CONTEXT_EVENT_MARKER));
        assert_eq!(
            TranscriptContextEvent::from_transcript_content(&encoded),
            Some(event)
        );
    }

    /// Verifies malformed, unsupported, unknown, and empty context records are
    /// ignored rather than becoming model-visible durable context.
    #[test]
    fn transcript_context_event_rejects_unsupported_payloads() {
        for payload in [
            "not json",
            r#"{"version":"mez-transcript-context-event/v2","kind":"routed_handoff","content":"summary"}"#,
            r#"{"version":"mez-transcript-context-event/v1","kind":"unknown","content":"summary"}"#,
            r#"{"version":"mez-transcript-context-event/v1","kind":"routed_handoff","content":""}"#,
        ] {
            let encoded = format!("{TRANSCRIPT_CONTEXT_EVENT_MARKER}{payload}");
            assert!(
                TranscriptContextEvent::from_transcript_content(&encoded).is_none(),
                "unexpectedly decoded {payload}"
            );
        }
        assert!(
            TranscriptContextEvent::from_transcript_content("ordinary system record").is_none()
        );
    }
}
