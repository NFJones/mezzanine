//! Provider-independent model-request assembly from canonical context.
//!
//! This module owns context-to-message projection, repository-guidance
//! placement, provider transcript preservation, prompt
//! typed cache identity, default action surfaces, and provider-specific
//! request defaults. Product code supplies stable turn identity and prompt
//! assets without exposing runtime records or filesystem access.

use std::collections::BTreeSet;

#[cfg(test)]
use crate::ProviderTranscriptEvent;
use crate::{
    AgentContext, AgentPromptAssetSource, AgentPromptProfile, AgentRequestAssemblyResult,
    AllowedActionSet, ContextBlock, ContextPlacement, ContextSourceKind, ModelInteractionKind,
    ModelMessage, ModelMessageRole, ModelProfile, ModelRequest, ProviderApiCompatibility,
    assemble_agent_system_prompt, constrain_skill_actions_for_loaded_context,
    model_context_block_header, validate_context_placement_order, validate_context_semantics,
    validate_model_profile_request,
};

/// Stable product identity required to assemble one provider request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelRequestIdentity<'a> {
    /// Active turn identifier.
    pub turn_id: &'a str,
    /// Active agent identifier.
    pub agent_id: &'a str,
    /// Pane identifier used by the prompt profile.
    pub pane_id: &'a str,
}

/// Assembles one complete provider request from canonical model context.
pub fn assemble_model_request_from_context(
    profile: &ModelProfile,
    identity: ModelRequestIdentity<'_>,
    context: &AgentContext,
    prompt_assets: &impl AgentPromptAssetSource,
) -> AgentRequestAssemblyResult<ModelRequest> {
    assemble_model_request_from_context_for_api(
        profile,
        ProviderApiCompatibility::default_for_kind(&profile.provider),
        identity,
        context,
        prompt_assets,
    )
}

/// Assembles one provider request with explicit wire API compatibility.
///
/// Provider-native continuity is replayed only when both this API and the
/// configured provider id exactly match its durable owner.
pub fn assemble_model_request_from_context_with_api(
    profile: &ModelProfile,
    api: ProviderApiCompatibility,
    identity: ModelRequestIdentity<'_>,
    context: &AgentContext,
    prompt_assets: &impl AgentPromptAssetSource,
) -> AgentRequestAssemblyResult<ModelRequest> {
    assemble_model_request_from_context_for_api(
        profile,
        Some(api),
        identity,
        context,
        prompt_assets,
    )
}

/// Assembles one provider request when no wire API compatibility is known.
///
/// Native continuity is never replayed through this entry point, even when a
/// configured provider id happens to equal a built-in provider kind.
pub fn assemble_model_request_from_context_api_unknown(
    profile: &ModelProfile,
    identity: ModelRequestIdentity<'_>,
    context: &AgentContext,
    prompt_assets: &impl AgentPromptAssetSource,
) -> AgentRequestAssemblyResult<ModelRequest> {
    assemble_model_request_from_context_for_api(profile, None, identity, context, prompt_assets)
}

