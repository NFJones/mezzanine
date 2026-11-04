//! Native continuity writes on the same original root's exclusive worker lane.
//!
//! Actual registration snapshots and metadata leases remain outside the worker
//! through observer cancellation and lost replies. The first row preimage is
//! frozen before I/O and retries reconcile only that exact batch after real join.
//! Positive adoption marks only the actual still-current binding; stored facts
//! alone grant no registration, reconnect, context or provider authority.

use std::sync::OnceLock;
use std::task::Context;

use mez_agent::messaging::{MessageNativeContinuityRequest, MessageNativeContinuityVerified};
use mez_core::ids::AgentId;

use super::*;
use crate::storage::mmp_custody::native_continuity::NativeContinuityPlan;

/// Original metadata input and row plan retained until actual join/adoption.
pub(super) struct Admission {
    request: MessageNativeContinuityRequest,
    anchor: AcceptancePlan,
    plan: Arc<OnceLock<NativeContinuityPlan>>,
    failures: u32,
}
/// Actual positive commit and updated store position held for exact adoption.
pub(super) struct Written {
    opened: Opened,
    verified: MessageNativeContinuityVerified,
}

impl MmpCustodyRoot {
    /// Captures one actual native interval while the root is idle and all lower
    /// admission/ACK reservations are settled. This is not a wire operation.
    pub fn start_native_continuity(&self, service: &MessageService, id: &AgentId) -> Result<()> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Idle(opened) = &*state else {
            return Err(MezError::conflict(
                "MMP native continuity requires an idle lane",
            ));
        };
        if self.0.stopping.load(Ordering::Acquire) {
            return Err(ownership_error());
        }
        let request = service
            .prepare_native_continuity(&self.0.storage_scope, id)
            .map_err(lower_error)?;
        let admission = Admission {
            request,
            anchor: opened.anchor.clone(),
            plan: Arc::new(OnceLock::new()),
            failures: 0,
        };
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        if let State::Idle(opened) = previous {
            let worker = self.spawn_native_continuity(Some(opened.store), &admission);
            *state = State::RecordingNative { admission, worker };
        }
        Ok(())
    }

    /// Retries only the same input/preimage after actual loss joined and backoff.
    pub fn retry_native_continuity(&self, now: Instant) -> Result<bool> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::RecordedNative {
            result, retry_at, ..
        } = &*state
        else {
            return Err(ownership_error());
        };
        if result.is_ok() {
            return Err(MezError::conflict(
                "MMP native continuity awaits original adoption",
            ));
        }
        if now < *retry_at {
            return Ok(false);
        }
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        if let State::RecordedNative { admission, .. } = previous {
            let worker = self.spawn_native_continuity(None, &admission);
            *state = State::RecordingNative { admission, worker };
        }
        Ok(true)
    }

    /// Publishes the exact joined fact for this runtime. Returns false when the
    /// actual binding changed during I/O, leaving its successor unconfirmed.
    /// A foreign adopter cannot consume the original positive input/result.
    pub fn adopt_native_continuity(&self, service: &mut MessageService) -> Result<bool> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::RecordedNative {
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
        let State::RecordedNative {
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
    fn spawn_native_continuity(
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
            let floor = write.snapshot().registered_after_sequence;
            let verified = {
                let mut owned = fence.bind_store(&mut store)?;
                if plan.get().is_none() {
                    if !owned.store_mut().acceptance_plan(0)?.same_position(&anchor) {
                        return Err(ownership_error());
                    }
                    let captured = owned
                        .store_mut()
                        .native_continuity_plan(&write.snapshot().native.agent_id)?;
                    plan.set(captured).map_err(|_| ownership_error())?;
                }
                #[cfg(test)]
                if let Some(probe) = &probe {
                    probe.before_write();
                }
                let original = plan.get().ok_or_else(ownership_error)?;
                let verified = write
                    .persist(&mut owned.store_mut().native_continuity_port(original))
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
                .validate_settled(&store, floor)?;
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
    let State::RecordingNative { worker, .. } = state else {
        return Poll::Ready(());
    };
    let result = match Pin::new(worker).poll(cx) {
        Poll::Pending => return Poll::Pending,
        Poll::Ready(result) => result.map_err(|_| worker_error()).and_then(|r| r),
    };
    let previous = std::mem::replace(state, State::Unavailable(ownership_error()));
    if let State::RecordingNative { mut admission, .. } = previous {
        if result.is_err() {
            admission.failures = admission.failures.saturating_add(1);
        }
        let retry_at = Instant::now() + retry_delay(admission.failures);
        *state = State::RecordedNative {
            admission,
            result: result.map(Box::new),
            retry_at,
        };
    }
    Poll::Ready(())
}
