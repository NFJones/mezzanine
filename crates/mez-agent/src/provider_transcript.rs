//! Provider-native transcript continuity events.
//!
//! The normal product transcript is provider-neutral and user-visible. Some
//! provider APIs also require opaque message fields to be replayed for
//! multi-turn correctness. This module stores those fields as hidden system
//! transcript entries and lets provider adapters opt into rendering them back
//! into native request messages.

use serde_json::Value;

/// Marker prefix for hidden provider-native transcript entries.
pub const PROVIDER_TRANSCRIPT_EVENT_MARKER: &str = "[mez-provider-transcript-event/v1]\n";

/// Wire-format version for hidden provider transcript events.
const PROVIDER_TRANSCRIPT_EVENT_VERSION: &str = "mez-provider-transcript-event/v1";
/// Provider identifier for DeepSeek-native transcript events.
const DEEPSEEK_PROVIDER_ID: &str = "deepseek";
/// DeepSeek assistant tool-call event kind.
const DEEPSEEK_ASSISTANT_TOOL_CALL_KIND: &str = "assistant_tool_call";
/// DeepSeek tool-result event kind.
const DEEPSEEK_TOOL_RESULT_KIND: &str = "tool_result";

/// Hidden provider-native transcript event replayed only by compatible
/// provider adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderTranscriptEvent {
    /// DeepSeek assistant message containing thinking-mode tool-call metadata.
    DeepSeekAssistantToolCall {
        /// Assistant-visible content associated with the tool call.
        content: String,
        /// DeepSeek thinking-mode reasoning content that must be replayed after
        /// tool calls.
        reasoning_content: Option<String>,
        /// Native DeepSeek tool call objects, including stable call ids.
        tool_calls: Vec<Value>,
    },
    /// DeepSeek tool response paired with a previous assistant tool-call id.
    DeepSeekToolResult {
        /// DeepSeek tool-call id being answered.
        tool_call_id: String,
        /// Provider-facing tool result text.
        content: String,
    },
}

impl ProviderTranscriptEvent {
    /// Encodes one event into hidden transcript content.
    pub fn to_transcript_content(&self) -> String {
        let payload = match self {
            Self::DeepSeekAssistantToolCall {
                content,
                reasoning_content,
                tool_calls,
            } => serde_json::json!({
                "version": PROVIDER_TRANSCRIPT_EVENT_VERSION,
                "provider": DEEPSEEK_PROVIDER_ID,
                "kind": DEEPSEEK_ASSISTANT_TOOL_CALL_KIND,
                "content": content,
                "reasoning_content": reasoning_content,
                "tool_calls": tool_calls,
            }),
            Self::DeepSeekToolResult {
                tool_call_id,
                content,
            } => serde_json::json!({
                "version": PROVIDER_TRANSCRIPT_EVENT_VERSION,
                "provider": DEEPSEEK_PROVIDER_ID,
                "kind": DEEPSEEK_TOOL_RESULT_KIND,
                "tool_call_id": tool_call_id,
                "content": content,
            }),
        };
        format!(
            "{}{}",
            PROVIDER_TRANSCRIPT_EVENT_MARKER,
            serde_json::to_string(&payload)
                .expect("provider transcript event payload contains only JSON values")
        )
    }

    /// Decodes one hidden transcript content block into a provider event.
    pub fn from_transcript_content(content: &str) -> Option<Self> {
        let payload = content.strip_prefix(PROVIDER_TRANSCRIPT_EVENT_MARKER)?;
        let value: Value = serde_json::from_str(payload.trim()).ok()?;
        if value.get("version")?.as_str()? != PROVIDER_TRANSCRIPT_EVENT_VERSION
            || value.get("provider")?.as_str()? != DEEPSEEK_PROVIDER_ID
        {
            return None;
        }
        match value.get("kind")?.as_str()? {
            DEEPSEEK_ASSISTANT_TOOL_CALL_KIND => {
                let tool_calls = value.get("tool_calls")?.as_array()?.clone();
                if tool_calls.is_empty() {
                    return None;
                }
                Some(Self::DeepSeekAssistantToolCall {
                    content: value
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    reasoning_content: value
                        .get("reasoning_content")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                        .map(str::to_string),
                    tool_calls,
                })
            }
            DEEPSEEK_TOOL_RESULT_KIND => Some(Self::DeepSeekToolResult {
                tool_call_id: value.get("tool_call_id")?.as_str()?.to_string(),
                content: value.get("content")?.as_str()?.to_string(),
            }),
            _ => None,
        }
    }

