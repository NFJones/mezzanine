//! Storage-qualified ACK reservation on the canonical pending delivery owner.
//!
//! Preparation freezes the offered cumulative prefix, exact registration and
//! original offer clock without committing slots or cursor. Payload remains in
//! ordinary pending custody; tickets/facts contain only bounded metadata. The
//! reservation protects its slots from terminal maintenance and ordinary ACK
//! bypass. Expiry exclusions prevent new clock-rollback offers while unresolved.
//! Positive publication applies the frozen decision without resolving selectors
//! or reviving a retired subscription. Application commit is still the trusted
//! caller's prerequisite; an offer alone does not prove native context commit.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

/// Bounded logical metadata and worker-encoding headroom per original ACK.
const MAX_METADATA: usize = 512 * 1024;
/// Leaves room for key/checksum/framing inside the256KiB storage batch bound.
const MAX_RECORD: usize = 240 * 1024;
/// Existing aggregate accepted-binding ceiling also bounds a cumulative prefix.
const MAX_ENTRIES: usize = 4096;
/// Version2 per-entry framing, including the exact original offer clock.
const RECORD_ENTRY_BYTES: usize = 53;

/// One exact pending occurrence decision, not application-commit authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageAcknowledgementEntry {
    /// Canonical accepted occurrence sequence.
    pub sequence: MessageSequence,
    /// Frozen original recipient binding slot, not current selector order.
    pub binding_index: u32,
    /// True means offered recipient commit; false preserves an expiry exclusion.
    pub acknowledged: bool,
    /// Exact original selection clock for a positive decision; expiry
    /// exclusions have no offer and must not invent one.
    pub offered_at_ms: Option<u64>,
}

/// Payload-free original shared by tickets, writes and positive completion.
#[derive(Debug)]
struct Identity {
    /// Same live service allocation, never restored from serialized metadata.
    owner: Arc<()>,
    /// Original recipient and returned cursor, retained through retirement.
    cursor: DeliveryCursor,
    /// Exact original registration interval.
    floor: MessageSequence,
    /// Actual original cursor before this cumulative ACK.
    after: MessageSequence,
    /// Original endpoint selection clock; later offers do not rebase expiry.
    observed_at_ms: u64,
    /// Ordered bounded immutable decisions, with no envelope references.
    entries: Vec<MessageAcknowledgementEntry>,
    /// Full logical metadata plus bounded future worker encoding charge.
    metadata_bytes: usize,
    /// Original actor publication fact, independent of later replay eviction.
    published: AtomicBool,
}

/// One reservation belongs to the same message owner, never another mailbox.
#[derive(Debug)]
pub(in crate::messaging) struct PendingAcknowledgement {
    /// Original registration and cumulative decision retained before issuance.
    identity: Arc<Identity>,
    /// At most one actual issued worker attempt.
    attempt: Option<Arc<()>>,
}
impl PendingAcknowledgement {
    /// Returns original caller address for synchronous ACK bypass fencing.
    pub(super) fn recipient(&self) -> &AgentId {
        &self.identity.cursor.recipient
    }
}

/// Payload-free unpublished ACK capability; allocation is not progress.
#[derive(Debug, Clone)]
pub struct MessageAcknowledgementTicket {
    identity: Arc<Identity>,
}
/// Linear original write; neither identity nor payload is regenerated on retry.
pub struct MessageAcknowledgementWrite {
    identity: Arc<Identity>,
    attempt: Arc<()>,
    /// Shared canonical sources stay charged in pending custody until commit.
    /// Success drops these references before returning a payload-free fact.
    sources: Vec<Arc<QueuedEnvelope>>,
}
impl std::fmt::Debug for MessageAcknowledgementWrite {
    /// Redacts shared source payload and frozen identity details from diagnostics.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MessageAcknowledgementWrite")
            .field("endpoint", &self.identity.cursor.last_sequence)
            .field("entries", &self.identity.entries.len())
            .finish_non_exhaustive()
    }
}

