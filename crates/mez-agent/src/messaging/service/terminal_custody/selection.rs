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
    ) -> Result<Option<MessageTerminalTicket>> {
        let policy = self
            .storage_custody
            .as_ref()
            .filter(|p| p.scope == *scope)
            .ok_or_else(changed)?;
        if self.terminal_commit.is_some() {
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
                    if indices.len() == MESSAGE_TERMINAL_SLOT_LIMIT {
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
                kind: MessageTerminalKind::ExpiryObservation,
                handoff: None,
                retirement: None,
                before: vec![None; indices.len()],
                indices,
                metadata_bytes: bytes,
                decode_payload_bytes: policy.limits.cache_payload_bytes,
                published: AtomicBool::new(false),
                _lease: lease,
            });
            self.terminal_commit = Some(PendingTerminal {
                identity: identity.clone(),
                attempt: None,
            });
            return Ok(Some(MessageTerminalTicket { identity }));
        }
        Ok(None)
    }

    /// Observes an unpublished original operation for restore/checkpoint fences.
    /// This is not physical join, commit, terminal status or noncommit evidence.
    pub fn has_unpublished_terminal(&self) -> bool {
        self.terminal_commit.is_some()
    }

    /// A real first expiry observation applies to the immutable source TTL,
    /// including unselected fanout slots. Pending observation blocks new receive
    /// without creating terminal outcomes; existing pre-TTL ACK rights remain.
    pub(in crate::messaging::service) fn expiry_excludes(&self, sequence: MessageSequence) -> bool {
        self.terminal_commit.as_ref().is_some_and(|p| {
            p.identity.kind == MessageTerminalKind::ExpiryObservation
                && p.identity.source.sequence == sequence
        }) || self
            .queued_by_sequence
            .get(&sequence)
            .is_some_and(|row| known_expiry(row).map_or(true, |clock| clock.is_some()))
    }

    /// A reserved slot is unavailable for receive or competing ACK while its
    /// actual expiry owner is unsettled; its terminal outcome is still absent.
    pub(in crate::messaging::service) fn terminal_protects(
        &self,
        sequence: MessageSequence,
        recipient: &AgentId,
    ) -> bool {
        self.terminal_commit.as_ref().is_some_and(|pending| {
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
pub(super) fn known_expiry(row: &QueuedEnvelope) -> Result<Option<u64>> {
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

impl MessageService {
    /// Returns the earliest strict original TTL boundary for an eligible slot.
    /// Zero means an actual expiry witness already exists, not a renewed clock.
    /// Protected or settled slots and unrepresentable strict deadlines create no
    /// wake. This does not release outcomes or establish elapsed-clock policy.
    pub fn next_storage_expiry_at_ms(&self, scope: &MessageStorageScope) -> Result<Option<u64>> {
        if !self
            .storage_custody
            .as_ref()
            .is_some_and(|p| p.scope == *scope)
        {
            return Err(changed());
        }
        let mut next = None;
        for row in self.pending.values() {
            let eligible = {
                let acknowledged = row.acknowledged.lock().map_err(|_| changed())?;
                let terminal = row.terminal.lock().map_err(|_| changed())?;
                if acknowledged.len() != row.recipient_bindings.len()
                    || terminal.len() != acknowledged.len()
                {
                    return Err(changed());
                }
                row.recipient_bindings
                    .iter()
                    .enumerate()
                    .any(|(i, (id, _))| {
                        !acknowledged[i]
                            && terminal[i].is_none()
                            && !self.acknowledgement_protects(row.sequence, id)
                    })
            };
            if !eligible {
                continue;
            }
            let at = if known_expiry(row)?.is_some() {
                Some(0)
            } else {
                row.envelope
                    .ttl_ms
                    .and_then(|ttl| row.accepted_at_ms.saturating_add(ttl).checked_add(1))
            };
            if let Some(at) = at {
                next = Some(next.map_or(at, |prior: u64| prior.min(at)));
            }
        }
        Ok(next)
    }
}
