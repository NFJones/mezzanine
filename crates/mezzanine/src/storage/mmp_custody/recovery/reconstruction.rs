//! Single-snapshot bridge from original-store evidence to canonical cold rows.
//!
//! The lower constructor owns the metadata/cache reservations. This adapter
//! qualifies the complete original store under the existing kernel fence, keeps
//! that evidence charged while moving one original at a time into lower custody,
//! and commits the read transaction before returning. It creates no endpoint
//! registrations and is not the actual session/native restoration authority.

use mez_agent::messaging::{
    MessageError, MessageService, MessageStorageLimits, MessageStorageRecoveredRecord,
    MessageStorageRecovery, MessageStorageRecoveryStore, MessageStorageScope,
};

use super::*;

/// One borrowed actual store, tied to the retained immutable checkpoint input.
struct Port<'a> {
    store: &'a MmpCustodyStore,
    checkpoint: Arc<MessageServiceSnapshot>,
}

impl OwnedCustodyStore<'_> {
    /// Rebuilds the single canonical cold owner under a new actual root scope.
    /// All originals and durable ACKs share one SQLite snapshot; corrupt,
    /// incomplete or over-budget history returns no candidate. No raw/native
    /// registration or shell epoch is restored by this storage-only operation.
    pub(crate) fn reconstruct_cold_service(
        &mut self,
        checkpoint: Arc<MessageServiceSnapshot>,
        scope: &MessageStorageScope,
        limits: MessageStorageLimits,
        retention_messages: usize,
        retention_bytes: usize,
    ) -> Result<MessageService> {
        self.revalidate()?;
        let service = MessageService::recover_storage_custody(
            scope,
            limits,
            retention_messages,
            retention_bytes,
            &mut Port {
                store: self.store_mut(),
                checkpoint,
            },
        )
        .map_err(|error| invalid(&format!("cold reconstruction failed: {error}")))?;
        self.revalidate()?;
        Ok(service)
    }
}

impl MessageStorageRecoveryStore for Port<'_> {
    /// Installs only fully verified originals, folding explicit slots without
    /// inferring positive ACK or native ownership from a saved cursor hint.
    fn recover(
        &mut self,
        target: &mut MessageStorageRecovery,
    ) -> mez_agent::messaging::Result<u64> {
        self.recover_in_snapshot(target).map_err(|error| {
            MessageError::invalid_state(format!("MMP original-store reconstruction: {error}"))
        })
    }
}

impl Port<'_> {
    /// Withholds the whole candidate until every source and the transaction join
    /// are qualified. Phantom headroom narrows the shared 8-MiB storage scan to
    /// the lower destination's actual policy without enlarging either budget.
    fn recover_in_snapshot(&self, target: &mut MessageStorageRecovery) -> Result<u64> {
        let (base, ceiling) = target.metadata_budget().map_err(lower_error)?;
        let phantom = (8 * 1024 * 1024usize)
            .checked_sub(ceiling)
            .ok_or_else(|| invalid("recovery policy exceeds ceiling"))?;
        let metadata_base = base
            .checked_sub(SCRATCH_METADATA)
            .and_then(|v| v.checked_add(phantom))
            .ok_or_else(|| invalid("recovery metadata basis"))?;
        let transaction = self
            .store
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let evidence = self.store.qualify_recovery_in_snapshot(
            self.checkpoint.clone(),
            target.decode_limits().map_err(lower_error)?,
            &transaction,
            metadata_base,
        )?;
        let held = evidence
            .history
            .retained_metadata
            .checked_sub(base)
            .and_then(|v| v.checked_sub(phantom))
            .ok_or_else(|| invalid("recovery metadata charge"))?;
        let _history = target.hold_metadata(held).map_err(lower_error)?;
        let data = evidence
            .checkpoint
            .storage_snapshot
            .as_ref()
            .ok_or_else(|| invalid("missing references"))?;
        for occurrence in &evidence.history.inventory.occurrences {
            let sequence = occurrence.sequence;
            let saved = data
                .sources
                .binary_search_by_key(&sequence, |row| row.sequence)
                .ok()
                .map(|i| &data.sources[i]);
            let expiry_observed_at_ms = evidence
                .history
                .expiries
                .binary_search_by_key(&sequence, |e| e.sequence)
                .ok()
                .map(|i| evidence.history.expiries[i].observed_at_ms);
            let acks = &evidence.history.acknowledgements;
            let start = acks.partition_point(|ack| ack.sequence < sequence);
            let end = acks.partition_point(|ack| ack.sequence <= sequence);
            target
                .restore_original(
                    MessageStorageRecoveredRecord {
                        sequence,
                        source_sha256: occurrence.digest,
                        expired_receipt: saved.is_some_and(|row| row.delivery.status == "expired"),
                        expiry_observed_at_ms,
                    },
                    acks[start..end]
                        .iter()
                        .map(|ack| (ack.binding_index, ack.offered_at_ms)),
                    |limits| {
                        let (source, digest, _) = self
                            .store
                            .read_accepted_source_in_snapshot(&transaction, sequence, limits)
                            .map_err(lower_error)?;
                        if digest != occurrence.digest {
                            return Err(MessageError::invalid_state(
                                "MMP original source changed during reconstruction",
                            ));
                        }
                        Ok(source)
                    },
                )
                .map_err(lower_error)?;
        }
        self.store.finish_transaction(transaction)?;
        Ok(evidence.next_sequence)
    }
}

/// Preserves bounded, payload-free errors across the trusted lower port.
fn lower_error(error: impl std::fmt::Display) -> MessageError {
    MessageError::invalid_state(format!("MMP recovery storage boundary: {error}"))
}
