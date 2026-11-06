//! Atomic history continuation over the actual canonical causal base.
//!
//! The suffix alone may contain only a native result whose declaration remains
//! in frozen history. Restore metadata only into newly appended occurrences,
//! seed group ordinals from the unchanged base, and validate native identities
//! over the combined source before publishing protection or frontier changes.
//! This boundary imports descriptive source, never execution/ACK authority.

use super::{
    AgentContext, AgentContextError, AgentContextResult, ContextBlock, ContextBlockRecord,
    ContextExecutionGroupId, ContextPlacement, ContextRetention, ImportedExecutionEvent,
    ProviderTranscriptEvent,
};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;

impl AgentContext {
    /// Appends an accounted suffix with execution metadata that may continue an
    /// existing group. Indexed records address only `blocks`; imported metadata
    /// matches only those new occurrences in order. Existing group ordinals must
    /// continue from the actual base; new groups start at one. Native records
    /// require explicit metadata and matching API/provider/call identities.
    /// Unanswered calls remain protected rather than requiring fake results.
    ///
    /// Stable/request-local blocks, changed/reordered/duplicate source, missing
    /// metadata, invalid ordinal/native pairs or exhausted sequences reject
    /// atomically. Existing records, sequences, metadata and handles are retained;
    /// new events keep their imported IDs/costs with fresh local sequences.
    /// The caller must independently qualify archive/pending source custody.
    pub fn append_imported_history_with_execution(
        &mut self,
        blocks: Vec<ContextBlock>,
        records: &[(usize, ContextBlockRecord)],
        imported: &[ImportedExecutionEvent],
    ) -> AgentContextResult<usize> {
        self.validate_durable()?;
        if blocks
            .iter()
            .any(|block| block.placement != ContextPlacement::ConversationAppend)
        {
            return Err(AgentContextError::new(
                "imported history suffix must contain only chronological blocks",
            ));
        }
        let history = Self::import_durable_blocks_with_records(blocks, records, &[])?;
        let start = self.chronology.len();
        let mut ordinals = BTreeMap::<ContextExecutionGroupId, u64>::new();
        for group in self
            .chronology
            .iter()
            .filter_map(|event| event.execution_group_id())
        {
            let count = ordinals.entry(group.clone()).or_default();
            *count = count.checked_add(1).ok_or_else(|| {
                AgentContextError::new("imported execution-group ordinal exhausted")
            })?;
        }
        let mut candidate = self.clone();
        for mut event in history.chronology {
            event.sequence = candidate.allocate_event_sequence()?;
            candidate.chronology.push(event);
        }
        let restored = candidate.restore_imported_metadata(imported, start, ordinals)?;
        for (index, event) in candidate.chronology.iter().enumerate().skip(start) {
            if event.block.retention() == ContextRetention::ExecutionGroup
                && ProviderTranscriptEvent::from_transcript_content(&event.block.content).is_some()
                && !restored.contains(&index)
            {
                return Err(AgentContextError::new(
                    "native history suffix requires explicit execution metadata",
                ));
            }
        }
        for group in imported
            .iter()
            .map(|event| event.execution_group_id().clone())
            .collect::<BTreeSet<_>>()
        {
            candidate.validate_native_group_source(&group, false)?;
            candidate.retain_pending_execution_group(&group)?;
        }
        for index in start..candidate.chronology.len() {
            candidate.protect_replay_arrival(&candidate.chronology[index].clone())?;
        }
        candidate.rebuild_projections();
        candidate.validate_durable()?;
        let appended = candidate.chronology.len() - start;
        *self = candidate;
        Ok(appended)
    }

    /// Applies metadata only within the selected suffix of an isolated owner.
    /// Returned indices identify actual matches; they are not durable receipts.
    pub(super) fn restore_imported_metadata(
        &mut self,
        imported: &[ImportedExecutionEvent],
        mut search_start: usize,
        mut ordinals: BTreeMap<ContextExecutionGroupId, u64>,
    ) -> AgentContextResult<BTreeSet<usize>> {
        let mut restored = BTreeSet::new();
        for record in imported {
            let previous = ordinals
                .get(record.execution_group_id())
                .copied()
                .unwrap_or(0);
            if previous.checked_add(1) != Some(record.ordinal()) {
                return Err(AgentContextError::new(
                    "imported execution-group ordinals must be contiguous with the actual source",
                ));
            }
            let relative = self.chronology[search_start..]
                .iter()
                .position(|event| {
                    event.block() == record.block()
                        && record.record().is_none_or(|stored| {
                            stored.occurrence_id() == event.record().occurrence_id()
                        })
                })
                .ok_or_else(|| {
                    AgentContextError::new(
                        "imported execution metadata has no matching chronological block",
                    )
                })?;
            let index = search_start + relative;
            let event = &mut self.chronology[index];
            event.semantic_kind = record.block().semantic_kind();
            event.retention = ContextRetention::ExecutionGroup;
            event.execution_group_id = Some(record.execution_group_id().clone());
            event.provider_owner = record.provider_owner().cloned();
            event.recoverable_for_compaction = true;
            ordinals.insert(record.execution_group_id().clone(), record.ordinal());
            restored.insert(index);
            search_start = index + 1;
        }
        Ok(restored)
    }
}
