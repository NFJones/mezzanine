//! Provider request execution and model dispatch.

use super::super::{
    AgentId, AgentTurnExecution, AgentTurnState, MezError, Result, RuntimeSessionService,
};
#[cfg(test)]
use super::super::{
    AgentTurnLedger, HookEvent, ModelProfile, RUNTIME_PROVIDER_CONTEXT_LIMIT_RETRY_LIMIT,
    RUNTIME_PROVIDER_OUTPUT_LIMIT_RETRY_LIMIT, assemble_model_request,
    runtime_agent_turn_start_hook_payload, runtime_execute_auto_sizing_with_provider,
    runtime_mezzanine_error_code,
};
#[cfg(test)]
use crate::integrations::agent::actions::AgentTurnRunner;
#[cfg(test)]
use crate::integrations::agent::provider::{ModelProvider, provider_error_retry_class};

#[cfg(test)]
use mez_agent::ProviderErrorRetryClass;

impl RuntimeSessionService {
    /// Completes queued context-limit compaction before a synchronous test
    /// provider retries the original turn.
    ///
    /// Production provider workers dispatch compaction asynchronously and
    /// resume through actor-owned completion events. These compatibility
    /// helpers use the supplied test provider directly, but preserve the same
    /// invariant: durable context must change before the rejected request is
    /// rebuilt and sent again.
    #[cfg(test)]
    fn complete_synchronous_context_limit_compaction<P: ModelProvider>(
        &mut self,
        turn_id: &str,
        provider: &P,
    ) -> Result<()> {
        let before = self
            .agent_turn_contexts()
            .get(turn_id)
            .map(|context| context.blocks().to_vec())
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        loop {
            let pane_id = self
                .agent_turn_ledger()
                .turns()
                .iter()
                .find(|turn| turn.turn_id == turn_id)
                .map(|turn| turn.pane_id.clone())
                .ok_or_else(|| {
                    MezError::invalid_state("compaction recovery turn is unavailable")
                })?;
            let task = self
                .take_pending_agent_compaction_task(&pane_id)
                .ok_or_else(|| {
                    MezError::invalid_state("context-limit recovery queued no compaction task")
                })?;
            if task.resume_turn_id.as_deref() != Some(turn_id) {
                return Err(MezError::invalid_state(
                    "context-limit compaction does not resume the rejected turn",
                ));
            }
            let request = task.request.clone();
            self.claim_agent_compaction_task_state(&pane_id, task);
            match provider.send_request(&request) {
                Ok(response) => {
                    self.apply_agent_compaction_completed_event(&pane_id, response)?;
                }
                Err(error) => {
                    self.apply_agent_compaction_failed_event(
                        &pane_id,
                        runtime_mezzanine_error_code(error.kind()),
                        error.message(),
                        error.provider_failure_json(),
                    )?;
                    return Err(error);
                }
            }
            if !self.agent_is_compacting(&pane_id) {
                break;
            }
        }
        self.remove_pending_agent_provider_task(turn_id);
        let after = self
            .agent_turn_contexts()
            .get(turn_id)
            .map(|context| context.blocks())
            .ok_or_else(|| MezError::invalid_state("compacted turn context is unavailable"))?;
        if after == before.as_slice() {
            return Err(MezError::invalid_state(
                "context-limit compaction did not change durable context",
            ));
        }
        Ok(())
    }

    /// Applies provider output progress through the transport-neutral transition contract.
    pub(crate) fn apply_agent_provider_output_progress_transition(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        pane_id: &str,
        preview: &mez_agent::ProvisionalSayPreview,
    ) -> crate::runtime::RuntimeTransition {
        let current = self.agent_turn_ledger().turns().iter().any(|turn| {
            turn.turn_id == turn_id
                && turn.agent_id == agent_id.as_str()
                && turn.pane_id == pane_id
                && turn.state == AgentTurnState::Running
        });
        if !current || !self.agent_provider_task_is_claimed(turn_id) {
            return crate::runtime::RuntimeTransition::default();
        }
        let applied = self
            .append_agent_provider_say_preview_to_terminal_buffer(pane_id, preview)
            .is_ok();
        self.runtime_transition_with_render(
            applied,
            Some(crate::runtime::RenderInvalidationReason::FullRedraw),
        )
    }

