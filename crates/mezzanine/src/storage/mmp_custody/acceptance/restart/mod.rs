//! Read-only inspection of one interrupted acceptance projection.
//!
//! A restart may no longer hold the original in-memory write or plan. This
//! inspection validates one candidate's bounded stage and dense chunk prefix,
//! and checks that the store's last committed batch matches its final append.
//! In particular, a missing marker is not enough: deleting the marker of a
//! committed record leaves marker-batch evidence which must not be mistaken for
//! an interrupted append. All reads share one SQLite snapshot. The result is
//! descriptive only; it cannot delete rows, establish nonacceptance, restore a
//! live identity, or replace exclusive ownership and full store reconciliation.

use super::*;

/// Bounded local evidence about one marker-free staged occurrence.
pub(crate) struct UnfinishedAcceptanceRef {
    /// Persisted store allocation observed in the same snapshot.
    pub store_id: Vec<u8>,
    /// Generation observed with the stage, chunks, and last batch.
    pub generation: u64,
    /// Original namespace sequence, not a live allocator or receipt.
    pub sequence: u64,
    /// Full encoded source length declared by the original stage.
    pub encoded_bytes: usize,
    /// Declared full-source digest; a partial prefix cannot verify this digest.
    pub source_digest: [u8; 32],
    /// Number of actual checksum-validated consecutive source chunks.
    pub present_chunks: usize,
    /// Number of chunks required by the declared complete source.
    pub total_chunks: usize,
}

impl MmpCustodyStore {
    /// Inspects one marker-free stage under a single read snapshot.
    ///
    /// Returns no recovery authority. Rejects a marker, noncanonical header or
    /// key, missing/corrupt chunks, invalid framing, and last-batch evidence
    /// inconsistent with this exact append. Other store namespaces are not
    /// inventoried by this operation and must be checked before any recovery.
    pub(crate) fn inspect_unfinished_acceptance(
        &self,
        sequence: u64,
    ) -> Result<UnfinishedAcceptanceRef> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let result = self.inspect_unfinished_in_snapshot(&transaction, sequence)?;
        transaction.commit().map_err(database_error)?;
        Ok(result)
    }

    /// Validates the candidate and last-batch state from the same transaction.
    pub(in crate::storage::mmp_custody) fn inspect_unfinished_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        sequence: u64,
    ) -> Result<UnfinishedAcceptanceRef> {
        if sequence == 0 || !std::ptr::eq(&self.connection, &**transaction) {
            return Err(changed());
        }
        let state = self.checked_state(transaction)?;
        let generation = u64::try_from(state.generation).map_err(|_| changed())?;
        let batch = state.batch;
        let store_id = self.store_id.clone();
        let prefix = format!("a1:{sequence:016x}:");
        let stage_key = format!("{prefix}stage");
        let stage = read_record(transaction, &stage_key)?.ok_or_else(changed)?;
        // A genuine v1 header is far smaller than the generic row limit. Check
        // before parsing so malformed nested JSON cannot consume unbounded work.
        if stage.len() > 512 {
            return Err(changed());
        }
        let header: Header = serde_json::from_slice(&stage).map_err(encoding_error)?;
        if header.record_version != RECORD_VERSION
            || header.sequence != sequence
            || header.encoded_bytes == 0
            || header.encoded_bytes > self.limits.logical_bytes
            || header.chunks != header.encoded_bytes.div_ceil(CHUNK_BYTES)
            || serde_json::to_vec(&header).map_err(encoding_error)? != stage
        {
            return Err(changed());
        }
        let mut present_chunks = 0usize;
        let mut saw_stage = false;
        let mut hash = Sha256::new();
        let mut last_batch = batch_digest(&[CustodyWrite {
            key: &stage_key,
            previous: None,
            value: Some(&stage),
        }])?;
        {
            let mut statement = transaction.prepare(
                "SELECT CASE WHEN length(CAST(key AS BLOB))<=256 THEN key ELSE NULL END FROM records WHERE substr(key,1,length(?1))=?1 ORDER BY key COLLATE BINARY",
            ).map_err(database_error)?;
            let mut rows = statement.query([&prefix]).map_err(database_error)?;
            while let Some(row) = rows.next().map_err(database_error)? {
                let key: Option<String> = row.get(0).map_err(database_error)?;
                let key = key.ok_or_else(changed)?;
                if key == stage_key && !saw_stage {
                    saw_stage = true;
                    continue;
                }
                if saw_stage
                    || present_chunks >= header.chunks
                    || key != chunk_key(&prefix, present_chunks)
                {
                    return Err(changed());
                }
                let chunk = read_record(transaction, &key)?.ok_or_else(changed)?;
                let offset = present_chunks
                    .checked_mul(CHUNK_BYTES)
                    .ok_or_else(changed)?;
                let expected = header
                    .encoded_bytes
                    .checked_sub(offset)
                    .ok_or_else(changed)?
                    .min(CHUNK_BYTES);
                if chunk.len() != expected {
                    return Err(changed());
                }
                hash.update(&chunk);
                last_batch = batch_digest(&[CustodyWrite {
                    key: &key,
                    previous: None,
                    value: Some(&chunk),
                }])?;
                present_chunks += 1;
            }
        }
        if !saw_stage
            || generation < (present_chunks as u64).saturating_add(1)
            || batch != last_batch
            || (present_chunks == header.chunks
                && <[u8; 32]>::from(hash.finalize()) != header.digest)
        {
            return Err(changed());
        }
        Ok(UnfinishedAcceptanceRef {
            store_id,
            generation,
            sequence,
            encoded_bytes: header.encoded_bytes,
            source_digest: header.digest,
            present_chunks,
            total_chunks: header.chunks,
        })
    }
}

/// Keeps diagnostics bounded and free of message contents and identities.
fn changed() -> MezError {
    MezError::invalid_state("MMP unfinished acceptance evidence changed")
}

#[cfg(test)]
mod tests;
