//! Freeze native exchange identity in the existing journal before any admission
//! await or transport poll. A plan is neither permission nor proof of a send.
//! Only finalized evidence can be checkpointed/ACKed; canceled stack custody
//! finalizes the same identity with unknown usage, without another expense event.

use super::*;

impl ProviderWireRequestObservation {
    /// Compare only the pre-send subject using the journal's same contract.
    /// Counter/outcome equality is separately enforced on final receipt capture.
    pub(crate) fn same_wire_subject(&self, observation: &Self) -> bool {
        same_subject(self, observation)
    }
}

impl ProviderWireObservationContext<'_> {
    fn planned_wire(&self) -> std::sync::MutexGuard<'_, Option<ProviderWireRequestObservation>> {
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
        if let Some(pending) = &self.observer.pending {
            pending.plan(observation.clone(), self.observer.budget_receipts.as_ref())?;
        }
        *planned = Some(observation);
        Ok(())
    }

    pub(super) fn planned_wire_id(&self) -> Option<String> {
        self.planned_wire()
            .as_ref()
            .map(|observation| observation.request_id.clone())
    }

    pub(super) fn finish_planned_wire(&self, id: &str) {
        let mut planned = self.planned_wire();
        if planned
            .as_ref()
            .is_some_and(|observation| observation.request_id == id)
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
