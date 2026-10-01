//! Typed snapshot record encoding and validated decoding.
//!
//! These helpers preserve private audience metadata and exact payload fields;
//! they never choose a live recipient or grant authority from persisted text.

use super::*;

/// Projects one identity into private snapshot metadata.
pub(super) fn identity_snapshot(identity: &SenderIdentity) -> MessageIdentitySnapshot {
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

/// Decodes a bounded identity under its snapshot schema's membership policy.
pub(super) fn sender_identity_from_snapshot(
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

/// Preserves declared presence and its last publication instant.
pub(super) fn presence_snapshot(record: &PresenceRecord) -> MessagePresenceSnapshot {
    MessagePresenceSnapshot {
        identity: identity_snapshot(&record.identity),
        status: presence_status_name(record.status).to_string(),
        updated_at_ms: record.updated_at_ms,
    }
}

/// Projects the recipient's durable acknowledgement boundary.
pub(super) fn cursor_snapshot(cursor: &DeliveryCursor) -> MessageDeliveryCursorSnapshot {
    MessageDeliveryCursorSnapshot {
        recipient: cursor.recipient.to_string(),
        last_sequence: cursor.last_sequence,
    }
}

/// Preserves retained envelope occurrence and acceptance-time audience.
pub(super) fn queued_envelope_snapshot(queued: &QueuedEnvelope) -> MessageQueuedEnvelopeSnapshot {
    MessageQueuedEnvelopeSnapshot {
        sequence: queued.sequence,
        accepted_at_ms: queued.accepted_at_ms,
        envelope: envelope_snapshot(&queued.envelope),
        audience: Some(audience_snapshot(&queued.audience)),
    }
}

/// Preserves acceptance idempotency metadata without recomputing delivery.
pub(super) fn accepted_message_snapshot(accepted: &AcceptedMessage) -> MessageAcceptedSnapshot {
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

/// Converts a resolved audience into private snapshot metadata.
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

/// Restores required resolved audience metadata without implicit widening.
pub(super) fn audience_from_snapshot(
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

/// Rejects restored audiences inconsistent with their sender membership.
pub(super) fn validate_snapshot_audience_provenance(
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

/// Projects payload, occurrence metadata and extension fields exactly.
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

/// Validates typed envelope fields before snapshot reconstruction.
pub(super) fn envelope_from_snapshot(
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

/// Encodes one typed selector without resolving a new recipient.
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

/// Decodes a selector, rejecting missing values and unsupported kinds.
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

/// Requires a nonempty, control-free recipient identifier.
fn parse_required_recipient_id(snapshot: &MessageRecipientSnapshot) -> Result<StableId> {
    parse_opaque_id(required_recipient_value(snapshot)?, "MMP recipient id")
}

/// Requires one nonempty selector value.
fn required_recipient_value(snapshot: &MessageRecipientSnapshot) -> Result<&str> {
    snapshot
        .value
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| MessageError::invalid_args("snapshot MMP recipient value must not be empty"))
}

/// Decodes persisted identifiers without interpreting them as authority.
pub(super) fn parse_opaque_id(value: &str, field: &'static str) -> Result<StableId> {
    StableId::opaque(value).ok_or_else(|| {
        MessageError::invalid_args(format!(
            "snapshot {field} is empty or contains control characters"
        ))
    })
}

/// Encodes a declared presence status with its stable wire spelling.
fn presence_status_name(status: AgentPresenceStatus) -> &'static str {
    match status {
        AgentPresenceStatus::Available => "available",
        AgentPresenceStatus::Busy => "busy",
        AgentPresenceStatus::Blocked => "blocked",
        AgentPresenceStatus::Offline => "offline",
    }
}

/// Rejects unsupported persisted presence status values.
pub(super) fn parse_presence_status(value: &str) -> Result<AgentPresenceStatus> {
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

/// Encodes accepted delivery status without recomputing acceptance.
fn delivery_status_name(status: DeliveryStatus) -> &'static str {
    match status {
        DeliveryStatus::Accepted => "accepted",
        DeliveryStatus::Undeliverable => "undeliverable",
        DeliveryStatus::Expired => "expired",
    }
}

/// Rejects unsupported persisted delivery status values.
pub(super) fn parse_delivery_status(value: &str) -> Result<DeliveryStatus> {
    match value {
        "accepted" => Ok(DeliveryStatus::Accepted),
        "undeliverable" => Ok(DeliveryStatus::Undeliverable),
        "expired" => Ok(DeliveryStatus::Expired),
        _ => Err(MessageError::invalid_args(
            "snapshot MMP delivery status is invalid",
        )),
    }
}
