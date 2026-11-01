//! Unpublished tracked-request ownership paired with the sole delivery ledger.
//!
//! A reservation retains the original native caller and charges future handoff
//! headroom before storage begins. It is deliberately absent from receipt scans,
//! expiry and conversation teardown: none proves an issued write did not commit.
//! The product must settle the retained original even after its caller stops.
//! Only a matching private storage fact can publish or release this ownership.

use super::*;
use crate::messaging::{
    MessageAcceptanceCommit, MessageAcceptanceRejected, MessageAcceptanceTicket,
};

/// One original caller retained while its canonical occurrence is unpublished.
#[derive(Debug)]
pub(super) struct PendingRequest {
    /// Future dependency identity, allocated once and never reused.
    generation: u64,
    /// Exact original conversation and turn, unaffected by later actor changes.
    origin: MessageRequestOrigin,
    /// Capability paired with the lower owner's single unpublished occurrence.
    ticket: MessageAcceptanceTicket,
    /// Strict deadline computed from original acceptance time and wire TTL.
    expires_at_ms: Option<u64>,
    /// Complete reserved count including future task and terminal edges.
    units: usize,
    /// Complete metadata/proof/ticket and future handoff reservation.
    metadata_bytes: usize,
}

impl MessageRequests {
    /// Reserves original caller ownership and lower custody before storage I/O.
    /// No accepted receipt is visible until `publish_acceptance`. Capacity and
    /// origin errors precede lower reservation; one unpublished request blocks
    /// further tracked admissions. The returned ticket can issue exactly one
    /// lower write, but conveys neither acceptance nor permission to schedule.
    pub fn prepare_acceptance(
        &mut self,
        service: &mut MessageService,
        mut origin: MessageRequestOrigin,
        envelope: Envelope,
        scope: MessageScope,
        now_ms: u64,
    ) -> Result<MessageAcceptanceTicket> {
        if self.pending.is_some() {
            return Err(capacity());
        }
        if origin.agent_id != envelope.sender.agent_id
            || origin.conversation_id.is_empty()
            || origin.turn_id.is_empty()
        {
            return Err(MessageError::invalid_args(
                "tracked request origin is invalid",
            ));
        }
        if self
            .find(&origin.agent_id, &origin.conversation_id, &envelope.id)
            .is_some()
        {
            return Err(MessageError::conflict(
                "request message ID is already outstanding",
            ));
        }
        let count = self
            .receipts
            .values()
            .filter(|receipt| {
                receipt.origin.agent_id == origin.agent_id
                    && receipt.origin.conversation_id == origin.conversation_id
            })
            .count();
        let next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or_else(|| MessageError::invalid_state("request generation exhausted"))?;
        let origin_bytes = RECEIPT_OVERHEAD
            .saturating_add(origin.agent_id.as_str().len())
            .saturating_add(origin.conversation_id.capacity())
            .saturating_add(origin.turn_id.capacity())
            .saturating_add(envelope.id.len());
        // Includes shared allocation/header and attempt escrow overhead. Lower
        // prepare independently verifies its actual ticket allocation fits.
        let ticket_budget = 512usize.saturating_add(envelope.id.len());
        let units_available = self.limits.units.saturating_sub(self.units);
        let bytes_available = self
            .limits
            .metadata_bytes
            .saturating_sub(self.metadata_bytes);
        let fixed_bytes = origin_bytes.saturating_add(ticket_budget);
        if count >= self.limits.per_conversation
            || units_available < 4
            || fixed_bytes > bytes_available
        {
            return Err(capacity());
        }
        origin.agent_id = origin.agent_id.clone();
        let expires_at_ms = envelope.ttl_ms.map(|ttl| now_ms.saturating_add(ttl));
        let ticket = service.prepare_acceptance(
            &origin.agent_id,
            envelope,
            scope,
            now_ms,
            ticket_budget,
            Some(MessageAcceptanceReservation {
                recipients: (units_available - 2) / 2,
                metadata_bytes: bytes_available - fixed_bytes,
                handoff_bytes_per_recipient: MESSAGE_REQUEST_HANDOFF_BYTES,
            }),
        )?;
        let units = 2 + 2 * ticket.recipient_count();
        let metadata_bytes = origin_bytes
            + ticket.metadata_bytes()
            + ticket.recipient_count() * MESSAGE_REQUEST_HANDOFF_BYTES;
        self.units += units;
        self.metadata_bytes += metadata_bytes;
        self.next_generation = next_generation;
        self.pending = Some(PendingRequest {
            generation: next_generation - 1,
            origin,
            ticket: ticket.clone(),
            expires_at_ms,
            units,
            metadata_bytes,
        });
        Ok(ticket)
    }

