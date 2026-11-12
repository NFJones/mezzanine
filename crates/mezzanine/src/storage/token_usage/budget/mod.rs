//! Worker-only lifetime budget repository in the existing accounting database.
//!
//! These records do not make a conversation runnable or authorize provider work.
//! Runtime activation must publish the exact membership to conversation metadata
//! and the active binding before advancing pending -> published -> active here.
//! Receipt transactions retain original identity and checked unsigned totals;
//! rolling event retention never deletes these records. Missing expected stores
//! refuse instead of recreating zero expense. No provider/effect replay lives here.

pub(crate) mod membership;
pub(crate) mod operations;
pub(crate) mod policy_projection;
pub(crate) mod publication;
pub(crate) mod receipts;
pub(crate) mod reporting;
mod schema;

use mez_agent::token_budget::{BudgetOverride, BudgetRootId, UsageTotals};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use super::{MezError, Result, TokenUsageStore};

use membership::BudgetMembershipState;

/// Called only inside the accounting store's held migration transaction.
pub(super) fn initialize_schema(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    schema::initialize_schema(transaction)
}

/// Adds only the version16 config-derived projection, preserving lifetime rows.
pub(super) fn initialize_policy_projection_schema(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<()> {
    policy_projection::initialize_schema(transaction)
}

/// Durable lifetime view; start boundary does not imply pre-feature coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetAccount {
    pub root: BudgetRootId,
    pub known: UsageTotals,
    pub input_override: BudgetOverride,
    pub output_override: BudgetOverride,
    pub accounting_revision: u64,
    pub policy_revision: u64,
    pub started_at: u64,
    /// True when expense before this explicit start boundary is not attributable.
    pub legacy_coverage_gap: bool,
}

impl TokenUsageStore {
    /// Loads the immutable namespace of an already initialized store. A namespace
    /// is descriptive accounting provenance, never a control capability.
    pub(crate) fn budget_namespace(&self) -> Result<String> {
        let connection = self.open_budget()?;
        namespace(&connection)
    }

    /// Reads one account without inventing absent totals or changing retention.
    pub(crate) fn budget_account(&self, root: &BudgetRootId) -> Result<BudgetAccount> {
        account(&self.open_budget()?, root)
    }

    /// Existing-only admission is crucial after a store was lost or replaced.
    /// Schema migration belongs to ordinary explicit store initialization.
    fn open_budget(&self) -> Result<Connection> {
        self.open_budget_mode(OpenFlags::SQLITE_OPEN_READ_WRITE)
    }

    /// ACK reconciliation cannot create a store or perform recovery writes.
    fn open_budget_query(&self) -> Result<Connection> {
        self.open_budget_mode(OpenFlags::SQLITE_OPEN_READ_ONLY)
    }

    fn open_budget_mode(&self, mode: OpenFlags) -> Result<Connection> {
        let connection =
            Connection::open_with_flags(self.path(), mode | OpenFlags::SQLITE_OPEN_NO_MUTEX)
                .map_err(|error| {
                    MezError::from(error).with_context("open existing budget accounting store")
                })?;
        connection.busy_timeout(std::time::Duration::from_millis(250))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version != super::store::SCHEMA_VERSION {
            return Err(MezError::invalid_state(
                "budget accounting schema unavailable",
            ));
        }
        connection.execute_batch("PRAGMA foreign_keys=ON;")?;
        Ok(connection)
    }
}

fn namespace(connection: &Connection) -> Result<String> {
    let value: String = connection.query_row(
        "SELECT namespace FROM budget_namespace WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    validate_id(&value)?;
    Ok(value)
}

fn account(connection: &Connection, root: &BudgetRootId) -> Result<BudgetAccount> {
    let body: String = connection
        .query_row(
            "SELECT body FROM budget_roots WHERE id=?1",
            [root.as_str()],
            |r| r.get(0),
        )
        .map_err(|error| MezError::from(error).with_context("read original budget account"))?;
    let account: BudgetAccount = decode(&body)?;
    if &account.root != root {
        return Err(MezError::conflict("budget account identity mismatch"));
    }
    Ok(account)
}

fn validate_id(value: &str) -> Result<()> {
    mez_agent::token_budget::BudgetReceiptId::new(value)
        .map(|_| ())
        .map_err(|_| MezError::invalid_args("invalid budget accounting identity"))
}

fn encode<T: Serialize>(value: &T) -> Result<String> {
    let body = serde_json::to_string(value)
        .map_err(|_| MezError::invalid_args("invalid budget accounting record"))?;
    if body.len() > 4096 {
        return Err(MezError::invalid_args(
            "budget accounting record exceeds bound",
        ));
    }
    Ok(body)
}

fn decode<T: for<'de> Deserialize<'de>>(body: &str) -> Result<T> {
    if body.len() > 4096 {
        return Err(MezError::invalid_state(
            "stored budget accounting record exceeds bound",
        ));
    }
    serde_json::from_str(body)
        .map_err(|_| MezError::invalid_state("invalid stored budget accounting record"))
}

#[cfg(test)]
mod tests;
