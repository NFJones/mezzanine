//! Bounded read-only settlement/dependency inspection of one retired original.
//!
//! The actual lower input retains source-absence evidence and charged scratch.
//! One owned SQLite snapshot validates the whole settled store while retaining
//! only this original's frozen slot accumulator. Shared cumulative ACKs and
//! native/canonical journals remain explicit dependency blockers, never deletion
//! permission. This is not a closed cleanup plan, intent or reclamation worker.

use mez_agent::messaging::{
    DeliveryTerminalReason, MessageSourceRetirementInspection, MessageStorageScope,
};

use super::inventory::{CustodyInventoryEntry, CustodyJournalKind as Journal, CustodyScanPosition};
use super::ownership::OwnedCustodyStore;
use super::*;

/// Fits the separately charged 256 KiB accumulator, including vector overhead.
const MAX_SLOTS: usize = 4096;
const SLOT_METADATA_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Default)]
struct Slot {
    acknowledged: bool,
    terminal: Option<(DeliveryTerminalReason, u64)>,
    released: Option<(DeliveryTerminalReason, u64)>,
}

/// Descriptive facts only. Even a fully settled isolated source still requires
/// a closed dependency plan, preserved native evidence and reserved crash-safe
/// cleanup before physical deletion. A result cannot be used at a new position.
pub(crate) struct SourceRetirementInspection {
    pub(super) position: CustodyScanPosition,
    pub sequence: u64,
    pub source_sha256: [u8; 32],
    pub recipient_count: usize,
    pub unsettled_recipients: usize,
    pub shared_acknowledgements: usize,
    pub native_journal_references: usize,
}

impl OwnedCustodyStore<'_> {
    /// Reads original facts under the live directory/database fence. The root
    /// must also validate the input against its exact current MessageService at
    /// admission/adoption. No row, watermark, clock or receipt is changed here.
    pub(crate) fn inspect_source_retirement(
        &mut self,
        input: &MessageSourceRetirementInspection,
        scope: &MessageStorageScope,
        sequence: u64,
    ) -> Result<SourceRetirementInspection> {
        if !input.belongs_to(scope) || !input.has_no_live_source(sequence) {
            return Err(unavailable());
        }
        self.revalidate()?;
        let result = inspect(self.store_mut(), input, sequence)?;
        self.revalidate()?;
        Ok(result)
    }
}

