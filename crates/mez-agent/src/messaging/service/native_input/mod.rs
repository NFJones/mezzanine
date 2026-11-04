//! Original native receive source and trusted canonical-storage qualification.
//!
//! An anchor freezes an actual accepted source/recipient/native owner under its
//! live storage scope. It is not canonical commit proof. Only an actual trusted
//! canonical-store callback may return that separate opaque proof. Recovery
//! lookups may describe old frozen native owners without reviving them; fresh
//! creation additionally requires a genuine offered current guarded endpoint.
//! Retained metadata and terminal protection end with the final actual owner.

use super::*;
use serde::{Deserialize, Serialize};

/// Source-owned transport attribution, never serializable or caller-constructed.
#[derive(Clone)]
pub struct MessageNativeInputAnchor(pub(super) Arc<Source>);

/// Immutable actual source retained through storage and exact ACK settlement.
pub(super) struct Source {
    pub(super) runtime: Arc<()>,
    pub(super) scope: MessageStorageScope,
    pub(super) row: Arc<QueuedEnvelope>,
    pub(super) index: usize,
    pub(super) digest: [u8; 32],
    pub(super) native: MessageNativeOwner,
    pub(super) offered_at_ms: Option<u64>,
    /// Dedicated small control block keeps row weak protection bounded even
    /// after the larger source/metadata reservation has actually retired.
    _protection: Arc<()>,
    _lease: MessageStorageSnapshotLease,
}

impl std::fmt::Debug for MessageNativeInputAnchor {
    /// Redacts source bodies, private conversation and scope identities.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageNativeInputAnchor")
            .field("sequence", &self.sequence())
            .field("slot", &self.binding_index())
            .finish_non_exhaustive()
    }
}

/// Descriptive actual canonical append result. Decoding this record alone never
/// creates input/ACK authority; the trusted port must qualify the real original.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageNativeInputRecord {
    /// Original private MMP allocation, verified by the product's owned root.
    pub store_id: [u8; 16],
    /// Exact canonical native occurrence key derived by the trusted port.
    pub occurrence: [u8; 32],
    /// Actual canonical archive row, not a live turn or provider generation.
    pub archive_sequence: u64,
    /// Digest of the exact committed canonical row and original attribution.
    pub canonical_sha256: [u8; 32],
    /// Original genuine native selection clock, not a replay clock renewal.
    pub offered_at_ms: u64,
    /// True only for this trusted callback's actual original archive read/sync.
    /// Historical c1 lookup alone does not claim current backing. Serialization
    /// deliberately discards this observation rather than importing it as fact.
    #[serde(skip)]
    pub archive_observed: bool,
}

/// Trusted synchronous canonical-storage boundary run on the owned worker.
pub trait MessageNativeInputStore {
    /// Returns only actual committed canonical input for this exact source.
    /// None is qualified absence, never permission to fabricate an input or ACK.
    fn verify(
        &mut self,
        anchor: &MessageNativeInputAnchor,
    ) -> Result<Option<MessageNativeInputRecord>>;
}

/// Actual canonical commit, separate from source selection and public metadata.
/// It remains valid for its original slot after the current endpoint changes.
#[derive(Debug, Clone)]
pub struct MessageNativeInputCommit {
    pub(super) anchor: MessageNativeInputAnchor,
    pub(super) record: MessageNativeInputRecord,
}

impl MessageNativeInputCommit {
    /// Borrows the original exact transport attribution qualified by storage.
    pub fn anchor(&self) -> &MessageNativeInputAnchor {
        &self.anchor
    }
    /// Borrows descriptive committed row facts, never a constructor capability.
    pub fn record(&self) -> &MessageNativeInputRecord {
        &self.record
    }
}

