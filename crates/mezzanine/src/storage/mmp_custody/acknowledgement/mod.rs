//! Bounded immutable ACK records in the same private custody projection.
//!
//! One linear lower ACK freezes the recipient interval and exact cumulative
//! decisions after trusted application commit. The session root must bind it to
//! this original store and serialize the plan through settlement. This adapter
//! verifies actual accepted source chunks before committing one bounded version2
//! record with the generic transactional receipt. Lost replies reconcile actual
//! identical bytes, not a saved batch label. No mailbox, application replay or
//! native caller authority is reconstructed here. Restore folding, compaction,
//! control headroom and actor ingress integration remain separate requirements.

use mez_agent::messaging::{
    MessageAcknowledgementStore, MessageAcknowledgementWrite, MessageError,
};

use super::acceptance::AcceptancePlan;
use super::*;

mod recovery;

/// Payload-free exact store position retained before physical ACK issuance.
#[derive(Debug, Clone)]
pub(crate) struct AcknowledgementPlan {
    /// Persistent private allocation, not an equal owner label or pane ID.
    store_id: Vec<u8>,
    /// Generation before this original operation, unchanged during retry.
    generation: u64,
}

impl AcknowledgementPlan {
    /// Checks cached allocation identity without changing the original source plan.
    pub(crate) fn matches_store(&self, store: &MmpCustodyStore) -> bool {
        self.store_id == store.store_id
    }

    /// Uses an idle root's already observed position without actor-side SQL.
    pub(super) fn from_position(store_id: Vec<u8>, generation: u64) -> Self {
        Self {
            store_id,
            generation,
        }
    }
}

/// Exact store position of a guarded ACK, captured in its actual transaction.
/// A generation number alone cannot identify which batch advanced the store.
#[derive(Clone)]
pub(crate) struct GuardedAckSettlement {
    store_id: Vec<u8>,
    original_sequence: u64,
    original_generation: u64,
    generation: u64,
    batch: Vec<u8>,
}

impl GuardedAckSettlement {
    /// Requires the original store allocation and exact suspended plan.
    pub(super) fn matches(&self, store: &MmpCustodyStore, original: &AcceptancePlan) -> bool {
        self.store_id == store.store_id
            && original.matches_store(store)
            && self.original_sequence == original.sequence()
            && self.original_generation == original.generation()
    }
    /// Returns the exact generation validated with the ACK postimage.
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
    /// Returns the exact bounded last-batch digest at that same position.
    pub(super) fn batch(&self) -> &[u8] {
        &self.batch
    }
}

