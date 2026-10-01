//! Compatibility history import policy and sparse event identity allocation.
//!
//! Inference is confined to legacy/test import boundaries. Typed runtime events
//! never reconstruct causal ownership from labels after they have committed.

use super::{
    AgentContextError, AgentContextResult, CONTEXT_EVENT_SEQUENCE_STRIDE, ContextBlock,
    ContextExecutionGroupId, ContextPlacement, ContextRetention, ContextSemanticKind,
    ContextSourceKind,
};
use std::ops::Range;

/// Allocates ordered identities for a replacement history prefix.
pub(super) fn history_prefix_replacement_sequences(
    count: usize,
    successor_sequence: Option<u64>,
) -> AgentContextResult<Vec<u64>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let count_u64 = u64::try_from(count)
        .map_err(|_| AgentContextError::new("imported history replacement is too large"))?;
    if let Some(successor) = successor_sequence {
        let divisor = count_u64
            .checked_add(1)
            .ok_or_else(|| AgentContextError::new("imported history replacement is too large"))?;
        let step = successor / divisor;
        if step == 0 {
            return Err(AgentContextError::new(
                "imported history replacement cannot preserve retained event identities",
            ));
        }
        return (1..=count_u64)
            .map(|index| {
                step.checked_mul(index).ok_or_else(|| {
                    AgentContextError::new("imported history replacement sequence exhausted")
                })
            })
            .collect();
    }
    (1..=count_u64)
        .map(|index| {
            CONTEXT_EVENT_SEQUENCE_STRIDE
                .checked_mul(index)
                .ok_or_else(|| {
                    AgentContextError::new("imported history replacement sequence exhausted")
                })
        })
        .collect()
}

/// Assigns explicit compatibility group identities while importing an ordered
/// legacy/test block vector.
pub(super) fn compatibility_execution_group_ids(
    blocks: &[ContextBlock],
) -> AgentContextResult<Vec<Option<ContextExecutionGroupId>>> {
    let chronology = blocks
        .iter()
        .filter(|block| block.placement == ContextPlacement::ConversationAppend)
        .collect::<Vec<_>>();
    let mut group_ids = vec![None; chronology.len()];
    for range in compatibility_execution_group_ranges(&chronology) {
        if !chronology[range.clone()]
            .iter()
            .any(|block| block.source == ContextSourceKind::TranscriptAssistant)
        {
            continue;
        }
        let group_id = ContextExecutionGroupId::new(format!(
            "compat-execution-group-{}",
            range.start.saturating_add(1)
        ))?;
        for index in range {
            if chronology[index].retention() == ContextRetention::ExecutionGroup {
                group_ids[index] = Some(group_id.clone());
            }
        }
    }
    Ok(group_ids)
}

/// Chooses the explicit compatibility contract for one imported event.
///
/// Persisted transcripts carry authoritative role and sequence but older
/// records do not carry an execution-group identifier. Contiguous
/// assistant/tool/result records can be grouped without changing chronology.
/// Evidence separated from its possible assistant owner by an exact barrier is
/// retained as an exact neutral reference instead of inventing a causal link or
/// rejecting otherwise complete history.
pub(super) fn compatibility_event_contract(
    block: &ContextBlock,
    execution_group_id: Option<&ContextExecutionGroupId>,
) -> (ContextSemanticKind, ContextRetention, bool) {
    if matches!(
        block.source,
        ContextSourceKind::TranscriptTool
            | ContextSourceKind::CommittedEvidence
            | ContextSourceKind::ActionResult
            | ContextSourceKind::McpRetrievedManifest
    ) && execution_group_id.is_none()
    {
        return (
            ContextSemanticKind::ReferenceEvent,
            ContextRetention::Exact,
            true,
        );
    }
    (
        block.semantic_kind(),
        block.retention(),
        block.recoverable_for_compaction(),
    )
}

/// Finds indivisible execution groups at the compatibility import boundary.
fn compatibility_execution_group_ranges(blocks: &[&ContextBlock]) -> Vec<Range<usize>> {
    let mut groups = Vec::new();
    let mut start = 0usize;
    let mut has_assistant = false;
    let mut has_native_tool = false;
    for (index, block) in blocks.iter().enumerate() {
        let protected = block.retention() == ContextRetention::Exact;
        let attaches_to_previous = match block.source {
            ContextSourceKind::CommittedEvidence => {
                !has_assistant
                    && blocks[start..index]
                        .iter()
                        .all(|candidate| candidate.source == ContextSourceKind::CommittedEvidence)
            }
            ContextSourceKind::TranscriptAssistant => blocks[start..index]
                .iter()
                .all(|candidate| candidate.source == ContextSourceKind::CommittedEvidence),
            ContextSourceKind::TranscriptTool => has_assistant,
            ContextSourceKind::ActionResult | ContextSourceKind::McpRetrievedManifest => {
                has_assistant || has_native_tool
            }
            _ => false,
        };
        let current_group_protected = blocks[start..index]
            .iter()
            .any(|candidate| candidate.retention() == ContextRetention::Exact);
        if index > start && (protected || current_group_protected || !attaches_to_previous) {
            groups.push(start..index);
            start = index;
            has_assistant = false;
            has_native_tool = false;
        }
        has_assistant |= block.source == ContextSourceKind::TranscriptAssistant;
        has_native_tool |= block.source == ContextSourceKind::TranscriptTool;
    }
    if start < blocks.len() {
        groups.push(start..blocks.len());
    }
    groups
}
