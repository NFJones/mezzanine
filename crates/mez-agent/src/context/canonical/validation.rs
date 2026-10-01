//! Deterministic placement, authorship, and causal-metadata validation.
//!
//! These rules inspect the canonical owner's records without maintaining a
//! second context store. Mutations remain candidate-before-commit operations.

use super::{
    AgentContextError, AgentContextResult, ContextBlock, ContextBlockMetadata, ContextPlacement,
    ContextRetention, ContextSemanticKind, ContextSourceKind, ProviderContinuityOwner,
    validate_context_required,
};
use crate::{ProviderApiCompatibility, ProviderTranscriptEvent};

/// Returns the stable insertion boundary for one lifecycle placement.
///
/// New stable blocks are placed after the existing stable prefix, new
/// conversation blocks after existing immutable chronology, and new ephemeral
/// blocks at the end. This preserves producer order within each phase without
/// globally sorting context and changing transcript semantics.
pub fn context_placement_insertion_index(
    blocks: &[ContextBlock],
    placement: ContextPlacement,
) -> usize {
    blocks
        .iter()
        .position(|block| block.placement > placement)
        .unwrap_or(blocks.len())
}

/// Inserts one context block at its lifecycle phase boundary.
pub fn insert_context_block_by_placement(blocks: &mut Vec<ContextBlock>, block: ContextBlock) {
    let insertion_index = context_placement_insertion_index(blocks, block.placement);
    blocks.insert(insertion_index, block);
}

/// Rejects cache-lifecycle regressions without changing producer order.
pub fn validate_context_placement_order(blocks: &[ContextBlock]) -> AgentContextResult<()> {
    let mut entered_phase = ContextPlacement::StablePrefix;
    for (index, block) in blocks.iter().enumerate() {
        if block.placement < entered_phase {
            return Err(AgentContextError::new(format!(
                "context lifecycle regression at block index {index}: label={:?} source={:?} placement={:?} entered_phase={entered_phase:?}",
                block.label, block.source, block.placement
            )));
        }
        entered_phase = block.placement;
    }
    Ok(())
}

/// Rejects semantic, retention, and authorship combinations that would make a
/// provider request ambiguous or move durable events into request-local state.
pub fn validate_context_semantics(blocks: &[ContextBlock]) -> AgentContextResult<()> {
    let mut active_user_seen = false;
    for (index, block) in blocks.iter().enumerate() {
        validate_context_required("context label", &block.label)?;
        let semantic = block.semantic_kind();
        let retention = block.retention();
        let invalid_reason = match block.placement {
            ContextPlacement::StablePrefix
                if semantic != ContextSemanticKind::AmbientInstruction =>
            {
                Some("stable-prefix blocks must be ambient instructions")
            }
            ContextPlacement::ConversationAppend
                if semantic == ContextSemanticKind::AmbientInstruction =>
            {
                Some("append-only blocks cannot contain ambient instructions")
            }
            _ => None,
        };
        if let Some(reason) = invalid_reason {
            return Err(context_semantic_error(index, block, reason));
        }
        if block.source == ContextSourceKind::UserInstruction {
            if block.placement != ContextPlacement::ConversationAppend
                || retention != ContextRetention::Exact
            {
                return Err(context_semantic_error(
                    index,
                    block,
                    "direct user instructions must be exact append-only user events",
                ));
            }
            active_user_seen = true;
        } else if active_user_seen && semantic == ContextSemanticKind::TaskPrelude {
            return Err(context_semantic_error(
                index,
                block,
                "task prelude cannot appear after the active user prompt",
            ));
        }
    }
    Ok(())
}

/// Builds one detailed semantic-validation failure.
pub(super) fn context_semantic_error(
    index: usize,
    block: &ContextBlock,
    reason: &str,
) -> AgentContextError {
    AgentContextError::new(format!(
        "context semantic violation at block index {index}: label={:?} source={:?} placement={:?} semantic={:?} retention={:?}: {reason}",
        block.label,
        block.source,
        block.placement,
        block.semantic_kind(),
        block.retention()
    ))
}

