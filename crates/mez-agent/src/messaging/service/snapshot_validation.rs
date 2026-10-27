//! Cross-record validation before message snapshot reconstruction.
//!
//! Audience provenance, sequence bounds, registered identities and accepted
//! receipts are checked together before a candidate becomes live service state.
//! Schema4 fresh raw floors additionally require complete, unique, exact sender
//! versions referenced by canonical retained authored mail. This does not change
//! active/reusable identity comparisons or infer sender authority from objectives.

use super::*;

/// Rejects cross-record contradictions before constructing any live state.
/// Fresh provenance is explicit schema4 data; legacy schemas never adopt it.
pub(super) fn validate_message_service_snapshot(
    snapshot: &MessageServiceSnapshot,
    pending_limits: PendingDeliveryLimits,
) -> Result<()> {
    if snapshot.protocol != MMP_PROTOCOL || !matches!(snapshot.schema_version, 1..=7) {
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
    super::pending::validate_pending_snapshot(snapshot, pending_limits)?;
    validate_recipient_bindings(snapshot, &registered_by_id)?;
    super::offers::validate_delivery_offer_snapshot(snapshot)?;
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
    if snapshot.schema_version < 4
        && snapshot
            .retired_delivery_floors
            .iter()
            .any(|floor| floor.fresh_sender_versions.is_some())
    {
        return Err(MessageError::invalid_args(
            "fresh MMP sender versions require schema version 4",
        ));
    }
    let mut retired_identities = HashMap::new();
    let mut fresh_sender_versions = HashMap::new();
    if snapshot.schema_version >= 3 {
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
            if let Some(snapshots) = floor.fresh_sender_versions.as_ref() {
                if snapshots.is_empty()
                    || snapshots.len() > snapshot.canonical_messages().count()
                    || identity.pane_id.is_some()
                    || identity.window_id.is_some()
                {
                    return Err(MessageError::invalid_args(
                        "fresh MMP sender evidence must be nonempty, retained and pane-less",
                    ));
                }
                let versions = snapshots
                    .iter()
                    .map(|version| sender_identity_from_snapshot(version, snapshot.schema_version))
                    .collect::<Result<Vec<_>>>()?;
                let mut objectives = HashSet::new();
                for version in &versions {
                    if let Some(objective) = version.objective.as_ref()
                        && normalize_objective(objective)? != *objective
                    {
                        return Err(MessageError::invalid_args(
                            "fresh MMP sender objectives must be normalized",
                        ));
                    }
                    if !same_immutable_sender(&identity, version)
                        || !objectives.insert(&version.objective)
                    {
                        return Err(MessageError::invalid_args(
                            "fresh MMP sender versions must be unique with exact immutable provenance",
                        ));
                    }
                }
                if versions.first() != Some(&identity) {
                    return Err(MessageError::invalid_args(
                        "fresh MMP sender anchor must be its first exact version",
                    ));
                }
                drop(objectives);
                fresh_sender_versions.insert(identity.agent_id.clone(), versions);
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
    let mut fresh_retained: HashMap<AgentId, Vec<(MessageSequence, SenderIdentity)>> =
        HashMap::new();
    if snapshot.schema_version >= 2 {
        let mut retained_sequences = HashSet::new();
        let mut previous_sequence = 0;
        for retained in snapshot.canonical_messages() {
            if snapshot.schema_version < 7
                && !fresh_sender_versions.is_empty()
                && retained.sequence <= previous_sequence
            {
                return Err(MessageError::invalid_args(
                    "fresh MMP sender evidence requires canonical retained FIFO order",
                ));
            }
            previous_sequence = retained.sequence;
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
            if snapshot
                .retained_messages
                .iter()
                .any(|row| std::ptr::eq(row, retained))
            {
                queued_bytes = queued_bytes.saturating_add(retained.envelope.payload.len());
            }
            let envelope = envelope_from_snapshot(&retained.envelope, snapshot.schema_version)?;
            if registered_by_id.get(envelope.sender.agent_id.as_str())
                != Some(&&retained.envelope.sender)
                && retired_identities.get(&envelope.sender.agent_id) != Some(&envelope.sender)
                && !fresh_sender_versions
                    .get(&envelope.sender.agent_id)
                    .is_some_and(|versions| versions.contains(&envelope.sender))
            {
                return Err(MessageError::invalid_args(
                    "snapshot MMP envelope sender must match a registered agent",
                ));
            }
            let audience = audience_from_snapshot(retained.audience.as_ref())?;
            validate_snapshot_audience_provenance(&audience, &envelope.sender)?;
            if fresh_sender_versions.contains_key(&envelope.sender.agent_id) {
                fresh_retained
                    .entry(envelope.sender.agent_id.clone())
                    .or_default()
                    .push((retained.sequence, envelope.sender.clone()));
            }
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
        // Fresh evidence is a complete exact projection of canonical authored
        // mail, not authority inferred from an agent id or mutable objective.
        for floor in &snapshot.retired_delivery_floors {
            let id = parse_opaque_id(&floor.identity.agent_id, "MMP retired identity")?;
            let Some(versions) = fresh_sender_versions.get(&id) else {
                continue;
            };
            let Some(mut authored) = fresh_retained.remove(&id) else {
                return Err(MessageError::invalid_args(
                    "fresh MMP sender evidence must reference retained authored mail",
                ));
            };
            authored.sort_by_key(|(sequence, _)| *sequence);
            if authored.last().map(|(sequence, _)| *sequence) != Some(floor.last_sequence) {
                return Err(MessageError::invalid_args(
                    "fresh MMP sender floor must equal its maximum authored sequence",
                ));
            }
            let mut objectives = HashSet::new();
            let expected = authored
                .iter()
                .filter_map(|(_, sender)| objectives.insert(&sender.objective).then_some(sender))
                .collect::<Vec<_>>();
            if expected.len() != versions.len()
                || expected
                    .iter()
                    .zip(versions)
                    .any(|(actual, declared)| *actual != declared)
            {
                return Err(MessageError::invalid_args(
                    "fresh MMP sender versions must exactly match canonical retained evidence",
                ));
            }
        }
        let mut accepted_by_id = HashMap::new();
        for accepted in &snapshot.accepted_messages {
            let envelope = envelope_from_snapshot(&accepted.envelope, snapshot.schema_version)?;
            if registered_by_id.get(envelope.sender.agent_id.as_str())
                != Some(&&accepted.envelope.sender)
                && retired_identities.get(&envelope.sender.agent_id) != Some(&envelope.sender)
                && !fresh_sender_versions
                    .get(&envelope.sender.agent_id)
                    .is_some_and(|versions| versions.contains(&envelope.sender))
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
