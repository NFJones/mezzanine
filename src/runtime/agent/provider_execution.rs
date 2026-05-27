//! Runtime agent provider execution and completion helpers.
//!
//! This module owns provider turn execution, provider completion ingress,
//! assistant/progress context insertion, and execution settlement after a
//! model response. The surrounding runtime agent facade still owns shared
//! session state, while this module keeps provider-response control flow in
//! one focused implementation unit.

use super::*;

impl RuntimeSessionService {
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
            .agent_turn_ledger
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
        self.agent_turn_model_profiles
            .insert(turn_id.to_string(), model_profile.clone());
        self.refresh_agent_turn_project_guidance_context(&turn)?;
        self.drain_pending_agent_turn_steering_context(&turn)?;
        let context = self
            .agent_turn_contexts
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let mcp_summary = self.mcp_registry.prompt_summary();
        let context = append_mcp_context(context, &mcp_summary)?;
        self.agent_turn_contexts
            .insert(turn_id.to_string(), context.clone());
        let mut routing_token_usage_by_model = std::collections::BTreeMap::new();
        if let Some(auto_sizing) =
            self.runtime_auto_sizing_dispatch_for_turn(&turn, &model_profile)?
        {
            let auto_sizing_execution =
                runtime_execute_auto_sizing_with_provider(provider, &auto_sizing, &turn, &context);
            routing_token_usage_by_model = auto_sizing_execution.token_usage_by_model();
            self.record_auto_sizing_outcome(
                &turn,
                &auto_sizing_execution.selected_profile,
                auto_sizing_execution.decision.as_ref(),
                auto_sizing_execution.fallback.as_deref(),
            )?;
            model_profile = auto_sizing_execution.selected_profile;
            self.agent_turn_model_profiles
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
        let context = self
            .agent_turn_contexts
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
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
            &mcp_summary.available_tools,
        );
        if self.agent_debug_enabled(&turn.pane_id) {
            match assemble_model_request_with_retained_tail_percent(
                &model_profile,
                &turn,
                &context,
                self.agent_compaction_raw_retention_percent,
            ) {
                Ok(mut request) => {
                    request.available_mcp_tools = mcp_summary.available_tools.clone();
                    self.append_agent_trace_maap_request(&turn, &request)?;
                }
                Err(error) => {
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
        let mut provider_context = context;
        let mut context_limit_recovery_attempts = 0u32;
        let mut output_limit_recovery_attempts = 0u32;
        let mut execution = loop {
            let mut provider_ledger = AgentTurnLedger::new(false);
            let runner = AgentTurnRunner {
                provider,
                model_profile: model_profile.clone(),
                permissions: &permission_policy,
                approvals: &self.session_approvals,
                path_scopes: path_scopes.as_ref(),
                subagent_scope: subagent_scope.as_ref(),
                available_mcp_servers: available_mcp_servers.clone(),
                available_mcp_tools: &mcp_summary.available_tools,
            };
            match runner.run_turn(&mut provider_ledger, turn.clone(), provider_context.clone()) {
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
                    if provider_error_is_context_limit_exceeded(
                        error.message(),
                        error.provider_failure_json(),
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
                            provider_context =
                                self.agent_turn_contexts.get(turn_id).cloned().ok_or_else(
                                    || {
                                        MezError::invalid_state(
                                            "runtime agent turn context is unavailable",
                                        )
                                    },
                                )?;
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
                    if provider_error_is_output_limit_exceeded(
                        error.message(),
                        error.provider_failure_json(),
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
                            provider_context =
                                self.agent_turn_contexts.get(turn_id).cloned().ok_or_else(
                                    || {
                                        MezError::invalid_state(
                                            "runtime agent turn context is unavailable",
                                        )
                                    },
                                )?;
                            model_profile = self
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
                    self.runtime_metrics.record_provider_failure();
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
            .agent_turn_ledger
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
        self.agent_turn_model_profiles
            .insert(turn_id.to_string(), model_profile.clone());
        let context = self
            .agent_turn_contexts
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let mcp_summary = self.mcp_registry.prompt_summary();
        let context = append_mcp_context(context, &mcp_summary)?;
        self.agent_turn_contexts
            .insert(turn_id.to_string(), context.clone());
        let mut routing_token_usage_by_model = std::collections::BTreeMap::new();
        if let Some(auto_sizing) =
            self.runtime_auto_sizing_dispatch_for_turn(&turn, &model_profile)?
        {
            let auto_sizing_execution =
                runtime_execute_auto_sizing_with_provider(provider, &auto_sizing, &turn, &context);
            routing_token_usage_by_model = auto_sizing_execution.token_usage_by_model();
            self.record_auto_sizing_outcome(
                &turn,
                &auto_sizing_execution.selected_profile,
                auto_sizing_execution.decision.as_ref(),
                auto_sizing_execution.fallback.as_deref(),
            )?;
            model_profile = auto_sizing_execution.selected_profile;
            self.agent_turn_model_profiles
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
        let context = self
            .agent_turn_contexts
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
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
            &mcp_summary.available_tools,
        );
        let subagent_scope = self.subagent_scope_declaration_for_turn(&turn);
        let path_scopes = if subagent_scope.is_some() {
            None
        } else {
            self.path_scopes_for_pane(&turn.pane_id)
        };
        let permission_policy = self.permission_policy_for_turn(&turn);
        let mut provider_context = context;
        let mut context_limit_recovery_attempts = 0u32;
        let mut output_limit_recovery_attempts = 0u32;
        let mut execution = loop {
            let mut provider_ledger = AgentTurnLedger::new(false);
            let runner = AgentTurnRunner {
                provider,
                model_profile: model_profile.clone(),
                permissions: &permission_policy,
                approvals: &self.session_approvals,
                path_scopes: path_scopes.as_ref(),
                subagent_scope: subagent_scope.as_ref(),
                available_mcp_servers: available_mcp_servers.clone(),
                available_mcp_tools: &mcp_summary.available_tools,
            };
            match runner.run_turn(&mut provider_ledger, turn.clone(), provider_context.clone()) {
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
                    if provider_error_is_context_limit_exceeded(
                        error.message(),
                        error.provider_failure_json(),
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
                            provider_context =
                                self.agent_turn_contexts.get(turn_id).cloned().ok_or_else(
                                    || {
                                        MezError::invalid_state(
                                            "runtime agent turn context is unavailable",
                                        )
                                    },
                                )?;
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
                    if provider_error_is_output_limit_exceeded(
                        error.message(),
                        error.provider_failure_json(),
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
                            provider_context =
                                self.agent_turn_contexts.get(turn_id).cloned().ok_or_else(
                                    || {
                                        MezError::invalid_state(
                                            "runtime agent turn context is unavailable",
                                        )
                                    },
                                )?;
                            model_profile = self
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
                    self.runtime_metrics.record_provider_failure();
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

    /// Applies a provider-worker completion event through actor-owned runtime
    /// ingress.
    ///
    /// Async provider workers perform network I/O outside the runtime actor and
    /// submit the deterministic turn execution back through this path. The
    /// completion event is validated against the active turn before it can
    /// update transcript, audit, scheduler, approval, prompt, or terminal state.
    pub async fn apply_agent_provider_completed_event(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        execution: AgentTurnExecution,
    ) -> Result<bool> {
        self.require_live()?;
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.pending_agent_provider_tasks.remove(turn_id);
            self.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.state != AgentTurnState::Running {
            self.pending_agent_provider_tasks.remove(turn_id);
            self.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        let Some(mut model_profile) = self.agent_turn_model_profiles.get(turn_id).cloned() else {
            let error = MezError::invalid_state("runtime agent turn has no model profile");
            self.fail_agent_turn_after_provider_completion_application_error(
                &turn,
                execution.response.provider.as_str(),
                None,
                &error,
            );
            return Ok(true);
        };
        if let Err(error) =
            runtime_validate_provider_completion_identity(&turn, agent_id, turn_id, &execution)
        {
            let provider_id = execution.response.provider.clone();
            self.fail_agent_turn_after_provider_completion_application_error(
                &turn,
                &provider_id,
                Some(&model_profile),
                &error,
            );
            return Ok(true);
        }
        if let Err(error) = runtime_validate_provider_completion_execution(&turn, &execution) {
            let provider_id = execution.response.provider.clone();
            self.fail_agent_turn_after_provider_completion_application_error(
                &turn,
                &provider_id,
                Some(&model_profile),
                &error,
            );
            return Ok(true);
        }
        let execution_profile =
            runtime_apply_auto_sizing_execution_profile(model_profile.clone(), &execution.request);
        if execution_profile != model_profile {
            self.agent_turn_model_profiles
                .insert(turn_id.to_string(), execution_profile.clone());
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                &format!(
                    "auto_sizing applied provider={} model={} reasoning={}",
                    execution_profile.provider,
                    execution_profile.model,
                    execution_profile
                        .reasoning_profile
                        .as_deref()
                        .unwrap_or("none")
                ),
            )?;
            model_profile = execution_profile;
        }
        self.pending_agent_provider_tasks.remove(turn_id);
        self.claimed_agent_provider_tasks.remove(turn_id);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            "provider_task completed reason=typed_provider_event",
        )?;
        let provider_id = execution.response.provider.clone();
        if let Err(error) = self
            .apply_agent_provider_execution_async(&turn, &model_profile, &provider_id, execution)
            .await
        {
            self.fail_agent_turn_after_provider_completion_application_error(
                &turn,
                &provider_id,
                Some(&model_profile),
                &error,
            );
        }
        Ok(true)
    }

    /// Appends the provider response's assistant-visible context to a running
    /// turn before any action results are observed.
    ///
    /// # Parameters
    /// - `turn`: The running agent turn receiving the assistant context block.
    /// - `execution`: The provider execution whose rationale and visible
    ///   assistant text should remain available to later provider requests.
    fn append_agent_execution_assistant_context(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let content = assistant_context_content_for_execution(execution);
        if content.trim().is_empty() {
            return Ok(());
        }
        let context = self
            .agent_turn_contexts
            .get_mut(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let label = format!("assistant response for {}", turn.turn_id);
        if context.blocks.iter().any(|block| {
            block.source == ContextSourceKind::TranscriptAssistant
                && block.label == label
                && block.content == content
        }) {
            return Ok(());
        }
        context.blocks.push(ContextBlock {
            source: ContextSourceKind::TranscriptAssistant,
            label,
            content,
        });
        Ok(())
    }

    /// Appends or updates the active-turn progress `say` ledger.
    ///
    /// # Parameters
    /// - `turn`: The running agent turn receiving the ledger context block.
    /// - `execution`: The provider execution whose progress `say` actions should
    ///   become explicit context for later continuations.
    fn append_agent_execution_progress_say_ledger_context(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let new_entries = runtime_progress_say_entries_for_execution(execution);
        if new_entries.is_empty() {
            return Ok(());
        }
        let context = self
            .agent_turn_contexts
            .get_mut(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let mut previous_entries = Vec::new();
        context.blocks.retain(|block| {
            let is_progress_say_ledger = block.source == ContextSourceKind::LocalMessage
                && block.label == RUNTIME_PROGRESS_SAY_LEDGER_LABEL;
            if is_progress_say_ledger {
                previous_entries.extend(runtime_progress_say_entries_from_ledger(&block.content));
            }
            !is_progress_say_ledger
        });
        let entries = runtime_merge_progress_say_entries(previous_entries, new_entries);
        if entries.is_empty() {
            return Ok(());
        }
        context.blocks.push(ContextBlock {
            source: ContextSourceKind::LocalMessage,
            label: RUNTIME_PROGRESS_SAY_LEDGER_LABEL.to_string(),
            content: runtime_progress_say_ledger_content(&entries),
        });
        Ok(())
    }

    /// Returns progress `say` entries already visible during an active turn.
    ///
    /// # Parameters
    /// - `turn_id`: Active turn whose current progress ledger should be read.
    fn current_turn_progress_say_entries(&self, turn_id: &str) -> Vec<String> {
        let Some(context) = self.agent_turn_contexts.get(turn_id) else {
            return Vec::new();
        };
        context
            .blocks
            .iter()
            .filter(|block| {
                block.source == ContextSourceKind::LocalMessage
                    && block.label == RUNTIME_PROGRESS_SAY_LEDGER_LABEL
            })
            .flat_map(|block| runtime_progress_say_entries_from_ledger(&block.content))
            .collect()
    }

    /// Returns rationale entries already emitted during an active turn.
    ///
    /// # Parameters
    /// - `turn_id`: Active turn whose current rationale ledger should be read.
    fn current_turn_rationale_entries(&self, turn_id: &str) -> Vec<String> {
        let Some(context) = self.agent_turn_contexts.get(turn_id) else {
            return Vec::new();
        };
        context
            .blocks
            .iter()
            .filter(|block| {
                block.source == ContextSourceKind::LocalMessage
                    && block.label == RUNTIME_RATIONALE_LEDGER_LABEL
            })
            .flat_map(|block| runtime_rationale_entries_from_ledger(&block.content))
            .collect()
    }

    /// Suppresses progress `say` actions that repeat an already visible update.
    ///
    /// The provider still receives a successful action result explaining the
    /// suppression, but the duplicate text is removed before user display,
    /// assistant context, copy retention, and progress-ledger updates.
    ///
    /// # Parameters
    /// - `turn`: Active turn receiving the provider execution.
    /// - `execution`: Provider execution whose progress actions may be filtered.
    fn suppress_redundant_progress_say_actions(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<usize> {
        let mut visible_entries = self.current_turn_progress_say_entries(&turn.turn_id);
        let Some(batch) = execution.response.action_batch.as_mut() else {
            return Ok(0);
        };
        if let Some(rationale_entry) = runtime_normalize_progress_say_entry(&batch.rationale)
            && runtime_progress_say_entry_repeats_existing(&rationale_entry, &visible_entries)
        {
            batch.rationale.clear();
        }
        let mut suppressed_actions = Vec::new();
        let mut suppressed_action_ids = Vec::new();
        for action in &mut batch.actions {
            let AgentActionPayload::Say {
                status,
                text,
                content_type: _,
            } = &mut action.payload
            else {
                continue;
            };
            if *status != SayStatus::Progress {
                continue;
            }
            let Some(entry) = runtime_normalize_progress_say_entry(text) else {
                continue;
            };
            if runtime_progress_say_entry_repeats_existing(&entry, &visible_entries) {
                text.clear();
                action.rationale.clear();
                suppressed_action_ids.push(action.id.clone());
                suppressed_actions.push(action.clone());
            } else {
                visible_entries.push(entry);
            }
        }
        for action_id in &suppressed_action_ids {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "action {action_id} progress_say suppressed reason=repeated_current_turn_progress"
                ),
            )?;
        }
        for action in &suppressed_actions {
            if let Some(result) = execution
                .action_results
                .iter_mut()
                .find(|result| result.action_id == action.id)
            {
                *result = ActionResult::succeeded(
                    turn,
                    action,
                    vec![
                        "progress say suppressed because it repeated an already visible current-turn update; continue with only materially new progress".to_string(),
                    ],
                    Some(
                        r#"{"kind":"say","status":"progress","display":"suppressed_duplicate_progress","reason":"repeated_current_turn_progress"}"#
                            .to_string(),
                    ),
                );
            }
        }
        if !suppressed_actions.is_empty() {
            execution.terminal_state = runtime_agent_turn_state_from_action_results(
                &execution.action_results,
                execution.final_turn,
            );
        }
        Ok(suppressed_actions.len())
    }

    /// Suppresses batch/action rationale that repeats already-emitted same-turn intent.
    ///
    /// Repeated investigative rationale is visible to the user in verbose
    /// thinking mode and can indirectly bias the next provider turn. Once a
    /// current-turn rationale ledger records that intent, later batches should
    /// mention only a materially new reason.
    fn suppress_redundant_rationale_entries(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &mut AgentTurnExecution,
    ) -> Result<usize> {
        let mut visible_entries = self.current_turn_rationale_entries(&turn.turn_id);
        let Some(batch) = execution.response.action_batch.as_mut() else {
            return Ok(0);
        };
        let mut suppressed = 0usize;
        if let Some(entry) = runtime_normalize_rationale_entry(&batch.rationale)
            && runtime_rationale_entry_repeats_existing(&entry, &visible_entries)
        {
            batch.rationale.clear();
            suppressed += 1;
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                "batch rationale suppressed reason=repeated_current_turn_rationale",
            )?;
        } else if let Some(entry) = runtime_normalize_rationale_entry(&batch.rationale) {
            visible_entries.push(entry);
        }
        for action in &mut batch.actions {
            let Some(entry) = runtime_normalize_rationale_entry(&action.rationale) else {
                continue;
            };
            if runtime_rationale_entry_repeats_existing(&entry, &visible_entries) {
                action.rationale.clear();
                suppressed += 1;
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    &format!(
                        "action {} rationale suppressed reason=repeated_current_turn_rationale",
                        action.id
                    ),
                )?;
                continue;
            }
            visible_entries.push(entry);
        }
        Ok(suppressed)
    }

    /// Appends or updates the active-turn rationale ledger.
    ///
    /// # Parameters
    /// - `turn`: The running agent turn receiving the ledger context block.
    /// - `execution`: The provider execution whose retained rationale should
    ///   become explicit same-turn context for later continuations.
    fn append_agent_execution_rationale_ledger_context(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<()> {
        let new_entries = runtime_rationale_entries_for_execution(execution);
        if new_entries.is_empty() {
            return Ok(());
        }
        let context = self
            .agent_turn_contexts
            .get_mut(&turn.turn_id)
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        let mut previous_entries = Vec::new();
        context.blocks.retain(|block| {
            let is_rationale_ledger = block.source == ContextSourceKind::LocalMessage
                && block.label == RUNTIME_RATIONALE_LEDGER_LABEL;
            if is_rationale_ledger {
                previous_entries.extend(runtime_rationale_entries_from_ledger(&block.content));
            }
            !is_rationale_ledger
        });
        let entries = runtime_merge_rationale_entries(previous_entries, new_entries);
        if entries.is_empty() {
            return Ok(());
        }
        context.blocks.push(ContextBlock {
            source: ContextSourceKind::LocalMessage,
            label: RUNTIME_RATIONALE_LEDGER_LABEL.to_string(),
            content: runtime_rationale_ledger_content(&entries),
        });
        Ok(())
    }

    /// Runs the apply agent provider execution operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub(super) fn apply_agent_provider_execution(
        &mut self,
        turn: &AgentTurnRecord,
        model_profile: &ModelProfile,
        provider_id: &str,
        mut execution: AgentTurnExecution,
    ) -> Result<AgentTurnExecution> {
        let turn_id = turn.turn_id.as_str();
        self.append_provider_request_audit(turn, model_profile, provider_id, "succeeded")?;
        let response_action_count = execution
            .response
            .action_batch
            .as_ref()
            .map(|batch| batch.actions.len())
            .unwrap_or(0);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "provider_response received provider={} terminal_state={} action_count={} final={}",
                provider_id,
                runtime_agent_turn_state_name(execution.terminal_state),
                response_action_count,
                execution.final_turn
            ),
        )?;
        let token_usage_key =
            ModelTokenUsageKey::new(model_profile.provider.clone(), model_profile.model.clone());
        for (key, usage) in &execution.routing_token_usage_by_model {
            self.runtime_metrics
                .record_provider_token_usage(*usage, *usage, key);
        }
        self.record_agent_provider_token_usage_by_model(
            &turn.pane_id,
            &execution.routing_token_usage_by_model,
        );
        self.runtime_metrics.record_provider_response(
            &execution.response,
            execution.latest_response_usage,
            &token_usage_key,
        );
        self.record_agent_provider_token_usage_with_profile(
            &turn.pane_id,
            execution.response.usage,
            execution.latest_response_usage,
            Some(model_profile),
        );
        self.record_agent_provider_quota_usage(&turn.pane_id, &execution.response.quota_usage);
        self.append_agent_trace_maap_response(turn, &execution.response)?;
        self.suppress_redundant_progress_say_actions(turn, &mut execution)?;
        self.suppress_redundant_rationale_entries(turn, &mut execution)?;
        self.reset_action_pressure_after_non_shell_effects(turn, &execution);
        self.present_agent_response_actions_to_terminal_buffer(&turn.pane_id, &execution)?;
        self.append_agent_execution_assistant_context(turn, &execution)?;
        self.append_agent_execution_progress_say_ledger_context(turn, &execution)?;
        self.append_agent_execution_rationale_ledger_context(turn, &execution)?;
        self.record_agent_copy_output(turn, &execution);
        let skill_actions_executed =
            self.execute_running_skill_actions_for_turn(turn, &mut execution)?;
        let message_actions_executed =
            self.execute_running_message_actions_for_turn(turn, &mut execution)?;
        let network_actions_executed = 0usize;
        let mcp_actions_executed =
            self.execute_running_mcp_actions_for_turn(turn, &mut execution)?;
        let spawn_actions_executed =
            self.execute_running_spawn_actions_for_turn(turn, &mut execution)?;
        let config_actions_executed =
            self.execute_running_config_change_actions_for_turn(turn, &mut execution)?;
        let shell_actions_dispatched =
            self.dispatch_running_shell_actions_to_panes(turn, &mut execution)?;
        self.append_agent_trace_maap_action_results(
            &turn.pane_id,
            &turn.turn_id,
            "action_results",
            &execution.action_results,
        )?;
        self.record_runtime_agent_patch_results_for_turn(turn, &execution);
        if execution.terminal_state == AgentTurnState::Failed {
            let error = runtime_agent_execution_failure_error(&execution);
            self.append_provider_request_failure_audit(turn, model_profile, provider_id, &error)?;
        }
        if execution.terminal_state == AgentTurnState::Blocked {
            self.apply_permission_request_hooks_for_execution(turn, &mut execution)?;
        }
        self.present_agent_action_outcomes_to_terminal_buffer(&turn.pane_id, &execution)?;
        let failure_feedback_queued = self.queue_agent_failure_feedback_for_correction(
            turn,
            &mut execution,
            "provider_execution_failed_action",
        )?;
        let _ = self.continue_completed_turn_for_pending_steering(turn, &mut execution)?;
        self.present_deferred_agent_say_actions_to_terminal_buffer(&turn.pane_id, &execution)?;
        let mut persisted_transcript_entries = 0usize;
        if failure_feedback_queued {
            self.agent_turn_executions.remove(turn_id);
        } else if execution.terminal_state == AgentTurnState::Blocked {
            persisted_transcript_entries =
                self.persist_runtime_agent_turn_execution_transcript(turn, &execution)?;
            self.queue_blocked_approvals_for_execution(turn, &execution)?;
            self.agent_turn_executions
                .insert(turn_id.to_string(), execution.clone());
            let _ = self.agent_scheduler.block_running(turn_id);
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                "scheduler running -> blocked reason=approval_required",
            )?;
            self.pending_agent_provider_tasks.remove(turn_id);
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                "provider_task removed reason=blocked_waiting_approval",
            )?;
            self.agent_turn_ledger
                .finish_turn(turn_id, AgentTurnState::Blocked)?;
            self.append_agent_trace_turn_transition(
                turn,
                turn.state,
                AgentTurnState::Blocked,
                "approval_required",
            )?;
            self.emit_subagent_task_status(
                turn,
                TaskState::Blocked,
                None,
                "subagent task blocked pending approval",
            )?;
            self.start_ready_agent_turns()?;
        } else if execution.terminal_state != AgentTurnState::Running {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                &format!(
                    "provider_execution terminal_state={} reason=action_results_settled",
                    runtime_agent_turn_state_name(execution.terminal_state)
                ),
            )?;
            persisted_transcript_entries =
                self.persist_runtime_agent_turn_execution_transcript(turn, &execution)?;
            self.emit_subagent_task_result_for_execution(turn, &execution)?;
            self.complete_running_agent_turn_and_start_ready(
                turn,
                execution.terminal_state,
                "provider_execution_settled",
            )?;
        } else {
            let waiting_for_joined_subagents =
                self.execution_waiting_for_live_joined_subagents(turn_id, &execution);
            if waiting_for_joined_subagents {
                self.agent_turn_executions
                    .insert(turn_id.to_string(), execution.clone());
                let _ = self.agent_scheduler.block_running(turn_id);
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    turn_id,
                    "scheduler running -> blocked reason=waiting_for_subagents",
                )?;
                self.pending_agent_provider_tasks.remove(turn_id);
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    turn_id,
                    "provider_task removed reason=waiting_for_subagents",
                )?;
                self.agent_turn_ledger
                    .finish_turn(turn_id, AgentTurnState::Blocked)?;
                self.append_agent_trace_turn_transition(
                    turn,
                    turn.state,
                    AgentTurnState::Blocked,
                    "waiting_for_subagents",
                )?;
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    "agent: waiting for subagents to finish",
                )?;
                self.emit_subagent_task_status(
                    turn,
                    TaskState::Blocked,
                    None,
                    "subagent task waiting for child subagents",
                )?;
                self.start_ready_agent_turns()?;
            } else if runtime_execution_ready_for_provider_continuation(&execution) {
                self.pending_agent_provider_tasks
                    .insert(turn_id.to_string());
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    turn_id,
                    "provider_task queued reason=ready_for_provider_continuation",
                )?;
            }
            if !waiting_for_joined_subagents {
                self.agent_turn_executions
                    .insert(turn_id.to_string(), execution.clone());
            }
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                &format!(
                    "execution stored state=running pending_shell_dispatch={} ready_for_provider_continuation={}",
                    self.execution_has_pending_shell_dispatch(turn_id, &execution),
                    runtime_execution_ready_for_provider_continuation(&execution)
                ),
            )?;
        }
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","provider":"{}","action_results":{},"shell_actions_dispatched":{},"transcript_entries":{}}}"#,
                json_escape(&turn.pane_id),
                json_escape(turn_id),
                runtime_agent_turn_state_name(execution.terminal_state),
                json_escape(provider_id),
                execution.action_results.len(),
                shell_actions_dispatched
                    .saturating_add(mcp_actions_executed)
                    .saturating_add(skill_actions_executed)
                    .saturating_add(network_actions_executed)
                    .saturating_add(message_actions_executed)
                    .saturating_add(spawn_actions_executed)
                    .saturating_add(config_actions_executed),
                persisted_transcript_entries
            ),
        )?;
        self.set_agent_prompt_display_lines_if_pane_present(
            &turn.pane_id,
            runtime_agent_execution_prompt_display_lines(
                turn_id,
                provider_id,
                &execution,
                shell_actions_dispatched
                    .saturating_add(mcp_actions_executed)
                    .saturating_add(skill_actions_executed)
                    .saturating_add(network_actions_executed)
                    .saturating_add(message_actions_executed)
                    .saturating_add(spawn_actions_executed)
                    .saturating_add(config_actions_executed),
                persisted_transcript_entries,
            ),
        )?;
        Ok(execution)
    }

    /// Runs the apply agent provider execution async operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    async fn apply_agent_provider_execution_async(
        &mut self,
        turn: &AgentTurnRecord,
        model_profile: &ModelProfile,
        provider_id: &str,
        mut execution: AgentTurnExecution,
    ) -> Result<AgentTurnExecution> {
        let turn_id = turn.turn_id.as_str();
        self.append_provider_request_audit(turn, model_profile, provider_id, "succeeded")?;
        let response_action_count = execution
            .response
            .action_batch
            .as_ref()
            .map(|batch| batch.actions.len())
            .unwrap_or(0);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "provider_response received provider={} terminal_state={} action_count={} final={}",
                provider_id,
                runtime_agent_turn_state_name(execution.terminal_state),
                response_action_count,
                execution.final_turn
            ),
        )?;
        let token_usage_key =
            ModelTokenUsageKey::new(model_profile.provider.clone(), model_profile.model.clone());
        for (key, usage) in &execution.routing_token_usage_by_model {
            self.runtime_metrics
                .record_provider_token_usage(*usage, *usage, key);
        }
        self.record_agent_provider_token_usage_by_model(
            &turn.pane_id,
            &execution.routing_token_usage_by_model,
        );
        self.runtime_metrics.record_provider_response(
            &execution.response,
            execution.latest_response_usage,
            &token_usage_key,
        );
        self.record_agent_provider_token_usage_with_profile(
            &turn.pane_id,
            execution.response.usage,
            execution.latest_response_usage,
            Some(model_profile),
        );
        self.record_agent_provider_quota_usage(&turn.pane_id, &execution.response.quota_usage);
        self.append_agent_trace_maap_response(turn, &execution.response)?;
        self.suppress_redundant_progress_say_actions(turn, &mut execution)?;
        self.suppress_redundant_rationale_entries(turn, &mut execution)?;
        self.reset_action_pressure_after_non_shell_effects(turn, &execution);
        self.present_agent_response_actions_to_terminal_buffer(&turn.pane_id, &execution)?;
        self.append_agent_execution_assistant_context(turn, &execution)?;
        self.append_agent_execution_progress_say_ledger_context(turn, &execution)?;
        self.append_agent_execution_rationale_ledger_context(turn, &execution)?;
        self.record_agent_copy_output(turn, &execution);
        let skill_actions_executed =
            self.execute_running_skill_actions_for_turn(turn, &mut execution)?;
        let message_actions_executed =
            self.execute_running_message_actions_for_turn(turn, &mut execution)?;
        let network_actions_executed = self
            .execute_running_network_actions_for_turn_async(turn, &mut execution)
            .await?;
        let mcp_actions_executed = self
            .execute_running_mcp_actions_for_turn_async(turn, &mut execution)
            .await?;
        let spawn_actions_executed =
            self.execute_running_spawn_actions_for_turn(turn, &mut execution)?;
        let config_actions_executed =
            self.execute_running_config_change_actions_for_turn(turn, &mut execution)?;
        let shell_actions_dispatched =
            self.dispatch_running_shell_actions_to_panes(turn, &mut execution)?;
        self.append_agent_trace_maap_action_results(
            &turn.pane_id,
            &turn.turn_id,
            "action_results",
            &execution.action_results,
        )?;
        self.record_runtime_agent_patch_results_for_turn(turn, &execution);
        if execution.terminal_state == AgentTurnState::Failed {
            let error = runtime_agent_execution_failure_error(&execution);
            self.append_provider_request_failure_audit(turn, model_profile, provider_id, &error)?;
        }
        if execution.terminal_state == AgentTurnState::Blocked {
            self.apply_permission_request_hooks_for_execution(turn, &mut execution)?;
        }
        self.present_agent_action_outcomes_to_terminal_buffer(&turn.pane_id, &execution)?;
        let failure_feedback_queued = self.queue_agent_failure_feedback_for_correction(
            turn,
            &mut execution,
            "provider_execution_failed_action",
        )?;
        let _ = self.continue_completed_turn_for_pending_steering(turn, &mut execution)?;
        self.present_deferred_agent_say_actions_to_terminal_buffer(&turn.pane_id, &execution)?;
        let mut persisted_transcript_entries = 0usize;
        if failure_feedback_queued {
            self.agent_turn_executions.remove(turn_id);
        } else if execution.terminal_state == AgentTurnState::Blocked {
            persisted_transcript_entries =
                self.persist_runtime_agent_turn_execution_transcript(turn, &execution)?;
            self.queue_blocked_approvals_for_execution(turn, &execution)?;
            self.agent_turn_executions
                .insert(turn_id.to_string(), execution.clone());
            let _ = self.agent_scheduler.block_running(turn_id);
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                "scheduler running -> blocked reason=approval_required",
            )?;
            self.pending_agent_provider_tasks.remove(turn_id);
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                "provider_task removed reason=blocked_waiting_approval",
            )?;
            self.agent_turn_ledger
                .finish_turn(turn_id, AgentTurnState::Blocked)?;
            self.append_agent_trace_turn_transition(
                turn,
                turn.state,
                AgentTurnState::Blocked,
                "approval_required",
            )?;
            self.emit_subagent_task_status(
                turn,
                TaskState::Blocked,
                None,
                "subagent task blocked pending approval",
            )?;
            self.start_ready_agent_turns()?;
        } else if execution.terminal_state != AgentTurnState::Running {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                &format!(
                    "provider_execution terminal_state={} reason=action_results_settled",
                    runtime_agent_turn_state_name(execution.terminal_state)
                ),
            )?;
            persisted_transcript_entries =
                self.persist_runtime_agent_turn_execution_transcript(turn, &execution)?;
            self.emit_subagent_task_result_for_execution(turn, &execution)?;
            self.complete_running_agent_turn_and_start_ready(
                turn,
                execution.terminal_state,
                "provider_execution_settled",
            )?;
        } else {
            let waiting_for_joined_subagents =
                self.execution_waiting_for_live_joined_subagents(turn_id, &execution);
            if waiting_for_joined_subagents {
                self.agent_turn_executions
                    .insert(turn_id.to_string(), execution.clone());
                let _ = self.agent_scheduler.block_running(turn_id);
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    turn_id,
                    "scheduler running -> blocked reason=waiting_for_subagents",
                )?;
                self.pending_agent_provider_tasks.remove(turn_id);
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    turn_id,
                    "provider_task removed reason=waiting_for_subagents",
                )?;
                self.agent_turn_ledger
                    .finish_turn(turn_id, AgentTurnState::Blocked)?;
                self.append_agent_trace_turn_transition(
                    turn,
                    turn.state,
                    AgentTurnState::Blocked,
                    "waiting_for_subagents",
                )?;
                self.append_agent_status_text_to_terminal_buffer(
                    &turn.pane_id,
                    "agent: waiting for subagents to finish",
                )?;
                self.emit_subagent_task_status(
                    turn,
                    TaskState::Blocked,
                    None,
                    "subagent task waiting for child subagents",
                )?;
                self.start_ready_agent_turns()?;
            } else if runtime_execution_ready_for_provider_continuation(&execution) {
                self.pending_agent_provider_tasks
                    .insert(turn_id.to_string());
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    turn_id,
                    "provider_task queued reason=ready_for_provider_continuation",
                )?;
            }
            if !waiting_for_joined_subagents {
                self.agent_turn_executions
                    .insert(turn_id.to_string(), execution.clone());
            }
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                turn_id,
                &format!(
                    "execution stored state=running pending_shell_dispatch={} ready_for_provider_continuation={}",
                    self.execution_has_pending_shell_dispatch(turn_id, &execution),
                    runtime_execution_ready_for_provider_continuation(&execution)
                ),
            )?;
        }
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"{}","provider":"{}","action_results":{},"shell_actions_dispatched":{},"transcript_entries":{}}}"#,
                json_escape(&turn.pane_id),
                json_escape(turn_id),
                runtime_agent_turn_state_name(execution.terminal_state),
                json_escape(provider_id),
                execution.action_results.len(),
                shell_actions_dispatched
                    .saturating_add(mcp_actions_executed)
                    .saturating_add(skill_actions_executed)
                    .saturating_add(network_actions_executed)
                    .saturating_add(message_actions_executed)
                    .saturating_add(spawn_actions_executed)
                    .saturating_add(config_actions_executed),
                persisted_transcript_entries
            ),
        )?;
        self.set_agent_prompt_display_lines_if_pane_present(
            &turn.pane_id,
            runtime_agent_execution_prompt_display_lines(
                turn_id,
                provider_id,
                &execution,
                shell_actions_dispatched
                    .saturating_add(mcp_actions_executed)
                    .saturating_add(skill_actions_executed)
                    .saturating_add(network_actions_executed)
                    .saturating_add(message_actions_executed)
                    .saturating_add(spawn_actions_executed)
                    .saturating_add(config_actions_executed),
                persisted_transcript_entries,
            ),
        )?;
        Ok(execution)
    }
}
