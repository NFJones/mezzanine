//! Retained authorization work for one original planned physical exchange.
//!
//! This owns the actual blocking authorization handle, not another admission
//! queue or expense ledger. Cancellation drops only a borrowed wait. One new
//! native grant can be delivered once; database reconciliation is evidence
//! only. A positively held, nonwriting attempt may be explicitly reconsidered
//! under current policy, with pacing and ordinary lifecycle checks owned by the
//! caller. Failed/uncertain authorization is never automatically repeated.

use super::request::{
    RuntimeBudgetAttemptAdmission, RuntimeBudgetAttemptWork, RuntimeBudgetUnusedAttempt,
};
use crate::error::{MezError, Result};
use crate::storage::token_usage::budget::admission::{
    BudgetAdmissionHold, BudgetAdmissionOutcome, BudgetAdmissionWaitState, BudgetSendPermit,
};
use std::sync::{Arc, Weak};
use tokio::{sync::Mutex, task::JoinHandle};

#[derive(Debug)]
enum Outcome {
    Grant(BudgetSendPermit),
    Evidence(BudgetAdmissionOutcome),
    Held(BudgetAdmissionHold),
}

impl Outcome {
    fn capture(result: RuntimeBudgetAttemptAdmission) -> Self {
        match result {
            RuntimeBudgetAttemptAdmission::Authorized(permit) => Self::Grant(permit.into_native()),
            RuntimeBudgetAttemptAdmission::PreviouslyAuthorized(outcome) => Self::Evidence(outcome),
            RuntimeBudgetAttemptAdmission::Held(reason) => Self::Held(reason),
        }
    }
}

#[derive(Debug, Default)]
enum Writer {
    #[default]
    Unstarted,
    Running(JoinHandle<Result<Outcome>>),
    Completed(Result<Outcome>),
}

#[derive(Debug, Default)]
struct State {
    writer: Writer,
    query: Option<JoinHandle<Result<Option<BudgetAdmissionOutcome>>>>,
    wait_query: Option<JoinHandle<Result<BudgetAdmissionWaitState>>>,
    recovered: Option<BudgetAdmissionOutcome>,
    retry_after_absence: bool,
    /// Only the retained Held state owns this epoch. Captures borrow it weakly
    /// so reconsideration revokes them before any new native writer can run.
    hold_epoch: Option<Arc<()>>,
}

/// Contains no idle reverse reference to the original AttemptSource. Even a
/// completed but unobserved worker result stores only native permit custody.
#[derive(Debug, Clone, Default)]
pub(super) struct SharedAdmission(Arc<Mutex<State>>);

/// Keep in the existing request/journal owner, outside cancellable observers.
/// Destruction is not actual join, safe retirement, or exposure-release proof.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeBudgetAdmissionWork {
    work: RuntimeBudgetAttemptWork,
    shared: SharedAdmission,
}

/// Sealed, revocable evidence of an observed nonwriting hold on one original
/// attempt. It supplies no grant, retry, or scheduler-resume authority. The
/// actor must still validate its current claim and original membership owner.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeBudgetHeldAttempt {
    work: RuntimeBudgetAttemptWork,
    epoch: Weak<()>,
}

impl RuntimeBudgetHeldAttempt {
    pub(crate) fn is_remember(&self) -> bool {
        matches!(
            self.work.auxiliary_claim(),
            Some(super::auxiliary::RuntimeBudgetAuxiliaryClaim::Remember { .. })
        )
    }

    pub(crate) fn is_title(&self) -> bool {
        matches!(
            self.work.auxiliary_claim(),
            Some(super::auxiliary::RuntimeBudgetAuxiliaryClaim::Title { .. })
        )
    }

    pub(super) fn auxiliary_claim(&self) -> Option<&super::auxiliary::RuntimeBudgetAuxiliaryClaim> {
        self.work.auxiliary_claim()
    }

