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
/// Provider identifier for OpenAI Responses-native transcript events.
const OPENAI_PROVIDER_ID: &str = "openai";
/// DeepSeek assistant tool-call event kind.
const DEEPSEEK_ASSISTANT_TOOL_CALL_KIND: &str = "assistant_tool_call";
/// DeepSeek tool-result event kind.
const DEEPSEEK_TOOL_RESULT_KIND: &str = "tool_result";
/// OpenAI Responses output-sequence event kind.
const OPENAI_RESPONSE_OUTPUT_KIND: &str = "response_output";
/// OpenAI Responses function-call-output event kind.
const OPENAI_FUNCTION_CALL_OUTPUT_KIND: &str = "function_call_output";

/// Hidden provider-native transcript event replayed only by compatible
/// provider adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderTranscriptEvent {
    /// Complete ordered output items returned by one OpenAI Responses call.
    OpenAiResponseOutput {
        /// Opaque Responses output items retained in provider order.
        items: Vec<Value>,
    },
    /// OpenAI-native function result paired with a retained function call.
    OpenAiFunctionCallOutput {
        /// Native Responses function-call identity being answered.
        call_id: String,
        /// Provider-facing action result text.
        output: String,
    },
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
            Self::OpenAiResponseOutput { items } => serde_json::json!({
                "version": PROVIDER_TRANSCRIPT_EVENT_VERSION,
                "provider": OPENAI_PROVIDER_ID,
                "kind": OPENAI_RESPONSE_OUTPUT_KIND,
                "items": items,
            }),
            Self::OpenAiFunctionCallOutput { call_id, output } => serde_json::json!({
                "version": PROVIDER_TRANSCRIPT_EVENT_VERSION,
                "provider": OPENAI_PROVIDER_ID,
                "kind": OPENAI_FUNCTION_CALL_OUTPUT_KIND,
                "call_id": call_id,
                "output": output,
            }),
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
        if value.get("version")?.as_str()? != PROVIDER_TRANSCRIPT_EVENT_VERSION {
            return None;
        }
        let provider = value.get("provider")?.as_str()?;
        let kind = value.get("kind")?.as_str()?;
        match (provider, kind) {
            (OPENAI_PROVIDER_ID, OPENAI_RESPONSE_OUTPUT_KIND) => {
                let items = value.get("items")?.as_array()?.clone();
                Self::validated_openai_response_output(items)
            }
            (OPENAI_PROVIDER_ID, OPENAI_FUNCTION_CALL_OUTPUT_KIND) => {
                let call_id = value.get("call_id")?.as_str()?;
                let output = value.get("output")?.as_str()?;
                if call_id.is_empty() {
                    return None;
                }
                Some(Self::OpenAiFunctionCallOutput {
                    call_id: call_id.to_string(),
                    output: crate::historical_tool_result_context_content(output)?,
                })
            }
            (DEEPSEEK_PROVIDER_ID, DEEPSEEK_ASSISTANT_TOOL_CALL_KIND) => {
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
            (DEEPSEEK_PROVIDER_ID, DEEPSEEK_TOOL_RESULT_KIND) => Some(Self::DeepSeekToolResult {
                tool_call_id: value.get("tool_call_id")?.as_str()?.to_string(),
                content: crate::historical_tool_result_context_content(
                    value.get("content")?.as_str()?,
                )?,
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
            Self::OpenAiResponseOutput { .. }
            | Self::OpenAiFunctionCallOutput { .. }
            | Self::DeepSeekToolResult { .. } => Vec::new(),
        }
    }

    /// Builds a validated opaque OpenAI Responses output event.
    ///
    /// Unknown item types remain opaque, but every item must be an object with
    /// a non-empty type. Function calls additionally require the native
    /// identity and fields needed for a later `function_call_output`.
    pub fn validated_openai_response_output(items: Vec<Value>) -> Option<Self> {
        if items.is_empty() || !items.iter().all(openai_response_output_item_is_valid) {
            return None;
        }
        Some(Self::OpenAiResponseOutput { items })
    }

    /// Returns OpenAI MAAP function-call ids present in this event.
    pub fn openai_function_call_ids(&self) -> Vec<String> {
        let Self::OpenAiResponseOutput { items } = self else {
            return Vec::new();
        };
        items
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
            .filter(|item| {
                item.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| {
                        name == crate::MAAP_ACTION_BATCH_TOOL_NAME
                            || crate::OpenAiMaapToolSurface::stable_surfaces()
                                .iter()
                                .any(|surface| name == surface.tool_name())
                    })
            })
            .filter_map(|item| item.get("call_id").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }

    /// Returns the provider id that exclusively owns this native event.
    pub fn provider_id(&self) -> &'static str {
        match self {
            Self::OpenAiResponseOutput { .. } | Self::OpenAiFunctionCallOutput { .. } => {
                OPENAI_PROVIDER_ID
            }
            Self::DeepSeekAssistantToolCall { .. } | Self::DeepSeekToolResult { .. } => {
                DEEPSEEK_PROVIDER_ID
            }
        }
    }

    /// Returns OpenAI Responses input items represented by this event.
    pub fn openai_input_items(&self) -> Option<Vec<Value>> {
        match self {
            Self::OpenAiResponseOutput { items } => Some(items.clone()),
            Self::OpenAiFunctionCallOutput { call_id, output } => Some(vec![serde_json::json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": output,
            })]),
            Self::DeepSeekAssistantToolCall { .. } | Self::DeepSeekToolResult { .. } => None,
        }
    }
}

