//! Typed model-context events stored inside durable transcripts.
//!
//! Ordinary transcript roles describe conversational messages, while some
//! provider-independent context must survive between turns without pretending
//! to be user or assistant speech. This module gives those records a reserved,
//! versioned system-entry encoding. Decoders reject malformed, unknown, and
//! unsupported payloads so durable audit records cannot become model context by
//! accident.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Marker prefix for provider-independent transcript context events.
pub const TRANSCRIPT_CONTEXT_EVENT_MARKER: &str = "[mez-transcript-context-event/v1]\n";

/// Wire-format version for transcript context events.
const TRANSCRIPT_CONTEXT_EVENT_VERSION: &str = "mez-transcript-context-event/v1";
/// Event kind for a summarized routed-worker handoff.
const ROUTED_HANDOFF_KIND: &str = "routed_handoff";
/// Event kind for a user turn stopped before normal completion.
const INTERRUPTED_TURN_KIND: &str = "interrupted_turn";
/// Event kind for one immutable pane-environment projection.
const ENVIRONMENT_SNAPSHOT_KIND: &str = "environment_snapshot";
/// Maximum serialized environment projection accepted from durable storage.
const ENVIRONMENT_SNAPSHOT_CONTENT_LIMIT_BYTES: usize = 64 * 1024;

/// Provider-independent context that is durable across conversation turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptContextEvent {
    /// Validated routed-worker summary presented through the parent model.
    RoutedHandoff {
        /// Serialized summarized handoff content.
        content: String,
    },
    /// Canonical pane-environment projection sampled at a user-prompt boundary.
    EnvironmentSnapshot {
        /// SHA-256 digest of the exact model-visible projection.
        projection_sha256: String,
        /// Exact model-visible environment projection.
        content: String,
    },
    /// Original intent and settled observations retained when a turn stops.
    InterruptedTurn {
        /// Original user prompt for the stopped turn.
        prompt: String,
        /// Runtime reason for the interruption.
        reason: String,
        /// Safely serializable action observations available at interruption.
        evidence: Vec<String>,
    },
}

impl TranscriptContextEvent {
    /// Builds one validated immutable environment-snapshot event.
    ///
    /// Empty or oversized projections return `None`; accepted content receives
    /// a digest over the exact bytes that durable replay will present.
    pub fn environment_snapshot(content: impl Into<String>) -> Option<Self> {
        let content = content.into();
        if content.trim().is_empty() || content.len() > ENVIRONMENT_SNAPSHOT_CONTENT_LIMIT_BYTES {
            return None;
        }
        let projection_sha256 = Sha256::digest(content.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Some(Self::EnvironmentSnapshot {
            projection_sha256,
            content,
        })
    }

    /// Encodes one event as a reserved system transcript entry.
    pub fn to_transcript_content(&self) -> String {
        let payload = match self {
            Self::RoutedHandoff { content } => serde_json::json!({
                "version": TRANSCRIPT_CONTEXT_EVENT_VERSION,
                "kind": ROUTED_HANDOFF_KIND,
                "content": content,
            }),
            Self::EnvironmentSnapshot {
                projection_sha256,
                content,
            } => serde_json::json!({
                "version": TRANSCRIPT_CONTEXT_EVENT_VERSION,
                "kind": ENVIRONMENT_SNAPSHOT_KIND,
                "projection_sha256": projection_sha256,
                "content": content,
            }),
            Self::InterruptedTurn {
                prompt,
                reason,
                evidence,
            } => serde_json::json!({
                "version": TRANSCRIPT_CONTEXT_EVENT_VERSION,
                "kind": INTERRUPTED_TURN_KIND,
                "prompt": prompt,
                "reason": reason,
                "evidence": evidence,
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
            ENVIRONMENT_SNAPSHOT_KIND => {
                let projection_sha256 = value.get("projection_sha256")?.as_str()?;
                let content = value.get("content")?.as_str()?;
                if !valid_environment_snapshot(projection_sha256, content) {
                    return None;
                }
                Some(Self::EnvironmentSnapshot {
                    projection_sha256: projection_sha256.to_string(),
                    content: content.to_string(),
                })
            }
            INTERRUPTED_TURN_KIND => {
                let prompt = value.get("prompt")?.as_str()?.trim();
                let reason = value.get("reason")?.as_str()?.trim();
                if prompt.is_empty() || reason.is_empty() {
                    return None;
                }
                let evidence = value
                    .get("evidence")?
                    .as_array()?
                    .iter()
                    .map(|entry| entry.as_str().map(str::to_string))
                    .collect::<Option<Vec<_>>>()?;
                Some(Self::InterruptedTurn {
                    prompt: prompt.to_string(),
                    reason: reason.to_string(),
                    evidence,
                })
            }
            _ => None,
        }
    }
}

/// Validates one environment snapshot before durable content becomes model-visible.
fn valid_environment_snapshot(projection_sha256: &str, content: &str) -> bool {
    if content.trim().is_empty()
        || content.len() > ENVIRONMENT_SNAPSHOT_CONTENT_LIMIT_BYTES
        || projection_sha256.len() != 64
        || !projection_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return false;
    }
    let digest = Sha256::digest(content.as_bytes());
    let expected = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    projection_sha256 == expected
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

    /// Verifies an environment snapshot retains its exact projection and
    /// digest across durable encoding and decoding.
    #[test]
    fn environment_snapshot_transcript_context_event_round_trips() {
        let content = "environment_state=known\nshell=posix-sh".to_string();
        let projection_sha256 = Sha256::digest(content.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let event = TranscriptContextEvent::EnvironmentSnapshot {
            projection_sha256,
            content,
        };

        let encoded = event.to_transcript_content();

        assert_eq!(
            TranscriptContextEvent::from_transcript_content(&encoded),
            Some(event)
        );
    }

    /// Verifies an interrupted turn retains its original intent and only the
    /// caller-provided safe action observations for subsequent continuation.
    #[test]
    fn interrupted_turn_transcript_context_event_round_trips() {
        let event = TranscriptContextEvent::InterruptedTurn {
            prompt: "repair the interrupted task".to_string(),
            reason: "agent turn stopped".to_string(),
            evidence: vec!["action_id=read-1 type=shell_command status=running".to_string()],
        };

        let encoded = event.to_transcript_content();

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
            r#"{"version":"mez-transcript-context-event/v1","kind":"environment_snapshot","projection_sha256":"invalid","content":"environment_state=known"}"#,
            r#"{"version":"mez-transcript-context-event/v1","kind":"environment_snapshot","projection_sha256":"0000000000000000000000000000000000000000000000000000000000000000","content":"environment_state=known"}"#,
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
