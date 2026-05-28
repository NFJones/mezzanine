//! Provider request assembly for agent context.
//!
//! This module owns the final context-to-provider-message projection. It keeps
//! repository-guidance embedding, cache-aware context ordering, prompt-cache
//! session extraction, and the default MAAP action surface out of the context
//! type facade.

use super::super::{
    AgentPromptProfile, AgentTurnRecord, ProviderTranscriptEvent,
    build_agent_system_prompt_with_repository_instructions, role_for_source, validate_non_empty,
};
use super::compaction::model_context_has_bulk_compaction_summary;
use super::evidence::prepare_model_context_blocks;
use super::skills::constrain_skill_actions_for_loaded_context;
use super::{
    AgentContext, AllowedActionSet, ContextBlock, ContextSourceKind, ContextStability,
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

    let prepared_blocks = prepare_model_context_blocks(context.blocks.clone());
    let blocks = if model_context_has_bulk_compaction_summary(&prepared_blocks) {
        prepared_blocks
    } else {
        order_model_context_blocks(prepared_blocks)
    };
    let repository_instruction_blocks = blocks
        .iter()
        .filter(|block| block.source == ContextSourceKind::ProjectGuidance)
        .map(|block| block.content.clone())
        .collect::<Vec<_>>();
    let mut messages = Vec::with_capacity(context.blocks.len() + 1);
    messages.push(ModelMessage {
        role: ModelMessageRole::System,
        source: ContextSourceKind::System,
        content: build_agent_system_prompt_with_repository_instructions(
            &AgentPromptProfile::default_for(&turn.agent_id, &turn.pane_id),
            &repository_instruction_blocks,
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
        messages.push(ModelMessage {
            role: role_for_source(block.source),
            source: block.source,
            content: format!("{}{}", model_context_block_header(block), block.content),
        });
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
        prompt_cache_session_id: prompt_cache_session_id_from_blocks(&blocks),
        turn_id: turn.turn_id.clone(),
        agent_id: turn.agent_id.clone(),
        available_mcp_tools: Vec::new(),
        interaction_kind: ModelInteractionKind::CapabilityDecision,
        allowed_actions: AllowedActionSet::capability_decision(),
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

/// Returns provider context blocks with stable reusable material before
/// turn-volatile material while preserving relative order inside each group.
///
/// The volatile suffix is chronological execution evidence. In particular,
/// action results appended after the user's instruction must remain after that
/// instruction so the model can see that the requested check already ran.
fn order_model_context_blocks(blocks: Vec<ContextBlock>) -> Vec<ContextBlock> {
    let mut indexed = blocks.into_iter().enumerate().collect::<Vec<_>>();
    indexed.sort_by_key(|(index, block)| (model_context_block_group_rank(block), *index));
    indexed.into_iter().map(|(_, block)| block).collect()
}

/// Returns the cache-aware ordering group for one context block.
fn model_context_block_group_rank(block: &ContextBlock) -> u8 {
    if block.stable_prefix_eligible() {
        0
    } else if block.stability() == ContextStability::SessionStable {
        1
    } else {
        2
    }
}
