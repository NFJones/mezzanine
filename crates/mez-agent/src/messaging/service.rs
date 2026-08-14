//! In-memory message service, delivery queues, subscriptions, and presence.
//!
//! The service validates authenticated sender identity, stores bounded messages,
//! matches recipients, manages fanout cursors, and filters expired envelopes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use mez_core::ids::{AgentId, IdFactory, PaneId, StableId, WindowId};

use super::error::{MessageError, MessageErrorKind, Result};

use super::types::{
    AcceptedMessage, AgentPresenceStatus, Delivery, DeliveryBatch, DeliveryCursor, DeliveryStatus,
    Envelope, FanoutBatch, FanoutBudget, MMP_DUPLICATE_MESSAGE_ID_MESSAGE, MMP_EXPIRED_MESSAGE,
    MMP_PAYLOAD_TOO_LARGE_MESSAGE, MMP_PROTOCOL, MMP_UNDELIVERABLE_MESSAGE,
    MessageAcceptedSnapshot, MessageDeliveryCursorSnapshot, MessageDeliverySnapshot,
    MessageEnvelopeSnapshot, MessageExtensionFieldSnapshot, MessageFanoutDiagnostics,
    MessageIdentitySnapshot, MessagePresenceSnapshot, MessageQueuedEnvelopeSnapshot,
    MessageRecipientSnapshot, MessageSequence, MessageService, MessageServiceSnapshot,
    PresenceRecord, QueuedEnvelope, Recipient, SenderIdentity, SequencedEnvelope,
};
use super::validation::{validate_message_type, validate_protocol, validate_sender_identity};

#[derive(Debug)]
struct IndexedReceiveSelection {
    batch: DeliveryBatch,
    sequence_lookups: u64,
    payload_bytes: usize,
}

impl Default for MessageService {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self::with_limits(1000, 1024 * 1024)
    }
}

