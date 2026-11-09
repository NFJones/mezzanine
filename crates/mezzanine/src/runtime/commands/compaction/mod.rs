//! Agent-shell conversation compaction commands and provider request helpers.
//!
//! This module owns `/compact`, queued canonical compaction tasks,
//! summary-only provider request construction, private summary staging,
//! and canonical replay publication. Keeping compaction isolated
//! avoids mixing long-running summarization workflow with ordinary shell
//! command dispatch. Manual model input keeps physical source proofs distinct
//! from summary replacements, and head publication compares the frozen epoch
//! and rows under the mutation lease before changing durable replay authority.
//! Test-only candidate previews measure prospective requests without affecting
//! acceptance; source/preparation owners retain exact publication fences.

#[cfg(test)]
use super::{AgentContext, ContextBlock, ModelInteractionKind, current_unix_seconds};
use super::{
    AgentShellCommandOutcome, AgentTurnRecord, AgentTurnState, AllowedActionSet, ContextSourceKind,
    DEFAULT_PROVIDER_TIMEOUT_MS, MezError, ModelProfile, ModelRequest, ModelResponse,
    ProviderApiCompatibility, ReqwestProviderHttpTransport, Result, RuntimeAgentCompactionDispatch,
    RuntimeAgentCompactionTask, RuntimeAgentProviderDispatchProvider, RuntimeSessionService,
    TranscriptEntry, TranscriptRole,
    deepseek_chat_completions_provider_from_auth_store_with_provider_options, json_escape,
    openai_compatible_provider_from_auth_store_with_provider_options_and_brand,
    openai_responses_provider_from_auth_store_with_provider_options, parse_slash_command,
    resolve_provider_api,
};
#[cfg(test)]
use crate::integrations::agent::context::assemble_model_request;
use crate::integrations::agent::provider::{
    anthropic_provider_from_auth_store_with_provider_options, bounded_provider_event_kind,
    provider_error_retry_class_from_parts, provider_event_error_kind,
};
use crate::runtime::agent_state::RuntimeActiveTurnCompactionTrigger;
use crate::runtime::config::runtime_effective_provider_options;
use crate::runtime::{AgentCompactionEvent, RenderInvalidationReason, RuntimeTransition};
use crate::security::auth::AuthProfileCredentialSource;
use mez_agent::ProviderErrorRetryClass;

#[cfg(test)]
mod candidate;
mod historical_handoff;
/// Canonical input-boundary regressions.
#[cfg(test)]
mod input;
mod no_work;
mod preparation;
mod replay_epoch;
pub(crate) use replay_epoch::{
    RuntimeCompactionReplaySource, RuntimeReplayEpochAttempt, RuntimeReplayEpochCompletion,
    RuntimeReplayEpochPublication,
};
mod source;
#[cfg(test)]
mod tests;
use no_work::NoWorkReason;
pub(crate) use preparation::{
    RuntimeManualCompactionPreparation, RuntimeManualCompactionRequestWork,
};

/// Content-free component estimates for a failed complete provider request.
#[cfg(test)]
fn runtime_compaction_candidate_size_diagnostic(
    context: &AgentContext,
    projection: Option<mez_agent::ProviderBudgetProjection<'_>>,
    total_tokens: usize,
    safe_tokens: usize,
) -> String {
    let costs = mez_agent::projected_context_block_input_tokens(context, projection);
    let mut protected = 0usize;
    let mut optional = 0usize;
    for (block, cost) in context.blocks().iter().zip(costs) {
        if block.retention() == mez_agent::ContextRetention::Exact
            || block.source == ContextSourceKind::TranscriptUser
        {
            protected = protected.saturating_add(cost);
        } else {
            optional = optional.saturating_add(cost);
        }
    }
    let overhead = total_tokens.saturating_sub(protected.saturating_add(optional));
    format!(
        "estimated_input_tokens={total_tokens} safe_input_tokens={safe_tokens} protected_block_estimate={protected} optional_block_estimate={optional} non_block_or_accounting_residual={overhead}"
    )
}

