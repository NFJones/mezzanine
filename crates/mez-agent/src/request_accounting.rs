//! Provider-independent request-input estimation.
//!
//! This module estimates the input size of the canonical provider wire body
//! after messages, request-local recovery input, action schemas, MCP schemas,
//! response wrappers, and provider controls have been rendered. The estimate
//! is deliberately deterministic rather than tokenizer-specific so product
//! scheduling can enforce an explicitly configured input cap before I/O while
//! retaining provider context-limit responses as the authoritative fallback.

use std::collections::BTreeMap;

use crate::{
    AnthropicMessagesOptions, ModelRequest, OpenAiChatCompletionsOptions, ProviderApiCompatibility,
    ProviderRequestAssemblyResult, anthropic_messages_request_body,
    deepseek_chat_completions_request_body_with_strategy, deepseek_effective_stream,
    deepseek_maap_request_strategy, openai_chat_completions_request_body_with_stream,
    openai_responses_request_body_with_stream,
};

/// Conservative number of canonical wire bytes represented by one estimated
/// provider input token.
const PROVIDER_REQUEST_ESTIMATED_BYTES_PER_TOKEN: usize = 4;

/// Estimates provider-visible tokens for one UTF-8 text fragment using the
/// same deterministic approximation as complete canonical request bodies.
pub fn provider_text_input_token_estimate(text: &str) -> usize {
    text.len()
        .saturating_add(PROVIDER_REQUEST_ESTIMATED_BYTES_PER_TOKEN - 1)
        .saturating_div(PROVIDER_REQUEST_ESTIMATED_BYTES_PER_TOKEN)
}

/// Complete canonical provider-request input estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRequestInputEstimate {
    /// Bytes in the exact canonical JSON body submitted by the provider adapter.
    pub wire_bytes: usize,
    /// Deterministic estimated provider-visible input tokens.
    pub input_tokens: usize,
}

impl ProviderRequestInputEstimate {
    /// Returns whether this estimate exceeds one explicit positive input cap.
    pub fn exceeds_explicit_cap(self, max_input_tokens: usize) -> bool {
        max_input_tokens > 0 && self.input_tokens > max_input_tokens
    }
}

