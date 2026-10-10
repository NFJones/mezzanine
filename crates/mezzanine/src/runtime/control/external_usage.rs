//! Exact-owned external telemetry admission and durable-checkpoint projection.
//!
//! The actor freezes registration provenance before storage leaves its ownership.
//! Only normalized content-free reports cross to the bounded worker. Completion
//! projects an absolute durable checkpoint, not a fresh delta: duplicate or
//! reordered replies cannot charge twice. Registration retirement does not erase
//! incurred usage. Pane views require the original root; session totals survive
//! close. Reset moves only a view baseline, never the durable stream checkpoint.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

use crate::control::{AuthenticatedPeer, ControlConnectionState, JsonRpcRequest};
use crate::error::{MezError, Result};
use crate::runtime::processes::RuntimePaneProcessIdentity;
use crate::runtime::{RuntimeSessionService, current_unix_seconds};
use crate::storage::token_usage::{
    ExternalCounters, ExternalUsageCommit, ExternalUsageReport, TokenUsageStore,
};
use mez_agent::{ModelTokenUsage, ModelTokenUsageKey};

const MAX_PROJECTED_STREAMS: usize = 4096;
const MAX_IN_FLIGHT: usize = 32;

/// Bounded checkpoint projection, independent of native request/context samples.
#[derive(Debug, Default)]
pub(super) struct ExternalUsageProjection {
    pending: BTreeSet<u64>,
    reservations: BTreeMap<String, usize>,
    next_admission: u64,
    streams: BTreeMap<String, ProjectedStream>,
    /// Test-only small boundary for exercising production admission logic.
    #[cfg(test)]
    stream_limit: Option<usize>,
    /// Test-owned barrier proving storage work does not hold the actor.
    #[cfg(test)]
    worker_gate: Option<(
        std::sync::Arc<tokio::sync::Notify>,
        std::sync::Arc<tokio::sync::Notify>,
    )>,
}

/// One absolute durable checkpoint and its pane-view reset baseline.
#[derive(Debug)]
struct ProjectedStream {
    pane_id: String,
    process: RuntimePaneProcessIdentity,
    project: Option<crate::storage::token_usage::AccountingProjectId>,
    harness: String,
    model: ModelTokenUsageKey,
    revision: u64,
    totals: ExternalCounters,
    reset_baseline: ExternalCounters,
}

/// Immutable admitted storage work, containing no launch credential or raw payload.
#[derive(Debug, Clone)]
pub(crate) struct ExternalUsageWork {
    /// Exact actor admission, consumed once independently of report replay.
    admission: u64,
    /// JSON-RPC reply identity, not command authority.
    pub request_id: String,
    /// Private durable store captured at admission.
    pub store: TokenUsageStore,
    /// Normalized accounting provenance and counters frozen by the actor.
    pub report: ExternalUsageReport,
    /// Exact originating pane, independent of later client focus.
    pane_id: String,
    /// Root incarnation required for pane-view attribution.
    process: RuntimePaneProcessIdentity,
    /// Acceptance clock used for retention and timestamp validation.
    pub now: u64,
    /// Original ready marker for native reports; absent for legacy streams.
    request_marker: Option<String>,
    /// Only actual native storage work can publish a durable completion.
    published: Arc<OnceLock<ExternalUsageCommit>>,
    /// Test-only worker start/release barrier, captured with immutable work.
    #[cfg(test)]
    pub worker_gate: Option<(
        std::sync::Arc<tokio::sync::Notify>,
        std::sync::Arc<tokio::sync::Notify>,
    )>,
}

impl ExternalUsageWork {
    /// Runs legacy or sealed native request accounting off actor. Native work
    /// cannot replace its originally reserved project/group with current state.
    pub(crate) fn execute(&self) -> Result<ExternalUsageCommit> {
        if let Some(marker) = &self.request_marker {
            let frozen = self
                .store
                .freeze_external_request_report(&self.report, marker)?;
            if frozen.report().project != self.report.project
                || frozen.report().epoch != self.report.epoch
            {
                return Err(MezError::conflict(
                    "external request original projection changed",
                ));
            }
            let commit = self.store.ingest_external_request(&frozen, self.now)?;
            self.published
                .set(commit.clone())
                .map_err(|_| MezError::conflict("external request result already published"))?;
            Ok(commit)
        } else {
            self.store.ingest_external(&self.report, self.now)
        }
    }
}

