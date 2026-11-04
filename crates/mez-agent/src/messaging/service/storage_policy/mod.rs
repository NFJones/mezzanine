//! Trusted storage-backed custody policy over the same canonical message owner.
//!
//! Logical pending mail has independent disk-scale ceilings; the resident cache
//! retains its smaller allocation budget. Only original-root-qualified payloads
//! may leave that cache. Weak allocation accounting includes external delivery
//! pins, unpublished admissions and owned decoder reservations. No policy comes
//! from saved snapshots, wire fields or model/config options.

use super::payload_budget::{PayloadBudget, PayloadReservation};
use super::*;

/// Always reserve the existing maximum ACK metadata/encoding quantum.
const WORKER_HEADROOM: usize = 512 * 1024 + MESSAGE_PAYLOAD_DEMAND_BYTES;
/// Bounded actor-owned cold-read continuation metadata, including IDs/overhead.
pub const MESSAGE_PAYLOAD_DEMAND_BYTES: usize = 64 * 1024;
/// Maximum deferred reader continuations on one serialized root.
pub const MESSAGE_PAYLOAD_DEMAND_RECIPIENTS: usize = 64;
/// Conservative ledger node/weak-token/index charge per possible cache slot.
const CACHE_SLOT_METADATA: usize = 256;

/// Independent destination-owned ceilings for storage-backed custody.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageStorageLimits {
    /// Accepted plus reserved original occurrences (at most 10,000).
    pub messages: usize,
    /// Unique logical pending payload (at most 64 MiB).
    pub payload_bytes: usize,
    /// Frozen obligations including reservations (at most 40,000).
    pub recipient_obligations: usize,
    /// Obligations per original recipient interval (at most 1,024).
    pub per_recipient: usize,
    /// Payload per original recipient interval (at most 8 MiB).
    pub per_recipient_payload_bytes: usize,
    /// Combined logical message metadata ceiling (at most 8 MiB).
    pub metadata_bytes: usize,
    /// Live/reserved cache allocations, including external pins (at most 1,000).
    pub cache_messages: usize,
    /// Live/reserved canonical cache payload (at most 1 MiB).
    pub cache_payload_bytes: usize,
}

impl Default for MessageStorageLimits {
    fn default() -> Self {
        Self {
            messages: 10_000,
            payload_bytes: 64 * 1024 * 1024,
            recipient_obligations: 40_000,
            per_recipient: 1024,
            per_recipient_payload_bytes: 8 * 1024 * 1024,
            metadata_bytes: 8 * 1024 * 1024,
            cache_messages: 1000,
            cache_payload_bytes: 1024 * 1024,
        }
    }
}

impl MessageStorageLimits {
    /// Rejects zero/enlarged values before constructing any storage authority.
    fn validate(self) -> Result<Self> {
        let max = Self::default();
        if [
            (self.messages, max.messages),
            (self.payload_bytes, max.payload_bytes),
            (self.recipient_obligations, max.recipient_obligations),
            (self.per_recipient, max.per_recipient),
            (
                self.per_recipient_payload_bytes,
                max.per_recipient_payload_bytes,
            ),
            (self.metadata_bytes, max.metadata_bytes),
            (self.cache_messages, max.cache_messages),
            (self.cache_payload_bytes, max.cache_payload_bytes),
        ]
        .iter()
        .any(|(actual, maximum)| *actual == 0 || actual > maximum)
        {
            return Err(MessageError::invalid_args(
                "MMP storage limits must be positive and cannot exceed production ceilings",
            ));
        }
        if self.metadata_bytes
            <= WORKER_HEADROOM
                .saturating_add(self.cache_messages.saturating_mul(CACHE_SLOT_METADATA))
        {
            return Err(MessageError::invalid_args(
                "MMP metadata budget cannot reserve its worker/cache headroom",
            ));
        }
        Ok(self)
    }
}

/// Internal counter view; public ephemeral ceilings remain independently valid.
#[derive(Clone, Copy)]
pub(super) struct CustodyLimits {
    pub(super) messages: usize,
    pub(super) payload_bytes: usize,
    pub(super) recipient_obligations: usize,
    pub(super) per_recipient: usize,
    pub(super) auxiliary_bytes: usize,
}

/// Runtime-only policy and allocation owner; never serialized or reconstructed.
#[derive(Debug)]
pub(in crate::messaging) struct StorageCustody {
    pub(super) scope: MessageStorageScope,
    pub(super) limits: MessageStorageLimits,
    pub(super) cache: Arc<PayloadBudget>,
    /// Captured checkpoint metadata and its serialization copies stay charged
    /// through actual snapshot publication, even after the root lane is released.
    pub(super) checkpoint_bytes: Arc<std::sync::atomic::AtomicUsize>,
    /// Trusted product admission barrier; never selected by saved/wire data.
    pub(super) native_continuity_required: bool,
}

