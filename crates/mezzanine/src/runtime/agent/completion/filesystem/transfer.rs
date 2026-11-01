//! Exact original filesystem source and producer custody transfer.
//!
//! The actor checks fresh original storage and the actual registered producer's
//! observed native join in one synchronous operation. It retains a bounded
//! acknowledgement in that same producer before removing either actor owner.
//! Lost replies therefore recover the original completed transfer without a
//! tombstone archive, effect replay or a new source check after group changes.
//! The acknowledgement retains metadata and raw evidence, never the producer
//! itself. Completing one transfer leaves all ordinary sibling/group gates intact.

use super::*;
use crate::runtime::processes::native_filesystem_producer::{
    NativeFilesystemProducer, NativeFilesystemProducerIdentity,
};

/// Historical exact handoff, not a new mutation or whole-group continuation grant.
#[derive(Clone)]
pub(crate) struct NativeFilesystemTransfer {
    /// Opaque actor realm and original occurrence remain valid after removal.
    attempt: NativeFilesystemAttempt,
    /// Actual producer allocation without a strong producer self-reference.
    producer: NativeFilesystemProducerIdentity,
    /// Accepted original checked source/storage at the serialized transfer boundary.
    storage: NativeFilesystemArchiveSettlement,
    /// Exact actual joined outcome, not a reconstructed equivalent result.
    raw: Arc<NativePatchWorkerOutcome>,
}

impl std::fmt::Debug for NativeFilesystemTransfer {
    /// Paths, diagnostics, source bytes and allocation addresses remain private.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeFilesystemTransfer")
    }
}

impl NativeFilesystemTransfer {
    /// Allows root removal only for the exact original allocation and observed
    /// joined result. Busy observation returns false; no handle is started/taken.
    pub(crate) fn acknowledges(&self, producer: &NativeFilesystemProducer) -> bool {
        self.belongs_to(producer)
            && producer
                .joined_outcome()
                .is_some_and(|raw| Arc::ptr_eq(&raw, &self.raw))
    }

    /// Checks immutable original allocation metadata without a transient join lock.
    pub(crate) fn belongs_to(&self, producer: &NativeFilesystemProducer) -> bool {
        self.attempt == *producer.attempt() && self.producer == producer.identity()
    }

    /// Compares the completed original source, without treating later group
    /// changes as authority to redo this historical transfer.
    fn matches(&self, storage: &NativeFilesystemArchiveSettlement) -> bool {
        self.storage.same_storage(storage)
    }
}

impl RuntimeSessionService {
    /// Transfers only an actual original joined producer and current checked
    /// storage source. Pending gates return None and retain both actor owners.
    /// First errors, foreign actors and substituted producers are rejected.
    /// A completed cached acknowledgement is recoverable after lost replies.
    pub(crate) fn transfer_native_filesystem_producer(
        &mut self,
        producer: &NativeFilesystemProducer,
        storage: &NativeFilesystemArchiveSettlement,
    ) -> Result<Option<NativeFilesystemTransfer>> {
        if !self
            .agent
            .native_filesystem_actions
            .owns_realm(producer.attempt())
            || storage.source().attempt() != producer.attempt()
            || !storage.source().producer_is(producer)
        {
            return Err(MezError::conflict(
                "filesystem transfer original producer changed",
            ));
        }
        if let Some(accepted) = producer.transfer_acknowledgement()? {
            return if accepted.matches(storage) && accepted.belongs_to(producer) {
                Ok(Some(accepted))
            } else {
                Err(MezError::conflict(
                    "filesystem transfer acknowledgement source changed",
                ))
            };
        }
        let key = self.filesystem_completion_key(producer.attempt())?;
        let receipt = self
            .agent
            .completion_settlements
            .filesystem
            .get(&key)
            .ok_or_else(|| MezError::invalid_state("filesystem transfer receipt unavailable"))?;
        if let Some(error) = &receipt.failure {
            return Err((**error).clone());
        }
        let work = Arc::clone(&receipt.work);
        if !self.native_filesystem_archive_settlement_is_current(storage) {
            return Ok(None);
        }
        let Some(raw) = producer.joined_outcome() else {
            return Ok(None);
        };
        if let Err(error) = producer.validate_confirmed_outcome(&raw) {
            self.fail_filesystem_completion(producer.attempt(), &error);
            return Err(error);
        }
        let accepted = NativeFilesystemTransfer {
            attempt: producer.attempt().clone(),
            producer: producer.identity(),
            storage: storage.clone(),
            raw: Arc::clone(&raw),
        };
        self.agent.native_filesystem_actions.transfer_joined(
            producer.attempt(),
            &work,
            producer,
            &raw,
            || producer.retain_transfer_acknowledgement(accepted.clone()),
        )?;
        // Both owners were validated before the producer stored its ack. There
        // is no await, fallible callback or actor reentry between these removals.
        self.agent.completion_settlements.filesystem.remove(&key);
        // Own transfer is already historical and acknowledged. Resume only
        // existing ordinary group intent; a suffix failure stays latched in its
        // original group and cannot undo this handoff or replay the native work.
        self.recover_filesystem_transfer_suffix(&key.0);
        Ok(Some(accepted))
    }
    /// Resumes only retained ordinary intent after own historical handoff.
    /// A suffix error remains latched and cannot invalidate/replay that transfer.
    pub(super) fn recover_filesystem_transfer_suffix(&mut self, turn: &str) {
        if let Err(error) = self.recover_retained_completion_settlement(turn) {
            let _ = self.append_runtime_diagnostic_event(format!(
                r#"{{"kind":"filesystem_transfer_settlement_suffix_failed","error_kind":"{:?}"}}"#,
                error.kind(),
            ));
        }
    }
}