/// Estimates one fully rendered provider request before transport I/O.
///
/// `provider_options` must be the non-secret options for the selected provider,
/// and `stream` must match the transport mode. The provider-specific body
/// builders remain the single source of truth for request schemas and wrappers.
pub fn provider_request_input_estimate(
    request: &ModelRequest,
    api: ProviderApiCompatibility,
    provider_options: &BTreeMap<String, String>,
    stream: bool,
) -> ProviderRequestAssemblyResult<ProviderRequestInputEstimate> {
    let wire_body = match api {
        ProviderApiCompatibility::OpenAiResponses => {
            openai_responses_request_body_with_stream(request, stream)?
        }
        ProviderApiCompatibility::OpenAiChatCompletions => {
            let options = OpenAiChatCompletionsOptions::from_provider_options(provider_options)?;
            openai_chat_completions_request_body_with_stream(
                request,
                options,
                stream && options.streaming_enabled(),
            )?
        }
        ProviderApiCompatibility::DeepSeekChatCompletions => {
            let strategy = deepseek_maap_request_strategy(request);
            deepseek_chat_completions_request_body_with_strategy(
                request,
                deepseek_effective_stream(stream, strategy),
                strategy,
            )?
        }
        ProviderApiCompatibility::AnthropicMessages => {
            let options = AnthropicMessagesOptions::from_provider_options(provider_options)?;
            anthropic_messages_request_body(request, stream, &options)?
        }
    };
    let wire_bytes = wire_body.len();
    let input_tokens = provider_text_input_token_estimate(&wire_body).max(1);
    Ok(ProviderRequestInputEstimate {
        wire_bytes,
        input_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AllowedActionSet, ContextPlacement, ContextSourceKind, ModelInteractionKind, ModelMessage,
        ModelMessageRole,
    };

    /// Builds a request whose canonical bodies include system/user messages,
    /// action schemas, and request-state wrappers.
    fn complete_test_request(provider: &str) -> ModelRequest {
        ModelRequest {
            provider: provider.to_string(),
            model: "test-model".to_string(),
            reasoning_effort: None,
            thinking_enabled: None,
            latency_preference: None,
            prompt_cache_retention: None,
            max_output_tokens: Some(512),
            temperature: None,
            prompt_cache_session_id: Some("session-1".to_string()),
            prompt_cache_lineage_id: Some("lineage-1".to_string()),
            turn_id: "turn-1".to_string(),
            agent_id: "agent-1".to_string(),
            available_mcp_tools: Vec::new(),
            memory_actions_enabled: true,
            issue_actions_enabled: true,
            interaction_kind: ModelInteractionKind::ActionExecution,
            allowed_actions: AllowedActionSet::say_only(),
            stop: None,
            messages: vec![
                ModelMessage {
                    role: ModelMessageRole::System,
                    source: ContextSourceKind::System,
                    placement: ContextPlacement::StablePrefix,
                    content: "system authority and complete response contract".to_string(),
                },
                ModelMessage {
                    role: ModelMessageRole::User,
                    source: ContextSourceKind::UserInstruction,
                    placement: ContextPlacement::ConversationAppend,
                    content: "return one schema-valid action batch".to_string(),
                },
            ]
            .into(),
        }
    }

    #[test]
    /// Verifies complete canonical request estimation has an exact inclusive
    /// cap boundary: equality dispatches while one token below fails closed.
    fn explicit_input_cap_boundary_is_inclusive() {
        let request = complete_test_request("openai");
        let estimate = provider_request_input_estimate(
            &request,
            ProviderApiCompatibility::OpenAiResponses,
            &BTreeMap::new(),
            true,
        )
        .unwrap();

        assert!(estimate.wire_bytes > 0);
        assert!(!estimate.exceeds_explicit_cap(estimate.input_tokens));
        assert!(estimate.exceeds_explicit_cap(estimate.input_tokens - 1));
    }

    #[test]
    /// Verifies every supported provider API estimates its canonical JSON body.
    fn complete_wire_estimation_covers_every_provider_api() {
        let cases = [
            ("openai", ProviderApiCompatibility::OpenAiResponses),
            (
                "openai-compatible",
                ProviderApiCompatibility::OpenAiChatCompletions,
            ),
            (
                "deepseek",
                ProviderApiCompatibility::DeepSeekChatCompletions,
            ),
            ("anthropic", ProviderApiCompatibility::AnthropicMessages),
        ];

        for (provider, api) in cases {
            let complete = complete_test_request(provider);
            let complete_estimate =
                provider_request_input_estimate(&complete, api, &BTreeMap::new(), true).unwrap();

            assert!(complete_estimate.wire_bytes > 0);
            assert!(complete_estimate.input_tokens > 0);
        }
    }

    /// Verifies generic Chat Completions accounting renders the configured
    /// streaming request body rather than estimating the legacy unary shape.
    #[test]
    fn generic_chat_accounting_uses_opt_in_stream_mode() {
        let request = complete_test_request("openai-compatible");
        let provider_options = BTreeMap::from([("streaming".to_string(), "enabled".to_string())]);
        let options =
            OpenAiChatCompletionsOptions::from_provider_options(&provider_options).unwrap();
        let expected =
            openai_chat_completions_request_body_with_stream(&request, options, true).unwrap();

        let estimate = provider_request_input_estimate(
            &request,
            ProviderApiCompatibility::OpenAiChatCompletions,
            &provider_options,
            true,
        )
        .unwrap();

        assert_eq!(estimate.wire_bytes, expected.len());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&expected).unwrap()["stream"],
            true
        );
    }
}
