//! Unpublished occurrence reservations and trusted storage completion proofs.
//!
//! One reservation belongs to the existing message owner, not another mailbox.
//! It reserves the original sequence, frozen audience/bindings and current
//! finite memory limits without exposing acceptance or delivery. Existing mail
//! can still drain while an owned blocking worker uses a linear write input.
//! Only the trusted synchronous storage port qualifies completion; serialized
//! records and caller error text never constitute publication capabilities.
//!
//! A joined ambiguous result retains the same input for reconciliation. A
//! definitive refusal releases only that exact reservation. Successful adoption
//! publishes once, including after sender/recipient retirement; it never rechecks
//! mutable selectors to replace the original audience. The product must still
//! implement actual disk policy, worker/join ownership and every ingress gate.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

mod record;

/// Conservative logical headroom for actor attempt escrow and shared controls.
const RECEIPT_TRACKING_BYTES: usize = 256;

/// Same-runtime, payload-free identity retained by receipts and tickets.
#[derive(Debug)]
struct AdmissionIdentity {
    /// Original service allocation, never reconstructed from snapshot text.
    owner: Arc<()>,
    /// Original future receipt, inaccessible until actor publication succeeds.
    delivery: Delivery,
    /// Optional already-budgeted request evidence, not a scheduling permission.
    proof: Option<Arc<MessageAcceptanceProof>>,
    /// Monotonic actor publication fact; a late duplicate cannot reinsert mail.
    published: AtomicBool,
}

/// Unpublished state in the single message ledger, including its resource charge.
#[derive(Debug)]
pub(in crate::messaging) struct PendingMessageAdmission {
    /// Shared immutable canonical candidate; no delivery index references it yet.
    row: Arc<QueuedEnvelope>,
    /// Exact original allocation, independent of IDs or mutable registration.
    identity: Arc<AdmissionIdentity>,
    /// One issued write attempt. An ambiguous result cannot free this charge.
    attempt: Option<Arc<()>>,
    /// Original sender retirement observed during the pending write, if any.
    sender_retired_fresh: Option<bool>,
}

impl PendingMessageAdmission {
    /// Supplies the reserved high-water without exposing a future acceptance.
    pub(super) fn sequence(&self) -> MessageSequence {
        self.row.sequence
    }
}

/// Payload-free capability for one unpublished original occurrence.
#[derive(Clone)]
pub struct MessageAcceptanceTicket {
    /// Private live allocation, not caller-constructible or serializable.
    identity: Arc<AdmissionIdentity>,
}

/// Linear worker input for an issued original write. It cannot be cloned.
pub struct MessageAcceptanceWrite {
    /// Exact original allocation shared with the actor's retained reservation.
    identity: Arc<AdmissionIdentity>,
    /// Exact issued attempt, preventing late refusals from releasing another one.
    attempt: Arc<()>,
    /// Shared immutable bytes; serialization borrows them without payload copies.
    row: Arc<QueuedEnvelope>,
}

/// Payload-free actor escrow for one issued attempt and its actual join owner.
#[derive(Clone)]
pub struct MessageAcceptanceAttempt {
    /// Original service/reservation identity, never inferred from public IDs.
    identity: Arc<AdmissionIdentity>,
    /// Exact worker attempt whose termination must be observed before recovery.
    attempt: Arc<()>,
}

/// Trusted nonblocking observation of the exact owned worker's termination.
/// A timeout, dropped observer, generic error or matching message ID is not join
/// evidence. The implementation checks its actual retained worker handle and
/// original attempt; it must not wait, perform I/O or call actor/provider code.
pub trait MessageAcceptanceJoin {
    /// Qualifies an already-observed join, without claiming the write was unsent.
    fn qualify_joined(&mut self, attempt: &MessageAcceptanceAttempt) -> Result<()>;
}

/// Exact positive result of the trusted store boundary, without payload custody.
pub struct MessageAcceptanceCommit {
    /// Positive fact remains attributable after ordinary replay/history evicts.
    identity: Arc<AdmissionIdentity>,
    /// Original joined attempt, not another write with matching public IDs.
    attempt: Arc<()>,
}

