//! Acceptance-time recipient registration validation and bounded metadata.
//!
//! Registration floors distinguish every interval that could own an occurrence.
//! Equal floors imply no accepted mail intervened. Persisted selector membership
//! is never used to invent a recipient set for older snapshots.

use super::*;

/// Hard aggregate bound on retained acceptance recipient records.
const MAX_RECIPIENT_BINDINGS: usize = 4096;
/// Conservative logical metadata bound, independently of payload retention.
const MAX_RECIPIENT_BINDING_BYTES: usize = 1024 * 1024;
/// Per-record charge covers the ID allocation, sequence and container overhead.
const RECIPIENT_BINDING_OVERHEAD: usize = 128;

/// Computes a conservative checked logical charge for immutable recipient IDs.
fn binding_bytes<'a>(ids: impl Iterator<Item = &'a str>) -> usize {
    ids.fold(0usize, |bytes, id| {
        bytes
            .saturating_add(RECIPIENT_BINDING_OVERHEAD)
            .saturating_add(id.len())
    })
}

impl MessageService {
    /// Resolves eligible registrations once, rejecting oversized fanout before
    /// cloning beyond the finite count/byte reservation. Sorting keeps captured
    /// snapshot bytes stable independently of registration hash-map order.
    pub(super) fn capture_recipient_bindings(
        &self,
        envelope: &Envelope,
        audience: &ResolvedMessageAudience,
    ) -> Result<Vec<(AgentId, MessageSequence)>> {
        let mut bindings = Vec::new();
        let mut bytes = 0usize;
        for identity in self.registered.values().filter(|identity| {
            self.recipient_is_available(&identity.agent_id)
                && recipient_matches(identity, &envelope.recipient)
                && audience_matches(identity, audience)
        }) {
            bytes =
                bytes.saturating_add(binding_bytes(std::iter::once(identity.agent_id.as_str())));
            if bindings.len() == MAX_RECIPIENT_BINDINGS || bytes > MAX_RECIPIENT_BINDING_BYTES {
                return Err(MessageError::new(
                    MessageErrorKind::Capacity,
                    "MMP recipient metadata capacity; message not accepted",
                ));
            }
            // Every registered identity is inserted with its registration floor;
            // metadata reconciliation never replaces that interval.
            bindings.push((
                identity.agent_id.clone(),
                self.registration_floors[&identity.agent_id],
            ));
        }
        bindings.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        Ok(bindings)
    }

    /// Reserves all recipient metadata before sequence allocation or mutation.
    /// Optional history that this enqueue already evicts does not consume the
    /// candidate budget. This remains replay storage, NOT pending custody.
    pub(super) fn reserve_recipient_bindings(
        &self,
        incoming: &[(AgentId, MessageSequence)],
        payload_bytes: usize,
    ) -> Result<()> {
        let mut retained_count = self.queue.len().saturating_add(1);
        let mut retained_bytes = self.queued_bytes.saturating_add(payload_bytes);
        let mut retained = self.queue.iter();
        while retained_count > self.retention_messages || retained_bytes > self.retention_bytes {
            let Some(evicted) = retained.next() else {
                break;
            };
            retained_count -= 1;
            retained_bytes = retained_bytes.saturating_sub(evicted.envelope.payload.len());
        }
        let mut count = incoming.len();
        let mut bytes = binding_bytes(incoming.iter().map(|(id, _)| id.as_str()));
        for queued in retained {
            count = count.saturating_add(queued.recipient_bindings.len());
            bytes = bytes.saturating_add(binding_bytes(
                queued.recipient_bindings.iter().map(|(id, _)| id.as_str()),
            ));
        }
        if count > MAX_RECIPIENT_BINDINGS || bytes > MAX_RECIPIENT_BINDING_BYTES {
            return Err(MessageError::new(
                MessageErrorKind::Capacity,
                "MMP recipient metadata capacity; message not accepted",
            ));
        }
        Ok(())
    }
}

/// Validates private registration intervals and occurrence-bound recipient sets.
/// Legacy snapshots without mail remain usable; retained legacy mail lacks the
/// evidence needed for live redelivery and is explicitly refused, not inferred.
pub(super) fn validate_recipient_bindings(
    snapshot: &MessageServiceSnapshot,
    registered: &HashMap<&str, &MessageIdentitySnapshot>,
) -> Result<()> {
    if snapshot.schema_version < 5 {
        if snapshot.registration_floors.is_some()
            || snapshot
                .retained_messages
                .iter()
                .any(|row| row.recipient_bindings.is_some())
        {
            return Err(MessageError::invalid_args(
                "MMP recipient bindings require schema version 5",
            ));
        }
        if snapshot.schema_version >= 2 && !snapshot.retained_messages.is_empty() {
            return Err(MessageError::invalid_state(
                "legacy MMP acceptance-time recipients are unknown; retained mail cannot be safely rebound",
            ));
        }
        return Ok(());
    }
    let floors = snapshot.registration_floors.as_ref().ok_or_else(|| {
        MessageError::invalid_args("snapshot MMP registration floors are required")
    })?;
    let mut by_id = HashMap::new();
    for binding in floors {
        parse_opaque_id(&binding.recipient, "MMP recipient registration")?;
        if !registered.contains_key(binding.recipient.as_str())
            || binding.registered_after_sequence >= snapshot.next_sequence
            || by_id
                .insert(
                    binding.recipient.as_str(),
                    binding.registered_after_sequence,
                )
                .is_some()
        {
            return Err(MessageError::invalid_args(
                "snapshot MMP registration intervals are invalid",
            ));
        }
    }
    if by_id.len() != registered.len() {
        return Err(MessageError::invalid_args(
            "snapshot MMP registration intervals are incomplete",
        ));
    }
    let mut binding_count = 0usize;
    let mut total_bytes = 0usize;
    let accepted_counts = snapshot
        .accepted_messages
        .iter()
        .map(|accepted| {
            (
                accepted.envelope.id.as_str(),
                accepted.delivery.queued_recipients,
            )
        })
        .collect::<HashMap<_, _>>();
    for row in &snapshot.retained_messages {
        let bindings = row.recipient_bindings.as_ref().ok_or_else(|| {
            MessageError::invalid_args("snapshot MMP accepted recipients are required")
        })?;
        let mut ids = HashSet::new();
        binding_count = binding_count.saturating_add(bindings.len());
        total_bytes = total_bytes.saturating_add(binding_bytes(
            bindings.iter().map(|binding| binding.recipient.as_str()),
        ));
        if binding_count > MAX_RECIPIENT_BINDINGS || total_bytes > MAX_RECIPIENT_BINDING_BYTES {
            return Err(MessageError::invalid_args(
                "snapshot MMP recipient metadata exceeds capacity",
            ));
        }
        if bindings.is_empty() {
            return Err(MessageError::invalid_args(
                "snapshot MMP accepted recipients must be nonempty",
            ));
        }
        for binding in bindings {
            parse_opaque_id(&binding.recipient, "MMP accepted recipient")?;
            if binding.registered_after_sequence >= row.sequence
                || !ids.insert(&binding.recipient)
                || by_id
                    .get(binding.recipient.as_str())
                    .is_some_and(|floor| *floor < binding.registered_after_sequence)
            {
                return Err(MessageError::invalid_args(
                    "snapshot MMP accepted recipient interval is invalid",
                ));
            }
        }
        if accepted_counts.get(row.envelope.id.as_str()) != Some(&bindings.len()) {
            return Err(MessageError::invalid_args(
                "snapshot MMP accepted recipient count disagrees",
            ));
        }
    }
    Ok(())
}
