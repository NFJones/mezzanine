//! Original live-action custody for neutral terminal delivery facts.
//!
//! An actual root absence precedes context preflight. One receipt freezes the
//! real action owner, allocated chronology and archive target before installing
//! the candidate context. It survives task/pane replacement and Stop until the
//! original root publishes sender consumption. Historical positives carry only
//! their sealed proof: they never create context, a task, or provider work.

use super::*;
use crate::storage::mmp_custody::terminal_handoff::TerminalHandoffProjection;
use crate::storage::transcript::AgentTranscriptStore;
use mez_agent::messaging::{MessageTerminalHandoffAnchor, MessageTerminalHandoffCommit};
use mez_agent::transcript::{TranscriptEntry, TranscriptRole};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Actual post-context owner, independent of mutable runtime presentation.
#[derive(Debug)]
struct TerminalHandoffWork {
    owner: OutboundDeliveryOwner,
    source: MessageTerminalHandoffAnchor,
    target: AgentTranscriptStore,
    projection: Arc<TerminalHandoffProjection>,
}

/// Physical issuance and canonical commit are distinct from consumption.
#[derive(Debug)]
enum Phase {
    Pending,
    Writing,
    Committed {
        proof: MessageTerminalHandoffCommit,
        applied: bool,
        retry: Option<(Instant, Box<MezError>)>,
    },
}

/// Single bounded sender continuation; decoded rows cannot construct this owner.
#[derive(Debug)]
pub(in crate::runtime::agent) struct TerminalHandoffReceipt {
    work: Option<Arc<TerminalHandoffWork>>,
    phase: Phase,
}

impl RuntimeSessionService {
    /// Earlier ordinary producers and an already issued inference must settle
    /// before a new sender fact can take canonical chronology ownership. A
    /// deferral retains lookup custody in the actor and creates no memory fact.
    pub(crate) fn terminal_handoff_preflight_blocked(
        &self,
        source: &MessageTerminalHandoffAnchor,
    ) -> bool {
        let conversation = source.native_owner().conversation_id();
        self.persistence
            .has_pending_transcript_entries(conversation)
            || self.persistence.bookkeeping_pending(conversation)
            || self.native_input_pending_for_conversation(conversation)
            || self
                .agent
                .outbound_delivery_owners
                .get(&source.sequence())
                .is_some_and(|owner| self.agent_provider_task_is_claimed(&owner.turn_id))
    }

    /// Preflights only the real accepted action after qualified storage absence.
    /// No live mutation occurs if identity, chronology or projection is invalid.
    pub(crate) fn prepare_terminal_handoff(
        &mut self,
        lookup: &MessageTerminalHandoffAnchor,
    ) -> Result<bool> {
        if self.agent.terminal_handoff.is_some()
            || self.terminal_handoff_preflight_blocked(lookup)
            || !self
                .message_service()
                .terminal_handoff_owner_is_current(lookup)
        {
            return Ok(false);
        }
        self.prune_outbound_delivery_owners();
        let Some(owner) = self
            .agent
            .outbound_delivery_owners
            .get(&lookup.sequence())
            .cloned()
        else {
            return Ok(false);
        };
        let outcome = lookup.outcome();
        let Some(turn) = self.agent_turn_ledger().turn(&owner.turn_id).cloned() else {
            return Ok(false);
        };
        if owner.agent_id != lookup.sender().as_str()
            || owner.message_id_hash != message_id_hash(&outcome.message_id)
            || turn.agent_id != owner.agent_id
            || turn.pane_id != owner.pane_id
            || turn.conversation_id != owner.conversation_id
            || turn.started_at_unix_seconds != owner.started_at_unix_seconds
            || lookup.native_owner().conversation_id() != owner.conversation_id
            || !matches!(
                turn.state,
                AgentTurnState::Queued | AgentTurnState::Running | AgentTurnState::Blocked
            )
            || !self
                .agent_shell_store()
                .get(&owner.pane_id)
                .is_some_and(|shell| shell.session_id == owner.conversation_id)
            || self.native_input_pending_for_conversation(&owner.conversation_id)
        {
            return Ok(false);
        }
        let Some(mut candidate) = self.agent_turn_contexts().get(&owner.turn_id).cloned() else {
            return Ok(false);
        };
        let source = self
            .message_service()
            .capture_terminal_handoff(lookup.sender(), &outcome)?;
        let (label, content) = mez_agent::transcript::terminal_outcome_reference(&outcome)
            .map_err(|_| MezError::invalid_state("MMP terminal fact cannot be encoded"))?;
        // A memory label alone is neither qualified absence nor a durable receipt.
        // Reusing it could assign an unrelated event to this canonical occurrence.
        if candidate.blocks().iter().any(|block| block.label == label) {
            return Err(MezError::invalid_state(
                "MMP terminal fact already exists without its original receipt",
            ));
        }
        candidate.append_runtime_fact_event(label.clone(), content.clone())?;
        let event = candidate
            .chronology()
            .last()
            .ok_or_else(|| MezError::invalid_state("MMP terminal fact chronology missing"))?;
        let prefix = self.project_canonical_message_prefix(
            &turn,
            &candidate,
            self.agent_turn_imported_history_sequence_high_water(&turn.turn_id),
            event.sequence().get(),
        )?;
        let projection = Arc::new(TerminalHandoffProjection::new(
            TranscriptEntry {
                conversation_id: owner.conversation_id.clone(),
                sequence: 1,
                created_at_unix_seconds: owner.started_at_unix_seconds,
                role: TranscriptRole::System,
                turn_id: owner.turn_id.clone(),
                agent_id: owner.agent_id.clone(),
                pane_id: owner.pane_id.clone(),
                content,
            },
            event.sequence().get(),
            label,
            prefix,
        )?);
        let target = self.agent_transcript_store().cloned().ok_or_else(|| {
            MezError::invalid_state("MMP terminal canonical transcript target missing")
        })?;
        let work = Arc::new(TerminalHandoffWork {
            owner,
            source,
            target,
            projection,
        });
        self.agent_turn_contexts_mut()
            .insert(turn.turn_id, candidate);
        self.agent.terminal_handoff = Some(TerminalHandoffReceipt {
            work: Some(work),
            phase: Phase::Pending,
        });
        Ok(true)
    }

