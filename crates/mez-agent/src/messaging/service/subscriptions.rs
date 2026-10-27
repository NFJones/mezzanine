//! Durable cursor ownership and retained delivery projections.
//!
//! Live reads retain presence and TTL filtering. Historical reconstruction
//! deliberately preserves acceptance-time evidence without advancing cursors.
//! Interrupted native commits can inspect existing exact offers outside live
//! filtering; that projection creates no authority and requires independent
//! canonical-context evidence before the caller acknowledges an occurrence.

use super::*;

impl MessageService {
    /// Projects previously offered, uncommitted occurrences for exact-owner
    /// context reconciliation, independent of current TTL and presence. This
    /// neither creates offers nor acknowledges delivery; callers must prove
    /// canonical commit before advancing the subscription. Pending custody is
    /// included even when disposable replay has evicted the occurrence.
    /// Returns Forbidden without a subscription and InvalidState when offer
    /// evidence is unavailable; no acknowledgement state changes on error.
    pub fn offered_uncommitted_deliveries(
        &self,
        recipient: &AgentId,
    ) -> Result<Vec<SequencedEnvelope>> {
        let cursor = self
            .subscriptions
            .get(recipient)
            .ok_or_else(|| MessageError::forbidden("agent has no delivery subscription"))?;
        self.queued_by_sequence
            .range((
                std::ops::Bound::Excluded(cursor.last_sequence),
                std::ops::Bound::Unbounded,
            ))
            .filter(|(_, row)| {
                self.accepted_recipient(row, recipient)
                    && !self.recipient_is_terminal(row, recipient)
            })
            .filter_map(|(_, row)| match super::offers::offered_at(row, recipient) {
                Ok(Some(_)) => Some(Ok(SequencedEnvelope {
                    sequence: row.sequence,
                    envelope: row.envelope.clone(),
                })),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }

    /// Checks lower ACK admissibility without claiming application commit.
    /// Native adapters still need canonical proof for every offered prefix.
    /// Returns false for an invalid/blocked endpoint or prefix; unavailable
    /// evidence and missing subscriptions retain their typed errors.
    pub fn can_advance_subscription(
        &self,
        recipient: &AgentId,
        sequence: MessageSequence,
    ) -> Result<bool> {
        let cursor = self
            .subscriptions
            .get(recipient)
            .ok_or_else(|| MessageError::forbidden("agent has no delivery subscription"))?;
        if sequence <= cursor.last_sequence {
            return Ok(true);
        }
        match self.validate_delivery_ack(recipient, cursor.last_sequence, sequence) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == MessageErrorKind::InvalidArgs => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Subscribes after current history without skipping accepted unread mail.
    pub fn subscribe(&mut self, recipient: &AgentId) -> Result<DeliveryCursor> {
        if let Some(cursor) = self.subscriptions.get(recipient) {
            return Ok(cursor.clone());
        }
        self.registered.get(recipient).ok_or_else(|| {
            MessageError::forbidden("delivery subscription requires registered agent")
        })?;
        let cursor = DeliveryCursor {
            recipient: recipient.clone(),
            last_sequence: self
                .pending
                .values()
                .find(|queued| {
                    self.accepted_recipient(queued, recipient)
                        && !self.recipient_has_committed(queued, recipient)
                })
                .map(|queued| queued.sequence - 1)
                .unwrap_or(self.last_sequence()),
        };
        self.subscription_order
            .insert(recipient.as_str().to_string(), recipient.clone());
        self.subscriptions.insert(recipient.clone(), cursor.clone());
        Ok(cursor)
    }

    /// Subscribes from retained history, preserving any retired-owner floor.
    /// Unlike [`Self::subscribe`], unread messages accepted while idle remain visible.
    pub fn subscribe_from_retained_start(&mut self, recipient: &AgentId) -> Result<DeliveryCursor> {
        if let Some(cursor) = self.subscriptions.get(recipient) {
            return Ok(cursor.clone());
        }
        self.registered.get(recipient).ok_or_else(|| {
            MessageError::forbidden("delivery subscription requires registered agent")
        })?;
        let cursor = DeliveryCursor {
            recipient: recipient.clone(),
            last_sequence: self
                .retired_delivery_floors
                .remove(recipient)
                .map(|floor| floor.last_sequence)
                .unwrap_or(0),
        };
        self.subscription_order
            .insert(recipient.as_str().to_string(), recipient.clone());
        self.subscriptions.insert(recipient.clone(), cursor.clone());
        Ok(cursor)
    }

    /// Returns the canonical acknowledgement cursor for a subscribed identity.
    pub fn subscription(&self, recipient: &AgentId) -> Option<&DeliveryCursor> {
        self.subscriptions.get(recipient)
    }

    /// Reads a live batch after the cursor without acknowledging it.
    pub fn receive_subscribed(
        &self,
        recipient: &AgentId,
        now_ms: u64,
        limit: usize,
    ) -> Result<DeliveryBatch> {
        let cursor = self
            .subscriptions
            .get(recipient)
            .ok_or_else(|| MessageError::forbidden("agent has no delivery subscription"))?;
        self.receive_after(cursor, now_ms, limit)
    }

    /// Projects retained deliveries through a sequence with live presence/TTL policy.
    /// This recovery view never advances the recipient cursor.
    pub fn receive_through_subscribed(
        &self,
        recipient: &AgentId,
        sequence: MessageSequence,
        now_ms: u64,
    ) -> Result<Vec<SequencedEnvelope>> {
        let cursor = self
            .subscriptions
            .get(recipient)
            .ok_or_else(|| MessageError::forbidden("agent has no delivery subscription"))?;
        let identity = self.registered.get(recipient).ok_or_else(|| {
            MessageError::forbidden("delivery cursor recipient is not registered")
        })?;
        if !self.recipient_is_available(&identity.agent_id) {
            return Ok(Vec::new());
        }
        let start = DeliveryCursor {
            recipient: cursor.recipient.clone(),
            last_sequence: 0,
        };
        Ok(self
            .receive_after_indexed(
                &start,
                identity,
                now_ms,
                usize::MAX,
                usize::MAX,
                super::receive::ReceiveSelectionFilter {
                    include_committed: true,
                    native_commits: None,
                },
            )
            .batch
            .messages
            .into_iter()
            .filter(|message| message.sequence <= sequence)
            .collect())
    }

    /// Reconstructs already acknowledged retained evidence independent of presence/TTL.
    /// Audience and selector checks remain mandatory, with occurrence deduplication.
    pub fn historical_receive_through_subscribed(
        &self,
        recipient: &AgentId,
        sequence: MessageSequence,
    ) -> Result<Vec<SequencedEnvelope>> {
        if !self.subscriptions.contains_key(recipient) {
            return Err(MessageError::forbidden(
                "agent has no delivery subscription",
            ));
        }
        let identity = self.registered.get(recipient).ok_or_else(|| {
            MessageError::forbidden("delivery cursor recipient is not registered")
        })?;
        let mut sequences = std::collections::BTreeSet::new();
        Ok(self
            .queue
            .iter()
            .filter(|queued| queued.sequence <= sequence)
            .filter(|queued| self.accepted_recipient(queued, recipient))
            .filter(|queued| !self.recipient_is_terminal(queued, recipient))
            .filter(|queued| recipient_matches(identity, &queued.envelope.recipient))
            .filter(|queued| audience_matches(identity, &queued.audience))
            .filter(|queued| sequences.insert(queued.sequence))
            .map(|queued| SequencedEnvelope {
                sequence: queued.sequence,
                envelope: queued.envelope.clone(),
            })
            .collect())
    }
}
