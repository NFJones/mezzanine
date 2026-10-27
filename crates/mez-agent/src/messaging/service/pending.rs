//! Same-runtime accepted-delivery custody independent of disposable replay.
//!
//! One bounded index shares immutable occurrences with replay and its delivery
//! indexes. Exact offered ACK evidence belongs to the occurrence, not mutable
//! subscription lifetime. Expired/retired unsatisfied rows remain conservatively
//! retained until a typed terminal-outcome owner is implemented; this module
//! never mistakes unavailability or expiry for successful recipient commit.

use super::*;

/// Maximum independently pending payload occurrences.
const MAX_PENDING_MESSAGES: usize = 1000;
/// Maximum unique pending payload bytes; fanout never multiplies this charge.
const MAX_PENDING_BYTES: usize = 1024 * 1024;
/// Maximum frozen obligations while their occurrence remains pending.
const MAX_PENDING_OBLIGATIONS: usize = 4096;
/// Maximum pending obligations belonging to any exact recipient interval.
const MAX_PENDING_PER_RECIPIENT: usize = 256;

impl MessageService {
    /// Checks exact occurrence commit evidence without inferring it from a
    /// mutable cursor. Poison preserves custody and fails closed for selection.
    pub(super) fn recipient_has_committed(
        &self,
        queued: &QueuedEnvelope,
        recipient: &AgentId,
    ) -> bool {
        queued
            .recipient_bindings
            .iter()
            .position(|(id, _)| id == recipient)
            .and_then(|index| {
                queued
                    .acknowledged
                    .lock()
                    .ok()
                    .and_then(|slots| slots.get(index).copied())
            })
            .unwrap_or(false)
    }

    /// Reserves all pending resources before sequence or acceptance mutation.
    /// Conservative metadata charging includes already committed fanout slots
    /// until the last recipient releases their shared occurrence.
    pub(super) fn reserve_pending_delivery(
        &self,
        incoming: &[(AgentId, MessageSequence)],
        payload_bytes: usize,
    ) -> Result<()> {
        let mut bytes = payload_bytes;
        let mut obligations = incoming.len();
        let mut per_recipient = HashMap::new();
        for binding in incoming {
            *per_recipient.entry(binding).or_insert(0usize) += 1;
        }
        for queued in self.pending.values() {
            bytes = bytes.saturating_add(queued.envelope.payload.len());
            obligations = obligations.saturating_add(queued.recipient_bindings.len());
            for binding in &queued.recipient_bindings {
                *per_recipient.entry(binding).or_insert(0usize) += 1;
            }
        }
        if self.pending.len() >= MAX_PENDING_MESSAGES
            || bytes > MAX_PENDING_BYTES
            || obligations > MAX_PENDING_OBLIGATIONS
            || per_recipient
                .values()
                .any(|count| *count > MAX_PENDING_PER_RECIPIENT)
        {
            return Err(MessageError::new(
                MessageErrorKind::Capacity,
                "MMP pending delivery capacity; message not accepted",
            ));
        }
        Ok(())
    }

