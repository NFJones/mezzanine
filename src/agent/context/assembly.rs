//! Provider request assembly for agent context.
//!
//! This module owns the final context-to-provider-message projection. It keeps
//! repository-guidance embedding, prompt-cache metadata extraction, and the
//! default MAAP action surface out of the context type facade.

use super::super::{
    AgentPromptProfile, AgentTurnRecord, ProviderTranscriptEvent,
    build_agent_system_prompt_with_repository_instructions, role_for_source, validate_non_empty,
};
use super::evidence::prepare_model_context_blocks;
use super::skills::constrain_skill_actions_for_loaded_context;
use super::{
    AgentContext, AllowedActionSet, ContextBlock, ContextSourceKind,
    DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT, ModelInteractionKind, ModelMessage,
    ModelMessageRole, ModelProfile, ModelRequest, model_context_block_header,
};
use crate::error::Result;

/// Runs the assemble model request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn assemble_model_request(
    profile: &ModelProfile,
    turn: &AgentTurnRecord,
    context: &AgentContext,
) -> Result<ModelRequest> {
    assemble_model_request_with_retained_tail_percent(
        profile,
        turn,
        context,
        DEFAULT_MODEL_CONTEXT_RETAINED_TAIL_PERCENT,
    )
}

/// Assembles a provider request without request-local fallback compaction.
///
/// The retained-tail argument is accepted by runtime call sites that also drive
/// explicit compaction paths, but provider request assembly itself preserves the
/// current context exactly and lets provider feedback trigger recovery.
pub fn assemble_model_request_with_retained_tail_percent(
    profile: &ModelProfile,
    turn: &AgentTurnRecord,
    context: &AgentContext,
    _retained_tail_percent: usize,
) -> Result<ModelRequest> {
    validate_non_empty("model provider", &profile.provider)?;
    validate_non_empty("model", &profile.model)?;
    validate_non_empty("turn_id", &turn.turn_id)?;

    let blocks = prepare_model_context_blocks(context.blocks.clone());
    let repository_instruction_blocks = blocks
        .iter()
        .filter(|block| block.source == ContextSourceKind::ProjectGuidance)
        .map(|block| block.content.clone())
        .collect::<Vec<_>>();
    let is_deepseek = profile.provider.as_str() == "deepseek";
    let repo_instructions_for_prompt = if is_deepseek {
        Vec::new()
    } else {
        repository_instruction_blocks.clone()
    };
    let mut messages = Vec::with_capacity(context.blocks.len() + 1);
    messages.push(ModelMessage {
        role: ModelMessageRole::System,
        source: ContextSourceKind::System,
        content: build_agent_system_prompt_with_repository_instructions(
            &AgentPromptProfile::default_for(&turn.agent_id, &turn.pane_id)
                .with_provider(&profile.provider),
            &repo_instructions_for_prompt,
        )?,
    });
    for block in &blocks {
        if ProviderTranscriptEvent::from_transcript_content(&block.content).is_some() {
            messages.push(ModelMessage {
                role: ModelMessageRole::System,
                source: block.source,
                content: block.content.clone(),
            });
            continue;
        }
        if matches!(block.source, ContextSourceKind::ProjectGuidance) {
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
            role: role_for_source(block.source),
            source: block.source,
            content: format!("{}{}", model_context_block_header(block), block.content),
        });
    }
    if is_deepseek && !repository_instruction_blocks.is_empty() {
        prepend_repository_instructions_to_first_user_message(
            &mut messages,
            &repository_instruction_blocks,
        );
    }

    let mut request = ModelRequest {
        provider: profile.provider.clone(),
        model: profile.model.clone(),
        reasoning_effort: profile
            .provider_options
            .get("reasoning_effort")
            .cloned()
            .or_else(|| profile.reasoning_profile.clone()),
        thinking_enabled: profile.thinking_enabled(),
        latency_preference: profile.latency_preference.clone(),
        prompt_cache_retention: profile
            .provider_options
            .get("prompt_cache_retention")
            .cloned(),
        max_output_tokens: profile.max_output_tokens(),
        temperature: profile.temperature().map(|t| t.to_string()).or_else(|| {
            if is_deepseek {
                Some("0.5".to_string())
            } else {
                None
            }
        }),
        prompt_cache_session_id: prompt_cache_session_id_from_blocks(&blocks),
        prompt_cache_lineage_id: prompt_cache_lineage_id_from_blocks(&blocks),
        turn_id: turn.turn_id.clone(),
        agent_id: turn.agent_id.clone(),
        available_mcp_tools: Vec::new(),
        interaction_kind: ModelInteractionKind::CapabilityDecision,
        allowed_actions: AllowedActionSet::capability_decision(),
        stop: if is_deepseek {
            Some(vec!["\n}".to_string()])
        } else {
            None
        },
        messages,
    };
    constrain_skill_actions_for_loaded_context(&mut request);
    Ok(request)
}

/// Extracts the live Mezzanine session UUID from runtime identity context.
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

/// Extracts the stable prompt-cache lineage id from hidden runtime metadata.
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

/// Prepends discovered repository instruction content to the first user message.
///
/// DeepSeek models weight user messages more strongly than system prompts.
/// Moving repository guidance into the first user turn places it where the
/// model's attention is strongest, improving instruction adherence without
/// altering the contract for other providers.
fn prepend_repository_instructions_to_first_user_message(
    messages: &mut [ModelMessage],
    repository_instruction_blocks: &[String],
) {
    let Some(first_user) = messages
        .iter_mut()
        .find(|m| m.role == ModelMessageRole::User)
    else {
        return;
    };
    let mut new_content = String::new();
    new_content.push_str("Active repository instructions:\n\n");
    for block in repository_instruction_blocks {
        new_content.push_str(block);
        new_content.push_str("\n\n");
    }
    new_content.push_str("---\n\n");
    new_content.push_str(&first_user.content);
    first_user.content = new_content;
}