impl MessageService {
    /// Runs the with limits operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn with_limits(retention_messages: usize, retention_bytes: usize) -> Self {
        Self {
            ids: IdFactory::default(),
            registered: HashMap::new(),
            presence: HashMap::new(),
            subscriptions: HashMap::new(),
            accepted_messages: HashMap::new(),
            queue: VecDeque::new(),
            queued_by_sequence: Default::default(),
            queued_by_recipient: HashMap::new(),
            subscription_order: Default::default(),
            fanout_after_recipient: None,
            fanout_diagnostics: MessageFanoutDiagnostics::default(),
            next_sequence: 1,
            retention_messages: retention_messages.max(1),
            retention_bytes: retention_bytes.max(1),
            queued_bytes: 0,
        }
    }

    /// Runs the register agent operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn register_agent(
        &mut self,
        pane_id: Option<PaneId>,
        window_id: Option<WindowId>,
        role: impl Into<String>,
        capabilities: Vec<String>,
    ) -> SenderIdentity {
        let identity = SenderIdentity {
            agent_id: self.ids.agent(),
            pane_id,
            window_id,
            role: Some(role.into()),
            capabilities,
        };
        self.registered
            .insert(identity.agent_id.clone(), identity.clone());
        self.presence.insert(
            identity.agent_id.clone(),
            PresenceRecord {
                identity: identity.clone(),
                status: AgentPresenceStatus::Available,
                updated_at_ms: 0,
            },
        );
        identity
    }

    /// Runs the ensure agent identity operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn ensure_agent_identity(
        &mut self,
        identity: SenderIdentity,
        updated_at_ms: u64,
    ) -> Result<SenderIdentity> {
        validate_sender_identity(&identity)?;
        if let Some(existing) = self.registered.get(&identity.agent_id) {
            return Ok(existing.clone());
        }
        self.registered
            .insert(identity.agent_id.clone(), identity.clone());
        self.presence.insert(
            identity.agent_id.clone(),
            PresenceRecord {
                identity: identity.clone(),
                status: AgentPresenceStatus::Available,
                updated_at_ms,
            },
        );
        Ok(identity)
    }

    /// Runs the accept operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn accept(&mut self, connection_agent: &AgentId, envelope: Envelope) -> Result<Delivery> {
        self.accept_at(connection_agent, envelope, 0)
    }

    /// Runs the accept at operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn accept_at(
        &mut self,
        connection_agent: &AgentId,
        envelope: Envelope,
        now_ms: u64,
    ) -> Result<Delivery> {
        validate_protocol(envelope.protocol)?;
        validate_message_type(&envelope.message_type)?;

        let registered = self
            .registered
            .get(connection_agent)
            .ok_or_else(|| MessageError::forbidden("unregistered agent connection"))?;

        if &envelope.sender != registered {
            return Err(MessageError::forbidden(
                "message sender does not match authenticated agent connection",
            ));
        }
        if let Some(accepted) = self.accepted_messages.get_mut(&envelope.id) {
            if accepted.envelope.as_ref() == &envelope {
                if envelope_expired_at(&accepted.envelope, accepted.accepted_at_ms, now_ms) {
                    accepted.delivery.status = DeliveryStatus::Expired;
                }
                return Ok(accepted.delivery.clone());
            }
            return Err(MessageError::conflict(MMP_DUPLICATE_MESSAGE_ID_MESSAGE));
        }

        let queued_recipients = self.matching_recipients(&envelope).len();
        if queued_recipients == 0 {
            return Err(MessageError::new(
                MessageErrorKind::NotFound,
                MMP_UNDELIVERABLE_MESSAGE,
            ));
        }
        if expires_before_delivery(&envelope) {
            return Err(MessageError::invalid_state(MMP_EXPIRED_MESSAGE));
        }
        let message_id = envelope.id.clone();
        let accepted_envelope = Arc::new(envelope);
        let sequence = self.enqueue(accepted_envelope.clone(), now_ms)?;

        let delivery = Delivery {
            accepted: true,
            message_id: message_id.clone(),
            sequence,
            queued_recipients,
            status: DeliveryStatus::Accepted,
        };
        self.accepted_messages.insert(
            message_id,
            AcceptedMessage {
                envelope: accepted_envelope,
                delivery: delivery.clone(),
                accepted_at_ms: now_ms,
            },
        );
        self.prune_accepted_messages_to_retained_queue();
        Ok(delivery)
    }

    /// Runs the discover agents operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn discover_agents(&self) -> Vec<SenderIdentity> {
        self.discover_agents_filtered(None, None, None, None, None, &[])
    }

    /// Counts queued local messages whose recipient is scoped to `window_id`.
    ///
    /// This is a delivery-queue view used by runtime status surfaces. It does
    /// not claim durable read receipts; it only reports messages that remain in
    /// the in-memory queue for the exact window recipient.
    pub fn queued_window_message_count(&self, window_id: &WindowId) -> usize {
        self.queue
            .iter()
            .filter(|queued| matches!(&queued.envelope.recipient, Recipient::Window(id) if id == window_id))
            .count()
    }

    /// Discovers registered agents that match every supplied identity,
    /// presence, and capability filter.
    pub fn discover_agents_filtered(
        &self,
        agent_id: Option<&str>,
        pane_id: Option<&str>,
        window_id: Option<&str>,
        role: Option<&str>,
        status: Option<AgentPresenceStatus>,
        capabilities: &[String],
    ) -> Vec<SenderIdentity> {
        let mut agents =
            self.registered
                .values()
                .filter(|identity| {
                    agent_id.is_none_or(|agent_id| identity.agent_id.as_str() == agent_id)
                        && pane_id.is_none_or(|pane_id| {
                            identity.pane_id.as_ref().is_some_and(|identity_pane_id| {
                                identity_pane_id.as_str() == pane_id
                            })
                        })
                        && window_id.is_none_or(|window_id| {
                            identity
                                .window_id
                                .as_ref()
                                .is_some_and(|identity_window_id| {
                                    identity_window_id.as_str() == window_id
                                })
                        })
                        && role.is_none_or(|role| identity.role.as_deref() == Some(role))
                        && status.is_none_or(|status| {
                            self.presence
                                .get(&identity.agent_id)
                                .is_some_and(|presence| presence.status == status)
                        })
                        && capabilities.iter().all(|capability| {
                            identity
                                .capabilities
                                .iter()
                                .any(|registered| registered == capability)
                        })
                })
                .cloned()
                .collect::<Vec<_>>();
        agents.sort_by(|left, right| left.agent_id.as_str().cmp(right.agent_id.as_str()));
        agents
    }

    /// Runs the registered identity operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn registered_identity(&self, agent_id: &AgentId) -> Option<&SenderIdentity> {
        self.registered.get(agent_id)
    }

    /// Runs the update presence operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn update_presence(
        &mut self,
        agent_id: &AgentId,
        status: AgentPresenceStatus,
        now_ms: u64,
    ) -> Result<()> {
        let presence = self
            .presence
            .get_mut(agent_id)
            .ok_or_else(|| MessageError::not_found("agent not found"))?;
        presence.status = status;
        presence.updated_at_ms = now_ms;
        Ok(())
    }

    /// Records a heartbeat for a registered agent without changing its
    /// declared presence status.
    pub fn record_heartbeat(&mut self, agent_id: &AgentId, now_ms: u64) -> Result<()> {
        let presence = self
            .presence
            .get_mut(agent_id)
            .ok_or_else(|| MessageError::not_found("agent not found"))?;
        presence.updated_at_ms = now_ms;
        Ok(())
    }

    /// Runs the presence operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn presence(&self) -> Vec<PresenceRecord> {
        let mut records = self.presence.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| {
            left.identity
                .agent_id
                .as_str()
                .cmp(right.identity.agent_id.as_str())
        });
        records
    }

    /// Returns a serializable snapshot of durable local message protocol state.
    pub fn snapshot_state(&self) -> MessageServiceSnapshot {
        let mut registered_agents = self
            .registered
            .values()
            .map(identity_snapshot)
            .collect::<Vec<_>>();
        registered_agents.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        let presence = self
            .presence()
            .iter()
            .map(presence_snapshot)
            .collect::<Vec<_>>();
        let mut subscriptions = self
            .subscriptions
            .values()
            .map(cursor_snapshot)
            .collect::<Vec<_>>();
        subscriptions.sort_by(|left, right| left.recipient.cmp(&right.recipient));
        let retained_messages = self
            .queue
            .iter()
            .map(|queued| queued_envelope_snapshot(queued))
            .collect::<Vec<_>>();
        let mut accepted_messages = self
            .accepted_messages
            .values()
            .map(accepted_message_snapshot)
            .collect::<Vec<_>>();
        accepted_messages.sort_by(|left, right| left.envelope.id.cmp(&right.envelope.id));

        MessageServiceSnapshot {
            protocol: MMP_PROTOCOL.to_string(),
            schema_version: 1,
            next_sequence: self.next_sequence,
            retention_messages: self.retention_messages,
            retention_bytes: self.retention_bytes,
            registered_agents,
            presence,
            subscriptions,
            retained_messages,
            accepted_messages,
        }
    }

    /// Rebuilds durable local message protocol state from a validated snapshot.
    pub fn from_snapshot_state(snapshot: &MessageServiceSnapshot) -> Result<Self> {
        validate_message_service_snapshot(snapshot)?;
        let mut registered = HashMap::new();
        let mut agent_ids = Vec::new();
        for identity in &snapshot.registered_agents {
            let identity = sender_identity_from_snapshot(identity)?;
            agent_ids.push(identity.agent_id.clone());
            registered.insert(identity.agent_id.clone(), identity);
        }
        let ids = IdFactory::after_existing_ids(agent_ids.iter());
        let mut presence = HashMap::new();
        for record in &snapshot.presence {
            let identity = sender_identity_from_snapshot(&record.identity)?;
            let status = parse_presence_status(&record.status)?;
            presence.insert(
                identity.agent_id.clone(),
                PresenceRecord {
                    identity,
                    status,
                    updated_at_ms: record.updated_at_ms,
                },
            );
        }
        let mut subscriptions = HashMap::new();
        let mut subscription_order = std::collections::BTreeMap::new();
        for cursor in &snapshot.subscriptions {
            let cursor = DeliveryCursor {
                recipient: parse_opaque_id(&cursor.recipient, "MMP delivery cursor recipient")?,
                last_sequence: cursor.last_sequence,
            };
            subscription_order.insert(
                cursor.recipient.as_str().to_string(),
                cursor.recipient.clone(),
            );
            subscriptions.insert(cursor.recipient.clone(), cursor);
        }
        let mut queue = VecDeque::new();
        let mut retained_envelopes = HashMap::new();
        let mut queued_bytes = 0usize;
        for retained in &snapshot.retained_messages {
            let envelope = Arc::new(envelope_from_snapshot(&retained.envelope)?);
            queued_bytes = queued_bytes.saturating_add(envelope.payload.len());
            retained_envelopes.insert(envelope.id.clone(), envelope.clone());
            queue.push_back(Arc::new(QueuedEnvelope {
                sequence: retained.sequence,
                envelope,
                accepted_at_ms: retained.accepted_at_ms,
            }));
        }
        let mut accepted_messages = HashMap::new();
        for accepted in &snapshot.accepted_messages {
            let envelope = envelope_from_snapshot(&accepted.envelope)?;
            let envelope = retained_envelopes
                .get(&envelope.id)
                .cloned()
                .unwrap_or_else(|| Arc::new(envelope));
            let delivery = Delivery {
                accepted: accepted.delivery.accepted,
                message_id: accepted.delivery.message_id.clone(),
                sequence: accepted.delivery.sequence,
                queued_recipients: accepted.delivery.queued_recipients,
                status: parse_delivery_status(&accepted.delivery.status)?,
            };
            accepted_messages.insert(
                envelope.id.clone(),
                AcceptedMessage {
                    envelope,
                    delivery,
                    accepted_at_ms: accepted.accepted_at_ms,
                },
            );
        }

        let mut service = Self {
            ids,
            registered,
            presence,
            subscriptions,
            accepted_messages,
            queue,
            queued_by_sequence: Default::default(),
            queued_by_recipient: HashMap::new(),
            subscription_order,
            fanout_after_recipient: None,
            fanout_diagnostics: MessageFanoutDiagnostics::default(),
            next_sequence: snapshot.next_sequence,
            retention_messages: snapshot.retention_messages,
            retention_bytes: snapshot.retention_bytes,
            queued_bytes,
        };
        service.rebuild_queue_indexes();
        service.prune_accepted_messages_to_retained_queue();
        Ok(service)
    }

    /// Runs the subscribe operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
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

    /// Creates a durable subscription that begins at the oldest retained
    /// message instead of at the current queue high-water mark.
    ///
    /// Runtime-owned agents use this boundary so a message accepted while no
    /// turn is active remains unread and can be committed immediately before
    /// the next prompt. Transport clients retain [`MessageService::subscribe`]
    /// semantics, which intentionally begin with only future messages.
    pub fn subscribe_from_retained_start(&mut self, recipient: &AgentId) -> Result<DeliveryCursor> {
        self.registered.get(recipient).ok_or_else(|| {
            MessageError::forbidden("delivery subscription requires registered agent")
        })?;
        let cursor = DeliveryCursor {
            recipient: recipient.clone(),
            last_sequence: 0,
        };
        self.subscription_order
            .insert(recipient.as_str().to_string(), recipient.clone());
        self.subscriptions.insert(recipient.clone(), cursor.clone());
        Ok(cursor)
    }

    /// Runs the subscription operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn subscription(&self, recipient: &AgentId) -> Option<&DeliveryCursor> {
        self.subscriptions.get(recipient)
    }

    /// Runs the receive subscribed operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
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

    /// Runs the fanout ready operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn fanout_ready(&mut self, now_ms: u64, limit_per_recipient: usize) -> Vec<FanoutBatch> {
        self.fanout_ready_with_budget(now_ms, limit_per_recipient, FanoutBudget::default())
    }

    /// Selects subscriber batches within one aggregate fair-work budget.
    pub fn fanout_ready_with_budget(
        &mut self,
        now_ms: u64,
        limit_per_recipient: usize,
        budget: FanoutBudget,
    ) -> Vec<FanoutBatch> {
        self.fanout_diagnostics.cycles = self.fanout_diagnostics.cycles.saturating_add(1);
        if budget.max_recipients == 0
            || budget.max_messages == 0
            || budget.max_payload_bytes == 0
            || limit_per_recipient == 0
            || self.subscription_order.is_empty()
        {
            return Vec::new();
        }

        let recipients = self.fanout_recipient_cycle(budget.max_recipients);
        let mut batches = Vec::new();
        let mut remaining_messages = budget.max_messages;
        let mut remaining_payload_bytes = budget.max_payload_bytes;
        for recipient in recipients.into_iter().take(budget.max_recipients) {
            self.fanout_after_recipient = Some(recipient.as_str().to_string());
            self.fanout_diagnostics.recipients_considered = self
                .fanout_diagnostics
                .recipients_considered
                .saturating_add(1);
            let Some(cursor) = self.subscriptions.get(&recipient).cloned() else {
                continue;
            };
            let Some(identity) = self.registered.get(&recipient) else {
                continue;
            };
            let selection = self.receive_after_indexed(
                &cursor,
                identity,
                now_ms,
                limit_per_recipient.min(remaining_messages),
                remaining_payload_bytes,
            );
            self.fanout_diagnostics.sequence_lookups = self
                .fanout_diagnostics
                .sequence_lookups
                .saturating_add(selection.sequence_lookups);
            if selection.batch.messages.is_empty() {
                continue;
            }
            let selected_messages = selection.batch.messages.len();
            remaining_messages = remaining_messages.saturating_sub(selected_messages);
            remaining_payload_bytes =
                remaining_payload_bytes.saturating_sub(selection.payload_bytes);
            self.fanout_diagnostics.messages_selected = self
                .fanout_diagnostics
                .messages_selected
                .saturating_add(u64::try_from(selected_messages).unwrap_or(u64::MAX));
            self.fanout_diagnostics.payload_bytes_selected = self
                .fanout_diagnostics
                .payload_bytes_selected
                .saturating_add(u64::try_from(selection.payload_bytes).unwrap_or(u64::MAX));
            batches.push(FanoutBatch {
                recipient,
                batch: selection.batch,
            });
            if remaining_messages == 0 || remaining_payload_bytes == 0 {
                break;
            }
        }
        batches
    }

    /// Returns cumulative bounded-fanout diagnostics.
    pub fn fanout_diagnostics(&self) -> MessageFanoutDiagnostics {
        self.fanout_diagnostics
    }

    fn fanout_recipient_cycle(&self, limit: usize) -> Vec<AgentId> {
        let mut recipients = Vec::with_capacity(limit.min(self.subscription_order.len()));
        if let Some(after) = self.fanout_after_recipient.as_ref() {
            recipients.extend(
                self.subscription_order
                    .range((
                        std::ops::Bound::Excluded(after.clone()),
                        std::ops::Bound::Unbounded,
                    ))
                    .take(limit)
                    .map(|(_, recipient)| recipient.clone()),
            );
            if recipients.len() < limit {
                recipients.extend(
                    self.subscription_order
                        .range(..=after.clone())
                        .take(limit - recipients.len())
                        .map(|(_, recipient)| recipient.clone()),
                );
            }
        } else {
            recipients.extend(self.subscription_order.values().take(limit).cloned());
        }
        recipients
    }

    /// Runs the fanout ready for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn fanout_ready_for(
        &self,
        recipient: &AgentId,
        now_ms: u64,
        limit: usize,
    ) -> Result<Option<FanoutBatch>> {
        let batch = self.receive_subscribed(recipient, now_ms, limit)?;
        if batch.messages.is_empty() {
            Ok(None)
        } else {
            Ok(Some(FanoutBatch {
                recipient: recipient.clone(),
                batch,
            }))
        }
    }

    /// Runs the acknowledge fanout batch operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn acknowledge_fanout_batch(&mut self, batch: &FanoutBatch) -> Result<DeliveryCursor> {
        let last_sequence = batch
            .batch
            .messages
            .last()
            .map(|message| message.sequence)
            .unwrap_or(batch.batch.cursor.last_sequence);
        self.advance_subscription(&batch.recipient, last_sequence)
    }

    /// Runs the receive after operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
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

    fn receive_after_indexed(
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
            if !expired(queued, now_ms) {
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

    /// Runs the advance subscription operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn advance_subscription(
        &mut self,
        recipient: &AgentId,
        sequence: MessageSequence,
    ) -> Result<DeliveryCursor> {
        if sequence > self.last_sequence() {
            return Err(MessageError::invalid_args(
                "delivery cursor cannot advance past the latest accepted message",
            ));
        }
        let cursor = self
            .subscriptions
            .get_mut(recipient)
            .ok_or_else(|| MessageError::forbidden("agent has no delivery subscription"))?;
        cursor.last_sequence = cursor.last_sequence.max(sequence);
        Ok(cursor.clone())
    }

    /// Runs the receive for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn receive_for(&self, recipient: &AgentId, now_ms: u64) -> Vec<Envelope> {
        self.queue
            .iter()
            .filter(|queued| !expired(queued, now_ms))
            .filter(|queued| {
                self.registered.get(recipient).is_some_and(|identity| {
                    self.recipient_is_available(&identity.agent_id)
                        && recipient_matches(identity, &queued.envelope.recipient)
                })
            })
            .map(|queued| queued.envelope.as_ref().clone())
            .collect()
    }

    /// Runs the responses for operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn responses_for(
        &self,
        recipient: &AgentId,
        correlation_id: &str,
        now_ms: u64,
    ) -> Vec<Envelope> {
        self.receive_for(recipient, now_ms)
            .into_iter()
            .filter(|envelope| envelope.correlation_id.as_deref() == Some(correlation_id))
            .collect()
    }

    /// Runs the enqueue operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn enqueue(&mut self, envelope: Arc<Envelope>, now_ms: u64) -> Result<MessageSequence> {
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
        Ok(sequence)
    }

    fn rebuild_queue_indexes(&mut self) {
        self.queued_by_sequence.clear();
        self.queued_by_recipient.clear();
        let retained = self.queue.iter().cloned().collect::<Vec<_>>();
        for queued in retained {
            self.insert_queue_indexes(&queued);
        }
    }

    fn insert_queue_indexes(&mut self, queued: &Arc<QueuedEnvelope>) {
        self.queued_by_sequence
            .insert(queued.sequence, queued.clone());
        self.queued_by_recipient
            .entry(queued.envelope.recipient.clone())
            .or_default()
            .insert(queued.sequence);
    }

    fn remove_queue_indexes(&mut self, queued: &QueuedEnvelope) {
        self.queued_by_sequence.remove(&queued.sequence);
        if let Some(sequences) = self.queued_by_recipient.get_mut(&queued.envelope.recipient) {
            sequences.remove(&queued.sequence);
            if sequences.is_empty() {
                self.queued_by_recipient.remove(&queued.envelope.recipient);
            }
        }
    }

    /// Runs the prune accepted messages to retained queue operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn prune_accepted_messages_to_retained_queue(&mut self) {
        let retained_ids = self
            .queue
            .iter()
            .map(|queued| queued.envelope.id.clone())
            .collect::<HashSet<_>>();
        self.accepted_messages
            .retain(|message_id, _| retained_ids.contains(message_id));
    }

    /// Runs the matching recipients operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn matching_recipients(&self, envelope: &Envelope) -> Vec<&SenderIdentity> {
        self.registered
            .values()
            .filter(|identity| {
                self.recipient_is_available(&identity.agent_id)
                    && recipient_matches(identity, &envelope.recipient)
            })
            .collect()
    }

    /// Runs the recipient is available operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn recipient_is_available(&self, agent_id: &AgentId) -> bool {
        self.presence
            .get(agent_id)
            .is_some_and(|presence| presence.status != AgentPresenceStatus::Offline)
    }

    /// Runs the last sequence operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn last_sequence(&self) -> MessageSequence {
        self.next_sequence.saturating_sub(1)
    }
}

