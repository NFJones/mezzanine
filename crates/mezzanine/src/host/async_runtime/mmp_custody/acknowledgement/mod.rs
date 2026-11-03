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
    DeliveryCursor, MessageAcceptanceUncertain, MessageAcknowledgementAttempt,
    MessageAcknowledgementJoin, MessageAcknowledgementTicket, MessageAcknowledgementWrite,
    MessageAcknowledgementWriteOutcome,
};

use super::*;
use crate::storage::mmp_custody::acknowledgement::{AcknowledgementPlan, GuardedAckSettlement};

/// Original metadata retained outside every abortable ACK worker/observer.
pub(super) struct Admission {
    /// Original private store allocation and physical generation.
    plan: AcknowledgementPlan,
    /// Exact current lower attempt, not recipient text or a completed status bit.
    attempt: MessageAcknowledgementAttempt,
    /// Completed failure count for monotonic non-catch-up retry spacing.
    failures: u32,
    /// Original uncertain acceptance retained while this guarded ACK runs.
    pub(super) parked: Option<ParkedAcceptance>,
}

/// Exact never-written acceptance and its original retry, never a new request.
pub(super) struct ParkedAcceptance {
    admission: super::Admission,
    uncertain: MessageAcceptanceUncertain,
    retry_at: Instant,
    /// Test-only witness for retention through actual ACK worker termination.
    #[cfg(test)]
    drop_witness: Option<Arc<()>>,
}

/// Actual joined physical result, retained until original lower adoption.
pub(super) struct Written {
    /// Same serialized connection returned by its sole actual worker.
    store: MmpCustodyStore,
    /// Original opaque commit/uncertainty, never an inferred ACK.
    pub(super) outcome: MessageAcknowledgementWriteOutcome,
    /// Next settled position read off actor after the original operation.
    pub(super) anchor: Result<AcceptancePlan>,
    /// Exact ACK transaction position, available only after guarded commit.
    settlement: Option<GuardedAckSettlement>,
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
        let anchor = match &*state {
            State::Idle(opened) => &opened.anchor,
            State::Joined {
                admission,
                result: Ok(written),
                ..
            } if !admission.recovery
                && written.unwritten
                && matches!(&written.outcome, MessageAcceptanceWriteOutcome::Unknown(_)) =>
            {
                service
                    .validate_acceptance_attempt(&admission.attempt)
                    .map_err(lower_error)?;
                let anchor = written.anchor.as_ref().map_err(Clone::clone)?;
                if !anchor.same_position(&admission.plan) {
                    return Err(MezError::conflict(
                        "MMP suspended acceptance position changed",
                    ));
                }
                anchor
            }
            _ => {
                return Err(MezError::conflict(
                    "MMP custody worker already owned or unavailable",
                ));
            }
        };
        let plan = anchor.for_acknowledgement();
        let write = service
            .begin_acknowledgement_write(ticket)
            .map_err(lower_error)?;
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        let (store, parked) = match previous {
            State::Idle(opened) => (opened.store, None),
            State::Joined {
                admission,
                result: Ok(written),
                retry_at,
            } => {
                let super::Written {
                    store,
                    outcome: MessageAcceptanceWriteOutcome::Unknown(uncertain),
                    ..
                } = *written
                else {
                    return Err(ownership_error());
                };
                (
                    store,
                    Some(ParkedAcceptance {
                        admission,
                        uncertain,
                        retry_at,
                        #[cfg(test)]
                        drop_witness: None,
                    }),
                )
            }
            _ => return Err(ownership_error()),
        };
        let admission = Admission {
            plan,
            attempt: write.attempt(),
            failures: 0,
            parked,
        };
        let worker = self.spawn_acknowledgement(
            Some(store),
            admission.plan.clone(),
            write,
            admission
                .parked
                .as_ref()
                .map(|parked| parked.admission.plan.clone()),
        );
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
        if now < *retry_at {
            return Ok(false);
        }
        let refresh = admission.parked.is_some()
            && matches!(
                result.as_deref(),
                Ok(Written {
                    outcome: MessageAcknowledgementWriteOutcome::Committed(_),
                    anchor: Err(_),
                    ..
                })
            );
        if !refresh
            && matches!(
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
        if let Some(parked) = admission.parked.as_ref() {
            service
                .validate_acceptance_attempt(&parked.admission.attempt)
                .map_err(lower_error)?;
        }
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        let State::Acknowledged {
            mut admission,
            result,
            ..
        } = previous
        else {
            return Err(ownership_error());
        };
        if refresh {
            let Ok(written) = result else {
                return Err(ownership_error());
            };
            let suspended = admission
                .parked
                .as_ref()
                .ok_or_else(ownership_error)?
                .admission
                .plan
                .clone();
            let worker = self.spawn_anchor_refresh(*written, suspended);
            *state = State::Acknowledging { admission, worker };
            return Ok(true);
        }
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
        let worker = self.spawn_acknowledgement(
            store,
            admission.plan.clone(),
            write,
            admission
                .parked
                .as_ref()
                .map(|parked| parked.admission.plan.clone()),
        );
        *state = State::Acknowledging { admission, worker };
        Ok(true)
    }