/// Implements request assembly with an optional proven API compatibility.
fn assemble_model_request_from_context_for_api(
    profile: &ModelProfile,
    api: Option<ProviderApiCompatibility>,
    identity: ModelRequestIdentity<'_>,
    context: &AgentContext,
    prompt_assets: &impl AgentPromptAssetSource,
) -> AgentRequestAssemblyResult<ModelRequest> {
    validate_model_profile_request(profile, identity.turn_id)?;
    validate_context_placement_order(context.blocks())?;
    validate_context_semantics(context.blocks())?;

    let blocks = context.blocks();
    let is_deepseek = api == Some(ProviderApiCompatibility::DeepSeekChatCompletions);
    let provider_native_execution_groups = blocks
        .iter()
        .enumerate()
        .filter_map(|(index, _)| context.metadata_for_block(index))
        .filter(|metadata| {
            metadata.provider_owner().is_some_and(|owner| {
                api.is_some_and(|api| owner.matches_provider(api, &profile.provider))
            })
        })
        .filter_map(|metadata| metadata.execution_group_id().cloned())
        .filter(|group| {
            api != Some(ProviderApiCompatibility::OpenAiChatCompletions)
                || openai_chat_completions_native_group_is_complete(
                    context,
                    group,
                    &profile.provider,
                )
        })
        .collect::<BTreeSet<_>>();
    let prompt_profile = AgentPromptProfile::for_model(&profile.model);
    let mut messages = Vec::with_capacity(blocks.len() + 1);
    messages.push(ModelMessage {
        role: ModelMessageRole::System,
        source: ContextSourceKind::System,
        placement: ContextPlacement::StablePrefix,
        content: assemble_agent_system_prompt(&prompt_profile, &[], prompt_assets)?,
    });
    for (index, block) in blocks.iter().enumerate() {
        let metadata = context.metadata_for_block(index).ok_or_else(|| {
            crate::AgentRequestAssemblyError::from(crate::AgentContextError::new(
                "context block is missing stored causal metadata",
            ))
        })?;
        if let Some(owner) = metadata.provider_owner() {
            if !api.is_some_and(|api| owner.matches_provider(api, &profile.provider)) {
                continue;
            }
            messages.push(ModelMessage {
                role: ModelMessageRole::System,
                source: block.source,
                placement: block.placement,
                content: block.content.clone(),
            });
            continue;
        }
        if metadata
            .execution_group_id()
            .is_some_and(|group| provider_native_execution_groups.contains(group))
            && matches!(
                block.source,
                ContextSourceKind::TranscriptAssistant | ContextSourceKind::ActionResult
            )
        {
            continue;
        }
        messages.push(ModelMessage {
            role: role_for_context_semantic(block, metadata.semantic_kind()),
            source: block.source,
            placement: block.placement,
            content: format!("{}{}", model_context_block_header(block), block.content),
        });
    }
    let mut request = ModelRequest {
        provider: profile.provider.clone(),
        model: profile.model.clone(),
        model_capabilities: api
            .map(|api| profile.model_capabilities.resolved_for_api(api))
            .unwrap_or_else(|| profile.model_capabilities.clone()),
        max_input_tokens: profile.max_input_tokens(),
        reasoning_effort: profile
            .reasoning_profile
            .clone()
            .or_else(|| profile.provider_options.get("reasoning_effort").cloned()),
        thinking_enabled: profile.thinking_enabled(),
        latency_preference: profile.latency_preference.clone(),
        prompt_cache_retention: profile
            .provider_options
            .get("prompt_cache_retention")
            .cloned(),
        max_output_tokens: profile.max_output_tokens(),
        temperature: profile
            .temperature()
            .map(|value| value.to_string())
            .or_else(|| {
                if is_deepseek {
                    Some("0.5".to_string())
                } else {
                    None
                }
            }),
        prompt_cache_session_id: context.metadata().prompt_cache_session_id.clone(),
        prompt_cache_lineage_id: context.metadata().prompt_cache_lineage_id.clone(),
        turn_id: identity.turn_id.to_string(),
        agent_id: identity.agent_id.to_string(),
        available_mcp_tools: Vec::new(),
        memory_actions_enabled: profile
            .provider_options
            .get("memory_actions_enabled")
            .is_some_and(|value| value == "true"),
        issue_actions_enabled: profile
            .provider_options
            .get("issue_actions_enabled")
            .is_none_or(|value| value != "false"),
        interaction_kind: ModelInteractionKind::ActionExecution,
        allowed_actions: AllowedActionSet::all_enabled(),
        stop: is_deepseek.then(|| vec!["\n}".to_string()]),
        messages: messages.into(),
    };
    constrain_skill_actions_for_loaded_context(&mut request);
    Ok(request)
}

