//! Launch-owned Pi lifecycle sequencing independent of extension instances.
//!
//! A future privately authorized launcher owns this reducer for one immutable
//! session. Extension reload only replaces the observer epoch: it cannot reset
//! delivery sequences or reuse old callbacks. Pending reports retain exact
//! identity until matching acknowledgment, permitting presentation-only replay
//! after a lost reply. This owner has no credentials, I/O, timers, registration
//! authority or accounting and does not certify a live adapter.

use std::collections::VecDeque;

use super::pi::Observation;
use crate::error::{MezError, Result};

/// Finite pending lifecycle reports, including one reserved retirement slot.
const MAX_PENDING: usize = 32;

/// One content-free operation retained until its exact delivery is acknowledged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Delivery {
    /// Reducer incarnation; coinciding sequences in another launch are inert.
    owner: String,
    /// Monotonic launch-local operation identity, never regenerated on retry.
    pub(crate) sequence: u64,
    /// Coarse observational operation; no executable/vendor action.
    pub(crate) operation: Operation,
}

/// Observational transport work, not vendor continuation or approval authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Operation {
    /// Restricted presentation with the exact sequence carried by Delivery.
    Present(&'static str),
    /// Exact session retirement; does not claim process death.
    Retire,
}

/// One immutable privately authorized session with replaceable observer epochs.
pub(crate) struct LifecycleOwner {
    /// Unique reducer incarnation, not a credential or vendor session identity.
    incarnation: String,
    /// Bound vendor session; callbacks cannot change it.
    session: String,
    /// Current observer epoch, revoked during reload/retirement.
    observer: u64,
    /// Next delivery identity, independent of callback count and reload.
    sequence: u64,
    /// Exact pending operations, consumed only after matching acknowledgment.
    pending: VecDeque<Delivery>,
    /// Most recent accepted non-UI presentation, restored after input wait.
    state: &'static str,
    /// Candidate outcome for this run; never implies final success alone.
    candidate: Option<&'static str>,
    /// Duplicate final callbacks cannot erase the accepted terminal state.
    settled: bool,
    /// Reload suspends observation until explicit replacement attachment.
    suspended: bool,
    /// Retired session cannot be revived or rebound.
    retired: bool,
}