/// Runs the recipient matches operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn recipient_matches(identity: &SenderIdentity, recipient: &Recipient) -> bool {
    match recipient {
        Recipient::Agent(agent_id) => &identity.agent_id == agent_id,
        Recipient::Pane(pane_id) => identity.pane_id.as_ref() == Some(pane_id),
        Recipient::Window(window_id) => identity.window_id.as_ref() == Some(window_id),
        Recipient::Session => true,
        Recipient::Role(role) => identity.role.as_ref() == Some(role),
        Recipient::Capability(capability) => {
            identity.capabilities.iter().any(|cap| cap == capability)
        }
        Recipient::Group(group) => {
            group == "session" || identity.capabilities.iter().any(|cap| cap == group)
        }
    }
}

fn recipient_selectors(identity: &SenderIdentity) -> Vec<Recipient> {
    let mut selectors = Vec::new();
    let mut seen = HashSet::new();
    push_recipient_selector(
        &mut selectors,
        &mut seen,
        Recipient::Agent(identity.agent_id.clone()),
    );
    push_recipient_selector(&mut selectors, &mut seen, Recipient::Session);
    push_recipient_selector(
        &mut selectors,
        &mut seen,
        Recipient::Group("session".to_string()),
    );
    if let Some(pane_id) = identity.pane_id.as_ref() {
        push_recipient_selector(&mut selectors, &mut seen, Recipient::Pane(pane_id.clone()));
    }
    if let Some(window_id) = identity.window_id.as_ref() {
        push_recipient_selector(
            &mut selectors,
            &mut seen,
            Recipient::Window(window_id.clone()),
        );
    }
    if let Some(role) = identity.role.as_ref() {
        push_recipient_selector(&mut selectors, &mut seen, Recipient::Role(role.clone()));
    }
    for capability in &identity.capabilities {
        push_recipient_selector(
            &mut selectors,
            &mut seen,
            Recipient::Capability(capability.clone()),
        );
        push_recipient_selector(
            &mut selectors,
            &mut seen,
            Recipient::Group(capability.clone()),
        );
    }
    selectors
}

