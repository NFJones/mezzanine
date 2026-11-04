//! Exact recipient registration allocations frozen before guarded acceptance.
//!
//! Native attribution identifies an application epoch; raw recipients need the
//! independent registration namespace as well. Numeric floors and public IDs
//! cannot distinguish replacement without an intervening allocation. Each live
//! source shares only its actual registration's one-way end marker, not a bearer
//! or a complete identity. Encoding persists bounded namespaces alone; decoded
//! facts have no end observation and cannot manufacture an endpoint or its end.

use super::*;

/// One namespace plus an initially empty or genuinely qualified end marker.
#[derive(Debug)]
pub(in crate::messaging) struct RecipientRegistration {
    pub(super) namespace: [u8; 16],
    pub(super) ended: Arc<std::sync::OnceLock<u64>>,
    _retention: Option<super::registration_end::RegistrationSourceRetention>,
}

/// Registration observations in the original immutable recipient slot order.
#[derive(Debug)]
pub(in crate::messaging) struct RegistrationBindings {
    pub(super) recipients: Vec<RecipientRegistration>,
}

impl RegistrationBindings {
    /// Charges vector capacity, marker allocation and future encoding per slot.
    pub(super) fn metadata_bytes(&self) -> usize {
        Self::metadata_for(self.recipients.len())
    }

    /// Computes the same conservative charge before any source allocation.
    pub(super) fn metadata_for(count: usize) -> usize {
        128usize.saturating_add(count.saturating_mul(128))
    }

    /// Builds inert decoded observations only after bounded UUID/count checks.
    pub(super) fn decoded(namespaces: Vec<[u8; 16]>) -> Self {
        Self {
            recipients: namespaces
                .into_iter()
                .map(|namespace| RecipientRegistration {
                    namespace,
                    ended: Default::default(),
                    _retention: None,
                })
                .collect(),
        }
    }
}

impl MessageService {
    /// Links a genuine original end positive to matching cold source markers.
    /// A decoded source may predate a later native classification in the same
    /// namespace. It must observe that allocation's actual end as well, without
    /// deriving retirement from its otherwise inert namespace alone.
    pub(super) fn check_registration_end_sources(
        &self,
        end: &MessageRegistrationEndSnapshot,
    ) -> Result<()> {
        for row in self.queued_by_sequence.values() {
            let Some(registrations) = &row.registration_bindings else {
                continue;
            };
            if registrations.recipients.len() != row.recipient_bindings.len() {
                return Err(unavailable());
            }
            for ((agent, floor), binding) in
                row.recipient_bindings.iter().zip(&registrations.recipients)
            {
                if binding.namespace != end.registration_namespace {
                    continue;
                }
                if agent.as_str() != end.agent_id
                    || *floor != end.registered_after_sequence
                    || row.sequence <= *floor
                    || row.sequence > end.ended_after_sequence
                    || binding
                        .ended
                        .get()
                        .is_some_and(|cutoff| *cutoff != end.ended_after_sequence)
                {
                    return Err(unavailable());
                }
            }
        }
        Ok(())
    }

    /// Publishes only after whole-source preflight and an actual original end
    /// positive. This marks lifecycle evidence, never terminal slots or ACKs.
    pub(super) fn publish_registration_end_to_sources(
        &self,
        end: &MessageRegistrationEndSnapshot,
    ) -> Result<()> {
        self.check_registration_end_sources(end)?;
        for row in self.queued_by_sequence.values() {
            if let Some(registrations) = &row.registration_bindings {
                for binding in &registrations.recipients {
                    if binding.namespace == end.registration_namespace {
                        let _ = binding.ended.get_or_init(|| end.ended_after_sequence);
                    }
                }
            }
        }
        Ok(())
    }

