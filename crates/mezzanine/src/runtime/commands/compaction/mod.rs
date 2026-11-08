//! Agent-shell conversation compaction commands and provider request helpers.
//!
//! This module owns `/compact`, queued model-backed compaction tasks,
//! compaction provider request construction, private summary staging,
//! and canonical replay publication. Keeping compaction isolated
//! avoids mixing long-running summarization workflow with ordinary shell
//! command dispatch. Manual model input keeps physical source proofs distinct
//! from summary replacements, and head publication compares the frozen epoch
//! and rows under the mutation lease before changing durable replay authority.
//! The candidate component reconstructs prospective history and measures the
//! complete request; source/preparation owners retain exact publication fences.

use super::{
    AgentContext, AgentShellCommandOutcome, AgentTurnRecord, AgentTurnState, AllowedActionSet,
    ContextBlock, ContextSourceKind, DEFAULT_PROVIDER_TIMEOUT_MS, MezError, ModelInteractionKind,
    ModelMessage, ModelMessageRole, ModelProfile, ModelRequest, ModelResponse,
    ProviderApiCompatibility, ReqwestProviderHttpTransport, Result, RuntimeAgentCompactionDispatch,
    RuntimeAgentCompactionTask, RuntimeAgentProviderDispatchProvider, RuntimeSessionService,
    TranscriptEntry, TranscriptRole, current_unix_seconds,
    deepseek_chat_completions_provider_from_auth_store_with_provider_options, json_escape,
    model_context_text_word_count,
    openai_compatible_provider_from_auth_store_with_provider_options_and_brand,
    openai_responses_provider_from_auth_store_with_provider_options, parse_slash_command,
    resolve_provider_api,
};
use crate::integrations::agent::context::assemble_model_request;
use crate::integrations::agent::provider::{
    anthropic_provider_from_auth_store_with_provider_options, bounded_provider_event_kind,
    provider_error_retry_class_from_parts, provider_event_error_kind,
};
use crate::runtime::agent_state::RuntimeAgentCompactionTarget;
use crate::runtime::agent_state::{
    RuntimeActiveTurnCompactionTrigger, RuntimeConversationCompactionChunks,
};
use crate::runtime::config::runtime_effective_provider_options;
use crate::runtime::{AgentCompactionEvent, RenderInvalidationReason, RuntimeTransition};
use crate::security::auth::AuthProfileCredentialSource;
use mez_agent::ProviderErrorRetryClass;

mod candidate;
mod handoff;
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
mod summary_correction;
#[cfg(test)]
mod tests;
use no_work::NoWorkReason;
pub(crate) use preparation::{
    RuntimeManualCompactionPreparation, RuntimeManualCompactionRequestWork,
};

