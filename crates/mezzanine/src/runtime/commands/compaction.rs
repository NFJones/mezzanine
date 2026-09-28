//! Agent-shell conversation compaction commands and provider request helpers.
//!
//! This module owns `/compact`, queued model-backed compaction tasks,
//! compaction provider request construction, durable summary extraction,
//! and retained transcript-tail calculations. Keeping compaction isolated
//! avoids mixing long-running summarization workflow with ordinary shell
//! command dispatch.

use super::{
    AGENT_COMPACT_TRANSCRIPT_ENTRY_CONTEXT_OVERHEAD_WORDS, AgentActionPayload, AgentContext,
    AgentShellCommandOutcome, AgentTurnRecord, AgentTurnState, AllowedActionSet, ContextBlock,
    ContextSourceKind, DEFAULT_PROVIDER_TIMEOUT_MS, MemoryRecord, MemoryScope, MemorySource,
    MezError, ModelInteractionKind, ModelMessage, ModelMessageRole, ModelProfile, ModelRequest,
    ModelResponse, ProviderApiCompatibility, ReqwestProviderHttpTransport, Result,
    RuntimeAgentCompactionDispatch, RuntimeAgentCompactionTask,
    RuntimeAgentProviderDispatchProvider, RuntimeSessionService, TranscriptEntry, TranscriptRole,
    append_mcp_context, current_unix_seconds,
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
use crate::runtime::{
    AgentCompactionEvent, RenderInvalidationReason, RuntimeTransition,
    runtime_agent_transcript_context_blocks,
};
use crate::security::auth::AuthProfileCredentialSource;
use crate::storage::transcript::{AgentCompactionEpoch, AgentCompactionRange};
use mez_agent::{ProviderErrorRetryClass, apply_model_context_compaction_plan};

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

/// Maps only an exact, contiguous frozen selection to durable execution rows.
/// Unmatched or ambiguous selections retain the existing full raw replay window.
fn selected_durable_compaction_range(
    plan: &mez_agent::ModelContextCompactionPlan,
    context: &AgentContext,
    entries: &[TranscriptEntry],
    summary: &str,
) -> Option<AgentCompactionRange> {
    let selected = plan.replacement_blocks();
    if selected.is_empty() || selected.len() != plan.replacement_event_sequences().len() {
        return None;
    }
    let selected_events = plan
        .replacement_event_sequences()
        .iter()
        .zip(selected)
        .map(|(sequence, block)| {
            context
                .chronology()
                .iter()
                .find(|event| event.sequence() == *sequence && event.block() == block)
        })
        .collect::<Option<Vec<_>>>()?;
    if selected_events
        .iter()
        .any(|event| event.execution_group_id().is_none())
    {
        return None;
    }
    let matching = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            let Some(mez_agent::TranscriptContextEvent::ExecutionBlock {
                source,
                label,
                content,
                execution_group_id: Some(_),
                ordinal: Some(_),
                ..
            }) = mez_agent::TranscriptContextEvent::from_transcript_content(&entry.content)
            else {
                return false;
            };
            selected[0].source == source
                && selected[0].label == label
                && selected[0].content == content
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return None;
    }
    let start = matching[0];
    let rows = entries.get(start..start.checked_add(selected.len())?)?;
    let mut selected_groups = std::collections::BTreeSet::new();
    let mut prior_group = None;
    let mut expected_ordinal = 0_u64;
    if !rows
        .iter()
        .zip(selected)
        .zip(&selected_events)
        .all(|((entry, block), event)| {
            matches!(mez_agent::TranscriptContextEvent::from_transcript_content(&entry.content),
            Some(mez_agent::TranscriptContextEvent::ExecutionBlock { source, label, content,
                execution_group_id: Some(group), ordinal: Some(ordinal), .. })
                if block.source == source && block.label == label && block.content == content
                    && event.execution_group_id() == Some(&group)
                    && {
                        if prior_group.as_ref() != Some(&group) {
                            if !selected_groups.insert(group.clone()) { return false; }
                            prior_group = Some(group.clone());
                            expected_ordinal = 0;
                        }
                        expected_ordinal = expected_ordinal.saturating_add(1);
                        ordinal == expected_ordinal
                    })
        })
    {
        return None;
    }
    if !rows
        .windows(2)
        .all(|pair| pair[0].sequence.checked_add(1) == Some(pair[1].sequence))
    {
        return None;
    }
    if entries.iter().enumerate().any(|(index, entry)| {
        (index < start || index >= start + rows.len())
            && matches!(mez_agent::TranscriptContextEvent::from_transcript_content(&entry.content),
                Some(mez_agent::TranscriptContextEvent::ExecutionBlock {
                    execution_group_id: Some(candidate), ..
                }) if selected_groups.contains(&candidate))
    }) {
        return None;
    }
    Some(AgentCompactionRange {
        first_sequence: rows.first()?.sequence,
        through_sequence: rows.last()?.sequence,
        summary: summary.to_string(),
    })
}

