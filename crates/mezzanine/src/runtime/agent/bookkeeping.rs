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
use crate::storage::token_usage::new_token_usage_event_id;
use crate::storage::transcript::ConversationTranscriptRead;
use mez_agent::TranscriptContextEvent;

/// Maximum recent execution groups retained for in-process idempotency.
const RUNTIME_PERSISTED_EXECUTION_TRANSCRIPT_LIMIT: usize = 4096;

/// Immutable chronology read captured before execution or interruption bookkeeping.
/// The checked store projection owns archive integrity; queued rows remain logical only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeBookkeepingTranscriptReadWork {
    store: crate::storage::transcript::AgentTranscriptStore,
    conversation_id: String,
    pane_id: String,
    turn_id: String,
    transcript_entries: u64,
    compaction_epoch: u64,
    committed_prefix_required: bool,
    pending: Vec<TranscriptEntry>,
}

impl RuntimeBookkeepingTranscriptReadWork {
    /// Rejects history after its captured pane changes conversations.
    pub(crate) fn check_owner(&self, service: &RuntimeSessionService) -> Result<()> {
        if service
            .agent_shell_store()
            .get(&self.pane_id)
            .is_none_or(|session| {
                session.session_id != self.conversation_id
                    || session.transcript_entries != self.transcript_entries
            })
            || service.agent_compaction_epoch(&self.pane_id) != self.compaction_epoch
        {
            return Err(MezError::invalid_state(
                "bookkeeping conversation or history changed before transcript acceptance",
            ));
        }
        Ok(())
    }

    /// Reads one coherent logical history without accessing live runtime state.
    pub(crate) fn execute(&self) -> Result<Vec<TranscriptEntry>> {
        Ok(self
            .store
            .conversation_transcript_view(
                &self.conversation_id,
                ConversationTranscriptRead::ForTurn(&self.turn_id),
                self.committed_prefix_required,
                &self.pending,
            )?
            .logical)
    }

    /// Reads the store sequence on the same worker that checks turn chronology.
    pub(crate) fn next_sequence(&self) -> Result<u64> {
        next_transcript_sequence(&self.store, &self.conversation_id)
    }
}

/// Immutable chronology retained independently of terminal turn cleanup.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeBookkeepingCandidate {
    pub(crate) generation: u64,
    pub(crate) turn: AgentTurnRecord,
    /// Conversation evidence retained even if its pane is replaced or removed.
    pub(crate) read: RuntimeBookkeepingTranscriptReadWork,
    pub(crate) persistence_key: (String, String),
    pub(crate) entries: Vec<TranscriptEntry>,
    pub(crate) directory: Option<TranscriptEntry>,
    pub(crate) blocked: bool,
}

/// Exact candidate and actor-captured history checked by a blocking worker.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeBookkeepingCandidateWork {
    pub(crate) candidate: RuntimeBookkeepingCandidate,
    pub(crate) read: RuntimeBookkeepingTranscriptReadWork,
}

impl RuntimeBookkeepingCandidateWork {
    /// Returns checked turn history and store sequence without accessing actor state.
    pub(crate) fn execute(&self) -> Result<(Vec<TranscriptEntry>, u64)> {
        Ok((self.read.execute()?, self.read.next_sequence()?))
    }
}

impl RuntimeSessionService {
    /// Claims ordered chronology candidates using current accepted pending rows.
    pub(crate) fn claim_bookkeeping_candidates(&mut self) -> Vec<RuntimeBookkeepingCandidateWork> {
        self.persistence
            .claim_bookkeeping_candidates()
            .into_iter()
            .map(|candidate| {
                let mut read = if self
                    .agent_shell_store()
                    .get(&candidate.turn.pane_id)
                    .is_some_and(|session| session.session_id == candidate.turn.conversation_id)
                {
                    self.capture_bookkeeping_transcript_read(
                        candidate.read.store.clone(),
                        &candidate.turn,
                    )
                } else {
                    candidate.read.clone()
                };
                read.pending.extend(
                    self.persistence
                        .pending_transcript_entries(&candidate.turn.conversation_id),
                );
                RuntimeBookkeepingCandidateWork { candidate, read }
            })
            .collect()
    }

