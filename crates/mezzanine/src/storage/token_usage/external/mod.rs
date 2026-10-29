//! Transactional external telemetry streams and durable replay receipts.
//!
//! A server-issued owner namespaces each epoch. Modes never mix within an epoch;
//! cumulative attachment starts with an explicit uncharged baseline. Optional
//! counters cannot change availability midstream: a new epoch is required. A
//! durable sequence high-water mark prevents old replay after receipt pruning.
//! Normalized deltas, receipts and checkpoints commit together, without prompts,
//! transcript text, pane paths or vendor payloads.
//! Native immutable observations additionally share model/category/project
//! streams and source-wide opaque ID tombstones. This internal storage mode
//! neither enables ordinary RPC admission nor authorizes historical import.
//! A separately scoped legacy-launch writer shares these transactions after
//! runtime capability admission. It preserves exact preexisting receipts while
//! charging fresh completed messages into shared streams, without allocating
//! native mappings or granting recovery authority.
//! Copilot terminal aliases share this same transaction while preserving unknown
//! request origin and keeping cross-reporter replay outside pane projection.

use mez_agent::{ModelTokenUsage, ModelTokenUsageKey};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{MezError, Result, TokenUsageStore, sqlite_i64};

/// Canonical terminal accounting under separately qualified native ingress.
pub(super) mod copilot_calls;

/// Read-transaction projection prerequisites; never another expense store.
mod group_snapshot;
pub(crate) use group_snapshot::ExternalUsageGroupSnapshot;

/// Group publication scope, fixed by the server entry point rather than input.
/// Legacy launches retain their existing capability authority only; they cannot
/// become mapped native sources or acquire cold-recovery/request authority.
enum GroupPublication<'a> {
    /// Default native operations require an exact durable native mapping.
    Native(Option<&'a mut Option<ExternalUsageGroupSnapshot>>),
    /// An already-authorized legacy OpenCode launch publishes one coherent group.
    LegacyLaunch(&'a mut Option<ExternalUsageGroupSnapshot>),
    /// Canonical terminal calls retain native mapping and one first reporter.
    CopilotTerminal(copilot_calls::Publication<'a>),
}

impl GroupPublication<'_> {
    /// Only the explicitly scoped legacy-launch writer can waive native mapping.
    fn is_legacy_launch(&self) -> bool {
        matches!(self, Self::LegacyLaunch(_))
    }
}

/// Immutable source ID tombstones are retained, never silently evicted.
const MAX_OBSERVATIONS_PER_SOURCE: i64 = 65_536;
/// Total durable observation namespace capacity across all native sources.
const MAX_OBSERVATIONS_TOTAL: i64 = 1_048_576;

/// Checks new-ID capacity after exact replay lookup, without modifying or
/// evicting existing evidence. Invalid count combinations fail closed too.
pub(super) fn require_observation_capacity(source_count: i64, total_count: i64) -> Result<()> {
    if source_count < 0
        || total_count < source_count
        || source_count >= MAX_OBSERVATIONS_PER_SOURCE
        || total_count >= MAX_OBSERVATIONS_TOTAL
    {
        return Err(MezError::new(
            crate::error::MezErrorKind::RateLimited,
            "durable external observation capacity unavailable",
        ));
    }
    Ok(())
}

/// Content-free counters; omitted fields stay unknown rather than becoming zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExternalCounters {
    /// Inclusive provider-visible input tokens.
    pub input_tokens: u64,
    /// Inclusive provider-visible output tokens.
    pub output_tokens: u64,
    /// Reasoning subset of output, or unknown.
    pub reasoning_tokens: Option<u64>,
    /// Cache-read subset of input, or unknown.
    pub cached_input_tokens: Option<u64>,
    /// Cache-write subset of input, or unknown.
    pub cache_write_input_tokens: Option<u64>,
}

