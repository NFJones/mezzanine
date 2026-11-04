//! Bounded expiry reservation and exact-slot exclusion, without terminal effects.

use super::*;

impl MessageService {
    /// Freezes up to64 eligible slots of one accepted expired original. A source
    /// with no finite TTL, ACK/native protection or an existing disposition is
    /// skipped. None means no currently eligible work, not terminal settlement.
    /// The caller supplies its original root scope and trusted observation clock;
    /// bodyless storage and rollback-safe elapsed policy belong to the adapter.
    pub fn prepare_storage_expiry(
        &mut self,
        scope: &MessageStorageScope,
        observed_at_ms: u64,
        metadata_budget: usize,
    ) -> Result<Option<MessageExpiryTicket>> {
        let policy = self
            .storage_custody
            .as_ref()
            .filter(|p| p.scope == *scope)
            .ok_or_else(changed)?;
        if self.expiry.is_some() {
            return Err(capacity());
        }
        for row in self.pending.values() {
            let Some(observed_at_ms) = known_expiry(row)?
                .or_else(|| expired(row, observed_at_ms).then_some(observed_at_ms))
            else {
                continue;
            };
            let acknowledged = row.acknowledged.lock().map_err(|_| changed())?;
            let terminals = row.terminal.lock().map_err(|_| changed())?;
            if acknowledged.len() != row.recipient_bindings.len()
                || terminals.len() != acknowledged.len()
            {
                return Err(changed());
            }
            let mut indices = Vec::new();
            for (index, (recipient, _)) in row.recipient_bindings.iter().enumerate() {
                if !acknowledged[index]
                    && terminals[index].is_none()
                    && !self.acknowledgement_protects(row.sequence, recipient)
                {
                    indices.push(u32::try_from(index).map_err(|_| capacity())?);
                    if indices.len() == MESSAGE_EXPIRY_SLOT_LIMIT {
                        break;
                    }
                }
            }
            if indices.is_empty() {
                continue;
            }
            let bytes = super::super::storage_policy::source_metadata_bytes(row)
                .saturating_add(MESSAGE_STORAGE_RECOVERY_SCRATCH_METADATA)
                .saturating_add(indices.len().saturating_mul(64));
            if bytes > metadata_budget.min(MAX_METADATA) {
                return Err(capacity());
            }
            let lease = super::super::checkpoint::reserve_checkpoint(
                policy.checkpoint_bytes.clone(),
                bytes,
                self.storage_metadata_base_usage()?,
                policy.limits.metadata_bytes,
            )?;
            let identity = Arc::new(Identity {
                owner: self.acceptance_runtime.clone(),
                scope: scope.clone(),
                source: row.clone(),
                digest: row.envelope.storage_digest(scope)?,
                observed_at_ms,
                indices,
                metadata_bytes: bytes,
                decode_payload_bytes: policy.limits.cache_payload_bytes,
                published: AtomicBool::new(false),
                _lease: lease,
            });
            self.expiry = Some(PendingExpiry {
                identity: identity.clone(),
                attempt: None,
            });
            return Ok(Some(MessageExpiryTicket { identity }));
        }
        Ok(None)
    }

    /// Observes an unpublished original operation for restore/checkpoint fences.
    /// This is not physical join, commit, terminal status or noncommit evidence.
    pub fn has_unpublished_expiry(&self) -> bool {
        self.expiry.is_some()
    }

    /// A real first expiry observation applies to the immutable source TTL,
    /// including unselected fanout slots. Pending observation blocks new receive
    /// without creating terminal outcomes; existing pre-TTL ACK rights remain.
    pub(in crate::messaging::service) fn expiry_excludes(&self, sequence: MessageSequence) -> bool {
        self.expiry
            .as_ref()
            .is_some_and(|p| p.identity.source.sequence == sequence)
            || self
                .queued_by_sequence
                .get(&sequence)
                .is_some_and(|row| known_expiry(row).map_or(true, |clock| clock.is_some()))
    }

    /// A reserved slot is unavailable for receive or competing ACK while its
    /// actual expiry owner is unsettled; its terminal outcome is still absent.
    pub(in crate::messaging::service) fn expiry_protects(
        &self,
        sequence: MessageSequence,
        recipient: &AgentId,
    ) -> bool {
        self.expiry.as_ref().is_some_and(|pending| {
            pending.identity.source.sequence == sequence
                && pending.identity.indices.iter().any(|index| {
                    pending
                        .identity
                        .source
                        .recipient_bindings
                        .get(*index as usize)
                        .is_some_and(|(id, _)| id == recipient)
                })
        })
    }
}

/// Preserves the earliest already observed strict expiry across later bounded
/// batches and wall-clock rollback. Recipient retirement is not TTL evidence.
fn known_expiry(row: &QueuedEnvelope) -> Result<Option<u64>> {
    let mut first = *row
        .ack_expiry_observed_at_ms
        .lock()
        .map_err(|_| changed())?;
    for terminal in row.terminal.lock().map_err(|_| changed())?.iter().flatten() {
        if terminal.reason == DeliveryTerminalReason::Expired {
            first = Some(first.map_or(terminal.observed_at_ms, |prior| {
                prior.min(terminal.observed_at_ms)
            }));
        }
    }
    if first.is_some_and(|clock| !expired(row, clock)) {
        return Err(changed());
    }
    Ok(first)
}
