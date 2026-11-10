//! Non-destructive, atomic preparation, not a payload-reclamation protocol.
//!
//! q1 contains one versioned manifest and the already-funded bodyless evidence.
//! Originals and every journal remain unchanged. The live root gates after
//! publication; a cold root may cancel ONLY this preparation, after comparing
//! every byte with the intact originals at its exact allocation/generation.
//! Unknown, partial, changed or excessive preparations remain evidence and fail
//! closed. There is no filesystem staging path, live-mail decoder or replay.

use super::*;
use std::io::{self, Write};

const MANIFEST: &str = "q1:prepare";
const MAGIC: &[u8; 8] = b"MMPPREP1";
const HEADER: usize = 112;
const MAX_SOURCES: usize = 63;
const MAX_EVIDENCE: usize = 128 * 1024;

fn key(sequence: u64) -> String {
    format!("q1:{sequence:020}")
}

fn count(connection: &Connection) -> Result<usize> {
    let n: i64 = connection
        .query_row(
            "SELECT count(*) FROM records WHERE key GLOB 'q1:*'",
            [],
            |r| r.get(0),
        )
        .map_err(database_error)?;
    usize::try_from(n).map_err(|_| invalid())
}

impl OwnedCustodyStore<'_> {
    /// One batch only, with no accepted-source mutation. False is a quantitative
    /// refusal before writes. A successful call requires the caller to gate the
    /// live root; it is NOT a native dependency plan or permission to reclaim.
    pub(crate) fn persist_retirement_preparation(
        &mut self,
        candidate: &SourceRetirementCandidate,
    ) -> Result<bool> {
        self.revalidate()?;
        let reservation = candidate.rewrite.as_ref().ok_or_else(invalid)?;
        let position = self.store_mut().acceptance_plan(0)?;
        if !self.matches_rewrite_reservation(reservation, &position) {
            return Err(invalid());
        }
        if !self.qualify_physical_capacity()? {
            return Ok(false);
        }
        let records = candidate.preserved_sources().ok_or_else(invalid)?;
        if records.is_empty() || records.len() > MAX_SOURCES {
            return Ok(false);
        }
        let bytes = records.iter().try_fold(0usize, |n, r| {
            n.checked_add(r.encoded().len()).ok_or_else(invalid)
        })?;
        // The existing staging framing loan covers this <=616-byte manifest,
        // <=64 borrowed write descriptors and <=63 bounded generated keys.
        if bytes > MAX_EVIDENCE || bytes + HEADER + 8 * records.len() > reservation.bytes() {
            return Ok(false);
        }
        let first = candidate.sources.first().ok_or_else(invalid)?;
        let store = self.store_mut();
        let state = store.current_state()?;
        if first.position.store_id != store.store_id
            || first.position.generation != state.generation as u64
            || first.position.sequence_watermark != state.watermark
            || !first.position.watermark_origin_trusted
            || records.len() != candidate.sources.len()
            || count(&store.connection)? != 0
        {
            return Err(invalid());
        }
        let mut manifest = Vec::with_capacity(HEADER + records.len() * 8);
        manifest.extend_from_slice(MAGIC);
        manifest.extend_from_slice(&store.store_id);
        manifest.extend_from_slice(&(state.generation as u64).to_le_bytes());
        manifest.extend_from_slice(&state.watermark.to_le_bytes());
        let directory = store.directory.metadata()?;
        for number in [
            directory.dev(),
            directory.ino(),
            store.database_identity.device,
            store.database_identity.inode,
        ] {
            manifest.extend_from_slice(&number.to_le_bytes());
        }
        manifest.extend_from_slice(&candidate.commitment());
        manifest.extend_from_slice(&(records.len() as u64).to_le_bytes());
        for (index, record) in records.iter().enumerate() {
            let source = &candidate.sources[index];
            if source.sequence != record.sequence
                || (index > 0 && records[index - 1].sequence >= record.sequence)
                || source.source_evidence.encoded_bytes != record.encoded().len() as u64
                || source.source_evidence.sha256
                    != <[u8; 32]>::from(Sha256::digest(record.encoded()))
            {
                return Err(invalid());
            }
            manifest.extend_from_slice(&record.sequence.to_le_bytes());
        }
        let keys: Vec<_> = records.iter().map(|r| key(r.sequence)).collect();
        let mut writes = Vec::with_capacity(records.len() + 1);
        writes.push(CustodyWrite {
            key: MANIFEST,
            previous: None,
            value: Some(&manifest),
        });
        for (record, key) in records.iter().zip(&keys) {
            writes.push(CustodyWrite {
                key,
                previous: None,
                value: Some(record.encoded()),
            });
        }
        store.commit(state.generation as u64, &writes)?;
        self.revalidate()?;
        Ok(true)
    }

    /// Cold disposition BEFORE strict inventory/checkpoint validation. Only q1
    /// records are removed; all accepted bytes, journals, watermark and clock
    /// origin survive. Validation and exact deletion share one SQL transaction.
    /// A lost cancellation reply is reconciled by observing the absent slot;
    /// malformed or foreign slots never become deletion authority.
    pub(crate) fn cancel_cold_retirement_preparation(&mut self) -> Result<()> {
        self.revalidate()?;
        if count(&self.store_mut().connection)? != 0 && !self.qualify_physical_capacity()? {
            return Err(invalid());
        }
        let store = self.store_mut();
        let transaction =
            rusqlite::Transaction::new_unchecked(&store.connection, TransactionBehavior::Immediate)
                .map_err(database_error)?;
        let state = store.checked_state(&transaction)?;
        let Some(manifest) = read_record(&transaction, MANIFEST)? else {
            if count(&transaction)? != 0 {
                return Err(invalid());
            }
            store.finish_transaction(transaction)?;
            return self.revalidate();
        };
        if manifest.len() < HEADER
            || manifest.len() > HEADER + MAX_SOURCES * 8
            || &manifest[..8] != MAGIC
            || manifest[8..24] != store.store_id
        {
            return Err(invalid());
        }
        let number = |offset: usize| -> Result<u64> {
            Ok(u64::from_le_bytes(
                manifest[offset..offset + 8]
                    .try_into()
                    .map_err(|_| invalid())?,
            ))
        };
        let directory = store.directory.metadata()?;
        let n = usize::try_from(number(104)?).map_err(|_| invalid())?;
        if n == 0
            || n > MAX_SOURCES
            || manifest.len() != HEADER + n * 8
            || number(24)?.checked_add(1) != Some(state.generation as u64)
            || number(32)? != state.watermark
            || number(40)? != directory.dev()
            || number(48)? != directory.ino()
            || number(56)? != store.database_identity.device
            || number(64)? != store.database_identity.inode
            || !state.watermark_origin_trusted
            || count(&transaction)? != n + 1
        {
            return Err(invalid());
        }
        // Only hashes are retained. One evidence row and one bounded original
        // metadata decoder are live at a time; no cumulative payload copy.
        let mut preimages = Vec::with_capacity(n + 1);
        preimages.push((
            MANIFEST.to_owned(),
            <[u8; 32]>::from(Sha256::digest(&manifest)),
        ));
        let mut publication = Sha256::new();
        publication.update(BATCH_DIGEST_DOMAIN);
        hash_batch_write(
            &mut publication,
            &CustodyWrite {
                key: MANIFEST,
                previous: None,
                value: Some(&manifest),
            },
        );
        let mut total = 0usize;
        let mut previous = 0;
        for index in 0..n {
            let sequence = number(HEADER + index * 8)?;
            if sequence <= previous || sequence > state.watermark {
                return Err(invalid());
            }
            previous = sequence;
            let key = key(sequence);
            let evidence = read_record(&transaction, &key)?.ok_or_else(invalid)?;
            total = total.checked_add(evidence.len()).ok_or_else(invalid)?;
            if total > MAX_EVIDENCE {
                return Err(invalid());
            }
            hash_batch_write(
                &mut publication,
                &CustodyWrite {
                    key: &key,
                    previous: None,
                    value: Some(&evidence),
                },
            );
            let limits = mez_agent::messaging::MessageAcceptanceDecodeLimits {
                payload_bytes: 1024 * 1024,
                auxiliary_bytes: 512 * 1024,
                recipient_count: 4096,
                recipient_bytes: 512 * 1024,
            };
            let (metadata, digest, original_bytes) =
                store.read_accepted_metadata_in_snapshot(&transaction, sequence, limits)?;
            let mut compare = Compare {
                expected: &evidence,
                offset: 0,
            };
            metadata
                .write_metadata_evidence(&mut compare, &digest, original_bytes)
                .map_err(|_| invalid())?;
            if compare.offset != evidence.len() {
                return Err(invalid());
            }
            preimages.push((key, <[u8; 32]>::from(Sha256::digest(&evidence))));
        }
        // Generation alone cannot prove the namespace is complete. Require
        // every original publication byte, including descriptive commitment,
        // source set and ordered evidence, before discarding any q1 record.
        if publication.finalize().as_slice() != state.batch.as_slice() {
            return Err(invalid());
        }
        let writes: Vec<_> = preimages
            .iter()
            .map(|(key, digest)| CustodyWrite {
                key,
                previous: Some(digest),
                value: None,
            })
            .collect();
        store.commit_in_transaction(&transaction, state.generation as u64, &writes)?;
        store.finish_transaction(transaction)?;
        self.revalidate()
    }
}

/// Streaming exact comparison avoids retaining a second evidence encoding.
struct Compare<'a> {
    expected: &'a [u8],
    offset: usize,
}
impl Write for Compare<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self
            .offset
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("comparison overflow"))?;
        if self.expected.get(self.offset..end) != Some(bytes) {
            return Err(io::Error::other("preparation differs from original"));
        }
        self.offset = end;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn invalid() -> MezError {
    MezError::invalid_state("MMP durable retirement preparation unavailable")
}
