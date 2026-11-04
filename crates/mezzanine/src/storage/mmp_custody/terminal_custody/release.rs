//! Immutable original terminal-horizon release records under the same fence.
//!
//! A release cites actual original t1 before-images and a frozen horizon
//! observation. No sender identity/consumption is inferred from these bytes.
//! Validation shares the original read/commit snapshot and preserves all other
//! rows. Frames describe history; only the trusted write seals a lower positive.

use super::*;
use mez_agent::messaging::{
    MESSAGE_TERMINAL_HANDOFF_MS, MESSAGE_TERMINAL_SLOT_LIMIT, MessageAcceptanceDecodeLimits,
};

/// Actual stored data, not a runtime source, original sender or release owner.
pub(in crate::storage::mmp_custody) struct StoredRelease {
    pub sequence: u64,
    pub observed_at_ms: u64,
    pub source_sha256: [u8; 32],
    pub entries: Vec<(u32, u64)>,
}

/// Validates bounded allocation/source/slot/first-clock framing; only expiry
/// horizons are implemented. Unknown reason/format must remain unavailable.
fn decode(bytes: &[u8], store_id: &[u8]) -> Result<StoredRelease> {
    if bytes.len() < 97
        || bytes.len() > 84 + 13 * MESSAGE_TERMINAL_SLOT_LIMIT
        || &bytes[..8] != b"MMPTRE01"
        || &bytes[8..24] != store_id
        || &bytes[24..32] != b"MMPREL01"
    {
        return Err(invalid());
    }
    let sequence = u64::from_le_bytes(bytes[32..40].try_into().map_err(|_| invalid())?);
    let observed_at_ms = u64::from_le_bytes(bytes[40..48].try_into().map_err(|_| invalid())?);
    let count = u32::from_le_bytes(bytes[48..52].try_into().map_err(|_| invalid())?) as usize;
    if sequence == 0
        || count == 0
        || count > MESSAGE_TERMINAL_SLOT_LIMIT
        || bytes.len() != 84 + 13 * count
    {
        return Err(invalid());
    }
    let mut entries = Vec::with_capacity(count);
    for entry in bytes[84..].as_chunks::<13>().0 {
        let index = u32::from_le_bytes(entry[..4].try_into().map_err(|_| invalid())?);
        let first = u64::from_le_bytes(entry[4..12].try_into().map_err(|_| invalid())?);
        if index >= 4096
            || entry[12] != 1
            || observed_at_ms.saturating_sub(first) < MESSAGE_TERMINAL_HANDOFF_MS
            || entries.last().is_some_and(|(prior, _)| *prior >= index)
        {
            return Err(invalid());
        }
        entries.push((index, first));
    }
    Ok(StoredRelease {
        sequence,
        observed_at_ms,
        source_sha256: bytes[52..84].try_into().map_err(|_| invalid())?,
        entries,
    })
}

impl PlannedTerminalStore<'_, '_> {
    /// Reconciles only exact original horizon bytes; failures retain uncertainty.
    pub(super) fn commit_release_record(&mut self, write: &MessageTerminalWrite) -> Result<()> {
        if !write.belongs_to(self.scope) || write.kind() != MessageTerminalKind::HorizonRelease {
            return Err(invalid());
        }
        self.owned.revalidate()?;
        let store = self.owned.store_mut();
        if store.store_id != self.plan.store_id {
            return Err(invalid());
        }
        let mut bytes = Vec::with_capacity(write.encoded_bytes() + 24);
        bytes.extend_from_slice(b"MMPTRE01");
        bytes.extend_from_slice(&self.plan.store_id);
        write.write_record(&mut bytes)?;
        let frame = decode(&bytes, &self.plan.store_id)?;
        if frame.sequence != write.sequence()
            || frame.observed_at_ms != write.observed_at_ms()
            || frame.entries.iter().copied().ne(write
                .terminal_entries()
                .map(|(index, terminal)| (index, terminal.observed_at_ms)))
        {
            return Err(invalid());
        }
        let key = record_key(&bytes);
        let transaction =
            rusqlite::Transaction::new_unchecked(&store.connection, TransactionBehavior::Immediate)
                .map_err(database_error)?;
        let state = store.checked_state(&transaction)?;
        if u64::try_from(state.generation).map_err(|_| invalid())? < self.plan.generation {
            return Err(invalid());
        }
        store.validate_release_in_snapshot(&transaction, &frame, write.decode_limits())?;
        store.visit_release_metadata(&transaction, |other_key, other| {
            if other_key != key
                && other.sequence == frame.sequence
                && other
                    .entries
                    .iter()
                    .any(|(index, _)| frame.entries.iter().any(|(target, _)| index == target))
            {
                return Err(invalid());
            }
            Ok(())
        })?;
        if let Some(existing) = read_record(&transaction, &key)? {
            if existing != bytes {
                return Err(invalid());
            }
            store.finish_transaction(transaction)?;
        } else {
            let unaffected = unaffected_rows_digest(&transaction, &[&key])?;
            store.commit_in_transaction(
                &transaction,
                self.plan.generation,
                &[CustodyWrite {
                    key: &key,
                    previous: None,
                    value: Some(&bytes),
                }],
            )?;
            store.validate_release_in_snapshot(&transaction, &frame, write.decode_limits())?;
            if unaffected_rows_digest(&transaction, &[&key])? != unaffected {
                return Err(invalid());
            }
            store.finish_transaction(transaction)?;
        }
        self.owned.revalidate()?;
        Ok(())
    }
}

