//! Actual expiry attempt lifecycle and idempotent source-bound publication.

use super::*;

impl MessageService {
    /// Issues the exact reserved original once under its actual storage scope.
    pub fn begin_storage_terminal_write(
        &mut self,
        ticket: &MessageTerminalTicket,
        scope: &MessageStorageScope,
    ) -> Result<MessageTerminalWrite> {
        self.require_terminal_owner(&ticket.identity)?;
        let pending = self.terminal_commit.as_ref().ok_or_else(changed)?;
        if &pending.identity.scope != scope || pending.attempt.is_some() {
            return Err(changed());
        }
        // The actual worker may decode this one source to validate original TTL
        // and slots. Existing backed cache pins can be evicted, while external
        // live pins and this real decoder reservation remain fully charged.
        let payload = self
            .reserve_storage_payload(0, pending.identity.decode_payload_bytes, 0, None)?
            .ok_or_else(changed)?;
        Ok(Self::replace_terminal_attempt(
            self.terminal_commit.as_mut().ok_or_else(changed)?,
            payload,
        ))
    }
    /// Validates the exact issued nonce without interpreting a status or clock.
    pub fn validate_terminal_attempt(&self, attempt: &MessageTerminalAttempt) -> Result<()> {
        self.require_terminal_attempt(&attempt.identity, &attempt.attempt)
    }
    /// Retries only actual joined uncertainty with a fresh nonce and unchanged
    /// first source/slots/clock. The caller owns physical join/backoff scheduling.
    pub fn retry_terminal_write(
        &mut self,
        uncertain: MessageTerminalUncertain,
    ) -> Result<MessageTerminalWrite> {
        self.require_terminal_attempt(&uncertain.write.identity, &uncertain.write.attempt)?;
        Ok(Self::replace_terminal_attempt(
            self.terminal_commit.as_mut().ok_or_else(changed)?,
            uncertain.write.payload,
        ))
    }
    /// Reconstructs lost linear input only after the trusted actual join port
    /// qualifies this exact attempt. A failed qualification changes no owner.
    pub fn recover_joined_terminal_write(
        &mut self,
        attempt: &MessageTerminalAttempt,
        joined: &mut impl MessageTerminalJoin,
    ) -> Result<MessageTerminalWrite> {
        self.require_terminal_attempt(&attempt.identity, &attempt.attempt)?;
        joined.qualify_joined(attempt)?;
        Ok(Self::replace_terminal_attempt(
            self.terminal_commit.as_mut().ok_or_else(changed)?,
            attempt.payload.clone(),
        ))
    }
    /// Cancels only an original that never issued physical work. Issued error,
    /// cancellation, panic or lost reply cannot release its exact slot fence.
    pub fn cancel_storage_terminal(&mut self, ticket: &MessageTerminalTicket) -> Result<()> {
        self.require_terminal_owner(&ticket.identity)?;
        if self
            .terminal_commit
            .as_ref()
            .is_none_or(|p| p.attempt.is_some())
        {
            return Err(changed());
        }
        self.terminal_commit = None;
        Ok(())
    }
    /// Applies one actual positive to its original slots after validating every
    /// before-image. Observation never releases outcomes; a horizon positive only
    /// releases its exact original terminal slots. Neither advances cursors.
    /// Repeated adoption of the same positive is inert even after later cleanup;
    /// a foreign service or stale issued attempt cannot consume it.
    pub fn publish_terminal(&mut self, commit: &MessageTerminalCommit) -> Result<usize> {
        if !Arc::ptr_eq(&commit.identity.owner, &self.acceptance_runtime) {
            return Err(changed());
        }
        if commit.identity.published.load(Ordering::Acquire) {
            return Ok(0);
        }
        self.require_terminal_attempt(&commit.identity, &commit.attempt)?;
        let row = &commit.identity.source;
        if !self
            .pending
            .get(&row.sequence)
            .is_some_and(|current| Arc::ptr_eq(current, row))
        {
            return Err(changed());
        }
        let mut horizon = row.horizon_observed_at_ms.lock().map_err(|_| changed())?;
        let acknowledged = row.acknowledged.lock().map_err(|_| changed())?;
        let mut terminal = row.terminal.lock().map_err(|_| changed())?;
        if commit.identity.indices.len() != commit.identity.before.len()
            || commit
                .identity
                .indices
                .iter()
                .zip(&commit.identity.before)
                .any(|(index, before)| {
                    acknowledged.get(*index as usize) != Some(&false)
                        || terminal.get(*index as usize) != Some(before)
                })
        {
            return Err(changed());
        }
        for index in &commit.identity.indices {
            match commit.identity.kind {
                MessageTerminalKind::ExpiryObservation => {
                    terminal[*index as usize] = Some(MessageDeliveryTerminalSnapshot {
                        reason: DeliveryTerminalReason::Expired,
                        observed_at_ms: commit.identity.observed_at_ms,
                        released: false,
                    });
                }
                MessageTerminalKind::HorizonRelease => {
                    // Every exact Some before-image was validated above.
                    if let Some(value) = terminal[*index as usize].as_mut() {
                        value.released = true;
                    }
                }
            }
        }
        if commit.identity.kind == MessageTerminalKind::HorizonRelease {
            *horizon = Some(horizon.map_or(commit.identity.observed_at_ms, |prior| {
                prior.max(commit.identity.observed_at_ms)
            }));
        }
        drop(horizon);
        let complete = acknowledged
            .iter()
            .zip(terminal.iter())
            .all(|(ack, terminal)| *ack || terminal.is_some_and(|terminal| terminal.released));
        drop(terminal);
        drop(acknowledged);
        // No fallible global scan follows mutation. Only this fully validated
        // original can leave pending custody, while optional replay may retain
        // it. The positive identity continues charging source metadata itself.
        if complete {
            self.pending.remove(&row.sequence);
            if !self
                .queue
                .iter()
                .any(|value| value.sequence == row.sequence)
            {
                self.remove_queue_indexes(row);
            }
            self.prune_accepted_messages_to_retained_queue();
            self.prune_retired_delivery_floors();
        }
        commit.identity.published.store(true, Ordering::Release);
        self.terminal_commit = None;
        Ok(commit.identity.indices.len())
    }
    /// Replaces only a validated current nonce; immutable source data is shared.
    fn replace_terminal_attempt(
        pending: &mut PendingTerminal,
        payload: super::super::payload_budget::PayloadReservation,
    ) -> MessageTerminalWrite {
        let attempt = Arc::new(());
        pending.attempt = Some(attempt.clone());
        MessageTerminalWrite {
            identity: pending.identity.clone(),
            attempt,
            payload,
        }
    }
    /// Requires the actual live service, original scope and existing reservation.
    fn require_terminal_owner(&self, identity: &Arc<Identity>) -> Result<()> {
        if !Arc::ptr_eq(&identity.owner, &self.acceptance_runtime)
            || !self
                .storage_custody
                .as_ref()
                .is_some_and(|p| p.scope == identity.scope)
            || !self
                .terminal_commit
                .as_ref()
                .is_some_and(|p| Arc::ptr_eq(&p.identity, identity))
        {
            return Err(changed());
        }
        Ok(())
    }
    /// Adds the exact currently issued worker nonce to original-owner checks.
    fn require_terminal_attempt(&self, identity: &Arc<Identity>, attempt: &Arc<()>) -> Result<()> {
        self.require_terminal_owner(identity)?;
        if !self
            .terminal_commit
            .as_ref()
            .and_then(|p| p.attempt.as_ref())
            .is_some_and(|current| Arc::ptr_eq(current, attempt))
        {
            return Err(changed());
        }
        Ok(())
    }
}
