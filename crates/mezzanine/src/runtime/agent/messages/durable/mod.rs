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

mod completion;

/// Monotonic application/admission backoff, never a storage retry or proof.
#[derive(Debug, Clone)]
pub(in crate::runtime::agent) struct NativeRetry {
    at: std::time::Instant,
    _failure: MezError,
}

/// Exact ACK already settled, but its original scheduler handoff still belongs
/// to this receipt. Presentation cleanup cannot discard this continuation.
#[derive(Debug, Clone)]
pub(in crate::runtime::agent) struct NativeContinuation {
    work: Arc<NativeWork>,
    continue_turn: bool,
    retry: Option<NativeRetry>,
}

/// Speculative data prepared before context commit, carrying no receive proof.
#[derive(Debug, Clone)]
struct PreparedNativeInput {
    target: AgentTranscriptStore,
    projection: Arc<NativeInputProjection>,
    turn: AgentTurnRecord,
    envelope: Arc<Envelope>,
}

/// One actual lower absence/selection, with an optional preflighted candidate.
/// Only the later private context boundary can turn it into `NativeWork`.
#[derive(Debug, Clone)]
pub(in crate::runtime::agent) struct NativeInputPermit {
    source: MessageNativeInputAnchor,
    prepared: Option<PreparedNativeInput>,
}

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
        applied: bool,
        retry: Option<NativeRetry>,
    },
    Acknowledged {
        continuation: Option<NativeContinuation>,
    },
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
                Some(
                    NativeReceipt::Pending(work)
                    | NativeReceipt::Committed { work, .. }
                    | NativeReceipt::Acknowledged {
                        continuation: Some(NativeContinuation { work, .. }),
                    },
                ) if work.turn.turn_id == turn => Some(work.projection.event_sequence()),
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
        self.agent.native_input_permit = Some(NativeInputPermit {
            source: selected,
            prepared: None,
        });
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
                .is_some_and(|permit| {
                    let source = &permit.source;
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

    /// Builds the complete immutable candidate before any live context or task
    /// mutation. The real context allocator supplies the event ID; this method
    /// never predicts IDs or manufactures a receipt from transcript metadata.
    pub(crate) fn prepare_native_input_context(
        &mut self,
        recipient: &AgentId,
        sequence: u64,
        turn: &AgentTurnRecord,
        context: &mez_agent::AgentContext,
        imported_high_water: u64,
    ) -> Result<()> {
        if !self.native_input_storage_required() {
            return Ok(());
        }
        let source = &self
            .agent
            .native_input_permit
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("original native input permit missing"))?
            .source;
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
        let event = context
            .chronology()
            .iter()
            .rev()
            .find(|e| {
                e.block().source == mez_agent::ContextSourceKind::PeerMessage
                    && e.block().label == label
            })
            .ok_or_else(|| MezError::invalid_state("planned native input chronology missing"))?;
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
        let prefix = self.project_native_input_prefix(
            turn,
            context,
            imported_high_water,
            event.sequence().get(),
        )?;
        let projection = Arc::new(
            NativeInputProjection::new(row, event.sequence().get(), label)?.with_prefix(prefix)?,
        );
        let target = self
            .agent_transcript_store()
            .cloned()
            .ok_or_else(|| MezError::invalid_state("canonical native transcript store missing"))?;
        let prepared = PreparedNativeInput {
            target,
            projection,
            turn: turn.clone(),
            envelope,
        };
        self.agent
            .native_input_permit
            .as_mut()
            .ok_or_else(|| MezError::invalid_state("original native input permit missing"))?
            .prepared = Some(prepared);
        Ok(())
    }

    /// Commits a real allocated peer occurrence only after its entire canonical
    /// projection passes preflight. Failure leaves the original live context
    /// untouched and the accepted lower source pending for retry.
    pub(super) fn append_received_peer_context(
        &mut self,
        recipient: &AgentId,
        sequence: u64,
        turn: &AgentTurnRecord,
        label: String,
        content: String,
    ) -> Result<()> {
        if !self.native_input_storage_required() {
            self.agent_turn_contexts_mut()
                .get_mut(&turn.turn_id)
                .ok_or_else(|| {
                    MezError::invalid_state("runtime agent turn context is unavailable")
                })?
                .append_peer_message_event(label, content)?;
            return Ok(());
        }
        let mut candidate = self
            .agent_turn_contexts()
            .get(&turn.turn_id)
            .cloned()
            .ok_or_else(|| MezError::invalid_state("runtime agent turn context is unavailable"))?;
        candidate.append_peer_message_event(label, content)?;
        self.prepare_native_input_context(
            recipient,
            sequence,
            turn,
            &candidate,
            self.agent_turn_imported_history_sequence_high_water(&turn.turn_id),
        )?;
        self.agent_turn_contexts_mut()
            .insert(turn.turn_id.clone(), candidate);
        Ok(())
    }

    /// Moves the preflighted candidate only after the actual context occurrence
    /// exists. No new prefix encoding or size admission can fail after commit.
    pub(super) fn capture_native_input_work(
        &mut self,
        recipient: &AgentId,
        sequence: u64,
        turn: &AgentTurnRecord,
    ) -> Result<Option<NativeReceipt>> {
        if !self.native_input_storage_required() {
            return Ok(None);
        }
        let permit = self
            .agent
            .native_input_permit
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("original native input permit missing"))?;
        let prepared = permit
            .prepared
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("native input preflight missing"))?;
        if permit.source.recipient() != recipient
            || permit.source.sequence() != sequence
            || prepared.turn != *turn
            || !self
                .agent_turn_contexts()
                .get(&turn.turn_id)
                .is_some_and(|context| {
                    context.chronology().iter().any(|event| {
                        prepared.projection.matches_event(
                            event.sequence().get(),
                            &event.block().label,
                            &event.block().content,
                        )
                    })
                })
        {
            return Err(MezError::invalid_state(
                "preflighted native input changed before commit",
            ));
        }
        let permit = self
            .agent
            .native_input_permit
            .take()
            .ok_or_else(|| MezError::invalid_state("original native input permit missing"))?;
        let prepared = permit
            .prepared
            .ok_or_else(|| MezError::invalid_state("native input preflight missing"))?;
        Ok(Some(NativeReceipt::Pending(Arc::new(NativeWork {
            source: permit.source,
            target: prepared.target,
            projection: prepared.projection,
            turn: prepared.turn,
            envelope: prepared.envelope,
        }))))
    }

    /// Provider and peer/scheduler continuations wait through exact native ACK.
    pub(crate) fn native_input_pending_for_conversation(&self, conversation: &str) -> bool {
        self.agent
            .native_input_permit
            .as_ref()
            .is_some_and(|p| p.source.native_owner().conversation_id() == conversation)
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
                        Some(
                            NativeReceipt::Pending(_)
                                | NativeReceipt::Committed { .. }
                                | NativeReceipt::Acknowledged {
                                    continuation: Some(_)
                                }
                        )
                    )
                })
    }

    /// An actual live receive owner must finish its application work before a
    /// historical c1 lookup can authorize ACK. Saved display data never matches.
    pub(crate) fn native_input_source_is_owned(&self, source: &MessageNativeInputAnchor) -> bool {
        self.agent
            .received_peer_message_presentations
            .values()
            .any(|receipt| {
                let work = match &receipt.durable_input {
                    Some(NativeReceipt::Pending(work) | NativeReceipt::Committed { work, .. }) => {
                        work
                    }
                    _ => return false,
                };
                work.source.sequence() == source.sequence()
                    && work.source.binding_index() == source.binding_index()
                    && work.source.source_sha256() == source.source_sha256()
                    && work.source.recipient() == source.recipient()
                    && work.source.native_owner() == source.native_owner()
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
}
