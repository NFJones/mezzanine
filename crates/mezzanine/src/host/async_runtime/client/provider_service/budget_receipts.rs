//! Original per-attempt receipt capture for the existing wire journal.
//!
//! This staged adapter consumes a descriptor registered BEFORE its exchange;
//! it cannot admit that exchange or manufacture a descriptor after its result.
//! One factory binds exactly one wire identity. Internal retry integration must
//! supply a new issued attempt, not reuse this factory. No exposure is released.

use super::*;
use crate::integrations::agent::provider::{
    ProviderObservationReceipt, ProviderObservationReceiptFactory,
};
use crate::runtime::RuntimeBudgetRegisteredAttempt;
use std::sync::{Arc, Mutex};

type Frozen = (ProviderWireRequestObservation, Result<Arc<Receipt>>);

#[cfg_attr(
    not(test),
    allow(dead_code, reason = "physical-attempt assembly remains staged")
)]
pub(super) struct BudgetObservationReceipts {
    registered: RuntimeBudgetRegisteredAttempt,
    frozen: Mutex<Option<Frozen>>,
}

#[cfg_attr(
    not(test),
    allow(dead_code, reason = "physical-attempt assembly remains staged")
)]
impl BudgetObservationReceipts {
    pub(super) fn new(registered: RuntimeBudgetRegisteredAttempt) -> Self {
        Self {
            registered,
            frozen: Mutex::new(None),
        }
    }

    /// One owner-directed bookkeeping recovery, never a provider retry loop.
    /// The caller supplies bounded pacing and keeps the original journal live.
    pub(super) async fn retry_after_absence(&self) -> Result<()> {
        let receipt = self
            .frozen
            .lock()
            .map_err(|_| MezError::invalid_state("original budget receipt capture unavailable"))?
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("budget receipt has not been captured"))?
            .1
            .clone()?;
        let owner = receipt.0.commit_owner();
        if owner.reconcile().await?.is_none() {
            owner.retry_after_absence().await?;
        }
        Ok(())
    }
}

struct Receipt(crate::runtime::RuntimeBudgetReceiptWork);

impl ProviderObservationReceipt for Receipt {
    fn checkpoint(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let owner = self.0.commit_owner();
            // Recover positive commit-before-ACK loss, but never automatically
            // repeat a refused or uncertain write. Backoff/retry is owner policy.
            match owner.commit().await {
                Ok(_) => Ok(()),
                Err(original) => match owner.reconcile().await? {
                    Some(_) => Ok(()),
                    None => Err(original),
                },
            }
        })
    }
}

impl ProviderObservationReceiptFactory for BudgetObservationReceipts {
    fn plan(&self, observation: &ProviderWireRequestObservation) -> Result<()> {
        self.registered.plan_wire_receipt_subject(observation)
    }

    fn freeze(
        &self,
        observation: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceipt>> {
        let mut frozen = self
            .frozen
            .lock()
            .map_err(|_| MezError::invalid_state("original budget receipt capture unavailable"))?;
        if let Some((original, receipt)) = frozen.as_ref() {
            return if original == observation {
                receipt
                    .clone()
                    .map(|receipt| receipt as Arc<dyn ProviderObservationReceipt>)
            } else {
                Err(MezError::conflict(
                    "registered budget attempt already owns another exchange",
                ))
            };
        }
        let receipt = self
            .registered
            .freeze_wire_receipt(observation)
            .map(|work| Arc::new(Receipt(work)));
        *frozen = Some((observation.clone(), receipt.clone()));
        receipt.map(|receipt| receipt as Arc<dyn ProviderObservationReceipt>)
    }
}
