//! Exact shell and approved-external budget claims inside the existing accounting
//! writer transaction. The existing offer owns its atomic cell; the original
//! query owns the only reservation guard. There is no second execution queue,
//! inference registration, durable effect replay or lock over actor handoff.

use super::*;
use crate::runtime::RuntimeNativeShellDispatch;
use std::sync::atomic::{AtomicU8, Ordering};

/// Shared identity of ONE original effect offer. States are available (0),
/// preparing (1), natively claimed (2), and transferred to the actor (3).
/// A snapshot/clone cannot perform or recreate the sealed claim transition.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeBudgetEffectClaim(Arc<AtomicU8>);

impl PartialEq for RuntimeBudgetEffectClaim {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for RuntimeBudgetEffectClaim {}

impl RuntimeBudgetEffectClaim {
    fn new() -> Self {
        Self(Arc::new(AtomicU8::new(0)))
    }

    fn reserve(&self) -> Option<ClaimReservation> {
        self.0
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .ok()?;
        Some(ClaimReservation {
            intent: self.clone(),
            handed_off: false,
        })
    }

    /// Only the writer-ordered callback invokes this transition.
    fn claim(&self) {
        let _ = self
            .0
            .compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub(in crate::runtime::agent) fn transferred(&self) -> bool {
        self.0.load(Ordering::SeqCst) == 3
    }
}

#[derive(Debug)]
struct ClaimReservation {
    intent: RuntimeBudgetEffectClaim,
    handed_off: bool,
}

impl Drop for ClaimReservation {
    fn drop(&mut self) {
        if !self.handed_off {
            // One CAS only: after releasing this reservation a new owner may
            // reserve immediately, and this guard must never touch its state.
            let state = self.intent.0.load(Ordering::SeqCst);
            if (1..=3).contains(&state) {
                let _ =
                    self.intent
                        .0
                        .compare_exchange(state, 0, Ordering::SeqCst, Ordering::SeqCst);
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum RuntimeBudgetNativeClaimPreparation {
    Disabled,
    Refused,
    Work(Box<RuntimeBudgetNativeClaimWork>),
}

#[derive(Debug)]
pub(crate) struct RuntimeBudgetNativeClaimWork {
    work: Box<RuntimeBudgetActivityWork>,
    reservation: ClaimReservation,
    action: String,
    source: ClaimSource,
    #[cfg(test)]
    writer_gate: Option<(Arc<tokio::sync::Notify>, std::sync::mpsc::Receiver<()>)>,
}

/// Noncloneable original result. Dropping it before actor acceptance positively
/// withdraws only this never-transferred claim; it never executes an effect.
#[derive(Debug)]
pub(crate) struct RuntimeBudgetNativeClaim {
    read: Box<RuntimeBudgetActivityRead>,
    reservation: ClaimReservation,
    action: String,
    source: ClaimSource,
}

/// Native budget authority binds either the exact shell marker/cancellation or
/// the original external approval allocation, never just action identifier text.
#[derive(Debug)]
enum ClaimSource {
    Shell {
        marker: String,
        cancellation: crate::runtime::processes::native_cancellation::NativeActionCancellation,
    },
    External {
        offer: Weak<()>,
    },
}

impl ClaimSource {
    fn is_live(&self) -> bool {
        match self {
            Self::Shell { cancellation, .. } => !cancellation.flag().load(Ordering::SeqCst),
            Self::External { offer } => offer.strong_count() > 0,
        }
    }
}

impl RuntimeBudgetNativeClaim {
    /// Original source identity for settling only its actor offer bookkeeping.
    pub(crate) fn subject(&self) -> (&str, &str) {
        (&self.read.work.turn.turn_id, &self.action)
    }
}

impl RuntimeBudgetNativeClaimWork {
    pub(crate) fn failure_capture(&self) -> RuntimeBudgetActivityFailure {
        self.work.failure_capture()
    }

    #[cfg(test)]
    pub(crate) fn gate_native_writer_for_tests(
        &mut self,
    ) -> (Arc<tokio::sync::Notify>, std::sync::mpsc::Sender<()>) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let (release, receiver) = std::sync::mpsc::channel();
        self.writer_gate = Some((entered.clone(), receiver));
        (entered, release)
    }

    /// Runs on the original blocking worker, never on the actor. The callback
    /// changes only the original offer's atomic state while writers are ordered.
    pub(crate) fn execute(self) -> Box<RuntimeBudgetNativeClaim> {
        let Self {
            work,
            reservation,
            action,
            source,
            #[cfg(test)]
            writer_gate,
        } = self;
        #[cfg(test)]
        if let Some((entered, receiver)) = writer_gate {
            entered.notify_one();
            assert!(
                receiver
                    .recv_timeout(std::time::Duration::from_secs(8))
                    .is_ok(),
                "native claim fixture gate unavailable"
            );
        }
        #[cfg(test)]
        if work.panic_reader {
            panic!("private budget activity reader fixture panic");
        }
        let observed = work.owner.store.claim_budget_activity(
            &work.owner.member,
            &work.owner.binding_owner,
            &work.owner.lease,
            || {
                if work.identity.strong_count() > 0 && source.is_live() {
                    reservation.intent.claim();
                }
            },
        );
        Box::new(RuntimeBudgetNativeClaim {
            read: Box::new(RuntimeBudgetActivityRead { work, observed }),
            reservation,
            action,
            source,
        })
    }
}

impl RuntimeSessionService {
    pub(crate) fn prepare_budget_native_shell_claim(
        &mut self,
        turn: &str,
        action: &str,
    ) -> RuntimeBudgetNativeClaimPreparation {
        if !self.agent.budget_memberships.enabled {
            return RuntimeBudgetNativeClaimPreparation::Disabled;
        }
        let Some(candidate) = self.budget_native_shell_candidate(turn, action) else {
            return RuntimeBudgetNativeClaimPreparation::Refused;
        };
        let marker = candidate.marker.clone();
        let cancellation = candidate.cancellation.clone();
        let Some(pending) = self
            .agent
            .pending_native_shell_dispatches
            .get_mut(&(turn.into(), action.into()))
        else {
            return RuntimeBudgetNativeClaimPreparation::Refused;
        };
        let intent = pending
            .budget_claim
            .get_or_insert_with(RuntimeBudgetEffectClaim::new);
        let Some(reservation) = intent.reserve() else {
            return RuntimeBudgetNativeClaimPreparation::Refused;
        };
        match self.prepare_budget_activity_claim(turn) {
            RuntimeBudgetActivityPreparation::Read(work) => {
                RuntimeBudgetNativeClaimPreparation::Work(Box::new(RuntimeBudgetNativeClaimWork {
                    work,
                    reservation,
                    action: action.into(),
                    source: ClaimSource::Shell {
                        marker,
                        cancellation,
                    },
                    #[cfg(test)]
                    writer_gate: None,
                }))
            }
            _ => RuntimeBudgetNativeClaimPreparation::Refused,
        }
    }

    /// Adopts the original observation and transfers only an already claimed
    /// exact offer. A later numeric crossing cannot revoke this issued budget
    /// claim; human pause, cancellation and all ordinary owner gates still do.
    pub(crate) fn finish_budget_native_shell_claim(
        &mut self,
        claim: RuntimeBudgetNativeClaim,
    ) -> Result<Option<RuntimeNativeShellDispatch>> {
        let RuntimeBudgetNativeClaim {
            read,
            mut reservation,
            action,
            source,
        } = claim;
        let ClaimSource::Shell { marker, .. } = source else {
            return Ok(None);
        };
        let turn = read.work.turn.turn_id.clone();
        let transaction_ok = read.observed.is_ok();
        if !self.apply_budget_activity_read(*read)
            || !transaction_ok
            || self.budget_activity_operationally_inhibits_turn(&turn)
            || reservation
                .intent
                .0
                .compare_exchange(2, 3, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return Ok(None);
        }
        let result = self.claim_native_shell_action_with_budget(
            &turn,
            &action,
            &marker,
            &reservation.intent,
        )?;
        reservation.handed_off = result.is_some();
        Ok(result)
    }

    pub(crate) fn prepare_budget_external_claim(
        &mut self,
        turn: &str,
        action: &str,
    ) -> RuntimeBudgetNativeClaimPreparation {
        if !self.agent.budget_memberships.enabled {
            return RuntimeBudgetNativeClaimPreparation::Disabled;
        }
        if !self.budget_external_candidate(turn, action) {
            return RuntimeBudgetNativeClaimPreparation::Refused;
        }
        let Some(pending) = self
            .agent
            .pending_approved_external_actions
            .get_mut(&(turn.into(), action.into()))
        else {
            return RuntimeBudgetNativeClaimPreparation::Refused;
        };
        let offer = Arc::downgrade(&pending.identity);
        let Some(reservation) = pending
            .budget_claim
            .get_or_insert_with(RuntimeBudgetEffectClaim::new)
            .reserve()
        else {
            return RuntimeBudgetNativeClaimPreparation::Refused;
        };
        match self.prepare_budget_activity_claim(turn) {
            RuntimeBudgetActivityPreparation::Read(work) => {
                RuntimeBudgetNativeClaimPreparation::Work(Box::new(RuntimeBudgetNativeClaimWork {
                    work,
                    reservation,
                    action: action.into(),
                    source: ClaimSource::External { offer },
                    #[cfg(test)]
                    writer_gate: None,
                }))
            }
            _ => RuntimeBudgetNativeClaimPreparation::Refused,
        }
    }

    pub(crate) fn finish_budget_external_claim(
        &mut self,
        claim: RuntimeBudgetNativeClaim,
    ) -> Result<Option<crate::runtime::RuntimeApprovedExternalActionDispatch>> {
        let RuntimeBudgetNativeClaim {
            read,
            mut reservation,
            action,
            source,
        } = claim;
        let ClaimSource::External { offer } = source else {
            return Ok(None);
        };
        let turn = read.work.turn.turn_id.clone();
        let transaction_ok = read.observed.is_ok();
        if !self.apply_budget_activity_read(*read)
            || !transaction_ok
            || self.budget_activity_operationally_inhibits_turn(&turn)
            || reservation
                .intent
                .0
                .compare_exchange(2, 3, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return Ok(None);
        }
        let result = self.claim_approved_external_action_with_budget(
            &turn,
            &action,
            &offer,
            &reservation.intent,
        )?;
        reservation.handed_off = result.is_some();
        Ok(result)
    }
}