impl RuntimeSessionService {
    /// Executes `/compact` by queuing model-backed conversation compaction.
    pub(super) fn execute_agent_shell_compact_command(
        &mut self,
        pane_id: &str,
        input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("compact command must be a slash command"))?;
        if !invocation.args.trim().is_empty() {
            return Err(MezError::invalid_args(
                "compact command does not accept arguments",
            ));
        }
        if self.agent.agent_command_is_active(pane_id) {
            return Err(MezError::conflict(
                "cannot compact while accepted command/history preparation is active",
            ));
        }
        if self.agent_source_preparation_uses_adapter() {
            return self.admit_manual_compaction_preparation(pane_id);
        }
        self.queue_manual_compaction(pane_id, None)
    }

    /// Queues manual compaction only from the shared canonical history owner.
    ///
    /// Source preparation owns legacy import and original pending rows. Neither
    /// pane counts nor flat transcript roles may select another summary engine.
    /// Prepared commands retain their existing generation through request work;
    /// inline commands capture the same source before queuing provider work.
    fn queue_manual_compaction(
        &mut self,
        pane_id: &str,
        prepared: Option<(
            crate::runtime::RuntimeAgentTranscriptSnapshot,
            RuntimeManualCompactionPreparation,
        )>,
    ) -> Result<AgentShellCommandOutcome> {
        let session = self
            .agent_shell_store()
            .get(pane_id)
            .cloned()
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "agent shell session not found for pane",
                )
            })?;
        if let Some(turn) = &session.running_turn_id {
            return Err(MezError::conflict(format!(
                "cannot compact conversation while turn {turn} is running"
            )));
        }
        if self.agent_is_compacting(pane_id)
            && !prepared.as_ref().is_some_and(|(_, owner)| {
                self.agent_compaction_task_is_current(pane_id, owner.task_generation)
            })
        {
            return Err(MezError::conflict(format!(
                "cannot compact conversation while pane {pane_id} is already compacting"
            )));
        }
        let _ = self.runtime_prune_expired_persistent_memory_best_effort();
        let no_work = |service: &mut Self, reason: NoWorkReason| {
            service.append_agent_status_text_to_terminal_buffer(pane_id, reason.status())?;
            service.record_manual_compaction_no_work(
                pane_id,
                &session.session_id,
                reason,
                "manual",
            )?;
            Ok(AgentShellCommandOutcome::Display {
                command: "compact".into(),
                body: format!(
                    "pane={} conversation={} previous_transcript_entries={} summarized_entries=0 compacted=false reason={} source=model-compact trigger=manual",
                    json_escape(pane_id),
                    json_escape(&session.session_id),
                    session.transcript_entries,
                    reason.code(),
                ),
            })
        };
        if session.transcript_entries == 0
            && prepared.is_none()
            && self.manual_source_is_known_empty(pane_id)
        {
            return no_work(self, NoWorkReason::NoTranscriptEntries);
        }
        let (snapshot, preparation) = if let Some((snapshot, preparation)) = prepared {
            (snapshot, Some(preparation))
        } else {
            if self.persistence.transcript_store().is_some() {
                self.prepare_agent_context_prerequisites(pane_id, true)?;
            }
            let high_water = self
                .agent_shell_store()
                .get(pane_id)
                .ok_or_else(|| MezError::invalid_state("manual source session unavailable"))?
                .transcript_high_water;
            let snapshot = source::ManualCompactionSourceWork::capture(
                self.persistence.transcript_store().cloned(),
                session.session_id.clone(),
            )
            .execute_with_history(
                self.prepare_runtime_agent_prompt_history_work(pane_id),
                high_water,
            )?;
            (snapshot, None)
        };
        if let Some(owner) = snapshot.history.replay_owner()? {
            return self.queue_manual_replay_compaction(
                pane_id,
                owner,
                snapshot.history,
                preparation,
            );
        }
        if !snapshot.rows.is_empty()
            || snapshot
                .history
                .blocks
                .iter()
                .any(|block| block.source != ContextSourceKind::Memory)
        {
            return Err(MezError::invalid_state(
                "manual compaction history lacks its canonical source owner",
            ));
        }
        // Optional memory does not turn actual source absence into model work.
        // Even a zero-model physical window must retain its checked importer.
        let reason = if session.transcript_entries == 0 {
            NoWorkReason::NoTranscriptEntries
        } else {
            NoWorkReason::NoDurableTranscript
        };
        no_work(self, reason)
    }

    /// Records only source eligibility, not provider execution or completion.
    fn record_manual_compaction_no_work(
        &mut self,
        pane: &str,
        conversation: &str,
        reason: NoWorkReason,
        trigger: &str,
    ) -> Result<()> {
        self.append_lifecycle_event(crate::runtime::EventKind::Diagnostic,
            serde_json::json!({"kind":"manual_compaction_no_work","pane_id":pane,"conversation_id":conversation,"reason":reason.code(),"compacted":false,"provider_queued":false,"trigger":trigger}).to_string())
    }

    /// Queues model-backed compaction for one frozen active-turn context.
    pub(crate) fn queue_agent_context_limit_recovery_compaction(
        &mut self,
        turn_id: &str,
        model_profile_name: String,
        model_profile: ModelProfile,
        recovery_attempt: u32,
    ) -> Result<bool> {
        self.queue_agent_active_turn_compaction(
            turn_id,
            model_profile_name,
            model_profile,
            RuntimeActiveTurnCompactionTrigger::ProviderContextLimit {
                attempt: recovery_attempt,
            },
        )
    }

    /// Queues one typed active-turn compaction while preserving its trigger.
    pub(crate) fn queue_agent_active_turn_compaction(
        &mut self,
        turn_id: &str,
        model_profile_name: String,
        model_profile: ModelProfile,
        trigger: RuntimeActiveTurnCompactionTrigger,
    ) -> Result<bool> {
        let Some(turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            return Ok(false);
        };
        if turn.state != AgentTurnState::Running || self.agent_is_compacting(&turn.pane_id) {
            return Ok(false);
        }
        let (conversation_id, transcript_entries) = self
            .agent_shell_store()
            .get(&turn.pane_id)
            .map(|session| (session.session_id.clone(), session.transcript_entries))
            .ok_or_else(|| {
                MezError::invalid_state("active-turn compaction pane session is unavailable")
            })?;
        let allowed_actions = self.capture_agent_session_allowed_actions_for_pane(&turn.pane_id)?;
        let replay_source = match self.prepare_active_replay_compaction(
            &turn,
            &model_profile,
            trigger,
            &allowed_actions,
        )? {
            replay_epoch::Preparation::NoWork(reason) => {
                if let NoWorkReason::UnchangedPublishedSource { head_revision } = reason {
                    let unavailable = matches!(
                        trigger,
                        RuntimeActiveTurnCompactionTrigger::ProviderContextLimit { .. }
                    );
                    let status = if unavailable {
                        "agent: context unavailable; unchanged published source cannot be compacted again automatically"
                    } else {
                        reason.status()
                    };
                    self.append_agent_status_text_to_terminal_buffer(&turn.pane_id, status)?;
                    self.append_agent_trace_turn_event(&turn.pane_id, turn_id, &format!(
                            "compaction_reentry suppressed head_revision={head_revision} reason=unchanged_published_source context_unavailable={unavailable}"
                        ))?;
                }
                return Ok(false);
            }
            replay_epoch::Preparation::Ready(source) => source,
        };
        let summarized_entries = replay_source.input_entry_count();
        let request = replay_source.requires_model_summary().then(|| {
            runtime_model_compaction_request_from_source(
                &model_profile,
                &turn.pane_id,
                &conversation_id,
                String::new(),
                allowed_actions,
            )
        });
        self.refresh_project_trust_store_from_disk_if_changed()?;
        self.queue_agent_compaction_task(RuntimeAgentCompactionTask {
            source_owner: Some(replay_source.original.clone()),
            replay_source: Some(replay_source),
            task_generation: 0,
            compaction_epoch: 0,
            transport_retry_attempts: 0,
            transport_retry_delay_ms: None,
            pane_id: turn.pane_id.clone(),
            accounting_origin: self.capture_agent_accounting_origin_for_pane(&turn.pane_id),
            conversation_id,
            source: match trigger {
                RuntimeActiveTurnCompactionTrigger::ProviderContextLimit { .. } => {
                    "provider-context-limit".to_string()
                }
                RuntimeActiveTurnCompactionTrigger::ObservedInputLimit { .. } => {
                    "observed-input-limit".to_string()
                }
            },
            // These physical counts are diagnostics. Both pressure triggers
            // retain their original source and wait for canonical publication,
            // including backing for ordinary observations not yet archived.
            transcript_entries,
            summarized_entries,
            model_profile_name,
            model_profile,
            request,
            resume_turn_id: Some(turn.turn_id.clone()),
            compaction_request_shape: None,
        });
        self.remove_pending_agent_provider_task(turn_id);
        let status = if self.agent_compaction_mode()
            == crate::runtime::config::CompactionMode::RawTail
        {
            format!(
                "agent: publishing raw-tail context compaction mode={}",
                self.agent_compaction_mode().name()
            )
        } else {
            match trigger {
            RuntimeActiveTurnCompactionTrigger::ProviderContextLimit { .. } => {
                "agent: provider rejected context as too large; requesting model-backed context compaction"
                    .to_string()
            }
            RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                observed_input_tokens,
                max_input_tokens,
            } => format!(
                "agent: observed execution input reached configured limit; requesting model-backed context compaction observed_input_tokens={observed_input_tokens} max_input_tokens={max_input_tokens}"
            ),
        }
        };
        self.append_agent_status_text_to_terminal_buffer(&turn.pane_id, &status)?;
        Ok(true)
    }

    /// Returns pane ids with queued summary or raw-tail compaction tasks.
    pub fn pending_agent_compaction_tasks(&self) -> Vec<String> {
        self.pending_agent_compaction_task_ids()
    }

    /// Returns running turns waiting on model-backed compaction before retry.
    ///
    /// Automatic output-limit recovery keeps the turn running while an async
    /// compaction worker summarizes the pane transcript. Actor-owned idle
    /// cleanup uses these ids as valid progress paths until compaction reports
    /// completion or failure.
    pub(crate) fn agent_compaction_resume_turn_ids(&self) -> Vec<String> {
        self.agent_compaction_resume_ids()
    }

    /// Claims one queued compaction task for execution outside the actor.
    pub fn claim_agent_compaction_task(
        &mut self,
        pane_id: &str,
        task_generation: u64,
    ) -> Result<Option<RuntimeAgentCompactionDispatch>> {
        if self.agent_is_human_paused(pane_id) {
            return Ok(None);
        }
        if self.pending_agent_compaction_task_generation(pane_id) != Some(task_generation) {
            return Ok(None);
        }
        if self
            .agent_compaction_retry_delay(pane_id, task_generation)
            .is_some()
        {
            return Ok(None);
        }
        if self
            .retained_agent_compaction_task(pane_id, task_generation)
            .is_some_and(|task| task.request.is_some())
        {
            self.require_auxiliary_accounting_capacity()?;
        }
        let Some(task) = self.take_pending_agent_compaction_task(pane_id) else {
            return Ok(None);
        };
        let task_generation = task.task_generation;
        if !self.agent_compaction_task_is_current(pane_id, task_generation) {
            let _ = self.fail_agent_compaction_task(pane_id, task_generation);
            return Ok(None);
        }
        self.claim_agent_compaction_task_state(pane_id, task.clone());
        let dispatch = if task.request.is_none() {
            self.queue_replay_epoch_publication(
                &task,
                mez_agent::ReplayHeadPublication::IntentionalOmission,
            )
            .map(|()| None)
        } else {
            self.prepare_claimed_agent_compaction_dispatch(pane_id, task)
                .map(Some)
        };
        match dispatch {
            Ok(dispatch) => Ok(dispatch),
            Err(error) => {
                // Construction has not handed out a producer. Settle here,
                // before a host can lose the refusal reply or attempt a lease.
                self.reject_undispatched_agent_compaction_task(pane_id, task_generation, &error)
                    .map_err(|settlement| {
                        MezError::invalid_state(format!(
                            "compaction claim failed: {}; settlement failed: {}",
                            error.message(),
                            settlement.message()
                        ))
                    })?;
                Err(error)
            }
        }
    }

    /// Constructs request-local adapters while the service still owns the whole
    /// undispatched claim. An error cannot leave a future expense producer.
    fn prepare_claimed_agent_compaction_dispatch(
        &mut self,
        pane_id: &str,
        mut task: RuntimeAgentCompactionTask,
    ) -> Result<RuntimeAgentCompactionDispatch> {
        // Source/configuration/process authority must still hold before a new
        // credential lookup or model producer, including successor wave claims.
        self.validate_replay_compaction_task(&task)?;
        let provider_config = self
            .provider_registry()
            .provider(&task.model_profile.provider)
            .cloned()
            .ok_or_else(|| {
                MezError::config(format!(
                    "provider `{}` for active model profile is not configured",
                    task.model_profile.provider
                ))
            })?;
        let provider_api = match task.compaction_request_shape.as_ref() {
            Some((api, _, _)) => *api,
            None => resolve_provider_api(&provider_config.kind, provider_config.api.as_deref())?,
        };
        self.append_credential_access_audit(
            &task.model_profile.provider,
            &provider_config.auth_profile,
            "provider_compact",
            "requested",
        )?;
        let Some(auth_store) = self.integration.auth_store() else {
            self.append_credential_access_audit(
                &task.model_profile.provider,
                &provider_config.auth_profile,
                "provider_compact",
                "denied",
            )?;
            return Err(MezError::invalid_state(format!(
                "provider API `{}` compaction requires an attached auth store",
                provider_api.as_str()
            )));
        };
        let endpoint_override = provider_config
            .base_url
            .as_deref()
            .filter(|endpoint| !endpoint.is_empty());
        let provider_options = task
            .compaction_request_shape
            .as_ref()
            .map(|(_, options, _)| options.clone())
            .unwrap_or_else(|| {
                runtime_effective_provider_options(&provider_config, &task.model_profile)
            });
        let credential_source =
            AuthProfileCredentialSource::new(auth_store, &provider_config.auth_profile);
        let provider = match provider_api {
            ProviderApiCompatibility::OpenAiResponses => {
                openai_responses_provider_from_auth_store_with_provider_options(
                    &credential_source,
                    &task.model_profile.provider,
                    endpoint_override,
                    &provider_options,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                )
                .map(RuntimeAgentProviderDispatchProvider::OpenAi)
            }
            ProviderApiCompatibility::OpenAiChatCompletions => {
                openai_compatible_provider_from_auth_store_with_provider_options_and_brand(
                    &credential_source,
                    &task.model_profile.provider,
                    provider_config.kind == "openai",
                    endpoint_override,
                    &provider_options,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                )
                .map(RuntimeAgentProviderDispatchProvider::OpenAiCompatible)
            }
            ProviderApiCompatibility::DeepSeekChatCompletions => {
                deepseek_chat_completions_provider_from_auth_store_with_provider_options(
                    &credential_source,
                    &task.model_profile.provider,
                    endpoint_override,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                )
                .map(RuntimeAgentProviderDispatchProvider::DeepSeek)
            }
            ProviderApiCompatibility::AnthropicMessages => {
                anthropic_provider_from_auth_store_with_provider_options(
                    &credential_source,
                    &task.model_profile.provider,
                    endpoint_override,
                    &provider_options,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                )
                .map(RuntimeAgentProviderDispatchProvider::Anthropic)
            }
        }?;
        self.append_credential_access_audit(
            &task.model_profile.provider,
            &provider_config.auth_profile,
            "provider_compact",
            "granted",
        )?;
        let request = task.request.as_mut().ok_or_else(|| {
            MezError::invalid_state("raw omission cannot construct a provider dispatch")
        })?;
        request.max_input_tokens = task.model_profile.max_input_tokens();
        let stream = provider.request_stream(request);
        task.compaction_request_shape = Some((provider_api, provider_options.clone(), stream));
        self.claim_agent_compaction_task_state(pane_id, task.clone());
        let delivery = crate::runtime::RuntimeCompactionDispatchDelivery::new();
        self.track_compaction_dispatch_delivery(&task, delivery.observer())?;
        Ok(RuntimeAgentCompactionDispatch {
            delivery,
            task,
            provider,
            provider_options,
            stream,
        })
    }

    /// Settles an exact claim proven not to have left its admission boundary.
    /// Callers must own the dispatch or synchronously know it was never handed
    /// off. Expiry, Stop and live-worker cancellation must retain expense until
    /// physical retirement instead. No provider retry or publication is granted.
    pub(crate) fn reject_undispatched_agent_compaction_task(
        &mut self,
        pane_id: &str,
        task_generation: u64,
        cause: &MezError,
    ) -> Result<bool> {
        let Some(task) = self
            .claimed_agent_compaction_task(pane_id, task_generation)
            .cloned()
        else {
            return Ok(false);
        };
        let current = self.agent_compaction_task_is_current(pane_id, task_generation);
        // Clear exact logical and zero-expense ownership before fallible
        // terminal presentation or recovery of separately accepted steering.
        self.record_compaction_failure_usage(pane_id, task_generation, Default::default());
        self.fail_agent_compaction_task(pane_id, task_generation);
        if current {
            self.report_compaction_failure(&task, "compact was not dispatched", cause)?;
        }
        Ok(true)
    }

    /// Retires an exact claimed compaction whose worker result was not delivered.
    /// Never retries an ambiguous provider request or publishes its summary.
    pub(crate) fn expire_claimed_agent_compaction_task(
        &mut self,
        pane_id: &str,
        task_generation: u64,
    ) -> Result<bool> {
        if self.settle_abandoned_compaction_dispatch(pane_id, task_generation)? {
            return Ok(true);
        }
        let Some(task) = self
            .claimed_agent_compaction_task(pane_id, task_generation)
            .cloned()
        else {
            return Ok(false);
        };
        let current = self.agent_compaction_task_is_current(pane_id, task_generation);
        self.fail_agent_compaction_task(pane_id, task_generation);
        if current {
            let diagnostic = "compaction worker result was not delivered before its claim deadline; provider execution outcome is unknown";
            self.report_compaction_failure(
                &task,
                "compaction claim expired",
                &MezError::invalid_state(diagnostic),
            )?;
        }
        Ok(true)
    }

    /// Retires a pending backoff owner when its mandatory timer cannot be admitted.
    pub(crate) fn fail_agent_compaction_retry_admission(
        &mut self,
        pane_id: &str,
        task_generation: u64,
    ) -> Result<bool> {
        if self
            .agent_compaction_retry_delay(pane_id, task_generation)
            .is_none()
        {
            return Ok(false);
        }
        let Some(task) = self
            .retained_agent_compaction_task(pane_id, task_generation)
            .cloned()
        else {
            return Ok(false);
        };
        self.fail_agent_compaction_task(pane_id, task_generation);
        let diagnostic =
            "compaction transport retry timer admission failed; retry request was not dispatched";
        self.report_compaction_failure(
            &task,
            "compaction retry was not dispatched",
            &MezError::invalid_state(diagnostic),
        )?;
        Ok(true)
    }

    /// Applies one model-backed compaction result through the transport-neutral transition contract.
    pub(crate) fn apply_agent_compaction_transition(
        &mut self,
        event: AgentCompactionEvent,
    ) -> Result<RuntimeTransition> {
        let (pane_id, applied) = match event {
            AgentCompactionEvent::Completed {
                pane_id,
                task_generation,
                response,
            } => (
                pane_id.clone(),
                self.apply_agent_compaction_completed_event_for_generation(
                    &pane_id,
                    task_generation,
                    *response,
                )?,
            ),
            AgentCompactionEvent::Failed {
                pane_id,
                task_generation,
                usage,
                kind,
                message,
                provider_failure_json,
                ..
            } => {
                self.record_compaction_failure_usage(&pane_id, task_generation, usage);
                (
                    pane_id.clone(),
                    self.apply_agent_compaction_failed_event_for_generation(
                        &pane_id,
                        task_generation,
                        &kind,
                        &message,
                        provider_failure_json.as_deref(),
                    )?,
                )
            }
        };
        if applied && !self.agent_is_compacting(&pane_id) {
            self.resume_agent_compaction_steering(&pane_id)?;
        }
        let mut transition = self.runtime_pane_transition_with_render(
            &pane_id,
            applied,
            Some(RenderInvalidationReason::FullRedraw),
        );
        if applied
            && let Some(generation) = self.pending_agent_compaction_task_generation(&pane_id)
            && let Some(delay_ms) = self.agent_compaction_retry_delay(&pane_id, generation)
        {
            transition
                .side_effects
                .push(crate::runtime::RuntimeSideEffect::ScheduleTimer {
                    key: crate::runtime::RuntimeTimerKey::new(
                        crate::runtime::RuntimeTimerKind::CompactionRetry,
                        &pane_id,
                        generation,
                    ),
                    delay_ms,
                });
        }
        Ok(transition)
    }

    /// Applies a completed model-backed compaction response.
    #[cfg(test)]
    pub fn apply_agent_compaction_completed_event(
        &mut self,
        pane_id: &str,
        response: ModelResponse,
    ) -> Result<bool> {
        let Some(task_generation) = self.claimed_agent_compaction_task_generation(pane_id) else {
            return Ok(false);
        };
        self.apply_agent_compaction_completed_event_for_generation(
            pane_id,
            task_generation,
            response,
        )
    }

    /// Applies a completion only when its exact task generation still owns the pane.
    fn apply_agent_compaction_completed_event_for_generation(
        &mut self,
        pane_id: &str,
        task_generation: u64,
        response: ModelResponse,
    ) -> Result<bool> {
        if self
            .replay_epoch_publication(pane_id, task_generation)
            .is_some()
        {
            return Ok(false);
        }
        let current_conversation = self.agent_compaction_task_is_current(pane_id, task_generation);
        self.record_compaction_failure_usage(pane_id, task_generation, response.usage);
        let retained = self
            .claimed_agent_compaction_task(pane_id, task_generation)
            .is_some_and(|task| task.replay_source.is_some());
        let claimed = if retained {
            self.claimed_agent_compaction_task(pane_id, task_generation)
                .cloned()
        } else {
            self.finish_agent_compaction_task(pane_id, task_generation)
        };
        let Some(task) = claimed else {
            return Ok(false);
        };
        self.record_agent_provider_quota_usage_for_conversation(
            pane_id,
            &task.conversation_id,
            &response.quota_usage,
        );
        if !current_conversation {
            if retained {
                self.finish_agent_compaction_task(pane_id, task_generation);
            }
            return Ok(false);
        }
        let application = (|| -> Result<()> {
            // New inference has one source contract. A missing owner cannot
            // send an accepted result through historical publication code.
            self.validate_replay_compaction_task(&task)?;
            let source = task.replay_source.as_ref().ok_or_else(|| {
                MezError::invalid_state("compaction result lost its frozen source")
            })?;
            match source.model_wave_outcome(task_generation)? {
                replay_epoch::WaveOutcome::Continue => {
                    self.queue_agent_compaction_task(task.clone())
                }
                replay_epoch::WaveOutcome::Complete(accepted) => {
                    self.queue_accepted_replay_epoch_publication(&task, &accepted)?;
                }
            }
            Ok(())
        })();
        if retained
            && self
                .replay_epoch_publication(pane_id, task_generation)
                .is_none()
        {
            self.finish_agent_compaction_task(pane_id, task_generation);
        }
        if let Err(error) = application {
            self.report_compaction_failure(
                &task,
                "compact failed while applying completion",
                &error,
            )?;
        }
        Ok(true)
    }

    /// Invalidates execution-only presentation after an owned replacement and
    /// checkpoints it before ordinary recovery admission or success diagnostics.
    /// Callers must have accepted the exact generation/conversation replacement;
    /// validation and stale/failure paths must not call this owner. A checkpoint
    /// error leaves the accepted context's display unknown and propagates to the
    /// existing waiting-turn failure path. Deferred persistence retains its own
    /// generation-fenced retry. Neither cumulative expense nor consumed pressure
    /// fences are changed here.
    fn checkpoint_compaction_replacement_usage(&mut self, conversation_id: &str) -> Result<()> {
        self.restore_agent_latest_request_usage(conversation_id, None);
        self.restore_agent_context_usage(conversation_id, None, None);
        self.checkpoint_agent_session_metadata()?;
        Ok(())
    }

    /// Captures bounded observations from the original opaque operation before
    /// exact task retirement. This does not infer provider recovery authority.
    pub(in crate::runtime) fn compaction_failure_with_progress(
        &self,
        task: &RuntimeAgentCompactionTask,
        error: MezError,
    ) -> MezError {
        let progress = task
            .replay_source
            .as_ref()
            .map(|source| source.model_progress());
        error.with_compaction_failure(crate::error::CompactionFailureEvidence {
            phase: progress.map_or("unavailable", |progress| progress.phase),
            operation_epoch: task.compaction_epoch,
            dispatch_generation: task.task_generation,
            model_calls: progress.map(|progress| progress.model_calls),
            serial_depth: progress.map(|progress| progress.serial_depth),
            elapsed_seconds: crate::runtime::current_unix_seconds().saturating_sub(
                self.agent_compaction_operation_started_at(task)
                    .unwrap_or_else(crate::runtime::current_unix_seconds),
            ),
        })
    }

    /// Reports exact bounded terminal observations independently of surviving
    /// expense producers. The caller owns logical settlement. All terminal,
    /// presentation and eligible steering obligations are attempted, with the
    /// first error retained; diagnostics grant no work or publication authority.
    fn report_compaction_failure(
        &mut self,
        task: &RuntimeAgentCompactionTask,
        status: &'static str,
        cause: &MezError,
    ) -> Result<()> {
        let error = self.compaction_failure_with_progress(task, cause.clone());
        // Routed recovery may remove the child pane. Paint its bounded status
        // while that original presentation target still exists; recovery and
        // steering must still run independently if presentation fails.
        let presentation = self.append_compaction_failure_status(&task.pane_id, status, &error);
        let terminal = if let Some(turn) = task.resume_turn_id.as_deref() {
            self.fail_running_turn_after_compaction_error(turn, &task.source, &error)
        } else {
            Ok(())
        };
        let steering = self.resume_agent_compaction_steering(&task.pane_id);
        presentation.and(terminal).and(steering)?;
        Ok(())
    }

    /// Renders bounded auxiliary facts for task and pre-provider preparation
    /// failures alike, without promoting diagnostic counters into authority.
    fn append_compaction_failure_status(
        &mut self,
        pane: &str,
        status: &'static str,
        error: &MezError,
    ) -> Result<()> {
        let facts = error
            .compaction_failure()
            .map(|facts| format!("\n{}", facts.diagnostic_line()))
            .unwrap_or_default();
        self.append_agent_status_text_to_terminal_buffer(
            pane,
            &format!("agent: {status}: {}{facts}", error.message()),
        )?;
        Ok(())
    }

    /// Applies a failed model-backed compaction worker result.
    #[cfg(test)]
    pub fn apply_agent_compaction_failed_event(
        &mut self,
        pane_id: &str,
        kind: &str,
        message: &str,
        provider_failure_json: Option<&str>,
    ) -> Result<bool> {
        let Some(task_generation) = self.claimed_agent_compaction_task_generation(pane_id) else {
            return Ok(false);
        };
        self.apply_agent_compaction_failed_event_for_generation(
            pane_id,
            task_generation,
            kind,
            message,
            provider_failure_json,
        )
    }

    /// Applies a failure only when its exact task generation still owns the pane.
    fn apply_agent_compaction_failed_event_for_generation(
        &mut self,
        pane_id: &str,
        task_generation: u64,
        kind: &str,
        message: &str,
        provider_failure_json: Option<&str>,
    ) -> Result<bool> {
        // Accepted inference transferred this generation to persistence. Only
        // that publication owner can settle it; late provider results cannot
        // cancel a pending write or discard a returned but unadopted receipt.
        if self
            .replay_epoch_publication(pane_id, task_generation)
            .is_some()
        {
            return Ok(false);
        }
        let Some(mut task) = self
            .claimed_agent_compaction_task(pane_id, task_generation)
            .cloned()
        else {
            return Ok(false);
        };
        if !self.agent_compaction_task_is_current(pane_id, task_generation) {
            let _ = self.finish_agent_compaction_task(pane_id, task_generation);
            return Ok(false);
        }
        let Some(parsed_kind) = provider_event_error_kind(kind) else {
            let diagnostic = format!(
                "provider event kind unknown: `{}`: {message}",
                bounded_provider_event_kind(kind)
            );
            self.fail_agent_compaction_task(pane_id, task_generation);
            self.report_compaction_failure(
                &task,
                "compact failed during provider request",
                &MezError::invalid_state(diagnostic),
            )?;
            return Ok(true);
        };
        // Validate against the exact current claim before retiring it. A
        // transport retry retains this graph and receives its own generation.
        if let Err(error) = self.validate_replay_compaction_task(&task) {
            self.finish_agent_compaction_task(pane_id, task_generation);
            self.report_compaction_failure(
                &task,
                "compact failed while validating source",
                &error,
            )?;
            return Ok(true);
        }
        // The exact claim was cloned above, and no mutation/await can replace
        // it before this retirement. Keep its immutable source for reporting or
        // the existing private-graph transport retry, never a new selection.
        self.finish_agent_compaction_task(pane_id, task_generation);
        let retry_class =
            provider_error_retry_class_from_parts(parsed_kind, message, provider_failure_json);
        let policy = self.provider_retry_policy();
        if retry_class == ProviderErrorRetryClass::RetryableTransport
            && policy.should_retry(task.transport_retry_attempts, retry_class)
            && task
                .replay_source
                .as_ref()
                .is_some_and(|source| source.model_work_can_retry(task_generation))
        {
            task.transport_retry_attempts = task.transport_retry_attempts.saturating_add(1);
            task.transport_retry_delay_ms = Some(
                policy
                    .delay_ms(
                        task.transport_retry_attempts,
                        mez_agent::provider_retry_after_delay_ms(
                            provider_failure_json,
                            crate::runtime::current_unix_millis(),
                        ),
                        Some(rand::random()),
                    )
                    .max(1),
            );
            // Preserve the entire frozen selection and staged position. Only
            // the dispatch generation changes; failed text is never applied.
            self.queue_agent_compaction_task(task);
            return Ok(true);
        }
        self.report_compaction_failure(
            &task,
            "compact failed during provider request",
            &MezError::new(parsed_kind, message),
        )?;
        Ok(true)
    }

    /// Preserves typed auxiliary provenance while adding automatic-recovery
    /// context. This wrapper must not manufacture ordinary MAAP recovery facts.
    fn fail_running_turn_after_compaction_error(
        &mut self,
        turn_id: &str,
        source: &str,
        cause: &MezError,
    ) -> Result<()> {
        let Some(turn) = self
            .agent_turn_ledger()
            .turns()
            .iter()
            .find(|turn| turn.turn_id == turn_id)
            .cloned()
        else {
            return Ok(());
        };
        if turn.state != AgentTurnState::Running {
            return Ok(());
        }
        let Some(model_profile) = self.agent_turn_model_profile(turn_id).cloned() else {
            return Ok(());
        };
        let error = MezError::invalid_state(format!(
            "automatic {} compaction failed before provider retry: {}",
            runtime_compaction_failure_source_label(source),
            cause.message(),
        ));
        let error = if let Some(facts) = cause.compaction_failure() {
            error.with_compaction_failure(facts.clone())
        } else {
            error
        };
        if let Err(application_error) = self.fail_agent_turn_for_provider_error(
            &turn,
            &model_profile.provider,
            &model_profile,
            &error,
        ) {
            self.fail_agent_turn_after_provider_completion_application_error(
                &turn,
                &model_profile.provider,
                Some(&model_profile),
                &application_error,
            );
        }
        if let Some(parent_turn_id) = self.routed_parent_turn_id_for_child(&turn.turn_id)
            && self.routed_workflow_waits_for_worker_result(&parent_turn_id)
        {
            self.handle_routed_child_missing_execution(&turn, AgentTurnState::Failed)?;
        }
        Ok(())
    }
}

