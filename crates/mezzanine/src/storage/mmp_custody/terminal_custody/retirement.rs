//! Exact original retired-recipient observations, linked to b1/o1 evidence.
//!
//! A lower witness follows actual canonical absence and exclusive source-slot
//! protection. This original transaction rechecks source, immutable provenance,
//! explicit namespace end, cutoff and p1/c1/ACK exclusion before writing d1.
//! First input/non-target state survives worker loss. Stored framing remains
//! descriptive and cannot create a native endpoint or successful delivery.

use super::*;
use mez_agent::messaging::{
    DeliveryTerminalReason, MessageAcceptanceDecodeLimits, MessageDeliveryTerminalSnapshot,
};

/// Actual allocation wrapper plus one fixed source/slot/provenance lower frame.
const RECORD_BYTES: usize = 168;

/// First exact retirement bytes and unrelated-row image, owned by TerminalPlan.
#[derive(Debug)]
pub(super) struct First {
    bytes: Vec<u8>,
    unaffected: [u8; 32],
}

/// Bounded journal data; it supplies no source or terminal mutation capability.
pub(in crate::storage::mmp_custody) struct StoredRetirement {
    /// Content-addressed original frame key, used for overlap checks.
    pub key: String,
    /// Immutable accepted occurrence, never a newly allocated sequence.
    pub sequence: u64,
    /// First selected retirement observation, preserved through retries.
    pub observed_at_ms: u64,
    /// Exact immutable accepted source serialization fingerprint.
    pub source_sha256: [u8; 32],
    /// Original frozen recipient slot, without membership re-resolution.
    pub binding_index: u32,
    /// Original namespace proved by the frozen native owner and b1/o1 rows.
    pub namespace: [u8; 16],
    /// Exact immutable original lifetime row image.
    pub lifetime_sha256: [u8; 32],
    /// Exact immutable original namespace-end row image.
    pub end_sha256: [u8; 32],
}

/// Validates exact bounded framing and original allocation before source reads.
fn decode(bytes: &[u8], store_id: &[u8]) -> Result<StoredRetirement> {
    if bytes.len() != RECORD_BYTES
        || &bytes[..8] != b"MMPTDR01"
        || &bytes[8..24] != store_id
        || &bytes[24..32] != b"MMPRET01"
        || bytes[48..52] != 1u32.to_le_bytes()
    {
        return Err(invalid());
    }
    let frame = StoredRetirement {
        key: record_key(bytes),
        sequence: u64::from_le_bytes(bytes[32..40].try_into().map_err(|_| invalid())?),
        observed_at_ms: u64::from_le_bytes(bytes[40..48].try_into().map_err(|_| invalid())?),
        source_sha256: bytes[52..84].try_into().map_err(|_| invalid())?,
        binding_index: u32::from_le_bytes(bytes[84..88].try_into().map_err(|_| invalid())?),
        namespace: bytes[88..104].try_into().map_err(|_| invalid())?,
        lifetime_sha256: bytes[104..136].try_into().map_err(|_| invalid())?,
        end_sha256: bytes[136..168].try_into().map_err(|_| invalid())?,
    };
    if frame.sequence == 0
        || frame.binding_index >= 4096
        || frame.source_sha256 == [0; 32]
        || frame.namespace == [0; 16]
        || frame.lifetime_sha256 == [0; 32]
        || frame.end_sha256 == [0; 32]
    {
        return Err(invalid());
    }
    Ok(frame)
}

