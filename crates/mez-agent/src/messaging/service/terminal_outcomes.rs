//! Bounded terminal custody on the canonical accepted occurrence.
//!
//! Acceptance reserves one fixed-size slot per recipient. Maintenance observes
//! strict TTL expiry or an ended exact registration interval, never presence
//! liveness. First observation wins. The original sender can consume a bounded,
//! payload-free receipt; replacement registrations cannot inherit it. Unsatisfied
//! source remains charged until every slot commits or its terminal receipt is
//! consumed/ages out. Optional replay may retain the same occurrence afterward.
//! No terminal transition writes an ACK, emits mail or schedules model work.

use super::*;

/// Ten-minute opportunity for exact-owner handoff after first observation.
/// Actor callers drive maintenance; elapsed wall-clock high-water is not a
/// promise of monotonic elapsed time or crash-atomic persistence.
const TERMINAL_HANDOFF_MS: u64 = 600_000;
/// One query cannot clone more than this many payload-free receipt records.
const OUTCOME_BATCH: usize = 100;

impl MessageService {
    /// Borrows the payload-free ID of an occurrence still in pending custody
    /// under this exact original currently registered sender interval. Trusted
    /// adapters can bound same-runtime metadata without cloning envelope data.
    /// This read-only view grants no delivery or acknowledgement authority.
    pub fn owned_pending_delivery_id(
        &self,
        sender: &AgentId,
        sequence: MessageSequence,
    ) -> Option<&str> {
        self.pending
            .get(&sequence)
            .filter(|queued| self.owns_outcomes(queued, sender))
            .map(|queued| queued.envelope.id.as_str())
    }

    /// Reports whether finite actor maintenance remains necessary. Future TTL
    /// boundaries, exact recipient retirement and unconsumed terminal horizons
    /// keep maintenance eligible; ordinary offline mail without TTL does not.
    /// Released slots and positive ACKs cannot keep an idle timer alive.
    /// Poisoned reservation evidence returns InvalidState without mutation.
    pub fn needs_delivery_outcome_maintenance(&self) -> Result<bool> {
        self.needs_delivery_outcome_maintenance_with_native_commits(&[])
    }

