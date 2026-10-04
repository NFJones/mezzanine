//! Actor-owned ordinary-turn steering receipt identities and local admission.
//!
//! Canonical chronology remains the execution source; this ledger only records
//! presentation delivery evidence. Equal submissions have distinct identities.
//! Admission uses exact prepared user-event sequences, never text or high-water
//! guesses. Auxiliary requests cannot acknowledge receipts. Terminal cleanup
//! settles unconsumed receipts without replaying input or effects. This initial
//! owner does not cover manual-compaction queues, persistence or log promotion.

use std::collections::BTreeSet;

use super::{AgentTurnRecord, RuntimeAgentProviderDispatch, RuntimeSessionService};
use crate::error::{MezError, Result};

/// Finite receipt/source retention for one active ordinary turn.
const CAPACITY: usize = 128;
/// Finite owner retention, including settled evidence awaiting presentation.
const OWNER_CAPACITY: usize = 256;
/// Exact accepted source bytes retained per owner, separate from entry count.
const SOURCE_CAPACITY: usize = 1024 * 1024;

/// Evidence of local task dispatch, not remote delivery or model cognition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Status {
    /// Canonically accepted but not included in an admitted task request.
    Pending,
    /// First admitted ordinary request generation containing this exact event.
    Admitted(u64),
    /// Turn ownership ended before an ordinary request consumed this event.
    NotSent,
}

/// Immutable accepted occurrence, with separately mutable delivery evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Receipt {
    /// Stable identity independent of content, timestamps and request retries.
    pub(crate) id: String,
    /// Canonical user event bound at acceptance, never reconstructed from text.
    pub(crate) sequence: u64,
    /// Exact model input, without presentation labels.
    pub(crate) input: String,
    /// Exact independently supplied user-facing source.
    pub(crate) display: String,
    /// Local admission or terminal-unconsumed evidence.
    pub(crate) status: Status,
}

/// One immutable turn/conversation/pane owner and its acceptance-ordered receipts.
#[derive(Debug)]
pub(crate) struct Receipts {
    turn: AgentTurnRecord,
    entries: Vec<Receipt>,
    /// Terminal ownership, not merely absence of pending entries, permits eviction.
    terminal: bool,
}

impl Receipts {
    /// Captures one actor-owned turn; no filesystem, clocks or model work occurs.
    fn new(turn: &AgentTurnRecord) -> Self {
        Self {
            turn: turn.clone(),
            entries: Vec::new(),
            terminal: false,
        }
    }

    /// Checks exact immutable identity without requiring unchanged ledger status.
    fn belongs_to(&self, turn: &AgentTurnRecord) -> bool {
        self.turn.turn_id == turn.turn_id
            && self.turn.conversation_id == turn.conversation_id
            && self.turn.pane_id == turn.pane_id
            && self.turn.agent_id == turn.agent_id
    }

    /// Acknowledges only pending exact occurrences, once, at a nonzero generation.
    fn admit(&mut self, generation: u64, sequences: &BTreeSet<u64>) {
        if generation == 0 {
            return;
        }
        for receipt in &mut self.entries {
            if receipt.status == Status::Pending && sequences.contains(&receipt.sequence) {
                receipt.status = Status::Admitted(generation);
            }
        }
    }

    /// Settles only unconsumed occurrences; admitted evidence is immutable.
    fn settle(&mut self) {
        self.terminal = true;
        for receipt in &mut self.entries {
            if receipt.status == Status::Pending {
                receipt.status = Status::NotSent;
            }
        }
    }
}

