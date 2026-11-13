//! Atomic native authorization, separate from bookkeeping registration.
//!
//! A committed authorization is issued exposure even if its reply is lost or
//! its one-use permit is dropped. Recovery returns only immutable evidence,
//! never another permit. Terminal expense is not local worker retirement:
//! the separate retirement/receipt transaction requires original consumed grant
//! custody and scoped transport evidence. Retirement never deletes this grant
//! or supplies another permit; production assembly remains staged.

use super::*;
use crate::storage::transcript::BudgetBindingLease;
use membership::BudgetMembership;
use mez_agent::token_budget::{BudgetAttemptId, BudgetLimits};
use receipts::BudgetAttempt;
use rusqlite::{OptionalExtension, params};

/// Exact independent revisions observed in the granting writer transaction.
/// Global projection and root override revisions cannot be summed or aliased.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetAdmissionRevisions {
    pub projection: u64,
    pub root_policy: u64,
    pub accounting: u64,
}

/// Durable grant evidence. Cloning/deserializing it grants no send authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetAdmissionOutcome {
    pub namespace: String,
    pub attempt: BudgetAttemptId,
    pub root: BudgetRootId,
    pub conversation: String,
    pub owner: String,
    pub revisions: BudgetAdmissionRevisions,
    pub limits: BudgetLimits,
    pub known: UsageTotals,
}

/// Local, nonnumeric refusals do not acquire retry authority from error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BudgetAdmissionHold {
    PolicyUnavailable,
    Exhausted {
        input: bool,
        output: bool,
    },
    /// Includes unlimited exposure issued before finite policy was installed.
    OutstandingExposure,
    Capacity,
    /// Registration alone cannot be upgraded into another possible send.
    RegistrationOnly,
}

/// Only the successful *new* transaction returns a permit. An exact replay is
/// evidence of possible issue, not permission to repeat the physical request.
#[derive(Debug)]
pub(crate) enum BudgetAttemptAdmission {
    Authorized(BudgetSendPermit),
    PreviouslyAuthorized(BudgetAdmissionOutcome),
    Held(BudgetAdmissionHold),
}

/// Not Clone or serializable; holds original native lifetime custody. There is
/// intentionally no constructor from a retained database outcome.
#[derive(Debug)]
pub(crate) struct BudgetSendPermit {
    outcome: BudgetAdmissionOutcome,
    lease: BudgetBindingLease,
}

/// Consumed grant custody retained by the actual request owner until its worker
/// retires. This is not a second send permit or a retirement witness.
#[derive(Debug)]
pub(crate) struct BudgetIssuedAttempt {
    outcome: BudgetAdmissionOutcome,
    _lease: BudgetBindingLease,
}

impl BudgetIssuedAttempt {
    /// Descriptive evidence only; cloning it cannot recreate a permit.
    pub(crate) fn outcome(&self) -> &BudgetAdmissionOutcome {
        &self.outcome
    }
}

impl BudgetSendPermit {
    /// Evidence from this original grant; it cannot construct another permit.
    pub(crate) fn outcome(&self) -> &BudgetAdmissionOutcome {
        &self.outcome
    }

    /// Worker-only consumption immediately before the concrete send. Failure
    /// retains durable exposure; it must not erase a possibly issued attempt.
    /// This grants only budget permission, never pause/Stop/approval bypass.
    pub(crate) fn consume(self) -> Result<BudgetIssuedAttempt> {
        self.lease.revalidate()?;
        Ok(BudgetIssuedAttempt {
            outcome: self.outcome,
            _lease: self.lease,
        })
    }
}

pub(super) fn initialize_schema(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    // No migration invents authorization for older registered attempts.
    tx.execute_batch(
        "CREATE TABLE budget_admissions (
            attempt TEXT PRIMARY KEY NOT NULL REFERENCES budget_attempts(id),
            root TEXT NOT NULL REFERENCES budget_roots(id),
            body TEXT NOT NULL CHECK(length(CAST(body AS BLOB))<=4096));
         CREATE INDEX budget_admissions_root ON budget_admissions(root);",
    )?;
    Ok(())
}

impl TokenUsageStore {
    /// Existing-store read-only reconciliation. A lost grant reply is never
    /// repaired by granting again, even after a later policy edit or restart.
    pub(crate) fn budget_attempt_admission(
        &self,
        attempt: &BudgetAttempt,
    ) -> Result<Option<BudgetAdmissionOutcome>> {
        attempt.validate()?;
        let mut connection = self.open_budget_query()?;
        let tx = connection.transaction()?;
        if namespace(&tx)? != attempt.namespace {
            return Err(MezError::conflict("budget admission namespace mismatch"));
        }
        read_admission(&tx, attempt)
    }