/// Content-free component estimates for a failed complete provider request.
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
        plan: mez_agent::ModelContextCompactionPlan,
    ) -> Result<bool> {
        self.queue_agent_active_turn_compaction(
            turn_id,
            model_profile_name,
            model_profile,
            RuntimeActiveTurnCompactionTrigger::ProviderContextLimit {
                attempt: recovery_attempt,
            },
            plan,
        )
    }

    /// Queues one typed active-turn compaction while preserving its trigger.
    pub(crate) fn queue_agent_active_turn_compaction(
        &mut self,
        turn_id: &str,
        model_profile_name: String,
        model_profile: ModelProfile,
        trigger: RuntimeActiveTurnCompactionTrigger,
        plan: mez_agent::ModelContextCompactionPlan,
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
        let replay_source =
            match self.prepare_active_replay_compaction(&turn, &model_profile, trigger)? {
                replay_epoch::Preparation::NoWork(_) => return Ok(false),
                replay_epoch::Preparation::Ready(source) => source,
            };
        let selected = replay_source.input_blocks();
        let mut current_blocks = runtime_redact_compaction_blocks(&selected);
        let mut pending_blocks = Vec::new();
        let (rejected_request_bytes, rejected_request_stream) = match trigger {
            RuntimeActiveTurnCompactionTrigger::ProviderContextLimit { .. } => (
                self.claimed_agent_provider_openai_request_bytes(turn_id),
                self.claimed_agent_provider_openai_request_stream(turn_id),
            ),
            RuntimeActiveTurnCompactionTrigger::ObservedInputLimit { .. } => (None, None),
        };
        let mut request = runtime_model_compaction_request_for_blocks(
            &model_profile,
            &turn.pane_id,
            &conversation_id,
            &current_blocks,
            allowed_actions.clone(),
        )?;
        // A legacy no-work projection has no summary allowance. Independently
        // selected canonical work must not inherit its zero-output guidance.
        if plan.summary_budget_words() > 0 {
            runtime_limit_compaction_summary_output(&mut request, plan.summary_budget_words());
            summary_correction::communicate_summary_target(
                &mut request,
                plan.summary_budget_words(),
                plan.planning_budget().1,
            );
        }
        if let (Some(rejected_bytes), Some(stream)) =
            (rejected_request_bytes, rejected_request_stream)
        {
            while runtime_openai_compaction_request_bytes(&request, Some(stream))?
                .is_some_and(|request_bytes| request_bytes >= rejected_bytes)
            {
                let Some((first, second)) = runtime_split_compaction_blocks(&current_blocks) else {
                    return Err(MezError::invalid_state(format!(
                        "context-limit recovery could not form a smaller compactor request: rejected_request_bytes={rejected_bytes}"
                    )));
                };
                pending_blocks.push(second);
                current_blocks = first;
                request = runtime_model_compaction_request_for_blocks(
                    &model_profile,
                    &turn.pane_id,
                    &conversation_id,
                    &current_blocks,
                    allowed_actions.clone(),
                )?;
                if plan.summary_budget_words() > 0 {
                    runtime_limit_compaction_summary_output(
                        &mut request,
                        plan.summary_budget_words(),
                    );
                    summary_correction::communicate_summary_target(
                        &mut request,
                        plan.summary_budget_words(),
                        plan.planning_budget().1,
                    );
                }
            }
        }
        self.refresh_project_trust_store_from_disk_if_changed()?;
        self.queue_agent_compaction_task(RuntimeAgentCompactionTask {
            source_owner: Some(replay_source.original.clone()),
            replay_source: Some(replay_source),
            task_generation: 0,
            compaction_epoch: 0,
            transport_retry_attempts: 0,
            transport_retry_delay_ms: None,
            summary_correction: Default::default(),
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
            summarized_entries: selected.len(),
            model_profile_name,
            model_profile,
            request,
            preserve_summary_output_budget: true,
            manual_retry_source: None,
            manual_final_retry: None,
            resume_turn_id: Some(turn.turn_id.clone()),
            target: RuntimeAgentCompactionTarget::ActiveTurn {
                turn_id: turn.turn_id.clone(),
                #[cfg(test)]
                trigger,
                final_request_retry: Box::default(),
                compaction_backoff_attempt: 0,
                rejected_request_bytes,
                rejected_request_stream,
                current_blocks,
                pending_blocks,
                completed_summaries: Vec::new(),
                synthesis_source_bytes: None,
                completed_responses: 0,
                plan: Box::new(plan),
            },
            conversation_chunks: None,
            compaction_request_shape: None,
        });
        self.remove_pending_agent_provider_task(turn_id);
        let status = match trigger {
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
        };
        self.append_agent_status_text_to_terminal_buffer(&turn.pane_id, &status)?;
        Ok(true)
    }

    /// Returns pane ids with queued model-backed compaction tasks.
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
        self.require_auxiliary_accounting_capacity()?;
        let Some(mut task) = self.take_pending_agent_compaction_task(pane_id) else {
            return Ok(None);
        };
        let task_generation = task.task_generation;
        if !self.agent_compaction_task_is_current(pane_id, task_generation) {
            let _ = self.fail_agent_compaction_task(pane_id, task_generation);
            return Ok(None);
        }
        self.claim_agent_compaction_task_state(pane_id, task.clone());
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
        if let Some(max_input_tokens) = task.model_profile.max_input_tokens() {
            loop {
                let estimate = mez_agent::provider_request_input_estimate(
                    &task.request,
                    provider_api,
                    &provider_options,
                    provider.request_stream(&task.request),
                )?;
                if !estimate.exceeds_explicit_cap(max_input_tokens) {
                    break;
                }
                if matches!(task.target, RuntimeAgentCompactionTarget::Conversation) {
                    runtime_prepare_conversation_compaction_chunks(&mut task)?;
                    let current = task
                        .conversation_chunks
                        .as_ref()
                        .ok_or_else(|| {
                            MezError::invalid_state(
                                "conversation compactor temporary source is unavailable",
                            )
                        })?
                        .current
                        .clone();
                    let (first, second) = runtime_split_conversation_compaction_source(&current)
                        .ok_or_else(|| MezError::invalid_state(format!(
                            "conversation compactor request cannot fit configured input cap: estimated_input_tokens={} max_input_tokens={max_input_tokens}",
                            estimate.input_tokens
                        )))?;
                    let chunks = task.conversation_chunks.as_mut().ok_or_else(|| {
                        MezError::invalid_state(
                            "conversation compactor temporary source is unavailable",
                        )
                    })?;
                    chunks.pending.push(second);
                    runtime_rebuild_conversation_compaction_request(&mut task, first)?;
                    continue;
                }
                let RuntimeAgentCompactionTarget::ActiveTurn {
                    current_blocks,
                    pending_blocks,
                    ..
                } = &mut task.target
                else {
                    return Err(MezError::invalid_state(format!(
                        "conversation compactor request exceeds configured input cap: estimated_input_tokens={} max_input_tokens={max_input_tokens}",
                        estimate.input_tokens
                    )));
                };
                let Some((first, second)) = runtime_split_compaction_blocks(current_blocks) else {
                    return Err(MezError::invalid_state(format!(
                        "active-turn compactor request cannot be split below configured input cap: estimated_input_tokens={} max_input_tokens={max_input_tokens}",
                        estimate.input_tokens
                    )));
                };
                pending_blocks.push(second);
                runtime_rebuild_active_turn_compaction_request(&mut task, first)?;
            }
        }
        self.append_credential_access_audit(
            &task.model_profile.provider,
            &provider_config.auth_profile,
            "provider_compact",
            "granted",
        )?;
        task.request.max_input_tokens = task.model_profile.max_input_tokens();
        let stream = provider.request_stream(&task.request);
        task.compaction_request_shape = Some((provider_api, provider_options.clone(), stream));
        self.claim_agent_compaction_task_state(pane_id, task.clone());
        Ok(Some(RuntimeAgentCompactionDispatch {
            task,
            provider,
            provider_options,
            stream,
        }))
    }

    /// Retires an exact claimed compaction whose worker result was not delivered.
    /// Never retries an ambiguous provider request or publishes its summary.
    pub(crate) fn expire_claimed_agent_compaction_task(
        &mut self,
        pane_id: &str,
        task_generation: u64,
    ) -> Result<bool> {
        let claimed = self.agent_compaction_task_is_claimed(pane_id, task_generation);
        if !claimed {
            return Ok(false);
        }
        let current = self.agent_compaction_task_is_current(pane_id, task_generation);
        let mut failed = self.fail_agent_compaction_task(pane_id, task_generation);
        if current {
            let diagnostic = "compaction worker result was not delivered before its claim deadline; provider execution outcome is unknown";
            if let Some((turn_id, source)) = failed.take_resume_turn_and_source() {
                self.fail_running_turn_after_compaction_failure(&turn_id, &source, diagnostic)?;
            }
            let _ = self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &format!("agent: {diagnostic}"),
            );
            self.resume_agent_compaction_steering(pane_id)?;
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
        let mut failed = self.fail_agent_compaction_task(pane_id, task_generation);
        let diagnostic =
            "compaction transport retry timer admission failed; retry request was not dispatched";
        if let Some((turn_id, source)) = failed.take_resume_turn_and_source() {
            self.fail_running_turn_after_compaction_failure(&turn_id, &source, diagnostic)?;
        }
        self.append_agent_status_text_to_terminal_buffer(pane_id, &format!("agent: {diagnostic}"))?;
        self.resume_agent_compaction_steering(pane_id)?;
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
        let Some(mut task) = claimed else {
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
            let summary = self.validate_or_correct_compaction_summary(&mut task, &response)?;
            let Some(summary) = summary else {
                return Ok(());
            };
            if let Some(chunks) = task.conversation_chunks.as_mut() {
                chunks.completed = chunks.completed.checked_add(1).ok_or_else(|| {
                    MezError::invalid_state("conversation compactor response count overflow")
                })?;
                chunks.summaries.push(summary.clone());
                if let Some(next) = chunks.pending.pop() {
                    task.summary_correction.accepted_summary = None;
                    runtime_rebuild_conversation_compaction_request(&mut task, next)?;
                    self.queue_agent_compaction_task(task.clone());
                    return Ok(());
                }
                if chunks.summaries.len() > 1 {
                    let synthesis = chunks
                        .summaries
                        .iter()
                        .enumerate()
                        .map(|(index, content)| {
                            format!("Chunk {} summary:\n{}", index + 1, content)
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    runtime_require_smaller_synthesis(
                        chunks.synthesis_source_bytes,
                        synthesis.len(),
                    )?;
                    chunks.synthesis_source_bytes = Some(synthesis.len());
                    chunks.summaries.clear();
                    task.summary_correction.accepted_summary = None;
                    runtime_rebuild_conversation_compaction_request(&mut task, synthesis)?;
                    self.queue_agent_compaction_task(task.clone());
                    return Ok(());
                }
            }
            if matches!(task.target, RuntimeAgentCompactionTarget::Conversation) {
                if !self.accept_manual_replay_compaction_candidate(&mut task, &summary)? {
                    return Ok(());
                }
                self.queue_replay_epoch_publication(&task, summary)?;
                return Ok(());
            }
            if matches!(task.target, RuntimeAgentCompactionTarget::ActiveTurn { .. }) {
                let (next_blocks, final_summary) = {
                    let RuntimeAgentCompactionTarget::ActiveTurn {
                        pending_blocks,
                        completed_summaries,
                        synthesis_source_bytes,
                        completed_responses,
                        ..
                    } = &mut task.target
                    else {
                        unreachable!("active-turn target was matched above");
                    };
                    *completed_responses = completed_responses.checked_add(1).ok_or_else(|| {
                        MezError::invalid_state("active-turn compactor response count overflow")
                    })?;
                    completed_summaries.push(summary);
                    if let Some(blocks) = pending_blocks.pop() {
                        if blocks.is_empty() {
                            return Err(MezError::invalid_state(
                                "active-turn compactor pending source did not advance",
                            ));
                        }
                        (Some(blocks), None)
                    } else if completed_summaries.len() == 1 {
                        (None, completed_summaries.pop())
                    } else {
                        let summaries = std::mem::take(completed_summaries);
                        let blocks = runtime_compaction_summary_blocks(summaries);
                        let bytes = blocks.iter().map(|block| block.content.len()).sum();
                        runtime_require_smaller_synthesis(*synthesis_source_bytes, bytes)?;
                        *synthesis_source_bytes = Some(bytes);
                        (Some(blocks), None)
                    }
                };
                if let Some(blocks) = next_blocks {
                    task.summary_correction.accepted_summary = None;
                    runtime_rebuild_active_turn_compaction_request(&mut task, blocks)?;
                    self.append_agent_trace_turn_event(
                        pane_id,
                        task.resume_turn_id.as_deref().unwrap_or("unknown"),
                        "context_limit_recovery compactor_continuing recursive_summary=true",
                    )?;
                    self.queue_agent_compaction_task(task.clone());
                    return Ok(());
                }
                let final_summary = final_summary.ok_or_else(|| {
                    MezError::invalid_state(
                        "recursive context compaction completed without a final summary",
                    )
                })?;
                if !self.accept_replay_compaction_candidate(&mut task, &final_summary)? {
                    return Ok(());
                }
                self.queue_replay_epoch_publication(&task, final_summary)?;
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
            let error = if error.compaction_failure().is_some() {
                error
            } else {
                error.with_compaction_failure(summary_correction::compaction_failure_evidence(
                    &task,
                    None,
                    crate::error::CompactionFailurePhase::CompleteCandidate,
                    crate::error::CompactionFailureDisposition::CandidateRejected,
                ))
            };
            if let Some(facts) = error.compaction_failure() {
                self.append_agent_status_text_to_terminal_buffer(
                    pane_id,
                    &facts.diagnostic_line(),
                )?;
            }
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &format!(
                    "agent: compact failed while applying completion: {}",
                    error.message()
                ),
            )?;
            if let Some(resume_turn_id) = task.resume_turn_id.as_deref() {
                self.fail_running_turn_after_compaction_error(
                    resume_turn_id,
                    &task.source,
                    &error,
                )?;
            }
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
        if !self.agent_compaction_task_is_claimed(pane_id, task_generation) {
            return Ok(false);
        }
        if !self.agent_compaction_task_is_current(pane_id, task_generation) {
            let _ = self.finish_agent_compaction_task(pane_id, task_generation);
            return Ok(false);
        }
        let Some(parsed_kind) = provider_event_error_kind(kind) else {
            let diagnostic = format!(
                "provider event kind unknown: `{}`: {message}",
                bounded_provider_event_kind(kind)
            );
            let mut failed = self.fail_agent_compaction_task(pane_id, task_generation);
            if failed.had_task() {
                self.append_agent_status_text_to_terminal_buffer(
                    pane_id,
                    &format!("agent: compact failed during provider request: {diagnostic}"),
                )?;
            }
            if let Some((resume_turn_id, source)) = failed.take_resume_turn_and_source() {
                self.fail_running_turn_after_compaction_failure(
                    &resume_turn_id,
                    &source,
                    &diagnostic,
                )?;
            }
            return Ok(failed.had_task());
        };
        let malformed = provider_failure_json
            .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
            .is_some_and(|payload| {
                payload.get("type").and_then(serde_json::Value::as_str)
                    == Some("malformed_model_output")
            });
        // Canonical source validation requires the original current claim.
        // Retire it only after correction admission; a replacement generation
        // queued by that admission must retain its independent ownership.
        if let Some(task) = self
            .claimed_agent_compaction_task(pane_id, task_generation)
            .cloned()
            && let Err(error) = self.validate_replay_compaction_task(&task)
        {
            self.finish_agent_compaction_task(pane_id, task_generation);
            if let Some(turn) = task.resume_turn_id.as_deref() {
                self.fail_running_turn_after_compaction_error(turn, &task.source, &error)?;
            }
            return Ok(true);
        }
        let task = if malformed {
            self.claimed_agent_compaction_task(pane_id, task_generation)
                .cloned()
        } else {
            self.finish_agent_compaction_task(pane_id, task_generation)
        };
        if let Some(mut task) = task {
            if malformed {
                let correction = self.correct_malformed_compaction_response(&mut task);
                self.finish_agent_compaction_task(pane_id, task_generation);
                if let Err(error) = correction {
                    if let Some(facts) = error.compaction_failure() {
                        self.append_agent_status_text_to_terminal_buffer(
                            pane_id,
                            &facts.diagnostic_line(),
                        )?;
                    }
                    self.append_agent_status_text_to_terminal_buffer(
                        pane_id,
                        &format!(
                            "agent: compact failed while correcting completed output: {}",
                            error.message()
                        ),
                    )?;
                    if let Some(turn) = task.resume_turn_id.as_deref() {
                        self.fail_running_turn_after_compaction_error(turn, &task.source, &error)?;
                    }
                }
                return Ok(true);
            }
            let retry_class =
                provider_error_retry_class_from_parts(parsed_kind, message, provider_failure_json);
            let policy = self.provider_retry_policy();
            if retry_class == ProviderErrorRetryClass::RetryableTransport
                && policy.should_retry(task.transport_retry_attempts, retry_class)
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
            if retry_class == ProviderErrorRetryClass::ContextLimit
                && matches!(task.target, RuntimeAgentCompactionTarget::Conversation)
            {
                runtime_prepare_conversation_compaction_chunks(&mut task)?;
                let (api, options, stream) = task
                    .compaction_request_shape
                    .as_ref()
                    .map(|(api, options, stream)| (*api, options.clone(), *stream))
                    .unwrap_or((
                        ProviderApiCompatibility::OpenAiResponses,
                        Default::default(),
                        false,
                    ));
                let failed_bytes = mez_agent::provider_request_input_estimate(
                    &task.request,
                    api,
                    &options,
                    stream,
                )?
                .wire_bytes;
                let chunks = task.conversation_chunks.as_mut().ok_or_else(|| {
                    MezError::invalid_state(
                        "conversation compactor temporary source is unavailable",
                    )
                })?;
                {
                    chunks.failures = chunks.failures.checked_add(1).ok_or_else(|| {
                        MezError::invalid_state("conversation compactor backoff count overflow")
                    })?;
                    let mut source = chunks.current.clone();
                    let mut siblings = Vec::new();
                    while let Some((first, second)) =
                        runtime_split_conversation_compaction_source(&source)
                    {
                        siblings.push(second);
                        runtime_rebuild_conversation_compaction_request(&mut task, first.clone())?;
                        let candidate_bytes = mez_agent::provider_request_input_estimate(
                            &task.request,
                            api,
                            &options,
                            stream,
                        )?
                        .wire_bytes;
                        if candidate_bytes < failed_bytes {
                            let chunks = task.conversation_chunks.as_mut().ok_or_else(|| {
                                MezError::invalid_state(
                                    "conversation compactor temporary source is unavailable",
                                )
                            })?;
                            chunks.pending.extend(siblings);
                            self.queue_agent_compaction_task(task);
                            return Ok(true);
                        }
                        source = first;
                    }
                }
            }
            if retry_class == ProviderErrorRetryClass::ContextLimit
                && matches!(task.target, RuntimeAgentCompactionTarget::ActiveTurn { .. })
            {
                // Keep the frozen canonical selection; only temporary input
                // may shrink after a provider rejection.
                let failed_request_bytes = {
                    let (api, options, stream) =
                        task.compaction_request_shape.as_ref().ok_or_else(|| {
                            MezError::invalid_state("compactor request wire shape is unavailable")
                        })?;
                    mez_agent::provider_request_input_estimate(
                        &task.request,
                        *api,
                        options,
                        *stream,
                    )?
                    .wire_bytes
                };
                let mut blocks = {
                    let RuntimeAgentCompactionTarget::ActiveTurn {
                        compaction_backoff_attempt,
                        current_blocks,
                        ..
                    } = &mut task.target
                    else {
                        unreachable!("active-turn target was matched above");
                    };
                    *compaction_backoff_attempt = compaction_backoff_attempt.saturating_add(1);
                    current_blocks.clone()
                };
                let mut queued_siblings = Vec::new();
                let mut retry_ready = false;
                while let Some((first, second)) = runtime_split_compaction_blocks(&blocks) {
                    queued_siblings.push(second);
                    runtime_rebuild_active_turn_compaction_request(&mut task, first.clone())?;
                    let (api, options, stream) =
                        task.compaction_request_shape.as_ref().ok_or_else(|| {
                            MezError::invalid_state("compactor request wire shape is unavailable")
                        })?;
                    let candidate_bytes = mez_agent::provider_request_input_estimate(
                        &task.request,
                        *api,
                        options,
                        *stream,
                    )?
                    .wire_bytes;
                    if candidate_bytes < failed_request_bytes {
                        retry_ready = true;
                        break;
                    }
                    blocks = first;
                }
                if retry_ready {
                    let RuntimeAgentCompactionTarget::ActiveTurn { pending_blocks, .. } =
                        &mut task.target
                    else {
                        unreachable!("active-turn target was matched above");
                    };
                    pending_blocks.extend(queued_siblings);
                    let attempt = match &task.target {
                        RuntimeAgentCompactionTarget::ActiveTurn {
                            compaction_backoff_attempt,
                            ..
                        } => *compaction_backoff_attempt,
                        RuntimeAgentCompactionTarget::Conversation => 0,
                    };
                    self.append_agent_status_text_to_terminal_buffer(
                    pane_id,
                    &format!(
                            "agent: compaction request exceeded provider context; retrying with smaller recursive input attempt={attempt}"
                    ),
                )?;
                    self.queue_agent_compaction_task(task);
                    return Ok(true);
                }
            }
            let disposition = match retry_class {
                ProviderErrorRetryClass::RetryableTransport => {
                    crate::error::CompactionFailureDisposition::TransportExhausted
                }
                ProviderErrorRetryClass::OutputLimit => {
                    crate::error::CompactionFailureDisposition::OutputLimit
                }
                ProviderErrorRetryClass::ContextLimit => {
                    crate::error::CompactionFailureDisposition::ContextFloor
                }
                _ => crate::error::CompactionFailureDisposition::ProviderRejected,
            };
            let facts = summary_correction::compaction_failure_evidence(
                &task,
                None,
                crate::error::CompactionFailurePhase::ProviderInference,
                disposition,
            );
            self.append_agent_status_text_to_terminal_buffer(pane_id, &facts.diagnostic_line())?;
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &format!("agent: compact failed during provider request: {message}"),
            )?;
            if let Some(resume_turn_id) = task.resume_turn_id.as_deref() {
                self.fail_running_turn_after_compaction_error(
                    resume_turn_id,
                    &task.source,
                    &MezError::invalid_state(message).with_compaction_failure(facts),
                )?;
            }
            return Ok(true);
        }
        let mut failed = self.fail_agent_compaction_task(pane_id, task_generation);
        if failed.had_task() {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &format!("agent: compact failed during provider request: {message}"),
            )?;
        }
        if let Some((resume_turn_id, source)) = failed.take_resume_turn_and_source() {
            self.fail_running_turn_after_compaction_failure(&resume_turn_id, &source, message)?;
        }
        Ok(failed.had_task())
    }

    /// Fails a turn whose automatic recovery compaction could not finish.
    fn fail_running_turn_after_compaction_failure(
        &mut self,
        turn_id: &str,
        source: &str,
        message: &str,
    ) -> Result<()> {
        self.fail_running_turn_after_compaction_error(
            turn_id,
            source,
            &MezError::invalid_state(message),
        )
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

/// Returns the complete serialized OpenAI Responses size for one compactor request.
fn runtime_openai_compaction_request_bytes(
    request: &ModelRequest,
    stream: Option<bool>,
) -> Result<Option<usize>> {
    stream
        .map(|stream| {
            mez_agent::openai_responses_request_body_with_stream(request, stream)
                .map(|body| body.len())
                .map_err(MezError::from)
        })
        .transpose()
}

/// Derives a conservative input allowance from configured provider limits.
///
/// Explicit input caps already account for provider output reservations. When
/// only a context window is configured, reserve the profile's output cap first.
/// Keep five percent headroom for deterministic-estimator and provider tokenizer
/// drift. `Some(0)` deliberately means that no request can safely fit.
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

/// Replaces only the temporary manual-compactor source, keeping its system and
/// developer instructions and the frozen durable selection unchanged.
fn runtime_rebuild_conversation_compaction_request(
    task: &mut RuntimeAgentCompactionTask,
    source: String,
) -> Result<()> {
    let mut messages = task.request.messages.iter().cloned().collect::<Vec<_>>();
    let last = messages.last_mut().ok_or_else(|| {
        MezError::invalid_state("conversation compactor request has no source message")
    })?;
    last.content = source.clone();
    task.request.messages = messages.into();
    let chunks = task.conversation_chunks.as_mut().ok_or_else(|| {
        MezError::invalid_state("conversation compactor temporary source is unavailable")
    })?;
    chunks.current = source;
    Ok(())
}

/// Splits a redacted temporary source at a UTF-8 boundary; no durable rows are
/// moved into the retained tail, and an irreducible envelope fails visibly.
fn runtime_split_conversation_compaction_source(source: &str) -> Option<(String, String)> {
    let midpoint = source.len() / 2;
    let split_at = source
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| *index >= midpoint)?;
    (split_at > 0 && split_at < source.len()).then(|| {
        (
            source[..split_at].to_string(),
            source[split_at..].to_string(),
        )
    })
}

/// Admit the first synthesis round, then require every later round to shrink
/// its preceding synthesis source; an empty candidate never makes progress.
fn runtime_require_smaller_synthesis(previous: Option<usize>, candidate: usize) -> Result<()> {
    if candidate == 0 || previous.is_some_and(|bytes| candidate >= bytes) {
        return Err(MezError::invalid_state(
            "conversation compactor synthesis did not make progress",
        ));
    }
    Ok(())
}

/// Starts bounded recursive manual work only after freezing and redacting the
/// original source in the already prepared compactor request.
fn runtime_prepare_conversation_compaction_chunks(
    task: &mut RuntimeAgentCompactionTask,
) -> Result<()> {
    if task.conversation_chunks.is_none() {
        let current = task
            .request
            .messages
            .last()
            .ok_or_else(|| {
                MezError::invalid_state("conversation compactor request has no source message")
            })?
            .content
            .clone();
        task.conversation_chunks = Some(RuntimeConversationCompactionChunks {
            current,
            pending: Vec::new(),
            summaries: Vec::new(),
            synthesis_source_bytes: None,
            failures: 0,
            completed: 0,
        });
    }
    Ok(())
}

/// Rebuilds one active-turn compactor request from temporary source blocks.
fn runtime_rebuild_active_turn_compaction_request(
    task: &mut RuntimeAgentCompactionTask,
    blocks: Vec<ContextBlock>,
) -> Result<()> {
    let RuntimeAgentCompactionTarget::ActiveTurn {
        plan,
        current_blocks,
        final_request_retry,
        ..
    } = &mut task.target
    else {
        return Err(MezError::invalid_state(
            "active-turn compaction request rebuild requires an active-turn target",
        ));
    };
    let blocks = runtime_redact_compaction_blocks(&blocks);
    task.summarized_entries = blocks.len();
    let correction_guidance = task
        .request
        .messages
        .iter()
        .filter(|message| {
            message.role == ModelMessageRole::Developer
                && message.source == ContextSourceKind::Configuration
                && message.content.starts_with("[private summary correction]")
        })
        .cloned()
        .collect::<Vec<_>>();
    task.request = runtime_model_compaction_request_for_blocks(
        &task.model_profile,
        &task.pane_id,
        &task.conversation_id,
        &blocks,
        task.request.allowed_actions.clone(),
    )?;
    let effective = final_request_retry.summary_ceiling;
    task.summary_correction.effective_input_tokens = effective;
    let (ceiling, tokens) = if let Some(target) = task.summary_correction.guidance_target {
        (target, task.summary_correction.guidance_is_input_tokens)
    } else if let Some(target) = effective {
        (target, true)
    } else {
        (plan.summary_budget_words(), plan.planning_budget().1)
    };
    // A legacy no-work advisory has no output allowance. Canonical selection
    // remains independently eligible, including during private backoff and
    // synthesis; preserve the initial request's absence of a zero-word order.
    if ceiling > 0 {
        summary_correction::communicate_summary_target(&mut task.request, ceiling, tokens);
    }
    let mut messages = task.request.messages.iter().cloned().collect::<Vec<_>>();
    for message in correction_guidance {
        messages.insert(2.min(messages.len()), message);
    }
    task.request.messages = messages.into();
    *current_blocks = blocks;
    Ok(())
}

/// Communicates the semantic word allowance without changing provider output
/// allocation. Words, estimated input tokens and provider generation tokens are
/// different units. Canonical adapters alone determine supported wire controls;
/// OpenAI deliberately transmits no max_output_tokens in any request.
fn runtime_limit_compaction_summary_output(
    request: &mut ModelRequest,
    summary_budget_words: usize,
) {
    summary_correction::communicate_summary_target(request, summary_budget_words, false);
}

/// Renders the frozen plan's read-only projection or private inference chunks.
/// Reimporting those blocks would reactivate historical user roots and invent
/// lifecycle for native fragments. Source selection and publication retain the
/// original typed owner independently of this auxiliary text envelope.
pub(super) fn runtime_model_compaction_request_for_blocks(
    profile: &ModelProfile,
    pane_id: &str,
    conversation_id: &str,
    blocks: &[ContextBlock],
    allowed_actions: AllowedActionSet,
) -> Result<ModelRequest> {
    let source_text = runtime_model_compaction_source_for_blocks(pane_id, conversation_id, blocks)?;
    Ok(runtime_model_compaction_request_from_source(
        profile,
        pane_id,
        conversation_id,
        source_text,
        allowed_actions,
    ))
}

/// Splits only temporary compactor input while leaving the atomic source plan unchanged.
fn runtime_split_compaction_blocks(
    blocks: &[ContextBlock],
) -> Option<(Vec<ContextBlock>, Vec<ContextBlock>)> {
    if blocks.len() > 1 {
        let split_at = blocks.len().div_ceil(2);
        return Some((blocks[..split_at].to_vec(), blocks[split_at..].to_vec()));
    }
    let block = blocks.first()?;
    if block.content.len() < 2 {
        return None;
    }
    let midpoint = block.content.len() / 2;
    let split_at = block
        .content
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| *index >= midpoint)
        .unwrap_or(block.content.len());
    if split_at == 0 || split_at >= block.content.len() {
        return None;
    }
    let mut first = block.clone();
    first.content = block.content[..split_at].to_string();
    let mut second = block.clone();
    second.content = block.content[split_at..].to_string();
    Some((vec![first], vec![second]))
}