    /// Adopts only the matching committed occurrence into its original caller.
    /// Positive duplicate completion borrows the same retained receipt without
    /// sending again. Missing/consumed ownership is an error, never permission
    /// to retry the effect. A foreign fact cannot publish either owner's mail.
    pub fn publish_acceptance(
        &mut self,
        service: &mut MessageService,
        commit: &MessageAcceptanceCommit,
    ) -> Result<&MessageRequestReceipt> {
        if let Some(generation) = self
            .receipts
            .values()
            .find(|receipt| {
                receipt
                    .admission_ticket
                    .as_ref()
                    .is_some_and(|ticket| ticket.matches_commit(commit))
            })
            .map(|receipt| receipt.generation)
        {
            service.publish_acceptance(commit)?;
            // Lookup uses the exact retained generation, with no intervening mutation.
            return self.receipts.get(&generation).ok_or_else(changed);
        }
        let pending = self.pending.as_ref().ok_or_else(changed)?;
        if !pending.ticket.matches_commit(commit) {
            return Err(changed());
        }
        let (delivery, proof) = service.publish_acceptance(commit)?;
        // prepare always requested a proof. A missing proof is an owner bug;
        // retain pending ownership rather than silently losing a committed send.
        let proof = proof.ok_or_else(changed)?;
        let pending = self.pending.take().ok_or_else(changed)?;
        Ok(self
            .receipts
            .entry(pending.generation)
            .or_insert(MessageRequestReceipt {
                generation: pending.generation,
                origin: pending.origin,
                delivery,
                proof,
                admission_ticket: Some(pending.ticket),
                expires_at_ms: pending.expires_at_ms,
                expiry_observed_at_ms: None,
                units: pending.units,
                metadata_bytes: pending.metadata_bytes,
            }))
    }

    /// Cancels a never-issued original and releases both reservations together.
    /// Issued or uncertain work stays charged even when its caller has stopped.
    pub fn cancel_prepared_acceptance(
        &mut self,
        service: &mut MessageService,
        ticket: &MessageAcceptanceTicket,
    ) -> Result<()> {
        let pending = self.pending.as_ref().ok_or_else(changed)?;
        if !pending.ticket.same_reservation(ticket) {
            return Err(changed());
        }
        service.cancel_acceptance(ticket)?;
        self.release_pending();
        Ok(())
    }

    /// Releases both owners only for the exact qualified noncommit fact.
    /// Generic storage errors, missing observers and conversation retirement
    /// cannot enter this path or imply that a send was unaccepted.
    pub fn release_rejected_acceptance(
        &mut self,
        service: &mut MessageService,
        rejected: &MessageAcceptanceRejected,
    ) -> Result<()> {
        let pending = self.pending.as_ref().ok_or_else(changed)?;
        if !pending.ticket.matches_rejected(rejected) {
            return Err(changed());
        }
        service.release_rejected_acceptance(rejected)?;
        self.release_pending();
        Ok(())
    }

    /// Releases already-qualified unpublished accounting without touching mail.
    fn release_pending(&mut self) {
        if let Some(pending) = self.pending.take() {
            self.units -= pending.units;
            self.metadata_bytes -= pending.metadata_bytes;
        }
    }
}

/// Reports lost or foreign ownership without exposing original request data.
fn changed() -> MessageError {
    MessageError::invalid_state("tracked request admission ownership changed")
}

#[cfg(test)]
mod tests;
