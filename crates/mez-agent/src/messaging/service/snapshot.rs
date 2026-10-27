//! Snapshot projection and validated restoration of the single message owner.
//!
//! Reconstruction retains resolved audiences, accepted-message identity and
//! sparse delivery floors; queue indexes remain projections of retained mail.
//! Schema4 records exact fresh raw sender objective versions without rewriting
//! envelopes or receipts. Legacy reusable floor semantics remain independent.
//! Schema5 freezes exact recipient registration intervals as well. Restoration
//! reserves all retained recipient IDs independently of sender floors, including
//! selector-based fanout targets. Legacy retained mail with unknown recipients
//! is refused explicitly; snapshot capture is not acceptance-atomic durability.
//! Schema6 preserves selected-recipient offers for cumulative ACK validation.
//! Schema5 restores proven bindings without inventing unknown offers; a fresh
//! sequenced selection is required before advancing an uncommitted cursor.
//! Schema7 stores each replay/custody source once and preserves exact recipient
//! commits independently of retired cursors. Legacy mail supplies only provable
//! retained custody; unknown evicted source is never reconstructed.

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
            .values()
            .map(|floor| MessageRetiredDeliveryFloorSnapshot {
                identity: identity_snapshot(&floor.identity),
                last_sequence: floor.last_sequence,
                fresh_sender_versions: floor
                    .fresh_sender_versions
                    .as_ref()
                    .map(|versions| versions.iter().map(identity_snapshot).collect()),
            })
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
            schema_version: 7,
            next_sequence: self.next_sequence,
            retention_messages: self.retention_messages,
            retention_bytes: self.retention_bytes,
            registered_agents,
            registration_floors: Some({
                let mut floors = self
                    .registration_floors
                    .iter()
                    .map(|(id, floor)| MessageRecipientBindingSnapshot {
                        recipient: id.to_string(),
                        registered_after_sequence: *floor,
                    })
                    .collect::<Vec<_>>();
                floors.sort_by(|a, b| a.recipient.cmp(&b.recipient));
                floors
            }),
            presence,
            subscriptions,
            retired_delivery_floors,
            retained_messages,
            custody_messages: Some({
                let replay = self
                    .queue
                    .iter()
                    .map(|row| row.sequence)
                    .collect::<HashSet<_>>();
                self.pending
                    .values()
                    .filter(|row| !replay.contains(&row.sequence))
                    .map(|row| queued_envelope_snapshot(row))
                    .collect()
            }),
            accepted_messages,
        }
    }

    /// Rebuilds durable local message protocol state from a validated snapshot.
    pub fn from_snapshot_state(snapshot: &MessageServiceSnapshot) -> Result<Self> {
        validate_message_service_snapshot(snapshot)?;
        let registration_floors = snapshot
            .registration_floors
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|binding| {
                Ok((
                    parse_opaque_id(&binding.recipient, "MMP recipient registration")?,
                    binding.registered_after_sequence,
                ))
            })
            .collect::<Result<HashMap<_, _>>>()?;
        let mut registered = HashMap::new();
        for identity in &snapshot.registered_agents {
            let identity = sender_identity_from_snapshot(identity, snapshot.schema_version)?;
            registered.insert(identity.agent_id.clone(), identity);
        }
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
            let fresh_sender_versions = floor
                .fresh_sender_versions
                .as_ref()
                .map(|versions| {
                    versions
                        .iter()
                        .map(|version| {
                            sender_identity_from_snapshot(version, snapshot.schema_version)
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?;
            retired_delivery_floors.insert(
                identity.agent_id.clone(),
                RetiredDeliveryFloor {
                    identity,
                    last_sequence: floor.last_sequence,
                    fresh_sender_versions,
                },
            );
        }
        let mut queue = VecDeque::new();
        let mut pending = std::collections::BTreeMap::new();
        let mut retained_envelopes = HashMap::new();
        let mut queued_bytes = 0usize;
        let replay_sequences = snapshot
            .retained_messages
            .iter()
            .map(|row| row.sequence)
            .collect::<HashSet<_>>();
        for retained in snapshot.canonical_messages() {
            if snapshot.schema_version == 1 {
                continue;
            }
            let envelope = Arc::new(envelope_from_snapshot(
                &retained.envelope,
                snapshot.schema_version,
            )?);
            if replay_sequences.contains(&retained.sequence) {
                queued_bytes = queued_bytes.saturating_add(envelope.payload.len());
            }
            retained_envelopes.insert(envelope.id.clone(), envelope.clone());
            let acknowledged = retained
                .recipient_bindings
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|binding| {
                    if snapshot.schema_version >= 7 {
                        retained
                            .acknowledged_recipients
                            .as_deref()
                            .unwrap_or(&[])
                            .contains(&binding.recipient)
                    } else {
                        // Legacy offers plus a still-exact subscription prove an ACK;
                        // absent/retired cursor evidence remains conservatively pending.
                        registration_floors.iter().any(|(id, floor)| {
                            id.as_str() == binding.recipient
                                && *floor == binding.registered_after_sequence
                        }) && subscriptions.iter().any(|(id, cursor)| {
                            id.as_str() == binding.recipient
                                && cursor.last_sequence >= retained.sequence
                        }) && retained
                            .offered_recipients
                            .as_deref()
                            .unwrap_or(&[])
                            .iter()
                            .any(|offer| offer.recipient == binding.recipient)
                    }
                })
                .collect::<Vec<_>>();
            let is_pending = acknowledged.iter().any(|ack| !ack);
            let queued = Arc::new(QueuedEnvelope {
                sequence: retained.sequence,
                envelope,
                audience: audience_from_snapshot(retained.audience.as_ref())?,
                recipient_bindings: retained
                    .recipient_bindings
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .map(|binding| {
                        Ok((
                            parse_opaque_id(&binding.recipient, "MMP accepted recipient")?,
                            binding.registered_after_sequence,
                        ))
                    })
                    .collect::<Result<Vec<_>>>()?,
                accepted_at_ms: retained.accepted_at_ms,
                offered_at_ms: std::sync::Mutex::new(super::offers::offers_from_snapshot(retained)),
                acknowledged: std::sync::Mutex::new(acknowledged),
                ack_expiry_observed_at_ms: std::sync::Mutex::new(
                    retained.ack_expiry_observed_at_ms,
                ),
            });
            if is_pending {
                pending.insert(queued.sequence, queued.clone());
            }
            if replay_sequences.contains(&queued.sequence) {
                queue.push_back(queued);
            }
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

        // Retained senders and direct recipients still own canonical evidence.
        // Recipient-only raw identities deliberately leave no authored floor;
        // reserve their IDs from bounded mail, without reviving registrations
        // or keeping independent tombstones after that mail has evicted.
        let recipient_ids = queue
            .iter()
            .chain(pending.values())
            .filter_map(|queued| match &queued.envelope.recipient {
                Recipient::Agent(agent_id) => Some(agent_id),
                _ => None,
            });
        let bound_recipient_ids = queue
            .iter()
            .chain(pending.values())
            .flat_map(|queued| queued.recipient_bindings.iter().map(|(id, _)| id));
        let ids = IdFactory::after_existing_ids(
            registered
                .keys()
                .chain(retired_delivery_floors.keys())
                .chain(bound_recipient_ids)
                .chain(recipient_ids),
        );
        let mut service = Self {
            ids,
            registered,
            registration_floors: if snapshot.schema_version >= 5 {
                registration_floors
            } else {
                snapshot
                    .registered_agents
                    .iter()
                    .map(|identity| {
                        Ok((
                            parse_opaque_id(&identity.agent_id, "MMP registration")?,
                            snapshot.next_sequence - 1,
                        ))
                    })
                    .collect::<Result<HashMap<_, _>>>()?
            },
            presence,
            subscriptions,
            retired_delivery_floors,
            accepted_messages,
            queue,
            pending,
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
        if snapshot.schema_version < 7 {
            // Legacy snapshots cannot bypass new custody bounds. Reconstruct
            // only provable retained source; pre-upgrade evicted mail is unknown.
            super::pending::validate_pending_snapshot(&service.snapshot_state())?;
        }
        Ok(service)
    }
}
