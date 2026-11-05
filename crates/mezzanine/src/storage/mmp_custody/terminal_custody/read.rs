//! Bounded expiry metadata and original-source qualification in one snapshot.

use super::*;
use mez_agent::messaging::{MESSAGE_TERMINAL_SLOT_LIMIT, MessageAcceptanceDecodeLimits};

/// Historical data only; neither source/ACK nor sender/native authority.
pub(in crate::storage::mmp_custody) struct StoredExpiry {
    pub sequence: u64,
    pub observed_at_ms: u64,
    pub source_sha256: [u8; 32],
    pub binding_indices: Vec<u32>,
}

/// Checks a fixed actual-allocation wrapper and the lower bodyless data frame.
pub(super) fn decode(bytes: &[u8], store_id: &[u8]) -> Result<StoredExpiry> {
    if bytes.len() < 88
        || bytes.len() > 84 + 4 * MESSAGE_TERMINAL_SLOT_LIMIT
        || &bytes[..8] != b"MMPTEX01"
        || &bytes[8..24] != store_id
        || &bytes[24..32] != b"MMPEXP01"
    {
        return Err(invalid());
    }
    let sequence = u64::from_le_bytes(bytes[32..40].try_into().map_err(|_| invalid())?);
    let observed_at_ms = u64::from_le_bytes(bytes[40..48].try_into().map_err(|_| invalid())?);
    let count = u32::from_le_bytes(bytes[48..52].try_into().map_err(|_| invalid())?) as usize;
    if sequence == 0
        || count == 0
        || count > MESSAGE_TERMINAL_SLOT_LIMIT
        || bytes.len() != 84 + 4 * count
    {
        return Err(invalid());
    }
    let mut binding_indices = Vec::with_capacity(count);
    for bytes in bytes[84..].as_chunks::<4>().0 {
        let index = u32::from_le_bytes(*bytes);
        if index >= 4096 || binding_indices.last().is_some_and(|prior| *prior >= index) {
            return Err(invalid());
        }
        binding_indices.push(index);
    }
    Ok(StoredExpiry {
        sequence,
        observed_at_ms,
        source_sha256: bytes[52..84].try_into().map_err(|_| invalid())?,
        binding_indices,
    })
}

impl MmpCustodyStore {
    /// Validates one immutable record and its original source under the caller's
    /// same-store read snapshot. No result publishes an endpoint or disposition.
    pub(in crate::storage::mmp_custody) fn read_stored_expiry_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        key: &str,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<StoredExpiry> {
        if !std::ptr::eq(&self.connection, &**transaction) {
            return Err(invalid());
        }
        self.checked_state(transaction)?;
        let bytes = read_record(transaction, key)?.ok_or_else(invalid)?;
        if record_key(&bytes) != key {
            return Err(invalid());
        }
        let frame = decode(&bytes, &self.store_id)?;
        self.validate_expiry_source_in_snapshot(transaction, &frame, limits)?;
        self.check_expiry_ack_conflicts_in_snapshot(transaction, &frame, limits)?;
        Ok(frame)
    }
    /// Streams only bounded metadata. Callers must separately qualify source
    /// facts before adopting any state; a conflicting frame can only refuse I/O.
    pub(in crate::storage::mmp_custody) fn visit_expiry_metadata(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        mut visit: impl FnMut(&str, &StoredExpiry) -> Result<()>,
    ) -> Result<()> {
        if !std::ptr::eq(&self.connection, &**transaction) {
            return Err(invalid());
        }
        let mut statement = transaction.prepare("SELECT CASE WHEN length(CAST(key AS BLOB))<=256 THEN key ELSE NULL END FROM records WHERE key LIKE 't1:%' ORDER BY key COLLATE BINARY").map_err(database_error)?;
        let mut rows = statement.query([]).map_err(database_error)?;
        let mut count = 0;
        while let Some(row) = rows.next().map_err(database_error)? {
            count += 1;
            if count > self.limits.records {
                return Err(invalid());
            }
            let key: Option<String> = row.get(0).map_err(database_error)?;
            let key = key.ok_or_else(invalid)?;
            let bytes = read_record(transaction, &key)?.ok_or_else(invalid)?;
            if record_key(&bytes) != key {
                return Err(invalid());
            }
            visit(&key, &decode(&bytes, &self.store_id)?)?;
        }
        Ok(())
    }
    /// Prevents a positive ACK/native input from contradicting an actual stored
    /// terminal slot. Presence refuses; these metadata bytes confer no positive.
    pub(in crate::storage::mmp_custody) fn ensure_slot_not_terminal_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        sequence: u64,
        index: u32,
    ) -> Result<()> {
        self.visit_expiry_metadata(transaction, |_, frame| {
            if frame.sequence == sequence && frame.binding_indices.contains(&index) {
                return Err(invalid());
            }
            Ok(())
        })?;
        self.ensure_slot_not_retired_in_snapshot(transaction, sequence, index)
    }
    /// Validates complete immutable source bytes before decoding one reserved
    /// payload quantum, checks original strict TTL and every frozen slot, then
    /// drops that source before any ACK decoder runs. Persistent native plans
    /// must settle before selecting expiry; they cannot be discarded here.
    pub(super) fn validate_expiry_source_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        frame: &StoredExpiry,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<()> {
        if self.validate_accepted_occurrence_in_snapshot(transaction, frame.sequence)?
            != frame.source_sha256
        {
            return Err(invalid());
        }
        let (source, digest, _) =
            self.read_accepted_metadata_in_snapshot(transaction, frame.sequence, limits)?;
        if digest != frame.source_sha256
            || source.envelope().ttl_ms.is_none_or(|ttl| {
                source.accepted_at_ms().saturating_add(ttl) >= frame.observed_at_ms
            })
            || frame
                .binding_indices
                .iter()
                .any(|index| *index as usize >= source.recipient_bindings().len())
        {
            return Err(invalid());
        }
        let store_id: [u8; 16] = self.store_id.as_slice().try_into().map_err(|_| invalid())?;
        for index in &frame.binding_indices {
            self.ensure_slot_not_retired_in_snapshot(transaction, frame.sequence, *index)?;
            let occurrence = super::super::native_input::source_occurrence(
                store_id,
                frame.source_sha256,
                *index,
            );
            for prefix in ["p1", "c1"] {
                let key = super::super::native_input::record_key(prefix, &occurrence);
                if read_record(transaction, &key)?.is_some() {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
    /// Checks original positive ACKs while retaining at most one decoded source
    /// at a time. The caller reserves a complete decoder quantum before I/O.
    pub(super) fn check_expiry_ack_conflicts_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        frame: &StoredExpiry,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<()> {
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
            if ack.decisions.iter().any(|entry| {
                entry.acknowledged
                    && entry.sequence == frame.sequence
                    && frame.binding_indices.contains(&entry.binding_index)
            }) {
                return Err(invalid());
            }
        }
        Ok(())
    }
}