    /// Returns DeepSeek tool-call ids present in this event.
    pub fn deepseek_tool_call_ids(&self) -> Vec<String> {
        match self {
            Self::DeepSeekAssistantToolCall { tool_calls, .. } => tool_calls
                .iter()
                .filter_map(|call| call.get("id").and_then(Value::as_str))
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .collect(),
            Self::DeepSeekToolResult { .. } => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies Anthropic-looking native `tool_use` transcript metadata is not
    /// decoded as provider replay state.
    ///
    /// The first Anthropic release uses Mezzanine's provider-neutral action
    /// result follow-up turns rather than replaying Claude-native `tool_use` /
    /// `tool_result` blocks. Decoding only DeepSeek-native replay records keeps
    /// that strategy explicit and prevents invalid mixed Anthropic continuity
    /// until a full native replay path is implemented deliberately.
    #[test]
    fn anthropic_tool_use_transcript_events_are_not_replayed() {
        let hidden = format!(
            "{}{}",
            PROVIDER_TRANSCRIPT_EVENT_MARKER,
            serde_json::json!({
                "version": PROVIDER_TRANSCRIPT_EVENT_VERSION,
                "provider": "anthropic",
                "kind": "assistant_tool_use",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_1",
                        "name": "submit_maap_action_batch",
                        "input": {"actions": []}
                    }
                ]
            })
        );

        assert_eq!(
            ProviderTranscriptEvent::from_transcript_content(&hidden),
            None
        );
    }

    /// Verifies unknown hidden provider transcript payloads fail closed instead
    /// of leaking opaque native metadata into a replay event.
    ///
    /// Provider-native continuity records are hidden system transcript entries.
    /// A malformed, unsupported, or future-provider payload must not become a
    /// replay event for another provider, because that could expose native tool
    /// metadata in the wrong request shape or user-visible transcript path.
    #[test]
    fn unknown_provider_transcript_events_fail_closed() {
        let hidden = format!(
            "{}{}",
            PROVIDER_TRANSCRIPT_EVENT_MARKER,
            serde_json::json!({
                "version": PROVIDER_TRANSCRIPT_EVENT_VERSION,
                "provider": "future-provider",
                "kind": "assistant_tool_call",
                "tool_calls": [{"id": "call_1"}]
            })
        );

        assert_eq!(
            ProviderTranscriptEvent::from_transcript_content(&hidden),
            None
        );
        assert_eq!(
            ProviderTranscriptEvent::from_transcript_content("ordinary transcript text"),
            None
        );
    }

    /// Verifies the existing DeepSeek replay format remains the only supported
    /// native provider-transcript event family.
    ///
    /// This protects the provider-neutral Anthropic continuity decision without
    /// regressing DeepSeek thinking-mode replay, which still needs hidden native
    /// tool-call metadata and paired tool results.
    #[test]
    fn deepseek_provider_transcript_events_round_trip() {
        let event = ProviderTranscriptEvent::DeepSeekAssistantToolCall {
            content: "visible assistant text".to_string(),
            reasoning_content: Some("hidden reasoning".to_string()),
            tool_calls: vec![serde_json::json!({
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "submit_maap_action_batch",
                    "arguments": "{}"
                }
            })],
        };

        let encoded = event.to_transcript_content();
        assert_eq!(
            ProviderTranscriptEvent::from_transcript_content(&encoded),
            Some(event)
        );
    }
}
