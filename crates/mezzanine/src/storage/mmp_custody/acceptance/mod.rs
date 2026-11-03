//! Streaming immutable acceptance records through bounded SQLite transactions.
//!
//! One original plan binds the existing private store allocation, sequence and
//! starting generation. The caller retains this payload-free plan through worker
//! loss and must settle it before issuing any later work to the same store.
//! Stage/chunk rows are private, unpublished custody; only the final marker means
//! the complete original committed. Reconciliation checks actual bytes and the
//! generation/row prefix before writing, so missing committed rows are corruption
//! rather than permission to reconstruct mail. This is a storage port, not a
//! mailbox or a runtime ingress/restart implementation.

use std::io::{self, Read, Write};

use mez_agent::messaging::{
    MessageAcceptanceDecodeLimits, MessageAcceptanceSource, MessageAcceptanceStore,
    MessageAcceptanceStoreError, MessageAcceptanceWrite, MessageError, decode_message_acceptance,
};
use serde::{Deserialize, Serialize};

use super::*;

mod recovery;
mod restart;

/// Examines a partial abort in an existing verified store snapshot.
pub(super) fn inspect_partial_abort(
    store: &MmpCustodyStore,
    transaction: &rusqlite::Transaction<'_>,
    sequence: u64,
    generation: u64,
    batch: &[u8],
    present_chunks: usize,
    stage: &[u8],
) -> Result<(usize, usize)> {
    recovery::inspect_partial_abort(
        store,
        transaction,
        sequence,
        generation,
        batch,
        present_chunks,
        stage,
    )
}

/// Recognizes and checks only a fully cleaned abort disposition.
pub(super) fn validate_finalized_abort(
    bytes: &[u8],
    sequence: u64,
    generation: u64,
    logical_bytes: usize,
) -> Result<()> {
    recovery::validate_finalized_abort(bytes, sequence, generation, logical_bytes)
}

/// Keeps encoded buffers and individual transactions below the generic batch cap.
const CHUNK_BYTES: usize = 64 * 1024;
/// Row format version independent of SQLite schema1 and admission input version1.
const RECORD_VERSION: u32 = 1;

/// Payload-free original destination and serial position retained by the owner.
#[derive(Clone, Debug)]
pub(crate) struct AcceptancePlan {
    /// Persistent allocation identity, never inferred from equal session text.
    store_id: Vec<u8>,
    /// Generation before the first private stage record can be written.
    generation: u64,
    /// Original reserved canonical occurrence, never a regenerated message ID.
    sequence: u64,
}

impl AcceptancePlan {
    /// Checks cached allocation identity without I/O; equal labels are insufficient.
    pub(crate) fn matches_store(&self, store: &MmpCustodyStore) -> bool {
        self.store_id == store.store_id
    }

    /// Converts only an idle settled root position to the next ACK transaction.
    /// Unsettled jobs must retain their original plan instead of calling this.
    pub(crate) fn for_acknowledgement(&self) -> super::acknowledgement::AcknowledgementPlan {
        super::acknowledgement::AcknowledgementPlan::from_position(
            self.store_id.clone(),
            self.generation,
        )
    }

    /// Binds an idle root's already-observed store position to its next occurrence.
    /// The root may call this only before issuance, with no unsettled store work.
    pub(crate) fn for_sequence(&self, sequence: u64) -> Self {
        Self {
            store_id: self.store_id.clone(),
            generation: self.generation,
            sequence,
        }
    }
}

/// Complete immutable content identity stored both before and after chunk writes.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    /// Rejects reinterpretation as another private chunk format.
    record_version: u32,
    /// Frozen lower occurrence identity.
    sequence: u64,
    /// Complete encoded input size, including escaped metadata and payload.
    encoded_bytes: usize,
    /// Exact number of bounded chunks in canonical order.
    chunks: usize,
    /// SHA256 over the full original serialized admission record.
    digest: [u8; 32],
}

