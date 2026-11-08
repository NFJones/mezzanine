//! Compatibility history import policy and sparse event identity allocation.
//!
//! Inference is confined to legacy/test import boundaries. Typed runtime events
//! never reconstruct causal ownership from labels after they have committed.

use super::{
    AgentContextError, AgentContextResult, CONTEXT_EVENT_SEQUENCE_STRIDE, ContextBlock,
    ContextExecutionGroupId, ContextPlacement, ContextRetention, ContextSemanticKind,
    ContextSourceKind,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

impl super::AgentContext {
    /// Describes the audited legacy importer’s expected group/provider framing
    /// without flattening actual task lifecycle or rebuilding source records.
    /// This is validation metadata only: it neither mutates canonical events
    /// nor qualifies an original producer interval, closure or execution right.
    /// Callers must still match original rows and honor explicit typed framing.
    /// `explicit` identifies independently qualified grouped wire occurrences;
    /// their framing is retained and their names are reserved against inference.
    pub fn legacy_import_framing(
        &self,
        explicit: &BTreeSet<super::ContextBlockOccurrenceId>,
    ) -> AgentContextResult<Vec<super::ContextBlockMetadata>> {
        self.validate_durable()?;
        let groups = compatibility_execution_group_ids(self.blocks())?;
        let mut reserved = self
            .chronology()
            .iter()
            .filter(|event| explicit.contains(&event.record().occurrence_id()))
            .filter_map(|event| event.execution_group_id().cloned())
            .collect::<BTreeSet<_>>();
        let mut aliases = BTreeMap::new();
        self.chronology()
            .iter()
            .zip(groups)
            .map(|(event, group)| {
                let mut metadata = event.metadata();
                if !explicit.contains(&event.record().occurrence_id()) {
                    metadata.execution_group_id = match group {
                        None => None,
                        Some(group) => {
                            if !aliases.contains_key(&group) {
                                let mut candidate = group.clone();
                                let mut suffix = 0u64;
                                while reserved.contains(&candidate) {
                                    suffix = suffix.checked_add(1).ok_or_else(|| {
                                        AgentContextError::new("legacy group namespace exhausted")
                                    })?;
                                    candidate = ContextExecutionGroupId::new(format!(
                                        "legacy-import-{}-{suffix}",
                                        String::from(event.record().occurrence_id())
                                    ))?;
                                }
                                reserved.insert(candidate.clone());
                                aliases.insert(group.clone(), candidate);
                            }
                            aliases.get(&group).cloned()
                        }
                    };
                    metadata.provider_owner = super::provider_owner_for_block(event.block());
                }
                Ok(metadata)
            })
            .collect::<AgentContextResult<Vec<_>>>()
    }

    /// Reconciles only inferred legacy framing at an unowned import boundary.
    /// Explicit wire records must match exact original occurrences, blocks and
    /// ownership before their groups are reserved. Inferred groups cannot alias
    /// those producer groups. Records, sequences and task lifecycle are unchanged;
    /// every resulting group remains conservatively protected, without claims
    /// of execution completion. Owned replay must use its original append ports.
    pub fn normalize_legacy_import_framing(
        &mut self,
        explicit: &[super::ImportedExecutionEvent],
    ) -> AgentContextResult<()> {
        if self.has_replay_owner() {
            return Err(AgentContextError::new(
                "owned replay cannot normalize legacy framing",
            ));
        }
        let by_id = self
            .chronology()
            .iter()
            .map(|event| (event.record().occurrence_id(), event))
            .collect::<BTreeMap<_, _>>();
        let mut ids = BTreeSet::new();
        for frame in explicit {
            let record = frame.record().ok_or_else(|| {
                AgentContextError::new("legacy explicit framing has no original record")
            })?;
            let event = by_id.get(&record.occurrence_id()).ok_or_else(|| {
                AgentContextError::new("legacy explicit framing has no original occurrence")
            })?;
            if !ids.insert(record.occurrence_id())
                || event.record() != record
                || event.block() != frame.block()
                || event.execution_group_id() != Some(frame.execution_group_id())
                || event.provider_owner() != frame.provider_owner()
            {
                return Err(AgentContextError::new(
                    "legacy explicit framing changed original source",
                ));
            }
        }
        let framing = self.legacy_import_framing(&ids)?;
        let mut candidate = self.clone();
        for (event, metadata) in candidate.chronology.iter_mut().zip(framing) {
            event.execution_group_id = metadata.execution_group_id;
            event.provider_owner = metadata.provider_owner;
        }
        let groups = candidate
            .chronology()
            .iter()
            .filter_map(|event| event.execution_group_id().cloned())
            .collect::<BTreeSet<_>>();
        // Reassigning an inferred name cannot retire its obligations. Remove
        // only obsolete names, then install fresh conservative protection for
        // every resulting group. Explicit ancestry remains a provisional bound.
        candidate
            .pending_execution_groups
            .retain(|group, _| groups.contains(group));
        candidate
            .execution_intervals
            .retain(|group, _| groups.contains(group));
        candidate.rebuild_projections();
        for group in groups {
            candidate.protect_unqualified_imported_group(&group)?;
        }
        candidate.validate_durable()?;
        *self = candidate;
        Ok(())
    }
}

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

#[cfg(test)]
mod import_framing_tests {
    use super::*;
    use crate::{AgentContext, ContextBlockRecord, ContextEventSequence, ImportedExecutionEvent};

    /// The task importer originally grouped each raw slice before attaching
    /// its head. Moving to complete-source names must retain all obligations.
    #[test]
    fn legacy_framing_renames_pending_groups_without_releasing_source() {
        let mut context = AgentContext::new(vec![
            ContextBlock::assistant_event("answer", "original answer"),
            ContextBlock::user_event("prompt", "original instruction"),
            ContextBlock::assistant_event("answer", "later answer"),
        ])
        .unwrap();
        let old = context
            .chronology()
            .iter()
            .filter_map(|event| event.execution_group_id().cloned())
            .collect::<BTreeSet<_>>();
        for group in &old {
            context.protect_unqualified_imported_group(group).unwrap();
        }
        let mut header = AgentContext::new(vec![ContextBlock::reference_event(
            ContextSourceKind::CompactionPrefix,
            "head",
            "old head",
        )])
        .unwrap();
        let mut head = header.chronology.remove(0);
        head.sequence = ContextEventSequence::new(1023).unwrap();
        context.chronology.insert(0, head);
        context.rebuild_projections();
        let expected = context
            .chronology()
            .iter()
            .map(|event| (event.record(), event.sequence(), event.block().clone()))
            .collect::<Vec<_>>();
        context.normalize_legacy_import_framing(&[]).unwrap();
        for (event, (record, sequence, block)) in context.chronology().iter().zip(expected) {
            assert_eq!(event.record(), record);
            assert_eq!(event.sequence(), sequence);
            assert_eq!(event.block(), &block);
            if let Some(group) = event.execution_group_id() {
                assert!(context.execution_group_is_pending(group));
                assert!(!old.contains(group));
            }
        }
        for group in old {
            assert!(!context.execution_group_is_pending(&group));
        }
        context.validate_durable().unwrap();
    }

    #[test]
    fn legacy_framing_refuses_foreign_records_and_owned_mutation() {
        let mut context = AgentContext::new(vec![ContextBlock::assistant_event(
            "answer",
            "original answer",
        )])
        .unwrap();
        let event = &context.chronology()[0];
        let foreign = ImportedExecutionEvent::new(
            event.block().clone(),
            event.execution_group_id().unwrap().clone(),
            1,
            None,
        )
        .unwrap()
        .with_record(ContextBlockRecord::allocate(event.block()).unwrap())
        .unwrap();
        let before = context.clone();
        assert!(context.normalize_legacy_import_framing(&[foreign]).is_err());
        assert_eq!(context, before);
        let owner = crate::ReplayEpochContext::new(
            crate::ReplayConversationId::new("source").unwrap(),
            context,
        )
        .unwrap();
        let mut body = owner.into_owned_context();
        let before = body.clone();
        assert!(body.normalize_legacy_import_framing(&[]).is_err());
        assert_eq!(body, before);
    }
}
