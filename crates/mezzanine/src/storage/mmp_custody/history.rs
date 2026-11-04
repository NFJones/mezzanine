//! Bounded, quarantined fold of complete stored ACK and expiry evidence.
//!
//! This is a storage observation, not a second mailbox or a restored live
//! owner. A single snapshot validates the complete inventory and each ACK's
//! original sources before combining recipient-specific positive decisions and
//! occurrence-wide ACK expiry exclusions and exact unreleased terminal slots.
//! Terminal batches preserve their first original observation; conflicting slot
//! or first-clock history refuses the whole candidate. No cursor, native input, registration,
//! sender receipt or allocator authority can be published from this fold alone.
//! A separate persistent watermark and trusted restoration boundary are still
//! required before compaction or delivery may discard or consume evidence.

use std::collections::HashMap;

use mez_agent::messaging::MessageAcceptanceDecodeLimits;

use super::{
    inventory::{CustodyInventory, charge},
    *,
};

/// Conservative charge for a distinct folded key, including hash table and
/// the sorted output retained during conversion. Shares inventory's 8 MiB cap.
const FOLDED_ENTRY_CHARGE: usize = 256;

/// One source-bound positive ACK, not a current registration or native commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FoldedAcknowledgement {
    /// Original occurrence sequence from verified stored evidence.
    pub sequence: u64,
    /// Exact original frozen recipient slot, not a newly resolved audience.
    pub binding_index: u32,
    /// Maximum recorded actual selection clock for this exact original slot.
    pub offered_at_ms: u64,
}

/// Occurrence-wide maximum clock of original observed expiry exclusions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FoldedExpiry {
    /// Original occurrence, applying to every captured recipient slot.
    pub sequence: u64,
    /// Maximum originally observed endpoint clock, never a renewed TTL.
    pub observed_at_ms: u64,
}

/// One original per-recipient first expiry observation, never an ACK or release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FoldedTerminalExpiry {
    /// Original accepted occurrence, qualified against its complete source.
    pub sequence: u64,
    /// Exact frozen recipient slot, never newly resolved membership.
    pub binding_index: u32,
    /// First actual strict TTL observation, shared by later bounded batches.
    pub observed_at_ms: u64,
    /// Actual qualified release observation, absent until its journal exists.
    pub release_observed_at_ms: Option<u64>,
    /// Actual canonical-proof consumption, with no horizon clock authority.
    pub sender_consumed: bool,
}

impl FoldedTerminalExpiry {
    /// A positive release requires either actual r1 or actual canonical s1.
    pub(crate) fn released(&self) -> bool {
        self.release_observed_at_ms.is_some() || self.sender_consumed
    }
}

/// Complete descriptive store inventory and order-independent ACK fold.
pub(crate) struct CustodyHistory {
    /// Original checked allocation and compact source/ACK/abort references.
    pub inventory: CustodyInventory,
    /// Sorted unique positive decisions, still without application authority.
    pub acknowledgements: Vec<FoldedAcknowledgement>,
    /// Sorted occurrence-wide maximum exclusion clocks.
    pub expiries: Vec<FoldedExpiry>,
    /// Sorted exact terminal slots retaining their original first observations.
    pub terminal_expiries: Vec<FoldedTerminalExpiry>,
    /// One above the highest *surviving* occurrence or abort, if representable.
    /// This is not a durable watermark and must never itself seed an allocator.
    pub observed_next_sequence: Option<u64>,
    /// Total conservative retained charge, including the caller's baseline and
    /// the simultaneous hash/sorted fold allocations. Later recovery phases
    /// must preserve this charge while retaining this evidence.
    pub(super) retained_metadata: usize,
}

impl MmpCustodyStore {
    /// Validates the whole settled store and folds all ACKs in one snapshot.
    /// Unresolved stages, corrupted rows, invalid ACKs or metadata exhaustion
    /// withhold the complete result. No returned value conveys live authority.
    pub(crate) fn fold_custody_history(
        &self,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<CustodyHistory> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let history = self.fold_custody_history_in_snapshot(&transaction, limits, 0)?;
        self.finish_transaction(transaction)?;
        Ok(history)
    }

