//! Cold reconstruction of the canonical custody owner through a trusted port.
//!
//! The product qualifies the original store and its complete transaction history;
//! this module owns allocation reservations, cold canonical rows and projections.
//! Each decoder runs only after reserving a single payload quantum. Its body is
//! moved into metadata and dropped before reading another original. Explicit
//! source-bound ACKs survive independently of cursors. No raw registration,
//! native epoch or transient offer acquires authority from source/snapshot data.
//! Actual session continuity and root-worker publication remain product gates.

use std::sync::atomic::Ordering;

use super::*;

/// Shared worst-case metadata scratch for one source, ACK input and chunk reader.
pub const MESSAGE_STORAGE_RECOVERY_SCRATCH_METADATA: usize = 2 * 1024 * 1024;

/// Original occurrence facts supplied only by the qualified storage-history port.
pub struct MessageStorageRecoveredRecord {
    /// Strictly increasing original sequence, never newly allocated here.
    pub sequence: MessageSequence,
    /// Verified full immutable source digest under this original root.
    pub source_sha256: [u8; 32],
    /// Original expired receipt proved by the trusted checkpoint/history reader.
    pub expired_receipt: bool,
    /// Original durable expiry exclusion clock, without restarting the TTL.
    pub expiry_observed_at_ms: Option<u64>,
}

/// Trusted product recovery boundary; never implemented by a generic decoder.
pub trait MessageStorageRecoveryStore {
    /// Qualifies the entire original history in one owned snapshot, installs its
    /// originals, then returns the persistent allocator floor. Any failure must
    /// withhold the whole candidate. No received bytes grant this port authority.
    fn recover(&mut self, target: &mut MessageStorageRecovery) -> Result<MessageSequence>;
}

/// Unpublished lower owner borrowed only by its original recovery port.
pub struct MessageStorageRecovery {
    service: MessageService,
    last_sequence: MessageSequence,
    /// Any refused row permanently prevents publication of this candidate.
    failed: bool,
    /// Bounded incremental counters avoid rebuilding a temporary recipient map.
    pending_bytes: usize,
    obligations: usize,
    per_recipient: HashMap<(AgentId, MessageSequence), (usize, usize)>,
    counter_metadata: usize,
    /// Fixed scratch stays charged through the full trusted-port invocation.
    _scratch: MessageStorageSnapshotLease,
    /// One payload quantum covers inventory, ACK validation and row decoding.
    _payload: super::payload_budget::PayloadReservation,
}

impl MessageService {
    /// Reconstructs original cold custody under an explicit trusted destination.
    /// The synchronous port must run on the product's owned storage worker.
    /// Returns no service on corrupt/incomplete/over-budget history. All restored
    /// endpoint identities remain unavailable until separately qualified; this
    /// does not revive raw reconnect, native context or live acceptance events.
    pub fn recover_storage_custody(
        scope: &MessageStorageScope,
        limits: MessageStorageLimits,
        retention_messages: usize,
        retention_bytes: usize,
        store: &mut impl MessageStorageRecoveryStore,
    ) -> Result<Self> {
        let mut service = Self::with_limits(retention_messages, retention_bytes);
        service.enable_storage_custody(scope, limits)?;
        let policy = service.storage_custody.as_ref().ok_or_else(unavailable)?;
        let scratch = checkpoint::reserve_checkpoint(
            policy.checkpoint_bytes.clone(),
            MESSAGE_STORAGE_RECOVERY_SCRATCH_METADATA,
            service.storage_metadata_base_usage()?,
            policy.limits.metadata_bytes,
        )?;
        let payload = policy
            .cache
            .reserve(0, policy.limits.cache_payload_bytes, 0)?;
        let mut target = MessageStorageRecovery {
            service,
            last_sequence: 0,
            failed: false,
            pending_bytes: 0,
            obligations: 0,
            per_recipient: HashMap::new(),
            counter_metadata: 0,
            _scratch: scratch,
            _payload: payload,
        };
        let next_sequence = store.recover(&mut target)?;
        if target.failed || next_sequence == 0 || next_sequence <= target.last_sequence {
            return Err(unavailable());
        }
        let MessageStorageRecovery {
            mut service,
            _scratch,
            _payload,
            ..
        } = target;
        service.next_sequence = next_sequence;
        service.acceptance_history_through = next_sequence - 1;
        service.ids =
            IdFactory::after_existing_ids(service.queued_by_sequence.values().flat_map(|row| {
                std::iter::once(&row.envelope.sender.agent_id)
                    .chain(row.recipient_bindings.iter().map(|(id, _)| id))
                    .chain(match &row.envelope.recipient {
                        Recipient::Agent(id) => Some(id),
                        _ => None,
                    })
            }));
        service.rebuild_queue_indexes();
        service.prune_accepted_messages_to_retained_queue();
        drop(_payload);
        drop(_scratch);
        Ok(service)
    }
}

