//! Single-state facade for in-memory MMP identity and message delivery.
//!
//! Identity and discovery, durable snapshot conversion/validation, subscriptions,
//! indexed receive, fair fanout, and retention live in focused child modules.
//! Every component operates on the same `MessageService`: none owns a second
//! registry, queue, acknowledgement cursor, or audience authority. Acceptance
//! authenticates sender provenance and freezes project/session membership before
//! queueing; all delivery paths use the predicates defined here. Snapshot restore
//! validates that same state before it becomes available to callers.

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

mod discovery;
mod identity;
mod snapshot;
mod snapshot_codec;
mod snapshot_validation;
use snapshot_codec::*;
mod fanout;
mod receive;
mod retention;
mod subscriptions;

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

    /// Returns the sequence that will be assigned to the next accepted message.
    ///
    /// Runtime-authored envelopes use this durable, monotonically increasing
    /// value as an occurrence identity. Because snapshots preserve the sequence,
    /// generated message ids cannot collide with accepted pre-restart traffic.
    pub fn next_message_sequence(&self) -> MessageSequence {
        self.next_sequence
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

/// Builds the deduplicated selectors used by the shared delivery index.
///
/// Keep these selectors equivalent to `recipient_matches`; indexes accelerate
/// matching but must never create a separate interpretation of recipient scope.
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

/// Appends a selector once without changing first-occurrence traversal order.
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

#[cfg(test)]
mod tests {
    use super::*;

    mod objectives;

    mod retirement {
        use super::*;

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
    }

    mod objective_refresh;

    mod objective_snapshot;

    mod identity_reconciliation;

    mod audience {
        use super::*;

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
}