    /// Borrows only a preflighted candidate after earlier ordinary writers drain.
    pub(crate) fn pending_terminal_handoff_work(
        &self,
    ) -> Option<(
        MessageTerminalHandoffAnchor,
        AgentTranscriptStore,
        Arc<TerminalHandoffProjection>,
    )> {
        let receipt = self.agent.terminal_handoff.as_ref()?;
        let work = receipt.work.as_ref()?;
        if !matches!(receipt.phase, Phase::Pending)
            || self
                .persistence
                .has_pending_transcript_entries(&work.owner.conversation_id)
            || self
                .persistence
                .bookkeeping_pending(&work.owner.conversation_id)
        {
            return None;
        }
        Some((
            work.source.clone(),
            work.target.clone(),
            work.projection.clone(),
        ))
    }

    /// Physical ownership changes only after actual root issuance succeeds.
    pub(crate) fn note_terminal_handoff_write_issued(&mut self) {
        if let Some(receipt) = &mut self.agent.terminal_handoff
            && matches!(receipt.phase, Phase::Pending)
        {
            receipt.phase = Phase::Writing;
        }
    }

    /// Retains the genuine root result before any fallible frontier application.
    /// Historical results have no application owner and cannot create one.
    pub(crate) fn retain_terminal_handoff_commit(&mut self, proof: MessageTerminalHandoffCommit) {
        let work = self
            .agent
            .terminal_handoff
            .take()
            .and_then(|receipt| receipt.work);
        self.agent.terminal_handoff = Some(TerminalHandoffReceipt {
            work,
            phase: Phase::Committed {
                proof,
                applied: false,
                retry: None,
            },
        });
    }

    /// Applies only a current original sender's actually observed archive frontier.
    /// No reply/wait/coordination/scheduler effects belong to this local fact.
    pub(crate) fn apply_terminal_handoff_commit(&mut self) -> Result<()> {
        let Some(receipt) = self.agent.terminal_handoff.as_ref() else {
            return Ok(());
        };
        let Phase::Committed {
            proof,
            applied: false,
            retry,
        } = &receipt.phase
        else {
            return Ok(());
        };
        if retry.as_ref().is_some_and(|(at, _)| *at > Instant::now()) {
            return Ok(());
        }
        if let Some(work) = &receipt.work
            && (work.source.sequence() != proof.anchor().sequence()
                || work.source.binding_index() != proof.anchor().binding_index()
                || work.source.source_sha256() != proof.anchor().source_sha256()
                || work.source.native_owner() != proof.anchor().native_owner())
        {
            return Err(MezError::invalid_state(
                "original MMP terminal handoff result changed",
            ));
        }
        if proof.record().archive_observed
            && self
                .message_service()
                .terminal_handoff_owner_is_current(proof.anchor())
            && let Some(pane) = proof.anchor().sender().as_str().strip_prefix("agent-")
            && let Some(shell) = self.agent_shell_store().get(pane)
            && shell.session_id == proof.anchor().native_owner().conversation_id()
        {
            let pane = pane.to_owned();
            let high = shell
                .transcript_high_water
                .max(proof.record().archive_sequence);
            let raw = shell
                .transcript_entries
                .checked_add(high - shell.transcript_high_water)
                .ok_or_else(|| {
                    MezError::invalid_state("MMP terminal transcript frontier exhausted")
                })?;
            self.agent_shell_store_mut().restore_transcript_position(
                &pane,
                mez_agent::AgentTranscriptPosition {
                    logical_high_water: high,
                    active_raw_entries: raw,
                },
            )?;
        }
        if let Some(receipt) = &mut self.agent.terminal_handoff
            && let Phase::Committed { applied, retry, .. } = &mut receipt.phase
        {
            *applied = true;
            *retry = None;
        }
        Ok(())
    }