/// Converts completed temporary summaries into one new synthesis round.
fn runtime_compaction_summary_blocks(summaries: Vec<String>) -> Vec<ContextBlock> {
    summaries
        .into_iter()
        .enumerate()
        .map(|(index, summary)| {
            ContextBlock::reference_event(
                ContextSourceKind::Memory,
                format!("recursive compaction summary {}", index.saturating_add(1)),
                summary,
            )
        })
        .collect()
}

/// Formats source as reference text without constructing an ordinary context.
/// Original labels, roles and redaction are unchanged; descriptive accounting
/// and lifecycle authority remain outside this provider-visible projection.
fn runtime_model_compaction_source_for_blocks(
    pane_id: &str,
    conversation_id: &str,
    blocks: &[ContextBlock],
) -> Result<String> {
    if blocks
        .iter()
        .any(|block| block.placement != mez_agent::ContextPlacement::ConversationAppend)
    {
        return Err(MezError::invalid_state(
            "compactor input must be selected chronological source",
        ));
    }
    let mut lines = vec![
        format!("Pane: {pane_id}"),
        format!("Conversation: {conversation_id}"),
        format!("Provider-bound context blocks supplied: {}", blocks.len()),
    ];
    for (index, block) in blocks.iter().enumerate() {
        lines.push(format!(
            "context_block={} source={} label={} content={}",
            index,
            runtime_context_source_kind_name(block.source),
            json_escape(&block.label),
            runtime_model_compaction_entry_content(&block.content)
        ));
    }
    Ok(lines.join("\n"))
}

