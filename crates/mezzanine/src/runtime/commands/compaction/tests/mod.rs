//! Regression tests for private manual compaction projections.
//!
//! These tests exercise valid typed context without exposing owner APIs or
//! relaxing canonical causal validation for runtime consumers.

use super::*;

mod replay;

impl RuntimeSessionService {
    /// Measures an initial observed-input candidate through the production
    /// refreshed-request estimator without publishing its prospective epoch.
    /// Focused fixtures use this to place a real input cap between two summaries
    /// as prompt/tool contracts grow. Missing or staged work is a fixture error;
    /// normal source validation and provider-estimation errors propagate.
    pub(crate) fn observed_compaction_candidate_input_tokens_for_tests(
        &mut self,
        pane_id: &str,
        summary: &str,
    ) -> Result<usize> {
        let task = self
            .pending_agent_compaction_task_for_tests(pane_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("expected pending compaction fixture"))?;
        let RuntimeAgentCompactionTarget::ActiveTurn {
            turn_id,
            plan,
            trigger:
                RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                    observed_input_tokens,
                    ..
                },
            staged: None,
            ..
        } = &task.target
        else {
            return Err(MezError::invalid_state(
                "expected initial observed-input fixture",
            ));
        };
        let turn = self.agent_turn_ledger().turn(turn_id).cloned().unwrap();
        let context = self.agent_turn_contexts().get(turn_id).cloned().unwrap();
        let profile = self.agent_turn_model_profile(turn_id).cloned().unwrap();
        let (compacted, _) = apply_model_context_compaction_plan(context.clone(), plan, summary)?;
        let projection =
            self.prospective_observed_compaction_epoch(&task, &context, plan, summary)?;
        let provider = self
            .provider_registry()
            .provider(&profile.provider)
            .unwrap();
        let api = resolve_provider_api(&provider.kind, provider.api.as_deref())?;
        let options = runtime_effective_provider_options(provider, &profile);
        self.validate_observed_input_compaction_refresh_candidate(
            &task,
            &turn,
            compacted,
            summary,
            projection,
            *observed_input_tokens,
            &profile,
            &options,
            api,
            true,
        )
        .map(|(tokens, _, _)| tokens)
    }
}
