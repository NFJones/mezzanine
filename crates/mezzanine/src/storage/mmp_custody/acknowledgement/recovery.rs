//! Verified, bounded reading of original stored ACK decisions.
//!
//! An ACK record is historical evidence, never a live recipient registration or
//! proof that a restored native conversation contains its input. This reader
//! validates its content-addressed key, framing, every exact source digest and
//! captured recipient interval in one store snapshot. Callers must keep its
//! descriptive result quarantined until a complete inventory and trusted
//! application-commit fold establish whether any decisions may be adopted.

use mez_agent::messaging::MessageAcceptanceDecodeLimits;

use super::*;

/// Fixed original format limits also enforced by the lower ACK writer.
const MAX_RECORD_BYTES: usize = 240 * 1024;
const MAX_ENTRIES: usize = 4096;
const HEADER_BYTES: usize = 48;
const ENTRY_BYTES: usize = 45;

/// One immutable ACK decision, without live delivery or input authority.
pub(crate) struct StoredAcknowledgementDecision {
    /// Original accepted occurrence, not a newly generated delivery.
    pub sequence: u64,
    /// Original captured recipient slot within that occurrence.
    pub binding_index: u32,
    /// True is an offered commit; false is an original expiry exclusion.
    pub acknowledged: bool,
}

/// Bounded, descriptive ACK evidence held until a complete restore fold.
pub(crate) struct StoredAcknowledgement {
    /// Original recipient label, not a restored registration capability.
    pub recipient: String,
    /// Captured exact registration interval shared by all decisions.
    pub registered_after_sequence: u64,
    /// Original cursor before this cumulative decision.
    pub after_sequence: u64,
    /// Original offered endpoint, required to be the final acknowledged entry.
    pub endpoint_sequence: u64,
    /// Clock recorded for the original offered endpoint.
    pub observed_at_ms: u64,
    /// Exact ordered source-bound decisions, without application authority.
    pub decisions: Vec<StoredAcknowledgementDecision>,
}

impl MmpCustodyStore {
    /// Reads one stored ACK and all its exact sources in one SQLite snapshot.
    /// This validates storage history only; it does not commit native input,
    /// revive a registration, advance a cursor, or publish any restored state.
    pub(crate) fn read_stored_acknowledgement(
        &self,
        key: &str,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<StoredAcknowledgement> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(database_error)?;
        let result = self.read_stored_acknowledgement_in_snapshot(&transaction, key, limits)?;
        self.finish_transaction(transaction)?;
        Ok(result)
    }

