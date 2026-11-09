//! Actor-captured manual compaction source-preparation ownership.
//!
//! Exact generation/epoch, conversation, configuration and pane incarnation are
//! captured before source I/O. A finite permit remains with cloned work through
//! actual worker retirement, not merely logical cancellation. The worker decodes
//! source/history without consulting live service state and performs no model
//! dispatch or new compaction publication. Existing settled terminal handoffs
//! reconcile before one leased source/history capture through the admitted
//! logical frontier. Actor adoption remains the only scheduling authority.
//! Durable decoding and immutable context/request rendering use separate worker
//! phases under the same logical owner. Live context/policy capture and source
//! eligibility remain actor-owned; no worker may rediscover or widen authority.

use super::*;
use crate::runtime::control::{RuntimeAgentPromptHistoryWork, RuntimeAgentTranscriptSnapshot};

mod request;
pub(crate) use request::RuntimeManualCompactionRequestWork;

/// Immutable source work and exact actor owner for one manual operation.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeManualCompactionPreparation {
    /// Originating pane, never retargeted by a worker.
    pub(crate) pane_id: String,
    /// Original conversation identity.
    pub(crate) conversation_id: String,
    /// Preparation generation that must still own the pane at adoption.
    pub(crate) task_generation: u64,
    /// Logical operation epoch, retained when provider work is queued.
    pub(crate) compaction_epoch: u64,
    /// Configuration freshness at admission.
    pub(crate) config_generation: u64,
    /// Pane-effective policy captured before any source worker is admitted.
    pub(crate) mode: crate::runtime::config::CompactionMode,
    /// Loaded layer bytes/trust, including disk-discovered overlays that need not
    /// advance the terminal session generation on every reconciliation.
    pub(crate) config_layers: Vec<crate::config::ConfigLayer>,
    /// Original active raw-tail row count.
    pub(crate) transcript_entries: u64,
    /// Frozen logical frontier, independent of the retained raw row count.
    pub(crate) transcript_high_water: u64,
    /// Save policy belongs to the admitted source, even at equal zero counts.
    pub(crate) ephemeral: bool,
    /// Original kernel pane incarnation, when a local root exists.
    pub(crate) process: Option<crate::runtime::processes::RuntimePaneProcessIdentity>,
    /// Installed store identity/settings captured before worker I/O.
    pub(crate) store: Option<crate::storage::transcript::AgentTranscriptStore>,
    /// No live service lookup is possible during durable source decoding.
    pub(super) source: source::ManualCompactionSourceWork,
    /// Immutable prior summary/history projection captured for context assembly.
    pub(crate) history: RuntimeAgentPromptHistoryWork,
    /// Retains finite worker capacity even after cancellation of its logical owner.
    pub(crate) _permit: std::sync::Arc<tokio::sync::OwnedSemaphorePermit>,
    /// Explicit pre-source worker gate; absent from production work.
    #[cfg(test)]
    pub(crate) probe: Option<(
        std::sync::Arc<tokio::sync::Notify>,
        std::sync::Arc<tokio::sync::Notify>,
        std::sync::Arc<tokio::sync::Notify>,
    )>,
}

impl RuntimeManualCompactionPreparation {
    /// Performs captured durable decoding with no live actor access, writes, or
    /// provider call. Corrupt source remains an error rather than a no-work skip.
    pub(crate) fn execute_source(&self) -> Result<RuntimeAgentTranscriptSnapshot> {
        self.source
            .clone()
            .execute_with_history(self.history.clone(), self.transcript_high_water)
    }
}

impl RuntimeSessionService {
    /// Distinguishes actual source absence from a zero display count. Saved
    /// bindings still need their typed-head probe. An explicitly empty NoStore
    /// window without a frozen owner must not inherit a saved namesake's head.
    pub(super) fn manual_source_is_known_empty(&self, pane: &str) -> bool {
        let Some(session) = self.agent_shell_store().get(pane) else {
            return false;
        };
        if session.ephemeral
            && self
                .frozen_replay_binding(pane, &session.session_id)
                .is_some()
        {
            return false;
        }
        self.persistence.transcript_store().is_none()
            || session.ephemeral
                && session.ephemeral_transcript_source_entries == 0
                && session
                    .ephemeral_transcript_source_conversation_id
                    .is_none()
    }

