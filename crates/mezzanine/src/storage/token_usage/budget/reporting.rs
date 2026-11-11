//! Receipt-owned projection into the existing native rolling-event ledger.
//!
//! Root totals remain the lifetime authority. Reporting uses original attempt
//! project/model plus immutable observation time, and differences normalized
//! cumulative counters against the last reported snapshot. It never adds cache
//! or reasoning to already inclusive input/output. Partial root evidence remains
//! valid without a reporting observation; no missing dimension becomes zero.

use super::receipts::BudgetReceipt;
use super::*;
use crate::storage::token_usage::{AccountingOrigin, AccountingProjectId, TokenUsageEvent};
use mez_agent::{ModelTokenUsage, token_budget::UsageSnapshot};

/// Inert original attribution. Possession grants no filesystem/control rights.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetReportingOrigin {
    pub project: Option<String>,
}

impl BudgetReportingOrigin {
    /// I/O-free capture from the existing trusted request-attribution owner.
    pub(crate) fn capture(origin: &AccountingOrigin) -> Self {
        Self {
            project: origin.project_id().map(|id| id.as_str().to_owned()),
        }
    }

    pub(super) fn validate(&self) -> Result<()> {
        if let Some(project) = &self.project {
            AccountingProjectId::from_stored(project.clone())?;
        }
        Ok(())
    }
}

/// Actual provider evidence with both normalized dimensions known. Its clock
/// is frozen with this receipt, not resampled during retries or recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetReportingObservation {
    pub observed_at_unix_seconds: u64,
    pub usage: ModelTokenUsage,
}

/// Precision of this rolling partition, not a provider-completeness claim.
/// Unassignable late categories remain exact in cumulative receipt evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetReportingProjection {
    pub event_written: bool,
    pub cache_read_partition_exact: bool,
    pub cache_write_partition_exact: bool,
    pub reasoning_partition_exact: bool,
}

/// Category coverage at the same boundary as the last reported input total.
/// Keeping old known counters does not prove a newer omitted category baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BudgetReportingBaseline {
    cache_read_known: bool,
    cache_write_known: bool,
}

impl BudgetReportingBaseline {
    pub(super) fn capture(usage: ModelTokenUsage) -> Self {
        Self {
            cache_read_known: usage.cached_input_tokens.is_some(),
            cache_write_known: usage.cache_write_input_tokens.is_some(),
        }
    }
}

pub(super) fn settle_reporting_event(
    tx: &rusqlite::Transaction<'_>,
    receipt: &BudgetReceipt,
    known: UsageSnapshot,
    previous: Option<ModelTokenUsage>,
    baseline: Option<BudgetReportingBaseline>,
) -> Result<(Option<ModelTokenUsage>, Option<BudgetReportingProjection>)> {
    let Some(observation) = &receipt.reporting else {
        return Ok((previous, None));
    };
    super::super::sqlite_i64(
        observation.observed_at_unix_seconds,
        "budget reporting timestamp",
    )?;
    let origin =
        receipt.attempt.reporting_origin.as_ref().ok_or_else(|| {
            MezError::conflict("budget reporting original attribution unavailable")
        })?;
    origin.validate()?;
    let usage = observation.usage;
    if known.input != Some(usage.input_tokens) || known.output != Some(usage.output_tokens) {
        return Err(MezError::conflict(
            "budget reporting differs from known inclusive usage",
        ));
    }
    let old = previous.unwrap_or_default();
    let merged = ModelTokenUsage {
        cached_input_tokens: usage.cached_input_tokens.or(old.cached_input_tokens),
        cache_write_input_tokens: usage
            .cache_write_input_tokens
            .or(old.cache_write_input_tokens),
        ..usage
    };
    let caches = merged
        .cached_input_tokens
        .unwrap_or(0)
        .checked_add(merged.cache_write_input_tokens.unwrap_or(0))
        .ok_or_else(|| MezError::conflict("budget reporting cache overflow"))?;
    if caches > merged.input_tokens || merged.reasoning_tokens > merged.output_tokens {
        return Err(MezError::conflict(
            "budget reporting category subsets inconsistent",
        ));
    }
    let mut delta = ModelTokenUsage {
        input_tokens: difference(usage.input_tokens, old.input_tokens)?,
        output_tokens: difference(usage.output_tokens, old.output_tokens)?,
        reasoning_tokens: difference(usage.reasoning_tokens, old.reasoning_tokens)?,
        cached_input_tokens: optional_difference(
            usage.cached_input_tokens,
            old.cached_input_tokens,
        )?,
        cache_write_input_tokens: optional_difference(
            usage.cache_write_input_tokens,
            old.cache_write_input_tokens,
        )?,
    };
    // A disclosure can revise category knowledge about an older prefix. There
    // is no evidence assigning the excess category to this newest interval.
    // Do not let legacy additive-input compatibility inflate inclusive expense.
    if previous.is_some() {
        if !baseline.is_some_and(|baseline| baseline.cache_read_known) {
            delta.cached_input_tokens = None;
        }
        if !baseline.is_some_and(|baseline| baseline.cache_write_known) {
            delta.cache_write_input_tokens = None;
        }
    }
    let delta_caches = delta
        .cached_input_tokens
        .unwrap_or(0)
        .checked_add(delta.cache_write_input_tokens.unwrap_or(0))
        .ok_or_else(|| MezError::conflict("budget reporting cache delta overflow"))?;
    if delta_caches > delta.input_tokens {
        if delta.cached_input_tokens != Some(0) {
            delta.cached_input_tokens = None;
        }
        if delta.cache_write_input_tokens != Some(0) {
            delta.cache_write_input_tokens = None;
        }
    }
    let reasoning_exact = delta.reasoning_tokens <= delta.output_tokens;
    if !reasoning_exact {
        delta.reasoning_tokens = 0;
    }
    let mut projection = BudgetReportingProjection {
        event_written: false,
        cache_read_partition_exact: delta.cached_input_tokens.is_some(),
        cache_write_partition_exact: delta.cache_write_input_tokens.is_some(),
        reasoning_partition_exact: reasoning_exact,
    };
    let event = TokenUsageEvent {
        id: format!("budget-receipt:{}", receipt.id.as_str()),
        project: origin
            .project
            .clone()
            .map(AccountingProjectId::from_stored)
            .transpose()?,
        observed_at_unix_seconds: observation.observed_at_unix_seconds,
        model: receipt.attempt.model.clone(),
        usage: delta,
    };
    // An exact receipt replay returned before this helper. With that receipt
    // absent, an occupied identity is foreign/inconsistent even if bytes match.
    let occupied: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM token_usage_events WHERE id=?1)",
        [&event.id],
        |row| row.get(0),
    )?;
    if occupied {
        return Err(MezError::conflict(
            "budget reporting event identity already owned",
        ));
    }
    // Late category disclosures can describe a previously observed prefix; only
    // inclusive input/output deltas add expense. All rows share this transaction.
    projection.event_written = TokenUsageStore::append_native_event_in_transaction(tx, &event)?;
    if projection.event_written && !reasoning_exact {
        tx.execute(
            "UPDATE token_usage_events SET reasoning_known=0 WHERE id=?1",
            [&event.id],
        )?;
    }
    Ok((Some(merged), Some(projection)))
}

fn difference(next: u64, previous: u64) -> Result<u64> {
    next.checked_sub(previous)
        .ok_or_else(|| MezError::conflict("budget reporting cumulative usage regressed"))
}

fn optional_difference(next: Option<u64>, previous: Option<u64>) -> Result<Option<u64>> {
    next.map(|next| difference(next, previous.unwrap_or(0)))
        .transpose()
}