impl RuntimeSessionService {
    /// Checks whether excluding a complete group could still yield a fitting
    /// provider request, without changing the running turn or durable epoch.
    fn compaction_walkback_candidate_fits(
        &mut self,
        task: &RuntimeAgentCompactionTask,
        turn_id: &str,
        plan: &mez_agent::ModelContextCompactionPlan,
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
        let Some(context) = self.agent_turn_contexts().get(turn_id) else {
            return Ok(false);
        };
        let Some(profile) = self.agent_turn_model_profile(turn_id).cloned() else {
            return Ok(false);
        };
        // One token is the smallest useful model-authored summary. A request
        // that cannot fit even this lower bound cannot benefit from walk-back.
        let (candidate, _) = apply_model_context_compaction_plan(context.clone(), plan, "x")
            .map_err(|error| MezError::invalid_state(error.message()))?;
        if let RuntimeAgentCompactionTarget::ActiveTurn {
            trigger:
                RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                    observed_input_tokens,
                    ..
                },
            ..
        } = &task.target
        {
            let projection =
                self.prospective_observed_compaction_epoch(task, context, plan, "x")?;
            let provider = self
                .provider_registry()
                .provider(&profile.provider)
                .ok_or_else(|| MezError::config("compaction walk-back provider is unavailable"))?;
            let api = resolve_provider_api(&provider.kind, provider.api.as_deref())?;
            let options = runtime_effective_provider_options(provider, &profile);
            if let Some(projection) = projection {
                let (tokens, limit, _) = self
                    .validate_observed_input_compaction_refresh_candidate(
                        task,
                        &turn,
                        candidate,
                        "x",
                        Some(projection),
                        *observed_input_tokens,
                        &profile,
                        &options,
                        api,
                        true,
                    )?;
                return Ok(tokens <= limit);
            }
            // An uncommitted first-turn selection has no durable replay to
            // preview. Measure the complete turn-local candidate below instead.
        }
        let mcp_summary = self.mcp_registry().prompt_summary();
        let (prepared, tools) =
            self.prepare_agent_turn_model_context(&turn, candidate, &mcp_summary, &profile)?;
        let provider = self
            .provider_registry()
            .provider(&profile.provider)
            .ok_or_else(|| MezError::config("compaction walk-back provider is unavailable"))?;
        let api = resolve_provider_api(&provider.kind, provider.api.as_deref())?;
        let options = runtime_effective_provider_options(provider, &profile);
        let mut request =
            assemble_model_request(&profile, api, &turn, &prepared.to_agent_context())?;
        let (actions, interaction) = self.agent_provider_request_control_for_turn(&turn)?;
        mez_agent::apply_model_request_control(&mut request, actions, interaction);
        mez_agent::apply_default_action_gates(
            &mut request,
            &tools,
            self.runtime_persistent_memory_enabled(),
            super::runtime_issues_enabled(self),
        );
        let Some(limit) = runtime_compaction_safe_input_limit(
            profile.max_input_tokens(),
            profile.context_window_tokens(),
            profile.max_output_tokens(),
        ) else {
            return Ok(false);
        };
        let tokens =
            mez_agent::provider_request_input_estimate(&request, api, &options, true)?.input_tokens;
        if let RuntimeAgentCompactionTarget::ActiveTurn {
            trigger:
                RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                    observed_input_tokens,
                    ..
                },
            ..
        } = &task.target
            && tokens <= limit
            && u64::try_from(tokens).unwrap_or(u64::MAX) >= *observed_input_tokens
        {
            return Ok(false);
        }
        Ok(tokens <= limit)
    }

    /// Builds a selective epoch only when every selected row already exists durably.
    /// Otherwise the existing raw replay boundary remains authoritative.
    fn prospective_observed_compaction_epoch(
        &self,
        task: &RuntimeAgentCompactionTask,
        context: &AgentContext,
        plan: &mez_agent::ModelContextCompactionPlan,
        summary: &str,
    ) -> Result<Option<AgentCompactionEpoch>> {
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Ok(None);
        };
        let previous = store.compaction_epoch(&task.conversation_id)?;
        let boundary = previous.as_ref().map_or(0, |epoch| epoch.through_sequence);
        let staged = match &task.target {
            RuntimeAgentCompactionTarget::ActiveTurn { staged, .. } => {
                staged.as_ref().and_then(|state| state.projection.as_ref())
            }
            RuntimeAgentCompactionTarget::Conversation => None,
        };
        if staged.is_some_and(|epoch| {
            epoch.through_sequence != boundary
                || epoch.conversation_id != task.conversation_id
                || epoch.summary != previous.as_ref().map_or("", |old| old.summary.as_str())
                || !epoch.ranges.starts_with(
                    previous
                        .as_ref()
                        .map_or(&[][..], |old| old.ranges.as_slice()),
                )
        }) {
            return Err(MezError::invalid_state("staged compaction epoch is stale"));
        }
        let pending = self
            .persistence
            .pending_transcript_entries(&task.conversation_id);
        let view = store.conversation_transcript_view(
            &task.conversation_id,
            crate::storage::transcript::ConversationTranscriptRead::All,
            boundary > 0 || task.transcript_entries > pending.len() as u64,
            &pending,
        )?;
        // A preceding provisional summary without a durable mapping cannot
        // become an epoch merely because later rows committed. Still read the
        // archive above: an absent required prefix is an integrity failure.
        if matches!(&task.target, RuntimeAgentCompactionTarget::ActiveTurn {
            staged: Some(state), ..
        } if state.projection.is_none())
        {
            return Ok(None);
        }
        let Some(range) =
            selected_durable_compaction_range(plan, context, &view.committed, summary)
        else {
            if staged.is_some() {
                return Err(MezError::invalid_state(
                    "staged compaction range cannot be mapped to durable transcript rows",
                ));
            }
            return Ok(None);
        };
        if range.first_sequence <= boundary
            || pending
                .iter()
                .any(|entry| entry.sequence <= range.through_sequence)
        {
            if staged.is_some() {
                return Err(MezError::invalid_state(
                    "staged compaction range crosses a pending or compacted transcript boundary",
                ));
            }
            return Ok(None);
        }
        let mut ranges = staged
            .or(previous.as_ref())
            .map_or_else(Vec::new, |epoch| epoch.ranges.clone());
        if ranges
            .last()
            .is_some_and(|last| last.through_sequence >= range.first_sequence)
        {
            if staged.is_some() {
                return Err(MezError::invalid_state(
                    "staged compaction range overlaps an earlier summary",
                ));
            }
            return Ok(None);
        }
        ranges.push(range);
        Ok(Some(AgentCompactionEpoch {
            version: 2,
            conversation_id: task.conversation_id.clone(),
            through_sequence: boundary,
            summary: previous.map_or_else(String::new, |epoch| epoch.summary),
            ranges,
        }))
    }

    /// Freezes only the newly selected committed rows, retaining the source
    /// identity of earlier provisional ranges across subsequent model calls.
    fn frozen_observed_compaction_rows(
        &self,
        task: &RuntimeAgentCompactionTask,
        context: &AgentContext,
        plan: &mez_agent::ModelContextCompactionPlan,
        summary: &str,
        projection: &AgentCompactionEpoch,
    ) -> Result<Vec<TranscriptEntry>> {
        let store = self.persistence.cloned_transcript_store().ok_or_else(|| {
            MezError::invalid_state("selective compaction requires transcript storage")
        })?;
        let committed = store
            .conversation_transcript_view(
                &task.conversation_id,
                crate::storage::transcript::ConversationTranscriptRead::All,
                true,
                &[],
            )?
            .committed;
        let range = selected_durable_compaction_range(plan, context, &committed, summary)
            .ok_or_else(|| MezError::conflict("selective compaction source changed"))?;
        if projection.ranges.last() != Some(&range) {
            return Err(MezError::conflict("selective compaction source changed"));
        }
        let mut frozen = task.frozen_compaction_rows.clone();
        frozen.extend(committed.into_iter().filter(|entry| {
            (range.first_sequence..=range.through_sequence).contains(&entry.sequence)
        }));
        Ok(frozen)
    }

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
        self.queue_agent_shell_compaction_with_model(pane_id, "manual", None)
    }

    /// Queues model-backed conversation compaction and marks the pane active.
    ///
    /// Manual `/compact` is submitted through synchronous prompt input, so it
    /// must publish visible state and return before provider I/O starts. The
    /// async provider service claims the queued task and reports completion
    /// through the runtime event loop.
    fn queue_agent_shell_compaction_with_model(
        &mut self,
        pane_id: &str,
        source: &str,
        resume_turn_id: Option<&str>,
    ) -> Result<AgentShellCommandOutcome> {
        let (conversation_id, transcript_entries, visibility, running_turn_id) = {
            let session = self.agent_shell_store().get(pane_id).ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotFound,
                    "agent shell session not found for pane",
                )
            })?;
            (
                session.session_id.clone(),
                session.transcript_entries,
                session.visibility,
                session.running_turn_id.clone(),
            )
        };
        if let Some(turn_id) = running_turn_id
            && resume_turn_id != Some(turn_id.as_str())
        {
            return Err(MezError::conflict(format!(
                "cannot compact conversation while turn {turn_id} is running"
            )));
        }
        if self.agent_is_compacting(pane_id) {
            return Err(MezError::conflict(format!(
                "cannot compact conversation while pane {pane_id} is already compacting"
            )));
        }
        let _ = self.runtime_prune_expired_persistent_memory_best_effort();
        if transcript_entries == 0 {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                "agent: compact skipped; no transcript entries are available",
            )?;
            return Ok(AgentShellCommandOutcome::Display {
                command: "compact".to_string(),
                body: format!(
                    "pane={} conversation={} previous_transcript_entries=0 summarized_entries=0 compacted=false reason=no-transcript-entries source=model-compact trigger={}",
                    json_escape(pane_id),
                    json_escape(&conversation_id),
                    json_escape(source)
                ),
            });
        }
        let transcript_records =
            self.inspect_agent_shell_transcript_for_compaction(&conversation_id)?;
        if transcript_records.is_empty() {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                "agent: compact skipped; no durable transcript entries are available",
            )?;
            return Ok(AgentShellCommandOutcome::Display {
                command: "compact".to_string(),
                body: format!(
                    "pane={} conversation={} previous_transcript_entries={} summarized_entries=0 compacted=false reason=no-durable-transcript source=model-compact trigger={}",
                    json_escape(pane_id),
                    json_escape(&conversation_id),
                    transcript_entries,
                    json_escape(source)
                ),
            });
        }

        let agent_id = format!("agent-{pane_id}");
        let (model_profile_name, model_profile) =
            self.active_model_profile_for_pane(pane_id, &agent_id, None)?;
        let retained_tail_percent = self.agent_compaction_raw_retention_percent();
        let context_budget_tokens = model_profile
            .max_input_tokens()
            .or_else(|| model_profile.context_window_tokens())
            .map(|limit| limit.min(model_profile.context_window_tokens().unwrap_or(limit)))
            .ok_or_else(|| {
            MezError::invalid_state(
                "model context compaction requires configured context_window_tokens or max_input_tokens",
            )
        })?;
        let retained_transcript_entries = if source == "manual" || resume_turn_id.is_some() {
            runtime_compact_forced_retained_transcript_entries(
                transcript_entries,
                &transcript_records,
                context_budget_tokens,
                retained_tail_percent,
            )
        } else {
            runtime_compact_retained_transcript_entries(
                transcript_entries,
                &transcript_records,
                context_budget_tokens,
                retained_tail_percent,
            )
        };
        let compactable_transcript_records = runtime_compact_transcript_entries_for_summary(
            transcript_entries,
            &transcript_records,
            retained_transcript_entries,
        );
        let retained_transcript_entries = u64::try_from(
            runtime_compact_active_transcript_entry_count(
                transcript_entries,
                transcript_records.len(),
            )
            .saturating_sub(compactable_transcript_records.len()),
        )
        .unwrap_or(u64::MAX);
        if compactable_transcript_records.is_empty() {
            let retained_tail_budget_words = runtime_compact_retained_context_tail_budget_words(
                context_budget_tokens,
                retained_tail_percent,
            );
            let retained_tail_words = runtime_compact_retained_transcript_tail_context_words(
                transcript_entries,
                &transcript_records,
                retained_transcript_entries,
            );
            let irreducible_tail = retained_tail_words > retained_tail_budget_words;
            let (status, reason) = if irreducible_tail {
                (
                    format!(
                        "agent: compaction skipped; exact unfinished transcript tail exceeds retention budget retained_tail_words={retained_tail_words} retained_tail_budget_words={retained_tail_budget_words}"
                    ),
                    "irreducible-exact-retained-tail",
                )
            } else {
                (
                    "agent: compact skipped; recent transcript tail already fits the active context budget".to_string(),
                    "within-retained-context-tail",
                )
            };
            self.append_agent_status_text_to_terminal_buffer(pane_id, &status)?;
            return Ok(AgentShellCommandOutcome::Display {
                command: "compact".to_string(),
                body: format!(
                    "pane={} conversation={} previous_transcript_entries={} summarized_entries=0 remaining_transcript_entries={} compacted=false reason={reason} retained_tail_words={retained_tail_words} retained_tail_budget_words={retained_tail_budget_words} retained_context_tail_percent={} source=model-compact trigger={}",
                    json_escape(pane_id),
                    json_escape(&conversation_id),
                    transcript_entries,
                    retained_transcript_entries,
                    retained_tail_percent,
                    json_escape(source)
                ),
            });
        }

        let compaction_context =
            self.agent_context_for_pane_prompt(pane_id, "[context compaction requested]", 100)?;
        let compaction_context =
            self.apply_agent_shell_preference_context(pane_id, compaction_context)?;
        let mcp_summary = self.mcp_registry().prompt_summary();
        let compaction_context = runtime_compaction_context_without_transcript_blocks(
            append_mcp_context(compaction_context, &mcp_summary)?,
        )?;
        let summarized_entries = compactable_transcript_records.len();
        let compacted_through_sequence = compactable_transcript_records
            .last()
            .map(|entry| entry.sequence);
        let allowed_actions = self.capture_agent_session_allowed_actions_for_pane(pane_id)?;
        let mut request = runtime_model_compaction_request(
            &model_profile,
            pane_id,
            &conversation_id,
            transcript_entries,
            compactable_transcript_records,
            &compaction_context,
            allowed_actions,
        )?;
        let summary_budget_words = request
            .messages
            .last()
            .map(|message| model_context_text_word_count(&message.content))
            .unwrap_or_default()
            .max(1);
        runtime_limit_compaction_summary_output(&mut request, summary_budget_words);
        let manual_retry_source = request
            .messages
            .last()
            .map(|message| message.content.clone());
        self.queue_agent_compaction_task(RuntimeAgentCompactionTask {
            task_generation: 0,
            compaction_epoch: 0,
            pane_id: pane_id.to_string(),
            conversation_id: conversation_id.clone(),
            source: source.to_string(),
            transcript_entries,
            compacted_through_sequence,
            frozen_compaction_rows: Vec::new(),
            retained_transcript_entries,
            summarized_entries,
            model_profile_name: model_profile_name.clone(),
            model_profile: model_profile.clone(),
            request,
            preserve_summary_output_budget: true,
            manual_retry_source,
            manual_final_retry: None,
            candidate_context: Some(compaction_context),
            resume_turn_id: resume_turn_id.map(str::to_string),
            target: RuntimeAgentCompactionTarget::Conversation,
            conversation_chunks: None,
            compaction_request_shape: None,
        });
        self.append_agent_status_text_to_terminal_buffer(
            pane_id,
            &format!(
                "agent: compacting conversation summary trigger={} provider={} model={} previous_transcript_entries={} summarized_entries={}",
                source,
                model_profile.provider,
                model_profile.model,
                transcript_entries,
                summarized_entries
            ),
        )?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "compact".to_string(),
            body: format!(
                "pane={} conversation={} previous_transcript_entries={} summarized_entries={} compacted=false state=queued source=model-compact trigger={} model_profile={} provider={} model={}",
                json_escape(pane_id),
                json_escape(&conversation_id),
                transcript_entries,
                summarized_entries,
                json_escape(source),
                json_escape(&model_profile_name),
                json_escape(&model_profile.provider),
                json_escape(&model_profile.model)
            ),
            visibility,
        })
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
        if !plan.changes_context() {
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
        let mut current_blocks = runtime_redact_compaction_blocks(plan.replacement_blocks());
        let mut pending_blocks = Vec::new();
        let recovery_attempt = match trigger {
            RuntimeActiveTurnCompactionTrigger::ProviderContextLimit { attempt } => attempt,
            RuntimeActiveTurnCompactionTrigger::ObservedInputLimit { .. } => 0,
        };
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
        runtime_limit_compaction_summary_output(&mut request, plan.summary_budget_words());
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
                runtime_limit_compaction_summary_output(&mut request, plan.summary_budget_words());
            }
        }
        self.queue_agent_compaction_task(RuntimeAgentCompactionTask {
            task_generation: 0,
            compaction_epoch: 0,
            pane_id: turn.pane_id.clone(),
            conversation_id,
            source: match trigger {
                RuntimeActiveTurnCompactionTrigger::ProviderContextLimit { .. } => {
                    "provider-context-limit".to_string()
                }
                RuntimeActiveTurnCompactionTrigger::ObservedInputLimit { .. } => {
                    "observed-input-limit".to_string()
                }
            },
            // Proactive compaction must establish a durable epoch as well as
            // replacing this turn's context. Provider-rejection recovery is
            // intentionally turn-local because its source can include
            // unpersisted same-turn observations.
            transcript_entries: match trigger {
                RuntimeActiveTurnCompactionTrigger::ProviderContextLimit { .. } => 0,
                RuntimeActiveTurnCompactionTrigger::ObservedInputLimit { .. } => transcript_entries,
            },
            compacted_through_sequence: None,
            frozen_compaction_rows: Vec::new(),
            // The live context planner can omit exact transcript user events
            // from summary input because they are protected barriers. Until
            // durable replay can represent the same selected event ranges,
            // preserve this raw window instead of silently dropping barriers.
            retained_transcript_entries: match trigger {
                RuntimeActiveTurnCompactionTrigger::ProviderContextLimit { .. } => 0,
                RuntimeActiveTurnCompactionTrigger::ObservedInputLimit { .. } => transcript_entries,
            },
            summarized_entries: plan.replacement_blocks().len(),
            model_profile_name,
            model_profile,
            request,
            preserve_summary_output_budget: true,
            manual_retry_source: None,
            manual_final_retry: None,
            candidate_context: None,
            resume_turn_id: Some(turn.turn_id.clone()),
            target: RuntimeAgentCompactionTarget::ActiveTurn {
                turn_id: turn.turn_id.clone(),
                trigger,
                recovery_attempt,
                final_request_retry: Box::default(),
                staged: None,
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
        if self.pending_agent_compaction_task_generation(pane_id) != Some(task_generation) {
            return Ok(None);
        }
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
        let provider_api =
            resolve_provider_api(&provider_config.kind, provider_config.api.as_deref())?;
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
        let provider_options =
            runtime_effective_provider_options(&provider_config, &task.model_profile);
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
                kind,
                message,
                provider_failure_json,
                ..
            } => (
                pane_id.clone(),
                self.apply_agent_compaction_failed_event_for_generation(
                    &pane_id,
                    task_generation,
                    &kind,
                    &message,
                    provider_failure_json.as_deref(),
                )?,
            ),
        };
        if applied && !self.agent_is_compacting(&pane_id) {
            self.resume_agent_compaction_steering(&pane_id)?;
        }
        Ok(self.runtime_pane_transition_with_render(
            &pane_id,
            applied,
            Some(RenderInvalidationReason::FullRedraw),
        ))
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
        let current_conversation = self.agent_compaction_task_is_current(pane_id, task_generation);
        let Some(mut task) = self.finish_agent_compaction_task(pane_id, task_generation) else {
            return Ok(false);
        };
        self.record_agent_provider_token_usage_for_conversation(
            pane_id,
            &task.conversation_id,
            &std::collections::BTreeMap::from([(
                mez_agent::ModelTokenUsageKey::new(
                    &task.model_profile.provider,
                    &task.model_profile.model,
                ),
                response.usage,
            )]),
        );
        self.record_agent_provider_quota_usage_for_conversation(
            pane_id,
            &task.conversation_id,
            &response.quota_usage,
        );
        if !current_conversation {
            return Ok(false);
        }
        let application = (|| -> Result<()> {
            let summary = runtime_model_compaction_summary_from_response(&response)?;
            if let Some(chunks) = task.conversation_chunks.as_mut() {
                chunks.completed = chunks.completed.checked_add(1).ok_or_else(|| {
                    MezError::invalid_state("conversation compactor response count overflow")
                })?;
                chunks.summaries.push(summary.clone());
                if let Some(next) = chunks.pending.pop() {
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
                    runtime_rebuild_conversation_compaction_request(&mut task, synthesis)?;
                    self.queue_agent_compaction_task(task.clone());
                    return Ok(());
                }
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
                let (
                    turn_id,
                    trigger,
                    recovery_attempt,
                    rejected_request_bytes,
                    rejected_request_stream,
                    plan,
                ) = match &task.target {
                    RuntimeAgentCompactionTarget::ActiveTurn {
                        turn_id,
                        trigger,
                        recovery_attempt,
                        rejected_request_bytes,
                        rejected_request_stream,
                        plan,
                        ..
                    } => (
                        turn_id.clone(),
                        *trigger,
                        *recovery_attempt,
                        *rejected_request_bytes,
                        *rejected_request_stream,
                        plan.clone(),
                    ),
                    RuntimeAgentCompactionTarget::Conversation => {
                        unreachable!("active-turn target was matched above")
                    }
                };
                let Some(turn) = self
                    .agent_turn_ledger()
                    .turns()
                    .iter()
                    .find(|turn| turn.turn_id == turn_id)
                    .cloned()
                else {
                    return Err(MezError::invalid_state(
                        "active-turn compaction completed after its turn disappeared",
                    ));
                };
                if turn.state != AgentTurnState::Running {
                    return Err(MezError::invalid_state(
                        "active-turn compaction completed after its turn became terminal",
                    ));
                }
                let context = self
                    .agent_turn_contexts()
                    .get(&turn_id)
                    .cloned()
                    .ok_or_else(|| {
                        MezError::invalid_state("active-turn compaction context is unavailable")
                    })?;
                let context = match &task.target {
                    RuntimeAgentCompactionTarget::ActiveTurn {
                        staged: Some(staged),
                        ..
                    } => {
                        let mut rebased = staged.context.clone();
                        for event in context
                            .chronology()
                            .iter()
                            .filter(|event| event.sequence().get() > staged.source_high_water)
                        {
                            if event.semantic_kind() != mez_agent::ContextSemanticKind::UserEvent
                                || event.retention() != mez_agent::ContextRetention::Exact
                            {
                                return Err(MezError::invalid_state(
                                    "staged compaction cannot rebase changed non-user chronology",
                                ));
                            }
                            rebased
                                .append_user_event(
                                    event.block().label.clone(),
                                    event.block().content.clone(),
                                )
                                .map_err(|error| MezError::invalid_state(error.message()))?;
                        }
                        rebased
                    }
                    _ => context,
                };
                let (compacted, report) = apply_model_context_compaction_plan(
                    context.clone(),
                    plan.as_ref(),
                    &final_summary,
                )
                .map_err(|error| MezError::invalid_state(error.message()))?;
                let model_profile = self
                    .agent_turn_model_profile(&turn_id)
                    .cloned()
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "active-turn compaction model profile is unavailable",
                        )
                    })?;
                let mcp_summary = self.mcp_registry().prompt_summary();
                let (prepared, available_mcp_tools) = self.prepare_agent_turn_model_context(
                    &turn,
                    compacted.clone(),
                    &mcp_summary,
                    &model_profile,
                )?;
                let provider_config = self
                    .provider_registry()
                    .provider(&model_profile.provider)
                    .ok_or_else(|| {
                        MezError::config(format!(
                            "provider `{}` for active model profile is not configured",
                            model_profile.provider
                        ))
                    })?;
                let api =
                    resolve_provider_api(&provider_config.kind, provider_config.api.as_deref())?;
                let provider_options =
                    runtime_effective_provider_options(provider_config, &model_profile);
                let mut retry_request = assemble_model_request(
                    &model_profile,
                    api,
                    &turn,
                    &prepared.to_agent_context(),
                )?;
                let (allowed_actions, interaction_kind) =
                    self.agent_provider_request_control_for_turn(&turn)?;
                mez_agent::apply_model_request_control(
                    &mut retry_request,
                    allowed_actions,
                    interaction_kind,
                );
                mez_agent::apply_default_action_gates(
                    &mut retry_request,
                    &available_mcp_tools,
                    self.runtime_persistent_memory_enabled(),
                    super::runtime_issues_enabled(self),
                );
                // In the observed-input path the original transport stream mode is not
                // retained. Use streaming for conservative wire accounting; configured
                // chat-completions adapters still honor their own streaming option.
                let estimate_stream = rejected_request_stream.unwrap_or(true);
                let retry_estimate = mez_agent::provider_request_input_estimate(
                    &retry_request,
                    api,
                    &provider_options,
                    estimate_stream,
                )?;
                // The planner describes additional eligible source, not a
                // requirement to consume it. Size the complete refreshed
                // request before staging another unpublished range.
                let candidate_fits =
                    if let RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                        observed_input_tokens,
                        ..
                    } = trigger
                    {
                        let projection = self.prospective_observed_compaction_epoch(
                            &task,
                            &context,
                            plan.as_ref(),
                            &final_summary,
                        )?;
                        if projection.is_some() {
                            let (tokens, limit, _) = self
                                .validate_observed_input_compaction_refresh_candidate(
                                    &task,
                                    &turn,
                                    compacted.clone(),
                                    &final_summary,
                                    projection,
                                    observed_input_tokens,
                                    &model_profile,
                                    &provider_options,
                                    api,
                                    estimate_stream,
                                )?;
                            tokens <= limit
                        } else {
                            runtime_compaction_safe_input_limit(
                                model_profile.max_input_tokens(),
                                model_profile.context_window_tokens(),
                                model_profile.max_output_tokens(),
                            )
                            .is_some_and(|limit| {
                                retry_estimate.input_tokens <= limit
                                    && (retry_estimate.input_tokens as u64) < observed_input_tokens
                            })
                        }
                    } else {
                        runtime_compaction_safe_input_limit(
                            model_profile.max_input_tokens(),
                            model_profile.context_window_tokens(),
                            model_profile.max_output_tokens(),
                        )
                        .is_some_and(|limit| retry_estimate.input_tokens <= limit)
                    };
                if plan.requires_additional_segments() && !candidate_fits {
                    let attempts = match &task.target {
                        RuntimeAgentCompactionTarget::ActiveTurn { staged, .. } => {
                            staged.as_ref().map_or(0, |state| state.attempts)
                        }
                        RuntimeAgentCompactionTarget::Conversation => 0,
                    };
                    let projection = if matches!(
                        trigger,
                        RuntimeActiveTurnCompactionTrigger::ObservedInputLimit { .. }
                    ) {
                        self.prospective_observed_compaction_epoch(
                            &task,
                            &context,
                            plan.as_ref(),
                            &final_summary,
                        )?
                    } else {
                        None
                    };
                    let (budget, token_costs) = plan.planning_budget();
                    let provider =
                        mez_agent::ProviderBudgetProjection::new(api, &model_profile.provider);
                    let next_plan = if token_costs {
                        mez_agent::plan_model_context_compaction_for_provider_tokens(
                            &compacted,
                            budget,
                            0,
                            plan.consumed_sequence_high_water(),
                            provider,
                        )
                    } else {
                        mez_agent::plan_model_context_compaction_for_provider(
                            &compacted,
                            budget,
                            0,
                            plan.consumed_sequence_high_water(),
                            provider,
                        )
                    }
                    .map_err(|error| MezError::invalid_state(error.message()))?;
                    if !next_plan.changes_context() {
                        return Err(MezError::invalid_state(
                            "pre-summary recovery has no additional closed segment",
                        ));
                    }
                    if next_plan.replacement_event_sequences().first()
                        <= plan.replacement_event_sequences().last()
                    {
                        return Err(MezError::invalid_state(
                            "pre-summary recovery did not advance to another closed segment",
                        ));
                    }
                    let source_high_water = self
                        .agent_turn_contexts()
                        .get(&turn_id)
                        .map_or(0, AgentContext::event_sequence_high_water_mark);
                    if let Some(projection) = projection.as_ref() {
                        task.frozen_compaction_rows = self.frozen_observed_compaction_rows(
                            &task,
                            &context,
                            plan.as_ref(),
                            &final_summary,
                            projection,
                        )?;
                    }
                    if let RuntimeAgentCompactionTarget::ActiveTurn {
                        plan,
                        staged,
                        final_request_retry,
                        completed_summaries,
                        pending_blocks,
                        synthesis_source_bytes,
                        ..
                    } = &mut task.target
                    {
                        **plan = next_plan;
                        *staged = Some(Box::new(
                            crate::runtime::agent_state::RuntimeStagedCompaction {
                                context: compacted,
                                projection,
                                attempts: attempts.saturating_add(1),
                                source_high_water,
                            },
                        ));
                        final_request_retry.last_input_tokens = Some(retry_estimate.input_tokens);
                        final_request_retry.attempts = 0;
                        final_request_retry.summary_ceiling = None;
                        completed_summaries.clear();
                        pending_blocks.clear();
                        *synthesis_source_bytes = None;
                        let blocks = runtime_redact_compaction_blocks(plan.replacement_blocks());
                        runtime_rebuild_active_turn_compaction_request(&mut task, blocks)?;
                        self.queue_agent_compaction_task(task.clone());
                        return Ok(());
                    }
                }
                if let Some(input_limit) = runtime_compaction_safe_input_limit(
                    model_profile.max_input_tokens(),
                    model_profile.context_window_tokens(),
                    model_profile.max_output_tokens(),
                ) && retry_estimate.input_tokens > input_limit
                    && !matches!(
                        trigger,
                        RuntimeActiveTurnCompactionTrigger::ObservedInputLimit { .. }
                    )
                {
                    if let RuntimeAgentCompactionTarget::ActiveTurn {
                        final_request_retry,
                        ..
                    } = &mut task.target
                    {
                        let summary_tokens =
                            mez_agent::provider_text_input_token_estimate(&final_summary);
                        let excess = retry_estimate.input_tokens.saturating_sub(input_limit);
                        let previous = final_request_retry.last_input_tokens;
                        let next_budget = summary_tokens
                            .saturating_sub(excess)
                            .min(summary_tokens / 2);
                        if next_budget > 0
                            && final_request_retry
                                .summary_ceiling
                                .is_none_or(|ceiling| next_budget < ceiling)
                            && previous.is_none_or(|value| retry_estimate.input_tokens < value)
                        {
                            final_request_retry.attempts =
                                final_request_retry.attempts.saturating_add(1);
                            final_request_retry.last_input_tokens =
                                Some(retry_estimate.input_tokens);
                            final_request_retry.summary_ceiling = Some(next_budget);
                            let attempt = final_request_retry.attempts;
                            let blocks =
                                runtime_redact_compaction_blocks(plan.replacement_blocks());
                            runtime_rebuild_active_turn_compaction_request(&mut task, blocks)?;
                            task.request.max_output_tokens = Some(next_budget);
                            self.append_agent_trace_turn_event(
                                pane_id,
                                &turn_id,
                                &format!(
                                    "context_compaction final_request_retry attempt={} estimated_input_tokens={} safe_input_tokens={input_limit} summary_budget_tokens={next_budget}",
                                    attempt,
                                    retry_estimate.input_tokens,
                                ),
                            )?;
                            self.queue_agent_compaction_task(task.clone());
                            return Ok(());
                        }
                    }
                    return Err(MezError::invalid_state(format!(
                        "context compaction candidate exceeds safe input allowance: {}",
                        runtime_compaction_candidate_size_diagnostic(
                            &prepared.to_agent_context(),
                            Some(mez_agent::ProviderBudgetProjection::new(
                                api,
                                &model_profile.provider
                            )),
                            retry_estimate.input_tokens,
                            input_limit,
                        ),
                    )));
                }
                if let (Some(rejected_bytes), Some(stream)) =
                    (rejected_request_bytes, rejected_request_stream)
                {
                    let retry_bytes = mez_agent::openai_responses_request_body_with_stream(
                        &retry_request,
                        stream,
                    )?
                    .len();
                    if retry_bytes >= rejected_bytes {
                        return Err(MezError::invalid_state(format!(
                            "context-limit recovery could not produce a smaller OpenAI Responses request: rejected_request_bytes={rejected_bytes} retry_request_bytes={retry_bytes}"
                        )));
                    }
                    self.append_agent_trace_turn_event(
                        pane_id,
                        &turn_id,
                        &format!(
                            "context_limit_recovery request_shrunk rejected_request_bytes={rejected_bytes} retry_request_bytes={retry_bytes}"
                        ),
                    )?;
                }
                if let RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                    observed_input_tokens,
                    ..
                } = trigger
                {
                    let projection = self.prospective_observed_compaction_epoch(
                        &task,
                        &context,
                        plan.as_ref(),
                        &final_summary,
                    )?;
                    let (candidate_tokens, input_limit, diagnostic) = if projection.is_some() {
                        self.validate_observed_input_compaction_refresh_candidate(
                            &task,
                            &turn,
                            compacted.clone(),
                            &final_summary,
                            projection.clone(),
                            observed_input_tokens,
                            &model_profile,
                            &provider_options,
                            api,
                            estimate_stream,
                        )?
                    } else {
                        // Nothing selected is committed. The running turn can
                        // use its compacted chronology, but durable replay must
                        // keep every original row. Size the complete ordinary
                        // request assembled above, not a speculative epoch.
                        let limit = runtime_compaction_safe_input_limit(
                            model_profile.max_input_tokens(),
                            model_profile.context_window_tokens(),
                            model_profile.max_output_tokens(),
                        )
                        .ok_or_else(|| {
                            MezError::invalid_state(
                                "turn-local compaction has no provider input allowance",
                            )
                        })?;
                        let estimate = retry_estimate.input_tokens;
                        if estimate <= limit && (estimate as u64) >= observed_input_tokens {
                            return Err(MezError::invalid_state(
                                "turn-local compaction did not reduce the triggering request",
                            ));
                        }
                        (
                            estimate,
                            limit,
                            runtime_compaction_candidate_size_diagnostic(
                                &prepared.to_agent_context(),
                                Some(mez_agent::ProviderBudgetProjection::new(
                                    api,
                                    &model_profile.provider,
                                )),
                                estimate,
                                limit,
                            ),
                        )
                    };
                    if candidate_tokens > input_limit {
                        let summary_tokens =
                            mez_agent::provider_text_input_token_estimate(&final_summary);
                        let excess = candidate_tokens.saturating_sub(input_limit);
                        let next_budget = summary_tokens
                            .saturating_sub(excess)
                            .min(summary_tokens / 2);
                        // Once this summary cannot account for the remaining excess,
                        // search the next closed segment in the unpublished context.
                        // Neither the live context nor its epoch changes until the
                        // combined projection has passed the complete request check.
                        if next_budget == 0 && projection.is_some() {
                            let budget = model_profile
                                .max_input_tokens()
                                .or_else(|| model_profile.context_window_tokens())
                                .unwrap_or(input_limit)
                                .max(1);
                            let next_plan =
                                mez_agent::plan_model_context_compaction_for_provider_tokens(
                                    &compacted,
                                    budget,
                                    0,
                                    plan.consumed_sequence_high_water(),
                                    mez_agent::ProviderBudgetProjection::new(
                                        api,
                                        &model_profile.provider,
                                    ),
                                )
                                .map_err(|error| MezError::invalid_state(error.message()))?;
                            let source_high_water = self
                                .agent_turn_contexts()
                                .get(&turn_id)
                                .map_or(0, AgentContext::event_sequence_high_water_mark);
                            let mut staged_task = task.clone();
                            if let RuntimeAgentCompactionTarget::ActiveTurn { staged, .. } =
                                &mut staged_task.target
                            {
                                *staged = projection.clone().map(|projection| {
                                    Box::new(crate::runtime::agent_state::RuntimeStagedCompaction {
                                        context: compacted.clone(),
                                        projection: Some(projection),
                                        attempts: 0,
                                        source_high_water,
                                    })
                                });
                            }
                            let next_range_is_durable = next_plan.changes_context()
                                && next_plan.replacement_event_sequences().first()
                                    > plan.replacement_event_sequences().last()
                                && self
                                    .prospective_observed_compaction_epoch(
                                        &staged_task,
                                        &compacted,
                                        &next_plan,
                                        "x",
                                    )?
                                    .is_some();
                            if next_range_is_durable {
                                let projection_ref = projection.as_ref().ok_or_else(|| {
                                    MezError::invalid_state(
                                        "durable compaction range projection is unavailable",
                                    )
                                })?;
                                task.frozen_compaction_rows = self
                                    .frozen_observed_compaction_rows(
                                        &task,
                                        &context,
                                        plan.as_ref(),
                                        &final_summary,
                                        projection_ref,
                                    )?;
                                let RuntimeAgentCompactionTarget::ActiveTurn {
                                    plan,
                                    staged,
                                    final_request_retry,
                                    completed_summaries,
                                    pending_blocks,
                                    synthesis_source_bytes,
                                    ..
                                } = &mut task.target
                                else {
                                    unreachable!()
                                };
                                **plan = next_plan;
                                let attempts = staged.as_ref().map_or(0, |state| state.attempts);
                                *staged = projection.clone().map(|projection| {
                                    Box::new(crate::runtime::agent_state::RuntimeStagedCompaction {
                                        context: compacted.clone(),
                                        projection: Some(projection),
                                        attempts: attempts.saturating_add(1),
                                        source_high_water,
                                    })
                                });
                                final_request_retry.last_input_tokens = Some(candidate_tokens);
                                final_request_retry.attempts = 0;
                                final_request_retry.summary_ceiling = None;
                                completed_summaries.clear();
                                pending_blocks.clear();
                                *synthesis_source_bytes = None;
                                let blocks =
                                    runtime_redact_compaction_blocks(plan.replacement_blocks());
                                runtime_rebuild_active_turn_compaction_request(&mut task, blocks)?;
                                self.queue_agent_compaction_task(task.clone());
                                return Ok(());
                            }
                        }
                        if let RuntimeAgentCompactionTarget::ActiveTurn {
                            final_request_retry,
                            ..
                        } = &mut task.target
                            && next_budget > 0
                            && final_request_retry
                                .summary_ceiling
                                .is_none_or(|ceiling| next_budget < ceiling)
                            && final_request_retry
                                .last_input_tokens
                                .is_none_or(|previous| candidate_tokens < previous)
                        {
                            final_request_retry.attempts =
                                final_request_retry.attempts.saturating_add(1);
                            final_request_retry.last_input_tokens = Some(candidate_tokens);
                            final_request_retry.summary_ceiling = Some(next_budget);
                            let blocks =
                                runtime_redact_compaction_blocks(plan.replacement_blocks());
                            runtime_rebuild_active_turn_compaction_request(&mut task, blocks)?;
                            self.queue_agent_compaction_task(task.clone());
                            return Ok(());
                        }
                        return Err(MezError::invalid_state(format!(
                            "active-turn compaction refreshed candidate exceeds safe input allowance: {}",
                            diagnostic,
                        )));
                    }
                    if projection.is_none() {
                        // Keep the original transcript authoritative: the
                        // selected source is visible to this turn but has no
                        // committed rows an epoch may reference. The complete
                        // retry request was checked above before this mutation.
                        self.agent_turn_contexts_mut()
                            .insert(turn_id.clone(), compacted);
                        self.clear_agent_turn_provider_request_chain(&turn_id);
                        self.queue_agent_provider_recovery_task_after_compaction(
                            &turn_id,
                            "observed_input_limit_turn_local_compaction",
                        )?;
                        self.append_agent_status_text_to_terminal_buffer(
                            pane_id,
                            "agent: observed input recovery applied turn-local model summary; raw transcript remains authoritative",
                        )?;
                        return Ok(());
                    }
                    self.persist_agent_compaction_epoch(
                        pane_id,
                        &task,
                        &final_summary,
                        projection.as_ref(),
                        Some((&context, plan.as_ref())),
                    )?;
                    self.agent_turn_contexts_mut()
                        .insert(turn_id.clone(), compacted.clone());
                    if self.refresh_running_turn_context_after_conversation_compaction(&turn_id)? {
                        self.clear_agent_turn_provider_request_chain(&turn_id);
                        self.queue_agent_provider_recovery_task_after_compaction(
                            &turn_id,
                            "observed_input_limit_compaction",
                        )?;
                        self.append_agent_status_text_to_terminal_buffer(
                            pane_id,
                            &format!(
                                "agent: observed execution input compaction applied durable model summary observed_input_tokens={} compacted_blocks={}",
                                match trigger {
                                    RuntimeActiveTurnCompactionTrigger::ObservedInputLimit { observed_input_tokens, .. } => observed_input_tokens,
                                    RuntimeActiveTurnCompactionTrigger::ProviderContextLimit { .. } => unreachable!("configured compaction trigger was matched above"),
                                },
                                report.compacted_blocks
                            ),
                        )?;
                        self.append_agent_trace_turn_event(
                            pane_id,
                            &turn_id,
                            "provider_request recovery_resuming reason=observed_input_limit_compaction_completed",
                        )?;
                        self.restore_agent_latest_request_usage(&task.conversation_id, None);
                        self.restore_agent_context_usage(&task.conversation_id, None, None);
                        self.checkpoint_agent_session_metadata()?;
                        return Ok(());
                    }
                }
                self.agent_turn_contexts_mut()
                    .insert(turn_id.clone(), compacted);
                self.clear_agent_turn_provider_request_chain(&turn_id);
                match trigger {
                    RuntimeActiveTurnCompactionTrigger::ProviderContextLimit { .. } => {
                        self.queue_agent_provider_recovery_task_after_context_compaction(
                            &turn_id,
                            recovery_attempt,
                        )?;
                    }
                    RuntimeActiveTurnCompactionTrigger::ObservedInputLimit { .. } => {
                        self.queue_agent_provider_recovery_task_after_compaction(
                            &turn_id,
                            "observed_input_limit_compaction",
                        )?;
                    }
                }
                let (status, trace) = match trigger {
                    RuntimeActiveTurnCompactionTrigger::ProviderContextLimit { .. } => (
                        format!(
                            "agent: provider context recovery applied model summary compacted_blocks={}",
                            report.compacted_blocks
                        ),
                        "provider_request recovery_resuming reason=provider_context_limit_compaction_completed",
                    ),
                    RuntimeActiveTurnCompactionTrigger::ObservedInputLimit {
                        observed_input_tokens,
                        ..
                    } => (
                        format!(
                            "agent: observed execution input compaction applied model summary observed_input_tokens={observed_input_tokens} compacted_blocks={}",
                            report.compacted_blocks
                        ),
                        "provider_request recovery_resuming reason=observed_input_limit_compaction_completed",
                    ),
                };
                self.append_agent_status_text_to_terminal_buffer(pane_id, &status)?;
                self.append_agent_trace_turn_event(pane_id, &turn_id, trace)?;
                self.restore_agent_latest_request_usage(&task.conversation_id, None);
                self.restore_agent_context_usage(&task.conversation_id, None, None);
                self.checkpoint_agent_session_metadata()?;
                return Ok(());
            }
            if matches!(task.target, RuntimeAgentCompactionTarget::Conversation)
                && task.candidate_context.is_some()
            {
                let (candidate_tokens, input_limit, diagnostic) =
                    self.validate_manual_compaction_candidate(&task, &summary)?;
                if candidate_tokens > input_limit {
                    let summary_tokens = mez_agent::provider_text_input_token_estimate(&summary);
                    let next_budget = summary_tokens
                        .saturating_sub(candidate_tokens.saturating_sub(input_limit))
                        .min(summary_tokens / 2);
                    let (previous, attempts) = task.manual_final_retry.unwrap_or((usize::MAX, 0));
                    if next_budget > 0
                        && task
                            .request
                            .max_output_tokens
                            .is_none_or(|ceiling| next_budget < ceiling)
                        && candidate_tokens < previous
                    {
                        let source = task.manual_retry_source.clone().ok_or_else(|| {
                            MezError::invalid_state("manual compaction retry source is unavailable")
                        })?;
                        runtime_prepare_conversation_compaction_chunks(&mut task)?;
                        runtime_rebuild_conversation_compaction_request(&mut task, source)?;
                        if let Some(chunks) = task.conversation_chunks.as_mut() {
                            chunks.summaries.clear();
                            chunks.pending.clear();
                        }
                        task.manual_final_retry =
                            Some((candidate_tokens, attempts.saturating_add(1)));
                        task.request.max_output_tokens = Some(next_budget);
                        self.queue_agent_compaction_task(task.clone());
                        return Ok(());
                    }
                    return Err(MezError::invalid_state(format!(
                        "manual compaction candidate exceeds safe input allowance: {}",
                        diagnostic,
                    )));
                }
            }
            self.persist_agent_compaction_epoch(pane_id, &task, &summary, None, None)?;
            if let Some(resume_turn_id) = task.resume_turn_id.as_deref() {
                let refreshed = self
                    .refresh_running_turn_context_after_conversation_compaction(resume_turn_id)?;
                if refreshed {
                    let recovery_reason = match task.source.as_str() {
                        "provider-output-limit" => "output_limit_compaction",
                        "observed-input-limit" => "observed_input_limit_compaction",
                        "provider-context-limit" => "provider_context_limit_compaction",
                        _ => "conversation_compaction",
                    };
                    self.queue_agent_provider_recovery_task_after_compaction(
                        resume_turn_id,
                        recovery_reason,
                    )?;
                    self.append_agent_trace_turn_event(
                        pane_id,
                        resume_turn_id,
                        &format!(
                            "provider_request recovery_resuming reason={recovery_reason}_completed"
                        ),
                    )?;
                }
            }
            self.restore_agent_latest_request_usage(&task.conversation_id, None);
            self.restore_agent_context_usage(&task.conversation_id, None, None);
            self.checkpoint_agent_session_metadata()?;
            Ok(())
        })();
        if let Err(error) = application {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &format!(
                    "agent: compact failed while applying completion: {}",
                    error.message()
                ),
            )?;
            if let Some(resume_turn_id) = task.resume_turn_id.as_deref() {
                self.fail_running_turn_after_compaction_failure(
                    resume_turn_id,
                    &task.source,
                    error.message(),
                )?;
            }
        }
        Ok(true)
    }

    /// Validates the full refreshed request, including transcript entries added
    /// while observed-input compaction was running, before the durable epoch is committed.
    #[allow(clippy::too_many_arguments)]
    fn validate_observed_input_compaction_refresh_candidate(
        &mut self,
        task: &RuntimeAgentCompactionTask,
        turn: &AgentTurnRecord,
        compacted: AgentContext,
        summary: &str,
        projection: Option<AgentCompactionEpoch>,
        observed_input_tokens: u64,
        model_profile: &ModelProfile,
        provider_options: &std::collections::BTreeMap<String, String>,
        api: ProviderApiCompatibility,
        stream: bool,
    ) -> Result<(usize, usize, String)> {
        let memory_id =
            mez_agent::memory::canonical_memory_uuid(&format!("compact-{}", task.conversation_id));
        let summary_block = ContextBlock::reference_event(
            ContextSourceKind::Memory,
            format!("memory {memory_id} (conversation)"),
            runtime_model_compact_memory_content(
                &task.pane_id,
                &task.conversation_id,
                task.transcript_entries,
                task.summarized_entries,
                &task.model_profile_name,
                model_profile,
                summary,
            ),
        );
        let mut mcp_epoch_blocks = Vec::new();
        if let Some(catalog) = mez_agent::configured_mcp_catalog_snapshot_content(
            &self.mcp_registry().prompt_summary(),
            self.integration.always_exposed_mcp_servers(),
        ) {
            mcp_epoch_blocks.push(ContextBlock::reference_event(
                ContextSourceKind::McpCatalogSnapshot,
                mez_agent::MCP_CATALOG_SNAPSHOT_CONTEXT_LABEL,
                catalog,
            ));
        }
        mcp_epoch_blocks.push(ContextBlock {
            source: ContextSourceKind::Configuration,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "MCP compaction re-retrieval guidance".to_string(),
            content: "Conversation compaction cleared previously retrieved MCP tool contracts. Before using mcp_call, retrieve the needed server again with mcp_server_get; existing directory, reference, and search evidence may still make a server referencable.".to_string(),
        });
        let Some(candidate) = self.preview_running_turn_context_after_conversation_compaction(
            &turn.turn_id,
            compacted,
            summary_block,
            mcp_epoch_blocks,
            projection,
        )?
        else {
            return Err(MezError::invalid_state(
                "observed-input compaction refreshed context is unavailable",
            ));
        };
        let mcp_summary = self.mcp_registry().prompt_summary();
        let (prepared, available_mcp_tools) =
            self.prepare_agent_turn_model_context(turn, candidate, &mcp_summary, model_profile)?;
        let mut request =
            assemble_model_request(model_profile, api, turn, &prepared.to_agent_context())?;
        let (allowed_actions, interaction_kind) =
            self.agent_provider_request_control_for_turn(turn)?;
        mez_agent::apply_model_request_control(&mut request, allowed_actions, interaction_kind);
        mez_agent::apply_default_action_gates(
            &mut request,
            &available_mcp_tools,
            self.runtime_persistent_memory_enabled(),
            super::runtime_issues_enabled(self),
        );
        let estimate =
            mez_agent::provider_request_input_estimate(&request, api, provider_options, stream)?;
        let safe_limit = runtime_compaction_safe_input_limit(
            model_profile.max_input_tokens(),
            model_profile.context_window_tokens(),
            model_profile.max_output_tokens(),
        )
        .ok_or_else(|| {
            MezError::invalid_state(
                "active-turn compaction refresh has no configured provider input allowance",
            )
        })?;
        let estimated_input_tokens = u64::try_from(estimate.input_tokens).unwrap_or(u64::MAX);
        if estimate.input_tokens <= safe_limit && estimated_input_tokens >= observed_input_tokens {
            return Err(MezError::invalid_state(format!(
                "observed-input compaction refreshed candidate did not reduce the triggering request: estimated_input_tokens={estimated_input_tokens} observed_input_tokens={observed_input_tokens}",
            )));
        }
        let diagnostic = runtime_compaction_candidate_size_diagnostic(
            &prepared.to_agent_context(),
            Some(mez_agent::ProviderBudgetProjection::new(
                api,
                &model_profile.provider,
            )),
            estimate.input_tokens,
            safe_limit,
        );
        Ok((estimate.input_tokens, safe_limit, diagnostic))
    }

    /// Validates the provider request that would follow a manual compaction.
    ///
    /// This runs before durable epoch mutation. It reconstructs the active
    /// summary, retained raw tail (including entries written while the
    /// compactor was running), current MCP catalog, and accepted steering.
    fn validate_manual_compaction_candidate(
        &mut self,
        task: &RuntimeAgentCompactionTask,
        summary: &str,
    ) -> Result<(usize, usize, String)> {
        let summary_words = model_context_text_word_count(summary);
        let summary_budget_words = task.request.max_output_tokens.unwrap_or(usize::MAX);
        if summary_words > summary_budget_words {
            return Err(MezError::invalid_state(format!(
                "manual compaction summary exceeds its frozen output budget: summary_words={summary_words} summary_budget_words={summary_budget_words}"
            )));
        }
        let Some(base_context) = task.candidate_context.as_ref() else {
            return Err(MezError::invalid_state(
                "manual compaction candidate context is unavailable",
            ));
        };
        let session = self
            .agent_shell_store()
            .get(&task.pane_id)
            .filter(|session| session.session_id == task.conversation_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("manual compaction session is unavailable"))?;
        let store = self.persistence.cloned_transcript_store().ok_or_else(|| {
            MezError::invalid_state("manual compaction candidate requires transcript storage")
        })?;
        let pending = self
            .persistence
            .pending_transcript_entries(&task.conversation_id);
        let entries = store
            .conversation_transcript_view(
                &task.conversation_id,
                crate::storage::transcript::ConversationTranscriptRead::All,
                task.transcript_entries > pending.len() as u64,
                &pending,
            )?
            .logical;
        let post_plan_entries = session
            .transcript_entries
            .saturating_sub(task.transcript_entries);
        let retained_count = task
            .retained_transcript_entries
            .saturating_add(post_plan_entries);
        let retained_count = usize::try_from(retained_count).unwrap_or(usize::MAX);
        let first_retained = entries.len().saturating_sub(retained_count);
        let retained_entries = &entries[first_retained..];
        let compact_memory_id =
            mez_agent::memory::canonical_memory_uuid(&format!("compact-{}", task.conversation_id));
        let mut blocks = base_context
            .blocks()
            .iter()
            .filter(|block| {
                !(block.source == ContextSourceKind::Memory
                    && block
                        .label
                        .starts_with(&format!("memory {compact_memory_id} ")))
                    && !(block.source == ContextSourceKind::UserInstruction
                        && block.label == "user prompt")
                    && block.source != ContextSourceKind::McpCatalogSnapshot
                    && !runtime_context_block_is_transcript_replay(block)
            })
            .cloned()
            .collect::<Vec<_>>();
        blocks.push(ContextBlock::reference_event(
            ContextSourceKind::Memory,
            format!("memory {compact_memory_id} (conversation)"),
            runtime_model_compact_memory_content(
                &task.pane_id,
                &task.conversation_id,
                task.transcript_entries,
                task.summarized_entries,
                &task.model_profile_name,
                &task.model_profile,
                summary,
            ),
        ));
        blocks.extend(
            runtime_agent_transcript_context_blocks(&task.pane_id, retained_entries)
                .into_iter()
                .filter(|block| block.source != ContextSourceKind::McpRetrievedManifest),
        );
        let steering = self.agent_compaction_steering_for_candidate(
            &task.pane_id,
            &task.conversation_id,
            task.compaction_epoch,
        );
        let prompt = if steering.is_empty() {
            "Continue after conversation compaction.".to_string()
        } else {
            steering.join("\n\n")
        };
        for block in mez_agent::mcp_server_reference_blocks_for_prompt(
            &prompt,
            &self.mcp_registry().prompt_summary(),
        ) {
            mez_agent::insert_context_block_by_placement(&mut blocks, block);
        }
        blocks.push(ContextBlock::user_event("user prompt", prompt));
        if let Some(catalog) = mez_agent::configured_mcp_catalog_snapshot_content(
            &self.mcp_registry().prompt_summary(),
            self.integration.always_exposed_mcp_servers(),
        ) {
            blocks.push(ContextBlock::reference_event(
                ContextSourceKind::McpCatalogSnapshot,
                mez_agent::MCP_CATALOG_SNAPSHOT_CONTEXT_LABEL,
                catalog,
            ));
        }
        blocks.push(ContextBlock {
            source: ContextSourceKind::Configuration,
            placement: mez_agent::ContextPlacement::ConversationAppend,
            label: "MCP compaction re-retrieval guidance".to_string(),
            content: "Conversation compaction cleared previously retrieved MCP tool contracts. Before using mcp_call, retrieve the needed server again with mcp_server_get; existing directory, reference, and search evidence may still make a server referencable.".to_string(),
        });
        let candidate_context = AgentContext::import_durable_blocks(blocks)?
            .with_metadata(base_context.metadata().clone());
        let mcp_summary = self.mcp_registry().prompt_summary();
        let configured_servers = self.integration.always_exposed_mcp_servers().to_vec();
        let candidate_context = mez_agent::append_mcp_context_with_configured(
            candidate_context,
            &mcp_summary,
            &configured_servers,
        )?;
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
            super::runtime_issues_enabled(self),
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

    /// Persists one compacted conversation epoch and removes its summarized raw replay prefix.
    fn persist_agent_compaction_epoch(
        &mut self,
        pane_id: &str,
        task: &RuntimeAgentCompactionTask,
        summary: &str,
        projection: Option<&AgentCompactionEpoch>,
        final_range: Option<(&AgentContext, &mez_agent::ModelContextCompactionPlan)>,
    ) -> Result<()> {
        let content = runtime_model_compact_memory_content(
            pane_id,
            &task.conversation_id,
            task.transcript_entries,
            task.summarized_entries,
            &task.model_profile_name,
            &task.model_profile,
            summary,
        );
        if let Some(store) = self.persistence.cloned_transcript_store() {
            let previous = store.compaction_epoch(&task.conversation_id)?;
            let previous_boundary = previous.as_ref().map_or(0, |epoch| epoch.through_sequence);
            let boundary = task.compacted_through_sequence.unwrap_or(previous_boundary);
            if let Some(projection) = projection {
                let prior_ranges = previous
                    .as_ref()
                    .map_or(&[][..], |epoch| epoch.ranges.as_slice());
                if projection.through_sequence != boundary
                    || previous
                        .as_ref()
                        .map_or_else(String::new, |epoch| epoch.summary.clone())
                        != projection.summary
                    || !projection.ranges.starts_with(prior_ranges)
                    || projection.ranges.len() <= prior_ranges.len()
                {
                    return Err(MezError::invalid_state(
                        "selective compaction boundary changed",
                    ));
                }
                let (context, plan) = final_range.ok_or_else(|| {
                    MezError::invalid_state("selective compaction source proof is unavailable")
                })?;
                let frozen =
                    self.frozen_observed_compaction_rows(task, context, plan, summary, projection)?;
                store.save_compaction_ranges_with_proof(projection.clone(), &frozen)?;
            } else if let Some(previous) = previous.filter(|epoch| !epoch.ranges.is_empty()) {
                if previous.ranges.iter().any(|range| {
                    range.first_sequence <= boundary && boundary < range.through_sequence
                }) {
                    return Err(MezError::invalid_state(
                        "prefix compaction cannot split a selective execution range",
                    ));
                }
                let remaining = previous
                    .ranges
                    .into_iter()
                    .filter(|range| range.first_sequence > boundary)
                    .collect::<Vec<_>>();
                if remaining.is_empty() {
                    store.save_compaction_epoch(&task.conversation_id, boundary, &content)?;
                } else {
                    store.save_compaction_ranges(
                        &task.conversation_id,
                        boundary,
                        &content,
                        remaining,
                    )?;
                }
            } else {
                store.save_compaction_epoch(&task.conversation_id, boundary, &content)?;
            }
        } else if task.compacted_through_sequence.is_some() {
            return Err(MezError::invalid_state(
                "durable compaction requires a transcript store",
            ));
        }
        let mcp_catalog_snapshot = mez_agent::configured_mcp_catalog_snapshot_content(
            &self.mcp_registry().prompt_summary(),
            self.integration.always_exposed_mcp_servers(),
        );
        let post_plan_transcript_entries = self
            .agent_shell_store()
            .get(pane_id)
            .filter(|session| session.session_id == task.conversation_id)
            .map(|session| {
                session
                    .transcript_entries
                    .saturating_sub(task.transcript_entries)
            })
            .unwrap_or_default();
        let mcp_epoch_entries = self.persist_mcp_compaction_epoch_transcript(
            pane_id,
            &task.conversation_id,
            mcp_catalog_snapshot,
        )?;
        let remaining_transcript_entries = self
            .agent_shell_store_mut()
            .retain_recent_transcript_entries(
                pane_id,
                task.retained_transcript_entries
                    .saturating_add(post_plan_transcript_entries)
                    .saturating_add(mcp_epoch_entries as u64),
            )?
            .transcript_entries;
        let now = current_unix_seconds().max(1);
        let memory_id =
            mez_agent::memory::canonical_memory_uuid(&format!("compact-{}", task.conversation_id));
        // Optional memory is a projection, not the source of continuity.
        let _ = self.upsert_session_memory(MemoryRecord::new_with_defaults(
            memory_id.clone(),
            MemoryScope::Pane {
                session_id: self.session.id.to_string(),
                pane_id: pane_id.to_string(),
            },
            now,
            now,
            MemorySource::Agent,
            224,
            content,
        ));
        self.clear_agent_conversation_provider_request_chain(&task.conversation_id);
        self.append_agent_status_text_to_terminal_buffer(
            pane_id,
            &format!(
                "agent: compacted conversation summary memory_id={} summarized_entries={} remaining_transcript_entries={} source=model-compact trigger={}",
                memory_id, task.summarized_entries, remaining_transcript_entries, task.source
            ),
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
        if let Some(mut task) = self.finish_agent_compaction_task(pane_id, task_generation) {
            let retry_class =
                provider_error_retry_class_from_parts(parsed_kind, message, provider_failure_json);
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
                // Before splitting all selected source into temporary chunks, try
                // retaining the newest complete group exactly. This is useful only
                // when the resulting *full* provider request can still fit; the
                // excluded group is never silently dropped from the live context.
                let walkback = match &task.target {
                    RuntimeAgentCompactionTarget::ActiveTurn {
                        turn_id,
                        plan,
                        pending_blocks,
                        completed_summaries,
                        current_blocks,
                        ..
                    } if pending_blocks.is_empty()
                        && completed_summaries.is_empty()
                        && current_blocks.len() == plan.replacement_blocks().len() =>
                    {
                        Some((turn_id.clone(), plan.as_ref().clone()))
                    }
                    _ => None,
                };
                if let Some((turn_id, mut candidate)) = walkback {
                    while candidate.exclude_newest_replacement_group() {
                        let fits = match self
                            .compaction_walkback_candidate_fits(&task, &turn_id, &candidate)
                        {
                            Ok(fits) => fits,
                            Err(error) => {
                                self.fail_running_turn_after_compaction_failure(
                                    &turn_id,
                                    &task.source,
                                    error.message(),
                                )?;
                                let _ = self.append_agent_status_text_to_terminal_buffer(
                                    pane_id,
                                    &format!(
                                        "agent: compact walk-back validation failed: {}",
                                        error.message()
                                    ),
                                );
                                return Ok(true);
                            }
                        };
                        if !fits {
                            continue;
                        }
                        let blocks =
                            runtime_redact_compaction_blocks(candidate.replacement_blocks());
                        if let RuntimeAgentCompactionTarget::ActiveTurn { plan, .. } =
                            &mut task.target
                        {
                            **plan = candidate;
                        }
                        runtime_rebuild_active_turn_compaction_request(&mut task, blocks)?;
                        self.queue_agent_compaction_task(task.clone());
                        return Ok(true);
                    }
                }
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
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &format!("agent: compact failed during provider request: {message}"),
            )?;
            if let Some(resume_turn_id) = task.resume_turn_id.as_deref() {
                self.fail_running_turn_after_compaction_failure(
                    resume_turn_id,
                    &task.source,
                    message,
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
            "automatic {} compaction failed before provider retry: {message}",
            runtime_compaction_failure_source_label(source),
        ));
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

    /// Runs the inspect agent shell transcript for compaction operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn inspect_agent_shell_transcript_for_compaction(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<TranscriptEntry>> {
        let Some(store) = self.persistence.transcript_store() else {
            return Ok(Vec::new());
        };
        match store.inspect(conversation_id) {
            Ok(entries) => Ok(entries),
            Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }
}

/// Builds the provider request used for model-authored conversation compaction.
pub(super) fn runtime_model_compaction_request(
    profile: &ModelProfile,
    pane_id: &str,
    conversation_id: &str,
    transcript_entries: u64,
    entries: &[TranscriptEntry],
    context: &AgentContext,
    allowed_actions: AllowedActionSet,
) -> Result<ModelRequest> {
    let agent_id = format!("agent-{pane_id}");
    let turn_id = format!("compact-{conversation_id}");
    Ok(ModelRequest {
        provider: profile.provider.clone(),
        model: profile.model.clone(),
        model_capabilities: profile.model_capabilities.clone(),
        max_input_tokens: None,
        reasoning_effort: profile
            .provider_options
            .get("reasoning_effort")
            .cloned()
            .or_else(|| profile.reasoning_profile.clone()),
        thinking_enabled: profile.thinking_enabled(),
        prompt_cache_retention: profile.provider_options.get("prompt_cache_retention").cloned(),
        latency_preference: profile.latency_preference.clone(),
        max_output_tokens: profile.max_output_tokens(),
        temperature: None,
        stop: None,
        prompt_cache_session_id: None,
                prompt_cache_lineage_id: None,
        turn_id,
        agent_id,
        available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
                issue_actions_enabled: true,
        interaction_kind: ModelInteractionKind::Compaction,
        allowed_actions,
        messages: vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                placement: mez_agent::ContextPlacement::StablePrefix,
                content: "You are Mezzanine's conversation compactor. Produce durable, concise summaries that preserve task-critical context and omit secrets."
                    .to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Developer,
                source: ContextSourceKind::DeveloperInstruction,
                placement: mez_agent::ContextPlacement::StablePrefix,
                content: "Return exactly one `say` action with `status` set to `final` and `content_type` set to `text/markdown; charset=utf-8`. Summarize only essential user goals, current decisions and state, blockers, pending work, and usable references for the next turn. Omit audit identities, counts, repeated history, and unsupported completion claims. Redact credentials and secrets."
                    .to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::Transcript,
                placement: mez_agent::ContextPlacement::ConversationAppend,
                content: runtime_model_compaction_source(
                    pane_id,
                    conversation_id,
                    transcript_entries,
                    entries,
                    context,
                ),
            },
        ].into(),
    })
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
    task.request = runtime_model_compaction_request_for_blocks(
        &task.model_profile,
        &task.pane_id,
        &task.conversation_id,
        &blocks,
        task.request.allowed_actions.clone(),
    )?;
    runtime_limit_compaction_summary_output(
        &mut task.request,
        final_request_retry
            .summary_ceiling
            .unwrap_or(plan.summary_budget_words())
            .min(plan.summary_budget_words()),
    );
    *current_blocks = blocks;
    Ok(())
}

