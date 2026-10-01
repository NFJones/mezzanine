//! Durable cursor ownership and retained delivery projections.
//!
//! Live reads retain presence and TTL filtering. Historical reconstruction
//! deliberately preserves acceptance-time evidence without advancing cursors.

use super::*;

impl MessageService {
    /// Subscribes a registered identity to messages after the current high-water.
    pub fn subscribe(&mut self, recipient: &AgentId) -> Result<DeliveryCursor> {
        self.registered.get(recipient).ok_or_else(|| {
            MessageError::forbidden("delivery subscription requires registered agent")
        })?;
        let cursor = DeliveryCursor {
            recipient: recipient.clone(),
            last_sequence: self.last_sequence(),
        };
        self.subscription_order
            .insert(recipient.as_str().to_string(), recipient.clone());
        self.subscriptions.insert(recipient.clone(), cursor.clone());
        Ok(cursor)
    }

    /// Subscribes from retained history, preserving any retired-owner floor.
    /// Unlike [`Self::subscribe`], unread messages accepted while idle remain visible.
    pub fn subscribe_from_retained_start(&mut self, recipient: &AgentId) -> Result<DeliveryCursor> {
        self.registered.get(recipient).ok_or_else(|| {
            MessageError::forbidden("delivery subscription requires registered agent")
        })?;
        let cursor = DeliveryCursor {
            recipient: recipient.clone(),
            last_sequence: self
                .retired_delivery_floors
                .remove(recipient)
                .map(|(_, last_sequence)| last_sequence)
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
            .receive_after_indexed(&start, identity, now_ms, usize::MAX, usize::MAX)
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
