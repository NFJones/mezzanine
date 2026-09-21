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
    MessageAcceptedSnapshot, MessageAudienceSnapshot, MessageDeliveryCursorSnapshot,
    MessageDeliverySnapshot, MessageEnvelopeSnapshot, MessageExtensionFieldSnapshot,
    MessageFanoutDiagnostics, MessageIdentitySnapshot, MessagePresenceSnapshot,
    MessageQueuedEnvelopeSnapshot, MessageRecipientSnapshot, MessageRetiredDeliveryFloorSnapshot,
    MessageScope, MessageSequence, MessageService, MessageServiceSnapshot, PresenceRecord,
    ProjectScopeId, QueuedEnvelope, Recipient, ResolvedMessageAudience, SenderIdentity,
    SequencedEnvelope,
};
use super::validation::{
    normalize_objective, normalize_optional_objective, validate_message_type, validate_protocol,
    validate_sender_identity,
};

#[derive(Debug)]
struct IndexedReceiveSelection {
    batch: DeliveryBatch,
    sequence_lookups: u64,
    payload_bytes: usize,
}

/// Reconciles one immutable optional identity field without allowing a
/// populated registered value to be overwritten by a conflicting refresh.
fn reconcile_identity_field<T: Clone + PartialEq>(
    existing: &mut Option<T>,
    incoming: &Option<T>,
    conflict_message: &str,
) -> Result<()> {
    match (&*existing, incoming) {
        (None, Some(incoming)) => *existing = Some(incoming.clone()),
        (Some(current), Some(incoming)) if current != incoming => {
            return Err(MessageError::conflict(conflict_message));
        }
        _ => {}
    }
    Ok(())
}

