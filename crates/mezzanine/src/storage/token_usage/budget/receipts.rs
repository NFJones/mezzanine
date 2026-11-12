//! Pre-issue attempts and immutable receipt outcomes. Root totals and attempt
//! prefixes commit in one transaction; exact replay returns the original outcome.
//! No receipt, including a terminal unknown snapshot, can reissue model work.

use super::*;
use mez_agent::{
    ModelTokenUsageKey,
    token_budget::{AttemptUsage, BudgetAttemptId, BudgetReceiptId, UsageSnapshot},
};
use rusqlite::{OptionalExtension, params};

/// Original concrete send identity captured by the provider owner, not by output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetAttempt {
    pub namespace: String,
    pub id: BudgetAttemptId,
    pub root: BudgetRootId,
    pub conversation: String,
    pub model: ModelTokenUsageKey,
    /// Closed runtime-selected purpose vocabulary, never arbitrary request text.
    pub purpose: BudgetAttemptPurpose,
    /// Captured before issue; legacy absence never invents project attribution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporting_origin: Option<super::reporting::BudgetReportingOrigin>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum BudgetAttemptPurpose {
    Execution,
    Routing,
    Compaction,
    Memory,
    Title,
    MacroJudge,
    SandboxAssessment,
    FailureSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetReceipt {
    pub id: BudgetReceiptId,
    pub attempt: BudgetAttempt,
    pub snapshot: UsageSnapshot,
    /// Optional normalized cumulative reporting evidence, not a second charge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporting: Option<super::reporting::BudgetReportingObservation>,
}

/// Original acknowledged outcome, stable even when later receipts advance totals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetReceiptOutcome {
    pub receipt: BudgetReceiptId,
    pub root: BudgetRootId,
    pub known: UsageTotals,
    pub accounting_revision: u64,
    /// Optional exact reporting/partition precision from this transaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporting: Option<super::reporting::BudgetReportingProjection>,
}

/// Keep the merged known subtotal separate from terminal missing-category facts.
/// Reconstructing from only the last terminal observation would lose prefixes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAttemptUsage {
    prefix: UsageSnapshot,
    terminal: Option<UsageSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reported: Option<mez_agent::ModelTokenUsage>,
    /// Coverage at the last reporting boundary, not the retained high-water.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reporting_baseline: Option<super::reporting::BudgetReportingBaseline>,
}

impl BudgetAttempt {
    pub(super) fn validate(&self) -> Result<()> {
        validate_id(&self.namespace)?;
        if let Some(origin) = &self.reporting_origin {
            origin.validate()?;
        }
        mez_agent::transcript::validate_conversation_id(&self.conversation)?;
        for value in [&self.model.provider, &self.model.model] {
            if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                return Err(MezError::invalid_args(
                    "invalid budget provider/model identity",
                ));
            }
        }
        Ok(())
    }
}

impl TokenUsageStore {
    /// Reconciles a lost registration acknowledgement without registering or
    /// reissuing anything. Absence is distinct from conflicting provenance or
    /// an unavailable original repository. This is never a spend permit.
    pub(crate) fn budget_attempt_registration(&self, attempt: &BudgetAttempt) -> Result<bool> {
        attempt.validate()?;
        let mut connection = self.open_budget_query()?;
        let snapshot = connection.transaction()?;
        if namespace(&snapshot)? != attempt.namespace {
            return Err(MezError::conflict("budget attempt namespace mismatch"));
        }
        attempt_registration(&snapshot, attempt)
    }

    /// Queries only the original immutable receipt outcome after commit/ACK
    /// uncertainty. Later account revisions cannot replace that outcome. A
    /// missing receipt is not a successful zero settlement or retry authority.
    pub(crate) fn budget_receipt_outcome(
        &self,
        receipt: &BudgetReceipt,
    ) -> Result<Option<BudgetReceiptOutcome>> {
        receipt.attempt.validate()?;
        let mut connection = self.open_budget_query()?;
        let snapshot = connection.transaction()?;
        if namespace(&snapshot)? != receipt.attempt.namespace {
            return Err(MezError::conflict("budget receipt namespace mismatch"));
        }
        receipt_outcome(&snapshot, receipt)
    }

