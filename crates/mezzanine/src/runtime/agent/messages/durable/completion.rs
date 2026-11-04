//! Application continuations retained independently of an already joined root.
//!
//! Canonical proof attaches before position/request-reply/coordination effects.
//! Each receipt owns its monotonic retry so one failing conversation cannot
//! starve others or turn a decoration failure into a physical storage replay.
//! Exact ACK releases the input gate, but its original scheduler continuation
//! stays retained until completed or suppressed by graceful shutdown. None of
//! these runtime owners is reconstructed from saved display or journal data.

use super::*;

impl RuntimeSessionService {
    /// Records a real joined original archive frontier without manufacturing a
    /// received context, task or provider continuation. Historical journal-only
    /// proof and a replacement native namespace cannot update current backing.
    pub(crate) fn observe_native_input_archive(
        &mut self,
        proof: &MessageNativeInputCommit,
    ) -> Result<()> {
        if !proof.record().archive_observed
            || !self
                .message_service()
                .native_input_owner_is_current(proof.anchor())
        {
            return Ok(());
        }
        let Some(pane) = proof.anchor().recipient().as_str().strip_prefix("agent-") else {
            return Ok(());
        };
        let Some(shell) = self.agent_shell_store().get(pane) else {
            return Ok(());
        };
        if shell.session_id != proof.anchor().native_owner().conversation_id() {
            return Ok(());
        }
        let pane = pane.to_owned();
        let high = shell
            .transcript_high_water
            .max(proof.record().archive_sequence);
        let added = high - shell.transcript_high_water;
        let raw = shell.transcript_entries.checked_add(added).ok_or_else(|| {
            MezError::invalid_state("native canonical transcript frontier exhausted")
        })?;
        self.agent_shell_store_mut().restore_transcript_position(
            &pane,
            mez_agent::AgentTranscriptPosition {
                logical_high_water: high,
                active_raw_entries: raw,
            },
        )?;
        Ok(())
    }

    /// Attaches genuine joined proof before any fallible application effects.
    /// A repeated attachment is inert only for the same original source and row.
    pub(crate) fn complete_native_input_commit(
        &mut self,
        id: &str,
        proof: MessageNativeInputCommit,
    ) -> Result<()> {
        let receipt = self
            .agent
            .received_peer_message_presentations
            .get_mut(id)
            .ok_or_else(|| MezError::invalid_state("original native receipt missing"))?;
        let work = match &receipt.durable_input {
            Some(NativeReceipt::Pending(work) | NativeReceipt::Writing(work)) => work.clone(),
            Some(NativeReceipt::Committed {
                proof: original, ..
            }) if original.anchor().sequence() == proof.anchor().sequence()
                && original.anchor().source_sha256() == proof.anchor().source_sha256()
                && original.anchor().binding_index() == proof.anchor().binding_index()
                && original.record() == proof.record() =>
            {
                return Ok(());
            }
            _ => return Err(MezError::invalid_state("original native receipt changed")),
        };
        if proof.anchor().sequence() != work.source.sequence()
            || proof.anchor().source_sha256() != work.source.source_sha256()
            || proof.anchor().binding_index() != work.source.binding_index()
            || proof.anchor().native_owner() != work.source.native_owner()
            || proof.anchor().recipient() != work.source.recipient()
        {
            return Err(MezError::invalid_state(
                "original native commit source changed",
            ));
        }
        receipt.durable_input = Some(NativeReceipt::Committed {
            work,
            proof,
            applied: false,
            retry: None,
        });
        Ok(())
    }

    /// Returns the earliest retained application or ACK-admission backoff. Ready
    /// ACKs without a deadline remain the ordinary root pump's responsibility.
    pub(crate) fn native_input_completion_at(
        &self,
        ack_admission_available: bool,
    ) -> Option<std::time::Instant> {
        self.agent
            .received_peer_message_presentations
            .values()
            .filter_map(|receipt| match &receipt.durable_input {
                Some(NativeReceipt::Committed { applied, retry, .. }) => {
                    if !applied || (ack_admission_available && retry.is_some()) {
                        Some(
                            retry
                                .as_ref()
                                .map_or_else(std::time::Instant::now, |r| r.at),
                        )
                    } else {
                        None
                    }
                }
                Some(NativeReceipt::Acknowledged {
                    continuation: Some(c),
                }) => Some(
                    c.retry
                        .as_ref()
                        .map_or_else(std::time::Instant::now, |r| r.at),
                ),
                _ => None,
            })
            .min()
    }

