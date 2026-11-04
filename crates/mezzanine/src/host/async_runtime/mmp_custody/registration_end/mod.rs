//! Actual original registration ends on the root's exclusive worker lane.
//!
//! Pre-reserved lower intents outlive live registration removal. This lane
//! freezes one exact o1 input/position before physical work, retains it through
//! lost replies and Stop, and releases it only after actual join and original
//! adoption. An end is descriptive evidence for later source-slot qualification;
//! this operation changes no delivery, ACK, context, provider or task state.

use std::sync::OnceLock;
use std::task::Context;

use mez_agent::messaging::{MessageRegistrationEndRequest, MessageRegistrationEndVerified};

use super::*;
use crate::storage::mmp_custody::registration_end::RegistrationEndPlan;

/// Original metadata input and row plan retained until actual join/adoption.
pub(super) struct Admission {
    request: MessageRegistrationEndRequest,
    anchor: AcceptancePlan,
    plan: Arc<OnceLock<RegistrationEndPlan>>,
    failures: u32,
}
/// Actual positive commit and updated store position held for exact adoption.
pub(super) struct Written {
    opened: Opened,
    verified: MessageRegistrationEndVerified,
}

impl MmpCustodyRoot {
    /// Captures one already-ended actual allocation while the root is idle.
    /// Stop permits this drain; no new message or live registration is created.
    /// Returns false when there is no pending actual end.
    pub fn start_registration_end(&self, service: &MessageService) -> Result<bool> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Idle(opened) = &*state else {
            return Err(MezError::conflict(
                "MMP registration end requires an idle lane",
            ));
        };
        let Some(request) = service
            .prepare_registration_end(&self.0.storage_scope)
            .map_err(lower_error)?
        else {
            return Ok(false);
        };
        let admission = Admission {
            request,
            anchor: opened.anchor.clone(),
            plan: Arc::new(OnceLock::new()),
            failures: 0,
        };
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        if let State::Idle(opened) = previous {
            let worker = self.spawn_registration_end(Some(opened.store), &admission);
            *state = State::RecordingRegistrationEnd { admission, worker };
        }
        Ok(true)
    }

    /// Retries only the same input/preimage after actual loss joined and backoff.
    pub fn retry_registration_end(&self, now: Instant) -> Result<bool> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::RecordedRegistrationEnd {
            result, retry_at, ..
        } = &*state
        else {
            return Err(ownership_error());
        };
        if result.is_ok() {
            return Err(MezError::conflict(
                "MMP registration end awaits original adoption",
            ));
        }
        if now < *retry_at {
            return Ok(false);
        }
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        if let State::RecordedRegistrationEnd { admission, .. } = previous {
            let worker = self.spawn_registration_end(None, &admission);
            *state = State::RecordingRegistrationEnd { admission, worker };
        }
        Ok(true)
    }

    /// Releases only this original retained end after its exact positive joins.
    /// A successor registration is neither marked nor removed by adoption.
    /// A foreign adopter cannot consume the original positive input/result.
    pub fn adopt_registration_end(&self, service: &mut MessageService) -> Result<bool> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::RecordedRegistrationEnd {
            admission,
            result: Ok(_),
            ..
        } = &*state
        else {
            return Err(ownership_error());
        };
        if !admission.request.belongs_to_service(service) {
            return Err(ownership_error());
        }
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        let State::RecordedRegistrationEnd {
            admission,
            result: Ok(written),
            ..
        } = previous
        else {
            return Err(ownership_error());
        };
        let Written { opened, verified } = *written;
        let current = admission
            .request
            .finish(service, verified)
            .map_err(lower_error);
        *state = State::Idle(opened);
        current
    }

    /// Starts one actual write using the retained original anchor/preimage cell.
    fn spawn_registration_end(
        &self,
        store: Option<MmpCustodyStore>,
        admission: &Admission,
    ) -> JoinHandle<Result<Written>> {
        let destination = self.0.destination.clone();
        let ownership = self.0.ownership.clone();
        let scope = self.0.storage_scope.clone();
        let anchor = admission.anchor.clone();
        let plan = admission.plan.clone();
        let write = admission.request.write();
        #[cfg(test)]
        let probe = self.0.probe.clone();
        self.0.runtime.spawn_blocking(move || {
            if !write.belongs_to(&scope) {
                return Err(ownership_error());
            }
            let fence = ownership
                .lock()
                .map_err(|_| ownership_error())?
                .as_ref()
                .cloned()
                .ok_or_else(ownership_error)?;
            fence.validate_root()?;
            let mut store = match store {
                Some(store) => store,
                None => MmpCustodyStore::open_existing(
                    &destination.root,
                    &destination.owner,
                    destination.limits,
                )?,
            };
            let verified = {
                let mut owned = fence.bind_store(&mut store)?;
                if plan.get().is_none() {
                    if !owned.store_mut().acceptance_plan(0)?.same_position(&anchor) {
                        return Err(ownership_error());
                    }
                    let captured = owned.store_mut().registration_end_plan(write.snapshot())?;
                    plan.set(captured).map_err(|_| ownership_error())?;
                }
                #[cfg(test)]
                if let Some(probe) = &probe {
                    probe.before_write();
                }
                let original = plan.get().ok_or_else(ownership_error)?;
                let verified = write
                    .persist(&mut owned.store_mut().registration_end_port(original))
                    .map_err(lower_error)?;
                owned.revalidate()?;
                verified
            };
            #[cfg(test)]
            if let Some(probe) = &probe {
                probe.after_payload_read();
            }
            {
                let _bound = fence.bind_store(&mut store)?;
            }
            plan.get()
                .ok_or_else(ownership_error)?
                .validate_settled(&store)?;
            let anchor = store.acceptance_plan(0)?;
            Ok(Written {
                opened: Opened { store, anchor },
                verified,
            })
        })
    }
}

/// Observes only the retained actual worker; cancellation cannot steal its input.
pub(super) fn poll_join(state: &mut State, cx: &mut Context<'_>) -> Poll<()> {
    let State::RecordingRegistrationEnd { worker, .. } = state else {
        return Poll::Ready(());
    };
    let result = match Pin::new(worker).poll(cx) {
        Poll::Pending => return Poll::Pending,
        Poll::Ready(result) => result.map_err(|_| worker_error()).and_then(|r| r),
    };
    let previous = std::mem::replace(state, State::Unavailable(ownership_error()));
    if let State::RecordingRegistrationEnd { mut admission, .. } = previous {
        if result.is_err() {
            admission.failures = admission.failures.saturating_add(1);
        }
        let retry_at = Instant::now() + retry_delay(admission.failures);
        *state = State::RecordedRegistrationEnd {
            admission,
            result: result.map(Box::new),
            retry_at,
        };
    }
    Poll::Ready(())
}