    /// Publishes the actual positive ACK to its original lower owner. Wrong
    /// adopters retain the positive fact and cannot free this connection/anchor.
    /// No context/provider action is executed or replayed by this transition.
    pub fn adopt_acknowledgement(&self, service: &mut MessageService) -> Result<DeliveryCursor> {
        let mut state = self.0.state.lock().map_err(|_| ownership_error())?;
        let State::Acknowledged {
            admission,
            result: Ok(written),
            ..
        } = &*state
        else {
            return Err(ownership_error());
        };
        let MessageAcknowledgementWriteOutcome::Committed(commit) = &written.outcome else {
            return Err(ownership_error());
        };
        if admission.parked.is_some() {
            written.anchor.as_ref().map_err(Clone::clone)?;
        }
        let cursor = service
            .publish_acknowledgement(commit)
            .map_err(lower_error)?;
        let previous = std::mem::replace(&mut *state, State::Unavailable(ownership_error()));
        if let State::Acknowledged {
            mut admission,
            result: Ok(written),
            ..
        } = previous
        {
            if let Some(mut parked) = admission.parked.take() {
                let anchor = written.anchor.as_ref().map_err(Clone::clone)?;
                parked.admission.plan = anchor.for_sequence(parked.admission.plan.sequence());
                *state = State::Joined {
                    admission: parked.admission,
                    result: Ok(Box::new(super::Written {
                        store: written.store,
                        outcome: MessageAcceptanceWriteOutcome::Unknown(parked.uncertain),
                        anchor: written.anchor,
                        unwritten: true,
                    })),
                    retry_at: parked.retry_at,
                };
            } else {
                *state = match written.anchor {
                    Ok(anchor) => State::Idle(Opened {
                        store: written.store,
                        anchor,
                    }),
                    Err(error) => State::Unavailable(error),
                };
            }
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
        suspended: Option<AcceptancePlan>,
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
            let (outcome, settlement) = {
                let mut owned = fence.bind_store(&mut store)?;
                if let Some(original) = suspended.as_ref() {
                    let mut port = owned
                        .store_mut()
                        .acknowledgement_port_guarded(&plan, original);
                    let outcome = write.persist(&mut port);
                    let settlement = port.take_guarded_settlement();
                    (outcome, settlement)
                } else {
                    (
                        write.persist(&mut owned.store_mut().acknowledgement_port(&plan)),
                        None,
                    )
                }
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
            let anchor = match suspended.as_ref() {
                Some(original) => match settlement.as_ref() {
                    Some(settlement) => {
                        verified_parked_anchor(&fence, &mut store, original, settlement)
                    }
                    None => Err(MezError::invalid_state(
                        "MMP guarded ACK settlement unavailable",
                    )),
                },
                None => store.acceptance_plan(0),
            };
            #[cfg(test)]
            let anchor = if suspended.is_some()
                && matches!(&outcome, MessageAcknowledgementWriteOutcome::Committed(_))
                && probe
                    .as_ref()
                    .is_some_and(|probe| probe.take_anchor_failure())
            {
                Err(MezError::invalid_state(
                    "injected guarded ACK position-read failure",
                ))
            } else {
                anchor
            };
            Ok(Written {
                store,
                outcome,
                anchor,
                settlement,
            })
        })
    }

    /// Retries only the owned post-commit position check; it keeps the actual
    /// ACK commit and suspended input intact and does not write another ACK.
    fn spawn_anchor_refresh(
        &self,
        mut written: Written,
        suspended: AcceptancePlan,
    ) -> JoinHandle<Result<Written>> {
        let ownership = self.0.ownership.clone();
        #[cfg(test)]
        let probe = self.0.ack_probe.clone();
        self.0.runtime.spawn_blocking(move || {
            #[cfg(test)]
            if probe
                .as_ref()
                .is_some_and(|probe| probe.take_anchor_refresh_panic())
            {
                panic!("injected guarded ACK position-refresh worker loss");
            }
            let anchor = (|| {
                let fence = ownership
                    .lock()
                    .map_err(|_| ownership_error())?
                    .as_ref()
                    .cloned()
                    .ok_or_else(ownership_error)?;
                let settlement = written.settlement.as_ref().ok_or_else(|| {
                    MezError::invalid_state("MMP guarded ACK settlement unavailable")
                })?;
                verified_parked_anchor(&fence, &mut written.store, &suspended, settlement)
            })();
            #[cfg(test)]
            let anchor = if probe
                .as_ref()
                .is_some_and(|probe| probe.take_anchor_failure())
            {
                Err(MezError::invalid_state(
                    "injected guarded ACK position-read failure",
                ))
            } else {
                anchor
            };
            written.anchor = anchor;
            Ok(written)
        })
    }
}

/// Rechecks the exact root, committed ACK batch and still-unwritten original.
/// A moved path, additional store write or reused sequence cannot grant rebase.
fn verified_parked_anchor(
    fence: &RootOwnership,
    store: &mut MmpCustodyStore,
    original: &AcceptancePlan,
    settlement: &GuardedAckSettlement,
) -> Result<AcceptancePlan> {
    fence
        .bind_store(store)?
        .verify_guarded_ack_settlement(original, settlement)
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
        let failed = match &result {
            Ok(Written {
                outcome: MessageAcknowledgementWriteOutcome::Committed(_),
                anchor,
                ..
            }) => admission.parked.is_some() && anchor.is_err(),
            _ => true,
        };
        if failed {
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
        } if written.anchor.is_err() => written.anchor.as_ref().err().cloned(),
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