/// Returns the exclusive owner encoded by one provider continuity payload.
pub(super) fn provider_owner_for_block(block: &ContextBlock) -> Option<ProviderContinuityOwner> {
    ProviderTranscriptEvent::from_transcript_content(&block.content).and_then(|event| match event {
        ProviderTranscriptEvent::OpenAiResponseOutput { .. }
        | ProviderTranscriptEvent::OpenAiFunctionCallOutput { .. } => {
            ProviderContinuityOwner::new(ProviderApiCompatibility::OpenAiResponses, "openai")
        }
        ProviderTranscriptEvent::OpenAiChatCompletionsAssistantToolCall { provider_id, .. }
        | ProviderTranscriptEvent::OpenAiChatCompletionsToolResult { provider_id, .. } => {
            ProviderContinuityOwner::new(
                ProviderApiCompatibility::OpenAiChatCompletions,
                provider_id,
            )
        }
        ProviderTranscriptEvent::DeepSeekAssistantToolCall { .. }
        | ProviderTranscriptEvent::DeepSeekToolResult { .. } => ProviderContinuityOwner::new(
            ProviderApiCompatibility::DeepSeekChatCompletions,
            "deepseek",
        ),
    })
}

/// Validates producer-selected metadata against structural placement rules.
pub(super) fn validate_context_block_metadata(
    index: usize,
    block: &ContextBlock,
    metadata: &ContextBlockMetadata,
) -> AgentContextResult<()> {
    let stable_slot_metadata_is_partial =
        metadata.stable_slot_id.is_some() != metadata.stable_source_fingerprint.is_some();
    let invalid_reason = match block.placement {
        ContextPlacement::StablePrefix
            if metadata.semantic_kind != ContextSemanticKind::AmbientInstruction
                || metadata.event_sequence.is_some()
                || stable_slot_metadata_is_partial =>
        {
            Some(
                "stable blocks must be unsequenced ambient instructions with complete slot metadata",
            )
        }
        ContextPlacement::ConversationAppend
            if metadata.semantic_kind == ContextSemanticKind::AmbientInstruction
                || metadata.event_sequence.is_none()
                || metadata.stable_slot_id.is_some()
                || metadata.stable_source_fingerprint.is_some() =>
        {
            Some("conversation events require a sequence and cannot own stable-slot metadata")
        }
        _ => None,
    };
    if let Some(reason) = invalid_reason {
        return Err(context_semantic_error(index, block, reason));
    }
    if metadata.provider_owner.is_some()
        && ProviderTranscriptEvent::from_transcript_content(&block.content).is_none()
    {
        return Err(context_semantic_error(
            index,
            block,
            "provider ownership requires a typed provider continuity payload",
        ));
    }
    if ProviderTranscriptEvent::from_transcript_content(&block.content).is_some()
        && metadata.provider_owner.is_none()
    {
        return Err(context_semantic_error(
            index,
            block,
            "provider continuity payload requires an explicit owner",
        ));
    }
    if let Some(owner) = metadata.provider_owner.as_ref()
        && ProviderTranscriptEvent::from_transcript_content(&block.content)
            .is_none_or(|event| !owner.accepts_transcript_event(&event))
    {
        return Err(context_semantic_error(
            index,
            block,
            "provider ownership API must match the typed continuity payload family",
        ));
    }
    if metadata.semantic_kind == ContextSemanticKind::UserEvent
        && (block.source != ContextSourceKind::UserInstruction
            && block.source != ContextSourceKind::TranscriptUser)
    {
        return Err(context_semantic_error(
            index,
            block,
            "only direct or transcript user sources may claim user-event semantics",
        ));
    }
    if metadata.retention == ContextRetention::ExecutionGroup
        && metadata.execution_group_id.is_none()
    {
        return Err(context_semantic_error(
            index,
            block,
            "execution-group retention requires an execution-group identity",
        ));
    }
    Ok(())
}

