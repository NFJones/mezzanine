//! Deferred external-effect queue operations.

use super::{RuntimePersistenceComponent, RuntimeSideEffect, TerminalSize};

impl RuntimePersistenceComponent {
    /// Cancels one queued or active shell delivery for an exact process.
    pub(crate) fn queue_shell_input_cancellation(
        &mut self,
        instance: crate::runtime::PaneProcessInstance,
        delivery_id: String,
    ) {
        self.queued_pane_input_effects.retain(|effect| {
            !matches!(
                effect,
                RuntimeSideEffect::PaneProcessIo {
                    instance: queued_instance,
                    effect: crate::runtime::PaneProcessIoEffect::WriteShellInput { delivery },
                } if queued_instance == &instance
                    && delivery.delivery_id.as_deref() == Some(delivery_id.as_str())
            )
        });
        self.queued_pane_input_effects.insert(
            0,
            RuntimeSideEffect::PaneProcessIo {
                instance,
                effect: crate::runtime::PaneProcessIoEffect::CancelShellInput { delivery_id },
            },
        );
    }

    /// Queues one pane-input effect in dispatch order.
    pub(crate) fn queue_pane_input(&mut self, effect: RuntimeSideEffect) {
        let priority_pane_id = match &effect {
            RuntimeSideEffect::PaneProcessIo {
                instance,
                effect: crate::runtime::PaneProcessIoEffect::WriteShellInput { delivery },
            } if delivery.priority => Some(instance.pane_id.as_str()),
            RuntimeSideEffect::WritePaneShellInput { pane_id, delivery } if delivery.priority => {
                Some(pane_id.as_str())
            }
            _ => None,
        };
        if let Some(pane_id) = priority_pane_id {
            let insert_at = self
                .queued_pane_input_effects
                .iter()
                .position(|queued| match queued {
                    RuntimeSideEffect::PaneProcessIo { instance, .. } => {
                        instance.pane_id == pane_id
                    }
                    RuntimeSideEffect::WritePaneInput {
                        pane_id: queued_pane_id,
                        ..
                    }
                    | RuntimeSideEffect::WritePaneInputPriority {
                        pane_id: queued_pane_id,
                        ..
                    }
                    | RuntimeSideEffect::WritePaneShellInput {
                        pane_id: queued_pane_id,
                        ..
                    } => queued_pane_id == pane_id,
                    _ => false,
                })
                .unwrap_or(self.queued_pane_input_effects.len());
            self.queued_pane_input_effects.insert(insert_at, effect);
        } else {
            self.queued_pane_input_effects.push(effect);
        }
    }

    /// Queues one ordered pane observation after prior PTY output is applied.
    pub(crate) fn queue_pane_observation(&mut self, effect: RuntimeSideEffect) {
        self.queued_pane_input_effects.push(effect);
    }

    /// Replaces the queued resize for one pane.
    pub(crate) fn queue_pane_resize(
        &mut self,
        pane_id: impl Into<String>,
        effect: RuntimeSideEffect,
    ) {
        let pane_id = pane_id.into();
        if let RuntimeSideEffect::PaneProcessIo {
            effect: crate::runtime::PaneProcessIoEffect::Resize { size },
            ..
        } = &effect
        {
            self.expected_pane_resize_sizes
                .insert(pane_id.clone(), *size);
        }
        self.queued_pane_resize_effects.insert(pane_id, effect);
    }

    /// Consumes the expected async resize size when one completion arrives.
    ///
    /// Returns `false` only when a later queued resize has made this completion
    /// stale; completions without a tracked adapter request remain valid.
    pub(crate) fn accept_pane_resize_completion(
        &mut self,
        pane_id: &str,
        size: TerminalSize,
    ) -> bool {
        let Some(expected) = self.expected_pane_resize_sizes.get(pane_id) else {
            return true;
        };
        if *expected != size {
            return false;
        }
        self.expected_pane_resize_sizes.remove(pane_id);
        true
    }

    /// Replaces the queued termination for one pane.
    pub(crate) fn queue_pane_termination(
        &mut self,
        pane_id: impl Into<String>,
        effect: RuntimeSideEffect,
    ) {
        self.queued_pane_termination_effects
            .insert(pane_id.into(), effect);
    }

    /// Retains an already-requested termination or queues the supplied fallback.
    ///
    /// Pane removal normally requests termination before cleanup. Defensive
    /// cleanup can instead discover an adapter-owned process after its layout
    /// pane has already disappeared; in that case the fallback guarantees the
    /// retired worker still receives one exact-generation termination without
    /// replacing an earlier graceful-versus-force decision.
    pub(crate) fn ensure_pane_termination(
        &mut self,
        pane_id: impl Into<String>,
        effect: RuntimeSideEffect,
    ) {
        self.queued_pane_termination_effects
            .entry(pane_id.into())
            .or_insert(effect);
    }