    /// Records commits and expiry exclusions only in the prevalidated cursor
    /// advancement interval. Earlier exclusions remain unchanged even when the
    /// new endpoint was offered before the prior ACK. Poison fails closed.
    pub(super) fn commit_pending_ack(
        &mut self,
        recipient: &AgentId,
        after: MessageSequence,
        sequence: MessageSequence,
    ) -> Result<()> {
        // Acquire and validate every slot before mutating any commit evidence.
        let endpoint = self
            .queued_by_sequence
            .get(&sequence)
            .ok_or_else(|| MessageError::invalid_state("MMP ACK endpoint is unavailable"))?;
        let observed_at_ms = super::offers::offered_at(endpoint, recipient)?
            .ok_or_else(|| MessageError::invalid_state("MMP ACK endpoint was not offered"))?;
        let mut slots = Vec::new();
        let mut expirations = Vec::new();
        for queued in self
            .pending
            .range((
                std::ops::Bound::Excluded(after),
                std::ops::Bound::Included(sequence),
            ))
            .map(|(_, row)| row)
        {
            if !self.accepted_recipient(queued, recipient) {
                continue;
            }
            let index = queued
                .recipient_bindings
                .iter()
                .position(|(id, _)| id == recipient)
                .ok_or_else(|| {
                    MessageError::invalid_state("MMP pending recipient is unavailable")
                })?;
            let offered = queued
                .offered_at_ms
                .lock()
                .map_err(|_| MessageError::invalid_state("MMP offer evidence is unavailable"))?;
            if offered.get(index).copied().flatten().is_none() {
                if !expired(queued, observed_at_ms) {
                    return Err(MessageError::invalid_state(
                        "MMP unoffered prefix is not expired",
                    ));
                }
                expirations.push(queued.ack_expiry_observed_at_ms.lock().map_err(|_| {
                    MessageError::invalid_state("MMP ACK expiry evidence is unavailable")
                })?);
                continue;
            }
            let acknowledged = queued
                .acknowledged
                .lock()
                .map_err(|_| MessageError::invalid_state("MMP commit evidence is unavailable"))?;
            if index >= acknowledged.len() {
                return Err(MessageError::invalid_state(
                    "MMP commit slot is unavailable",
                ));
            }
            slots.push((acknowledged, index));
        }
        for (mut acknowledged, index) in slots {
            acknowledged[index] = true;
        }
        for mut expiration in expirations {
            *expiration =
                Some(expiration.map_or(observed_at_ms, |prior| prior.max(observed_at_ms)));
        }
        let completed = self
            .pending
            .iter()
            .filter_map(|(sequence, queued)| {
                queued
                    .acknowledged
                    .lock()
                    .ok()
                    .filter(|slots| slots.iter().all(|ack| *ack))
                    .map(|_| *sequence)
            })
            .collect::<Vec<_>>();
        for sequence in completed {
            if let Some(queued) = self.pending.remove(&sequence)
                && !self.queue.iter().any(|row| row.sequence == sequence)
            {
                self.remove_queue_indexes(&queued);
            }
        }
        self.prune_accepted_messages_to_retained_queue();
        self.prune_retired_delivery_floors();
        Ok(())
    }
}