fn push_recipient_selector(
    selectors: &mut Vec<Recipient>,
    seen: &mut HashSet<Recipient>,
    selector: Recipient,
) {
    if seen.insert(selector.clone()) {
        selectors.push(selector);
    }
}

/// Runs the expired operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn expired(queued: &QueuedEnvelope, now_ms: u64) -> bool {
    envelope_expired_at(&queued.envelope, queued.accepted_at_ms, now_ms)
}

/// Runs the envelope expired at operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn envelope_expired_at(envelope: &Envelope, accepted_at_ms: u64, now_ms: u64) -> bool {
    envelope
        .ttl_ms
        .is_some_and(|ttl| accepted_at_ms.saturating_add(ttl) < now_ms)
}

/// Returns true when an envelope cannot remain live long enough to be delivered.
fn expires_before_delivery(envelope: &Envelope) -> bool {
    envelope.ttl_ms == Some(0)
}

/// Runs the validate message service snapshot operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn validate_message_service_snapshot(snapshot: &MessageServiceSnapshot) -> Result<()> {
    if snapshot.protocol != MMP_PROTOCOL || snapshot.schema_version == 0 {
        return Err(MessageError::invalid_args(
            "snapshot MMP state has unsupported protocol or schema version",
        ));
    }
    if snapshot.next_sequence == 0
        || snapshot.retention_messages == 0
        || snapshot.retention_bytes == 0
    {
        return Err(MessageError::invalid_args(
            "snapshot MMP state sequence and retention values must be non-zero",
        ));
    }
    for identity in &snapshot.registered_agents {
        sender_identity_from_snapshot(identity)?;
    }
    for presence in &snapshot.presence {
        sender_identity_from_snapshot(&presence.identity)?;
        parse_presence_status(&presence.status)?;
    }
    for cursor in &snapshot.subscriptions {
        parse_opaque_id(&cursor.recipient, "MMP delivery cursor recipient")?;
    }
    let mut max_sequence = 0;
    let mut queued_bytes = 0usize;
    for retained in &snapshot.retained_messages {
        if retained.sequence == 0 {
            return Err(MessageError::invalid_args(
                "snapshot MMP retained message sequence must be non-zero",
            ));
        }
        max_sequence = max_sequence.max(retained.sequence);
        queued_bytes = queued_bytes.saturating_add(retained.envelope.payload.len());
        envelope_from_snapshot(&retained.envelope)?;
    }
    for accepted in &snapshot.accepted_messages {
        let envelope = envelope_from_snapshot(&accepted.envelope)?;
        parse_delivery_status(&accepted.delivery.status)?;
        if accepted.delivery.message_id != envelope.id {
            return Err(MessageError::invalid_args(
                "snapshot MMP accepted delivery id must match envelope id",
            ));
        }
        max_sequence = max_sequence.max(accepted.delivery.sequence);
    }
    if snapshot.next_sequence <= max_sequence {
        return Err(MessageError::invalid_args(
            "snapshot MMP next sequence must be greater than retained sequences",
        ));
    }
    if queued_bytes > snapshot.retention_bytes {
        return Err(MessageError::invalid_args(
            "snapshot MMP retained messages exceed retention bytes",
        ));
    }
    Ok(())
}