/// Bounds active-turn compactor output to the summary budget reserved by its
/// frozen replacement plan.
///
/// Configured input-cap retries tighten that plan after a non-reducing pass.
/// The model request must carry the same ceiling; otherwise a provider may keep
/// returning a profile-sized summary that is too large for the rebuilt request.
fn runtime_limit_compaction_summary_output(
    request: &mut ModelRequest,
    summary_budget_words: usize,
) {
    let bounded_budget = summary_budget_words.max(1);
    request.max_output_tokens = Some(
        request
            .max_output_tokens
            .unwrap_or(bounded_budget)
            .min(bounded_budget),
    );
}

/// Builds one compactor request from temporary active-turn source blocks.
fn runtime_model_compaction_request_for_blocks(
    profile: &ModelProfile,
    pane_id: &str,
    conversation_id: &str,
    blocks: &[ContextBlock],
    allowed_actions: AllowedActionSet,
) -> Result<ModelRequest> {
    let source_context = AgentContext::new_durable(blocks.to_vec())
        .map_err(|error| MezError::invalid_state(error.message()))?;
    runtime_model_compaction_request(
        profile,
        pane_id,
        conversation_id,
        0,
        &[],
        &source_context,
        allowed_actions,
    )
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

/// Formats bounded transcript source material for a model compaction request.
pub(super) fn runtime_model_compaction_source(
    pane_id: &str,
    conversation_id: &str,
    transcript_entries: u64,
    entries: &[TranscriptEntry],
    context: &AgentContext,
) -> String {
    let mut lines = vec![
        format!("Pane: {pane_id}"),
        format!("Conversation: {conversation_id}"),
        format!("Transcript entries before compaction: {transcript_entries}"),
        format!("Durable entries supplied for compaction: {}", entries.len()),
        format!(
            "Provider-bound context blocks supplied: {}",
            context.blocks().len()
        ),
    ];
    for (index, block) in context.blocks().iter().enumerate() {
        lines.push(format!(
            "context_block={} source={} label={} content={}",
            index,
            runtime_context_source_kind_name(block.source),
            json_escape(&block.label),
            runtime_model_compaction_entry_content(&block.content)
        ));
    }
    for entry in entries {
        lines.push(format!(
            "entry={} role={} turn={} pane={} content={}",
            entry.sequence,
            runtime_transcript_role_name(entry.role),
            entry.turn_id,
            entry.pane_id,
            runtime_model_compaction_entry_content(&entry.content)
        ));
    }
    lines.join("\n")
}

/// Returns a stable source label for context included in compaction input.
pub(super) fn runtime_context_source_kind_name(source: ContextSourceKind) -> &'static str {
    match source {
        ContextSourceKind::System => "system",
        ContextSourceKind::UserInstruction => "user-instruction",
        ContextSourceKind::SkillInstruction => "skill-instruction",
        ContextSourceKind::DeveloperInstruction => "developer-instruction",
        ContextSourceKind::Policy => "policy",
        ContextSourceKind::Configuration => "configuration",
        ContextSourceKind::LocalMessage => "local-message",
        ContextSourceKind::PeerMessage => "peer-message",
        ContextSourceKind::RuntimeHint => "runtime-hint",
        ContextSourceKind::ProjectGuidance => "project-guidance",
        ContextSourceKind::Memory => "memory",
        ContextSourceKind::PersistedContextDocument => "persisted-context-document",
        ContextSourceKind::Transcript => "transcript",
        ContextSourceKind::TranscriptUser => "transcript-user",
        ContextSourceKind::TranscriptAssistant => "transcript-assistant",
        ContextSourceKind::TranscriptTool => "transcript-tool",
        ContextSourceKind::CommittedEvidence => "committed-evidence",
        ContextSourceKind::RoutedHandoff => "routed-handoff",
        ContextSourceKind::ActionResult => "action-result",
        ContextSourceKind::McpCatalogSnapshot => "mcp-catalog-snapshot",
        ContextSourceKind::McpServerReference => "mcp-server-reference",
        ContextSourceKind::McpServerSearchResult => "mcp-server-search-result",
        ContextSourceKind::McpRetrievedManifest => "mcp-retrieved-manifest",
    }
}

