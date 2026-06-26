//! Runtime agent provider task queue and worker lease helpers.
//!
//! This module owns provider task claiming, retry bookkeeping, failure ingress,
//! compatibility polling, and async worker lease tracking. It keeps provider
//! queue lifecycle decisions separate from provider execution and action
//! dispatch while preserving the runtime service method surface used by the
//! async actor and tests.

use super::*;
use crate::agent::anthropic_provider_from_auth_store_with_provider_options;

impl RuntimeSessionService {
    /// Builds a runtime provider dispatch from one configured provider API.
    ///
    /// Provider `kind` describes the brand/defaults, while `api` selects the
    /// wire compatibility implementation. This helper keeps ordinary turns and
    /// router turns on the same resolution path so adding a provider that
    /// speaks an existing API does not duplicate dispatch branches.
    fn runtime_dispatch_provider_from_config(
        &mut self,
        provider_name: &str,
        provider_config: &RuntimeProviderConfig,
        audit_scope: &str,
    ) -> Result<RuntimeAgentProviderDispatchProvider> {
        let api = effective_provider_api(&provider_config.kind, provider_config.api.as_deref())?;
        self.append_credential_access_audit(
            provider_name,
            &provider_config.auth_profile,
            audit_scope,
            "requested",
        )?;
        let provider_result = (|| {
            let auth_store = self.auth_store.as_ref().ok_or_else(|| {
                MezError::invalid_state(format!(
                    "provider `{provider_name}` execution requires an attached auth store"
                ))
            })?;
            let endpoint_override = provider_config
                .base_url
                .as_deref()
                .filter(|endpoint| !endpoint.is_empty());
            match api {
                ProviderApiCompatibility::OpenAiResponses => {
                    openai_responses_provider_from_auth_store_with_provider_options(
                        auth_store,
                        provider_name,
                        endpoint_override,
                        &provider_config.options,
                        DEFAULT_PROVIDER_TIMEOUT_MS,
                        ReqwestProviderHttpTransport,
                    )
                    .map(RuntimeAgentProviderDispatchProvider::OpenAi)
                }
                ProviderApiCompatibility::OpenAiChatCompletions => {
                    openai_compatible_provider_from_auth_store_with_provider_options(
                        auth_store,
                        provider_name,
                        endpoint_override,
                        &provider_config.options,
                        DEFAULT_PROVIDER_TIMEOUT_MS,
                        ReqwestProviderHttpTransport,
                    )
                    .map(RuntimeAgentProviderDispatchProvider::OpenAiCompatible)
                }
                ProviderApiCompatibility::DeepSeekChatCompletions => {
                    deepseek_chat_completions_provider_from_auth_store_with_provider_options(
                        auth_store,
                        provider_name,
                        endpoint_override,
                        DEFAULT_PROVIDER_TIMEOUT_MS,
                        ReqwestProviderHttpTransport,
                    )
                    .map(RuntimeAgentProviderDispatchProvider::DeepSeek)
                }
                ProviderApiCompatibility::AnthropicMessages => {
                    anthropic_provider_from_auth_store_with_provider_options(
                        auth_store,
                        provider_name,
                        endpoint_override,
                        &provider_config.options,
                        DEFAULT_PROVIDER_TIMEOUT_MS,
                        ReqwestProviderHttpTransport,
                    )
                    .map(RuntimeAgentProviderDispatchProvider::Anthropic)
                }
            }
        })();
        match provider_result {
            Ok(provider) => {
                self.append_credential_access_audit(
                    provider_name,
                    &provider_config.auth_profile,
                    audit_scope,
                    "granted",
                )?;
                Ok(provider)
            }
            Err(error) => {
                self.append_credential_access_audit(
                    provider_name,
                    &provider_config.auth_profile,
                    audit_scope,
                    "denied",
                )?;
                Err(error)
            }
        }
    }

    /// Claims one configured provider task for execution outside the runtime
    /// actor.
    ///
    /// The actor remains responsible for validating turn identity, running
    /// pre-request hooks, recording audit/trace state, snapshotting the policy
    /// and MCP context, and constructing the provider from runtime
    /// configuration. The returned dispatch contains only deterministic inputs
    /// needed by a supervised worker to perform the provider request and plan
    /// action results without holding the actor.
    pub fn claim_configured_agent_provider_task(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
    ) -> Result<Option<RuntimeAgentProviderDispatch>> {
        match self.try_claim_configured_agent_provider_task(agent_id, turn_id) {
            Ok(dispatch) => Ok(dispatch),
            Err(error) => {
                self.fail_configured_agent_provider_task(turn_id, &error)?;
                Ok(None)
            }
        }
    }