/// Reports whether one generic Chat Completions execution group contains one
/// complete assistant-call/result chain in declaration order.
///
/// Neutral assistant and action-result blocks are suppressed only after this
/// check succeeds. Partial, duplicated, reordered, or foreign native events
/// therefore fail closed to the provider-neutral projection.
fn openai_chat_completions_native_group_is_complete(
    context: &AgentContext,
    group: &crate::ContextExecutionGroupId,
    provider_id: &str,
) -> bool {
    let mut expected_ids = None;
    let mut result_ids = Vec::new();
    for (index, block) in context.blocks().iter().enumerate() {
        let Some(metadata) = context.metadata_for_block(index) else {
            return false;
        };
        if metadata.execution_group_id() != Some(group) || metadata.provider_owner().is_none() {
            continue;
        }
        let Some(event) = crate::ProviderTranscriptEvent::from_transcript_content(&block.content)
        else {
            return false;
        };
        match event {
            crate::ProviderTranscriptEvent::OpenAiChatCompletionsAssistantToolCall {
                provider_id: event_provider_id,
                tool_calls,
                ..
            } if event_provider_id == provider_id && expected_ids.is_none() => {
                expected_ids = Some(
                    tool_calls
                        .iter()
                        .filter_map(|call| call.get("id").and_then(serde_json::Value::as_str))
                        .map(str::to_string)
                        .collect::<Vec<_>>(),
                );
            }
            crate::ProviderTranscriptEvent::OpenAiChatCompletionsToolResult {
                provider_id: event_provider_id,
                tool_call_id,
                ..
            } if event_provider_id == provider_id && expected_ids.is_some() => {
                result_ids.push(tool_call_id);
            }
            _ => return false,
        }
    }
    expected_ids.is_some_and(|expected_ids| expected_ids == result_ids)
}

/// Maps canonical context semantics to provider-neutral message roles.
pub fn role_for_context_block(block: &ContextBlock) -> ModelMessageRole {
    role_for_context_semantic(block, block.semantic_kind())
}