/// Redacts one transcript entry before sending it for compaction while
/// preserving its structural whitespace for model interpretation.
pub(super) fn runtime_model_compaction_entry_content(content: &str) -> String {
    content
        .split_inclusive(char::is_whitespace)
        .map(|segment| {
            let token = segment.trim_end_matches(char::is_whitespace);
            let whitespace = &segment[token.len()..];
            format!(
                "{}{}",
                runtime_compact_redact_sensitive_token(token),
                whitespace
            )
        })
        .collect()
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

/// Extracts the model-authored markdown summary from a compaction response.
pub(super) fn runtime_model_compaction_summary_from_response(
    response: &ModelResponse,
) -> Result<String> {
    let summary = response
        .action_batch
        .as_ref()
        .and_then(|batch| {
            batch.actions.iter().find_map(|action| {
                if let AgentActionPayload::Say { text, .. } = &action.payload {
                    Some(text.trim().to_string())
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| response.raw_text.trim().to_string());
    if summary.trim().is_empty() {
        return Err(MezError::invalid_state(
            "model compaction response did not contain a summary",
        ));
    }
    Ok(summary)
}

/// Formats the durable memory record stored after model-authored compaction.
pub(super) fn runtime_model_compact_memory_content(
    _pane_id: &str,
    _conversation_id: &str,
    _transcript_entries: u64,
    _summarized_entries: usize,
    _model_profile_name: &str,
    _profile: &ModelProfile,
    summary: &str,
) -> String {
    [
        "Older durable transcript entries were summarized; this summary is lossy and only the retained recent raw tail is exact. Reinspect source when exact details matter.".to_string(),
        summary.trim().to_string(),
    ]
    .join("\n")
}

/// Removes raw transcript replay blocks from the context supplied to the model
/// compactor so the retained tail is not summarized a second time.
///
/// # Parameters
/// - `context`: The provider context assembled for the compaction turn.
pub(super) fn runtime_compaction_context_without_transcript_blocks(
    mut context: AgentContext,
) -> Result<AgentContext> {
    context.retain_blocks(|block| !runtime_context_block_is_transcript_replay(block))?;
    Ok(context.revalidate()?)
}

/// Returns true when a context block is raw transcript replay.
///
/// # Parameters
/// - `block`: The context block being classified.
pub(super) fn runtime_context_block_is_transcript_replay(block: &ContextBlock) -> bool {
    matches!(
        block.source,
        ContextSourceKind::Transcript
            | ContextSourceKind::TranscriptUser
            | ContextSourceKind::TranscriptAssistant
            | ContextSourceKind::TranscriptTool
    )
}

/// Returns how many recent durable transcript entries should remain in raw
/// replay after a compaction summary is stored.
///
/// # Parameters
/// - `transcript_entries`: The active raw replay count before compaction.
/// - `durable_entries`: The durable transcript entries found for the
///   conversation.
/// - `context_budget_words`: The estimated context budget for the active model.
/// - `retained_tail_percent`: The model-context percentage reserved for raw replay.
pub(super) fn runtime_compact_retained_transcript_entries(
    transcript_entries: u64,
    durable_entries: &[TranscriptEntry],
    context_budget_words: usize,
    retained_tail_percent: usize,
) -> u64 {
    let active_count =
        runtime_compact_active_transcript_entry_count(transcript_entries, durable_entries.len());
    if active_count == 0 {
        return 0;
    }
    let active_entries = &durable_entries[durable_entries.len() - active_count..];
    let groups = runtime_compact_transcript_execution_groups(active_entries);
    if groups.is_empty() {
        return 0;
    }
    let closed_group_count = groups
        .iter()
        .position(|group| {
            !active_entries[group.clone()]
                .iter()
                .any(|entry| entry.role == TranscriptRole::Assistant)
        })
        .unwrap_or(groups.len());
    let tail_budget = runtime_compact_retained_context_tail_budget_words(
        context_budget_words,
        retained_tail_percent,
    );
    let mut retained_words = 0usize;
    let mut tail_start = active_count;
    for (group_index, group) in groups.iter().enumerate().rev() {
        let group_words = active_entries[group.clone()]
            .iter()
            .map(runtime_compact_transcript_entry_context_words)
            .fold(0usize, usize::saturating_add);
        let must_retain = group_index >= closed_group_count;
        if !must_retain && retained_words.saturating_add(group_words) >= tail_budget {
            break;
        }
        retained_words = retained_words.saturating_add(group_words);
        tail_start = group.start;
    }
    let retained_entries = active_count.saturating_sub(tail_start);
    u64::try_from(retained_entries).unwrap_or(u64::MAX)
}

/// Estimates the context words in the raw suffix that would remain after
/// compaction. This is used to distinguish a fitting tail from exact unfinished
/// transcript material that cannot be safely summarized as a complete group.
fn runtime_compact_retained_transcript_tail_context_words(
    transcript_entries: u64,
    durable_entries: &[TranscriptEntry],
    retained_transcript_entries: u64,
) -> usize {
    let active_count =
        runtime_compact_active_transcript_entry_count(transcript_entries, durable_entries.len());
    let active_start = durable_entries.len().saturating_sub(active_count);
    let retained_count = usize::try_from(retained_transcript_entries)
        .unwrap_or(usize::MAX)
        .min(active_count);
    let retained_start = active_start.saturating_add(active_count.saturating_sub(retained_count));
    durable_entries[retained_start..active_start.saturating_add(active_count)]
        .iter()
        .map(runtime_compact_transcript_entry_context_words)
        .fold(0usize, usize::saturating_add)
}

/// Returns the raw tail count for an explicit user-forced compaction.
///
/// Manual `/compact` is an explicit request to compact, so it should not skip
/// only because all active entries currently fit inside the retained-tail
/// budget. Keep the normal budget-derived tail when it already leaves a prefix
/// to summarize, otherwise shrink the tail enough to summarize at least one
/// active durable entry.
///
/// # Parameters
/// - `transcript_entries`: The active raw replay count before compaction.
/// - `durable_entries`: The durable transcript entries found for the
///   conversation.
/// - `context_budget_words`: The estimated context budget for the active model.
/// - `retained_tail_percent`: The model-context percentage reserved for raw replay.
pub(super) fn runtime_compact_forced_retained_transcript_entries(
    transcript_entries: u64,
    durable_entries: &[TranscriptEntry],
    context_budget_words: usize,
    retained_tail_percent: usize,
) -> u64 {
    let retained = runtime_compact_retained_transcript_entries(
        transcript_entries,
        durable_entries,
        context_budget_words,
        retained_tail_percent,
    );
    let active_count =
        runtime_compact_active_transcript_entry_count(transcript_entries, durable_entries.len());
    if active_count == 0 {
        return 0;
    }
    if !runtime_compact_transcript_entries_for_summary(
        transcript_entries,
        durable_entries,
        retained,
    )
    .is_empty()
    {
        return retained;
    }
    let active_entries = &durable_entries[durable_entries.len() - active_count..];
    let groups = runtime_compact_transcript_execution_groups(active_entries);
    let Some(first_closed_group) = groups.iter().find(|group| {
        active_entries[(*group).clone()]
            .iter()
            .any(|entry| entry.role == TranscriptRole::Assistant)
    }) else {
        return u64::try_from(active_count).unwrap_or(u64::MAX);
    };
    u64::try_from(active_count.saturating_sub(first_closed_group.end)).unwrap_or(u64::MAX)
}

/// Returns the active transcript entry count represented by the current shell
/// session and durable store.
///
/// # Parameters
/// - `transcript_entries`: The active raw replay count before compaction.
/// - `durable_entries`: The number of durable transcript entries found.
pub(super) fn runtime_compact_active_transcript_entry_count(
    transcript_entries: u64,
    durable_entries: usize,
) -> usize {
    usize::try_from(transcript_entries)
        .unwrap_or(usize::MAX)
        .min(durable_entries)
}

/// Returns the durable transcript prefix that should be summarized, excluding
/// the exact raw tail retained for future turns.
///
/// # Parameters
/// - `transcript_entries`: The active raw replay count before compaction.
/// - `durable_entries`: The durable transcript entries found for the
///   conversation.
/// - `retained_transcript_entries`: The retained raw tail count.
pub(super) fn runtime_compact_transcript_entries_for_summary(
    transcript_entries: u64,
    durable_entries: &[TranscriptEntry],
    retained_transcript_entries: u64,
) -> &[TranscriptEntry] {
    let active_count =
        runtime_compact_active_transcript_entry_count(transcript_entries, durable_entries.len());
    let retained_count = usize::try_from(retained_transcript_entries)
        .unwrap_or(usize::MAX)
        .min(active_count);
    let active_start = durable_entries.len().saturating_sub(active_count);
    let active_entries = &durable_entries[active_start..];
    let requested_compactable_count = active_count.saturating_sub(retained_count);
    let compactable_count = runtime_compact_transcript_execution_groups(active_entries)
        .into_iter()
        .take_while(|group| group.end <= requested_compactable_count)
        .take_while(|group| {
            runtime_compaction_group_is_summarizable(&active_entries[group.clone()])
        })
        .map(|group| group.end)
        .last()
        .unwrap_or(0);
    &active_entries[..compactable_count]
}

/// Returns whether one complete transcript group can enter a compacted prefix.
///
/// Ordinary groups require an assistant response. The dedicated MCP epoch and
/// compact directory snapshot are durable control metadata that replace older
/// MCP authority at each compaction boundary, so they may be summarized as one
/// intact group during a subsequent compaction.
fn runtime_compaction_group_is_summarizable(entries: &[TranscriptEntry]) -> bool {
    entries
        .iter()
        .any(|entry| entry.role == TranscriptRole::Assistant)
        || entries.iter().all(|entry| {
            if entry.role != TranscriptRole::System {
                return false;
            }
            match mez_agent::TranscriptContextEvent::from_transcript_content(&entry.content) {
                Some(mez_agent::TranscriptContextEvent::McpCompactionEpoch)
                | Some(mez_agent::TranscriptContextEvent::McpCatalogSnapshot { .. }) => true,
                Some(mez_agent::TranscriptContextEvent::PromptBoundary {
                    source: ContextSourceKind::Configuration,
                    label,
                    ..
                }) => label == "MCP compaction re-retrieval guidance",
                _ => false,
            }
        })
}

/// Returns contiguous durable execution groups keyed by stable turn id.
///
/// All request messages, provider-native events, assistant output, and action
/// results emitted by terminal persistence share one turn id. Grouping by that
/// identity prevents compaction from separating any of those records.
fn runtime_compact_transcript_execution_groups(
    entries: &[TranscriptEntry],
) -> Vec<std::ops::Range<usize>> {
    let mut groups = Vec::new();
    let mut start = 0usize;
    for index in 1..entries.len() {
        if entries[index].turn_id != entries[start].turn_id {
            groups.push(start..index);
            start = index;
        }
    }
    if start < entries.len() {
        groups.push(start..entries.len());
    }
    groups
}

/// Returns the word budget reserved for retained exact transcript replay.
///
/// # Parameters
/// - `context_budget_words`: The estimated context budget for the active model.
/// - `retained_tail_percent`: The model-context percentage reserved for raw replay.
pub(super) fn runtime_compact_retained_context_tail_budget_words(
    context_budget_words: usize,
    retained_tail_percent: usize,
) -> usize {
    context_budget_words
        .saturating_mul(runtime_compact_retained_tail_percent(retained_tail_percent))
        .saturating_div(100)
        .max(1)
}

/// Normalizes retained-tail percentages for defensive runtime callers.
pub(super) fn runtime_compact_retained_tail_percent(retained_tail_percent: usize) -> usize {
    retained_tail_percent.clamp(1, 100)
}

/// Estimates one transcript entry's provider-context footprint.
///
/// # Parameters
/// - `entry`: The transcript entry being estimated.
pub(super) fn runtime_compact_transcript_entry_context_words(entry: &TranscriptEntry) -> usize {
    AGENT_COMPACT_TRANSCRIPT_ENTRY_CONTEXT_OVERHEAD_WORDS.saturating_add(
        mez_agent::provider_text_input_token_estimate(&entry.content),
    )
}

/// Runs the runtime compact redact sensitive token operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_compact_redact_sensitive_token(token: &str) -> String {
    let lower = token.to_ascii_lowercase();
    if lower.contains("private")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower == "api"
        || lower.contains("password")
        || lower.contains("token")
        || lower.contains("credential")
        || lower.contains("secret")
        || token.contains("sk-")
        || token.contains('@') && token.contains('.')
    {
        "[redacted]".to_string()
    } else {
        token.to_string()
    }
}

/// Runs the runtime transcript role name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_transcript_role_name(role: TranscriptRole) -> &'static str {
    match role {
        TranscriptRole::User => "user",
        TranscriptRole::Assistant => "assistant",
        TranscriptRole::Tool => "tool",
        TranscriptRole::System => "system",
    }
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