    /// Checks one ACK and its sources under the caller's same-store snapshot.
    /// Any output remains private until all inventory checks and commit succeed.
    pub(in crate::storage::mmp_custody) fn read_stored_acknowledgement_in_snapshot(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        key: &str,
        limits: MessageAcceptanceDecodeLimits,
    ) -> Result<StoredAcknowledgement> {
        if !std::ptr::eq(&self.connection, &**transaction) {
            return Err(invalid("snapshot belongs to another store"));
        }
        self.checked_state(transaction)?;
        let bytes = self
            .read(key)?
            .ok_or_else(|| invalid("record is unavailable"))?;
        if bytes.len() < HEADER_BYTES
            || bytes.len() > MAX_RECORD_BYTES
            || &bytes[..8] != b"MMPACK01"
            || ack_key(&bytes) != key
        {
            return Err(invalid("record framing or key changed"));
        }
        let after_sequence = integer64(&bytes, 8)?;
        let endpoint_sequence = integer64(&bytes, 16)?;
        let registered_after_sequence = integer64(&bytes, 24)?;
        let observed_at_ms = integer64(&bytes, 32)?;
        let recipient_bytes = integer32(&bytes, 40)? as usize;
        let entry_count = integer32(&bytes, 44)? as usize;
        let entries_start = HEADER_BYTES
            .checked_add(recipient_bytes)
            .ok_or_else(|| invalid("record length overflow"))?;
        let expected = entry_count
            .checked_mul(ENTRY_BYTES)
            .and_then(|bytes| entries_start.checked_add(bytes))
            .ok_or_else(|| invalid("record length overflow"))?;
        if entry_count == 0
            || entry_count > MAX_ENTRIES
            || expected != bytes.len()
            || endpoint_sequence <= after_sequence
            || endpoint_sequence <= registered_after_sequence
        {
            return Err(invalid("record bounds or cursor are invalid"));
        }
        let recipient = std::str::from_utf8(&bytes[HEADER_BYTES..entries_start])
            .map_err(|_| invalid("recipient is not valid UTF-8"))?;
        if recipient.is_empty() || recipient.chars().any(char::is_control) {
            return Err(invalid("recipient identity is invalid"));
        }
        let mut decisions = Vec::new();
        decisions
            .try_reserve_exact(entry_count)
            .map_err(|_| invalid("decision allocation failed"))?;
        let mut previous = after_sequence;
        let (entries, remainder) = bytes[entries_start..].as_chunks::<ENTRY_BYTES>();
        if !remainder.is_empty() {
            return Err(invalid("decision framing changed"));
        }
        for entry in entries {
            let sequence = integer64(entry, 0)?;
            let binding_index = integer32(entry, 8)?;
            let acknowledged = match entry[12] {
                0 => false,
                1 => true,
                _ => return Err(invalid("decision flag is invalid")),
            };
            if sequence <= previous
                || sequence <= registered_after_sequence
                || sequence > endpoint_sequence
            {
                return Err(invalid("decision sequence is invalid"));
            }
            let (source, digest, _) =
                self.read_accepted_source_in_snapshot(transaction, sequence, limits)?;
            if digest.as_slice() != &entry[13..] {
                return Err(invalid("decision source digest changed"));
            }
            let binding = source
                .recipient_bindings()
                .get(binding_index as usize)
                .ok_or_else(|| invalid("recipient binding is missing"))?;
            if binding.recipient != recipient
                || binding.registered_after_sequence != registered_after_sequence
            {
                return Err(invalid("recipient binding changed"));
            }
            if !acknowledged
                && source
                    .envelope()
                    .ttl_ms
                    .is_none_or(|ttl| source.accepted_at_ms().saturating_add(ttl) >= observed_at_ms)
            {
                return Err(invalid("expiry exclusion is inconsistent"));
            }
            previous = sequence;
            decisions.push(StoredAcknowledgementDecision {
                sequence,
                binding_index,
                acknowledged,
            });
        }
        if decisions
            .last()
            .is_none_or(|decision| decision.sequence != endpoint_sequence || !decision.acknowledged)
        {
            return Err(invalid("acknowledged endpoint is missing"));
        }
        let mut recipient_owned = String::new();
        recipient_owned
            .try_reserve_exact(recipient.len())
            .map_err(|_| invalid("recipient allocation failed"))?;
        recipient_owned.push_str(recipient);
        Ok(StoredAcknowledgement {
            recipient: recipient_owned,
            registered_after_sequence,
            after_sequence,
            endpoint_sequence,
            observed_at_ms,
            decisions,
        })
    }
}

/// Reads a checked fixed-width little-endian integer from an untrusted record.
fn integer64(bytes: &[u8], offset: usize) -> Result<u64> {
    let field: [u8; 8] = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| invalid("integer is truncated"))?
        .try_into()
        .map_err(|_| invalid("integer framing changed"))?;
    Ok(u64::from_le_bytes(field))
}

/// Reads a checked fixed-width little-endian length or binding index.
fn integer32(bytes: &[u8], offset: usize) -> Result<u32> {
    let field: [u8; 4] = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid("integer is truncated"))?
        .try_into()
        .map_err(|_| invalid("integer framing changed"))?;
    Ok(u32::from_le_bytes(field))
}

/// Gives context without including private recipient or payload data.
fn invalid(detail: &str) -> MezError {
    MezError::invalid_state(format!("MMP stored ACK {detail}"))
}
