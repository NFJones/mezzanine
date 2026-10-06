//! Immutable manual compactor request rendering after actor-owned context capture.
//!
//! This worker uses the existing redaction, request and output-budget helpers.
//! It neither resolves current policy nor calls a provider; the actor supplies
//! the exact eligible source, model/context/action snapshot and accounting origin.
//! The original source-preparation owner retains capacity and logical identity
//! until actual construction and actor adoption finish. No execution queue or
//! durability boundary competes with the established compactor lifecycle.

use super::*;

/// Owned request inputs for the existing manual compaction task, not authority.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeManualCompactionRequestWork {
    /// Original generation, epoch and finite source-preparation permit.
    pub(crate) owner: RuntimeManualCompactionPreparation,
    profile_name: String,
    profile: ModelProfile,
    context: AgentContext,
    mcp_summary: mez_agent::McpPromptSummary,
    allowed_actions: AllowedActionSet,
    entries: Vec<TranscriptEntry>,
    retained_entries: u64,
    origin: crate::storage::token_usage::AccountingOrigin,
    /// Independent request-rendering barrier, absent from production work.
    #[cfg(test)]
    pub(crate) probe: Option<(
        std::sync::Arc<tokio::sync::Notify>,
        std::sync::Arc<tokio::sync::Notify>,
        std::sync::Arc<tokio::sync::Notify>,
    )>,
}

impl RuntimeManualCompactionRequestWork {
    /// Captures immutable eligible source and live policy projection. This does
    /// not render provider input or acquire any new execution authority.
    #[allow(
        clippy::too_many_arguments,
        reason = "exact owner, model, context, source and accounting inputs are independent"
    )]
    pub(in crate::runtime::commands::compaction) fn capture(
        owner: RuntimeManualCompactionPreparation,
        profile_name: String,
        profile: ModelProfile,
        context: AgentContext,
        mcp_summary: mez_agent::McpPromptSummary,
        allowed_actions: AllowedActionSet,
        entries: Vec<TranscriptEntry>,
        retained_entries: u64,
        origin: crate::storage::token_usage::AccountingOrigin,
    ) -> Self {
        Self {
            owner,
            profile_name,
            profile,
            context,
            mcp_summary,
            allowed_actions,
            entries,
            retained_entries,
            origin,
            #[cfg(test)]
            probe: None,
        }
    }

    /// Adds an explicit fixture gate while leaving immutable product inputs
    /// and generation/capacity ownership untouched.
    #[cfg(test)]
    pub(crate) fn with_probe(
        mut self,
        probe: Option<(
            std::sync::Arc<tokio::sync::Notify>,
            std::sync::Arc<tokio::sync::Notify>,
            std::sync::Arc<tokio::sync::Notify>,
        )>,
    ) -> Self {
        self.probe = probe;
        self
    }

    /// Builds exactly the existing manual task using frozen inputs. No actor
    /// lookup, I/O, provider call or publication occurs. Existing request errors
    /// propagate and the caller must validate original ownership before queuing.
    pub(crate) fn execute_request(&self) -> Result<RuntimeAgentCompactionTask> {
        let context = runtime_compaction_context_without_transcript_blocks(append_mcp_context(
            self.context.clone(),
            &self.mcp_summary,
        )?)?;
        let mut request = runtime_model_compaction_request(
            &self.profile,
            &self.owner.pane_id,
            &self.owner.conversation_id,
            self.owner.transcript_entries,
            &self.entries,
            &context,
            self.allowed_actions.clone(),
        )?;
        let words = request
            .messages
            .last()
            .map(|message| model_context_text_word_count(&message.content))
            .unwrap_or_default()
            .max(1);
        runtime_limit_compaction_summary_output(&mut request, words);
        let retry_source = request
            .messages
            .last()
            .map(|message| message.content.clone());
        Ok(RuntimeAgentCompactionTask {
            task_generation: 0,
            compaction_epoch: self.owner.compaction_epoch,
            pane_id: self.owner.pane_id.clone(),
            accounting_origin: self.origin.clone(),
            conversation_id: self.owner.conversation_id.clone(),
            source: "manual".into(),
            transcript_entries: self.owner.transcript_entries,
            compacted_through_sequence: self.entries.last().map(|entry| entry.sequence),
            frozen_compaction_rows: Vec::new(),
            retained_transcript_entries: self.retained_entries,
            summarized_entries: self.entries.len(),
            model_profile_name: self.profile_name.clone(),
            model_profile: self.profile.clone(),
            request,
            preserve_summary_output_budget: true,
            manual_retry_source: retry_source,
            manual_final_retry: None,
            candidate_context: Some(context),
            resume_turn_id: None,
            target: RuntimeAgentCompactionTarget::Conversation,
            conversation_chunks: None,
            compaction_request_shape: None,
        })
    }
}