    /// Drains input, resize, and termination effects in canonical order.
    pub(crate) fn take_pane_io_effects(&mut self) -> Vec<RuntimeSideEffect> {
        let mut effects = std::mem::take(&mut self.queued_pane_input_effects);
        effects.extend(std::mem::take(&mut self.queued_pane_resize_effects).into_values());
        effects.extend(std::mem::take(&mut self.queued_pane_termination_effects).into_values());
        effects
    }

    /// Removes obsolete queued pane I/O and pipe effects for a closed pane.
    ///
    /// A queued termination is deliberately retained: pane cleanup runs after
    /// the runtime has requested termination from an async process owner, and
    /// that owner still needs the effect even though the pane has left the
    /// session layout.
    pub(crate) fn cleanup_pane_io(&mut self, pane_id: &str) {
        self.queued_pane_input_effects
            .retain(|effect| match effect {
                RuntimeSideEffect::WritePaneInput {
                    pane_id: target, ..
                }
                | RuntimeSideEffect::WritePaneInputPriority {
                    pane_id: target, ..
                }
                | RuntimeSideEffect::WritePaneShellInput {
                    pane_id: target, ..
                } => target != pane_id,
                RuntimeSideEffect::PaneProcessIo { instance, .. } => instance.pane_id != pane_id,
                _ => true,
            });
        self.queued_pane_resize_effects.remove(pane_id);
        self.expected_pane_resize_sizes.remove(pane_id);
        self.queued_pane_pipe_effects
            .retain(|(queued_pane_id, _)| queued_pane_id != pane_id);
    }

    /// Queues one pane-pipe effect together with its cleanup owner.
    pub(crate) fn queue_pane_pipe(
        &mut self,
        pane_id: impl Into<String>,
        effect: RuntimeSideEffect,
    ) {
        self.queued_pane_pipe_effects.push((pane_id.into(), effect));
    }

    /// Drains pane-pipe effects while discarding cleanup keys.
    pub(crate) fn take_pane_pipe_effects(&mut self) -> Vec<RuntimeSideEffect> {
        std::mem::take(&mut self.queued_pane_pipe_effects)
            .into_iter()
            .map(|(_, effect)| effect)
            .collect()
    }

    /// Queues one audit persistence effect.
    pub(crate) fn queue_audit(&mut self, effect: RuntimeSideEffect) {
        self.queued_audit_effects.push(effect);
    }

    /// Drains queued audit persistence effects.
    pub(crate) fn take_audit_effects(&mut self) -> Vec<RuntimeSideEffect> {
        std::mem::take(&mut self.queued_audit_effects)
    }

    /// Queues one transcript or prompt-history persistence effect.
    pub(crate) fn queue_transcript(&mut self, effect: RuntimeSideEffect) {
        self.queued_transcript_effects.push(effect);
    }

