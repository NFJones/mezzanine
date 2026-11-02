//! Native tracked metadata paired with actual unpublished storage custody.
//!
//! One pending record retains original recipient tasks, parent anchor and semantic
//! fingerprint under the lower handoff reservation. It stays absent from accepted
//! receipts/edges and survives conversation teardown, task-terminal observation
//! and caller loss. Opaque original capabilities fence actor adoption/release.
//! Trusted callbacks must be bounded owner transitions, never filesystem/provider
//! execution or original action redispatch. Permission/source capture and final
//! native action publication remain separate integration requirements.

use super::*;
use mez_agent::messaging::MessageAcceptanceTicket;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// Metadata charged to the existing per-recipient handoff reservation.
#[derive(Debug)]
pub(super) struct TrackedRequestMetadata {
    /// Exact original recipient task incarnation and future outcome slots.
    pub(super) slots: Vec<RecipientState>,
    /// Original native parent/process evidence, never refreshed on completion.
    pub(super) anchor: ParentAnchor,
    /// Semantic identity independent of later sender telemetry/time.
    pub(super) fingerprint: [u8; 32],
}

/// Payload-free original native reservation capability retained by the actor.
#[derive(Clone)]
pub(crate) struct RuntimePreparedTrackedMessage {
    /// Original lower capability; matching public IDs cannot replace it.
    lower: MessageAcceptanceTicket,
    /// Unique native metadata owner; nonzero records only actual installation.
    identity: Arc<AtomicU64>,
}
impl std::fmt::Debug for RuntimePreparedTrackedMessage {
    /// Reveals no payload, process identity or allocation address.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimePreparedTrackedMessage")
            .field("sequence", &self.lower.sequence())
            .finish_non_exhaustive()
    }
}

/// One unpublished native owner, invisible to accepted receipt/wait selection.
#[derive(Debug)]
pub(super) struct PendingTrackedMessage {
    /// Retains original settlement capability independently of observers.
    ticket: RuntimePreparedTrackedMessage,
    /// Original metadata, including terminal observations during storage I/O.
    metadata: TrackedRequestMetadata,
}

impl RuntimeRequestWaits {
    /// Names only the actual paired receipt awaiting native installation.
    /// The generation derives from its private admission capability, never IDs.
    pub(super) fn pending_published_generation(&self) -> Option<u64> {
        let pending = self.pending.as_ref()?;
        self.requests
            .receipt_for_admission(&pending.ticket.lower)
            .map(|receipt| receipt.generation())
    }

    /// Reads original slots for custody/expiry only, including the paired pending
    /// receipt. This must not supply unpublished wait or edge-selection authority.
    pub(super) fn custody_recipient_slots(&self, generation: u64) -> Option<&[RecipientState]> {
        if let Some(slots) = self.recipients.get(&generation) {
            return Some(slots.as_slice());
        }
        let pending = self.pending.as_ref()?;
        self.requests
            .receipt_for_admission(&pending.ticket.lower)
            .filter(|receipt| receipt.generation() == generation)?;
        Some(pending.metadata.slots.as_slice())
    }

    /// Captured task sets for terminal observation, never wait-edge authority.
    pub(super) fn recipient_sets(&self) -> impl Iterator<Item = &Vec<RecipientState>> {
        self.recipients
            .values()
            .chain(self.pending.iter().map(|p| &p.metadata.slots))
    }
    /// Includes pending outcomes without exposing an unpublished receipt.
    pub(super) fn recipient_sets_mut(&mut self) -> impl Iterator<Item = &mut Vec<RecipientState>> {
        self.recipients
            .values_mut()
            .chain(self.pending.iter_mut().map(|p| &mut p.metadata.slots))
    }
}