    pub(crate) fn matches_owner(&self, owner: &super::request::RuntimeBudgetRequestOwner) -> bool {
        self.epoch.upgrade().is_some() && self.work.matches_owner(owner)
    }

    pub(super) fn matches_claim(&self, agent: &str, turn: &str, generation: u64) -> bool {
        self.epoch.upgrade().is_some() && self.work.matches_claim(agent, turn, generation)
    }

    pub(super) fn same_capture(&self, other: &Self) -> bool {
        self.epoch.ptr_eq(&other.epoch) && self.epoch.upgrade().is_some()
    }
}

impl RuntimeBudgetAdmissionWork {
    pub(super) fn new(work: RuntimeBudgetAttemptWork, shared: SharedAdmission) -> Self {
        Self { work, shared }
    }

    /// Capture only an already joined, positively nonwriting original hold.
    /// No store read or writer starts here; cancellation cannot lose native
    /// work. All clones share the same epoch until explicit reconsideration.
    pub(crate) async fn capture_held(&self) -> Result<RuntimeBudgetHeldAttempt> {
        let mut state = self.shared.0.lock().await;
        if state.recovered.is_some()
            || !matches!(state.writer, Writer::Completed(Ok(Outcome::Held(_))))
        {
            return Err(MezError::invalid_state(
                "budget parking requires original hold",
            ));
        }
        let epoch = Arc::downgrade(state.hold_epoch.get_or_insert_with(|| Arc::new(())));
        Ok(RuntimeBudgetHeldAttempt {
            work: self.work.clone(),
            epoch,
        })
    }

    /// Observe current dependencies only after a positively nonwriting hold.
    /// Retains the actual reader across cancellation; clones join that reader
    /// instead of spawning replacements. Caller owns pacing, fair reacquisition
    /// and lifecycle checks; this method never retries authorization or sends.
    pub(crate) async fn observe_held(&self) -> Result<BudgetAdmissionWaitState> {
        let work = self.work.clone();
        self.observe_held_using(move || work.observe_admission_wait())
            .await
    }

    async fn observe_held_using<F>(&self, query: F) -> Result<BudgetAdmissionWaitState>
    where
        F: FnOnce() -> Result<BudgetAdmissionWaitState> + Send + 'static,
    {
        let mut state = self.shared.0.lock().await;
        if state.recovered.is_some()
            || !matches!(state.writer, Writer::Completed(Ok(Outcome::Held(_))))
        {
            return Err(MezError::invalid_state(
                "budget wait observation requires original hold",
            ));
        }
        if state.wait_query.is_none() {
            state.wait_query = Some(tokio::task::spawn_blocking(query));
        }
        observe_wait_query(&mut state)
            .await?
            .ok_or_else(|| MezError::invalid_state("budget wait observation unavailable"))
    }

    /// Delivers at most one original permit. Clones share its actual worker;
    /// later claims return only evidence, the original hold, or sticky failure.
    pub(crate) async fn claim(&self) -> Result<RuntimeBudgetAttemptAdmission> {
        let work = self.work.clone();
        self.claim_using(move || work.authorize()).await
    }

    async fn claim_using<F>(&self, authorize: F) -> Result<RuntimeBudgetAttemptAdmission>
    where
        F: FnOnce() -> Result<RuntimeBudgetAttemptAdmission> + Send + 'static,
    {
        let mut state = self.shared.0.lock().await;
        observe_query(&mut state).await?;
        if let Some(outcome) = &state.recovered {
            return Ok(RuntimeBudgetAttemptAdmission::PreviouslyAuthorized(
                outcome.clone(),
            ));
        }
        if matches!(state.writer, Writer::Unstarted) {
            state.writer = spawn(authorize);
        }
        observe_writer(&mut state).await;
        self.take_outcome(&mut state)
    }

