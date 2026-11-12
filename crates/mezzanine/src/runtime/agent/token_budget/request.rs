//! Frozen request expense custody shared by ordinary and turn-less inference.
//!
//! Registration remains bookkeeping only. Native authorization additionally
//! checks current durable policy and exposure in one transaction; only a new
//! grant returns a one-use permit. Physical-send/retirement driver assembly is
//! still downstream. Neither ACK recovery nor receipt replay reissues inference.

use super::*;
use crate::storage::token_usage::AccountingOrigin;
use crate::storage::token_usage::budget::admission::{
    BudgetAdmissionHold, BudgetAdmissionOutcome, BudgetAttemptAdmission, BudgetIssuedAttempt,
    BudgetSendPermit,
};
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
    /// Worker-only actual-admission port, not preparation-time permission. The
    /// existing frozen identity survives a held result or uncertain grant ACK.
    pub(crate) fn authorize(&self) -> Result<RuntimeBudgetAttemptAdmission> {
        let owner = &self.0.owner;
        match owner.store.authorize_budget_attempt(
            &self.0.attempt,
            &owner.member,
            &owner.binding_owner,
            &owner.lease,
        )? {
            BudgetAttemptAdmission::Authorized(permit) => Ok(
                RuntimeBudgetAttemptAdmission::Authorized(RuntimeBudgetSendPermit {
                    source: self.0.clone(),
                    permit,
                }),
            ),
            BudgetAttemptAdmission::PreviouslyAuthorized(outcome) => {
                Ok(RuntimeBudgetAttemptAdmission::PreviouslyAuthorized(outcome))
            }
            BudgetAttemptAdmission::Held(reason) => Ok(RuntimeBudgetAttemptAdmission::Held(reason)),
        }
    }

    /// Existing-only read of original authorization evidence, never a permit.
    pub(crate) fn reconcile_admission(&self) -> Result<Option<BudgetAdmissionOutcome>> {
        self.0.owner.store.budget_attempt_admission(&self.0.attempt)
    }

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

/// Native grant result; replay evidence cannot be consumed as another permit.
#[derive(Debug)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "physical-send driver integration is downstream")
)]
pub(crate) enum RuntimeBudgetAttemptAdmission {
    Authorized(RuntimeBudgetSendPermit),
    PreviouslyAuthorized(BudgetAdmissionOutcome),
    Held(BudgetAdmissionHold),
}

#[derive(Debug)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "physical-send driver integration is downstream")
)]
pub(crate) struct RuntimeBudgetSendPermit {
    source: Arc<AttemptSource>,
    permit: BudgetSendPermit,
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "physical-send driver integration is downstream")
)]
impl RuntimeBudgetSendPermit {
    /// Consume once off actor immediately before the physical send, retaining
    /// original request/native custody for settlement and actual retirement.
    pub(crate) fn consume(self) -> Result<RuntimeBudgetIssuedAttempt> {
        Ok(RuntimeBudgetIssuedAttempt {
            registered: RuntimeBudgetRegisteredAttempt(self.source),
            issued: self.permit.consume()?,
        })
    }
}

/// Non-cloneable consumed grant. Its original expense descriptor may be cloned
/// for receipt work, but neither value supplies another send permission.
#[derive(Debug)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "physical-send driver integration is downstream")
)]
pub(crate) struct RuntimeBudgetIssuedAttempt {
    registered: RuntimeBudgetRegisteredAttempt,
    issued: BudgetIssuedAttempt,
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "physical-send driver integration is downstream")
)]
impl RuntimeBudgetIssuedAttempt {
    pub(crate) fn registered(&self) -> &RuntimeBudgetRegisteredAttempt {
        &self.registered
    }

    pub(crate) fn outcome(&self) -> &BudgetAdmissionOutcome {
        self.issued.outcome()
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
            commit: std::sync::OnceLock::new(),
        })))
    }
}

#[derive(Debug)]
struct ReceiptSource {
    attempt: RuntimeBudgetRegisteredAttempt,
    receipt: BudgetReceipt,
    commit: std::sync::OnceLock<super::receipt_commit::SharedReceiptCommit>,
}

/// Exact known usage plus original custody retained until real durable ACK.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeBudgetReceiptWork(Arc<ReceiptSource>);

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "receipt driver integration is downstream")
)]
impl RuntimeBudgetReceiptWork {
    /// Pure capture; every clone of this immutable receipt shares one writer.
    /// The service must retain this owner outside cancellable ACK waiters.
    pub(crate) fn commit_owner(&self) -> super::receipt_commit::RuntimeBudgetReceiptCommit {
        super::receipt_commit::RuntimeBudgetReceiptCommit::new(
            self.clone(),
            self.0.commit.get_or_init(Default::default).clone(),
        )
    }

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