    /// Accepts one checked candidate, preserving exact owner and append sequencing.
    pub(crate) fn complete_bookkeeping_candidate(
        &mut self,
        work: RuntimeBookkeepingCandidateWork,
        history: Result<(Vec<TranscriptEntry>, u64)>,
    ) -> Result<bool> {
        let Some(mut candidate) = self
            .persistence
            .take_bookkeeping_candidate(work.candidate.generation)
        else {
            return Ok(false);
        };
        let turn = candidate.turn.clone();
        let still_bound = self
            .agent_shell_store()
            .get(&turn.pane_id)
            .is_some_and(|session| session.session_id == turn.conversation_id);
        let (history, sequence) = match history.and_then(|history| {
            if still_bound {
                work.read.check_owner(self)?;
            }
            Ok(history)
        }) {
            Ok(history) => history,
            Err(error) => {
                self.persistence.block_bookkeeping_candidate(candidate);
                self.append_agent_error_text_to_terminal_buffer(
                    &turn.pane_id,
                    &format!("agent: transcript bookkeeping blocked: {}", error.message()),
                )?;
                return Err(error);
            }
        };
        let first_sequence = self
            .persistence
            .deferred_transcript_next_sequence(&turn.conversation_id)
            .unwrap_or(sequence)
            .max(sequence);
        if first_sequence == 1
            && let Some(directory) = candidate.directory.take()
        {
            candidate.entries.insert(0, directory);
        }
        let entries =
            Self::new_runtime_transcript_entries(candidate.entries, &history, first_sequence);
        if entries.is_empty() {
            return Ok(true);
        }
        let store = work.read.store;
        self.persistence
            .queue_transcript(RuntimeSideEffect::PersistTranscriptEntries {
                path: store.transcript_path(&turn.conversation_id)?,
                store,
                entries: entries.clone(),
            });
        self.agent
            .agent_persisted_execution_transcripts
            .insert(candidate.persistence_key);
        if still_bound {
            self.agent_shell_store_mut()
                .record_transcript_entries(&turn.pane_id, entries.len())?;
            self.record_pane_transcript_ref(
                &turn.pane_id,
                format!("transcript:{}:{}", turn.pane_id, turn.conversation_id),
            )?;
        }
        if first_sequence == 1 {
            self.queue_saved_session_retention_operation(current_unix_seconds().max(1), false)?;
        }
        Ok(true)
    }

