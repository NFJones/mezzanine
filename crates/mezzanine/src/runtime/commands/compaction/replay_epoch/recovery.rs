//! Read-only ordinary-request sizing for focused test calibration.
//!
//! Hypothetical heads are sizing-only values: they grant no publication receipt
//! and never replace live source. The persistence worker allocates and retains
//! the actual accepted head. No size reported here admits work, accepts a
//! summary, or changes its frozen source/tail policy.

use super::super::{
    RuntimeAgentCompactionTask, assemble_model_request, resolve_provider_api,
    runtime_effective_provider_options,
};
use super::*;

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
    }

    fn replay_compaction_candidate_size(
        &mut self,
        task: &RuntimeAgentCompactionTask,
        source: &RuntimeCompactionReplaySource,
        summary: &str,
    ) -> Result<usize> {
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
        let stream = task
            .compaction_request_shape
            .as_ref()
            .is_none_or(|shape| shape.2);
        let estimate = mez_agent::provider_request_input_estimate(&request, api, &options, stream)?;
        Ok(estimate.input_tokens)
    }
}
