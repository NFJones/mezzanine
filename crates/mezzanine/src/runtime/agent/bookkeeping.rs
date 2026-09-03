//! Runtime agent transcript and usage bookkeeping helpers.
//!
//! This module owns durable transcript entry construction, retained patch
//! records, copyable assistant output, and provider token/quota accounting. It
//! keeps persistence and accounting details out of execution-state code.

use super::{
    ActionResult, ActionStatus, AgentActionPayload, AgentTurnExecution, AgentTurnRecord, BTreeMap,
    ContextSourceKind, MezError, ModelProfile, ModelTokenUsage, ModelTokenUsageKey,
    ProviderQuotaUsage, Result, RuntimeAgentCopyOutput, RuntimeAgentPatchRecord,
    RuntimeSessionService, RuntimeSideEffect, TranscriptEntry, TranscriptRole,
    current_unix_seconds, discover_project_root, next_transcript_sequence,
    runtime_action_status_name, runtime_agent_provider_context_usage_display,
    runtime_unrecovered_action_failure_output, transcript_entries_for_execution,
};
use crate::storage::token_usage::{TokenUsageEvent, new_token_usage_event_id};
use mez_agent::TranscriptContextEvent;

/// Maximum recent execution groups retained for in-process idempotency.
const RUNTIME_PERSISTED_EXECUTION_TRANSCRIPT_LIMIT: usize = 4096;

