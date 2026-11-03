//! Original ACK jobs on the same exclusive root-owned custody connection.
//!
//! Acceptance and ACK states share one lane: no job can take the connection or
//! settled anchor until its predecessor is actually joined and canonically
//! adopted. Hashing/storage/position reads stay off actor; lower reservation and
//! issuance are bounded metadata transitions. Stop preserves these drains because
//! their trusted caller already committed accepted input to the application.
//! Observer loss, worker panic and storage error retain the original ACK attempt
//! and plan; retry never reinjects input or creates application/native authority.

use std::task::Context;

use mez_agent::messaging::{
    DeliveryCursor, MessageAcknowledgementAttempt, MessageAcknowledgementJoin,
    MessageAcknowledgementTicket, MessageAcknowledgementWrite, MessageAcknowledgementWriteOutcome,
};

use super::*;
use crate::storage::mmp_custody::acknowledgement::AcknowledgementPlan;

/// Original metadata retained outside every abortable ACK worker/observer.
pub(super) struct Admission {
    /// Original private store allocation and physical generation.
    plan: AcknowledgementPlan,
    /// Exact current lower attempt, not recipient text or a completed status bit.
    attempt: MessageAcknowledgementAttempt,
    /// Completed failure count for monotonic non-catch-up retry spacing.
    failures: u32,
}

/// Actual joined physical result, retained until original lower adoption.
pub(super) struct Written {
    /// Same serialized connection returned by its sole actual worker.
    store: MmpCustodyStore,
    /// Original opaque commit/uncertainty, never an inferred ACK.
    pub(super) outcome: MessageAcknowledgementWriteOutcome,
    /// Next settled position read off actor after the original operation.
    anchor: Result<AcceptancePlan>,
}

impl MmpCustodyRoot {
    /// Issues a prepared original ACK after securing the root's idle connection.
    /// Busy errors leave lower issuance unchanged. Stop permits accepted-input
    /// drains; caller must prove actual application commit before preparing ACK.
    pub fn start_acknowledgement(
        &self,
        service: &mut MessageService,
        ticket: &MessageAcknowledgementTicket,
    ) -> Result<()> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Idle(opened) = &*state else {
            return Err(MezError::conflict(
                "MMP custody worker already owned or unavailable",
            ));
        };
        let plan = opened.anchor.for_acknowledgement();
        let write = service
            .begin_acknowledgement_write(ticket)
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
        let worker = self.spawn_acknowledgement(Some(opened.store), admission.plan.clone(), write);
        *state = State::Acknowledging { admission, worker };
        Ok(())
    }

    /// Retries only an actually joined original after its observed-failure delay.
    /// Current lower allocation validation precedes consuming uncertain input.
    /// Lost input is recovered solely through its exact observed actual join.
    pub fn retry_acknowledgement(
        &self,
        service: &mut MessageService,
        now: Instant,
    ) -> Result<bool> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Acknowledged {
            admission,
            result,
            retry_at,
        } = &*state
        else {
            return Ok(false);
        };
        if now < *retry_at
            || matches!(
                result.as_deref(),
                Ok(Written {
                    outcome: MessageAcknowledgementWriteOutcome::Committed(_),
                    ..
                })
            )
        {
            return Ok(false);
        }
        service
            .validate_acknowledgement_attempt(&admission.attempt)
            .map_err(lower_error)?;
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        let State::Acknowledged {
            mut admission,
            result,
            ..
        } = previous
        else {
            return Err(ownership_error());
        };
        let (store, recovered) = match result.map(|written| *written) {
            Ok(Written {
                store,
                outcome: MessageAcknowledgementWriteOutcome::Unknown(unknown),
                ..
            }) => (Some(store), service.retry_acknowledgement_write(unknown)),
            Err(_) => {
                let mut joined = ObservedJoin(admission.attempt.clone());
                (
                    None,
                    service.recover_joined_acknowledgement_write(&admission.attempt, &mut joined),
                )
            }
            _ => return Err(ownership_error()),
        };
        // Validation and transfer use one exclusive service borrow. Even an
        // invariant fault retains joined escrow for later exact reconciliation.
        let write = match recovered {
            Ok(write) => write,
            Err(error) => {
                let error = lower_error(error);
                self.0.runtime.spawn_blocking(move || drop(store));
                let retry_at = Instant::now() + retry_delay(admission.failures);
                *state = State::Acknowledged {
                    admission,
                    result: Err(error.clone()),
                    retry_at,
                };
                return Err(error);
            }
        };
        admission.attempt = write.attempt();
        let worker = self.spawn_acknowledgement(store, admission.plan.clone(), write);
        *state = State::Acknowledging { admission, worker };
        Ok(true)
    }

    /// Publishes the actual positive ACK to its original lower owner. Wrong
    /// adopters retain the positive fact and cannot free this connection/anchor.
    /// No context/provider action is executed or replayed by this transition.
    pub fn adopt_acknowledgement(&self, service: &mut MessageService) -> Result<DeliveryCursor> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Acknowledged {
            result: Ok(written),
            ..
        } = &*state
        else {
            return Err(ownership_error());
        };
        let MessageAcknowledgementWriteOutcome::Committed(commit) = &written.outcome else {
            return Err(ownership_error());
        };
        let cursor = service
            .publish_acknowledgement(commit)
            .map_err(lower_error)?;
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        if let State::Acknowledged {
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
        Ok(cursor)
    }

    /// Transfers physical input to one root-owned worker with immutable plan
    /// escrow already retained. Opening/hash/storage/position reads are off actor.
    fn spawn_acknowledgement(
        &self,
        store: Option<MmpCustodyStore>,
        plan: AcknowledgementPlan,
        write: MessageAcknowledgementWrite,
    ) -> JoinHandle<Result<Written>> {
        let destination = self.0.destination.clone();
        let ownership = self.0.ownership.clone();
        #[cfg(test)]
        let probe = self.0.ack_probe.clone();
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
            // Discard only a connection already proven foreign by its private
            // allocation. Reopen the trusted path under the ORIGINAL plan; a
            // same-label replacement still cannot gain authority from retry.
            let store = store.filter(|store| plan.matches_store(store));
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
                write.persist(&mut owned.store_mut().acknowledgement_port(&plan))
            };
            // Do not publish an ACK result if its bound path changed while the
            // worker was active; the root retains the original for settlement.
            {
                let _bound = fence.bind_store(&mut store)?;
            }
            #[cfg(test)]
            if let Some(probe) = &probe {
                probe.after_write(&outcome);
            }
            let anchor = store.acceptance_plan(0);
            Ok(Written {
                store,
                outcome,
                anchor,
            })
        })
    }
}