    /// Runs the try claim configured agent provider task operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn try_claim_configured_agent_provider_task(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
    ) -> Result<Option<RuntimeAgentProviderDispatch>> {
        self.require_live()?;
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(None);
        };
        if turn.agent_id != agent_id.as_str() {
            return Err(MezError::invalid_args(
                "agent provider dispatch agent id does not match turn",
            ));
        }
        if self
            .agent_turn_executions
            .get(turn_id)
            .is_some_and(|execution| self.execution_has_pending_shell_dispatch(turn_id, execution))
        {
            self.pending_agent_provider_tasks.remove(turn_id);
            let _ = self.dispatch_stored_running_shell_actions(turn_id)?;
            return Ok(None);
        }
        if !self.pending_agent_provider_tasks.contains(turn_id) {
            return Ok(None);
        }
        if turn.state != AgentTurnState::Running {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(None);
        }

        let model_profile = self
            .agent_turn_model_profiles
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn has no model profile"))?;
        let provider_config = self
            .provider_registry
            .provider(&model_profile.provider)
            .cloned()
            .ok_or_else(|| {
                MezError::config(format!(
                    "provider `{}` for active model profile is not configured",
                    model_profile.provider
                ))
            })?;
        let provider = self.runtime_dispatch_provider_from_config(
            &model_profile.provider,
            &provider_config,
            "provider_request",
        )?;

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
        let auto_sizing = self.runtime_auto_sizing_dispatch_for_turn(&turn, &model_profile)?;
        if let Some(auto_sizing) = auto_sizing.as_ref() {
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "auto_sizing queued router_profile={} small={} medium={} large={}",
                    auto_sizing.router_profile_name,
                    auto_sizing.small.profile_name,
                    auto_sizing.medium.profile_name,
                    auto_sizing.large.profile_name
                ),
            )?;
            self.append_agent_verbose_status_text_to_terminal_buffer(
                &turn.pane_id,
                "agent: routing selecting model and reasoning effort",
            )?;
        }
        let auto_sizing_provider = if let Some(auto_sizing) = auto_sizing.as_ref()
            && auto_sizing.router_profile.provider != model_profile.provider
        {
            let router_provider_config = self
                .provider_registry
                .provider(&auto_sizing.router_profile.provider)
                .cloned()
                .ok_or_else(|| {
                    MezError::config(format!(
                        "auto-sizing router provider `{}` is not configured",
                        auto_sizing.router_profile.provider
                    ))
                })?;
            let result = self.runtime_dispatch_provider_from_config(
                &auto_sizing.router_profile.provider,
                &router_provider_config,
                "provider_request",
            );
            match result {
                Ok(provider) => Some(provider),
                Err(error) => {
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!(
                            "auto_sizing router provider unavailable error_kind={} error={}",
                            runtime_mezzanine_error_code(error.kind()),
                            error.message()
                        ),
                    )?;
                    None
                }
            }
        } else {
            None
        };
        let mut auto_sizing_target_providers = std::collections::BTreeMap::new();
        if let Some(auto_sizing) = auto_sizing.as_ref() {
            for provider_id in [
                auto_sizing.small.profile.provider.as_str(),
                auto_sizing.medium.profile.provider.as_str(),
                auto_sizing.large.profile.provider.as_str(),
            ] {
                if provider_id == model_profile.provider
                    || auto_sizing_target_providers.contains_key(provider_id)
                {
                    continue;
                }
                let Some(target_provider_config) =
                    self.provider_registry.provider(provider_id).cloned()
                else {
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        &format!("auto_sizing target provider `{provider_id}` is not configured"),
                    )?;
                    continue;
                };
                match self.runtime_dispatch_provider_from_config(
                    provider_id,
                    &target_provider_config,
                    "provider_request",
                ) {
                    Ok(provider) => {
                        auto_sizing_target_providers.insert(provider_id.to_string(), provider);
                    }
                    Err(error) => {
                        self.append_agent_trace_turn_event(
                            &turn.pane_id,
                            &turn.turn_id,
                            &format!(
                                "auto_sizing target provider unavailable provider={} error_kind={} error={}",
                                provider_id,
                                runtime_mezzanine_error_code(error.kind()),
                                error.message()
                            ),
                        )?;
                    }
                }
            }
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
            &mcp_summary.available_tools,
            self.runtime_persistent_memory_enabled(),
            super::issues::runtime_issues_enabled(self),
        );
        if self.agent_debug_enabled(&turn.pane_id) {
            match assemble_model_request_with_retained_tail_percent(
                &model_profile,
                &turn,
                &context,
                self.agent_compaction_raw_retention_percent,
            ) {
                Ok(mut request) => {
                    crate::agent::apply_default_action_gates(
                        &mut request,
                        &mcp_summary.available_tools,
                        self.runtime_persistent_memory_enabled(),
                        super::issues::runtime_issues_enabled(self),
                    );
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
        self.pending_agent_provider_tasks.remove(turn_id);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            "provider_task claimed reason=async_provider_worker",
        )?;
        let local_action_executor = self.agent_local_action_executor_for_pane(&turn.pane_id);
        let native_shell_path = self.session.shell.path().to_path_buf();
        let native_working_directory = self.pane_current_working_directory(&turn.pane_id);
        let terminal_shell_output_preview_lines = self.terminal_shell_output_preview_lines;
        Ok(Some(RuntimeAgentProviderDispatch {
            turn,
            context,
            model_profile,
            auto_sizing,
            auto_sizing_provider,
            auto_sizing_target_providers,
            provider,
            permission_policy,
            session_approvals: self.session_approvals.clone(),
            path_scopes,
            subagent_scope,
            available_mcp_servers,
            available_mcp_tools: mcp_summary.available_tools,
            memory_actions_enabled: self.runtime_persistent_memory_enabled(),
            issue_actions_enabled: super::issues::runtime_issues_enabled(self),
            local_action_executor,
            native_shell_path,
            native_working_directory,
            terminal_shell_output_preview_lines,
            loop_turn: self.agent_loop_turns.get(turn_id).cloned(),
        }))
    }

    /// Runs the fail configured agent provider task operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn fail_configured_agent_provider_task(
        &mut self,
        turn_id: &str,
        error: &MezError,
    ) -> Result<()> {
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.pending_agent_provider_tasks.remove(turn_id);
            self.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(());
        };
        if !matches!(
            turn.state,
            AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
        ) {
            self.pending_agent_provider_tasks.remove(turn_id);
            self.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(());
        }
        let Some(model_profile) = self.agent_turn_model_profiles.get(turn_id).cloned() else {
            self.pending_agent_provider_tasks.remove(turn_id);
            self.claimed_agent_provider_tasks.remove(turn_id);
            return Err(MezError::invalid_state(
                "runtime agent turn has no model profile",
            ));
        };
        self.pending_agent_provider_tasks.remove(turn_id);
        self.claimed_agent_provider_tasks.remove(turn_id);
        self.append_provider_request_failure_audit(
            &turn,
            &model_profile,
            &model_profile.provider,
            error,
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!(
                "provider_task failed provider={} error_kind={}",
                model_profile.provider,
                runtime_mezzanine_error_code(error.kind())
            ),
        )?;
        self.append_agent_trace_provider_error(
            &turn,
            &model_profile.provider,
            &model_profile,
            error,
        )?;
        self.runtime_metrics.record_provider_failure();
        self.fail_agent_turn_for_provider_error(
            &turn,
            &model_profile.provider,
            &model_profile,
            error,
        )
    }

    /// Runs the record agent provider retry event operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn record_agent_provider_retry_event(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        error: &MezError,
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
    ) -> Result<bool> {
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.agent_id != agent_id.as_str() {
            return Err(MezError::invalid_args(
                "agent provider event agent id does not match turn",
            ));
        }
        if turn.state != AgentTurnState::Running {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        let Some(model_profile) = self.agent_turn_model_profiles.get(turn_id).cloned() else {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Err(MezError::invalid_state(
                "runtime agent turn has no model profile",
            ));
        };
        self.pending_agent_provider_tasks.remove(turn_id);
        self.append_provider_request_failure_audit(
            &turn,
            &model_profile,
            &model_profile.provider,
            error,
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!(
                "provider_task retry_scheduled provider={} error_kind={} attempt={} max_attempts={} delay_ms={}",
                model_profile.provider,
                runtime_mezzanine_error_code(error.kind()),
                attempt,
                max_attempts,
                delay_ms
            ),
        )?;
        self.append_agent_trace_provider_error(
            &turn,
            &model_profile.provider,
            &model_profile,
            error,
        )?;
        self.append_agent_status_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent: provider {} request failed; retrying attempt {attempt}/{max_attempts} in {} ms",
                model_profile.provider, delay_ms
            ),
        )?;
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"running","provider":"{}","provider_retry":"scheduled","attempt":{},"max_attempts":{},"delay_ms":{},"error_kind":"{}"}}"#,
                json_escape(&turn.pane_id),
                json_escape(turn_id),
                json_escape(&model_profile.provider),
                attempt,
                max_attempts,
                delay_ms,
                json_escape(runtime_mezzanine_error_code(error.kind()))
            ),
        )?;
        Ok(true)
    }

    /// Runs the queue agent provider retry task operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn queue_agent_provider_retry_task(
        &mut self,
        turn_id: &str,
        attempt: u64,
    ) -> Result<bool> {
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.state != AgentTurnState::Running {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        if !self.agent_turn_model_profiles.contains_key(turn_id) {
            return Err(MezError::invalid_state(
                "runtime agent turn has no model profile",
            ));
        }
        self.pending_agent_provider_tasks
            .insert(turn_id.to_string());
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!("provider_task queued reason=provider_retry_timer attempt={attempt}"),
        )?;
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"running","provider_retry":"ready","attempt":{}}}"#,
                json_escape(&turn.pane_id),
                json_escape(turn_id),
                attempt
            ),
        )?;
        Ok(true)
    }

    /// Queues a running provider turn after automatic compaction recovery.
    ///
    /// This is used after an output-limit failure triggers model-backed
    /// conversation compaction. The turn remains running, but its provider
    /// context has been refreshed to include compacted memory and the shorter
    /// raw transcript tail before the next provider request is dispatched.
    pub(crate) fn queue_agent_provider_recovery_task_after_compaction(
        &mut self,
        turn_id: &str,
    ) -> Result<bool> {
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.state != AgentTurnState::Running {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        if !self.agent_turn_model_profiles.contains_key(turn_id) {
            return Err(MezError::invalid_state(
                "runtime agent turn has no model profile",
            ));
        }
        self.pending_agent_provider_tasks
            .insert(turn_id.to_string());
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            "provider_task queued reason=provider_output_limit_compaction_completed",
        )?;
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"running","provider_retry":"ready","recovery":"output_limit_compaction"}}"#,
                json_escape(&turn.pane_id),
                json_escape(turn_id)
            ),
        )?;
        Ok(true)
    }

    /// Applies an async provider-worker failure event through actor-owned
    /// runtime ingress.
    ///
    /// Provider workers can fail before producing a model response. The event
    /// carries enough identity and error information to fail the active turn
    /// using the same audit, transcript, prompt-display, scheduler, and
    /// lifecycle paths as the configured compatibility poller.
    pub fn apply_agent_provider_failed_event(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        kind: &str,
        message: &str,
        provider_failure_json: Option<&str>,
        provider_raw_text: Option<&str>,
    ) -> Result<bool> {
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.agent_id != agent_id.as_str() {
            return Err(MezError::invalid_args(
                "agent provider event agent id does not match turn",
            ));
        }
        let error =
            runtime_provider_event_error(kind, message, provider_failure_json, provider_raw_text);
        self.fail_configured_agent_provider_task(turn_id, &error)?;
        Ok(true)
    }

    /// Runs the pending agent provider tasks operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn pending_agent_provider_tasks(&self) -> Vec<RuntimeAgentProviderTask> {
        self.pending_agent_provider_tasks
            .iter()
            .filter_map(|turn_id| self.runtime_agent_provider_task(turn_id))
            .collect()
    }

    /// Records that an async provider worker owns a claimed task.
    ///
    /// Claimed provider tasks are no longer visible in the pending queue, so the
    /// runtime keeps this lease record to make worker loss observable and
    /// recoverable through a timer.
    pub(crate) fn record_claimed_agent_provider_task(
        &mut self,
        dispatch: &RuntimeAgentProviderDispatch,
        generation: u64,
        timeout_ms: u64,
    ) -> Result<()> {
        let turn = &dispatch.turn;
        self.claimed_agent_provider_tasks.insert(
            turn.turn_id.clone(),
            RuntimeAgentProviderClaim {
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                generation,
                claimed_at_unix_ms: current_unix_millis(),
                timeout_ms,
            },
        );
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "provider_task claim_lease started generation={generation} timeout_ms={timeout_ms}"
            ),
        )?;
        Ok(())
    }

    /// Clears the provider-worker claim lease for a settled turn.
    pub(crate) fn clear_claimed_agent_provider_task(&mut self, turn_id: &str) {
        self.claimed_agent_provider_tasks.remove(turn_id);
    }

    /// Fails a running turn when its claimed provider worker lease expires.
    ///
    /// Stale timer generations are ignored so a late timer from an older claim
    /// cannot fail a turn whose provider work has already been retried.
    pub(crate) fn fail_expired_claimed_agent_provider_task(
        &mut self,
        turn_id: &str,
        generation: u64,
    ) -> Result<bool> {
        let Some(claim) = self.claimed_agent_provider_tasks.get(turn_id).cloned() else {
            return Ok(false);
        };
        if claim.turn_id != turn_id {
            self.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        if claim.generation != generation {
            return Ok(false);
        }
        let Some(turn) = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.state != AgentTurnState::Running {
            self.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        self.append_agent_status_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent: provider worker timed out after {} ms; failing turn",
                claim.timeout_ms
            ),
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!(
                "provider_task failed reason=provider_claim_timeout generation={} timeout_ms={}",
                claim.generation, claim.timeout_ms
            ),
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!(
                "provider_task claim_lease expired agent_id={} claimed_at_unix_ms={}",
                claim.agent_id, claim.claimed_at_unix_ms
            ),
        )?;
        let error = MezError::invalid_state(format!(
            "provider worker did not settle claimed task within {} ms",
            claim.timeout_ms
        ));
        self.fail_configured_agent_provider_task(turn_id, &error)?;
        Ok(true)
    }

    /// Returns whether the provider worker for a turn should continue.
    ///
    /// `/stop` can finish a turn after the async provider task has already
    /// claimed it from `pending_agent_provider_tasks`. The provider service
    /// polls this predicate while waiting so cancelled turns do not keep
    /// holding memory or network work after the user has stopped them.
    pub fn agent_turn_is_running(&self, turn_id: &str) -> bool {
        self.agent_turn_ledger
            .turns()
            .iter()
            .any(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)
    }

    /// Runs the prune stale agent provider tasks operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    fn prune_stale_agent_provider_tasks(&mut self) {
        let stale_turn_ids =
            self.pending_agent_provider_tasks
                .iter()
                .filter(|turn_id| {
                    let turn_id = turn_id.as_str();
                    !self.agent_turn_ledger.turns().iter().any(|turn| {
                        turn.turn_id == turn_id && turn.state == AgentTurnState::Running
                    }) || !self.agent_turn_model_profiles.contains_key(turn_id)
                })
                .cloned()
                .collect::<Vec<_>>();
        for turn_id in stale_turn_ids {
            self.pending_agent_provider_tasks.remove(&turn_id);
        }
    }

    /// Runs the poll agent provider tasks with provider operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn poll_agent_provider_tasks_with_provider<P: ModelProvider>(
        &mut self,
        provider: &P,
        limit: usize,
    ) -> Result<Vec<AgentTurnExecution>> {
        self.require_live()?;
        if limit == 0 {
            return Err(MezError::invalid_args(
                "agent provider task poll limit must be greater than zero",
            ));
        }

        self.prune_stale_agent_provider_tasks();
        let task_ids = self
            .pending_agent_provider_tasks
            .iter()
            .filter(|turn_id| {
                self.agent_turn_model_profiles
                    .get(*turn_id)
                    .is_some_and(|profile| profile.provider == provider.provider_id())
            })
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let mut executions = Vec::with_capacity(task_ids.len());
        for turn_id in task_ids {
            if self
                .agent_turn_executions
                .get(&turn_id)
                .is_some_and(|execution| {
                    self.execution_has_pending_shell_dispatch(&turn_id, execution)
                })
            {
                self.pending_agent_provider_tasks.remove(&turn_id);
                if let Some(execution) = self.dispatch_stored_running_shell_actions(&turn_id)? {
                    executions.push(execution);
                }
                continue;
            }
            let Some(model_profile) = self.agent_turn_model_profiles.get(&turn_id).cloned() else {
                self.pending_agent_provider_tasks.remove(&turn_id);
                continue;
            };
            self.pending_agent_provider_tasks.remove(&turn_id);
            if let Some(turn) = self
                .agent_turn_ledger
                .turns()
                .iter()
                .find(|turn| turn.turn_id == turn_id)
                .cloned()
            {
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn_id,
                    &format!(
                        "provider_task claimed reason=test_provider_poll provider={}",
                        provider.provider_id()
                    ),
                )?;
            }
            executions.push(self.execute_agent_turn_with_provider(
                &turn_id,
                provider,
                model_profile,
            )?);
        }
        Ok(executions)
    }

    /// Runs the runtime agent provider task operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn runtime_agent_provider_task(
        &self,
        turn_id: &str,
    ) -> Option<RuntimeAgentProviderTask> {
        let turn = self
            .agent_turn_ledger
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)?;
        let model_profile = self.agent_turn_model_profiles.get(turn_id)?.clone();
        Some(RuntimeAgentProviderTask {
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            model_profile,
        })
    }
}
