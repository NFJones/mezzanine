//! Original grant retirement paired with immutable terminal evidence.
//!
//! The native driver qualifies actual scoped transport destruction or withdraws
//! a retained never-consumed permit after joining its original writer. These
//! data are not physical proof or send authority themselves.
//! The existing receipt transaction validates original grant/prefix and commits
//! exact terminal evidence plus its release marker atomically. Original grants
//! remain forever replay-only; neither stale membership nor lost ACK reissues work.

use super::admission::BudgetAdmissionOutcome;
use super::receipts::{BudgetReceipt, BudgetReceiptOutcome};
use super::*;
use rusqlite::params;

/// Pure proposal from qualified original custody, never from actor enqueue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BudgetExposureRetirement {
    grant: BudgetAdmissionOutcome,
    prefix: BudgetReceipt,
    terminal: mez_agent::token_budget::UsageSnapshot,
}

impl BudgetExposureRetirement {
    #[cfg(test)]
    pub(crate) fn new(grant: BudgetAdmissionOutcome, prefix: BudgetReceipt) -> Self {
        Self::with_terminal(
            grant,
            prefix,
            mez_agent::token_budget::UsageSnapshot {
                terminal: true,
                ..Default::default()
            },
        )
    }

    /// Descriptive proposal only. The native owner independently qualifies any
    /// complete dimensions; storage binds them to the exact original subtotal.
    pub(crate) fn with_terminal(
        grant: BudgetAdmissionOutcome,
        prefix: BudgetReceipt,
        terminal: mez_agent::token_budget::UsageSnapshot,
    ) -> Self {
        Self {
            grant,
            prefix,
            terminal,
        }
    }
}

pub(super) fn initialize_schema(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "ALTER TABLE budget_admissions ADD COLUMN retired_receipt TEXT
             REFERENCES budget_receipts(id);
         CREATE INDEX budget_admissions_outstanding ON budget_admissions(root)
             WHERE retired_receipt IS NULL;",
    )?;
    Ok(())
}

/// Requires positive original subtotal transfer and exact proposed finality.
/// Qualified complete dimensions must equal that subtotal; unknown dimensions
/// preserve prior knowledge without promoting it to complete coverage.
pub(super) fn validate(
    connection: &Connection,
    receipt: &BudgetReceipt,
    evidence: &BudgetExposureRetirement,
) -> Result<Option<String>> {
    let prefix = &evidence.prefix;
    if !receipt.snapshot.terminal
        || receipt.snapshot != evidence.terminal
        || receipt
            .snapshot
            .input
            .is_some_and(|value| prefix.snapshot.input != Some(value))
        || receipt
            .snapshot
            .output
            .is_some_and(|value| prefix.snapshot.output != Some(value))
        || receipt.reporting.is_some()
        || prefix.snapshot.terminal
        || prefix.id == receipt.id
        || prefix.attempt != receipt.attempt
        || admission::read_admission(connection, &receipt.attempt)?.as_ref()
            != Some(&evidence.grant)
        || receipts::receipt_outcome(connection, prefix)?.is_none()
    {
        return Err(MezError::conflict(
            "budget retirement requires original grant and durable subtotal",
        ));
    }
    let retired: Option<String> = connection.query_row(
        "SELECT retired_receipt FROM budget_admissions WHERE attempt=?1",
        [receipt.attempt.id.as_str()],
        |r| r.get(0),
    )?;
    if retired
        .as_deref()
        .is_some_and(|id| id != receipt.id.as_str())
    {
        return Err(MezError::conflict(
            "budget exposure already owns another retirement",
        ));
    }
    Ok(retired)
}

pub(super) fn mark_retired(connection: &Connection, receipt: &BudgetReceipt) -> Result<()> {
    if connection.execute(
        "UPDATE budget_admissions SET retired_receipt=?1 WHERE attempt=?2 AND retired_receipt IS NULL",
        params![receipt.id.as_str(), receipt.attempt.id.as_str()],
    )? != 1 {
        return Err(MezError::conflict("budget original retirement changed"));
    }
    Ok(())
}

impl TokenUsageStore {
    /// Read-only reconciliation validates BOTH receipt and retirement marker.
    /// Receipt presence without its original atomic release is not success.
    pub(crate) fn budget_retirement_receipt_outcome(
        &self,
        receipt: &BudgetReceipt,
        evidence: &BudgetExposureRetirement,
    ) -> Result<Option<BudgetReceiptOutcome>> {
        receipt.attempt.validate()?;
        let mut connection = self.open_budget_query()?;
        let tx = connection.transaction()?;
        if namespace(&tx)? != receipt.attempt.namespace {
            return Err(MezError::conflict("budget retirement namespace mismatch"));
        }
        let retired = validate(&tx, receipt, evidence)?;
        let outcome = receipts::receipt_outcome(&tx, receipt)?;
        if outcome.is_some() != retired.is_some() {
            return Err(MezError::conflict(
                "budget retirement receipt/marker mismatch",
            ));
        }
        Ok(outcome)
    }
}
