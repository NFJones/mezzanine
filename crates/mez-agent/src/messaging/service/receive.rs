//! Indexed live receive selection over canonical retained envelopes.
//!
//! Queue indexes are projections, not separate delivery authority. Selection
//! preserves occurrence order, TTL, audience and aggregate work budgets without
//! advancing a recipient cursor.

use super::*;

/// One selected batch and its bounded fanout work accounting.
#[derive(Debug)]
pub(super) struct IndexedReceiveSelection {
    pub(super) batch: DeliveryBatch,
    pub(super) sequence_lookups: u64,
    pub(super) payload_bytes: usize,
}

impl MessageService {
    /// Selects live deliveries after an authenticated recipient cursor.
    pub fn receive_after(
        &self,
        cursor: &DeliveryCursor,
        now_ms: u64,
        limit: usize,
    ) -> Result<DeliveryBatch> {
        let identity = self.registered.get(&cursor.recipient).ok_or_else(|| {
            MessageError::forbidden("delivery cursor recipient is not registered")
        })?;
        if !self.recipient_is_available(&identity.agent_id) {
            return Ok(DeliveryBatch {
                cursor: cursor.clone(),
                messages: Vec::new(),
            });
        }
        Ok(self
            .receive_after_indexed(cursor, identity, now_ms, limit, usize::MAX)
            .batch)
    }

    /// Merges selector indexes in occurrence order under message and byte limits.
    pub(super) fn receive_after_indexed(
        &self,
        cursor: &DeliveryCursor,
        identity: &SenderIdentity,
        now_ms: u64,
        limit: usize,
        max_payload_bytes: usize,
    ) -> IndexedReceiveSelection {
        let selectors = recipient_selectors(identity);
        let mut next_by_selector = std::collections::BinaryHeap::new();
        for (index, selector) in selectors.iter().enumerate() {
            if let Some(sequence) = self
                .queued_by_recipient
                .get(selector)
                .and_then(|sequences| {
                    sequences
                        .range((
                            std::ops::Bound::Excluded(cursor.last_sequence),
                            std::ops::Bound::Unbounded,
                        ))
                        .next()
                        .copied()
                })
            {
                next_by_selector.push(std::cmp::Reverse((sequence, index)));
            }
        }

        let mut messages = Vec::new();
        let mut sequence_lookups = 0u64;
        let mut payload_bytes = 0usize;
        while messages.len() < limit {
            let Some(std::cmp::Reverse((sequence, selector_index))) = next_by_selector.pop() else {
                break;
            };
            sequence_lookups = sequence_lookups.saturating_add(1);
            let Some(queued) = self.queued_by_sequence.get(&sequence) else {
                continue;
            };
            let message_bytes = queued.envelope.payload.len();
            if !expired(queued, now_ms) && audience_matches(identity, &queued.audience) {
                if payload_bytes.saturating_add(message_bytes) > max_payload_bytes {
                    break;
                }
                payload_bytes = payload_bytes.saturating_add(message_bytes);
                messages.push(SequencedEnvelope {
                    sequence,
                    envelope: queued.envelope.clone(),
                });
            }
            if let Some(next_sequence) = self
                .queued_by_recipient
                .get(&selectors[selector_index])
                .and_then(|sequences| {
                    sequences
                        .range((
                            std::ops::Bound::Excluded(sequence),
                            std::ops::Bound::Unbounded,
                        ))
                        .next()
                        .copied()
                })
            {
                next_by_selector.push(std::cmp::Reverse((next_sequence, selector_index)));
            }
        }

        IndexedReceiveSelection {
            batch: DeliveryBatch {
                cursor: cursor.clone(),
                messages,
            },
            sequence_lookups,
            payload_bytes,
        }
    }
}
