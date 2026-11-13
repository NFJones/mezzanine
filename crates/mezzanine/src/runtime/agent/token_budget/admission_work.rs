//! Retained authorization work for one original planned physical exchange.
//!
//! This owns the actual blocking authorization handle, not another admission
//! queue or expense ledger. Cancellation drops only a borrowed wait. One new
//! native grant can be delivered once; database reconciliation is evidence
//! only. A positively held, nonwriting attempt may be explicitly reconsidered
//! under current policy, with pacing and ordinary lifecycle checks owned by the
//! caller. Failed/uncertain authorization is never automatically repeated.

use super::request::{RuntimeBudgetAttemptAdmission, RuntimeBudgetAttemptWork};
use crate::error::{MezError, Result};
use crate::storage::token_usage::budget::admission::{
    BudgetAdmissionHold, BudgetAdmissionOutcome, BudgetSendPermit,
};
use std::sync::Arc;
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
    recovered: Option<BudgetAdmissionOutcome>,
    retry_after_absence: bool,
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

impl RuntimeBudgetAdmissionWork {
    pub(super) fn new(work: RuntimeBudgetAttemptWork, shared: SharedAdmission) -> Self {
        Self { work, shared }
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
        observe_query(&mut state).await?;
        if state.recovered.is_some()
            || !matches!(state.writer, Writer::Completed(Ok(Outcome::Held(_))))
        {
            return Err(MezError::invalid_state(
                "budget authorization retry requires an observed hold",
            ));
        }
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
        state.recovered = Some(outcome.clone());
    }
    result
}
