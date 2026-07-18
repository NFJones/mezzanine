//! Assistant chronology and controller-side rationale suppression.

use super::super::{
    AgentTurnExecution, AgentTurnRecord, ContextBlock, ContextSourceKind, MezError, Result,
    RuntimeSessionService, assistant_context_content_for_execution,
    runtime_suppress_redundant_batch_rationale,
};

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
            .agent_turn_contexts_mut()
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
        let insertion_index = context
            .blocks
            .iter()
            .position(|block| block.placement == mez_agent::ContextPlacement::EphemeralTail)
            .unwrap_or(context.blocks.len());
        context.blocks.insert(
            insertion_index,
            ContextBlock {
                source: ContextSourceKind::TranscriptAssistant,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                label,
                content,
            },
        );
        Ok(())
    }

    /// Suppresses batch/action rationale that repeats already-emitted same-turn intent.
    ///
    /// Repeated investigative rationale is visible to the user in verbose
    /// thinking mode. This suppresses duplicates within the current response
    /// without replaying a controller ledger into later model requests.
    pub(super) fn suppress_redundant_rationale_entries(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<usize> {
        let Some(batch) = execution.response.action_batch.as_mut() else {
            return Ok(0);
        };
        let suppression = runtime_suppress_redundant_batch_rationale(batch, &[]);
        if suppression.batch_suppressed {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                "batch rationale suppressed reason=repeated_current_turn_rationale",
            )?;
        }
        for action_id in &suppression.action_ids {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "action {action_id} rationale suppressed reason=repeated_current_turn_rationale"
                ),
            )?;
        }
        Ok(suppression.count())
    }
}