/// Validates the minimum replay contract for one opaque Responses output item.
fn openai_response_output_item_is_valid(item: &Value) -> bool {
    let Some(item_type) = item
        .get("type")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
    else {
        return false;
    };
    if item_type != "function_call" {
        return true;
    }
    ["id", "call_id", "name", "arguments"].iter().all(|field| {
        item.get(*field)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies OpenAI continuity events retain opaque output fields while
    /// malformed function calls fail closed at the hidden transcript boundary.
    ///
    /// Replaying a partial native call would corrupt a stateless Responses
    /// chain, so all identity fields required by `function_call_output` are
    /// validated before the event can acquire OpenAI ownership.
    #[test]
    fn openai_provider_transcript_events_round_trip_and_reject_partial_calls() {
        let items = vec![
            serde_json::json!({
                "type": "reasoning",
                "id": "rs_1",
                "encrypted_content": "opaque-ciphertext"
            }),
            serde_json::json!({
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "submit_maap_action_batch",
                "arguments": "{}"
            }),
        ];
        let event = ProviderTranscriptEvent::validated_openai_response_output(items).unwrap();

        assert_eq!(
            ProviderTranscriptEvent::from_transcript_content(&event.to_transcript_content()),
            Some(event.clone())
        );
        assert_eq!(event.provider_id(), "openai");
        assert_eq!(event.openai_function_call_ids(), vec!["call_1"]);
        assert_eq!(
            ProviderTranscriptEvent::validated_openai_response_output(vec![serde_json::json!({
                "type": "function_call",
                "id": "fc_1",
                "name": "submit_maap_action_batch",
                "arguments": "{}"
            })]),
            None
        );
    }

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

    #[test]
    /// Verifies legacy provider-native tool results cannot replay raw output
    /// bodies while canonical action identity and status remain available.
    fn deepseek_tool_result_replay_omits_legacy_raw_output() {
        let event = ProviderTranscriptEvent::DeepSeekToolResult {
            tool_call_id: "call_1".to_string(),
            content: "[action_result a1 shell_command succeeded]\noutput:\nnative-secret"
                .to_string(),
        };

        let decoded =
            ProviderTranscriptEvent::from_transcript_content(&event.to_transcript_content())
                .unwrap();
        let ProviderTranscriptEvent::DeepSeekToolResult { content, .. } = decoded else {
            panic!("expected DeepSeek tool result");
        };

        assert!(content.contains("[action_result a1 shell_command succeeded]"));
        assert!(content.contains("historical_output: omitted"));
        assert!(!content.contains("native-secret"));
    }
}
