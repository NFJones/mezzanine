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
    ModelMessage, ModelMessageRole, ModelProfile, ModelRequest, assemble_agent_system_prompt,
    constrain_skill_actions_for_loaded_context, model_context_block_header,
    validate_context_placement_order, validate_context_semantics, validate_model_profile_request,
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
    validate_model_profile_request(profile, identity.turn_id)?;
    validate_context_placement_order(context.blocks())?;
    validate_context_semantics(context.blocks())?;

    let blocks = context.blocks();
    let repository_instruction_blocks = blocks
        .iter()
        .filter(|block| block.source == ContextSourceKind::ProjectGuidance)
        .map(|block| block.content.clone())
        .collect::<Vec<_>>();
    let is_deepseek = profile.provider == "deepseek";
    let repo_instructions_for_prompt = if is_deepseek && !repository_instruction_blocks.is_empty() {
        vec![deepseek_repository_instructions_system_prompt_pointer()]
    } else {
        repository_instruction_blocks.clone()
    };
    let provider_native_execution_groups = blocks
        .iter()
        .enumerate()
        .filter_map(|(index, _)| context.metadata_for_block(index))
        .filter(|metadata| {
            metadata
                .provider_owner()
                .is_some_and(|owner| owner.matches_provider(&profile.provider))
        })
        .filter_map(|metadata| metadata.execution_group_id().cloned())
        .collect::<BTreeSet<_>>();
    let prompt_profile = AgentPromptProfile::default_for(identity.agent_id, identity.pane_id)
        .with_provider(&profile.provider);
    let mut messages = Vec::with_capacity(blocks.len() + 1);
    messages.push(ModelMessage {
        role: ModelMessageRole::System,
        source: ContextSourceKind::System,
        placement: ContextPlacement::StablePrefix,
        content: assemble_agent_system_prompt(
            &prompt_profile,
            &repo_instructions_for_prompt,
            prompt_assets,
        )?,
    });
    if is_deepseek && !repository_instruction_blocks.is_empty() {
        messages.push(deepseek_repository_instructions_message(
            &repository_instruction_blocks,
        ));
    }
    for (index, block) in blocks.iter().enumerate() {
        let metadata = context.metadata_for_block(index).ok_or_else(|| {
            crate::AgentRequestAssemblyError::from(crate::AgentContextError::new(
                "context block is missing stored causal metadata",
            ))
        })?;
        if let Some(owner) = metadata.provider_owner() {
            if !owner.matches_provider(&profile.provider) {
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
        if block.source == ContextSourceKind::ProjectGuidance {
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
        interaction_kind: ModelInteractionKind::CapabilityDecision,
        allowed_actions: AllowedActionSet::capability_decision(),
        stop: is_deepseek.then(|| vec!["\n}".to_string()]),
        messages,
    };
    constrain_skill_actions_for_loaded_context(&mut request);
    Ok(request)
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
        | crate::ContextSemanticKind::ReferenceEvent
        | crate::ContextSemanticKind::LiveState => ModelMessageRole::Context,
    }
}

/// Returns the system-prompt pointer used for DeepSeek repository guidance.
fn deepseek_repository_instructions_system_prompt_pointer() -> String {
    "DeepSeek provider note: active repository instructions are provided in a dedicated neutral-context message immediately after this system prompt. The provider may transport that block through a user-compatible envelope, but it is not user-authored. Treat it as the authoritative repository instruction contents for this turn; do not reread repository instruction files merely because the full text is reinforced outside section 3.".to_string()
}

/// Builds the fixed-position repository-guidance message used by DeepSeek.
fn deepseek_repository_instructions_message(
    repository_instruction_blocks: &[String],
) -> ModelMessage {
    let mut content = String::from("Active repository instructions:\n\n");
    for block in repository_instruction_blocks {
        content.push_str(block);
        content.push_str("\n\n");
    }
    ModelMessage {
        role: ModelMessageRole::Context,
        source: ContextSourceKind::ProjectGuidance,
        placement: ContextPlacement::StablePrefix,
        content,
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
                Some(crate::ProviderContinuityOwner::DeepSeek),
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
                Some(crate::ProviderContinuityOwner::DeepSeek),
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