impl ExternalCounters {
    /// Rejects overflow and inconsistent inclusive cache/reasoning subsets.
    /// Known cache reads/writes are disjoint; unknown categories stay unknown
    /// while each known category must still fit the inclusive input total.
    pub(crate) fn validate(self) -> Result<()> {
        for value in [
            Some(self.input_tokens),
            Some(self.output_tokens),
            self.reasoning_tokens,
            self.cached_input_tokens,
            self.cache_write_input_tokens,
        ]
        .into_iter()
        .flatten()
        {
            sqlite_i64(value, "external counter")?;
        }
        if self
            .reasoning_tokens
            .is_some_and(|n| n > self.output_tokens)
            || self
                .cached_input_tokens
                .is_some_and(|n| n > self.input_tokens)
            || self
                .cache_write_input_tokens
                .is_some_and(|n| n > self.input_tokens)
            || self
                .cached_input_tokens
                .zip(self.cache_write_input_tokens)
                .is_some_and(|(read, write)| {
                    read.checked_add(write)
                        .is_none_or(|total| total > self.input_tokens)
                })
        {
            return Err(MezError::invalid_args(
                "external counters must use inclusive input/output subsets",
            ));
        }
        Ok(())
    }

    /// Computes a monotonic difference without guessing unknown categories.
    pub(crate) fn difference(self, previous: Self) -> Result<Self> {
        fn scalar(next: u64, old: u64) -> Result<u64> {
            next.checked_sub(old).ok_or_else(|| {
                MezError::conflict("external cumulative counters regressed; start a new epoch")
            })
        }
        fn optional(next: Option<u64>, old: Option<u64>) -> Result<Option<u64>> {
            match (next, old) {
                (Some(next), Some(old)) => scalar(next, old).map(Some),
                (None, None) => Ok(None),
                _ => Err(MezError::conflict(
                    "external counter availability changed; start a new epoch",
                )),
            }
        }
        Ok(Self {
            input_tokens: scalar(self.input_tokens, previous.input_tokens)?,
            output_tokens: scalar(self.output_tokens, previous.output_tokens)?,
            reasoning_tokens: optional(self.reasoning_tokens, previous.reasoning_tokens)?,
            cached_input_tokens: optional(self.cached_input_tokens, previous.cached_input_tokens)?,
            cache_write_input_tokens: optional(
                self.cache_write_input_tokens,
                previous.cache_write_input_tokens,
            )?,
        })
    }

    /// Adds disjoint observations with checked arithmetic and unknown fidelity.
    pub(crate) fn add(self, delta: Self) -> Result<Self> {
        fn scalar(a: u64, b: u64) -> Result<u64> {
            a.checked_add(b)
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(|| {
                    MezError::invalid_args("external stream total exceeded SQLite range")
                })
        }
        fn optional(a: Option<u64>, b: Option<u64>) -> Result<Option<u64>> {
            match (a, b) {
                (Some(a), Some(b)) => scalar(a, b).map(Some),
                _ => Ok(None),
            }
        }
        Ok(Self {
            input_tokens: scalar(self.input_tokens, delta.input_tokens)?,
            output_tokens: scalar(self.output_tokens, delta.output_tokens)?,
            reasoning_tokens: optional(self.reasoning_tokens, delta.reasoning_tokens)?,
            cached_input_tokens: optional(self.cached_input_tokens, delta.cached_input_tokens)?,
            cache_write_input_tokens: optional(
                self.cache_write_input_tokens,
                delta.cache_write_input_tokens,
            )?,
        })
    }

    /// Creates a zero baseline retaining the sample's category availability.
    pub(crate) fn zero_like(self) -> Self {
        Self {
            reasoning_tokens: self.reasoning_tokens.map(|_| 0),
            cached_input_tokens: self.cached_input_tokens.map(|_| 0),
            cache_write_input_tokens: self.cache_write_input_tokens.map(|_| 0),
            ..Self::default()
        }
    }

    /// Adapts to existing normalized totals; coverage retains unknown reasoning.
    pub(crate) fn normalized(self) -> ModelTokenUsage {
        ModelTokenUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            reasoning_tokens: self.reasoning_tokens.unwrap_or(0),
            cached_input_tokens: self.cached_input_tokens,
            cache_write_input_tokens: self.cache_write_input_tokens,
        }
    }
}

/// Exact actor-authorized immutable accounting report. Owner/harness are server-owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalUsageReport {
    /// Opaque server-issued accounting namespace, not client-selected identity.
    pub owner: String,
    /// Server-frozen project origin; never a hook-supplied label.
    pub project: Option<super::AccountingProjectId>,
    /// Harness fixed by launch authority.
    pub harness: String,
    /// Disjoint source epoch within the registered owner.
    pub epoch: String,
    /// Stable observation identity used for fingerprint comparison.
    pub event_id: String,
    /// Positive legacy sequence; immutable observations use a server ordinal.
    pub sequence: u64,
    /// Delta, cumulative, or internal immutable observation stream semantics.
    pub mode: String,
    /// Whether the first cumulative observation is an uncharged attachment.
    pub baseline: bool,
    /// Observation time used for rolling windows and replay eligibility.
    pub observed_at: u64,
    /// Normalized model identity, immutable within the epoch.
    pub model: ModelTokenUsageKey,
    /// Allowlisted inclusive counters, never raw vendor telemetry.
    pub counters: ExternalCounters,
}