/// Reconciles immutable capability metadata as one vector rather than merging
/// partially overlapping capability sets.
fn reconcile_capabilities(existing: &mut Vec<String>, incoming: &[String]) -> Result<()> {
    if existing.is_empty() {
        if !incoming.is_empty() {
            *existing = incoming.to_vec();
        }
    } else if !incoming.is_empty() && existing.as_slice() != incoming {
        return Err(MessageError::conflict(
            "MMP sender capabilities cannot change after registration",
        ));
    }
    Ok(())
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
            retired_delivery_floors: HashMap::new(),
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
            project_scope: None,
            pane_id,
            window_id,
            role: Some(role.into()),
            capabilities,
            objective: None,
        };
        self.insert_registered_identity(identity, 0)
    }

    /// Registers one agent identity carrying an initial bounded objective.
    ///
    /// The objective is optional and additive to mmp/1. An objective that is
    /// empty, unbounded, or control-character bearing is rejected instead of
    /// being published to discovery.
    pub fn register_agent_with_objective(
        &mut self,
        pane_id: Option<PaneId>,
        window_id: Option<WindowId>,
        role: impl Into<String>,
        capabilities: Vec<String>,
        objective: Option<&str>,
    ) -> Result<SenderIdentity> {
        let identity = SenderIdentity {
            agent_id: self.ids.agent(),
            project_scope: None,
            pane_id,
            window_id,
            role: Some(role.into()),
            capabilities,
            objective: normalize_optional_objective(objective)?,
        };
        validate_sender_identity(&identity)?;
        Ok(self.insert_registered_identity(identity, 0))
    }

    /// Inserts one identity into registered-agent and presence state.
    fn insert_registered_identity(
        &mut self,
        identity: SenderIdentity,
        updated_at_ms: u64,
    ) -> SenderIdentity {
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
            let mut reconciled = existing.clone();
            reconcile_identity_field(
                &mut reconciled.project_scope,
                &identity.project_scope,
                "MMP sender project scope cannot change after registration",
            )?;
            reconcile_identity_field(
                &mut reconciled.pane_id,
                &identity.pane_id,
                "MMP sender pane id cannot change after registration",
            )?;
            reconcile_identity_field(
                &mut reconciled.window_id,
                &identity.window_id,
                "MMP sender window id cannot change after registration",
            )?;
            reconcile_identity_field(
                &mut reconciled.role,
                &identity.role,
                "MMP sender role cannot change after registration",
            )?;
            reconcile_capabilities(&mut reconciled.capabilities, &identity.capabilities)?;

            self.registered
                .insert(identity.agent_id.clone(), reconciled.clone());
            if let Some(presence) = self.presence.get_mut(&identity.agent_id) {
                presence.identity = reconciled.clone();
            }
            return Ok(reconciled);
        }
        Ok(self.insert_registered_identity(identity, updated_at_ms))
    }

    /// Replaces an already registered agent's project membership for a
    /// runtime-trusted conversation lifecycle transition.
    ///
    /// Ordinary registration intentionally rejects scope drift. This narrow
    /// operation updates only identity and presence metadata, preserving the
    /// discovery objective, presence status and timestamp, subscriptions,
    /// delivery cursor, retained envelopes, and queued messages.
    pub fn rebind_agent_project_scope(
        &mut self,
        agent_id: &AgentId,
        project_scope: Option<ProjectScopeId>,
    ) -> Result<SenderIdentity> {
        let identity = self.registered.get_mut(agent_id).ok_or_else(|| {
            MessageError::not_found("project-scope rebind requires a registered agent")
        })?;
        identity.project_scope = project_scope.clone();
        if let Some(presence) = self.presence.get_mut(agent_id) {
            presence.identity.project_scope = project_scope;
        }
        Ok(identity.clone())
    }

    /// Publishes one bounded agent objective with identical-value throttling.
    ///
    /// Returns `Ok(true)` only when the published value actually changed. A
    /// refresh that carries no objective (`None`) is a no-op: it publishes
    /// nothing and leaves the previous objective and presence timestamp in
    /// place, so an objective-less or failed refresh never clears discovery
    /// text and never churns discovery rows or resume views. An unchanged value
    /// likewise publishes nothing. Callers keep the previous objective on
    /// error, so a failed refresh never fails a turn.
    pub fn update_agent_objective(
        &mut self,
        agent_id: &AgentId,
        objective: Option<&str>,
        updated_at_ms: u64,
    ) -> Result<bool> {
        let Some(objective) = normalize_optional_objective(objective)? else {
            return Ok(false);
        };
        let current = self
            .registered
            .get(agent_id)
            .ok_or_else(|| {
                MessageError::not_found("agent objective update requires a registered agent")
            })?
            .objective
            .clone();
        if current.as_deref() == Some(objective.as_str()) {
            return Ok(false);
        }
        if let Some(identity) = self.registered.get_mut(agent_id) {
            identity.objective = Some(objective.clone());
        }
        if let Some(record) = self.presence.get_mut(agent_id) {
            record.identity.objective = Some(objective);
            record.updated_at_ms = updated_at_ms;
        }
        Ok(true)
    }

    /// Explicitly clears one registered agent's published objective.
    ///
    /// Unlike [`Self::update_agent_objective`], this is an intentional state
    /// transition. Protocol `null` and an omitted objective remain no-ops so a
    /// malformed or objective-less refresh can never accidentally clear peer
    /// discovery state.
    pub fn clear_agent_objective(
        &mut self,
        agent_id: &AgentId,
        updated_at_ms: u64,
    ) -> Result<bool> {
        let current = self
            .registered
            .get(agent_id)
            .ok_or_else(|| {
                MessageError::not_found("agent objective clear requires a registered agent")
            })?
            .objective
            .clone();
        if current.is_none() {
            return Ok(false);
        }
        if let Some(identity) = self.registered.get_mut(agent_id) {
            identity.objective = None;
        }
        if let Some(record) = self.presence.get_mut(agent_id) {
            record.identity.objective = None;
            record.updated_at_ms = updated_at_ms;
        }
        Ok(true)
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
        self.accept_at_with_scope(connection_agent, envelope, MessageScope::Project, now_ms)
    }

    /// Accepts one authenticated envelope after resolving its requested audience.
    pub fn accept_at_with_scope(
        &mut self,
        connection_agent: &AgentId,
        envelope: Envelope,
        scope: MessageScope,
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
        let audience = resolve_message_audience(registered, scope)?;
        if let Some(accepted) = self.accepted_messages.get_mut(&envelope.id) {
            if accepted.envelope.as_ref() == &envelope && accepted.audience == audience {
                if envelope_expired_at(&accepted.envelope, accepted.accepted_at_ms, now_ms) {
                    accepted.delivery.status = DeliveryStatus::Expired;
                }
                return Ok(accepted.delivery.clone());
            }
            return Err(MessageError::conflict(MMP_DUPLICATE_MESSAGE_ID_MESSAGE));
        }

        let queued_recipients = self.matching_recipients(&envelope, &audience).len();
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
        let sequence = self.enqueue(accepted_envelope.clone(), audience.clone(), now_ms)?;

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
                audience,
                delivery: delivery.clone(),
                accepted_at_ms: now_ms,
            },
        );
        self.prune_accepted_messages_to_retained_queue();
        Ok(delivery)
    }

    /// Runs administrative session-wide agent discovery for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn discover_agents(&self) -> Vec<SenderIdentity> {
        self.discover_agents_filtered_session_wide(None, None, None, None, None, &[])
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

    /// Discovers agents visible to one authenticated requester.
    ///
    /// Project scope includes the requester and peers with the same trusted
    /// project membership. Session scope deliberately widens that view.
    #[allow(clippy::too_many_arguments)]
    pub fn discover_agents_filtered_for_requester(
        &self,
        requester: &AgentId,
        scope: MessageScope,
        agent_id: Option<&str>,
        pane_id: Option<&str>,
        window_id: Option<&str>,
        role: Option<&str>,
        status: Option<AgentPresenceStatus>,
        capabilities: &[String],
    ) -> Vec<SenderIdentity> {
        let requester_scope = self
            .registered
            .get(requester)
            .and_then(|identity| identity.project_scope.as_ref());
        self.discover_agents_filtered_session_wide(
            agent_id,
            pane_id,
            window_id,
            role,
            status,
            capabilities,
        )
        .into_iter()
        .filter(|identity| {
            identity.agent_id == *requester
                || matches!(scope, MessageScope::Session)
                || requester_scope
                    .is_some_and(|scope| identity.project_scope.as_ref() == Some(scope))
        })
        .collect()
    }

    /// Performs explicitly administrative session-wide discovery.
    pub fn discover_agents_filtered_session_wide(
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

    /// Retires one agent identity and its delivery subscription.
    ///
    /// Retained envelopes matching the retired identity are discarded. A
    /// future pane owner can reuse the same opaque id, while unrelated
    /// outbound task results retain their sender provenance for durable
    /// snapshot recovery.
    pub fn retire_agent_identity(&mut self, agent_id: &AgentId) -> bool {
        let identity = self.registered.get(agent_id).cloned();
        let removed = self.registered.remove(agent_id).is_some();
        self.presence.remove(agent_id);
        self.subscriptions.remove(agent_id);
        self.subscription_order.remove(agent_id.as_str());
        if self.fanout_after_recipient.as_deref() == Some(agent_id.as_str()) {
            self.fanout_after_recipient = None;
        }
        if let Some(identity) = identity.filter(|_| removed) {
            self.retired_delivery_floors
                .insert(agent_id.clone(), (identity.clone(), self.last_sequence()));
            self.queue
                .retain(|queued| !recipient_matches(&identity, &queued.envelope.recipient));
            self.queued_bytes = self
                .queue
                .iter()
                .map(|queued| queued.envelope.payload.len())
                .sum();
            self.rebuild_queue_indexes();
            self.prune_accepted_messages_to_retained_queue();
            self.prune_retired_delivery_floors();
        }
        removed
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

    /// Returns the sequence that will be assigned to the next accepted message.
    ///
    /// Runtime-authored envelopes use this durable, monotonically increasing
    /// value as an occurrence identity. Because snapshots preserve the sequence,
    /// generated message ids cannot collide with accepted pre-restart traffic.
    pub fn next_message_sequence(&self) -> MessageSequence {
        self.next_sequence
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

    /// Returns retained deliveries accepted at or before one recipient cursor.
    ///
    /// Snapshot recovery uses this bounded historical projection to reconcile
    /// a durable delivery acknowledgement with a separately persisted receiver
    /// presentation. The normal live delivery API intentionally returns only
    /// messages after its cursor; this method is therefore restricted to the
    /// already-retained interval and preserves ordinary audience and presence
    /// filtering without advancing any cursor.
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

    /// Returns retained accepted deliveries through one acknowledged cursor for recovery.
    ///
    /// Unlike live receive, this historical projection deliberately ignores current
    /// presence and TTL. Snapshot recovery needs the acceptance-time audience and
    /// recipient resolution that produced an already acknowledged context event,
    /// even when the recipient is currently offline or the envelope has expired.
    /// Retained queue order and sequence deduplication keep selector overlap from
    /// creating duplicate receiver reconstruction candidates.
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
                        && audience_matches(identity, &queued.audience)
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
    fn enqueue(
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

    fn prune_retired_delivery_floors(&mut self) {
        let Some(oldest_retained_sequence) = self.queue.front().map(|queued| queued.sequence)
        else {
            self.retired_delivery_floors.clear();
            return;
        };
        self.retired_delivery_floors
            .retain(|_, (_, floor)| *floor >= oldest_retained_sequence);
    }

    /// Runs the matching recipients operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn matching_recipients(
        &self,
        envelope: &Envelope,
        audience: &ResolvedMessageAudience,
    ) -> Vec<&SenderIdentity> {
        self.registered
            .values()
            .filter(|identity| {
                self.recipient_is_available(&identity.agent_id)
                    && recipient_matches(identity, &envelope.recipient)
                    && audience_matches(identity, audience)
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

/// Resolves a caller-selected scope into the immutable audience recorded for
/// one accepted message.
fn resolve_message_audience(
    sender: &SenderIdentity,
    scope: MessageScope,
) -> Result<ResolvedMessageAudience> {
    match scope {
        MessageScope::Project => sender
            .project_scope
            .clone()
            .map(ResolvedMessageAudience::Project)
            .ok_or_else(|| MessageError::not_found(MMP_UNDELIVERABLE_MESSAGE)),
        MessageScope::Session => Ok(ResolvedMessageAudience::Session),
    }
}

/// Returns whether an available recipient belongs to one accepted audience.
fn audience_matches(identity: &SenderIdentity, audience: &ResolvedMessageAudience) -> bool {
    match audience {
        ResolvedMessageAudience::Project(scope) => identity.project_scope.as_ref() == Some(scope),
        ResolvedMessageAudience::Session => true,
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
    if snapshot.protocol != MMP_PROTOCOL || !matches!(snapshot.schema_version, 1 | 2 | 3) {
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
    let mut registered_by_id = HashMap::new();
    for identity in &snapshot.registered_agents {
        sender_identity_from_snapshot(identity, snapshot.schema_version)?;
        if registered_by_id
            .insert(identity.agent_id.as_str(), identity)
            .is_some()
        {
            return Err(MessageError::invalid_args(
                "snapshot MMP registered agent ids must be unique",
            ));
        }
    }
    let mut presence_ids = HashSet::new();
    for presence in &snapshot.presence {
        sender_identity_from_snapshot(&presence.identity, snapshot.schema_version)?;
        parse_presence_status(&presence.status)?;
        if !presence_ids.insert(presence.identity.agent_id.as_str()) {
            return Err(MessageError::invalid_args(
                "snapshot MMP presence agent ids must be unique",
            ));
        }
        if registered_by_id.get(presence.identity.agent_id.as_str()) != Some(&&presence.identity) {
            return Err(MessageError::invalid_args(
                "snapshot MMP presence identity must match a registered agent",
            ));
        }
    }
    if snapshot.schema_version >= 2 && presence_ids.len() != registered_by_id.len() {
        return Err(MessageError::invalid_args(
            "snapshot MMP registered agents must each have a presence record",
        ));
    }
    let mut subscription_ids = HashSet::new();
    for cursor in &snapshot.subscriptions {
        let recipient = parse_opaque_id(&cursor.recipient, "MMP delivery cursor recipient")?;
        if !subscription_ids.insert(cursor.recipient.as_str()) {
            return Err(MessageError::invalid_args(
                "snapshot MMP delivery cursor recipients must be unique",
            ));
        }
        if !registered_by_id.contains_key(recipient.as_str()) {
            return Err(MessageError::invalid_args(
                "snapshot MMP delivery cursor recipient must be registered",
            ));
        }
        if cursor.last_sequence >= snapshot.next_sequence {
            return Err(MessageError::invalid_args(
                "snapshot MMP delivery cursor exceeds the retained sequence range",
            ));
        }
    }
    let mut retired_identities = HashMap::new();
    if snapshot.schema_version == 3 {
        for floor in &snapshot.retired_delivery_floors {
            let identity = sender_identity_from_snapshot(&floor.identity, snapshot.schema_version)?;
            if floor.last_sequence >= snapshot.next_sequence {
                return Err(MessageError::invalid_args(
                    "snapshot MMP retired delivery floor exceeds the retained sequence range",
                ));
            }
            if registered_by_id.contains_key(identity.agent_id.as_str())
                || subscription_ids.contains(identity.agent_id.as_str())
            {
                return Err(MessageError::invalid_args(
                    "snapshot MMP retired delivery floor identity must not be active",
                ));
            }
            if retired_identities
                .insert(identity.agent_id.clone(), identity)
                .is_some()
            {
                return Err(MessageError::invalid_args(
                    "snapshot MMP retired delivery floor identities must be unique",
                ));
            }
        }
    } else if !snapshot.retired_delivery_floors.is_empty() {
        return Err(MessageError::invalid_args(
            "snapshot MMP retired delivery floors require schema version 3",
        ));
    }
    let mut max_sequence = 0;
    let mut queued_bytes = 0usize;
    let mut retained_by_id = HashMap::new();
    if snapshot.schema_version >= 2 {
        let mut retained_sequences = HashSet::new();
        for retained in &snapshot.retained_messages {
            if retained.sequence == 0 {
                return Err(MessageError::invalid_args(
                    "snapshot MMP retained message sequence must be non-zero",
                ));
            }
            if !retained_sequences.insert(retained.sequence) {
                return Err(MessageError::invalid_args(
                    "snapshot MMP retained message sequences must be unique",
                ));
            }
            max_sequence = max_sequence.max(retained.sequence);
            queued_bytes = queued_bytes.saturating_add(retained.envelope.payload.len());
            let envelope = envelope_from_snapshot(&retained.envelope, snapshot.schema_version)?;
            if registered_by_id.get(envelope.sender.agent_id.as_str())
                != Some(&&retained.envelope.sender)
                && retired_identities.get(&envelope.sender.agent_id) != Some(&envelope.sender)
            {
                return Err(MessageError::invalid_args(
                    "snapshot MMP envelope sender must match a registered agent",
                ));
            }
            let audience = audience_from_snapshot(retained.audience.as_ref())?;
            validate_snapshot_audience_provenance(&audience, &envelope.sender)?;
            if retained_by_id
                .insert(
                    envelope.id.clone(),
                    (
                        envelope,
                        audience,
                        retained.sequence,
                        retained.accepted_at_ms,
                    ),
                )
                .is_some()
            {
                return Err(MessageError::invalid_args(
                    "snapshot MMP retained message ids must be unique",
                ));
            }
        }
        let mut accepted_by_id = HashMap::new();
        for accepted in &snapshot.accepted_messages {
            let envelope = envelope_from_snapshot(&accepted.envelope, snapshot.schema_version)?;
            if registered_by_id.get(envelope.sender.agent_id.as_str())
                != Some(&&accepted.envelope.sender)
                && retired_identities.get(&envelope.sender.agent_id) != Some(&envelope.sender)
            {
                return Err(MessageError::invalid_args(
                    "snapshot MMP envelope sender must match a registered agent",
                ));
            }
            let audience = audience_from_snapshot(accepted.audience.as_ref())?;
            validate_snapshot_audience_provenance(&audience, &envelope.sender)?;
            if accepted_by_id
                .insert(envelope.id.clone(), (envelope.clone(), audience.clone()))
                .is_some()
            {
                return Err(MessageError::invalid_args(
                    "snapshot MMP accepted message ids must be unique",
                ));
            }
            let Some((retained_envelope, retained_audience, retained_sequence, retained_at_ms)) =
                retained_by_id.get(&envelope.id)
            else {
                return Err(MessageError::invalid_args(
                    "snapshot MMP accepted message must correspond to a retained message",
                ));
            };
            if retained_envelope != &envelope
                || retained_audience != &audience
                || accepted.delivery.sequence != *retained_sequence
                || accepted.accepted_at_ms != *retained_at_ms
            {
                return Err(MessageError::invalid_args(
                    "snapshot MMP retained and accepted message records disagree",
                ));
            }
            parse_delivery_status(&accepted.delivery.status)?;
            if accepted.delivery.message_id != envelope.id {
                return Err(MessageError::invalid_args(
                    "snapshot MMP accepted delivery id must match envelope id",
                ));
            }
            max_sequence = max_sequence.max(accepted.delivery.sequence);
        }
        if accepted_by_id.len() != retained_by_id.len() {
            return Err(MessageError::invalid_args(
                "snapshot MMP retained messages must each have an accepted record",
            ));
        }
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
        objective: identity.objective.clone(),
        project_scope: identity
            .project_scope
            .as_ref()
            .map(|scope| scope.snapshot_value().to_string()),
    }
}

/// Runs the sender identity from snapshot operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn sender_identity_from_snapshot(
    snapshot: &MessageIdentitySnapshot,
    schema_version: u32,
) -> Result<SenderIdentity> {
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
    if let Some(objective) = snapshot.objective.as_deref() {
        normalize_objective(objective)?;
    }
    Ok(SenderIdentity {
        agent_id: parse_opaque_id(&snapshot.agent_id, "MMP agent id")?,
        project_scope: if schema_version == 1 {
            None
        } else {
            match snapshot.project_scope.as_deref() {
                None => None,
                Some(value) => {
                    Some(ProjectScopeId::from_snapshot_value(value).ok_or_else(|| {
                        MessageError::invalid_args("snapshot MMP project scope is invalid")
                    })?)
                }
            }
        },
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
        objective: snapshot.objective.clone(),
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
        audience: Some(audience_snapshot(&queued.audience)),
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
        audience: Some(audience_snapshot(&accepted.audience)),
    }
}

/// Converts a resolved in-memory audience into private snapshot metadata.
fn audience_snapshot(audience: &ResolvedMessageAudience) -> MessageAudienceSnapshot {
    match audience {
        ResolvedMessageAudience::Project(scope) => MessageAudienceSnapshot {
            kind: "project".to_string(),
            project_scope: Some(scope.snapshot_value().to_string()),
        },
        ResolvedMessageAudience::Session => MessageAudienceSnapshot {
            kind: "session".to_string(),
            project_scope: None,
        },
    }
}

/// Restores a required resolved audience from private schema-v2 metadata.
fn audience_from_snapshot(
    snapshot: Option<&MessageAudienceSnapshot>,
) -> Result<ResolvedMessageAudience> {
    let snapshot = snapshot
        .ok_or_else(|| MessageError::invalid_args("snapshot MMP message audience is missing"))?;
    match (snapshot.kind.as_str(), snapshot.project_scope.as_deref()) {
        ("session", None) => Ok(ResolvedMessageAudience::Session),
        ("project", Some(scope)) => ProjectScopeId::from_snapshot_value(scope)
            .map(ResolvedMessageAudience::Project)
            .ok_or_else(|| MessageError::invalid_args("snapshot MMP project scope is invalid")),
        _ => Err(MessageError::invalid_args(
            "snapshot MMP message audience is invalid",
        )),
    }
}

/// Validates that a restored project audience remains bound to its sender's
/// trusted registration instead of retargeting delivery across projects.
fn validate_snapshot_audience_provenance(
    audience: &ResolvedMessageAudience,
    sender: &SenderIdentity,
) -> Result<()> {
    if let ResolvedMessageAudience::Project(scope) = audience
        && sender.project_scope.as_ref() != Some(scope)
    {
        return Err(MessageError::invalid_args(
            "snapshot MMP project audience must match the sender project scope",
        ));
    }
    Ok(())
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
fn envelope_from_snapshot(
    snapshot: &MessageEnvelopeSnapshot,
    schema_version: u32,
) -> Result<Envelope> {
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
        sender: sender_identity_from_snapshot(&snapshot.sender, schema_version)?,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Registering with an objective normalizes it and publishes it through the
    /// same registry that discovery reads.
    #[test]
    fn registered_objective_is_published_normalized_to_discovery() {
        let mut service = MessageService::default();
        let identity = service
            .register_agent_with_objective(
                None,
                None,
                "agent",
                vec!["agent-harness".to_string()],
                Some("  Review   the discovery contract "),
            )
            .unwrap();
        assert_eq!(
            identity.objective.as_deref(),
            Some("Review the discovery contract")
        );
        let discovered =
            service.discover_agents_filtered_session_wide(None, None, None, None, None, &[]);
        assert_eq!(discovered.len(), 1);
        assert_eq!(
            discovered[0].objective.as_deref(),
            Some("Review the discovery contract")
        );
        assert!(
            service
                .register_agent_with_objective(None, None, "agent", Vec::new(), Some("  "))
                .is_err()
        );
    }

    /// Retiring a runtime-owned child removes every MMP surface that could
    /// otherwise leave a rolled-back or closed child reusable.
    #[test]
    fn retired_agent_identity_is_not_discoverable_or_subscribed() {
        let mut service = MessageService::default();
        let identity = service
            .register_agent_with_objective(
                None,
                None,
                "worker",
                vec!["subagent".to_string()],
                Some("Handle peer work"),
            )
            .unwrap();
        service
            .subscribe_from_retained_start(&identity.agent_id)
            .unwrap();

        assert!(service.retire_agent_identity(&identity.agent_id));
        assert!(service.registered_identity(&identity.agent_id).is_none());
        assert!(service.subscription(&identity.agent_id).is_none());
        assert!(service.presence().is_empty());
        assert!(
            service
                .discover_agents_filtered_session_wide(None, None, None, None, None, &[])
                .is_empty()
        );
        assert!(!service.retire_agent_identity(&identity.agent_id));
    }

    /// A retired runtime identity must not replay retained session-scoped mail
    /// to a later owner of the same pane-derived opaque id, even after the
    /// message service is restored from a durable snapshot.
    #[test]
    fn retired_identity_delivery_floor_survives_snapshot_restore() {
        let mut service = MessageService::default();
        let sender = service.register_agent(None, None, "agent", Vec::new());
        let sender_agent_id = sender.agent_id.clone();
        let retired = SenderIdentity {
            agent_id: AgentId::opaque("agent-%1").unwrap(),
            project_scope: None,
            pane_id: Some(PaneId::parse('%', "%1").unwrap()),
            window_id: Some(WindowId::parse('@', "@1").unwrap()),
            role: Some("agent".to_string()),
            capabilities: Vec::new(),
            objective: None,
        };
        service.ensure_agent_identity(retired.clone(), 0).unwrap();
        service
            .accept_at_with_scope(
                &sender.agent_id,
                Envelope {
                    protocol: MMP_PROTOCOL,
                    id: "retired-session-message".to_string(),
                    message_type: "send".to_string(),
                    time: "runtime:0".to_string(),
                    sender: sender.clone(),
                    recipient: Recipient::Session,
                    correlation_id: None,
                    ttl_ms: None,
                    content_type: "text/plain; charset=utf-8".to_string(),
                    payload: "must not replay".to_string(),
                    extension_fields: Vec::new(),
                },
                MessageScope::Session,
                0,
            )
            .unwrap();
        service
            .accept_at_with_scope(
                &retired.agent_id,
                Envelope {
                    protocol: MMP_PROTOCOL,
                    id: "retired-outbound-message".to_string(),
                    message_type: "send".to_string(),
                    time: "runtime:0".to_string(),
                    sender: retired.clone(),
                    recipient: Recipient::Agent(sender.agent_id.clone()),
                    correlation_id: None,
                    ttl_ms: None,
                    content_type: "text/plain; charset=utf-8".to_string(),
                    payload: "must retire with the sender".to_string(),
                    extension_fields: Vec::new(),
                },
                MessageScope::Session,
                0,
            )
            .unwrap();
        assert!(service.retire_agent_identity(&retired.agent_id));

        let snapshot = service.snapshot_state();
        assert!(snapshot.accepted_messages.iter().any(|message| {
            message.envelope.id == "retired-outbound-message"
                && message.envelope.sender.agent_id == retired.agent_id.as_str()
        }));
        let mut forged = snapshot.clone();
        forged.retired_delivery_floors[0].identity.agent_id = "agent-forged".to_string();
        assert!(MessageService::from_snapshot_state(&forged).is_err());

        let mut duplicate_floor = snapshot.clone();
        duplicate_floor
            .retired_delivery_floors
            .push(duplicate_floor.retired_delivery_floors[0].clone());
        assert!(MessageService::from_snapshot_state(&duplicate_floor).is_err());

        let mut active_floor = snapshot.clone();
        active_floor
            .registered_agents
            .push(active_floor.retired_delivery_floors[0].identity.clone());
        assert!(MessageService::from_snapshot_state(&active_floor).is_err());

        let mut out_of_range_floor = snapshot.clone();
        out_of_range_floor.retired_delivery_floors[0].last_sequence =
            out_of_range_floor.next_sequence;
        assert!(MessageService::from_snapshot_state(&out_of_range_floor).is_err());

        let mut lowered_floor = snapshot.clone();
        lowered_floor.retired_delivery_floors[0].last_sequence = 0;
        let mut lowered_restored = MessageService::from_snapshot_state(&lowered_floor).unwrap();
        lowered_restored
            .ensure_agent_identity(retired.clone(), 1)
            .unwrap();
        lowered_restored
            .subscribe_from_retained_start(&retired.agent_id)
            .unwrap();
        assert!(
            lowered_restored
                .receive_subscribed(&retired.agent_id, 1, usize::MAX)
                .unwrap()
                .messages
                .is_empty()
        );

        let mut legacy_floor = snapshot.clone();
        legacy_floor.schema_version = 2;
        assert!(MessageService::from_snapshot_state(&legacy_floor).is_err());

        let mut restored = MessageService::from_snapshot_state(&snapshot).unwrap();
        restored.ensure_agent_identity(retired.clone(), 1).unwrap();
        restored
            .subscribe_from_retained_start(&retired.agent_id)
            .unwrap();
        assert!(
            restored
                .receive_subscribed(&retired.agent_id, 1, usize::MAX)
                .unwrap()
                .messages
                .is_empty()
        );

        restored
            .accept_at_with_scope(
                &sender_agent_id,
                Envelope {
                    protocol: MMP_PROTOCOL,
                    id: "replacement-session-message".to_string(),
                    message_type: "send".to_string(),
                    time: "runtime:1".to_string(),
                    sender,
                    recipient: Recipient::Session,
                    correlation_id: None,
                    ttl_ms: None,
                    content_type: "text/plain; charset=utf-8".to_string(),
                    payload: "belongs to the replacement".to_string(),
                    extension_fields: Vec::new(),
                },
                MessageScope::Session,
                1,
            )
            .unwrap();
        let messages = restored
            .receive_subscribed(&retired.agent_id, 1, usize::MAX)
            .unwrap()
            .messages;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].envelope.payload, "belongs to the replacement");
    }

    /// An unchanged objective publishes nothing and does not churn presence, so
    /// discovery rows and resume views stay identical.
    #[test]
    fn unchanged_objective_update_is_throttled() {
        let mut service = MessageService::default();
        let identity = service
            .register_agent_with_objective(
                None,
                None,
                "agent",
                Vec::new(),
                Some("Inspect the backlog"),
            )
            .unwrap();
        assert_eq!(service.presence()[0].updated_at_ms, 0);

        assert!(
            !service
                .update_agent_objective(&identity.agent_id, Some("Inspect the backlog"), 500)
                .unwrap()
        );
        assert_eq!(service.presence()[0].updated_at_ms, 0);

        assert!(
            service
                .update_agent_objective(&identity.agent_id, Some("Review the backlog"), 900)
                .unwrap()
        );
        assert_eq!(service.presence()[0].updated_at_ms, 900);
        assert_eq!(
            service
                .registered_identity(&identity.agent_id)
                .and_then(|identity| identity.objective.as_deref()),
            Some("Review the backlog")
        );
        assert_eq!(
            service.presence()[0].identity.objective.as_deref(),
            Some("Review the backlog")
        );
    }

    /// Verifies explicit clearing removes discovery state while a protocol-like
    /// absent refresh remains a no-op and cannot accidentally clear it.
    #[test]
    fn explicit_objective_clear_is_distinct_from_an_absent_refresh() {
        let mut service = MessageService::default();
        let identity = service
            .register_agent_with_objective(
                None,
                None,
                "agent",
                Vec::new(),
                Some("Inspect the backlog"),
            )
            .unwrap();
        assert!(
            !service
                .update_agent_objective(&identity.agent_id, None, 10)
                .unwrap()
        );
        assert!(
            service
                .clear_agent_objective(&identity.agent_id, 20)
                .unwrap()
        );
        assert!(
            service
                .registered_identity(&identity.agent_id)
                .and_then(|identity| identity.objective.as_deref())
                .is_none()
        );
        assert!(
            !service
                .clear_agent_objective(&identity.agent_id, 30)
                .unwrap()
        );
    }

    /// A failed objective refresh keeps the previous published objective and
    /// leaves the presence timestamp untouched.
    #[test]
    fn failed_objective_update_keeps_previous_objective() {
        let mut service = MessageService::default();
        let identity = service
            .register_agent_with_objective(
                None,
                None,
                "agent",
                Vec::new(),
                Some("Inspect the backlog"),
            )
            .unwrap();
        assert_eq!(
            service
                .update_agent_objective(&identity.agent_id, Some("inspect\u{7}the pane"), 700)
                .unwrap_err()
                .message(),
            "MMP objective must not contain control characters"
        );
        assert!(
            service
                .update_agent_objective(&identity.agent_id, Some(String::new().as_str()), 700)
                .is_err()
        );
        assert_eq!(
            service
                .registered_identity(&identity.agent_id)
                .and_then(|identity| identity.objective.as_deref()),
            Some("Inspect the backlog")
        );
        assert_eq!(service.presence()[0].updated_at_ms, 0);
    }

    /// An absent objective refresh is a no-op that keeps the previous published
    /// objective and leaves the presence timestamp untouched.
    #[test]
    fn absent_objective_refresh_keeps_previous_objective_and_timestamp() {
        let mut service = MessageService::default();
        let identity = service
            .register_agent_with_objective(
                None,
                None,
                "agent",
                Vec::new(),
                Some("Inspect the backlog"),
            )
            .unwrap();
        let published_at_ms = service.presence()[0].updated_at_ms;

        assert!(
            !service
                .update_agent_objective(&identity.agent_id, None, published_at_ms + 900)
                .unwrap()
        );
        assert_eq!(
            service
                .registered_identity(&identity.agent_id)
                .and_then(|identity| identity.objective.as_deref()),
            Some("Inspect the backlog")
        );
        assert_eq!(
            service.presence()[0].identity.objective.as_deref(),
            Some("Inspect the backlog")
        );
        assert_eq!(service.presence()[0].updated_at_ms, published_at_ms);
    }

    /// Snapshot round trips carry the objective, and a legacy snapshot payload
    /// without the field restores to no objective instead of failing.
    #[test]
    fn snapshot_round_trip_carries_objective_and_legacy_payload_defaults_none() {
        let mut service = MessageService::default();
        let identity = service
            .register_agent_with_objective(
                None,
                None,
                "agent",
                Vec::new(),
                Some("Inspect the backlog"),
            )
            .unwrap();
        let snapshot = service.snapshot_state();
        assert_eq!(
            snapshot.registered_agents[0].objective.as_deref(),
            Some("Inspect the backlog")
        );
        let restored = MessageService::from_snapshot_state(&snapshot).unwrap();
        assert_eq!(
            restored
                .registered_identity(&identity.agent_id)
                .and_then(|identity| identity.objective.as_deref()),
            Some("Inspect the backlog")
        );

        let legacy_payload = format!(
            r#"{{"protocol":"{}","schema_version":1,"next_sequence":1,"retention_messages":1000,"retention_bytes":1048576,"registered_agents":[{{"agent_id":"{}","pane_id":null,"window_id":null,"role":"agent","capabilities":[]}}],"presence":[],"subscriptions":[],"retained_messages":[],"accepted_messages":[]}}"#,
            MMP_PROTOCOL,
            identity.agent_id.as_str()
        );
        let legacy = serde_json::from_str::<MessageServiceSnapshot>(&legacy_payload).unwrap();
        assert!(legacy.registered_agents[0].objective.is_none());
        let restored = MessageService::from_snapshot_state(&legacy).unwrap();
        assert!(
            restored
                .registered_identity(&identity.agent_id)
                .is_some_and(|identity| identity.objective.is_none())
        );
    }

    /// Project membership derives deterministically from canonical bytes and is
    /// immutable once two trusted registrations disagree on the same agent.
    #[test]
    fn project_scope_is_stable_distinct_and_immutable_per_agent() {
        let first = ProjectScopeId::from_canonical_root_bytes(b"/workspace/alpha");
        assert_eq!(
            first,
            ProjectScopeId::from_canonical_root_bytes(b"/workspace/alpha")
        );
        assert_ne!(
            first,
            ProjectScopeId::from_canonical_root_bytes(b"/workspace/beta")
        );

        let mut service = MessageService::default();
        let identity = SenderIdentity {
            agent_id: AgentId::opaque("agent-%1").unwrap(),
            project_scope: Some(first),
            pane_id: None,
            window_id: None,
            role: Some("agent".to_string()),
            capabilities: Vec::new(),
            objective: None,
        };
        service.ensure_agent_identity(identity.clone(), 0).unwrap();
        let mut mismatched = identity;
        mismatched.project_scope = Some(ProjectScopeId::from_canonical_root_bytes(
            b"/workspace/beta",
        ));
        assert_eq!(
            service
                .ensure_agent_identity(mismatched, 1)
                .unwrap_err()
                .message(),
            "MMP sender project scope cannot change after registration"
        );

        let unscoped = SenderIdentity {
            agent_id: AgentId::opaque("agent-%2").unwrap(),
            project_scope: None,
            pane_id: None,
            window_id: None,
            role: Some("agent".to_string()),
            capabilities: Vec::new(),
            objective: None,
        };
        service.ensure_agent_identity(unscoped.clone(), 0).unwrap();
        let mut upgraded = unscoped;
        let trusted_scope = ProjectScopeId::from_canonical_root_bytes(b"/workspace/trusted");
        upgraded.project_scope = Some(trusted_scope.clone());
        assert_eq!(
            service
                .ensure_agent_identity(upgraded, 1)
                .unwrap()
                .project_scope,
            Some(trusted_scope)
        );
    }

    /// A sparse placeholder identity accepts trusted runtime metadata once,
    /// mirrors that complete identity into presence, and preserves its
    /// objective and publication timestamp.
    #[test]
    fn ensure_agent_identity_fills_sparse_identity_and_presence_atomically() {
        let mut service = MessageService::default();
        let agent_id = AgentId::opaque("agent-%1").unwrap();
        service
            .ensure_agent_identity(
                SenderIdentity {
                    agent_id: agent_id.clone(),
                    project_scope: None,
                    pane_id: None,
                    window_id: None,
                    role: None,
                    capabilities: Vec::new(),
                    objective: Some("Keep objective".to_string()),
                },
                42,
            )
            .unwrap();
        let complete = SenderIdentity {
            agent_id: agent_id.clone(),
            project_scope: Some(ProjectScopeId::from_canonical_root_bytes(
                b"/workspace/alpha",
            )),
            pane_id: Some(PaneId::parse('%', "%1").unwrap()),
            window_id: Some(WindowId::parse('@', "@1").unwrap()),
            role: Some("agent".to_string()),
            capabilities: vec!["agent-harness".to_string()],
            objective: None,
        };

        let reconciled = service.ensure_agent_identity(complete, 99).unwrap();

        assert_eq!(reconciled.pane_id.as_ref().map(PaneId::as_str), Some("%1"));
        assert_eq!(
            reconciled.window_id.as_ref().map(WindowId::as_str),
            Some("@1")
        );
        assert_eq!(reconciled.role.as_deref(), Some("agent"));
        assert_eq!(reconciled.capabilities, vec!["agent-harness"]);
        assert_eq!(reconciled.objective.as_deref(), Some("Keep objective"));
        assert_eq!(service.presence()[0].identity, reconciled);
        assert_eq!(service.presence()[0].updated_at_ms, 42);
    }

    /// Every populated immutable identity field conflicts rather than replacing
    /// registered metadata, and a mixed fill plus scope conflict leaves both
    /// identity and presence exactly unchanged.
    #[test]
    fn ensure_agent_identity_rejects_conflicts_without_partial_mutation() {
        let mut service = MessageService::default();
        let agent_id = AgentId::opaque("agent-%2").unwrap();
        let original = SenderIdentity {
            agent_id: agent_id.clone(),
            project_scope: Some(ProjectScopeId::from_canonical_root_bytes(
                b"/workspace/alpha",
            )),
            pane_id: Some(PaneId::parse('%', "%2").unwrap()),
            window_id: Some(WindowId::parse('@', "@2").unwrap()),
            role: Some("agent".to_string()),
            capabilities: vec!["agent-harness".to_string()],
            objective: Some("Preserve objective".to_string()),
        };
        service.ensure_agent_identity(original.clone(), 42).unwrap();

        for conflicting in [
            SenderIdentity {
                pane_id: Some(PaneId::parse('%', "%3").unwrap()),
                ..original.clone()
            },
            SenderIdentity {
                window_id: Some(WindowId::parse('@', "@3").unwrap()),
                ..original.clone()
            },
            SenderIdentity {
                role: Some("worker".to_string()),
                ..original.clone()
            },
            SenderIdentity {
                capabilities: vec!["worker".to_string()],
                ..original.clone()
            },
        ] {
            assert!(service.ensure_agent_identity(conflicting, 99).is_err());
            assert_eq!(service.registered_identity(&agent_id), Some(&original));
            assert_eq!(service.presence()[0].identity, original);
            assert_eq!(service.presence()[0].updated_at_ms, 42);
        }

        let sparse_id = AgentId::opaque("agent-%3").unwrap();
        let sparse = SenderIdentity {
            agent_id: sparse_id.clone(),
            project_scope: Some(ProjectScopeId::from_canonical_root_bytes(
                b"/workspace/alpha",
            )),
            pane_id: None,
            window_id: None,
            role: None,
            capabilities: Vec::new(),
            objective: None,
        };
        service.ensure_agent_identity(sparse.clone(), 7).unwrap();
        let mixed_conflict = SenderIdentity {
            agent_id: sparse_id.clone(),
            project_scope: Some(ProjectScopeId::from_canonical_root_bytes(
                b"/workspace/beta",
            )),
            pane_id: Some(PaneId::parse('%', "%3").unwrap()),
            window_id: Some(WindowId::parse('@', "@3").unwrap()),
            role: Some("agent".to_string()),
            capabilities: vec!["agent-harness".to_string()],
            objective: None,
        };
        assert!(service.ensure_agent_identity(mixed_conflict, 99).is_err());
        assert_eq!(service.registered_identity(&sparse_id), Some(&sparse));
        assert_eq!(
            service
                .presence()
                .into_iter()
                .find(|presence| presence.identity.agent_id == sparse_id)
                .map(|presence| presence.identity),
            Some(sparse)
        );
    }

    /// Project-default delivery excludes cross-project direct recipients while
    /// an explicit session audience deliberately widens the same selector.
    #[test]
    fn project_audience_isolates_direct_recipients_and_session_scope_widens() {
        let mut service = MessageService::default();
        let alpha = ProjectScopeId::from_canonical_root_bytes(b"/workspace/alpha");
        let beta = ProjectScopeId::from_canonical_root_bytes(b"/workspace/beta");
        let sender = SenderIdentity {
            agent_id: AgentId::opaque("agent-alpha").unwrap(),
            project_scope: Some(alpha),
            pane_id: None,
            window_id: None,
            role: Some("agent".to_string()),
            capabilities: Vec::new(),
            objective: None,
        };
        let target = SenderIdentity {
            agent_id: AgentId::opaque("agent-beta").unwrap(),
            project_scope: Some(beta),
            pane_id: None,
            window_id: None,
            role: Some("agent".to_string()),
            capabilities: Vec::new(),
            objective: None,
        };
        service.ensure_agent_identity(sender.clone(), 0).unwrap();
        service.ensure_agent_identity(target.clone(), 0).unwrap();
        service
            .subscribe_from_retained_start(&target.agent_id)
            .unwrap();
        let envelope = |id: &str| Envelope {
            protocol: MMP_PROTOCOL,
            id: id.to_string(),
            message_type: "send".to_string(),
            time: "runtime:0".to_string(),
            sender: sender.clone(),
            recipient: Recipient::Agent(target.agent_id.clone()),
            correlation_id: None,
            ttl_ms: None,
            content_type: "text/plain; charset=utf-8".to_string(),
            payload: "scope test".to_string(),
            extension_fields: Vec::new(),
        };

        assert_eq!(
            service
                .accept_at(&sender.agent_id, envelope("project"), 0)
                .unwrap_err()
                .message(),
            MMP_UNDELIVERABLE_MESSAGE
        );
        assert!(service.receive_for(&target.agent_id, 0).is_empty());
        service
            .accept_at_with_scope(
                &sender.agent_id,
                envelope("session"),
                MessageScope::Session,
                0,
            )
            .unwrap();
        assert_eq!(service.receive_for(&target.agent_id, 0).len(), 1);
        assert_eq!(
            service
                .receive_subscribed(&target.agent_id, 0, 10)
                .unwrap()
                .messages
                .len(),
            1
        );
        assert_eq!(service.fanout_ready(0, 10).len(), 1);
    }

    /// Schema-v2 snapshots preserve private trusted memberships and resolved
    /// delivery audiences without widening restored project traffic.
    #[test]
    fn snapshot_v2_preserves_project_membership_and_resolved_audiences() {
        let mut service = MessageService::default();
        let alpha = ProjectScopeId::from_canonical_root_bytes(b"/workspace/alpha");
        let sender = SenderIdentity {
            agent_id: AgentId::opaque("agent-snapshot-alpha").unwrap(),
            project_scope: Some(alpha.clone()),
            pane_id: None,
            window_id: None,
            role: Some("agent".to_string()),
            capabilities: Vec::new(),
            objective: None,
        };
        let target = SenderIdentity {
            agent_id: AgentId::opaque("agent-snapshot-target").unwrap(),
            project_scope: Some(alpha),
            pane_id: None,
            window_id: None,
            role: Some("agent".to_string()),
            capabilities: Vec::new(),
            objective: None,
        };
        service.ensure_agent_identity(sender.clone(), 0).unwrap();
        service.ensure_agent_identity(target.clone(), 0).unwrap();
        service
            .subscribe_from_retained_start(&target.agent_id)
            .unwrap();
        let envelope = |id: &str| Envelope {
            protocol: MMP_PROTOCOL,
            id: id.to_string(),
            message_type: "send".to_string(),
            time: "runtime:0".to_string(),
            sender: sender.clone(),
            recipient: Recipient::Agent(target.agent_id.clone()),
            correlation_id: None,
            ttl_ms: None,
            content_type: "text/plain; charset=utf-8".to_string(),
            payload: "snapshot audience".to_string(),
            extension_fields: Vec::new(),
        };
        service
            .accept_at(&sender.agent_id, envelope("project"), 0)
            .unwrap();
        service
            .accept_at_with_scope(
                &sender.agent_id,
                envelope("session"),
                MessageScope::Session,
                0,
            )
            .unwrap();

        let snapshot = service.snapshot_state();
        assert_eq!(snapshot.schema_version, 3);
        assert!(
            snapshot
                .registered_agents
                .iter()
                .all(|identity| identity.project_scope.is_some())
        );
        assert!(
            snapshot
                .retained_messages
                .iter()
                .all(|message| message.audience.is_some())
        );
        assert!(
            snapshot
                .accepted_messages
                .iter()
                .all(|message| message.audience.is_some())
        );

        let restored = MessageService::from_snapshot_state(&snapshot).unwrap();
        assert_eq!(restored.snapshot_state(), snapshot);
        assert_eq!(restored.receive_for(&target.agent_id, 0).len(), 2);
    }

    /// Schema-v2 snapshots reject missing or inconsistent resolved-audience
    /// metadata instead of widening restored traffic.
    #[test]
    fn snapshot_v2_rejects_missing_and_inconsistent_audience_metadata() {
        let mut service = MessageService::default();
        let project = ProjectScopeId::from_canonical_root_bytes(b"/workspace/snapshot-invalid");
        let sender = SenderIdentity {
            agent_id: AgentId::opaque("agent-snapshot-invalid-sender").unwrap(),
            project_scope: Some(project.clone()),
            pane_id: None,
            window_id: None,
            role: Some("agent".to_string()),
            capabilities: Vec::new(),
            objective: None,
        };
        let target = SenderIdentity {
            agent_id: AgentId::opaque("agent-snapshot-invalid-target").unwrap(),
            project_scope: Some(project),
            pane_id: None,
            window_id: None,
            role: Some("agent".to_string()),
            capabilities: Vec::new(),
            objective: None,
        };
        service.ensure_agent_identity(sender.clone(), 0).unwrap();
        service.ensure_agent_identity(target.clone(), 0).unwrap();
        service
            .subscribe_from_retained_start(&target.agent_id)
            .unwrap();
        service
            .accept_at(
                &sender.agent_id,
                Envelope {
                    protocol: MMP_PROTOCOL,
                    id: "snapshot-invalid-audience".to_string(),
                    message_type: "send".to_string(),
                    time: "runtime:0".to_string(),
                    sender: sender.clone(),
                    recipient: Recipient::Agent(target.agent_id),
                    correlation_id: None,
                    ttl_ms: None,
                    content_type: "text/plain; charset=utf-8".to_string(),
                    payload: "snapshot validation".to_string(),
                    extension_fields: Vec::new(),
                },
                0,
            )
            .unwrap();

        let snapshot = service.snapshot_state();
        let mut missing = snapshot.clone();
        missing.retained_messages[0].audience = None;
        assert!(MessageService::from_snapshot_state(&missing).is_err());

        let mut inconsistent = snapshot;
        inconsistent.accepted_messages[0].audience = Some(MessageAudienceSnapshot {
            kind: "session".to_string(),
            project_scope: None,
        });
        assert!(MessageService::from_snapshot_state(&inconsistent).is_err());

        let mut duplicate = service.snapshot_state();
        let mut duplicate_record = duplicate.accepted_messages[0].clone();
        duplicate_record.audience = Some(MessageAudienceSnapshot {
            kind: "session".to_string(),
            project_scope: None,
        });
        duplicate.accepted_messages.push(duplicate_record);
        assert!(MessageService::from_snapshot_state(&duplicate).is_err());

        let mut malformed_scope = service.snapshot_state();
        malformed_scope.registered_agents[0].project_scope = Some("not-a-scope-id".to_string());
        assert!(MessageService::from_snapshot_state(&malformed_scope).is_err());

        let valid = service.snapshot_state();
        let mut retargeted = valid.clone();
        let replacement_scope = ProjectScopeId::from_canonical_root_bytes(b"/workspace/other")
            .snapshot_value()
            .to_string();
        retargeted.retained_messages[0]
            .audience
            .as_mut()
            .unwrap()
            .project_scope = Some(replacement_scope.clone());
        retargeted.accepted_messages[0]
            .audience
            .as_mut()
            .unwrap()
            .project_scope = Some(replacement_scope);
        assert!(MessageService::from_snapshot_state(&retargeted).is_err());

        let mut unscoped_sender = valid.clone();
        for identity in &mut unscoped_sender.registered_agents {
            identity.project_scope = None;
        }
        for presence in &mut unscoped_sender.presence {
            presence.identity.project_scope = None;
        }
        unscoped_sender.retained_messages[0]
            .envelope
            .sender
            .project_scope = None;
        unscoped_sender.accepted_messages[0]
            .envelope
            .sender
            .project_scope = None;
        assert!(MessageService::from_snapshot_state(&unscoped_sender).is_err());

        let mut duplicate_presence = valid.clone();
        duplicate_presence
            .presence
            .push(duplicate_presence.presence[0].clone());
        assert!(MessageService::from_snapshot_state(&duplicate_presence).is_err());

        let mut missing_presence = valid.clone();
        missing_presence.presence.pop();
        assert!(MessageService::from_snapshot_state(&missing_presence).is_err());

        let mut duplicate_subscription = valid.clone();
        duplicate_subscription
            .subscriptions
            .push(duplicate_subscription.subscriptions[0].clone());
        assert!(MessageService::from_snapshot_state(&duplicate_subscription).is_err());

        let mut unknown_subscription = valid.clone();
        unknown_subscription.subscriptions[0].recipient = "agent-unknown".to_string();
        assert!(MessageService::from_snapshot_state(&unknown_subscription).is_err());

        let mut advanced_cursor = valid.clone();
        advanced_cursor.subscriptions[0].last_sequence = advanced_cursor.next_sequence;
        assert!(MessageService::from_snapshot_state(&advanced_cursor).is_err());

        let mut duplicate_sequence = valid.clone();
        let mut duplicate_retained = duplicate_sequence.retained_messages[0].clone();
        duplicate_retained.envelope.id = "snapshot-invalid-duplicate-sequence".to_string();
        duplicate_sequence
            .retained_messages
            .push(duplicate_retained);
        assert!(MessageService::from_snapshot_state(&duplicate_sequence).is_err());

        let mut orphan_accepted = valid.clone();
        orphan_accepted.retained_messages.clear();
        assert!(MessageService::from_snapshot_state(&orphan_accepted).is_err());

        let mut retained_without_accepted = valid.clone();
        retained_without_accepted.accepted_messages.clear();
        assert!(MessageService::from_snapshot_state(&retained_without_accepted).is_err());

        let mut mismatched_sequence = valid.clone();
        mismatched_sequence.accepted_messages[0].delivery.sequence = 9;
        assert!(MessageService::from_snapshot_state(&mismatched_sequence).is_err());

        let mut mismatched_acceptance_time = valid;
        mismatched_acceptance_time.accepted_messages[0].accepted_at_ms = 9;
        assert!(MessageService::from_snapshot_state(&mismatched_acceptance_time).is_err());
    }

    /// Schema-v1 records lacked resolved audiences, so retained traffic is
    /// discarded rather than being reinterpreted as session or project traffic.
    #[test]
    fn snapshot_v1_discards_audience_less_retained_and_accepted_messages() {
        let mut service = MessageService::default();
        let project = ProjectScopeId::from_canonical_root_bytes(b"/workspace/legacy");
        let sender = SenderIdentity {
            agent_id: AgentId::opaque("agent-legacy-sender").unwrap(),
            project_scope: Some(project.clone()),
            pane_id: None,
            window_id: None,
            role: Some("agent".to_string()),
            capabilities: Vec::new(),
            objective: None,
        };
        let target = SenderIdentity {
            agent_id: AgentId::opaque("agent-legacy-target").unwrap(),
            project_scope: Some(project),
            pane_id: None,
            window_id: None,
            role: Some("agent".to_string()),
            capabilities: Vec::new(),
            objective: None,
        };
        service.ensure_agent_identity(sender.clone(), 0).unwrap();
        service.ensure_agent_identity(target.clone(), 0).unwrap();
        service
            .subscribe_from_retained_start(&target.agent_id)
            .unwrap();
        service
            .accept_at(
                &sender.agent_id,
                Envelope {
                    protocol: MMP_PROTOCOL,
                    id: "legacy-message".to_string(),
                    message_type: "send".to_string(),
                    time: "runtime:0".to_string(),
                    sender: sender.clone(),
                    recipient: Recipient::Agent(target.agent_id.clone()),
                    correlation_id: None,
                    ttl_ms: None,
                    content_type: "text/plain; charset=utf-8".to_string(),
                    payload: "must not replay".to_string(),
                    extension_fields: Vec::new(),
                },
                0,
            )
            .unwrap();

        let mut legacy = service.snapshot_state();
        legacy.schema_version = 1;
        for identity in &mut legacy.registered_agents {
            identity.project_scope = None;
        }
        for presence in &mut legacy.presence {
            presence.identity.project_scope = None;
        }
        for message in &mut legacy.retained_messages {
            message.audience = None;
            message.envelope.sender.project_scope = None;
        }
        for message in &mut legacy.accepted_messages {
            message.audience = None;
            message.envelope.sender.project_scope = None;
        }
        legacy.retained_messages[0].envelope.message_type = "invalid legacy type".to_string();
        legacy.accepted_messages[0].delivery.status = "invalid legacy status".to_string();

        let restored = MessageService::from_snapshot_state(&legacy).unwrap();
        assert!(restored.receive_for(&target.agent_id, 0).is_empty());
        assert!(restored.snapshot_state().retained_messages.is_empty());
        assert!(restored.snapshot_state().accepted_messages.is_empty());
        assert!(
            restored
                .registered_identity(&target.agent_id)
                .is_some_and(|identity| identity.project_scope.is_none())
        );
    }

    /// Scope-less acceptance is always project-scoped: matching membership
    /// succeeds while an unscoped sender fails closed instead of widening.
    #[test]
    fn default_acceptance_requires_sender_project_membership() {
        let mut service = MessageService::default();
        let project = ProjectScopeId::from_canonical_root_bytes(b"/workspace/alpha");
        let sender = SenderIdentity {
            agent_id: AgentId::opaque("agent-alpha-sender").unwrap(),
            project_scope: Some(project.clone()),
            pane_id: None,
            window_id: None,
            role: Some("agent".to_string()),
            capabilities: Vec::new(),
            objective: None,
        };
        let target = SenderIdentity {
            agent_id: AgentId::opaque("agent-alpha-target").unwrap(),
            project_scope: Some(project),
            pane_id: None,
            window_id: None,
            role: Some("agent".to_string()),
            capabilities: Vec::new(),
            objective: None,
        };
        service.ensure_agent_identity(sender.clone(), 0).unwrap();
        service.ensure_agent_identity(target.clone(), 0).unwrap();
        let envelope = |id: &str, sender: SenderIdentity| Envelope {
            protocol: MMP_PROTOCOL,
            id: id.to_string(),
            message_type: "send".to_string(),
            time: "runtime:0".to_string(),
            sender,
            recipient: Recipient::Agent(target.agent_id.clone()),
            correlation_id: None,
            ttl_ms: None,
            content_type: "text/plain; charset=utf-8".to_string(),
            payload: "scope test".to_string(),
            extension_fields: Vec::new(),
        };
        assert!(
            service
                .accept_at(
                    &sender.agent_id,
                    envelope("same-project", sender.clone()),
                    0
                )
                .is_ok()
        );

        let unscoped = service.register_agent(None, None, "agent", Vec::new());
        assert_eq!(
            service
                .accept(
                    &unscoped.agent_id,
                    envelope("unscoped-accept", unscoped.clone())
                )
                .unwrap_err()
                .message(),
            MMP_UNDELIVERABLE_MESSAGE
        );
        assert_eq!(
            service
                .accept_at(
                    &unscoped.agent_id,
                    envelope("unscoped-accept-at", unscoped.clone()),
                    0,
                )
                .unwrap_err()
                .message(),
            MMP_UNDELIVERABLE_MESSAGE
        );
        assert_eq!(service.receive_for(&target.agent_id, 0).len(), 1);
    }

    /// Project discovery exposes only the requester and trusted same-project
    /// peers, while an explicit session scope widens that visibility.
    #[test]
    fn requester_scoped_discovery_isolates_projects_and_unscoped_peers() {
        let mut service = MessageService::default();
        let alpha = ProjectScopeId::from_canonical_root_bytes(b"/workspace/alpha");
        let beta = ProjectScopeId::from_canonical_root_bytes(b"/workspace/beta");
        let identity = |agent_id: &str, project_scope: Option<ProjectScopeId>| SenderIdentity {
            agent_id: AgentId::opaque(agent_id).unwrap(),
            project_scope,
            pane_id: None,
            window_id: None,
            role: Some("agent".to_string()),
            capabilities: Vec::new(),
            objective: None,
        };
        let requester = identity("agent-alpha", Some(alpha.clone()));
        let same_project = identity("agent-alpha-peer", Some(alpha));
        let other_project = identity("agent-beta", Some(beta));
        let unscoped = identity("agent-unscoped", None);
        for candidate in [
            requester.clone(),
            same_project.clone(),
            other_project.clone(),
            unscoped.clone(),
        ] {
            service.ensure_agent_identity(candidate, 0).unwrap();
        }

        let project = service.discover_agents_filtered_for_requester(
            &requester.agent_id,
            MessageScope::Project,
            None,
            None,
            None,
            None,
            None,
            &[],
        );
        assert_eq!(
            project
                .iter()
                .map(|identity| identity.agent_id.as_str())
                .collect::<Vec<_>>(),
            vec!["agent-alpha", "agent-alpha-peer"]
        );
        let session = service.discover_agents_filtered_for_requester(
            &requester.agent_id,
            MessageScope::Session,
            None,
            None,
            None,
            None,
            None,
            &[],
        );
        assert_eq!(session.len(), 4);
        let unscoped_project = service.discover_agents_filtered_for_requester(
            &unscoped.agent_id,
            MessageScope::Project,
            None,
            None,
            None,
            None,
            None,
            &[],
        );
        assert_eq!(unscoped_project, vec![unscoped]);
    }

    /// Trusted lifecycle rebinding changes only project membership, retaining
    /// the registered objective, presence status, and delivery subscription.
    #[test]
    fn lifecycle_project_scope_rebind_preserves_identity_delivery_state() {
        let mut service = MessageService::default();
        let identity = service
            .register_agent_with_objective(None, None, "agent", Vec::new(), Some("Keep state"))
            .unwrap();
        service
            .subscribe_from_retained_start(&identity.agent_id)
            .unwrap();
        service
            .update_presence(&identity.agent_id, AgentPresenceStatus::Busy, 42)
            .unwrap();
        let scope = ProjectScopeId::from_canonical_root_bytes(b"/workspace/rebound");

        let rebound = service
            .rebind_agent_project_scope(&identity.agent_id, Some(scope.clone()))
            .unwrap();

        assert_eq!(rebound.project_scope, Some(scope));
        assert_eq!(rebound.objective.as_deref(), Some("Keep state"));
        assert!(service.subscription(&identity.agent_id).is_some());
        assert_eq!(service.presence()[0].status, AgentPresenceStatus::Busy);
        assert_eq!(service.presence()[0].updated_at_ms, 42);
    }
}
