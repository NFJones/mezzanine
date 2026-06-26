//! Agent-shell conversation compaction commands and provider request helpers.
//!
//! This module owns `/compact`, queued model-backed compaction tasks,
//! compaction provider request construction, durable summary extraction,
//! and retained transcript-tail calculations. Keeping compaction isolated
//! avoids mixing long-running summarization workflow with ordinary shell
//! command dispatch.

use super::*;
use crate::agent::anthropic_provider_from_auth_store_with_provider_options;

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
            let session = self.agent_shell_store.get(pane_id).ok_or_else(|| {
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
        if self.agent_compacting_panes.contains_key(pane_id) {
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
        let retained_tail_percent = self.agent_compaction_raw_retention_percent;
        let retained_transcript_entries = if source == "manual" || resume_turn_id.is_some() {
            runtime_compact_forced_retained_transcript_entries(
                transcript_entries,
                &transcript_records,
                model_profile.context_window_budget_words(),
                retained_tail_percent,
            )
        } else {
            runtime_compact_retained_transcript_entries(
                transcript_entries,
                &transcript_records,
                model_profile.context_window_budget_words(),
                retained_tail_percent,
            )
        };
        let compactable_transcript_records = runtime_compact_transcript_entries_for_summary(
            transcript_entries,
            &transcript_records,
            retained_transcript_entries,
        );
        if compactable_transcript_records.is_empty() {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                "agent: compact skipped; recent transcript tail already fits the active context budget",
            )?;
            return Ok(AgentShellCommandOutcome::Display {
                command: "compact".to_string(),
                body: format!(
                    "pane={} conversation={} previous_transcript_entries={} summarized_entries=0 remaining_transcript_entries={} compacted=false reason=within-retained-context-tail retained_context_tail_percent={} source=model-compact trigger={}",
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
        let mcp_summary = self.mcp_registry.prompt_summary();
        let compaction_context = runtime_compaction_context_without_transcript_blocks(
            append_mcp_context(compaction_context, &mcp_summary)?,
        )?;
        let summarized_entries = compactable_transcript_records.len();
        let request = runtime_model_compaction_request(
            &model_profile,
            pane_id,
            &conversation_id,
            transcript_entries,
            compactable_transcript_records,
            &compaction_context,
        )?;
        self.agent_compacting_panes
            .insert(pane_id.to_string(), current_unix_seconds().max(1));
        self.pending_agent_compaction_tasks.insert(
            pane_id.to_string(),
            RuntimeAgentCompactionTask {
                pane_id: pane_id.to_string(),
                conversation_id: conversation_id.clone(),
                source: source.to_string(),
                transcript_entries,
                retained_transcript_entries,
                summarized_entries,
                model_profile_name: model_profile_name.clone(),
                model_profile: model_profile.clone(),
                request,
                resume_turn_id: resume_turn_id.map(str::to_string),
            },
        );
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

    /// Executes `/compact` by asking the active model to produce the durable
    /// conversation summary that replaces older transcript context.
    pub(super) async fn execute_agent_shell_compact_command_async(
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

    /// Queues internal output-limit recovery compaction for a running turn.
    ///
    /// Provider `max_output_tokens` exhaustion can leave a running turn with a
    /// request shape that repeatedly burns its output budget before producing a
    /// valid MAAP batch. This helper uses the same model-backed conversation
    /// compactor as `/compact`, but it is runtime-owned and resumes the active
    /// turn after the compacted memory is written.
    pub(crate) fn queue_agent_output_limit_recovery_compaction(
        &mut self,
        agent_id: &AgentId,
        turn_id: &str,
        error: &MezError,
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
                "agent provider recovery agent id does not match turn",
            ));
        }
        if turn.state != AgentTurnState::Running {
            self.pending_agent_provider_tasks.remove(turn_id);
            return Ok(false);
        }
        if self.agent_compacting_panes.contains_key(&turn.pane_id) {
            return Ok(false);
        }
        let outcome = self.queue_agent_shell_compaction_with_model(
            &turn.pane_id,
            "provider-output-limit",
            Some(turn_id),
        )?;
        self.pending_agent_provider_tasks.remove(turn_id);
        self.append_agent_status_text_to_terminal_buffer(
            &turn.pane_id,
            &format!(
                "agent: provider output-limit retries exhausted; compacting conversation before continuing error_kind={}",
                runtime_mezzanine_error_code(error.kind())
            ),
        )?;
        self.append_agent_trace_turn_event(
            &turn.pane_id,
            turn_id,
            &format!(
                "provider_request recovery_queued reason=provider_output_limit_compaction error_kind={} outcome={}",
                runtime_mezzanine_error_code(error.kind()),
                runtime_compaction_outcome_name(&outcome)
            ),
        )?;
        Ok(matches!(outcome, AgentShellCommandOutcome::Mutated { .. }))
    }

    /// Returns pane ids with queued model-backed compaction tasks.
    pub fn pending_agent_compaction_tasks(&self) -> Vec<String> {
        self.pending_agent_compaction_tasks
            .keys()
            .cloned()
            .collect()
    }

    /// Returns running turns waiting on model-backed compaction before retry.
    ///
    /// Automatic output-limit recovery keeps the turn running while an async
    /// compaction worker summarizes the pane transcript. Actor-owned idle
    /// cleanup uses these ids as valid progress paths until compaction reports
    /// completion or failure.
    pub(crate) fn agent_compaction_resume_turn_ids(&self) -> Vec<String> {
        self.pending_agent_compaction_tasks
            .values()
            .chain(self.claimed_agent_compaction_tasks.values())
            .filter_map(|task| task.resume_turn_id.clone())
            .collect()
    }

    /// Claims one queued compaction task for execution outside the actor.
    pub fn claim_agent_compaction_task(
        &mut self,
        pane_id: &str,
    ) -> Result<Option<RuntimeAgentCompactionDispatch>> {
        let Some(task) = self.pending_agent_compaction_tasks.remove(pane_id) else {
            return Ok(None);
        };
        if !self.agent_compacting_panes.contains_key(pane_id) {
            return Ok(None);
        }
        let provider_config = self
            .provider_registry
            .provider(&task.model_profile.provider)
            .cloned()
            .ok_or_else(|| {
                MezError::config(format!(
                    "provider `{}` for active model profile is not configured",
                    task.model_profile.provider
                ))
            })?;
        let provider_api =
            effective_provider_api(&provider_config.kind, provider_config.api.as_deref())?;
        self.append_credential_access_audit(
            &task.model_profile.provider,
            &provider_config.auth_profile,
            "provider_compact",
            "requested",
        )?;
        let Some(auth_store) = self.auth_store.as_ref() else {
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
        let provider = match provider_api {
            ProviderApiCompatibility::OpenAiResponses => {
                openai_responses_provider_from_auth_store_with_provider_options(
                    auth_store,
                    &task.model_profile.provider,
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
                    &task.model_profile.provider,
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
                    &task.model_profile.provider,
                    endpoint_override,
                    DEFAULT_PROVIDER_TIMEOUT_MS,
                    ReqwestProviderHttpTransport,
                )
                .map(RuntimeAgentProviderDispatchProvider::DeepSeek)
            }
            ProviderApiCompatibility::AnthropicMessages => {
                anthropic_provider_from_auth_store_with_provider_options(
                    auth_store,
                    &task.model_profile.provider,
                    endpoint_override,
                    &task.model_profile.provider_options,
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
        self.claimed_agent_compaction_tasks
            .insert(pane_id.to_string(), task.clone());
        Ok(Some(RuntimeAgentCompactionDispatch { task, provider }))
    }

    /// Applies a completed model-backed compaction response.
    pub fn apply_agent_compaction_completed_event(
        &mut self,
        pane_id: &str,
        response: ModelResponse,
    ) -> Result<bool> {
        let Some(task) = self.claimed_agent_compaction_tasks.remove(pane_id) else {
            self.agent_compacting_panes.remove(pane_id);
            return Ok(false);
        };
        self.agent_compacting_panes.remove(pane_id);
        self.record_agent_provider_token_usage_with_profile(
            pane_id,
            response.usage,
            response.usage,
            Some(&task.model_profile),
        );
        self.record_agent_provider_quota_usage(pane_id, &response.quota_usage);
        let summary = match runtime_model_compaction_summary_from_response(&response) {
            Ok(summary) => summary,
            Err(error) => {
                self.append_agent_status_text_to_terminal_buffer(
                    pane_id,
                    &format!(
                        "agent: compact failed while reading summary: {}",
                        error.message()
                    ),
                )?;
                return Ok(true);
            }
        };
        let now = current_unix_seconds().max(1);
        let memory_id = format!("compact-{}", task.conversation_id);
        let content = runtime_model_compact_memory_content(
            pane_id,
            &task.conversation_id,
            task.transcript_entries,
            task.summarized_entries,
            &task.model_profile_name,
            &task.model_profile,
            &summary,
        );
        self.upsert_session_memory(MemoryRecord::new_with_defaults(
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
        ))?;
        let remaining_transcript_entries = self
            .agent_shell_store
            .retain_recent_transcript_entries(pane_id, task.retained_transcript_entries)?
            .transcript_entries;
        self.append_agent_status_text_to_terminal_buffer(
            pane_id,
            &format!(
                "agent: compacted conversation summary memory_id={} summarized_entries={} remaining_transcript_entries={} source=model-compact trigger={}",
                memory_id, task.summarized_entries, remaining_transcript_entries, task.source
            ),
        )?;
        if let Some(resume_turn_id) = task.resume_turn_id.as_deref() {
            let refreshed =
                self.refresh_running_turn_context_after_conversation_compaction(resume_turn_id)?;
            if refreshed {
                self.queue_agent_provider_recovery_task_after_compaction(resume_turn_id)?;
                self.append_agent_trace_turn_event(
                    pane_id,
                    resume_turn_id,
                    "provider_request recovery_resuming reason=provider_output_limit_compaction_completed",
                )?;
            }
        }
        Ok(true)
    }

    /// Applies a failed model-backed compaction worker result.
    pub fn apply_agent_compaction_failed_event(
        &mut self,
        pane_id: &str,
        message: &str,
    ) -> Result<bool> {
        let pending_task = self.pending_agent_compaction_tasks.remove(pane_id);
        let claimed_task = self.claimed_agent_compaction_tasks.remove(pane_id);
        let resume_turn_id = claimed_task
            .as_ref()
            .or(pending_task.as_ref())
            .and_then(|task| task.resume_turn_id.clone());
        let had_task = pending_task.is_some()
            || claimed_task.is_some()
            || self.agent_compacting_panes.remove(pane_id).is_some();
        if had_task {
            self.append_agent_status_text_to_terminal_buffer(
                pane_id,
                &format!("agent: compact failed during provider request: {message}"),
            )?;
        }
        if let Some(resume_turn_id) = resume_turn_id {
            self.fail_running_turn_after_output_limit_compaction_failure(&resume_turn_id, message)?;
        }
        Ok(had_task)
    }

    /// Fails a turn whose automatic output-limit compaction could not finish.
    fn fail_running_turn_after_output_limit_compaction_failure(
        &mut self,
        turn_id: &str,
        message: &str,
    ) -> Result<()> {
        let Some(turn) = self
            .agent_turn_ledger
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
        let Some(model_profile) = self.agent_turn_model_profiles.get(turn_id).cloned() else {
            return Ok(());
        };
        let error = MezError::invalid_state(format!(
            "automatic output-limit compaction failed before provider retry: {message}"
        ));
        self.fail_agent_turn_for_provider_error(
            &turn,
            &model_profile.provider,
            &model_profile,
            &error,
        )
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
        let Some(store) = self.agent_transcript_store.as_ref() else {
            return Ok(Vec::new());
        };
        match store.inspect(conversation_id) {
            Ok(entries) => Ok(entries),
            Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }
}

/// Returns a compact diagnostic name for a compaction queue outcome.
fn runtime_compaction_outcome_name(outcome: &AgentShellCommandOutcome) -> &'static str {
    match outcome {
        AgentShellCommandOutcome::Mutated { .. } => "queued",
        AgentShellCommandOutcome::Display { .. } => "skipped",
        AgentShellCommandOutcome::RequiresRuntime { .. } => "requires-runtime",
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
) -> Result<ModelRequest> {
    let agent_id = format!("agent-{pane_id}");
    let turn_id = format!("compact-{conversation_id}");
    Ok(ModelRequest {
        provider: profile.provider.clone(),
        model: profile.model.clone(),
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
        interaction_kind: ModelInteractionKind::ActionExecution,
        allowed_actions: AllowedActionSet::say_only(),
        messages: vec![
            ModelMessage {
                role: ModelMessageRole::System,
                source: ContextSourceKind::System,
                content: "You are Mezzanine's conversation compactor. Produce durable, concise summaries that preserve task-critical context and omit secrets."
                    .to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::Developer,
                source: ContextSourceKind::DeveloperInstruction,
                content: "Return exactly one `say` action with `status` set to `final` and `content_type` set to `text/markdown; charset=utf-8`. The text must summarize the conversation for a future agent turn. Preserve user goals, current plan, decisions, file paths, commands, test results, blockers, and pending follow-up. Do not claim work was completed unless the transcript proves it. Redact credentials and secrets."
                    .to_string(),
            },
            ModelMessage {
                role: ModelMessageRole::User,
                source: ContextSourceKind::Transcript,
                content: runtime_model_compaction_source(
                    pane_id,
                    conversation_id,
                    transcript_entries,
                    entries,
                    context,
                ),
            },
        ],
    })
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
            context.blocks.len()
        ),
    ];
    for (index, block) in context.blocks.iter().enumerate() {
        lines.push(format!(
            "context_block={} source={} label={} content={}",
            index,
            runtime_context_source_kind_name(block.source),
            json_escape(&block.label),
            runtime_model_compaction_entry_content(&block.content)
        ));
    }
    let selected = runtime_compact_selected_entries(entries);
    let omitted = entries.len().saturating_sub(selected.len());
    if omitted > 0 {
        lines.push(format!(
            "Middle durable transcript entries omitted from compaction source: {omitted}"
        ));
    }
    for entry in selected {
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
        ContextSourceKind::RuntimeHint => "runtime-hint",
        ContextSourceKind::ProjectGuidance => "project-guidance",
        ContextSourceKind::Memory => "memory",
        ContextSourceKind::Transcript => "transcript",
        ContextSourceKind::TranscriptUser => "transcript-user",
        ContextSourceKind::TranscriptAssistant => "transcript-assistant",
        ContextSourceKind::TranscriptTool => "transcript-tool",
        ContextSourceKind::EvidenceLedger => "evidence-ledger",
        ContextSourceKind::CommittedEvidence => "committed-evidence",
        ContextSourceKind::ActionResult => "action-result",
    }
}

/// Bounds and redacts one transcript entry before sending it for compaction.
pub(super) fn runtime_model_compaction_entry_content(content: &str) -> String {
    const MAX_MODEL_COMPACTION_ENTRY_BYTES: usize = 4096;
    let redacted = content
        .split_whitespace()
        .map(runtime_compact_redact_sensitive_token)
        .collect::<Vec<_>>()
        .join(" ");
    if redacted.len() <= MAX_MODEL_COMPACTION_ENTRY_BYTES {
        return redacted;
    }
    let mut end = MAX_MODEL_COMPACTION_ENTRY_BYTES;
    while !redacted.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!(
        "{}...[entry content elided before compaction; original_bytes={}]",
        &redacted[..end],
        redacted.len()
    )
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
    pane_id: &str,
    conversation_id: &str,
    transcript_entries: u64,
    summarized_entries: usize,
    model_profile_name: &str,
    profile: &ModelProfile,
    summary: &str,
) -> String {
    [
        format!("Model-generated compacted conversation summary for {conversation_id}."),
        format!("Pane: {pane_id}."),
        format!("Transcript entries before compaction: {transcript_entries}."),
        format!("Durable entries supplied to model: {summarized_entries}."),
        format!("Model profile: {model_profile_name}."),
        format!("Provider: {}.", profile.provider),
        format!("Model: {}.", profile.model),
        String::new(),
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
    context: AgentContext,
) -> Result<AgentContext> {
    AgentContext::new(
        context
            .blocks
            .into_iter()
            .filter(|block| !runtime_context_block_is_transcript_replay(block))
            .collect(),
    )
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
    let tail_budget = runtime_compact_retained_context_tail_budget_words(
        context_budget_words,
        retained_tail_percent,
    );
    let mut retained_entries = 0usize;
    let mut retained_words = 0usize;
    for entry in active_entries.iter().rev() {
        let entry_words = runtime_compact_transcript_entry_context_words(entry);
        if retained_entries > 0 && retained_words.saturating_add(entry_words) > tail_budget {
            break;
        }
        retained_entries += 1;
        retained_words = retained_words.saturating_add(entry_words);
    }
    u64::try_from(retained_entries).unwrap_or(u64::MAX)
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
    let maximum_forced_retained = u64::try_from(active_count.saturating_sub(1)).unwrap_or(u64::MAX);
    retained.min(maximum_forced_retained)
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
    let compactable_count = active_count.saturating_sub(retained_count);
    let active_start = durable_entries.len().saturating_sub(active_count);
    &durable_entries[active_start..active_start + compactable_count]
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
    AGENT_COMPACT_TRANSCRIPT_ENTRY_CONTEXT_OVERHEAD_WORDS
        .saturating_add(model_context_text_word_count(&entry.content))
        .saturating_add(model_context_text_word_count(&entry.turn_id))
        .saturating_add(model_context_text_word_count(&entry.agent_id))
        .saturating_add(model_context_text_word_count(&entry.pane_id))
}

/// Runs the runtime compact selected entries operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn runtime_compact_selected_entries(
    entries: &[TranscriptEntry],
) -> Vec<&TranscriptEntry> {
    /// Defines the MAX COMPACTED TRANSCRIPT ENTRIES const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    const MAX_COMPACTED_TRANSCRIPT_ENTRIES: usize = 12;
    /// Defines the LEADING COMPACTED TRANSCRIPT ENTRIES const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    const LEADING_COMPACTED_TRANSCRIPT_ENTRIES: usize = 4;
    /// Defines the TRAILING COMPACTED TRANSCRIPT ENTRIES const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    const TRAILING_COMPACTED_TRANSCRIPT_ENTRIES: usize = 8;
    if entries.len() <= MAX_COMPACTED_TRANSCRIPT_ENTRIES {
        return entries.iter().collect();
    }
    entries
        .iter()
        .take(LEADING_COMPACTED_TRANSCRIPT_ENTRIES)
        .chain(
            entries.iter().skip(
                entries
                    .len()
                    .saturating_sub(TRAILING_COMPACTED_TRANSCRIPT_ENTRIES),
            ),
        )
        .collect()
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
