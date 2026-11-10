//! Single-snapshot bridge from original-store evidence to canonical cold rows.
//!
//! The lower constructor owns the metadata/cache reservations. This adapter
//! qualifies the complete original store under the existing kernel fence, keeps
//! that evidence charged while moving one original at a time into lower custody,
//! and commits the read transaction before returning. Storage-only reconstruction
//! leaves endpoints empty; the optional private-context path qualifies native
//! endpoints in that same read snapshot and returns unpublished opaque epochs.

use crate::runtime::PreparedNativeCustodyContext;
use mez_agent::messaging::{
    MessageError, MessageRecoveredNativeBinding, MessageService, MessageStorageLimits,
    MessageStorageRecoveredRecord, MessageStorageRecovery, MessageStorageRecoveryStore,
    MessageStorageScope, MessageStorageSnapshotLease,
};

use super::*;

/// One borrowed actual store, tied to the retained immutable checkpoint input.
struct Port<'a> {
    store: &'a MmpCustodyStore,
    checkpoint: Arc<MessageServiceSnapshot>,
    /// Survives the callback so owned root custody can charge retained input.
    input_lease: Option<MessageStorageSnapshotLease>,
    native_context: Option<&'a PreparedNativeCustodyContext>,
    native_lease: Option<MessageStorageSnapshotLease>,
    transaction: Option<rusqlite::Transaction<'a>>,
}

/// Unpublished native/context result sharing the original cold read snapshot.
pub(crate) struct NativeReconstruction {
    pub service: MessageService,
    pub input_lease: MessageStorageSnapshotLease,
    pub native_lease: Option<MessageStorageSnapshotLease>,
    pub bindings: Vec<MessageRecoveredNativeBinding>,
}