impl LifecycleOwner {
    /// Captures a bounded inert session from independently authorized launch.
    /// This validates spelling only; it cannot issue registration authority.
    pub(crate) fn new(session: &str) -> Result<Self> {
        if session.is_empty()
            || session.len() > 128
            || !session.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            })
        {
            return Err(MezError::invalid_args("Pi launch session unavailable"));
        }
        Ok(Self {
            incarnation: crate::storage::token_usage::new_token_usage_event_id(),
            session: session.into(),
            observer: 1,
            sequence: 0,
            pending: VecDeque::new(),
            state: "ready",
            candidate: None,
            settled: false,
            suspended: false,
            retired: false,
        })
    }

    /// Returns the current observer handle; it carries no registration authority.
    pub(crate) fn observer_epoch(&self) -> u64 {
        self.observer
    }

    /// Returns immutable reducer binding for its privately supplied transport.
    /// Neither spelling nor incarnation is a registration credential.
    pub(crate) fn transport_binding(&self) -> (&str, &str) {
        (&self.session, &self.incarnation)
    }

    /// Attaches an explicitly replaced observer to the same authorized session.
    /// Session replacement requires a distinct externally authorized owner.
    pub(crate) fn attach_after_reload(&mut self, session: &str) -> Result<u64> {
        if self.retired || !self.suspended || session != self.session {
            return Err(MezError::conflict("Pi observer replacement unavailable"));
        }
        self.suspended = false;
        Ok(self.observer)
    }

    /// Accepts one current-epoch bound observation. Queue pressure is explicit
    /// and leaves the reducer unchanged, so callers lose telemetry rather than
    /// mutate vendor decisions. Candidate facts are not sent as terminal state.
    pub(crate) fn observe(&mut self, epoch: u64, session: &str, fact: Observation) -> Result<()> {
        if self.retired || self.suspended || epoch != self.observer || session != self.session {
            return Err(MezError::conflict("Pi observation owner changed"));
        }
        match fact {
            Observation::SessionShutdown { reason: "reload" } => {
                let observer = self
                    .observer
                    .checked_add(1)
                    .ok_or_else(|| MezError::invalid_state("Pi observer epoch exhausted"))?;
                self.observer = observer;
                self.suspended = true;
                // A reload can lose before-settle chronology. Do not reuse an
                // old candidate as proof of a newly observed final outcome.
                self.candidate = None;
                self.settled = false;
                Ok(())
            }
            Observation::SessionShutdown { .. } => {
                self.enqueue(Operation::Retire, true)?;
                self.retired = true;
                self.candidate = None;
                Ok(())
            }
            Observation::CandidateOutcome { outcome } => {
                self.candidate = Some(outcome);
                Ok(())
            }
            Observation::Running => {
                self.enqueue(Operation::Present("running"), false)?;
                self.state = "running";
                self.candidate = None;
                self.settled = false;
                Ok(())
            }
            Observation::InputWait => self.enqueue(Operation::Present("input-wait"), false),
            Observation::InputEnded => self.enqueue(Operation::Present(self.state), false),
            Observation::Settled => {
                if self.settled {
                    return Ok(());
                }
                let state = match self.candidate {
                    Some("completed") => "complete",
                    Some("aborted") => "interrupted",
                    Some("error") => "failed",
                    _ => "ready", // settled is not proof of success
                };
                self.enqueue(Operation::Present(state), false)?;
                self.state = state;
                self.candidate = None;
                self.settled = true;
                Ok(())
            }
            Observation::SessionStarted { .. } => {
                self.enqueue(Operation::Present("ready"), false)?;
                self.state = "ready";
                self.candidate = None;
                self.settled = false;
                Ok(())
            }
        }
    }

    /// Retains a finite exact report without changing earlier retry identities.
    /// Retirement may use its reserved final slot, never evict pending reports.
    fn enqueue(&mut self, operation: Operation, retirement: bool) -> Result<()> {
        let limit = if retirement {
            MAX_PENDING
        } else {
            MAX_PENDING - 1
        };
        if self.pending.len() >= limit {
            return Err(MezError::invalid_state("Pi presentation queue full"));
        }
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("Pi presentation sequence exhausted"))?;
        self.pending.push_back(Delivery {
            owner: self.incarnation.clone(),
            sequence,
            operation,
        });
        self.sequence = sequence;
        Ok(())
    }

    /// Returns original pending work; failed/lost replies do not dequeue it.
    pub(crate) fn pending(&self) -> Option<&Delivery> {
        self.pending.front()
    }

    /// Consumes only the acknowledged head. Stale or out-of-order replies are
    /// inert; transport must validate successful RPC result before calling this.
    pub(crate) fn acknowledge(&mut self, delivery: &Delivery) -> bool {
        if self.pending.front() != Some(delivery) {
            return false;
        }
        self.pending.pop_front();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exhausted operation or observer counters fail before state changes;
    /// resource failures cannot fabricate settlement or enable replacement.
    #[test]
    fn pi_owner_exhaustion_preserves_pending_and_observer_state() {
        let mut owner = LifecycleOwner::new("bound").unwrap();
        owner.sequence = u64::MAX;
        assert!(owner.observe(1, "bound", Observation::Running).is_err());
        assert!(owner.pending().is_none());
        assert_eq!(owner.state, "ready");
        assert!(!owner.settled);
        owner.observer = u64::MAX;
        assert!(
            owner
                .observe(
                    u64::MAX,
                    "bound",
                    Observation::SessionShutdown { reason: "reload" }
                )
                .is_err()
        );
        assert!(!owner.suspended);
        assert_eq!(owner.observer, u64::MAX);
    }

    /// Launch-local sequence numbers can coincide across independent owners.
    /// An acknowledgment from another launch must not consume pending work;
    /// duplicate final callbacks must not downgrade a completed observation.
    #[test]
    fn pi_owner_acknowledgments_and_settlement_are_launch_fenced() {
        let mut first = LifecycleOwner::new("bound").unwrap();
        let mut second = LifecycleOwner::new("bound").unwrap();
        first.observe(1, "bound", Observation::Running).unwrap();
        second.observe(1, "bound", Observation::Running).unwrap();
        let foreign = first.pending().unwrap().clone();
        assert!(!second.acknowledge(&foreign));
        assert!(first.acknowledge(&foreign));
        first
            .observe(
                1,
                "bound",
                Observation::CandidateOutcome {
                    outcome: "completed",
                },
            )
            .unwrap();
        first.observe(1, "bound", Observation::Settled).unwrap();
        let completed = first.pending().unwrap().clone();
        first.observe(1, "bound", Observation::Settled).unwrap();
        assert_eq!(first.pending.len(), 1);
        assert_eq!(first.pending(), Some(&completed));
        assert_eq!(first.state, "complete");
    }

    /// Lost acknowledgments retain exact report identity across reload; old
    /// observer epochs and mismatched sessions never produce new presentation.
    #[test]
    fn pi_owner_reload_preserves_pending_identity_and_fences_old_observers() {
        let mut owner = LifecycleOwner::new("bound").unwrap();
        let old = owner.observer_epoch();
        owner.observe(old, "bound", Observation::Running).unwrap();
        let first = owner.pending().unwrap().clone();
        owner
            .observe(
                old,
                "bound",
                Observation::SessionShutdown { reason: "reload" },
            )
            .unwrap();
        assert_eq!(owner.pending(), Some(&first));
        assert!(owner.observe(old, "bound", Observation::Settled).is_err());
        assert!(owner.attach_after_reload("other").is_err());
        let new = owner.attach_after_reload("bound").unwrap();
        assert!(new > old);
        assert!(owner.observe(old, "bound", Observation::Running).is_err());
        owner.observe(new, "bound", Observation::Settled).unwrap();
        assert!(owner.acknowledge(&first));
        let second = owner.pending().unwrap().clone();
        assert_eq!(second.operation, Operation::Present("ready"));
        assert!(second.sequence > first.sequence);
        assert!(!owner.acknowledge(&first));
        assert!(owner.acknowledge(&second));
    }

    /// Provisional outcomes do not send terminal presentation, queue pressure
    /// cannot change reducer state, and one retirement slot prevents a stale
    /// session from remaining observationally live after session replacement.
    #[test]
    fn pi_owner_bounds_queue_and_keeps_finality_separate_from_candidates() {
        let mut owner = LifecycleOwner::new("bound").unwrap();
        let epoch = owner.observer_epoch();
        owner
            .observe(
                epoch,
                "bound",
                Observation::CandidateOutcome { outcome: "error" },
            )
            .unwrap();
        assert!(owner.pending().is_none());
        owner.observe(epoch, "bound", Observation::Settled).unwrap();
        assert_eq!(
            owner.pending().unwrap().operation,
            Operation::Present("failed")
        );
        for _ in 1..MAX_PENDING - 1 {
            owner
                .observe(epoch, "bound", Observation::InputWait)
                .unwrap();
        }
        let before = owner.pending.clone();
        assert!(owner.observe(epoch, "bound", Observation::Running).is_err());
        assert_eq!(owner.pending, before);
        owner
            .observe(
                epoch,
                "bound",
                Observation::SessionShutdown { reason: "new" },
            )
            .unwrap();
        assert_eq!(owner.pending.len(), MAX_PENDING);
        assert!(
            owner
                .observe(
                    epoch,
                    "bound",
                    Observation::SessionStarted { reason: "new" }
                )
                .is_err()
        );
        assert!(owner.attach_after_reload("bound").is_err());
        assert_eq!(owner.pending.back().unwrap().operation, Operation::Retire);
    }
}