    /// Join the actual writer and withdraw ONLY its retained original permit.
    /// Evidence from a prior handoff/query cannot prove unused send authority.
    /// No await separates withdrawal from transfer into non-send custody.
    pub(crate) async fn retire_unused(&self) -> Result<Option<RuntimeBudgetUnusedAttempt>> {
        let mut state = self.shared.0.lock().await;
        observe_writer(&mut state).await;
        if !matches!(state.writer, Writer::Completed(Ok(Outcome::Grant(_)))) {
            return Ok(None);
        }
        match self.take_outcome(&mut state)? {
            RuntimeBudgetAttemptAdmission::Authorized(permit) => Ok(Some(permit.retire_unused())),
            _ => Err(MezError::invalid_state(
                "budget unused permit custody changed",
            )),
        }
    }

    fn take_outcome(&self, state: &mut State) -> Result<RuntimeBudgetAttemptAdmission> {
        match &state.writer {
            Writer::Completed(Ok(Outcome::Grant(permit))) => {
                let evidence = permit.outcome().clone();
                let previous = std::mem::replace(
                    &mut state.writer,
                    Writer::Completed(Ok(Outcome::Evidence(evidence))),
                );
                // No await separates taking the original permit and delivering
                // it. The replaced state can never mint a second permit.
                if let Writer::Completed(Ok(Outcome::Grant(permit))) = previous {
                    Ok(RuntimeBudgetAttemptAdmission::Authorized(
                        self.work.wrap_permit(permit),
                    ))
                } else {
                    Err(MezError::invalid_state(
                        "budget authorization result changed",
                    ))
                }
            }
            Writer::Completed(Ok(Outcome::Evidence(outcome))) => Ok(
                RuntimeBudgetAttemptAdmission::PreviouslyAuthorized(outcome.clone()),
            ),
            Writer::Completed(Ok(Outcome::Held(reason))) => {
                Ok(RuntimeBudgetAttemptAdmission::Held(*reason))
            }
            Writer::Completed(Err(error)) => Err(error.clone()),
            _ => Err(MezError::invalid_state(
                "budget authorization worker not observed",
            )),
        }
    }

    /// Explicitly reconsider ONLY an observed native hold, which wrote nothing.
    /// No internal timer/loop, fresh identity, or retry of an uncertain grant.
    pub(crate) async fn retry_held(&self) -> Result<RuntimeBudgetAttemptAdmission> {
        let mut state = self.shared.0.lock().await;
        observe_wait_query(&mut state).await?;
        observe_query(&mut state).await?;
        if state.recovered.is_some()
            || !matches!(state.writer, Writer::Completed(Ok(Outcome::Held(_))))
        {
            return Err(MezError::invalid_state(
                "budget authorization retry requires an observed hold",
            ));
        }
        state.hold_epoch = None;
        let work = self.work.clone();
        state.writer = spawn(move || work.authorize());
        observe_writer(&mut state).await;
        self.take_outcome(&mut state)
    }

    /// Join the original authorization before querying exact durable evidence.
    /// Positive recovery can NEVER reconstruct a lost/dropped spend permit.
    pub(crate) async fn reconcile(&self) -> Result<Option<BudgetAdmissionOutcome>> {
        let work = self.work.clone();
        self.reconcile_using(move || work.reconcile_admission())
            .await
    }

    async fn reconcile_using<F>(&self, query: F) -> Result<Option<BudgetAdmissionOutcome>>
    where
        F: FnOnce() -> Result<Option<BudgetAdmissionOutcome>> + Send + 'static,
    {
        let mut state = self.shared.0.lock().await;
        // The original journal's cancellation drain also joins a retained wait
        // reader before its native source/store can be retired.
        observe_wait_query(&mut state).await?;
        observe_writer(&mut state).await;
        if let Some(outcome) = &state.recovered {
            return Ok(Some(outcome.clone()));
        }
        match &state.writer {
            Writer::Completed(Ok(Outcome::Grant(permit))) => {
                return Ok(Some(permit.outcome().clone()));
            }
            Writer::Completed(Ok(Outcome::Evidence(outcome))) => return Ok(Some(outcome.clone())),
            _ => {}
        }
        if state.query.is_none() {
            state.retry_after_absence = false;
            state.query = Some(tokio::task::spawn_blocking(query));
        }
        observe_query(&mut state).await
    }

