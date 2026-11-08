//! Manual commands select the same canonical source and publication as turns.
//!
//! Current trusted slots are assembled separately from immutable model history.
//! No command prompt is admitted to the source, no original task is retired,
//! and an idle completion never creates an ordinary provider continuation.

use super::super::{
    AgentShellCommandOutcome, AllowedActionSet, ModelProfile, RuntimeAgentCompactionTask,
    RuntimeManualCompactionPreparation, RuntimeManualCompactionRequestWork,
    runtime_model_compaction_request_from_source,
};
use super::*;

impl RuntimeSessionService {
    /// Previews the frozen head/tail with the same current policy and next
    /// prompt composition used after publication. No history reread, ordinary
    /// turn, permission, append acknowledgment or live cache rewrite is created.
    #[cfg(test)]
    pub(in crate::runtime::commands::compaction) fn manual_replay_compaction_candidate_context(
        &mut self,
        task: &RuntimeAgentCompactionTask,
        summary: &str,
    ) -> Result<AgentContext> {
        self.validate_replay_compaction_task(task)?;
        let source = task.replay_source.as_ref().ok_or_else(|| {
            MezError::invalid_state("manual canonical candidate source disappeared")
        })?;
        if source.turn_id().is_some() {
            return Err(MezError::invalid_state(
                "manual candidate requires an idle canonical source",
            ));
        }
        let current = ReplayEpochContext::from_owned_context(
            ReplayConversationId::new(&task.conversation_id)?,
            source.current_body(self)?,
        )?;
        let proposed = current.prepare_compaction_publication(
            source.plan.clone(),
            ReplayHeadPublication::Summary(summary.into()),
        )?;
        let history = crate::runtime::control::RuntimeAgentTranscriptContext::for_replay_preview(
            proposed.candidate().clone(),
        )?;
        let steering = self.agent_compaction_steering_for_candidate(
            &task.pane_id,
            &task.conversation_id,
            task.compaction_epoch,
        );
        let prompt = if steering.is_empty() {
            "Continue after conversation compaction.".into()
        } else {
            steering.join("\n\n")
        };
        let mut context = self
            .agent_context_for_pane_prompt_with_history(&task.pane_id, &prompt, false, history)?
            .context;
        let mut metadata = context.metadata().clone();
        metadata.prompt_cache_lineage_id =
            Some("compaction-00000000-0000-0000-0000-000000000000".into());
        context.set_metadata(metadata);
        Ok(context)
    }

    pub(in crate::runtime::commands::compaction) fn queue_manual_replay_compaction(
        &mut self,
        pane: &str,
        mut owner: ReplayEpochContext,
        history: crate::runtime::control::RuntimeAgentTranscriptContext,
        preparation: Option<RuntimeManualCompactionPreparation>,
    ) -> Result<AgentShellCommandOutcome> {
        let session = self
            .agent_shell_store()
            .get(pane)
            .cloned()
            .ok_or_else(|| MezError::conflict("manual canonical conversation disappeared"))?;
        self.ensure_primary_agent_name(pane)?;
        self.refresh_project_trust_store_from_disk_if_changed()?;
        let original = history.source.clone();
        // Reuse current policy composition, but copy only its stable slots.
        // Its temporary request prompt and task transitions never enter source.
        let current = self
            .agent_context_for_pane_prompt_with_history(
                pane,
                "[context compaction requested]",
                false,
                history,
            )?
            .context;
        owner.replace_stable_slots(current.stable_slots().to_vec())?;
        let (name, profile) =
            self.active_model_profile_for_pane(pane, &format!("agent-{pane}"), None)?;
        let permit = preparation.as_ref().map(|work| work._permit.clone());
        let source = match self
            .prepare_manual_replay_compaction(pane, owner, &name, &profile, original, permit)?
        {
            Preparation::Ready(source) => source,
            Preparation::NoWork(reason) => {
                self.append_agent_status_text_to_terminal_buffer(pane, reason.status())?;
                self.record_manual_compaction_no_work(pane, &session.session_id, reason, "manual")?;
                return Ok(AgentShellCommandOutcome::Display {
                    command: "compact".into(),
                    body: format!(
                        "pane={pane} conversation={} previous_transcript_entries={} summarized_entries=0 compacted=false reason={} source=model-compact trigger=manual",
                        session.session_id,
                        session.transcript_entries,
                        reason.code()
                    ),
                });
            }
        };
        let allowed = self.capture_agent_session_allowed_actions_for_pane(pane)?;
        let origin = self.capture_agent_accounting_origin_for_pane(pane);
        if let Some(preparation) = preparation {
            let work = RuntimeManualCompactionRequestWork::capture(
                preparation,
                name,
                profile,
                source,
                allowed,
                origin,
            );
            #[cfg(test)]
            let work = work.with_probe(self.manual_compaction_request_probe_for_tests());
            self.queue_manual_compaction_request(work)?;
            return Ok(AgentShellCommandOutcome::Mutated {
                command: "compact".into(),
                visibility: session.visibility,
                body: format!(
                    "pane={pane} conversation={} compacted=false state=preparing phase=request source=model-compact trigger=manual",
                    session.session_id
                ),
            });
        }
        let task = manual_replay_task(
            pane,
            &name,
            &profile,
            source,
            0,
            session.transcript_entries,
            origin,
            allowed,
        )?;
        self.queue_agent_compaction_task(task);
        self.append_agent_status_text_to_terminal_buffer(
            pane,
            "agent: compacting canonical conversation history",
        )?;
        Ok(AgentShellCommandOutcome::Mutated {
            command: "compact".into(),
            visibility: session.visibility,
            body: format!(
                "pane={pane} conversation={} compacted=false state=queued source=model-compact trigger=manual",
                session.session_id
            ),
        })
    }
}

/// Immutable request rendering shared by inline and prepared manual commands.
/// Source/receipt ownership remains with the existing compactor lifecycle.
#[allow(
    clippy::too_many_arguments,
    reason = "original source, model, operation, expense and policy are independent inputs"
)]
pub(in crate::runtime::commands::compaction) fn manual_replay_task(
    pane: &str,
    name: &str,
    profile: &ModelProfile,
    source: Arc<RuntimeCompactionReplaySource>,
    operation_epoch: u64,
    transcript_entries: u64,
    accounting_origin: crate::storage::token_usage::AccountingOrigin,
    allowed: AllowedActionSet,
) -> Result<RuntimeAgentCompactionTask> {
    let request = runtime_model_compaction_request_from_source(
        profile,
        pane,
        source.owner.conversation_id().as_str(),
        String::new(),
        allowed,
    );
    Ok(RuntimeAgentCompactionTask {
        source_owner: Some(source.original.clone()),
        replay_source: Some(source.clone()),
        task_generation: 0,
        compaction_epoch: operation_epoch,
        transport_retry_attempts: 0,
        transport_retry_delay_ms: None,
        pane_id: pane.to_owned(),
        accounting_origin,
        conversation_id: source.owner.conversation_id().as_str().to_owned(),
        source: "manual".into(),
        transcript_entries,
        summarized_entries: source.plan.summary_input().len(),
        model_profile_name: name.to_owned(),
        model_profile: profile.clone(),
        request,
        resume_turn_id: None,
        compaction_request_shape: None,
    })
}