/// Trusted synchronous port borrowing the serialized original store.
pub(crate) struct PlannedAcknowledgementStore<'a> {
    /// One existing canonical store, never selected from envelope path fields.
    store: &'a mut MmpCustodyStore,
    /// Original source allocation and position retained outside any worker.
    plan: &'a AcknowledgementPlan,
    /// Optional exact original whose absence fences a temporarily yielded lane.
    suspended: Option<&'a AcceptancePlan>,
    /// Set only after a guarded ACK transaction actually commits.
    settlement: Option<GuardedAckSettlement>,
    /// Test-only interleaving after source validation and before ACK settlement.
    #[cfg(test)]
    after_sources: Option<&'a dyn Fn()>,
}
impl MmpCustodyStore {
    /// Captures physical position off actor before issuing one original ACK.
    pub fn acknowledgement_plan(&self) -> Result<AcknowledgementPlan> {
        let generation = self.current_state()?.generation;
        Ok(AcknowledgementPlan {
            store_id: self.store_id.clone(),
            generation: u64::try_from(generation)
                .map_err(|_| MezError::invalid_state("invalid MMP ACK generation"))?,
        })
    }
    /// Borrows exact original ACK storage; all errors remain uncertain.
    pub fn acknowledgement_port<'a>(
        &'a mut self,
        plan: &'a AcknowledgementPlan,
    ) -> PlannedAcknowledgementStore<'a> {
        PlannedAcknowledgementStore {
            store: self,
            plan,
            suspended: None,
            settlement: None,
            #[cfg(test)]
            after_sources: None,
        }
    }

    /// Binds an ACK transaction to an original acceptance that has not written
    /// any stage. The same transaction rechecks that fact around its mutation.
    pub(crate) fn acknowledgement_port_guarded<'a>(
        &'a mut self,
        plan: &'a AcknowledgementPlan,
        suspended: &'a AcceptancePlan,
    ) -> PlannedAcknowledgementStore<'a> {
        PlannedAcknowledgementStore {
            store: self,
            plan,
            suspended: Some(suspended),
            settlement: None,
            #[cfg(test)]
            after_sources: None,
        }
    }

    /// Supplies a deterministic competing-writer interleaving to tests only.
    #[cfg(test)]
    fn acknowledgement_port_with_probe<'a>(
        &'a mut self,
        plan: &'a AcknowledgementPlan,
        after_sources: &'a dyn Fn(),
    ) -> PlannedAcknowledgementStore<'a> {
        PlannedAcknowledgementStore {
            store: self,
            plan,
            suspended: None,
            settlement: None,
            after_sources: Some(after_sources),
        }
    }
}
impl MessageAcknowledgementStore for PlannedAcknowledgementStore<'_> {
    /// Qualifies only actual canonical ACK bytes and complete original sources.
    /// The caller/root still proves actual application commit and live ownership.
    fn commit(&mut self, record: &MessageAcknowledgementWrite) -> mez_agent::messaging::Result<()> {
        self.settlement = None;
        self.commit_record(record)
            .map_err(|error| MessageError::invalid_state(format!("MMP ACK storage: {error}")))
    }
}
impl PlannedAcknowledgementStore<'_> {
    /// Transfers only the exact successfully committed guarded position.
    pub(crate) fn take_guarded_settlement(&mut self) -> Option<GuardedAckSettlement> {
        self.settlement.take()
    }

    /// Commits one bounded immutable record or reconciles its exact postimage.
    fn commit_record(&mut self, record: &MessageAcknowledgementWrite) -> Result<()> {
        if self.store.store_id != self.plan.store_id {
            return Err(MezError::conflict(
                "MMP ACK original store allocation changed",
            ));
        }
        if record.encoded_bytes() > 240 * 1024 {
            return Err(MezError::invalid_args("MMP ACK encoded batch capacity"));
        }
        let mut bytes = Vec::with_capacity(record.encoded_bytes());
        record.write_record(&mut bytes).map_err(MezError::from)?;
        if bytes.len() != record.encoded_bytes() {
            return Err(MezError::invalid_state("MMP ACK original encoding changed"));
        }

        let offset = 48 + record.recipient().as_str().len();
        let (encoded_entries, remainder) = bytes[offset..].as_chunks::<53>();
        if !remainder.is_empty() || encoded_entries.len() != record.entries().count() {
            return Err(MezError::invalid_state("MMP ACK entry framing changed"));
        }
        // The same immediate transaction fences source validation, existing
        // ACK reconciliation and any new ACK write against other writers.
        let transaction = rusqlite::Transaction::new_unchecked(
            &self.store.connection,
            TransactionBehavior::Immediate,
        )
        .map_err(database_error)?;
        let state = self.store.checked_state(&transaction)?;
        let key = ack_key(&bytes);
        let existing = read_record(&transaction, &key)?;
        let actual_generation = u64::try_from(state.generation)
            .map_err(|_| MezError::invalid_state("invalid MMP ACK generation"))?;
        if let Some(original) = self.suspended {
            if original.generation() != self.plan.generation {
                return Err(MezError::conflict("MMP suspended acceptance plan changed"));
            }
            let initial = actual_generation == self.plan.generation;
            let exact_lost_reply = self.plan.generation.checked_add(1) == Some(actual_generation)
                && existing.as_deref() == Some(bytes.as_slice())
                && state.batch
                    == batch_digest(&[CustodyWrite {
                        key: &key,
                        previous: None,
                        value: Some(&bytes),
                    }])?;
            if !(initial || exact_lost_reply)
                || !self.store.acceptance_never_written_in_snapshot(
                    &transaction,
                    original,
                    actual_generation,
                )?
            {
                return Err(MezError::conflict(
                    "MMP suspended acceptance position changed",
                ));
            }
        }
        if u64::try_from(state.generation)
            .map_err(|_| MezError::invalid_state("invalid MMP ACK generation"))?
            < self.plan.generation
        {
            return Err(MezError::invalid_state(
                "MMP ACK store generation rolled back",
            ));
        }
        let mut previous = 0;
        for (entry, encoded) in record.entries().zip(encoded_entries) {
            if entry.sequence <= previous || entry.sequence <= record.registered_after_sequence() {
                return Err(MezError::invalid_state(
                    "MMP ACK original occurrence order changed",
                ));
            }
            let original = self
                .store
                .validate_accepted_occurrence_in_snapshot(&transaction, entry.sequence)?;
            if encoded[21..] != original {
                return Err(MezError::conflict(
                    "MMP ACK original source content changed",
                ));
            }
            previous = entry.sequence;
        }
        if previous != record.endpoint_sequence() {
            return Err(MezError::invalid_state("MMP ACK endpoint source missing"));
        }
        #[cfg(test)]
        if let Some(after_sources) = self.after_sources {
            after_sources();
        }
        if let Some(existing) = existing {
            if existing != bytes {
                return Err(MezError::conflict("MMP ACK committed postimage changed"));
            }
            // The ACK may predate this plan; a later valid batch need not be
            // this ACK's batch. Exact content and source checks are decisive.
            if let Some(original) = self.suspended
                && !self.store.acceptance_never_written_in_snapshot(
                    &transaction,
                    original,
                    actual_generation,
                )?
            {
                return Err(MezError::conflict(
                    "MMP suspended acceptance position changed",
                ));
            }
            self.store.finish_transaction(transaction)?;
            if let Some(original) = self.suspended {
                self.settlement = Some(GuardedAckSettlement {
                    store_id: self.store.store_id.clone(),
                    original_sequence: original.sequence(),
                    original_generation: original.generation(),
                    generation: actual_generation,
                    batch: state.batch,
                });
            }
            return Ok(());
        }
        let committed = self.store.commit_in_transaction(
            &transaction,
            self.plan.generation,
            &[CustodyWrite {
                key: &key,
                previous: None,
                value: Some(&bytes),
            }],
        )?;
        // A write trigger can alter a referenced original inside this same
        // transaction. Revalidate before commit so no ACK outlives its source.
        for (entry, encoded) in record.entries().zip(encoded_entries) {
            let original = self
                .store
                .validate_accepted_occurrence_in_snapshot(&transaction, entry.sequence)?;
            if encoded[21..] != original {
                return Err(MezError::conflict(
                    "MMP ACK original source content changed",
                ));
            }
        }
        if let Some(original) = self.suspended {
            let generation = self
                .plan
                .generation
                .checked_add(1)
                .ok_or_else(|| MezError::invalid_state("MMP ACK generation exhausted"))?;
            if !self.store.acceptance_never_written_in_snapshot(
                &transaction,
                original,
                generation,
            )? {
                return Err(MezError::conflict(
                    "MMP suspended acceptance position changed",
                ));
            }
        }
        self.store.finish_transaction(transaction)?;
        if let Some(original) = self.suspended {
            self.settlement = Some(GuardedAckSettlement {
                store_id: self.store.store_id.clone(),
                original_sequence: original.sequence(),
                original_generation: original.generation(),
                generation: committed.generation(),
                batch: committed.digest().to_vec(),
            });
        }
        Ok(())
    }
}

/// Domain-separated content identity over the full immutable versioned record.
/// The key's v1 namespace versions the hash scheme, independently of the
/// record format included in the hashed bytes.
fn ack_key(bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"mezzanine-mmp-ack-record-v1\0");
    hash.update(bytes);
    let digest = hash.finalize();
    let mut key = String::from("k1:");
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(key, "{byte:02x}");
    }
    key
}

#[cfg(test)]
mod tests;