    /// Durably registers a concrete attempt. Caller still owns actual admission,
    /// finite-root concurrency and revision checks; this is not a spend permit.
    pub(crate) fn register_budget_attempt(&self, attempt: &BudgetAttempt) -> Result<bool> {
        attempt.validate()?;
        let mut connection = self.open_budget()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let inserted = register_attempt(&tx, attempt)?;
        tx.commit()?;
        Ok(inserted)
    }

    /// Commits ONLY the new known delta under original frozen provenance. Root
    /// totals survive rolling retention. Overflow/regression leaves all rows
    /// unchanged; caller retains the refused receipt and installs integrity hold.
    pub(crate) fn commit_budget_receipt(
        &self,
        receipt: &BudgetReceipt,
    ) -> Result<BudgetReceiptOutcome> {
        receipt.attempt.validate()?;
        let body = encode(receipt)?;
        let mut connection = self.open_budget()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if namespace(&tx)? != receipt.attempt.namespace {
            return Err(MezError::conflict("budget receipt namespace mismatch"));
        }
        if let Some(outcome) = receipt_outcome(&tx, receipt)? {
            return Ok(outcome);
        }
        let (original, snapshot, root, conversation, terminal): (
            String,
            String,
            String,
            String,
            bool,
        ) = tx.query_row(
            "SELECT body,snapshot,root,conversation,terminal FROM budget_attempts WHERE id=?1",
            [receipt.attempt.id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
        if decode::<BudgetAttempt>(&original)? != receipt.attempt
            || root != receipt.attempt.root.as_str()
            || conversation != receipt.attempt.conversation
        {
            return Err(MezError::conflict(
                "budget receipt original attempt mismatch",
            ));
        }
        let previous: StoredAttemptUsage = decode(&snapshot)?;
        if terminal != previous.terminal.is_some()
            || previous.prefix.terminal
            || previous.terminal.is_some_and(|s| !s.terminal)
        {
            return Err(MezError::conflict(
                "budget attempt terminal evidence mismatch",
            ));
        }
        let mut state = AttemptUsage::new(receipt.attempt.root.clone(), receipt.attempt.id.clone());
        state
            .observe(&receipt.attempt.root, &receipt.attempt.id, previous.prefix)
            .map_err(usage_error)?;
        if let Some(final_snapshot) = previous.terminal {
            state
                .observe(&receipt.attempt.root, &receipt.attempt.id, final_snapshot)
                .map_err(usage_error)?;
        }
        let mut account = account(&tx, &receipt.attempt.root)?;
        let (next, totals) = state
            .reconcile(
                &receipt.attempt.root,
                &receipt.attempt.id,
                receipt.snapshot,
                account.known,
            )
            .map_err(usage_error)?;
        let (reported, reporting) = super::reporting::settle_reporting_event(
            &tx,
            receipt,
            next.observed(),
            previous.reported,
            previous.reporting_baseline,
        )?;
        account.known = totals;
        account.accounting_revision = account
            .accounting_revision
            .checked_add(1)
            .ok_or_else(|| MezError::conflict("budget accounting revision overflow"))?;
        let outcome = BudgetReceiptOutcome {
            receipt: receipt.id.clone(),
            root: receipt.attempt.root.clone(),
            known: totals,
            accounting_revision: account.accounting_revision,
            reporting,
        };
        let stored = StoredAttemptUsage {
            prefix: UsageSnapshot {
                terminal: false,
                ..next.observed()
            },
            terminal: if receipt.snapshot.terminal {
                Some(receipt.snapshot)
            } else {
                previous.terminal
            },
            reported,
            reporting_baseline: receipt
                .reporting
                .as_ref()
                .map(|observation| {
                    super::reporting::BudgetReportingBaseline::capture(observation.usage)
                })
                .or(previous.reporting_baseline),
        };
        tx.execute(
            "UPDATE budget_attempts SET snapshot=?1,terminal=?2 WHERE id=?3",
            params![
                encode(&stored)?,
                stored.terminal.is_some(),
                receipt.attempt.id.as_str()
            ],
        )?;
        tx.execute(
            "UPDATE budget_roots SET body=?1 WHERE id=?2",
            params![encode(&account)?, account.root.as_str()],
        )?;
        tx.execute(
            "INSERT INTO budget_receipts(id,attempt,body,outcome) VALUES(?1,?2,?3,?4)",
            params![
                receipt.id.as_str(),
                receipt.attempt.id.as_str(),
                body,
                encode(&outcome)?
            ],
        )?;
        tx.commit()?;
        Ok(outcome)
    }
}

/// Shares one registration transaction with native authorization. This helper
/// does not commit, authorize a send, or upgrade an older registration.
pub(super) fn register_attempt(connection: &Connection, attempt: &BudgetAttempt) -> Result<bool> {
    if namespace(connection)? != attempt.namespace {
        return Err(MezError::conflict("budget attempt namespace mismatch"));
    }
    let (member, phase) = super::membership::membership(connection, &attempt.conversation)?
        .ok_or_else(|| MezError::conflict("budget attempt membership unavailable"))?;
    if member.root != attempt.root || phase != BudgetMembershipState::Active {
        return Err(MezError::conflict(
            "budget attempt requires original active membership",
        ));
    }
    if attempt_registration(connection, attempt)? {
        return Ok(false);
    }
    if outstanding_attempts(connection)? >= 4096 {
        return Err(MezError::invalid_state(
            "budget unsettled attempt capacity reached",
        ));
    }
    connection.execute("INSERT INTO budget_attempts(id,root,conversation,body,snapshot,terminal) VALUES(?1,?2,?3,?4,?5,0)",
        params![attempt.id.as_str(), attempt.root.as_str(), attempt.conversation, encode(attempt)?, encode(&StoredAttemptUsage::default())?])?;
    Ok(true)
}

/// A terminal expense receipt cannot remove still-authorized local exposure.
/// Both indexed branches are bounded by the existing unsettled capacity.
pub(super) fn outstanding_attempts(connection: &Connection) -> Result<i64> {
    Ok(connection.query_row(
        "SELECT COUNT(*) FROM (
            SELECT id FROM budget_attempts WHERE terminal=0
            UNION SELECT attempt FROM budget_admissions)",
        [],
        |r| r.get(0),
    )?)
}

/// Shared replay/query validation keeps the read-only recovery port at least
/// as strict as mutation replay, including indexed-column/body consistency.
pub(super) fn attempt_registration(
    connection: &Connection,
    attempt: &BudgetAttempt,
) -> Result<bool> {
    let existing: Option<(String, String, String)> = connection
        .query_row(
            "SELECT body,root,conversation FROM budget_attempts WHERE id=?1",
            [attempt.id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((body, root, conversation)) = existing else {
        return Ok(false);
    };
    if decode::<BudgetAttempt>(&body)? != *attempt
        || root != attempt.root.as_str()
        || conversation != attempt.conversation
    {
        return Err(MezError::conflict("budget attempt changed replay"));
    }
    Ok(true)
}

fn receipt_outcome(
    connection: &Connection,
    receipt: &BudgetReceipt,
) -> Result<Option<BudgetReceiptOutcome>> {
    let existing: Option<(String, String, String)> = connection
        .query_row(
            "SELECT body,outcome,attempt FROM budget_receipts WHERE id=?1",
            [receipt.id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((body, outcome, attempt)) = existing else {
        return Ok(None);
    };
    if decode::<BudgetReceipt>(&body)? != *receipt || attempt != receipt.attempt.id.as_str() {
        return Err(MezError::conflict("budget receipt changed replay"));
    }
    let outcome: BudgetReceiptOutcome = decode(&outcome)?;
    if outcome.receipt != receipt.id || outcome.root != receipt.attempt.root {
        return Err(MezError::conflict(
            "budget receipt outcome identity mismatch",
        ));
    }
    Ok(Some(outcome))
}

fn usage_error(error: mez_agent::token_budget::BudgetUsageError) -> MezError {
    MezError::conflict(error.to_string()).with_context("reconcile original budget receipt")
}
