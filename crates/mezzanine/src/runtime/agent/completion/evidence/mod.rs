//! Canonical custody for safe completion observations before archive projection.
//!
//! A bookkeeping-only occurrence cannot be published as original model source.
//! Retain each actual observation in its original context first. Its independent
//! observation group owns no action, native result, task or append receipt and
//! cannot advance the original action group's retirement fence.

use super::retirement::{CompletionCapture, CompletionTranscriptTarget};
use super::*;
use sha2::{Digest, Sha256};

impl RuntimeSessionService {
    /// Retains one immutable observation before projecting a fresh original
    /// completion candidate. Repeated staging reuses the original occurrence;
    /// changed safe facts produce a new observation, never an in-place edit.
    /// Frozen accepted candidates keep their existing source and cannot borrow
    /// a replacement live context. Failed groups select only captured backing.
    pub(super) fn retain_completion_settlement_evidence(
        &mut self,
        turn_id: &str,
        evidence: &str,
    ) -> Result<Option<mez_agent::ContextBlockRecord>> {
        let group = self
            .agent
            .completion_settlements
            .groups
            .get(turn_id)
            .ok_or_else(|| MezError::invalid_state("completion evidence group is unavailable"))?;
        if !matches!(
            group.transcript,
            CompletionTranscriptTarget::Original { .. }
        ) || group
            .provider_handoff
            .as_ref()
            .is_some_and(|owner| owner.rejection.is_some() && owner.source_frozen)
        {
            return Ok(None);
        }
        let original = group.group.clone();
        let original_receipt = group.replay_owner.clone();
        let conversation = mez_agent::ReplayConversationId::new(&group.turn.conversation_id)?;
        let failed = group.failed_backing.is_some();
        let execution_present = self.agent_turn_executions().contains_key(turn_id)
            || group.correction_execution.as_ref().is_some();
        let context = if failed {
            self.agent
                .completion_settlements
                .groups
                .get_mut(turn_id)
                .and_then(|group| group.failed_backing.as_mut())
                .and_then(|backing| match &mut backing.context {
                    CompletionCapture::Captured(context) => context.as_mut(),
                    CompletionCapture::NotCaptured => None,
                })
        } else if execution_present {
            self.agent_turn_contexts_mut().get_mut(turn_id)
        } else {
            None
        };
        let Some(context) = context.filter(|context| context.has_replay_owner()) else {
            return Ok(None);
        };
        let original_receipt = original_receipt.ok_or_else(|| {
            MezError::conflict("completion observation original source fence is missing")
        })?;
        if context.replay_execution_append_owner(&original) != Some(&original_receipt) {
            // Terminal finalization may have already retired this exact group.
            // Its actual closure proves custody; descriptive cold completion,
            // renewed protection and newer appends cannot satisfy this check.
            let owner =
                mez_agent::ReplayEpochContext::from_owned_context(conversation, context.clone())?;
            if !owner.execution_group_finished(&original_receipt) {
                return Err(MezError::conflict(
                    "completion observation original source fence changed",
                ));
            }
        }
        let digest = Sha256::digest(evidence.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let observation = mez_agent::ContextExecutionGroupId::new(format!(
            "{}/settlement/{digest}",
            original.as_str(),
        ))?;
        let block = mez_agent::ContextBlock::evidence_event(
            ContextSourceKind::CommittedEvidence,
            "completion settlement evidence v1",
            evidence,
        );
        let mut existing = context
            .chronology()
            .iter()
            .filter(|event| event.execution_group_id() == Some(&observation));
        if let Some(event) = existing.next() {
            if existing.next().is_some() || event.block() != &block {
                return Err(MezError::conflict(
                    "completion observation differs from its original source",
                ));
            }
            return Ok(Some(event.record()));
        }
        // Use the observation port, as for canonical recovery diagnostics.
        // These facts have no pending assistant/native execution to close.
        // The original action group's captured fence remains unchanged.
        context.append_conversation_events([mez_agent::ContextConversationAppend::evidence(
            block.source,
            block.label,
            block.content,
            observation,
            None,
            true,
        )])?;
        context
            .chronology()
            .last()
            .map(|event| Some(event.record()))
            .ok_or_else(|| MezError::invalid_state("completion observation was not retained"))
    }
}
