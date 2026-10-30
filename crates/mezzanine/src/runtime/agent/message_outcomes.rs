//! Payload-free terminal MMP facts for their original live action owner.
//!
//! Lower custody owns reservations and finite receipt retention. This adapter
//! commits a neutral canonical reference only when the exact accepted action,
//! sender registration interval, task context and conversation still agree.
//! Receipt consumption follows that commit. It never manufactures peer mail,
//! settles a wait, schedules provider work, changes an accepted action result,
//! or retries effects. Ended or unprovable owners retain the lower receipt until
//! its documented finite expiry; another conversation cannot inherit it.

use super::{AgentId, AgentTurnState, Result, RuntimeSessionService};
use crate::error::MezError;
use mez_agent::ContextSourceKind;
use mez_agent::messaging::MessageErrorKind;
use sha2::{Digest, Sha256};

/// Private same-runtime ownership captured only after native send acceptance.
/// One row references one canonical pending occurrence, never a new mailbox.
#[derive(Debug, Clone)]
pub(super) struct OutboundDeliveryOwner {
    /// Original sending task; a later task cannot inherit its action.
    turn_id: String,
    /// Sending turn incarnation captured at acceptance, not later delivery.
    started_at_unix_seconds: u64,
    /// Native sender identity, qualified by lower immutable registration state.
    agent_id: String,
    /// Original pane and conversation, independent of current presentation.
    pane_id: String,
    /// Original conversation, never selected from later focus or binding.
    conversation_id: String,
    /// Exact immutable accepted occurrence, shared by all recipient slots.
    message_id_hash: [u8; 32],
    /// Execution group frozen at first acceptance; later batches cannot adopt it.
    group: Option<mez_agent::ContextExecutionGroupId>,
    /// Exact accepted sequence, independent of provider batch replacement.
    sequence: u64,
}

impl RuntimeSessionService {
    /// Prunes same-runtime owner metadata to canonical custody. The lower
    /// pending bound (at most 1000 occurrences) also bounds this map. No model
    /// result, mutable presentation identity or current batch can create rows.
    fn prune_outbound_delivery_owners(&mut self) {
        let service = self.control.message_service();
        self.agent.outbound_delivery_owners.retain(|_, owner| {
            AgentId::opaque(owner.agent_id.clone()).is_some_and(|sender| {
                service
                    .owned_pending_delivery_id(&sender, owner.sequence)
                    .is_some_and(|id| message_id_hash(id) == owner.message_id_hash)
            })
        });
    }

    /// Captures a native action owner after real lower acceptance, before any
    /// fallible delivery/projection work. Repeated acceptance reuses the same
    /// immutable row. Zero-recipient/already-committed sends need no metadata.
    pub(super) fn retain_outbound_delivery_owner(
        &mut self,
        turn: &mez_agent::AgentTurnRecord,
        action: &mez_agent::AgentAction,
        delivery: &mez_agent::messaging::Delivery,
    ) {
        self.prune_outbound_delivery_owners();
        let Some(sender) = AgentId::opaque(turn.agent_id.clone()) else {
            return;
        };
        let group = self
            .agent
            .agent_execution_groups_by_turn
            .get(&turn.turn_id)
            .and_then(|groups| groups.get(&action.id))
            .cloned();
        if delivery.accepted
            && delivery
                .message_id
                .strip_prefix(&turn.turn_id)
                .and_then(|suffix| suffix.strip_prefix(':'))
                == Some(action.id.as_str())
            && self
                .control
                .message_service()
                .owned_pending_delivery_id(&sender, delivery.sequence)
                == Some(delivery.message_id.as_str())
        {
            self.agent
                .outbound_delivery_owners
                .entry(delivery.sequence)
                .or_insert_with(|| OutboundDeliveryOwner {
                    turn_id: turn.turn_id.clone(),
                    started_at_unix_seconds: turn.started_at_unix_seconds,
                    agent_id: turn.agent_id.clone(),
                    pane_id: turn.pane_id.clone(),
                    conversation_id: turn.conversation_id.clone(),
                    message_id_hash: message_id_hash(&delivery.message_id),
                    sequence: delivery.sequence,
                    group,
                });
        }
    }

    /// Proves that this batch actually accepted a send to the exact recipient.
    /// A duplicated action from an earlier batch retains its first group and
    /// cannot close a later question. This does not imply recipient observation.
    pub(super) fn runtime_accepted_reply_targets(
        &self,
        turn: &mez_agent::AgentTurnRecord,
        delivery: &mez_agent::messaging::Delivery,
        group: &mez_agent::ContextExecutionGroupId,
        recipient: &str,
    ) -> bool {
        let Some(owner) = self.agent.outbound_delivery_owners.get(&delivery.sequence) else {
            return false;
        };
        let (Some(sender), Some(recipient)) = (
            AgentId::opaque(turn.agent_id.clone()),
            AgentId::opaque(recipient.to_string()),
        ) else {
            return false;
        };
        delivery.accepted
            && owner.turn_id == turn.turn_id
            && owner.agent_id == turn.agent_id
            && owner.pane_id == turn.pane_id
            && owner.conversation_id == turn.conversation_id
            && owner.started_at_unix_seconds == turn.started_at_unix_seconds
            && owner.message_id_hash == message_id_hash(&delivery.message_id)
            && owner.group.as_ref() == Some(group)
            && self
                .control
                .message_service()
                .owned_pending_delivery_targets(&sender, delivery.sequence, &recipient)
    }

