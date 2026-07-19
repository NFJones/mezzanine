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
const PROVIDER_ACTION_EXECUTION_SEPARATOR: &str = "~mez~";
const PROVIDER_ACTION_EXECUTION_DIGEST_LEN: usize = 16;

impl RuntimeSessionService {
    /// Replaces response-local MAAP action ordinals with execution-scoped ids.
    ///
    /// Provider parsers deliberately synthesize ordinal ids such as `action-1`
    /// for each independently parsed response. Those ordinals are not unique
    /// across provider continuations in one logical turn. This boundary adds a
    /// compact suffix derived from the accepted request/response execution
    /// identity before traces, approvals, dispatch, or result settlement can
    /// retain an ambiguous id.
    ///
    /// Explicit runtime-owned ids are already unique and remain unchanged.
    /// Existing execution suffixes are replaced rather than nested, making an
    /// exact replay of the same execution idempotent. Preplanned results for
    /// parser-generated actions are remapped together with their actions so
    /// every downstream adapter sees one identity.
    pub(crate) fn scope_provider_execution_action_ids(
        &self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<()> {
        let identity_content = provider_execution_identity_content(execution);
        let group_id = provider_execution_group_id(turn, execution, &identity_content)?;
        let execution_digest = provider_execution_action_digest(&group_id)?;
        let Some(batch) = execution.response.action_batch.as_mut() else {
            return Ok(());
        };

        let mut remapped_ids = BTreeMap::new();
        for action in &mut batch.actions {
            let original_id = action.id.clone();
            let local_id = provider_execution_local_action_id(&original_id).to_string();
            if !provider_response_local_action_id(&local_id) {
                continue;
            }
            let scoped_id =
                format!("{local_id}{PROVIDER_ACTION_EXECUTION_SEPARATOR}{execution_digest}");
            remapped_ids.insert(original_id, scoped_id.clone());
            remapped_ids.insert(local_id, scoped_id.clone());
            action.id = scoped_id;
        }
        for result in &mut execution.action_results {
            let local_id = provider_execution_local_action_id(&result.action_id).to_string();
            let Some(scoped_id) = remapped_ids
                .get(&result.action_id)
                .or_else(|| remapped_ids.get(&local_id))
                .cloned()
            else {
                continue;
            };
            let original_id = std::mem::replace(&mut result.action_id, scoped_id.clone());
            if let Some(structured_content) = result.structured_content_json.as_mut() {
                remap_structured_action_ids(structured_content, &original_id, &scoped_id)?;
                if original_id != local_id {
                    remap_structured_action_ids(structured_content, &local_id, &scoped_id)?;
                }
            }
        }
        Ok(())
    }

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

        let identity_content = provider_execution_identity_content(execution);
        let group_id = provider_execution_group_id(turn, execution, &identity_content)?;
        let content = assistant_context_content_for_execution(execution);
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

/// Returns assistant content with execution suffixes removed for identity hashing.
///
/// Action ids contain a digest of the provider execution that owns them. The
/// digest cannot itself depend on those suffixed ids, so identity hashing uses
/// the parser-local ordinal while durable assistant context retains the fully
/// scoped id shown by action results and audit records.
fn provider_execution_identity_content(execution: &AgentTurnExecution) -> String {
    let mut canonical = execution.clone();
    if let Some(batch) = canonical.response.action_batch.as_mut() {
        for action in &mut batch.actions {
            action.id = provider_execution_local_action_id(&action.id).to_string();
        }
    }
    assistant_context_content_for_execution(&canonical)
}

/// Returns the parser-local portion of an execution-scoped action id.
fn provider_execution_local_action_id(action_id: &str) -> &str {
    let Some((local_id, suffix)) = action_id.rsplit_once(PROVIDER_ACTION_EXECUTION_SEPARATOR)
    else {
        return action_id;
    };
    if !provider_response_local_action_id(local_id)
        || suffix.len() != PROVIDER_ACTION_EXECUTION_DIGEST_LEN
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return action_id;
    }
    local_id
}

/// Returns whether an id is the ordinal synthesized by provider parsing.
fn provider_response_local_action_id(action_id: &str) -> bool {
    let Some(ordinal) = action_id.strip_prefix("action-") else {
        return false;
    };
    !ordinal.is_empty()
        && ordinal.bytes().all(|byte| byte.is_ascii_digit())
        && ordinal.bytes().any(|byte| byte != b'0')
}

/// Returns the compact digest suffix used to namespace one execution's actions.
fn provider_execution_action_digest(group_id: &ContextExecutionGroupId) -> Result<&str> {
    let digest = group_id
        .as_str()
        .rsplit_once(":provider-response:")
        .map(|(_, digest)| digest)
        .ok_or_else(|| {
            MezError::invalid_state("provider execution group is missing its response digest")
        })?;
    if digest.len() < PROVIDER_ACTION_EXECUTION_DIGEST_LEN {
        return Err(MezError::invalid_state(
            "provider execution group response digest is too short",
        ));
    }
    Ok(&digest[digest.len() - PROVIDER_ACTION_EXECUTION_DIGEST_LEN..])
}

/// Rewrites action-identity fields in one structured result payload.
fn remap_structured_action_ids(
    structured_content: &mut String,
    original_id: &str,
    scoped_id: &str,
) -> Result<()> {
    let mut value: serde_json::Value = serde_json::from_str(structured_content).map_err(|error| {
        MezError::invalid_state(format!(
            "action result structured content is not valid JSON while scoping its identity: {error}"
        ))
    })?;
    remap_structured_action_id_value(&mut value, original_id, scoped_id);
    *structured_content = serde_json::to_string(&value).map_err(|error| {
        MezError::invalid_state(format!(
            "action result structured content could not be serialized after scoping its identity: {error}"
        ))
    })?;
    Ok(())
}

/// Recursively rewrites explicit action-id fields without changing payload text.
fn remap_structured_action_id_value(
    value: &mut serde_json::Value,
    original_id: &str,
    scoped_id: &str,
) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                remap_structured_action_id_value(value, original_id, scoped_id);
            }
        }
        serde_json::Value::Object(fields) => {
            for (name, value) in fields {
                if matches!(name.as_str(), "action_id" | "original_action_id")
                    && value.as_str() == Some(original_id)
                {
                    *value = serde_json::Value::String(scoped_id.to_string());
                } else {
                    remap_structured_action_id_value(value, original_id, scoped_id);
                }
            }
        }
        _ => {}
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
