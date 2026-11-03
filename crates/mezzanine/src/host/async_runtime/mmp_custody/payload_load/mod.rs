//! Original-source reloads on the single root-owned blocking lane.
//!
//! Reads share acceptance/ACK connection ownership, actual join observation and
//! monotonic retry spacing. A cancelled observer cannot release a worker or its
//! decoded allocation. Original store identity/position, physical ownership and
//! lower source fingerprint all gate publication. Reload never acknowledges a
//! recipient, reaccepts mail, renews TTL or consumes a provider slot.

use std::task::Context;

use mez_agent::messaging::{
    MessagePayloadLoadAttempt, MessagePayloadLoadJoin, MessagePayloadLoadResult,
    MessagePayloadLoadWrite,
};

use super::*;

/// Payload-free original retained independently of the actual read worker.
pub(super) struct Admission {
    plan: AcceptancePlan,
    attempt: MessagePayloadLoadAttempt,
    failures: u32,
}

/// Actual joined connection and private decoded bytes; no actor state changed.
pub(super) struct Written {
    opened: Opened,
    /// None means the original became obsolete before a lost read was retried.
    pub(super) outcome: Option<MessagePayloadLoadResult>,
}

impl MmpCustodyRoot {
    /// Issues an original-root reload after securing this root's idle lane.
    /// False means an existing allocation already supplies the bytes. Busy or
    /// foreign-root errors leave lower issuance unchanged. Stop permits reads
    /// needed to drain already accepted input; no new send is authorized.
    pub fn start_payload_load(&self, service: &mut MessageService, sequence: u64) -> Result<bool> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Idle(opened) = &*state else {
            return Err(MezError::conflict(
                "MMP custody worker already owned or unavailable",
            ));
        };
        let plan = opened.anchor.clone();
        let Some(write) = service
            .begin_payload_load(sequence, &self.0.storage_scope)
            .map_err(lower_error)?
        else {
            return Ok(false);
        };
        let attempt = write.attempt();
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        if let State::Idle(opened) = previous {
            let worker = self.spawn_payload_load(Some(opened.store), plan.clone(), Some(write));
            *state = State::Loading {
                admission: Admission {
                    plan,
                    attempt,
                    failures: 0,
                },
                worker,
            };
        }
        Ok(true)
    }

    /// Reissues only the original actual joined read after its backoff. Reads
    /// never mutate storage, but still retain their exact scope/source/nonce.
    pub fn retry_payload_load(&self, service: &mut MessageService, now: Instant) -> Result<bool> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Loaded {
            admission,
            result,
            retry_at,
        } = &*state
        else {
            return Err(ownership_error());
        };
        if result.as_ref().is_ok_and(|written| {
            written
                .outcome
                .as_ref()
                .is_none_or(|outcome| outcome.error().is_none())
        }) {
            return Err(MezError::conflict(
                "MMP payload read awaits original publication",
            ));
        }
        if now < *retry_at {
            return Ok(false);
        }
        let mut joined = ObservedJoin(admission.attempt.clone());
        let write = service
            .recover_payload_load(&admission.attempt, &mut joined)
            .map_err(lower_error)?;
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        if let State::Loaded {
            mut admission,
            result,
            ..
        } = previous
        {
            if let Some(write) = &write {
                admission.attempt = write.attempt();
            }
            let store = result.ok().map(|written| written.opened.store);
            let worker = self.spawn_payload_load(store, admission.plan.clone(), write);
            *state = State::Loading { admission, worker };
        }
        Ok(true)
    }

    /// Publishes joined original bytes or discards an obsolete read, then
    /// returns the same connection to idle. On error all input stays retained.
    pub fn publish_payload_load(&self, service: &mut MessageService) -> Result<bool> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Loaded {
            result: Ok(written),
            admission,
            ..
        } = &mut *state
        else {
            return Err(ownership_error());
        };
        service
            .validate_payload_load_owner(&admission.attempt)
            .map_err(lower_error)?;
        let restored = match written.outcome.as_mut() {
            Some(outcome) => service.publish_payload_load(outcome).map_err(lower_error)?,
            None => false,
        };
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        if let State::Loaded {
            result: Ok(written),
            ..
        } = previous
        {
            *state = State::Idle(written.opened);
        }
        Ok(restored)
    }

    /// Registers one actual blocking read; original metadata remains outside
    /// the closure so a panic/lost result can reconcile the same source.
    fn spawn_payload_load(
        &self,
        store: Option<MmpCustodyStore>,
        plan: AcceptancePlan,
        write: Option<MessagePayloadLoadWrite>,
    ) -> JoinHandle<Result<Written>> {
        let destination = self.0.destination.clone();
        let ownership = self.0.ownership.clone();
        #[cfg(test)]
        let probe = self.0.probe.clone();
        self.0.runtime.spawn_blocking(move || {
            #[cfg(test)]
            if let Some(probe) = &probe {
                probe.before_write();
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
            let outcome = {
                let mut owned = fence.bind_store(&mut store)?;
                if !owned.store_mut().acceptance_plan(0)?.same_position(&plan) {
                    return Err(ownership_error());
                }
                let outcome = write.map(|write| write.load(&mut owned));
                owned.revalidate()?;
                if !owned.store_mut().acceptance_plan(0)?.same_position(&plan) {
                    return Err(ownership_error());
                }
                outcome
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
            // Keep the original settled anchor: a read never advances it.
            Ok(Written {
                opened: Opened {
                    store,
                    anchor: plan,
                },
                outcome,
            })
        })
    }
}

/// Witness constructed only after the retained actual handle reached Loaded.
struct ObservedJoin(MessagePayloadLoadAttempt);
impl MessagePayloadLoadJoin for ObservedJoin {
    fn qualify_joined(
        &mut self,
        attempt: &MessagePayloadLoadAttempt,
    ) -> mez_agent::messaging::Result<()> {
        if self.0.same_attempt(attempt) {
            Ok(())
        } else {
            Err(MessageError::invalid_state(
                "MMP original payload join changed",
            ))
        }
    }
}

/// Polls only the registered actual handle; parent serialization owns observers.
pub(super) fn poll_join(state: &mut State, cx: &mut Context<'_>) -> Poll<()> {
    let State::Loading { worker, .. } = state else {
        return Poll::Ready(());
    };
    let result = match Pin::new(worker).poll(cx) {
        Poll::Pending => return Poll::Pending,
        Poll::Ready(result) => result.map_err(|_| worker_error()).and_then(|result| result),
    };
    let previous = std::mem::replace(state, State::Unavailable(ownership_error()));
    if let State::Loading { mut admission, .. } = previous {
        if !result.as_ref().is_ok_and(|written| {
            written
                .outcome
                .as_ref()
                .is_none_or(|outcome| outcome.error().is_none())
        }) {
            admission.failures = admission.failures.saturating_add(1);
        }
        let retry_at = Instant::now() + retry_delay(admission.failures);
        *state = State::Loaded {
            admission,
            result: result.map(Box::new),
            retry_at,
        };
    }
    Poll::Ready(())
}

/// Returns bounded read context while retaining original worker/source custody.
pub(super) fn failure(state: &State) -> Option<MezError> {
    match state {
        State::Loaded {
            result: Err(error), ..
        } => Some(error.clone()),
        State::Loaded {
            result: Ok(written),
            ..
        } => written
            .outcome
            .as_ref()
            .and_then(|result| result.error())
            .cloned()
            .map(lower_error),
        _ => None,
    }
}