    /// Captures the current transcript owner and pending receipts for checked bookkeeping.
    pub(crate) fn capture_bookkeeping_transcript_read(
        &self,
        store: crate::storage::transcript::AgentTranscriptStore,
        turn: &AgentTurnRecord,
    ) -> RuntimeBookkeepingTranscriptReadWork {
        let pending = self
            .persistence
            .pending_transcript_entries(&turn.conversation_id);
        let committed_prefix_required =
            self.agent_shell_store()
                .get(&turn.pane_id)
                .is_some_and(|session| {
                    session.transcript_entries
                        > pending
                            .iter()
                            .map(|entry| entry.sequence)
                            .collect::<std::collections::BTreeSet<_>>()
                            .len() as u64
                });
        RuntimeBookkeepingTranscriptReadWork {
            store,
            conversation_id: turn.conversation_id.clone(),
            pane_id: turn.pane_id.clone(),
            turn_id: turn.turn_id.clone(),
            transcript_entries: self
                .agent_shell_store()
                .get(&turn.pane_id)
                .map_or(0, |session| session.transcript_entries),
            compaction_epoch: self.agent_compaction_epoch(&turn.pane_id),
            committed_prefix_required,
            pending,
        }
    }

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
        self.persist_runtime_agent_turn_execution_transcript_with_read(turn, execution, None)
    }

    /// Accepts worker-checked chronology and sequence evidence before queuing an append.
    /// The synchronous path is retained for callers without prepared worker history.
    pub(crate) fn persist_runtime_agent_turn_execution_transcript_with_read(
        &mut self,
        turn: &AgentTurnRecord,
        execution: &AgentTurnExecution,
        prepared: Option<(
            RuntimeBookkeepingTranscriptReadWork,
            Vec<TranscriptEntry>,
            u64,
        )>,
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
        if prepared.is_none() && self.persistence.transcript_uses_adapter() {
            let timestamp = current_unix_seconds().max(1);
            let entries = self.runtime_transcript_entries_for_execution(
                &conversation_id,
                2,
                timestamp,
                turn,
                execution,
            )?;
            let directory = self.runtime_session_directory_transcript_entry(
                &conversation_id,
                1,
                timestamp,
                turn,
            );
            self.persistence
                .queue_bookkeeping_candidate(RuntimeBookkeepingCandidate {
                    generation: 0,
                    turn: turn.clone(),
                    read: self.capture_bookkeeping_transcript_read(store.clone(), turn),
                    persistence_key,
                    entries,
                    directory,
                    blocked: false,
                });
            return Ok(0);
        }
        let (read, existing_entries, prepared_sequence) =
            if let Some((read, entries, sequence)) = prepared {
                (read, entries, Some(sequence))
            } else {
                let read = self.capture_bookkeeping_transcript_read(store.clone(), turn);
                let entries = read.execute()?;
                (read, entries, None)
            };
        read.check_owner(self)?;
        let created_at_unix_seconds = current_unix_seconds().max(1);
        let entries = if self.persistence.transcript_uses_adapter() {
            let first_sequence = self
                .persistence
                .deferred_transcript_next_sequence(&conversation_id)
                .or(prepared_sequence)
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
            match store.append_many(&entries) {
                Ok(_) => {}
                Err(error) if error.local_transcript_precommit_retryable() => {
                    // The store proved that no row was written. Retry only the
                    // identical local append, never the accepted execution.
                    store.append_many(&entries)?;
                }
                Err(error) => return Err(error),
            }
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

    /// Persists an MCP authorization-reset boundary and fresh safe catalog after compaction.
    pub(crate) fn persist_mcp_compaction_epoch_transcript(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        catalog_snapshot: Option<String>,
    ) -> Result<usize> {
        let Some(session) = self.agent_shell_store().get(pane_id) else {
            return Ok(0);
        };
        if session.session_id != conversation_id || session.ephemeral {
            return Ok(0);
        }
        let Some(store) = self.persistence.cloned_transcript_store() else {
            return Ok(0);
        };
        let sequence = if self.persistence.transcript_uses_adapter() {
            self.persistence
                .deferred_transcript_next_sequence(conversation_id)
                .map(Ok)
                .unwrap_or_else(|| next_transcript_sequence(&store, conversation_id))?
        } else {
            next_transcript_sequence(&store, conversation_id)?
        };
        let entry = TranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence,
            created_at_unix_seconds: current_unix_seconds().max(1),
            role: TranscriptRole::System,
            turn_id: format!("compact-{conversation_id}-{sequence}"),
            agent_id: format!("agent-{pane_id}"),
            pane_id: pane_id.to_string(),
            content: TranscriptContextEvent::McpCompactionEpoch.to_transcript_content(),
        };
        entry.validate()?;
        let mut entries = vec![entry];
        if let Some(catalog_snapshot) = catalog_snapshot {
            let catalog_event = TranscriptContextEvent::mcp_catalog_snapshot(
                catalog_snapshot,
                sequence.saturating_add(1),
            )
            .ok_or_else(|| {
                MezError::invalid_state("compaction MCP catalog snapshot is empty or oversized")
            })?;
            let catalog_entry = TranscriptEntry {
                conversation_id: conversation_id.to_string(),
                sequence: sequence.saturating_add(1),
                created_at_unix_seconds: current_unix_seconds().max(1),
                role: TranscriptRole::System,
                turn_id: format!("compact-{conversation_id}-{sequence}"),
                agent_id: format!("agent-{pane_id}"),
                pane_id: pane_id.to_string(),
                content: catalog_event.to_transcript_content(),
            };
            catalog_entry.validate()?;
            entries.push(catalog_entry);
        }
        let guidance_event = TranscriptContextEvent::prompt_boundary(
            ContextSourceKind::Configuration,
            "MCP compaction re-retrieval guidance",
            "Conversation compaction cleared previously retrieved MCP tool contracts. Before using mcp_call, retrieve the needed server again with mcp_server_get; existing directory, reference, and search evidence may still make a server referencable.",
        )
        .ok_or_else(|| {
            MezError::invalid_state("compaction MCP re-retrieval guidance is invalid")
        })?;
        let guidance_entry = TranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence: sequence.saturating_add(entries.len() as u64),
            created_at_unix_seconds: current_unix_seconds().max(1),
            role: TranscriptRole::System,
            turn_id: format!("compact-{conversation_id}-{sequence}"),
            agent_id: format!("agent-{pane_id}"),
            pane_id: pane_id.to_string(),
            content: guidance_event.to_transcript_content(),
        };
        guidance_entry.validate()?;
        entries.push(guidance_entry);
        if self.persistence.transcript_uses_adapter() {
            self.persistence
                .queue_transcript(RuntimeSideEffect::PersistTranscriptEntries {
                    path: store.transcript_path(conversation_id)?,
                    store,
                    entries: entries.clone(),
                });
        } else {
            store.append_many(&entries)?;
        }
        self.record_pane_transcript_ref(
            pane_id,
            format!("transcript:{pane_id}:{conversation_id}"),
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
        let owner = if matches!(
            TranscriptContextEvent::from_transcript_content(&entry.content),
            Some(TranscriptContextEvent::McpCatalogSnapshot { .. })
        ) {
            String::new()
        } else {
            entry.turn_id.clone()
        };
        (owner, role, entry.content.clone())
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
            // Candidate sequences are placeholders until checked actor acceptance.
            1
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
        self.append_exact_execution_block_transcript_events(
            &mut entries,
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
            self.persistence
                .queue_bookkeeping_candidate(RuntimeBookkeepingCandidate {
                    generation: 0,
                    turn: turn.clone(),
                    read: self.capture_bookkeeping_transcript_read(store.clone(), turn),
                    persistence_key,
                    entries,
                    directory: None,
                    blocked: false,
                });
            return Ok(0);
        }
        let read = self.capture_bookkeeping_transcript_read(store.clone(), turn);
        let existing_entries = read.execute()?;
        read.check_owner(self)?;
        entries = Self::new_runtime_transcript_entries(entries, &existing_entries, first_sequence);
        if entries.is_empty() {
            return Ok(0);
        }
        if self.persistence.transcript_uses_adapter() {
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
                turn_id: execution.request.turn_id.clone(),
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
        // Canonical chronology owns every user-event occurrence. Request
        // projections cannot distinguish identical prompt and steering text,
        // so exclude them rather than backdating or coalescing user events.
        execution_entries.retain(|entry| entry.role != TranscriptRole::User);
        self.insert_initial_user_transcript_event(
            &mut execution_entries,
            conversation_id,
            sequence,
            created_at_unix_seconds,
            turn,
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
            sequence,
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

    /// Inserts the canonical initial prompt before its display projection.
    fn insert_initial_user_transcript_event(
        &self,
        entries: &mut Vec<TranscriptEntry>,
        conversation_id: &str,
        sequence: u64,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        let Some(event) = self
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .and_then(|context| {
                context.chronology().iter().find(|event| {
                    event.block().source == ContextSourceKind::UserInstruction
                        && event.block().label == "user prompt"
                })
            })
        else {
            return Ok(());
        };
        let transcript_event = TranscriptContextEvent::user_event(
            event.sequence().get(),
            event.block().label.clone(),
            event.block().content.clone(),
        )
        .ok_or_else(|| MezError::invalid_state("turn initial user event is invalid"))?;
        let entry = TranscriptEntry {
            conversation_id: conversation_id.to_string(),
            sequence,
            created_at_unix_seconds,
            role: TranscriptRole::System,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            content: transcript_event.to_transcript_content(),
        };
        entry.validate()?;
        entries.insert(0, entry);
        Ok(())
    }

    /// Inserts exact prompt-boundary context before its owning user entry.
    fn insert_prompt_boundary_transcript_events(
        &self,
        entries: &mut Vec<TranscriptEntry>,
        conversation_id: &str,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        let Some(insertion_index) = entries.iter().position(|entry| {
            entry.role == TranscriptRole::User
                || matches!(
                    TranscriptContextEvent::from_transcript_content(&entry.content),
                    Some(TranscriptContextEvent::UserEvent { .. })
                )
        }) else {
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
        let imported_history_sequence_high_water =
            self.agent_turn_imported_history_sequence_high_water(&turn.turn_id);
        let mut sequence = first_sequence;
        let mut entries = Vec::new();
        for event in context
            .chronology()
            .iter()
            .filter(|event| event.sequence().get() > imported_history_sequence_high_water)
        {
            let block = event.block();
            if block.source == ContextSourceKind::UserInstruction && block.label == "user prompt" {
                break;
            }
            let event = if block.source == ContextSourceKind::Configuration
                && block.label == "task environment snapshot"
            {
                TranscriptContextEvent::environment_snapshot(block.content.clone()).ok_or_else(
                    || MezError::invalid_state("turn environment snapshot is empty or oversized"),
                )?
            } else if block.source == ContextSourceKind::McpCatalogSnapshot
                && block.label == mez_agent::MCP_CATALOG_SNAPSHOT_CONTEXT_LABEL
            {
                TranscriptContextEvent::mcp_catalog_snapshot(
                    block.content.clone(),
                    event.sequence().get(),
                )
                .ok_or_else(|| {
                    MezError::invalid_state("turn MCP catalog snapshot is empty or oversized")
                })?
            } else if matches!(
                block.source,
                ContextSourceKind::SkillInstruction
                    | ContextSourceKind::LocalMessage
                    | ContextSourceKind::PeerMessage
                    | ContextSourceKind::Policy
                    | ContextSourceKind::Configuration
            ) {
                TranscriptContextEvent::prompt_boundary_with_event_sequence(
                    block.source,
                    event.sequence().get(),
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

    /// Appends canonical user events and exact execution blocks after the
    /// ordinary display transcript projection for one completed turn.
    fn append_exact_execution_block_transcript_events(
        &self,
        entries: &mut Vec<TranscriptEntry>,
        conversation_id: &str,
        first_sequence: u64,
        created_at_unix_seconds: u64,
        turn: &AgentTurnRecord,
    ) -> Result<()> {
        let Some(context) = self.agent_turn_contexts().get(&turn.turn_id) else {
            return Ok(());
        };
        let imported_history_sequence_high_water =
            self.agent_turn_imported_history_sequence_high_water(&turn.turn_id);
        let mut sequence = entries
            .last()
            .map_or(first_sequence, |entry| entry.sequence.saturating_add(1));
        let mut group_ordinals = BTreeMap::<String, u64>::new();
        let mut active_user_seen = false;
        for event in context
            .chronology()
            .iter()
            .filter(|event| event.sequence().get() > imported_history_sequence_high_water)
        {
            let block = event.block();
            if block.source == ContextSourceKind::UserInstruction {
                active_user_seen = true;
                if block.label == "user prompt" {
                    continue;
                }
                let transcript_event = TranscriptContextEvent::user_event(
                    event.sequence().get(),
                    block.label.clone(),
                    block.content.clone(),
                )
                .ok_or_else(|| {
                    MezError::invalid_state("turn user event is empty, oversized, or unsupported")
                })?;
                let entry = TranscriptEntry {
                    conversation_id: conversation_id.to_string(),
                    sequence,
                    created_at_unix_seconds,
                    role: TranscriptRole::System,
                    turn_id: turn.turn_id.clone(),
                    agent_id: turn.agent_id.clone(),
                    pane_id: turn.pane_id.clone(),
                    content: transcript_event.to_transcript_content(),
                };
                entry.validate()?;
                entries.push(entry);
                sequence = sequence.saturating_add(1);
                continue;
            }
            let transcript_event = if block.source == ContextSourceKind::McpCatalogSnapshot
                && active_user_seen
            {
                TranscriptContextEvent::mcp_catalog_snapshot(
                    block.content.clone(),
                    event.sequence().get(),
                )
            } else if active_user_seen
                && matches!(
                    block.source,
                    ContextSourceKind::LocalMessage
                        | ContextSourceKind::PeerMessage
                        | ContextSourceKind::Policy
                        | ContextSourceKind::Configuration
                )
            {
                TranscriptContextEvent::prompt_boundary_with_event_sequence(
                    block.source,
                    event.sequence().get(),
                    block.label.clone(),
                    block.content.clone(),
                )
            } else if !matches!(
                block.source,
                ContextSourceKind::CommittedEvidence
                    | ContextSourceKind::TranscriptAssistant
                    | ContextSourceKind::TranscriptTool
                    | ContextSourceKind::ActionResult
                    | ContextSourceKind::McpServerReference
                    | ContextSourceKind::McpServerSearchResult
                    | ContextSourceKind::McpRetrievedManifest
            ) {
                continue;
            } else if let Some(group) = event.execution_group_id() {
                let ordinal = group_ordinals
                    .entry(group.as_str().to_string())
                    .or_default();
                *ordinal = ordinal.saturating_add(1);
                TranscriptContextEvent::execution_block_with_metadata(
                    block.source,
                    block.label.clone(),
                    block.content.clone(),
                    group.clone(),
                    *ordinal,
                    event.provider_owner().cloned(),
                )
            } else {
                TranscriptContextEvent::execution_block(
                    block.source,
                    block.label.clone(),
                    block.content.clone(),
                )
            }
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
                content: transcript_event.to_transcript_content(),
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
        self.record_native_usage_observation(
            &conversation_id,
            Some(pane_id),
            &crate::storage::token_usage::AccountingOrigin::Unattributed,
            &token_usage_key,
            usage,
            new_token_usage_event_id(),
        );
        self.record_agent_latest_context_usage(&conversation_id, latest_context_usage, profile);
    }

    /// Updates only the accepted ordinary request sample, never cumulative expense.
    pub(super) fn record_agent_latest_context_usage(
        &mut self,
        conversation_id: &str,
        latest_context_usage: ModelTokenUsage,
        profile: Option<&ModelProfile>,
    ) {
        let conversation_id = conversation_id.to_string();
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
        let conversation_id = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .unwrap_or_else(|| format!("pane:{pane_id}"));
        self.record_agent_provider_token_usage_for_conversation(
            pane_id,
            &conversation_id,
            usage_by_model,
        );
    }

    /// Stores provider token usage against its originating conversation.
    pub(crate) fn record_agent_provider_token_usage_for_conversation(
        &mut self,
        pane_id: &str,
        conversation_id: &str,
        usage_by_model: &BTreeMap<ModelTokenUsageKey, ModelTokenUsage>,
    ) {
        if usage_by_model.is_empty() {
            return;
        }
        for (key, usage) in usage_by_model {
            self.record_native_usage_observation(
                conversation_id,
                Some(pane_id),
                &crate::storage::token_usage::AccountingOrigin::Unattributed,
                key,
                *usage,
                new_token_usage_event_id(),
            );
        }
    }

    /// Charges one accepted output-cutoff attempt without replacing the latest
    /// successful ordinary-execution input sample used for context display.
    pub(crate) fn record_agent_output_cutoff_usage(
        &mut self,
        turn: &AgentTurnRecord,
        error: &MezError,
    ) {
        let Some(state) = error.provider_output_limit_state() else {
            return;
        };
        let Some(profile) = self.agent_turn_model_profile(&turn.turn_id) else {
            return;
        };
        let key = ModelTokenUsageKey::new(&profile.provider, &profile.model);
        let usage = state.usage;
        if usage.is_zero() {
            return;
        }
        self.integration
            .runtime_metrics_mut()
            .record_provider_cumulative_token_usage(usage, &key);
        self.record_agent_provider_token_usage_for_conversation(
            &turn.pane_id,
            &turn.conversation_id,
            &BTreeMap::from([(key, usage)]),
        );
    }

    /// Settles exact issued title expense without touching latest request samples.
    /// Conversation/session totals survive cancellation; pane totals require the
    /// original conversation and root incarnation. Storage uses the attempt ID.
    pub(super) fn record_session_title_usage(
        &mut self,
        task: &super::RuntimeAgentSessionTitleTask,
        usage: ModelTokenUsage,
    ) {
        if usage.is_zero() {
            return;
        }
        let key = ModelTokenUsageKey::new(&task.model_profile.provider, &task.model_profile.model);
        self.integration
            .runtime_metrics_mut()
            .record_provider_cumulative_token_usage(usage, &key);
        let same_pane = self
            .agent_shell_store()
            .get(&task.pane_id)
            .is_some_and(|session| session.session_id == task.conversation_id)
            && task.pane_process.as_ref().is_some_and(|identity| {
                self.pane_process_identity(&task.pane_id)
                    .ok()
                    .is_some_and(|current| identity.same_incarnation(&current))
            });
        self.record_native_usage_observation(
            &task.conversation_id,
            same_pane.then_some(task.pane_id.as_str()),
            &task.accounting_origin,
            &key,
            usage,
            format!("title:{}", task.attempt_id),
        );
    }

    /// Stores the latest provider-reported quota usage for the active pane conversation.
    pub(crate) fn record_agent_provider_quota_usage(
        &mut self,
        pane_id: &str,
        quota_usage: &[ProviderQuotaUsage],
    ) {
        let conversation_id = self
            .agent_shell_store()
            .get(pane_id)
            .map(|session| session.session_id.clone())
            .unwrap_or_else(|| format!("pane:{pane_id}"));
        self.record_agent_provider_quota_usage_for_conversation(
            pane_id,
            &conversation_id,
            quota_usage,
        );
    }

    /// Stores provider quota usage against its originating conversation.
    pub(crate) fn record_agent_provider_quota_usage_for_conversation(
        &mut self,
        _pane_id: &str,
        conversation_id: &str,
        quota_usage: &[ProviderQuotaUsage],
    ) {
        if quota_usage.is_empty() {
            return;
        }
        self.agent
            .agent_quota_usage_by_conversation
            .insert(conversation_id.to_string(), quota_usage.to_vec());
        let _ = self.checkpoint_agent_session_metadata();
    }
}