    /// Admits exact preparation ownership before durable decoding. Empty logical
    /// history retains the synchronous no-work response; other operations become
    /// visibly preparing while worker admission and source I/O are outstanding.
    pub(super) fn admit_manual_compaction_preparation(
        &mut self,
        pane_id: &str,
    ) -> Result<AgentShellCommandOutcome> {
        let session = self
            .agent_shell_store()
            .get(pane_id)
            .ok_or_else(|| MezError::invalid_state("manual compaction session unavailable"))?;
        if session.transcript_entries == 0 && self.manual_source_is_known_empty(pane_id) {
            return self.queue_manual_compaction(pane_id, None);
        }
        if session.running_turn_id.is_some() || self.agent_is_compacting(pane_id) {
            return Err(MezError::conflict(
                "manual compaction requires an idle, non-compacting conversation",
            ));
        }
        let conversation_id = session.session_id.clone();
        let transcript_entries = session.transcript_entries;
        let transcript_high_water = session.transcript_high_water;
        let ephemeral = session.ephemeral;
        let visibility = session.visibility;
        self.prepare_agent_context_prerequisites(pane_id, true)?;
        let permit = self.reserve_agent_source_preparation()?;
        let process = self.steering_process_binding(pane_id)?;
        let store = self.persistence.transcript_store().cloned();
        let work = RuntimeManualCompactionPreparation {
            pane_id: pane_id.to_string(),
            conversation_id: conversation_id.clone(),
            task_generation: 0,
            compaction_epoch: 0,
            config_generation: self.session.config_generation,
            config_layers: self.integration.config_layers().to_vec(),
            mode: self.effective_agent_compaction_mode(pane_id),
            transcript_entries,
            transcript_high_water,
            ephemeral,
            process,
            store,
            source: source::ManualCompactionSourceWork::capture(
                self.persistence.transcript_store().cloned(),
                conversation_id.clone(),
            ),
            history: self.prepare_runtime_agent_prompt_history_work(pane_id),
            _permit: permit,
            #[cfg(test)]
            probe: self.manual_compaction_preparation_probe_for_tests(),
        };
        self.queue_manual_compaction_preparation(work)?;
        let _ = self.append_agent_status_text_to_terminal_buffer(
            pane_id,
            "agent: compacting; preparing conversation source",
        );
        Ok(AgentShellCommandOutcome::Mutated {
            command: "compact".into(),
            visibility,
            body: format!(
                "pane={} conversation={} previous_transcript_entries={} compacted=false state=preparing source=model-compact trigger=manual",
                json_escape(pane_id),
                json_escape(&conversation_id),
                transcript_entries
            ),
        })
    }

    /// Adopts source only while exact generation, configuration, conversation,
    /// installed store and root incarnation remain current. Stale results never
    /// clear a newer compactor or dispatch provider work. Terminal failure/no-work
    /// clears only this owner and resumes its accepted steering exactly once.
    pub(crate) fn complete_manual_compaction_preparation(
        &mut self,
        work: &RuntimeManualCompactionPreparation,
        result: Result<RuntimeAgentTranscriptSnapshot>,
    ) -> Result<bool> {
        if !self.agent_compaction_task_is_current(&work.pane_id, work.task_generation) {
            return Ok(false);
        }
        let started = self
            .agent_compaction_started_at(&work.pane_id)
            .unwrap_or_else(crate::runtime::current_unix_seconds);
        let freshness = self.validate_manual_compaction_owner(work);
        let outcome = freshness
            .and(result)
            .and_then(|snapshot| {
                self.queue_manual_compaction(&work.pane_id, Some((snapshot, work.clone())))
            })
            .map_err(|error| manual_preparation_failure(work, started, error));
        if outcome.is_err() || matches!(outcome, Ok(AgentShellCommandOutcome::Display { .. })) {
            self.fail_agent_compaction_task(&work.pane_id, work.task_generation);
        }
        if let Err(error) = &outcome {
            let _ = self.append_compaction_failure_status(
                &work.pane_id,
                "compact preparation failed",
                error,
            );
        }
        if !self.agent_is_compacting(&work.pane_id) {
            self.resume_agent_compaction_steering(&work.pane_id)?;
        }
        outcome.map(|_| true)
    }

