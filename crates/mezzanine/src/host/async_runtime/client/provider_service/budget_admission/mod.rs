//! One concrete exchange's native authorization and original receipt custody.
//!
//! The existing journal retains this factory before awaiting the original
//! blocking admission worker. A new permit is consumed once at the adapter's
//! transport boundary. Cancellation/reconciliation never reconstruct permission.
//! Receipt-only factories remain separate descriptive ports. This staged factory
//! is not installed by production dispatch yet; fairness, reversible root holds,
//! terminal coverage and physical retirement/release remain required.

use super::*;
use crate::integrations::agent::provider::{
    ProviderObservationReceipt, ProviderObservationReceiptFactory,
};
use crate::runtime::{
    RuntimeBudgetAttemptAdmission, RuntimeBudgetAttemptWork, RuntimeBudgetIssuedAttempt,
    RuntimeBudgetReceiptWork,
};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Phase {
    started: bool,
    finalized: Option<ProviderWireRequestObservation>,
    // Retain the actual consumed grant independently of a canceled send future.
    issued: Option<RuntimeBudgetIssuedAttempt>,
}

struct Source {
    work: RuntimeBudgetAttemptWork,
    phase: Mutex<Phase>,
}

/// One factory per original attempt, never shared as authority for a retry.
pub(super) struct BudgetAttemptAdmission(Arc<Source>);

impl BudgetAttemptAdmission {
    pub(super) fn new(work: RuntimeBudgetAttemptWork) -> Self {
        Self(Arc::new(Source {
            work,
            phase: Mutex::new(Phase::default()),
        }))
    }
}

fn suspended(message: &'static str) -> MezError {
    MezError::new(MezErrorKind::BudgetSuspended, message)
}

impl ProviderObservationReceiptFactory for BudgetAttemptAdmission {
    fn plan(&self, observation: &ProviderWireRequestObservation) -> Result<()> {
        let phase = self
            .0
            .phase
            .lock()
            .map_err(|_| suspended("budget attempt custody unavailable"))?;
        if phase.finalized.is_some() {
            return Err(suspended("budget attempt already finalized"));
        }
        self.0.work.plan_wire_receipt_subject(observation)
    }

    fn before_send<'a>(
        &'a self,
        observation: &'a ProviderWireRequestObservation,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            {
                let mut phase = self
                    .0
                    .phase
                    .lock()
                    .map_err(|_| suspended("budget attempt custody unavailable"))?;
                self.0.work.plan_wire_receipt_subject(observation)?;
                if phase.started || phase.finalized.is_some() {
                    return Err(suspended("budget attempt cannot issue another exchange"));
                }
                phase.started = true;
            }
            let admission = self
                .0
                .work
                .admission_owner()
                .claim()
                .await
                .map_err(|_| suspended("original budget authorization unavailable"))?;
            let RuntimeBudgetAttemptAdmission::Authorized(permit) = admission else {
                return Err(suspended(
                    "original budget authorization held or already issued",
                ));
            };
            let mut phase = self
                .0
                .phase
                .lock()
                .map_err(|_| suspended("budget attempt custody unavailable"))?;
            if phase.finalized.is_some() {
                return Err(suspended("budget attempt finalized during authorization"));
            }
            // No await between consumption and retaining its original custody.
            // The adapter then polls only this exchange's transport future.
            phase.issued = Some(
                permit
                    .consume()
                    .map_err(|_| suspended("original budget send custody invalidated"))?,
            );
            Ok(())
        })
    }

    fn freeze(
        &self,
        observation: &ProviderWireRequestObservation,
    ) -> Result<Arc<dyn ProviderObservationReceipt>> {
        let mut phase = self
            .0
            .phase
            .lock()
            .map_err(|_| suspended("budget attempt custody unavailable"))?;
        if let Some(original) = &phase.finalized {
            if original != observation {
                return Err(MezError::conflict("budget attempt final evidence changed"));
            }
        } else {
            phase.finalized = Some(observation.clone());
        }
        Ok(Arc::new(Receipt {
            source: self.0.clone(),
            observation: observation.clone(),
        }))
    }
}

struct Receipt {
    source: Arc<Source>,
    observation: ProviderWireRequestObservation,
}

impl Receipt {
    async fn work(&self) -> Result<Option<RuntimeBudgetReceiptWork>> {
        // Observe the actual original authorization worker before querying.
        // Recovery supplies receipt evidence ONLY, never a replacement permit.
        let outcome = self.source.work.admission_owner().reconcile().await?;
        if let Some(outcome) = outcome {
            let registered = self.source.work.registered_from_admission(&outcome)?;
            return registered.freeze_wire_receipt(&self.observation).map(Some);
        }
        if self.observation.usage.is_some()
            || self
                .observation
                .usage_presence
                .is_some_and(|presence| presence.any())
        {
            return Err(MezError::conflict(
                "unissued budget attempt reported expense",
            ));
        }
        // A positively absent, non-issued attempt has no receipt to write. This
        // is not fabricated zero usage or an authority to repeat authorization.
        Ok(None)
    }
}

impl ProviderObservationReceipt for Receipt {
    fn checkpoint(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let Some(work) = self.work().await? else {
                return Ok(());
            };
            let owner = work.commit_owner();
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
