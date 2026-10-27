//! Bounded same-owner OpenCode checkpoint views derived from replay aliases.
//!
//! One deferred read transaction pins source, membership and absolute checkpoints.
//! Native reads require mapped sources; admitted legacy-launch writer reads may
//! lack that mapping but never create one or authorize native recovery.
//! Checkpoints lack owner/epoch columns: only source-wide aliases and independently
//! recomputed stream digests bind membership. Unaliased legacy history is excluded,
//! not imported or charged. Shared checkpoints are counted once, with checked
//! revisions/totals and exact model/project/category grouping. This prerequisite
//! does not consolidate actor caches, preserve their reset baselines by itself,
//! authorize native admission, acknowledge expense or recover another owner.

#![allow(
    dead_code,
    reason = "admitted projection consolidation consumes bounded snapshots"
)]

use std::collections::BTreeMap;

use super::{
    ExternalCounters, ExternalUsageCommit, ExternalUsageReport, MAX_OBSERVATIONS_PER_SOURCE,
    MezError, OptionalExtension, Result, TokenUsageStore, decode, external_observation_epoch,
    external_usage_stream_id,
};

/// Immutable read evidence, never a standalone storage commit or control grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalUsageGroupSnapshot {
    stream_id: String,
    revision: u64,
    totals: ExternalCounters,
    members: Vec<ExternalUsageCommit>,
}

impl ExternalUsageGroupSnapshot {
    /// Canonical model/project/category group under the original mapped owner.
    pub(crate) fn stream_id(&self) -> &str {
        &self.stream_id
    }
    /// Sum of unique checkpoint revisions; replay cannot increment this view.
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
    /// Absolute charged totals; unknown categories remain unknown.
    pub(crate) fn totals(&self) -> ExternalCounters {
        self.totals
    }
    /// Bounded original checkpoints for later actor-owned baseline reconciliation.
    pub(crate) fn members(&self) -> &[ExternalUsageCommit] {
        &self.members
    }
}

/// Canonical opaque observation IDs; plaintext and case aliases cannot select history.
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Validates bounded normalized names without reading any vendor payload.
fn inert(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

/// Strictly decodes an already-bounded BLOB slice, retaining NUL and byte identity.
fn text(bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes)
        .map_err(|_| MezError::invalid_state("OpenCode group metadata encoding invalid"))
}

/// Reads one non-null bounded raw field; never SQLite TEXT substring semantics.
fn field(row: &rusqlite::Row<'_>, index: usize) -> Result<String> {
    text(row.get(index)?)
}

/// Preserves a genuinely absent project/checkpoint separately from invalid bytes.
fn optional_field(row: &rusqlite::Row<'_>, index: usize) -> Result<Option<String>> {
    row.get::<_, Option<Vec<u8>>>(index)?.map(text).transpose()
}

impl TokenUsageStore {
    /// Derives one immutable group from an actor-frozen observation selector.
    /// Mapped source and canonical checkpoint evidence are required; absence is
    /// None, not fabricated zero. Missing/corrupt/over-capacity evidence rejects.
    /// Opening uses the existing store owner; the read transaction writes no
    /// aliases, checkpoints, receipts, request metadata or incurred usage.
    pub(crate) fn opencode_observation_group_snapshot(
        &self,
        report: &ExternalUsageReport,
    ) -> Result<Option<ExternalUsageGroupSnapshot>> {
        let mut connection = self.open()?;
        let tx = connection.transaction()?;
        let snapshot = snapshot_in_transaction(&tx, report)?;
        tx.commit()?;
        Ok(snapshot)
    }
}

/// Uses the supplied read or writer transaction without committing it. An exact
/// handoff can therefore validate coherent group evidence before alias commit;
/// failure leaves rollback with the transaction's existing owner.
pub(super) fn snapshot_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    report: &ExternalUsageReport,
) -> Result<Option<ExternalUsageGroupSnapshot>> {
    snapshot_with_scope(tx, report, false)
}

/// Called only by the admitted legacy-launch writer in its own transaction.
/// Missing native mapping is allowed here, not in native snapshot/recovery APIs;
/// all alias/checkpoint ownership, grouping and capacity checks remain shared.
pub(super) fn snapshot_for_legacy_launch(
    tx: &rusqlite::Transaction<'_>,
    report: &ExternalUsageReport,
) -> Result<Option<ExternalUsageGroupSnapshot>> {
    snapshot_with_scope(tx, report, true)
}

