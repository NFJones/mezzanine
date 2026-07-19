//! Agent tests for provider contract behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

#[test]
/// Verifies model provider trait returns model response.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn model_provider_trait_returns_model_response() {
    let request = assemble_model_request(
        &ModelProfile {
            provider: "echo".to_string(),
            model: "test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "user".to_string(),
            content: "hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();

    let response = EchoProvider.send_request(&request).unwrap();

    assert_eq!(response.provider, "echo");
    assert_eq!(response.model, "test");
    assert_eq!(response.raw_text, "ok");
}

#[test]
/// Verifies every provider projection preserves canonical chronology and keeps
/// controller context distinguishable from direct user speech.
///
/// The same uniquely marked request is rendered through OpenAI Responses,
/// OpenAI Chat, Anthropic, and DeepSeek. Any adapter that moves the
/// user prompt after its evidence or relabels neutral state as user-authored
/// will fail this shared request-shape contract.
fn provider_projection_matrix_preserves_chronology_and_neutral_authorship() {
    let request = ModelRequest {
        provider: "provider-matrix".to_string(),
        model: "model-matrix".to_string(),
        reasoning_effort: None,
        thinking_enabled: None,
        latency_preference: None,
        prompt_cache_retention: None,
        max_output_tokens: None,
        temperature: None,
        prompt_cache_session_id: None,
        prompt_cache_lineage_id: None,
        turn_id: "turn-provider-matrix".to_string(),
        agent_id: "agent-provider-matrix".to_string(),
        available_mcp_tools: Vec::new(),
        memory_actions_enabled: false,
        issue_actions_enabled: false,
        interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
        allowed_actions: mez_agent::AllowedActionSet::say_only(),
        stop: None,
        messages: vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                placement: mez_agent::ContextPlacement::StablePrefix,
                content: "SYSTEM_AUTHORITY_MARKER".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::UserInstruction,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                content: "USER_PROMPT_MARKER".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Assistant,
                source: ContextSourceKind::TranscriptAssistant,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                content: "ASSISTANT_ACTION_MARKER".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Tool,
                source: ContextSourceKind::ActionResult,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                content: "ACTION_RESULT_MARKER".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Context,
                source: ContextSourceKind::Memory,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                content: "NEUTRAL_REFERENCE_MARKER".to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Context,
                source: ContextSourceKind::RuntimeHint,
                placement: mez_agent::ContextPlacement::EphemeralTail,
                content: "LIVE_STATE_MARKER".to_string(),
            },
        ],
    };

    let rendered = [
        (
            "openai-responses",
            mez_agent::openai_responses_request_body(&request).unwrap(),
        ),
        (
            "openai-chat",
            mez_agent::openai_chat_completions_request_body(
                &request,
                mez_agent::OpenAiChatCompletionsOptions::default(),
            )
            .unwrap(),
        ),
        (
            "anthropic",
            mez_agent::anthropic_messages_request_body(
                &request,
                false,
                &mez_agent::AnthropicMessagesOptions::default(),
            )
            .unwrap(),
        ),
        (
            "deepseek",
            mez_agent::deepseek_chat_completions_request_body_with_strategy(
                &request,
                false,
                mez_agent::DeepSeekMaapRequestStrategy::NoTool,
            )
            .unwrap(),
        ),
    ];

    for (provider, body) in rendered {
        let markers = [
            "USER_PROMPT_MARKER",
            "ASSISTANT_ACTION_MARKER",
            "ACTION_RESULT_MARKER",
            "NEUTRAL_REFERENCE_MARKER",
            "LIVE_STATE_MARKER",
        ];
        let positions = markers
            .iter()
            .map(|marker| {
                body.find(marker)
                    .unwrap_or_else(|| panic!("{provider} omitted {marker}: {body}"))
            })
            .collect::<Vec<_>>();
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "{provider} reordered canonical context: {body}"
        );
        assert_eq!(
            body.matches("USER_PROMPT_MARKER").count(),
            1,
            "{provider} duplicated the user prompt: {body}"
        );
        assert!(
            body.contains("not user-authored"),
            "{provider} did not identify neutral context: {body}"
        );
    }
}

#[test]
/// Verifies the effective adapter payload switch matrix consumes DeepSeek
/// native continuity only for its owner and otherwise preserves the eligible
/// canonical subsequence around the omitted event.
///
/// DeepSeek is currently the sole provider with opaque native continuity. The
/// test assembles through the product boundary and then renders every concrete
/// adapter payload so a canonical-only fixture cannot hide a later lift into a
/// foreign system channel.
fn provider_effective_payloads_omit_foreign_native_events_without_reordering() {
    let native_marker = "DEEPSEEK_NATIVE_CONTINUITY_MARKER";
    let native_event = mez_agent::ProviderTranscriptEvent::DeepSeekAssistantToolCall {
        content: native_marker.to_string(),
        reasoning_content: Some("DEEPSEEK_NATIVE_REASONING_MARKER".to_string()),
        tool_calls: vec![serde_json::json!({
            "id": "call_native_matrix",
            "type": "function",
            "function": {"name": "submit_maap_action_batch", "arguments": "{}"}
        })],
    }
    .to_transcript_content();
    let context = AgentContext::new(vec![
        ContextBlock::user_event("user prompt", "SWITCH_PROMPT_MARKER"),
        ContextBlock {
            source: ContextSourceKind::Transcript,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "deepseek native continuity".to_string(),
            content: native_event,
        },
        ContextBlock::reference_event(
            ContextSourceKind::Memory,
            "post-switch reference",
            "SWITCH_REFERENCE_MARKER",
        ),
    ])
    .unwrap();
    let profile = |provider: &str| ModelProfile {
        provider: provider.to_string(),
        model: "test-model".to_string(),
        reasoning_profile: None,
        latency_preference: None,
        multimodal_required: false,
        provider_options: std::collections::BTreeMap::new(),
        safety_tier: None,
    };

    for provider in ["openai", "openai-chat", "anthropic", "deepseek"] {
        let request = assemble_model_request(&profile(provider), &turn(), &context).unwrap();
        let body = match provider {
            "openai" => mez_agent::openai_responses_request_body(&request).unwrap(),
            "openai-chat" => mez_agent::openai_chat_completions_request_body(
                &request,
                mez_agent::OpenAiChatCompletionsOptions::default(),
            )
            .unwrap(),
            "anthropic" => mez_agent::anthropic_messages_request_body(
                &request,
                false,
                &mez_agent::AnthropicMessagesOptions::default(),
            )
            .unwrap(),
            "deepseek" => mez_agent::deepseek_chat_completions_request_body_with_strategy(
                &request,
                false,
                mez_agent::DeepSeekMaapRequestStrategy::NoTool,
            )
            .unwrap(),
            _ => unreachable!(),
        };
        let prompt = body.find("SWITCH_PROMPT_MARKER").unwrap();
        let reference = body.find("SWITCH_REFERENCE_MARKER").unwrap();
        assert!(
            prompt < reference,
            "{provider} reordered eligible events: {body}"
        );
        if provider == "deepseek" {
            let native = body.find(native_marker).unwrap();
            assert!(
                prompt < native && native < reference,
                "DeepSeek moved its native continuity event: {body}"
            );
        } else {
            assert!(
                !body.contains(native_marker)
                    && !body.contains("DEEPSEEK_NATIVE_REASONING_MARKER")
                    && !body.contains("call_native_matrix"),
                "{provider} received foreign DeepSeek continuity: {body}"
            );
        }
    }
}
