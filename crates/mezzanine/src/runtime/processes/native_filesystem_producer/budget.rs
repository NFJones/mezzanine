//! The original producer owns its one actual native budget claim worker. No
//! observer owns the sole handle, and a worker never owns its producer back.
//! Closing fences future patch issuance; retirement joins this reader too.

use super::*;
use crate::runtime::agent::token_budget::activity::RuntimeBudgetNativeClaimWork;
use crate::runtime::{RuntimeBudgetActivityFailure, RuntimeBudgetNativeClaim};

pub(super) struct Worker {
    handle: tokio::task::JoinHandle<Box<RuntimeBudgetNativeClaim>>,
    failure: RuntimeBudgetActivityFailure,
}

impl NativeFilesystemProducer {
    pub(crate) async fn has_budget_claim(&self) -> bool {
        self.0.budget.lock().await.is_some()
    }

    /// Installs the only physical handle before yielding. No producer/admission
    /// mutex is held over SQLite; close racing here either prevents the spawn or
    /// fences its original source before actor transfer.
    pub(crate) async fn start_budget_claim(
        &self,
        work: Box<RuntimeBudgetNativeClaimWork>,
    ) -> Result<bool> {
        let mut current = self.0.budget.lock().await;
        if current.is_some() || self.is_closed()? {
            return Ok(false);
        }
        let failure = work.failure_capture();
        *current = Some(Worker {
            handle: tokio::task::spawn_blocking(move || work.execute()),
            failure,
        });
        Ok(true)
    }

    /// Cancellation drops only this borrowed observation. Taking the worker
    /// happens synchronously AFTER its actual result, never before the await.
    pub(crate) async fn observe_budget_claim(
        &self,
    ) -> std::result::Result<Option<Box<RuntimeBudgetNativeClaim>>, RuntimeBudgetActivityFailure>
    {
        let mut current = self.0.budget.lock().await;
        let Some(worker) = current.as_mut() else {
            return Ok(None);
        };
        let joined = (&mut worker.handle).await;
        let worker = current
            .take()
            .expect("observed original budget worker remains installed");
        joined.map(Some).map_err(|_| worker.failure)
    }
}