/// Runs the identity snapshot operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn identity_snapshot(identity: &SenderIdentity) -> MessageIdentitySnapshot {
    MessageIdentitySnapshot {
        agent_id: identity.agent_id.to_string(),
        pane_id: identity.pane_id.as_ref().map(ToString::to_string),
        window_id: identity.window_id.as_ref().map(ToString::to_string),
        role: identity.role.clone(),
        capabilities: identity.capabilities.clone(),
    }
}

/// Runs the sender identity from snapshot operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn sender_identity_from_snapshot(snapshot: &MessageIdentitySnapshot) -> Result<SenderIdentity> {
    if snapshot
        .capabilities
        .iter()
        .any(|capability| capability.is_empty())
        || snapshot.role.as_deref().is_some_and(str::is_empty)
    {
        return Err(MessageError::invalid_args(
            "snapshot MMP sender identity fields must not be empty",
        ));
    }
    Ok(SenderIdentity {
        agent_id: parse_opaque_id(&snapshot.agent_id, "MMP agent id")?,
        pane_id: snapshot
            .pane_id
            .as_deref()
            .map(|id| parse_opaque_id(id, "MMP pane id"))
            .transpose()?,
        window_id: snapshot
            .window_id
            .as_deref()
            .map(|id| parse_opaque_id(id, "MMP window id"))
            .transpose()?,
        role: snapshot.role.clone(),
        capabilities: snapshot.capabilities.clone(),
    })
}

