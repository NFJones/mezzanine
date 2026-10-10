//! Durable source-scoped request origins, not reporting credentials.
//!
//! Native admission owns source/harness and qualifies the project before this
//! worker-only transaction. An opaque request marker narrows one native source;
//! possession alone grants no reporting or execution rights. Exact retries keep
//! the first origin/time, including explicit unattributed state. No paths,
//! capabilities, callback bodies or provider work are persisted here.

use super::{AccountingProjectId, MezError, Result, TokenUsageStore, sqlite_i64};
use rusqlite::{OptionalExtension, params};

/// First accepted immutable origin for a source-scoped request marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalRequestOrigin {
    /// Opaque canonical digest supplied as an idempotent request selector.
    pub request_id: String,
    /// Server-frozen project identity, never a caller-selected label.
    pub project: Option<AccountingProjectId>,
    /// Original accepted request boundary, not a replay/reconnect timestamp.
    pub admitted_at: u64,
}

/// Sealed report provenance loaded under a mapped native source. Fields remain
/// private so completion cannot substitute a new project after qualification.
#[derive(Debug, Clone)]
pub(crate) struct ExternalRequestReport {
    report: super::ExternalUsageReport,
    origin: ExternalRequestOrigin,
}

impl ExternalRequestReport {
    /// Returns only the original frozen normalized report for queue/projection.
    pub(crate) fn report(&self) -> &super::ExternalUsageReport {
        &self.report
    }
    /// Returns source-scoped immutable marker provenance, not a credential.
    pub(crate) fn origin(&self) -> &ExternalRequestOrigin {
        &self.origin
    }
}

/// Canonical opaque selectors cannot store callback content or case aliases.
pub(super) fn valid_request_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl TokenUsageStore {
    /// Loads the original request origin for a native observation without
    /// reading current cwd or accepting a report-selected project. Missing or
    /// foreign markers cannot authorize historical import. Call only off actor.
    pub(crate) fn freeze_external_request_report(
        &self,
        report: &super::ExternalUsageReport,
        request_id: &str,
    ) -> Result<ExternalRequestReport> {
        crate::integrations::harness_policy::require_active_external_harness(&report.harness)?;
        if report.mode != "observation"
            || !valid_request_id(request_id)
            || report.owner.is_empty()
            || report.owner.len() > 128
        {
            return Err(MezError::invalid_args(
                "external request report unavailable",
            ));
        }
        report.counters.validate()?;
        let connection = self.open()?;
        let row:Option<(Option<String>,u64)>=connection.query_row(
            "SELECT r.project_id,r.admitted_at FROM external_usage_requests r JOIN external_usage_sources s ON s.owner=r.owner
             WHERE r.owner=?1 AND r.request_id=?2 AND r.harness=?3 AND s.harness=?3",
            params![report.owner,request_id,report.harness],|row|Ok((row.get(0)?,super::store::row_u64(row,1)?)),
        ).optional()?;
        let (project, admitted_at) =
            row.ok_or_else(|| MezError::forbidden("external request original source unavailable"))?;
        if report.observed_at < admitted_at {
            return Err(MezError::conflict(
                "external observation predates its admitted request",
            ));
        }
        let origin = ExternalRequestOrigin {
            request_id: request_id.into(),
            project: project.map(AccountingProjectId::from_stored).transpose()?,
            admitted_at,
        };
        let mut report = report.clone();
        report.project = origin.project.clone();
        report.epoch = super::external::external_observation_epoch(&report)?;
        Ok(ExternalRequestReport { report, origin })
    }

    /// Records one qualified native request origin without emitting expense.
    /// Reopen/retry preserves first provenance; unknown sources and finite
    /// namespace exhaustion reject without eviction. Call only off actor.
    pub(crate) fn begin_external_request(
        &self,
        owner: &str,
        harness: &str,
        request_id: &str,
        project: Option<&AccountingProjectId>,
        admitted_at: u64,
    ) -> Result<ExternalRequestOrigin> {
        crate::integrations::harness_policy::require_active_external_harness(harness)?;
        if !valid_request_id(request_id)
            || admitted_at == 0
            || owner.is_empty()
            || owner.len() > 128
        {
            return Err(MezError::invalid_args(
                "external request marker unavailable",
            ));
        }
        let timestamp = sqlite_i64(admitted_at, "request admission time")?;
        let mut connection = self.open()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let source: Option<String> = tx
            .query_row(
                "SELECT harness FROM external_usage_sources WHERE owner=?1",
                [owner],
                |row| row.get(0),
            )
            .optional()?;
        if source.as_deref() != Some(harness) {
            return Err(MezError::forbidden(
                "external request requires a mapped native source",
            ));
        }
        let previous:Option<(String,Option<String>,u64)>=tx.query_row("SELECT harness,project_id,admitted_at FROM external_usage_requests WHERE owner=?1 AND request_id=?2",params![owner,request_id],|row|Ok((row.get(0)?,row.get(1)?,super::store::row_u64(row,2)?))).optional()?;
        if let Some((old_harness, project, admitted_at)) = previous {
            if old_harness != harness {
                return Err(MezError::conflict("external request source changed"));
            }
            return Ok(ExternalRequestOrigin {
                request_id: request_id.into(),
                project: project.map(AccountingProjectId::from_stored).transpose()?,
                admitted_at,
            });
        }
        let source_count: i64 = tx.query_row(
            "SELECT count(*) FROM external_usage_requests WHERE owner=?1",
            [owner],
            |row| row.get(0),
        )?;
        let total_count: i64 =
            tx.query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                row.get(0)
            })?;
        if source_count >= 65_536 || total_count >= 1_048_576 {
            return Err(MezError::new(
                crate::error::MezErrorKind::RateLimited,
                "durable external request capacity unavailable",
            ));
        }
        tx.execute("INSERT INTO external_usage_requests(owner,request_id,harness,project_id,admitted_at) VALUES(?1,?2,?3,?4,?5)",params![owner,request_id,harness,project.map(AccountingProjectId::as_str),timestamp])?;
        tx.commit()?;
        Ok(ExternalRequestOrigin {
            request_id: request_id.into(),
            project: project.cloned(),
            admitted_at,
        })
    }
}
