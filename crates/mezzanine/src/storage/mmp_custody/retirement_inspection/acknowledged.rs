//! Atomic positive-ACK-only payload retirement with preserved native proofs.
//!
//! The original lower absence loan and complete inspection remain live through
//! one transaction. Every original chunk/marker preimage is deleted together
//! with installation of a physically bound bodyless record. Journals are never
//! deleted. Unfinished native/terminal and oversized cases refuse; no intent or q1
//! cancellation authority is reused. Readers cannot turn this record into mail.

use super::*;
use mez_agent::messaging::{
    MessageAcceptanceDecodeLimits, MessageAcceptanceMetadata, MessageSourcePreservationReservation,
    decode_message_acceptance_metadata_evidence,
};

const MAGIC: &[u8; 8] = b"MMPRET01";
const NATIVE_MAGIC: &[u8; 8] = b"MMPRET02";
const HEADER: usize = 80;
const MAX_RETIRED_RECORD_BYTES: usize = 128 * 1024 + HEADER;
// Includes <=64 bounded keys, digest vectors and CustodyWrite descriptors that
// coexist with the encoded row. Source decoder/slot scratch is already loaned.
const WRITE_INDEX_BYTES: usize = 32 * 1024;
pub(crate) const WRITE_BUFFER_BYTES: usize = MAX_RETIRED_RECORD_BYTES + WRITE_INDEX_BYTES;

fn key(sequence: u64) -> String {
    format!("a1:{sequence:016x}:retired")
}

fn non_native(source: &MessageAcceptanceMetadata) -> bool {
    source.has_native_binding_record()
        && source.native_sender().is_none()
        && (0..source.recipient_bindings().len()).all(|i| source.native_recipient(i).is_none())
}

/// Matches native ACKs to the original canonical selection clock. The source
/// metadata has been dropped before this scan, so source-qualified ACK readers
/// do not allocate a second simultaneous source. The <=4096 fixed slot vector
/// and seen bits use charged slot scratch (or cold recovery's decoder scratch)
/// and die before postinspection. Inventory has already bounded the key set.
pub(in crate::storage::mmp_custody) fn native_acks_match(
    store: &MmpCustodyStore,
    transaction: &rusqlite::Transaction<'_>,
    sequence: u64,
    clocks: &[Option<u64>],
    limits: MessageAcceptanceDecodeLimits,
) -> Result<bool> {
    let mut seen = vec![false; clocks.len()];
    let mut statement = transaction
        .prepare("SELECT key FROM records WHERE key GLOB 'k1:*' ORDER BY key")
        .map_err(database_error)?;
    let mut rows = statement.query([]).map_err(database_error)?;
    while let Some(row) = rows.next().map_err(database_error)? {
        let key: String = row.get(0).map_err(database_error)?;
        let ack = store.read_stored_acknowledgement_in_snapshot(transaction, &key, limits)?;
        for decision in ack.decisions.iter().filter(|d| d.sequence == sequence) {
            if !decision.acknowledged {
                return Ok(false);
            }
            if let Some(Some(expected)) = clocks.get(decision.binding_index as usize) {
                if decision.offered_at_ms != Some(*expected) {
                    return Ok(false);
                }
                seen[decision.binding_index as usize] = true;
            }
        }
    }
    Ok(clocks
        .iter()
        .zip(seen)
        .all(|(clock, seen)| clock.is_none() || seen))
}

/// Exact preservation commitment for every non-target row, not merely rows in
/// the selected dependency closure. Hash actual checksum-verified bytes one row
/// at a time in binary key order. The existing source decoder scratch funds the
/// <=256-KiB row; no growing key/digest collection or second inventory is kept.
/// This also detects valid-checksum edits, inserted rows and whole-group removal.
fn preserved_rows(
    store: &MmpCustodyStore,
    transaction: &rusqlite::Transaction<'_>,
    excluded: &str,
) -> Result<[u8; 32]> {
    let mut query = transaction.prepare(
        "SELECT CASE WHEN length(CAST(key AS BLOB))<=256 THEN key ELSE NULL END FROM records WHERE key NOT GLOB ?1 ORDER BY key COLLATE BINARY",
    ).map_err(database_error)?;
    let mut rows = query.query([excluded]).map_err(database_error)?;
    let mut hash = Sha256::new();
    hash.update(b"mezzanine-mmp-reclamation-preserved-rows-v1\0");
    let mut count = 0usize;
    while let Some(row) = rows.next().map_err(database_error)? {
        count = count.checked_add(1).ok_or_else(unavailable)?;
        if count > store.limits.records {
            return Err(unavailable());
        }
        let key: Option<String> = row.get(0).map_err(database_error)?;
        let key = key.ok_or_else(unavailable)?;
        let value = read_record(transaction, &key)?.ok_or_else(unavailable)?;
        hash.update((key.len() as u64).to_le_bytes());
        hash.update(key.as_bytes());
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(&value);
    }
    hash.update((count as u64).to_le_bytes());
    Ok(hash.finalize().into())
}

