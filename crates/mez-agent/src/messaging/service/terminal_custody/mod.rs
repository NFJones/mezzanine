//! Source-owned terminal observation, horizon release and sender consumption.
//!
//! One accepted source and at most64 exact original slots share one reservation,
//! nonce, worker escrow and trusted-store positive. Observation freezes first
//! strict expiry; horizon release freezes actual terminal before-images and a
//! release clock. Consumption owns one actual canonical proof and first terminal
//! before-image, without horizon-clock authority. No live slot changes before
//! original adoption. Unknown
//! writes and joined loss retain immutable data and decoder/source metadata
//! through the final owner, independently of pending/replay cleanup. Generic
//! frames confer no source, sender, release or task authority. Live actor/runtime
//! handoff, validated retirement and elapsed-clock policy remain product gates.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

mod consumption;
mod horizon;
mod ownership;
mod retirement;
mod selection;

/// Maximum slots in one source-bound terminal operation.
pub const MESSAGE_TERMINAL_SLOT_LIMIT: usize = 64;
/// Fixed source/clock framing followed by one u32 per original slot.
const HEADER_BYTES: usize = 60;
/// Bounds retained control data and future encoding independently of payload.
const MAX_METADATA: usize = 8 * 1024 * 1024;

/// The exact terminal mutation frozen before issuance and original adoption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageTerminalKind {
    /// Publish the first strict original TTL observation into unsettled slots.
    ExpiryObservation,
    /// Publish one exact ended-native-recipient observation after canonical absence.
    RecipientRetirement,
    /// Release actual terminal outcomes after their finite handoff horizon.
    HorizonRelease,
    /// Consume one original outcome only after actual canonical sender handoff.
    SenderConsumption,
}

/// One actual source/reservation identity, never reconstructed from framing.
struct Identity {
    owner: Arc<()>,
    scope: MessageStorageScope,
    source: Arc<QueuedEnvelope>,
    digest: [u8; 32],
    observed_at_ms: u64,
    kind: MessageTerminalKind,
    indices: Vec<u32>,
    before: Vec<Option<MessageDeliveryTerminalSnapshot>>,
    handoff: Option<MessageTerminalHandoffCommit>,
    retirement: Option<MessageNativeRetirementWitness>,
    metadata_bytes: usize,
    decode_payload_bytes: usize,
    published: AtomicBool,
    _lease: MessageStorageSnapshotLease,
}
impl std::fmt::Debug for Identity {
    /// Logs counts only, without source bodies or private native attribution.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalIdentity")
            .field("sequence", &self.source.sequence)
            .field("slots", &self.indices.len())
            .finish()
    }
}

/// The existing canonical service retains every unpublished terminal operation.
#[derive(Debug)]
pub(in crate::messaging) struct PendingTerminal {
    identity: Arc<Identity>,
    attempt: Option<Arc<()>>,
}

/// Exact reserved source and slots; allocation alone is not a disposition.
#[derive(Debug, Clone)]
pub struct MessageTerminalTicket {
    identity: Arc<Identity>,
}
/// One original physical write capability, moved through the trusted store.
pub struct MessageTerminalWrite {
    identity: Arc<Identity>,
    attempt: Arc<()>,
    payload: super::payload_budget::PayloadReservation,
}
impl std::fmt::Debug for MessageTerminalWrite {
    /// Decoder reservations and source/private metadata stay out of diagnostics.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MessageTerminalWrite")
            .field("identity", &self.identity)
            .finish()
    }
}
/// Payload-free actual-worker escrow for exact joined-loss recovery.
#[derive(Clone)]
pub struct MessageTerminalAttempt {
    identity: Arc<Identity>,
    attempt: Arc<()>,
    payload: super::payload_budget::PayloadReservation,
}
impl std::fmt::Debug for MessageTerminalAttempt {
    /// A nonce comparison is the only observer of private actual-worker escrow.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MessageTerminalAttempt")
            .field("identity", &self.identity)
            .finish()
    }
}
/// Positive trusted-store result; inert until the original service adopts it.
#[derive(Debug)]
pub struct MessageTerminalCommit {
    identity: Arc<Identity>,
    attempt: Arc<()>,
}
/// Uncertainty retains the same linear write rather than asserting noncommit.
#[derive(Debug)]
pub struct MessageTerminalUncertain {
    write: MessageTerminalWrite,
    error: MessageError,
}
/// Errors cannot release the original slots or discard a possible disk commit.
#[derive(Debug)]
pub enum MessageTerminalWriteOutcome {
    /// The trusted synchronous store completed the original operation.
    Committed(MessageTerminalCommit),
    /// Commit may exist; reconcile the same original after actual worker join.
    Unknown(MessageTerminalUncertain),
}