/// Runs the presence snapshot operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn presence_snapshot(record: &PresenceRecord) -> MessagePresenceSnapshot {
    MessagePresenceSnapshot {
        identity: identity_snapshot(&record.identity),
        status: presence_status_name(record.status).to_string(),
        updated_at_ms: record.updated_at_ms,
    }
}

/// Runs the cursor snapshot operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cursor_snapshot(cursor: &DeliveryCursor) -> MessageDeliveryCursorSnapshot {
    MessageDeliveryCursorSnapshot {
        recipient: cursor.recipient.to_string(),
        last_sequence: cursor.last_sequence,
    }
}

/// Runs the queued envelope snapshot operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn queued_envelope_snapshot(queued: &QueuedEnvelope) -> MessageQueuedEnvelopeSnapshot {
    MessageQueuedEnvelopeSnapshot {
        sequence: queued.sequence,
        accepted_at_ms: queued.accepted_at_ms,
        envelope: envelope_snapshot(&queued.envelope),
    }
}

/// Runs the accepted message snapshot operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn accepted_message_snapshot(accepted: &AcceptedMessage) -> MessageAcceptedSnapshot {
    MessageAcceptedSnapshot {
        accepted_at_ms: accepted.accepted_at_ms,
        envelope: envelope_snapshot(&accepted.envelope),
        delivery: MessageDeliverySnapshot {
            accepted: accepted.delivery.accepted,
            message_id: accepted.delivery.message_id.clone(),
            sequence: accepted.delivery.sequence,
            queued_recipients: accepted.delivery.queued_recipients,
            status: delivery_status_name(accepted.delivery.status).to_string(),
        },
    }
}