/// Returns a stable source label for context included in compaction input.
pub(super) fn runtime_context_source_kind_name(source: ContextSourceKind) -> &'static str {
    mez_agent::epoch_compaction::provider::source_kind_name(source)
}

/// Redacts one transcript entry before sending it for compaction while
/// preserving its structural whitespace for model interpretation.
pub(super) fn runtime_model_compaction_entry_content(content: &str) -> String {
    mez_agent::epoch_compaction::provider::redact_content(content)
}

/// Creates temporary compactor source blocks whose content is redacted before
/// request sizing or UTF-8-boundary splitting. The frozen replacement plan
/// continues to own the original context and is applied only after synthesis.
fn runtime_redact_compaction_blocks(blocks: &[ContextBlock]) -> Vec<ContextBlock> {
    blocks
        .iter()
        .cloned()
        .map(|mut block| {
            block.content = runtime_model_compaction_entry_content(&block.content);
            block
        })
        .collect()
}

/// Extracts the completed payload through the same local optional-frame decoder
/// used before frozen-budget validation. Parser errors carry no response text.
pub(super) fn runtime_model_compaction_summary_from_response(
    response: &ModelResponse,
    turn_id: &str,
    agent_id: &str,
) -> Result<String> {
    summary_correction::completed_summary(response, turn_id, agent_id).map_err(|()| {
        MezError::invalid_state("model compaction response contained a malformed summary frame")
    })
}