/// One bounded cursor over chunks in the caller's open SQLite read snapshot.
/// The cursor never returns a complete-record buffer or a verification fact.
struct AcceptedChunks<'a> {
    store: &'a MmpCustodyStore,
    prefix: String,
    next: usize,
    chunks: usize,
    chunk: Vec<u8>,
    offset: usize,
    remaining: usize,
    hash: Sha256,
}

impl Read for AcceptedChunks<'_> {
    /// Loads and checks at most one original chunk before supplying bytes.
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.remaining == 0 {
            return Ok(0);
        }
        if self.offset == self.chunk.len() {
            if self.next >= self.chunks {
                return Err(io::Error::other("MMP acceptance source chunk missing"));
            }
            self.chunk = self
                .store
                .read(&chunk_key(&self.prefix, self.next))
                .map_err(io::Error::other)?
                .ok_or_else(|| io::Error::other("MMP acceptance source chunk missing"))?;
            if self.chunk.len() != self.remaining.min(CHUNK_BYTES) {
                return Err(io::Error::other("MMP acceptance chunk framing changed"));
            }
            self.next += 1;
            self.offset = 0;
        }
        let count = output.len().min(self.chunk.len() - self.offset);
        let bytes = &self.chunk[self.offset..self.offset + count];
        output[..count].copy_from_slice(bytes);
        self.hash.update(bytes);
        self.offset += count;
        self.remaining -= count;
        Ok(count)
    }
}

/// Trusted synchronous port borrowing the one serialized existing store.
pub(crate) struct PlannedAcceptanceStore<'a> {
    /// Concrete private connection; no path comes from envelope data.
    store: &'a mut MmpCustodyStore,
    /// Original actor/root-retained plan, unchanged after ambiguous completion.
    plan: &'a AcceptancePlan,
}

impl MmpCustodyStore {
    /// Decodes one quarantined original only after its complete source verifies.
    ///
    /// The supplied limits must cover the policy under which this store accepted
    /// the original. Neither a verified record nor matching labels restore any
    /// registration, native input, acknowledgment or delivery authority.
    pub(crate) fn read_accepted_source(
        &self,
        sequence: u64,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<(MessageAcceptanceSource, [u8; 32], usize)> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let result = self.read_accepted_source_in_snapshot(&transaction, sequence, limits)?;
        transaction.commit().map_err(database_error)?;
        Ok(result)
    }

    /// Decodes an original inside the caller's existing same-store snapshot.
    /// The caller must keep the result private until its entire read transaction
    /// and every cross-record validation have succeeded.
    pub(super) fn read_accepted_source_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        sequence: u64,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<(MessageAcceptanceSource, [u8; 32], usize)> {
        self.verified_accepted_in_snapshot(transaction, sequence, |reader| {
            let source = decode_message_acceptance(reader, limits).map_err(|error| {
                MezError::invalid_state(format!("MMP acceptance decode failed: {error}"))
            })?;
            if source.sequence() != sequence {
                return Err(MezError::invalid_state(
                    "MMP acceptance record sequence disagrees with its committed marker",
                ));
            }
            Ok(source)
        })
    }

    /// Verifies the actual complete accepted original before durable ACK work.
    /// Marker labels alone are insufficient: every bounded chunk, its framing
    /// and complete SHA256 must still agree. Unknown formats fail closed.
    pub(super) fn validate_accepted_occurrence(&self, sequence: u64) -> Result<[u8; 32]> {
        self.stream_accepted_occurrence(sequence, &mut io::sink())
            .map(|(digest, _)| digest)
    }