impl RuntimeSessionService {
    /// Runs the persist runtime agent turn execution transcript operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(crate) fn persist_runtime_agent_turn_execution_transcript(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) -> Result<usize> {
        let Some((session_conversation_id, session_ephemeral)) = self
            .agent_shell_store()
            .get(&turn.pane_id)
            .map(|session| (session.session_id.clone(), session.ephemeral))
        else {
            return Ok(0);
        };
        if session_conversation_id != turn.conversation_id {
            return Err(MezError::invalid_state(
                "agent turn conversation no longer owns transcript target",
            ));
        }
        let conversation_id = turn.conversation_id.clone();
        self.record_runtime_agent_patch_results(&conversation_id, execution);
        if session_ephemeral {
            return Ok(0);
        }
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Ok(0);
        };
        let persistence_key = (conversation_id.clone(), turn.turn_id.clone());
        let mut existing_entries = match store.inspect(&conversation_id) {
            Ok(entries) => entries,
            Err(error) if error.kind() == crate::error::MezErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        existing_entries.extend(
            self.persistence
                .pending_transcript_entries(&conversation_id),
        );
        let created_at_unix_seconds = current_unix_seconds().max(1);
        let entries = if self.persistence.transcript_uses_adapter() {
            let first_sequence = self
                .persistence
                .deferred_transcript_next_sequence(&conversation_id)
                .map(Ok)
                .unwrap_or_else(|| next_transcript_sequence(&store, &conversation_id))?;
            let first_persistence = first_sequence == 1;
            let entries = self.runtime_transcript_entries_for_execution(
                &conversation_id,
                first_sequence,
                created_at_unix_seconds,
                turn,
                execution,
            )?;
            let entries =
                Self::new_runtime_transcript_entries(entries, &existing_entries, first_sequence);
            if entries.is_empty() {
                return Ok(0);
            }
            if let Some(next_sequence) =
                entries.last().map(|entry| entry.sequence.saturating_add(1))
            {
                self.persistence
                    .set_deferred_transcript_next_sequence(conversation_id.clone(), next_sequence);
            }
            self.persistence
                .queue_transcript(RuntimeSideEffect::PersistTranscriptEntries {
                    path: store.transcript_path(&conversation_id)?,
                    store,
                    entries: entries.clone(),
                });
            if first_persistence {
                self.queue_saved_session_retention_operation(created_at_unix_seconds, false)?;
            }
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
            let entries =
                Self::new_runtime_transcript_entries(entries, &existing_entries, first_sequence);
            if entries.is_empty() {
                return Ok(0);
            }
            store.append_many(&entries)?;
            entries
        };
        self.agent
            .agent_persisted_execution_transcripts
            .insert(persistence_key);
        while self.agent.agent_persisted_execution_transcripts.len()
            > RUNTIME_PERSISTED_EXECUTION_TRANSCRIPT_LIMIT
        {
            let _ = self.agent.agent_persisted_execution_transcripts.pop_first();
        }
        self.agent_shell_store_mut()
            .record_transcript_entries(&turn.pane_id, entries.len())?;
        self.record_pane_transcript_ref(
            &turn.pane_id,
            format!("transcript:{}:{conversation_id}", turn.pane_id),
        )?;
        Ok(entries.len())
    }

    /// Keeps only transcript records that have not already been stored or
    /// queued for the same turn, then assigns one contiguous fresh sequence.
    ///
    /// A blocked turn can persist before approval and later acquire additional
    /// execution groups. Content identity makes that later persistence a delta
    /// while also making exact retries and process-restored writes idempotent.
    fn new_runtime_transcript_entries(
        entries: Vec<TranscriptEntry>,
        existing_entries: &[TranscriptEntry],
        first_sequence: u64,
    ) -> Vec<TranscriptEntry> {
        let mut identities = existing_entries
            .iter()
            .map(Self::runtime_transcript_entry_identity)
            .collect::<std::collections::BTreeSet<_>>();
        let mut sequence = first_sequence;
        entries
            .into_iter()
            .filter_map(|mut entry| {
                if !identities.insert(Self::runtime_transcript_entry_identity(&entry)) {
                    return None;
                }
                entry.sequence = sequence;
                sequence = sequence.saturating_add(1);
                Some(entry)
            })
            .collect()
    }

    /// Returns the durable identity of one transcript row without volatile
    /// sequence or timestamp fields.
    fn runtime_transcript_entry_identity(entry: &TranscriptEntry) -> (String, u8, String) {
        let role = match entry.role {
            TranscriptRole::User => 0,
            TranscriptRole::Assistant => 1,
            TranscriptRole::Tool => 2,
            TranscriptRole::System => 3,
        };
        (entry.turn_id.clone(), role, entry.content.clone())
    }

    /// Persists the originating prompt and available settled observations when
    /// an active turn is interrupted before it can produce terminal execution.
    pub(crate) fn persist_interrupted_agent_turn_transcript(
        &mut self,
        turn: &AgentTurnRecord,
        reason: &str,
    ) -> Result<usize> {
        let Some((session_conversation_id, session_ephemeral)) = self
            .agent_shell_store()
            .get(&turn.pane_id)
            .map(|session| (session.session_id.clone(), session.ephemeral))
        else {
            return Ok(0);
        };
        if session_conversation_id != turn.conversation_id || session_ephemeral {
            return Ok(0);
        }
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Ok(0);
        };
        let persistence_key = (
            turn.conversation_id.clone(),
            format!("interrupted:{}", turn.turn_id),
        );
        if self
            .agent
            .agent_persisted_execution_transcripts
            .contains(&persistence_key)
        {
            return Ok(0);
        }
        let Some(prompt) = self
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .and_then(|context| {
                context.blocks().iter().rev().find_map(|block| {
                    (block.source == ContextSourceKind::UserInstruction
                        && block.label == "user prompt"
                        && !block.content.trim().is_empty())
                    .then(|| block.content.trim().to_string())
                })
            })
        else {
            return Ok(0);
        };
        let evidence = self
            .agent_turn_executions()
            .get(&turn.turn_id)
            .map(|execution| {
                execution
                    .action_results
                    .iter()
                    .map(|result| {
                        format!(
                            "action_id={} type={} status={}",
                            result.action_id,
                            result.action_type,
                            runtime_action_status_name(result.status),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let created_at_unix_seconds = current_unix_seconds().max(1);
        let first_sequence = if self.persistence.transcript_uses_adapter() {
            self.persistence
                .deferred_transcript_next_sequence(&turn.conversation_id)
                .map(Ok)
                .unwrap_or_else(|| next_transcript_sequence(&store, &turn.conversation_id))?
        } else {
            next_transcript_sequence(&store, &turn.conversation_id)?
        };
        let first_persistence = first_sequence == 1;
        let mut entries = self.prompt_boundary_transcript_entries_for_turn(
            &turn.conversation_id,
            first_sequence,
            created_at_unix_seconds,
            turn,
        )?;
        let interrupted_entry = TranscriptEntry {
            conversation_id: turn.conversation_id.clone(),
            sequence: first_sequence.saturating_add(entries.len() as u64),
            created_at_unix_seconds,
            role: TranscriptRole::System,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            content: TranscriptContextEvent::InterruptedTurn {
                prompt,
                reason: reason.to_string(),
                evidence,
            }
            .to_transcript_content(),
        };
        interrupted_entry.validate()?;
        entries.push(interrupted_entry);
        if self.persistence.transcript_uses_adapter() {
            self.persistence.set_deferred_transcript_next_sequence(
                turn.conversation_id.clone(),
                first_sequence.saturating_add(entries.len() as u64),
            );
            self.persistence
                .queue_transcript(RuntimeSideEffect::PersistTranscriptEntries {
                    path: store.transcript_path(&turn.conversation_id)?,
                    store,
                    entries: entries.clone(),
                });
            if first_persistence {
                self.queue_saved_session_retention_operation(created_at_unix_seconds, false)?;
            }
        } else {
            store.append_many(&entries)?;
        }
        self.agent
            .agent_persisted_execution_transcripts
            .insert(persistence_key);
        while self.agent.agent_persisted_execution_transcripts.len()
            > RUNTIME_PERSISTED_EXECUTION_TRANSCRIPT_LIMIT
        {
            let _ = self.agent.agent_persisted_execution_transcripts.pop_first();
        }
        self.agent_shell_store_mut()
            .record_transcript_entries(&turn.pane_id, entries.len())?;
        self.record_pane_transcript_ref(
            &turn.pane_id,
            format!("transcript:{}:{}", turn.pane_id, turn.conversation_id),
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
    pub(crate) fn record_runtime_agent_patch_results_for_turn(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
    ) {
        let Some(conversation_id) = self
            .agent_shell_store()
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
        let mut execution_entries = transcript_entries_for_execution(
            conversation_id,
            sequence,
            created_at_unix_seconds,
            turn,
            execution,
        )?;
        self.insert_prompt_boundary_transcript_events(
            &mut execution_entries,
            conversation_id,
            created_at_unix_seconds,
            turn,
        )?;
        self.append_exact_execution_block_transcript_events(
            &mut execution_entries,
            conversation_id,
            created_at_unix_seconds,
            turn,
        )?;
        if execution.terminal_state == mez_agent::AgentTurnState::Completed
            && self.routed_presentation_turn(&turn.turn_id)
            && let Some(content) = self.routed_handoff_transcript_content(&turn.turn_id)
        {
            Self::insert_routed_handoff_transcript_event(
                &mut execution_entries,
                conversation_id,
                created_at_unix_seconds,
                turn,
                content,
            )?;
        }
        entries.extend(execution_entries);
        Ok(entries)
    }

    /// Inserts exact prompt-boundary context before its owning user entry.
    fn insert_prompt_boundary_transcript_events(
        &self,
        entries: &mut Vec<TranscriptEntry>,
        conversation_id: &str,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        let Some(insertion_index) = entries
            .iter()
            .position(|entry| entry.role == TranscriptRole::User)
        else {
            return Ok(());
        };
        let first_sequence = entries[insertion_index].sequence;
        let prompt_entries = self.prompt_boundary_transcript_entries_for_turn(
            conversation_id,
            first_sequence,
            created_at_unix_seconds,
            turn,
        )?;
        if prompt_entries.is_empty() {
            return Ok(());
        }
        let inserted = prompt_entries.len() as u64;
        for entry in &mut entries[insertion_index..] {
            entry.sequence = entry.sequence.saturating_add(inserted);
        }
        entries.splice(insertion_index..insertion_index, prompt_entries);
        Ok(())
    }

    /// Builds exact durable events for newly introduced context before a user prompt.
    fn prompt_boundary_transcript_entries_for_turn(
        &self,
        conversation_id: &str,
        first_sequence: u64,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
    ) -> Result<Vec<TranscriptEntry>> {
        let Some(context) = self.agent_turn_contexts().get(&turn.turn_id) else {
            return Ok(Vec::new());
        };
        let imported_history_events = self.agent_turn_imported_history_events(&turn.turn_id);
        let mut sequence = first_sequence;
        let mut entries = Vec::new();
        for block in context
            .chronology()
            .iter()
            .skip(imported_history_events)
            .map(|event| event.block())
        {
            if block.source == ContextSourceKind::UserInstruction && block.label == "user prompt" {
                break;
            }
            let event = if block.source == ContextSourceKind::Configuration
                && block.label == "task environment snapshot"
            {
                TranscriptContextEvent::environment_snapshot(block.content.clone()).ok_or_else(
                    || MezError::invalid_state("turn environment snapshot is empty or oversized"),
                )?
            } else if matches!(
                block.source,
                ContextSourceKind::SkillInstruction
                    | ContextSourceKind::LocalMessage
                    | ContextSourceKind::Policy
                    | ContextSourceKind::Configuration
            ) {
                TranscriptContextEvent::prompt_boundary(
                    block.source,
                    block.label.clone(),
                    block.content.clone(),
                )
                .ok_or_else(|| {
                    MezError::invalid_state(
                        "turn prompt-boundary context is empty, oversized, or unsupported",
                    )
                })?
            } else {
                continue;
            };
            let entry = TranscriptEntry {
                conversation_id: conversation_id.to_string(),
                sequence,
                created_at_unix_seconds,
                role: TranscriptRole::System,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                pane_id: turn.pane_id.clone(),
                content: event.to_transcript_content(),
            };
            entry.validate()?;
            entries.push(entry);
            sequence = sequence.saturating_add(1);
        }
        Ok(entries)
    }

    /// Appends exact cache-visible execution blocks after the ordinary display
    /// transcript projection for one completed turn.
    fn append_exact_execution_block_transcript_events(
        &self,
        entries: &mut Vec<TranscriptEntry>,
        conversation_id: &str,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        let Some(context) = self.agent_turn_contexts().get(&turn.turn_id) else {
            return Ok(());
        };
        let imported_history_events = self.agent_turn_imported_history_events(&turn.turn_id);
        let mut sequence = entries
            .last()
            .map_or(1, |entry| entry.sequence.saturating_add(1));
        for block in context
            .chronology()
            .iter()
            .skip(imported_history_events)
            .map(|event| event.block())
            .filter(|block| {
                matches!(
                    block.source,
                    ContextSourceKind::CommittedEvidence
                        | ContextSourceKind::TranscriptAssistant
                        | ContextSourceKind::TranscriptTool
                        | ContextSourceKind::ActionResult
                )
            })
        {
            let event = TranscriptContextEvent::execution_block(
                block.source,
                block.label.clone(),
                block.content.clone(),
            )
            .ok_or_else(|| {
                MezError::invalid_state("turn execution block is empty, oversized, or unsupported")
            })?;
            let entry = TranscriptEntry {
                conversation_id: conversation_id.to_string(),
                sequence,
                created_at_unix_seconds,
                role: TranscriptRole::System,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                pane_id: turn.pane_id.clone(),
                content: event.to_transcript_content(),
            };
            entry.validate()?;
            entries.push(entry);
            sequence = sequence.saturating_add(1);
        }
        Ok(())
    }

    /// Returns the summarized routed handoff selected for durable replay.
    ///
    /// The exact worker output and presentation-only instructions use different
    /// labels and are deliberately excluded. The summary block exists only on
    /// the parent presentation turn while transcript persistence is running.
    fn routed_handoff_transcript_content(&self, turn_id: &str) -> Option<String> {
        self.agent_turn_contexts()
            .get(turn_id)?
            .blocks()
            .iter()
            .rev()
            .find(|block| {
                block.source == ContextSourceKind::RoutedHandoff
                    && block.label == "routed worker handoff context"
                    && !block.content.trim().is_empty()
            })
            .map(|block| block.content.clone())
    }

    /// Inserts one typed routed-handoff event immediately before the visible
    /// parent assistant entry and advances later sequence numbers.
    fn insert_routed_handoff_transcript_event(
        entries: &mut Vec<TranscriptEntry>,
        conversation_id: &str,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
        content: String,
    ) -> Result<()> {
        let assistant_index = entries
            .iter()
            .position(|entry| entry.role == TranscriptRole::Assistant)
            .ok_or_else(|| {
                MezError::invalid_state(
                    "routed presentation transcript is missing its assistant entry",
                )
            })?;
        let sequence = entries[assistant_index].sequence;
        for entry in &mut entries[assistant_index..] {
            entry.sequence = entry.sequence.saturating_add(1);
        }
        let entry = TranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence,
            created_at_unix_seconds,
            role: TranscriptRole::System,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            content: TranscriptContextEvent::RoutedHandoff { content }.to_transcript_content(),
        };
        entry.validate()?;
        entries.insert(assistant_index, entry);
        Ok(())
    }

    /// Builds the one-time system transcript entry that makes saved sessions
    /// self-describing in `/resume` flows.
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
    pub(crate) fn record_agent_copy_output(
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
    pub(crate) fn record_agent_provider_token_usage(
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
    pub(crate) fn record_agent_provider_token_usage_with_profile(
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
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .unwrap_or_else(|| format!("pane:{pane_id}"));
        let token_usage_key = profile
            .map(|profile| ModelTokenUsageKey::new(profile.provider.clone(), profile.model.clone()))
            .unwrap_or_else(ModelTokenUsageKey::unknown);
        self.record_durable_token_usage(&token_usage_key, usage, current_unix_seconds());
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
            let profile_key = ModelTokenUsageKey::new(&profile.provider, &profile.model);
            let context_usage = if latest_context_usage.input_tokens > 0 {
                self.agent
                    .agent_latest_request_usage_by_conversation
                    .insert(
                        conversation_id.clone(),
                        mez_agent::LatestModelRequestUsage {
                            model: profile_key.clone(),
                            usage: latest_context_usage,
                        },
                    );
                Some(latest_context_usage)
            } else {
                self.agent
                    .agent_latest_request_usage_by_conversation
                    .get(&conversation_id)
                    .filter(|sample| sample.model == profile_key)
                    .map(|sample| sample.usage)
            };
            if let Some(snapshot) = context_usage
                .and_then(|usage| mez_agent::agent_context_usage_snapshot(profile, usage))
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
                if context_usage.is_none() {
                    self.agent
                        .agent_latest_request_usage_by_conversation
                        .remove(&conversation_id);
                }
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
    pub(crate) fn record_agent_provider_token_usage_by_model(
        &mut self,
        pane_id: &str,
        usage_by_model: &BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    ) {
        if usage_by_model.is_empty() {
            return;
        }
        let observed_at_unix_seconds = current_unix_seconds();
        for (key, usage) in usage_by_model {
            if !usage.is_zero() {
                self.record_durable_token_usage(key, *usage, observed_at_unix_seconds);
            }
        }
        let conversation_id = self
            .agent_shell_store()
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

    /// Best-effort records one settled provider usage delta without affecting
    /// provider response settlement or retry behavior.
    fn record_durable_token_usage(
        &mut self,
        model: &ModelTokenUsageKey,
        usage: ModelTokenUsage,
        observed_at_unix_seconds: u64,
    ) {
        let Some(store) = self.persistence.cloned_token_usage_store() else {
            return;
        };
        let event = TokenUsageEvent {
            id: new_token_usage_event_id(),
            observed_at_unix_seconds,
            model: model.clone(),
            usage,
        };
        if self.persistence.token_usage_uses_adapter() {
            self.persistence
                .queue_token_usage(RuntimeSideEffect::PersistTokenUsage { store, event });
            return;
        }
        match store.append(&event) {
            Ok(_) => self.persistence.clear_token_usage_health_error(),
            Err(_) => self.persistence.set_token_usage_health_error(
                "persistent token accounting is degraded after a storage write failure",
            ),
        }
    }

    /// Stores the latest provider-reported quota usage for the active pane conversation.
    pub(crate) fn record_agent_provider_quota_usage(
        &mut self,
        pane_id: &str,
        quota_usage: &[ProviderQuotaUsage],
    ) {
        if quota_usage.is_empty() {
            return;
        }
        let conversation_id = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .unwrap_or_else(|| format!("pane:{pane_id}"));
        self.agent
            .agent_quota_usage_by_conversation
            .insert(conversation_id, quota_usage.to_vec());
        let _ = self.checkpoint_agent_session_metadata();
    }
}