/// Durable checkpoint returned even on replay, allowing projection/reply-loss recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalUsageCommit {
    /// Digest of the server owner and disjoint epoch.
    pub stream_id: String,
    /// Durable legacy sequence or server-assigned observation ordinal.
    pub revision: u64,
    /// Absolute charged total, excluding attachment baseline.
    pub totals: ExternalCounters,
    /// Whether this invocation committed a new checkpoint.
    pub applied: bool,
}

/// Encodes bounded normalized state, never raw vendor data.
fn encode(value: ExternalCounters) -> Result<String> {
    serde_json::to_string(&value)
        .map_err(|_| MezError::invalid_state("external counter encoding failed"))
}

/// Decodes validated stored normalized state.
fn decode(value: &str) -> Result<ExternalCounters> {
    let counters: ExternalCounters = serde_json::from_str(value)
        .map_err(|_| MezError::invalid_state("stored external counters are invalid"))?;
    counters.validate()?;
    Ok(counters)
}

/// Returns the same opaque stream identity for admission and durable storage.
pub(crate) fn external_usage_stream_id(owner: &str, epoch: &str) -> Result<String> {
    Ok(Sha256::digest(
        serde_json::to_vec(&(owner, epoch))
            .map_err(|_| MezError::invalid_state("external stream encoding failed"))?,
    )
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect())
}

/// Groups immutable observations by server-frozen expense identity and category
/// coverage, never per-message IDs or caller epochs. Source-wide replay identity
/// is separate, so changing model/category/project cannot recharge one event.
pub(crate) fn external_observation_epoch(report: &ExternalUsageReport) -> Result<String> {
    let bytes = serde_json::to_vec(&(
        "mez-observation-stream/1",
        &report.harness,
        &report.model.provider,
        &report.model.model,
        report
            .project
            .as_ref()
            .map(super::AccountingProjectId::as_str),
        report.counters.reasoning_tokens.is_some(),
        report.counters.cached_input_tokens.is_some(),
        report.counters.cache_write_input_tokens.is_some(),
    ))
    .map_err(|_| MezError::invalid_state("external observation stream encoding failed"))?;
    Ok(format!(
        "observation:{}",
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ))
}

impl TokenUsageStore {
    /// Commits receipt, checkpoint and normalized delta in one writer transaction.
    /// Identical replay is inert, conflicting IDs fail, stale sequences cannot
    /// revive pruned observations, and a cumulative baseline is never charged.
    pub(crate) fn ingest_external(
        &self,
        report: &ExternalUsageReport,
        now: u64,
    ) -> Result<ExternalUsageCommit> {
        self.ingest_external_with_request(report, now, None, None, GroupPublication::Native(None))
    }