    /// Borrows one due original application continuation without issuing I/O.
    pub(crate) fn pending_native_input_application(&self) -> Option<String> {
        self.agent
            .received_peer_message_presentations
            .iter()
            .find_map(|(id, receipt)| {
                let retry = match &receipt.durable_input {
                    Some(NativeReceipt::Committed {
                        applied: false,
                        retry,
                        ..
                    }) => retry,
                    Some(NativeReceipt::Acknowledged {
                        continuation: Some(c),
                    }) => &c.retry,
                    _ => return None,
                };
                retry
                    .as_ref()
                    .is_none_or(|r| r.at <= std::time::Instant::now())
                    .then(|| id.clone())
            })
    }

    /// Retains one bounded diagnostic/deadline with the original receipt. This
    /// never retries an idle root or blocks application work for another owner.
    pub(crate) fn defer_native_input_completion(&mut self, id: &str, error: MezError) {
        let Some(receipt) = self.agent.received_peer_message_presentations.get_mut(id) else {
            return;
        };
        let retry = match &mut receipt.durable_input {
            Some(NativeReceipt::Committed { retry, .. }) => retry,
            Some(NativeReceipt::Acknowledged {
                continuation: Some(c),
            }) => &mut c.retry,
            _ => return,
        };
        *retry = Some(NativeRetry {
            at: std::time::Instant::now() + std::time::Duration::from_secs(1),
            _failure: error,
        });
    }

    /// Applies idempotent decorations from retained actual work and proof. A
    /// failure leaves that proof and source owned; ACK eligibility stays closed.
    pub(crate) fn apply_native_input_completion(
        &mut self,
        id: &str,
        continue_turn: bool,
    ) -> Result<()> {
        let state = self
            .agent
            .received_peer_message_presentations
            .get(id)
            .and_then(|receipt| receipt.durable_input.clone())
            .ok_or_else(|| MezError::invalid_state("native application receipt missing"))?;
        match state {
            NativeReceipt::Committed {
                work,
                proof,
                applied: false,
                ..
            } => {
                if self
                    .message_service()
                    .native_input_owner_is_current(&work.source)
                    && self
                        .agent_shell_store()
                        .get(&work.turn.pane_id)
                        .is_some_and(|s| s.session_id == work.turn.conversation_id)
                {
                    let row = work.projection.committed_row(&proof)?;
                    self.agent_shell_store_mut()
                        .record_transcript_append(&work.turn.pane_id, &[row])?;
                }
                if self.native_receive_turn_is_current(&work) {
                    self.record_request_reply_commit(
                        &work.turn,
                        work.source.sequence(),
                        &work.envelope,
                    )?;
                    self.record_joined_coordination_mail(
                        &work.turn,
                        work.source.sequence(),
                        &work.envelope,
                    );
                }
                if let Some(receipt) = self.agent.received_peer_message_presentations.get_mut(id) {
                    receipt.durable_input = Some(NativeReceipt::Committed {
                        work,
                        proof,
                        applied: true,
                        retry: None,
                    });
                }
            }
            NativeReceipt::Acknowledged {
                continuation: Some(c),
            } => {
                if !continue_turn || !c.continue_turn {
                    self.remove_pending_agent_provider_task(&c.work.turn.turn_id);
                } else {
                    self.continue_acknowledged_native_input(&c.work)?;
                }
                if let Some(receipt) = self.agent.received_peer_message_presentations.get_mut(id) {
                    receipt.durable_input =
                        Some(NativeReceipt::Acknowledged { continuation: None });
                }
                let _ = self.flush_received_peer_message_presentations();
            }
            _ => (),
        }
        Ok(())
    }