    /// Linearizes current policy, usage, original native membership/binding and
    /// exposure with receipt/policy writers. Called off actor, just before one
    /// physical attempt, never as a queued/preparation-time reusable decision.
    /// Held admission writes nothing and consumes no accounting/attempt ID.
    pub(crate) fn authorize_budget_attempt(
        &self,
        attempt: &BudgetAttempt,
        member: &BudgetMembership,
        owner: &str,
        lease: &BudgetBindingLease,
    ) -> Result<BudgetAttemptAdmission> {
        attempt.validate()?;
        validate_id(owner)?;
        if attempt.namespace != member.namespace
            || attempt.root != member.root
            || attempt.conversation != member.conversation
            || !lease.matches(member, owner)
        {
            return Err(MezError::conflict(
                "budget admission native subject mismatch",
            ));
        }
        let mut connection = self.open_budget()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if namespace(&tx)? != attempt.namespace {
            return Err(MezError::conflict("budget admission namespace mismatch"));
        }
        if let Some(outcome) = read_admission(&tx, attempt)? {
            if outcome.owner != owner {
                return Err(MezError::conflict(
                    "budget admission original owner changed",
                ));
            }
            return Ok(BudgetAttemptAdmission::PreviouslyAuthorized(outcome));
        }
        let (original, phase) = membership::membership(&tx, &attempt.conversation)?
            .ok_or_else(|| MezError::conflict("budget admission membership unavailable"))?;
        let binding: Option<String> = tx
            .query_row(
                "SELECT owner FROM budget_bindings WHERE conversation=?1",
                [&attempt.conversation],
                |r| r.get(0),
            )
            .optional()?;
        if original != *member
            || phase != BudgetMembershipState::Active
            || binding.as_deref() != Some(owner)
        {
            return Err(MezError::conflict(
                "budget admission requires original active native binding",
            ));
        }
        lease.revalidate()?;
        if receipts::attempt_registration(&tx, attempt)? {
            return Ok(BudgetAttemptAdmission::Held(
                BudgetAdmissionHold::RegistrationOnly,
            ));
        }
        let projection = policy_projection::read_projection(&tx)?;
        let Some(defaults) = projection.admission_policy() else {
            return Ok(BudgetAttemptAdmission::Held(
                BudgetAdmissionHold::PolicyUnavailable,
            ));
        };
        let account = account(&tx, &attempt.root)?;
        let limits = BudgetLimits {
            input: account.input_override.resolve(defaults.input),
            output: account.output_override.resolve(defaults.output),
        };
        let input = limits.input.exhausted(account.known.input);
        let output = limits.output.exhausted(account.known.output);
        if input || output {
            return Ok(BudgetAttemptAdmission::Held(
                BudgetAdmissionHold::Exhausted { input, output },
            ));
        }
        if limits.requires_serial_inference()
            && tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM budget_admissions WHERE root=?1 AND retired_receipt IS NULL)
                 OR EXISTS(SELECT 1 FROM budget_attempts WHERE root=?1 AND terminal=0)",
                [attempt.root.as_str()],
                |r| r.get::<_, bool>(0),
            )?
        {
            return Ok(BudgetAttemptAdmission::Held(
                BudgetAdmissionHold::OutstandingExposure,
            ));
        }
        // Count the union: a terminal receipt does not retire authorized work.
        let outstanding = receipts::outstanding_attempts(&tx)?;
        if outstanding >= 4096 {
            return Ok(BudgetAttemptAdmission::Held(BudgetAdmissionHold::Capacity));
        }
        let outcome = BudgetAdmissionOutcome {
            namespace: attempt.namespace.clone(),
            attempt: attempt.id.clone(),
            root: attempt.root.clone(),
            conversation: attempt.conversation.clone(),
            owner: owner.into(),
            limits,
            known: account.known,
            revisions: BudgetAdmissionRevisions {
                projection: projection.revision,
                root_policy: account.policy_revision,
                accounting: account.accounting_revision,
            },
        };
        let body = encode(&outcome)?;
        receipts::register_attempt(&tx, attempt)?;
        tx.execute(
            "INSERT INTO budget_admissions(attempt,root,body) VALUES(?1,?2,?3)",
            params![attempt.id.as_str(), attempt.root.as_str(), body],
        )?;
        lease.revalidate()?;
        tx.commit()?;
        Ok(BudgetAttemptAdmission::Authorized(BudgetSendPermit {
            outcome,
            lease: lease.clone(),
        }))
    }
}

pub(super) fn read_admission(
    connection: &Connection,
    attempt: &BudgetAttempt,
) -> Result<Option<BudgetAdmissionOutcome>> {
    let row: Option<(String, String)> = connection
        .query_row(
            "SELECT root,body FROM budget_admissions WHERE attempt=?1",
            [attempt.id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((root, body)) = row else {
        return Ok(None);
    };
    let outcome: BudgetAdmissionOutcome = decode(&body)?;
    validate_id(&outcome.owner)?;
    if !receipts::attempt_registration(connection, attempt)?
        || root != attempt.root.as_str()
        || outcome.root != attempt.root
        || outcome.attempt != attempt.id
        || outcome.namespace != attempt.namespace
        || outcome.conversation != attempt.conversation
    {
        return Err(MezError::conflict(
            "budget admission original evidence changed",
        ));
    }
    Ok(Some(outcome))
}