impl OwnedCustodyStore<'_> {
    /// Reclaims exactly one absent original only if all its recipients have
    /// positive ACKs, every native input has an exact committed journal, and every
    /// mutation fits the existing batch and reserved encoding/rewrite budgets.
    /// False changes no rows. Errors retain the original or atomic outcome for
    /// exact reconciliation; no source/action/provider work may be replayed.
    pub(crate) fn reclaim_acknowledged_source(
        &mut self,
        candidate: &SourceRetirementCandidate,
        sequence: u64,
        encoding: &MessageSourcePreservationReservation,
    ) -> Result<bool> {
        self.revalidate()?;
        let reservation = candidate.rewrite.as_ref().ok_or_else(unavailable)?;
        let plan = self.store_mut().acceptance_plan(0)?;
        if !self.matches_rewrite_reservation(reservation, &plan)
            || !encoding.belongs_to(&candidate._input)
            || !candidate._input.has_no_live_source(sequence)
        {
            return Err(unavailable());
        }
        if !self.qualify_physical_capacity()? {
            return Ok(false);
        }
        let source = candidate
            .sources
            .iter()
            .find(|s| s.sequence == sequence)
            .ok_or_else(unavailable)?;
        if source.acknowledged_recipients != source.recipient_count
            || source.pending_native_journal_references != 0
            || source.unsettled_recipients != 0
        {
            return Ok(false);
        }
        let evidence = candidate
            .preserved_sources()
            .and_then(|records| records.iter().find(|r| r.sequence == sequence))
            .ok_or_else(unavailable)?;
        let bytes = HEADER
            .checked_add(evidence.encoded().len())
            .ok_or_else(unavailable)?;
        if bytes > MAX_RETIRED_RECORD_BYTES
            || bytes.saturating_add(WRITE_INDEX_BYTES) > encoding.encoded_bytes_limit()
            || bytes > reservation.bytes()
        {
            return Ok(false);
        }
        let store = self.store_mut();
        let transaction =
            rusqlite::Transaction::new_unchecked(&store.connection, TransactionBehavior::Immediate)
                .map_err(database_error)?;
        let state = store.checked_state(&transaction)?;
        if state.generation as u64 != source.position.generation
            || source.position.store_id != store.store_id
            || state.watermark != source.position.sequence_watermark
            || !state.watermark_origin_trusted
        {
            return Err(unavailable());
        }
        if store
            .read_retired_metadata_in_snapshot(
                &transaction,
                sequence,
                candidate._input.decode_limits(),
            )?
            .is_some()
        {
            return Ok(false);
        }
        let (metadata, digest, _) = store.read_accepted_metadata_in_snapshot(
            &transaction,
            sequence,
            candidate._input.decode_limits(),
        )?;
        if digest != source.source_sha256 || !metadata.has_native_binding_record() {
            return Ok(false);
        }
        let native = !non_native(&metadata);
        let native_count = (0..metadata.recipient_bindings().len())
            .filter(|index| metadata.native_recipient(*index).is_some())
            .count();
        if source.native_journal_references != native_count {
            return Ok(false);
        }
        let clocks = if native {
            let Some(clocks) = store.native_retirement_clocks(&transaction, &metadata, digest)?
            else {
                return Ok(false);
            };
            Some(clocks)
        } else {
            None
        };
        drop(metadata);
        if let Some(clocks) = clocks
            && !native_acks_match(
                store,
                &transaction,
                sequence,
                &clocks,
                candidate._input.decode_limits(),
            )?
        {
            return Ok(false);
        }
        let prefix = format!("a1:{sequence:016x}:*");
        // Only fixed-size keys and checked digests are retained, never another
        // original payload collection. A larger source is refused before writes.
        let mut preimages = Vec::with_capacity(MAX_BATCH_RECORDS);
        let mut query = transaction.prepare(
            "SELECT key,CASE WHEN length(digest)=32 THEN digest ELSE NULL END FROM records WHERE key GLOB ?1 ORDER BY key LIMIT 64",
        ).map_err(database_error)?;
        let mut rows = query.query([&prefix]).map_err(database_error)?;
        while let Some(row) = rows.next().map_err(database_error)? {
            let key: String = row.get(0).map_err(database_error)?;
            let digest: Option<Vec<u8>> = row.get(1).map_err(database_error)?;
            preimages.push((key, digest.ok_or_else(unavailable)?));
        }
        drop(rows);
        drop(query);
        if preimages.len() + 1 > MAX_BATCH_RECORDS {
            return Ok(false);
        }
        let measured = preimages.iter().try_fold(
            bytes + key(sequence).len() + ROW_METADATA_BYTES,
            |n, (key, _)| {
                n.checked_add(key.len() + 32 + ROW_METADATA_BYTES)
                    .ok_or_else(unavailable)
            },
        )?;
        if measured > MAX_BATCH_BYTES {
            return Ok(false);
        }
        let preserved = preserved_rows(store, &transaction, &prefix)?;
        let mut value = Vec::with_capacity(bytes);
        value.extend_from_slice(if native { NATIVE_MAGIC } else { MAGIC });
        value.extend_from_slice(&store.store_id);
        value.extend_from_slice(&(state.generation as u64 + 1).to_le_bytes());
        value.extend_from_slice(&state.watermark.to_le_bytes());
        let directory = store.directory.metadata()?;
        for n in [
            directory.dev(),
            directory.ino(),
            store.database_identity.device,
            store.database_identity.inode,
        ] {
            value.extend_from_slice(&n.to_le_bytes());
        }
        value.extend_from_slice(&(evidence.encoded().len() as u64).to_le_bytes());
        value.extend_from_slice(evidence.encoded());
        let retired_key = key(sequence);
        let mut writes: Vec<_> = preimages
            .iter()
            .map(|(key, digest)| CustodyWrite {
                key,
                previous: Some(digest),
                value: None,
            })
            .collect();
        writes.push(CustodyWrite {
            key: &retired_key,
            previous: None,
            value: Some(&value),
        });
        store.commit_in_transaction(&transaction, state.generation as u64, &writes)?;
        // Triggers run inside the transaction too. Revalidate the installed
        // representation and every original dependency before losing rollback.
        let after = inspect_in_snapshot(
            store,
            &transaction,
            &candidate._input,
            sequence,
            &mut |_| Ok(()),
            None,
        )?;
        if after.source_sha256 != source.source_sha256
            || after.source_evidence.sha256 != source.source_evidence.sha256
            || after.source_evidence.encoded_bytes != source.source_evidence.encoded_bytes
            || after.recipient_count != source.recipient_count
            || after.acknowledged_recipients != after.recipient_count
            || after.unsettled_recipients != 0
            || after.native_journal_references != source.native_journal_references
            || after.pending_native_journal_references != 0
            || after.dependencies != source.dependencies
            || after.preserved_registrations != source.preserved_registrations
            || preserved_rows(store, &transaction, &prefix)? != preserved
        {
            return Err(unavailable());
        }
        store.finish_transaction(transaction)?;
        self.revalidate()?;
        Ok(true)
    }
}

