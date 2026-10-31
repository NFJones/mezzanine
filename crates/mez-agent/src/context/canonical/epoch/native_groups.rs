//! Native declaration/result closure on exact API/provider/call identities.
//! Runtime retirement alone cannot make unmatched serialized native calls safe
//! to summarize. Validation observes canonical source without executing it.

use super::*;
use crate::ProviderTranscriptEvent;

/// Still-unanswered call identities and their exact declaring occurrences.
/// Answered IDs may be reused by later source occurrences without conflating
/// those occurrences or treating repeated visible text as source identity.
#[derive(Default)]
struct NativeGroupCalls {
    pending: BTreeMap<String, ContextEventSequence>,
}

impl ReplayEpochContext {
    /// Rejects incomplete, repeated or out-of-order native pairs before pending
    /// protection can be released. Ordinary closed assistant/action groups need
    /// no synthetic native declaration or result to pass this check.
    pub(super) fn validate_native_group_closure(
        &self,
        group: &ContextExecutionGroupId,
    ) -> AgentContextResult<()> {
        let mut owners = BTreeMap::<(&'static str, String), NativeGroupCalls>::new();
        for event in self
            .context
            .chronology()
            .iter()
            .filter(|event| event.execution_group_id() == Some(group))
        {
            let Some(native) =
                ProviderTranscriptEvent::from_transcript_content(&event.block().content)
            else {
                continue;
            };
            let owner = event.provider_owner().ok_or_else(|| {
                AgentContextError::new("native group source has no provider owner")
            })?;
            let calls = match &native {
                ProviderTranscriptEvent::OpenAiResponseOutput { items } => items
                    .iter()
                    .filter(|item| {
                        item.get("type").and_then(serde_json::Value::as_str)
                            == Some("function_call")
                    })
                    .filter_map(|item| item.get("call_id").and_then(serde_json::Value::as_str))
                    .map(str::to_owned)
                    .collect(),
                ProviderTranscriptEvent::OpenAiChatCompletionsAssistantToolCall { .. } => {
                    native.openai_chat_completions_tool_call_ids()
                }
                ProviderTranscriptEvent::DeepSeekAssistantToolCall { .. } => {
                    native.deepseek_tool_call_ids()
                }
                _ => Vec::new(),
            };
            let state = owners
                .entry((owner.api().as_str(), owner.provider_id().to_owned()))
                .or_default();
            for call in calls {
                if state.pending.insert(call, event.sequence()).is_some() {
                    return Err(AgentContextError::new(
                        "native group repeats an unanswered declaration identity",
                    ));
                }
            }
            let result = match &native {
                ProviderTranscriptEvent::OpenAiFunctionCallOutput { call_id, .. } => Some(call_id),
                ProviderTranscriptEvent::OpenAiChatCompletionsToolResult {
                    tool_call_id, ..
                }
                | ProviderTranscriptEvent::DeepSeekToolResult { tool_call_id, .. } => {
                    Some(tool_call_id)
                }
                _ => None,
            };
            if let Some(result) = result
                && state.pending.remove(result).is_none()
            {
                return Err(AgentContextError::new(
                    "native group result has no unanswered declaration",
                ));
            }
        }
        if owners.values().any(|state| !state.pending.is_empty()) {
            return Err(AgentContextError::new(
                "native group still has unanswered declarations",
            ));
        }
        Ok(())
    }
}
