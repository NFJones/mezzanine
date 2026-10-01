//! Bounded retained-message storage and its derived lookup indexes.
//!
//! Enqueue assigns one occurrence sequence before retention pruning. Accepted
//! message receipts and retired-owner floors remain bounded by the same queue;
//! no independent delivery store or cursor authority is introduced here.

use super::*;

impl MessageService {
    /// Assigns an occurrence sequence and retains the envelope within queue budgets.
    pub(super) fn enqueue(
        &mut self,
        envelope: Arc<Envelope>,
        audience: ResolvedMessageAudience,
        now_ms: u64,
    ) -> Result<MessageSequence> {
        let size = envelope.payload.len();
        if size > self.retention_bytes {
            return Err(MessageError::invalid_args(MMP_PAYLOAD_TOO_LARGE_MESSAGE));
        }
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| MessageError::invalid_state("message sequence number exhausted"))?;
        self.queued_bytes += size;
        let queued = Arc::new(QueuedEnvelope {
            sequence,
            envelope,
            audience,
            accepted_at_ms: now_ms,
        });
        self.insert_queue_indexes(&queued);
        self.queue.push_back(queued);
        while self.queue.len() > self.retention_messages || self.queued_bytes > self.retention_bytes
        {
            if let Some(removed) = self.queue.pop_front() {
                self.remove_queue_indexes(&removed);
                self.queued_bytes = self
                    .queued_bytes
                    .saturating_sub(removed.envelope.payload.len());
            }
        }
        self.prune_retired_delivery_floors();
        Ok(sequence)
    }

    /// Reconstructs lookup projections from canonical retained queue order.
    pub(super) fn rebuild_queue_indexes(&mut self) {
        self.queued_by_sequence.clear();
        self.queued_by_recipient.clear();
        let retained = self.queue.iter().cloned().collect::<Vec<_>>();
        for queued in retained {
            self.insert_queue_indexes(&queued);
        }
    }

    /// Adds one retained occurrence to both lookup projections.
    fn insert_queue_indexes(&mut self, queued: &Arc<QueuedEnvelope>) {
        self.queued_by_sequence
            .insert(queued.sequence, queued.clone());
        self.queued_by_recipient
            .entry(queued.envelope.recipient.clone())
            .or_default()
            .insert(queued.sequence);
    }

    /// Removes an evicted occurrence and any empty recipient index.
    fn remove_queue_indexes(&mut self, queued: &QueuedEnvelope) {
        self.queued_by_sequence.remove(&queued.sequence);
        if let Some(sequences) = self.queued_by_recipient.get_mut(&queued.envelope.recipient) {
            sequences.remove(&queued.sequence);
            if sequences.is_empty() {
                self.queued_by_recipient.remove(&queued.envelope.recipient);
            }
        }
    }

    /// Retires idempotency records once their canonical envelopes are evicted.
    pub(super) fn prune_accepted_messages_to_retained_queue(&mut self) {
        let retained_ids = self
            .queue
            .iter()
            .map(|queued| queued.envelope.id.clone())
            .collect::<HashSet<_>>();
        self.accepted_messages
            .retain(|message_id, _| retained_ids.contains(message_id));
    }

    /// Retains retired-owner floors only while their sequence interval exists.
    pub(super) fn prune_retired_delivery_floors(&mut self) {
        let Some(oldest_retained_sequence) = self.queue.front().map(|queued| queued.sequence)
        else {
            self.retired_delivery_floors.clear();
            return;
        };
        self.retired_delivery_floors
            .retain(|_, (_, floor)| *floor >= oldest_retained_sequence);
    }
}
