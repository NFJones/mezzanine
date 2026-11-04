//! Original-store qualification for a cold MMP recovery candidate.
//!
//! A live kernel owner checks checkpoint references, the entire settled store,
//! and all ACK evidence in one SQLite snapshot. Input, inventory, fold indexes,
//! and decoder scratch share one metadata ceiling. Bodies are decoded one at a
//! time and never retained in the result. This is an unpublished transport
//! candidate: native continuity, canonical input deduplication, durable terminal
//! releases, and actual root/session installation remain separate gates. No
//! generic snapshot data or matching owner label acquires live authority here.

use std::io::{self, Write};
use std::sync::Arc;

use mez_agent::messaging::{MessageAcceptanceDecodeLimits, MessageServiceSnapshot};

use super::{history::CustodyHistory, inventory::charge, ownership::OwnedCustodyStore, *};

mod native;
mod reconstruction;

/// Metadata reserved for one bounded source decode, ACK decisions and reader.
/// Payload scratch has its own one-allocation, at-most-1-MiB ceiling below.
const SCRATCH_METADATA: usize = mez_agent::messaging::MESSAGE_STORAGE_RECOVERY_SCRATCH_METADATA;

/// Verified, unpublished observations tied to the retained original checkpoint.
/// Deliberately has no serialization or conversion into a live message service.
pub(crate) struct CustodyRecoveryEvidence {
    /// Exact immutable input retained without a deep copy. Source metadata and
    /// positive ACK slots are checked; saved native continuity, transient offers,
    /// cursors and other mutable facts remain inert input for later qualification.
    pub checkpoint: Arc<MessageServiceSnapshot>,
    /// Complete original history, including occurrences accepted after capture.
    pub history: CustodyHistory,
    /// Allocator floor from the persistent original watermark, never row maxima.
    pub next_sequence: u64,
    /// Input-only projection charge to retain through actual worker adoption.
    pub checkpoint_metadata: usize,
}

impl OwnedCustodyStore<'_> {
    /// Qualifies cold references under this actual original kernel/store owner.
    /// Replaced allocations, stale watermarks, corruption, unfinished stages,
    /// unsupported terminal evidence or exhausted budgets withhold the result.
    /// The caller must run this synchronous operation on its owned worker and
    /// keep the result unpublished until whole-session recovery is qualified.
    pub(crate) fn qualify_recovery_checkpoint(
        &mut self,
        checkpoint: Arc<MessageServiceSnapshot>,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<CustodyRecoveryEvidence> {
        self.revalidate()?;
        let evidence = self
            .store_mut()
            .qualify_recovery_checkpoint(checkpoint, limits)?;
        self.revalidate()?;
        Ok(evidence)
    }
}

impl MmpCustodyStore {
    /// Builds only private observations; the fenced wrapper publishes the result
    /// after its final root/store revalidation. All SQL reads share a snapshot.
    fn qualify_recovery_checkpoint(
        &self,
        checkpoint: Arc<MessageServiceSnapshot>,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<CustodyRecoveryEvidence> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let evidence = self.qualify_recovery_in_snapshot(checkpoint, limits, &transaction, 0)?;
        self.finish_transaction(transaction)?;
        Ok(evidence)
    }

