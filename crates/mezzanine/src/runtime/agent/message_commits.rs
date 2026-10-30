//! Native canonical receive proof independent of live turn-context lifetime.
//!
//! Only the post-context native receipt factory seals commit proof. Restored
//! presentation projections cannot assert it. Ordered cursor reconciliation
//! consults only that bounded same-runtime proof, never presentation text or
//! historical context labels.
//! A cumulative native ACK cannot claim an offered but uncommitted prefix.
//! Terminal maintenance preserves proven commits while an earlier prefix waits;
//! exact recipient retirement commits only the proven occurrences atomically.
//! None of these operations admits tasks, schedules providers or retries work.

use super::{AgentId, Result, RuntimeSessionService};
use mez_agent::messaging::SequencedEnvelope;
use std::collections::BTreeSet;

impl RuntimeSessionService {
    /// Tests exact native commit evidence. A sealed live receipt remains proof
    /// after Stop, failed-backing moves or context cleanup. Snapshot projections
    /// and historical context labels are never independent commit authority.
    pub(crate) fn runtime_native_message_commit_is_proven(
        &self,
        recipient: &AgentId,
        message: &SequencedEnvelope,
    ) -> bool {
        let identity = Self::received_peer_message_presentation_id(
            recipient,
            message.sequence,
            &message.envelope,
        );
        if self
            .agent
            .received_peer_message_presentations
            .get(&identity)
            .is_some_and(|receipt| {
                receipt.native_commit_proven
                    && receipt.recipient_agent_id == *recipient
                    && receipt.sequence == message.sequence
            })
        {
            return true;
        }
        false
    }

    /// Selects current native recipients from existing task/commit owners, not
    /// mutable discovery labels or a new mailbox. Restored display rows alone
    /// never add a native commit owner.
    fn runtime_native_commit_recipients(&self) -> BTreeSet<String> {
        self.agent
            .received_peer_message_presentations
            .values()
            .filter(|receipt| receipt.native_commit_proven)
            .map(|receipt| receipt.recipient_agent_id.to_string())
            .collect()
    }

    /// Lists exact offered native commits that still await their transport
    /// cursor. The lower service validates the live registration incarnation;
    /// this bounded set protects real canonical input from terminal expiry.
    pub(super) fn proven_runtime_message_commits(&self) -> Result<Vec<(AgentId, u64)>> {
        let mut result = Vec::new();
        for recipient in self.runtime_native_commit_recipients() {
            let Some(recipient) = AgentId::opaque(recipient) else {
                continue;
            };
            if self
                .control
                .message_service()
                .subscription(&recipient)
                .is_none()
            {
                continue;
            }
            for message in self
                .control
                .message_service()
                .offered_uncommitted_deliveries(&recipient)?
            {
                if self.runtime_native_message_commit_is_proven(&recipient, &message) {
                    result.push((recipient.clone(), message.sequence));
                }
            }
        }
        Ok(result)
    }

    /// Advances only a prefix whose own offered occurrences have canonical
    /// native proof. An earlier uncommitted offer leaves the cursor unchanged;
    /// a later live occurrence cannot silently ACK it. Foreign/forged endpoints
    /// and unavailable lower evidence still fail through lower validation.
    pub(crate) fn advance_runtime_message_subscription(
        &mut self,
        recipient: &AgentId,
        sequence: u64,
    ) -> Result<bool> {
        let offered = self
            .control
            .message_service()
            .offered_uncommitted_deliveries(recipient)?;
        if offered
            .iter()
            .filter(|message| message.sequence <= sequence)
            .any(|message| !self.runtime_native_message_commit_is_proven(recipient, message))
        {
            return Ok(false);
        }
        if !self
            .control
            .message_service()
            .can_advance_subscription(recipient, sequence)?
        {
            return Ok(false);
        }
        self.control
            .message_service_mut()
            .advance_subscription(recipient, sequence)?;
        Ok(true)
    }

    /// Finishes interrupted commits in occurrence order without recreating
    /// input or granting admission. Expiry/orphan scheduling classification is
    /// owned by the caller before this changes transport evidence.
    pub(super) fn reconcile_committed_runtime_peer_messages(&mut self) -> Result<()> {
        for recipient in self.runtime_native_commit_recipients() {
            let Some(recipient) = AgentId::opaque(recipient) else {
                continue;
            };
            if self
                .control
                .message_service()
                .subscription(&recipient)
                .is_none()
            {
                continue;
            }
            for message in self
                .control
                .message_service()
                .offered_uncommitted_deliveries(&recipient)?
            {
                if !self.runtime_native_message_commit_is_proven(&recipient, &message) {
                    break;
                }
                if !self.advance_runtime_message_subscription(&recipient, message.sequence)? {
                    break;
                }
            }
        }
        Ok(())
    }

    /// A ready canonical prefix needs one transport pass, while a stable
    /// blocked prefix must not keep a permanent timer or replay committed mail.
    pub(super) fn has_reconcilable_runtime_message_commits(&self) -> Result<bool> {
        for recipient in self.runtime_native_commit_recipients() {
            let Some(recipient) = AgentId::opaque(recipient) else {
                continue;
            };
            if self
                .control
                .message_service()
                .subscription(&recipient)
                .is_none()
            {
                continue;
            }
            if let Some(first) = self
                .control
                .message_service()
                .offered_uncommitted_deliveries(&recipient)?
                .first()
                && self.runtime_native_message_commit_is_proven(&recipient, first)
                && self
                    .control
                    .message_service()
                    .can_advance_subscription(&recipient, first.sequence)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Ends original native receive/transfer owners before replacing runtime
    /// state. No live private proof or outbound action owner may alias a restored
    /// occurrence with the same public IDs. Snapshot display rows are rebuilt
    /// separately and never restore these capabilities.
    pub(crate) fn retire_runtime_message_transfers_for_restore(&mut self) -> Result<()> {
        let recipients = self
            .runtime_native_commit_recipients()
            .into_iter()
            .chain(
                self.agent_turn_ledger()
                    .turns()
                    .iter()
                    .map(|turn| turn.agent_id.clone()),
            )
            .collect::<BTreeSet<_>>();
        for recipient in recipients {
            let Some(recipient) = AgentId::opaque(recipient) else {
                continue;
            };
            self.retire_runtime_message_identity_after_native_commits(&recipient)?;
        }
        self.agent.received_peer_message_presentations.clear();
        self.agent.outbound_delivery_owners.clear();
        self.retire_request_waits_for_restore();
        Ok(())
    }

    /// Ends a native recipient only after committing its exact proven offers.
    /// No cumulative cursor can invent commits for other unsatisfied rows.
    /// All lower validation precedes retirement; receipt cleanup follows it.
    pub(crate) fn retire_runtime_message_identity_after_native_commits(
        &mut self,
        recipient: &AgentId,
    ) -> Result<bool> {
        let mut committed = Vec::new();
        if self
            .control
            .message_service()
            .subscription(recipient)
            .is_some()
        {
            for message in self
                .control
                .message_service()
                .offered_uncommitted_deliveries(recipient)?
            {
                if self.runtime_native_message_commit_is_proven(recipient, &message) {
                    committed.push(message.sequence);
                }
            }
        }
        let retired = self
            .control
            .message_service_mut()
            .retire_agent_identity_with_committed_occurrences(recipient, &committed)?;
        self.agent
            .received_peer_message_presentations
            .retain(|_, receipt| receipt.recipient_agent_id != *recipient);
        self.prune_request_wait_owners();
        Ok(retired)
    }
}
