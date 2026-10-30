//! Bounded same-runtime ownership of explicitly tracked accepted requests.
//!
//! This is not another mailbox: receipts retain only opaque acceptance evidence,
//! caller identity and pre-reserved handoff headroom. Admission borrows the sole
//! message owner exclusively and reserves conversation, recipient, edge, terminal
//! and metadata capacity before lower acceptance. Failed admission publishes no
//! occurrence. Product adapters remain responsible for proving caller/task
//! ownership, capturing exact child attempts in the same exclusive transaction,
//! committing context before consuming a receipt, and retiring conversations.
//! No snapshot reconstructs this store; generation allocation is not progress.

use std::collections::BTreeMap;

use mez_core::ids::AgentId;

use super::{
    Delivery, Envelope, MessageAcceptanceProof, MessageAcceptanceReservation, MessageError,
    MessageErrorKind, MessageScope, MessageService, Result,
};

/// Pre-reserved task/terminal reference headroom for each frozen recipient.
/// Product adapters must bound their retained data by this charge or reject
/// admission before sending; this allowance grants no task or output authority.
pub const MESSAGE_REQUEST_HANDOFF_BYTES: usize = 6144;
/// Conservative container/edge/receipt overhead, in addition to owned strings.
const RECEIPT_OVERHEAD: usize = 512;

/// Construction-time request bounds, independent of ordinary delivery custody.
#[derive(Debug, Clone, Copy)]
pub struct MessageRequestLimits {
    /// Maximum unresolved tracked requests for one exact sender conversation.
    pub per_conversation: usize,
    /// Global count units: one receipt, one reserved edge, and two per recipient
    /// (frozen binding and terminal handoff), including synchronous admission.
    pub units: usize,
    /// Independently measured receipt/proof bytes plus future handoff headroom.
    pub metadata_bytes: usize,
}

