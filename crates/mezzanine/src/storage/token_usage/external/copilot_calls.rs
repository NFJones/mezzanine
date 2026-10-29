//! Canonical terminal Copilot calls inside the existing expense transaction.
//!
//! A call digest narrows metadata only after native runtime admission. It is
//! never a source credential or a request-start/original-project claim. The
//! first mapped reporter owns the immutable expense and observation time.
//! Other reporters may acknowledge that same call, but cannot project its
//! checkpoint into their pane. Alias and expense commit atomically; no retained
//! tombstone is evicted to permit another charge.

use super::{
    ExternalUsageCommit, ExternalUsageReport, GroupPublication, MezError, Result, TokenUsageStore,
    decode, external_observation_epoch, external_usage_stream_id,
};
use rusqlite::{OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

/// Actual durable terminal outcome; only the original owner may project it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopilotTerminalCommit {
    /// Private to prevent a cross-owner receipt becoming a pane checkpoint.
    commit: ExternalUsageCommit,
    /// Owner equality is necessary, not sufficient, for runtime projection.
    same_owner: bool,
}

impl CopilotTerminalCommit {
    /// Returns a checkpoint only for the original accounting owner. The caller
    /// must still qualify its retained native pane/root and accounting instance.
    pub(crate) fn projection(&self) -> Option<&ExternalUsageCommit> {
        self.same_owner.then_some(&self.commit)
    }

    /// True only when this committed transaction accepted a previously unseen
    /// call. Explicit zero counters may be accepted without a nonzero expense.
    pub(crate) fn applied(&self) -> bool {
        self.commit.applied
    }

    /// Durable original stream revision, never a newly allocated replay ID.
    pub(crate) fn revision(&self) -> u64 {
        self.commit.revision
    }
}

/// Hashes a client-redacted call selector under the native authenticated UID.
/// This is expense idempotency metadata, never native source authorization.
pub(crate) fn copilot_terminal_call_id(uid: u32, call: &str) -> Result<String> {
    if !super::super::external_requests::valid_request_id(call) {
        return Err(MezError::invalid_args("Copilot call identity unavailable"));
    }
    digest(&("mez-copilot-terminal-call/1", uid, call))
}

/// Canonical encoding keeps variable-length fields and domains unambiguous.
fn digest(value: &impl serde::Serialize) -> Result<String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| MezError::invalid_state("Copilot call encoding failed"))?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Transaction-local alias scope constructed only by the fixed store entry.
pub(super) struct Publication<'a> {
    /// UID-separated opaque call identity, also the observation's event ID.
    key: String,
    /// Semantic call payload excludes reporter and delayed arrival time.
    fingerprint: String,
    /// Published only after the real commit, including compatible replay.
    output: &'a mut Option<CopilotTerminalCommit>,
}

/// First durable reporter/time; no vendor payload or original project is kept.
struct StoredCall {
    /// Strict fingerprint of reported model and optional-category counters.
    fingerprint: String,
    /// First server-owned mapped reporter.
    owner: String,
    /// Original checkpoint, never a successor reporter's stream.
    stream: String,
    /// First terminal observation time, not claimed provider dispatch time.
    observed_at: u64,
}

