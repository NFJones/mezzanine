//! Consistent bounded rolling telemetry reads over immutable attributed deltas.
//!
//! One read transaction captures the oldest eligible event and scans the largest
//! requested window once. Folding preserves unknown categories and groups exact
//! project/harness/model identity. No root discovery, registry I/O, transcript
//! reconstruction or per-project repeated scans occur in this owner.

use super::{AccountingProjectId, MezError, Result, TokenUsageStore, sqlite_i64};
use mez_agent::{ModelTokenUsage, ModelTokenUsageKey};
use rusqlite::params;
use std::collections::BTreeMap;

const MAX_HISTORY_EVENTS: usize = 1_000_000;

/// Immutable query scope; absent project plus false selects all partitions.
#[derive(Debug, Clone, Default)]
pub(crate) struct TokenHistoryScope {
    /// Exact opaque project filter, never a root path or current cwd.
    pub(crate) project: Option<AccountingProjectId>,
    /// Select legacy/unqualified rows only; incompatible with a project filter.
    pub(crate) unattributed_only: bool,
    /// Isolate the pre-harness native status reader until its migration lands.
    pub(crate) native_only: bool,
}

/// Exact expense partition; unknown project is explicit and harnesses never merge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TokenHistoryKey {
    /// Frozen project ID, absent for legacy/unqualified expense.
    pub(crate) project: Option<AccountingProjectId>,
    /// Reported source harness; native events use mez.
    pub(crate) harness: String,
    /// Existing normalized provider/model identity.
    pub(crate) model: ModelTokenUsageKey,
}

/// Normalized counters with reasoning coverage retained separately from numeric sum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenHistoryUsage {
    /// Raw inclusive counters; ratios and billed input are derived afterward.
    pub(crate) usage: ModelTokenUsage,
    /// False if any contributing record omitted reasoning; numeric zero is not proof.
    pub(crate) reasoning_known: bool,
}

/// All requested windows and their age boundary from one consistent database view.
#[derive(Debug, Clone)]
pub(crate) struct TokenHistorySnapshot {
    /// Caller-frozen UTC query instant shared across every project/window.
    pub(crate) now: u64,
    /// Oldest same-scope retained event at or before now, including outside windows.
    pub(crate) oldest_observed_at: Option<u64>,
    /// Exact day windows grouped by immutable accounting identity.
    pub(crate) windows: BTreeMap<u16, BTreeMap<TokenHistoryKey, TokenHistoryUsage>>,
}

/// Adds exact history counters while preserving existing unknown-cache semantics.
/// Every field is checked before returning a replacement, so errors cannot leave
/// a partly updated aggregate or a plausible saturated total.
pub(crate) fn checked_usage_sum(
    current: ModelTokenUsage,
    next: ModelTokenUsage,
) -> Result<ModelTokenUsage> {
    fn sum(a: u64, b: u64) -> Result<u64> {
        a.checked_add(b)
            .ok_or_else(|| MezError::invalid_state("token history aggregate overflow"))
    }
    fn optional(
        a: Option<u64>,
        b: Option<u64>,
        had_usage: bool,
        next_has_usage: bool,
    ) -> Result<Option<u64>> {
        match (a, b) {
            (Some(a), Some(b)) => sum(a, b).map(Some),
            (None, Some(b)) if !had_usage => Ok(Some(b)),
            (Some(a), None) if !next_has_usage => Ok(Some(a)),
            _ => Ok(None),
        }
    }
    Ok(ModelTokenUsage {
        input_tokens: sum(current.input_tokens, next.input_tokens)?,
        output_tokens: sum(current.output_tokens, next.output_tokens)?,
        reasoning_tokens: sum(current.reasoning_tokens, next.reasoning_tokens)?,
        cached_input_tokens: optional(
            current.cached_input_tokens,
            next.cached_input_tokens,
            !current.is_zero(),
            !next.is_zero(),
        )?,
        cache_write_input_tokens: optional(
            current.cache_write_input_tokens,
            next.cache_write_input_tokens,
            !current.is_zero(),
            !next.is_zero(),
        )?,
    })
}

impl TokenHistorySnapshot {
    /// Combines native project partitions for the legacy status renderer using
    /// checked arithmetic. Validate every window before exposing any table.
    pub(crate) fn native_model_windows(
        &self,
    ) -> Result<BTreeMap<u16, BTreeMap<ModelTokenUsageKey, ModelTokenUsage>>> {
        let mut windows = BTreeMap::new();
        for (days, partitions) in &self.windows {
            let mut models = BTreeMap::<ModelTokenUsageKey, ModelTokenUsage>::new();
            for (key, total) in partitions {
                let current = models.entry(key.model.clone()).or_default();
                *current = checked_usage_sum(*current, total.usage)?;
            }
            windows.insert(*days, models);
        }
        Ok(windows)
    }
}