    /// Adopts only exact same-owner OpenCode legacy evidence into immutable
    /// observation replay. Native source authorization stays with the caller;
    /// this worker-only path never recovers another owner or imports history.
    /// Existing legacy checkpoints remain intact and adoption emits no expense.
    #[allow(
        dead_code,
        reason = "ordinary adapter migration consumes explicit ledger handoff"
    )]
    pub(crate) fn ingest_external_opencode_handoff(
        &self,
        report: &ExternalUsageReport,
        legacy: &ExternalUsageReport,
        now: u64,
    ) -> Result<ExternalUsageCommit> {
        Self::require_opencode_handoff_pair(report, legacy)?;
        self.ingest_external_with_request(
            report,
            now,
            None,
            Some(legacy),
            GroupPublication::Native(None),
        )
    }

    /// Commits exact alias adoption and captures bounded group evidence under
    /// the same immediate writer. Invalid group evidence rolls back adoption;
    /// success returns only after commit. Neither result grants native authority.
    #[cfg(test)]
    pub(crate) fn ingest_external_opencode_handoff_with_group(
        &self,
        report: &ExternalUsageReport,
        legacy: &ExternalUsageReport,
        now: u64,
    ) -> Result<(ExternalUsageCommit, ExternalUsageGroupSnapshot)> {
        Self::require_opencode_handoff_pair(report, legacy)?;
        let mut group = None;
        let commit = self.ingest_external_with_request(
            report,
            now,
            None,
            Some(legacy),
            GroupPublication::Native(Some(&mut group)),
        )?;
        let group = group.ok_or_else(|| {
            MezError::invalid_state("OpenCode committed group evidence unavailable")
        })?;
        Ok((commit, group))
    }

    /// Ingests an exact completed-message report from an already-authorized
    /// legacy launch. The caller must supply its server-owned owner/project,
    /// never a claimed native mapping. Existing exact legacy receipts adopt with
    /// zero expense; fresh messages charge one canonical checkpoint atomically
    /// with their alias and bounded coherent group. This cannot allocate native
    /// mappings, admit ordinary producers or enable source recovery.
    pub(crate) fn ingest_admitted_legacy_opencode_with_group(
        &self,
        report: &ExternalUsageReport,
        legacy: &ExternalUsageReport,
        now: u64,
    ) -> Result<(ExternalUsageCommit, ExternalUsageGroupSnapshot)> {
        Self::require_opencode_handoff_pair(report, legacy)?;
        let mut group = None;
        let commit = self.ingest_external_with_request(
            report,
            now,
            None,
            Some(legacy),
            GroupPublication::LegacyLaunch(&mut group),
        )?;
        let group = group.ok_or_else(|| {
            MezError::invalid_state("legacy OpenCode group publication unavailable")
        })?;
        Ok((commit, group))
    }

    /// Both handoff entry points require the identical compiled legacy proof.
    fn require_opencode_handoff_pair(
        report: &ExternalUsageReport,
        legacy: &ExternalUsageReport,
    ) -> Result<()> {
        let millis = legacy
            .event_id
            .strip_prefix("completed:")
            .and_then(|value| value.parse::<u64>().ok());
        if report.harness != "opencode"
            || report.mode != "observation"
            || legacy.harness != report.harness
            || legacy.owner != report.owner
            || legacy.mode != "delta"
            || legacy.sequence != 1
            || legacy.baseline
            || legacy.epoch != format!("opencode-message:{}", report.event_id)
            || legacy.model != report.model
            || legacy.project != report.project
            || legacy.counters != report.counters
            || legacy.observed_at != report.observed_at
            || !millis.is_some_and(|value| {
                value > 0
                    && value / 1000 == report.observed_at
                    && legacy.event_id == format!("completed:{value}")
            })
        {
            return Err(MezError::conflict(
                "OpenCode legacy handoff evidence changed",
            ));
        }
        Ok(())
    }

    /// Commits one sealed original-request observation and its completion link
    /// atomically. Stored request metadata is rechecked under writer ownership;
    /// no caller can overwrite the original origin or bill a second ID for it.
    pub(crate) fn ingest_external_request(
        &self,
        request: &super::external_requests::ExternalRequestReport,
        now: u64,
    ) -> Result<ExternalUsageCommit> {
        self.ingest_external_with_request(
            request.report(),
            now,
            Some(request),
            None,
            GroupPublication::Native(None),
        )
    }

    /// Uses the sole existing ledger transaction and observes only an actual
    /// refused expense-writer acquisition. No statements or commit are replayable.
    pub(crate) fn ingest_external_request_with_admission(
        &self,
        request: &super::external_requests::ExternalRequestReport,
        now: u64,
        admission: &mut super::ExternalUsageAdmissionWitness,
    ) -> Result<ExternalUsageCommit> {
        self.ingest_external_with_admission(
            request.report(),
            now,
            Some(request),
            None,
            GroupPublication::Native(None),
            admission,
        )
    }

    /// Shared legacy/native transaction owner; request binding grants no runtime
    /// admission by itself and cannot be detached from its immutable report.
    fn ingest_external_with_request(
        &self,
        report: &ExternalUsageReport,
        now: u64,
        request: Option<&super::external_requests::ExternalRequestReport>,
        legacy: Option<&ExternalUsageReport>,
        group: GroupPublication<'_>,
    ) -> Result<ExternalUsageCommit> {
        self.ingest_external_with_admission(
            report,
            now,
            request,
            legacy,
            group,
            &mut super::ExternalUsageAdmissionWitness::default(),
        )
    }

    /// Shared transaction implementation; an invocation-local witness cannot
    /// change legacy or curated scheduling and is never a ledger acknowledgment.
    fn ingest_external_with_admission(
        &self,
        report: &ExternalUsageReport,
        now: u64,
        request: Option<&super::external_requests::ExternalRequestReport>,
        legacy: Option<&ExternalUsageReport>,
        group: GroupPublication<'_>,
        admission: &mut super::ExternalUsageAdmissionWitness,
    ) -> Result<ExternalUsageCommit> {
        admission.clear();
        let legacy_launch = group.is_legacy_launch();
        let observation = report.mode == "observation";
        crate::integrations::harness_policy::require_active_external_harness(&report.harness)?;
        report.counters.validate()?;
        for field in [
            &report.owner,
            &report.harness,
            &report.epoch,
            &report.event_id,
            &report.model.provider,
            &report.model.model,
        ] {
            if field.is_empty() || field.len() > 128 || field.chars().any(char::is_control) {
                return Err(MezError::invalid_args(
                    "external accounting identifiers must be bounded inert text",
                ));
            }
        }
        if !matches!(report.mode.as_str(), "delta" | "cumulative" | "observation")
            || report.sequence == 0
            || (report.baseline && report.mode != "cumulative")
        {
            return Err(MezError::invalid_args(
                "invalid external accounting mode, sequence or baseline",
            ));
        }
        if observation
            && (report.event_id.len() != 64
                || !report
                    .event_id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        {
            return Err(MezError::invalid_args(
                "external observation requires a canonical opaque identity digest",
            ));
        }
        if report.observed_at > now || report.observed_at < now.saturating_sub(91 * 86_400) {
            return Err(MezError::invalid_args(
                "external usage is outside the 91-day replay horizon",
            ));
        }
        let canonical;
        let report = if observation {
            canonical = ExternalUsageReport {
                epoch: external_observation_epoch(report)?,
                ..report.clone()
            };
            &canonical
        } else {
            report
        };
        let stream_id = external_usage_stream_id(&report.owner, &report.epoch)?;
        let mut payload = serde_json::json!({"harness":report.harness,"provider":report.model.provider,
            "model":report.model.model,"sequence":report.sequence,"mode":report.mode,
            "baseline":report.baseline,"observed_at":report.observed_at,"counters":report.counters});
        // Preserve pre-v4 fingerprints for unattributed streams. A qualified
        // project is additional immutable provenance, not migration-time backfill.
        if let Some(project) = &report.project {
            payload["project"] = serde_json::json!(project.as_str());
        }
        if observation {
            // Layout/arrival sequence is not incurred-expense identity. Native
            // admission still owns owner/harness/project; stored IDs grant no rights.
            payload
                .as_object_mut()
                .ok_or_else(|| MezError::invalid_state("external observation encoding failed"))?
                .remove("sequence");
        }
        let payload = payload.to_string();
        // Handoff replay retains exact legacy milliseconds even though rolling
        // observation time is seconds. Generic replay cannot bypass this domain.
        let payload = if let Some(legacy) = legacy {
            serde_json::to_string(&("mez-opencode-legacy-handoff/1", &payload, &legacy.event_id))
                .map_err(|_| MezError::invalid_state("external handoff encoding failed"))?
        } else {
            payload
        };
        let fingerprint = Sha256::digest(payload.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let mut connection = self.open_with_external_admission(admission)?;
        #[cfg(test)]
        admission.install_commit_abort_for_tests(&connection);
        let tx = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|error| {
                // No transaction exists on this error. Later reads, writes and
                // COMMIT use their unchanged terminal/uncertain error path.
                admission.refused(super::ExternalPrecommitPhase::LedgerBeginImmediate, &error);
                MezError::from(error)
            })?;
        let request_complete = if let Some(request) = request {
            let row:Option<(String,Option<String>,u64,Option<String>)>=tx.query_row("SELECT harness,project_id,admitted_at,observation_id FROM external_usage_requests WHERE owner=?1 AND request_id=?2",
                params![report.owner,request.origin().request_id],|row|Ok((row.get(0)?,row.get(1)?,super::store::row_u64(row,2)?,row.get(3)?))).optional()?;
            let (harness, project, admitted_at, completed) = row.ok_or_else(|| {
                MezError::forbidden("external request original marker unavailable")
            })?;
            if harness != report.harness
                || project.as_deref()
                    != request
                        .origin()
                        .project
                        .as_ref()
                        .map(super::AccountingProjectId::as_str)
                || admitted_at != request.origin().admitted_at
            {
                return Err(MezError::conflict(
                    "external request original provenance changed",
                ));
            }
            if completed.as_ref().is_some_and(|id| id != &report.event_id) {
                return Err(MezError::conflict(
                    "external request already completed with another observation",
                ));
            }
            completed.is_some()
        } else {
            false
        };
        if observation {
            let harness: Option<String> = tx
                .query_row(
                    "SELECT harness FROM external_usage_sources WHERE owner=?1",
                    [&report.owner],
                    |row| row.get(0),
                )
                .optional()?;
            if harness.as_deref() != Some(report.harness.as_str())
                && !(legacy_launch && harness.is_none())
            {
                return Err(MezError::forbidden(
                    "external observation requires a mapped native source",
                ));
            }
            let replay = match &group {
                GroupPublication::CopilotTerminal(publication) => {
                    publication.replay(&tx, report)?
                }
                _ => None,
            };
            if let Some(replay) = replay {
                return match group {
                    GroupPublication::CopilotTerminal(publication) => {
                        publication.finish_replay(tx, replay)
                    }
                    _ => Err(MezError::invalid_state("Copilot replay scope unavailable")),
                };
            }
            let legacy_stream = external_usage_stream_id(
                &report.owner,
                &format!("opencode-message:{}", report.event_id),
            )?;
            let legacy_checkpoint = if report.harness == "opencode" {
                let state: Option<(u64, String, String)> = tx
                    .query_row(
                        "SELECT revision,totals,mode FROM external_usage_streams WHERE id=?1",
                        [&legacy_stream],
                        |row| Ok((super::store::row_u64(row, 0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?;
                if let Some((revision, totals, mode)) = state {
                    let legacy = legacy.ok_or_else(|| {
                        MezError::conflict("explicit OpenCode legacy handoff required")
                    })?;
                    let mut old_payload = serde_json::json!({"harness":legacy.harness,"provider":legacy.model.provider,
                        "model":legacy.model.model,"sequence":legacy.sequence,"mode":legacy.mode,
                        "baseline":legacy.baseline,"observed_at":legacy.observed_at,"counters":legacy.counters});
                    if let Some(project) = &legacy.project {
                        old_payload["project"] = serde_json::json!(project.as_str());
                    }
                    let expected = Sha256::digest(old_payload.to_string().as_bytes())
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>();
                    let receipt: Option<String> = tx.query_row("SELECT fingerprint FROM external_usage_receipts WHERE stream_id=?1 AND event_id=?2",
                        params![legacy_stream, legacy.event_id], |row| row.get(0)).optional()?;
                    let totals = decode(&totals)?;
                    if receipt.as_deref() != Some(expected.as_str())
                        || mode != "delta"
                        || revision != 1
                        || totals != legacy.counters
                    {
                        return Err(MezError::conflict(
                            "OpenCode legacy receipt payload changed or unavailable",
                        ));
                    }
                    Some((revision, totals))
                } else {
                    if legacy.is_some() && !legacy_launch {
                        return Err(MezError::conflict("OpenCode legacy checkpoint unavailable"));
                    }
                    None
                }
            } else {
                None
            };
            let previous: Option<(String,String,Option<String>)> = tx.query_row("SELECT fingerprint,stream_id,request_id FROM external_usage_observations WHERE owner=?1 AND event_id=?2",params![report.owner,report.event_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
            if let Some((old_fingerprint, old_stream, old_request)) = previous {
                if old_request.as_deref()
                    != request.map(|request| request.origin().request_id.as_str())
                {
                    return Err(MezError::conflict(
                        "external observation original request binding changed",
                    ));
                }
                if old_fingerprint != fingerprint {
                    return Err(MezError::conflict(
                        "external observation id has conflicting immutable payload",
                    ));
                }
                let (revision, totals) = tx.query_row(
                    "SELECT revision,totals FROM external_usage_streams WHERE id=?1",
                    [&old_stream],
                    |row| Ok((super::store::row_u64(row, 0)?, row.get::<_, String>(1)?)),
                )?;
                return Self::finish_external_transaction(
                    tx,
                    report,
                    ExternalUsageCommit {
                        stream_id: old_stream,
                        revision,
                        totals: decode(&totals)?,
                        applied: false,
                    },
                    group,
                );
            }
            if request_complete {
                return Err(MezError::invalid_state(
                    "external completed request lost its observation evidence",
                ));
            }
            let owner_count: i64 = tx.query_row(
                "SELECT count(*) FROM external_usage_observations WHERE owner=?1",
                [&report.owner],
                |row| row.get(0),
            )?;
            let total_count: i64 = tx.query_row(
                "SELECT count(*) FROM external_usage_observations",
                [],
                |row| row.get(0),
            )?;
            require_observation_capacity(owner_count, total_count)?;
            if let Some((revision, totals)) = legacy_checkpoint {
                tx.execute("INSERT INTO external_usage_observations(owner,event_id,fingerprint,stream_id,request_id) VALUES(?1,?2,?3,?4,NULL)",
                    params![report.owner, report.event_id, fingerprint, legacy_stream])?;
                return Self::finish_external_transaction(
                    tx,
                    report,
                    ExternalUsageCommit {
                        stream_id: legacy_stream,
                        revision,
                        totals,
                        applied: false,
                    },
                    group,
                );
            }
        }
        // Each admitted report also maintains long-lived-session retention.
        // High-water checkpoints are never pruned with their raw receipts.
        let cutoff = sqlite_i64(now.saturating_sub(91 * 86_400), "external retention cutoff")?;
        tx.execute(
            "DELETE FROM external_usage_receipts WHERE observed_at < ?1",
            [cutoff],
        )?;
        tx.execute(
            "DELETE FROM token_usage_events WHERE event_source='external' AND observed_at < ?1",
            [cutoff],
        )?;
        let state = tx.query_row("SELECT harness, provider, model, mode, revision, sample, totals, project_id FROM external_usage_streams WHERE id=?1",
            [&stream_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                row.get::<_, String>(3)?, super::store::row_u64(row, 4)?, row.get::<_, String>(5)?, row.get::<_, String>(6)?, row.get::<_, Option<String>>(7)?))).optional()?;
        let receipt = tx.query_row("SELECT fingerprint FROM external_usage_receipts WHERE stream_id=?1 AND event_id=?2",
            params![stream_id, report.event_id], |row| row.get::<_, String>(0)).optional()?;
        if let Some(receipt) = receipt {
            if receipt != fingerprint {
                return Err(MezError::conflict(
                    "external usage event id has conflicting payload",
                ));
            }
            let (_, _, _, _, revision, _, totals, _) = state
                .ok_or_else(|| MezError::invalid_state("external receipt lost its checkpoint"))?;
            return Ok(ExternalUsageCommit {
                stream_id,
                revision,
                totals: decode(&totals)?,
                applied: false,
            });
        }
        let (old_revision, old_sample, old_totals) = match state {
            Some((harness, provider, model, mode, revision, sample, totals, project)) => {
                if harness != report.harness
                    || project.as_deref()
                        != report
                            .project
                            .as_ref()
                            .map(super::AccountingProjectId::as_str)
                    || provider != report.model.provider
                    || model != report.model.model
                    || mode != report.mode
                {
                    return Err(MezError::conflict(
                        "external stream identity or mode changed; start a new epoch",
                    ));
                }
                if report.baseline {
                    return Err(MezError::conflict(
                        "external stream baseline already exists",
                    ));
                }
                if !observation && report.sequence <= revision {
                    if report.mode == "delta" {
                        return Err(MezError::conflict(
                            "external delta sequence is stale or outside replay horizon",
                        ));
                    }
                    return Ok(ExternalUsageCommit {
                        stream_id,
                        revision,
                        totals: decode(&totals)?,
                        applied: false,
                    });
                }
                (revision, decode(&sample)?, decode(&totals)?)
            }
            None => {
                if report.mode == "cumulative" && !report.baseline {
                    return Err(MezError::conflict(
                        "external cumulative attachment requires an explicit uncharged baseline",
                    ));
                }
                (0, report.counters.zero_like(), report.counters.zero_like())
            }
        };
        let delta = if report.baseline {
            report.counters.zero_like()
        } else if report.mode == "cumulative" {
            report.counters.difference(old_sample)?
        } else {
            if !observation && report.sequence != old_revision.saturating_add(1) {
                return Err(MezError::conflict(
                    "external delta sequence has a gap; submit missing observations first",
                ));
            }
            report.counters
        };
        if matches!(report.mode.as_str(), "delta" | "observation") && old_revision > 0 {
            report.counters.difference(old_sample.zero_like())?;
        }
        delta.validate()?;
        let totals = old_totals.add(delta)?;
        let revision = if observation {
            old_revision
                .checked_add(1)
                .ok_or_else(|| MezError::invalid_state("external observation revision exhausted"))?
        } else {
            report.sequence
        };
        tx.execute("INSERT INTO external_usage_streams(id,harness,provider,model,mode,revision,sample,totals,project_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
            ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,sample=excluded.sample,totals=excluded.totals",
            params![stream_id, report.harness, report.model.provider, report.model.model, report.mode,
                sqlite_i64(revision, "external sequence")?, encode(report.counters)?, encode(totals)?, report.project.as_ref().map(super::AccountingProjectId::as_str)])?;
        tx.execute("INSERT INTO external_usage_receipts(stream_id,event_id,fingerprint,observed_at) VALUES(?1,?2,?3,?4)",
            params![stream_id, report.event_id, fingerprint, sqlite_i64(report.observed_at, "external timestamp")?])?;
        if observation {
            tx.execute("INSERT INTO external_usage_observations(owner,event_id,fingerprint,stream_id,request_id) VALUES(?1,?2,?3,?4,?5)",params![report.owner,report.event_id,fingerprint,stream_id,request.map(|request|request.origin().request_id.as_str())])?;
            if let Some(request) = request {
                tx.execute("UPDATE external_usage_requests SET observation_id=?1 WHERE owner=?2 AND request_id=?3 AND observation_id IS NULL",params![report.event_id,report.owner,request.origin().request_id])?;
            }
        }
        let normalized = delta.normalized();
        if !normalized.is_zero() {
            let id = format!("external:{stream_id}:{revision}");
            tx.execute("INSERT INTO token_usage_events(id,observed_at,provider,model,input_tokens,output_tokens,reasoning_tokens,cached_input_tokens,cache_write_input_tokens,harness,reasoning_known,event_source,project_id)
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'external',?12)", params![id, sqlite_i64(report.observed_at, "external timestamp")?,
                    report.model.provider, report.model.model, sqlite_i64(normalized.input_tokens,"input")?,
                    sqlite_i64(normalized.output_tokens,"output")?, sqlite_i64(normalized.reasoning_tokens,"reasoning")?,
                    normalized.cached_input_tokens.map(|n| sqlite_i64(n,"cache")).transpose()?,
                    normalized.cache_write_input_tokens.map(|n| sqlite_i64(n,"cache write")).transpose()?, report.harness,
                    i64::from(delta.reasoning_tokens.is_some()), report.project.as_ref().map(super::AccountingProjectId::as_str)])?;
        }
        Self::finish_external_transaction(
            tx,
            report,
            ExternalUsageCommit {
                stream_id,
                revision,
                totals,
                applied: true,
            },
            group,
        )
    }

    /// Captures a coherent group before publication and verifies its exact
    /// checkpoint member. A failed read/check/commit publishes neither result;
    /// transaction drop retains the existing all-effects rollback boundary.
    fn finish_external_transaction(
        tx: rusqlite::Transaction<'_>,
        report: &ExternalUsageReport,
        commit: ExternalUsageCommit,
        group: GroupPublication<'_>,
    ) -> Result<ExternalUsageCommit> {
        let (output, legacy_launch) = match group {
            GroupPublication::Native(output) => (output, false),
            GroupPublication::LegacyLaunch(output) => (Some(output), true),
            GroupPublication::CopilotTerminal(publication) => {
                return publication.finish_new(tx, report, commit);
            }
        };
        if let Some(output) = output {
            let snapshot = if legacy_launch {
                group_snapshot::snapshot_for_legacy_launch(&tx, report)?
            } else {
                group_snapshot::snapshot_in_transaction(&tx, report)?
            }
            .ok_or_else(|| {
                MezError::invalid_state("OpenCode handoff group evidence unavailable")
            })?;
            if !snapshot.members().iter().any(|member| {
                member.stream_id == commit.stream_id
                    && member.revision == commit.revision
                    && member.totals == commit.totals
            }) {
                return Err(MezError::conflict(
                    "OpenCode handoff checkpoint and group evidence changed",
                ));
            }
            *output = Some(snapshot);
        }
        tx.commit()?;
        Ok(commit)
    }
}