    /// Streams one committed immutable acceptance under one SQLite read snapshot.
    ///
    /// The private marker, original stage, each bounded chunk and the full digest
    /// are checked against the same database snapshot. The sink may receive an
    /// incomplete prefix before an error; its caller must quarantine that output
    /// and discard it unless this method returns the verified digest and byte
    /// count. This is source material for a future restore fold, not a mailbox,
    /// acceptance, recipient registration or application-commit authority.
    /// Reads and sink writes are synchronous and belong on the storage worker.
    pub(crate) fn stream_accepted_occurrence(
        &self,
        sequence: u64,
        sink: &mut impl Write,
    ) -> Result<([u8; 32], usize)> {
        let (_, digest, bytes) = self.with_verified_accepted_occurrence(sequence, |reader| {
            io::copy(reader, sink).map_err(|error| {
                MezError::invalid_state(format!("MMP acceptance stream failed: {error}"))
            })?;
            Ok(())
        })?;
        Ok((digest, bytes))
    }

    /// Runs a bounded decoder over one committed source in a consistent snapshot.
    ///
    /// The decoder must consume the entire stream and must not publish side
    /// effects while it runs. Its private result is returned only after the
    /// marker, all chunks and the final digest verify. Decoder errors, early
    /// return and source corruption discard the result and grant no authority.
    /// The decoder and database reads are synchronous storage-worker work.
    pub(crate) fn with_verified_accepted_occurrence<T>(
        &self,
        sequence: u64,
        decode: impl FnOnce(&mut dyn Read) -> Result<T>,
    ) -> Result<(T, [u8; 32], usize)> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let result = self.verified_accepted_in_snapshot(&transaction, sequence, decode)?;
        transaction.commit().map_err(database_error)?;
        Ok(result)
    }

    /// Validates one source against the caller's already-open store snapshot.
    /// The caller retains and commits the transaction before publishing results.
    fn verified_accepted_in_snapshot<T>(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        sequence: u64,
        decode: impl FnOnce(&mut dyn Read) -> Result<T>,
    ) -> Result<(T, [u8; 32], usize)> {
        if !std::ptr::eq(&self.connection, &**transaction) {
            return Err(MezError::invalid_state(
                "MMP acceptance snapshot belongs to another store",
            ));
        }
        let prefix = format!("a1:{sequence:016x}:");
        let marker = self
            .read(&format!("{prefix}commit"))?
            .ok_or_else(|| MezError::invalid_state("MMP original acceptance is unavailable"))?;
        let header: Header = serde_json::from_slice(&marker).map_err(encoding_error)?;
        if header.record_version != RECORD_VERSION
            || header.sequence != sequence
            || header.encoded_bytes == 0
            || header.encoded_bytes > self.limits.logical_bytes
            || header.chunks != header.encoded_bytes.div_ceil(CHUNK_BYTES)
            || self.read(&format!("{prefix}stage"))?.as_deref() != Some(marker.as_slice())
        {
            return Err(MezError::invalid_state(
                "MMP original acceptance framing changed",
            ));
        }
        let mut reader = AcceptedChunks {
            store: self,
            prefix,
            next: 0,
            chunks: header.chunks,
            chunk: Vec::new(),
            offset: 0,
            remaining: header.encoded_bytes,
            hash: Sha256::new(),
        };
        let decoded = decode(&mut reader)?;
        if reader.remaining != 0 || reader.next != header.chunks {
            return Err(MezError::invalid_state(
                "MMP acceptance decoder did not consume the complete source",
            ));
        }
        if <[u8; 32]>::from(reader.hash.finalize()) != header.digest {
            return Err(MezError::invalid_state(
                "MMP original acceptance digest changed",
            ));
        }
        Ok((decoded, header.digest, header.encoded_bytes))
    }

    /// Captures a plan before issuance, on the owned I/O worker. The runtime root
    /// must retain it before handing out an attempt and reuse it after worker loss.
    pub fn acceptance_plan(&self, sequence: u64) -> Result<AcceptancePlan> {
        let generation: i64 = self
            .connection
            .query_row(
                "SELECT generation FROM state WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        Ok(AcceptancePlan {
            store_id: self.store_id.clone(),
            generation: u64::try_from(generation)
                .map_err(|_| MezError::invalid_state("invalid MMP acceptance generation"))?,
            sequence,
        })
    }

    /// Borrows a port for one original plan; actual allocation and sequence checks
    /// precede every write. Reopening a same-label replacement is not authority.
    pub fn acceptance_port<'a>(
        &'a mut self,
        plan: &'a AcceptancePlan,
    ) -> PlannedAcceptanceStore<'a> {
        PlannedAcceptanceStore { store: self, plan }
    }
}