    /// Revalidates transcript-free live prerequisites and the exact captured
    /// owner at every preparation adoption boundary. This does not read source.
    fn validate_manual_compaction_owner(
        &mut self,
        work: &RuntimeManualCompactionPreparation,
    ) -> Result<()> {
        self.prepare_agent_context_prerequisites(&work.pane_id, true)?;
        let session = self
            .agent_shell_store()
            .get(&work.pane_id)
            .ok_or_else(|| MezError::conflict("compaction conversation was removed"))?;
        if session.session_id != work.conversation_id
            || session.ephemeral != work.ephemeral
            || session.transcript_entries != work.transcript_entries
            || session.transcript_high_water != work.transcript_high_water
            || self.session.config_generation != work.config_generation
            || self.integration.config_layers() != work.config_layers.as_slice()
            || self.persistence.transcript_store() != work.store.as_ref()
            || session
                .ephemeral
                .then(|| self.frozen_replay_binding(&work.pane_id, &session.session_id))
                .flatten()
                != work.history.frozen_replay.as_ref()
        {
            return Err(MezError::conflict("compaction preparation became stale"));
        }
        let current = self.steering_process_binding(&work.pane_id)?;
        let same = match (work.process.as_ref(), current.as_ref()) {
            (Some(old), Some(current)) => old.same_incarnation(current),
            (None, None) => true,
            _ => false,
        };
        if !same {
            return Err(MezError::conflict("compaction pane incarnation changed"));
        }
        Ok(())
    }

    /// Adopts a rendered request only under the original logical preparation.
    /// Errors/cancellation never redispatch source or replay command input.
    /// Provider work continues through the established compactor queue/claims.
    pub(crate) fn complete_manual_compaction_request(
        &mut self,
        work: &RuntimeManualCompactionRequestWork,
        result: Result<RuntimeAgentCompactionTask>,
    ) -> Result<bool> {
        let owner = &work.owner;
        if !self.agent_compaction_task_is_current(&owner.pane_id, owner.task_generation) {
            return Ok(false);
        }
        let started = self
            .agent_compaction_started_at(&owner.pane_id)
            .unwrap_or_else(crate::runtime::current_unix_seconds);
        let result = self.validate_manual_compaction_owner(owner).and(result);
        self.fail_agent_compaction_task(&owner.pane_id, owner.task_generation);
        let outcome = result.and_then(|task| {
            if task.pane_id != owner.pane_id || task.conversation_id != owner.conversation_id
                || task.compaction_epoch != owner.compaction_epoch || task.transcript_entries != owner.transcript_entries {
                return Err(MezError::conflict("manual rendered request owner changed"));
            }
            work.validate_task(&task)?;
            let status = format!("agent: compacting conversation summary trigger=manual provider={} model={} previous_transcript_entries={} summarized_entries={}",
                task.model_profile.provider,task.model_profile.model,task.transcript_entries,task.summarized_entries);
            self.queue_agent_compaction_task(task);
            let _ = self.append_agent_status_text_to_terminal_buffer(&owner.pane_id,&status);
            Ok(())
        });
        let outcome = outcome.map_err(|error| manual_preparation_failure(owner, started, error));
        if let Err(error) = &outcome {
            let _ = self.append_compaction_failure_status(
                &owner.pane_id,
                "compact request preparation failed",
                error,
            );
        }
        if !self.agent_is_compacting(&owner.pane_id) {
            self.resume_agent_compaction_steering(&owner.pane_id)?;
        }
        outcome.map(|()| true)
    }
}

/// Neither source decoding nor immutable request rendering can invoke a model.
/// Capture the original clock before logical retirement; these inert facts do
/// not certify physical retirement of the worker holding its finite permit.
fn manual_preparation_failure(
    owner: &RuntimeManualCompactionPreparation,
    started: u64,
    error: MezError,
) -> MezError {
    error.with_compaction_failure(crate::error::CompactionFailureEvidence {
        phase: "preparing",
        operation_epoch: owner.compaction_epoch,
        dispatch_generation: owner.task_generation,
        model_calls: Some(0),
        serial_depth: Some(0),
        elapsed_seconds: crate::runtime::current_unix_seconds().saturating_sub(started),
    })
}
