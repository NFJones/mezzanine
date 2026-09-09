//! Provider-native transcript continuity events.
//!
//! The normal product transcript is provider-neutral and user-visible. Some
//! provider APIs also require opaque message fields to be replayed for
//! multi-turn correctness. This module stores those fields as hidden system
//! transcript entries and lets provider adapters opt into rendering them back
//! into native request messages.

use std::collections::BTreeSet;

use serde_json::Value;

/// Marker prefix for hidden provider-native transcript entries.
pub const PROVIDER_TRANSCRIPT_EVENT_MARKER: &str = "[mez-provider-transcript-event/v1]\n";

/// Wire-format version for hidden provider transcript events.
const PROVIDER_TRANSCRIPT_EVENT_VERSION: &str = "mez-provider-transcript-event/v1";
/// Provider identifier for DeepSeek-native transcript events.
const DEEPSEEK_PROVIDER_ID: &str = "deepseek";
/// Provider identifier for OpenAI Responses-native transcript events.
const OPENAI_PROVIDER_ID: &str = "openai";
/// API-family marker for generic OpenAI-compatible Chat Completions events.
const OPENAI_CHAT_COMPLETIONS_API_ID: &str = "openai_chat_completions";
/// DeepSeek assistant tool-call event kind.
const DEEPSEEK_ASSISTANT_TOOL_CALL_KIND: &str = "assistant_tool_call";
/// DeepSeek tool-result event kind.
const DEEPSEEK_TOOL_RESULT_KIND: &str = "tool_result";
/// OpenAI Responses output-sequence event kind.
const OPENAI_RESPONSE_OUTPUT_KIND: &str = "response_output";
/// OpenAI Responses function-call-output event kind.
const OPENAI_FUNCTION_CALL_OUTPUT_KIND: &str = "function_call_output";
/// Generic Chat Completions assistant tool-call event kind.
const OPENAI_CHAT_COMPLETIONS_ASSISTANT_TOOL_CALL_KIND: &str =
    "chat_completions_assistant_tool_call";
