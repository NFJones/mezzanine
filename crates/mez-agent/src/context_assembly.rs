//! Provider-independent model-request assembly from canonical context.
//!
//! This module owns context-to-message projection, repository-guidance
//! placement, provider transcript preservation, prompt
//! cache identity extraction, default action surfaces, and provider-specific
//! request defaults. Product code supplies stable turn identity and prompt
//! assets without exposing runtime records or filesystem access.

use crate::{
    AgentContext, AgentContextError, AgentPromptAssetSource, AgentPromptProfile,
    AgentRequestAssemblyResult, AllowedActionSet, ContextBlock, ContextPlacement,
    ContextSourceKind, ModelInteractionKind, ModelMessage, ModelMessageRole, ModelProfile,
    ModelRequest, ProviderTranscriptEvent, assemble_agent_system_prompt,
    constrain_skill_actions_for_loaded_context, model_context_block_header,
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
    validate_model_profile_request(profile, identity.turn_id)?;
    validate_context_placement_order(&context.blocks)?;

    let blocks = context.blocks.clone();
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
    for block in &blocks {
        if ProviderTranscriptEvent::from_transcript_content(&block.content).is_some() {
            messages.push(ModelMessage {
                role: ModelMessageRole::System,
                source: block.source,
                placement: block.placement,
                content: block.content.clone(),
            });
            continue;
        }
        if block.source == ContextSourceKind::ProjectGuidance {
            continue;
        }
        if block.source == ContextSourceKind::Configuration
            && matches!(
                block.label.as_str(),
                "session identity" | "pane identity" | "prompt cache lineage"
            )
        {
            continue;
        }
        messages.push(ModelMessage {
            role: role_for_context_source(block.source),
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
        prompt_cache_session_id: prompt_cache_session_id_from_blocks(&blocks),
        prompt_cache_lineage_id: prompt_cache_lineage_id_from_blocks(&blocks),
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

/// Rejects cache-lifecycle regressions without changing producer order.
fn validate_context_placement_order(blocks: &[ContextBlock]) -> AgentRequestAssemblyResult<()> {
    let mut entered_phase = ContextPlacement::StablePrefix;
    for (index, block) in blocks.iter().enumerate() {
        if block.placement < entered_phase {
            return Err(AgentContextError::new(format!(
                "context lifecycle regression at block index {index}: label={:?} source={:?} placement={:?} entered_phase={entered_phase:?}",
                block.label, block.source, block.placement
            ))
            .into());
        }
        entered_phase = block.placement;
    }
    Ok(())
}

/// Maps context provenance to provider-neutral message roles.
pub fn role_for_context_source(source: ContextSourceKind) -> ModelMessageRole {
    match source {
        ContextSourceKind::System => ModelMessageRole::System,
        ContextSourceKind::DeveloperInstruction
        | ContextSourceKind::Policy
        | ContextSourceKind::Configuration
        | ContextSourceKind::RuntimeHint
        | ContextSourceKind::EvidenceLedger
        | ContextSourceKind::CommittedEvidence
        | ContextSourceKind::RoutedHandoff => ModelMessageRole::Developer,
        ContextSourceKind::ActionResult | ContextSourceKind::TranscriptTool => {
            ModelMessageRole::Tool
        }
        ContextSourceKind::TranscriptAssistant => ModelMessageRole::Assistant,
        ContextSourceKind::UserInstruction
        | ContextSourceKind::SkillInstruction
        | ContextSourceKind::LocalMessage
        | ContextSourceKind::ProjectGuidance
        | ContextSourceKind::Memory
        | ContextSourceKind::Transcript
        | ContextSourceKind::TranscriptUser => ModelMessageRole::User,
    }
}

/// Extracts the live Mezzanine session id from hidden identity context.
fn prompt_cache_session_id_from_blocks(blocks: &[ContextBlock]) -> Option<String> {
    blocks
        .iter()
        .find(|block| {
            block.source == ContextSourceKind::Configuration && block.label == "session identity"
        })
        .and_then(|block| {
            block
                .content
                .split_whitespace()
                .find_map(|field| field.strip_prefix("session_id="))
        })
        .filter(|session_id| !session_id.trim().is_empty())
        .map(ToOwned::to_owned)
}

/// Extracts stable prompt-cache lineage from hidden metadata.
fn prompt_cache_lineage_id_from_blocks(blocks: &[ContextBlock]) -> Option<String> {
    blocks
        .iter()
        .find(|block| {
            block.source == ContextSourceKind::Configuration
                && block.label == "prompt cache lineage"
        })
        .map(|block| block.content.trim())
        .filter(|lineage_id| !lineage_id.is_empty())
        .map(ToOwned::to_owned)
}

/// Returns the system-prompt pointer used for DeepSeek repository guidance.
fn deepseek_repository_instructions_system_prompt_pointer() -> String {
    "DeepSeek provider note: active repository instructions are provided in a dedicated user message immediately after this system prompt. Treat that block as the authoritative repository instruction contents for this turn; do not reread repository instruction files merely because the full text is reinforced outside section 3.".to_string()
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
        role: ModelMessageRole::User,
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
    /// extracts cache identity without exposing hidden blocks as messages.
    #[test]
    fn model_request_assembly_preserves_hidden_context_contracts() {
        let event = ProviderTranscriptEvent::DeepSeekToolResult {
            tool_call_id: "call-1".to_string(),
            content: "result".to_string(),
        }
        .to_transcript_content();
        let context = AgentContext::new(vec![
            ContextBlock {
                source: ContextSourceKind::Configuration,
                placement: crate::ContextPlacement::StablePrefix,
                label: "session identity".to_string(),
                content: "session_id=session-1".to_string(),
            },
            ContextBlock {
                source: ContextSourceKind::Transcript,
                placement: crate::ContextPlacement::ConversationAppend,
                label: "provider event".to_string(),
                content: event.clone(),
            },
            ContextBlock {
                source: ContextSourceKind::UserInstruction,
                placement: crate::ContextPlacement::EphemeralTail,
                label: "user".to_string(),
                content: "continue".to_string(),
            },
        ])
        .unwrap();
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
