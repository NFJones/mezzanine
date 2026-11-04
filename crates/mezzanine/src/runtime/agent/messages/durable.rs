//! Original-root native receive gating and post-context persistence custody.
//!
//! Only an actually joined original-root absence may permit fresh input. The
//! permit owns one real selected source and is consumed at the private receipt
//! boundary after the actual canonical event exists. That receipt retains the
//! original target/projection through pane/context replacement. Canonical and
//! exact transport settlement hold provider, scheduler and peer continuations;
//! neither historical c1 recovery nor display restoration creates a new task.

use std::sync::Arc;

use mez_agent::messaging::{MessageNativeInputAnchor, MessageNativeInputCommit, SequencedEnvelope};
use mez_agent::transcript::{TranscriptEntry, TranscriptRole};

use super::*;
use crate::storage::mmp_custody::native_input::NativeInputProjection;
use crate::storage::transcript::AgentTranscriptStore;

/// One genuine post-context input owner, never deserialized from presentation.
#[derive(Debug, Clone)]
pub(in crate::runtime::agent) struct NativeWork {
    source: MessageNativeInputAnchor,
    target: AgentTranscriptStore,
    projection: Arc<NativeInputProjection>,
    turn: AgentTurnRecord,
    envelope: Arc<Envelope>,
}
/// Canonical and transport settlement are separate source-owned boundaries.
#[derive(Debug, Clone)]
pub(in crate::runtime::agent) enum NativeReceipt {
    Pending(Arc<NativeWork>),
    Committed {
        work: Arc<NativeWork>,
        proof: MessageNativeInputCommit,
    },
    Acknowledged,
}

impl RuntimeSessionService {
    /// Freezes the exact peer occurrences whose canonical persistence already
    /// belongs to the root, so interrupted/provider bookkeeping cannot race a
    /// second peer projection into the archive. This grants no ACK authority.
    pub(in crate::runtime::agent) fn native_owned_peer_events(&self, turn: &str) -> BTreeSet<u64> {
        self.agent
            .received_peer_message_presentations
            .values()
            .filter_map(|r| match &r.durable_input {
                Some(NativeReceipt::Pending(work) | NativeReceipt::Committed { work, .. })
                    if work.turn.turn_id == turn =>
                {
                    Some(work.projection.event_sequence())
                }
                _ => None,
            })
            .collect()
    }

    /// Live storage mode cannot use memory-only receive/ACK shortcuts.
    pub(crate) fn native_input_storage_required(&self) -> bool {
        self.control
            .message_service()
            .native_input_storage_required()
    }

    /// A real root absence permits only its exact original, still-current
    /// native source. Endpoint selection itself remains the lower owner's work.
    pub(crate) fn activate_native_input_absence(
        &mut self,
        source: &MessageNativeInputAnchor,
        now_ms: u64,
    ) -> Result<bool> {
        if self.agent.native_input_permit.is_some() {
            return Ok(false);
        }
        if !self
            .control
            .message_service()
            .native_input_can_receive(source, now_ms)
        {
            return Ok(false);
        }
        let Some(envelope) = source.resident_envelope().ok() else {
            return Ok(false);
        };
        if self
            .runtime_agent_active_turn(source.recipient().as_str())
            .is_none()
            && crate::runtime::control::runtime_owned_bridge_message(&envelope)
        {
            return Ok(false);
        }
        if self
            .control
            .message_service()
            .subscription(source.recipient())
            .is_none()
        {
            self.control
                .message_service_mut()
                .subscribe_from_retained_start(source.recipient())?;
        }
        let Some(selected) = self
            .control
            .message_service()
            .select_native_input(source, now_ms)?
        else {
            return Ok(false);
        };
        self.agent.native_input_permit = Some(selected);
        let result = self.deliver_pending_runtime_agent_messages(now_ms);
        // A refusal did not commit canonical input. Release the permit rather
        // than holding an unrelated recipient behind this source indefinitely.
        self.agent.native_input_permit.take();
        result.map(|count| count > 0)
    }