impl MessageAcceptanceStore for PlannedAcceptanceStore<'_> {
    /// Returns success only after exact full-record marker commit/reconciliation.
    /// All failures remain Unknown: a diagnostic alone never proves noncommit.
    fn commit(
        &mut self,
        record: &MessageAcceptanceWrite,
    ) -> std::result::Result<(), MessageAcceptanceStoreError> {
        self.commit_record(record).map_err(|error| {
            MessageAcceptanceStoreError::Unknown(MessageError::invalid_state(error.to_string()))
        })
    }
}

impl PlannedAcceptanceStore<'_> {
    /// Preflights the full finite projection, then streams and commits its marker.
    fn commit_record(&mut self, record: &MessageAcceptanceWrite) -> Result<()> {
        if self.plan.store_id != self.store.store_id || self.plan.sequence != record.sequence() {
            return Err(MezError::conflict(
                "MMP acceptance destination or occurrence changed",
            ));
        }
        let (header, header_bytes) = record_header(record, self.store.limits.logical_bytes)?;
        let prefix = format!("a1:{:016x}:", self.plan.sequence);
        let stage_key = format!("{prefix}stage");
        let commit_key = format!("{prefix}commit");
        let (generation, count, own_bytes, all_count, all_bytes): (i64, i64, i64, i64, i64) = self.store.connection.query_row(
            "SELECT generation, (SELECT COUNT(*) FROM records WHERE substr(key,1,length(?1))=?1), (SELECT COALESCE(SUM(length(CAST(key AS BLOB))+length(value)+length(digest)+64),0) FROM records WHERE substr(key,1,length(?1))=?1), (SELECT COUNT(*) FROM records), (SELECT COALESCE(SUM(length(CAST(key AS BLOB))+length(value)+length(digest)+64),0) FROM records) FROM state WHERE singleton=1",
            [&prefix], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
        ).map_err(database_error)?;
        let generation = u64::try_from(generation)
            .map_err(|_| MezError::invalid_state("invalid MMP acceptance generation"))?;
        let count = checked_count(count)?;
        let own_bytes = checked_count(own_bytes)?;
        let all_count = checked_count(all_count)?;
        let all_bytes = checked_count(all_bytes)?;
        // Every original row is one transaction. Before any other work may run,
        // an absent row or an unrelated write necessarily violates this equality.
        let expected = self
            .plan
            .generation
            .checked_add(count as u64)
            .ok_or_else(|| MezError::invalid_state("MMP acceptance generation overflow"))?;
        if self
            .plan
            .generation
            .checked_add((2 * header.chunks + 2) as u64)
            .is_none_or(|last| last > i64::MAX as u64)
        {
            return Err(MezError::invalid_state(
                "MMP acceptance generation capacity",
            ));
        }
        if generation != expected || count > header.chunks + 2 {
            return Err(MezError::invalid_state(
                "MMP acceptance storage prefix changed",
            ));
        }
        let required_bytes = header.encoded_bytes
            + (stage_key.len() + commit_key.len() + 2 * header_bytes.len())
            + 2 * (32 + ROW_METADATA_BYTES)
            + (0..header.chunks)
                .map(|index| chunk_key(&prefix, index).len() + 32 + ROW_METADATA_BYTES)
                .sum::<usize>();
        if all_count - count + header.chunks + 2 > self.store.limits.records
            || all_bytes - own_bytes + required_bytes > self.store.limits.logical_bytes
        {
            return Err(MezError::invalid_state(
                "MMP acceptance full-record storage capacity",
            ));
        }
        let committed = self.store.read(&commit_key)?;
        if committed
            .as_ref()
            .is_some_and(|bytes| bytes != &header_bytes)
        {
            return Err(MezError::conflict(
                "MMP acceptance committed identity mismatch",
            ));
        }
        let staged = self.store.read(&stage_key)?;
        if staged.as_ref().is_some_and(|bytes| bytes != &header_bytes)
            || (staged.is_none() && count != 0)
        {
            return Err(MezError::invalid_state(
                "MMP acceptance staged identity missing or changed",
            ));
        }
        // Validate a dense immutable prefix before filling any absent suffix.
        // Missing committed chunks never become permission to rebuild custody.
        let existing_chunks =
            count.saturating_sub(usize::from(staged.is_some()) + usize::from(committed.is_some()));
        if committed.is_some() && existing_chunks != header.chunks {
            return Err(MezError::invalid_state(
                "MMP acceptance committed chunks missing",
            ));
        }
        for index in 0..existing_chunks {
            if self.store.read(&chunk_key(&prefix, index))?.is_none() {
                return Err(MezError::invalid_state(
                    "MMP acceptance chunk prefix has a hole",
                ));
            }
        }
        let mut next_generation = generation;
        if staged.is_none() {
            append(self.store, &mut next_generation, &stage_key, &header_bytes)?;
        }
        let mut writer = ChunkWriter {
            store: self.store,
            prefix: &prefix,
            generation: next_generation,
            existing_chunks,
            committed: committed.is_some(),
            index: 0,
            buffer: Vec::with_capacity(CHUNK_BYTES),
            hash: Sha256::new(),
            bytes: 0,
        };
        serde_json::to_writer(&mut writer, record).map_err(encoding_error)?;
        writer.finish_chunk()?;
        if writer.bytes != header.encoded_bytes
            || writer.index != header.chunks
            || <[u8; 32]>::from(writer.hash.clone().finalize()) != header.digest
        {
            return Err(MezError::invalid_state(
                "MMP acceptance encoded original changed",
            ));
        }
        next_generation = writer.generation;
        if committed.is_none() {
            append(
                writer.store,
                &mut next_generation,
                &commit_key,
                &header_bytes,
            )?;
        }
        Ok(())
    }
}