impl RuntimeSessionService {
    /// Seeds historical binding provenance for forward-retirement tests only.
    #[cfg(test)]
    pub(crate) fn set_external_binding_harness_for_tests(&mut self, harness: &str) {
        for binding in self.control.external_agents_mut().bindings.values_mut() {
            binding.harness = harness.to_string();
        }
    }

    /// Reports worker and stream reservations without exposing credentials.
    #[cfg(test)]
    pub(crate) fn external_usage_reservation_counts_for_tests(&self) -> (usize, usize) {
        let usage = &self.control.external_agents().usage;
        (usage.pending.len(), usage.reservations.len())
    }

    /// Sets a bounded projection limit for deterministic capacity regressions.
    #[cfg(test)]
    pub(crate) fn set_external_usage_stream_limit_for_tests(&mut self, limit: usize) {
        self.control.external_agents_mut().usage.stream_limit = Some(limit);
    }

    /// Installs a deterministic off-actor storage barrier for this test service.
    #[cfg(test)]
    pub(crate) fn gate_external_usage_worker_for_tests(
        &mut self,
        started: std::sync::Arc<tokio::sync::Notify>,
        release: std::sync::Arc<tokio::sync::Notify>,
    ) {
        self.control.external_agents_mut().usage.worker_gate = Some((started, release));
    }

