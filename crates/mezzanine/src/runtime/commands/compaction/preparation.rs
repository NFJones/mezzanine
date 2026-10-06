//! Actor-captured manual compaction source-preparation ownership.
//!
//! Exact generation/epoch, conversation, configuration and pane incarnation are
//! captured before source I/O. A finite permit remains with cloned work through
//! actual worker retirement, not merely logical cancellation. The worker decodes
//! source/history without consulting live service state and performs no model
//! dispatch or publication. Actor adoption remains the only scheduling authority.
//! This first preparation phase moves durable decoding; immutable request/context
//! construction is a later phase and must not be claimed off-actor here.

use super::*;
use crate::runtime::control::{RuntimeAgentPromptHistoryWork, RuntimeAgentTranscriptContext};

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
    /// Loaded layer bytes/trust, including disk-discovered overlays that need not
    /// advance the terminal session generation on every reconciliation.
    pub(crate) config_layers: Vec<crate::config::ConfigLayer>,
    /// Original logical transcript count.
    pub(crate) transcript_entries: u64,
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
    pub(crate) fn execute_source(
        &self,
    ) -> Result<(Vec<TranscriptEntry>, RuntimeAgentTranscriptContext)> {
        let rows = self.source.clone().execute()?;
        let history =
            crate::runtime::execute_runtime_agent_prompt_history_work(self.history.clone())?;
        Ok((rows, history))
    }
}

impl RuntimeSessionService {
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
        if session.transcript_entries == 0 {
            return self.queue_agent_shell_compaction_with_model(pane_id, "manual", None, None);
        }
        if session.running_turn_id.is_some() || self.agent_is_compacting(pane_id) {
            return Err(MezError::conflict(
                "manual compaction requires an idle, non-compacting conversation",
            ));
        }
        let conversation_id = session.session_id.clone();
        let transcript_entries = session.transcript_entries;
        let visibility = session.visibility;
        self.prepare_agent_context_prerequisites(pane_id, true)?;
        let permit = self.reserve_manual_compaction_preparation()?;
        let process = self.steering_process_binding(pane_id)?;
        let store = self.persistence.transcript_store().cloned();
        let work = RuntimeManualCompactionPreparation {
            pane_id: pane_id.to_string(),
            conversation_id: conversation_id.clone(),
            task_generation: 0,
            compaction_epoch: 0,
            config_generation: self.session.config_generation,
            config_layers: self.integration.config_layers().to_vec(),
            transcript_entries,
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
        result: Result<(Vec<TranscriptEntry>, RuntimeAgentTranscriptContext)>,
    ) -> Result<bool> {
        if !self.agent_compaction_task_is_current(&work.pane_id, work.task_generation) {
            return Ok(false);
        }
        let freshness = (|| -> Result<()> {
            self.prepare_agent_context_prerequisites(&work.pane_id, true)?;
            let session = self
                .agent_shell_store()
                .get(&work.pane_id)
                .ok_or_else(|| MezError::conflict("compaction conversation was removed"))?;
            if session.session_id != work.conversation_id
                || session.transcript_entries != work.transcript_entries
                || self.session.config_generation != work.config_generation
                || self.integration.config_layers() != work.config_layers.as_slice()
                || self.persistence.transcript_store() != work.store.as_ref()
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
        })();
        self.fail_agent_compaction_task(&work.pane_id, work.task_generation);
        let outcome = freshness.and(result).and_then(|(rows, history)| {
            self.queue_agent_shell_compaction_with_model(
                &work.pane_id,
                "manual",
                None,
                Some((rows, history, work.compaction_epoch)),
            )
        });
        if let Err(error) = &outcome {
            let _ = self.append_agent_error_text_to_terminal_buffer(
                &work.pane_id,
                &format!("agent: compact preparation failed: {}", error.message()),
            );
        }
        if !self.agent_is_compacting(&work.pane_id) {
            self.resume_agent_compaction_steering(&work.pane_id)?;
        }
        outcome.map(|_| true)
    }
}
