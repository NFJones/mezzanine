//! Live layout restore continuation over the actor's original message ledger.
//!
//! Validated incoming display/layout bytes retain one original control request.
//! Existing issued jobs, canonical proofs, application receipts and end intents
//! drain on the unchanged root. Native pane identities then end on that same
//! ledger before layout teardown. Raw socket leases survive; generic snapshots
//! neither replace durable custody nor qualify cold unavailability or restart.

use super::{AsyncRuntimeRequest, AsyncRuntimeSessionActor};

/// The one accepted restore request and its irreversible retirement boundary.
pub(in crate::host::async_runtime) struct PendingSnapshotResume {
    request: AsyncRuntimeRequest,
    identities_retired: bool,
}
impl std::fmt::Debug for PendingSnapshotResume {
    /// Debug output never exposes the retained snapshot or control payload.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingSnapshotResume")
            .field("identities_retired", &self.identities_retired)
            .finish_non_exhaustive()
    }
}

impl PendingSnapshotResume {
    /// Retains original response ownership without cloning payloads or replies.
    pub(super) fn new(request: AsyncRuntimeRequest) -> Self {
        Self {
            request,
            identities_retired: false,
        }
    }
}
impl AsyncRuntimeSessionActor {
    /// Advances a validated restore only after original custody is quiescent.
    /// A failed root or application retains the original request and proof; it
    /// cannot turn a cancelled observer into a safe-to-drop storage operation.
    pub(super) fn poll_snapshot_resume(&mut self) -> Option<AsyncRuntimeRequest> {
        self.snapshot_resume.as_ref()?;
        if self.message_custody_shutdown_pending()
            || !self.message_custody_quiescent_for_restore()
            || self
                .service
                .ensure_runtime_message_transfers_settled_for_restore()
                .is_err()
        {
            return None;
        }
        if !self.snapshot_resume.as_ref()?.identities_retired {
            if self
                .service
                .retire_runtime_message_transfers_for_restore()
                .is_err()
            {
                return None;
            }
            self.snapshot_resume.as_mut()?.identities_retired = true;
            // Ending actual old pane registrations can create new O1 work.
            // Acquire its original lane now so idle ingress is not needed to
            // wake the restore continuation after this in-memory phase.
            self.pump_message_registration_end();
            if !self.message_custody_quiescent_for_restore()
                || self
                    .service
                    .ensure_runtime_message_transfers_settled_for_restore()
                    .is_err()
            {
                return None;
            }
        }
        let pending = self.snapshot_resume.take()?;
        self.service.set_message_restore_pending(false);
        Some(pending.request)
    }
}

/// Test controls select actual repository preparation and the production actor
/// completion branch while leaving provider dispatch outside the fixture.
#[cfg(test)]
impl AsyncRuntimeSessionActor {
    /// Stages genuine repository work without observing an already issued root
    /// job, allowing tests to distinguish physical custody from response tasks.
    pub(in crate::host) async fn stage_live_restore_for_tests(
        &mut self,
        snapshots: crate::storage::snapshot::SnapshotRepository,
        snapshot_id: &str,
        caller: mez_core::ids::ClientId,
    ) -> crate::Result<tokio::sync::oneshot::Receiver<crate::Result<super::AsyncControlInputResult>>>
    {
        let connection = crate::control::ControlConnectionState::trusted_existing_client(caller);
        let body = serde_json::json!({"jsonrpc":"2.0","id":"original-restore","method":"snapshot/resume","params":{"snapshot_id":snapshot_id,"idempotency_key":"original-restore-control"}}).to_string();
        let work = self
            .service
            .prepare_runtime_snapshot_control_async_work(&body, &connection)
            .ok_or_else(|| crate::MezError::invalid_state("test restore work missing"))?
            .map_err(crate::MezError::invalid_state)?;
        let outcome =
            super::construction::execute_snapshot_control_async_work(&snapshots, &work).await;
        let (reply, response) = tokio::sync::oneshot::channel();
        let request = AsyncRuntimeRequest::CompleteSnapshotControlInput {
            message_restore_drained: false,
            consumed_prefix: 0,
            output_prefix: Vec::new(),
            remaining_input: Vec::new(),
            max_content_length: 1024 * 1024,
            snapshots,
            connection,
            work,
            outcome: Box::new(outcome),
            reply,
        };
        let _ = Box::pin(self.handle_request(request)).await;
        Ok(response)
    }

    /// Advances only the actual restore continuation after its message pump.
    pub(in crate::host) async fn poll_live_restore_for_tests(&mut self) -> bool {
        if let Some(request) = self.poll_snapshot_resume() {
            let _ = Box::pin(self.handle_request(request)).await;
            true
        } else {
            false
        }
    }
}