/// Exact actual-worker escrow, without payload or another write capability.
#[derive(Debug, Clone)]
pub struct MessageAcknowledgementAttempt {
    identity: Arc<Identity>,
    attempt: Arc<()>,
}
/// Opaque positive storage fact, available only after the trusted store returns.
#[derive(Debug)]
pub struct MessageAcknowledgementCommit {
    identity: Arc<Identity>,
    attempt: Arc<()>,
}
/// Joined ambiguous outcome retaining the same linear original for reconciliation.
#[derive(Debug)]
pub struct MessageAcknowledgementUncertain {
    write: MessageAcknowledgementWrite,
    error: MessageError,
}
/// Qualified store completion; all errors retain the original and its charge.
#[derive(Debug)]
pub enum MessageAcknowledgementWriteOutcome {
    /// Actual original ACK record committed, awaiting exact actor adoption.
    Committed(MessageAcknowledgementCommit),
    /// Actual commit may exist; retain input and reconcile the same occurrence.
    Unknown(MessageAcknowledgementUncertain),
}
/// Trusted synchronous storage boundary; finish owned I/O before returning.
/// Success requires exact canonical ACK commit, never a saved label alone.
/// Errors cannot establish noncommit or release an already committed application.
pub trait MessageAcknowledgementStore {
    /// Commits or reconciles one frozen original before reporting success.
    fn commit(&mut self, record: &MessageAcknowledgementWrite) -> Result<()>;
}
/// Nonblocking actual join observation; generic timeout/status is insufficient.
pub trait MessageAcknowledgementJoin {
    /// Qualifies only an already observed exact owned worker join, without I/O.
    fn qualify_joined(&mut self, attempt: &MessageAcknowledgementAttempt) -> Result<()>;
}
impl MessageAcknowledgementTicket {
    /// Returns complete metadata and future encoding charge, never payload bytes.
    pub fn metadata_bytes(&self) -> usize {
        self.identity.metadata_bytes
    }
}
impl MessageAcknowledgementAttempt {
    /// Compares exact original allocation and attempt rather than public IDs.
    pub fn same_attempt(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity) && Arc::ptr_eq(&self.attempt, &other.attempt)
    }
}
impl MessageAcknowledgementUncertain {
    /// Borrows diagnostic context; its error kind never proves nonexecution.
    pub fn error(&self) -> &MessageError {
        &self.error
    }
}
impl MessageAcknowledgementWrite {
    /// Captures exact payload-free worker escrow before transferring linear input.
    pub fn attempt(&self) -> MessageAcknowledgementAttempt {
        MessageAcknowledgementAttempt {
            identity: self.identity.clone(),
            attempt: self.attempt.clone(),
        }
    }
    /// Borrows the original recipient, not a current registry projection.
    pub fn recipient(&self) -> &AgentId {
        &self.identity.cursor.recipient
    }
    /// Returns the frozen offered endpoint, including an otherwise empty prefix.
    pub fn endpoint_sequence(&self) -> MessageSequence {
        self.identity.cursor.last_sequence
    }

    /// Returns the exact original registration interval.
    pub fn registered_after_sequence(&self) -> MessageSequence {
        self.identity.floor
    }
    /// Returns immutable occurrence decisions in canonical sequence order.
    pub fn entries(&self) -> impl Iterator<Item = MessageAcknowledgementEntry> + '_ {
        self.identity.entries.iter().copied()
    }
    /// Returns exact version2 encoded size within the reserved worker budget.
    pub fn encoded_bytes(&self) -> usize {
        48 + self.recipient().as_str().len() + RECORD_ENTRY_BYTES * self.identity.entries.len()
    }
    /// Streams fixed version2 framing with source fingerprints on the owned
    /// blocking worker; hashing must not run on the actor. No full-copy
    /// allocation. Serialization carries no live capability or native authority.
    pub fn write_record(&self, writer: &mut impl Write) -> io::Result<()> {
        writer.write_all(b"MMPACK02")?;
        for value in [
            self.identity.after,
            self.identity.cursor.last_sequence,
            self.identity.floor,
            self.identity.observed_at_ms,
        ] {
            writer.write_all(&value.to_le_bytes())?;
        }
        writer.write_all(&(self.recipient().as_str().len() as u32).to_le_bytes())?;
        writer.write_all(&(self.identity.entries.len() as u32).to_le_bytes())?;
        writer.write_all(self.recipient().as_str().as_bytes())?;
        for (entry, source) in self.identity.entries.iter().zip(&self.sources) {
            writer.write_all(&entry.sequence.to_le_bytes())?;
            writer.write_all(&entry.binding_index.to_le_bytes())?;
            writer.write_all(&[u8::from(entry.acknowledged)])?;
            writer.write_all(&entry.offered_at_ms.unwrap_or_default().to_le_bytes())?;
            writer.write_all(
                &super::admission::acknowledgement_source_digest(source)
                    .map_err(io::Error::other)?,
            )?;
        }
        Ok(())
    }
    /// Runs the original through the trusted actual store, retaining uncertainty.
    pub fn persist(
        self,
        store: &mut impl MessageAcknowledgementStore,
    ) -> MessageAcknowledgementWriteOutcome {
        match store.commit(&self) {
            Ok(()) => MessageAcknowledgementWriteOutcome::Committed(MessageAcknowledgementCommit {
                identity: self.identity,
                attempt: self.attempt,
            }),
            Err(error) => {
                MessageAcknowledgementWriteOutcome::Unknown(MessageAcknowledgementUncertain {
                    write: self,
                    error,
                })
            }
        }
    }
}

