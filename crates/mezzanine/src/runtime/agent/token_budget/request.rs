//! Frozen request expense custody shared by ordinary and turn-less inference.
//!
//! These ports prepare immutable bookkeeping, never provider admission. Workers
//! must register each concrete attempt before its send, retain exact receipts
//! through ACK uncertainty, and independently enforce policy/source authority.

use super::*;
use crate::storage::token_usage::AccountingOrigin;
use crate::storage::token_usage::budget::receipts::{
    BudgetAttempt, BudgetAttemptPurpose, BudgetReceipt, BudgetReceiptOutcome,
};
use crate::storage::token_usage::budget::reporting::{
    BudgetReportingObservation, BudgetReportingOrigin,
};
use mez_agent::token_budget::{BudgetAttemptId, UsageSnapshot};

/// Actual issued-request custody, independent of later actor/pane replacement.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeBudgetRequestOwner {
    pub(crate) member: BudgetMembership,
    pub(crate) lease: BudgetBindingLease,
    pub(super) binding_owner: String,
    pub(super) store: TokenUsageStore,
    pub(super) origin: AccountingOrigin,
}

impl RuntimeBudgetRequestOwner {
    /// Pure exact-subject check. This is neither a policy permit nor disk I/O.
    pub(crate) fn validate_request(&self, conversation: &str) -> Result<()> {
        if self.member.conversation != conversation
            || !self.lease.matches(&self.member, &self.binding_owner)
        {
            return Err(MezError::conflict("budget request subject changed"));
        }
        Ok(())
    }

    /// Capture once for ONE planned concrete attempt, before any provider send.
    /// Retry of registration must reuse this value, not allocate another ID.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "concrete attempt admission is downstream")
    )]
    pub(crate) fn prepare_attempt(
        &self,
        model: mez_agent::ModelTokenUsageKey,
        purpose: BudgetAttemptPurpose,
    ) -> Result<RuntimeBudgetAttemptWork> {
        self.validate_request(&self.member.conversation)?;
        let attempt = BudgetAttempt {
            namespace: self.member.namespace.clone(),
            id: BudgetAttemptId::new(crate::storage::token_usage::new_token_usage_event_id())
                .map_err(|error| MezError::invalid_state(error.to_string()))?,
            root: self.member.root.clone(),
            conversation: self.member.conversation.clone(),
            model,
            purpose,
            reporting_origin: Some(BudgetReportingOrigin::capture(&self.origin)),
        };
        Ok(RuntimeBudgetAttemptWork(Arc::new(AttemptSource {
            owner: self.clone(),
            attempt,
        })))
    }
}

#[derive(Debug)]
struct AttemptSource {
    owner: RuntimeBudgetRequestOwner,
    attempt: BudgetAttempt,
}

/// Immutable original registration work; keep it until the ACK is reconciled.
#[derive(Debug, Clone)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "concrete attempt admission is downstream")
)]
pub(crate) struct RuntimeBudgetAttemptWork(Arc<AttemptSource>);

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "concrete attempt admission is downstream")
)]
impl RuntimeBudgetAttemptWork {
    /// Blocking-worker only. A successful database registration is NOT a spend
    /// permit, proof of a send, or authority to repeat an uncertain remote call.
    pub(crate) fn execute(&self) -> Result<RuntimeBudgetRegisteredAttempt> {
        self.0
            .owner
            .store
            .register_budget_attempt(&self.0.attempt)?;
        Ok(RuntimeBudgetRegisteredAttempt(self.0.clone()))
    }

    /// Blocking-worker read-only recovery; absence grants no retry authority.
    pub(crate) fn reconcile_registration(&self) -> Result<Option<RuntimeBudgetRegisteredAttempt>> {
        let exists = self
            .0
            .owner
            .store
            .budget_attempt_registration(&self.0.attempt)?;
        Ok(exists.then(|| RuntimeBudgetRegisteredAttempt(self.0.clone())))
    }
}

/// Positive registration ACK bound to the same original request descriptor.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeBudgetRegisteredAttempt(Arc<AttemptSource>);

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "receipt driver integration is downstream")
)]
impl RuntimeBudgetRegisteredAttempt {
    /// Freeze reported knowledge once before cancellable delivery. Reporting
    /// time/counters and receipt ID survive retries byte-for-byte; missing
    /// dimensions remain missing rather than becoming an invented zero.
    pub(crate) fn freeze_receipt(
        &self,
        snapshot: UsageSnapshot,
        reporting: Option<BudgetReportingObservation>,
    ) -> Result<RuntimeBudgetReceiptWork> {
        let receipt = BudgetReceipt {
            id: BudgetReceiptId::new(crate::storage::token_usage::new_token_usage_event_id())
                .map_err(|error| MezError::invalid_state(error.to_string()))?,
            attempt: self.0.attempt.clone(),
            snapshot,
            reporting,
        };
        Ok(RuntimeBudgetReceiptWork(Arc::new(ReceiptSource {
            attempt: self.clone(),
            receipt,
        })))
    }
}

#[derive(Debug)]
struct ReceiptSource {
    attempt: RuntimeBudgetRegisteredAttempt,
    receipt: BudgetReceipt,
}

/// Exact known usage plus original custody retained until real durable ACK.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeBudgetReceiptWork(Arc<ReceiptSource>);

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "receipt driver integration is downstream")
)]
impl RuntimeBudgetReceiptWork {
    /// Blocking-worker only; repeat ONLY this immutable bookkeeping work after
    /// reconciliation, never the provider/effects that produced its counters.
    pub(crate) fn execute(&self) -> Result<BudgetReceiptOutcome> {
        self.0
            .attempt
            .0
            .owner
            .store
            .commit_budget_receipt(&self.0.receipt)
    }

    /// Query the original committed outcome without repeating settlement.
    pub(crate) fn reconcile(&self) -> Result<Option<BudgetReceiptOutcome>> {
        self.0
            .attempt
            .0
            .owner
            .store
            .budget_receipt_outcome(&self.0.receipt)
    }
}