    /// Runs only after a sealed one-use native recovery port has qualified the
    /// old store/context. Explicit raw slots have no reconnect owner after a
    /// process restart. Preserve a previously qualified o1 cutoff when present;
    /// otherwise the original persistent allocator bounds this cold end.
    pub(super) fn retain_cold_raw_registration_ends(&self) -> Result<()> {
        for row in self.pending.values() {
            let (Some(registrations), Some(native)) =
                (&row.registration_bindings, &row.native_bindings)
            else {
                continue;
            };
            if registrations.recipients.len() != row.recipient_bindings.len()
                || native.recipients.len() != row.recipient_bindings.len()
            {
                return Err(unavailable());
            }
            let acknowledged = row.acknowledged.lock().map_err(|_| unavailable())?;
            let terminal = row.terminal.lock().map_err(|_| unavailable())?;
            for (index, ((agent, floor), binding)) in row
                .recipient_bindings
                .iter()
                .zip(&registrations.recipients)
                .enumerate()
            {
                if native.recipients[index].is_some()
                    || acknowledged[index]
                    || terminal[index].is_some()
                    || self
                        .registration_tokens
                        .values()
                        .any(|live| live.native_namespace.as_bytes() == &binding.namespace)
                {
                    continue;
                }
                // A marker populated by the trusted original-history callback
                // already names committed o1. Its immutable target must never
                // be reseated as a fresh journal intent at the current position.
                if binding.ended.get().is_some() {
                    continue;
                }
                let cutoff = self.next_sequence.saturating_sub(1);
                if row.sequence <= *floor || row.sequence > cutoff {
                    return Err(unavailable());
                }
                self.track_cold_raw_end(
                    agent,
                    binding.namespace,
                    *floor,
                    binding.ended.clone(),
                    cutoff,
                )?;
            }
        }
        Ok(())
    }

    /// Captures exact actual recipients before reserving their accepted source.
    /// End intent retention is reserved first, so raw removal never allocates
    /// after losing its registration. Legacy/unguarded sources stay unknown.
    pub(super) fn capture_registration_bindings(
        &self,
        envelope: &Envelope,
        bindings: &[(AgentId, MessageSequence)],
    ) -> Result<Option<RegistrationBindings>> {
        if self
            .storage_custody
            .as_ref()
            .is_none_or(|p| !p.native_continuity_required)
        {
            return Ok(None);
        }
        self.reserve_storage_metadata(
            envelope,
            bindings,
            RegistrationBindings::metadata_for(bindings.len()),
        )?;
        let mut recipients = Vec::new();
        recipients
            .try_reserve_exact(bindings.len())
            .map_err(|_| unavailable())?;
        for (id, floor) in bindings {
            let registration = self
                .capture_sender_registration(id)
                .ok_or_else(unavailable)?;
            if self.registration_floors.get(id) != Some(floor) {
                return Err(unavailable());
            }
            let retention = self.track_source_registration(&registration)?;
            recipients.push(RecipientRegistration {
                namespace: *registration.identity.native_namespace.as_bytes(),
                ended: registration.identity.ended_after_sequence.clone(),
                _retention: Some(retention),
            });
        }
        Ok(Some(RegistrationBindings { recipients }))
    }

    /// Rejects a replacement allocation independently of equal labels/floors.
    /// Missing legacy attribution remains unknown rather than a forged match.
    pub(super) fn registration_recipient_is_current(
        &self,
        row: &QueuedEnvelope,
        id: &AgentId,
    ) -> bool {
        let Some(registrations) = &row.registration_bindings else {
            return true;
        };
        let Some(index) = row
            .recipient_bindings
            .iter()
            .position(|(recipient, _)| recipient == id)
        else {
            return false;
        };
        registrations.recipients.get(index).is_some_and(|original| {
            self.registration_tokens.get(id).is_some_and(|current| {
                current.native_namespace.as_bytes() == &original.namespace
                    && current.ended_after_sequence.get().is_none()
            })
        })
    }
}

/// Content-free integrity/capacity failure, without a namespace or bearer.
fn unavailable() -> MessageError {
    MessageError::invalid_state("MMP original recipient registration unavailable")
}

#[cfg(test)]
mod tests;
