//! Initial descriptive history construction, before any live replay ownership.
//!
//! Task partitions retire only explicitly superseded instructions. Temporary
//! sequence slots have no handles or membership; checked anchors are restored
//! over the entire source before creating the owner. Live arrivals never use
//! this boundary and retain their original reserved-frontier checks.

use super::*;
use crate::{ContextBlockRecord, ImportedExecutionEvent};

impl ReplayEpochContext {
    /// Builds initial unsummarized history from an independently qualified raw
    /// projection. Every chronological block needs its original record in index
    /// order. `tasks` contains (user root, first suffix occurrence) pairs from
    /// actual recorded admissions, never inferred labels or prose. The caller
    /// must qualify their original producer and archive/pending custody.
    ///
    /// Unnumbered rows receive ordered slots between original `reservations`.
    /// Conflicting anchors, missing/duplicate records, overlapping tasks and bad
    /// native framing reject without publishing an owner. Historical task
    /// supersession grants no execution closure, dispatch or append receipt.
    pub fn from_imported_history(
        conversation: ReplayConversationId,
        blocks: Vec<ContextBlock>,
        records: &[(usize, ContextBlockRecord)],
        execution: &[ImportedExecutionEvent],
        reservations: &[(ContextBlockOccurrenceId, ContextEventSequence)],
        tasks: &[(ContextBlockOccurrenceId, ContextBlockOccurrenceId)],
    ) -> AgentContextResult<Self> {
        let invalid = || AgentContextError::new("invalid initial replay history projection");
        if records.len() != blocks.len()
            || records
                .iter()
                .enumerate()
                .any(|(index, (stored, _))| index != *stored)
        {
            return Err(invalid());
        }
        let by_id = records
            .iter()
            .map(|(index, record)| (record.occurrence_id(), *index))
            .collect::<BTreeMap<_, _>>();
        if by_id.len() != blocks.len() {
            return Err(invalid());
        }
        let mut boundaries = tasks
            .iter()
            .map(|(root, first)| {
                let start = *by_id.get(first).ok_or_else(invalid)?;
                let prompt = *by_id.get(root).ok_or_else(invalid)?;
                if start > prompt || blocks[prompt].source != ContextSourceKind::UserInstruction {
                    return Err(invalid());
                }
                Ok((start, prompt))
            })
            .collect::<AgentContextResult<Vec<_>>>()?;
        boundaries.sort_unstable();
        if boundaries.windows(2).any(|pair| pair[0].1 >= pair[1].0) {
            return Err(invalid());
        }
        let metadata = execution
            .iter()
            .map(|event| {
                let record = event.record().ok_or_else(invalid)?;
                Ok((
                    *by_id.get(&record.occurrence_id()).ok_or_else(invalid)?,
                    event,
                ))
            })
            .collect::<AgentContextResult<Vec<_>>>()?;
        let parts = std::iter::once((0, boundaries.first().map_or(blocks.len(), |b| b.0), false))
            .chain(boundaries.iter().enumerate().map(|(index, (start, _))| {
                (
                    *start,
                    boundaries.get(index + 1).map_or(blocks.len(), |b| b.0),
                    true,
                )
            }));
        let mut body = AgentContext::empty();
        let mut completed = BTreeSet::new();
        for (start, end, task) in parts {
            if start == end {
                continue;
            }
            if task {
                let instructions = body
                    .chronology()
                    .iter()
                    .filter(|event| {
                        event.retention() == ContextRetention::Exact
                            && instruction_event_can_complete(event)
                    })
                    .map(ConversationEvent::sequence)
                    .collect();
                completed.extend(body.retire_instruction_occurrences(&instructions)?);
            }
            let records = records[start..end]
                .iter()
                .map(|(index, record)| (index - start, *record))
                .collect::<Vec<_>>();
            let execution = metadata
                .iter()
                .filter(|(index, _)| (start..end).contains(index))
                .map(|(_, event)| (*event).clone())
                .collect::<Vec<_>>();
            body.append_live_task_source(blocks[start..end].to_vec(), &records, &execution)?;
        }
        body.restore_imported_history_sequences(reservations)?;
        let mut owner = Self::new(conversation, body)?;
        owner.source_lifecycle.completed_instructions = completed;
        owner.source_lifecycle.task_admissions = tasks.iter().copied().collect();
        owner.validate_membership()?;
        Ok(owner)
    }
}
