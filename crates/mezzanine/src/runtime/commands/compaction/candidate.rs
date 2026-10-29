//! Complete prospective manual requests from one authoritative history projection.
//!
//! Candidate sizing shares prefix/range replacement with durable publication,
//! reads the checked physical suffix and rebuilds each owned summary once.
//! Nonhistorical policy, optional memory and exact references retain metadata;
//! native groups are restored only within the new history before composition.
//! Provider tools, controls and output/headroom rules still determine fit. This
//! module neither dispatches provider work nor publishes an epoch or replays an
//! action; frozen source and mutation-lane validation remain with their owners.

use super::*;

impl RuntimeSessionService {
    /// Validates the provider request that would follow a manual compaction.
    ///
    /// This runs before durable epoch mutation. It reconstructs the active
    /// summary, retained raw tail (including entries written while the
    /// compactor was running), current MCP catalog, and accepted steering.
    pub(super) fn validate_manual_compaction_candidate(
        &mut self,
        task: &RuntimeAgentCompactionTask,
        summary: &str,
    ) -> Result<(usize, usize, String)> {
        let candidate_context = self.manual_compaction_candidate_context(task, summary)?;
        self.estimate_manual_compaction_candidate(task, candidate_context)
    }

    /// Builds prospective replay without publishing or dispatching a request.
    /// Requires the same conversation, frozen epoch/rows and bounded summary;
    /// invalid prefix cuts, changed source and unavailable storage fail. Exact
    /// base metadata and newly restored native history remain separately owned.
    pub(crate) fn manual_compaction_candidate_context(
        &self,
        task: &RuntimeAgentCompactionTask,
        summary: &str,
    ) -> Result<AgentContext> {
        let summary_words = model_context_text_word_count(summary);
        let summary_budget_words = task.summary_correction.manual_words.ok_or_else(|| {
            MezError::invalid_state("manual compaction frozen word allowance is missing")
        })?;
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
        self.agent_shell_store()
            .get(&task.pane_id)
            .filter(|session| session.session_id == task.conversation_id)
            .ok_or_else(|| MezError::invalid_state("manual compaction session is unavailable"))?;
        let store = self.persistence.cloned_transcript_store().ok_or_else(|| {
            MezError::invalid_state("manual compaction candidate requires transcript storage")
        })?;
        let pending = self
            .persistence
            .pending_transcript_entries(&task.conversation_id);
        let snapshot =
            store.conversation_replay_snapshot(&task.conversation_id, &pending, |_| {
                Some((
                    crate::storage::transcript::ConversationTranscriptRead::All,
                    task.transcript_entries > pending.len() as u64,
                ))
            })?;
        let entries = snapshot.view.logical;
        let previous_epoch = snapshot.epoch;
        if task
            .frozen_compaction_epoch
            .as_ref()
            .is_some_and(|baseline| baseline != &previous_epoch)
        {
            return Err(MezError::conflict(
                "manual compaction source epoch changed before completion",
            ));
        }
        let boundary = task.compacted_through_sequence.unwrap_or_else(|| {
            previous_epoch
                .as_ref()
                .map_or(0, |epoch| epoch.through_sequence)
        });
        let first_retained = entries.partition_point(|row| row.sequence <= boundary);
        let retained_entries = &entries[first_retained..];
        let projection = crate::storage::transcript::project_compaction_prefix(
            &task.conversation_id,
            previous_epoch.as_ref(),
            boundary,
            &runtime_model_compact_memory_content(
                &task.pane_id,
                &task.conversation_id,
                task.transcript_entries,
                task.summarized_entries,
                &task.model_profile_name,
                &task.model_profile,
                summary,
            ),
        )?;
        if task.frozen_compaction_epoch.is_some() {
            crate::storage::transcript::validate_compaction_prefix_source(
                &entries,
                previous_epoch
                    .as_ref()
                    .map_or(0, |epoch| epoch.through_sequence),
                boundary,
                &task.frozen_compaction_rows,
            )
            .map_err(|_| MezError::conflict("summary correction frozen manual source changed"))?;
        } else {
            // An uncaptured legacy proposal does not provide a frozen epoch witness.
            if task
                .frozen_compaction_rows
                .iter()
                .any(|row| !entries.contains(row))
            {
                return Err(MezError::conflict(
                    "summary correction frozen manual source changed",
                ));
            }
        }
        let replay = runtime_agent_compaction_replay_context(
            &task.pane_id,
            retained_entries,
            &projection.ranges,
        );
        let compact_memory_id =
            mez_agent::memory::canonical_memory_uuid(&format!("compact-{}", task.conversation_id));
        let mut base_context =
            runtime_compaction_context_without_transcript_blocks(base_context.clone())?;
        // The proposed epoch owns the entire historical summary projection.
        // Rebuild it once; do not retain an absorbed or surviving selective
        // summary from the old base alongside its newly rendered replacement.
        // Only the exact controller-owned legacy prefix ID is a UUID fallback.
        base_context.retain_blocks_with_metadata(|block, _| {
            !mez_agent::context_block_is_compaction_summary(block)
                && !(block.source == ContextSourceKind::Memory
                    && block
                        .label
                        .starts_with(&format!("memory {compact_memory_id} ")))
                && !(block.source == ContextSourceKind::UserInstruction
                    && block.label == "user prompt")
                && block.source != ContextSourceKind::McpCatalogSnapshot
                && !runtime_context_block_is_transcript_replay(block)
        })?;
        let mut blocks = vec![ContextBlock::reference_event(
            ContextSourceKind::CompactionPrefix,
            format!("memory {compact_memory_id} (conversation)"),
            projection.summary,
        )];
        blocks.extend(replay.blocks);
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
        blocks.push(ContextBlock::user_event("user prompt", prompt));
        // Restore native ownership only within newly rendered history. Matching
        // against the base could steal an unrelated exact reference with the
        // same bytes; appending the typed result preserves all base metadata.
        let mut history_context = AgentContext::import_durable_blocks(blocks)?;
        history_context
            .restore_imported_execution_events(&replay.execution_events)
            .map_err(|error| MezError::invalid_state(error.to_string()))?;
        base_context.append_imported_history(&history_context)?;
        let candidate_context = base_context;
        let mcp_summary = self.mcp_registry().prompt_summary();
        let configured_servers = self.integration.always_exposed_mcp_servers().to_vec();
        let candidate_context = mez_agent::append_mcp_context_with_configured(
            candidate_context,
            &mcp_summary,
            &configured_servers,
        )?;
        Ok(candidate_context)
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
    fn estimate_manual_compaction_candidate(
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
