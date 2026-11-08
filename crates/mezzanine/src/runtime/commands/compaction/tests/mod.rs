//! Regression tests for private manual compaction projections.
//!
//! These tests exercise valid typed context without exposing owner APIs or
//! relaxing canonical causal validation for runtime consumers.

use super::*;

mod replay;
mod retention;

impl RuntimeSessionService {
    /// Measures an initial observed-input candidate through the production
    /// refreshed-request estimator without publishing its prospective epoch.
    /// Focused fixtures use this to place a real input cap between two summaries
    /// as prompt/tool contracts grow. Missing canonical work is a fixture error;
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
        self.replay_compaction_candidate_input_tokens_for_tests(&task, summary)
    }
}