impl MessageNativeInputAnchor {
    /// Checks the actual root scope; equal IDs/DTOs cannot reproduce this token.
    pub fn belongs_to(&self, scope: &MessageStorageScope) -> bool {
        self.0.scope == *scope
    }
    /// Returns the original accepted occurrence without allocating a new one.
    pub fn sequence(&self) -> u64 {
        self.0.row.sequence
    }
    /// Returns the immutable recipient slot in the original audience.
    pub fn binding_index(&self) -> usize {
        self.0.index
    }
    /// Borrows the original public recipient attribution, not a reconnect token.
    pub fn recipient(&self) -> &AgentId {
        &self.0.row.recipient_bindings[self.0.index].0
    }
    /// Returns the exact original registration interval floor.
    pub fn registered_after_sequence(&self) -> u64 {
        self.0.row.recipient_bindings[self.0.index].1
    }
    /// Borrows frozen native ownership; it does not revive a current endpoint.
    pub fn native_owner(&self) -> &MessageNativeOwner {
        &self.0.native
    }
    /// Returns the original acceptance serialization fingerprint.
    pub fn source_sha256(&self) -> [u8; 32] {
        self.0.digest
    }
    /// Returns the immutable payload length while its actual allocation is cold.
    pub fn payload_bytes(&self) -> usize {
        self.0.row.envelope.payload_bytes
    }
    /// Returns the genuine selection used by a fresh receive, absent on lookup.
    pub fn offered_at_ms(&self) -> Option<u64> {
        self.0.offered_at_ms
    }
    /// Acquires only an existing charged source allocation, never storage I/O.
    pub fn resident_envelope(&self) -> Result<Arc<Envelope>> {
        self.0.row.envelope.resident()
    }
    /// Qualifies canonical storage through its trusted port. Missing/corrupt or
    /// foreign facts cannot yield a commit; original clocks may predate acceptance
    /// after rollback, but may not exceed the original finite TTL boundary.
    pub fn verify(
        self,
        store: &mut impl MessageNativeInputStore,
    ) -> Result<Option<MessageNativeInputCommit>> {
        let Some(record) = store.verify(&self)? else {
            return Ok(None);
        };
        if record.archive_sequence == 0
            || record.store_id.iter().all(|b| *b == 0)
            || record.occurrence.iter().all(|b| *b == 0)
            || record.canonical_sha256.iter().all(|b| *b == 0)
            || self.0.row.envelope.ttl_ms.is_some_and(|ttl| {
                self.0.row.accepted_at_ms.saturating_add(ttl) < record.offered_at_ms
            })
        {
            return Err(unavailable());
        }
        Ok(Some(MessageNativeInputCommit {
            anchor: self,
            record,
        }))
    }
}

impl MessageService {
    /// Reports the actual live storage policy; saved/wire values cannot select it.
    pub fn native_input_storage_required(&self) -> bool {
        self.storage_custody.is_some()
    }

    /// Selects one unACKed frozen native slot in stable source/slot order for
    /// original canonical-proof lookup. No body, offer or live endpoint is
    /// required. `after` is only bounded scan progress, never delivery authority.
    pub fn next_native_input_lookup(
        &self,
        after: Option<(u64, usize)>,
    ) -> Result<Option<MessageNativeInputAnchor>> {
        for row in self.pending.values() {
            for index in 0..row.recipient_bindings.len() {
                if after.is_some_and(|cursor| (row.sequence, index) <= cursor)
                    || row
                        .native_bindings
                        .as_ref()
                        .and_then(|n| n.recipients.get(index))
                        .is_none_or(Option::is_none)
                    || row
                        .acknowledged
                        .lock()
                        .map_err(|_| unavailable())?
                        .get(index)
                        .copied()
                        .unwrap_or(true)
                    || row
                        .terminal
                        .lock()
                        .map_err(|_| unavailable())?
                        .get(index)
                        .is_none_or(Option::is_some)
                {
                    continue;
                }
                return self.native_input_anchor(row, index, None).map(Some);
            }
        }
        Ok(None)
    }

    /// Checks the original slot's still-current native owner independently of
    /// presence, TTL or transport settlement. This can route actual canonical
    /// archive observations, but never revives a stale recipient namespace.
    pub fn native_input_owner_is_current(&self, source: &MessageNativeInputAnchor) -> bool {
        self.native_input_anchor_is_original(source)
            && self.accepted_recipient(&source.0.row, source.recipient())
    }

    /// Tests current delivery eligibility for an original lookup without
    /// reviving a registration, renewing TTL or manufacturing a genuine offer.
    pub fn native_input_can_receive(&self, source: &MessageNativeInputAnchor, now_ms: u64) -> bool {
        self.native_input_anchor_is_original(source)
            && source
                .0
                .row
                .acknowledged
                .lock()
                .is_ok_and(|slots| slots.get(source.0.index) == Some(&false))
            && source
                .0
                .row
                .terminal
                .lock()
                .is_ok_and(|slots| slots.get(source.0.index).is_some_and(Option::is_none))
            && self.accepted_recipient(&source.0.row, source.recipient())
            && self.recipient_is_available(source.recipient())
            && self.native_delivery_is_available(source.recipient())
            && !expired(&source.0.row, now_ms)
            && self
                .subscriptions
                .get(source.recipient())
                .is_none_or(|c| c.last_sequence < source.sequence())
    }

    /// Genuinely selects the first current source after qualified absence.
    /// Cold payloads remain absent; the caller must use the existing owned read.
    pub fn select_native_input(
        &self,
        source: &MessageNativeInputAnchor,
        now_ms: u64,
    ) -> Result<Option<MessageNativeInputAnchor>> {
        if !self.native_input_can_receive(source, now_ms) || source.resident_envelope().is_err() {
            return Ok(None);
        }
        let batch = self.receive_subscribed(source.recipient(), now_ms, 1)?;
        if !batch
            .messages
            .iter()
            .any(|m| m.sequence == source.sequence())
        {
            return Ok(None);
        }
        self.capture_native_input_anchor(source.recipient(), source.sequence())
            .map(Some)
    }

