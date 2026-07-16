//! Runtime agent transcript and usage bookkeeping helpers.
//!
//! This module owns durable transcript entry construction, retained patch
//! records, copyable assistant output, and provider token/quota accounting. It
//! keeps persistence and accounting details out of execution-state code.

use super::*;

impl RuntimeSessionService {
    /// Runs the persist runtime agent turn execution transcript operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(in crate::runtime) fn persist_runtime_agent_turn_execution_transcript(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<usize> {
        let conversation_id = self
            .agent_shell_store
            .get(&turn.pane_id)
            .map(|session| session.session_id.clone());
        if let Some(conversation_id) = conversation_id.as_deref() {
            self.record_runtime_agent_patch_results(conversation_id, execution);
        }
        if self
            .agent_shell_store
            .get(&turn.pane_id)
            .is_some_and(|session| session.ephemeral)
        {
            return Ok(0);
        }
        let Some(store) = self.agent_transcript_store.clone() else {
            return Ok(0);
        };
        let conversation_id = conversation_id
            .ok_or_else(|| MezError::invalid_state("agent shell session missing for transcript"))?;
        let created_at_unix_seconds = current_unix_seconds().max(1);
        let entries = if self.transcript_effects_use_adapter {
            let first_sequence = self
                .deferred_transcript_next_sequences
                .get(&conversation_id)
                .copied()
                .map(Ok)
                .unwrap_or_else(|| next_transcript_sequence(&store, &conversation_id))?;
            let entries = self.runtime_transcript_entries_for_execution(
                &conversation_id,
                first_sequence,
                created_at_unix_seconds,
                turn,
                execution,
            )?;
            if let Some(next_sequence) =
                entries.last().map(|entry| entry.sequence.saturating_add(1))
            {
                self.deferred_transcript_next_sequences
                    .insert(conversation_id.clone(), next_sequence);
            }
            self.queued_transcript_effects
                .push(RuntimeSideEffect::PersistTranscriptEntries {
                    path: store.transcript_path(&conversation_id)?,
                    store,
                    entries: entries.clone(),
                });
            entries
        } else {
            let first_sequence = next_transcript_sequence(&store, &conversation_id)?;
            let entries = self.runtime_transcript_entries_for_execution(
                &conversation_id,
                first_sequence,
                created_at_unix_seconds,
                turn,
                execution,
            )?;
            store.append_many(&entries)?;
            entries
        };
        self.agent_shell_store
            .record_transcript_entries(&turn.pane_id, entries.len())?;
        self.record_pane_transcript_ref(
            &turn.pane_id,
            format!("transcript:{}:{conversation_id}", turn.pane_id),
        )?;
        Ok(entries.len())
    }

    /// Retains exact `apply_patch` payloads and observed outcomes for export.
    ///
    /// Durable transcript entries intentionally summarize patch actions so
    /// model context stays compact. This separate pane-session ledger preserves
    /// the exact patches for `/copy-patches` without feeding them back into later
    /// model prompts.
    fn record_runtime_agent_patch_results(
        &mut self,
        conversation_id: &str,
        execution: &AgentTurnExecution,
    ) {
        let Some(batch) = execution.response.action_batch.as_ref() else {
            return;
        };
        for action in &batch.actions {
            let AgentActionPayload::ApplyPatch { patch, strip } = &action.payload else {
                continue;
            };
            let Some(result) = execution
                .action_results
                .iter()
                .find(|candidate| candidate.action_id == action.id)
            else {
                continue;
            };
            if result.status == ActionStatus::Running {
                continue;
            }
            let record = RuntimeAgentPatchRecord {
                turn_id: batch.turn_id.clone(),
                action_id: action.id.clone(),
                status: runtime_action_status_name(result.status).to_string(),
                patch: patch.clone(),
                strip: *strip,
                error_code: result.error.as_ref().map(|error| error.code.clone()),
                error_message: Self::runtime_agent_patch_record_error_message(result),
            };
            let records = self
                .agent
                .agent_session_patch_records
                .entry(conversation_id.to_string())
                .or_default();
            // Running records are per-attempt placeholders. Settled records are
            // immutable so a later retry with the same action id stays visible.
            if let Some(existing) = records.iter_mut().rev().find(|candidate| {
                candidate.turn_id == record.turn_id
                    && candidate.action_id == record.action_id
                    && candidate.patch == record.patch
                    && candidate.status == "running"
            }) {
                *existing = record;
            } else if result.status == ActionStatus::Running
                || !records.iter().any(|candidate| candidate == &record)
            {
                records.push(record);
            }
        }
    }

    /// Retains patch action outcomes for the pane session that owns a turn.
    ///
    /// Recovery paths can remove an in-flight execution before transcript
    /// persistence runs, so action-result boundaries call this helper to keep
    /// `/copy-patches` complete for failed attempts as well as settled turns.
    pub(in crate::runtime) fn record_runtime_agent_patch_results_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) {
        let Some(conversation_id) = self
            .agent_shell_store
            .get(&turn.pane_id)
            .map(|session| session.session_id.clone())
        else {
            return;
        };
        self.record_runtime_agent_patch_results(&conversation_id, execution);
    }

    /// Returns the most useful retained diagnostic for one patch attempt.
    ///
    /// The action error often only says that the shell command exited nonzero.
    /// For `apply_patch` debugging, the captured patcher's stderr/stdout is the
    /// actionable text because it includes the failed hunk, affected path, and
    /// current-file context hints.
    fn runtime_agent_patch_record_error_message(result: &ActionResult) -> Option<String> {
        let generic = result.error.as_ref().map(|error| error.message.clone());
        if !result.is_error {
            return generic;
        }
        runtime_unrecovered_action_failure_output(result)
            .map(|output| output.trim().to_string())
            .filter(|output| !output.is_empty())
            .or(generic)
    }

    /// Builds durable transcript entries for one completed turn, including one
    /// initial environment entry that preserves the session directory.
    ///
    /// # Parameters
    /// - `conversation_id`: The durable transcript conversation id.
    /// - `first_sequence`: The next sequence number in the transcript.
    /// - `created_at_unix_seconds`: The timestamp assigned to appended entries.
    /// - `turn`: The turn whose execution is being persisted.
    /// - `execution`: The completed execution being converted into entries.
    fn runtime_transcript_entries_for_execution(
        &self,
        conversation_id: &str,
        first_sequence: u64,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<Vec<TranscriptEntry>> {
        let mut sequence = first_sequence;
        let mut entries = Vec::new();
        if sequence == 1
            && let Some(entry) = self.runtime_session_directory_transcript_entry(
                conversation_id,
                sequence,
                created_at_unix_seconds,
                turn,
            )
        {
            sequence = sequence.saturating_add(1);
            entries.push(entry);
        }
        entries.extend(transcript_entries_for_execution(
            conversation_id,
            sequence,
            created_at_unix_seconds,
            turn,
            execution,
        )?);
        Ok(entries)
    }

    /// Builds the one-time system transcript entry that makes saved sessions
    /// self-describing in `/list-sessions` and `/resume` flows.
    ///
    /// # Parameters
    /// - `conversation_id`: The durable transcript conversation id.
    /// - `sequence`: The sequence assigned to the context entry.
    /// - `created_at_unix_seconds`: The timestamp assigned to the context entry.
    /// - `turn`: The turn whose pane owns the saved session.
    fn runtime_session_directory_transcript_entry(
        &self,
        conversation_id: &str,
        sequence: u64,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
    ) -> Option<TranscriptEntry> {
        let working_directory = self.pane_current_working_directory(&turn.pane_id)?;
        let project_root = discover_project_root(&working_directory);
        let mut content = format!("cwd={}", working_directory.to_string_lossy());
        if !project_root.as_os_str().is_empty() {
            content.push('\n');
            content.push_str(&format!("project_root={}", project_root.to_string_lossy()));
        }
        Some(TranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence,
            created_at_unix_seconds,
            role: TranscriptRole::System,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            content,
        })
    }

    /// Retains the latest model-authored `say` text for pane-local copy commands.
    pub(in crate::runtime) fn record_agent_copy_output(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) {
        let Some(batch) = execution.response.action_batch.as_ref() else {
            return;
        };
        let Some((output, content_type)) = batch.actions.iter().rev().find_map(|action| {
            if let AgentActionPayload::Say {
                text, content_type, ..
            } = &action.payload
                && !text.trim().is_empty()
            {
                Some((text.clone(), content_type.clone()))
            } else {
                None
            }
        }) else {
            return;
        };
        self.agent.agent_copy_outputs.insert(
            turn.pane_id.clone(),
            RuntimeAgentCopyOutput {
                turn_id: turn.turn_id.clone(),
                output,
                content_type,
            },
        );
    }

    /// Adds provider-reported token usage to the active pane conversation.
    #[cfg(test)]
    pub(in crate::runtime) fn record_agent_provider_token_usage(
        &mut self,
        pane_id: &str,
        usage: ModelTokenUsage,
    ) {
        let agent_id = format!("agent-{pane_id}");
        let profile = self
            .active_model_profile_for_pane(pane_id, &agent_id, None)
            .ok()
            .map(|(_, profile)| profile);
        self.record_agent_provider_token_usage_with_profile(
            pane_id,
            usage,
            usage,
            profile.as_ref(),
        );
    }

    /// Adds provider-reported token usage using the exact selected model profile.
    pub(in crate::runtime) fn record_agent_provider_token_usage_with_profile(
        &mut self,
        pane_id: &str,
        usage: ModelTokenUsage,
        latest_context_usage: ModelTokenUsage,
        profile: Option<&ModelProfile>,
    ) {
        if usage.is_zero() {
            return;
        }
        let conversation_id = self
            .agent_shell_store
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .unwrap_or_else(|| format!("pane:{pane_id}"));
        let token_usage_key = profile
            .map(|profile| ModelTokenUsageKey::new(profile.provider.clone(), profile.model.clone()))
            .unwrap_or_else(ModelTokenUsageKey::unknown);
        self.agent
            .agent_token_usage_by_conversation
            .entry(conversation_id.clone())
            .or_default()
            .entry(token_usage_key.clone())
            .or_default()
            .add_assign(usage);
        self.agent
            .agent_token_usage_by_pane
            .entry(pane_id.to_string())
            .or_default()
            .entry(token_usage_key)
            .or_default()
            .add_assign(usage);
        if let Some(profile) = profile {
            if let Some(snapshot) =
                mez_agent::agent_context_usage_snapshot(profile, latest_context_usage)
            {
                if let Some(display) = runtime_agent_provider_context_usage_display(snapshot) {
                    self.agent
                        .agent_context_usage_by_conversation
                        .insert(conversation_id.clone(), display);
                }
                self.agent
                    .agent_context_usage_snapshot_by_conversation
                    .insert(conversation_id, snapshot);
            } else {
                self.agent
                    .agent_context_usage_by_conversation
                    .remove(&conversation_id);
                self.agent
                    .agent_context_usage_snapshot_by_conversation
                    .remove(&conversation_id);
            }
        }
        let _ = self.checkpoint_agent_session_metadata();
    }

    /// Stores auxiliary provider token usage for the active pane conversation.
    ///
    /// Router/auto-sizing requests happen before the main assistant response and
    /// therefore do not have a user-visible model profile for context-window
    /// display. They should still appear in provider/model token accounting so
    /// `/status` and durable metadata include their cost.
    pub(in crate::runtime) fn record_agent_provider_token_usage_by_model(
        &mut self,
        pane_id: &str,
        usage_by_model: &BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    ) {
        if usage_by_model.is_empty() {
            return;
        }
        let conversation_id = self
            .agent_shell_store
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .unwrap_or_else(|| format!("pane:{pane_id}"));
        let mut changed = false;
        let conversation_usage = self
            .agent
            .agent_token_usage_by_conversation
            .entry(conversation_id)
            .or_default();
        let pane_usage = self
            .agent
            .agent_token_usage_by_pane
            .entry(pane_id.to_string())
            .or_default();
        for (key, usage) in usage_by_model {
            if usage.is_zero() {
                continue;
            }
            conversation_usage
                .entry(key.clone())
                .or_default()
                .add_assign(*usage);
            pane_usage
                .entry(key.clone())
                .or_default()
                .add_assign(*usage);
            changed = true;
        }
        if changed {
            let _ = self.checkpoint_agent_session_metadata();
        }
    }

    /// Stores the latest provider-reported quota usage for the active pane conversation.
    pub(in crate::runtime) fn record_agent_provider_quota_usage(
        &mut self,
        pane_id: &str,
        quota_usage: &[ProviderQuotaUsage],
    ) {
        if quota_usage.is_empty() {
            return;
        }
        let conversation_id = self
            .agent_shell_store
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .unwrap_or_else(|| format!("pane:{pane_id}"));
        self.agent
            .agent_quota_usage_by_conversation
            .insert(conversation_id, quota_usage.to_vec());
        let _ = self.checkpoint_agent_session_metadata();
    }
}
