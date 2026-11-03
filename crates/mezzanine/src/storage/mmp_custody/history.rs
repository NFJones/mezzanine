//! Bounded, quarantined fold of complete stored acknowledgement evidence.
//!
//! This is a storage observation, not a second mailbox or a restored live
//! owner. A single snapshot validates the complete inventory and each ACK's
//! original sources before combining recipient-specific positive decisions and
//! occurrence-wide expiry observations. No cursor, native input, registration,
//! sender receipt or allocator authority can be published from this fold alone.
//! A separate persistent watermark and trusted restoration boundary are still
//! required before compaction or delivery may discard or consume evidence.

use std::collections::{HashMap, HashSet};

use mez_agent::messaging::MessageAcceptanceDecodeLimits;

use super::{
    inventory::{CustodyInventory, charge},
    *,
};

/// Conservative charge for a distinct folded key, including hash table and
/// the sorted output retained during conversion. Shares inventory's 8 MiB cap.
const FOLDED_ENTRY_CHARGE: usize = 256;

/// One source-bound positive ACK, not a current registration or native commit.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FoldedAcknowledgement {
    /// Original occurrence sequence from verified stored evidence.
    pub sequence: u64,
    /// Exact original frozen recipient slot, not a newly resolved audience.
    pub binding_index: u32,
}

/// Occurrence-wide maximum clock of original observed expiry exclusions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct FoldedExpiry {
    /// Original occurrence, applying to every captured recipient slot.
    pub sequence: u64,
    /// Maximum originally observed endpoint clock, never a renewed TTL.
    pub observed_at_ms: u64,
}

/// Complete descriptive store inventory and order-independent ACK fold.
pub(crate) struct CustodyHistory {
    /// Original checked allocation and compact source/ACK/abort references.
    pub inventory: CustodyInventory,
    /// Sorted unique positive decisions, still without application authority.
    pub acknowledgements: Vec<FoldedAcknowledgement>,
    /// Sorted occurrence-wide maximum exclusion clocks.
    pub expiries: Vec<FoldedExpiry>,
    /// One above the highest *surviving* occurrence or abort, if representable.
    /// This is not a durable watermark and must never itself seed an allocator.
    pub observed_next_sequence: Option<u64>,
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
        let inventory = self.inventory_in_snapshot(&transaction, limits)?;
        let highest = inventory
            .occurrences
            .iter()
            .map(|source| source.sequence)
            .chain(inventory.aborted_sequences.iter().copied())
            .max()
            .unwrap_or(0);
        let mut metadata = inventory.retained_metadata;
        let mut acknowledgements = HashSet::new();
        let mut expiries: HashMap<u64, u64> = HashMap::new();
        for key in &inventory.acknowledgement_keys {
            let ack = self.read_stored_acknowledgement_in_snapshot(&transaction, key, limits)?;
            for decision in ack.decisions {
                if decision.acknowledged {
                    let entry = FoldedAcknowledgement {
                        sequence: decision.sequence,
                        binding_index: decision.binding_index,
                    };
                    if !acknowledgements.contains(&entry) {
                        charge(&mut metadata, FOLDED_ENTRY_CHARGE)?;
                        acknowledgements.try_reserve(1).map_err(|_| capacity())?;
                        acknowledgements.insert(entry);
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
        let mut folded_acknowledgements = Vec::new();
        folded_acknowledgements
            .try_reserve_exact(acknowledgements.len())
            .map_err(|_| capacity())?;
        folded_acknowledgements.extend(acknowledgements);
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
        transaction.commit().map_err(database_error)?;
        Ok(CustodyHistory {
            inventory,
            acknowledgements: folded_acknowledgements,
            expiries: folded_expiries,
            observed_next_sequence: highest.checked_add(1),
        })
    }
}

/// Reports bounded allocation failure without including private message data.
fn capacity() -> MezError {
    MezError::invalid_state("MMP custody history allocation exceeds its budget")
}
