//! Provider-response chronology and causal execution ownership.

use std::collections::BTreeMap;

use super::super::{
    AgentTurnExecution, AgentTurnRecord, ContextSourceKind, MezError, Result,
    RuntimeSessionService, assistant_context_content_for_execution,
};
use mez_agent::{ContextExecutionGroupId, ContextPlacement, ModelMessageRole};
use sha2::{Digest, Sha256};

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
        let group_id = provider_execution_group_id(turn, execution, &content)?;
        let action_ids = execution
            .response
            .action_batch
            .as_ref()
            .map(|batch| {
                batch
                    .actions
                    .iter()
                    .map(|action| action.id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let retained_groups = self.agent.agent_execution_groups_by_turn.get(&turn.turn_id);
        let mapped_groups = action_ids
            .iter()
            .filter_map(|action_id| retained_groups.and_then(|groups| groups.get(action_id)))
            .cloned()
            .collect::<Vec<_>>();
        if !mapped_groups.is_empty() && mapped_groups.len() != action_ids.len() {
            return Err(MezError::invalid_state(
                "provider execution action ownership is only partially registered",
            ));
        }
        if mapped_groups
            .iter()
            .any(|retained_group| retained_group != &group_id)
        {
            return Err(MezError::invalid_state(
                "provider execution action ownership does not match its stable execution identity",
            ));
        }
        let retained_assistant = context.chronology().iter().find(|event| {
            event.execution_group_id() == Some(&group_id)
                && event.block().source == ContextSourceKind::TranscriptAssistant
        });
        if let Some(retained_assistant) = retained_assistant {
            return match retained_assistant {
                event if event.block().content == content => Ok(()),
                _ => Err(MezError::invalid_state(
                    "replayed provider execution changed its assistant response content",
                )),
            };
        }
        if !mapped_groups.is_empty() {
            return Err(MezError::invalid_state(
                "provider execution ownership exists without its assistant event",
            ));
        }
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
        context
            .append_assistant_event(
                format!(
                    "assistant response for {} execution {}",
                    turn.turn_id,
                    &group_id.as_str()[group_id.as_str().len().saturating_sub(16)..]
                ),
                content,
                group_id.clone(),
            )
            .map_err(|error| MezError::invalid_state(error.to_string()))?;
        let mut provider_tool_call_ids = Vec::new();
        for (index, event) in execution
            .response
            .provider_transcript_events
            .iter()
            .enumerate()
        {
            provider_tool_call_ids.extend(event.deepseek_tool_call_ids());
            context
                .append_evidence_event(
                    ContextSourceKind::TranscriptTool,
                    format!("provider continuity event {}", index.saturating_add(1)),
                    event.to_transcript_content(),
                    group_id.clone(),
                    Some(mez_agent::ProviderContinuityOwner::DeepSeek),
                    true,
                )
                .map_err(|error| MezError::invalid_state(error.to_string()))?;
        }
        self.agent_turn_contexts_mut()
            .insert(turn.turn_id.clone(), context);
        let groups = self
            .agent
            .agent_execution_groups_by_turn
            .entry(turn.turn_id.clone())
            .or_default();
        for action_id in action_ids {
            groups.insert(action_id, group_id.clone());
        }
        if !provider_tool_call_ids.is_empty() {
            self.agent
                .agent_provider_tool_calls_by_turn
                .entry(turn.turn_id.clone())
                .or_default()
                .insert(group_id, provider_tool_call_ids);
        }
        Ok(())
    }
}

/// Builds a stable identity for one accepted provider request/response pair.
///
/// Exact replay of the same completion produces the same group while two
/// identical response strings reached from different request chronology remain
/// distinct because their consumed message sequence differs.
fn provider_execution_group_id(
    turn: &AgentTurnRecord,
    execution: &AgentTurnExecution,
    assistant_content: &str,
) -> Result<ContextExecutionGroupId> {
    let mut digest = Sha256::new();
    update_provider_execution_digest(&mut digest, &turn.turn_id);
    update_provider_execution_digest(&mut digest, &execution.request.provider);
    update_provider_execution_digest(&mut digest, &execution.request.model);
    update_provider_execution_digest(
        &mut digest,
        &format!("{:?}", execution.request.interaction_kind),
    );
    for message in &execution.request.messages {
        update_provider_execution_digest(&mut digest, &format!("{:?}", message.role));
        update_provider_execution_digest(&mut digest, &format!("{:?}", message.source));
        update_provider_execution_digest(&mut digest, &format!("{:?}", message.placement));
        update_provider_execution_digest(&mut digest, &message.content);
    }
    update_provider_execution_digest(&mut digest, &execution.response.provider);
    update_provider_execution_digest(&mut digest, &execution.response.model);
    update_provider_execution_digest(&mut digest, &execution.response.raw_text);
    update_provider_execution_digest(&mut digest, assistant_content);
    for event in &execution.response.provider_transcript_events {
        update_provider_execution_digest(&mut digest, &event.to_transcript_content());
    }
    let digest = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    ContextExecutionGroupId::new(format!("{}:provider-response:{digest}", turn.turn_id))
        .map_err(|error| MezError::invalid_state(error.to_string()))
}

/// Adds one length-delimited field to the provider-execution identity digest.
fn update_provider_execution_digest(digest: &mut Sha256, value: &str) {
    digest.update(value.len().to_le_bytes());
    digest.update(value.as_bytes());
}
