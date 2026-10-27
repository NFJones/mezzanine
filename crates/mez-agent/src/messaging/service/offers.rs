//! Exact selected-occurrence evidence and cumulative acknowledgement authority.
//!
//! Each retained envelope owns at most one offer record per frozen recipient.
//! The immutable binding supplies its registration interval; mutable presence,
//! selectors, arbitrary cursors and global sequence gaps cannot create rights.
//! Shared receive APIs record selections through a narrow per-occurrence mutex.
//! Poison fails closed, retaining payload rather than authorizing a cursor jump.

use super::*;

impl MessageService {
    /// Records only actual sequenced live selections for their exact recipient.
    /// Repeated selection updates its clock but allocates no additional record.
    pub(super) fn record_delivery_offers(&self, batch: &DeliveryBatch, now_ms: u64) -> Result<()> {
        for message in &batch.messages {
            let queued = self
                .queued_by_sequence
                .get(&message.sequence)
                .ok_or_else(|| {
                    MessageError::invalid_state("MMP offered occurrence is no longer retained")
                })?;
            if !self.accepted_recipient(queued, &batch.cursor.recipient) {
                return Err(MessageError::forbidden(
                    "MMP offer recipient interval changed",
                ));
            }
            let index = queued
                .recipient_bindings
                .iter()
                .position(|(id, _)| id == &batch.cursor.recipient)
                .ok_or_else(|| MessageError::forbidden("MMP offer recipient is not accepted"))?;
            let mut offers = queued
                .offered_at_ms
                .lock()
                .map_err(|_| MessageError::invalid_state("MMP offer evidence is unavailable"))?;
            let slot = offers.get_mut(index).ok_or_else(|| {
                MessageError::invalid_state("MMP offer reservation is unavailable")
            })?;
            // A later clock rollback cannot invalidate an earlier genuine
            // selection's evidence that a preceding occurrence had expired.
            *slot = Some(slot.map_or(now_ms, |previous| previous.max(now_ms)));
        }
        Ok(())
    }

    /// Validates the exact endpoint and every earlier live own retained row
    /// before ANY cursor mutation. Foreign global gaps are harmless. Expiry at
    /// the endpoint's genuine selection time cannot strand a later valid ACK.
    pub(super) fn validate_delivery_ack(
        &self,
        recipient: &AgentId,
        after: MessageSequence,
        sequence: MessageSequence,
    ) -> Result<()> {
        let invalid = || {
            MessageError::invalid_args(
                "delivery ACK requires an offered own occurrence and complete own prefix",
            )
        };
        let endpoint = self.queued_by_sequence.get(&sequence).ok_or_else(invalid)?;
        if !self.accepted_recipient(endpoint, recipient)
            || self.recipient_is_terminal(endpoint, recipient)
        {
            return Err(invalid());
        }
        let observed_at_ms = offered_at(endpoint, recipient)?.ok_or_else(invalid)?;
        for (_, queued) in self.queued_by_sequence.range((
            std::ops::Bound::Excluded(after),
            std::ops::Bound::Included(sequence),
        )) {
            if self.accepted_recipient(queued, recipient)
                && !self.recipient_is_terminal(queued, recipient)
                && !expired(queued, observed_at_ms)
                && offered_at(queued, recipient)?.is_none()
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

/// Reads the pre-reserved slot belonging to one immutable recipient binding.
pub(super) fn offered_at(queued: &QueuedEnvelope, recipient: &AgentId) -> Result<Option<u64>> {
    let Some(index) = queued
        .recipient_bindings
        .iter()
        .position(|(id, _)| id == recipient)
    else {
        return Ok(None);
    };
    Ok(queued
        .offered_at_ms
        .lock()
        .map_err(|_| MessageError::invalid_state("MMP offer evidence is unavailable"))?
        .get(index)
        .copied()
        .flatten())
}

/// Restores slots in immutable binding order after cross-record validation.
pub(super) fn offers_from_snapshot(row: &MessageQueuedEnvelopeSnapshot) -> Vec<Option<u64>> {
    let offers = row
        .offered_recipients
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|offer| (offer.recipient.as_str(), offer.observed_at_ms))
        .collect::<HashMap<_, _>>();
    row.recipient_bindings
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|binding| offers.get(binding.recipient.as_str()).copied())
        .collect()
}

/// Rejects missing, duplicate, foreign or already-expired claimed offers.
/// Schema5 has provable recipients but no offer evidence: require re-selection.
pub(super) fn validate_delivery_offer_snapshot(snapshot: &MessageServiceSnapshot) -> Result<()> {
    for row in snapshot.canonical_messages() {
        if snapshot.schema_version < 6 {
            if row.offered_recipients.is_some() {
                return Err(MessageError::invalid_args(
                    "MMP offer evidence requires schema version 6",
                ));
            }
            continue;
        }
        let offers = row.offered_recipients.as_ref().ok_or_else(|| {
            MessageError::invalid_args("snapshot MMP offered recipient evidence is required")
        })?;
        let bindings = row.recipient_bindings.as_deref().unwrap_or(&[]);
        if offers.len() > bindings.len() {
            return Err(MessageError::invalid_args(
                "snapshot MMP offers exceed accepted recipients",
            ));
        }
        let accepted = bindings
            .iter()
            .map(|binding| binding.recipient.as_str())
            .collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        for offer in offers {
            if !accepted.contains(offer.recipient.as_str())
                || !seen.insert(&offer.recipient)
                || row.envelope.ttl_ms.is_some_and(|ttl| {
                    row.accepted_at_ms.saturating_add(ttl) < offer.observed_at_ms
                })
            {
                return Err(MessageError::invalid_args(
                    "snapshot MMP offered recipient evidence is invalid",
                ));
            }
        }
    }
    Ok(())
}
