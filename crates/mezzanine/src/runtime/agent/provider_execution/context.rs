//! Provider-response chronology and controller-side rationale suppression.

use std::collections::BTreeMap;

use super::super::{
    AgentTurnExecution, AgentTurnRecord, ContextSourceKind, MezError, Result,
    RuntimeSessionService, assistant_context_content_for_execution,
    runtime_suppress_redundant_batch_rationale,
};
use mez_agent::{ContextExecutionGroupId, ContextPlacement, ModelMessageRole};

const CAPABILITY_DECISION_LABEL: &str = "controller capability decision";
const CAPABILITY_DECISION_PREFIX: &str = "[controller capability decision]\n";

impl RuntimeSessionService {
    /// Appends settled controller decisions and the provider response to a
    /// running turn before any action results are observed.
    ///
    /// # Parameters
    /// - `turn`: The running agent turn receiving the assistant context block.
    /// - `execution`: The provider execution whose rationale and visible
    ///   assistant text should remain available to later provider requests.
    pub(crate) fn append_agent_execution_chronology(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let mut context = self
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;

        let mut retained_decision_counts = BTreeMap::<String, usize>::new();
        for block in context.blocks().iter().filter(|block| {
            block.source == ContextSourceKind::CommittedEvidence
                && block.label == CAPABILITY_DECISION_LABEL
        }) {
            *retained_decision_counts
                .entry(block.content.clone())
                .or_default() += 1;
        }

        let mut request_decision_counts = BTreeMap::<String, usize>::new();
        let mut new_decisions = Vec::new();
        for message in &execution.request.messages {
            if message.role != ModelMessageRole::Context
                || message.placement != ContextPlacement::ConversationAppend
                || !matches!(
                    message.source,
                    ContextSourceKind::RuntimeHint | ContextSourceKind::CommittedEvidence
                )
            {
                continue;
            }
            let Some(content) = message.content.strip_prefix(CAPABILITY_DECISION_PREFIX) else {
                continue;
            };
            let occurrence = request_decision_counts
                .entry(content.to_string())
                .and_modify(|count| *count = count.saturating_add(1))
                .or_insert(1);
            if message.source == ContextSourceKind::CommittedEvidence {
                continue;
            }
            if *occurrence
                <= retained_decision_counts
                    .get(content)
                    .copied()
                    .unwrap_or_default()
            {
                continue;
            }
            new_decisions.push(content.to_string());
        }

        let content = assistant_context_content_for_execution(execution);
        let label = format!("assistant response for {}", turn.turn_id);
        let append_assistant = !content.trim().is_empty()
            && !context.blocks().iter().any(|block| {
                block.source == ContextSourceKind::TranscriptAssistant
                    && block.label == label
                    && block.content == content
            });
        if new_decisions.is_empty() && !append_assistant {
            return Ok(());
        }
        let group_id = ContextExecutionGroupId::new(format!(
            "{}:provider-response:{}",
            turn.turn_id,
            context.event_sequence_high_water_mark().saturating_add(1)
        ))
        .map_err(|error| MezError::invalid_state(error.to_string()))?;
        for decision in new_decisions {
            context
                .append_evidence_event(
                    ContextSourceKind::CommittedEvidence,
                    CAPABILITY_DECISION_LABEL,
                    decision,
                    group_id.clone(),
                    None,
                    true,
                )
                .map_err(|error| MezError::invalid_state(error.to_string()))?;
        }
        if append_assistant {
            context
                .append_assistant_event(label, content, group_id)
                .map_err(|error| MezError::invalid_state(error.to_string()))?;
        }
        self.agent_turn_contexts_mut()
            .insert(turn.turn_id.clone(), context);
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