    /// Returns the exact native action task behind a still-owned accepted send.
    /// Sender labels, envelope correlation text and replay bytes cannot create
    /// this proof or transfer an old attempt into a replacement task.
    pub(super) fn runtime_outbound_delivery_origin(
        &self,
        sequence: u64,
        envelope: &super::Envelope,
    ) -> Option<mez_agent::AgentTurnRecord> {
        let owner = self.agent.outbound_delivery_owners.get(&sequence)?;
        let turn = self.agent_turn_ledger().turn(&owner.turn_id)?;
        if owner.sequence != sequence
            || owner.message_id_hash != message_id_hash(&envelope.id)
            || owner.agent_id != envelope.sender.agent_id.as_str()
            || turn.agent_id != owner.agent_id
            || turn.pane_id != owner.pane_id
            || turn.conversation_id != owner.conversation_id
            || turn.started_at_unix_seconds != owner.started_at_unix_seconds
            || self
                .control
                .message_service()
                .owned_pending_delivery_id(&envelope.sender.agent_id, sequence)
                != Some(envelope.id.as_str())
        {
            return None;
        }
        Some(turn.clone())
    }

    /// Commits exact original-action terminal facts, then releases their lower
    /// receipts. Returns consumed count. Missing owners remain bounded lower
    /// custody; invalid context/receipt evidence fails without consuming it.
    pub(crate) fn handoff_runtime_message_delivery_outcomes(&mut self) -> Result<usize> {
        self.prune_outbound_delivery_owners();
        let owners = self
            .agent
            .outbound_delivery_owners
            .values()
            .filter(|owner| {
                self.agent_turn_ledger()
                    .turn(&owner.turn_id)
                    .is_some_and(|current| {
                        current.agent_id == owner.agent_id
                            && current.conversation_id == owner.conversation_id
                            && current.pane_id == owner.pane_id
                            && matches!(
                                current.state,
                                AgentTurnState::Queued
                                    | AgentTurnState::Running
                                    | AgentTurnState::Blocked
                            )
                    })
                    && self.agent_turn_contexts().contains_key(&owner.turn_id)
                    && self
                        .agent_shell_store()
                        .get(&owner.pane_id)
                        .is_some_and(|shell| shell.session_id == owner.conversation_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        let senders = owners
            .iter()
            .map(|owner| owner.agent_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let mut consumed = 0;
        for sender in senders {
            let Some(sender) = AgentId::opaque(sender) else {
                continue;
            };
            let mut remaining = 100;
            for owner in owners
                .iter()
                .filter(|owner| owner.agent_id == sender.as_str())
            {
                if remaining == 0 {
                    break;
                }
                let outcomes = match self
                    .control
                    .message_service()
                    .delivery_outcomes_for_occurrence(&sender, owner.sequence, remaining)
                {
                    Ok(outcomes) => outcomes,
                    Err(error) if error.kind() == MessageErrorKind::Forbidden => continue,
                    Err(error) => return Err(error.into()),
                };
                remaining -= outcomes.len();
                for outcome in outcomes {
                    if message_id_hash(&outcome.message_id) != owner.message_id_hash {
                        continue;
                    }
                    let Some(turn) = self.agent_turn_ledger().turn(&owner.turn_id).cloned() else {
                        continue;
                    };
                    let recipient_hash = message_id_hash(&outcome.recipient.recipient)
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>();
                    let label = format!(
                        "mmp-delivery-outcome sequence={} recipient={recipient_hash} floor={}",
                        outcome.sequence, outcome.recipient.registered_after_sequence
                    );
                    let content = format!(
                        "Accepted MMP recipient delivery ended without recipient commit. This is a local delivery fact, not a peer answer, approval, task success or permission to retry effects. The original send action remains accepted.\n{}",
                        serde_json::to_string(&outcome).map_err(|_| MezError::invalid_state(
                            "MMP delivery outcome cannot be encoded"
                        ))?
                    );
                    let context = self
                        .agent_turn_contexts_mut()
                        .get_mut(&turn.turn_id)
                        .ok_or_else(|| {
                            MezError::invalid_state("MMP outcome owner context is unavailable")
                        })?;
                    if let Some(existing) =
                        context.blocks().iter().find(|block| block.label == label)
                    {
                        if existing.source != ContextSourceKind::RuntimeHint
                            || existing.content != content
                        {
                            return Err(MezError::invalid_state(
                                "MMP outcome canonical evidence conflicts",
                            ));
                        }
                    } else {
                        context.append_runtime_fact_event(label, content)?;
                    }
                    consumed += usize::from(
                        self.control
                            .message_service_mut()
                            .acknowledge_delivery_outcome(&sender, &outcome)?,
                    );
                }
            }
        }
        self.prune_outbound_delivery_owners();
        Ok(consumed)
    }
}

/// Fixed-size identity commitment avoids copying arbitrarily long action IDs
/// into the bounded owner map. Full IDs remain at canonical lower custody.
fn message_id_hash(id: &str) -> [u8; 32] {
    Sha256::digest(id.as_bytes()).into()
}
