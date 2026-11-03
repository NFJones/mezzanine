//! Original-store checkpoint verification on the root's exclusive worker lane.
//!
//! Capture metadata stays outside the actual worker until its join is observed.
//! Cancellation cannot replace it or free the lane. Failed read-only verification
//! retries the exact capture after monotonic backoff, including after worker loss;
//! no acceptance, cursor, allocator or payload-cache state is changed.

use super::*;
use mez_agent::messaging::{
    MessageServiceSnapshot, MessageStorageSnapshotRequest, MessageStorageSnapshotVerified,
    MessageStorageSnapshotWrite,
};
use std::task::Context;

/// Exact immutable capture and position retained across actual worker loss.
pub(super) struct Admission {
    request: MessageStorageSnapshotRequest,
    plan: AcceptancePlan,
    failures: u32,
}
/// Positive read-only result, held until the original actor adopts its checkpoint.
pub(super) struct Written {
    opened: Opened,
    verified: MessageStorageSnapshotVerified,
}

impl MmpCustodyRoot {
    /// Secures the idle lane before capturing any lower metadata or issuing I/O.
    /// Busy, stopped, foreign or unsettled service state changes no owner.
    pub fn start_checkpoint(&self, service: &MessageService) -> Result<()> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Idle(opened) = &*state else {
            return Err(MezError::conflict(
                "MMP checkpoint requires an idle custody lane",
            ));
        };
        if self.0.stopping.load(Ordering::Acquire) {
            return Err(ownership_error());
        }
        let request = service
            .prepare_storage_snapshot(&self.0.storage_scope)
            .map_err(lower_error)?;
        let admission = Admission {
            request,
            plan: opened.anchor.clone(),
            failures: 0,
        };
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        if let State::Idle(opened) = previous {
            let worker = self.spawn_checkpoint(
                Some(opened.store),
                admission.plan.clone(),
                admission.request.write(),
            );
            *state = State::Snapshotting { admission, worker };
        }
        Ok(())
    }
    /// Retries only the exact capture after its actual worker joined and backoff.
    pub fn retry_checkpoint(&self, now: Instant) -> Result<bool> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Snapshotted {
            result, retry_at, ..
        } = &*state
        else {
            return Err(ownership_error());
        };
        if result.is_ok() {
            return Err(MezError::conflict(
                "MMP checkpoint awaits original adoption",
            ));
        }
        if now < *retry_at {
            return Ok(false);
        }
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        if let State::Snapshotted { admission, .. } = previous {
            let worker =
                self.spawn_checkpoint(None, admission.plan.clone(), admission.request.write());
            *state = State::Snapshotting { admission, worker };
        }
        Ok(true)
    }
    /// Moves qualified metadata only after actual join, returning the lane to idle.
    pub fn adopt_checkpoint(
        &self,
    ) -> Result<(
        MessageServiceSnapshot,
        mez_agent::messaging::MessageStorageSnapshotLease,
    )> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        if !matches!(&*state, State::Snapshotted { result: Ok(_), .. }) {
            return Err(ownership_error());
        }
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        let State::Snapshotted {
            admission,
            result: Ok(written),
            ..
        } = previous
        else {
            return Err(ownership_error());
        };
        let Written { opened, verified } = *written;
        let snapshot = admission.request.finish(verified).map_err(lower_error);
        *state = State::Idle(opened);
        snapshot
    }
    /// Installs one owned blocking verifier; the immutable capture stays retained.
    fn spawn_checkpoint(
        &self,
        store: Option<MmpCustodyStore>,
        plan: AcceptancePlan,
        write: MessageStorageSnapshotWrite,
    ) -> JoinHandle<Result<Written>> {
        let destination = self.0.destination.clone();
        let ownership = self.0.ownership.clone();
        let scope = self.0.storage_scope.clone();
        #[cfg(test)]
        let probe = self.0.probe.clone();
        self.0.runtime.spawn_blocking(move || {
            #[cfg(test)]
            if let Some(probe) = &probe {
                probe.before_write();
            }
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
                None => MmpCustodyStore::open_with_limits(
                    &destination.root,
                    &destination.owner,
                    destination.limits,
                )?,
            };
            let verified = {
                let mut owned = fence.bind_store(&mut store)?;
                if !owned.store_mut().acceptance_plan(0)?.same_position(&plan) {
                    return Err(ownership_error());
                }
                let verified = write.verify(&mut owned).map_err(lower_error)?;
                owned.revalidate()?;
                if !owned.store_mut().acceptance_plan(0)?.same_position(&plan) {
                    return Err(ownership_error());
                }
                verified
            };
            #[cfg(test)]
            if let Some(probe) = &probe {
                probe.after_payload_read();
            }
            {
                let _bound = fence.bind_store(&mut store)?;
            }
            if !store.acceptance_plan(0)?.same_position(&plan) {
                return Err(ownership_error());
            }
            Ok(Written {
                opened: Opened {
                    store,
                    anchor: plan,
                },
                verified,
            })
        })
    }
}

/// Observes only the retained actual handle; cancellation leaves all owners intact.
pub(super) fn poll_join(state: &mut State, cx: &mut Context<'_>) -> Poll<()> {
    let State::Snapshotting { worker, .. } = state else {
        return Poll::Ready(());
    };
    let result = match Pin::new(worker).poll(cx) {
        Poll::Pending => return Poll::Pending,
        Poll::Ready(result) => result.map_err(|_| worker_error()).and_then(|r| r),
    };
    let previous = std::mem::replace(state, State::Unavailable(ownership_error()));
    if let State::Snapshotting { mut admission, .. } = previous {
        if result.is_err() {
            admission.failures = admission.failures.saturating_add(1);
        }
        let retry_at = Instant::now() + retry_delay(admission.failures);
        *state = State::Snapshotted {
            admission,
            result: result.map(Box::new),
            retry_at,
        };
    }
    Poll::Ready(())
}
