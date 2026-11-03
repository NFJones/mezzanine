//! Qualified release of an unpublished original, preserving positive commit facts.
//!
//! Recovery first proves the original store/plan and actual dense prefix. A real
//! acceptance marker always wins. Otherwise a fixed-size abort disposition replaces
//! the stage header in its existing cell at exactly the same encoded length. Each
//! bounded tail deletion atomically updates that disposition with the previous
//! stage and deleted-row digests. Its generation/count equation and reconstructable
//! last batch detect changed progress rather than repairing uncertain custody.
//! The final tombstone is bounded stored metadata, not mail or live authority.

use super::*;

/// Fixed version2 prefix, padded to the preallocated original header cell.
pub(super) const ABORT_BYTES: usize = 136;
/// Distinct versioned tag; future or malformed dispositions fail closed.
const ABORT_MAGIC: &[u8; 8] = b"MMPABT02";

/// Immutable declaration that this original entered uncommitted cleanup.
#[derive(Clone)]
struct Abort {
    /// Exact original reserved canonical occurrence.
    sequence: u64,
    /// Original physical plan position, not a refreshed retry generation.
    initial_generation: u64,
    /// Generation that committed this declaration before any chunk deletion.
    declared_generation: u64,
    /// Dense chunk prefix present at declaration, including its possible tail.
    chunks: u16,
    /// Dense prefix still present after the last recorded deletion.
    remaining: u16,
    /// Full original encoded size, independent of partial storage progress.
    encoded_bytes: u32,
    /// SHA256 of the exact complete original versioned header.
    header_digest: [u8; 32],
    /// Stage preimage digest used by the last committed abort transaction.
    previous_stage_digest: [u8; 32],
    /// Deleted row preimage for the last step; zero on initial declaration.
    last_deleted_digest: [u8; 32],
}

/// Trusted recovery port; only actual original-state evidence qualifies refusal.
pub(crate) struct RecoveryAcceptanceStore<'a> {
    /// One serialized original store, never selected from envelope paths.
    store: &'a mut MmpCustodyStore,
    /// Retained immutable original plan, including persistent allocation identity.
    plan: &'a AcceptancePlan,
}

impl MmpCustodyStore {
    /// Borrows recovery after an actual joined failed/lost write. The caller must
    /// preserve this intent until settlement, including through another lost join.
    pub fn acceptance_recovery_port<'a>(
        &'a mut self,
        plan: &'a AcceptancePlan,
    ) -> RecoveryAcceptanceStore<'a> {
        RecoveryAcceptanceStore { store: self, plan }
    }
}

impl MessageAcceptanceStore for RecoveryAcceptanceStore<'_> {
    /// Returns success for a real committed original; qualified cleanup yields
    /// NotCommitted only after the exact absence/abort postimage is verified.
    /// Every error, including cleanup I/O failure, remains Unknown.
    fn commit(
        &mut self,
        record: &MessageAcceptanceWrite,
    ) -> std::result::Result<(), MessageAcceptanceStoreError> {
        match self.reconcile(record) {
            Ok(true) => Ok(()),
            Ok(false) => Err(MessageAcceptanceStoreError::NotCommitted(
                MessageError::invalid_state("MMP original uncommitted stage released"),
            )),
            Err(error) => Err(MessageAcceptanceStoreError::Unknown(
                MessageError::invalid_state(error.to_string()),
            )),
        }
    }
}

