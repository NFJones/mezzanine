//! Logical byte reservations for immutable occurrence metadata and outcome copies.
//!
//! Payload and frozen recipient records have independent owners. This module
//! charges all remaining envelope strings, empty variable-length entries, and
//! conservative container/index overhead over the shared replay/custody union.
//! Charges use saturating arithmetic without cloning or serializing input. They
//! bound logical retained content rather than allocator-specific capacity/RSS.
//! Outcome record fit is reserved before acceptance so byte-limited queries can
//! always make progress. No reservation evicts pending delivery obligations.

use super::*;

/// A query can copy at most this many logical outcome bytes, independently of
/// its record limit. Acceptance requires every individual record to fit.
pub(super) const OUTCOME_BYTES: usize = 64 * 1024;

/// Charges a result's two owned strings plus fixed record/container overhead.
pub(super) fn outcome_bytes(id: &str, recipient: &str) -> usize {
    128usize
        .saturating_add(id.len())
        .saturating_add(recipient.len())
}

/// Charges string content and conservative space for its owned copies/indexes.
fn string_bytes(value: &str) -> usize {
    64usize.saturating_add(value.len().saturating_mul(4))
}

/// Charges fixed fields plus every variable entry, including empty strings.
fn parts_bytes<'a>(
    fixed: impl IntoIterator<Item = &'a str>,
    capabilities: &'a [String],
    extensions: impl Iterator<Item = (&'a str, &'a str)>,
) -> usize {
    fixed
        .into_iter()
        .chain(capabilities.iter().map(String::as_str))
        .chain(extensions.flat_map(|(key, value)| [key, value]))
        .fold(1024usize, |sum, value| {
            sum.saturating_add(string_bytes(value))
        })
}

/// Borrows the selector's kind and value without allocating wire projections.
fn recipient_parts(recipient: &Recipient) -> (&str, &str) {
    match recipient {
        Recipient::Agent(id) => ("agent", id.as_str()),
        Recipient::Pane(id) => ("pane", id.as_str()),
        Recipient::Window(id) => ("window", id.as_str()),
        Recipient::Session => ("session", ""),
        Recipient::Role(value) => ("role", value),
        Recipient::Capability(value) => ("capability", value),
        Recipient::Group(value) => ("group", value),
    }
}

/// Computes the immutable logical charge shared by replay and pending owners.
pub(super) fn envelope_bytes(envelope: &Envelope) -> usize {
    let sender = &envelope.sender;
    let (kind, value) = recipient_parts(&envelope.recipient);
    parts_bytes(
        [
            envelope.protocol,
            &envelope.id,
            &envelope.message_type,
            &envelope.time,
            &envelope.content_type,
            envelope.correlation_id.as_deref().unwrap_or(""),
            sender.agent_id.as_str(),
            sender.pane_id.as_ref().map_or("", PaneId::as_str),
            sender.window_id.as_ref().map_or("", WindowId::as_str),
            sender.role.as_deref().unwrap_or(""),
            sender.objective.as_deref().unwrap_or(""),
            sender
                .project_scope
                .as_ref()
                .map_or("", ProjectScopeId::snapshot_value),
            kind,
            value,
        ],
        &sender.capabilities,
        envelope
            .extension_fields
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    )
}

/// Applies exactly the same logical charge to borrowed snapshot fields.
fn snapshot_envelope_bytes(envelope: &MessageEnvelopeSnapshot) -> usize {
    let sender = &envelope.sender;
    parts_bytes(
        [
            envelope.protocol.as_str(),
            &envelope.id,
            &envelope.message_type,
            &envelope.time,
            &envelope.content_type,
            envelope.correlation_id.as_deref().unwrap_or(""),
            &sender.agent_id,
            sender.pane_id.as_deref().unwrap_or(""),
            sender.window_id.as_deref().unwrap_or(""),
            sender.role.as_deref().unwrap_or(""),
            sender.objective.as_deref().unwrap_or(""),
            sender.project_scope.as_deref().unwrap_or(""),
            &envelope.recipient.kind,
            envelope.recipient.value.as_deref().unwrap_or(""),
        ],
        &sender.capabilities,
        envelope
            .extension_fields
            .iter()
            .map(|field| (field.key.as_str(), field.value_json.as_str())),
    )
}

impl MessageService {
    /// Reserves metadata after the normal replay eviction candidate is known,
    /// retaining all custody-only occurrences. Rejection changes no state.
    pub(super) fn reserve_auxiliary_delivery(
        &self,
        envelope: &Envelope,
        bindings: &[(AgentId, MessageSequence)],
    ) -> Result<()> {
        let mut retained_count = self.queue.len().saturating_add(1);
        let mut payload_bytes = self.queued_bytes.saturating_add(envelope.payload.len());
        let mut retained = self.queue.iter();
        while retained_count > self.retention_messages || payload_bytes > self.retention_bytes {
            let Some(evicted) = retained.next() else {
                break;
            };
            retained_count -= 1;
            payload_bytes = payload_bytes.saturating_sub(evicted.envelope.payload.len());
        }
        let mut bytes = envelope_bytes(envelope);
        let mut seen = HashSet::new();
        for row in retained.chain(self.pending.values()) {
            if seen.insert(row.sequence) {
                bytes = bytes.saturating_add(envelope_bytes(&row.envelope));
            }
        }
        if bytes > self.pending_limits.auxiliary_bytes
            || bindings
                .iter()
                .any(|(id, _)| outcome_bytes(&envelope.id, id.as_str()) > OUTCOME_BYTES)
        {
            return Err(MessageError::new(
                MessageErrorKind::Capacity,
                "MMP auxiliary metadata capacity; message not accepted",
            ));
        }
        Ok(())
    }
}

/// Validates both canonical sources and their accepted-envelope projections
/// before reconstruction clones anything. Matching projections are subsequently
/// checked by the ordinary snapshot validator; no snapshot selects its policy.
pub(super) fn validate_auxiliary_snapshot(
    snapshot: &MessageServiceSnapshot,
    limits: PendingDeliveryLimits,
) -> Result<()> {
    let mut bytes = 0usize;
    for row in snapshot.canonical_messages() {
        bytes = bytes.saturating_add(snapshot_envelope_bytes(&row.envelope));
        if bytes > limits.auxiliary_bytes
            || row
                .recipient_bindings
                .iter()
                .flatten()
                .any(|binding| outcome_bytes(&row.envelope.id, &binding.recipient) > OUTCOME_BYTES)
        {
            return Err(MessageError::invalid_args(
                "snapshot MMP auxiliary metadata exceeds capacity",
            ));
        }
    }
    let accepted_bytes = snapshot
        .accepted_messages
        .iter()
        .fold(0usize, |bytes, accepted| {
            bytes.saturating_add(snapshot_envelope_bytes(&accepted.envelope))
        });
    if accepted_bytes > limits.auxiliary_bytes {
        return Err(MessageError::invalid_args(
            "snapshot MMP acceptance metadata exceeds capacity",
        ));
    }
    Ok(())
}