/// Content-free logical usage, independent of allocator-specific RSS or wire buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageStorageUsage {
    /// Accepted pending occurrences; settled replay is a separate projection.
    pub pending_messages: usize,
    /// Unique accepted pending payload, including cold originals.
    pub pending_payload_bytes: usize,
    /// One unpublished acceptance, if currently reserved.
    pub reserved_messages: usize,
    /// Its original immutable payload reservation.
    pub reserved_payload_bytes: usize,
    /// Combined retained/orphan metadata plus reserved worker/cache headroom.
    pub metadata_bytes: usize,
    /// All currently live/reserved canonical allocations, including worker pins.
    pub cache: MessagePayloadUsage,
}

/// Charges immutable headers and the complete frozen source binding metadata.
/// Worker-held source rows retain both after their canonical index disappears;
/// external envelope-only pins conservatively keep this same original charge.
pub(super) fn message_metadata_bytes(
    headers: usize,
    bindings: &[(AgentId, MessageSequence)],
) -> usize {
    bindings.iter().fold(headers, |sum, (id, _)| {
        sum.saturating_add(192).saturating_add(id.as_str().len())
    })
}

/// Counts the complete frozen source, including native application metadata.
pub(super) fn source_metadata_bytes(row: &QueuedEnvelope) -> usize {
    message_metadata_bytes(
        auxiliary::retained_envelope_bytes(&row.envelope),
        &row.recipient_bindings,
    )
    .saturating_add(
        row.native_bindings
            .as_ref()
            .map_or(0, |owners| owners.metadata_bytes()),
    )
}

impl MessageService {
    /// Resolves the trusted destination policy without constructing an invalid
    /// enlarged PendingDeliveryLimits or permitting a snapshot to select it.
    pub(super) fn custody_limits(&self) -> CustodyLimits {
        match &self.storage_custody {
            Some(policy) => CustodyLimits {
                messages: policy.limits.messages,
                payload_bytes: policy.limits.payload_bytes,
                recipient_obligations: policy.limits.recipient_obligations,
                per_recipient: policy.limits.per_recipient,
                auxiliary_bytes: policy.limits.metadata_bytes,
            },
            None => CustodyLimits {
                messages: self.pending_limits.messages,
                payload_bytes: self.pending_limits.payload_bytes,
                recipient_obligations: self.pending_limits.recipient_obligations,
                per_recipient: self.pending_limits.per_recipient,
                auxiliary_bytes: self.pending_limits.auxiliary_bytes,
            },
        }
    }

    /// Enables original-root storage custody before any message is admitted.
    /// Existing identities/subscriptions are preserved. Nonempty or already
    /// configured owners fail closed; no snapshot or matching label grants this.
    pub fn enable_storage_custody(
        &mut self,
        scope: &MessageStorageScope,
        limits: MessageStorageLimits,
    ) -> Result<()> {
        let limits = limits.validate()?;
        if self.storage_custody.is_some()
            || self.admission.is_some()
            || self.acknowledgement.is_some()
            || self.terminal_commit.is_some()
            || !self.queued_by_sequence.is_empty()
            || !self.accepted_messages.is_empty()
        {
            return Err(MessageError::invalid_state(
                "MMP storage custody requires an empty unconfigured message owner",
            ));
        }
        self.storage_custody = Some(StorageCustody {
            checkpoint_bytes: Arc::default(),
            native_continuity_required: false,
            scope: scope.clone(),
            limits,
            cache: PayloadBudget::new(limits.cache_messages, limits.cache_payload_bytes),
        });
        Ok(())
    }

    /// Returns content-free cache counters that remain valid during worker I/O.
    pub fn payload_observer(&self) -> Option<MessagePayloadObserver> {
        self.storage_custody
            .as_ref()
            .map(|policy| policy.cache.observer())
    }

    /// Reports original logical pending/reserved mail and all retained payload
    /// allocations. No payload or recipient identifiers are exposed.
    pub fn storage_usage(&self) -> Result<Option<MessageStorageUsage>> {
        let Some(policy) = &self.storage_custody else {
            return Ok(None);
        };
        Ok(Some(MessageStorageUsage {
            pending_messages: self.pending.len(),
            pending_payload_bytes: self.pending.values().fold(0usize, |n, row| {
                n.saturating_add(row.envelope.payload_bytes)
            }),
            reserved_messages: usize::from(self.admission.is_some()),
            reserved_payload_bytes: self
                .admission
                .as_ref()
                .map_or(0, |pending| pending.payload_bytes()),
            metadata_bytes: self.storage_metadata_usage()?,
            cache: policy.cache.usage()?,
        }))
    }

