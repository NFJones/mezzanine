//! Payload-free evidence of one authenticated live acceptance occurrence.
//!
//! Trusted adapters capture this evidence before releasing delivery custody.
//! It freezes the original sender and eligible recipient registration intervals
//! without keeping a second payload mailbox or consulting mutable selectors.
//! The opaque runtime token prevents serialized history, another service, or a
//! restored session from recreating live acceptance authority. Callers own the
//! finite reservation and consumption policy for retained proofs; these read
//! APIs neither allocate subscriptions nor acknowledge or offer any mail.

use super::*;

/// Immutable same-runtime acceptance evidence for trusted local adapters.
///
/// This value is deliberately neither serializable nor caller-constructible.
/// It proves an occurrence and frozen audience, not delivery, answer quality,
/// child-task ownership, or permission to schedule a provider continuation.
#[derive(Debug, Clone)]
pub struct MessageAcceptanceProof {
    /// Identity of the live service that actually accepted the occurrence.
    runtime: Arc<()>,
    /// Original occurrence identity, independent of disposable payload replay.
    message_id: String,
    /// Authenticated sender address and exact original registration interval.
    sender: (AgentId, MessageSequence),
    /// Eligible registrations captured by the original acceptance owner.
    recipients: Vec<(AgentId, MessageSequence)>,
    /// Untrusted correlation text, compared only after sender authentication.
    correlation_id: Option<String>,
    /// Sequence orders occurrences within this one live acceptance owner.
    sequence: MessageSequence,
}

impl MessageAcceptanceProof {
    /// Returns the actual accepted message ID, rather than a provider action ID.
    pub fn message_id(&self) -> &str {
        &self.message_id
    }

    /// Returns the accepted occurrence sequence within this proof's runtime.
    pub fn sequence(&self) -> MessageSequence {
        self.sequence
    }

    /// Iterates only acceptance-time eligible recipient addresses in stable order.
    pub fn recipients(&self) -> impl Iterator<Item = &AgentId> {
        self.recipients.iter().map(|(id, _)| id)
    }

    /// Reports a conservative retained metadata charge, including vector slots,
    /// string capacities and the shared runtime allocation. Payload is absent.
    /// Adapters must reserve this metadata in addition to their own outcome slots.
    pub fn metadata_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(2 * std::mem::size_of::<usize>())
            .saturating_add(self.message_id.capacity())
            .saturating_add(self.sender.0.as_str().len())
            .saturating_add(self.correlation_id.as_ref().map_or(0, String::capacity))
            .saturating_add(
                self.recipients
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(AgentId, MessageSequence)>()),
            )
            .saturating_add(self.recipients.iter().fold(0usize, |bytes, (id, _)| {
                bytes.saturating_add(id.as_str().len())
            }))
    }
}

impl MessageService {
    /// Accepts a fresh occurrence and returns its opaque proof in one transaction.
    /// The trusted adapter supplies already reserved metadata headroom. Capacity
    /// or validation failure changes no acceptance state. Existing message IDs
    /// return Conflict; retries must use the adapter's original tracked receipt,
    /// never retroactively upgrade an ordinary send. This enforces only proof
    /// bytes; adapters must also reserve request, recipient and outcome counts.
    pub fn accept_at_with_scope_and_proof(
        &mut self,
        sender: &AgentId,
        envelope: Envelope,
        scope: MessageScope,
        now_ms: u64,
        metadata_budget: usize,
    ) -> Result<(Delivery, MessageAcceptanceProof)> {
        let (delivery, proof) = self.accept_with_optional_proof(
            sender,
            envelope,
            scope,
            now_ms,
            Some(metadata_budget),
        )?;
        // The selected transaction constructs proof before enqueue and has no
        // successful duplicate path. Absence indicates a programming error.
        let proof = proof.ok_or_else(|| {
            MessageError::invalid_state("tracked acceptance proof was not reserved")
        })?;
        Ok((delivery, proof))
    }