impl MmpCustodyStore {
    /// Reads only the sealed retired representation under one original snapshot.
    /// Coexisting source rows, changed physical allocation, future generation,
    /// mismatched native/legacy classification or malformed evidence refuse.
    /// Version two still requires independent complete ACK/c1 qualification by
    /// the recovery consumer; this metadata read alone grants no settlement.
    pub(in crate::storage::mmp_custody) fn read_retired_metadata_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        sequence: u64,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<Option<(MessageAcceptanceMetadata, [u8; 32], usize)>> {
        let Some(value) = read_record(transaction, &key(sequence))? else {
            return Ok(None);
        };
        if value.len() < HEADER
            || value.len() > MAX_RETIRED_RECORD_BYTES
            || (&value[..8] != MAGIC && &value[..8] != NATIVE_MAGIC)
            || value[8..24] != self.store_id
        {
            return Err(unavailable());
        }
        let number = |offset: usize| -> Result<u64> {
            Ok(u64::from_le_bytes(
                value[offset..offset + 8]
                    .try_into()
                    .map_err(|_| unavailable())?,
            ))
        };
        let state = self.checked_state(transaction)?;
        let directory = self.directory.metadata()?;
        let rows: i64 = transaction
            .query_row(
                "SELECT count(*) FROM records WHERE key GLOB ?1",
                [format!("a1:{sequence:016x}:*")],
                |r| r.get(0),
            )
            .map_err(database_error)?;
        if rows != 1
            || number(24)? == 0
            || number(24)? > state.generation as u64
            || number(32)? > state.watermark
            || sequence > number(32)?
            || number(40)? != directory.dev()
            || number(48)? != directory.ino()
            || number(56)? != self.database_identity.device
            || number(64)? != self.database_identity.inode
            || number(72)? != (value.len() - HEADER) as u64
            || !state.watermark_origin_trusted
        {
            return Err(unavailable());
        }
        let evidence = decode_message_acceptance_metadata_evidence(
            &value[HEADER..],
            limits,
            self.limits.logical_bytes,
        )
        .map_err(|_| unavailable())?;
        let digest = *evidence.source_sha256();
        let original = evidence.original_encoded_bytes();
        let metadata = evidence.into_metadata();
        self.validate_decoded_source(transaction, sequence, &metadata)?;
        if !metadata.has_native_binding_record()
            || (non_native(&metadata) != (&value[..8] == MAGIC))
        {
            return Err(unavailable());
        }
        Ok(Some((metadata, digest, original)))
    }
}