impl RuntimeSessionService {
    /// Prepares custody after trusted original permission/source capture and
    /// actual capacity selection. Caller must settle this exact owner even after
    /// issuance/teardown; no accepted receipt or task edge is visible yet.
    #[allow(
        dead_code,
        reason = "native source activation continues under e048f9a8"
    )]
    pub(crate) fn prepare_tracked_runtime_message(
        &mut self,
        turn: &AgentTurnRecord,
        envelope: Envelope,
        scope: MessageScope,
        now_ms: u64,
    ) -> Result<RuntimePreparedTrackedMessage> {
        if self.agent.request_waits.pending.is_some() {
            return Err(changed());
        }
        self.prune_request_wait_owners_at(now_ms);
        let origin = Self::request_origin(turn)?;
        let metadata = self.capture_tracked_request_metadata(
            turn,
            &origin,
            &envelope,
            scope,
            request_fingerprint(&envelope, scope),
        )?;
        let lower = self.agent.request_waits.requests.prepare_acceptance(
            self.control.message_service_mut(),
            origin,
            envelope,
            scope,
            now_ms,
        )?;
        let ticket = RuntimePreparedTrackedMessage {
            lower,
            identity: Arc::new(AtomicU64::new(0)),
        };
        self.agent.request_waits.pending = Some(PendingTrackedMessage {
            ticket: ticket.clone(),
            metadata,
        });
        Ok(ticket)
    }

    /// Checks actual native/lower allocation before a trusted root touches work.
    /// Equal-looking origin text never qualifies, and validation performs no I/O.
    pub(crate) fn validate_prepared_tracked_runtime_message(
        &self,
        ticket: &RuntimePreparedTrackedMessage,
    ) -> Result<()> {
        self.require_prepared_native_metadata(ticket)?;
        self.agent
            .request_waits
            .requests
            .prepared_origin(&ticket.lower)?;
        Ok(())
    }

    /// Checks only native metadata allocation so a previously published actual
    /// lower receipt can finish upper adoption after a lost callback result.
    fn require_prepared_native_metadata(
        &self,
        ticket: &RuntimePreparedTrackedMessage,
    ) -> Result<()> {
        let pending = self
            .agent
            .request_waits
            .pending
            .as_ref()
            .ok_or_else(changed)?;
        if Arc::ptr_eq(&pending.ticket.identity, &ticket.identity) {
            Ok(())
        } else {
            Err(changed())
        }
    }

    /// Calls a bounded trusted issuance transition with both real owners. Failed
    /// start retains original reservation for explicit never-issued cancellation.
    pub(crate) fn start_prepared_tracked_runtime_message(
        &mut self,
        ticket: &RuntimePreparedTrackedMessage,
        start: impl FnOnce(
            &mut mez_agent::messaging::MessageService,
            &MessageRequests,
            &MessageAcceptanceTicket,
        ) -> Result<()>,
    ) -> Result<()> {
        self.validate_prepared_tracked_runtime_message(ticket)?;
        start(
            self.control.message_service_mut(),
            &self.agent.request_waits.requests,
            &ticket.lower,
        )
    }

    /// Adopts actual root completion and installs original native slots under
    /// its real lower generation. Duplicates do not call the operation again;
    /// consumed receipts cannot recreate dependency state or send another effect.
    pub(crate) fn adopt_prepared_tracked_runtime_message(
        &mut self,
        ticket: &RuntimePreparedTrackedMessage,
        adopt: impl FnOnce(
            &mut mez_agent::messaging::MessageService,
            &mut MessageRequests,
        ) -> Result<Delivery>,
    ) -> Result<Delivery> {
        let installed = ticket.identity.load(Ordering::Acquire);
        if installed != 0 {
            return self
                .agent
                .request_waits
                .requests
                .receipt_for_admission(&ticket.lower)
                .filter(|receipt| receipt.generation() == installed)
                .map(|receipt| receipt.delivery().clone())
                .ok_or_else(changed);
        }
        self.require_prepared_native_metadata(ticket)?;
        // An actual published paired receipt is stronger than a lost callback
        // result. Do not re-enter root publication or reinterpret that as unsent.
        if self
            .agent
            .request_waits
            .requests
            .receipt_for_admission(&ticket.lower)
            .is_none()
        {
            self.validate_prepared_tracked_runtime_message(ticket)?;
            let delivery = adopt(
                self.control.message_service_mut(),
                &mut self.agent.request_waits.requests,
            )?;
            let receipt = self
                .agent
                .request_waits
                .requests
                .receipt_for_admission(&ticket.lower)
                .ok_or_else(changed)?;
            if receipt.delivery() != &delivery {
                return Err(changed());
            }
        }
        let receipt = self
            .agent
            .request_waits
            .requests
            .receipt_for_admission(&ticket.lower)
            .ok_or_else(changed)?;
        let delivery = receipt.delivery().clone();
        let generation = receipt.generation();
        let pending = self
            .agent
            .request_waits
            .pending
            .take()
            .ok_or_else(changed)?;
        self.install_tracked_request_metadata(generation, pending.metadata);
        ticket.identity.store(generation, Ordering::Release);
        Ok(delivery)
    }

    /// Releases only the exact never-issued native/lower pair. Issued/unknown
    /// work stays charged regardless of caller conversation lifecycle.
    pub(crate) fn cancel_prepared_tracked_runtime_message(
        &mut self,
        ticket: &RuntimePreparedTrackedMessage,
    ) -> Result<()> {
        self.validate_prepared_tracked_runtime_message(ticket)?;
        self.agent
            .request_waits
            .requests
            .cancel_prepared_acceptance(self.control.message_service_mut(), &ticket.lower)?;
        self.agent.request_waits.pending.take();
        Ok(())
    }

    /// Drops native metadata only after the root qualifies actual noncommit and
    /// releases both lower owners. Generic I/O errors never qualify this release.
    pub(crate) fn release_rejected_tracked_runtime_message(
        &mut self,
        ticket: &RuntimePreparedTrackedMessage,
        release: impl FnOnce(
            &mut mez_agent::messaging::MessageService,
            &mut MessageRequests,
        ) -> Result<()>,
    ) -> Result<()> {
        self.validate_prepared_tracked_runtime_message(ticket)?;
        release(
            self.control.message_service_mut(),
            &mut self.agent.request_waits.requests,
        )?;
        if self
            .agent
            .request_waits
            .requests
            .receipt_for_admission(&ticket.lower)
            .is_some()
            || self
                .agent
                .request_waits
                .requests
                .prepared_origin(&ticket.lower)
                .is_ok()
            || self.control.message_service().has_unpublished_acceptance()
        {
            return Err(changed());
        }
        self.agent.request_waits.pending.take();
        Ok(())
    }

    /// Restore cannot replace an owner that still owes actual storage settlement;
    /// even a never-issued reservation requires explicit original cancellation.
    pub(crate) fn ensure_no_prepared_tracked_message_for_restore(&self) -> Result<()> {
        if self.agent.request_waits.pending.is_some() {
            Err(changed())
        } else {
            Ok(())
        }
    }
}

/// Fixed diagnostic excludes payload, private path and process identity.
fn changed() -> crate::MezError {
    crate::MezError::conflict("original tracked MMP storage owner must settle before replacement")
}