    /// Retry only this authorization transaction after a joined failure and a
    /// fresh positive absence query. Never retries a send, or a lost grant.
    pub(crate) async fn retry_after_absence(&self) -> Result<RuntimeBudgetAttemptAdmission> {
        let mut state = self.shared.0.lock().await;
        observe_query(&mut state).await?;
        if !state.retry_after_absence || !matches!(state.writer, Writer::Completed(Err(_))) {
            return Err(MezError::invalid_state(
                "budget authorization retry requires joined failure and original absence",
            ));
        }
        state.retry_after_absence = false;
        let work = self.work.clone();
        state.writer = spawn(move || work.authorize());
        observe_writer(&mut state).await;
        self.take_outcome(&mut state)
    }

    #[cfg(test)]
    pub(crate) async fn claim_with_for_test<F>(
        &self,
        authorize: F,
    ) -> Result<RuntimeBudgetAttemptAdmission>
    where
        F: FnOnce() -> Result<RuntimeBudgetAttemptAdmission> + Send + 'static,
    {
        self.claim_using(authorize).await
    }

    #[cfg(test)]
    pub(crate) async fn reconcile_with_for_test<F>(
        &self,
        query: F,
    ) -> Result<Option<BudgetAdmissionOutcome>>
    where
        F: FnOnce() -> Result<Option<BudgetAdmissionOutcome>> + Send + 'static,
    {
        self.reconcile_using(query).await
    }

    #[cfg(test)]
    pub(crate) async fn observe_held_with_for_test<F>(
        &self,
        query: F,
    ) -> Result<BudgetAdmissionWaitState>
    where
        F: FnOnce() -> Result<BudgetAdmissionWaitState> + Send + 'static,
    {
        self.observe_held_using(query).await
    }

    /// Scheduling-only probe: never joins or takes a completed grant result.
    #[cfg(test)]
    pub(crate) async fn worker_finished_for_test(&self) -> bool {
        matches!(&self.shared.0.lock().await.writer, Writer::Running(handle) if handle.is_finished())
    }
}

fn spawn<F>(authorize: F) -> Writer
where
    F: FnOnce() -> Result<RuntimeBudgetAttemptAdmission> + Send + 'static,
{
    Writer::Running(tokio::task::spawn_blocking(move || {
        authorize().map(Outcome::capture)
    }))
}

async fn observe_wait_query(state: &mut State) -> Result<Option<BudgetAdmissionWaitState>> {
    let Some(handle) = &mut state.wait_query else {
        return Ok(None);
    };
    let result = handle.await;
    state.wait_query = None;
    result
        .map_err(|_| MezError::invalid_state("budget wait reader join failed"))?
        .map(Some)
}

async fn observe_writer(state: &mut State) {
    if let Writer::Running(handle) = &mut state.writer {
        let result = handle.await.unwrap_or_else(|_| {
            Err(MezError::invalid_state(
                "budget authorization worker join failed",
            ))
        });
        state.writer = Writer::Completed(result);
    }
}

async fn observe_query(state: &mut State) -> Result<Option<BudgetAdmissionOutcome>> {
    let Some(handle) = &mut state.query else {
        return Ok(state.recovered.clone());
    };
    let result = handle.await.unwrap_or_else(|_| {
        Err(MezError::invalid_state(
            "budget authorization query join failed",
        ))
    });
    state.query = None;
    state.retry_after_absence =
        matches!(&result, Ok(None)) && matches!(state.writer, Writer::Completed(Err(_)));
    if let Ok(Some(outcome)) = &result {
        state.hold_epoch = None;
        state.recovered = Some(outcome.clone());
    }
    result
}