/// Joined storage refusal proving this exact attempt committed no acceptance.
pub struct MessageAcceptanceRejected {
    /// Original allocation; no payload is held outside the actor reservation.
    identity: Arc<AdmissionIdentity>,
    /// Exact completed write attempt to be released by the original actor.
    attempt: Arc<()>,
    /// Typed diagnostic, not the source of commit-certainty authority.
    error: MessageError,
}

/// Joined ambiguous write retaining its original linear reconciliation input.
pub struct MessageAcceptanceUncertain {
    /// Same immutable occurrence and attempt, never a newly generated send.
    write: MessageAcceptanceWrite,
    /// Diagnostic only; callers cannot infer absence from this error category.
    error: MessageError,
}

/// Storage disposition, assigned by a trusted adapter rather than error text.
#[derive(Debug)]
pub enum MessageAcceptanceStoreError {
    /// Exact reconciliation proves the original did not commit.
    NotCommitted(MessageError),
    /// Acceptance may have committed; reconcile the same original occurrence.
    Unknown(MessageError),
}

/// Outcome after the synchronous owned storage operation has returned.
#[derive(Debug)]
pub enum MessageAcceptanceWriteOutcome {
    /// Original canonical bytes and receipt have durably committed.
    Committed(MessageAcceptanceCommit),
    /// The original is provably unaccepted and can be released by its owner.
    NotCommitted(MessageAcceptanceRejected),
    /// Retain ownership and reconcile; never fabricate nonexecution.
    Unknown(MessageAcceptanceUncertain),
}

/// Trusted product storage boundary, never implemented by model or wire data.
///
/// The adapter must bind the original store/source, finish owned synchronous I/O
/// before returning, and report success only for an exact idempotent canonical
/// commit. Lost replies reconcile current postimages, not just a saved label.
/// `NotCommitted` requires actual absence/rollback evidence; uncertain I/O is
/// `Unknown`. Implementations must not call the actor or provider. Product
/// offloading, storage/resource limits and backoff remain caller obligations.
pub trait MessageAcceptanceStore {
    /// Commits or reconciles this immutable original before reporting success.
    fn commit(
        &mut self,
        record: &MessageAcceptanceWrite,
    ) -> std::result::Result<(), MessageAcceptanceStoreError>;
}

/// Redacts payloads and avoids displaying an unpublished future receipt as success.
macro_rules! admission_debug {
    ($kind:ty, $($field:ident).+) => {
        impl fmt::Debug for $kind {
            /// Formats only the occurrence identity, never payload or future success.
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                let identity: &AdmissionIdentity = &self.$($field).+;
                formatter.debug_struct(stringify!($kind))
                    .field("message_id", &identity.delivery.message_id)
                    .field("sequence", &identity.delivery.sequence)
                    .finish_non_exhaustive()
            }
        }
    };
}
admission_debug!(MessageAcceptanceTicket, identity);
admission_debug!(MessageAcceptanceWrite, identity);
admission_debug!(MessageAcceptanceCommit, identity);
admission_debug!(MessageAcceptanceAttempt, identity);
admission_debug!(MessageAcceptanceRejected, identity);
admission_debug!(MessageAcceptanceUncertain, write.identity);

impl MessageAcceptanceTicket {
    /// Returns the reserved occurrence sequence, not an acceptance assertion.
    pub fn sequence(&self) -> MessageSequence {
        self.identity.delivery.sequence
    }
    /// Reports retained receipt/proof metadata for caller-owned pool accounting.
    /// The canonical payload remains charged to the message service, not this ticket.
    pub fn metadata_bytes(&self) -> usize {
        std::mem::size_of::<AdmissionIdentity>()
            + RECEIPT_TRACKING_BYTES
            + 2 * std::mem::size_of::<usize>()
            + self.identity.delivery.message_id.capacity()
            + self.identity.proof.as_ref().map_or(0, |proof| {
                proof.metadata_bytes() + 2 * std::mem::size_of::<usize>()
            })
    }
}

impl MessageAcceptanceAttempt {
    /// Compares nonserializable original allocations, not public source labels.
    pub fn same_attempt(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity) && Arc::ptr_eq(&self.attempt, &other.attempt)
    }
    /// Returns the original occurrence, not a join or acceptance assertion.
    pub fn sequence(&self) -> MessageSequence {
        self.identity.delivery.sequence
    }
}

