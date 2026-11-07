//! Explicit user-task supersession at a checked chronological admission boundary.
//!
//! Live admission and qualified durable replay use the same task lifecycle.
//! The caller identifies the task root and complete new suffix; arbitrary user
//! or peer arrivals never select this port. Existing instructions retire before
//! the new prelude is appended. All mutations, including retirement, are atomic.

use super::*;
use crate::{ContextBlockRecord, ImportedExecutionEvent};

impl ReplayEpochContext {
    /// Admits an explicitly requested task, retiring the exact prior instruction
    /// scope through its actual lifecycle owner. `prompt_index` identifies the
    /// user instruction in the new suffix, including any preceding task prelude.
    /// Imported records and execution metadata retain their usual validation.
    /// Failure leaves source, protection, accounting and task witnesses unchanged.
    pub fn admit_instruction_task(
        &mut self,
        blocks: Vec<ContextBlock>,
        records: &[(usize, ContextBlockRecord)],
        execution: &[ImportedExecutionEvent],
        prompt_index: usize,
    ) -> AgentContextResult<()> {
        self.append_instruction_task(blocks, records, execution, prompt_index, None)
    }

    /// Replays one explicitly recorded task admission over the actual causal
    /// base. Every suffix block requires its original record; sequence hints
    /// cannot rewind source. The product must independently qualify the durable
    /// admission and its exact first suffix occurrence, never infer it from prose.
    /// This reconstructs only instruction lifecycle, never native completion,
    /// transport acknowledgment, task dispatch or provider execution.
    pub fn restore_instruction_task(
        &mut self,
        blocks: Vec<ContextBlock>,
        records: &[(usize, ContextBlockRecord)],
        execution: &[ImportedExecutionEvent],
        prompt_index: usize,
        sequences: &[(usize, ContextEventSequence)],
    ) -> AgentContextResult<()> {
        self.append_instruction_task(blocks, records, execution, prompt_index, Some(sequences))
    }

    /// Shares actual lifecycle semantics while retaining the stronger source
    /// accounting requirements of checked durable arrivals.
    fn append_instruction_task(
        &mut self,
        blocks: Vec<ContextBlock>,
        records: &[(usize, ContextBlockRecord)],
        execution: &[ImportedExecutionEvent],
        prompt_index: usize,
        sequences: Option<&[(usize, ContextEventSequence)]>,
    ) -> AgentContextResult<()> {
        if blocks.get(prompt_index).is_none_or(|block| {
            block.source != ContextSourceKind::UserInstruction
                || block.placement != ContextPlacement::ConversationAppend
        }) {
            return Err(AgentContextError::new(
                "task admission requires an explicit user root",
            ));
        }
        let mut candidate = self.clone();
        let instructions = candidate
            .context()
            .chronology()
            .iter()
            .filter(|event| {
                event.retention() == ContextRetention::Exact
                    && instruction_source_can_complete(event.block())
            })
            .map(|event| event.sequence())
            .collect::<Vec<_>>();
        if !instructions.is_empty() {
            let task = candidate.begin_instruction_task()?;
            candidate.bind_instruction_occurrences(&task, &instructions)?;
            candidate.complete_instruction_task(&task)?;
        }
        let start = candidate.context().chronology().len();
        let conversation = candidate.conversation.clone();
        let mut body = candidate.into_owned_context();
        match sequences {
            Some(sequences) => {
                body.append_imported_replay_arrivals(blocks, records, execution, sequences)?;
            }
            None => {
                body.append_live_task_source(blocks, records, execution)?;
            }
        }
        let first = body.chronology()[start].record().occurrence_id();
        let prompt = body.chronology()[start + prompt_index]
            .record()
            .occurrence_id();
        let mut candidate = Self::from_owned_context(conversation, body)?;
        candidate
            .source_lifecycle
            .task_admissions
            .insert(prompt, first);
        candidate.validate_membership()?;
        *self = candidate;
        Ok(())
    }

    /// Descriptive original boundary for immutable source backing and export.
    /// Only actual admission methods populate this fact; checkpoint restoration
    /// does not reconstruct producer authority from blocks or labels.
    pub fn task_admission_start(
        &self,
        prompt: ContextBlockOccurrenceId,
    ) -> Option<ContextBlockOccurrenceId> {
        self.source_lifecycle.task_admissions.get(&prompt).copied()
    }
}
