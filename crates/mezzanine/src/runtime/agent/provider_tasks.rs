//! Runtime agent provider task queue and worker lease helpers.
//!
//! This module owns provider task claiming, retry bookkeeping, failure ingress,
//! compatibility polling, and async worker lease tracking. It keeps provider
//! queue lifecycle decisions separate from provider execution and action
//! dispatch while preserving the runtime service method surface used by the
//! async actor and tests.

use crate::integrations::agent::provider::anthropic_provider_from_auth_store_with_provider_options;
use crate::runtime::config::runtime_effective_provider_options;
use crate::runtime::{
    RuntimeSideEffect, RuntimeTimerKey, RuntimeTimerKind, RuntimeTransition, current_unix_millis,
};
use mez_agent::{
    ProviderErrorRetryClass, ProviderRetryDispatchResult, ProviderRetryEffect, ProviderRetryEvent,
    ProviderRetryRecovery, ProviderRetryRecoveryResult, ProviderRetryTransition,
    provider_retry_after_delay_ms,
};

/// Runtime-facing details for one scheduled provider retry.
#[derive(Debug, Clone, Copy)]
struct RuntimeProviderRetrySchedule {
    /// One-based retry attempt and timer generation.
    attempt: u64,
    /// Configured finite retry limit retained for status reporting.
    max_attempts: u32,
    /// Whether eligible transient failures bypass the finite limit.
    unlimited: bool,
    /// Selected bounded backoff delay.
    delay_ms: u64,
}

/// Returns the complete serialized OpenAI Responses body size for one dispatch.
fn runtime_openai_dispatch_request_shape(
    dispatch: &RuntimeAgentProviderDispatch,
) -> Result<Option<(mez_agent::ModelRequest, usize, bool)>> {
    let RuntimeAgentProviderDispatchProvider::OpenAi(provider) = &dispatch.provider else {
        return Ok(None);
    };
    let mut request = if let Some(request) = dispatch
        .macro_judge_request
        .as_ref()
        .or(dispatch.sandbox_failure_assessment_request.as_ref())
    {
        request.clone()
    } else {
        assemble_model_request(
            &dispatch.model_profile,
            &dispatch.turn,
            &dispatch.context.to_agent_context(),
        )?
    };
    mez_agent::apply_model_request_control(
        &mut request,
        dispatch.allowed_actions.clone(),
        dispatch.interaction_kind,
    );
    mez_agent::apply_default_action_gates(
        &mut request,
        &dispatch.available_mcp_tools,
        dispatch.memory_actions_enabled,
        dispatch.issue_actions_enabled,
    );
    mez_agent::append_request_state_transition(&mut request);
    let stream = provider.streams_responses();
    mez_agent::prepare_openai_request_prefix_extension_with_context(
        &mut request,
        dispatch.context.previous_request(),
        &crate::integrations::agent::provider::AsyncModelProvider::cache_namespace(provider),
        stream,
    )?;
    let body = mez_agent::openai_responses_request_body_with_stream(&request, stream)?;
    Ok(Some((request, body.len(), stream)))
}

#[cfg(test)]
use super::AgentTurnExecution;
use super::{
    AgentId, AgentTurnRecord, AgentTurnState, DEFAULT_PROVIDER_TIMEOUT_MS, EventKind, HookEvent,
    MezError, PaneReadinessState, ProviderApiCompatibility, ReqwestProviderHttpTransport, Result,
    RuntimeAgentProviderClaim, RuntimeAgentProviderDispatch, RuntimeAgentProviderDispatchProvider,
    RuntimeAgentProviderTask, RuntimeProviderConfig, RuntimeSessionService, assemble_model_request,
    deepseek_chat_completions_provider_from_auth_store_with_provider_options, json_escape,
    openai_compatible_provider_from_auth_store_with_provider_options,
    openai_responses_provider_from_auth_store_with_provider_options, resolve_provider_api,
    runtime_agent_turn_start_hook_payload, runtime_mezzanine_error_code,
    runtime_provider_event_error,
};
#[cfg(test)]
use crate::integrations::agent::provider::ModelProvider;

impl RuntimeSessionService {
    /// Resolves the provider control state that remains active for one logical
    /// turn across actor-owned action execution boundaries.
    ///
    /// Routed presentation is always response-only. Otherwise the most recent
    /// accepted execution owns the cumulative capability surface and
    /// interaction mode; the turn's initial capability is used only before an
    /// execution exists. Explicit exceptional interactions remain authoritative.
    pub(crate) fn agent_provider_request_control_for_turn(
        &self,
        turn: &AgentTurnRecord,
    ) -> (
        Option<mez_agent::AllowedActionSet>,
        Option<mez_agent::ModelInteractionKind>,
    ) {
        let previous_execution = self.agent_turn_executions().get(&turn.turn_id);
        let allowed_actions = if self.routed_presentation_turn(&turn.turn_id) {
            Some(mez_agent::AllowedActionSet::for_capability(
                mez_agent::AgentCapability::RespondOnly,
            ))
        } else {
            previous_execution
                .map(|execution| execution.request.allowed_actions.clone())
                .or_else(|| {
                    turn.initial_capability
                        .map(mez_agent::AllowedActionSet::for_capability)
                })
        };
        let interaction_kind = self
            .agent
            .agent_turn_interaction_kinds
            .get(&turn.turn_id)
            .copied()
            .or_else(|| previous_execution.map(|execution| execution.request.interaction_kind));
        (allowed_actions, interaction_kind)
    }