impl RecoveryAcceptanceStore<'_> {
    /// Reconciles positive acceptance or completes the original private cleanup.
    fn reconcile(&mut self, record: &MessageAcceptanceWrite) -> Result<bool> {
        if self.plan.store_id != self.store.store_id || self.plan.sequence != record.sequence() {
            return Err(changed());
        }
        let prefix = format!("a1:{:016x}:", self.plan.sequence);
        let stage_key = format!("{prefix}stage");
        let commit_key = format!("{prefix}commit");
        let (generation, count, batch) = position(self.store, &prefix)?;
        // Exact absence at the untouched starting generation is positive
        // noncommit evidence, including capacity refusal before any encoding.
        if count == 0 && generation == self.plan.generation {
            return Ok(false);
        }
        let (header, header_bytes) = record_header(record, self.store.limits.logical_bytes)?;
        if self.store.read(&commit_key)?.is_some() {
            // Normal reconciliation validates every byte and cannot append when
            // a complete accepted marker is present. Positive acceptance wins.
            self.store
                .acceptance_port(self.plan)
                .commit_record(record)?;
            return Ok(true);
        }
        let staged = self.store.read(&stage_key)?.ok_or_else(changed)?;
        let remaining = count.checked_sub(1).ok_or_else(changed)?;
        let (mut abort, mut next_generation) = if staged == header_bytes {
            if count > header.chunks + 1
                || generation
                    != self
                        .plan
                        .generation
                        .checked_add(count as u64)
                        .ok_or_else(changed)?
            {
                return Err(changed());
            }
            verify_prefix(self.store, &prefix, record, remaining, None)?;
            let last_key = if remaining == 0 {
                stage_key.clone()
            } else {
                chunk_key(&prefix, remaining - 1)
            };
            let last = self.store.read(&last_key)?.ok_or_else(changed)?;
            if batch
                != batch_digest(&[CustodyWrite {
                    key: &last_key,
                    previous: None,
                    value: Some(&last),
                }])?
            {
                return Err(changed());
            }
            let declared_generation = generation.checked_add(1).ok_or_else(changed)?;
            let final_generation = declared_generation
                .checked_add(remaining as u64)
                .ok_or_else(changed)?;
            if final_generation > i64::MAX as u64 {
                return Err(changed());
            }
            let header_digest: [u8; 32] = Sha256::digest(&header_bytes).into();
            let chunks = u16::try_from(remaining).map_err(|_| changed())?;
            let abort = Abort {
                sequence: self.plan.sequence,
                initial_generation: self.plan.generation,
                declared_generation,
                chunks,
                remaining: chunks,
                encoded_bytes: u32::try_from(header.encoded_bytes).map_err(|_| changed())?,
                header_digest,
                previous_stage_digest: header_digest,
                last_deleted_digest: [0; 32],
            };
            // Equal-sized update of the existing private header cell requires
            // no new record or primary-key slot at the database page ceiling.
            let replacement = abort.padded(header_bytes.len());
            let digest = Sha256::digest(&staged);
            let committed = self.store.commit(
                generation,
                &[CustodyWrite {
                    key: &stage_key,
                    previous: Some(&digest),
                    value: Some(&replacement),
                }],
            )?;
            (abort, committed.generation())
        } else {
            if staged.len() != header_bytes.len() {
                return Err(changed());
            }
            let abort = Abort::decode(&staged)?;
            if abort.sequence != self.plan.sequence
                || abort.initial_generation != self.plan.generation
                || abort.encoded_bytes as usize != header.encoded_bytes
                || usize::from(abort.chunks) > header.chunks
                || abort.header_digest != <[u8; 32]>::from(Sha256::digest(&header_bytes))
                || abort.declared_generation
                    != self
                        .plan
                        .generation
                        .checked_add(abort.chunks as u64 + 2)
                        .ok_or_else(changed)?
                || remaining != usize::from(abort.remaining)
                || abort.remaining > abort.chunks
            {
                return Err(changed());
            }
            let expected = abort
                .declared_generation
                .checked_add(u64::from(abort.chunks - abort.remaining))
                .ok_or_else(changed)?;
            if generation != expected {
                return Err(changed());
            }
            verify_abort_batch(&prefix, &staged, &abort, &batch)?;
            (abort, generation)
        };
        let deleted_digest = (abort.remaining < abort.chunks).then_some(&abort.last_deleted_digest);
        verify_prefix(self.store, &prefix, record, remaining, deleted_digest)?;
        for index in (0..remaining).rev() {
            remove_with_evidence(
                self.store,
                &prefix,
                &mut abort,
                &mut next_generation,
                index,
                header_bytes.len(),
            )?;
        }
        // Retained descriptor, actual chunk absence and the exact generation
        // jointly qualify refusal. A lost reply repeats checks, not deletions.
        let (actual, rows, batch) = position(self.store, &prefix)?;
        if actual != next_generation
            || rows != 1
            || self.store.read(&stage_key)?.as_deref()
                != Some(abort.padded(header_bytes.len()).as_slice())
        {
            return Err(changed());
        }
        verify_abort_batch(&prefix, &abort.padded(header_bytes.len()), &abort, &batch)?;
        Ok(false)
    }
}