impl TokenUsageStore {
    /// Reads bounded global, exact-project or unattributed history in one snapshot.
    /// Rejects invalid windows, contradictory scope and oversized scans rather than
    /// returning truncated totals. SQL parameters never carry filesystem authority.
    pub(crate) fn history_snapshot(
        &self,
        now: u64,
        days: &[u16],
        scope: &TokenHistoryScope,
    ) -> Result<TokenHistorySnapshot> {
        if days.len() > 5
            || days.iter().any(|day| !(1..=90).contains(day))
            || (scope.project.is_some() && scope.unattributed_only)
        {
            return Err(MezError::invalid_args(
                "invalid token history windows or scope",
            ));
        }
        let mut snapshot = TokenHistorySnapshot {
            now,
            oldest_observed_at: None,
            windows: days
                .iter()
                .copied()
                .map(|day| (day, BTreeMap::new()))
                .collect(),
        };
        let mut connection = self.open()?;
        let tx = connection.transaction()?;
        let project = scope.project.as_ref().map(AccountingProjectId::as_str);
        let oldest: Option<i64> = tx.query_row(
            "SELECT MIN(observed_at) FROM token_usage_events WHERE observed_at<=?1
                AND (?2 IS NULL OR project_id=?2) AND (?3=0 OR project_id IS NULL)
                AND (?4=0 OR event_source='native')",
            params![
                sqlite_i64(now, "history timestamp")?,
                project,
                scope.unattributed_only,
                scope.native_only
            ],
            |row| row.get(0),
        )?;
        snapshot.oldest_observed_at = oldest
            .map(|value| {
                u64::try_from(value)
                    .map_err(|_| MezError::invalid_state("stored history timestamp is negative"))
            })
            .transpose()?;
        let Some(largest) = days.iter().copied().max() else {
            return Ok(snapshot);
        };
        let cutoff = now.saturating_sub(u64::from(largest) * 86_400);
        let mut statement = tx.prepare(
            "SELECT observed_at,project_id,harness,provider,model,input_tokens,output_tokens,
                reasoning_tokens,cached_input_tokens,cache_write_input_tokens,reasoning_known
             FROM token_usage_events WHERE observed_at>=?1 AND observed_at<=?2
                AND (?3 IS NULL OR project_id=?3) AND (?4=0 OR project_id IS NULL)
                AND (?5=0 OR event_source='native') ORDER BY observed_at,id LIMIT ?6",
        )?;
        let mut rows = statement.query(params![
            sqlite_i64(cutoff, "history cutoff")?,
            sqlite_i64(now, "history timestamp")?,
            project,
            scope.unattributed_only,
            scope.native_only,
            (MAX_HISTORY_EVENTS + 1) as i64
        ])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            count += 1;
            if count > MAX_HISTORY_EVENTS {
                return Err(MezError::invalid_state(
                    "token history scan exceeds its bound",
                ));
            }
            let observed = super::store::row_u64(row, 0)?;
            let key = TokenHistoryKey {
                project: row
                    .get::<_, Option<String>>(1)?
                    .map(AccountingProjectId::from_stored)
                    .transpose()?,
                harness: row.get(2)?,
                model: ModelTokenUsageKey::new(row.get::<_, String>(3)?, row.get::<_, String>(4)?),
            };
            let usage = ModelTokenUsage {
                input_tokens: super::store::row_u64(row, 5)?,
                output_tokens: super::store::row_u64(row, 6)?,
                reasoning_tokens: super::store::row_u64(row, 7)?,
                cached_input_tokens: super::store::row_optional_u64(row, 8)?,
                cache_write_input_tokens: super::store::row_optional_u64(row, 9)?,
            };
            let reasoning_known: bool = row.get(10)?;
            for (day, window) in &mut snapshot.windows {
                if observed < now.saturating_sub(u64::from(*day) * 86_400) {
                    continue;
                }
                let total = window.entry(key.clone()).or_insert(TokenHistoryUsage {
                    usage: Default::default(),
                    reasoning_known: true,
                });
                total.usage = checked_usage_sum(total.usage, usage)?;
                total.reasoning_known &= reasoning_known;
            }
        }
        Ok(snapshot)
    }
}