impl MmpCustodyStore {
    /// Qualifies actual terminal before-images in this same original snapshot.
    /// Each full t1 reader releases its decoded source before the next read.
    fn validate_release_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        frame: &StoredRelease,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<()> {
        let mut found = vec![false; frame.entries.len()];
        self.visit_expiry_metadata(transaction, |key, expiry| {
            if expiry.sequence != frame.sequence {
                return Ok(());
            }
            let relevant = frame
                .entries
                .iter()
                .any(|(index, _)| expiry.binding_indices.contains(index));
            if !relevant {
                return Ok(());
            }
            let expiry = self.read_stored_expiry_in_snapshot(transaction, key, limits)?;
            if expiry.source_sha256 != frame.source_sha256 {
                return Err(invalid());
            }
            for (slot, (index, first)) in frame.entries.iter().enumerate() {
                if expiry.binding_indices.contains(index) {
                    if found[slot] || expiry.observed_at_ms != *first {
                        return Err(invalid());
                    }
                    found[slot] = true;
                }
            }
            Ok(())
        })?;
        if found.iter().any(|found| !found) {
            return Err(invalid());
        }
        Ok(())
    }

    /// Streams bounded descriptive metadata; not a positive-release constructor.
    pub(in crate::storage::mmp_custody) fn visit_release_metadata(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        mut visit: impl FnMut(&str, &StoredRelease) -> Result<()>,
    ) -> Result<()> {
        if !std::ptr::eq(&self.connection, &**transaction) {
            return Err(invalid());
        }
        let mut statement = transaction.prepare("SELECT CASE WHEN length(CAST(key AS BLOB))<=256 THEN key ELSE NULL END FROM records WHERE key LIKE 'r1:%' ORDER BY key COLLATE BINARY").map_err(database_error)?;
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

    /// Full original-source qualification for inventory/history/reconstruction.
    pub(in crate::storage::mmp_custody) fn read_stored_release_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        key: &str,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<StoredRelease> {
        if !std::ptr::eq(&self.connection, &**transaction) {
            return Err(invalid());
        }
        self.checked_state(transaction)?;
        let bytes = read_record(transaction, key)?.ok_or_else(invalid)?;
        if record_key(&bytes) != key {
            return Err(invalid());
        }
        let frame = decode(&bytes, &self.store_id)?;
        self.validate_release_in_snapshot(transaction, &frame, limits)?;
        Ok(frame)
    }
}

/// Binds every actual allocation/source/slot/clock byte under a distinct domain.
pub(super) fn record_key(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut digest = Sha256::new();
    digest.update(b"mezzanine-mmp-terminal-release-v1\0");
    digest.update(bytes);
    let mut key = String::from("r1:");
    for byte in digest.finalize() {
        let _ = write!(key, "{byte:02x}");
    }
    key
}
fn invalid() -> MezError {
    MezError::invalid_state("MMP original terminal horizon release unavailable")
}