impl MessageStorageRecovery {
    /// Charges retained checkpoint/inventory/fold evidence against the candidate's
    /// own budget while it is borrowed by the product. Scratch is already held.
    pub fn hold_metadata(&self, bytes: usize) -> Result<MessageStorageSnapshotLease> {
        let policy = self
            .service
            .storage_custody
            .as_ref()
            .ok_or_else(unavailable)?;
        checkpoint::reserve_checkpoint(
            policy.checkpoint_bytes.clone(),
            bytes,
            self.service
                .storage_metadata_base_usage()?
                .saturating_add(self.counter_metadata),
            policy.limits.metadata_bytes,
        )
    }

    /// Returns currently charged metadata and its trusted destination ceiling.
    /// The product includes both in its own inventory preflight before decoding.
    pub fn metadata_budget(&self) -> Result<(usize, usize)> {
        let policy = self
            .service
            .storage_custody
            .as_ref()
            .ok_or_else(unavailable)?;
        Ok((
            self.service
                .storage_metadata_base_usage()?
                .saturating_add(policy.checkpoint_bytes.load(Ordering::Acquire))
                .saturating_add(self.counter_metadata),
            policy.limits.metadata_bytes,
        ))
    }

    /// Exposes reservation counts for the actual decoder without exposing mail.
    pub fn payload_observer(&self) -> Result<MessagePayloadObserver> {
        self.service.payload_observer().ok_or_else(unavailable)
    }

    /// Returns decoder ceilings covered by this candidate's actual payload and
    /// scratch reservations. The trusted port must use these limits for every
    /// inventory, ACK-source and original decode, including reads before rows
    /// are installed. Saved data cannot enlarge a narrowed cache quantum.
    pub fn decode_limits(&self) -> Result<MessageAcceptanceDecodeLimits> {
        let policy = self
            .service
            .storage_custody
            .as_ref()
            .ok_or_else(unavailable)?;
        Ok(MessageAcceptanceDecodeLimits {
            payload_bytes: policy.limits.cache_payload_bytes,
            auxiliary_bytes: 512 * 1024,
            recipient_count: 4096,
            recipient_bytes: 1024 * 1024,
        })
    }

    /// Reads one completely verified original only after reserving its payload.
    /// Positive decisions are exact original slot/offer-clock pairs, never a
    /// cumulative cursor. Duplicate slots, stale sequences or policy exhaustion
    /// withhold the whole recovery; the caller must not swallow these failures.
    pub fn restore_original(
        &mut self,
        record: MessageStorageRecoveredRecord,
        acknowledgements: impl IntoIterator<Item = (u32, u64)>,
        read: impl FnOnce(MessageAcceptanceDecodeLimits) -> Result<MessageAcceptanceSource>,
    ) -> Result<()> {
        if self.failed {
            return Err(unavailable());
        }
        let result = self.restore_original_inner(record, acknowledgements, read);
        self.failed |= result.is_err();
        result
    }

