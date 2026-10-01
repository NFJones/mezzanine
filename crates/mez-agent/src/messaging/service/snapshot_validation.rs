//! Cross-record validation before message snapshot reconstruction.
//!
//! Audience provenance, sequence bounds, registered identities and accepted
//! receipts are checked together before a candidate becomes live service state.

use super::*;

pub(super) fn validate_message_service_snapshot(snapshot: &MessageServiceSnapshot) -> Result<()> {
    if snapshot.protocol != MMP_PROTOCOL || !matches!(snapshot.schema_version, 1..=3) {
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