    /// Authorizes a report under a current capability-only registration and
    /// reserves bounded worker/projection capacity before any database write.
    pub(crate) fn prepare_external_usage(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
    ) -> Result<ExternalUsageWork> {
        self.require_live()?;
        if request.method != "agent/external/usage" || connection.initialized() {
            return Err(MezError::forbidden(
                "external usage requires capability-only ingress",
            ));
        }
        let Some(AuthenticatedPeer::UnixUser { uid }) = connection.authenticated_peer() else {
            return Err(MezError::forbidden(
                "external usage requires authenticated Unix transport",
            ));
        };
        crate::control::validate_control_method_params_schema(request)?;
        let params: serde_json::Value =
            serde_json::from_str(request.params.as_deref().unwrap_or("{}"))
                .map_err(|_| MezError::invalid_args("external usage params must be an object"))?;
        let digest = super::external_agents::credential(&params)?;
        self.reconcile_external_agent_registrations();
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .filter(|binding| {
                !binding.retired
                    && binding.uid == *uid
                    && params.get("generation").and_then(serde_json::Value::as_u64)
                        == Some(binding.generation)
            })
            .ok_or_else(|| MezError::forbidden("external launch capability unavailable"))?;
        crate::integrations::harness_policy::require_active_external_harness(&binding.harness)?;
        let session = super::external_agents::text(&params, "external_session_id", 128)?;
        if binding
            .registration
            .as_ref()
            .is_none_or(|registration| registration.external_session_id != session)
        {
            return Err(MezError::forbidden(
                "external usage registration unavailable",
            ));
        }
        if let Some(enrollment) = &binding.enrollment {
            enrollment.authorize(connection)?;
            return Err(MezError::new(
                crate::error::MezErrorKind::NotImplemented,
                "ordinary external usage source continuity unavailable",
            ));
        }
        if params.get("mode").and_then(serde_json::Value::as_str) == Some("observation") {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotImplemented,
                "immutable external observation reporting is not enabled",
            ));
        }
        let sequence = params
            .get("sequence")
            .and_then(serde_json::Value::as_u64)
            .filter(|n| *n > 0)
            .ok_or_else(|| MezError::invalid_args("external usage sequence must be positive"))?;
        let observed_at = params
            .get("observed_at")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| MezError::invalid_args("external usage requires observed_at"))?;
        let counters: ExternalCounters = serde_json::from_value(
            params
                .get("counters")
                .cloned()
                .ok_or_else(|| MezError::invalid_args("external usage requires counters"))?,
        )
        .map_err(|_| MezError::invalid_args("external usage counters are invalid"))?;
        counters.validate()?;
        let baseline = match params.get("baseline") {
            None => false,
            Some(serde_json::Value::Bool(value)) => *value,
            _ => {
                return Err(MezError::invalid_args(
                    "external usage baseline must be boolean",
                ));
            }
        };
        let report = ExternalUsageReport {
            owner: binding.accounting_owner.clone(),
            project: binding.accounting_origin.project_id().cloned(),
            harness: binding.harness.clone(),
            epoch: super::external_agents::text(&params, "epoch", 128)?,
            event_id: super::external_agents::text(&params, "event_id", 128)?,
            sequence,
            mode: super::external_agents::text(&params, "mode", 16)?,
            baseline,
            observed_at,
            model: ModelTokenUsageKey::new(
                super::external_agents::text(&params, "provider", 128)?,
                super::external_agents::text(&params, "model", 128)?,
            ),
            counters,
        };
        let work = ExternalUsageWork {
            admission: 0,
            request_id: request.id.clone(),
            store: self.persistence.cloned_token_usage_store().ok_or_else(|| {
                MezError::invalid_state("durable external accounting unavailable")
            })?,
            report,
            pane_id: binding.pane_id.clone(),
            process: binding.process.clone(),
            now: current_unix_seconds(),
            request_marker: None,
            published: Arc::new(OnceLock::new()),
            #[cfg(test)]
            worker_gate: self.control.external_agents().usage.worker_gate.clone(),
        };
        self.reserve_external_usage_work(work)
    }

    /// Admits a finalized observation under a ready original native request.
    /// Caller epochs/projects/PIDs and legacy modes cannot select attribution;
    /// missing readiness requires retry of the same begin, not a newest lookup.
    pub(crate) fn prepare_external_usage_end(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
    ) -> Result<ExternalUsageWork> {
        self.require_live()?;
        if request.method != "agent/external/usage-end" || connection.initialized() {
            return Err(MezError::forbidden(
                "external request end requires capability-only ingress",
            ));
        }
        let Some(AuthenticatedPeer::UnixUser { uid }) = connection.authenticated_peer() else {
            return Err(MezError::forbidden(
                "external request end requires Unix transport",
            ));
        };
        crate::control::validate_control_method_params_schema(request)?;
        let params = crate::protocol::strict_json::decode(
            request.params.as_deref().unwrap_or("{}").as_bytes(),
        )?;
        let digest = super::external_agents::credential(&params)?;
        self.reconcile_external_agent_registrations();
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .filter(|binding| {
                !binding.retired
                    && binding.uid == *uid
                    && params["generation"].as_u64() == Some(binding.generation)
            })
            .ok_or_else(|| MezError::forbidden("external request producer unavailable"))?;
        crate::integrations::harness_policy::require_active_external_harness(&binding.harness)?;
        let session = super::external_agents::text(&params, "external_session_id", 128)?;
        if binding
            .registration
            .as_ref()
            .is_none_or(|registration| registration.external_session_id != session)
        {
            return Err(MezError::forbidden("external request session unavailable"));
        }
        let enrollment = binding
            .enrollment
            .as_ref()
            .filter(|enrollment| enrollment.usage_owner_is_durable)
            .ok_or_else(|| {
                MezError::new(
                    crate::error::MezErrorKind::NotImplemented,
                    "external request native namespace unavailable",
                )
            })?;
        enrollment.authorize(connection)?;
        let marker = super::external_agents::text(&params, "request_id", 64)?;
        let origin = self
            .control
            .external_agents()
            .usage_begins
            .origins
            .get(&(binding.accounting_owner.clone(), marker.clone()))
            .ok_or_else(|| {
                MezError::conflict(
                    "external request original readiness unavailable; retry its original begin",
                )
            })?;
        let now = current_unix_seconds();
        let observed_at=params["observed_at"].as_u64().filter(|time|*time>=origin.admitted_at && *time<=now && *time>=now.saturating_sub(91*86_400))
            .ok_or_else(||MezError::invalid_args("external observation requires its original request boundary and replay horizon"))?;
        let counters: ExternalCounters = serde_json::from_value(
            params
                .get("counters")
                .cloned()
                .ok_or_else(|| MezError::invalid_args("external request counters unavailable"))?,
        )
        .map_err(|_| MezError::invalid_args("external request counters invalid"))?;
        counters.validate()?;
        let mut report = ExternalUsageReport {
            owner: binding.accounting_owner.clone(),
            project: origin.project.clone(),
            harness: binding.harness.clone(),
            epoch: "request-observation".into(),
            event_id: super::external_agents::text(&params, "event_id", 64)?,
            sequence: 1,
            mode: "observation".into(),
            baseline: false,
            observed_at,
            model: ModelTokenUsageKey::new(
                super::external_agents::text(&params, "provider", 128)?,
                super::external_agents::text(&params, "model", 128)?,
            ),
            counters,
        };
        if report.event_id.len() != 64
            || !report
                .event_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(MezError::invalid_args(
                "external observation requires an opaque canonical identity",
            ));
        }
        report.epoch = crate::storage::token_usage::external_observation_epoch(&report)?;
        let work = ExternalUsageWork {
            admission: 0,
            request_id: request.id.clone(),
            store: self.persistence.cloned_token_usage_store().ok_or_else(|| {
                MezError::invalid_state("durable external accounting unavailable")
            })?,
            report,
            pane_id: binding.pane_id.clone(),
            process: binding.process.clone(),
            now,
            request_marker: Some(marker),
            published: Arc::new(OnceLock::new()),
            #[cfg(test)]
            worker_gate: self.control.external_agents().usage.worker_gate.clone(),
        };
        self.reserve_external_usage_work(work)
    }

    /// Shares exact stream reservations between legacy and native request work.
    /// New groups fail explicitly at capacity; no paid observation is evicted.
    fn reserve_external_usage_work(
        &mut self,
        mut work: ExternalUsageWork,
    ) -> Result<ExternalUsageWork> {
        let stream_id = crate::storage::token_usage::external_usage_stream_id(
            &work.report.owner,
            &work.report.epoch,
        )?;
        let projection = &mut self.control.external_agents_mut().usage;
        #[cfg(test)]
        let stream_limit = projection.stream_limit.unwrap_or(MAX_PROJECTED_STREAMS);
        #[cfg(not(test))]
        let stream_limit = MAX_PROJECTED_STREAMS;
        let existing = projection.streams.contains_key(&stream_id)
            || projection.reservations.contains_key(&stream_id);
        let reserved_new = projection
            .reservations
            .keys()
            .filter(|id| !projection.streams.contains_key(*id))
            .count();
        if projection.pending.len() >= MAX_IN_FLIGHT
            || (!existing && projection.streams.len() + reserved_new >= stream_limit)
        {
            return Err(MezError::new(
                crate::error::MezErrorKind::RateLimited,
                "external accounting capacity exhausted",
            ));
        }
        projection.next_admission = projection
            .next_admission
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("external accounting admission exhausted"))?;
        work.admission = projection.next_admission;
        projection.pending.insert(work.admission);
        *projection.reservations.entry(stream_id).or_default() += 1;
        Ok(work)
    }

    /// Projects one committed absolute checkpoint even after registration expiry.
    /// Completion never revives a registration or schedules a provider action.
    pub(crate) fn complete_external_usage(
        &mut self,
        work: ExternalUsageWork,
        result: Result<ExternalUsageCommit>,
    ) -> String {
        let projection = &mut self.control.external_agents_mut().usage;
        if !projection.pending.remove(&work.admission) {
            return crate::runtime::runtime_json_rpc_error(
                &work.request_id,
                crate::error::MezErrorKind::Conflict,
                "external accounting admission already settled",
            );
        }
        if let Ok(id) = crate::storage::token_usage::external_usage_stream_id(
            &work.report.owner,
            &work.report.epoch,
        ) && let Some(count) = projection.reservations.get_mut(&id)
        {
            *count -= 1;
            if *count == 0 {
                projection.reservations.remove(&id);
            }
        }
        let result = if work.request_marker.is_some() {
            result.and_then(|commit| {
                if work.published.get() == Some(&commit) {
                    Ok(commit)
                } else {
                    Err(MezError::conflict(
                        "external request durability result unpublished or mismatched",
                    ))
                }
            })
        } else {
            result
        };
        if let Some(marker) = &work.request_marker {
            self.control
                .external_agents_mut()
                .usage_begins
                .origins
                .remove(&(work.report.owner.clone(), marker.clone()));
        }
        if work.request_marker.is_some() && result.is_ok() {
            for binding in self.control.external_agents_mut().bindings.values_mut() {
                if !binding.retired
                    && binding.accounting_owner == work.report.owner
                    && binding.harness == work.report.harness
                    && binding.process.same_incarnation(&work.process)
                    && let Some(enrollment) = &mut binding.enrollment
                {
                    enrollment.request_reports_seen = true;
                }
            }
        }
        let projection = &mut self.control.external_agents_mut().usage;
        match result {
            Ok(commit) => {
                let stream = projection
                    .streams
                    .entry(commit.stream_id.clone())
                    .or_insert_with(|| ProjectedStream {
                        pane_id: work.pane_id,
                        process: work.process,
                        project: work.report.project,
                        harness: work.report.harness,
                        model: work.report.model,
                        revision: 0,
                        totals: commit.totals.zero_like(),
                        reset_baseline: commit.totals.zero_like(),
                    });
                if commit.revision > stream.revision {
                    stream.revision = commit.revision;
                    stream.totals = commit.totals;
                }
                serde_json::json!({"jsonrpc":"2.0","id":serde_json::from_str::<serde_json::Value>(&work.request_id).unwrap_or_default(),
                    "result":{"accepted":true,"durable":true,"applied":commit.applied,"revision":commit.revision,
                        "coverage":{"reasoning_known":commit.totals.reasoning_tokens.is_some(),"source":"external-reported"}}}).to_string()
            }
            Err(error) => crate::runtime::runtime_json_rpc_error(
                &work.request_id,
                error.kind(),
                error.message(),
            ),
        }
    }

    /// Returns harness-separated external pane or instance totals without I/O.
    /// Pane views exclude replaced roots and subtract only their reset baseline.
    pub(crate) fn external_token_usage(
        &self,
        pane_id: Option<&str>,
    ) -> BTreeMap<(String, ModelTokenUsageKey), ModelTokenUsage> {
        let mut totals = BTreeMap::<(String, ModelTokenUsageKey), ModelTokenUsage>::new();
        for stream in self.control.external_agents().usage.streams.values() {
            let usage = if let Some(pane) = pane_id {
                if stream.pane_id != pane
                    || !self.pane_process_identity_is_current(pane, &stream.process)
                {
                    continue;
                }
                let Ok(delta) = stream.totals.difference(stream.reset_baseline) else {
                    continue;
                };
                delta.normalized()
            } else {
                stream.totals.normalized()
            };
            totals
                .entry((stream.harness.clone(), stream.model.clone()))
                .or_default()
                .add_assign(usage);
        }
        totals
    }

    /// Moves only the pane-view baseline; durable deduplication remains intact.
    /// Projects exact external partitions and coverage without querying storage.
    pub(crate) fn external_usage_partitions(
        &self,
        pane_id: Option<&str>,
    ) -> Vec<(
        crate::storage::token_usage::TokenHistoryKey,
        crate::storage::token_usage::TokenHistoryUsage,
    )> {
        self.control
            .external_agents()
            .usage
            .streams
            .values()
            .filter_map(|stream| {
                let counters = if let Some(pane) = pane_id {
                    if stream.pane_id != pane
                        || !self.pane_process_identity_is_current(pane, &stream.process)
                    {
                        return None;
                    }
                    stream.totals.difference(stream.reset_baseline).ok()?
                } else {
                    stream.totals
                };
                Some((
                    crate::storage::token_usage::TokenHistoryKey {
                        project: stream.project.clone(),
                        harness: stream.harness.clone(),
                        model: stream.model.clone(),
                    },
                    crate::storage::token_usage::TokenHistoryUsage {
                        usage: counters.normalized(),
                        reasoning_known: counters.reasoning_tokens.is_some(),
                    },
                ))
            })
            .collect()
    }

    /// Moves only the pane-view baseline; durable deduplication remains intact.
    pub(crate) fn reset_external_token_usage(&mut self, pane_id: &str) -> bool {
        let mut changed = false;
        for stream in self
            .control
            .external_agents_mut()
            .usage
            .streams
            .values_mut()
        {
            if stream.pane_id == pane_id {
                changed |= stream.reset_baseline != stream.totals;
                stream.reset_baseline = stream.totals;
            }
        }
        changed
    }
}