impl super::AgentContext {
    /// Validates stored metadata alignment and strictly increasing chronology.
    pub(super) fn validate_stored_metadata(&self) -> AgentContextResult<()> {
        let typed_len = self
            .stable_slots
            .len()
            .saturating_add(self.chronology.len());
        if self.blocks.len() != self.block_metadata.len() || self.blocks.len() != typed_len {
            return Err(AgentContextError::new(format!(
                "context projection length mismatch: stable={} chronology={} blocks={} metadata={}; mutate context through checked APIs",
                self.stable_slots.len(),
                self.chronology.len(),
                self.blocks.len(),
                self.block_metadata.len()
            )));
        }
        let typed_projection = self
            .stable_slots
            .iter()
            .map(|slot| (&slot.block, slot.metadata()))
            .chain(
                self.chronology
                    .iter()
                    .map(|event| (&event.block, event.metadata())),
            );
        for (index, ((expected_block, expected_metadata), (block, metadata))) in typed_projection
            .zip(self.blocks.iter().zip(&self.block_metadata))
            .enumerate()
        {
            if expected_block != block || expected_metadata != *metadata {
                return Err(AgentContextError::new(format!(
                    "context read projection drifted from typed storage at block index {index}"
                )));
            }
        }
        let mut last_sequence = 0u64;
        let mut stable_slot_ids = std::collections::BTreeSet::new();
        let mut active_prompt_count = 0usize;
        let mut assistant_execution_groups = std::collections::BTreeSet::new();
        for (index, (block, metadata)) in self.blocks.iter().zip(&self.block_metadata).enumerate() {
            validate_context_block_metadata(index, block, metadata)?;
            if let Some(slot_id) = metadata.stable_slot_id.as_ref()
                && !stable_slot_ids.insert(slot_id.as_str())
            {
                return Err(context_semantic_error(
                    index,
                    block,
                    "stable slot identities must be unique",
                ));
            }
            if block.source == ContextSourceKind::UserInstruction && block.label == "user prompt" {
                active_prompt_count = active_prompt_count.saturating_add(1);
                if active_prompt_count > 1 {
                    return Err(context_semantic_error(
                        index,
                        block,
                        "durable context may contain only one active user prompt",
                    ));
                }
            }
            if metadata.semantic_kind == ContextSemanticKind::AssistantEvent {
                let Some(group_id) = metadata.execution_group_id.as_ref() else {
                    return Err(context_semantic_error(
                        index,
                        block,
                        "assistant execution events require an execution-group identity",
                    ));
                };
                assistant_execution_groups.insert(group_id.clone());
            }
            if matches!(
                block.source,
                ContextSourceKind::ActionResult | ContextSourceKind::TranscriptTool
            ) && metadata.retention == ContextRetention::ExecutionGroup
                && !metadata
                    .execution_group_id
                    .as_ref()
                    .is_some_and(|group_id| assistant_execution_groups.contains(group_id))
            {
                return Err(context_semantic_error(
                    index,
                    block,
                    "action and native-tool evidence requires a preceding owning assistant execution",
                ));
            }
            if let Some(sequence) = metadata.event_sequence {
                if sequence.get() <= last_sequence {
                    return Err(context_semantic_error(
                        index,
                        block,
                        "conversation event sequences must be strictly increasing",
                    ));
                }
                last_sequence = sequence.get();
            }
        }
        if last_sequence >= self.next_event_sequence {
            return Err(AgentContextError::new(
                "next context event sequence must exceed the committed high-water mark",
            ));
        }
        Ok(())
    }

    /// Validates the semantic and lifetime contract for stored turn context.
    pub fn validate_durable(&self) -> AgentContextResult<()> {
        self.validate_stored_metadata()?;
        validate_context_placement_order(&self.blocks)?;
        validate_context_semantics(&self.blocks)?;
        Ok(())
    }

    /// Validates that blocks advance monotonically through cache lifecycle phases.
    ///
    /// This check remains separate from [`super::AgentContext::new`] because low-level
    /// tests and a small number of builders need to represent an intermediate
    /// context before it reaches a finalized runtime boundary. Production
    /// prompt submission and provider assembly must validate before side effects.
    pub fn validate_placement_order(&self) -> AgentContextResult<()> {
        validate_context_placement_order(&self.blocks)
    }
}