    /// Queues one archive lifecycle operation unless that conversation already has work pending.
    #[allow(
        dead_code,
        reason = "archive operation planning is consumed by the dependent resume browser work"
    )]
    pub(crate) fn queue_session_archive(
        &mut self,
        effect: RuntimeSideEffect,
    ) -> crate::error::Result<bool> {
        let RuntimeSideEffect::PersistSessionArchive {
            conversation_id, ..
        } = &effect
        else {
            return Err(crate::error::MezError::invalid_args(
                "session archive queue requires a session archive side effect",
            ));
        };
        if !self
            .pending_session_archive_conversation_ids
            .insert(conversation_id.clone())
        {
            return Ok(false);
        }
        self.queued_transcript_effects.push(effect);
        Ok(true)
    }

    /// Clears actor-owned duplicate suppression after archive work settles.
    pub(crate) fn finish_session_archive(&mut self, conversation_id: &str) {
        self.pending_session_archive_conversation_ids
            .remove(conversation_id);
    }

    /// Retains the primary client and pane that should resume after one restore.
    pub(crate) fn set_session_archive_resume(
        &mut self,
        conversation_id: String,
        client_id: mez_core::ids::ClientId,
        pane_id: String,
    ) {
        self.pending_session_archive_resumes
            .insert(conversation_id, (client_id, pane_id));
    }

    /// Takes the deferred resume continuation for one settled archive operation.
    pub(crate) fn take_session_archive_resume(
        &mut self,
        conversation_id: &str,
    ) -> Option<(mez_core::ids::ClientId, String)> {
        self.pending_session_archive_resumes.remove(conversation_id)
    }

    /// Queues one active saved-session retention pass unless work is already pending.
    pub(crate) fn queue_saved_session_retention(
        &mut self,
        effect: RuntimeSideEffect,
    ) -> crate::error::Result<bool> {
        let RuntimeSideEffect::PersistSavedSessionRetention { schedule_next, .. } = &effect else {
            return Err(crate::error::MezError::invalid_args(
                "saved-session retention queue requires a retention side effect",
            ));
        };
        if self.saved_session_retention_pending {
            self.saved_session_retention_rerun_requested = true;
            self.saved_session_retention_rerun_schedule_next |= *schedule_next;
            return Ok(false);
        }
        self.saved_session_retention_pending = true;
        self.queued_transcript_effects.push(effect);
        Ok(true)
    }

    /// Clears duplicate suppression and returns deferred rerun scheduling intent.
    pub(crate) fn finish_saved_session_retention(&mut self) -> Option<bool> {
        self.saved_session_retention_pending = false;
        if !std::mem::take(&mut self.saved_session_retention_rerun_requested) {
            self.saved_session_retention_rerun_schedule_next = false;
            return None;
        }
        Some(std::mem::take(
            &mut self.saved_session_retention_rerun_schedule_next,
        ))
    }

    /// Returns queued transcript entries for one conversation without draining
    /// the external persistence worker's ordered effect queue.
    pub(crate) fn pending_transcript_entries(
        &self,
        conversation_id: &str,
    ) -> Vec<mez_agent::transcript::TranscriptEntry> {
        self.queued_transcript_effects
            .iter()
            .filter_map(|effect| match effect {
                RuntimeSideEffect::PersistTranscriptEntries { entries, .. } => Some(entries),
                _ => None,
            })
            .flatten()
            .filter(|entry| entry.conversation_id == conversation_id)
            .cloned()
            .collect()
    }

    /// Queues one presentation append in transcript/archive ordering.
    pub(crate) fn queue_presentation(&mut self, effect: RuntimeSideEffect) {
        if let RuntimeSideEffect::PersistPresentationEntries { entries, .. } = &effect
            && let Some(conversation_id) = entries.first().map(|entry| &entry.conversation_id)
        {
            let pending = self
                .pending_presentation_entries
                .entry(conversation_id.clone())
                .or_default();
            *pending = pending.saturating_add(entries.len());
        }
        self.queued_transcript_effects.push(effect);
    }

    /// Reports whether one conversation has presentation entries awaiting settlement.
    pub(crate) fn presentation_write_pending(&self, conversation_id: &str) -> bool {
        self.pending_presentation_entries
            .get(conversation_id)
            .is_some_and(|pending| *pending > 0)
    }

    /// Settles persisted presentation entries and reports when the conversation is clear.
    pub(crate) fn finish_presentation_write(
        &mut self,
        conversation_id: &str,
        entries: usize,
    ) -> bool {
        let Some(pending) = self.pending_presentation_entries.get_mut(conversation_id) else {
            return false;
        };
        if *pending > entries {
            *pending -= entries;
            return false;
        }
        self.pending_presentation_entries.remove(conversation_id);
        true
    }

    /// Drains queued transcript and prompt-history effects.
    pub(crate) fn take_transcript_effects(&mut self) -> Vec<RuntimeSideEffect> {
        std::mem::take(&mut self.queued_transcript_effects)
    }

    /// Queues one durable token-accounting append.
    pub(crate) fn queue_token_usage(&mut self, effect: RuntimeSideEffect) {
        self.queued_token_usage_effects.push(effect);
    }

    /// Drains queued durable token-accounting appends in settlement order.
    pub(crate) fn take_token_usage_effects(&mut self) -> Vec<RuntimeSideEffect> {
        std::mem::take(&mut self.queued_token_usage_effects)
    }

    /// Queues one actor-validated provider persistence settlement.
    pub(crate) fn queue_provider_settlement(&mut self, effect: RuntimeSideEffect) {
        self.queued_provider_settlement_effects.push(effect);
    }

    /// Drains provider persistence settlements in actor-validation order.
    pub(crate) fn take_provider_settlement_effects(&mut self) -> Vec<RuntimeSideEffect> {
        std::mem::take(&mut self.queued_provider_settlement_effects)
    }

    /// Queues one configuration persistence effect.
    pub(crate) fn queue_config(&mut self, effect: RuntimeSideEffect) {
        self.queued_config_effects.push(effect);
    }

    /// Drains queued configuration persistence effects.
    pub(crate) fn take_config_effects(&mut self) -> Vec<RuntimeSideEffect> {
        std::mem::take(&mut self.queued_config_effects)
    }

    /// Queues one non-blocking program-hook effect.
    pub(crate) fn queue_program_hook(&mut self, effect: RuntimeSideEffect) {
        self.queued_program_hook_effects.push(effect);
    }

    /// Drains queued non-blocking program-hook effects.
    pub(crate) fn take_program_hook_effects(&mut self) -> Vec<RuntimeSideEffect> {
        std::mem::take(&mut self.queued_program_hook_effects)
    }
}