impl MessageAcceptanceWrite {
    /// Runs this linear original through the trusted port. Completion drops the
    /// worker's payload reference except on ambiguity, when it remains pinned.
    pub fn persist(self, store: &mut impl MessageAcceptanceStore) -> MessageAcceptanceWriteOutcome {
        match store.commit(&self) {
            Ok(()) => MessageAcceptanceWriteOutcome::Committed(MessageAcceptanceCommit {
                identity: self.identity,
                attempt: self.attempt,
            }),
            Err(MessageAcceptanceStoreError::NotCommitted(error)) => {
                MessageAcceptanceWriteOutcome::NotCommitted(MessageAcceptanceRejected {
                    identity: self.identity,
                    attempt: self.attempt,
                    error,
                })
            }
            Err(MessageAcceptanceStoreError::Unknown(error)) => {
                MessageAcceptanceWriteOutcome::Unknown(MessageAcceptanceUncertain {
                    write: self,
                    error,
                })
            }
        }
    }

    /// Returns original borrowed input for trusted streaming/storage adapters.
    /// This data grants no publication or reconnect capability.
    pub fn envelope(&self) -> &Envelope {
        &self.row.envelope
    }

    /// Returns the originally reserved occurrence sequence.
    pub fn sequence(&self) -> MessageSequence {
        self.row.sequence
    }
}

impl MessageAcceptanceRejected {
    /// Returns the storage diagnostic after a qualified noncommit result.
    pub fn error(&self) -> &MessageError {
        &self.error
    }
}

impl MessageAcceptanceUncertain {
    /// Returns the error without pretending it proves the original was unsent.
    pub fn error(&self) -> &MessageError {
        &self.error
    }
}

impl MessageService {
    /// Reports a retained unpublished original for trusted scheduler admission.
    /// This is neither an acceptance assertion nor permission to cancel I/O.
    pub fn has_unpublished_acceptance(&self) -> bool {
        self.admission.is_some()
    }