impl Abort {
    /// Encodes fixed fields without variable-length untrusted strings or payload.
    fn encode(&self) -> [u8; ABORT_BYTES] {
        let mut bytes = [0; ABORT_BYTES];
        bytes[..8].copy_from_slice(ABORT_MAGIC);
        bytes[8..16].copy_from_slice(&self.sequence.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.initial_generation.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.declared_generation.to_le_bytes());
        bytes[32..34].copy_from_slice(&self.chunks.to_le_bytes());
        bytes[34..36].copy_from_slice(&self.remaining.to_le_bytes());
        bytes[36..40].copy_from_slice(&self.encoded_bytes.to_le_bytes());
        bytes[40..72].copy_from_slice(&self.header_digest);
        bytes[72..104].copy_from_slice(&self.previous_stage_digest);
        bytes[104..136].copy_from_slice(&self.last_deleted_digest);
        bytes
    }
    /// Preserves the preallocated original header cell size for abort publication.
    /// Callers already validated that the original header fits the descriptor.
    fn padded(&self, length: usize) -> Vec<u8> {
        let mut bytes = vec![0; length];
        bytes[..ABORT_BYTES].copy_from_slice(&self.encode());
        bytes
    }

    /// Validates version/framing before checked integer conversion or allocation.
    fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < ABORT_BYTES
            || bytes.get(..8) != Some(ABORT_MAGIC.as_slice())
            || bytes[ABORT_BYTES..].iter().any(|byte| *byte != 0)
        {
            return Err(changed());
        }
        let mut header_digest = [0; 32];
        header_digest.copy_from_slice(&bytes[40..72]);
        let mut previous_stage_digest = [0; 32];
        previous_stage_digest.copy_from_slice(&bytes[72..104]);
        let mut last_deleted_digest = [0; 32];
        last_deleted_digest.copy_from_slice(&bytes[104..136]);
        Ok(Self {
            sequence: integer(bytes, 8)?,
            initial_generation: integer(bytes, 16)?,
            declared_generation: integer(bytes, 24)?,
            chunks: u16::from_le_bytes(bytes[32..34].try_into().map_err(|_| changed())?),
            remaining: u16::from_le_bytes(bytes[34..36].try_into().map_err(|_| changed())?),
            encoded_bytes: u32::from_le_bytes(bytes[36..40].try_into().map_err(|_| changed())?),
            header_digest,
            previous_stage_digest,
            last_deleted_digest,
        })
    }
}

/// Validates a fully cleaned abort descriptor as descriptive nonacceptance.
/// Its sequence cannot confer sender, receipt, or recipient authority.
pub(super) fn validate_finalized_abort(
    bytes: &[u8],
    sequence: u64,
    generation: u64,
    logical_bytes: usize,
) -> Result<()> {
    let abort = Abort::decode(bytes)?;
    let declared = abort
        .initial_generation
        .checked_add(abort.chunks as u64)
        .and_then(|value| value.checked_add(2))
        .ok_or_else(changed)?;
    let completed = abort
        .declared_generation
        .checked_add(abort.chunks as u64)
        .ok_or_else(changed)?;
    // The original cell was the encoded header. Its digest bytes can vary in
    // decimal width, but the other fields and resulting chunk count are known.
    // Bound the padded descriptor to the range any real original header could
    // have occupied without attempting to reconstruct the missing original.
    let header = |digest| Header {
        record_version: RECORD_VERSION,
        sequence,
        encoded_bytes: abort.encoded_bytes as usize,
        chunks: (abort.encoded_bytes as usize).div_ceil(CHUNK_BYTES),
        digest,
    };
    let shortest = serde_json::to_vec(&header([0; 32]))
        .map_err(encoding_error)?
        .len();
    let longest = serde_json::to_vec(&header([255; 32]))
        .map_err(encoding_error)?
        .len();
    if abort.sequence != sequence
        || abort.encoded_bytes == 0
        || abort.encoded_bytes as usize > logical_bytes
        || usize::from(abort.chunks) > (abort.encoded_bytes as usize).div_ceil(CHUNK_BYTES)
        || abort.remaining != 0
        || (abort.chunks == 0
            && (abort.previous_stage_digest != abort.header_digest
                || abort.last_deleted_digest != [0; 32]))
        || abort.declared_generation != declared
        || completed > generation
        || !(shortest..=longest).contains(&bytes.len())
    {
        return Err(changed());
    }
    Ok(())
}