    /// Folds evidence inside the caller's original-store read transaction.
    /// The supplied baseline includes retained input and bounded decoder scratch;
    /// no result may escape until the caller commits all cross-record checks.
    pub(super) fn fold_custody_history_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        limits: MessageAcceptanceDecodeLimits,
        metadata_base: usize,
    ) -> Result<CustodyHistory> {
        let inventory =
            self.inventory_in_snapshot_with_metadata(transaction, limits, metadata_base)?;
        let highest = inventory
            .occurrences
            .iter()
            .map(|source| source.sequence)
            .chain(inventory.aborted_sequences.iter().copied())
            .max()
            .unwrap_or(0);
        let mut metadata = inventory.retained_metadata;
        let mut acknowledgements: HashMap<(u64, u32), u64> = HashMap::new();
        let mut expiries: HashMap<u64, u64> = HashMap::new();
        for key in &inventory.acknowledgement_keys {
            let ack = self.read_stored_acknowledgement_in_snapshot(transaction, key, limits)?;
            for decision in ack.decisions {
                if decision.acknowledged {
                    let entry = (decision.sequence, decision.binding_index);
                    let offered_at_ms = decision.offered_at_ms.ok_or_else(|| {
                        MezError::invalid_state("MMP custody ACK offer evidence is unavailable")
                    })?;
                    if let Some(prior) = acknowledgements.get_mut(&entry) {
                        *prior = (*prior).max(offered_at_ms);
                    } else {
                        charge(&mut metadata, FOLDED_ENTRY_CHARGE)?;
                        acknowledgements.try_reserve(1).map_err(|_| capacity())?;
                        acknowledgements.insert(entry, offered_at_ms);
                    }
                } else if let Some(prior) = expiries.get_mut(&decision.sequence) {
                    *prior = (*prior).max(ack.observed_at_ms);
                } else {
                    charge(&mut metadata, FOLDED_ENTRY_CHARGE)?;
                    expiries.try_reserve(1).map_err(|_| capacity())?;
                    expiries.insert(decision.sequence, ack.observed_at_ms);
                }
            }
        }
        let mut terminals = HashMap::new();
        let mut terminal_source_clocks = HashMap::new();
        for key in &inventory.expiry_keys {
            let frame = self.read_stored_expiry_in_snapshot(transaction, key, limits)?;
            if let Some(prior) = terminal_source_clocks.get(&frame.sequence) {
                if *prior != frame.observed_at_ms {
                    return Err(MezError::invalid_state(
                        "MMP terminal expiry has conflicting first observations",
                    ));
                }
            } else {
                charge(&mut metadata, FOLDED_ENTRY_CHARGE)?;
                terminal_source_clocks
                    .try_reserve(1)
                    .map_err(|_| capacity())?;
                terminal_source_clocks.insert(frame.sequence, frame.observed_at_ms);
            }
            for index in frame.binding_indices {
                let slot = (frame.sequence, index);
                if terminals.contains_key(&slot) || acknowledgements.contains_key(&slot) {
                    return Err(MezError::invalid_state(
                        "MMP terminal expiry conflicts with original slot history",
                    ));
                }
                charge(&mut metadata, FOLDED_ENTRY_CHARGE)?;
                terminals.try_reserve(1).map_err(|_| capacity())?;
                terminals.insert(slot, frame.observed_at_ms);
            }
            if let Some(prior) = expiries.get_mut(&frame.sequence) {
                *prior = (*prior).max(frame.observed_at_ms);
            } else {
                charge(&mut metadata, FOLDED_ENTRY_CHARGE)?;
                expiries.try_reserve(1).map_err(|_| capacity())?;
                expiries.insert(frame.sequence, frame.observed_at_ms);
            }
        }
        drop(terminal_source_clocks);
        let mut terminal_expiries = Vec::new();
        terminal_expiries
            .try_reserve_exact(terminals.len())
            .map_err(|_| capacity())?;
        terminal_expiries.extend(terminals.into_iter().map(
            |((sequence, binding_index), observed_at_ms)| FoldedTerminalExpiry {
                sequence,
                binding_index,
                observed_at_ms,
                release_observed_at_ms: None,
                sender_consumed: false,
            },
        ));
        terminal_expiries.sort_unstable();
        for key in &inventory.release_keys {
            let release = self.read_stored_release_in_snapshot(transaction, key, limits)?;
            for (index, first) in release.entries {
                let slot = (release.sequence, index);
                let position = terminal_expiries
                    .binary_search_by_key(&slot, |value| (value.sequence, value.binding_index))
                    .map_err(|_| {
                        MezError::invalid_state("MMP release lacks original terminal slot")
                    })?;
                let terminal = &mut terminal_expiries[position];
                if terminal.release_observed_at_ms.is_some() || terminal.observed_at_ms != first {
                    return Err(MezError::invalid_state(
                        "MMP terminal release conflicts with original history",
                    ));
                }
                terminal.release_observed_at_ms = Some(release.observed_at_ms);
            }
        }
        for key in &inventory.consumption_keys {
            let consumed = self.read_stored_consumption_in_snapshot(transaction, key, limits)?;
            let slot = (consumed.sequence, consumed.binding_index);
            let position = terminal_expiries
                .binary_search_by_key(&slot, |value| (value.sequence, value.binding_index))
                .map_err(|_| {
                    MezError::invalid_state("MMP consumption lacks original terminal slot")
                })?;
            let terminal = &mut terminal_expiries[position];
            if terminal.released() || terminal.observed_at_ms != consumed.terminal.observed_at_ms {
                return Err(MezError::invalid_state(
                    "MMP consumption conflicts with original history",
                ));
            }
            terminal.sender_consumed = true;
        }
        let mut folded_acknowledgements = Vec::new();
        folded_acknowledgements
            .try_reserve_exact(acknowledgements.len())
            .map_err(|_| capacity())?;
        folded_acknowledgements.extend(acknowledgements.into_iter().map(
            |((sequence, binding_index), offered_at_ms)| FoldedAcknowledgement {
                sequence,
                binding_index,
                offered_at_ms,
            },
        ));
        folded_acknowledgements.sort_unstable();
        let mut folded_expiries = Vec::new();
        folded_expiries
            .try_reserve_exact(expiries.len())
            .map_err(|_| capacity())?;
        folded_expiries.extend(expiries.into_iter().map(|(sequence, observed_at_ms)| {
            FoldedExpiry {
                sequence,
                observed_at_ms,
            }
        }));
        folded_expiries.sort_unstable();
        Ok(CustodyHistory {
            inventory,
            acknowledgements: folded_acknowledgements,
            expiries: folded_expiries,
            terminal_expiries,
            observed_next_sequence: highest.checked_add(1),
            retained_metadata: metadata,
        })
    }
}

/// Reports bounded allocation failure without including private message data.
fn capacity() -> MezError {
    MezError::invalid_state("MMP custody history allocation exceeds its budget")
}
