//! Complete ordinary-request acceptance for a canonical frozen source.
//!
//! Hypothetical heads are sizing-only values: they grant no publication receipt
//! and never replace live source. The persistence worker allocates and retains
//! the actual accepted head. Rejected auxiliary input is split into temporary
//! chunks while its canonical source selection and exact raw tail stay frozen.

use super::super::{
    RuntimeAgentCompactionTarget, RuntimeAgentCompactionTask, assemble_model_request,
    resolve_provider_api, runtime_compaction_safe_input_limit, runtime_effective_provider_options,
    runtime_rebuild_active_turn_compaction_request,
};
use super::*;

struct CandidateSize {
    tokens: usize,
    safe_tokens: usize,
    wire_bytes: usize,
}

impl RuntimeSessionService {
    /// Measures the actual canonical continuation without publishing or
    /// changing the frozen claim. Test calibration uses this same production
    /// estimator before configuring an independent operation's input cap.
    #[cfg(test)]
    pub(in crate::runtime::commands::compaction) fn replay_compaction_candidate_input_tokens_for_tests(
        &mut self,
        task: &RuntimeAgentCompactionTask,
        summary: &str,
    ) -> Result<usize> {
        self.validate_replay_compaction_task(task)?;
        let source = task
            .replay_source
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("canonical candidate fixture has no source"))?;
        self.replay_compaction_candidate_size(task, source, summary)
            .map(|size| size.tokens)
    }

    /// Checks the entire actual continuation before any publication write.
    /// A smaller advisory target never overrides bounded-output acceptance;
    /// retries must reduce both the complete estimate and issued ceiling.
    pub(in crate::runtime::commands::compaction) fn accept_replay_compaction_candidate(
        &mut self,
        task: &mut RuntimeAgentCompactionTask,
        summary: &str,
    ) -> Result<bool> {
        let source = task
            .replay_source
            .clone()
            .ok_or_else(|| MezError::invalid_state("canonical candidate source disappeared"))?;
        let size = self.replay_compaction_candidate_size(task, &source, summary)?;
        if let RuntimeAgentCompactionTarget::ActiveTurn {
            rejected_request_bytes: Some(rejected),
            ..
        } = &task.target
            && size.wire_bytes >= *rejected
        {
            return Err(MezError::invalid_state(format!(
                "context-limit recovery could not produce a smaller OpenAI Responses request: rejected_request_bytes={rejected} retry_request_bytes={}",
                size.wire_bytes
            )));
        }
        if size.tokens <= size.safe_tokens {
            return Ok(true);
        }
        let RuntimeAgentCompactionTarget::ActiveTurn {
            final_request_retry,
            completed_summaries,
            pending_blocks,
            synthesis_source_bytes,
            ..
        } = &mut task.target
        else {
            return Err(MezError::invalid_state(
                "canonical continuation lost its active target",
            ));
        };
        let limit = size.safe_tokens;
        let summary_tokens = mez_agent::provider_text_input_token_estimate(summary);
        let next = summary_tokens
            .saturating_sub(size.tokens.saturating_sub(limit))
            .min(summary_tokens / 2)
            // A fixed amount of non-summary context can make the calculated
            // fitting target identical for several genuinely smaller outputs.
            // Reduce the issued guidance independently; actual size must also
            // decrease, so neither repeated output nor a zero target can loop.
            .min(
                final_request_retry
                    .summary_ceiling
                    .map_or(usize::MAX, |previous| previous / 2),
            );
        if next > 0
            && final_request_retry
                .summary_ceiling
                .is_none_or(|previous| next < previous)
            && final_request_retry
                .last_input_tokens
                .is_none_or(|previous| size.tokens < previous)
        {
            final_request_retry.attempts = final_request_retry.attempts.saturating_add(1);
            final_request_retry.summary_ceiling = Some(next);
            final_request_retry.last_input_tokens = Some(size.tokens);
            completed_summaries.clear();
            pending_blocks.clear();
            *synthesis_source_bytes = None;
            task.summary_correction.reset_for_candidate_retry();
            runtime_rebuild_active_turn_compaction_request(task, source.input_blocks())?;
            self.queue_agent_compaction_task(task.clone());
            return Ok(false);
        }
        Err(MezError::invalid_state(format!(
            "canonical compaction candidate cannot reduce the complete request to its safe allowance: estimated_input_tokens={} safe_input_tokens={limit}",
            size.tokens
        )))
    }

    /// Renders real stable policy, current tools/controls and native projection
    /// around the candidate. Metadata-only IDs stay outside model source. The
    /// placeholder has the same shape as the fresh lineage installed on actual
    /// adoption, and cannot become a live cache or publication identity.
    fn replay_compaction_candidate_size(
        &mut self,
        task: &RuntimeAgentCompactionTask,
        source: &RuntimeCompactionReplaySource,
        summary: &str,
    ) -> Result<CandidateSize> {
        let turn_id = source
            .turn_id()
            .ok_or_else(|| MezError::invalid_state("canonical continuation has no active turn"))?;
        let turn = self
            .agent_turn_ledger()
            .turn(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("canonical continuation turn disappeared"))?;
        let current = ReplayEpochContext::from_owned_context(
            ReplayConversationId::new(&task.conversation_id)?,
            source.current_body(self)?,
        )?;
        let preview = current.prepare_compaction_publication(
            source.plan.clone(),
            ReplayHeadPublication::Summary(summary.into()),
        )?;
        let mut context = preview.candidate().clone().into_owned_context();
        let mut metadata = context.metadata().clone();
        metadata.prompt_cache_lineage_id =
            Some("compaction-00000000-0000-0000-0000-000000000000".into());
        context.set_metadata(metadata);
        let profile = &task.model_profile;
        let (prepared, tools) = self.prepare_agent_turn_model_context(
            &turn,
            context,
            &self.mcp_registry().prompt_summary(),
            profile,
        )?;
        let provider = self
            .provider_registry()
            .provider(&profile.provider)
            .ok_or_else(|| MezError::config("canonical continuation provider is unavailable"))?;
        let api = resolve_provider_api(&provider.kind, provider.api.as_deref())?;
        let options = runtime_effective_provider_options(provider, profile);
        let mut request =
            assemble_model_request(profile, api, &turn, &prepared.to_agent_context())?;
        let (actions, interaction) = self.agent_provider_request_control_for_turn(&turn)?;
        mez_agent::apply_model_request_control(&mut request, actions, interaction);
        mez_agent::apply_default_action_gates(
            &mut request,
            &tools,
            self.runtime_persistent_memory_enabled(),
            super::super::super::runtime_issues_enabled(self),
        );
        let stream = match &task.target {
            RuntimeAgentCompactionTarget::ActiveTurn {
                rejected_request_stream,
                ..
            } => rejected_request_stream.unwrap_or(true),
            RuntimeAgentCompactionTarget::Conversation => true,
        };
        let estimate = mez_agent::provider_request_input_estimate(&request, api, &options, stream)?;
        let safe_tokens = runtime_compaction_safe_input_limit(
            profile.max_input_tokens(),
            profile.context_window_tokens(),
            profile.max_output_tokens(),
        )
        .ok_or_else(|| MezError::invalid_state("canonical continuation has no input allowance"))?;
        Ok(CandidateSize {
            tokens: estimate.input_tokens,
            wire_bytes: estimate.wire_bytes,
            safe_tokens,
        })
    }
}