    /// Gives the actor only decorated, due source-bound ACK candidates. The
    /// original receipt retains proof across a pre-issuance admission refusal.
    pub(crate) fn pending_native_input_ack(&self) -> Option<(String, MessageNativeInputCommit)> {
        self.agent
            .received_peer_message_presentations
            .iter()
            .find_map(|(id, receipt)| match &receipt.durable_input {
                Some(NativeReceipt::Committed {
                    proof,
                    applied: true,
                    retry,
                    ..
                }) if retry
                    .as_ref()
                    .is_none_or(|r| r.at <= std::time::Instant::now()) =>
                {
                    Some((id.clone(), proof.clone()))
                }
                _ => None,
            })
    }

    /// Records actual exact-slot settlement before any fallible scheduler work.
    /// Its original continuation remains retained independently of presentation.
    pub(crate) fn complete_native_input_ack(
        &mut self,
        id: &str,
        continue_turn: bool,
    ) -> Result<()> {
        let receipt = self
            .agent
            .received_peer_message_presentations
            .get_mut(id)
            .ok_or_else(|| MezError::invalid_state("original native ACK receipt missing"))?;
        match &receipt.durable_input {
            Some(NativeReceipt::Committed {
                work,
                applied: true,
                ..
            }) => {
                receipt.durable_input = Some(NativeReceipt::Acknowledged {
                    continuation: Some(NativeContinuation {
                        work: work.clone(),
                        continue_turn,
                        retry: None,
                    }),
                });
            }
            Some(NativeReceipt::Acknowledged { .. }) => (),
            _ => {
                return Err(MezError::invalid_state(
                    "original native ACK receipt changed",
                ));
            }
        }
        self.apply_native_input_completion(id, continue_turn)
    }

    /// Faults the actual scheduler continuation after its genuine canonical and
    /// transport commits. This control manufactures neither completion proof.
    #[cfg(test)]
    pub(crate) fn fail_next_native_ack_continuation_for_tests(&mut self) {
        self.agent.fail_next_native_ack_continuation = true;
    }

    /// Resumes only the original live turn after ACK. Repeated admission is
    /// coalesced by existing scheduler/provider ownership, never a new task.
    fn continue_acknowledged_native_input(&mut self, work: &NativeWork) -> Result<()> {
        #[cfg(test)]
        if std::mem::take(&mut self.agent.fail_next_native_ack_continuation) {
            return Err(MezError::invalid_state(
                "injected native ACK scheduler handoff failure",
            ));
        }

        if self.native_receive_turn_is_current(work) {
            let turn = self
                .agent_turn_ledger()
                .turn(&work.turn.turn_id)
                .cloned()
                .ok_or_else(|| MezError::invalid_state("original native turn missing"))?;
            if turn.state == AgentTurnState::Blocked
                && !crate::runtime::control::runtime_owned_bridge_message(&work.envelope)
            {
                self.agent
                    .agent_turn_peer_wait_wake_pending
                    .insert(turn.turn_id.clone());
                let _ = self.resume_parked_peer_wait(&turn)?;
            }
            if turn.state == AgentTurnState::Running
                && !self.agent_provider_task_is_owned(&turn.turn_id)
            {
                self.queue_agent_provider_task(turn.turn_id);
            }
            self.recover_received_peer_message_turn_scheduling(
                crate::runtime::current_unix_millis(),
            )?;
        } else {
            // Reused pane/CID/task labels cannot release an old namespace's
            // already queued provider offer into a replacement incarnation.
            self.remove_pending_agent_provider_task(&work.turn.turn_id);
        }
        Ok(())
    }

    /// Original application ownership must survive in the actual live runtime.
    fn native_receive_turn_is_current(&self, work: &NativeWork) -> bool {
        let turn = &work.turn;
        self.message_service()
            .native_input_owner_is_current(&work.source)
            && self
                .agent_shell_store()
                .get(&turn.pane_id)
                .is_some_and(|s| s.session_id == turn.conversation_id)
            && self.agent_turn_contexts().contains_key(&turn.turn_id)
            && self
                .agent_turn_ledger()
                .turn(&turn.turn_id)
                .is_some_and(|t| {
                    matches!(
                        t.state,
                        AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
                    )
                })
    }
}
