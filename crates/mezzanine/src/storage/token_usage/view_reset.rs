//! Durable pane-view reset facts, not expense, producer authority or a ledger.
//!
//! A reset freezes already projected absolute checkpoints. One transaction
//! validates every checkpoint against durable truth before moving any baseline.
//! Later committed expense is deliberately not included. An older worker cannot
//! move a newer baseline backwards; identical retries are inert. Cardinality is
//! bounded by existing durable streams, with at most 4096 members per operation.

use super::{ExternalCounters, ExternalUsageCommit, MezError, Result, TokenUsageStore};
use rusqlite::{OptionalExtension, params};

/// Reads one optional bounded reset inside the caller's coherent transaction.
/// Corrupt, future or counter-incompatible evidence is never treated as zero.
pub(super) fn baseline(
    tx: &rusqlite::Transaction<'_>,
    checkpoint: &ExternalUsageCommit,
) -> Result<ExternalCounters> {
    let saved: Option<(i64, Vec<u8>)> = tx.query_row(
        "SELECT revision,substr(CAST(totals AS BLOB),1,1025) FROM external_usage_view_resets WHERE stream_id=?1",
        [&checkpoint.stream_id], |row| Ok((row.get(0)?,row.get(1)?)),
    ).optional()?;
    let Some((revision, bytes)) = saved else {
        return Ok(checkpoint.totals.zero_like());
    };
    let revision = u64::try_from(revision)
        .ok()
        .filter(|revision| *revision > 0 && *revision <= checkpoint.revision)
        .ok_or_else(|| MezError::conflict("external reset checkpoint revision invalid"))?;
    let counters: ExternalCounters = serde_json::from_slice(&bytes)
        .map_err(|_| MezError::conflict("external reset checkpoint encoding invalid"))?;
    if bytes.len() > 1024 {
        return Err(MezError::conflict(
            "external reset checkpoint exceeds bound",
        ));
    }
    counters.validate()?;
    checkpoint.totals.difference(counters)?;
    if revision == checkpoint.revision && counters != checkpoint.totals {
        return Err(MezError::conflict(
            "external reset checkpoint totals changed",
        ));
    }
    Ok(counters)
}

impl TokenUsageStore {
    /// Commits ONLY captured pane-view baselines after checking real durable
    /// checkpoints. This worker-only operation never charges, creates streams,
    /// resets global totals, or grants source recovery. All members commit or
    /// none do; missing and inconsistent preimages refuse without partial reset.
    pub(crate) fn reset_external_usage_view(
        &self,
        checkpoints: &[ExternalUsageCommit],
    ) -> Result<()> {
        if checkpoints.len() > 4096 {
            return Err(MezError::invalid_args(
                "external reset checkpoint capacity unavailable",
            ));
        }
        let mut connection = self.open()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut unique = std::collections::BTreeSet::new();
        for captured in checkpoints {
            if captured.stream_id.len() != 64
                || !captured
                    .stream_id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                || captured.revision == 0
                || !unique.insert(&captured.stream_id)
            {
                return Err(MezError::invalid_args(
                    "external reset checkpoint identity invalid",
                ));
            }
            captured.totals.validate()?;
            let current: Option<(i64, Vec<u8>)> = tx.query_row(
                "SELECT revision,substr(CAST(totals AS BLOB),1,1025) FROM external_usage_streams WHERE id=?1",
                [&captured.stream_id], |row| Ok((row.get(0)?,row.get(1)?)),
            ).optional()?;
            let (revision, bytes) =
                current.ok_or_else(|| MezError::conflict("external reset checkpoint missing"))?;
            let revision = u64::try_from(revision)
                .map_err(|_| MezError::conflict("external reset checkpoint revision invalid"))?;
            let totals: ExternalCounters = serde_json::from_slice(&bytes)
                .map_err(|_| MezError::conflict("external reset checkpoint encoding invalid"))?;
            if bytes.len() > 1024 || revision < captured.revision {
                return Err(MezError::conflict("external reset checkpoint unavailable"));
            }
            totals.validate()?;
            totals.difference(captured.totals)?;
            if revision == captured.revision && totals != captured.totals {
                return Err(MezError::conflict("external reset captured totals changed"));
            }
            let current = ExternalUsageCommit {
                stream_id: captured.stream_id.clone(),
                revision,
                totals,
                applied: false,
            };
            let old = baseline(&tx, &current)?;
            // A newer accepted reset wins even if this worker finished late.
            // Counter monotonicity, not caller-selected operation order, proves
            // this reset already covered the older captured point.
            if old.difference(captured.totals).is_ok() {
                continue;
            }
            captured.totals.difference(old)?;
            let encoded = serde_json::to_string(&captured.totals).map_err(|_| {
                MezError::invalid_state("external reset checkpoint encoding failed")
            })?;
            tx.execute(
                "INSERT INTO external_usage_view_resets(stream_id,revision,totals) VALUES(?1,?2,?3) ON CONFLICT(stream_id) DO UPDATE SET revision=excluded.revision,totals=excluded.totals",
                params![captured.stream_id, super::sqlite_i64(captured.revision,"reset revision")?, encoded],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