/// Parses one fixed little-endian field after bounds checking its original frame.
fn integer(bytes: &[u8], offset: usize) -> Result<u64> {
    let field = bytes.get(offset..offset + 8).ok_or_else(changed)?;
    let mut value = [0; 8];
    value.copy_from_slice(field);
    Ok(u64::from_le_bytes(value))
}

/// Reads generation, prefix row count and bounded last-batch framing together.
fn position(store: &MmpCustodyStore, prefix: &str) -> Result<(u64, usize, Vec<u8>)> {
    let (generation,count,batch,store_id):(i64,i64,Option<Vec<u8>>,Option<Vec<u8>>)=store.connection.query_row(
        "SELECT generation,(SELECT COUNT(*) FROM records WHERE substr(key,1,length(?1))=?1),CASE WHEN length(batch) IN (0,32) THEN batch ELSE NULL END,CASE WHEN length(store_id)=16 THEN store_id ELSE NULL END FROM state WHERE singleton=1",
        [prefix],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
    ).map_err(database_error)?;
    let generation = u64::try_from(generation).map_err(|_| changed())?;
    let batch = batch.ok_or_else(changed)?;
    if store_id.as_deref() != Some(store.store_id.as_slice())
        || (generation == 0 && !batch.is_empty())
        || (generation > 0 && batch.len() != 32)
    {
        return Err(changed());
    }
    Ok((generation, checked_count(count)?, batch))
}

/// Reconstructs the exact latest declaration or atomic tail-deletion batch.
fn verify_abort_batch(prefix: &str, stage: &[u8], abort: &Abort, batch: &[u8]) -> Result<()> {
    if abort.remaining > abort.chunks {
        return Err(changed());
    }
    let stage_key = format!("{prefix}stage");
    let stage_write = CustodyWrite {
        key: &stage_key,
        previous: Some(&abort.previous_stage_digest),
        value: Some(stage),
    };
    let expected = if abort.remaining == abort.chunks {
        if abort.previous_stage_digest != abort.header_digest
            || abort.last_deleted_digest != [0; 32]
        {
            return Err(changed());
        }
        batch_digest(&[stage_write])?
    } else {
        let deleted_key = chunk_key(prefix, usize::from(abort.remaining));
        batch_digest(&[
            stage_write,
            CustodyWrite {
                key: &deleted_key,
                previous: Some(&abort.last_deleted_digest),
                value: None,
            },
        ])?
    };
    if expected != batch {
        return Err(changed());
    }
    Ok(())
}

/// Atomically records the exact deletion preimages and removes one tail row.
fn remove_with_evidence(
    store: &mut MmpCustodyStore,
    prefix: &str,
    abort: &mut Abort,
    generation: &mut u64,
    index: usize,
    stage_length: usize,
) -> Result<()> {
    if usize::from(abort.remaining) != index.checked_add(1).ok_or_else(changed)? {
        return Err(changed());
    }
    let stage_key = format!("{prefix}stage");
    let previous_stage = abort.padded(stage_length);
    if store.read(&stage_key)?.as_deref() != Some(previous_stage.as_slice()) {
        return Err(changed());
    }
    let key = chunk_key(prefix, index);
    let value = store.read(&key)?.ok_or_else(changed)?;
    let stage_digest = Sha256::digest(&previous_stage);
    let deleted_digest = Sha256::digest(&value);
    let mut next = abort.clone();
    next.remaining = u16::try_from(index).map_err(|_| changed())?;
    next.previous_stage_digest = stage_digest.into();
    next.last_deleted_digest = deleted_digest.into();
    let replacement = next.padded(stage_length);
    let committed = store.commit(
        *generation,
        &[
            CustodyWrite {
                key: &stage_key,
                previous: Some(&next.previous_stage_digest),
                value: Some(&replacement),
            },
            CustodyWrite {
                key: &key,
                previous: Some(&next.last_deleted_digest),
                value: None,
            },
        ],
    )?;
    *generation = committed.generation();
    *abort = next;
    Ok(())
}