impl MessageService {
    /// Reports a reserved or issued original ACK for trusted replacement fencing.
    /// This is observational state, never commit, absence or actual-join evidence.
    pub fn has_unpublished_acknowledgement(&self) -> bool {
        self.acknowledgement.is_some()
    }

    /// Freezes one valid offered prefix without advancing any slot or cursor.
    /// None is an inert duplicate/regressive ACK. Bounded preflight errors change
    /// no state. Caller must prove canonical/application commit before calling.
    pub fn prepare_acknowledgement(
        &mut self,
        recipient: &AgentId,
        sequence: MessageSequence,
        metadata_budget: usize,
    ) -> Result<Option<MessageAcknowledgementTicket>> {
        if sequence > self.last_sequence() {
            return Err(MessageError::invalid_args(
                "ACK exceeds accepted high-water",
            ));
        }
        let cursor = self
            .subscriptions
            .get(recipient)
            .ok_or_else(|| MessageError::forbidden("agent has no delivery subscription"))?;
        if sequence <= cursor.last_sequence {
            return Ok(None);
        }
        if self.acknowledgement.is_some() {
            return Err(capacity());
        }
        self.validate_delivery_ack(recipient, cursor.last_sequence, sequence)?;
        let floor = *self
            .registration_floors
            .get(recipient)
            .ok_or_else(changed)?;
        let endpoint = self.queued_by_sequence.get(&sequence).ok_or_else(changed)?;
        let observed_at_ms = super::offers::offered_at(endpoint, recipient)?.ok_or_else(changed)?;
        let mut entries = Vec::new();
        let budget = metadata_budget.min(MAX_METADATA);
        let fixed = std::mem::size_of::<Identity>()
            + 256
            + 2 * std::mem::size_of::<usize>()
            + recipient.as_str().len();
        if fixed > budget {
            return Err(capacity());
        }
        for row in self
            .pending
            .range((
                std::ops::Bound::Excluded(cursor.last_sequence),
                std::ops::Bound::Included(sequence),
            ))
            .map(|(_, row)| row)
        {
            if !self.accepted_recipient(row, recipient)
                || self.recipient_is_terminal(row, recipient)
            {
                continue;
            }
            let index = row
                .recipient_bindings
                .iter()
                .position(|(id, _)| id == recipient)
                .ok_or_else(changed)?;
            let acknowledged = row.acknowledged.lock().map_err(|_| changed())?;
            if *acknowledged.get(index).ok_or_else(changed)? {
                continue;
            }
            drop(acknowledged);
            let offered_at_ms = super::offers::offered_at(row, recipient)?;
            if offered_at_ms.is_none() && !expired(row, observed_at_ms) {
                return Err(changed());
            }
            if entries.len() == MAX_ENTRIES {
                return Err(capacity());
            }
            entries.push(MessageAcknowledgementEntry {
                sequence: row.sequence,
                binding_index: u32::try_from(index).map_err(|_| capacity())?,
                acknowledged: offered_at_ms.is_some(),
                offered_at_ms,
            });
            let encoded = 48 + recipient.as_str().len() + RECORD_ENTRY_BYTES * entries.len();
            if encoded > MAX_RECORD
                || fixed
                    + encoded
                    + entries.capacity() * std::mem::size_of::<MessageAcknowledgementEntry>()
                    + entries.len() * std::mem::size_of::<Arc<QueuedEnvelope>>()
                    > budget
            {
                return Err(capacity());
            }
        }
        let encoded = 48 + recipient.as_str().len() + RECORD_ENTRY_BYTES * entries.len();
        let metadata_bytes = fixed
            + encoded
            + entries.capacity() * std::mem::size_of::<MessageAcknowledgementEntry>()
            + entries.len() * std::mem::size_of::<Arc<QueuedEnvelope>>();
        if encoded > MAX_RECORD || metadata_bytes > budget {
            return Err(capacity());
        }
        if entries
            .last()
            .is_none_or(|entry| entry.sequence != sequence || !entry.acknowledged)
        {
            return Err(changed());
        }
        let identity = Arc::new(Identity {
            owner: self.acceptance_runtime.clone(),
            cursor: DeliveryCursor {
                recipient: recipient.clone(),
                last_sequence: sequence,
            },
            floor,
            after: cursor.last_sequence,
            observed_at_ms,
            entries,
            metadata_bytes,
            published: AtomicBool::new(false),
        });
        self.acknowledgement = Some(PendingAcknowledgement {
            identity: identity.clone(),
            attempt: None,
        });
        Ok(Some(MessageAcknowledgementTicket { identity }))
    }