    /// Builds one private row while preserving sticky whole-candidate refusal.
    fn restore_original_inner(
        &mut self,
        record: MessageStorageRecoveredRecord,
        acknowledgements: impl IntoIterator<Item = (u32, u64)>,
        read: impl FnOnce(MessageAcceptanceDecodeLimits) -> Result<MessageAcceptanceSource>,
    ) -> Result<()> {
        if record.sequence <= self.last_sequence {
            return Err(unavailable());
        }
        let policy = self
            .service
            .storage_custody
            .as_ref()
            .ok_or_else(unavailable)?;
        let limits = self.decode_limits()?;
        let source = read(limits)?;
        if source.sequence() != record.sequence
            || self
                .service
                .accepted_messages
                .contains_key(&source.envelope().id)
        {
            return Err(unavailable());
        }
        let count = source.recipient_bindings().len();
        if count == 0
            || count > limits.recipient_count
            || source.envelope().payload.len() > limits.payload_bytes
            || record.expiry_observed_at_ms.is_some_and(|clock| {
                !source
                    .envelope()
                    .ttl_ms
                    .is_some_and(|ttl| source.accepted_at_ms().saturating_add(ttl) < clock)
            })
        {
            return Err(unavailable());
        }
        let mut offered = vec![None; count];
        let mut acknowledged = vec![false; count];
        // A genuine endpoint clock can roll back before acceptance, including
        // to zero. Durable source-bound selection, not clock ordering, proves
        // the offer; only the original TTL's upper boundary constrains it.
        for (slot, clock) in acknowledgements {
            let slot = slot as usize;
            if slot >= count
                || acknowledged[slot]
                || source
                    .envelope()
                    .ttl_ms
                    .is_some_and(|ttl| source.accepted_at_ms().saturating_add(ttl) < clock)
            {
                return Err(unavailable());
            }
            acknowledged[slot] = true;
            offered[slot] = Some(clock);
        }
        if record.expired_receipt && record.expiry_observed_at_ms.is_none() {
            return Err(unavailable());
        }
        let pending = acknowledged.iter().any(|ack| !*ack);
        let row = Arc::new(source.into_cold_occurrence(
            &policy.scope,
            record.source_sha256,
            offered,
            acknowledged,
            record.expiry_observed_at_ms,
        )?);
        let additional = self.reserve_row(&row, pending)?;
        let scope = policy.scope.clone();
        if pending {
            self.per_recipient
                .try_reserve(row.recipient_bindings.len())
                .map_err(|_| capacity())?;
            for binding in &row.recipient_bindings {
                let value = self.per_recipient.entry(binding.clone()).or_default();
                value.0 += 1;
                value.1 += row.envelope.payload_bytes;
            }
            self.pending_bytes += row.envelope.payload_bytes;
            self.obligations += count;
            self.counter_metadata += additional;
        }
        let delivery = Delivery {
            accepted: true,
            message_id: row.envelope.id.clone(),
            sequence: row.sequence,
            queued_recipients: count,
            status: if record.expired_receipt {
                DeliveryStatus::Expired
            } else {
                DeliveryStatus::Accepted
            },
        };
        self.service.install_accepted_row(row.clone());
        if !pending {
            self.service.pending.remove(&row.sequence);
        }
        self.service.accepted_messages.insert(
            row.envelope.id.clone(),
            AcceptedMessage {
                storage_scope: Some(scope),
                source: row,
                delivery,
            },
        );
        self.last_sequence = record.sequence;
        Ok(())
    }
    /// Checks all destination counters and charges incremental key allocations
    /// before cloning recipient IDs or installing any canonical row/index.
    fn reserve_row(&self, row: &QueuedEnvelope, pending: bool) -> Result<usize> {
        let policy = self
            .service
            .storage_custody
            .as_ref()
            .ok_or_else(unavailable)?;
        let mut additional = 0usize;
        if pending {
            if self.service.pending.len() >= policy.limits.messages
                || self
                    .pending_bytes
                    .saturating_add(row.envelope.payload_bytes)
                    > policy.limits.payload_bytes
                || self
                    .obligations
                    .saturating_add(row.recipient_bindings.len())
                    > policy.limits.recipient_obligations
            {
                return Err(capacity());
            }
            for binding in &row.recipient_bindings {
                let (count, bytes) = self.per_recipient.get(binding).copied().unwrap_or_default();
                if count >= policy.limits.per_recipient
                    || bytes.saturating_add(row.envelope.payload_bytes)
                        > policy.limits.per_recipient_payload_bytes
                {
                    return Err(capacity());
                }
                if count == 0 {
                    additional = additional
                        .saturating_add(256)
                        .saturating_add(binding.0.as_str().len().saturating_mul(4));
                }
            }
        }
        let (used, ceiling) = self.metadata_budget()?;
        if used
            .saturating_add(additional)
            .saturating_add(storage_policy::source_metadata_bytes(row))
            > ceiling
            || self.service.accepted_messages.len() >= policy.limits.messages
        {
            return Err(capacity());
        }
        Ok(additional)
    }
}

/// Reports resource exhaustion without including original private content.
fn capacity() -> MessageError {
    MessageError::new(MessageErrorKind::Capacity, "MMP recovery custody capacity")
}

/// Bounded content-free error; partial candidates never leave the constructor.
fn unavailable() -> MessageError {
    MessageError::invalid_state("MMP original storage recovery evidence is unavailable")
}

#[cfg(test)]
mod tests;
