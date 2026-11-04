//! Original-slot expiry ownership before durable terminal publication.
//!
//! A reservation retains one accepted source and at most64 unacknowledged,
//! unprotected recipient slots. It grants neither expiry nor ACK progress.
//! Only a trusted store's actual positive result can publish the frozen first
//! observation into those slots. The same source, clock and indices survive
//! uncertainty or joined worker loss; retries only replace their attempt nonce.
//! Source metadata remains charged through the last ticket/write/positive owner,
//! independently of later custody/replay eviction. Bodies are never copied here.
//! Product journal validation, real joins, cold recovery and elapsed-clock policy
//! remain the concrete adapter's responsibility; no decoder creates this owner.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

mod ownership;
mod selection;

/// Maximum slots in one source-bound terminal operation.
pub const MESSAGE_EXPIRY_SLOT_LIMIT: usize = 64;
/// Fixed source/clock framing followed by one u32 per original slot.
const HEADER_BYTES: usize = 60;
/// Bounds retained control data and future encoding independently of payload.
const MAX_METADATA: usize = 8 * 1024 * 1024;

/// One actual source/reservation identity, never reconstructed from framing.
struct Identity {
    owner: Arc<()>,
    scope: MessageStorageScope,
    source: Arc<QueuedEnvelope>,
    digest: [u8; 32],
    observed_at_ms: u64,
    indices: Vec<u32>,
    metadata_bytes: usize,
    decode_payload_bytes: usize,
    published: AtomicBool,
    _lease: MessageStorageSnapshotLease,
}
impl std::fmt::Debug for Identity {
    /// Logs counts only, without source bodies or private native attribution.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExpiryIdentity")
            .field("sequence", &self.source.sequence)
            .field("slots", &self.indices.len())
            .finish()
    }
}

/// The existing canonical service retains every unpublished expiry operation.
#[derive(Debug)]
pub(in crate::messaging) struct PendingExpiry {
    identity: Arc<Identity>,
    attempt: Option<Arc<()>>,
}

/// Exact reserved source and slots; allocation alone is not a disposition.
#[derive(Debug, Clone)]
pub struct MessageExpiryTicket {
    identity: Arc<Identity>,
}
/// One original physical write capability, moved through the trusted store.
pub struct MessageExpiryWrite {
    identity: Arc<Identity>,
    attempt: Arc<()>,
    payload: super::payload_budget::PayloadReservation,
}
impl std::fmt::Debug for MessageExpiryWrite {
    /// Decoder reservations and source/private metadata stay out of diagnostics.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MessageExpiryWrite")
            .field("identity", &self.identity)
            .finish()
    }
}
/// Payload-free actual-worker escrow for exact joined-loss recovery.
#[derive(Clone)]
pub struct MessageExpiryAttempt {
    identity: Arc<Identity>,
    attempt: Arc<()>,
    payload: super::payload_budget::PayloadReservation,
}
impl std::fmt::Debug for MessageExpiryAttempt {
    /// A nonce comparison is the only observer of private actual-worker escrow.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MessageExpiryAttempt")
            .field("identity", &self.identity)
            .finish()
    }
}
/// Positive trusted-store result; inert until the original service adopts it.
#[derive(Debug)]
pub struct MessageExpiryCommit {
    identity: Arc<Identity>,
    attempt: Arc<()>,
}
/// Uncertainty retains the same linear write rather than asserting noncommit.
#[derive(Debug)]
pub struct MessageExpiryUncertain {
    write: MessageExpiryWrite,
    error: MessageError,
}
/// Errors cannot release the original slots or discard a possible disk commit.
#[derive(Debug)]
pub enum MessageExpiryWriteOutcome {
    /// The trusted synchronous store completed the original operation.
    Committed(MessageExpiryCommit),
    /// Commit may exist; reconcile the same original after actual worker join.
    Unknown(MessageExpiryUncertain),
}

/// Trusted synchronous original-store boundary, invoked off the actor.
pub trait MessageExpiryStore {
    /// Commits/reconciles exact immutable bytes and source/TTL/slot qualification.
    /// Success must not mean merely that a record was queued or decoded.
    fn commit(&mut self, write: &MessageExpiryWrite) -> Result<()>;
}
/// Trusted nonblocking observation of an already joined exact physical worker.
pub trait MessageExpiryJoin {
    /// Rejects a foreign or still-running attempt without granting new work.
    fn qualify_joined(&mut self, attempt: &MessageExpiryAttempt) -> Result<()>;
}

impl MessageExpiryTicket {
    /// Returns the complete reserved logical metadata and encoding charge.
    pub fn metadata_bytes(&self) -> usize {
        self.identity.metadata_bytes
    }
}
impl MessageExpiryAttempt {
    /// Requires both the actual original allocation and exact attempt nonce.
    pub fn same_attempt(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity) && Arc::ptr_eq(&self.attempt, &other.attempt)
    }
}
impl MessageExpiryUncertain {
    /// Borrows a diagnostic; no error category establishes nonexecution.
    pub fn error(&self) -> &MessageError {
        &self.error
    }
}
impl MessageExpiryWrite {
    /// Keeps exact worker escrow outside an abortable physical observer.
    pub fn attempt(&self) -> MessageExpiryAttempt {
        MessageExpiryAttempt {
            identity: self.identity.clone(),
            attempt: self.attempt.clone(),
            payload: self.payload.clone(),
        }
    }
    /// Requires the original live storage-root scope, not equal source labels.
    pub fn belongs_to(&self, scope: &MessageStorageScope) -> bool {
        self.identity.scope == *scope
    }
    /// Returns the accepted original occurrence, never a current selector.
    pub fn sequence(&self) -> MessageSequence {
        self.identity.source.sequence
    }
    /// Returns the first frozen strict-expiry observation; retry never renews it.
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
        HEADER_BYTES + 4 * self.identity.indices.len()
    }
    /// Writes bodyless version1 expiry data. These bytes are descriptive and
    /// cannot construct a source, positive result, sender receipt or live owner.
    pub fn write_record(&self, writer: &mut impl Write) -> io::Result<()> {
        writer.write_all(b"MMPEXP01")?;
        writer.write_all(&self.sequence().to_le_bytes())?;
        writer.write_all(&self.observed_at_ms().to_le_bytes())?;
        writer.write_all(&(self.identity.indices.len() as u32).to_le_bytes())?;
        writer.write_all(&self.identity.digest)?;
        for index in &self.identity.indices {
            writer.write_all(&index.to_le_bytes())?;
        }
        Ok(())
    }
    /// Invokes actual trusted I/O before sealing a positive. All failures retain
    /// this exact original, including its first clock, source and metadata lease.
    pub fn persist(self, store: &mut impl MessageExpiryStore) -> MessageExpiryWriteOutcome {
        match store.commit(&self) {
            Ok(()) => MessageExpiryWriteOutcome::Committed(MessageExpiryCommit {
                identity: self.identity,
                attempt: self.attempt,
            }),
            Err(error) => {
                MessageExpiryWriteOutcome::Unknown(MessageExpiryUncertain { write: self, error })
            }
        }
    }
}

/// Refuses stale/foreign/unavailable ownership without logging authored data.
fn changed() -> MessageError {
    MessageError::invalid_state("MMP original expiry ownership unavailable")
}
/// Reports bounded pre-issuance refusal without changing any source slot.
fn capacity() -> MessageError {
    MessageError::new(MessageErrorKind::Capacity, "MMP expiry metadata capacity")
}

#[cfg(test)]
mod tests;