    /// Due application and consumption admission retries share one local owner.
    pub(crate) fn terminal_handoff_completion_at(&self, root_idle: bool) -> Option<Instant> {
        let Phase::Committed { applied, retry, .. } = &self.agent.terminal_handoff.as_ref()?.phase
        else {
            return None;
        };
        (!applied || root_idle).then(|| retry.as_ref().map_or_else(Instant::now, |(at, _)| *at))
    }

    /// Pre-issuance errors retain the actual proof rather than retry an idle root.
    pub(crate) fn defer_terminal_handoff_completion(&mut self, error: MezError) {
        if let Some(receipt) = &mut self.agent.terminal_handoff
            && let Phase::Committed { retry, .. } = &mut receipt.phase
        {
            *retry = Some((Instant::now() + Duration::from_secs(1), Box::new(error)));
        }
    }

    /// Gives the common terminal lane only an applied, due, sealed positive.
    pub(crate) fn pending_terminal_handoff_consumption(
        &self,
    ) -> Option<MessageTerminalHandoffCommit> {
        let Phase::Committed {
            proof,
            applied: true,
            retry,
        } = &self.agent.terminal_handoff.as_ref()?.phase
        else {
            return None;
        };
        retry
            .as_ref()
            .is_none_or(|(at, _)| *at <= Instant::now())
            .then(|| proof.clone())
    }

    /// Actual lower adoption is the only normal release of this runtime owner.
    pub(crate) fn complete_terminal_handoff_consumption(&mut self) {
        self.agent.terminal_handoff = None;
        self.prune_outbound_delivery_owners();
    }

    /// Shutdown retains both original live work and historical positive custody.
    pub(crate) fn has_pending_terminal_handoff(&self) -> bool {
        self.agent.terminal_handoff.is_some()
    }

    /// Provider, restore and peer continuations wait for this original fact.
    pub(crate) fn terminal_handoff_pending_for_conversation(&self, conversation: &str) -> bool {
        self.agent.terminal_handoff.as_ref().is_some_and(|receipt| {
            receipt.work.as_ref().map_or_else(
                || match &receipt.phase {
                    Phase::Committed { proof, .. } => {
                        proof.anchor().native_owner().conversation_id() == conversation
                    }
                    _ => false,
                },
                |work| work.owner.conversation_id == conversation,
            )
        })
    }

    /// Canonical input and sender facts share provider/conversation gates.
    pub(crate) fn canonical_message_pending_for_conversation(&self, conversation: &str) -> bool {
        self.native_input_pending_for_conversation(conversation)
            || self.terminal_handoff_pending_for_conversation(conversation)
    }

    /// Restore may build an initial native permit, but cannot replace owned facts.
    pub(crate) fn canonical_message_receipt_pending_for_conversation(
        &self,
        conversation: &str,
    ) -> bool {
        self.native_input_receipt_pending_for_conversation(conversation)
            || self.terminal_handoff_pending_for_conversation(conversation)
    }

    /// Ordinary producers must not race either actual canonical writer.
    pub(in crate::runtime::agent) fn canonical_message_writing_conversations(
        &self,
    ) -> std::collections::BTreeSet<String> {
        let mut conversations = self.native_input_writing_conversations();
        if let Some(conversation) = self.terminal_handoff_writing_conversation() {
            conversations.insert(conversation.to_owned());
        }
        conversations
    }

    /// Only issued canonical work excludes ordinary transcript producers.
    pub(in crate::runtime::agent) fn terminal_handoff_writing_conversation(&self) -> Option<&str> {
        let receipt = self.agent.terminal_handoff.as_ref()?;
        matches!(receipt.phase, Phase::Writing)
            .then(|| {
                receipt
                    .work
                    .as_ref()
                    .map(|w| w.owner.conversation_id.as_str())
            })
            .flatten()
    }
}