    /// Captures an actual offered current native source after its application
    /// owner has committed input. This source anchor alone grants no durable ACK.
    pub fn capture_native_input_anchor(
        &self,
        recipient: &AgentId,
        sequence: u64,
    ) -> Result<MessageNativeInputAnchor> {
        let row = self.pending.get(&sequence).ok_or_else(unavailable)?;
        if !self.accepted_recipient(row, recipient)
            || !self.recipient_is_available(recipient)
            || !self.native_delivery_is_available(recipient)
        {
            return Err(unavailable());
        }
        let index = row
            .recipient_bindings
            .iter()
            .position(|(id, _)| id == recipient)
            .ok_or_else(unavailable)?;
        let offered = super::offers::offered_at(row, recipient)?.ok_or_else(unavailable)?;
        self.native_input_anchor(row, index, Some(offered))
    }

    /// Captures frozen native source metadata for canonical-proof lookup only.
    /// An unavailable/replaced endpoint needs no new offer or live registration;
    /// it cannot create fresh canonical input merely by reading this anchor.
    pub fn capture_native_input_lookup(
        &self,
        sequence: u64,
        binding_index: usize,
    ) -> Result<MessageNativeInputAnchor> {
        let row = self.pending.get(&sequence).ok_or_else(unavailable)?;
        self.native_input_anchor(row, binding_index, None)
    }

    /// Validates the original service/source allocation without resolving its
    /// current replacement. Matching labels or decoded records grant no rights.
    pub fn native_input_anchor_is_original(&self, anchor: &MessageNativeInputAnchor) -> bool {
        Arc::ptr_eq(&self.acceptance_runtime, &anchor.0.runtime)
            && self
                .storage_custody
                .as_ref()
                .is_some_and(|p| p.scope == anchor.0.scope)
            && self
                .pending
                .get(&anchor.sequence())
                .is_some_and(|row| Arc::ptr_eq(row, &anchor.0.row))
    }

    /// Precharges all copied attribution before allocating a retained anchor.
    fn native_input_anchor(
        &self,
        row: &Arc<QueuedEnvelope>,
        index: usize,
        offered_at_ms: Option<u64>,
    ) -> Result<MessageNativeInputAnchor> {
        let policy = self.storage_custody.as_ref().ok_or_else(unavailable)?;
        let native = row
            .native_bindings
            .as_ref()
            .and_then(|n| n.recipients.get(index))
            .and_then(Option::as_ref)
            .ok_or_else(unavailable)?;
        let (recipient, _) = row.recipient_bindings.get(index).ok_or_else(unavailable)?;
        if row
            .acknowledged
            .lock()
            .map_err(|_| unavailable())?
            .get(index)
            .is_none_or(|ack| *ack)
            || row
                .terminal
                .lock()
                .map_err(|_| unavailable())?
                .get(index)
                .is_none_or(Option::is_some)
        {
            return Err(unavailable());
        }
        let digest = row.envelope.storage_digest(&policy.scope)?;
        // The proof may outlive both pending custody and replay retention.
        // Charge its retained original source as well as copied attribution;
        // dropping the ledger owner must not leave this Arc unaccounted.
        let bytes = super::storage_policy::source_metadata_bytes(row)
            .saturating_add(4096)
            .saturating_add(
                8usize.saturating_mul(
                    recipient
                        .as_str()
                        .len()
                        .saturating_add(native.conversation_id().len())
                        .saturating_add(native.incarnation().len()),
                ),
            );
        let lease = checkpoint::reserve_checkpoint(
            policy.checkpoint_bytes.clone(),
            bytes,
            self.storage_metadata_base_usage()?,
            policy.limits.metadata_bytes,
        )?;
        let protection = {
            let mut slots = row
                .native_input_protection
                .lock()
                .map_err(|_| unavailable())?;
            let slot = slots.get_mut(index).ok_or_else(unavailable)?;
            let protection = slot.upgrade().unwrap_or_else(|| Arc::new(()));
            *slot = Arc::downgrade(&protection);
            protection
        };
        Ok(MessageNativeInputAnchor(Arc::new(Source {
            runtime: self.acceptance_runtime.clone(),
            scope: policy.scope.clone(),
            row: row.clone(),
            index,
            digest,
            native: native.clone(),
            offered_at_ms,
            _protection: protection,
            _lease: lease,
        })))
    }
}

/// Content-free refusal never exposes a private native identity or source body.
fn unavailable() -> MessageError {
    MessageError::invalid_state("MMP original native input unavailable")
}

#[cfg(test)]
mod tests;

/// Allocates only the original frozen slot count, before canonical source install.
pub(super) fn protection_slots(count: usize) -> std::sync::Mutex<Vec<std::sync::Weak<()>>> {
    std::sync::Mutex::new(
        std::iter::repeat_with(std::sync::Weak::new)
            .take(count)
            .collect(),
    )
}

/// A poisoned protection slot fails closed by retaining accepted custody.
pub(super) fn protects(row: &QueuedEnvelope, recipient: &AgentId) -> bool {
    let Some(index) = row
        .recipient_bindings
        .iter()
        .position(|(id, _)| id == recipient)
    else {
        return false;
    };
    row.native_input_protection.lock().map_or(true, |slots| {
        slots.get(index).is_none_or(|slot| slot.strong_count() > 0)
    })
}
