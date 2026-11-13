//! Freeze native exchange identity in the existing journal before any admission
//! await or transport poll. A plan is neither permission nor proof of a send.
//! Only finalized evidence can be checkpointed/ACKed; canceled stack custody
//! finalizes the same identity with unknown usage, without another expense event.

use super::*;

/// One concrete subject and the exact descriptor selected for it. Clones retain
/// custody only; they cannot allocate a replacement factory or a send permit.
#[derive(Clone)]
pub(super) struct PlannedWireAttempt {
    pub(super) observation: ProviderWireRequestObservation,
    pub(super) factory: Option<Arc<dyn ProviderObservationReceiptFactory>>,
    pub(super) transport_attached: bool,
}

impl ProviderWireRequestObservation {
    /// Compare only the pre-send subject using the journal's same contract.
    /// Counter/outcome equality is separately enforced on final receipt capture.
    pub(crate) fn same_wire_subject(&self, observation: &Self) -> bool {
        same_subject(self, observation)
    }
}

impl ProviderWireObservationContext<'_> {
    fn planned_wire(&self) -> std::sync::MutexGuard<'_, Option<PlannedWireAttempt>> {
        // This lock contains only content-free identity, never mutable grants.
        self.planned_wire
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    pub(super) fn plan_wire_attempt(
        &self,
        request: &ModelRequest,
        attempt_index: usize,
        retry_reason: Option<&str>,
    ) -> Result<()> {
        let observation = self.build_wire_observation(
            request,
            attempt_index,
            retry_reason,
            FinalWireObservation {
                final_wire_diagnostics: None,
                response_diagnostics: None,
                elapsed_ms: None,
                usage_evidence: WireUsageEvidence::Concrete(None),
                usage_presence: Some(Default::default()),
            },
            None,
            false,
            Some("interrupted".into()),
        );
        let mut planned = self.planned_wire();
        if planned.is_some() {
            return Err(MezError::conflict(
                "provider context already owns a wire attempt",
            ));
        }
        let factory = match &self.observer.budget_receipt_source {
            Some(source) => Some(source.for_exchange(&observation)?),
            None => self.observer.budget_receipts.clone(),
        };
        if let Some(pending) = &self.observer.pending {
            pending.plan(observation.clone(), factory.as_ref())?;
        }
        *planned = Some(PlannedWireAttempt {
            observation,
            factory,
            transport_attached: false,
        });
        Ok(())
    }

    pub(super) fn planned_wire_id(&self) -> Option<String> {
        self.planned_wire()
            .as_ref()
            .map(|planned| planned.observation.request_id.clone())
    }

    /// Select only the original plan's factory, never allocate during final
    /// evidence transfer (including cancellation). Fixed legacy diagnostic
    /// factories still support providers without native pre-send planning.
    pub(super) fn planned_wire_factory(
        &self,
        id: &str,
    ) -> Option<Arc<dyn ProviderObservationReceiptFactory>> {
        self.planned_wire()
            .as_ref()
            .filter(|planned| planned.observation.request_id == id)
            .map(|planned| planned.factory.clone())
            .unwrap_or_else(|| {
                if self.observer.budget_receipt_source.is_some() {
                    self.observer
                        .pending
                        .as_ref()
                        .and_then(|pending| pending.original_factory(id))
                } else {
                    self.observer.budget_receipts.clone()
                }
            })
    }

    pub(super) fn finish_planned_wire(&self, id: &str) {
        let mut planned = self.planned_wire();
        if planned
            .as_ref()
            .is_some_and(|planned| planned.observation.request_id == id)
        {
            *planned = None;
        }
    }
}

/// Only response evidence may change when a planned identity is finalized.
/// Request/owner/ordinal/purpose metadata cannot be rebound by a late result.
pub(super) fn same_subject(
    planned: &ProviderWireRequestObservation,
    observation: &ProviderWireRequestObservation,
) -> bool {
    let mut identity = observation.clone();
    identity.final_wire_diagnostics = planned.final_wire_diagnostics.clone();
    identity.response_diagnostics = planned.response_diagnostics.clone();
    identity.usage = planned.usage;
    identity.usage_presence = planned.usage_presence;
    identity.succeeded = planned.succeeded;
    identity.failure_kind = planned.failure_kind.clone();
    identity.elapsed_ms = planned.elapsed_ms;
    identity == *planned
}