/// Runs the envelope snapshot operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn envelope_snapshot(envelope: &Envelope) -> MessageEnvelopeSnapshot {
    MessageEnvelopeSnapshot {
        protocol: envelope.protocol.to_string(),
        id: envelope.id.clone(),
        message_type: envelope.message_type.clone(),
        time: envelope.time.clone(),
        sender: identity_snapshot(&envelope.sender),
        recipient: recipient_snapshot(&envelope.recipient),
        correlation_id: envelope.correlation_id.clone(),
        ttl_ms: envelope.ttl_ms,
        content_type: envelope.content_type.clone(),
        payload: envelope.payload.clone(),
        extension_fields: envelope
            .extension_fields
            .iter()
            .map(|(key, value)| MessageExtensionFieldSnapshot {
                key: key.clone(),
                value_json: value.clone(),
            })
            .collect(),
    }
}

/// Runs the envelope from snapshot operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn envelope_from_snapshot(snapshot: &MessageEnvelopeSnapshot) -> Result<Envelope> {
    validate_protocol(&snapshot.protocol)?;
    validate_message_type(&snapshot.message_type)?;
    if snapshot.id.is_empty()
        || snapshot.time.is_empty()
        || snapshot.content_type.is_empty()
        || snapshot.extension_fields.iter().any(|field| {
            field.key.is_empty()
                || serde_json::from_str::<serde_json::Value>(&field.value_json).is_err()
        })
    {
        return Err(MessageError::invalid_args(
            "snapshot MMP envelope fields must be valid and non-empty",
        ));
    }
    Ok(Envelope {
        protocol: MMP_PROTOCOL,
        id: snapshot.id.clone(),
        message_type: snapshot.message_type.clone(),
        time: snapshot.time.clone(),
        sender: sender_identity_from_snapshot(&snapshot.sender)?,
        recipient: recipient_from_snapshot(&snapshot.recipient)?,
        correlation_id: snapshot.correlation_id.clone(),
        ttl_ms: snapshot.ttl_ms,
        content_type: snapshot.content_type.clone(),
        payload: snapshot.payload.clone(),
        extension_fields: snapshot
            .extension_fields
            .iter()
            .map(|field| (field.key.clone(), field.value_json.clone()))
            .collect(),
    })
}

