//! Original registration-end journal, separate from source-slot dispositions.
//!
//! The trusted lower input comes from an actual retained registration allocation
//! and its first removal cutoff. One immutable o1 row binds those facts to this
//! original store and publication generation. The first plan, every non-target
//! row and the exact postimage remain fixed through SQL effects and lost replies.
//! Reads supply descriptive end evidence; they do not retire source slots,
//! acknowledge delivery, revive an endpoint or reconstruct a live capability.

use mez_agent::messaging::{
    MessageNativeCheckpoint, MessageRegistrationEndSnapshot, MessageRegistrationEndStore,
    MessageRegistrationEndWrite,
};
use serde::{Deserialize, Serialize};

use super::{integrity::unaffected_rows_digest, *};

/// Bounds end framing and decoded fixed-shape metadata independently of payload.
const MAX_RECORD: usize = 4096;

/// Immutable descriptive end facts, bound to an actual private store allocation.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredRegistrationEnd {
    format_version: u32,
    store_id: [u8; 16],
    /// Original publication generation, never refreshed by retry or reads.
    pub generation: u64,
    /// Actual original namespace, interval floor and first removal cutoff.
    pub snapshot: MessageRegistrationEndSnapshot,
}

/// First original position, input, target and every non-target before-image.
pub(crate) struct RegistrationEndPlan {
    store_id: Vec<u8>,
    generation: u64,
    watermark: u64,
    key: String,
    snapshot: MessageRegistrationEndSnapshot,
    bytes: Vec<u8>,
    unaffected: [u8; 32],
}

impl RegistrationEndPlan {
    /// Rechecks the exact original post-position after actual owned I/O.
    pub(crate) fn validate_settled(&self, store: &MmpCustodyStore) -> Result<()> {
        let state = store.current_state()?;
        if self.store_id != store.store_id
            || self.generation.checked_add(1) != Some(state.generation as u64)
            || state.watermark != self.watermark.max(self.snapshot.ended_after_sequence)
        {
            return Err(invalid("settled original end position changed"));
        }
        Ok(())
    }
}

/// Actual-root borrowed writer; the root retains its plan outside this worker.
pub(crate) struct RegistrationEndPort<'a> {
    store: &'a mut MmpCustodyStore,
    plan: &'a RegistrationEndPlan,
}

impl MmpCustodyStore {
    /// Freezes one actual end input and absent target in this store's snapshot.
    /// Existing ends are immutable and cannot be reseated as a fresh plan.
    pub(crate) fn registration_end_plan(
        &self,
        snapshot: &MessageRegistrationEndSnapshot,
    ) -> Result<RegistrationEndPlan> {
        snapshot.validate().map_err(|_| invalid("end tuple"))?;
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let state = self.checked_state(&transaction)?;
        let key = record_key(&snapshot.registration_namespace);
        if self
            .read_registration_end_in_snapshot(&transaction, &key)?
            .is_some()
        {
            return Err(invalid("original namespace already has a journaled end"));
        }
        let record = StoredRegistrationEnd {
            format_version: 1,
            store_id: self
                .store_id
                .as_slice()
                .try_into()
                .map_err(|_| invalid("end allocation"))?,
            generation: (state.generation as u64)
                .checked_add(1)
                .ok_or_else(|| invalid("end generation overflow"))?,
            snapshot: snapshot.clone(),
        };
        let bytes = serde_json::to_vec(&record).map_err(|_| invalid("end encoding"))?;
        if bytes.len() > MAX_RECORD {
            return Err(invalid("end record capacity"));
        }
        let unaffected = unaffected_rows_digest(&transaction, &[&key])?;
        self.finish_transaction(transaction)?;
        Ok(RegistrationEndPlan {
            store_id: self.store_id.clone(),
            generation: state.generation as u64,
            watermark: state.watermark,
            key,
            snapshot: snapshot.clone(),
            bytes,
            unaffected,
        })
    }

