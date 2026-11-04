//! Source-bound terminal journals in the existing private custody store.
//!
//! One actual lower reservation supplies immutable source/slots/first-clock
//! data or exact horizon before-images. The concrete port binds the operation
//! to the original allocation and scope under
//! a retained kernel owner, checks source TTL and conflicting ACK/native facts
//! in one immediate transaction, and preserves every non-target row around SQL
//! effects. Bytes are descriptive; only the trusted callback can seal a lower
//! positive. Inventory/fold/reconstruction and the original root/actor producer
//! remain responsible for publishing recovered terminal state, never a decoder.

use super::{integrity::unaffected_rows_digest, ownership::OwnedCustodyStore, *};
use mez_agent::messaging::{
    MessageError, MessageStorageScope, MessageTerminalKind, MessageTerminalStore,
    MessageTerminalWrite,
};

mod read;
mod release;

/// Actual original allocation and pre-operation generation; not a capability.
#[derive(Clone, Debug)]
pub(crate) struct TerminalPlan {
    store_id: Vec<u8>,
    generation: u64,
}

impl TerminalPlan {
    /// Converts only a trusted idle root's already observed physical position.
    pub(super) fn from_position(store_id: Vec<u8>, generation: u64) -> Self {
        Self {
            store_id,
            generation,
        }
    }
}

/// This borrow cannot outlive the actual original kernel/store owner.
pub(crate) struct PlannedTerminalStore<'borrow, 'owner> {
    owned: &'borrow mut OwnedCustodyStore<'owner>,
    plan: &'borrow TerminalPlan,
    scope: &'borrow MessageStorageScope,
}

impl<'owner> OwnedCustodyStore<'owner> {
    /// Captures the original physical position off actor under this live fence.
    #[cfg(test)]
    pub(crate) fn terminal_plan(&mut self) -> Result<TerminalPlan> {
        self.revalidate()?;
        let store = self.store_mut();
        let state = store.current_state()?;
        let plan = TerminalPlan {
            store_id: store.store_id.clone(),
            generation: u64::try_from(state.generation).map_err(|_| invalid())?,
        };
        self.revalidate()?;
        Ok(plan)
    }
    /// Borrows original expiry I/O while retaining the actual root owner.
    pub(crate) fn terminal_port<'borrow>(
        &'borrow mut self,
        plan: &'borrow TerminalPlan,
        scope: &'borrow MessageStorageScope,
    ) -> PlannedTerminalStore<'borrow, 'owner> {
        PlannedTerminalStore {
            owned: self,
            plan,
            scope,
        }
    }
}
impl MessageTerminalStore for PlannedTerminalStore<'_, '_> {
    /// Success follows exact original commit/reconciliation and final physical
    /// fence revalidation. Every failure remains uncertain to the lower owner.
    fn commit(&mut self, write: &MessageTerminalWrite) -> mez_agent::messaging::Result<()> {
        match write.kind() {
            MessageTerminalKind::ExpiryObservation => self.commit_expiry_record(write),
            MessageTerminalKind::HorizonRelease => self.commit_release_record(write),
        }
        .map_err(|error| MessageError::invalid_state(format!("MMP terminal storage: {error}")))
    }
}
impl PlannedTerminalStore<'_, '_> {
    /// Never refreshes a first plan or regenerates slots/clock after failure.
    fn commit_expiry_record(&mut self, write: &MessageTerminalWrite) -> Result<()> {
        if !write.belongs_to(self.scope) {
            return Err(invalid());
        }
        self.owned.revalidate()?;
        let store = self.owned.store_mut();
        if store.store_id != self.plan.store_id {
            return Err(invalid());
        }
        let mut bytes = Vec::with_capacity(write.encoded_bytes() + 24);
        bytes.extend_from_slice(b"MMPTEX01");
        bytes.extend_from_slice(&self.plan.store_id);
        write.write_record(&mut bytes)?;
        let frame = read::decode(&bytes, &self.plan.store_id)?;
        if frame.sequence != write.sequence()
            || frame.observed_at_ms != write.observed_at_ms()
            || frame
                .binding_indices
                .iter()
                .copied()
                .ne(write.binding_indices())
        {
            return Err(invalid());
        }
        let key = record_key(&bytes);
        let transaction =
            rusqlite::Transaction::new_unchecked(&store.connection, TransactionBehavior::Immediate)
                .map_err(database_error)?;
        let state = store.checked_state(&transaction)?;
        if u64::try_from(state.generation).map_err(|_| invalid())? < self.plan.generation {
            return Err(invalid());
        }
        store.validate_expiry_source_in_snapshot(&transaction, &frame, write.decode_limits())?;
        store.check_expiry_ack_conflicts_in_snapshot(
            &transaction,
            &frame,
            write.decode_limits(),
        )?;
        let existing = read_record(&transaction, &key)?;
        store.visit_expiry_metadata(&transaction, |other_key, other| {
            if other.sequence == frame.sequence {
                if other.source_sha256 != frame.source_sha256
                    || other.observed_at_ms != frame.observed_at_ms
                {
                    return Err(invalid());
                }
                if other_key != key
                    && other
                        .binding_indices
                        .iter()
                        .any(|index| frame.binding_indices.contains(index))
                {
                    return Err(invalid());
                }
            }
            Ok(())
        })?;
        if let Some(existing) = existing {
            if existing != bytes {
                return Err(invalid());
            }
            store.finish_transaction(transaction)?;
        } else {
            let unaffected = unaffected_rows_digest(&transaction, &[&key])?;
            store.commit_in_transaction(
                &transaction,
                self.plan.generation,
                &[CustodyWrite {
                    key: &key,
                    previous: None,
                    value: Some(&bytes),
                }],
            )?;
            store.validate_expiry_source_in_snapshot(
                &transaction,
                &frame,
                write.decode_limits(),
            )?;
            store.check_expiry_ack_conflicts_in_snapshot(
                &transaction,
                &frame,
                write.decode_limits(),
            )?;
            if unaffected_rows_digest(&transaction, &[&key])? != unaffected {
                return Err(invalid());
            }
            store.finish_transaction(transaction)?;
        }
        self.owned.revalidate()?;
        Ok(())
    }
}

/// Includes the concrete original store binding and every immutable frame byte.
fn record_key(bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"mezzanine-mmp-terminal-expiry-v1\0");
    hash.update(bytes);
    let mut key = String::from("t1:");
    for byte in hash.finalize() {
        use std::fmt::Write;
        let _ = write!(key, "{byte:02x}");
    }
    key
}
/// Bounded source/ownership diagnostic without authored content.
fn invalid() -> MezError {
    MezError::invalid_state("MMP original terminal custody unavailable")
}

#[cfg(test)]
mod tests;