/// Runs the recipient snapshot operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn recipient_snapshot(recipient: &Recipient) -> MessageRecipientSnapshot {
    match recipient {
        Recipient::Agent(id) => MessageRecipientSnapshot {
            kind: "agent".to_string(),
            value: Some(id.to_string()),
        },
        Recipient::Pane(id) => MessageRecipientSnapshot {
            kind: "pane".to_string(),
            value: Some(id.to_string()),
        },
        Recipient::Window(id) => MessageRecipientSnapshot {
            kind: "window".to_string(),
            value: Some(id.to_string()),
        },
        Recipient::Session => MessageRecipientSnapshot {
            kind: "session".to_string(),
            value: None,
        },
        Recipient::Role(role) => MessageRecipientSnapshot {
            kind: "role".to_string(),
            value: Some(role.clone()),
        },
        Recipient::Capability(capability) => MessageRecipientSnapshot {
            kind: "capability".to_string(),
            value: Some(capability.clone()),
        },
        Recipient::Group(group) => MessageRecipientSnapshot {
            kind: "group".to_string(),
            value: Some(group.clone()),
        },
    }
}

/// Runs the recipient from snapshot operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn recipient_from_snapshot(snapshot: &MessageRecipientSnapshot) -> Result<Recipient> {
    match snapshot.kind.as_str() {
        "agent" => Ok(Recipient::Agent(parse_required_recipient_id(snapshot)?)),
        "pane" => Ok(Recipient::Pane(parse_required_recipient_id(snapshot)?)),
        "window" => Ok(Recipient::Window(parse_required_recipient_id(snapshot)?)),
        "session" if snapshot.value.is_none() => Ok(Recipient::Session),
        "role" => Ok(Recipient::Role(
            required_recipient_value(snapshot)?.to_string(),
        )),
        "capability" => Ok(Recipient::Capability(
            required_recipient_value(snapshot)?.to_string(),
        )),
        "group" => Ok(Recipient::Group(
            required_recipient_value(snapshot)?.to_string(),
        )),
        _ => Err(MessageError::invalid_args(
            "snapshot MMP recipient selector is invalid",
        )),
    }
}

/// Runs the parse required recipient id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_required_recipient_id(snapshot: &MessageRecipientSnapshot) -> Result<StableId> {
    parse_opaque_id(required_recipient_value(snapshot)?, "MMP recipient id")
}

/// Runs the required recipient value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn required_recipient_value(snapshot: &MessageRecipientSnapshot) -> Result<&str> {
    snapshot
        .value
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| MessageError::invalid_args("snapshot MMP recipient value must not be empty"))
}

/// Runs the parse opaque id operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_opaque_id(value: &str, field: &'static str) -> Result<StableId> {
    StableId::opaque(value).ok_or_else(|| {
        MessageError::invalid_args(format!(
            "snapshot {field} is empty or contains control characters"
        ))
    })
}

/// Runs the presence status name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn presence_status_name(status: AgentPresenceStatus) -> &'static str {
    match status {
        AgentPresenceStatus::Available => "available",
        AgentPresenceStatus::Busy => "busy",
        AgentPresenceStatus::Blocked => "blocked",
        AgentPresenceStatus::Offline => "offline",
    }
}

/// Runs the parse presence status operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_presence_status(value: &str) -> Result<AgentPresenceStatus> {
    match value {
        "available" => Ok(AgentPresenceStatus::Available),
        "busy" => Ok(AgentPresenceStatus::Busy),
        "blocked" => Ok(AgentPresenceStatus::Blocked),
        "offline" => Ok(AgentPresenceStatus::Offline),
        _ => Err(MessageError::invalid_args(
            "snapshot MMP presence status is invalid",
        )),
    }
}

/// Runs the delivery status name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn delivery_status_name(status: DeliveryStatus) -> &'static str {
    match status {
        DeliveryStatus::Accepted => "accepted",
        DeliveryStatus::Undeliverable => "undeliverable",
        DeliveryStatus::Expired => "expired",
    }
}

/// Runs the parse delivery status operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn parse_delivery_status(value: &str) -> Result<DeliveryStatus> {
    match value {
        "accepted" => Ok(DeliveryStatus::Accepted),
        "undeliverable" => Ok(DeliveryStatus::Undeliverable),
        "expired" => Ok(DeliveryStatus::Expired),
        _ => Err(MessageError::invalid_args(
            "snapshot MMP delivery status is invalid",
        )),
    }
}