/// Computes original framing/checksum without retaining a complete encoded copy.
fn record_header(record: &MessageAcceptanceWrite, maximum: usize) -> Result<(Header, Vec<u8>)> {
    let mut counter = Counter {
        bytes: 0,
        maximum,
        hash: Sha256::new(),
    };
    serde_json::to_writer(&mut counter, record).map_err(encoding_error)?;
    let header = Header {
        record_version: RECORD_VERSION,
        sequence: record.sequence(),
        encoded_bytes: counter.bytes,
        chunks: counter.bytes.div_ceil(CHUNK_BYTES),
        digest: counter.hash.finalize().into(),
    };
    let bytes = serde_json::to_vec(&header).map_err(encoding_error)?;
    // The abort descriptor must fit the original header cell at the same length.
    if bytes.len() < recovery::ABORT_BYTES {
        return Err(MezError::invalid_state(
            "MMP acceptance control framing capacity",
        ));
    }
    Ok((header, bytes))
}

/// Counts/hashes the original without allocating an encoded copy before preflight.
struct Counter {
    /// Checked total serialized bytes, including escaping.
    bytes: usize,
    /// Trusted total store limit, never enlarged to fit a record.
    maximum: usize,
    /// Incremental full-record checksum.
    hash: Sha256,
}
impl Write for Counter {
    /// Consumes only a bounded logical stream; oversized input stops immediately.
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self
            .bytes
            .checked_add(bytes.len())
            .filter(|next| *next <= self.maximum)
            .ok_or_else(|| io::Error::other("MMP acceptance encoding exceeds store policy"))?;
        self.bytes = next;
        self.hash.update(bytes);
        Ok(bytes.len())
    }
    /// Performs no I/O and retains no additional buffers.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Streaming second pass retaining one chunk plus a bounded verified read buffer.