fn inspect(
    store: &MmpCustodyStore,
    input: &MessageSourceRetirementInspection,
    sequence: u64,
) -> Result<SourceRetirementInspection> {
    let transaction = store
        .connection
        .unchecked_transaction()
        .map_err(database_error)?;
    let limits = input.decode_limits();
    let (source, source_sha256, _) =
        store.read_accepted_metadata_in_snapshot(&transaction, sequence, limits)?;
    let recipient_count = source.recipient_bindings().len();
    drop(source);
    if recipient_count == 0
        || recipient_count > MAX_SLOTS
        || recipient_count
            .saturating_mul(std::mem::size_of::<Slot>())
            .saturating_add(4096)
            > SLOT_METADATA_BYTES
    {
        return Err(unavailable());
    }
    let mut slots = Vec::new();
    slots
        .try_reserve_exact(recipient_count)
        .map_err(|_| unavailable())?;
    slots.resize(recipient_count, Slot::default());
    let mut shared_acknowledgements = 0usize;
    let mut native_journal_references = 0usize;
    let mut original_seen = false;
    let mut visit = |entry: CustodyInventoryEntry<'_>| {
        match entry {
            CustodyInventoryEntry::Occurrence(original) if original.sequence == sequence => {
                if original_seen
                    || original.digest != source_sha256
                    || original.recipient_count != recipient_count
                {
                    return Err(unavailable());
                }
                original_seen = true;
            }
            CustodyInventoryEntry::Occurrence(_) | CustodyInventoryEntry::Aborted(_) => (),
            CustodyInventoryEntry::Journal(kind, key) => match kind {
                Journal::Acknowledgement => {
                    let ack =
                        store.read_stored_acknowledgement_in_snapshot(&transaction, key, limits)?;
                    if ack.decisions.iter().any(|d| d.sequence == sequence) {
                        shared_acknowledgements +=
                            usize::from(ack.decisions.iter().any(|d| d.sequence != sequence));
                        for decision in ack.decisions.iter().filter(|d| d.sequence == sequence) {
                            if decision.acknowledged {
                                slot(&mut slots, decision.binding_index)?.acknowledged = true;
                            }
                            // An expiry exclusion alone does not release the
                            // sender's independently retained terminal outcome.
                        }
                    }
                }
                Journal::Expiry => {
                    let frame = store.read_stored_expiry_in_snapshot(&transaction, key, limits)?;
                    if frame.sequence == sequence {
                        for index in frame.binding_indices {
                            terminal(
                                slot(&mut slots, index)?,
                                DeliveryTerminalReason::Expired,
                                frame.observed_at_ms,
                            )?;
                        }
                    }
                }
                Journal::Retirement => {
                    let frame =
                        store.read_stored_retirement_in_snapshot(&transaction, key, limits)?;
                    if frame.sequence == sequence {
                        terminal(
                            slot(&mut slots, frame.binding_index)?,
                            DeliveryTerminalReason::RecipientRetired,
                            frame.observed_at_ms,
                        )?;
                    }
                }
                Journal::Release => {
                    let frame = store.read_stored_release_in_snapshot(&transaction, key, limits)?;
                    if frame.sequence == sequence {
                        for (index, first, reason) in frame.entries {
                            release(slot(&mut slots, index)?, reason, first)?;
                        }
                    }
                }
                Journal::Consumption => {
                    let frame =
                        store.read_stored_consumption_in_snapshot(&transaction, key, limits)?;
                    if frame.sequence == sequence {
                        release(
                            slot(&mut slots, frame.binding_index)?,
                            frame.terminal.reason,
                            frame.terminal.observed_at_ms,
                        )?;
                    }
                }
                Journal::NativeInput => {
                    let journal = store
                        .read_native_input_in_snapshot(&transaction, key, limits)?
                        .ok_or_else(unavailable)?;
                    native_journal_references +=
                        usize::from(journal.original_sequence() == sequence);
                }
                Journal::TerminalHandoff => {
                    let journal = store
                        .read_handoff_in_snapshot(&transaction, key, limits)?
                        .ok_or_else(unavailable)?;
                    native_journal_references +=
                        usize::from(journal.original_sequence() == sequence);
                }
                // Registration provenance is deliberately preserved. This
                // observation never proposes deletion of b1/n1/o1 records.
                Journal::NativeContinuity | Journal::NativeLifetime | Journal::RegistrationEnd => {}
            },
        }
        Ok(())
    };
    // The lower input already owns full source/scratch/slot/framing leases.
    // Give the scanner no allowance for a second retained reference index.
    let position =
        store.visit_inventory_in_snapshot(&transaction, limits, 8 * 1024 * 1024, &mut visit)?;
    if !original_seen
        || !position.watermark_origin_trusted
        || sequence > position.sequence_watermark
    {
        return Err(unavailable());
    }
    for slot in &slots {
        if (slot.acknowledged && slot.terminal.is_some())
            || (slot.released.is_some() && slot.released != slot.terminal)
        {
            return Err(unavailable());
        }
    }
    let unsettled_recipients = slots
        .iter()
        .filter(|s| !s.acknowledged && s.released.is_none())
        .count();
    store.finish_transaction(transaction)?;
    Ok(SourceRetirementInspection {
        position,
        sequence,
        source_sha256,
        recipient_count,
        unsettled_recipients,
        shared_acknowledgements,
        native_journal_references,
    })
}

fn slot(slots: &mut [Slot], index: u32) -> Result<&mut Slot> {
    slots.get_mut(index as usize).ok_or_else(unavailable)
}

fn terminal(slot: &mut Slot, reason: DeliveryTerminalReason, first: u64) -> Result<()> {
    let observation = (reason, first);
    if slot.terminal.is_some_and(|prior| prior != observation) {
        return Err(unavailable());
    }
    slot.terminal = Some(observation);
    Ok(())
}

fn release(slot: &mut Slot, reason: DeliveryTerminalReason, first: u64) -> Result<()> {
    if slot.released.replace((reason, first)).is_some() {
        return Err(unavailable());
    }
    Ok(())
}

fn unavailable() -> MezError {
    MezError::invalid_state("MMP original source retirement inspection unavailable")
}

#[cfg(test)]
mod tests;