/// Polls only the actual retained ACK handle; the parent owns the observer slot.
pub(super) fn poll_join(state: &mut State, cx: &mut Context<'_>) -> Poll<()> {
    let State::Acknowledging { worker, .. } = state else {
        return Poll::Ready(());
    };
    let result = match Pin::new(worker).poll(cx) {
        Poll::Pending => return Poll::Pending,
        Poll::Ready(result) => result.map_err(|_| worker_error()).and_then(|result| result),
    };
    let previous = std::mem::replace(state, State::Unavailable(ownership_error()));
    if let State::Acknowledging { mut admission, .. } = previous {
        if !matches!(
            &result,
            Ok(Written {
                outcome: MessageAcknowledgementWriteOutcome::Committed(_),
                ..
            })
        ) {
            admission.failures = admission.failures.saturating_add(1);
        }
        let retry_at = Instant::now() + retry_delay(admission.failures);
        *state = State::Acknowledged {
            admission,
            result: result.map(Box::new),
            retry_at,
        };
    }
    Poll::Ready(())
}

/// Borrows original failure diagnostics without consuming payload/input ownership.
pub(super) fn failure(state: &State) -> Option<MezError> {
    match state {
        State::Acknowledged {
            result: Err(error), ..
        } => Some(error.clone()),
        State::Acknowledged {
            result: Ok(written),
            ..
        } => match &written.outcome {
            MessageAcknowledgementWriteOutcome::Unknown(unknown) => {
                Some(lower_error(unknown.error().clone()))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Constructed only after the root actually polled this exact handle to completion.
struct ObservedJoin(MessageAcknowledgementAttempt);
impl MessageAcknowledgementJoin for ObservedJoin {
    /// Qualifies the same private allocation/nonce; text/status is insufficient.
    fn qualify_joined(
        &mut self,
        attempt: &MessageAcknowledgementAttempt,
    ) -> mez_agent::messaging::Result<()> {
        if self.0.same_attempt(attempt) {
            Ok(())
        } else {
            Err(MessageError::invalid_state("MMP ACK original join changed"))
        }
    }
}

#[cfg(test)]
pub(super) mod tests;