/// Wraps already rendered source as compactor data. This does not reconstruct
/// task lifecycle, native ownership, ordinary turn admission or publication.
fn runtime_model_compaction_request_from_source(
    profile: &ModelProfile,
    pane_id: &str,
    conversation_id: &str,
    source_text: String,
    allowed_actions: AllowedActionSet,
) -> ModelRequest {
    mez_agent::epoch_compaction::provider::request_from_source(
        profile,
        pane_id,
        conversation_id,
        source_text,
        allowed_actions,
    )
}

/// Returns the content-free label for one recorded compaction trigger.
fn runtime_compaction_failure_source_label(source: &str) -> &'static str {
    match source {
        "provider-output-limit" => "output-limit",
        "provider-context-limit" => "provider-context-limit",
        "observed-input-limit" => "observed-input-limit",
        _ => "conversation",
    }
}

/// Derives a conservative input allowance from configured provider limits.
///
/// Explicit input caps already account for provider output reservations. When
/// only a context window is configured, reserve the profile's output cap first.
/// Keep five percent headroom for deterministic-estimator and provider tokenizer
/// drift. `Some(0)` deliberately means that no request can safely fit.
#[cfg(test)]
pub(super) fn runtime_compaction_safe_input_limit(
    max_input_tokens: Option<usize>,
    context_window_tokens: Option<usize>,
    max_output_tokens: Option<usize>,
) -> Option<usize> {
    let context_input_limit = context_window_tokens
        .map(|window| window.saturating_sub(max_output_tokens.unwrap_or_default()));
    let configured_limit = match (max_input_tokens, context_input_limit) {
        (Some(input), Some(context)) => Some(input.min(context)),
        (Some(input), None) => Some(input),
        (None, Some(context)) => Some(context),
        (None, None) => None,
    }?;
    let headroom = configured_limit / 20 + usize::from(configured_limit % 20 != 0);
    Some(configured_limit.saturating_sub(headroom))
}

#[cfg(test)]
mod size_diagnostic_tests {
    use super::*;

    /// Irreducible diagnostics report only estimated component sizes, never
    /// the protected instruction or recoverable source supplied by the user.
    #[test]
    fn exhausted_candidate_diagnostic_is_content_free() {
        let context = AgentContext::new_durable(vec![
            ContextBlock::user_event("user prompt", "SECRET_DIRECT_USER_INSTRUCTION"),
            ContextBlock::assistant_event("assistant", "SECRET_RECOVERABLE_HISTORY"),
        ])
        .unwrap();
        let diagnostic = runtime_compaction_candidate_size_diagnostic(&context, None, 500, 100);
        assert!(diagnostic.contains("protected_block_estimate="));
        assert!(diagnostic.contains("optional_block_estimate="));
        assert!(diagnostic.contains("non_block_or_accounting_residual="));
        assert!(!diagnostic.contains("SECRET_"));
    }
}
