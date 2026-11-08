//! Complete prospective manual requests from one authoritative history projection.
//!
//! Candidate sizing uses the canonical frozen head/tail plan without I/O or a
//! second physical-row replacement algorithm.
//! Nonhistorical policy, optional memory and exact references retain metadata;
//! native groups are restored only within the new history before composition.
//! Provider tools, controls and output/headroom rules still determine fit. This
//! module neither dispatches provider work nor publishes an epoch or replays an
//! action; frozen source and mutation-lane validation remain with their owners.

use super::*;

impl RuntimeSessionService {
    /// Previews only the original canonical source. Source custody, task and
    /// model freshness are checked without rebuilding an archive-row projection
    /// or creating another summary identity/publication authority.
    pub(crate) fn manual_compaction_candidate_context(
        &mut self,
        task: &RuntimeAgentCompactionTask,
        summary: &str,
    ) -> Result<AgentContext> {
        self.manual_replay_compaction_candidate_context(task, summary)
    }

    /// Exposes the actual runtime request estimator to focused fit regressions.
    /// Includes tools, provider controls and the existing allowance; errors
    /// propagate unchanged, and no request is dispatched or epoch published.
    #[cfg(test)]
    pub(crate) fn manual_compaction_candidate_size_for_tests(
        &self,
        task: &RuntimeAgentCompactionTask,
        candidate_context: AgentContext,
    ) -> Result<(usize, usize, String)> {
        self.estimate_manual_compaction_candidate(task, candidate_context)
    }

    /// Sizes the complete provider request, including runtime tool augmentation,
    /// configured provider controls and the existing output/headroom allowance.
    pub(super) fn estimate_manual_compaction_candidate(
        &self,
        task: &RuntimeAgentCompactionTask,
        candidate_context: AgentContext,
    ) -> Result<(usize, usize, String)> {
        let mcp_summary = self.mcp_registry().prompt_summary();
        let configured_servers = self.integration.always_exposed_mcp_servers().to_vec();
        let available_mcp_tools = mez_agent::invoked_mcp_tools_for_context_with_configured(
            &candidate_context,
            &mcp_summary,
            &configured_servers,
        );
        let turn = AgentTurnRecord {
            turn_id: format!("compact-candidate-{}", task.compaction_epoch),
            conversation_id: task.conversation_id.clone(),
            agent_id: format!("agent-{}", task.pane_id),
            pane_id: task.pane_id.clone(),
            trigger: mez_agent::AgentTurnTrigger::UserPrompt,
            started_at_unix_seconds: current_unix_seconds(),
            deadline_at_unix_millis: 0,
            policy_profile: "runtime".to_string(),
            model_profile: task.model_profile_name.clone(),
            parent_turn_id: None,
            state: AgentTurnState::Queued,
            cooperation_mode: None,
            initial_capability: None,
        };
        let provider_config = self
            .provider_registry()
            .provider(&task.model_profile.provider)
            .ok_or_else(|| {
                MezError::config(format!(
                    "provider `{}` for manual compaction candidate is not configured",
                    task.model_profile.provider
                ))
            })?;
        let api = resolve_provider_api(&provider_config.kind, provider_config.api.as_deref())?;
        let provider_options =
            runtime_effective_provider_options(provider_config, &task.model_profile);
        let mut request =
            assemble_model_request(&task.model_profile, api, &turn, &candidate_context)?;
        mez_agent::apply_model_request_control(
            &mut request,
            Some(task.request.allowed_actions.clone()),
            Some(ModelInteractionKind::ActionExecution),
        );
        mez_agent::apply_default_action_gates(
            &mut request,
            &available_mcp_tools,
            self.runtime_persistent_memory_enabled(),
            super::super::runtime_issues_enabled(self),
        );
        let estimate =
            mez_agent::provider_request_input_estimate(&request, api, &provider_options, true)?;
        let safe_limit = runtime_compaction_safe_input_limit(
            task.model_profile.max_input_tokens(),
            task.model_profile.context_window_tokens(),
            task.model_profile.max_output_tokens(),
        )
        .ok_or_else(|| {
            MezError::invalid_state(
                "manual compaction candidate has no configured provider input allowance",
            )
        })?;
        let diagnostic = runtime_compaction_candidate_size_diagnostic(
            &candidate_context,
            Some(mez_agent::ProviderBudgetProjection::new(
                api,
                &task.model_profile.provider,
            )),
            estimate.input_tokens,
            safe_limit,
        );
        Ok((estimate.input_tokens, safe_limit, diagnostic))
    }
}