    /// Borrows only the supplied immutable original end plan.
    pub(crate) fn registration_end_port<'a>(
        &'a mut self,
        plan: &'a RegistrationEndPlan,
    ) -> RegistrationEndPort<'a> {
        RegistrationEndPort { store: self, plan }
    }

    /// Reads one bounded end fact in the actual same-connection snapshot.
    /// Corruption fails closed; absence is unknown, never inferred retirement.
    pub(crate) fn read_registration_end_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        key: &str,
    ) -> Result<Option<StoredRegistrationEnd>> {
        if !std::ptr::eq(&self.connection, &**transaction) {
            return Err(invalid("end snapshot belongs to another connection"));
        }
        let state = self.checked_state(transaction)?;
        self.read(key)?
            .map(|bytes| {
                if bytes.len() > MAX_RECORD {
                    return Err(invalid("end record capacity"));
                }
                let record: StoredRegistrationEnd =
                    serde_json::from_slice(&bytes).map_err(|_| invalid("end framing"))?;
                record
                    .snapshot
                    .validate()
                    .map_err(|_| invalid("end tuple"))?;
                if record.format_version != 1
                    || record.store_id.as_slice() != self.store_id
                    || record.generation == 0
                    || record.generation > state.generation as u64
                    || record.snapshot.ended_after_sequence > state.watermark
                    || record_key(&record.snapshot.registration_namespace) != key
                {
                    return Err(invalid("end allocation, key, generation or cutoff"));
                }
                Ok(record)
            })
            .transpose()
    }

    /// Qualifies an original native tuple against its namespace's explicit end.
    /// A mismatched relation is corrupt evidence, not a missing or revived owner.
    pub(super) fn native_registration_has_ended(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        native: &MessageNativeCheckpoint,
        floor: u64,
        publication: Option<u64>,
    ) -> Result<bool> {
        let Some(end) = self.read_registration_end_in_snapshot(
            transaction,
            &record_key(&native.registration_namespace),
        )?
        else {
            return Ok(false);
        };
        if end.snapshot.agent_id != native.agent_id
            || end.snapshot.registered_after_sequence != floor
            || publication.is_some_and(|generation| generation >= end.generation)
        {
            return Err(invalid(
                "native tuple disagrees with original registration end",
            ));
        }
        Ok(true)
    }
}

impl MessageRegistrationEndStore for RegistrationEndPort<'_> {
    /// Applies/reconciles one exact end batch under its original position.
    fn commit(&mut self, write: &MessageRegistrationEndWrite) -> mez_agent::messaging::Result<()> {
        self.persist(write).map_err(|error| {
            mez_agent::messaging::MessageError::invalid_state(format!(
                "MMP registration end storage: {error}"
            ))
        })
    }
}

impl RegistrationEndPort<'_> {
    /// Preserves the first input and every non-target row before and after SQL.
    fn persist(&mut self, write: &MessageRegistrationEndWrite) -> Result<()> {
        if self.plan.store_id != self.store.store_id || self.plan.snapshot != *write.snapshot() {
            return Err(invalid("original end input changed"));
        }
        let transaction = self
            .store
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let state = self.store.checked_state(&transaction)?;
        if state.watermark < self.plan.watermark
            || unaffected_rows_digest(&transaction, &[&self.plan.key])? != self.plan.unaffected
        {
            return Err(invalid("original end position or non-target rows changed"));
        }
        self.store.commit_in_transaction_with_floor(
            &transaction,
            self.plan.generation,
            &[CustodyWrite {
                key: &self.plan.key,
                previous: None,
                value: Some(&self.plan.bytes),
            }],
            self.plan.snapshot.ended_after_sequence,
        )?;
        if self.store.read(&self.plan.key)?.as_deref() != Some(self.plan.bytes.as_slice())
            || unaffected_rows_digest(&transaction, &[&self.plan.key])? != self.plan.unaffected
        {
            return Err(invalid("end SQL effects changed original rows"));
        }
        self.store.finish_transaction(transaction)
    }
}

/// Exact canonical namespace attribution; this key is never a filesystem path.
pub(super) fn record_key(namespace: &[u8; 16]) -> String {
    use std::fmt::Write;
    let mut key = String::with_capacity(35);
    key.push_str("o1:");
    for byte in namespace {
        let _ = write!(key, "{byte:02x}");
    }
    key
}

/// Content-free context for invalid original registration-end evidence.
fn invalid(context: &str) -> MezError {
    MezError::invalid_state(format!("MMP registration end: {context}"))
}

#[cfg(test)]
mod tests;
