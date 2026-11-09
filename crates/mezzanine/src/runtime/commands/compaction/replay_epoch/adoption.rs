//! Actor-only publication admission, bounded recovery and receipt adoption.
//! Only an accepted original receipt resets request/cache state. A stale or
//! failed callback cannot clear a replacement task or replay provider actions.

use super::*;
use crate::runtime::{RuntimeSideEffect, RuntimeTransition};

impl RuntimeSessionService {
    /// The common graph transfers a complete typed proof, never reconstructed
    /// from an arbitrary worker response. The publisher retains the identical
    /// frozen source plan and checks its live/store fences again at mutation.
    pub(in crate::runtime::commands::compaction) fn queue_accepted_replay_epoch_publication(
        &mut self,
        task: &crate::runtime::RuntimeAgentCompactionTask,
        accepted: &mez_agent::epoch_compaction::AcceptedEpochCompaction,
    ) -> Result<()> {
        let source = task
            .replay_source
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("accepted compaction has no original source"))?;
        if accepted.plan() != &source.plan {
            return Err(MezError::conflict(
                "accepted compaction belongs to another frozen epoch",
            ));
        }
        self.queue_replay_epoch_publication(
            task,
            ReplayHeadPublication::Summary(accepted.summary().to_owned()),
        )
    }

    pub(in crate::runtime::commands::compaction) fn validate_replay_compaction_task(
        &self,
        task: &crate::runtime::RuntimeAgentCompactionTask,
    ) -> Result<()> {
        let source = task
            .replay_source
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("canonical compaction source missing"))?;
        source.validate_live(self, &task.pane_id, task.task_generation)?;
        let accepted = self
            .retained_agent_compaction_task(&task.pane_id, task.task_generation)
            .ok_or_else(|| MezError::conflict("canonical compaction task is no longer retained"))?;
        if !task
            .source_owner
            .as_ref()
            .is_some_and(|owner| Arc::ptr_eq(owner, &source.original))
            || task.request.is_some() != source.plan.requires_model_summary()
            || task.model_profile != accepted.model_profile
            || task.model_profile_name != accepted.model_profile_name
        {
            return Err(MezError::conflict(
                "canonical compaction source/model changed from accepted task",
            ));
        }
        Ok(())
    }

    /// Retains the provider claim until the physical write and actual adoption.
    /// No ordinary transcript append claim or result is created by this effect.
    pub(in crate::runtime::commands::compaction) fn queue_replay_epoch_publication(
        &mut self,
        task: &crate::runtime::RuntimeAgentCompactionTask,
        output: ReplayHeadPublication,
    ) -> Result<()> {
        self.validate_replay_compaction_task(task)?;
        let requires_summary = task.request.is_some();
        if requires_summary != matches!(output, ReplayHeadPublication::Summary(_)) {
            return Err(MezError::conflict(
                "publication output does not match captured compaction mode",
            ));
        }
        if self
            .replay_epoch_publication(&task.pane_id, task.task_generation)
            .is_some()
        {
            return Err(MezError::conflict(
                "canonical compaction publication already admitted",
            ));
        }
        let source = task
            .replay_source
            .clone()
            .ok_or_else(|| MezError::invalid_state("canonical source missing"))?;
        let body = source.current_body(self)?;
        let publication_source = ReplayEpochContext::from_owned_context(
            ReplayConversationId::new(task.conversation_id.clone())?,
            body,
        )?;
        let work = RuntimeReplayEpochPublication::new(
            task.pane_id.clone(),
            task.task_generation,
            source,
            publication_source,
            output,
        );
        let attempt = work
            .claim_attempt()
            .ok_or_else(|| MezError::invalid_state("canonical publication attempt unavailable"))?;
        self.install_replay_epoch_publication(work);
        self.persistence
            .queue_transcript(RuntimeSideEffect::PersistReplayEpoch { attempt });
        Ok(())
    }

    /// Observes only the exact retained opaque work. Lost callbacks recover its
    /// actual receipt; idle I/O failures retry its original proposal at most three
    /// times, after all physical attempt guards retire. No provider redispatch.
    pub(crate) fn poll_replay_epoch_publication(
        &mut self,
        pane: &str,
        generation: u64,
    ) -> Result<RuntimeTransition> {
        let Some(work) = self.replay_epoch_publication(pane, generation) else {
            return Ok(RuntimeTransition::default());
        };
        if work.busy() {
            return Ok(RuntimeTransition::default());
        }
        if let Some(completion) = work.completed()
            && work.mark_handled(completion.attempt)
        {
            return self.apply_replay_epoch_completion_inner(completion);
        }
        if !self.agent_compaction_task_is_current(pane, generation) {
            self.fail_replay_epoch_publication(
                &work,
                &MezError::conflict("canonical publication conversation binding changed"),
            )?;
            return Ok(self.runtime_pane_transition_with_render(
                pane,
                true,
                Some(crate::runtime::RenderInvalidationReason::FullRedraw),
            ));
        }
        if let Err(error) = work.source().validate_live(self, pane, generation) {
            self.fail_replay_epoch_publication(&work, &error)?;
            return Ok(self.runtime_pane_transition_with_render(
                pane,
                true,
                Some(crate::runtime::RenderInvalidationReason::FullRedraw),
            ));
        }
        if work.attempts() >= worker::MAX_ATTEMPTS {
            self.fail_replay_epoch_publication(
                &work,
                &MezError::invalid_state("canonical publication exhausted storage recovery"),
            )?;
        } else if let Some(attempt) = work.claim_attempt() {
            self.persistence
                .queue_transcript(RuntimeSideEffect::PersistReplayEpoch { attempt });
        }
        Ok(self.runtime_pane_transition_with_render(
            pane,
            true,
            Some(crate::runtime::RenderInvalidationReason::FullRedraw),
        ))
    }

    /// Actual host callbacks and lost-callback polling use the same actor owner.
    pub(crate) fn apply_replay_epoch_completion(
        &mut self,
        completion: RuntimeReplayEpochCompletion,
    ) -> Result<RuntimeTransition> {
        let Some(work) = self.replay_epoch_publication(completion.pane(), completion.generation())
        else {
            return Ok(RuntimeTransition::default());
        };
        if work != completion.work || work.busy() || !work.mark_handled(completion.attempt) {
            return Ok(RuntimeTransition::default());
        }
        self.apply_replay_epoch_completion_inner(completion)
    }

    fn apply_replay_epoch_completion_inner(
        &mut self,
        completion: RuntimeReplayEpochCompletion,
    ) -> Result<RuntimeTransition> {
        let work = &completion.work;
        let pane = work.pane().to_owned();
        let generation = work.generation();
        // Polling must retain the same allocation as a direct callback.
        if self.replay_epoch_publication(&pane, generation).as_ref() != Some(work) {
            return Ok(RuntimeTransition::default());
        }
        if !self.agent_compaction_task_is_current(&pane, generation) {
            self.fail_replay_epoch_publication(
                work,
                &MezError::conflict("canonical publication conversation binding changed"),
            )?;
            return Ok(self.runtime_pane_transition_with_render(
                &pane,
                true,
                Some(crate::runtime::RenderInvalidationReason::FullRedraw),
            ));
        }
        let result = (|| {
            work.source().validate_live(self, &pane, generation)?;
            if let Some(error) = completion.error() {
                if error.kind() == crate::error::MezErrorKind::Io
                    && work.attempts() < worker::MAX_ATTEMPTS
                {
                    let _ = self.append_agent_status_text_to_terminal_buffer(&pane,
                        &format!("agent: canonical compaction publication requires storage recovery; retaining original proposal: {}", error.message()));
                    return Ok(());
                }
                return Err(MezError::invalid_state(format!(
                    "canonical compaction publication failed: {}",
                    error.message()
                )));
            }
            let turn_id = work.source().turn_id();
            let mut body = work.source().current_body(self)?;
            let history = completion
                .history()
                .map(|snapshot| {
                    crate::runtime::control::RuntimeAgentHistorySource::with_replay_publication(
                        work.source().original.history.as_ref(),
                        work.source().original.store.clone().ok_or_else(|| {
                            MezError::invalid_state("canonical original store disappeared")
                        })?,
                        &pane,
                        snapshot.clone(),
                        work.source().ephemeral,
                    )
                })
                .transpose()?;
            let before_rewrite = work.source().ephemeral.then(|| body.clone());
            completion.adopt(&mut body)?;
            if let Some(position) = completion.position() {
                let conversation = work.source().owner.conversation_id().as_str();
                let store = work.source().original.store.as_ref().ok_or_else(|| {
                    MezError::invalid_state("publication position lost its original store")
                })?;
                // Position counts original rows, including zero-model or legacy
                // pending owners. It grants none of their semantic authority.
                let pending = self.persistence.pending_transcript_entries_at_path(
                    &store.transcript_path(conversation)?,
                    conversation,
                )?;
                let logical_high_water = position
                    .logical_high_water
                    .max(pending.last().map_or(0, |row| row.sequence))
                    .max(
                        self.agent_shell_store()
                            .get(&pane)
                            .map_or(0, |session| session.transcript_high_water),
                    );
                let cut = completion
                    .history()
                    .ok_or_else(|| {
                        MezError::invalid_state(
                            "durable publication position has no original snapshot",
                        )
                    })?
                    .covered_archive_frontier();
                self.agent_shell_store_mut().restore_transcript_position(
                    &pane,
                    mez_agent::AgentTranscriptPosition {
                        logical_high_water,
                        active_raw_entries: logical_high_water.checked_sub(cut).ok_or_else(
                            || {
                                MezError::invalid_state(
                                    "publication cut exceeds logical transcript position",
                                )
                            },
                        )?,
                    },
                )?;
            }
            // Session/native identities survive; provider cache routing starts
            // a distinct epoch only after actual receipt acceptance.
            let mut metadata = body.metadata().clone();
            let lineage = format!(
                "compaction-{}",
                String::from(mez_agent::ContextBlockOccurrenceId::allocate())
            );
            metadata.prompt_cache_lineage_id = Some(lineage.clone());
            body.set_metadata(metadata);
            if work.source().ephemeral {
                let producer = turn_id.map(|turn| {
                    (
                        work.source().origin.clone(),
                        self.agent_turn_imported_history_sequence_high_water(turn),
                    )
                });
                self.retain_ephemeral_replay_body(
                    &pane,
                    work.source().owner.conversation_id().as_str(),
                    &body,
                    work.source().original.history.clone(),
                    producer,
                    before_rewrite.as_ref(),
                )?;
            }
            self.agent_shell_store_mut()
                .set_prompt_cache_lineage_for_conversation(
                    &pane,
                    work.source().owner.conversation_id().as_str(),
                    lineage,
                )?;
            if let Some(turn_id) = turn_id {
                self.agent_turn_contexts_mut()
                    .insert(turn_id.to_owned(), body);
                if let Some(history) = history {
                    self.set_agent_turn_history_source(turn_id.to_owned(), Some(history));
                }
                self.clear_agent_turn_provider_request_chain(turn_id);
            }
            self.clear_agent_conversation_provider_request_chain(
                work.source().owner.conversation_id().as_str(),
            );
            self.checkpoint_compaction_replacement_usage(
                work.source().owner.conversation_id().as_str(),
            )?;
            work.source().resume_after_publication(self, &pane)?;
            self.finish_agent_compaction_task(&pane, generation);
            let _ = self.append_agent_status_text_to_terminal_buffer(
                &pane,
                if work.source().requires_model_summary() {
                    "agent: canonical compaction head accepted"
                } else {
                    "agent: canonical raw-tail compaction accepted"
                },
            );
            self.resume_agent_compaction_steering(&pane)?;
            Ok(())
        })();
        if let Err(error) = result {
            self.fail_replay_epoch_publication(work, &error)?;
        }
        Ok(self.runtime_pane_transition_with_render(
            &pane,
            true,
            Some(crate::runtime::RenderInvalidationReason::FullRedraw),
        ))
    }

    /// Failure is confined to the original waiting turn. Durable receipts remain
    /// with their conversation and no replacement pane owner is touched.
    fn fail_replay_epoch_publication(
        &mut self,
        work: &RuntimeReplayEpochPublication,
        error: &MezError,
    ) -> Result<()> {
        let current = self.agent_compaction_task_is_current(work.pane(), work.generation());
        let owns_generation =
            self.agent_compaction_generation_is_current(work.pane(), work.generation());
        let task = self
            .claimed_agent_compaction_task(work.pane(), work.generation())
            .cloned();
        self.fail_agent_compaction_task(work.pane(), work.generation());
        if current {
            if let Some(task) = task.as_ref() {
                self.report_compaction_failure(
                    task,
                    "canonical compaction publication failed",
                    error,
                )?;
            } else {
                // Preserve failure containment even if the original claim was
                // lost; scalar work identity cannot reconstruct its authority.
                if let Some(turn) = work.source().turn_id() {
                    self.fail_running_turn_after_compaction_error(
                        turn,
                        "canonical_epoch_compaction",
                        error,
                    )?;
                }
                let _ =
                    self.append_agent_error_text_to_terminal_buffer(work.pane(), error.message());
                self.resume_agent_compaction_steering(work.pane())?;
            }
        } else if owns_generation
            && let Some(turn) = work
                .source()
                .turn_id()
                .and_then(|turn| self.agent_turn_ledger().turn(turn))
                .cloned()
            && turn.state == mez_agent::AgentTurnState::Running
            && turn.pane_id == work.pane()
            && turn.conversation_id == work.source().owner.conversation_id().as_str()
        {
            // The binding has moved, but this exact logical compactor still
            // held its original turn. Retire that waiting turn through the
            // detached path; do not paint or mutate the replacement binding.
            let _ = self.append_agent_trace_turn_event(
                &turn.pane_id,
                &turn.turn_id,
                &format!(
                    "canonical publication retired after binding replacement: {}",
                    error.message()
                ),
            );
            self.complete_running_agent_turn_and_start_ready(
                &turn,
                mez_agent::AgentTurnState::Failed,
                "canonical_publication_binding_replaced",
            )?;
        }
        Ok(())
    }
}
