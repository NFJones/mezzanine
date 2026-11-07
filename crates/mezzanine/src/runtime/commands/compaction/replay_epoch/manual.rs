//! Manual commands select the same canonical source and publication as turns.
//!
//! Current trusted slots are assembled separately from immutable model history.
//! No command prompt is admitted to the source, no original task is retired,
//! and an idle completion never creates an ordinary provider continuation.

use super::super::{
    AgentShellCommandOutcome, AllowedActionSet, ModelProfile, NoWorkReason,
    RuntimeAgentCompactionTarget, RuntimeAgentCompactionTask, RuntimeManualCompactionPreparation,
    RuntimeManualCompactionRequestWork, model_context_text_word_count,
    runtime_limit_compaction_summary_output, runtime_model_compaction_request_for_blocks,
};
use super::*;

impl RuntimeSessionService {
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
        let source = match self
            .prepare_manual_replay_compaction(pane, owner, &name, &profile, original)?
        {
            Preparation::Ready(source) => source,
            Preparation::NoWork => {
                let reason = NoWorkReason::NoEligibleClosedPrefix;
                self.append_agent_status_text_to_terminal_buffer(pane, reason.status())?;
                self.record_manual_compaction_no_work(pane, &session.session_id, reason, "manual")?;
                return Ok(AgentShellCommandOutcome::Display {
                    command: "compact".into(),
                    body: format!(
                        "pane={pane} conversation={} compacted=false reason={} source=model-compact trigger=manual",
                        session.session_id,
                        reason.code()
                    ),
                });
            }
            Preparation::Unowned => {
                return Err(MezError::conflict("manual canonical source lost ownership"));
            }
        };
        let allowed = self.capture_agent_session_allowed_actions_for_pane(pane)?;
        let origin = self.capture_agent_accounting_origin_for_pane(pane);
        if let Some(preparation) = preparation {
            let work = RuntimeManualCompactionRequestWork::capture(
                preparation,
                name,
                profile,
                source.owner.clone().into_owned_context(),
                self.mcp_registry().prompt_summary(),
                allowed,
                Vec::new(),
                Vec::new(),
                false,
                None,
                session.transcript_entries,
                origin,
            )
            .with_replay_source(source);
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
    let blocks = source.input_blocks();
    let mut request = runtime_model_compaction_request_for_blocks(
        profile,
        pane,
        source.owner.conversation_id().as_str(),
        &blocks,
        allowed,
    )?;
    let words = request
        .messages
        .last()
        .map(|message| model_context_text_word_count(&message.content))
        .unwrap_or_default()
        .max(1);
    runtime_limit_compaction_summary_output(&mut request, words);
    let retry = request
        .messages
        .last()
        .map(|message| message.content.clone());
    Ok(RuntimeAgentCompactionTask {
        source_owner: Some(source.original.clone()),
        replay_source: Some(source.clone()),
        summary_proposal: None,
        frozen_compaction_epoch: None,
        task_generation: 0,
        compaction_epoch: operation_epoch,
        transport_retry_attempts: 0,
        transport_retry_delay_ms: None,
        summary_correction: crate::runtime::agent_state::RuntimeSummaryCorrection::for_manual_words(
            words,
        ),
        pane_id: pane.to_owned(),
        accounting_origin,
        conversation_id: source.owner.conversation_id().as_str().to_owned(),
        source: "manual".into(),
        transcript_entries,
        compacted_through_sequence: None,
        frozen_compaction_rows: Vec::new(),
        retained_transcript_entries: transcript_entries,
        summarized_entries: blocks.len(),
        model_profile_name: name.to_owned(),
        model_profile: profile.clone(),
        request,
        preserve_summary_output_budget: true,
        manual_retry_source: retry,
        manual_final_retry: None,
        candidate_context: Some(source.owner.clone().into_owned_context()),
        resume_turn_id: None,
        target: RuntimeAgentCompactionTarget::Conversation,
        conversation_chunks: None,
        compaction_request_shape: None,
    })
}
