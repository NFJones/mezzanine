//! Exact unreleased terminal before-images for bounded durable horizon release.
//!
//! This is automatic terminal retention, not original-sender consumption. Its
//! captured wall observation is never refreshed by retry. A later elapsed-clock
//! policy remains required before normal durable activation across rollback.

use super::*;

impl MessageService {
    /// Freezes up to64 actual terminal outcomes from one original whose handoff
    /// horizon has elapsed. Sender DTOs, receipt labels and current registration
    /// absence cannot select a release. No live slot changes before adoption.
    pub fn prepare_storage_horizon_release(
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
            let observed_at_ms = (*row.horizon_observed_at_ms.lock().map_err(|_| changed())?)
                .map_or(observed_at_ms, |actual| actual.max(observed_at_ms));
            let acknowledged = row.acknowledged.lock().map_err(|_| changed())?;
            let terminals = row.terminal.lock().map_err(|_| changed())?;
            if acknowledged.len() != row.recipient_bindings.len()
                || terminals.len() != acknowledged.len()
            {
                return Err(changed());
            }
            let mut indices = Vec::new();
            let mut before = Vec::new();
            for (index, (recipient, _)) in row.recipient_bindings.iter().enumerate() {
                if let Some(terminal) = terminals[index]
                    && !acknowledged[index]
                    && !terminal.released
                    && observed_at_ms.saturating_sub(terminal.observed_at_ms)
                        >= MESSAGE_TERMINAL_HANDOFF_MS
                    && !self.acknowledgement_protects(row.sequence, recipient)
                {
                    indices.push(u32::try_from(index).map_err(|_| capacity())?);
                    before.push(Some(terminal));
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
                kind: MessageTerminalKind::HorizonRelease,
                handoff: None,
                retirement: None,
                raw_retirement: None,
                indices,
                before,
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

    /// Returns the earliest representable actual terminal horizon. Released,
    /// acknowledged or protected slots create no wake; no state is mutated.
    pub fn next_storage_horizon_at_ms(&self, scope: &MessageStorageScope) -> Result<Option<u64>> {
        if !self
            .storage_custody
            .as_ref()
            .is_some_and(|p| p.scope == *scope)
        {
            return Err(changed());
        }
        let mut next = None;
        for row in self.pending.values() {
            let known = *row.horizon_observed_at_ms.lock().map_err(|_| changed())?;
            let acknowledged = row.acknowledged.lock().map_err(|_| changed())?;
            let terminals = row.terminal.lock().map_err(|_| changed())?;
            if acknowledged.len() != row.recipient_bindings.len()
                || terminals.len() != acknowledged.len()
            {
                return Err(changed());
            }
            for (index, (recipient, _)) in row.recipient_bindings.iter().enumerate() {
                if let Some(terminal) = terminals[index]
                    && !acknowledged[index]
                    && !terminal.released
                    && !self.acknowledgement_protects(row.sequence, recipient)
                    && let Some(at) = terminal
                        .observed_at_ms
                        .checked_add(MESSAGE_TERMINAL_HANDOFF_MS)
                {
                    let at = if known.is_some_and(|clock| clock >= at) {
                        0
                    } else {
                        at
                    };
                    next = Some(next.map_or(at, |prior: u64| prior.min(at)));
                }
            }
        }
        Ok(next)
    }
}