    /// Computes currently retained message metadata, including orphan allocations.
    pub(super) fn storage_metadata_base_usage(&self) -> Result<usize> {
        let Some(policy) = &self.storage_custody else {
            return Ok(0);
        };
        let mut bytes = WORKER_HEADROOM.saturating_add(
            policy
                .limits
                .cache_messages
                .saturating_mul(CACHE_SLOT_METADATA),
        );
        for row in self
            .queued_by_sequence
            .values()
            .map(AsRef::as_ref)
            .chain(self.admission.iter().map(|pending| pending.source()))
        {
            bytes = bytes.saturating_add(source_metadata_bytes(row));
        }
        bytes = bytes.saturating_add(policy.cache.orphan_metadata(|sequence| {
            self.queued_by_sequence.contains_key(&sequence)
                || self
                    .admission
                    .as_ref()
                    .is_some_and(|pending| pending.sequence() == sequence)
        })?);
        Ok(bytes)
    }

    /// Includes live checkpoint/encoding reservations independently of source cache.
    fn storage_metadata_usage(&self) -> Result<usize> {
        let captured = self.storage_custody.as_ref().map_or(0, |p| {
            p.checkpoint_bytes
                .load(std::sync::atomic::Ordering::Acquire)
        });
        Ok(self.storage_metadata_base_usage()?.saturating_add(captured))
    }

    /// Reserves a canonical allocation, evicting only original-root backed
    /// cache pins. Actual external pins remain charged after cache release.
    pub(super) fn reserve_storage_payload(
        &self,
        sequence: MessageSequence,
        bytes: usize,
        metadata: usize,
        keep: Option<MessageSequence>,
    ) -> Result<Option<PayloadReservation>> {
        let Some(policy) = &self.storage_custody else {
            return Ok(None);
        };
        if bytes > policy.limits.cache_payload_bytes {
            return policy.cache.reserve(sequence, bytes, metadata).map(Some);
        }
        match policy.cache.reserve(sequence, bytes, metadata) {
            Ok(reservation) => return Ok(Some(reservation)),
            Err(error) if error.kind() == MessageErrorKind::Capacity => (),
            Err(error) => return Err(error),
        }
        for row in self.queued_by_sequence.values() {
            if Some(row.sequence) == keep {
                continue;
            }
            if row.envelope.release_stored(&policy.scope).is_err() {
                continue;
            }
            match policy.cache.reserve(sequence, bytes, metadata) {
                Ok(reservation) => return Ok(Some(reservation)),
                Err(error) if error.kind() == MessageErrorKind::Capacity => (),
                Err(error) => return Err(error),
            }
        }
        policy.cache.reserve(sequence, bytes, metadata).map(Some)
    }

    /// Charges message headers and recipient slots together, reserving worker
    /// and cache-index headroom before accepting another immutable occurrence.
    pub(super) fn reserve_storage_metadata(
        &self,
        envelope: &Envelope,
        incoming: &[(AgentId, MessageSequence)],
        native_bytes: usize,
    ) -> Result<()> {
        let Some(policy) = &self.storage_custody else {
            return Ok(());
        };
        let mut bytes = WORKER_HEADROOM
            .saturating_add(
                policy
                    .limits
                    .cache_messages
                    .saturating_mul(CACHE_SLOT_METADATA),
            )
            .saturating_add(auxiliary::envelope_bytes(envelope));
        bytes = message_metadata_bytes(bytes, incoming).saturating_add(native_bytes);
        for row in self.pending.values() {
            bytes = bytes.saturating_add(source_metadata_bytes(row));
        }
        let fits = || -> Result<bool> {
            let orphan = policy.cache.orphan_metadata(|sequence| {
                self.pending.contains_key(&sequence) || sequence == self.next_sequence
            })?;
            Ok(bytes.saturating_add(orphan).saturating_add(
                policy
                    .checkpoint_bytes
                    .load(std::sync::atomic::Ordering::Acquire),
            ) <= policy.limits.metadata_bytes)
        };
        if fits()? {
            return Ok(());
        }
        for row in self
            .queue
            .iter()
            .filter(|row| !self.pending.contains_key(&row.sequence))
        {
            if row.envelope.release_stored(&policy.scope).is_ok() && fits()? {
                return Ok(());
            }
        }
        Err(MessageError::new(
            MessageErrorKind::Capacity,
            "MMP storage metadata capacity; message not accepted",
        ))
    }

    /// Prevents a generic/memory port from issuing under storage-mode budgets.
    pub(super) fn require_storage_scope(&self, scope: Option<&MessageStorageScope>) -> Result<()> {
        if self
            .storage_custody
            .as_ref()
            .is_some_and(|policy| Some(&policy.scope) != scope)
        {
            Err(MessageError::invalid_state(
                "MMP storage custody requires its original qualified root",
            ))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;
