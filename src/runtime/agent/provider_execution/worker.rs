//! Provider request execution and model dispatch.

use super::super::*;
#[cfg(test)]
use mez_agent::ProviderErrorRetryClass;

impl RuntimeSessionService {
    /// Applies provider output progress through the transport-neutral transition contract.
    pub(crate) fn apply_agent_provider_output_progress_transition(
        &mut self,
        pane_id: &str,
        lines: &[String],
    ) -> crate::runtime::RuntimeTransition {
        let _ = self.append_agent_shell_output_status_lines_to_terminal_buffer(pane_id, lines);
        crate::runtime::RuntimeTransition {
            applied: true,
            side_effects: Vec::new(),
        }
    }

    /// Applies provider completion through the transport-neutral transition contract.
    pub(crate) async fn apply_agent_provider_completed_transition(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        execution: AgentTurnExecution,
    ) -> Result<crate::runtime::RuntimeTransition> {
        let applied = self
            .apply_agent_provider_completed_event(agent_id, turn_id, execution)
            .await?;
        Ok(self.runtime_transition_with_render(
            applied,
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
        self.drain_pending_agent_turn_steering_context(&turn)?;
        let context = self
            .agent_turn_contexts()
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let mcp_summary = self.mcp_registry.prompt_summary();
        let context = append_mcp_context(context, &mcp_summary)?;
        let available_mcp_tools = invoked_mcp_tools_for_context(&context, &mcp_summary);
        self.agent_turn_contexts_mut()
            .insert(turn_id.to_string(), context.clone());
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
                context.blocks.len()
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
        if self.agent_debug_enabled(&turn.pane_id) {
            match assemble_model_request(&model_profile, &turn, &context) {
                Ok(mut request) => {
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
        let permission_policy = self.permission_policy_for_turn(&turn);
        let loop_allowed_actions = turn
            .initial_capability
            .map(mez_agent::AllowedActionSet::for_capability);
        let mut provider_context = context;
        let mut context_limit_recovery_attempts = 0u32;
        let mut output_limit_recovery_attempts = 0u32;
        let mut execution = loop {
            let mut provider_ledger = AgentTurnLedger::new(false);
            let runner = AgentTurnRunner {
                provider,
                model_profile: model_profile.clone(),
                permissions: &crate::permissions::ProductPermissionPlanning::new(
                    &permission_policy,
                    &self.session_approvals,
                    path_scopes.as_ref(),
                ),
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
                loop_allowed_actions.clone(),
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
                            provider_context = self
                                .agent_turn_contexts()
                                .get(turn_id)
                                .cloned()
                                .ok_or_else(|| {
                                    MezError::invalid_state(
                                        "runtime agent turn context is unavailable",
                                    )
                                })?;
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
                            provider_context = self
                                .agent_turn_contexts()
                                .get(turn_id)
                                .cloned()
                                .ok_or_else(|| {
                                    MezError::invalid_state(
                                        "runtime agent turn context is unavailable",
                                    )
                                })?;
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
        let context = self
            .agent_turn_contexts()
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let mcp_summary = self.mcp_registry.prompt_summary();
        let context = append_mcp_context(context, &mcp_summary)?;
        let available_mcp_tools = invoked_mcp_tools_for_context(&context, &mcp_summary);
        self.agent_turn_contexts_mut()
            .insert(turn_id.to_string(), context.clone());
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
                context.blocks.len()
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
        let permission_policy = self.permission_policy_for_turn(&turn);
        let loop_allowed_actions = turn
            .initial_capability
            .map(mez_agent::AllowedActionSet::for_capability);
        let mut provider_context = context;
        let mut context_limit_recovery_attempts = 0u32;
        let mut output_limit_recovery_attempts = 0u32;
        let mut execution = loop {
            let mut provider_ledger = AgentTurnLedger::new(false);
            let runner = AgentTurnRunner {
                provider,
                model_profile: model_profile.clone(),
                permissions: &crate::permissions::ProductPermissionPlanning::new(
                    &permission_policy,
                    &self.session_approvals,
                    path_scopes.as_ref(),
                ),
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
                loop_allowed_actions.clone(),
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
                            provider_context = self
                                .agent_turn_contexts()
                                .get(turn_id)
                                .cloned()
                                .ok_or_else(|| {
                                    MezError::invalid_state(
                                        "runtime agent turn context is unavailable",
                                    )
                                })?;
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
                            provider_context = self
                                .agent_turn_contexts()
                                .get(turn_id)
                                .cloned()
                                .ok_or_else(|| {
                                    MezError::invalid_state(
                                        "runtime agent turn context is unavailable",
                                    )
                                })?;
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
        )
        .await
    }
}