/// A legacy scope permits an absent mapping only; a conflicting existing native
/// mapping still rejects. No source mapping or replay authority is written here.
fn snapshot_with_scope(
    tx: &rusqlite::Transaction<'_>,
    report: &ExternalUsageReport,
    legacy_launch: bool,
) -> Result<Option<ExternalUsageGroupSnapshot>> {
    if report.harness != "opencode"
        || report.mode != "observation"
        || report.baseline
        || !digest(&report.event_id)
        || !inert(&report.owner)
        || !inert(&report.model.provider)
        || !inert(&report.model.model)
    {
        return Err(MezError::invalid_args(
            "OpenCode group selector is not a normalized observation",
        ));
    }
    report.counters.validate()?;
    let group = external_observation_epoch(report)?;
    let stream_id = external_usage_stream_id(&report.owner, &group)?;
    let harness: Option<Vec<u8>> = tx
        .query_row(
            "SELECT substr(CAST(harness AS BLOB),1,9) FROM external_usage_sources WHERE owner=?1",
            [&report.owner],
            |row| row.get(0),
        )
        .optional()?;
    if harness.as_deref() != Some(b"opencode".as_slice()) && !(legacy_launch && harness.is_none()) {
        return Err(MezError::forbidden(
            "OpenCode group requires a mapped native source",
        ));
    }
    let mut members = BTreeMap::new();
    {
        // The extra row detects corrupted/exhausted membership without an
        // unbounded scan. LEFT JOIN makes missing checkpoints fail closed.
        let mut statement = tx.prepare("SELECT substr(CAST(o.event_id AS BLOB),1,65),substr(CAST(o.stream_id AS BLOB),1,65),substr(CAST(s.harness AS BLOB),1,129),substr(CAST(s.provider AS BLOB),1,129),substr(CAST(s.model AS BLOB),1,129),substr(CAST(s.mode AS BLOB),1,17),s.revision,substr(CAST(s.totals AS BLOB),1,1025),substr(CAST(s.project_id AS BLOB),1,129),substr(CAST(o.fingerprint AS BLOB),1,65)
                FROM external_usage_observations o LEFT JOIN external_usage_streams s ON s.id=o.stream_id
                WHERE o.owner=?1 ORDER BY o.event_id LIMIT ?2")?;
        let mut rows = statement.query(rusqlite::params![
            report.owner,
            MAX_OBSERVATIONS_PER_SOURCE + 1
        ])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            count += 1;
            if count > MAX_OBSERVATIONS_PER_SOURCE {
                return Err(MezError::new(
                    crate::error::MezErrorKind::RateLimited,
                    "OpenCode group membership capacity unavailable",
                ));
            }
            let event = field(row, 0)?;
            let id = field(row, 1)?;
            let harness = optional_field(row, 2)?;
            let fingerprint = field(row, 9)?;
            if !digest(&event)
                || !digest(&id)
                || !digest(&fingerprint)
                || harness.as_deref() != Some("opencode")
            {
                return Err(MezError::invalid_state(
                    "OpenCode group checkpoint evidence unavailable",
                ));
            }
            let provider = field(row, 3)?;
            let model = field(row, 4)?;
            let mode = field(row, 5)?;
            let revision = super::super::store::row_u64(row, 6)?;
            let encoded = field(row, 7)?;
            let project = optional_field(row, 8)?;
            if !inert(&provider)
                || !inert(&model)
                || revision == 0
                || encoded.len() > 1024
                || project.as_deref().is_some_and(|value| !inert(value))
            {
                return Err(MezError::invalid_state(
                    "OpenCode group checkpoint identity invalid",
                ));
            }
            let totals = decode(&encoded)?;
            let mut checkpoint = report.clone();
            // Stored identities are evidence, not input to normalization.
            checkpoint.model = mez_agent::ModelTokenUsageKey { provider, model };
            checkpoint.project = project
                .map(super::super::AccountingProjectId::from_stored)
                .transpose()?;
            checkpoint.counters = totals;
            let epoch = external_observation_epoch(&checkpoint)?;
            let expected = match mode.as_str() {
                "observation" => external_usage_stream_id(&report.owner, &epoch)?,
                "delta" if revision == 1 => {
                    external_usage_stream_id(&report.owner, &format!("opencode-message:{event}"))?
                }
                _ => {
                    return Err(MezError::invalid_state(
                        "OpenCode group checkpoint mode changed",
                    ));
                }
            };
            if id != expected {
                return Err(MezError::invalid_state(
                    "OpenCode group checkpoint source changed",
                ));
            }
            if epoch == group {
                members.entry(id.clone()).or_insert(ExternalUsageCommit {
                    stream_id: id,
                    revision,
                    totals,
                    applied: false,
                });
            }
        }
    }
    if members.is_empty() {
        return Ok(None);
    }
    let mut totals = report.counters.zero_like();
    let mut revision = 0u64;
    for member in members.values() {
        totals = totals.add(member.totals)?;
        revision = revision
            .checked_add(member.revision)
            .filter(|value| *value <= i64::MAX as u64)
            .ok_or_else(|| MezError::invalid_state("OpenCode group revision exhausted"))?;
    }
    totals.validate()?;
    Ok(Some(ExternalUsageGroupSnapshot {
        stream_id,
        revision,
        totals,
        members: members.into_values().collect(),
    }))
}