impl PlannedTerminalStore<'_, '_> {
    /// Reconciles only the original bounded batch and retained non-target image.
    pub(super) fn commit_retirement_record(&mut self, write: &MessageTerminalWrite) -> Result<()> {
        if !write.belongs_to(self.scope) || write.kind() != MessageTerminalKind::RecipientRetirement
        {
            return Err(invalid());
        }
        let witness = write.retired_recipient().ok_or_else(invalid)?;
        self.owned.revalidate()?;
        let store = self.owned.store_mut();
        if store.store_id != self.plan.store_id
            || witness.record().store_id.as_slice() != self.plan.store_id
        {
            return Err(invalid());
        }
        let mut bytes = Vec::with_capacity(RECORD_BYTES);
        bytes.extend_from_slice(b"MMPTDR01");
        bytes.extend_from_slice(&self.plan.store_id);
        write.write_record(&mut bytes)?;
        let frame = decode(&bytes, &self.plan.store_id)?;
        if frame.sequence != witness.anchor().sequence()
            || frame.source_sha256 != witness.anchor().source_sha256()
            || frame.binding_index as usize != witness.anchor().binding_index()
        {
            return Err(invalid());
        }
        let transaction =
            rusqlite::Transaction::new_unchecked(&store.connection, TransactionBehavior::Immediate)
                .map_err(database_error)?;
        let state = store.checked_state(&transaction)?;
        if self.plan.retirement.get().is_none() {
            if state.generation as u64 != self.plan.generation || store.read(&frame.key)?.is_some()
            {
                return Err(invalid());
            }
            self.plan
                .retirement
                .set(First {
                    bytes: bytes.clone(),
                    unaffected: unaffected_rows_digest(&transaction, &[&frame.key])?,
                })
                .map_err(|_| invalid())?;
        }
        let first = self.plan.retirement.get().ok_or_else(invalid)?;
        if first.bytes != bytes
            || unaffected_rows_digest(&transaction, &[&frame.key])? != first.unaffected
        {
            return Err(invalid());
        }
        store.validate_retirement_in_snapshot(&transaction, &frame, write.decode_limits())?;
        store.ensure_source_not_expiry_observed(
            &transaction,
            frame.sequence,
            write.decode_limits(),
        )?;
        store.commit_in_transaction(
            &transaction,
            self.plan.generation,
            &[CustodyWrite {
                key: &frame.key,
                previous: None,
                value: Some(&bytes),
            }],
        )?;
        store.validate_retirement_in_snapshot(&transaction, &frame, write.decode_limits())?;
        store.ensure_source_not_expiry_observed(
            &transaction,
            frame.sequence,
            write.decode_limits(),
        )?;
        if store.read(&frame.key)?.as_deref() != Some(bytes.as_slice())
            || unaffected_rows_digest(&transaction, &[&frame.key])? != first.unaffected
        {
            return Err(invalid());
        }
        store.finish_transaction(transaction)?;
        self.owned.revalidate()?;
        Ok(())
    }
}

