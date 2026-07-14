//! OpenAI request rendering and prompt-cache diagnostics.
//!
//! This module owns the OpenAI-specific conversion from Mezzanine model
//! messages into Responses API `instructions` and `input` material. It also
//! computes non-model-visible prompt-cache fingerprints used for diagnostics.

use super::OPENAI_MAAP_FUNCTION_TOOL_NAME;
use super::openai_request::openai_responses_request_control_shape_with_stream;
use super::schema::openai_maap_action_batch_tools;
#[cfg(test)]
use crate::agent::{ContextSourceKind, ModelMessage, ModelMessageRole};
use crate::agent::{ModelInteractionKind, ModelRequest};
#[cfg(test)]
use mez_agent::openai_stable_prefix_material;
use mez_agent::{
    OpenAiPromptCacheDiagnostics, OpenAiRenderedMessages, ProviderRequestAssemblyResult,
    openai_allowed_action_surface_message, openai_auto_sizing_response_format,
    openai_macro_judge_response_format, openai_prompt_cache_diagnostics,
    openai_prompt_cache_key as provider_prompt_cache_key, openai_render_messages,
    validate_provider_request_required,
};

/// Renders request messages and captures canonical stable-prefix material.
pub(super) fn openai_render_request_messages(
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<OpenAiRenderedMessages> {
    let appended_message =
        openai_allowed_action_surface_message(request.interaction_kind, &request.allowed_actions);
    openai_render_messages(&request.messages, appended_message.as_ref())
}

/// Returns the OpenAI response-format field for special request modes.
pub(super) fn openai_response_format(request: &ModelRequest) -> Option<serde_json::Value> {
    match request.interaction_kind {
        ModelInteractionKind::AutoSizing => Some(openai_auto_sizing_response_format()),
        ModelInteractionKind::MacroJudge => Some(openai_macro_judge_response_format()),
        _ => None,
    }
}

/// Builds a stable, non-secret OpenAI prompt-cache routing key for a request.
pub(super) fn openai_prompt_cache_key(request: &ModelRequest) -> String {
    provider_prompt_cache_key(
        &request.provider,
        request.prompt_cache_lineage_id.as_deref(),
    )
}

/// Returns non-model-visible OpenAI prompt-cache diagnostics for one request.
pub fn openai_prompt_cache_diagnostics_for_request(
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<OpenAiPromptCacheDiagnostics> {
    openai_prompt_cache_diagnostics_for_request_with_stream(request, false)
}

/// Returns non-model-visible OpenAI prompt-cache diagnostics for one request and stream mode.
pub fn openai_prompt_cache_diagnostics_for_request_with_stream(
    request: &ModelRequest,
    stream: bool,
) -> ProviderRequestAssemblyResult<OpenAiPromptCacheDiagnostics> {
    validate_provider_request_required("OpenAI model", &request.model)?;
    let rendered = openai_render_request_messages(request)?;
    let response_format = openai_response_format(request).unwrap_or(serde_json::Value::Null);
    let tools = if request.interaction_kind.expects_structured_json() {
        serde_json::json!([])
    } else {
        serde_json::json!(openai_maap_action_batch_tools(request))
    };
    let tool_choice = if request.interaction_kind.expects_structured_json() {
        serde_json::json!("none")
    } else {
        serde_json::json!({
            "type": "function",
            "name": OPENAI_MAAP_FUNCTION_TOOL_NAME
        })
    };
    let provider_request_shape =
        openai_responses_request_control_shape_with_stream(request, stream)?;
    openai_prompt_cache_diagnostics(
        openai_prompt_cache_key(request),
        &rendered,
        &response_format,
        &tools,
        &tool_choice,
        &provider_request_shape,
    )
}

/// Returns canonical OpenAI stable-prefix material for tests and diagnostics.
#[cfg(test)]
pub(crate) fn openai_stable_prefix_material_for_request(
    request: &ModelRequest,
) -> ProviderRequestAssemblyResult<String> {
    let rendered = openai_render_request_messages(request)?;
    openai_stable_prefix_material(&rendered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AllowedActionSet, ProviderTranscriptEvent};

    /// Verifies OpenAI request rendering ignores hidden provider-native
    /// transcript events.
    ///
    /// DeepSeek can persist hidden replay metadata into shared transcript
    /// history. If a later request is routed through OpenAI, that metadata must
    /// not become an instruction or input message because OpenAI does not
    /// understand DeepSeek `reasoning_content` or Chat Completions tool-call
    /// replay fields.
    #[test]
    fn openai_rendering_omits_hidden_provider_transcript_events() {
        let event = ProviderTranscriptEvent::DeepSeekAssistantToolCall {
            content: "".to_string(),
            reasoning_content: Some("DeepSeek-only reasoning".to_string()),
            tool_calls: vec![serde_json::json!({
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "submit_maap_action_batch",
                    "arguments": "{}"
                }
            })],
        };
        let request = ModelRequest {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: None,
            temperature: None,
            stop: None,
            prompt_cache_session_id: None,
            prompt_cache_lineage_id: None,
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: false,
            issue_actions_enabled: true,
            interaction_kind: ModelInteractionKind::CapabilityDecision,
            allowed_actions: AllowedActionSet::capability_decision(),
            messages: vec![
                ModelMessage {
                    role: ModelMessageRole::System,
                    source: ContextSourceKind::System,
                    content: "system prompt".to_string(),
                },
                ModelMessage {
                    role: ModelMessageRole::System,
                    source: ContextSourceKind::Transcript,
                    content: event.to_transcript_content(),
                },
                ModelMessage {
                    role: ModelMessageRole::User,
                    source: ContextSourceKind::UserInstruction,
                    content: "continue".to_string(),
                },
            ],
        };

        let rendered = openai_render_request_messages(&request).unwrap();
        let rendered_json = serde_json::to_string(&rendered.input).unwrap();

        assert_eq!(rendered.input.len(), 2);
        assert!(rendered.instructions.contains("system prompt"));
        assert!(rendered_json.contains("continue"));
        assert!(!rendered.instructions.contains("DeepSeek-only reasoning"));
        assert!(!rendered_json.contains("reasoning_content"));
        assert!(!rendered_json.contains("call_1"));
    }
}