/// Verifies every retained original chunk and, when provided, the digest of the
/// immediately following deleted chunk. Never materializes the complete source
/// or fills an absent suffix while checking a prefix.
fn verify_prefix(
    store: &MmpCustodyStore,
    prefix: &str,
    record: &MessageAcceptanceWrite,
    chunks: usize,
    deleted_digest: Option<&[u8; 32]>,
) -> Result<()> {
    let mut verifier = Verifier {
        store,
        prefix,
        chunks,
        index: 0,
        buffer: Vec::with_capacity(CHUNK_BYTES),
        deleted_digest,
        deleted_hash: Sha256::new(),
        deleted_bytes: 0,
    };
    serde_json::to_writer(&mut verifier, record).map_err(encoding_error)?;
    verifier.finish()?;
    if verifier.index != chunks
        || verifier.deleted_digest.is_some_and(|expected| {
            verifier.deleted_bytes == 0
                || <[u8; 32]>::from(verifier.deleted_hash.clone().finalize()) != *expected
        })
    {
        return Err(changed());
    }
    Ok(())
}

/// Bounded comparison sink; optionally hashes the first deleted source chunk.
struct Verifier<'a> {
    /// Original serialized store, borrowed without mutation during validation.
    store: &'a MmpCustodyStore,
    /// Canonical internal sequence namespace.
    prefix: &'a str,
    /// Exact dense prefix chunk count proven by current row/generation facts.
    chunks: usize,
    /// Next actual original chunk to compare.
    index: usize,
    /// One bounded64KiB encoded comparison buffer.
    buffer: Vec<u8>,
    /// Expected first deleted source chunk, absent before any deletion.
    deleted_digest: Option<&'a [u8; 32]>,
    /// Streaming hash of at most one original deleted chunk.
    deleted_hash: Sha256,
    /// Number of original deleted bytes absorbed by that hash.
    deleted_bytes: usize,
}
impl Verifier<'_> {
    /// Compares a complete or final partial chunk without ever repairing it.
    fn finish(&mut self) -> Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        if self.index >= self.chunks
            || self
                .store
                .read(&chunk_key(self.prefix, self.index))?
                .as_deref()
                != Some(self.buffer.as_slice())
        {
            return Err(changed());
        }
        self.index += 1;
        self.buffer.clear();
        Ok(())
    }
}
impl Write for Verifier<'_> {
    /// Splits original encoder writes at the same immutable chunk boundaries.
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut remaining = bytes;
        while !remaining.is_empty() && self.index < self.chunks {
            let take = remaining.len().min(CHUNK_BYTES - self.buffer.len());
            self.buffer.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
            if self.buffer.len() == CHUNK_BYTES {
                self.finish().map_err(io::Error::other)?;
            }
        }
        if self.deleted_digest.is_some()
            && self.index == self.chunks
            && self.deleted_bytes < CHUNK_BYTES
        {
            let take = remaining.len().min(CHUNK_BYTES - self.deleted_bytes);
            self.deleted_hash.update(&remaining[..take]);
            self.deleted_bytes += take;
        }
        Ok(bytes.len())
    }
    /// Does not change chunk boundaries or mutate any persisted source.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Rejects unknown/corrupt/stale recovery state without inferring nonexecution.
fn changed() -> MezError {
    MezError::invalid_state("MMP original recovery evidence changed")
}

#[cfg(test)]
mod tests;