/// Generic Chat Completions tool-result event kind.
const OPENAI_CHAT_COMPLETIONS_TOOL_RESULT_KIND: &str = "chat_completions_tool_result";

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
    /// Generic Chat Completions assistant message with native MAAP calls.
    OpenAiChatCompletionsAssistantToolCall {
        /// Configured provider instance that produced the opaque call ids.
        provider_id: String,
        /// Assistant-visible content associated with the native call.
        content: String,
        /// Native Chat Completions tool-call objects in declaration order.
        tool_calls: Vec<Value>,
    },
    /// Generic Chat Completions result paired with one native call id.
    OpenAiChatCompletionsToolResult {
        /// Configured provider instance that owns the opaque call id.
        provider_id: String,
        /// Native Chat Completions tool-call identity being answered.
        tool_call_id: String,
        /// Provider-facing action result text.
        content: String,
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
            Self::OpenAiChatCompletionsAssistantToolCall {
                provider_id,
                content,
                tool_calls,
            } => serde_json::json!({
                "version": PROVIDER_TRANSCRIPT_EVENT_VERSION,
                "api": OPENAI_CHAT_COMPLETIONS_API_ID,
                "provider": provider_id,
                "kind": OPENAI_CHAT_COMPLETIONS_ASSISTANT_TOOL_CALL_KIND,
                "content": content,
                "tool_calls": tool_calls,
            }),
            Self::OpenAiChatCompletionsToolResult {
                provider_id,
                tool_call_id,
                content,
            } => serde_json::json!({
                "version": PROVIDER_TRANSCRIPT_EVENT_VERSION,
                "api": OPENAI_CHAT_COMPLETIONS_API_ID,
                "provider": provider_id,
                "kind": OPENAI_CHAT_COMPLETIONS_TOOL_RESULT_KIND,
                "tool_call_id": tool_call_id,
                "content": content,
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
        if value.get("api").and_then(Value::as_str) == Some(OPENAI_CHAT_COMPLETIONS_API_ID) {
            return match kind {
                OPENAI_CHAT_COMPLETIONS_ASSISTANT_TOOL_CALL_KIND => {
                    Self::validated_openai_chat_completions_assistant_tool_call(
                        provider.to_string(),
                        value
                            .get("content")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        value.get("tool_calls")?.as_array()?.clone(),
                    )
                }
                OPENAI_CHAT_COMPLETIONS_TOOL_RESULT_KIND => {
                    let tool_call_id = value.get("tool_call_id")?.as_str()?;
                    if !provider_instance_id_is_valid(provider) || tool_call_id.is_empty() {
                        return None;
                    }
                    Some(Self::OpenAiChatCompletionsToolResult {
                        provider_id: provider.to_string(),
                        tool_call_id: tool_call_id.to_string(),
                        content: value.get("content")?.as_str()?.to_string(),
                    })
                }
                _ => None,
            };
        }
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
                    output: output.to_string(),
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
            (DEEPSEEK_PROVIDER_ID, DEEPSEEK_TOOL_RESULT_KIND) => {
                let tool_call_id = value.get("tool_call_id")?.as_str()?;
                if tool_call_id.is_empty() {
                    return None;
                }
                Some(Self::DeepSeekToolResult {
                    tool_call_id: tool_call_id.to_string(),
                    content: value.get("content")?.as_str()?.to_string(),
                })
            }
            _ => None,
        }
    }

    /// Returns a reduced event suitable for replay from a legacy transcript.
    ///
    /// New typed execution records replay the exact event without calling this
    /// method. Untyped legacy transcript import retains the reduction because
    /// those records cannot prove which bytes were originally model-visible.
    pub fn sanitized_for_historical_replay(&self) -> Option<Self> {
        match self {
            Self::OpenAiFunctionCallOutput { call_id, output } => {
                Some(Self::OpenAiFunctionCallOutput {
                    call_id: call_id.clone(),
                    output: crate::historical_tool_result_context_content(output)?,
                })
            }
            Self::OpenAiChatCompletionsToolResult {
                provider_id,
                tool_call_id,
                content,
            } => Some(Self::OpenAiChatCompletionsToolResult {
                provider_id: provider_id.clone(),
                tool_call_id: tool_call_id.clone(),
                content: crate::historical_tool_result_context_content(content)?,
            }),
            Self::DeepSeekToolResult {
                tool_call_id,
                content,
            } => Some(Self::DeepSeekToolResult {
                tool_call_id: tool_call_id.clone(),
                content: crate::historical_tool_result_context_content(content)?,
            }),
            Self::OpenAiResponseOutput { .. }
            | Self::OpenAiChatCompletionsAssistantToolCall { .. }
            | Self::DeepSeekAssistantToolCall { .. } => Some(self.clone()),
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
            | Self::OpenAiChatCompletionsAssistantToolCall { .. }
            | Self::OpenAiChatCompletionsToolResult { .. }
            | Self::DeepSeekToolResult { .. } => Vec::new(),
        }
    }

    /// Builds a strictly validated generic Chat Completions assistant event.
    pub fn validated_openai_chat_completions_assistant_tool_call(
        provider_id: String,
        content: String,
        tool_calls: Vec<Value>,
    ) -> Option<Self> {
        if !provider_instance_id_is_valid(&provider_id)
            || Self::validated_openai_chat_completions_tool_calls(&tool_calls).is_none()
        {
            return None;
        }
        Some(Self::OpenAiChatCompletionsAssistantToolCall {
            provider_id,
            content,
            tool_calls,
        })
    }

    /// Validates native generic Chat Completions MAAP calls for safe replay.
    pub fn validated_openai_chat_completions_tool_calls(tool_calls: &[Value]) -> Option<()> {
        if tool_calls.is_empty()
            || !tool_calls
                .iter()
                .all(openai_chat_completions_tool_call_is_valid)
        {
            return None;
        }
        let mut ids = BTreeSet::new();
        tool_calls
            .iter()
            .all(|call| ids.insert(call["id"].as_str().expect("validated call id").to_string()))
            .then_some(())
    }

    /// Returns generic Chat Completions MAAP call ids in declaration order.
    pub fn openai_chat_completions_tool_call_ids(&self) -> Vec<String> {
        let Self::OpenAiChatCompletionsAssistantToolCall { tool_calls, .. } = self else {
            return Vec::new();
        };
        tool_calls
            .iter()
            .filter_map(|call| call.get("id").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
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
    pub fn provider_id(&self) -> &str {
        match self {
            Self::OpenAiResponseOutput { .. } | Self::OpenAiFunctionCallOutput { .. } => {
                OPENAI_PROVIDER_ID
            }
            Self::OpenAiChatCompletionsAssistantToolCall { provider_id, .. }
            | Self::OpenAiChatCompletionsToolResult { provider_id, .. } => provider_id,
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
            Self::OpenAiChatCompletionsAssistantToolCall { .. }
            | Self::OpenAiChatCompletionsToolResult { .. }
            | Self::DeepSeekAssistantToolCall { .. }
            | Self::DeepSeekToolResult { .. } => None,
        }
    }
}

fn provider_instance_id_is_valid(provider_id: &str) -> bool {
    !provider_id.is_empty()
        && provider_id.trim() == provider_id
        && provider_id.chars().all(|character| !character.is_control())
}

fn openai_chat_completions_tool_call_is_valid(call: &Value) -> bool {
    call.get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty())
        && call.get("type").and_then(Value::as_str) == Some("function")
        && call
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            == Some(crate::MAAP_ACTION_BATCH_TOOL_NAME)
        && call
            .get("function")
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str)
            .is_some_and(|arguments| {
                !arguments.is_empty()
                    && serde_json::from_str::<Value>(arguments).is_ok_and(|value| value.is_object())
            })
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

    /// Verifies generic Chat Completions events retain the configured provider
    /// instance and reject native MAAP calls without replay-safe identity.
    #[test]
    fn openai_chat_completions_events_round_trip_and_reject_partial_calls() {
        let tool_calls = vec![serde_json::json!({
            "id": "call_compatible_1",
            "type": "function",
            "function": {
                "name": "submit_maap_action_batch",
                "arguments": "{}"
            }
        })];
        let event = ProviderTranscriptEvent::validated_openai_chat_completions_assistant_tool_call(
            "bedrock".to_string(),
            "visible assistant content".to_string(),
            tool_calls,
        )
        .unwrap();

        assert_eq!(
            ProviderTranscriptEvent::from_transcript_content(&event.to_transcript_content()),
            Some(event.clone())
        );
        assert_eq!(event.provider_id(), "bedrock");
        assert_eq!(
            event.openai_chat_completions_tool_call_ids(),
            ["call_compatible_1"]
        );
        assert_eq!(
            ProviderTranscriptEvent::validated_openai_chat_completions_assistant_tool_call(
                "bedrock".to_string(),
                String::new(),
                vec![serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": "submit_maap_action_batch",
                        "arguments": "{}"
                    }
                })],
            ),
            None
        );
        assert_eq!(
            ProviderTranscriptEvent::validated_openai_chat_completions_assistant_tool_call(
                "bedrock".to_string(),
                String::new(),
                vec![
                    serde_json::json!({
                        "id": "duplicate-call",
                        "type": "function",
                        "function": {
                            "name": "submit_maap_action_batch",
                            "arguments": "{}"
                        }
                    }),
                    serde_json::json!({
                        "id": "duplicate-call",
                        "type": "function",
                        "function": {
                            "name": "submit_maap_action_batch",
                            "arguments": "{}"
                        }
                    }),
                ],
            ),
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

    /// Verifies decoding preserves live provider-native tool output while the
    /// explicit durable-history policy removes raw result bodies.
    ///
    /// Current provider continuations and historical transcript restoration
    /// share the event encoding, so this boundary test prevents either live
    /// output loss or disclosure of persisted command output.
    #[test]
    fn provider_tool_results_are_lossless_until_historical_sanitization() {
        let deepseek_event = ProviderTranscriptEvent::DeepSeekToolResult {
            tool_call_id: "call_1".to_string(),
            content: "[action_result a1 shell_command succeeded]\noutput:\nnative-secret"
                .to_string(),
        };
        let openai_event = ProviderTranscriptEvent::OpenAiFunctionCallOutput {
            call_id: "call_2".to_string(),
            output: "[action_result a2 shell_command succeeded]\noutput:\nopenai-secret"
                .to_string(),
        };

        for (event, secret) in [
            (deepseek_event, "native-secret"),
            (openai_event, "openai-secret"),
        ] {
            let decoded =
                ProviderTranscriptEvent::from_transcript_content(&event.to_transcript_content())
                    .unwrap();
            assert_eq!(decoded, event);

            let historical = decoded.sanitized_for_historical_replay().unwrap();
            let encoded = historical.to_transcript_content();
            assert!(encoded.contains("historical_output: omitted"));
            assert!(!encoded.contains(secret));
        }
    }

    /// Verifies provider-native tool results require non-empty provider call
    /// identities before adapters can claim and replay the hidden event.
    #[test]
    fn provider_tool_results_reject_empty_call_ids() {
        for event in [
            ProviderTranscriptEvent::DeepSeekToolResult {
                tool_call_id: String::new(),
                content: "result".to_string(),
            },
            ProviderTranscriptEvent::OpenAiFunctionCallOutput {
                call_id: String::new(),
                output: "result".to_string(),
            },
        ] {
            assert_eq!(
                ProviderTranscriptEvent::from_transcript_content(&event.to_transcript_content()),
                None
            );
        }
    }
}
