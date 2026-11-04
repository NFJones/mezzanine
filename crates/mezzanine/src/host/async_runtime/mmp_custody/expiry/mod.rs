//! Original expiry work on the existing root's exclusive physical lane.
//!
//! Issuance retains exact lower escrow and the first settled position before a
//! worker can run. Observers never take its handle or linear input. Unknown
//! writes and actual joined worker loss retain the source/slots/clock and retry
//! only the original plan. A joined positive remains owned through position-read
//! errors and adoption; neither status nor a decoder supplies terminal authority.
//! Stop rejects fresh selection but permits issued reconciliation and adoption.

use std::task::Context;

use mez_agent::messaging::{
    MessageExpiryAttempt, MessageExpiryJoin, MessageExpiryTicket, MessageExpiryWrite,
    MessageExpiryWriteOutcome,
};

use super::*;
use crate::storage::mmp_custody::expiry::ExpiryPlan;

/// Exact first plan and lower decoder/source escrow outside abortable workers.
pub(super) struct Admission {
    plan: ExpiryPlan,
    attempt: MessageExpiryAttempt,
    failures: u32,
}
/// Joined physical result retained until original lower adoption.
pub(super) struct Written {
    store: MmpCustodyStore,
    pub(super) outcome: MessageExpiryWriteOutcome,
    pub(super) anchor: Result<AcceptancePlan>,
}

impl MmpCustodyRoot {
    /// Issues one original reservation only after securing the idle root. Busy
    /// or stopping refusal leaves lower issuance untouched and cancellable.
    pub fn start_expiry(
        &self,
        service: &mut MessageService,
        ticket: &MessageExpiryTicket,
    ) -> Result<()> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Idle(opened) = &*state else {
            return Err(MezError::conflict("MMP expiry requires an idle lane"));
        };
        if self.0.stopping.load(Ordering::Acquire) {
            return Err(ownership_error());
        }
        let plan = opened.anchor.for_expiry();
        let write = service
            .begin_storage_expiry_write(ticket, &self.0.storage_scope)
            .map_err(lower_error)?;
        let admission = Admission {
            plan,
            attempt: write.attempt(),
            failures: 0,
        };
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        let State::Idle(opened) = previous else {
            return Err(ownership_error());
        };
        let worker = self.spawn_expiry(Some(opened.store), admission.plan.clone(), write);
        *state = State::Expiring { admission, worker };
        Ok(())
    }

    /// Retries only after the actual original join and monotonic backoff. A
    /// positive with a failed position read refreshes that read without dropping
    /// its commit or issuing another lower write. Stop still permits this drain.
    pub fn retry_expiry(&self, service: &mut MessageService, now: Instant) -> Result<bool> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Expired {
            admission,
            result,
            retry_at,
        } = &*state
        else {
            return Ok(false);
        };
        if now < *retry_at {
            return Ok(false);
        }
        if matches!(
            result.as_deref(),
            Ok(Written {
                outcome: MessageExpiryWriteOutcome::Committed(_),
                anchor: Ok(_),
                ..
            })
        ) {
            return Ok(false);
        }
        service
            .validate_expiry_attempt(&admission.attempt)
            .map_err(lower_error)?;
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        let State::Expired {
            mut admission,
            result,
            ..
        } = previous
        else {
            return Err(ownership_error());
        };
        if let Ok(written) = &result
            && matches!(written.outcome, MessageExpiryWriteOutcome::Committed(_))
        {
            let written = *result.map_err(|_| ownership_error())?;
            let ownership = self.0.ownership.clone();
            let worker = self.0.runtime.spawn_blocking(move || {
                let mut written = written;
                written.anchor = (|| {
                    let fence = original_fence(&ownership)?;
                    let _bound = fence.bind_store(&mut written.store)?;
                    written.store.acceptance_plan(0)
                })();
                Ok(written)
            });
            *state = State::Expiring { admission, worker };
            return Ok(true);
        }
        let (store, recovered) = match result.map(|written| *written) {
            Ok(Written {
                store,
                outcome: MessageExpiryWriteOutcome::Unknown(unknown),
                ..
            }) => (Some(store), service.retry_expiry_write(unknown)),
            Err(_) => {
                let mut joined = ObservedJoin(admission.attempt.clone());
                (
                    None,
                    service.recover_joined_expiry_write(&admission.attempt, &mut joined),
                )
            }
            _ => return Err(ownership_error()),
        };
        let write = match recovered {
            Ok(write) => write,
            Err(error) => {
                let error = lower_error(error);
                self.0.runtime.spawn_blocking(move || drop(store));
                let retry_at = Instant::now() + retry_delay(admission.failures);
                *state = State::Expired {
                    admission,
                    result: Err(error.clone()),
                    retry_at,
                };
                return Err(error);
            }
        };
        admission.attempt = write.attempt();
        let worker = self.spawn_expiry(store, admission.plan.clone(), write);
        *state = State::Expiring { admission, worker };
        Ok(true)
    }

    /// Publishes only the exact joined positive after every fallible physical
    /// position check. Foreign adopters retain the result and exclusive lane.
    pub fn adopt_expiry(&self, service: &mut MessageService) -> Result<usize> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Expired {
            result: Ok(written),
            ..
        } = &*state
        else {
            return Err(ownership_error());
        };
        let MessageExpiryWriteOutcome::Committed(commit) = &written.outcome else {
            return Err(ownership_error());
        };
        written.anchor.as_ref().map_err(Clone::clone)?;
        let count = service.publish_expiry(commit).map_err(lower_error)?;
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        if let State::Expired {
            result: Ok(written),
            ..
        } = previous
        {
            *state = match written.anchor {
                Ok(anchor) => State::Idle(Opened {
                    store: written.store,
                    anchor,
                }),
                Err(error) => State::Unavailable(error),
            };
        }
        Ok(count)
    }

    /// All store opening, hashing, commits and position reads stay off actor.
    fn spawn_expiry(
        &self,
        store: Option<MmpCustodyStore>,
        plan: ExpiryPlan,
        write: MessageExpiryWrite,
    ) -> JoinHandle<Result<Written>> {
        let destination = self.0.destination.clone();
        let ownership = self.0.ownership.clone();
        let scope = self.0.storage_scope.clone();
        #[cfg(test)]
        let probe = self.0.expiry_probe.clone();
        self.0.runtime.spawn_blocking(move || {
            #[cfg(test)]
            if let Some(probe) = &probe {
                probe.before_write();
            }
            let fence = original_fence(&ownership)?;
            let mut store = match store {
                Some(store) => store,
                None => MmpCustodyStore::open_existing(
                    &destination.root,
                    &destination.owner,
                    destination.limits,
                )?,
            };
            let outcome = {
                let mut owned = fence.bind_store(&mut store)?;
                write.persist(&mut owned.expiry_port(&plan, &scope))
            };
            #[cfg(test)]
            if matches!(&outcome, MessageExpiryWriteOutcome::Committed(_))
                && let Some(probe) = &probe
            {
                probe.after_payload_read();
            }
            // A failed physical recheck remains part of this retained positive;
            // adoption cannot consume it until the original check succeeds.
            let anchor = (|| {
                let _bound = fence.bind_store(&mut store)?;
                store.acceptance_plan(0)
            })();
            Ok(Written {
                store,
                outcome,
                anchor,
            })
        })
    }
}