    /// Keeps qualification and cold reconstruction under the same SQL snapshot.
    /// Additional retained candidate state and narrowed-policy headroom are
    /// charged before structural indexes or any source decoder allocates.
    fn qualify_recovery_in_snapshot(
        &self,
        checkpoint: Arc<MessageServiceSnapshot>,
        limits: MessageAcceptanceDecodeLimits,
        transaction: &rusqlite::Transaction<'_>,
        metadata_base: usize,
    ) -> Result<CustodyRecoveryEvidence> {
        let mut count = ByteCount(0);
        serde_json::to_writer(&mut count, checkpoint.as_ref())
            .map_err(|_| invalid("checkpoint metadata size"))?;
        let checkpoint_metadata = count
            .0
            .checked_mul(8)
            .ok_or_else(|| invalid("metadata overflow"))?;
        let mut metadata = SCRATCH_METADATA;
        charge(&mut metadata, metadata_base)?;
        charge(&mut metadata, checkpoint_metadata)?;
        checkpoint
            .validate_storage_snapshot()
            .map_err(|_| invalid("invalid cold checkpoint"))?;
        // The input's own declared ceilings cannot enlarge recovery memory.
        // These limits and the fixed reader/ACK headroom fit SCRATCH_METADATA.
        let limits = MessageAcceptanceDecodeLimits {
            payload_bytes: limits.payload_bytes.min(1024 * 1024),
            auxiliary_bytes: limits.auxiliary_bytes.min(512 * 1024),
            recipient_count: limits.recipient_count.min(4096),
            recipient_bytes: limits.recipient_bytes.min(1024 * 1024),
        };
        let data = checkpoint
            .storage_snapshot
            .as_ref()
            .ok_or_else(|| invalid("missing references"))?;
        let binding = data
            .binding
            .as_ref()
            .ok_or_else(|| invalid("missing original binding"))?;
        let directory = self.directory.metadata()?;
        if binding.owner != self.owner
            || binding.store_id.as_slice() != self.store_id
            || binding.directory_device != directory.dev()
            || binding.directory_inode != directory.ino()
            || binding.database_device != self.database_identity.device
            || binding.database_inode != self.database_identity.inode
        {
            return Err(invalid("original allocation changed"));
        }
        let history = self.fold_custody_history_in_snapshot(transaction, limits, metadata)?;
        if !history.inventory.watermark_origin_trusted
            || history.inventory.sequence_watermark < binding.sequence_watermark
            || history.inventory.generation < binding.generation
        {
            return Err(invalid("original history rolled back or is unqualified"));
        }
        for row in &data.sources {
            let (source, digest, _) =
                self.read_accepted_source_in_snapshot(transaction, row.sequence, limits)?;
            if digest != row.source_sha256
                || source.envelope().id != row.delivery.message_id
                || source.envelope().payload.len() != row.payload_bytes
                || source.accepted_at_ms() != row.accepted_at_ms
                || source.envelope().ttl_ms != row.ttl_ms
                || source.recipient_bindings() != row.recipient_bindings
            {
                return Err(invalid("checkpoint disagrees with original source"));
            }
            for (index, acknowledged) in row.acknowledged.iter().enumerate() {
                if *acknowledged {
                    let key = (row.sequence, index as u32);
                    let found = history
                        .acknowledgements
                        .binary_search_by_key(&key, |ack| (ack.sequence, ack.binding_index));
                    if found.ok().is_none_or(|i| {
                        row.offered_at_ms[index]
                            .is_none_or(|clock| clock > history.acknowledgements[i].offered_at_ms)
                    }) {
                        return Err(invalid("checkpoint ACK lacks original durable evidence"));
                    }
                }
            }
            // Initial expiry observations require exact actual t1 slots. Owner
            // retirement and consumed/horizon releases still need their own
            // durable producers; snapshot bits never grant those transitions.
            for (index, terminal) in row.terminal.iter().enumerate() {
                if let Some(terminal) = terminal {
                    let key = (row.sequence, index as u32);
                    let found = history
                        .terminal_expiries
                        .binary_search_by_key(&key, |value| (value.sequence, value.binding_index));
                    if terminal.reason != mez_agent::messaging::DeliveryTerminalReason::Expired
                        || found.ok().is_none_or(|i| {
                            let actual = &history.terminal_expiries[i];
                            actual.observed_at_ms != terminal.observed_at_ms
                                || (terminal.released && !actual.released())
                        })
                    {
                        return Err(invalid("terminal outcome needs durable recovery evidence"));
                    }
                }
            }
            if row.delivery.status == "expired"
                && history
                    .expiries
                    .binary_search_by_key(&row.sequence, |e| e.sequence)
                    .is_err()
            {
                return Err(invalid("terminal outcome needs durable recovery evidence"));
            }
        }
        let next_sequence = history
            .inventory
            .sequence_watermark
            .checked_add(1)
            .ok_or_else(|| invalid("original allocator exhausted"))?
            .max(checkpoint.next_sequence);
        Ok(CustodyRecoveryEvidence {
            checkpoint,
            history,
            next_sequence,
            checkpoint_metadata,
        })
    }
}

/// Counts encoded input without allocating a second serialized checkpoint.
struct ByteCount(usize);
impl Write for ByteCount {
    /// Counts a serialized fragment without copying or retaining its bytes.
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("checkpoint size overflow"))?;
        Ok(bytes.len())
    }
    /// A counting sink has no buffered output to publish.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Content-free failure that never discloses a body or private native identity.
fn invalid(detail: &str) -> MezError {
    MezError::invalid_state(format!("MMP recovery {detail}"))
}

#[cfg(test)]
mod tests;
