//! Actual expiry attempt lifecycle and idempotent source-bound publication.

use super::*;

impl MessageService {
    /// Issues the exact reserved original once under its actual storage scope.
    pub fn begin_storage_expiry_write(
        &mut self,
        ticket: &MessageExpiryTicket,
        scope: &MessageStorageScope,
    ) -> Result<MessageExpiryWrite> {
        self.require_expiry_owner(&ticket.identity)?;
        let pending = self.expiry.as_ref().ok_or_else(changed)?;
        if &pending.identity.scope != scope || pending.attempt.is_some() {
            return Err(changed());
        }
        // The actual worker may decode this one source to validate original TTL
        // and slots. Existing backed cache pins can be evicted, while external
        // live pins and this real decoder reservation remain fully charged.
        let payload = self
            .reserve_storage_payload(0, pending.identity.decode_payload_bytes, 0, None)?
            .ok_or_else(changed)?;
        Ok(Self::replace_expiry_attempt(
            self.expiry.as_mut().ok_or_else(changed)?,
            payload,
        ))
    }
    /// Validates the exact issued nonce without interpreting a status or clock.
    pub fn validate_expiry_attempt(&self, attempt: &MessageExpiryAttempt) -> Result<()> {
        self.require_expiry_attempt(&attempt.identity, &attempt.attempt)
    }
    /// Retries only actual joined uncertainty with a fresh nonce and unchanged
    /// first source/slots/clock. The caller owns physical join/backoff scheduling.
    pub fn retry_expiry_write(
        &mut self,
        uncertain: MessageExpiryUncertain,
    ) -> Result<MessageExpiryWrite> {
        self.require_expiry_attempt(&uncertain.write.identity, &uncertain.write.attempt)?;
        Ok(Self::replace_expiry_attempt(
            self.expiry.as_mut().ok_or_else(changed)?,
            uncertain.write.payload,
        ))
    }
    /// Reconstructs lost linear input only after the trusted actual join port
    /// qualifies this exact attempt. A failed qualification changes no owner.
    pub fn recover_joined_expiry_write(
        &mut self,
        attempt: &MessageExpiryAttempt,
        joined: &mut impl MessageExpiryJoin,
    ) -> Result<MessageExpiryWrite> {
        self.require_expiry_attempt(&attempt.identity, &attempt.attempt)?;
        joined.qualify_joined(attempt)?;
        Ok(Self::replace_expiry_attempt(
            self.expiry.as_mut().ok_or_else(changed)?,
            attempt.payload.clone(),
        ))
    }
    /// Cancels only an original that never issued physical work. Issued error,
    /// cancellation, panic or lost reply cannot release its exact slot fence.
    pub fn cancel_storage_expiry(&mut self, ticket: &MessageExpiryTicket) -> Result<()> {
        self.require_expiry_owner(&ticket.identity)?;
        if self.expiry.as_ref().is_none_or(|p| p.attempt.is_some()) {
            return Err(changed());
        }
        self.expiry = None;
        Ok(())
    }
    /// Applies one actual positive to its original slots after validating every
    /// before-image. It never advances cursors or releases sender outcomes.
    /// Repeated adoption of the same positive is inert even after later cleanup;
    /// a foreign service or stale issued attempt cannot consume it.
    pub fn publish_expiry(&mut self, commit: &MessageExpiryCommit) -> Result<usize> {
        if !Arc::ptr_eq(&commit.identity.owner, &self.acceptance_runtime) {
            return Err(changed());
        }
        if commit.identity.published.load(Ordering::Acquire) {
            return Ok(0);
        }
        self.require_expiry_attempt(&commit.identity, &commit.attempt)?;
        let row = &commit.identity.source;
        if !self
            .pending
            .get(&row.sequence)
            .is_some_and(|current| Arc::ptr_eq(current, row))
        {
            return Err(changed());
        }
        let acknowledged = row.acknowledged.lock().map_err(|_| changed())?;
        let mut terminal = row.terminal.lock().map_err(|_| changed())?;
        if commit.identity.indices.iter().any(|index| {
            acknowledged.get(*index as usize) != Some(&false)
                || terminal.get(*index as usize).is_none_or(Option::is_some)
        }) {
            return Err(changed());
        }
        for index in &commit.identity.indices {
            terminal[*index as usize] = Some(MessageDeliveryTerminalSnapshot {
                reason: DeliveryTerminalReason::Expired,
                observed_at_ms: commit.identity.observed_at_ms,
                released: false,
            });
        }
        // New observations are unreleased, so they cannot remove pending rows.
        // Publication has no fallible cleanup after the first actual mutation.
        commit.identity.published.store(true, Ordering::Release);
        self.expiry = None;
        Ok(commit.identity.indices.len())
    }
    /// Replaces only a validated current nonce; immutable source data is shared.
    fn replace_expiry_attempt(
        pending: &mut PendingExpiry,
        payload: super::super::payload_budget::PayloadReservation,
    ) -> MessageExpiryWrite {
        let attempt = Arc::new(());
        pending.attempt = Some(attempt.clone());
        MessageExpiryWrite {
            identity: pending.identity.clone(),
            attempt,
            payload,
        }
    }
    /// Requires the actual live service, original scope and existing reservation.
    fn require_expiry_owner(&self, identity: &Arc<Identity>) -> Result<()> {
        if !Arc::ptr_eq(&identity.owner, &self.acceptance_runtime)
            || !self
                .storage_custody
                .as_ref()
                .is_some_and(|p| p.scope == identity.scope)
            || !self
                .expiry
                .as_ref()
                .is_some_and(|p| Arc::ptr_eq(&p.identity, identity))
        {
            return Err(changed());
        }
        Ok(())
    }
    /// Adds the exact currently issued worker nonce to original-owner checks.
    fn require_expiry_attempt(&self, identity: &Arc<Identity>, attempt: &Arc<()>) -> Result<()> {
        self.require_expiry_owner(identity)?;
        if !self
            .expiry
            .as_ref()
            .and_then(|p| p.attempt.as_ref())
            .is_some_and(|current| Arc::ptr_eq(current, attempt))
        {
            return Err(changed());
        }
        Ok(())
    }
}
