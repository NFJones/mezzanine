//! Assistant, progress, and rationale context ledger updates.

use super::super::*;

impl RuntimeSessionService {
    /// Appends the provider response's assistant-visible context to a running
    /// turn before any action results are observed.
    ///
    /// # Parameters
    /// - `turn`: The running agent turn receiving the assistant context block.
    /// - `execution`: The provider execution whose rationale and visible
    ///   assistant text should remain available to later provider requests.
    pub(super) fn append_agent_execution_assistant_context(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let content = assistant_context_content_for_execution(execution);
        if content.trim().is_empty() {
            return Ok(());
        }
        let context = self
            .agent_turn_contexts
            .get_mut(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let label = format!("assistant response for {}", turn.turn_id);
        if context.blocks.iter().any(|block| {
            block.source == ContextSourceKind::TranscriptAssistant
                && block.label == label
                && block.content == content
        }) {
            return Ok(());
        }
        context.blocks.push(ContextBlock {
            source: ContextSourceKind::TranscriptAssistant,
            label,
            content,
        });
        Ok(())
    }

    /// Appends or updates the active-turn progress `say` ledger.
    ///
    /// # Parameters
    /// - `turn`: The running agent turn receiving the ledger context block.
    /// - `execution`: The provider execution whose progress `say` actions should
    ///   become explicit context for later continuations.
    pub(super) fn append_agent_execution_progress_say_ledger_context(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let new_entries = runtime_progress_say_entries_for_execution(execution);
        if new_entries.is_empty() {
            return Ok(());
        }
        let context = self
            .agent_turn_contexts
            .get_mut(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let mut previous_entries = Vec::new();
        context.blocks.retain(|block| {
            let is_progress_say_ledger = block.source == ContextSourceKind::RuntimeHint
                && block.label == RUNTIME_PROGRESS_SAY_LEDGER_LABEL;
            if is_progress_say_ledger {
                previous_entries.extend(runtime_progress_say_entries_from_ledger(&block.content));
            }
            !is_progress_say_ledger
        });
        let entries = runtime_merge_progress_say_entries(previous_entries, new_entries);
        if entries.is_empty() {
            return Ok(());
        }
        context.blocks.push(ContextBlock {
            source: ContextSourceKind::RuntimeHint,
            label: RUNTIME_PROGRESS_SAY_LEDGER_LABEL.to_string(),
            content: runtime_progress_say_ledger_content(&entries),
        });
        Ok(())
    }

    /// Returns rationale entries already emitted during an active turn.
    ///
    /// # Parameters
    /// - `turn_id`: Active turn whose current rationale ledger should be read.
    pub(super) fn current_turn_rationale_entries(&self, turn_id: &str) -> Vec<String> {
        let Some(context) = self.agent_turn_contexts.get(turn_id) else {
            return Vec::new();
        };
        context
            .blocks
            .iter()
            .filter(|block| {
                block.source == ContextSourceKind::RuntimeHint
                    && block.label == RUNTIME_RATIONALE_LEDGER_LABEL
            })
            .flat_map(|block| runtime_rationale_entries_from_ledger(&block.content))
            .collect()
    }

    /// Suppresses batch/action rationale that repeats already-emitted same-turn intent.
    ///
    /// Repeated investigative rationale is visible to the user in verbose
    /// thinking mode and can indirectly bias the next provider turn. Once a
    /// current-turn rationale ledger records that intent, later batches should
    /// mention only a materially new reason.
    pub(super) fn suppress_redundant_rationale_entries(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<usize> {
        let mut visible_entries = self.current_turn_rationale_entries(&turn.turn_id);
        let Some(batch) = execution.response.action_batch.as_mut() else {
            return Ok(0);
        };
        let mut suppressed = 0usize;
        if let Some(entry) = runtime_normalize_rationale_entry(&batch.rationale)
            && runtime_rationale_entry_repeats_existing(&entry, &visible_entries)
        {
            batch.rationale.clear();
            suppressed += 1;
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                "batch rationale suppressed reason=repeated_current_turn_rationale",
            )?;
        } else if let Some(entry) = runtime_normalize_rationale_entry(&batch.rationale) {
            visible_entries.push(entry);
        }
        for action in &mut batch.actions {
            let Some(entry) = runtime_normalize_rationale_entry(&action.rationale) else {
                continue;
            };
            if runtime_rationale_entry_repeats_existing(&entry, &visible_entries) {
                action.rationale.clear();
                suppressed += 1;
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    &format!(
                        "action {} rationale suppressed reason=repeated_current_turn_rationale",
                        action.id
                    ),
                )?;
                continue;
            }
            visible_entries.push(entry);
        }
        Ok(suppressed)
    }

    /// Appends or updates the active-turn rationale ledger.
    ///
    /// # Parameters
    /// - `turn`: The running agent turn receiving the ledger context block.
    /// - `execution`: The provider execution whose retained rationale should
    ///   become explicit same-turn context for later continuations.
    pub(super) fn append_agent_execution_rationale_ledger_context(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let new_entries = runtime_rationale_entries_for_execution(execution);
        if new_entries.is_empty() {
            return Ok(());
        }
        let context = self
            .agent_turn_contexts
            .get_mut(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let mut previous_entries = Vec::new();
        context.blocks.retain(|block| {
            let is_rationale_ledger = block.source == ContextSourceKind::RuntimeHint
                && block.label == RUNTIME_RATIONALE_LEDGER_LABEL;
            if is_rationale_ledger {
                previous_entries.extend(runtime_rationale_entries_from_ledger(&block.content));
            }
            !is_rationale_ledger
        });
        let entries = runtime_merge_rationale_entries(previous_entries, new_entries);
        if entries.is_empty() {
            return Ok(());
        }
        context.blocks.push(ContextBlock {
            source: ContextSourceKind::RuntimeHint,
            label: RUNTIME_RATIONALE_LEDGER_LABEL.to_string(),
            content: runtime_rationale_ledger_content(&entries),
        });
        Ok(())
    }
}