/// Trusted synchronous original-store boundary, invoked off the actor.
pub trait MessageTerminalStore {
    /// Commits/reconciles exact immutable bytes and source/TTL/slot qualification.
    /// Success must not mean merely that a record was queued or decoded.
    fn commit(&mut self, write: &MessageTerminalWrite) -> Result<()>;
}
/// Trusted nonblocking observation of an already joined exact physical worker.
pub trait MessageTerminalJoin {
    /// Rejects a foreign or still-running attempt without granting new work.
    fn qualify_joined(&mut self, attempt: &MessageTerminalAttempt) -> Result<()>;
}

impl MessageTerminalTicket {
    /// Reports the selected original mutation without granting another write.
    pub fn kind(&self) -> MessageTerminalKind {
        self.identity.kind
    }
    /// Returns the complete reserved logical metadata and encoding charge.
    pub fn metadata_bytes(&self) -> usize {
        self.identity.metadata_bytes
    }
}
impl MessageTerminalAttempt {
    /// Requires both the actual original allocation and exact attempt nonce.
    pub fn same_attempt(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity) && Arc::ptr_eq(&self.attempt, &other.attempt)
    }
}
impl MessageTerminalUncertain {
    /// Borrows a diagnostic; no error category establishes nonexecution.
    pub fn error(&self) -> &MessageError {
        &self.error
    }
}
impl MessageTerminalWrite {
    /// Keeps exact worker escrow outside an abortable physical observer.
    pub fn attempt(&self) -> MessageTerminalAttempt {
        MessageTerminalAttempt {
            identity: self.identity.clone(),
            attempt: self.attempt.clone(),
            payload: self.payload.clone(),
        }
    }
    /// Requires the original live storage-root scope, not equal source labels.
    pub fn belongs_to(&self, scope: &MessageStorageScope) -> bool {
        self.identity.scope == *scope
    }
    /// Returns the immutable operation kind selected by this actual owner.
    pub fn kind(&self) -> MessageTerminalKind {
        self.identity.kind
    }
    /// Borrows actual opaque canonical proof only for sender consumption.
    /// Framing or a descriptive record cannot construct this proof.
    pub fn canonical_handoff(&self) -> Option<&MessageTerminalHandoffCommit> {
        self.identity.handoff.as_ref()
    }
    /// Borrows actual canonical-absence/end qualification only for retirement.
    pub fn retired_recipient(&self) -> Option<&MessageNativeRetirementWitness> {
        self.identity.retirement.as_ref()
    }
    /// Borrows original terminal before-images for horizon release. Observation
    /// writes have none; descriptive entries alone cannot grant release rights.
    pub fn terminal_entries(
        &self,
    ) -> impl Iterator<Item = (u32, MessageDeliveryTerminalSnapshot)> + '_ {
        self.identity
            .indices
            .iter()
            .copied()
            .zip(self.identity.before.iter().copied())
            .filter_map(|(index, value)| value.map(|value| (index, value)))
    }
    /// Returns the accepted original occurrence, never a current selector.
    pub fn sequence(&self) -> MessageSequence {
        self.identity.source.sequence
    }
    /// Returns the original operation clock: first terminal observation or frozen release.
    /// Retry never observes a newer clock or changes selected before-images.
    pub fn observed_at_ms(&self) -> u64 {
        self.identity.observed_at_ms
    }
    /// Returns the original acceptance clock covered by the immutable source hash.
    pub fn accepted_at_ms(&self) -> u64 {
        self.identity.source.accepted_at_ms
    }
    /// Borrows the original finite TTL; absence must refuse concrete expiry I/O.
    pub fn ttl_ms(&self) -> Option<u64> {
        self.identity.source.envelope.ttl_ms
    }
    /// Iterates at most64 ordered exact original binding indices.
    pub fn binding_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.identity.indices.iter().copied()
    }
    /// Bounds a concrete decoder to the exact original's already reserved body.
    pub fn payload_bytes(&self) -> usize {
        self.identity.source.envelope.payload_bytes
    }
    /// Bounds source metadata decoding to its exact original recipient count.
    pub fn recipient_count(&self) -> usize {
        self.identity.source.recipient_bindings.len()
    }
    /// Supplies the reserved one-source quantum for physical source/ACK checks.
    /// The adapter must decode sources one at a time and release each body.
    pub fn decode_limits(&self) -> MessageAcceptanceDecodeLimits {
        MessageAcceptanceDecodeLimits {
            payload_bytes: self.identity.decode_payload_bytes,
            auxiliary_bytes: 512 * 1024,
            recipient_count: 4096,
            recipient_bytes: 1024 * 1024,
        }
    }
    /// Returns the fixed bounded physical framing size; no body is encoded.
    pub fn encoded_bytes(&self) -> usize {
        if self.kind() == MessageTerminalKind::SenderConsumption {
            return consumption::ENCODED_BYTES;
        }
        HEADER_BYTES
            + match self.kind() {
                MessageTerminalKind::ExpiryObservation => 4,
                MessageTerminalKind::RecipientRetirement => 84,
                MessageTerminalKind::HorizonRelease => 13,
                MessageTerminalKind::SenderConsumption => 0,
            } * self.identity.indices.len()
    }
    /// Writes bodyless version1 terminal data. These bytes are descriptive and
    /// cannot construct a source, positive result, sender receipt or live owner.
    pub fn write_record(&self, writer: &mut impl Write) -> io::Result<()> {
        writer.write_all(match self.kind() {
            MessageTerminalKind::ExpiryObservation => b"MMPEXP01",
            MessageTerminalKind::RecipientRetirement => b"MMPRET01",
            MessageTerminalKind::HorizonRelease => b"MMPREL01",
            MessageTerminalKind::SenderConsumption => return self.write_consumption_record(writer),
        })?;
        writer.write_all(&self.sequence().to_le_bytes())?;
        writer.write_all(&self.observed_at_ms().to_le_bytes())?;
        writer.write_all(&(self.identity.indices.len() as u32).to_le_bytes())?;
        writer.write_all(&self.identity.digest)?;
        for (slot, index) in self.identity.indices.iter().enumerate() {
            writer.write_all(&index.to_le_bytes())?;
            if self.kind() == MessageTerminalKind::RecipientRetirement {
                let record = self
                    .retired_recipient()
                    .ok_or_else(|| io::Error::other("MMP original retirement witness missing"))?
                    .record();
                writer.write_all(&record.end.registration_namespace)?;
                writer.write_all(&record.lifetime_sha256)?;
                writer.write_all(&record.end_sha256)?;
            }
            if self.kind() == MessageTerminalKind::HorizonRelease {
                let before = self
                    .identity
                    .before
                    .get(slot)
                    .copied()
                    .flatten()
                    .ok_or_else(|| {
                        io::Error::other("MMP original terminal before-image missing")
                    })?;
                writer.write_all(&before.observed_at_ms.to_le_bytes())?;
                writer.write_all(&[match before.reason {
                    DeliveryTerminalReason::Expired => 1,
                    DeliveryTerminalReason::RecipientRetired => 2,
                }])?;
            }
        }
        Ok(())
    }
    /// Invokes actual trusted I/O before sealing a positive. All failures retain
    /// this exact original, including its first clock, source and metadata lease.
    pub fn persist(self, store: &mut impl MessageTerminalStore) -> MessageTerminalWriteOutcome {
        match store.commit(&self) {
            Ok(()) => MessageTerminalWriteOutcome::Committed(MessageTerminalCommit {
                identity: self.identity,
                attempt: self.attempt,
            }),
            Err(error) => MessageTerminalWriteOutcome::Unknown(MessageTerminalUncertain {
                write: self,
                error,
            }),
        }
    }
}

/// Refuses stale/foreign/unavailable ownership without logging authored data.
fn changed() -> MessageError {
    MessageError::invalid_state("MMP original terminal ownership unavailable")
}
/// Reports bounded pre-issuance refusal without changing any source slot.
fn capacity() -> MessageError {
    MessageError::new(MessageErrorKind::Capacity, "MMP terminal metadata capacity")
}

#[cfg(test)]
mod tests;
