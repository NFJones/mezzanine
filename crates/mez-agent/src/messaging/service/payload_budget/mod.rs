//! Bounded ownership accounting for canonical payload allocations and reads.
//!
//! Weak envelope references survive removal of a canonical row while a delivery
//! or worker still pins its allocation. Unbound read reservations remain charged
//! through actual worker loss; only qualified joined recovery may retire them.
//! Binding replaces a reservation with the exact allocation, so retained tickets
//! cannot charge dead payloads forever or release externally pinned live bytes.
//! This ledger owns no mailbox, payload, cursor, recipient or storage authority.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use super::*;

/// Logical cache occupancy, including unpublished worker allocations/reservations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MessagePayloadUsage {
    /// Live canonical allocations, regardless of cache, worker or delivery owner.
    pub live_allocations: usize,
    /// Original UTF-8 payload length of those unique live allocations.
    pub live_payload_bytes: usize,
    /// Issued allocation reservations not yet bound to an envelope.
    pub reserved_allocations: usize,
    /// Original payload lengths reserved before worker decoding/allocation.
    pub reserved_payload_bytes: usize,
}

/// Read-only cache accounting across actor/worker ownership boundaries.
/// Retaining it owns no payload, cache pin, source or publication capability.
#[derive(Clone, Debug)]
pub struct MessagePayloadObserver(Arc<PayloadBudget>);

impl MessagePayloadObserver {
    /// Measures all live/reserved allocations without accessing message content.
    pub fn snapshot(&self) -> Result<MessagePayloadUsage> {
        self.0.usage()
    }
}

/// One live allocation or unbound reservation; neither weak reference owns bytes.
struct Slot {
    source: MessageSequence,
    metadata: usize,
    bytes: usize,
    allocation: Weak<Envelope>,
    reservation: Weak<()>,
}

/// Monotonic slot namespace; retired handles can never alias a later allocation.
struct State {
    next: u64,
    slots: BTreeMap<u64, Slot>,
}

/// Shared logical owner independent of any one canonical row's lifetime.
pub(super) struct PayloadBudget {
    messages: usize,
    bytes: usize,
    state: Mutex<State>,
}

impl std::fmt::Debug for PayloadBudget {
    /// Reports policy without locking the ledger or exposing live source data.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PayloadBudget")
            .field("messages", &self.messages)
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

/// Payload-free reservation, retained by actual worker escrow across lost input.
#[derive(Clone)]
pub(super) struct PayloadReservation {
    budget: Arc<PayloadBudget>,
    slot: u64,
    _token: Arc<()>,
}

impl PayloadBudget {
    /// Creates an observational handle with no allocation/publication authority.
    pub(super) fn observer(self: &Arc<Self>) -> MessagePayloadObserver {
        MessagePayloadObserver(self.clone())
    }
    /// Constructs a trusted, already-validated destination cache budget.
    pub(super) fn new(messages: usize, bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            messages,
            bytes,
            state: Mutex::new(State {
                next: 1,
                slots: BTreeMap::new(),
            }),
        })
    }

    /// Measures live/reserved resources and prunes only truly dead weak entries.
    pub(super) fn usage(&self) -> Result<MessagePayloadUsage> {
        let mut state = self.state.lock().map_err(|_| unavailable())?;
        Ok(measure(&mut state))
    }

    /// Counts headers retained by worker/delivery allocations after their source
    /// leaves the canonical owner. The callback only checks a local index.
    pub(super) fn orphan_metadata(
        &self,
        retained: impl Fn(MessageSequence) -> bool,
    ) -> Result<usize> {
        let mut state = self.state.lock().map_err(|_| unavailable())?;
        measure(&mut state);
        Ok(state
            .slots
            .values()
            .filter(|slot| !retained(slot.source))
            .fold(0usize, |sum, slot| sum.saturating_add(slot.metadata)))
    }

    /// Reserves before handing input to a worker or constructing a source owner.
    pub(super) fn reserve(
        self: &Arc<Self>,
        source: MessageSequence,
        bytes: usize,
        metadata: usize,
    ) -> Result<PayloadReservation> {
        let mut state = self.state.lock().map_err(|_| unavailable())?;
        let usage = measure(&mut state);
        if usage
            .live_allocations
            .saturating_add(usage.reserved_allocations)
            >= self.messages
            || usage
                .live_payload_bytes
                .saturating_add(usage.reserved_payload_bytes)
                .saturating_add(bytes)
                > self.bytes
        {
            return Err(capacity());
        }
        let slot = state.next;
        let next = slot.checked_add(1).ok_or_else(unavailable)?;
        let token = Arc::new(());
        state.slots.insert(
            slot,
            Slot {
                source,
                metadata,
                bytes,
                allocation: Weak::new(),
                reservation: Arc::downgrade(&token),
            },
        );
        state.next = next;
        Ok(PayloadReservation {
            budget: self.clone(),
            slot,
            _token: token,
        })
    }
}

impl PayloadReservation {
    /// Converts reserved bytes to a weak reference to this exact allocation.
    /// A second, different allocation cannot replace still-live original bytes.
    pub(super) fn bind(&self, envelope: &Arc<Envelope>) -> Result<()> {
        let mut state = self.budget.state.lock().map_err(|_| unavailable())?;
        let slot = state.slots.get_mut(&self.slot).ok_or_else(unavailable)?;
        if envelope.payload.len() != slot.bytes {
            return Err(unavailable());
        }
        if let Some(original) = slot.allocation.upgrade() {
            return if Arc::ptr_eq(&original, envelope) {
                Ok(())
            } else {
                Err(unavailable())
            };
        }
        if slot.reservation.strong_count() == 0 {
            return Err(unavailable());
        }
        slot.allocation = Arc::downgrade(envelope);
        slot.reservation = Weak::new();
        Ok(())
    }

    /// Retires an old reservation only after its original actual worker joined.
    /// A retained positive allocation cannot be released by metadata/join alone.
    pub(super) fn retire_joined(&self) -> Result<()> {
        let mut state = self.budget.state.lock().map_err(|_| unavailable())?;
        if state
            .slots
            .get(&self.slot)
            .is_some_and(|slot| slot.allocation.strong_count() > 0)
        {
            return Err(MessageError::conflict(
                "MMP payload allocation is still pinned",
            ));
        }
        state.slots.remove(&self.slot);
        Ok(())
    }
}

/// Counts each exact allocation once and drops metadata only after all owners die.
fn measure(state: &mut State) -> MessagePayloadUsage {
    let mut usage = MessagePayloadUsage::default();
    state.slots.retain(|_, slot| {
        if slot.allocation.strong_count() > 0 {
            usage.live_allocations = usage.live_allocations.saturating_add(1);
            usage.live_payload_bytes = usage.live_payload_bytes.saturating_add(slot.bytes);
            true
        } else if slot.reservation.strong_count() > 0 {
            usage.reserved_allocations = usage.reserved_allocations.saturating_add(1);
            usage.reserved_payload_bytes = usage.reserved_payload_bytes.saturating_add(slot.bytes);
            true
        } else {
            false
        }
    });
    usage
}

/// Refusal leaves all live allocations/reservations owned by their originals.
fn capacity() -> MessageError {
    MessageError::new(
        MessageErrorKind::Capacity,
        "MMP resident payload capacity; message not accepted",
    )
}

/// Poison/slot mismatch cannot be interpreted as newly available cache capacity.
fn unavailable() -> MessageError {
    MessageError::invalid_state("MMP payload allocation ownership unavailable")
}

#[cfg(test)]
mod tests;