#[cfg(test)]
mod size_diagnostic_tests {
    use super::*;

    /// A synthesis round must shrink the frozen source; equal, growing, and
    /// empty candidates cannot cause another model request.
    #[test]
    fn manual_synthesis_requires_strict_source_reduction() {
        assert!(runtime_require_smaller_synthesis(Some(100), 99).is_ok());
        assert!(runtime_require_smaller_synthesis(Some(100), 100).is_err());
        assert!(runtime_require_smaller_synthesis(Some(100), 101).is_err());
        assert!(runtime_require_smaller_synthesis(Some(100), 0).is_err());
        assert!(runtime_require_smaller_synthesis(None, 1).is_ok());
    }

    /// A single oversized source can be split repeatedly without cutting a
    /// Unicode scalar or losing its final sentinel between temporary chunks.
    #[test]
    fn active_turn_chunk_split_preserves_unicode_and_tail() {
        let source = format!("{}END_SENTINEL", "文🙂".repeat(1024));
        let mut fragments = vec![ContextBlock::assistant_event("source", source.clone())];
        for _ in 0..5 {
            let first = fragments.remove(0);
            let (left, right) = runtime_split_compaction_blocks(&[first]).unwrap();
            fragments.insert(0, right.into_iter().next().unwrap());
            fragments.insert(0, left.into_iter().next().unwrap());
        }
        assert_eq!(
            fragments
                .iter()
                .map(|block| block.content.as_str())
                .collect::<String>(),
            source
        );
        assert!(fragments.last().unwrap().content.ends_with("END_SENTINEL"));
    }

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