    /// Issues exactly one original after reserving all metadata and prefix slots.
    pub fn begin_acknowledgement_write(
        &mut self,
        ticket: &MessageAcknowledgementTicket,
    ) -> Result<MessageAcknowledgementWrite> {
        if !Arc::ptr_eq(&ticket.identity.owner, &self.acceptance_runtime) {
            return Err(changed());
        }
        let pending = self.acknowledgement.as_ref().ok_or_else(changed)?;
        if !Arc::ptr_eq(&pending.identity, &ticket.identity) || pending.attempt.is_some() {
            return Err(changed());
        }
        let sources = self.acknowledgement_sources(&ticket.identity)?;
        self.replace_acknowledgement_attempt(sources)
    }
    /// Validates the original current attempt before consuming uncertain input.
    pub fn validate_acknowledgement_attempt(
        &self,
        attempt: &MessageAcknowledgementAttempt,
    ) -> Result<()> {
        self.require_acknowledgement(&attempt.identity, &attempt.attempt)
    }
    /// Reconciles one joined uncertain original with a fresh attempt nonce.
    pub fn retry_acknowledgement_write(
        &mut self,
        unknown: MessageAcknowledgementUncertain,
    ) -> Result<MessageAcknowledgementWrite> {
        self.require_acknowledgement(&unknown.write.identity, &unknown.write.attempt)?;
        self.replace_acknowledgement_attempt(unknown.write.sources)
    }
    /// Reissues lost input only after the exact actual worker join was observed.
    pub fn recover_joined_acknowledgement_write(
        &mut self,
        attempt: &MessageAcknowledgementAttempt,
        joined: &mut impl MessageAcknowledgementJoin,
    ) -> Result<MessageAcknowledgementWrite> {
        self.require_acknowledgement(&attempt.identity, &attempt.attempt)?;
        joined.qualify_joined(attempt)?;
        let identity = self
            .acknowledgement
            .as_ref()
            .ok_or_else(changed)?
            .identity
            .clone();
        let sources = self.acknowledgement_sources(&identity)?;
        self.replace_acknowledgement_attempt(sources)
    }
    /// Cancels only a never-issued original; issued uncertainty stays charged.
    pub fn cancel_acknowledgement(&mut self, ticket: &MessageAcknowledgementTicket) -> Result<()> {
        if !Arc::ptr_eq(&ticket.identity.owner, &self.acceptance_runtime) {
            return Err(changed());
        }
        let pending = self.acknowledgement.as_ref().ok_or_else(changed)?;
        if !Arc::ptr_eq(&ticket.identity, &pending.identity) || pending.attempt.is_some() {
            return Err(changed());
        }
        self.acknowledgement = None;
        Ok(())
    }
    /// Applies only the frozen committed decisions. Retired/replacement recipients
    /// cannot inherit a cursor; duplicate positive facts never mutate later state.
    pub fn publish_acknowledgement(
        &mut self,
        commit: &MessageAcknowledgementCommit,
    ) -> Result<DeliveryCursor> {
        if !Arc::ptr_eq(&commit.identity.owner, &self.acceptance_runtime) {
            return Err(changed());
        }
        if commit.identity.published.load(Ordering::Acquire) {
            return Ok(commit.identity.cursor.clone());
        }
        self.require_acknowledgement(&commit.identity, &commit.attempt)?;
        let identity = commit.identity.clone();
        let mut acknowledgements = Vec::new();
        let mut exclusions = Vec::new();
        for entry in &identity.entries {
            let row = self.pending.get(&entry.sequence).ok_or_else(changed)?;
            let index = entry.binding_index as usize;
            if row
                .recipient_bindings
                .get(index)
                .is_none_or(|(recipient, floor)| {
                    recipient != &identity.cursor.recipient || *floor != identity.floor
                })
            {
                return Err(changed());
            }
            if row
                .terminal
                .lock()
                .map_err(|_| changed())?
                .get(index)
                .is_none_or(Option::is_some)
            {
                return Err(changed());
            }
            if entry.acknowledged {
                let slots = row.acknowledged.lock().map_err(|_| changed())?;
                if index >= slots.len() {
                    return Err(changed());
                }
                acknowledgements.push((slots, index));
            } else {
                exclusions.push(
                    row.ack_expiry_observed_at_ms
                        .lock()
                        .map_err(|_| changed())?,
                );
            }
        }
        for (mut slots, index) in acknowledgements {
            slots[index] = true;
        }
        for mut exclusion in exclusions {
            *exclusion = Some(exclusion.map_or(identity.observed_at_ms, |prior| {
                prior.max(identity.observed_at_ms)
            }));
        }
        if self.registration_floors.get(&identity.cursor.recipient) == Some(&identity.floor)
            && let Some(cursor) = self.subscriptions.get_mut(&identity.cursor.recipient)
        {
            cursor.last_sequence = cursor.last_sequence.max(identity.cursor.last_sequence);
        }
        identity.published.store(true, Ordering::Release);
        self.acknowledgement = None;
        self.release_completed_pending()?;
        Ok(identity.cursor.clone())
    }
    /// Protects only frozen recipient slots; global TTL exclusions protect all
    /// recipients of that original until their observation can be published.
    pub(super) fn acknowledgement_protects(
        &self,
        sequence: MessageSequence,
        recipient: &AgentId,
    ) -> bool {
        self.acknowledgement.as_ref().is_some_and(|pending| {
            pending
                .identity
                .entries
                .binary_search_by_key(&sequence, |entry| entry.sequence)
                .ok()
                .is_some_and(|index| {
                    !pending.identity.entries[index].acknowledged
                        || pending.recipient() == recipient
                })
        })
    }
    /// Prevents clock rollback from selecting a captured expired prefix anew.
    pub(super) fn acknowledgement_excludes(&self, sequence: MessageSequence) -> bool {
        self.acknowledgement.as_ref().is_some_and(|pending| {
            pending
                .identity
                .entries
                .binary_search_by_key(&sequence, |entry| entry.sequence)
                .ok()
                .is_some_and(|index| !pending.identity.entries[index].acknowledged)
        })
    }
    /// Replaces only a previously validated current nonce, preserving original data.
    fn replace_acknowledgement_attempt(
        &mut self,
        sources: Vec<Arc<QueuedEnvelope>>,
    ) -> Result<MessageAcknowledgementWrite> {
        let pending = self.acknowledgement.as_mut().ok_or_else(changed)?;
        let attempt = Arc::new(());
        pending.attempt = Some(attempt.clone());
        Ok(MessageAcknowledgementWrite {
            identity: pending.identity.clone(),
            attempt,
            sources,
        })
    }
    /// Recovers only exact canonical references already protected by this ACK.
    /// No selectors/current registrations are resolved and no payload is copied.
    fn acknowledgement_sources(&self, identity: &Identity) -> Result<Vec<Arc<QueuedEnvelope>>> {
        identity
            .entries
            .iter()
            .map(|entry| {
                let row = self.pending.get(&entry.sequence).ok_or_else(changed)?;
                if row
                    .recipient_bindings
                    .get(entry.binding_index as usize)
                    .is_none_or(|(recipient, floor)| {
                        recipient != &identity.cursor.recipient || *floor != identity.floor
                    })
                {
                    return Err(changed());
                }
                Ok(row.clone())
            })
            .collect()
    }

    /// Checks live owner, original reservation and exact current issued attempt.
    fn require_acknowledgement(&self, identity: &Arc<Identity>, attempt: &Arc<()>) -> Result<()> {
        let pending = self.acknowledgement.as_ref().ok_or_else(changed)?;
        if !Arc::ptr_eq(&identity.owner, &self.acceptance_runtime)
            || !Arc::ptr_eq(identity, &pending.identity)
            || !pending
                .attempt
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, attempt))
        {
            return Err(changed());
        }
        Ok(())
    }
}
/// Bounded preflight refusal without echoing recipient/message content.
fn capacity() -> MessageError {
    MessageError::new(
        MessageErrorKind::Capacity,
        "MMP acknowledgement metadata capacity",
    )
}
/// Lost, stale or corrupt ownership never proves an ACK or nonexecution.
fn changed() -> MessageError {
    MessageError::invalid_state("MMP acknowledgement ownership/evidence changed")
}

#[cfg(test)]
mod tests;