impl RuntimeSessionService {
    /// Checks finite receipt capacity before canonical input is committed.
    pub(crate) fn check_steering_receipt_capacity(
        &mut self,
        turn: &AgentTurnRecord,
        input: &str,
        display: &str,
    ) -> Result<()> {
        if !self.agent.steering_receipts.contains_key(&turn.turn_id)
            && self.agent.steering_receipts.len() >= OWNER_CAPACITY
        {
            let settled = self
                .agent
                .steering_receipts
                .iter()
                .find_map(|(id, owner)| owner.terminal.then(|| id.clone()));
            if let Some(id) = settled {
                self.agent.steering_receipts.remove(&id);
            } else {
                return Err(MezError::invalid_state("steering receipt owners exhausted"));
            }
        }
        let retained = self
            .agent
            .steering_receipts
            .get(&turn.turn_id)
            .map_or(0, |owner| {
                owner
                    .entries
                    .iter()
                    .map(|entry| entry.input.len() + entry.display.len())
                    .sum::<usize>()
            });
        if retained
            .saturating_add(input.len())
            .saturating_add(display.len())
            > SOURCE_CAPACITY
        {
            return Err(MezError::invalid_state(
                "steering receipt source budget exhausted",
            ));
        }
        if self
            .agent
            .steering_receipts
            .get(&turn.turn_id)
            .is_some_and(|owner| !owner.belongs_to(turn) || owner.entries.len() >= CAPACITY)
        {
            return Err(MezError::invalid_state(
                "steering receipt capacity or owner unavailable",
            ));
        }
        Ok(())
    }

    /// Records accepted canonical identity before fallible trace/presentation work.
    pub(crate) fn retain_steering_receipt(
        &mut self,
        turn: &AgentTurnRecord,
        sequence: u64,
        input: &str,
        display: &str,
    ) {
        self.agent
            .steering_receipts
            .entry(turn.turn_id.clone())
            .or_insert_with(|| Receipts::new(turn))
            .entries
            .push(Receipt {
                id: crate::storage::token_usage::new_token_usage_event_id(),
                sequence,
                input: input.into(),
                display: display.into(),
                status: Status::Pending,
            });
    }

    /// Records local ordinary dispatch admission after the actor lease succeeds.
    /// Exact prepared user-event identity, not snapshot high-water, is evidence.
    pub(crate) fn acknowledge_admitted_steering(
        &mut self,
        dispatch: &RuntimeAgentProviderDispatch,
    ) {
        if dispatch.auto_sizing.is_some()
            || dispatch.macro_judge_request.is_some()
            || dispatch.sandbox_failure_assessment_request.is_some()
            || dispatch.claim_generation == 0
            || !self
                .agent_shell_store()
                .get(&dispatch.turn.pane_id)
                .is_some_and(|session| {
                    session.session_id == dispatch.turn.conversation_id
                        && session.running_turn_id.as_deref()
                            == Some(dispatch.turn.turn_id.as_str())
                })
            || !self
                .agent
                .claimed_agent_provider_tasks
                .get(&dispatch.turn.turn_id)
                .is_some_and(|claim| {
                    claim.generation == dispatch.claim_generation
                        && claim.conversation_id == dispatch.turn.conversation_id
                        && claim.agent_id == dispatch.turn.agent_id
                })
        {
            return;
        }
        let context = dispatch.context.durable();
        let sequences = (0..context.blocks().len())
            .filter_map(|index| {
                let metadata = context.metadata_for_block(index)?;
                (metadata.semantic_kind() == mez_agent::ContextSemanticKind::UserEvent
                    && metadata.provider_owner().is_none())
                .then(|| metadata.event_sequence().map(|sequence| sequence.get()))
                .flatten()
            })
            .collect();
        if let Some(owner) = self.agent.steering_receipts.get_mut(&dispatch.turn.turn_id)
            && owner.belongs_to(&dispatch.turn)
        {
            owner.admit(dispatch.claim_generation, &sequences);
        }
    }

    /// Retains truthful unconsumed evidence when terminal cleanup releases a turn.
    pub(crate) fn settle_steering_receipts(&mut self, turn_id: &str) {
        if let Some(owner) = self.agent.steering_receipts.get_mut(turn_id) {
            owner.settle();
        }
    }

    /// Exposes acceptance-ordered evidence without a second input queue.
    #[cfg(test)]
    pub(crate) fn steering_receipts_for_tests(&self, turn_id: &str) -> &[Receipt] {
        self.agent
            .steering_receipts
            .get(turn_id)
            .map_or(&[], |owner| owner.entries.as_slice())
    }
}

#[cfg(test)]
mod tests;