/// Validates finite custody and explicit commit evidence before restoration.
pub(super) fn validate_pending_snapshot(snapshot: &MessageServiceSnapshot) -> Result<()> {
    if snapshot.schema_version < 7 {
        if snapshot.custody_messages.is_some()
            || snapshot.canonical_messages().any(|row| {
                row.acknowledged_recipients.is_some() || row.ack_expiry_observed_at_ms.is_some()
            })
        {
            return Err(MessageError::invalid_args(
                "MMP custody requires schema version 7",
            ));
        }
        if matches!(snapshot.schema_version, 5 | 6) {
            for row in snapshot.canonical_messages() {
                for binding in row.recipient_bindings.as_deref().unwrap_or(&[]) {
                    if exact_cursor(snapshot, binding).is_some_and(|cursor| cursor >= row.sequence)
                        && !row
                            .offered_recipients
                            .as_deref()
                            .unwrap_or(&[])
                            .iter()
                            .any(|offer| offer.recipient == binding.recipient)
                    {
                        return Err(MessageError::invalid_state(
                            "legacy MMP cursor has unknown commit or expiry evidence; custody restoration refused",
                        ));
                    }
                }
            }
        }
        return Ok(());
    }
    let extra = snapshot
        .custody_messages
        .as_ref()
        .ok_or_else(|| MessageError::invalid_args("snapshot MMP custody sources are required"))?;
    if snapshot
        .retained_messages
        .windows(2)
        .any(|rows| rows[0].sequence >= rows[1].sequence)
        || extra
            .windows(2)
            .any(|rows| rows[0].sequence >= rows[1].sequence)
    {
        return Err(MessageError::invalid_args(
            "snapshot MMP source order is not canonical",
        ));
    }
    let mut messages = 0usize;
    let mut bytes = 0usize;
    let mut obligations = 0usize;
    let mut per_recipient = HashMap::new();
    for row in snapshot.canonical_messages() {
        let acknowledged = row.acknowledged_recipients.as_ref().ok_or_else(|| {
            MessageError::invalid_args("snapshot MMP recipient commits are required")
        })?;
        let bindings = row.recipient_bindings.as_deref().unwrap_or(&[]);
        let offers = row.offered_recipients.as_deref().unwrap_or(&[]);
        if row.ack_expiry_observed_at_ms.is_some_and(|observed| {
            !row.envelope
                .ttl_ms
                .is_some_and(|ttl| row.accepted_at_ms.saturating_add(ttl) < observed)
        }) {
            return Err(MessageError::invalid_args(
                "snapshot MMP ACK expiry evidence is invalid",
            ));
        }
        for binding in bindings {
            if exact_cursor(snapshot, binding).is_some_and(|cursor| cursor >= row.sequence)
                && !acknowledged.contains(&binding.recipient)
                && (row.ack_expiry_observed_at_ms.is_none()
                    || offers
                        .iter()
                        .any(|offer| offer.recipient == binding.recipient))
            {
                return Err(MessageError::invalid_args(
                    "snapshot MMP cursor conceals live uncommitted custody",
                ));
            }
        }
        let mut seen = HashSet::new();
        for id in acknowledged {
            if !seen.insert(id)
                || !bindings.iter().any(|binding| binding.recipient == *id)
                || !offers.iter().any(|offer| offer.recipient == *id)
            {
                return Err(MessageError::invalid_args(
                    "snapshot MMP recipient commit lacks exact offer evidence",
                ));
            }
            let binding = bindings.iter().find(|binding| binding.recipient == *id);
            if binding.is_some_and(|binding| {
                snapshot
                    .registration_floors
                    .as_deref()
                    .unwrap_or(&[])
                    .contains(binding)
            }) && !snapshot
                .subscriptions
                .iter()
                .any(|cursor| cursor.recipient == *id && cursor.last_sequence >= row.sequence)
            {
                return Err(MessageError::invalid_args(
                    "snapshot MMP recipient commit exceeds its exact cursor",
                ));
            }
        }
        if acknowledged.len() == bindings.len() {
            if extra.iter().any(|extra| extra.sequence == row.sequence) {
                return Err(MessageError::invalid_args(
                    "snapshot MMP custody-only source has no pending recipient",
                ));
            }
            continue;
        }
        messages += 1;
        bytes = bytes.saturating_add(row.envelope.payload.len());
        obligations = obligations.saturating_add(bindings.len());
        for binding in bindings {
            *per_recipient
                .entry((&binding.recipient, binding.registered_after_sequence))
                .or_insert(0usize) += 1;
        }
    }
    if messages > MAX_PENDING_MESSAGES
        || bytes > MAX_PENDING_BYTES
        || obligations > MAX_PENDING_OBLIGATIONS
        || per_recipient
            .values()
            .any(|count| *count > MAX_PENDING_PER_RECIPIENT)
    {
        return Err(MessageError::invalid_args(
            "snapshot MMP pending custody exceeds capacity",
        ));
    }
    Ok(())
}

/// Returns a cursor only when its registration interval is still exactly the
/// acceptance-time owner. Reused IDs cannot prove old commits or prefix skips.
fn exact_cursor(
    snapshot: &MessageServiceSnapshot,
    binding: &MessageRecipientBindingSnapshot,
) -> Option<MessageSequence> {
    snapshot
        .registration_floors
        .as_deref()
        .unwrap_or(&[])
        .contains(binding)
        .then(|| {
            snapshot
                .subscriptions
                .iter()
                .find(|cursor| cursor.recipient == binding.recipient)
                .map(|cursor| cursor.last_sequence)
        })
        .flatten()
}