    /// Applies provider completion through the transport-neutral transition contract.
    pub(crate) async fn apply_agent_provider_completed_transition(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        execution: AgentTurnExecution,
    ) -> Result<crate::runtime::RuntimeTransition> {
        let pane_id = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .map(|turn| turn.pane_id.clone());
        if let Some(pane_id) = pane_id {
            self.clear_agent_shell_output_status_line(&pane_id)?;
        }
        let applied = self
            .apply_agent_provider_completed_event(agent_id, turn_id, execution)
            .await?;
        Ok(self.runtime_transition_with_render(
            applied,
            Some(crate::runtime::RenderInvalidationReason::FullRedraw),
        ))
    }

    /// Applies worker-settled provider persistence through actor-owned state.
    pub(crate) async fn apply_agent_provider_persistence_settled_transition(
        &mut self,
        outcome: crate::runtime::RuntimeAgentProviderPersistenceOutcome,
    ) -> Result<crate::runtime::RuntimeTransition> {
        let turn_id = outcome.turn.turn_id.clone();
        if !self.clear_agent_provider_persistence_pending(&turn_id) {
            return Ok(crate::runtime::RuntimeTransition::default());
        }
        let current = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id);
        let current_execution_owns_turn = current.is_some_and(|turn| {
            turn.state == AgentTurnState::Running
                && turn.agent_id == outcome.turn.agent_id
                && turn.conversation_id == outcome.turn.conversation_id
                && turn.pane_id == outcome.turn.pane_id
        });
        let conversation_still_owns_pane = self
            .agent_shell_store()
            .get(&outcome.turn.pane_id)
            .is_some_and(|session| session.session_id == outcome.turn.conversation_id);
        if !current_execution_owns_turn || !conversation_still_owns_pane {
            return Ok(crate::runtime::RuntimeTransition::default());
        }
        let turn = outcome.turn.clone();
        let model_profile = outcome.model_profile.clone();
        let provider_id = outcome.provider_id.clone();
        let applied = match self.apply_agent_provider_persistence_outcome(outcome).await {
            Ok(_) => true,
            Err(error) => {
                self.fail_agent_turn_after_provider_completion_application_error(
                    &turn,
                    &provider_id,
                    Some(&model_profile),
                    &error,
                );
                true
            }
        };
        Ok(self.runtime_transition_with_render(
            applied,
            Some(crate::runtime::RenderInvalidationReason::FullRedraw),
        ))
    }

    /// Contains a provider-persistence worker failure to the affected turn.
    pub(crate) fn apply_agent_provider_persistence_failed_transition(
        &mut self,
        turn_id: &str,
        provider_id: &str,
        kind: &str,
        message: &str,
    ) -> Result<crate::runtime::RuntimeTransition> {
        if !self.clear_agent_provider_persistence_pending(turn_id) {
            return Ok(crate::runtime::RuntimeTransition::default());
        }
        let Some(turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)
            .cloned()
        else {
            return Ok(crate::runtime::RuntimeTransition::default());
        };
        let model_profile = self.agent.agent_turn_model_profiles.get(turn_id).cloned();
        let error = MezError::invalid_state(format!(
            "provider persistence settlement failed ({kind}): {message}"
        ));
        self.fail_agent_turn_after_provider_completion_application_error(
            &turn,
            provider_id,
            model_profile.as_ref(),
            &error,
        );
        Ok(self.runtime_transition_with_render(
            true,
            Some(crate::runtime::RenderInvalidationReason::FullRedraw),
        ))
    }

    /// Runs the execute agent turn with provider operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn execute_agent_turn_with_provider<P: ModelProvider>(
        &mut self,
        turn_id: &str,
        provider: &P,
        mut model_profile: ModelProfile,
    ) -> Result<AgentTurnExecution> {
        self.require_live()?;
        let turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        if turn.state != AgentTurnState::Running {
            return Err(MezError::conflict(
                "only running runtime agent turns can execute through a provider",
            ));
        }
        self.agent
            .agent_turn_model_profiles
            .insert(turn_id.to_string(), model_profile.clone());
        if let Some(step_index) = self.macro_judge_step_index_for_turn(turn_id) {
            return self.execute_macro_judge_with_provider(
                provider,
                &turn,
                &model_profile,
                step_index,
            );
        }
        self.refresh_agent_turn_project_guidance_context(&turn)?;
        let durable = self
            .agent_turn_contexts()
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let mcp_summary = self.mcp_registry().prompt_summary();
        let (prepared_context, available_mcp_tools) =
            self.prepare_agent_turn_model_context(&turn, durable, &mcp_summary, &model_profile)?;
        let context = prepared_context.into_agent_context();
        let mut routing_token_usage_by_model = std::collections::BTreeMap::new();
        if let Some(auto_sizing) =
            self.runtime_auto_sizing_dispatch_for_turn(&turn, &model_profile)?
        {
            let auto_sizing_execution = match runtime_execute_auto_sizing_with_provider(
                provider,
                &auto_sizing,
                &turn,
                &context,
            ) {
                Ok(execution) => execution,
                Err(error) => {
                    self.append_agent_trace_provider_error(
                        &turn,
                        provider.provider_id(),
                        &auto_sizing.router_profile,
                        &error,
                    )?;
                    self.append_provider_request_failure_audit(
                        &turn,
                        &auto_sizing.router_profile,
                        provider.provider_id(),
                        &error,
                    )?;
                    self.integration
                        .runtime_metrics_mut()
                        .record_provider_failure();
                    self.fail_agent_turn_for_provider_error(
                        &turn,
                        provider.provider_id(),
                        &auto_sizing.router_profile,
                        &error,
                    )?;
                    return Err(error);
                }
            };
            routing_token_usage_by_model = auto_sizing_execution.token_usage_by_model();
            self.record_auto_sizing_outcome(
                &turn,
                &auto_sizing_execution.selected_profile,
                auto_sizing_execution.decision.as_ref(),
                auto_sizing_execution.fallback.as_deref(),
            )?;
            model_profile = auto_sizing_execution.selected_profile;
            self.agent
                .agent_turn_model_profiles
                .insert(turn_id.to_string(), model_profile.clone());
        }
        if let Some(block) = self.run_configured_pre_action_hooks(
            HookEvent::AgentTurnStart,
            &runtime_agent_turn_start_hook_payload(&turn, &model_profile),
        )? {
            self.fail_agent_turn_for_hook_block(&turn, &model_profile, &block)?;
            return Err(MezError::forbidden(format!(
                "agent turn blocked by hook `{}`: {}",
                block.hook_id, block.message
            )));
        }
        let available_mcp_servers = mcp_summary
            .available_tools
            .iter()
            .map(|tool| tool.server_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        self.append_provider_request_audit(
            &turn,
            &model_profile,
            provider.provider_id(),
            "started",
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "provider_request started provider={} model={} context_blocks={}",
                provider.provider_id(),
                model_profile.model,
                context.blocks().len()
            ),
        )?;
        let (allowed_actions, interaction_kind) =
            self.agent_provider_request_control_for_turn(&turn);
        self.record_runtime_provider_request_shape_for_context(
            &model_profile,
            &turn,
            &context,
            &available_mcp_tools,
            self.runtime_persistent_memory_enabled(),
            super::super::issues::runtime_issues_enabled(self),
        );
        if self.agent_debug_enabled(&turn.pane_id) {
            match assemble_model_request(&model_profile, &turn, &context) {
                Ok(mut request) => {
                    mez_agent::apply_model_request_control(
                        &mut request,
                        allowed_actions.clone(),
                        interaction_kind,
                    );
                    mez_agent::apply_default_action_gates(
                        &mut request,
                        &available_mcp_tools,
                        self.runtime_persistent_memory_enabled(),
                        super::super::issues::runtime_issues_enabled(self),
                    );
                    self.append_agent_trace_maap_request(&turn, &request)?;
                }
                Err(error) => {
                    let error = crate::error::MezError::from(error);
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "maap request trace unavailable error_kind={} error={}",
                            runtime_mezzanine_error_code(error.kind()),
                            error.message()
                        ),
                    )?;
                }
            }
        }
        self.append_agent_verbose_status_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent: thinking with {} model {}",
                provider.provider_id(),
                model_profile.model
            ),
        )?;
        let subagent_scope = self.subagent_scope_declaration_for_turn(&turn);
        let path_scopes = if subagent_scope.is_some() {
            None
        } else {
            self.path_scopes_for_pane(&turn.pane_id)
        };
        let shell_classification = self.shell_classification_for_pane(&turn.pane_id);
        let permission_policy = self.permission_policy_for_turn(&turn);
        let sandbox_config = self.sandbox_config_for_pane(&turn.pane_id);
        let sandbox_first_local_prompts = crate::runtime::config::bubblewrap_applies_to_policy(
            &sandbox_config,
            &permission_policy,
        );
        let mut provider_context = context;
        let mut context_limit_recovery_attempts = 0u32;
        let mut output_limit_recovery_attempts = 0u32;
        let mut execution = loop {
            let (allowed_actions, interaction_kind) =
                self.agent_provider_request_control_for_turn(&turn);
            let mut provider_ledger = AgentTurnLedger::new(false);
            let runner = AgentTurnRunner {
                provider,
                model_profile: model_profile.clone(),
                permissions: &crate::security::permissions::ProductPermissionPlanning::new(
                    &permission_policy,
                    self.session_approvals(),
                    path_scopes.as_ref(),
                )
                .with_shell_classification(shell_classification.as_str())
                .with_sandbox_first_local_prompts(sandbox_first_local_prompts),
                subagent_scope: subagent_scope.as_ref(),
                subagent_scope_enforcement: &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
                available_mcp_servers: available_mcp_servers.clone(),
                available_mcp_tools: &available_mcp_tools,
                memory_actions_enabled: self.runtime_persistent_memory_enabled(),
                issue_actions_enabled: super::super::issues::runtime_issues_enabled(self),
            };
            match runner.run_turn_ref_with_allowed_actions(
                &mut provider_ledger,
                turn.clone(),
                &provider_context,
                allowed_actions.clone(),
                interaction_kind,
            ) {
                Ok(execution) => break execution,
                Err(error) => {
                    self.append_agent_trace_provider_error(
                        &turn,
                        provider.provider_id(),
                        &model_profile,
                        &error,
                    )?;
                    self.append_provider_request_failure_audit(
                        &turn,
                        &model_profile,
                        provider.provider_id(),
                        &error,
                    )?;
                    if matches!(
                        provider_error_retry_class(&error),
                        ProviderErrorRetryClass::ContextLimit
                    ) && context_limit_recovery_attempts
                        < RUNTIME_PROVIDER_CONTEXT_LIMIT_RETRY_LIMIT
                    {
                        context_limit_recovery_attempts =
                            context_limit_recovery_attempts.saturating_add(1);
                        let agent_id = AgentId::opaque(turn.agent_id.clone()).ok_or_else(|| {
                            MezError::invalid_state("runtime agent turn agent id is invalid")
                        })?;
                        if self.recover_agent_provider_context_limit_failure(
                            &agent_id,
                            turn_id,
                            &error,
                            context_limit_recovery_attempts,
                        )? {
                            self.complete_synchronous_context_limit_compaction(turn_id, provider)?;
                            let durable = self
                                .agent_turn_contexts()
                                .get(turn_id)
                                .cloned()
                                .ok_or_else(|| {
                                    MezError::invalid_state(
                                        "runtime agent turn context is unavailable",
                                    )
                                })?;
                            provider_context = self
                                .prepare_agent_turn_model_context(
                                    &turn,
                                    durable,
                                    &mcp_summary,
                                    &model_profile,
                                )?
                                .0
                                .into_agent_context();
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "provider_request retrying reason=provider_context_limit attempt={context_limit_recovery_attempts}"
                                ),
                            )?;
                            continue;
                        }
                    }
                    if matches!(
                        provider_error_retry_class(&error),
                        ProviderErrorRetryClass::OutputLimit
                    ) && output_limit_recovery_attempts
                        < RUNTIME_PROVIDER_OUTPUT_LIMIT_RETRY_LIMIT
                    {
                        output_limit_recovery_attempts =
                            output_limit_recovery_attempts.saturating_add(1);
                        let agent_id = AgentId::opaque(turn.agent_id.clone()).ok_or_else(|| {
                            MezError::invalid_state("runtime agent turn agent id is invalid")
                        })?;
                        if self.recover_agent_provider_output_limit_failure(
                            &agent_id,
                            turn_id,
                            &error,
                            output_limit_recovery_attempts,
                        )? {
                            let durable = self
                                .agent_turn_contexts()
                                .get(turn_id)
                                .cloned()
                                .ok_or_else(|| {
                                    MezError::invalid_state(
                                        "runtime agent turn context is unavailable",
                                    )
                                })?;
                            provider_context = self
                                .prepare_agent_turn_model_context(
                                    &turn,
                                    durable,
                                    &mcp_summary,
                                    &model_profile,
                                )?
                                .0
                                .into_agent_context();
                            model_profile = self
                                .agent
                                .agent_turn_model_profiles
                                .get(turn_id)
                                .cloned()
                                .ok_or_else(|| {
                                    MezError::invalid_state(
                                        "runtime agent turn model profile is unavailable",
                                    )
                                })?;
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "provider_request retrying reason=provider_output_limit attempt={output_limit_recovery_attempts}"
                                ),
                            )?;
                            continue;
                        }
                    }
                    self.integration
                        .runtime_metrics_mut()
                        .record_provider_failure();
                    self.fail_agent_turn_for_provider_error(
                        &turn,
                        provider.provider_id(),
                        &model_profile,
                        &error,
                    )?;
                    return Err(error);
                }
            }
        };
        execution.routing_token_usage_by_model = routing_token_usage_by_model;
        self.apply_agent_provider_execution(
            &turn,
            &model_profile,
            provider.provider_id(),
            execution,
        )
    }

    /// Runs the execute agent turn with provider async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub async fn execute_agent_turn_with_provider_async<P: ModelProvider>(
        &mut self,
        turn_id: &str,
        provider: &P,
        mut model_profile: ModelProfile,
    ) -> Result<AgentTurnExecution> {
        self.require_live()?;
        let turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::new(crate::error::MezErrorKind::NotFound, "turn not found"))?;
        if turn.state != AgentTurnState::Running {
            return Err(MezError::conflict(
                "only running runtime agent turns can execute through a provider",
            ));
        }
        self.agent
            .agent_turn_model_profiles
            .insert(turn_id.to_string(), model_profile.clone());
        if let Some(step_index) = self.macro_judge_step_index_for_turn(turn_id) {
            return self.execute_macro_judge_with_provider(
                provider,
                &turn,
                &model_profile,
                step_index,
            );
        }
        self.refresh_agent_turn_project_guidance_context(&turn)?;
        let durable = self
            .agent_turn_contexts()
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let mcp_summary = self.mcp_registry().prompt_summary();
        let (prepared_context, available_mcp_tools) =
            self.prepare_agent_turn_model_context(&turn, durable, &mcp_summary, &model_profile)?;
        let context = prepared_context.into_agent_context();
        let mut routing_token_usage_by_model = std::collections::BTreeMap::new();
        if let Some(auto_sizing) =
            self.runtime_auto_sizing_dispatch_for_turn(&turn, &model_profile)?
        {
            let auto_sizing_execution = match runtime_execute_auto_sizing_with_provider(
                provider,
                &auto_sizing,
                &turn,
                &context,
            ) {
                Ok(execution) => execution,
                Err(error) => {
                    self.append_agent_trace_provider_error(
                        &turn,
                        provider.provider_id(),
                        &auto_sizing.router_profile,
                        &error,
                    )?;
                    self.append_provider_request_failure_audit(
                        &turn,
                        &auto_sizing.router_profile,
                        provider.provider_id(),
                        &error,
                    )?;
                    self.integration
                        .runtime_metrics_mut()
                        .record_provider_failure();
                    self.fail_agent_turn_for_provider_error(
                        &turn,
                        provider.provider_id(),
                        &auto_sizing.router_profile,
                        &error,
                    )?;
                    return Err(error);
                }
            };
            routing_token_usage_by_model = auto_sizing_execution.token_usage_by_model();
            self.record_auto_sizing_outcome(
                &turn,
                &auto_sizing_execution.selected_profile,
                auto_sizing_execution.decision.as_ref(),
                auto_sizing_execution.fallback.as_deref(),
            )?;
            model_profile = auto_sizing_execution.selected_profile;
            self.agent
                .agent_turn_model_profiles
                .insert(turn_id.to_string(), model_profile.clone());
        }
        if let Some(block) = self.run_configured_pre_action_hooks(
            HookEvent::AgentTurnStart,
            &runtime_agent_turn_start_hook_payload(&turn, &model_profile),
        )? {
            self.fail_agent_turn_for_hook_block(&turn, &model_profile, &block)?;
            return Err(MezError::forbidden(format!(
                "agent turn blocked by hook `{}`: {}",
                block.hook_id, block.message
            )));
        }
        let available_mcp_servers = available_mcp_tools
            .iter()
            .map(|tool| tool.server_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        self.append_provider_request_audit(
            &turn,
            &model_profile,
            provider.provider_id(),
            "started",
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "provider_request started provider={} model={} context_blocks={}",
                provider.provider_id(),
                model_profile.model,
                context.blocks().len()
            ),
        )?;
        self.record_runtime_provider_request_shape_for_context(
            &model_profile,
            &turn,
            &context,
            &available_mcp_tools,
            self.runtime_persistent_memory_enabled(),
            super::super::issues::runtime_issues_enabled(self),
        );
        let subagent_scope = self.subagent_scope_declaration_for_turn(&turn);
        let path_scopes = if subagent_scope.is_some() {
            None
        } else {
            self.path_scopes_for_pane(&turn.pane_id)
        };
        let shell_classification = self.shell_classification_for_pane(&turn.pane_id);
        let permission_policy = self.permission_policy_for_turn(&turn);
        let sandbox_config = self.sandbox_config_for_pane(&turn.pane_id);
        let sandbox_first_local_prompts = crate::runtime::config::bubblewrap_applies_to_policy(
            &sandbox_config,
            &permission_policy,
        );
        let mut provider_context = context;
        let mut context_limit_recovery_attempts = 0u32;
        let mut output_limit_recovery_attempts = 0u32;
        let mut execution = loop {
            let (allowed_actions, interaction_kind) =
                self.agent_provider_request_control_for_turn(&turn);
            let mut provider_ledger = AgentTurnLedger::new(false);
            let runner = AgentTurnRunner {
                provider,
                model_profile: model_profile.clone(),
                permissions: &crate::security::permissions::ProductPermissionPlanning::new(
                    &permission_policy,
                    self.session_approvals(),
                    path_scopes.as_ref(),
                )
                .with_shell_classification(shell_classification.as_str())
                .with_sandbox_first_local_prompts(sandbox_first_local_prompts),
                subagent_scope: subagent_scope.as_ref(),
                subagent_scope_enforcement: &mez_agent::DEFAULT_SUBAGENT_SCOPE_ENFORCEMENT,
                available_mcp_servers: available_mcp_servers.clone(),
                available_mcp_tools: &available_mcp_tools,
                memory_actions_enabled: self.runtime_persistent_memory_enabled(),
                issue_actions_enabled: super::super::issues::runtime_issues_enabled(self),
            };
            match runner.run_turn_ref_with_allowed_actions(
                &mut provider_ledger,
                turn.clone(),
                &provider_context,
                allowed_actions.clone(),
                interaction_kind,
            ) {
                Ok(execution) => break execution,
                Err(error) => {
                    self.append_agent_trace_provider_error(
                        &turn,
                        provider.provider_id(),
                        &model_profile,
                        &error,
                    )?;
                    self.append_provider_request_failure_audit(
                        &turn,
                        &model_profile,
                        provider.provider_id(),
                        &error,
                    )?;
                    if matches!(
                        provider_error_retry_class(&error),
                        ProviderErrorRetryClass::ContextLimit
                    ) && context_limit_recovery_attempts
                        < RUNTIME_PROVIDER_CONTEXT_LIMIT_RETRY_LIMIT
                    {
                        context_limit_recovery_attempts =
                            context_limit_recovery_attempts.saturating_add(1);
                        let agent_id = AgentId::opaque(turn.agent_id.clone()).ok_or_else(|| {
                            MezError::invalid_state("runtime agent turn agent id is invalid")
                        })?;
                        if self.recover_agent_provider_context_limit_failure(
                            &agent_id,
                            turn_id,
                            &error,
                            context_limit_recovery_attempts,
                        )? {
                            self.complete_synchronous_context_limit_compaction(turn_id, provider)?;
                            let durable = self
                                .agent_turn_contexts()
                                .get(turn_id)
                                .cloned()
                                .ok_or_else(|| {
                                    MezError::invalid_state(
                                        "runtime agent turn context is unavailable",
                                    )
                                })?;
                            provider_context = self
                                .prepare_agent_turn_model_context(
                                    &turn,
                                    durable,
                                    &mcp_summary,
                                    &model_profile,
                                )?
                                .0
                                .into_agent_context();
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "provider_request retrying reason=provider_context_limit attempt={context_limit_recovery_attempts}"
                                ),
                            )?;
                            continue;
                        }
                    }
                    if matches!(
                        provider_error_retry_class(&error),
                        ProviderErrorRetryClass::OutputLimit
                    ) && output_limit_recovery_attempts
                        < RUNTIME_PROVIDER_OUTPUT_LIMIT_RETRY_LIMIT
                    {
                        output_limit_recovery_attempts =
                            output_limit_recovery_attempts.saturating_add(1);
                        let agent_id = AgentId::opaque(turn.agent_id.clone()).ok_or_else(|| {
                            MezError::invalid_state("runtime agent turn agent id is invalid")
                        })?;
                        if self.recover_agent_provider_output_limit_failure(
                            &agent_id,
                            turn_id,
                            &error,
                            output_limit_recovery_attempts,
                        )? {
                            let durable = self
                                .agent_turn_contexts()
                                .get(turn_id)
                                .cloned()
                                .ok_or_else(|| {
                                    MezError::invalid_state(
                                        "runtime agent turn context is unavailable",
                                    )
                                })?;
                            provider_context = self
                                .prepare_agent_turn_model_context(
                                    &turn,
                                    durable,
                                    &mcp_summary,
                                    &model_profile,
                                )?
                                .0
                                .into_agent_context();
                            model_profile = self
                                .agent
                                .agent_turn_model_profiles
                                .get(turn_id)
                                .cloned()
                                .ok_or_else(|| {
                                    MezError::invalid_state(
                                        "runtime agent turn model profile is unavailable",
                                    )
                                })?;
                            self.append_agent_trace_turn_event(
                                &turn.pane_id,
                                &turn.turn_id,
                                &format!(
                                    "provider_request retrying reason=provider_output_limit attempt={output_limit_recovery_attempts}"
                                ),
                            )?;
                            continue;
                        }
                    }
                    self.integration
                        .runtime_metrics_mut()
                        .record_provider_failure();
                    self.fail_agent_turn_for_provider_error(
                        &turn,
                        provider.provider_id(),
                        &model_profile,
                        &error,
                    )?;
                    return Err(error);
                }
            }
        };
        execution.routing_token_usage_by_model = routing_token_usage_by_model;
        self.apply_agent_provider_execution_async(
            &turn,
            &model_profile,
            provider.provider_id(),
            execution,
            false,
        )
        .await
    }
}
