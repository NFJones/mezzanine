//! Historical sender facts independent of current delivery or registry rights.
//!
//! Schema9 records the fixed-size digest of the immutable sender and original interval once per
//! retained canonical occurrence. Reused addresses and mutable discovery facts
//! cannot rewrite that source. This is checked snapshot consistency, never a
//! live acceptance proof, socket reconnect credential or permission grant.
//! Provenance is bounded by existing canonical count/metadata reservations and
//! contains no identity-string or payload copy. Legacy restoration supplies only provable records.

use sha2::{Digest, Sha256};

use super::*;

/// Captures each canonical source before current registry changes can reinterpret
/// it. Queue indexes already contain the shared replay/custody union exactly once.
pub(super) fn capture(service: &MessageService) -> Vec<MessageSenderProvenanceSnapshot> {
    service
        .queued_by_sequence
        .values()
        .map(|row| MessageSenderProvenanceSnapshot {
            sequence: row.sequence,
            sender_sha256: live_identity_digest(&row.envelope.sender),
            registered_after_sequence: row.sender_registered_after_sequence,
            fresh_retirement: !service
                .registered
                .contains_key(&row.envelope.sender.agent_id)
                && service
                    .retired_delivery_floors
                    .get(&row.envelope.sender.agent_id)
                    .is_some_and(|floor| floor.fresh_sender_versions.is_some()),
        })
        .collect()
}

/// Checks complete, uniquely ordered provenance against bounded canonical rows.
/// No unknown sender address, invented interval, payload or live capability is
/// admitted; callers still validate every envelope/audience/receipt separately.
pub(super) fn validate(snapshot: &MessageServiceSnapshot) -> Result<HashSet<MessageSequence>> {
    if snapshot.schema_version < 9 {
        if snapshot.sender_provenance.is_some() {
            return Err(MessageError::invalid_args(
                "MMP sender provenance requires schema9",
            ));
        }
        return Ok(HashSet::new());
    }
    let sources = snapshot
        .sender_provenance
        .as_ref()
        .ok_or_else(|| MessageError::invalid_args("snapshot MMP sender provenance is required"))?;
    let canonical = snapshot
        .canonical_messages()
        .map(|row| (row.sequence, row))
        .collect::<HashMap<_, _>>();
    if sources.len() != canonical.len()
        || sources
            .windows(2)
            .any(|pair| pair[0].sequence >= pair[1].sequence)
    {
        return Err(MessageError::invalid_args(
            "snapshot MMP sender provenance must exactly cover ordered canonical sources",
        ));
    }
    let registered = snapshot
        .registered_agents
        .iter()
        .map(|id| id.agent_id.as_str())
        .collect::<HashSet<_>>();
    let retired = snapshot
        .retired_delivery_floors
        .iter()
        .map(|floor| (floor.identity.agent_id.as_str(), floor))
        .collect::<HashMap<_, _>>();
    let mut result = HashSet::new();
    for source in sources {
        let row = canonical.get(&source.sequence).ok_or_else(|| {
            MessageError::invalid_args(
                "snapshot MMP sender provenance references an unknown occurrence",
            )
        })?;
        // Canonical rows are preflighted by the existing auxiliary budget before
        // this function. Equality bounds provenance before any sender cloning.
        if source.sender_sha256 != identity_digest(&row.envelope.sender)
            || source.registered_after_sequence != row.sender_registered_after_sequence
        {
            return Err(MessageError::invalid_args(
                "snapshot MMP sender provenance disagrees with canonical source",
            ));
        }
        let address = row.envelope.sender.agent_id.as_str();
        if !registered.contains(address) && !retired.contains_key(address) {
            return Err(MessageError::invalid_args(
                "snapshot MMP sender provenance has no known address",
            ));
        }
        let fresh = !registered.contains(address)
            && retired
                .get(address)
                .is_some_and(|floor| floor.fresh_sender_versions.is_some());
        if source.fresh_retirement != fresh {
            return Err(MessageError::invalid_args(
                "snapshot MMP fresh sender provenance disagrees with retirement owner",
            ));
        }
        result.insert(source.sequence);
    }
    Ok(result)
}

/// Computes the versioned metadata digest without cloning or serializing strings.
/// Presence tags, byte lengths and vector cardinality preserve optional/empty
/// distinctions, field boundaries and capability order. Changing this encoding
/// requires a snapshot schema migration; it is not a live authentication token.
pub(super) fn identity_digest(identity: &MessageIdentitySnapshot) -> [u8; 32] {
    digest_fields(
        &identity.agent_id,
        identity.project_scope.as_deref(),
        identity.pane_id.as_deref(),
        identity.window_id.as_deref(),
        identity.role.as_deref(),
        &identity.capabilities,
        identity.objective.as_deref(),
    )
}

/// Hashes the original shared envelope identity using the same schema9 encoding.
fn live_identity_digest(identity: &SenderIdentity) -> [u8; 32] {
    digest_fields(
        identity.agent_id.as_str(),
        identity
            .project_scope
            .as_ref()
            .map(ProjectScopeId::snapshot_value),
        identity.pane_id.as_ref().map(PaneId::as_str),
        identity.window_id.as_ref().map(WindowId::as_str),
        identity.role.as_deref(),
        &identity.capabilities,
        identity.objective.as_deref(),
    )
}

/// Frames every historical identity field in a fixed order into SHA256.
fn digest_fields(
    agent: &str,
    project: Option<&str>,
    pane: Option<&str>,
    window: Option<&str>,
    role: Option<&str>,
    capabilities: &[String],
    objective: Option<&str>,
) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"mezzanine-mmp-sender-identity-v1");
    hash.update((agent.len() as u64).to_le_bytes());
    hash.update(agent.as_bytes());
    for field in [project, pane, window, role, objective] {
        hash.update([u8::from(field.is_some())]);
        if let Some(value) = field {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value.as_bytes());
        }
    }
    hash.update((capabilities.len() as u64).to_le_bytes());
    for capability in capabilities {
        hash.update((capability.len() as u64).to_le_bytes());
        hash.update(capability.as_bytes());
    }
    hash.finalize().into()
}

/// Recognizes only a provable schema5–8 redundant active retirement floor.
/// Validation and reconstruction omit that floor without cloning the snapshot.
/// The exact identity and registration high-water must already agree. Frozen
/// recipient intervals exclude old mail; unknown provenance is never invented.
pub(super) fn redundant_legacy_floor(
    snapshot: &MessageServiceSnapshot,
    floor: &MessageRetiredDeliveryFloorSnapshot,
) -> bool {
    (5..=8).contains(&snapshot.schema_version)
        && snapshot.sender_provenance.is_none()
        && snapshot
            .registered_agents
            .iter()
            .any(|identity| identity == &floor.identity)
        && snapshot.registration_floors.as_ref().is_some_and(|floors| {
            floors.iter().any(|binding| {
                binding.recipient == floor.identity.agent_id
                    && floor.last_sequence <= binding.registered_after_sequence
            })
        })
        && floor
            .fresh_sender_versions
            .as_ref()
            .is_none_or(|versions| versions.len() == 1 && versions[0] == floor.identity)
}