impl Publication<'_> {
    /// Looks up canonical replay before source-local observation capacity.
    /// A corrupt/missing original receipt fails instead of minting new expense.
    pub(super) fn replay(
        &self,
        tx: &Transaction<'_>,
        report: &ExternalUsageReport,
    ) -> Result<Option<CopilotTerminalCommit>> {
        let stored = tx
            .query_row(
                "SELECT fingerprint,owner,stream_id,observed_at
                 FROM external_copilot_calls WHERE id=?1",
                [&self.key],
                |row| {
                    Ok(StoredCall {
                        fingerprint: row.get(0)?,
                        owner: row.get(1)?,
                        stream: row.get(2)?,
                        observed_at: super::super::store::row_u64(row, 3)?,
                    })
                },
            )
            .optional()?;
        let Some(stored) = stored else {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM external_usage_observations
                 WHERE owner=?1 AND event_id=?2)",
                params![report.owner, self.key],
                |row| row.get(0),
            )?;
            if exists {
                return Err(MezError::conflict(
                    "Copilot observation lacks its atomic canonical alias",
                ));
            }
            return Ok(None);
        };
        if stored.fingerprint != self.fingerprint
            || stored.stream
                != external_usage_stream_id(&stored.owner, &external_observation_epoch(report)?)?
        {
            return Err(MezError::conflict("Copilot call immutable payload changed"));
        }
        let receipt: Option<(String, String, Option<u64>)> = tx
            .query_row(
                "SELECT s.harness,o.stream_id,r.observed_at
                 FROM external_usage_sources s
                 JOIN external_usage_observations o ON o.owner=s.owner
                 LEFT JOIN external_usage_receipts r
                   ON r.stream_id=o.stream_id AND r.event_id=o.event_id
                 WHERE s.owner=?1 AND o.event_id=?2",
                params![stored.owner, self.key],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        super::super::store::row_optional_u64(row, 2)?,
                    ))
                },
            )
            .optional()?;
        if receipt.as_ref().is_none_or(|(harness, stream, time)| {
            harness != "copilot"
                || stream != &stored.stream
                || time.is_some_and(|time| time != stored.observed_at)
        }) {
            return Err(MezError::conflict(
                "Copilot original call receipt unavailable",
            ));
        }
        let (revision, totals, harness, provider, model, project, mode): (
            u64,
            String,
            String,
            String,
            String,
            Option<String>,
            String,
        ) = tx.query_row(
            "SELECT revision,totals,harness,provider,model,project_id,mode
             FROM external_usage_streams WHERE id=?1",
            [&stored.stream],
            |row| {
                Ok((
                    super::super::store::row_u64(row, 0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )?;
        if harness != "copilot"
            || provider != report.model.provider
            || model != report.model.model
            || project.is_some()
            || mode != "observation"
        {
            return Err(MezError::conflict(
                "Copilot original call checkpoint changed",
            ));
        }
        Ok(Some(CopilotTerminalCommit {
            same_owner: stored.owner == report.owner,
            commit: ExternalUsageCommit {
                stream_id: stored.stream,
                revision,
                totals: decode(&totals)?,
                applied: false,
            },
        }))
    }

    /// Commits a read-only compatible replay before publishing its receipt.
    pub(super) fn finish_replay(
        self,
        tx: Transaction<'_>,
        replay: CopilotTerminalCommit,
    ) -> Result<ExternalUsageCommit> {
        tx.commit()?;
        let commit = replay.commit.clone();
        *self.output = Some(replay);
        Ok(commit)
    }

    /// Inserts the canonical alias after the expense/checkpoint statements but
    /// before their SAME commit. Failure rolls back every effect together.
    pub(super) fn finish_new(
        self,
        tx: Transaction<'_>,
        report: &ExternalUsageReport,
        commit: ExternalUsageCommit,
    ) -> Result<ExternalUsageCommit> {
        if !commit.applied {
            return Err(MezError::conflict("Copilot canonical call was not new"));
        }
        tx.execute(
            "INSERT INTO external_copilot_calls(id,fingerprint,owner,stream_id,observed_at)
             VALUES(?1,?2,?3,?4,?5)",
            params![
                self.key,
                self.fingerprint,
                report.owner,
                commit.stream_id,
                super::sqlite_i64(report.observed_at, "Copilot observation time")?
            ],
        )?;
        tx.commit()?;
        *self.output = Some(CopilotTerminalCommit {
            commit: commit.clone(),
            same_owner: true,
        });
        Ok(commit)
    }
}

impl TokenUsageStore {
    /// Commits one already-native-admitted terminal call with unknown request
    /// origin. No request-start ticket, project or source mapping is created.
    /// Cross-reporter replay acknowledges the original expense without exposing
    /// a projectable checkpoint. Call ONLY off actor after native authorization.
    pub(crate) fn ingest_copilot_terminal(
        &self,
        report: &ExternalUsageReport,
        uid: u32,
        call: &str,
        now: u64,
    ) -> Result<CopilotTerminalCommit> {
        self.ingest_copilot_terminal_with_admission(
            report,
            uid,
            call,
            now,
            &mut super::super::ExternalUsageAdmissionWitness::default(),
        )
    }

    /// Shares the real writer while exposing only an invocation-local refusal
    /// witness to storage tests. This does not enable runtime retry scheduling.
    pub(in crate::storage::token_usage) fn ingest_copilot_terminal_with_admission(
        &self,
        report: &ExternalUsageReport,
        uid: u32,
        call: &str,
        now: u64,
        admission: &mut super::super::ExternalUsageAdmissionWitness,
    ) -> Result<CopilotTerminalCommit> {
        let key = copilot_terminal_call_id(uid, call)?;
        if report.harness != "copilot"
            || report.model.provider != "copilot"
            || report.project.is_some()
            || report.mode != "observation"
            || report.baseline
            || report.sequence != 1
            || report.observed_at == 0
            || report.event_id != key
        {
            return Err(MezError::invalid_args(
                "Copilot terminal call scope unavailable",
            ));
        }
        let fingerprint = digest(&(
            "mez-copilot-terminal-payload/1",
            &report.model.provider,
            &report.model.model,
            report.counters,
        ))?;
        let mut output = None;
        self.ingest_external_with_admission(
            report,
            now,
            None,
            None,
            GroupPublication::CopilotTerminal(Publication {
                key,
                fingerprint,
                output: &mut output,
            }),
            admission,
        )?;
        output.ok_or_else(|| MezError::invalid_state("Copilot durable call receipt unpublished"))
    }
}
