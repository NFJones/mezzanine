//! Bounded retained-message storage and its derived lookup indexes.
//!
//! Enqueue reserves custody before assigning an occurrence or pruning replay.
//! Replay and pending references share the same immutable occurrence; delivery
//! indexes, receipts and provenance survive until both references are released.

use super::*;

impl MessageService {
    /// Assigns an occurrence sequence and retains the envelope within queue budgets.
    pub(super) fn enqueue(
        &mut self,
        envelope: Arc<Envelope>,
        audience: ResolvedMessageAudience,
        recipient_bindings: Vec<(AgentId, MessageSequence)>,
        now_ms: u64,
    ) -> Result<MessageSequence> {
        let size = envelope.payload.len();
        if size > self.retention_bytes {
            return Err(MessageError::invalid_args(MMP_PAYLOAD_TOO_LARGE_MESSAGE));
        }
        self.reserve_auxiliary_delivery(&envelope, &recipient_bindings)?;
        self.reserve_recipient_bindings(&recipient_bindings, size)?;
        self.reserve_pending_delivery(&recipient_bindings, size)?;
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| MessageError::invalid_state("message sequence number exhausted"))?;
        self.queued_bytes += size;
        let sender_registered_after_sequence = self
            .registration_floors
            .get(&envelope.sender.agent_id)
            .copied();
        let queued = Arc::new(QueuedEnvelope {
            sequence,
            envelope,
            audience,
            offered_at_ms: std::sync::Mutex::new(vec![None; recipient_bindings.len()]),
            acknowledged: std::sync::Mutex::new(vec![false; recipient_bindings.len()]),
            terminal: std::sync::Mutex::new(vec![None; recipient_bindings.len()]),
            sender_registered_after_sequence,
            ack_expiry_observed_at_ms: std::sync::Mutex::new(None),
            recipient_bindings,
            accepted_at_ms: now_ms,
        });
        self.insert_queue_indexes(&queued);
        self.pending.insert(sequence, queued.clone());
        self.queue.push_back(queued);
        while self.queue.len() > self.retention_messages || self.queued_bytes > self.retention_bytes
        {
            if let Some(removed) = self.queue.pop_front() {
                if !self.pending.contains_key(&removed.sequence) {
                    self.remove_queue_indexes(&removed);
                }
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
        let retained = self
            .queue
            .iter()
            .chain(self.pending.values())
            .cloned()
            .collect::<Vec<_>>();
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
    pub(super) fn remove_queue_indexes(&mut self, queued: &QueuedEnvelope) {
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
            .queued_by_sequence
            .values()
            .map(|queued| queued.envelope.id.clone())
            .collect::<HashSet<_>>();
        self.accepted_messages
            .retain(|message_id, _| retained_ids.contains(message_id));
    }

    /// Retains reusable floors for their original delivery interval. Fresh
    /// floors lose only recorded versions no longer referenced by canonical
    /// authored mail. One queue projection plus per-owner objective hash sets
    /// avoids all-floors-by-all-mail scans or cloning large objectives per enqueue.
    /// A projection can never synthesize a new sender version or rewrite mail.
    pub(super) fn prune_retired_delivery_floors(&mut self) {
        let Some(oldest_retained_sequence) = self
            .queued_by_sequence
            .first_key_value()
            .map(|(sequence, _)| *sequence)
        else {
            self.retired_delivery_floors.clear();
            return;
        };
        let referenced = {
            let known = self
                .retired_delivery_floors
                .iter()
                .filter_map(|(id, floor)| {
                    floor.fresh_sender_versions.as_ref().map(|versions| {
                        (
                            id,
                            versions
                                .iter()
                                .map(|version| &version.objective)
                                .collect::<HashSet<_>>(),
                        )
                    })
                })
                .collect::<HashMap<_, _>>();
            let mut referenced: HashMap<
                AgentId,
                (HashMap<&Option<String>, MessageSequence>, MessageSequence),
            > = HashMap::new();
            if !known.is_empty() {
                for queued in self.queued_by_sequence.values() {
                    let sender = &queued.envelope.sender;
                    let Some(objectives) = known.get(&sender.agent_id) else {
                        continue;
                    };
                    let floor = &self.retired_delivery_floors[&sender.agent_id];
                    if same_immutable_sender(&floor.identity, sender)
                        && objectives.contains(&sender.objective)
                    {
                        let entry = referenced.entry(sender.agent_id.clone()).or_default();
                        entry
                            .0
                            .entry(&sender.objective)
                            .and_modify(|first| *first = (*first).min(queued.sequence))
                            .or_insert(queued.sequence);
                        entry.1 = entry.1.max(queued.sequence);
                    }
                }
            }
            referenced
        };
        self.retired_delivery_floors.retain(|id, floor| {
            let Some(versions) = floor.fresh_sender_versions.as_mut() else {
                return floor.last_sequence >= oldest_retained_sequence;
            };
            let Some((first_sequences, last_sequence)) = referenced.get(id) else {
                return false;
            };
            versions.retain(|version| first_sequences.contains_key(&version.objective));
            // A->B->A recurrence can leave B as the first surviving occurrence
            // after the initial A evicts. Sort only recorded, retained variants;
            // filtering above proves every sort key exists without a fallback.
            versions.sort_by_key(|version| first_sequences[&version.objective]);
            let Some(first) = versions.first() else {
                return false;
            };
            if floor.identity != *first {
                floor.identity = first.clone();
            }
            floor.last_sequence = *last_sequence;
            true
        });
    }
}