    /// Both live delivery and prompt builders share this exact input gate.
    pub(crate) fn native_input_is_permitted(
        &self,
        recipient: &AgentId,
        message: &SequencedEnvelope,
    ) -> bool {
        !self.native_input_storage_required()
            || self
                .agent
                .native_input_permit
                .as_ref()
                .is_some_and(|source| {
                    source.recipient() == recipient
                        && source.sequence() == message.sequence
                        && self
                            .control
                            .message_service()
                            .native_input_anchor_is_original(source)
                        && self
                            .agent_shell_store()
                            .get(recipient.as_str().trim_start_matches("agent-"))
                            .is_some_and(|s| {
                                s.session_id == source.native_owner().conversation_id()
                            })
                })
    }

    /// Captures the actual context chronology event and original storage target
    /// before the receipt can expose canonical proof to any continuation.
    pub(super) fn capture_native_input_work(
        &mut self,
        recipient: &AgentId,
        sequence: u64,
        turn: &AgentTurnRecord,
    ) -> Result<Option<NativeReceipt>> {
        if !self.native_input_storage_required() {
            return Ok(None);
        }
        let source = self
            .agent
            .native_input_permit
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("original native input permit missing"))?;
        if source.recipient() != recipient
            || source.sequence() != sequence
            || source.native_owner().conversation_id() != turn.conversation_id
        {
            return Err(MezError::invalid_state(
                "original native receive owner changed",
            ));
        }
        let envelope = source.resident_envelope()?;
        let label =
            crate::runtime::control::runtime_peer_message_block_label(sequence, &envelope.id);
        let event = self
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .and_then(|c| {
                c.chronology().iter().rev().find(|e| {
                    e.block().source == mez_agent::ContextSourceKind::PeerMessage
                        && e.block().label == label
                })
            })
            .ok_or_else(|| MezError::invalid_state("actual native input chronology missing"))?;
        let row = TranscriptEntry {
            conversation_id: turn.conversation_id.clone(),
            sequence: 1,
            created_at_unix_seconds: turn.started_at_unix_seconds,
            role: TranscriptRole::System,
            turn_id: turn.turn_id.clone(),
            agent_id: turn.agent_id.clone(),
            pane_id: turn.pane_id.clone(),
            content: event.block().content.clone(),
        };
        let projection = Arc::new(NativeInputProjection::new(
            row,
            event.sequence().get(),
            event.block().label.clone(),
        )?);
        let target = self
            .agent_transcript_store()
            .cloned()
            .ok_or_else(|| MezError::invalid_state("canonical native transcript store missing"))?;
        let work = NativeWork {
            source: source.clone(),
            target,
            projection,
            turn: turn.clone(),
            envelope,
        };
        self.agent.native_input_permit.take();
        Ok(Some(NativeReceipt::Pending(Arc::new(work))))
    }

    /// Provider and peer/scheduler continuations wait through exact native ACK.
    pub(crate) fn native_input_pending_for_conversation(&self, conversation: &str) -> bool {
        self.agent
            .native_input_permit
            .as_ref()
            .is_some_and(|s| s.native_owner().conversation_id() == conversation)
            || self
                .agent
                .received_peer_message_presentations
                .values()
                .any(|r| {
                    r.conversation_id == conversation
                        && matches!(
                            r.durable_input,
                            Some(NativeReceipt::Pending(_) | NativeReceipt::Committed { .. })
                        )
                })
    }

    /// History admission waits for already committed memory input, while the
    /// qualified absence permit may build its own first idle context normally.
    pub(crate) fn native_input_receipt_pending_for_conversation(&self, conversation: &str) -> bool {
        self.agent
            .received_peer_message_presentations
            .values()
            .any(|r| {
                r.conversation_id == conversation
                    && matches!(
                        r.durable_input,
                        Some(NativeReceipt::Pending(_) | NativeReceipt::Committed { .. })
                    )
            })
    }

    /// Shutdown must retain every actual receipt, including those awaiting the
    /// ordinary transcript writer before their native projection can be issued.
    pub(crate) fn has_pending_native_input(&self) -> bool {
        self.agent.native_input_permit.is_some()
            || self
                .agent
                .received_peer_message_presentations
                .values()
                .any(|r| {
                    matches!(
                        r.durable_input,
                        Some(NativeReceipt::Pending(_) | NativeReceipt::Committed { .. })
                    )
                })
    }

    /// Borrows the existing bounded receipt owner; no second mailbox is built.
    pub(crate) fn pending_native_input_work(
        &self,
    ) -> Option<(
        String,
        MessageNativeInputAnchor,
        AgentTranscriptStore,
        Arc<NativeInputProjection>,
    )> {
        self.agent
            .received_peer_message_presentations
            .iter()
            .find_map(|(id, receipt)| {
                let Some(NativeReceipt::Pending(work)) = &receipt.durable_input else {
                    return None;
                };
                if self
                    .persistence
                    .bookkeeping_pending(&work.turn.conversation_id)
                {
                    return None;
                }
                Some((
                    id.clone(),
                    work.source.clone(),
                    work.target.clone(),
                    work.projection.clone(),
                ))
            })
    }

    /// Actual joined storage proof attaches only to the original receipt. Late
    /// completion still settles old transport, without feeding its successor.
    pub(crate) fn complete_native_input_commit(
        &mut self,
        id: &str,
        proof: MessageNativeInputCommit,
    ) -> Result<()> {
        let work = self
            .agent
            .received_peer_message_presentations
            .get(id)
            .and_then(|r| match &r.durable_input {
                Some(NativeReceipt::Pending(work)) => Some(work.clone()),
                _ => None,
            })
            .ok_or_else(|| MezError::invalid_state("original native receipt missing"))?;
        if proof.anchor().sequence() != work.source.sequence()
            || proof.anchor().source_sha256() != work.source.source_sha256()
            || proof.anchor().binding_index() != work.source.binding_index()
        {
            return Err(MezError::invalid_state(
                "original native commit source changed",
            ));
        }
        if self
            .agent_shell_store()
            .get(&work.turn.pane_id)
            .is_some_and(|s| s.session_id == work.turn.conversation_id)
        {
            let row = work.projection.committed_row(&proof)?;
            self.agent_shell_store_mut()
                .record_transcript_append(&work.turn.pane_id, &[row])?;
        }
        if self.native_receive_turn_is_current(&work.turn) {
            self.record_request_reply_commit(&work.turn, work.source.sequence(), &work.envelope)?;
            self.record_joined_coordination_mail(
                &work.turn,
                work.source.sequence(),
                &work.envelope,
            );
        }
        if let Some(receipt) = self.agent.received_peer_message_presentations.get_mut(id) {
            receipt.durable_input = Some(NativeReceipt::Committed { work, proof });
        }
        Ok(())
    }

    /// Gives the actor one already-sealed exact original native ACK candidate.
    pub(crate) fn pending_native_input_ack(&self) -> Option<(String, MessageNativeInputCommit)> {
        self.agent
            .received_peer_message_presentations
            .iter()
            .find_map(|(id, r)| match &r.durable_input {
                Some(NativeReceipt::Committed { proof, .. }) => Some((id.clone(), proof.clone())),
                _ => None,
            })
    }

    /// Records actual exact-slot settlement and resumes only the original live
    /// turn. Historical proof lookup with no receipt never schedules anything.
    pub(crate) fn complete_native_input_ack(
        &mut self,
        id: &str,
        now_ms: u64,
        continue_turn: bool,
    ) -> Result<()> {
        let work = self
            .agent
            .received_peer_message_presentations
            .get(id)
            .and_then(|r| match &r.durable_input {
                Some(NativeReceipt::Committed { work, .. }) => Some(work.clone()),
                _ => None,
            })
            .ok_or_else(|| MezError::invalid_state("original native ACK receipt missing"))?;
        if let Some(receipt) = self.agent.received_peer_message_presentations.get_mut(id) {
            receipt.durable_input = Some(NativeReceipt::Acknowledged);
        }
        if !continue_turn {
            // Transport settlement must survive shutdown, without publishing a
            // fresh provider or scheduler continuation after admission closed.
            self.remove_pending_agent_provider_task(&work.turn.turn_id);
            return Ok(());
        }
        if self.native_receive_turn_is_current(&work.turn) {
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
        }
        self.recover_received_peer_message_turn_scheduling(now_ms)?;
        let _ = self.flush_received_peer_message_presentations();
        Ok(())
    }

    /// Original application ownership must survive in the actual live runtime.
    fn native_receive_turn_is_current(&self, turn: &AgentTurnRecord) -> bool {
        self.agent_shell_store()
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