    /// Reports finite terminal work after excluding exact proven native commits
    /// waiting on a cursor prefix. Such canonical input must not create a
    /// permanent TTL timer. The adapter separately schedules reconcilable ACKs.
    pub fn needs_delivery_outcome_maintenance_with_native_commits(
        &self,
        native_commits: &[(AgentId, MessageSequence)],
    ) -> Result<bool> {
        let protected = self.native_commit_protection(native_commits)?;
        for queued in self.pending.values() {
            let acknowledged = queued.acknowledged.lock().map_err(|_| unavailable())?;
            let terminals = queued.terminal.lock().map_err(|_| unavailable())?;
            if acknowledged.len() != queued.recipient_bindings.len()
                || terminals.len() != acknowledged.len()
            {
                return Err(unavailable());
            }
            for (index, (recipient, floor)) in queued.recipient_bindings.iter().enumerate() {
                if acknowledged[index] || protected.contains(&(recipient.clone(), queued.sequence))
                {
                    continue;
                }
                if let Some(terminal) = terminals[index] {
                    if !terminal.released {
                        return Ok(true);
                    }
                } else if queued.envelope.ttl_ms.is_some()
                    || self.registration_floors.get(recipient) != Some(floor)
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Observes expiry/authoritative retirement and ages existing dispositions.
    /// Returns the count of newly terminal recipients. Repeated passes do not
    /// renew a horizon. Clock rollback cannot revive a terminal occurrence.
    /// At most the configured pending occurrences/recipient slots are visited;
    /// poisoned evidence returns InvalidState before changing any slot.
    pub fn settle_delivery_outcomes_at(&mut self, now_ms: u64) -> Result<usize> {
        self.settle_delivery_outcomes_with_native_commits_at(now_ms, &[])
    }

    /// Maintains terminal custody while preserving exact offered occurrences
    /// already canonically committed by the trusted native adapter but waiting
    /// for an earlier cursor prefix. Claims validate before any mutation and
    /// are not persisted as new task authority. They cannot revive terminal
    /// mail or protect foreign/unoffered occurrences. This has no wire action.
    pub fn settle_delivery_outcomes_with_native_commits_at(
        &mut self,
        now_ms: u64,
        native_commits: &[(AgentId, MessageSequence)],
    ) -> Result<usize> {
        let protected = self.native_commit_protection(native_commits)?;
        let mut guards = Vec::new();
        for queued in self.pending.values() {
            let acknowledged = queued.acknowledged.lock().map_err(|_| unavailable())?;
            let terminal = queued.terminal.lock().map_err(|_| unavailable())?;
            if acknowledged.len() != queued.recipient_bindings.len()
                || terminal.len() != acknowledged.len()
            {
                return Err(unavailable());
            }
            guards.push((queued, acknowledged, terminal));
        }
        let mut changed = 0;
        for (queued, acknowledged, mut terminals) in guards {
            for (index, (recipient, floor)) in queued.recipient_bindings.iter().enumerate() {
                if acknowledged[index] || protected.contains(&(recipient.clone(), queued.sequence))
                {
                    continue;
                }
                let terminal = &mut terminals[index];
                if let Some(outcome) = terminal {
                    // Saturating subtraction avoids overflow at u64::MAX and
                    // does not renew the horizon on wall-clock rollback.
                    if now_ms.saturating_sub(outcome.observed_at_ms) >= TERMINAL_HANDOFF_MS {
                        outcome.released = true;
                    }
                    continue;
                }
                if now_ms < queued.accepted_at_ms {
                    continue;
                }
                let reason = if expired(queued, now_ms) {
                    Some(DeliveryTerminalReason::Expired)
                } else if self.registration_floors.get(recipient) != Some(floor) {
                    Some(DeliveryTerminalReason::RecipientRetired)
                } else {
                    None
                };
                if let Some(reason) = reason {
                    *terminal = Some(MessageDeliveryTerminalSnapshot {
                        reason,
                        observed_at_ms: now_ms,
                        released: false,
                    });
                    changed += 1;
                }
            }
        }
        self.release_completed_pending()?;
        Ok(changed)
    }

    /// Validates bounded transient native commit evidence before any derived
    /// read exclusion or terminal mutation. It never stores new task authority.
    pub(super) fn native_commit_protection(
        &self,
        native_commits: &[(AgentId, MessageSequence)],
    ) -> Result<HashSet<(AgentId, MessageSequence)>> {
        if native_commits.len() > self.pending_limits.recipient_obligations {
            return Err(MessageError::invalid_args(
                "native commit protection exceeds obligation bound",
            ));
        }
        let mut protected = HashSet::new();
        for (recipient, sequence) in native_commits {
            let row = self.pending.get(sequence).ok_or_else(|| {
                MessageError::invalid_args("native commit occurrence is unavailable")
            })?;
            if !self.accepted_recipient(row, recipient)
                || self.recipient_is_terminal(row, recipient)
                || super::offers::offered_at(row, recipient)?.is_none()
            {
                return Err(MessageError::invalid_args(
                    "native commit protection requires an exact own offer",
                ));
            }
            protected.insert((recipient.clone(), *sequence));
        }
        Ok(protected)
    }

    /// Returns up to min(limit,100) unconsumed outcomes in occurrence/binding
    /// order for an authenticated currently registered ORIGINAL sender interval.
    /// Results also stop at 64 KiB of logical cloned string/record bytes.
    /// No payload, private task result, new message or inferred wake is exposed.
    /// Legacy unknown sender intervals are unclaimable. Unregistered callers
    /// receive Forbidden; poisoned evidence returns InvalidState.
    pub fn delivery_outcomes_for(
        &self,
        sender: &AgentId,
        limit: usize,
    ) -> Result<Vec<MessageDeliveryOutcome>> {
        self.require_outcome_sender(sender)?;
        let mut result = Vec::new();
        let mut bytes = 0usize;
        let limit = limit.min(OUTCOME_BATCH);
        for queued in self
            .pending
            .values()
            .filter(|row| self.owns_outcomes(row, sender))
        {
            let terminals = queued.terminal.lock().map_err(|_| unavailable())?;
            for (index, terminal) in terminals.iter().enumerate() {
                if result.len() == limit {
                    return Ok(result);
                }
                if let Some(terminal) = terminal.filter(|terminal| !terminal.released) {
                    let (recipient, floor) = &queued.recipient_bindings[index];
                    let charge =
                        super::auxiliary::outcome_bytes(&queued.envelope.id, recipient.as_str());
                    if bytes.saturating_add(charge) > super::auxiliary::OUTCOME_BYTES {
                        return Ok(result);
                    }
                    bytes = bytes.saturating_add(charge);
                    result.push(MessageDeliveryOutcome {
                        sequence: queued.sequence,
                        message_id: queued.envelope.id.clone(),
                        recipient: MessageRecipientBindingSnapshot {
                            recipient: recipient.to_string(),
                            registered_after_sequence: *floor,
                        },
                        reason: terminal.reason,
                        observed_at_ms: terminal.observed_at_ms,
                    });
                }
            }
        }
        Ok(result)
    }

    /// Queries one exact original-sender-owned occurrence without scanning
    /// unrelated earlier receipts. Returns at most min(limit,100) records, no
    /// payload and no new authority, within 64 KiB of logical result bytes.
    /// Missing/foreign occurrences return empty;
    /// unregistered senders receive Forbidden and poisoned evidence InvalidState.
    pub fn delivery_outcomes_for_occurrence(
        &self,
        sender: &AgentId,
        sequence: MessageSequence,
        limit: usize,
    ) -> Result<Vec<MessageDeliveryOutcome>> {
        self.require_outcome_sender(sender)?;
        let Some(queued) = self
            .pending
            .get(&sequence)
            .filter(|row| self.owns_outcomes(row, sender))
        else {
            return Ok(Vec::new());
        };
        let terminals = queued.terminal.lock().map_err(|_| unavailable())?;
        let mut result = Vec::new();
        let mut bytes = 0usize;
        for (index, terminal) in terminals.iter().enumerate() {
            if result.len() == limit.min(OUTCOME_BATCH) {
                break;
            }
            if let Some(terminal) = terminal.filter(|terminal| !terminal.released) {
                let (recipient, floor) = &queued.recipient_bindings[index];
                let charge =
                    super::auxiliary::outcome_bytes(&queued.envelope.id, recipient.as_str());
                if bytes.saturating_add(charge) > super::auxiliary::OUTCOME_BYTES {
                    return Ok(result);
                }
                bytes = bytes.saturating_add(charge);
                result.push(MessageDeliveryOutcome {
                    sequence: queued.sequence,
                    message_id: queued.envelope.id.clone(),
                    recipient: MessageRecipientBindingSnapshot {
                        recipient: recipient.to_string(),
                        registered_after_sequence: *floor,
                    },
                    reason: terminal.reason,
                    observed_at_ms: terminal.observed_at_ms,
                });
            }
        }
        Ok(result)
    }

    /// Releases exactly one receipt only after its original owner has committed
    /// the handoff. Returns false for a retained already released receipt.
    /// Missing, forged, foreign or replacement-owner evidence fails before
    /// mutation; this never changes delivery cursors or recipient ACK state.
    pub fn acknowledge_delivery_outcome(
        &mut self,
        sender: &AgentId,
        outcome: &MessageDeliveryOutcome,
    ) -> Result<bool> {
        self.require_outcome_sender(sender)?;
        let queued = self
            .queued_by_sequence
            .get(&outcome.sequence)
            .ok_or_else(|| MessageError::not_found("MMP terminal occurrence is unavailable"))?;
        if !self.owns_outcomes(queued, sender) || queued.envelope.id != outcome.message_id {
            return Err(MessageError::forbidden(
                "MMP terminal outcome owner changed",
            ));
        }
        let index = queued
            .recipient_bindings
            .iter()
            .position(|(id, floor)| {
                id.as_str() == outcome.recipient.recipient
                    && *floor == outcome.recipient.registered_after_sequence
            })
            .ok_or_else(|| MessageError::invalid_args("MMP terminal recipient is invalid"))?;
        let changed = {
            let mut terminals = queued.terminal.lock().map_err(|_| unavailable())?;
            let terminal = terminals
                .get_mut(index)
                .and_then(Option::as_mut)
                .ok_or_else(|| {
                    MessageError::invalid_args("MMP terminal outcome was not observed")
                })?;
            if terminal.reason != outcome.reason
                || terminal.observed_at_ms != outcome.observed_at_ms
            {
                return Err(MessageError::invalid_args(
                    "MMP terminal outcome evidence changed",
                ));
            }
            let changed = !terminal.released;
            terminal.released = true;
            changed
        };
        self.release_completed_pending()?;
        Ok(changed)
    }

    /// A terminal slot is never deliverable again, even after clock rollback
    /// and after its handoff was consumed. Poison remains fail-closed.
    pub(super) fn recipient_is_terminal(
        &self,
        queued: &QueuedEnvelope,
        recipient: &AgentId,
    ) -> bool {
        queued
            .recipient_bindings
            .iter()
            .position(|(id, _)| id == recipient)
            .is_some_and(|index| {
                queued
                    .terminal
                    .lock()
                    .ok()
                    .and_then(|slots| slots.get(index).map(Option::is_some))
                    .unwrap_or(true)
            })
    }

    /// Original sender authentication is separate from mutable objective or
    /// presence telemetry. Exact registration floor prevents reused-ID claims.
    fn owns_outcomes(&self, queued: &QueuedEnvelope, sender: &AgentId) -> bool {
        queued.envelope.sender.agent_id == *sender
            && queued
                .sender_registered_after_sequence
                .is_some_and(|floor| self.registration_floors.get(sender) == Some(&floor))
    }

    /// Checks connection identity without granting a caller-supplied interval.
    fn require_outcome_sender(&self, sender: &AgentId) -> Result<()> {
        if self.registered.contains_key(sender) {
            Ok(())
        } else {
            Err(MessageError::forbidden(
                "MMP terminal outcome requires a registered sender",
            ))
        }
    }

    /// Releases shared custody only after ALL slots have positive ACK or
    /// released terminal evidence. Full fanout metadata is charged until then.
    /// Inspect every row before mutation so poison never drops accepted source.
    pub(super) fn release_completed_pending(&mut self) -> Result<()> {
        let mut completed = Vec::new();
        for (sequence, queued) in &self.pending {
            let acknowledged = queued.acknowledged.lock().map_err(|_| unavailable())?;
            let terminals = queued.terminal.lock().map_err(|_| unavailable())?;
            if acknowledged
                .iter()
                .zip(terminals.iter())
                .all(|(ack, terminal)| *ack || terminal.is_some_and(|terminal| terminal.released))
            {
                completed.push(*sequence);
            }
        }
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

/// Bounded diagnostic; never logs source payload or raw poisoned state.
fn unavailable() -> MessageError {
    MessageError::invalid_state("MMP terminal custody evidence is unavailable")
}

/// Cross-validates terminal reservations, reasons and original sender interval
/// before restoration. Old formats cannot smuggle newer authority/state.
pub(super) fn validate_terminal_snapshot(snapshot: &MessageServiceSnapshot) -> Result<()> {
    let invalid = || MessageError::invalid_args("snapshot MMP terminal custody is invalid");
    let floors = snapshot.registration_floors.as_deref().unwrap_or(&[]);
    for row in snapshot.canonical_messages() {
        if snapshot.schema_version < 8 {
            if row.terminal_recipients.is_some() || row.sender_registered_after_sequence.is_some() {
                return Err(invalid());
            }
            continue;
        }
        let bindings = row.recipient_bindings.as_deref().unwrap_or(&[]);
        let terminals = row.terminal_recipients.as_ref().ok_or_else(invalid)?;
        if terminals.len() != bindings.len() {
            return Err(invalid());
        }
        if let Some(sender_floor) = row.sender_registered_after_sequence {
            if sender_floor >= row.sequence {
                return Err(invalid());
            }
            if let Some(current) = floors
                .iter()
                .find(|floor| floor.recipient == row.envelope.sender.agent_id)
                && current.registered_after_sequence < row.sequence
                && current.registered_after_sequence != sender_floor
            {
                return Err(invalid());
            }
        }
        for (binding, terminal) in bindings
            .iter()
            .zip(terminals)
            .filter_map(|(binding, terminal)| terminal.as_ref().map(|terminal| (binding, terminal)))
        {
            if terminal.observed_at_ms < row.accepted_at_ms
                || row
                    .acknowledged_recipients
                    .as_deref()
                    .unwrap_or(&[])
                    .contains(&binding.recipient)
            {
                return Err(invalid());
            }
            match terminal.reason {
                DeliveryTerminalReason::Expired => {
                    if !row.envelope.ttl_ms.is_some_and(|ttl| {
                        row.accepted_at_ms.saturating_add(ttl) < terminal.observed_at_ms
                    }) {
                        return Err(invalid());
                    }
                }
                DeliveryTerminalReason::RecipientRetired => {
                    if floors.contains(binding) {
                        return Err(invalid());
                    }
                }
            }
        }
    }
    Ok(())
}
