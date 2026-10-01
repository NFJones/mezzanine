//! Snapshot projection and validated restoration of the single message owner.
//!
//! Reconstruction retains resolved audiences, accepted-message identity and
//! sparse delivery floors; queue indexes remain projections of retained mail.

use super::snapshot_validation::validate_message_service_snapshot;
use super::*;

impl MessageService {
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
        let mut retired_delivery_floors = self
            .retired_delivery_floors
            .iter()
            .map(
                |(_, (identity, last_sequence))| MessageRetiredDeliveryFloorSnapshot {
                    identity: identity_snapshot(identity),
                    last_sequence: *last_sequence,
                },
            )
            .collect::<Vec<_>>();
        retired_delivery_floors
            .sort_by(|left, right| left.identity.agent_id.cmp(&right.identity.agent_id));
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
            schema_version: 3,
            next_sequence: self.next_sequence,
            retention_messages: self.retention_messages,
            retention_bytes: self.retention_bytes,
            registered_agents,
            presence,
            subscriptions,
            retired_delivery_floors,
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
            let identity = sender_identity_from_snapshot(identity, snapshot.schema_version)?;
            agent_ids.push(identity.agent_id.clone());
            registered.insert(identity.agent_id.clone(), identity);
        }
        let ids = IdFactory::after_existing_ids(agent_ids.iter());
        let mut presence = HashMap::new();
        for record in &snapshot.presence {
            let identity =
                sender_identity_from_snapshot(&record.identity, snapshot.schema_version)?;
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
        let mut retired_delivery_floors = HashMap::new();
        for floor in &snapshot.retired_delivery_floors {
            let identity = sender_identity_from_snapshot(&floor.identity, snapshot.schema_version)?;
            retired_delivery_floors
                .insert(identity.agent_id.clone(), (identity, floor.last_sequence));
        }
        let mut queue = VecDeque::new();
        let mut retained_envelopes = HashMap::new();
        let mut queued_bytes = 0usize;
        for retained in &snapshot.retained_messages {
            if snapshot.schema_version == 1 {
                continue;
            }
            let envelope = Arc::new(envelope_from_snapshot(
                &retained.envelope,
                snapshot.schema_version,
            )?);
            queued_bytes = queued_bytes.saturating_add(envelope.payload.len());
            retained_envelopes.insert(envelope.id.clone(), envelope.clone());
            queue.push_back(Arc::new(QueuedEnvelope {
                sequence: retained.sequence,
                envelope,
                audience: audience_from_snapshot(retained.audience.as_ref())?,
                accepted_at_ms: retained.accepted_at_ms,
            }));
        }
        let mut accepted_messages = HashMap::new();
        for accepted in &snapshot.accepted_messages {
            if snapshot.schema_version == 1 {
                continue;
            }
            let envelope = envelope_from_snapshot(&accepted.envelope, snapshot.schema_version)?;
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
                    audience: audience_from_snapshot(accepted.audience.as_ref())?,
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
            retired_delivery_floors,
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
        service.prune_retired_delivery_floors();
        Ok(service)
    }
}
