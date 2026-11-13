//! Cancellation-safe custody for one original durable budget receipt writer.
//!
//! This is a request-local owner, not a queue or a second expense ledger. Frozen
//! receipt clones share the actual blocking handle and its immutable outcome.
//! Cancelling an ACK waiter drops only a mutex borrow. Failed/uncertain writes
//! are never automatically repeated: observe the writer, query the exact receipt,
//! then explicitly retry bookkeeping only after positive absence. This owner
//! cannot qualify physical retirement or authorize provider work. An already
//! qualified terminal work item may atomically commit its original release.

use super::request::RuntimeBudgetReceiptWork;
use crate::error::{MezError, Result};
use crate::storage::token_usage::budget::receipts::BudgetReceiptOutcome;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

#[derive(Debug, Default)]
enum Writer {
    #[default]
    Unstarted,
    Running(JoinHandle<Result<BudgetReceiptOutcome>>),
    Completed(Result<BudgetReceiptOutcome>),
}

#[derive(Debug, Default)]
struct State {
    writer: Writer,
    query: Option<JoinHandle<Result<Option<BudgetReceiptOutcome>>>>,
    recovered: Option<BudgetReceiptOutcome>,
    retry_after_absence: bool,
}

/// Stored on the original receipt source, without a reverse source reference.
/// Only a running blocking closure retains the source; there is no idle cycle.
#[derive(Debug, Clone, Default)]
pub(super) struct SharedReceiptCommit(Arc<Mutex<State>>);

/// Retain in the service/request owner until actual writer join and ACK adoption.
/// Last-owner destruction is neither acknowledgment nor proof of writer cleanup.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeBudgetReceiptCommit {
    work: RuntimeBudgetReceiptWork,
    shared: SharedReceiptCommit,
}

impl RuntimeBudgetReceiptCommit {
    pub(super) fn new(work: RuntimeBudgetReceiptWork, shared: SharedReceiptCommit) -> Self {
        Self { work, shared }
    }

    /// Starts at most one writer, then borrows its handle until actual join.
    /// Repeated/concurrent calls return the same original result, including a
    /// refusal. They cannot silently retry an uncertain transaction.
    pub(crate) async fn commit(&self) -> Result<BudgetReceiptOutcome> {
        let work = self.work.clone();
        self.commit_using(move || work.execute()).await
    }

    async fn commit_using<F>(&self, execute: F) -> Result<BudgetReceiptOutcome>
    where
        F: FnOnce() -> Result<BudgetReceiptOutcome> + Send + 'static,
    {
        let mut state = self.shared.0.lock().await;
        observe_query(&mut state).await?;
        if let Some(outcome) = &state.recovered {
            return Ok(outcome.clone());
        }
        if matches!(state.writer, Writer::Unstarted) {
            state.retry_after_absence = false;
            state.writer = Writer::Running(tokio::task::spawn_blocking(execute));
        }
        observe_writer(&mut state).await
    }

    /// Read-only exact-outcome recovery, after observing any original writer.
    /// A lost query waiter also leaves its actual handle in this owner. Absence
    /// is not a zero receipt, positive ACK, or permission to repeat inference.
    pub(crate) async fn reconcile(&self) -> Result<Option<BudgetReceiptOutcome>> {
        let work = self.work.clone();
        self.reconcile_using(move || work.reconcile()).await
    }

    async fn reconcile_using<F>(&self, query: F) -> Result<Option<BudgetReceiptOutcome>>
    where
        F: FnOnce() -> Result<Option<BudgetReceiptOutcome>> + Send + 'static,
    {
        let mut state = self.shared.0.lock().await;
        if matches!(state.writer, Writer::Running(_)) {
            // Even a failed join must precede reconciliation; no query races a
            // still-running original writer merely because its observer left.
            let _ = observe_writer(&mut state).await;
        }
        if let Some(outcome) = &state.recovered {
            return Ok(Some(outcome.clone()));
        }
        if let Writer::Completed(Ok(outcome)) = &state.writer {
            return Ok(Some(outcome.clone()));
        }
        if state.query.is_none() {
            state.retry_after_absence = false;
            state.query = Some(tokio::task::spawn_blocking(query));
        }
        observe_query(&mut state).await
    }

    /// One explicit bookkeeping retry after a new positive absence query and
    /// actual failed writer join. Never retries provider/effects. This method
    /// has no internal retry loop; its caller owns bounded backoff and holds.
    pub(crate) async fn retry_after_absence(&self) -> Result<BudgetReceiptOutcome> {
        let mut state = self.shared.0.lock().await;
        observe_query(&mut state).await?;
        if let Some(outcome) = &state.recovered {
            return Ok(outcome.clone());
        }
        if let Writer::Completed(Ok(outcome)) = &state.writer {
            return Ok(outcome.clone());
        }
        if !state.retry_after_absence || !matches!(state.writer, Writer::Completed(Err(_))) {
            return Err(MezError::invalid_state(
                "budget receipt retry requires joined failure and original absence",
            ));
        }
        state.retry_after_absence = false;
        let work = self.work.clone();
        state.writer = Writer::Running(tokio::task::spawn_blocking(move || work.execute()));
        observe_writer(&mut state).await
    }

    #[cfg(test)]
    pub(crate) async fn commit_with_for_test<F>(&self, execute: F) -> Result<BudgetReceiptOutcome>
    where
        F: FnOnce() -> Result<BudgetReceiptOutcome> + Send + 'static,
    {
        self.commit_using(execute).await
    }

    #[cfg(test)]
    pub(crate) async fn reconcile_with_for_test<F>(
        &self,
        query: F,
    ) -> Result<Option<BudgetReceiptOutcome>>
    where
        F: FnOnce() -> Result<Option<BudgetReceiptOutcome>> + Send + 'static,
    {
        self.reconcile_using(query).await
    }
}

async fn observe_writer(state: &mut State) -> Result<BudgetReceiptOutcome> {
    if let Writer::Running(handle) = &mut state.writer {
        let result = handle.await.unwrap_or_else(|_| {
            // A panic payload can contain private source data. It is neither
            // a receipt nor a safe diagnostic, and must never be displayed.
            Err(MezError::invalid_state("budget receipt writer join failed"))
        });
        // No await between positive join and retaining its sole result.
        state.writer = Writer::Completed(result);
    }
    match &state.writer {
        Writer::Completed(result) => result.clone(),
        _ => Err(MezError::invalid_state("budget receipt writer not started")),
    }
}

async fn observe_query(state: &mut State) -> Result<Option<BudgetReceiptOutcome>> {
    let Some(handle) = &mut state.query else {
        return Ok(state.recovered.clone());
    };
    let result = handle
        .await
        .unwrap_or_else(|_| Err(MezError::invalid_state("budget receipt query join failed")));
    state.query = None;
    state.retry_after_absence = false;
    match result {
        Ok(Some(outcome)) => {
            state.recovered = Some(outcome.clone());
            Ok(Some(outcome))
        }
        Ok(None) => {
            state.retry_after_absence = matches!(state.writer, Writer::Completed(Err(_)));
            Ok(None)
        }
        Err(error) => Err(error),
    }
}