    /// Freezes a fresh original without accepting, offering or delivering it.
    ///
    /// One unpublished reservation fits the existing pending payload/count,
    /// recipient and metadata limits. A successful reservation may discard
    /// already-settled optional replay to honor those budgets, never pending
    /// delivery. The sequence is reserved monotonically even after cancellation.
    /// `receipt_budget` covers the external payload-free ticket/fact; optional
    /// proof capacity is supplied independently. Failure changes no state.
    pub fn prepare_acceptance(
        &mut self,
        sender: &AgentId,
        envelope: Envelope,
        scope: MessageScope,
        now_ms: u64,
        receipt_budget: usize,
        proof_budget: Option<MessageAcceptanceReservation>,
    ) -> Result<MessageAcceptanceTicket> {
        validate_protocol(envelope.protocol)?;
        validate_message_type(&envelope.message_type)?;
        let identity = self
            .registered
            .get(sender)
            .ok_or_else(|| MessageError::forbidden("unregistered agent connection"))?;
        if &envelope.sender != identity {
            return Err(MessageError::forbidden(
                "message sender does not match authenticated agent connection",
            ));
        }
        validate_sender_identity(&envelope.sender)?;
        if super::auxiliary::envelope_bytes(&envelope) > self.pending_limits.auxiliary_bytes {
            return Err(admission_capacity());
        }
        if envelope.id.is_empty()
            || envelope.time.is_empty()
            || envelope.content_type.is_empty()
            || envelope.extension_fields.iter().any(|(key, value)| {
                key.is_empty() || serde_json::from_str::<serde_json::Value>(value).is_err()
            })
        {
            return Err(MessageError::invalid_args(
                "MMP admission envelope fields must be valid and non-empty",
            ));
        }
        let sender_floor = self
            .registration_floors
            .get(sender)
            .copied()
            .ok_or_else(|| {
                MessageError::invalid_state("MMP admission sender interval unavailable")
            })?;
        let audience = resolve_message_audience(identity, scope)?;
        if self.admission.is_some() {
            return Err(admission_capacity());
        }
        if self.accepted_messages.contains_key(&envelope.id) {
            return Err(MessageError::conflict(
                "prepared acceptance requires a fresh occurrence",
            ));
        }
        if envelope.payload.len() > self.retention_bytes {
            return Err(MessageError::invalid_args(MMP_PAYLOAD_TOO_LARGE_MESSAGE));
        }
        if expires_before_delivery(&envelope) {
            return Err(MessageError::invalid_state(MMP_EXPIRED_MESSAGE));
        }
        let next = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| MessageError::invalid_state("message sequence number exhausted"))?;
        let bindings = self.capture_recipient_bindings(&envelope, &audience)?;
        if bindings.is_empty() {
            return Err(MessageError::new(
                MessageErrorKind::NotFound,
                MMP_UNDELIVERABLE_MESSAGE,
            ));
        }
        self.reserve_pending_delivery(&bindings, envelope.payload.len())?;
        self.reserve_pending_recipient_bindings(&bindings)?;
        let metadata = self
            .pending
            .values()
            .fold(super::auxiliary::envelope_bytes(&envelope), |bytes, row| {
                bytes.saturating_add(super::auxiliary::envelope_bytes(&row.envelope))
            });
        if metadata > self.pending_limits.auxiliary_bytes
            || bindings.iter().any(|(id, _)| {
                super::auxiliary::outcome_bytes(&envelope.id, id.as_str())
                    > super::auxiliary::OUTCOME_BYTES
            })
        {
            return Err(admission_capacity());
        }
        let receipt_bytes = std::mem::size_of::<AdmissionIdentity>()
            .saturating_add(RECEIPT_TRACKING_BYTES)
            .saturating_add(2 * std::mem::size_of::<usize>())
            .saturating_add(envelope.id.len());
        if receipt_bytes > receipt_budget {
            return Err(admission_capacity());
        }
        let proof = proof_budget
            .map(|budget| self.reserve_acceptance_proof(&envelope, &bindings, budget))
            .transpose()?
            .map(Arc::new);
        let future = Delivery {
            accepted: true,
            message_id: envelope.id.clone(),
            sequence: self.next_sequence,
            queued_recipients: bindings.len(),
            status: DeliveryStatus::Accepted,
        };
        let actual_receipt_bytes = std::mem::size_of::<AdmissionIdentity>()
            .saturating_add(RECEIPT_TRACKING_BYTES)
            .saturating_add(2 * std::mem::size_of::<usize>())
            .saturating_add(future.message_id.capacity())
            .saturating_add(if proof.is_some() {
                2 * std::mem::size_of::<usize>()
            } else {
                0
            });
        if actual_receipt_bytes > receipt_budget {
            return Err(admission_capacity());
        }
        let allocation = Arc::new(AdmissionIdentity {
            owner: self.acceptance_runtime.clone(),
            delivery: future,
            proof,
            published: AtomicBool::new(false),
        });
        let row = Arc::new(QueuedEnvelope {
            sequence: self.next_sequence,
            envelope: Arc::new(envelope),
            audience,
            offered_at_ms: std::sync::Mutex::new(vec![None; bindings.len()]),
            acknowledged: std::sync::Mutex::new(vec![false; bindings.len()]),
            terminal: std::sync::Mutex::new(vec![None; bindings.len()]),
            sender_registered_after_sequence: Some(sender_floor),
            ack_expiry_observed_at_ms: std::sync::Mutex::new(None),
            recipient_bindings: bindings,
            accepted_at_ms: now_ms,
        });
        self.discard_settled_admission_replay();
        self.next_sequence = next;
        self.admission = Some(PendingMessageAdmission {
            row,
            identity: allocation.clone(),
            attempt: None,
            sender_retired_fresh: None,
        });
        Ok(MessageAcceptanceTicket {
            identity: allocation,
        })
    }

    /// Issues one linear write input while retaining the actor's full charge.
    /// The caller owns the worker/join until a qualified outcome returns.
    pub fn begin_acceptance_write(
        &mut self,
        ticket: &MessageAcceptanceTicket,
    ) -> Result<MessageAcceptanceWrite> {
        self.require_admission_owner(&ticket.identity)?;
        let pending = self.admission.as_mut().ok_or_else(admission_changed)?;
        if !Arc::ptr_eq(&pending.identity, &ticket.identity) || pending.attempt.is_some() {
            return Err(admission_changed());
        }
        let attempt = Arc::new(());
        pending.attempt = Some(attempt.clone());
        Ok(MessageAcceptanceWrite {
            identity: pending.identity.clone(),
            attempt,
            row: pending.row.clone(),
        })
    }

    /// Retains payload-free actor escrow before handing a write to its worker.
    /// This is not a second write capability or proof that the worker joined.
    pub fn acceptance_write_attempt(
        &self,
        ticket: &MessageAcceptanceTicket,
    ) -> Result<MessageAcceptanceAttempt> {
        self.require_admission_owner(&ticket.identity)?;
        let pending = self.admission.as_ref().ok_or_else(admission_changed)?;
        if !Arc::ptr_eq(&pending.identity, &ticket.identity) {
            return Err(admission_changed());
        }
        let attempt = pending.attempt.as_ref().ok_or_else(admission_changed)?;
        Ok(MessageAcceptanceAttempt {
            identity: pending.identity.clone(),
            attempt: attempt.clone(),
        })
    }

    /// Authorizes one new reconciliation attempt from a joined uncertain result.
    /// Consuming the old linear input and replacing its nonce prevent a stale
    /// join observation from qualifying this later attempt. Original bytes,
    /// occurrence, proof and resource charges remain unchanged.
    pub fn retry_acceptance_write(
        &mut self,
        unknown: MessageAcceptanceUncertain,
    ) -> Result<MessageAcceptanceWrite> {
        self.require_admission_attempt(&unknown.write.identity, &unknown.write.attempt)?;
        let pending = self.admission.as_mut().ok_or_else(admission_changed)?;
        let attempt = Arc::new(());
        pending.attempt = Some(attempt.clone());
        Ok(MessageAcceptanceWrite {
            identity: pending.identity.clone(),
            attempt,
            row: pending.row.clone(),
        })
    }

    /// Reconciles a lost worker result only after its exact join is qualified.
    /// A fresh attempt retains the same original bytes/sequence and budget.
    /// Join alone never proves noncommit, so cancellation remains prohibited.
    pub fn recover_joined_acceptance_write(
        &mut self,
        previous: &MessageAcceptanceAttempt,
        joined: &mut impl MessageAcceptanceJoin,
    ) -> Result<MessageAcceptanceWrite> {
        self.require_admission_attempt(&previous.identity, &previous.attempt)?;
        joined.qualify_joined(previous)?;
        let pending = self.admission.as_mut().ok_or_else(admission_changed)?;
        let attempt = Arc::new(());
        pending.attempt = Some(attempt.clone());
        Ok(MessageAcceptanceWrite {
            identity: pending.identity.clone(),
            attempt,
            row: pending.row.clone(),
        })
    }

    /// Adopts an exact committed original once. Duplicate proof returns only the
    /// original fact, even after ACK/replay eviction; it never reinserts bytes.
    /// Retirement/current selectors cannot rewrite already committed recipients.
    /// A stale-owner error after storage does not mean unaccepted: the product
    /// must retain the committed fact for its original owner, never replay effects.
    pub fn publish_acceptance(
        &mut self,
        commit: &MessageAcceptanceCommit,
    ) -> Result<(Delivery, Option<Arc<MessageAcceptanceProof>>)> {
        self.require_admission_owner(&commit.identity)?;
        if commit.identity.published.load(Ordering::Acquire) {
            return Ok((
                commit.identity.delivery.clone(),
                commit.identity.proof.clone(),
            ));
        }
        self.require_admission_attempt(&commit.identity, &commit.attempt)?;
        let pending = self.admission.take().ok_or_else(admission_changed)?;
        let row = pending.row;
        self.install_accepted_row(row.clone());
        self.retain_admission_sender(&row, pending.sender_retired_fresh);
        self.accepted_messages.insert(
            row.envelope.id.clone(),
            AcceptedMessage {
                envelope: row.envelope.clone(),
                audience: row.audience.clone(),
                delivery: commit.identity.delivery.clone(),
                accepted_at_ms: row.accepted_at_ms,
            },
        );
        self.prune_accepted_messages_to_retained_queue();
        commit.identity.published.store(true, Ordering::Release);
        Ok((
            commit.identity.delivery.clone(),
            commit.identity.proof.clone(),
        ))
    }

    /// Cancels only a never-issued original. Issued/unknown work stays charged
    /// until its actual joined commit or qualified noncommit result is adopted.
    pub fn cancel_acceptance(&mut self, ticket: &MessageAcceptanceTicket) -> Result<()> {
        self.require_admission_owner(&ticket.identity)?;
        let pending = self.admission.as_ref().ok_or_else(admission_changed)?;
        if !Arc::ptr_eq(&pending.identity, &ticket.identity) || pending.attempt.is_some() {
            return Err(admission_changed());
        }
        self.admission.take();
        Ok(())
    }

    /// Releases only the exact joined refusal, without classifying generic I/O
    /// errors as absence. The refusal contains no external payload reference.
    pub fn release_rejected_acceptance(
        &mut self,
        rejected: &MessageAcceptanceRejected,
    ) -> Result<()> {
        self.require_admission_attempt(&rejected.identity, &rejected.attempt)?;
        self.admission.take();
        Ok(())
    }

    /// Checks same-runtime allocation before consulting mutable public IDs.
    fn require_admission_owner(&self, identity: &AdmissionIdentity) -> Result<()> {
        if Arc::ptr_eq(&self.acceptance_runtime, &identity.owner) {
            Ok(())
        } else {
            Err(admission_changed())
        }
    }

    /// Matches one joined attempt to the retained original reservation.
    fn require_admission_attempt(
        &self,
        identity: &Arc<AdmissionIdentity>,
        attempt: &Arc<()>,
    ) -> Result<()> {
        self.require_admission_owner(identity)?;
        let pending = self.admission.as_ref().ok_or_else(admission_changed)?;
        if Arc::ptr_eq(&pending.identity, identity)
            && pending
                .attempt
                .as_ref()
                .is_some_and(|actual| Arc::ptr_eq(actual, attempt))
        {
            Ok(())
        } else {
            Err(admission_changed())
        }
    }

    /// Disposes only settled optional history after every new budget fits.
    fn discard_settled_admission_replay(&mut self) {
        let mut removed = Vec::new();
        self.queue.retain(|row| {
            if self.pending.contains_key(&row.sequence) {
                true
            } else {
                removed.push(row.clone());
                false
            }
        });
        for row in removed {
            self.remove_queue_indexes(&row);
        }
        self.queued_bytes = self
            .queue
            .iter()
            .map(|row| row.envelope.payload.len())
            .sum();
        self.prune_accepted_messages_to_retained_queue();
        self.prune_retired_delivery_floors();
    }

    /// Records retirement only for the originally captured sender interval.
    pub(super) fn mark_admission_sender_retired(
        &mut self,
        sender: &AgentId,
        fresh: bool,
        original_floor: Option<MessageSequence>,
    ) {
        if let Some(pending) = self.admission.as_mut()
            && pending.row.envelope.sender.agent_id == *sender
            && pending.row.sender_registered_after_sequence == original_floor
        {
            pending.sender_retired_fresh = Some(fresh);
        }
    }

    /// Reinstates only real committed sender provenance if its original owner
    /// retired during I/O. This never recreates a registration or subscription.
    fn retain_admission_sender(&mut self, row: &Arc<QueuedEnvelope>, retired: Option<bool>) {
        let sender = &row.envelope.sender;
        if self.registered.contains_key(&sender.agent_id) {
            return;
        }
        let Some(fresh) = retired else { return };
        if let Some(floor) = self.retired_delivery_floors.get_mut(&sender.agent_id) {
            if fresh
                && let Some(versions) = floor.fresh_sender_versions.as_mut()
                && same_immutable_sender(&floor.identity, sender)
            {
                if !versions.contains(sender) {
                    versions.push(sender.clone());
                }
                floor.last_sequence = floor.last_sequence.max(row.sequence);
            }
        } else {
            self.retired_delivery_floors.insert(
                sender.agent_id.clone(),
                RetiredDeliveryFloor {
                    identity: sender.clone(),
                    last_sequence: row.sequence,
                    fresh_sender_versions: fresh.then(|| vec![sender.clone()]),
                },
            );
        }
    }
}

/// Reports bounded temporary admission pressure without an accepted effect.
fn admission_capacity() -> MessageError {
    MessageError::new(
        MessageErrorKind::Capacity,
        "MMP unpublished admission capacity; message not accepted",
    )
}
/// Reports stale/foreign allocations without exposing another owner's data.
fn admission_changed() -> MessageError {
    MessageError::conflict("MMP unpublished admission allocation changed or remains issued")
}

#[cfg(test)]
mod tests;