impl OwnedCustodyStore<'_> {
    /// Rebuilds the single canonical cold owner under a new actual root scope.
    /// All originals and durable ACKs share one SQLite snapshot; corrupt,
    /// incomplete or over-budget history returns no candidate. No raw/native
    /// registration or shell epoch is restored by this storage-only operation.
    /// The input lease must outlive every retained checkpoint input reference;
    /// root adoption drops its owned input before releasing this reservation.
    pub(crate) fn reconstruct_cold_service(
        &mut self,
        checkpoint: Arc<MessageServiceSnapshot>,
        scope: &MessageStorageScope,
        limits: MessageStorageLimits,
        retention_messages: usize,
        retention_bytes: usize,
    ) -> Result<(MessageService, MessageStorageSnapshotLease)> {
        let result = self.reconstruct_common(
            checkpoint,
            scope,
            limits,
            retention_messages,
            retention_bytes,
            None,
        )?;
        Ok((result.service, result.input_lease))
    }

    /// Qualifies actual private native contexts before closing the same original
    /// inventory/source/ACK read transaction. No endpoint or epoch escapes on any
    /// failure; original input/context leases remain charged through adoption.
    pub(crate) fn reconstruct_native_service(
        &mut self,
        checkpoint: Arc<MessageServiceSnapshot>,
        scope: &MessageStorageScope,
        limits: MessageStorageLimits,
        retention_messages: usize,
        retention_bytes: usize,
        context: &PreparedNativeCustodyContext,
    ) -> Result<NativeReconstruction> {
        self.reconstruct_common(
            checkpoint,
            scope,
            limits,
            retention_messages,
            retention_bytes,
            Some(context),
        )
    }

    fn reconstruct_common(
        &mut self,
        checkpoint: Arc<MessageServiceSnapshot>,
        scope: &MessageStorageScope,
        limits: MessageStorageLimits,
        retention_messages: usize,
        retention_bytes: usize,
        native_context: Option<&PreparedNativeCustodyContext>,
    ) -> Result<NativeReconstruction> {
        self.revalidate()?;
        let mut port = Port {
            store: self.store_mut(),
            checkpoint,
            input_lease: None,
            native_context,
            native_lease: None,
            transaction: None,
        };
        let service = MessageService::recover_storage_custody(
            scope,
            limits,
            retention_messages,
            retention_bytes,
            &mut port,
        )
        .map_err(|error| invalid(&format!("cold reconstruction failed: {error}")))?;
        let input_lease = port
            .input_lease
            .take()
            .ok_or_else(|| invalid("missing input reservation"))?;
        let (service, bindings) = if let Some(context) = native_context {
            let transaction = port
                .transaction
                .take()
                .ok_or_else(|| invalid("native snapshot missing"))?;
            let recovered =
                super::native::recover(port.store, &transaction, context, service, scope)?;
            port.store.finish_transaction(transaction)?;
            recovered
        } else {
            (service, Vec::new())
        };
        let native_lease = port.native_lease.take();
        drop(port);
        self.revalidate()?;
        Ok(NativeReconstruction {
            service,
            input_lease,
            native_lease,
            bindings,
        })
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

impl<'a> Port<'a> {
    /// Withholds the whole candidate until every source and the transaction join
    /// are qualified. Phantom headroom narrows the shared 8-MiB storage scan to
    /// the lower destination's actual policy without enlarging either budget.
    fn recover_in_snapshot(&mut self, target: &mut MessageStorageRecovery) -> Result<u64> {
        if let Some(context) = self.native_context {
            self.native_lease = Some(
                target
                    .hold_metadata(context.metadata_bytes())
                    .map_err(lower_error)?,
            );
            context.revalidate()?;
        }
        let (base, ceiling) = target.metadata_budget().map_err(lower_error)?;
        let phantom = (8 * 1024 * 1024usize)
            .checked_sub(ceiling)
            .ok_or_else(|| invalid("recovery policy exceeds ceiling"))?;
        let metadata_base = base
            .checked_sub(SCRATCH_METADATA)
            .and_then(|v| v.checked_add(phantom))
            .ok_or_else(|| invalid("recovery metadata basis"))?;
        let store: &'a MmpCustodyStore = self.store;
        let transaction = store
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
        let history_metadata = held
            .checked_sub(evidence.checkpoint_metadata)
            .ok_or_else(|| invalid("input metadata charge"))?;
        self.input_lease = Some(
            target
                .hold_metadata(evidence.checkpoint_metadata)
                .map_err(lower_error)?,
        );
        let history_lease = target
            .hold_metadata(history_metadata)
            .map_err(lower_error)?;
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
            if occurrence.retired {
                if expiry_observed_at_ms.is_some()
                    || saved.is_some_and(|row| row.delivery.status == "expired")
                    || evidence
                        .history
                        .terminal_observations
                        .iter()
                        .any(|value| value.sequence == sequence)
                {
                    return Err(invalid(
                        "retired acknowledged source has terminal obligations",
                    ));
                }
                let limits = target.decode_limits().map_err(lower_error)?;
                let (source, digest, _) = self
                    .store
                    .read_retired_metadata_in_snapshot(&transaction, sequence, limits)?
                    .ok_or_else(|| invalid("retired source is missing"))?;
                if digest != occurrence.digest {
                    return Err(invalid("retired source changed"));
                }
                let native = source.native_sender().is_some()
                    || (0..source.recipient_bindings().len())
                        .any(|index| source.native_recipient(index).is_some());
                let acknowledgements = acks[start..end]
                    .iter()
                    .map(|ack| (ack.binding_index, ack.offered_at_ms));
                if native {
                    let Some(clocks) =
                        self.store
                            .native_retirement_clocks(&transaction, &source, digest)?
                    else {
                        return Err(invalid("retired native source lacks committed input"));
                    };
                    // Validate every original ACK, not only the maximum folded
                    // clock. Drop this source before ACK readers decode theirs.
                    drop(source);
                    if !super::super::retirement_inspection::acknowledged::native_acks_match(
                        self.store,
                        &transaction,
                        sequence,
                        &clocks,
                        limits,
                    )? {
                        return Err(invalid("retired native ACK differs from canonical commit"));
                    }
                    target.restore_retired_acknowledged_native(
                        sequence,
                        acknowledgements,
                        |limits| {
                            let (source, digest, _) = self
                                .store
                                .read_retired_metadata_in_snapshot(&transaction, sequence, limits)
                                .map_err(lower_error)?
                                .ok_or_else(|| {
                                    MessageError::invalid_state("MMP retired native source missing")
                                })?;
                            if digest != occurrence.digest {
                                return Err(MessageError::invalid_state(
                                    "MMP retired native source changed",
                                ));
                            }
                            Ok(source)
                        },
                    )
                } else {
                    target.restore_retired_acknowledged(sequence, acknowledgements, |_| Ok(source))
                }
                .map_err(lower_error)?;
                continue;
            }
            target
                .restore_original(
                    MessageStorageRecoveredRecord {
                        sequence,
                        source_sha256: occurrence.digest,
                        expired_receipt: saved.is_some_and(|row| row.delivery.status == "expired"),
                        expiry_observed_at_ms,
                        horizon_observed_at_ms: {
                            let values = &evidence.history.terminal_observations;
                            let start = values.partition_point(|value| value.sequence < sequence);
                            let end = values.partition_point(|value| value.sequence <= sequence);
                            values[start..end]
                                .iter()
                                .filter_map(|value| value.release_observed_at_ms)
                                .max()
                        },
                        terminal_releases: {
                            let values = &evidence.history.terminal_observations;
                            let start = values.partition_point(|value| value.sequence < sequence);
                            let end = values.partition_point(|value| value.sequence <= sequence);
                            values[start..end]
                                .iter()
                                .filter(|value| value.release_observed_at_ms.is_some())
                                .map(|value| value.binding_index)
                                .collect()
                        },
                        terminal_observations: {
                            let values = &evidence.history.terminal_observations;
                            let start = values.partition_point(|value| value.sequence < sequence);
                            let end = values.partition_point(|value| value.sequence <= sequence);
                            values[start..end]
                                .iter()
                                .map(|value| {
                                    (value.binding_index, value.reason, value.observed_at_ms)
                                })
                                .collect()
                        },
                        terminal_consumptions: {
                            let values = &evidence.history.terminal_observations;
                            let start = values.partition_point(|value| value.sequence < sequence);
                            let end = values.partition_point(|value| value.sequence <= sequence);
                            values[start..end]
                                .iter()
                                .filter(|value| value.sender_consumed)
                                .map(|value| value.binding_index)
                                .collect()
                        },
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
            target
                .restore_raw_registration_ends(sequence, |namespace| {
                    let key = super::super::registration_end::record_key(&namespace);
                    self.store
                        .read_registration_end_in_snapshot(&transaction, &key)
                        .map(|record| record.map(|record| record.snapshot))
                        .map_err(lower_error)
                })
                .map_err(lower_error)?;
        }
        if self.native_context.is_some() {
            self.transaction = Some(transaction);
        } else {
            self.store.finish_transaction(transaction)?;
        }
        let next_sequence = evidence.next_sequence;
        drop(evidence);
        drop(history_lease);
        Ok(next_sequence)
    }
}

/// Preserves bounded, payload-free errors across the trusted lower port.
fn lower_error(error: impl std::fmt::Display) -> MessageError {
    MessageError::invalid_state(format!("MMP recovery storage boundary: {error}"))
}