    /// Installs a deterministic provider claim at a supplied context boundary
    /// for actor-level causal-order regression tests.
    #[cfg(test)]
    pub(crate) fn record_claimed_agent_provider_context_for_tests(
        &mut self,
        turn_id: &str,
        context_event_high_water_mark: u64,
    ) -> Result<()> {
        let turn = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("provider claim test turn is unavailable"))?;
        self.agent.claimed_agent_provider_tasks.insert(
            turn_id.to_string(),
            RuntimeAgentProviderClaim {
                turn_id: turn_id.to_string(),
                conversation_id: turn.conversation_id,
                agent_id: turn.agent_id,
                generation: 1,
                claimed_at_unix_ms: current_unix_millis(),
                timeout_ms: DEFAULT_PROVIDER_TIMEOUT_MS,
                context_event_high_water_mark,
                openai_request_bytes: None,
                openai_request_stream: None,
            },
        );
        self.agent.pending_agent_provider_tasks.remove(turn_id);
        Ok(())
    }

    /// Clears retry-attempt state for one provider turn.
    pub(crate) fn clear_agent_provider_retry_attempt(&mut self, turn_id: &str) {
        self.agent
            .provider_retry_scheduler
            .apply(ProviderRetryEvent::TurnSettled {
                turn_id: turn_id.to_string(),
            });
    }

    /// Returns the next bounded output-limit recovery stage for one turn.
    ///
    /// The state is owned by the active turn, so duplicate or stale provider
    /// failures cannot restart the continuation sequence after settlement.
    pub(crate) fn next_agent_output_limit_recovery_attempt(&self, turn_id: &str) -> u32 {
        self.agent
            .agent_turn_output_limit_recovery_attempts
            .get(turn_id)
            .copied()
            .unwrap_or_default()
            .saturating_add(1)
    }

    /// Returns provider turns whose progress is represented by retry policy state.
    pub(crate) fn agent_provider_retry_turn_ids(&self) -> impl Iterator<Item = &String> {
        self.agent.provider_retry_scheduler.turn_ids()
    }

    /// Builds the desired provider-poll timer transition for an external timer adapter.
    pub(crate) fn provider_poll_timer_transition(
        &self,
        timer_active: bool,
        generation: u64,
        delay_ms: u64,
    ) -> RuntimeTransition {
        if timer_active
            || self.pending_agent_provider_tasks().is_empty()
                && self.pending_agent_compaction_tasks().is_empty()
        {
            return RuntimeTransition::default();
        }
        RuntimeTransition {
            applied: false,
            side_effects: vec![RuntimeSideEffect::ScheduleTimer {
                key: RuntimeTimerKey::new(
                    RuntimeTimerKind::ProviderPoll,
                    "agent-provider",
                    generation,
                ),
                delay_ms: delay_ms.max(1),
            }],
        }
    }

    /// Applies runtime-owned provider retry recovery and emits the delayed retry effect.
    ///
    /// Returns `None` when the failure is not retryable or the retry budget is
    /// exhausted so the caller can continue with terminal failure handling.
    pub(crate) fn schedule_agent_provider_retry_transition(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        retry_class: ProviderErrorRetryClass,
        error: &MezError,
    ) -> Result<Option<RuntimeTransition>> {
        let decision = self.agent.provider_retry_scheduler.apply(
            ProviderRetryEvent::FailureObservedWithTiming {
                turn_id: turn_id.to_string(),
                retry_class,
                advised_delay_ms: provider_retry_after_delay_ms(
                    error.provider_failure_json(),
                    current_unix_millis(),
                ),
                jitter_sample: rand::random(),
            },
        );
        let ProviderRetryTransition::Effect(ProviderRetryEffect::Recover {
            recovery,
            attempt,
            max_attempts,
            unlimited,
            delay_ms,
            ..
        }) = decision
        else {
            return match decision {
                ProviderRetryTransition::Terminal => Ok(None),
                ProviderRetryTransition::Ignored
                | ProviderRetryTransition::Applied
                | ProviderRetryTransition::Abandoned => Ok(Some(RuntimeTransition::default())),
                ProviderRetryTransition::Effect(_) => Err(MezError::invalid_state(
                    "provider retry failure produced an unexpected scheduler effect",
                )),
            };
        };
        let recovered = match recovery {
            ProviderRetryRecovery::ContextLimit => self
                .recover_agent_provider_context_limit_failure(
                    agent_id,
                    turn_id,
                    error,
                    u32::try_from(attempt).map_err(|_| {
                        MezError::invalid_state(
                            "context-limit retry attempt exceeds supported range",
                        )
                    })?,
                )?,
            ProviderRetryRecovery::OutputLimit => self
                .recover_agent_provider_output_limit_failure(
                    agent_id,
                    turn_id,
                    error,
                    u32::try_from(attempt).map_err(|_| {
                        MezError::invalid_state(
                            "output-limit retry attempt exceeds supported range",
                        )
                    })?,
                )?,
            ProviderRetryRecovery::None => true,
        };
        if recovered
            && matches!(recovery, ProviderRetryRecovery::ContextLimit)
            && self
                .agent_compaction_resume_ids()
                .iter()
                .any(|resume_turn_id| resume_turn_id == turn_id)
        {
            let pane_id = self
                .agent_turn_ledger()
                .turns()
                .iter()
                .find(|turn| turn.turn_id == turn_id)
                .map(|turn| turn.pane_id.clone())
                .ok_or_else(|| MezError::invalid_state("queued compaction turn is unavailable"))?;
            return Ok(Some(RuntimeTransition {
                applied: true,
                side_effects: vec![RuntimeSideEffect::DispatchAgentCompaction { pane_id }],
            }));
        }
        if !recovered {
            self.agent
                .provider_retry_scheduler
                .apply(ProviderRetryEvent::RecoveryCompleted {
                    turn_id: turn_id.to_string(),
                    attempt,
                    result: ProviderRetryRecoveryResult::Failed,
                });
            return Ok(None);
        }
        let applied = match self.record_agent_provider_retry_event(
            agent_id,
            turn_id,
            error,
            RuntimeProviderRetrySchedule {
                attempt,
                max_attempts,
                unlimited,
                delay_ms,
            },
        ) {
            Ok(applied) => applied,
            Err(error) => {
                self.agent
                    .provider_retry_scheduler
                    .apply(ProviderRetryEvent::RecoveryCompleted {
                        turn_id: turn_id.to_string(),
                        attempt,
                        result: ProviderRetryRecoveryResult::TurnUnavailable,
                    });
                return Err(error);
            }
        };
        if !applied {
            let completion =
                self.agent
                    .provider_retry_scheduler
                    .apply(ProviderRetryEvent::RecoveryCompleted {
                        turn_id: turn_id.to_string(),
                        attempt,
                        result: ProviderRetryRecoveryResult::TurnUnavailable,
                    });
            return match completion {
                ProviderRetryTransition::Abandoned | ProviderRetryTransition::Ignored => {
                    Ok(Some(RuntimeTransition::default()))
                }
                _ => Err(MezError::invalid_state(
                    "unavailable provider retry turn produced an invalid scheduler transition",
                )),
            };
        }
        let completion =
            self.agent
                .provider_retry_scheduler
                .apply(ProviderRetryEvent::RecoveryCompleted {
                    turn_id: turn_id.to_string(),
                    attempt,
                    result: ProviderRetryRecoveryResult::Ready,
                });
        match completion {
            ProviderRetryTransition::Effect(ProviderRetryEffect::ScheduleTimer {
                turn_id,
                attempt,
                delay_ms,
            }) => Ok(Some(RuntimeTransition {
                applied: true,
                side_effects: vec![RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(RuntimeTimerKind::ProviderRetry, turn_id, attempt),
                    delay_ms,
                }],
            })),
            _ => Err(MezError::invalid_state(
                "provider retry recovery produced an invalid scheduler transition",
            )),
        }
    }

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
        model_profile: &mez_agent::ModelProfile,
        audit_scope: &str,
    ) -> Result<RuntimeAgentProviderDispatchProvider> {
        let api = resolve_provider_api(&provider_config.kind, provider_config.api.as_deref())?;
        let provider_options = runtime_effective_provider_options(provider_config, model_profile);
        self.append_credential_access_audit(
            provider_name,
            &provider_config.auth_profile,
            audit_scope,
            "requested",
        )?;
        let provider_result = (|| {
            let auth_store = self.integration.auth_store().ok_or_else(|| {
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
                        &provider_options,
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
                        &provider_options,
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
                        &provider_options,
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
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            return Ok(None);
        };
        if turn.agent_id != agent_id.as_str() {
            return Err(MezError::invalid_args(
                "agent provider dispatch agent id does not match turn",
            ));
        }
        if self
            .agent_shell_store()
            .get(&turn.pane_id)
            .is_none_or(|session| session.session_id != turn.conversation_id)
        {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            self.agent.claimed_agent_provider_tasks.remove(turn_id);
            let _ = self.agent.agent_scheduler.cancel(turn_id);
            self.finish_agent_turn_without_shell_session(&turn, AgentTurnState::Interrupted)?;
            return Ok(None);
        }
        if self.agent_is_compacting(&turn.pane_id) {
            return Ok(None);
        }
        if self
            .agent_turn_executions()
            .get(turn_id)
            .is_some_and(|execution| self.execution_has_pending_shell_dispatch(turn_id, execution))
        {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            let _ = self.dispatch_stored_running_shell_actions(turn_id)?;
            return Ok(None);
        }
        if !self.agent.pending_agent_provider_tasks.contains(turn_id) {
            return Ok(None);
        }
        if turn.state != AgentTurnState::Running {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            return Ok(None);
        }
        let native_mode = self.effective_agent_shell_mode_for_pane(&turn.pane_id)
            == crate::runtime::config::ShellMode::Native;
        let native_context = native_mode
            .then(|| self.native_shell_context_for_pane(&turn.pane_id))
            .transpose()?;
        if !native_mode && self.pane_has_uncertified_foreign_shell_boundary(&turn.pane_id) {
            if self.pane_foreign_shell_bootstrap_has_bounded_progress_owner(&turn.pane_id) {
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    "provider_task deferred reason=foreign_shell_bootstrap_pending",
                )?;
                return Ok(None);
            }
            return Err(MezError::invalid_state(
                "foreign shell bootstrap is unavailable; return to an empty prompt in the foreign environment and retry",
            ));
        }

        let primary_path_resolution_request =
            self.primary_path_resolution_request(&turn.pane_id)?;
        let subagent_scope = self.subagent_scope_declaration_for_turn(&turn);
        let subagent_path_resolution_request = subagent_scope
            .as_ref()
            .map(Self::subagent_path_resolution_request)
            .transpose()?
            .flatten();
        let path_resolution_required = !native_mode
            && (primary_path_resolution_request.is_some()
                || subagent_path_resolution_request.is_some());
        if path_resolution_required {
            match self.pane_environment_authority(&turn.pane_id) {
                crate::runtime::processes::RuntimePaneEnvironmentAuthority::Certified => {}
                crate::runtime::processes::RuntimePaneEnvironmentAuthority::Pending => {
                if !self.pane_bootstrap_has_bounded_progress_owner(&turn.pane_id)
                    && matches!(
                        self.pane_readiness_state(&turn.pane_id),
                        PaneReadinessState::Ready | PaneReadinessState::PromptCandidate
                    )
                {
                    self.dispatch_bootstrap_to_pane(&turn.pane_id)?;
                }
                if !self.pane_bootstrap_has_bounded_progress_owner(&turn.pane_id) {
                    return Err(MezError::invalid_state(
                        "pane bootstrap is pending without a bounded runtime progress owner",
                    ));
                }
                self.append_agent_trace_turn_event(
                    &turn.pane_id,
                    &turn.turn_id,
                    "provider_task deferred reason=pane_bootstrap_pending",
                )?;
                return Ok(None);
                }
                authority @ (crate::runtime::processes::RuntimePaneEnvironmentAuthority::Unavailable(_)
                | crate::runtime::processes::RuntimePaneEnvironmentAuthority::Unknown) => {
                    return Err(MezError::invalid_state(
                        authority.failure_message().unwrap_or_else(|| {
                            "pane environment authority is unavailable".to_string()
                        }),
                    ));
                }
            }
        }

        if path_resolution_required {
            match self.pane_readiness_state(&turn.pane_id) {
                PaneReadinessState::Ready => {}
                PaneReadinessState::Unknown
                | PaneReadinessState::PromptCandidate
                | PaneReadinessState::Degraded => {
                    if !self.turn_has_running_readiness_probe(&turn.turn_id) {
                        self.dispatch_readiness_probe_to_pane(&turn)?;
                    }
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        "provider_task deferred reason=path_resolution_readiness_probe",
                    )?;
                    return Ok(None);
                }
                PaneReadinessState::Probing => {
                    self.append_agent_trace_turn_event(
                        &turn.pane_id,
                        &turn.turn_id,
                        "provider_task deferred reason=path_resolution_readiness_probe_pending",
                    )?;
                    return Ok(None);
                }
                PaneReadinessState::Busy
                | PaneReadinessState::FullScreen
                | PaneReadinessState::PasswordPrompt
                | PaneReadinessState::InteractiveBlocked => {}
            }
        }

        let native_path_scopes = native_context
            .as_ref()
            .map(|context| self.native_path_scopes_for_turn(&turn, context))
            .transpose()?
            .flatten();
        let resolved_primary_path_scopes = if native_mode {
            subagent_scope
                .is_none()
                .then_some(native_path_scopes.clone())
                .flatten()
        } else if let Some(request) = primary_path_resolution_request {
            match self.path_scopes_for_pane_request(&turn.pane_id, &request)? {
                Some(scopes) => Some(scopes),
                None => {
                    let _ = self.dispatch_path_resolution_to_pane(&turn.pane_id, request)?;
                    return Ok(None);
                }
            }
        } else {
            None
        };

        let resolved_subagent_path_scopes = if native_mode {
            subagent_scope
                .is_some()
                .then_some(native_path_scopes)
                .flatten()
        } else if let Some(scope) = subagent_scope.as_ref() {
            if let Some(request) = subagent_path_resolution_request {
                match self.path_scopes_for_pane_request(&turn.pane_id, &request)? {
                    Some(scopes) => Some(if let Some(primary) = &resolved_primary_path_scopes {
                        primary
                            .intersection(&scopes)
                            .map_err(|error| MezError::invalid_state(error.message()))?
                    } else {
                        scopes
                    }),
                    None => {
                        let _ = self.dispatch_path_resolution_to_pane(&turn.pane_id, request)?;
                        return Ok(None);
                    }
                }
            } else {
                Some(
                    mez_agent::permissions::PathScopes::try_shell_resolved(
                        scope.current_directory.clone(),
                        Vec::new(),
                        Vec::new(),
                        Default::default(),
                    )
                    .map_err(|error| MezError::invalid_state(error.message()))?,
                )
            }
        } else {
            None
        };

        let model_profile = self
            .agent
            .agent_turn_model_profiles
            .get(turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn has no model profile"))?;
        let provider_config = self
            .provider_registry()
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
            &model_profile,
            "provider_request",
        )?;
        let macro_judge_step_index = self.macro_judge_step_index_for_turn(turn_id);
        let macro_judge_request = macro_judge_step_index
            .map(|step_index| self.macro_judge_request_for_turn(&turn, &model_profile, step_index))
            .transpose()?;
        let sandbox_failure_assessment_request =
            self.sandbox_failure_assessment_request_for_turn(turn_id);

        self.agent
            .agent_turn_model_profiles
            .insert(turn_id.to_string(), model_profile.clone());
        let (context, available_mcp_tools) =
            if macro_judge_step_index.is_some() || sandbox_failure_assessment_request.is_some() {
                let durable = self
                    .agent_turn_contexts()
                    .get(turn_id)
                    .cloned()
                    .ok_or_else(|| {
                        MezError::invalid_state("runtime agent turn context is unavailable")
                    })?;
                (
                    mez_agent::PreparedModelContext::from_durable(durable)?,
                    Vec::new(),
                )
            } else {
                self.refresh_agent_turn_mcp_catalog_context(&turn)?;
                self.refresh_agent_turn_project_guidance_context(&turn)?;
                let durable = self
                    .agent_turn_contexts()
                    .get(turn_id)
                    .cloned()
                    .ok_or_else(|| {
                        MezError::invalid_state("runtime agent turn context is unavailable")
                    })?;
                let mcp_summary = self.mcp_registry().prompt_summary();
                self.prepare_agent_turn_model_context(&turn, durable, &mcp_summary, &model_profile)?
            };
        let provider_context = context.to_agent_context();
        let respond_only = self.routed_presentation_turn(turn_id);
        let (allowed_actions, interaction_kind) =
            self.agent_provider_request_control_for_turn(&turn);
        let auto_sizing = if macro_judge_step_index.is_some()
            || sandbox_failure_assessment_request.is_some()
            || respond_only
        {
            None
        } else {
            self.runtime_auto_sizing_dispatch_for_turn(&turn, &model_profile)?
        };
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
        let auto_sizing_provider = if let Some(auto_sizing) = auto_sizing.as_ref() {
            let router_provider_config = self
                .provider_registry()
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
                &auto_sizing.router_profile,
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
        if let Some(auto_sizing) = auto_sizing.as_ref()
            && let Some(max_input_tokens) = auto_sizing.router_profile.max_input_tokens()
        {
            let router_provider_config = self
                .provider_registry()
                .provider(&auto_sizing.router_profile.provider)
                .cloned()
                .ok_or_else(|| {
                    MezError::config(format!(
                        "auto-sizing router provider `{}` is not configured",
                        auto_sizing.router_profile.provider
                    ))
                })?;
            let router_provider = auto_sizing_provider.as_ref().unwrap_or(&provider);
            let router_request =
                mez_agent::auto_sizing_request(auto_sizing, &turn, &provider_context)
                    .map_err(|error| MezError::invalid_state(error.message()))?;
            let router_api = resolve_provider_api(
                &router_provider_config.kind,
                router_provider_config.api.as_deref(),
            )?;
            let estimate = mez_agent::provider_request_input_estimate(
                &router_request,
                router_api,
                &runtime_effective_provider_options(
                    &router_provider_config,
                    &auto_sizing.router_profile,
                ),
                router_provider.request_stream(&router_request),
            )?;
            if self.defer_agent_provider_for_configured_input_limit(
                &turn,
                &auto_sizing.router_profile,
                &context,
                estimate,
                max_input_tokens,
            )? {
                return Ok(None);
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
        let available_mcp_servers = available_mcp_tools
            .iter()
            .map(|tool| tool.server_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if let Some(max_input_tokens) = model_profile.max_input_tokens() {
            let mut estimated_request = if let Some(request) = macro_judge_request
                .as_ref()
                .or(sandbox_failure_assessment_request.as_ref())
            {
                request.clone()
            } else {
                assemble_model_request(&model_profile, &turn, &provider_context)?
            };
            mez_agent::apply_model_request_control(
                &mut estimated_request,
                allowed_actions.clone(),
                interaction_kind,
            );
            mez_agent::apply_default_action_gates(
                &mut estimated_request,
                &available_mcp_tools,
                self.runtime_persistent_memory_enabled(),
                super::issues::runtime_issues_enabled(self),
            );
            let api = resolve_provider_api(&provider_config.kind, provider_config.api.as_deref())?;
            let estimate = mez_agent::provider_request_input_estimate(
                &estimated_request,
                api,
                &runtime_effective_provider_options(&provider_config, &model_profile),
                provider.request_stream(&estimated_request),
            )?;
            if self.defer_agent_provider_for_configured_input_limit(
                &turn,
                &model_profile,
                &context,
                estimate,
                max_input_tokens,
            )? {
                return Ok(None);
            }
            self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "configured_input_limit satisfied estimated_input_tokens={} max_input_tokens={max_input_tokens}",
                    estimate.input_tokens
                ),
            )?;
        }
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
                context.len()
            ),
        )?;
        self.record_runtime_provider_request_shape_for_context(
            &model_profile,
            &turn,
            &provider_context,
            &available_mcp_tools,
            self.runtime_persistent_memory_enabled(),
            super::issues::runtime_issues_enabled(self),
        );
        if self.agent_debug_enabled(&turn.pane_id) {
            match assemble_model_request(&model_profile, &turn, &provider_context) {
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
                        super::issues::runtime_issues_enabled(self),
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
        let path_scopes = if subagent_scope.is_some() {
            resolved_subagent_path_scopes
        } else if native_mode {
            resolved_primary_path_scopes
        } else {
            resolved_primary_path_scopes.or_else(|| self.path_scopes_for_pane(&turn.pane_id))
        };
        let permission_policy = self.permission_policy_for_turn(&turn);
        let sandbox_config = self.sandbox_config_for_pane(&turn.pane_id);
        let sandbox_first_local_prompts =
            crate::runtime::config::sandbox_applies_to_policy(&sandbox_config, &permission_policy);
        let shell_classification = native_context
            .as_ref()
            .map(crate::runtime::processes::NativeShellContext::classification)
            .unwrap_or_else(|| self.shell_classification_for_pane(&turn.pane_id))
            .as_str()
            .to_string();
        self.agent.pending_agent_provider_tasks.remove(turn_id);
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            "provider_task claimed reason=async_provider_worker",
        )?;
        Ok(Some(RuntimeAgentProviderDispatch {
            turn,
            context,
            allowed_actions,
            interaction_kind,
            model_profile,
            macro_judge_request,
            sandbox_failure_assessment_request,
            auto_sizing,
            auto_sizing_provider,
            provider,
            permission_policy,
            sandbox_first_local_prompts,
            shell_classification,
            session_approvals: self.session_approvals().clone(),
            path_scopes,
            subagent_scope,
            available_mcp_servers,
            available_mcp_tools,
            memory_actions_enabled: self.runtime_persistent_memory_enabled(),
            issue_actions_enabled: super::issues::runtime_issues_enabled(self),
            loop_turn: self.agent.agent_loop_turns.get(turn_id).cloned(),
        }))
    }

    /// Runs the fail configured agent provider task operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn fail_configured_agent_provider_task(
        &mut self,
        turn_id: &str,
        error: &MezError,
    ) -> Result<()> {
        if self.settle_pending_sandbox_failure_assessment(turn_id, "provider_failure")? {
            return Ok(());
        }
        let Some(turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            self.agent.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(());
        };
        if !matches!(
            turn.state,
            AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
        ) {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            self.agent.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(());
        }
        let Some(model_profile) = self.agent.agent_turn_model_profiles.get(turn_id).cloned() else {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            self.agent.claimed_agent_provider_tasks.remove(turn_id);
            return Err(MezError::invalid_state(
                "runtime agent turn has no model profile",
            ));
        };
        self.agent.pending_agent_provider_tasks.remove(turn_id);
        self.agent.claimed_agent_provider_tasks.remove(turn_id);
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
        self.integration
            .runtime_metrics_mut()
            .record_provider_failure();
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
    fn record_agent_provider_retry_event(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        error: &MezError,
        schedule: RuntimeProviderRetrySchedule,
    ) -> Result<bool> {
        let RuntimeProviderRetrySchedule {
            attempt,
            max_attempts,
            unlimited,
            delay_ms,
        } = schedule;
        let Some(turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.agent_id != agent_id.as_str() {
            return Err(MezError::invalid_args(
                "agent provider event agent id does not match turn",
            ));
        }
        if turn.state != AgentTurnState::Running {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        let Some(model_profile) = self.agent.agent_turn_model_profiles.get(turn_id).cloned() else {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            return Err(MezError::invalid_state(
                "runtime agent turn has no model profile",
            ));
        };
        self.agent.pending_agent_provider_tasks.remove(turn_id);
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
                "provider_task retry_scheduled provider={} error_kind={} attempt={} max_attempts={} unlimited={} delay_ms={}",
                model_profile.provider,
                runtime_mezzanine_error_code(error.kind()),
                attempt,
                max_attempts,
                unlimited,
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
            &if unlimited {
                format!(
                    "agent: provider {} request failed ({}); retrying attempt {attempt} (unlimited) in {delay_ms} ms",
                    model_profile.provider,
                    runtime_mezzanine_error_code(error.kind())
                )
            } else {
                format!(
                    "agent: provider {} request failed ({}); retrying attempt {attempt}/{max_attempts} in {delay_ms} ms",
                    model_profile.provider,
                    runtime_mezzanine_error_code(error.kind())
                )
            },
        )?;
        self.append_lifecycle_event(
            EventKind::AgentStatus,
            format!(
                r#"{{"pane_id":"{}","agent_prompt_turn":"{}","state":"running","provider":"{}","provider_retry":"scheduled","attempt":{},"max_attempts":{},"unlimited":{},"delay_ms":{},"error_kind":"{}"}}"#,
                json_escape(&turn.pane_id),
                json_escape(turn_id),
                json_escape(&model_profile.provider),
                attempt,
                max_attempts,
                unlimited,
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
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.state != AgentTurnState::Running {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        if !self.agent.agent_turn_model_profiles.contains_key(turn_id) {
            return Err(MezError::invalid_state(
                "runtime agent turn has no model profile",
            ));
        }
        self.agent
            .pending_agent_provider_tasks
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

    /// Applies a provider retry timer through the runtime-owned transition boundary.
    pub(crate) fn apply_agent_provider_retry_timer_transition(
        &mut self,
        turn_id: &str,
        attempt: u64,
    ) -> Result<RuntimeTransition> {
        let decision =
            self.agent
                .provider_retry_scheduler
                .apply(ProviderRetryEvent::TimerElapsed {
                    turn_id: turn_id.to_string(),
                    attempt,
                });
        let ProviderRetryTransition::Effect(ProviderRetryEffect::DispatchProvider { .. }) =
            decision
        else {
            return match decision {
                ProviderRetryTransition::Ignored | ProviderRetryTransition::Abandoned => {
                    Ok(RuntimeTransition::default())
                }
                _ => Err(MezError::invalid_state(
                    "provider retry timer produced an invalid scheduler transition",
                )),
            };
        };
        let queued = match self.queue_agent_provider_retry_task(turn_id, attempt) {
            Ok(queued) => queued,
            Err(error) => {
                self.agent
                    .provider_retry_scheduler
                    .apply(ProviderRetryEvent::DispatchCompleted {
                        turn_id: turn_id.to_string(),
                        attempt,
                        result: ProviderRetryDispatchResult::TurnUnavailable,
                    });
                return Err(error);
            }
        };
        let completion =
            self.agent
                .provider_retry_scheduler
                .apply(ProviderRetryEvent::DispatchCompleted {
                    turn_id: turn_id.to_string(),
                    attempt,
                    result: if queued {
                        ProviderRetryDispatchResult::Ready
                    } else {
                        ProviderRetryDispatchResult::TurnUnavailable
                    },
                });
        if !queued {
            return match completion {
                ProviderRetryTransition::Abandoned | ProviderRetryTransition::Ignored => {
                    Ok(RuntimeTransition::default())
                }
                _ => Err(MezError::invalid_state(
                    "unavailable provider retry dispatch produced an invalid scheduler transition",
                )),
            };
        }
        if completion != ProviderRetryTransition::Applied {
            return Err(MezError::invalid_state(
                "provider retry dispatch produced an invalid scheduler transition",
            ));
        }
        let task = self.runtime_agent_provider_task(turn_id).ok_or_else(|| {
            self.clear_agent_provider_retry_attempt(turn_id);
            MezError::invalid_state("queued provider retry has no dispatch task")
        })?;
        let agent_id = AgentId::opaque(task.agent_id).ok_or_else(|| {
            MezError::invalid_state("queued provider retry has an invalid agent id")
        })?;
        Ok(RuntimeTransition {
            applied: true,
            side_effects: vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id,
                turn_id: task.turn_id,
            }],
        })
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
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.state != AgentTurnState::Running {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        if !self.agent.agent_turn_model_profiles.contains_key(turn_id) {
            return Err(MezError::invalid_state(
                "runtime agent turn has no model profile",
            ));
        }
        self.agent
            .pending_agent_provider_tasks
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

    /// Completes deferred context-limit recovery after model compaction and
    /// queues exactly one provider retry while preserving its bounded attempt.
    pub(crate) fn queue_agent_provider_recovery_task_after_context_compaction(
        &mut self,
        turn_id: &str,
        attempt: u32,
    ) -> Result<bool> {
        if self.agent.provider_retry_scheduler.attempt(turn_id) == 0 {
            return self.queue_agent_provider_recovery_task_after_compaction(turn_id);
        }
        let recovery =
            self.agent
                .provider_retry_scheduler
                .apply(ProviderRetryEvent::RecoveryCompleted {
                    turn_id: turn_id.to_string(),
                    attempt: u64::from(attempt),
                    result: ProviderRetryRecoveryResult::Ready,
                });
        if !matches!(
            recovery,
            ProviderRetryTransition::Effect(ProviderRetryEffect::ScheduleTimer { .. })
        ) {
            return Err(MezError::invalid_state(
                "context compaction completed outside its provider retry recovery phase",
            ));
        }
        let timer = self
            .agent
            .provider_retry_scheduler
            .apply(ProviderRetryEvent::TimerElapsed {
                turn_id: turn_id.to_string(),
                attempt: u64::from(attempt),
            });
        if !matches!(
            timer,
            ProviderRetryTransition::Effect(ProviderRetryEffect::DispatchProvider { .. })
        ) {
            return Err(MezError::invalid_state(
                "context compaction retry did not become dispatchable",
            ));
        }
        let queued = self.queue_agent_provider_retry_task(turn_id, u64::from(attempt))?;
        let completion =
            self.agent
                .provider_retry_scheduler
                .apply(ProviderRetryEvent::DispatchCompleted {
                    turn_id: turn_id.to_string(),
                    attempt: u64::from(attempt),
                    result: if queued {
                        ProviderRetryDispatchResult::Ready
                    } else {
                        ProviderRetryDispatchResult::TurnUnavailable
                    },
                });
        if queued && completion != ProviderRetryTransition::Applied {
            return Err(MezError::invalid_state(
                "context compaction provider retry produced an invalid scheduler transition",
            ));
        }
        Ok(queued)
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
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.agent.pending_agent_provider_tasks.remove(turn_id);
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

    /// Applies a terminal provider failure through the transport-neutral transition contract.
    pub(crate) fn apply_agent_provider_failed_transition(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        kind: &str,
        message: &str,
        provider_failure_json: Option<&str>,
        provider_raw_text: Option<&str>,
    ) -> Result<crate::runtime::RuntimeTransition> {
        let pane_id = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .map(|turn| turn.pane_id.clone());
        let applied = self.apply_agent_provider_failed_event(
            agent_id,
            turn_id,
            kind,
            message,
            provider_failure_json,
            provider_raw_text,
        )?;
        if let Some(ref pane_id) = pane_id {
            self.discard_agent_streaming_say_presentation(pane_id, Some(turn_id))?;
            self.clear_agent_shell_output_status_line(pane_id)?;
        }
        Ok(pane_id.map_or_else(
            || {
                self.runtime_transition_with_render(
                    applied,
                    Some(crate::runtime::RenderInvalidationReason::FullRedraw),
                )
            },
            |pane_id| {
                self.runtime_pane_transition_with_render(
                    &pane_id,
                    applied,
                    Some(crate::runtime::RenderInvalidationReason::FullRedraw),
                )
            },
        ))
    }

    /// Runs the pending agent provider tasks operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn pending_agent_provider_tasks(&self) -> Vec<RuntimeAgentProviderTask> {
        self.agent
            .pending_agent_provider_tasks
            .iter()
            .filter_map(|turn_id| self.runtime_agent_provider_task(turn_id))
            .filter(|task| !self.agent_is_compacting(&task.pane_id))
            .filter(|task| {
                !self.routed_provider_task_waits_for_managed_shell_startup(&task.turn_id)
            })
            .collect()
    }

    /// Reports whether a routed worker must remain internal to the runtime
    /// while managed shell startup owns the interval before bootstrap can be
    /// registered safely.
    ///
    /// The provider claim guard deliberately does not accept this admission as
    /// pane environment authority. Suppressing actor dispatch here prevents a
    /// worker from claiming the task until prompt readiness installs a timed
    /// bootstrap transaction or managed startup settles terminally.
    fn routed_provider_task_waits_for_managed_shell_startup(&self, turn_id: &str) -> bool {
        if !self
            .agent
            .routed_workflow_by_child_turn
            .contains_key(turn_id)
        {
            return false;
        }
        let Some(turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
        else {
            return false;
        };
        let primary_path_resolution_required = self
            .primary_path_resolution_request(&turn.pane_id)
            .is_ok_and(|request| request.is_some());
        let subagent_path_resolution_required = self
            .subagent_scope_declaration_for_turn(turn)
            .as_ref()
            .and_then(|scope| Self::subagent_path_resolution_request(scope).ok())
            .flatten()
            .is_some();
        (primary_path_resolution_required || subagent_path_resolution_required)
            && self.pane_environment_authority(&turn.pane_id)
                == crate::runtime::processes::RuntimePaneEnvironmentAuthority::Pending
            && self.pane_readiness_state(&turn.pane_id) == PaneReadinessState::Unknown
            && !self.pane_bootstrap_has_bounded_progress_owner(&turn.pane_id)
            && self.pane_has_bounded_managed_shell_startup(&turn.pane_id)
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
    ) -> Result<RuntimeTransition> {
        let turn = &dispatch.turn;
        let retain_request_chain = dispatch.auto_sizing.is_none()
            && dispatch.macro_judge_request.is_none()
            && dispatch.sandbox_failure_assessment_request.is_none();
        let openai_request_shape = runtime_openai_dispatch_request_shape(dispatch)?;
        let openai_request_bytes = openai_request_shape
            .as_ref()
            .map(|(_, request_bytes, _)| *request_bytes);
        let openai_request_stream = openai_request_shape.as_ref().map(|(_, _, stream)| *stream);
        if retain_request_chain && let Some((request, _, _)) = openai_request_shape {
            self.agent
                .agent_turn_provider_request_chains
                .insert(turn.turn_id.clone(), request);
        }
        self.agent.claimed_agent_provider_tasks.insert(
            turn.turn_id.clone(),
            RuntimeAgentProviderClaim {
                turn_id: turn.turn_id.clone(),
                conversation_id: turn.conversation_id.clone(),
                agent_id: turn.agent_id.clone(),
                generation,
                claimed_at_unix_ms: current_unix_millis(),
                timeout_ms,
                context_event_high_water_mark: dispatch
                    .context
                    .durable()
                    .event_sequence_high_water_mark(),
                openai_request_bytes,
                openai_request_stream,
            },
        );
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            &turn.turn_id,
            &format!(
                "provider_task claim_lease started generation={generation} timeout_ms={timeout_ms}"
            ),
        )?;
        Ok(RuntimeTransition {
            applied: true,
            side_effects: vec![RuntimeSideEffect::ScheduleTimer {
                key: RuntimeTimerKey::new(
                    RuntimeTimerKind::ProviderClaim,
                    turn.turn_id.clone(),
                    generation,
                ),
                delay_ms: timeout_ms,
            }],
        })
    }

    /// Clears the provider-worker claim lease for a settled turn.
    pub(crate) fn clear_claimed_agent_provider_task(&mut self, turn_id: &str) {
        let pane_id = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .map(|turn| turn.pane_id.clone());
        self.agent.claimed_agent_provider_tasks.remove(turn_id);
        if let Some(pane_id) = pane_id {
            let _ = self.clear_agent_shell_output_status_line(&pane_id);
        }
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
        let Some(claim) = self
            .agent
            .claimed_agent_provider_tasks
            .get(turn_id)
            .cloned()
        else {
            return Ok(false);
        };
        if claim.turn_id != turn_id {
            self.agent.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        if claim.generation != generation {
            return Ok(false);
        }
        let Some(turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            self.agent.claimed_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        };
        if turn.state != AgentTurnState::Running {
            self.agent.claimed_agent_provider_tasks.remove(turn_id);
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
        self.agent_turn_ledger()
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
            self.agent
                .pending_agent_provider_tasks
                .iter()
                .filter(|turn_id| {
                    let turn_id = turn_id.as_str();
                    !self.agent_turn_ledger().turns().iter().any(|turn| {
                        turn.turn_id == turn_id && turn.state == AgentTurnState::Running
                    }) || !self.agent.agent_turn_model_profiles.contains_key(turn_id)
                })
                .cloned()
                .collect::<Vec<_>>();
        for turn_id in stale_turn_ids {
            self.agent.pending_agent_provider_tasks.remove(&turn_id);
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
            .agent
            .pending_agent_provider_tasks
            .iter()
            .filter(|turn_id| {
                self.agent
                    .agent_turn_model_profiles
                    .get(*turn_id)
                    .is_some_and(|profile| profile.provider == provider.provider_id())
            })
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let mut executions = Vec::with_capacity(task_ids.len());
        for turn_id in task_ids {
            if self
                .agent_turn_executions()
                .get(&turn_id)
                .is_some_and(|execution| {
                    self.execution_has_pending_shell_dispatch(&turn_id, execution)
                })
            {
                self.agent.pending_agent_provider_tasks.remove(&turn_id);
                if let Some(execution) = self.dispatch_stored_running_shell_actions(&turn_id)? {
                    executions.push(execution);
                }
                continue;
            }
            let Some(model_profile) = self.agent.agent_turn_model_profiles.get(&turn_id).cloned()
            else {
                self.agent.pending_agent_provider_tasks.remove(&turn_id);
                continue;
            };
            self.agent.pending_agent_provider_tasks.remove(&turn_id);
            if let Some(turn) = self
                .agent_turn_ledger()
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
                if !self.routed_presentation_turn(&turn_id)
                    && let Some(auto_sizing) =
                        self.runtime_auto_sizing_dispatch_for_turn(&turn, &model_profile)?
                {
                    let context = self
                        .agent_turn_contexts()
                        .get(&turn_id)
                        .cloned()
                        .ok_or_else(|| {
                            MezError::invalid_state("runtime agent turn context is unavailable")
                        })?;
                    let auto_sizing_execution =
                        match crate::runtime::runtime_execute_auto_sizing_with_provider(
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
                    self.record_auto_sizing_outcome(
                        &turn,
                        &auto_sizing_execution.selected_profile,
                        auto_sizing_execution.decision.as_ref(),
                        auto_sizing_execution.fallback.as_deref(),
                    )?;
                    let agent_id = AgentId::opaque(turn.agent_id.clone()).ok_or_else(|| {
                        MezError::invalid_state("runtime agent turn has an invalid agent id")
                    })?;
                    self.apply_routing_selected_transition(
                        &agent_id,
                        &turn_id,
                        auto_sizing_execution.into_routing_selection(),
                    )?;
                    continue;
                }
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
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id && turn.state == AgentTurnState::Running)?;
        let model_profile = self.agent.agent_turn_model_profiles.get(turn_id)?.clone();
        Some(RuntimeAgentProviderTask {
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            model_profile,
        })
    }
}