    /// Measures immutable metadata before cloning or accepting a tracked source.
    /// Recipient bindings already obey the lower fanout count/byte ceilings.
    /// The candidate uses next_sequence, which enqueue allocates in this same
    /// exclusive transaction; enqueue failure drops the reservation unchanged.
    pub(super) fn reserve_acceptance_proof(
        &self,
        envelope: &Envelope,
        recipients: &[(AgentId, MessageSequence)],
        budget: usize,
    ) -> Result<MessageAcceptanceProof> {
        let bytes = std::mem::size_of::<MessageAcceptanceProof>()
            .saturating_add(2 * std::mem::size_of::<usize>())
            .saturating_add(envelope.id.len())
            .saturating_add(envelope.sender.agent_id.as_str().len())
            .saturating_add(envelope.correlation_id.as_ref().map_or(0, String::len))
            .saturating_add(std::mem::size_of_val(recipients))
            .saturating_add(recipients.iter().fold(0usize, |bytes, (id, _)| {
                bytes.saturating_add(id.as_str().len())
            }));
        if bytes > budget {
            return Err(MessageError::new(
                MessageErrorKind::Capacity,
                "tracked acceptance metadata capacity exceeded",
            ));
        }
        let floor = self
            .registration_floors
            .get(&envelope.sender.agent_id)
            .copied()
            .ok_or_else(|| {
                MessageError::invalid_state("sender registration evidence unavailable")
            })?;
        Ok(MessageAcceptanceProof {
            runtime: self.acceptance_runtime.clone(),
            message_id: envelope.id.clone(),
            sender: (envelope.sender.agent_id.clone(), floor),
            recipients: recipients.to_vec(),
            correlation_id: envelope.correlation_id.clone(),
            sequence: self.next_sequence,
        })
    }

    /// Captures bounded payload-free evidence while the canonical occurrence is
    /// still pending under its exact original sender. Call before ACK/release.
    /// Foreign, retired, restored-history and unavailable occurrences yield None.
    /// A caller must reserve request metadata before accepting tracked work; this
    /// extraction itself does not create a tracking promise or retain a payload.
    pub fn acceptance_proof(
        &self,
        sender: &AgentId,
        sequence: MessageSequence,
    ) -> Option<MessageAcceptanceProof> {
        if sequence <= self.acceptance_history_through {
            return None;
        }
        let row = self.pending.get(&sequence)?;
        let floor = row.sender_registered_after_sequence?;
        if row.envelope.sender.agent_id != *sender
            || self.registration_floors.get(sender) != Some(&floor)
        {
            return None;
        }
        Some(MessageAcceptanceProof {
            runtime: self.acceptance_runtime.clone(),
            message_id: row.envelope.id.clone(),
            sender: (sender.clone(), floor),
            recipients: row.recipient_bindings.clone(),
            correlation_id: row.envelope.correlation_id.clone(),
            sequence,
        })
    }

    /// Checks whether a frozen recipient registration is still current.
    /// Offline presence and sparse metadata updates do not end a registration;
    /// retirement, same-ID replacement and a different runtime return false.
    /// This is availability evidence only and reveals no private task state.
    pub fn acceptance_recipient_is_current(
        &self,
        proof: &MessageAcceptanceProof,
        recipient: &AgentId,
    ) -> bool {
        Arc::ptr_eq(&self.acceptance_runtime, &proof.runtime)
            && proof.recipients.iter().any(|(id, floor)| {
                id == recipient && self.registration_floors.get(id) == Some(floor)
            })
    }

    /// Authenticates one later reverse occurrence against a frozen request.
    /// The reply must originate in the selected recipient's original interval,
    /// target the original sender's interval, and correlate to the accepted ID.
    /// No current discovery, payload text or guessed task identity participates.
    /// Later retirement does not erase a genuinely captured earlier response;
    /// product owners separately gate current requester/conversation liveness.
    pub fn acceptance_reply_matches(
        &self,
        request: &MessageAcceptanceProof,
        recipient: &AgentId,
        reply: &MessageAcceptanceProof,
    ) -> bool {
        Arc::ptr_eq(&self.acceptance_runtime, &request.runtime)
            && Arc::ptr_eq(&self.acceptance_runtime, &reply.runtime)
            && reply.sequence > request.sequence
            && reply.correlation_id.as_deref() == Some(request.message_id.as_str())
            && reply.sender.0 == *recipient
            && request.recipients.contains(&reply.sender)
            && reply.recipients.contains(&request.sender)
    }
}
