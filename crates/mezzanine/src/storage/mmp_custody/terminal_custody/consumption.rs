//! Canonical-proof-qualified sender consumption on the original terminal lane.
//!
//! s1 binds one original terminal before-image to actual h1 canonical evidence.
//! Only an opaque source-owned write selects it; bytes are descriptive. The
//! same fenced transaction verifies source/ACK/h1 and mutual release exclusion,
//! preserving every non-target row. Lost replies reconcile the exact first
//! bytes/plan. Cold reads qualify the same evidence but grant no live sender,
//! task, acknowledgement or elapsed-horizon clock authority.

use super::super::terminal_handoff::HandoffConsumption;
use super::*;
use mez_agent::messaging::{
    DeliveryTerminalReason, MessageAcceptanceDecodeLimits, MessageDeliveryTerminalSnapshot,
    MessageTerminalHandoffRecord,
};

/// Fixed 24-byte original allocation wrapper plus the 133-byte lower frame.
const RECORD_BYTES: usize = 157;

/// Validates bounded original framing without creating a source or capability.
fn decode(bytes: &[u8], store_id: &[u8]) -> Result<HandoffConsumption> {
    if bytes.len() != RECORD_BYTES
        || &bytes[..8] != b"MMPTSC01"
        || &bytes[8..24] != store_id
        || &bytes[24..32] != b"MMPCON01"
    {
        return Err(invalid());
    }
    let sequence = u64::from_le_bytes(bytes[32..40].try_into().map_err(|_| invalid())?);
    let source_sha256 = bytes[40..72].try_into().map_err(|_| invalid())?;
    let binding_index = u32::from_le_bytes(bytes[72..76].try_into().map_err(|_| invalid())?);
    let observed_at_ms = u64::from_le_bytes(bytes[76..84].try_into().map_err(|_| invalid())?);
    let reason = match bytes[84] {
        1 => DeliveryTerminalReason::Expired,
        2 => DeliveryTerminalReason::RecipientRetired,
        _ => return Err(invalid()),
    };
    let canonical = MessageTerminalHandoffRecord {
        store_id: store_id.try_into().map_err(|_| invalid())?,
        occurrence: bytes[85..117].try_into().map_err(|_| invalid())?,
        archive_sequence: u64::from_le_bytes(bytes[117..125].try_into().map_err(|_| invalid())?),
        canonical_sha256: bytes[125..157].try_into().map_err(|_| invalid())?,
        archive_observed: false,
    };
    if sequence == 0
        || binding_index >= 4096
        || source_sha256 == [0; 32]
        || canonical.archive_sequence == 0
        || canonical.canonical_sha256 == [0; 32]
    {
        return Err(invalid());
    }
    Ok(HandoffConsumption {
        sequence,
        source_sha256,
        binding_index,
        terminal: MessageDeliveryTerminalSnapshot {
            reason,
            observed_at_ms,
            released: false,
        },
        canonical,
    })
}

impl PlannedTerminalStore<'_, '_> {
    /// Carries actual canonical proof through same-store commit and uncertainty.
    pub(super) fn commit_consumption_record(&mut self, write: &MessageTerminalWrite) -> Result<()> {
        if !write.belongs_to(self.scope) || write.kind() != MessageTerminalKind::SenderConsumption {
            return Err(invalid());
        }
        let proof = write.canonical_handoff().ok_or_else(invalid)?;
        self.owned.revalidate()?;
        let store = self.owned.store_mut();
        if store.store_id != self.plan.store_id
            || proof.record().store_id.as_slice() != self.plan.store_id
        {
            return Err(invalid());
        }
        let mut bytes = Vec::with_capacity(RECORD_BYTES);
        bytes.extend_from_slice(b"MMPTSC01");
        bytes.extend_from_slice(&self.plan.store_id);
        write.write_record(&mut bytes)?;
        let frame = decode(&bytes, &self.plan.store_id)?;
        if frame.sequence != write.sequence()
            || frame.binding_index as usize != proof.anchor().binding_index()
            || frame.source_sha256 != proof.anchor().source_sha256()
            || frame.terminal != proof.anchor().terminal()
        {
            return Err(invalid());
        }
        let key = record_key(&bytes);
        let transaction =
            rusqlite::Transaction::new_unchecked(&store.connection, TransactionBehavior::Immediate)
                .map_err(database_error)?;
        let state = store.checked_state(&transaction)?;
        if (state.generation as u64) < self.plan.generation {
            return Err(invalid());
        }
        store.validate_consumption_in_snapshot(&transaction, &frame, write.decode_limits())?;
        store.visit_consumption_metadata(&transaction, |other_key, other| {
            if other_key != key
                && other.sequence == frame.sequence
                && other.binding_index == frame.binding_index
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
            store.validate_consumption_in_snapshot(&transaction, &frame, write.decode_limits())?;
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
    /// Checks exact canonical relation and excludes actual horizon release.
    fn validate_consumption_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        frame: &HandoffConsumption,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<()> {
        self.qualify_handoff_consumption(transaction, frame, limits)?;
        self.visit_release_metadata(transaction, |_, release| {
            if release.sequence == frame.sequence
                && release
                    .entries
                    .iter()
                    .any(|(index, _, _)| *index == frame.binding_index)
            {
                return Err(invalid());
            }
            Ok(())
        })
    }
    /// Reads full source/canonical/ACK qualification under the cold snapshot.
    pub(in crate::storage::mmp_custody) fn read_stored_consumption_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        key: &str,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<HandoffConsumption> {
        if !std::ptr::eq(&self.connection, &**transaction) {
            return Err(invalid());
        }
        self.checked_state(transaction)?;
        let bytes = read_record(transaction, key)?.ok_or_else(invalid)?;
        if record_key(&bytes) != key {
            return Err(invalid());
        }
        let frame = decode(&bytes, &self.store_id)?;
        self.validate_consumption_in_snapshot(transaction, &frame, limits)?;
        Ok(frame)
    }
    /// Streams only bounded original s1 metadata; output never grants authority.
    fn visit_consumption_metadata(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        mut visit: impl FnMut(&str, &HandoffConsumption) -> Result<()>,
    ) -> Result<()> {
        if !std::ptr::eq(&self.connection, &**transaction) {
            return Err(invalid());
        }
        let mut statement = transaction.prepare("SELECT CASE WHEN length(CAST(key AS BLOB))<=256 THEN key ELSE NULL END FROM records WHERE key LIKE 's1:%' ORDER BY key COLLATE BINARY").map_err(database_error)?;
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
    /// Presence only refuses competing release; metadata is never proof of ACK.
    pub(in crate::storage::mmp_custody) fn ensure_slot_not_consumed_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        sequence: u64,
        index: u32,
    ) -> Result<()> {
        self.visit_consumption_metadata(transaction, |_, frame| {
            if frame.sequence == sequence && frame.binding_index == index {
                return Err(invalid());
            }
            Ok(())
        })
    }
}

/// Distinct content-addressed source/canonical domain for the original s1 frame.
fn record_key(bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"mezzanine-mmp-terminal-consumption-v1\0");
    hash.update(bytes);
    let mut key = String::from("s1:");
    for byte in hash.finalize() {
        use std::fmt::Write;
        let _ = write!(key, "{byte:02x}");
    }
    key
}