/// Requires the retained actual root fence, never an owner label or new lock.
fn original_fence(ownership: &Mutex<Option<Arc<RootOwnership>>>) -> Result<Arc<RootOwnership>> {
    let fence = ownership
        .lock()
        .map_err(|_| ownership_error())?
        .as_ref()
        .cloned()
        .ok_or_else(ownership_error)?;
    fence.validate_root()?;
    Ok(fence)
}

/// Observes only the retained actual worker and installs its exact disposition.
pub(super) fn poll_join(state: &mut State, cx: &mut Context<'_>) -> Poll<()> {
    let State::Expiring { worker, .. } = state else {
        return Poll::Ready(());
    };
    let result = match Pin::new(worker).poll(cx) {
        Poll::Pending => return Poll::Pending,
        Poll::Ready(result) => result.map_err(|_| worker_error()).and_then(|result| result),
    };
    let previous = std::mem::replace(state, State::Unavailable(ownership_error()));
    if let State::Expiring { mut admission, .. } = previous {
        if !matches!(
            result.as_ref(),
            Ok(Written {
                outcome: MessageExpiryWriteOutcome::Committed(_),
                anchor: Ok(_),
                ..
            })
        ) {
            admission.failures = admission.failures.saturating_add(1);
        }
        let retry_at = Instant::now() + retry_delay(admission.failures);
        *state = State::Expired {
            admission,
            result: result.map(Box::new),
            retry_at,
        };
    }
    Poll::Ready(())
}

/// Borrows diagnostics without consuming input, positive or physical ownership.
pub(super) fn failure(state: &State) -> Option<MezError> {
    match state {
        State::Expired {
            result: Err(error), ..
        } => Some(error.clone()),
        State::Expired {
            result: Ok(written),
            ..
        } => match &written.outcome {
            MessageExpiryWriteOutcome::Unknown(unknown) => {
                Some(lower_error(unknown.error().clone()))
            }
            _ => written.anchor.as_ref().err().cloned(),
        },
        _ => None,
    }
}

/// Constructed only after the root has actually observed this handle's join.
struct ObservedJoin(MessageExpiryAttempt);
impl MessageExpiryJoin for ObservedJoin {
    fn qualify_joined(
        &mut self,
        attempt: &MessageExpiryAttempt,
    ) -> mez_agent::messaging::Result<()> {
        if self.0.same_attempt(attempt) {
            Ok(())
        } else {
            Err(MessageError::invalid_state(
                "MMP expiry original join changed",
            ))
        }
    }
}