impl Default for MessageRequestLimits {
    /// Returns the hard production ceilings; trusted fixtures may narrow them.
    fn default() -> Self {
        Self {
            per_conversation: 64,
            units: 4096,
            metadata_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Exact native caller identity supplied by the trusted product owner.
/// This data is not model authority: adapters must derive it from their live
/// conversation/task owner, and independently revalidate liveness on wake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRequestOrigin {
    /// Authenticated sending registration address.
    pub agent_id: AgentId,
    /// Originating conversation, never inferred from later pane focus.
    pub conversation_id: String,
    /// Original accepted action's task, not a later persistent-agent task.
    pub turn_id: String,
    /// Task incarnation checked alongside its stable task ID.
    pub started_at_unix_seconds: u64,
}

/// One local accepted request and its outstanding handoff reservation.
/// Borrowing a receipt does not consume it or settle a wait. The product owner
/// must retain it through context commit and scheduler/wake handoff.
#[derive(Debug)]
pub struct MessageRequestReceipt {
    /// Local dependency identity; allocation itself is not a progress witness.
    generation: u64,
    /// Exact original caller provenance from admission.
    origin: MessageRequestOrigin,
    /// Immutable lower acceptance result, including the real message ID.
    delivery: Delivery,
    /// Payload-free occurrence and frozen registration evidence.
    proof: MessageAcceptanceProof,
    /// Full reserved units, retained until explicit owner consumption.
    units: usize,
    /// Full metadata charge, including future task/outcome references.
    metadata_bytes: usize,
}

impl MessageRequestReceipt {
    /// Returns the local monotonic dependency identity, not evidence of progress.
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// Borrows the exact original caller/task identity.
    pub fn origin(&self) -> &MessageRequestOrigin {
        &self.origin
    }
    /// Borrows the actual accepted send result without replaying the effect.
    pub fn delivery(&self) -> &Delivery {
        &self.delivery
    }
    /// Borrows the immutable occurrence proof for authenticated reply matching.
    pub fn proof(&self) -> &MessageAcceptanceProof {
        &self.proof
    }
    /// Returns the already reserved task/outcome bytes for each frozen recipient.
    pub fn handoff_bytes_per_recipient(&self) -> usize {
        MESSAGE_REQUEST_HANDOFF_BYTES
    }
}

/// Finite request receipts owned by one live product runtime.
/// Retire the store on restore; never reconstruct receipts by scanning history.
#[derive(Debug)]
pub struct MessageRequests {
    /// Trusted construction policy, never enlarged by incoming model data.
    limits: MessageRequestLimits,
    /// Outstanding requests keyed by a locally assigned monotonic generation.
    receipts: BTreeMap<u64, MessageRequestReceipt>,
    /// Next identity; exhaustion rejects before message acceptance.
    next_generation: u64,
    /// Includes all future edge/terminal headroom, not just current allocations.
    units: usize,
    /// Includes measured metadata and every reserved future handoff slot.
    metadata_bytes: usize,
}

impl Default for MessageRequests {
    /// Constructs an empty runtime-local owner using production ceilings.
    fn default() -> Self {
        Self {
            limits: MessageRequestLimits::default(),
            receipts: BTreeMap::new(),
            next_generation: 1,
            units: 0,
            metadata_bytes: 0,
        }
    }
}

impl MessageRequests {
    /// Constructs narrowed positive limits; returns InvalidArgs for enlargement
    /// or zero before adopting state. These are not wire/config options.
    pub fn with_limits(limits: MessageRequestLimits) -> Result<Self> {
        let ceiling = MessageRequestLimits::default();
        if [
            (limits.per_conversation, ceiling.per_conversation),
            (limits.units, ceiling.units),
            (limits.metadata_bytes, ceiling.metadata_bytes),
        ]
        .iter()
        .any(|(value, maximum)| *value == 0 || value > maximum)
        {
            return Err(MessageError::invalid_args(
                "request limits must be positive and cannot exceed ceilings",
            ));
        }
        Ok(Self {
            limits,
            ..Self::default()
        })
    }

    /// Accepts one explicitly tracked request only after all retention headroom
    /// fits. Ordinary sends use MessageService directly and are not blocked by
    /// this store. Existing unresolved IDs conflict even after lower replay
    /// eviction; retries borrow the original receipt instead of sending again.
    /// The product's native execution owner must allocate message IDs uniquely
    /// for the conversation lifetime, including after receipt consumption. This
    /// finite store cannot reconstruct uniqueness from discarded history.
    /// Failure leaves both owners unchanged. No provider scheduling occurs.
    pub fn accept(
        &mut self,
        service: &mut MessageService,
        mut origin: MessageRequestOrigin,
        envelope: Envelope,
        scope: MessageScope,
        now_ms: u64,
    ) -> Result<&MessageRequestReceipt> {
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
        let following_generation = self
            .next_generation
            .checked_add(1)
            .ok_or_else(|| MessageError::invalid_state("request generation exhausted"))?;
        let origin_bytes = RECEIPT_OVERHEAD
            .saturating_add(origin.agent_id.as_str().len())
            .saturating_add(origin.conversation_id.capacity())
            .saturating_add(origin.turn_id.capacity())
            .saturating_add(envelope.id.len()); // Delivery owns a second ID.
        let available_units = self.limits.units.saturating_sub(self.units);
        let available_bytes = self
            .limits
            .metadata_bytes
            .saturating_sub(self.metadata_bytes);
        if count >= self.limits.per_conversation
            || available_units < 4
            || origin_bytes > available_bytes
        {
            return Err(capacity());
        }
        // AgentId hides its owned string capacity. Clone only after the length
        // fits, so the retained ID has the measured compact allocation.
        origin.agent_id = origin.agent_id.clone();
        let (delivery, proof) = service.accept_at_with_scope_and_reservation(
            &origin.agent_id,
            envelope,
            scope,
            now_ms,
            MessageAcceptanceReservation {
                recipients: (available_units - 2) / 2,
                metadata_bytes: available_bytes - origin_bytes,
                handoff_bytes_per_recipient: MESSAGE_REQUEST_HANDOFF_BYTES,
            },
        )?;
        let recipients = proof.recipients().count();
        let units = 2 + 2 * recipients;
        let metadata_bytes =
            origin_bytes + proof.metadata_bytes() + recipients * MESSAGE_REQUEST_HANDOFF_BYTES;
        let generation = self.next_generation;
        self.next_generation = following_generation;
        self.units += units;
        self.metadata_bytes += metadata_bytes;
        Ok(self
            .receipts
            .entry(generation)
            .or_insert(MessageRequestReceipt {
                generation,
                origin,
                delivery,
                proof,
                units,
                metadata_bytes,
            }))
    }

    /// Finds only a live outstanding accepted request in this exact conversation.
    /// The scan is bounded by the global count ceiling and never reads history.
    pub fn find(
        &self,
        sender: &AgentId,
        conversation: &str,
        message_id: &str,
    ) -> Option<&MessageRequestReceipt> {
        self.receipts.values().find(|receipt| {
            receipt.origin.agent_id == *sender
                && receipt.origin.conversation_id == conversation
                && receipt.delivery.message_id == message_id
        })
    }

    /// Explicitly consumes an exact original receipt after the product's context
    /// and wake handoff commit. A foreign incarnation cannot release capacity.
    /// Calling this before handoff is a product ownership bug, not inferred ACK.
    pub fn consume(&mut self, generation: u64, origin: &MessageRequestOrigin) -> bool {
        if self
            .receipts
            .get(&generation)
            .is_none_or(|receipt| &receipt.origin != origin)
        {
            return false;
        }
        if let Some(receipt) = self.receipts.remove(&generation) {
            self.units -= receipt.units;
            self.metadata_bytes -= receipt.metadata_bytes;
        }
        true
    }

    /// Retires only one originating conversation's retained ownership. Product
    /// teardown must call this when that conversation ends; task completion alone
    /// does not retire a persistent conversation's unresolved requests.
    pub fn retire_conversation(&mut self, sender: &AgentId, conversation: &str) {
        self.receipts.retain(|_, receipt| {
            let keep = receipt.origin.agent_id != *sender
                || receipt.origin.conversation_id != conversation;
            if !keep {
                self.units -= receipt.units;
                self.metadata_bytes -= receipt.metadata_bytes;
            }
            keep
        });
    }

    /// Returns unresolved request count and fully reserved global units/bytes.
    pub fn usage(&self) -> (usize, usize, usize) {
        (self.receipts.len(), self.units, self.metadata_bytes)
    }

    /// Returns immutable trusted limits for bounded adapter preflight. This
    /// cannot enlarge policy or replace the authoritative acceptance checks.
    pub fn limits(&self) -> MessageRequestLimits {
        self.limits
    }

    /// Borrows outstanding receipts without scanning delivery or turn history.
    pub fn receipts(&self) -> impl Iterator<Item = &MessageRequestReceipt> {
        self.receipts.values()
    }

    /// Borrows one exact dependency generation; missing runtime proof stays absent.
    pub fn receipt(&self, generation: u64) -> Option<&MessageRequestReceipt> {
        self.receipts.get(&generation)
    }
}

/// Bounded model-correctable admission failure, without echoing request data.
fn capacity() -> MessageError {
    MessageError::new(
        MessageErrorKind::Capacity,
        "tracked request capacity exceeded",
    )
}

#[cfg(test)]
mod tests;