impl MmpCustodyStore {
    /// A prior source-wide TTL observation wins unselected slots. This write
    /// precondition is separate from reads: later expiry of a different slot
    /// must not invalidate an earlier genuine retired-recipient observation.
    fn ensure_source_not_expiry_observed(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        sequence: u64,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<()> {
        self.visit_expiry_metadata(transaction, |_, frame| {
            if frame.sequence == sequence {
                return Err(invalid());
            }
            Ok(())
        })?;
        let mut statement = transaction.prepare("SELECT CASE WHEN length(CAST(key AS BLOB))<=256 THEN key ELSE NULL END FROM records WHERE key LIKE 'k1:%' ORDER BY key COLLATE BINARY").map_err(database_error)?;
        let mut rows = statement.query([]).map_err(database_error)?;
        let mut count = 0;
        while let Some(row) = rows.next().map_err(database_error)? {
            count += 1;
            if count > self.limits.records {
                return Err(invalid());
            }
            let key: Option<String> = row.get(0).map_err(database_error)?;
            let ack = self.read_stored_acknowledgement_in_snapshot(
                transaction,
                &key.ok_or_else(invalid)?,
                limits,
            )?;
            if ack
                .decisions
                .iter()
                .any(|entry| entry.sequence == sequence && !entry.acknowledged)
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
    /// Checks one complete original source and exact immutable end/provenance.
    /// Decoded source drops before the ACK loop acquires another decoder body.
    fn validate_retirement_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        frame: &StoredRetirement,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<()> {
        if self.validate_accepted_occurrence_in_snapshot(transaction, frame.sequence)?
            != frame.source_sha256
        {
            return Err(invalid());
        }
        {
            let (source, digest, _) =
                self.read_accepted_source_in_snapshot(transaction, frame.sequence, limits)?;
            let index = frame.binding_index as usize;
            let binding = source.recipient_bindings().get(index).ok_or_else(invalid)?;
            let owner = source.native_recipient(index).ok_or_else(invalid)?;
            let lifetime_key = format!("b1:{}", owner.incarnation());
            let lifetime = self
                .read_native_lifetime_in_snapshot(transaction, &lifetime_key)?
                .ok_or_else(invalid)?;
            let end_key = super::super::registration_end::record_key(&frame.namespace);
            let end = self
                .read_registration_end_in_snapshot(transaction, &end_key)?
                .ok_or_else(invalid)?;
            let lifetime_bytes = self.read(&lifetime_key)?.ok_or_else(invalid)?;
            let end_bytes = self.read(&end_key)?.ok_or_else(invalid)?;
            if digest != frame.source_sha256
                || source.accepted_at_ms() > frame.observed_at_ms
                || lifetime.native.owner != *owner
                || lifetime.native.agent_id != binding.recipient
                || lifetime.native.registration_namespace != frame.namespace
                || lifetime.registered_after_sequence != binding.registered_after_sequence
                || end.snapshot.agent_id != binding.recipient
                || end.snapshot.registered_after_sequence != binding.registered_after_sequence
                || frame.sequence <= binding.registered_after_sequence
                || frame.sequence > end.snapshot.ended_after_sequence
                || lifetime.publication_generation >= end.generation
                || <[u8; 32]>::from(Sha256::digest(&lifetime_bytes)) != frame.lifetime_sha256
                || <[u8; 32]>::from(Sha256::digest(&end_bytes)) != frame.end_sha256
            {
                return Err(invalid());
            }
        }
        let occurrence = super::super::native_input::source_occurrence(
            self.store_id.as_slice().try_into().map_err(|_| invalid())?,
            frame.source_sha256,
            frame.binding_index,
        );
        for prefix in ["p1", "c1"] {
            if self
                .read(&super::super::native_input::record_key(prefix, &occurrence))?
                .is_some()
            {
                return Err(invalid());
            }
        }
        self.visit_expiry_metadata(transaction, |_, expiry| {
            if expiry.sequence == frame.sequence
                && expiry.binding_indices.contains(&frame.binding_index)
            {
                return Err(invalid());
            }
            Ok(())
        })?;
        self.visit_retirement_metadata(transaction, |other| {
            if other.key != frame.key
                && other.sequence == frame.sequence
                && other.binding_index == frame.binding_index
            {
                return Err(invalid());
            }
            Ok(())
        })?;
        self.check_expiry_ack_conflicts_in_snapshot(
            transaction,
            &super::read::StoredExpiry {
                sequence: frame.sequence,
                observed_at_ms: frame.observed_at_ms,
                source_sha256: frame.source_sha256,
                binding_indices: vec![frame.binding_index],
            },
            limits,
        )
    }

    /// Reads and fully qualifies descriptive d1 inside the original snapshot.
    pub(in crate::storage::mmp_custody) fn read_stored_retirement_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        key: &str,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<StoredRetirement> {
        if !std::ptr::eq(&self.connection, &**transaction) {
            return Err(invalid());
        }
        self.checked_state(transaction)?;
        let bytes = self.read(key)?.ok_or_else(invalid)?;
        let frame = decode(&bytes, &self.store_id)?;
        if frame.key != key {
            return Err(invalid());
        }
        self.validate_retirement_in_snapshot(transaction, &frame, limits)?;
        Ok(frame)
    }

    /// Streams bounded d1 metadata only; presence can refuse competing work but
    /// cannot supply an ACK, terminal mutation or original namespace capability.
    pub(in crate::storage::mmp_custody) fn visit_retirement_metadata(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        mut visit: impl FnMut(&StoredRetirement) -> Result<()>,
    ) -> Result<()> {
        if !std::ptr::eq(&self.connection, &**transaction) {
            return Err(invalid());
        }
        let mut statement = transaction.prepare("SELECT CASE WHEN length(CAST(key AS BLOB))<=256 THEN key ELSE NULL END FROM records WHERE key LIKE 'd1:%' ORDER BY key COLLATE BINARY").map_err(database_error)?;
        let mut rows = statement.query([]).map_err(database_error)?;
        let mut count = 0;
        while let Some(row) = rows.next().map_err(database_error)? {
            count += 1;
            if count > self.limits.records {
                return Err(invalid());
            }
            let key: Option<String> = row.get(0).map_err(database_error)?;
            let key = key.ok_or_else(invalid)?;
            let bytes = self.read(&key)?.ok_or_else(invalid)?;
            let frame = decode(&bytes, &self.store_id)?;
            if frame.key != key {
                return Err(invalid());
            }
            visit(&frame)?;
        }
        Ok(())
    }

    /// Excludes an actual retired slot before ACK, input or expiry mutation.
    pub(in crate::storage::mmp_custody) fn ensure_slot_not_retired_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        sequence: u64,
        index: u32,
    ) -> Result<()> {
        self.visit_retirement_metadata(transaction, |frame| {
            if frame.sequence == sequence && frame.binding_index == index {
                return Err(invalid());
            }
            Ok(())
        })
    }
}

/// Binds exact source, original allocation, slot and immutable evidence images.
fn record_key(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut hash = Sha256::new();
    hash.update(b"mezzanine-mmp-terminal-retirement-v1\0");
    hash.update(bytes);
    let mut key = String::from("d1:");
    for byte in hash.finalize() {
        let _ = write!(key, "{byte:02x}");
    }
    key
}

/// Complete original terminal observation, independent of its t1/d1 encoding.
pub(in crate::storage::mmp_custody) struct StoredTerminalObservation {
    /// Immutable source fingerprint shared with its exact terminal frame.
    pub source_sha256: [u8; 32],
    /// Qualified first reason/clock, without ACK or release authority.
    pub terminal: MessageDeliveryTerminalSnapshot,
}

impl MmpCustodyStore {
    /// Resolves one original terminal slot with full source/ACK/provenance
    /// qualification. Duplicate or mixed reasons are conflicts, never precedence.
    pub(in crate::storage::mmp_custody) fn stored_terminal_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        sequence: u64,
        index: u32,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<Option<StoredTerminalObservation>> {
        let mut found = None;
        self.visit_expiry_metadata(transaction, |key, frame| {
            if frame.sequence == sequence && frame.binding_indices.contains(&index) {
                if found.is_some() {
                    return Err(invalid());
                }
                let actual = self.read_stored_expiry_in_snapshot(transaction, key, limits)?;
                found = Some(StoredTerminalObservation {
                    source_sha256: actual.source_sha256,
                    terminal: MessageDeliveryTerminalSnapshot {
                        reason: DeliveryTerminalReason::Expired,
                        observed_at_ms: actual.observed_at_ms,
                        released: false,
                    },
                });
            }
            Ok(())
        })?;
        self.visit_retirement_metadata(transaction, |frame| {
            if frame.sequence == sequence && frame.binding_index == index {
                if found.is_some() {
                    return Err(invalid());
                }
                let actual =
                    self.read_stored_retirement_in_snapshot(transaction, &frame.key, limits)?;
                found = Some(StoredTerminalObservation {
                    source_sha256: actual.source_sha256,
                    terminal: MessageDeliveryTerminalSnapshot {
                        reason: DeliveryTerminalReason::RecipientRetired,
                        observed_at_ms: actual.observed_at_ms,
                        released: false,
                    },
                });
            }
            Ok(())
        })?;
        Ok(found)
    }
}