struct ChunkWriter<'a> {
    /// Existing serialized store, never a second delivery owner.
    store: &'a mut MmpCustodyStore,
    /// Fixed internal sequence namespace, not user-controlled path text.
    prefix: &'a str,
    /// Next expected generation for exact fresh suffix appends.
    generation: u64,
    /// Number of verified dense original chunks already present.
    existing_chunks: usize,
    /// Committed originals can only be validated, never reconstructed.
    committed: bool,
    /// Next chunk's monotonic index.
    index: usize,
    /// Bounded64KiB encoded data buffer.
    buffer: Vec<u8>,
    /// Full immutable content checksum across arbitrary serializer write sizes.
    hash: Sha256,
    /// Complete encoded count for second-pass consistency.
    bytes: usize,
}
impl ChunkWriter<'_> {
    /// Reconciles one immutable chunk or appends one bounded fresh transaction.
    fn finish_chunk(&mut self) -> Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let key = chunk_key(self.prefix, self.index);
        match self.store.read(&key)? {
            Some(bytes) if bytes == self.buffer => (),
            Some(_) => return Err(MezError::conflict("MMP acceptance chunk content changed")),
            None if self.committed || self.index < self.existing_chunks => {
                return Err(MezError::invalid_state(
                    "MMP acceptance original chunk missing",
                ));
            }
            None => append(self.store, &mut self.generation, &key, &self.buffer)?,
        }
        self.index += 1;
        self.buffer.clear();
        Ok(())
    }
}
impl Write for ChunkWriter<'_> {
    /// Splits even a single large serializer call without allocating its length.
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut remaining = bytes;
        while !remaining.is_empty() {
            let take = remaining.len().min(CHUNK_BYTES - self.buffer.len());
            self.buffer.extend_from_slice(&remaining[..take]);
            self.hash.update(&remaining[..take]);
            self.bytes += take;
            remaining = &remaining[take..];
            if self.buffer.len() == CHUNK_BYTES {
                self.finish_chunk().map_err(io::Error::other)?;
            }
        }
        Ok(bytes.len())
    }
    /// Flushes a partial final chunk; does not publish the occurrence marker.
    fn flush(&mut self) -> io::Result<()> {
        self.finish_chunk().map_err(io::Error::other)
    }
}

/// Rejects malformed negative/overflowed SQL counters before arithmetic or writes.
fn checked_count(value: i64) -> Result<usize> {
    usize::try_from(value)
        .map_err(|_| MezError::invalid_state("invalid MMP acceptance storage counters"))
}

/// Uses fixed-width internal keys independent of envelope ID length or content.
fn chunk_key(prefix: &str, index: usize) -> String {
    format!("{prefix}chunk:{index:08x}")
}

/// Adds one exact absent immutable row within the generic64-record/256KiB cap.
fn append(
    store: &mut MmpCustodyStore,
    generation: &mut u64,
    key: &str,
    bytes: &[u8],
) -> Result<()> {
    let commit = store.commit(
        *generation,
        &[CustodyWrite {
            key,
            previous: None,
            value: Some(bytes),
        }],
    )?;
    *generation = commit.generation();
    Ok(())
}

/// Keeps serialization and I/O diagnostics contextual without classifying absence.
fn encoding_error(error: serde_json::Error) -> MezError {
    MezError::invalid_state(format!("MMP acceptance encoding/storage: {error}"))
}

#[cfg(test)]
mod test_fixture;
#[cfg(test)]
mod tests;