/// Maps one producer-selected canonical semantic to a provider-neutral role.
fn role_for_context_semantic(
    block: &ContextBlock,
    semantic_kind: crate::ContextSemanticKind,
) -> ModelMessageRole {
    match semantic_kind {
        crate::ContextSemanticKind::AmbientInstruction => {
            if block.source == ContextSourceKind::System {
                ModelMessageRole::System
            } else if block.source == ContextSourceKind::PersistedContextDocument {
                ModelMessageRole::Context
            } else {
                ModelMessageRole::Developer
            }
        }
        crate::ContextSemanticKind::UserEvent => ModelMessageRole::User,
        crate::ContextSemanticKind::AssistantEvent => ModelMessageRole::Assistant,
        crate::ContextSemanticKind::EvidenceEvent
            if matches!(
                block.source,
                ContextSourceKind::ActionResult | ContextSourceKind::TranscriptTool
            ) =>
        {
            ModelMessageRole::Tool
        }
        crate::ContextSemanticKind::TaskPrelude
        | crate::ContextSemanticKind::EvidenceEvent
        | crate::ContextSemanticKind::ReferenceEvent => ModelMessageRole::Context,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic prompt source for assembly tests that do not require product
    /// embedded Markdown assets.
    struct TestPromptAssets;

    impl AgentPromptAssetSource for TestPromptAssets {
        fn system_fragment<'a>(&'a self, path: &str) -> crate::AgentPromptResult<&'a str> {
            Ok(match path {
                "identity.md" => "profile {profile_name} version {profile_version}",
                "repository_instructions.md" => "repository contract",
                "subagents.md" => "subagent contract",
                "mcp.md" => "mcp contract",
                _ => "generic contract",
            })
        }

        fn provider_fragment<'a>(&'a self, _path: &str) -> crate::AgentPromptResult<&'a str> {
            Ok("provider contract")
        }
    }

    /// Verifies lower request assembly preserves hidden provider events and
    /// carries typed cache identity without projecting it as model text.
    #[test]
    fn model_request_assembly_preserves_typed_metadata_and_provider_events() {
        let event = ProviderTranscriptEvent::DeepSeekToolResult {
            tool_call_id: "call-1".to_string(),
            content: "result".to_string(),
        }
        .to_transcript_content();
        let context = AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::Transcript,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "provider event".to_string(),
                content: event.clone(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "user".to_string(),
                content: "continue".to_string(),
            },
        ])
        .unwrap()
        .with_metadata(crate::ModelContextMetadata::new(
            Some("session-1"),
            Some("lineage-1"),
        ));
        let request = assemble_model_request_from_context(
            &model_profile("deepseek"),
            ModelRequestIdentity {
                turn_id: "turn-1",
                agent_id: "agent-1",
                pane_id: "%1",
            },
            &context,
            &TestPromptAssets,
        )
        .unwrap();

        assert_eq!(
            request.prompt_cache_session_id.as_deref(),
            Some("session-1")
        );
        assert_eq!(
            request.prompt_cache_lineage_id.as_deref(),
            Some("lineage-1")
        );
        assert!(
            !request
                .messages
                .iter()
                .any(|message| message.content.contains("session-1")
                    || message.content.contains("lineage-1"))
        );
        assert!(
            request
                .messages
                .iter()
                .any(|message| message.content == event)
        );
        assert!(
            !request
                .messages
                .iter()
                .any(|message| message.content.contains("session_id="))
        );
    }

    /// Verifies provider-native replay replaces only the neutral assistant and
    /// result projection for its owning provider.
    ///
    /// The canonical context retains both representations so provider switches
    /// remain possible. DeepSeek receives one valid adjacent native tool-call
    /// pair, while another provider receives the neutral causal history and no
    /// DeepSeek-only payload.
    #[test]
    fn model_request_assembly_selects_native_or_neutral_execution_projection() {
        let mut context = AgentContext::new(vec![ContextBlock::user_event(
            "user",
            "inspect the issue backlog",
        )])
        .unwrap();
        let group = crate::ContextExecutionGroupId::new("provider-execution-1").unwrap();
        context
            .append_assistant_event(
                "assistant",
                "rationale: inspect issues\naction query-1: issue_query",
                group.clone(),
            )
            .unwrap();
        let assistant_native = ProviderTranscriptEvent::DeepSeekAssistantToolCall {
            content: String::new(),
            reasoning_content: Some("inspect issues".to_string()),
            tool_calls: vec![serde_json::json!({
                "id": "call-1",
                "type": "function",
                "function": {
                    "name": "submit_maap_action_batch",
                    "arguments": "{}"
                }
            })],
        }
        .to_transcript_content();
        context
            .append_evidence_event(
                ContextSourceKind::TranscriptTool,
                "native assistant",
                assistant_native.clone(),
                group.clone(),
                Some(
                    crate::ProviderContinuityOwner::new(
                        ProviderApiCompatibility::DeepSeekChatCompletions,
                        "deepseek",
                    )
                    .unwrap(),
                ),
                true,
            )
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "action result query-1",
                "[action_result query-1 issue_query succeeded]",
                group.clone(),
                None,
                true,
            )
            .unwrap();
        let tool_native = ProviderTranscriptEvent::DeepSeekToolResult {
            tool_call_id: "call-1".to_string(),
            content: "[action_result query-1 issue_query succeeded]".to_string(),
        }
        .to_transcript_content();
        context
            .append_evidence_event(
                ContextSourceKind::TranscriptTool,
                "native tool result",
                tool_native.clone(),
                group,
                Some(
                    crate::ProviderContinuityOwner::new(
                        ProviderApiCompatibility::DeepSeekChatCompletions,
                        "deepseek",
                    )
                    .unwrap(),
                ),
                true,
            )
            .unwrap();

        let deepseek = assemble_model_request_from_context(
            &model_profile("deepseek"),
            ModelRequestIdentity {
                turn_id: "turn-1",
                agent_id: "agent-1",
                pane_id: "%1",
            },
            &context,
            &TestPromptAssets,
        )
        .unwrap();
        assert!(
            deepseek
                .messages
                .iter()
                .any(|message| message.content == assistant_native)
        );
        assert!(
            deepseek
                .messages
                .iter()
                .any(|message| message.content == tool_native)
        );
        assert!(!deepseek.messages.iter().any(|message| {
            message.content.contains("rationale: inspect issues")
                || message
                    .content
                    .contains("[action_result query-1 issue_query succeeded]")
                    && !message
                        .content
                        .starts_with(crate::PROVIDER_TRANSCRIPT_EVENT_MARKER)
        }));

        let openai = assemble_model_request_from_context(
            &model_profile("openai"),
            ModelRequestIdentity {
                turn_id: "turn-1",
                agent_id: "agent-1",
                pane_id: "%1",
            },
            &context,
            &TestPromptAssets,
        )
        .unwrap();
        assert!(
            openai
                .messages
                .iter()
                .any(|message| message.content.contains("rationale: inspect issues"))
        );
        assert!(openai.messages.iter().any(|message| {
            message
                .content
                .contains("[action_result query-1 issue_query succeeded]")
        }));
        assert!(openai.messages.iter().all(|message| {
            !message
                .content
                .starts_with(crate::PROVIDER_TRANSCRIPT_EVENT_MARKER)
        }));
    }

    /// Verifies exact continuity ownership selects native replay only when the
    /// configured provider id and API compatibility both match.
    #[test]
    fn model_request_assembly_requires_both_exact_owner_dimensions() {
        let native = ProviderTranscriptEvent::validated_openai_response_output(vec![
            serde_json::json!({"type":"reasoning","id":"reasoning-1"}),
        ])
        .unwrap()
        .to_transcript_content();
        let mut context = AgentContext::new(vec![ContextBlock::user_event(
            "user",
            "continue the exact provider execution",
        )])
        .unwrap();
        let group = crate::ContextExecutionGroupId::new("provider-execution-exact").unwrap();
        context
            .append_assistant_event("assistant", "neutral assistant fallback", group.clone())
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::TranscriptTool,
                "native response",
                native.clone(),
                group.clone(),
                Some(
                    crate::ProviderContinuityOwner::new(
                        ProviderApiCompatibility::OpenAiResponses,
                        "configured-openai",
                    )
                    .unwrap(),
                ),
                true,
            )
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "action result",
                "neutral result fallback",
                group,
                None,
                true,
            )
            .unwrap();

        for (api, provider, expects_native) in [
            (
                ProviderApiCompatibility::OpenAiResponses,
                "configured-openai",
                true,
            ),
            (
                ProviderApiCompatibility::OpenAiResponses,
                "other-openai",
                false,
            ),
            (
                ProviderApiCompatibility::OpenAiChatCompletions,
                "configured-openai",
                false,
            ),
        ] {
            let request = assemble_model_request_from_context_with_api(
                &model_profile(provider),
                api,
                ModelRequestIdentity {
                    turn_id: "turn-1",
                    agent_id: "agent-1",
                    pane_id: "%1",
                },
                &context,
                &TestPromptAssets,
            )
            .unwrap();
            assert_eq!(
                request
                    .messages
                    .iter()
                    .any(|message| message.content == native),
                expects_native
            );
            assert_eq!(
                request
                    .messages
                    .iter()
                    .any(|message| message.content.contains("neutral assistant fallback")),
                !expects_native
            );
            assert_eq!(
                request
                    .messages
                    .iter()
                    .any(|message| message.content.contains("neutral result fallback")),
                !expects_native
            );
            assert!(request.messages.iter().all(|message| {
                expects_native
                    || !message
                        .content
                        .starts_with(crate::PROVIDER_TRANSCRIPT_EVENT_MARKER)
            }));
        }
    }

    /// Verifies generic Chat Completions native state is selected only for the
    /// exact configured provider and API, with neutral history retained for
    /// provider-instance and API switches.
    #[test]
    fn model_request_assembly_scopes_generic_chat_continuity_to_exact_owner() {
        let provider = "configured-chat";
        let assistant_native =
            ProviderTranscriptEvent::validated_openai_chat_completions_assistant_tool_call(
                provider.to_string(),
                String::new(),
                vec![serde_json::json!({
                    "id": "call-chat-1",
                    "type": "function",
                    "function": {
                        "name": "submit_maap_action_batch",
                        "arguments": "{}"
                    }
                })],
            )
            .unwrap()
            .to_transcript_content();
        let result_native = ProviderTranscriptEvent::OpenAiChatCompletionsToolResult {
            provider_id: provider.to_string(),
            tool_call_id: "call-chat-1".to_string(),
            content: "native result".to_string(),
        }
        .to_transcript_content();
        let owner = crate::ProviderContinuityOwner::new(
            ProviderApiCompatibility::OpenAiChatCompletions,
            provider,
        )
        .unwrap();
        let mut context = AgentContext::new(vec![ContextBlock::user_event(
            "user",
            "continue the compatible execution",
        )])
        .unwrap();
        let group = crate::ContextExecutionGroupId::new("provider-execution-chat").unwrap();
        context
            .append_assistant_event("assistant", "neutral assistant", group.clone())
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::TranscriptTool,
                "native assistant",
                assistant_native.clone(),
                group.clone(),
                Some(owner.clone()),
                true,
            )
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "action result",
                "neutral result",
                group.clone(),
                None,
                true,
            )
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::TranscriptTool,
                "native result",
                result_native.clone(),
                group,
                Some(owner),
                true,
            )
            .unwrap();

        for (api, selected_provider, expects_native) in [
            (
                ProviderApiCompatibility::OpenAiChatCompletions,
                provider,
                true,
            ),
            (
                ProviderApiCompatibility::OpenAiChatCompletions,
                "other-chat",
                false,
            ),
            (ProviderApiCompatibility::OpenAiResponses, provider, false),
        ] {
            let request = assemble_model_request_from_context_with_api(
                &model_profile(selected_provider),
                api,
                ModelRequestIdentity {
                    turn_id: "turn-1",
                    agent_id: "agent-1",
                    pane_id: "%1",
                },
                &context,
                &TestPromptAssets,
            )
            .unwrap();
            assert_eq!(
                request.messages.iter().any(|message| {
                    message.content == assistant_native || message.content == result_native
                }),
                expects_native
            );
            assert_eq!(
                request.messages.iter().any(|message| {
                    message.content.contains("neutral assistant")
                        || message.content.contains("neutral result")
                }),
                !expects_native
            );
        }
    }

    /// Verifies API-unknown assembly stays neutral even when the configured
    /// provider id is literally the built-in `openai` identifier.
    #[test]
    fn model_request_api_unknown_keeps_literal_openai_projection_neutral() {
        let native = ProviderTranscriptEvent::validated_openai_response_output(vec![
            serde_json::json!({"type":"reasoning","id":"reasoning-unknown-api"}),
        ])
        .unwrap()
        .to_transcript_content();
        let mut context = AgentContext::new(vec![ContextBlock::user_event(
            "user",
            "continue without a proven API",
        )])
        .unwrap();
        let group = crate::ContextExecutionGroupId::new("provider-execution-unknown-api").unwrap();
        context
            .append_assistant_event("assistant", "neutral assistant fallback", group.clone())
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::TranscriptTool,
                "native response",
                native.clone(),
                group.clone(),
                Some(
                    crate::ProviderContinuityOwner::new(
                        ProviderApiCompatibility::OpenAiResponses,
                        "openai",
                    )
                    .unwrap(),
                ),
                true,
            )
            .unwrap();
        context
            .append_evidence_event(
                ContextSourceKind::ActionResult,
                "action result",
                "neutral result fallback",
                group,
                None,
                true,
            )
            .unwrap();

        let request = assemble_model_request_from_context_api_unknown(
            &model_profile("openai"),
            ModelRequestIdentity {
                turn_id: "turn-1",
                agent_id: "agent-1",
                pane_id: "%1",
            },
            &context,
            &TestPromptAssets,
        )
        .unwrap();

        assert!(
            request
                .messages
                .iter()
                .all(|message| message.content != native)
        );
        assert!(
            request
                .messages
                .iter()
                .any(|message| message.content.contains("neutral assistant fallback"))
        );
        assert!(
            request
                .messages
                .iter()
                .any(|message| message.content.contains("neutral result fallback"))
        );
    }

    /// Verifies durable transcript import reconstructs one provider execution
    /// group before selecting its provider-specific projection.
    ///
    /// Persistence stores the neutral assistant first, followed by the hidden
    /// native call/result pair and the generic action result. Compatibility
    /// import must group that contiguous sequence so a restored DeepSeek turn
    /// receives only the native pair, while a provider switch receives only
    /// the neutral assistant and generic result.
    #[test]
    fn restored_model_request_selects_one_complete_execution_projection() {
        let assistant_native = ProviderTranscriptEvent::DeepSeekAssistantToolCall {
            content: String::new(),
            reasoning_content: Some("inspect issues".to_string()),
            tool_calls: vec![serde_json::json!({
                "id": "call-1",
                "type": "function",
                "function": {
                    "name": "submit_maap_action_batch",
                    "arguments": "{}"
                }
            })],
        }
        .to_transcript_content();
        let tool_native = ProviderTranscriptEvent::DeepSeekToolResult {
            tool_call_id: "call-1".to_string(),
            content: "[action_result query-1 issue_query succeeded]".to_string(),
        }
        .to_transcript_content();
        let context = AgentContext::import_durable_blocks(vec![
            ContextBlock::user_event("user", "inspect the issue backlog"),
            ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                placement: ContextPlacement::ConversationAppend,
                label: "previous assistant".to_string(),
                content: "rationale: inspect issues\naction query-1: issue_query".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::TranscriptTool,
                placement: ContextPlacement::ConversationAppend,
                label: "native assistant".to_string(),
                content: assistant_native.clone(),
            },
            ContextBlock {
                source: ContextSourceKind::TranscriptTool,
                placement: ContextPlacement::ConversationAppend,
                label: "native tool result".to_string(),
                content: tool_native.clone(),
            },
            ContextBlock {
                source: ContextSourceKind::ActionResult,
                placement: ContextPlacement::ConversationAppend,
                label: "generic action result".to_string(),
                content: "[action_result query-1 issue_query succeeded]".to_string(),
            },
        ])
        .unwrap();

        let deepseek = assemble_model_request_from_context(
            &model_profile("deepseek"),
            ModelRequestIdentity {
                turn_id: "turn-1",
                agent_id: "agent-1",
                pane_id: "%1",
            },
            &context,
            &TestPromptAssets,
        )
        .unwrap();
        let native_messages = deepseek
            .messages
            .iter()
            .filter(|message| {
                message
                    .content
                    .starts_with(crate::PROVIDER_TRANSCRIPT_EVENT_MARKER)
            })
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            native_messages,
            vec![assistant_native.as_str(), tool_native.as_str()]
        );
        assert!(!deepseek.messages.iter().any(|message| {
            !message
                .content
                .starts_with(crate::PROVIDER_TRANSCRIPT_EVENT_MARKER)
                && (message.content.contains("rationale: inspect issues")
                    || message
                        .content
                        .contains("[action_result query-1 issue_query succeeded]"))
        }));

        let openai = assemble_model_request_from_context(
            &model_profile("openai"),
            ModelRequestIdentity {
                turn_id: "turn-1",
                agent_id: "agent-1",
                pane_id: "%1",
            },
            &context,
            &TestPromptAssets,
        )
        .unwrap();
        assert!(
            openai
                .messages
                .iter()
                .any(|message| message.content.contains("rationale: inspect issues"))
        );
        assert!(openai.messages.iter().any(|message| {
            message
                .content
                .contains("[action_result query-1 issue_query succeeded]")
        }));
        assert!(openai.messages.iter().all(|message| {
            !message
                .content
                .starts_with(crate::PROVIDER_TRANSCRIPT_EVENT_MARKER)
        }));
    }

    /// Builds one minimal profile for lower request-assembly tests.
    fn model_profile(provider: &str) -> ModelProfile {
        ModelProfile {
            provider: provider.to_string(),
            model: "test-model".to_string(),
            model_capabilities: Default::default(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        }
    }
}

#[cfg(test)]
#[path = "context_assembly/tests/policy.rs"]
mod policy_tests;
