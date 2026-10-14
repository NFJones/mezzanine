//! Exact-owned external telemetry admission and durable-checkpoint projection.
//!
//! The actor freezes registration provenance before storage leaves its ownership.
//! Only normalized content-free reports cross to the bounded worker. Completion
//! projects an absolute durable checkpoint, not a fresh delta: duplicate or
//! reordered replies cannot charge twice. Registration retirement does not erase
//! incurred usage. Pane views require the original root; session totals survive
//! close. Reset moves only a view baseline, never the durable stream checkpoint.
//! This single projection/reservation owner belongs to the control service, not
//! disposable registrations. Registration loss preserves pending work and resets;
//! exact incarnation checks fence completion/release after full owner replacement
//! without undoing real expense already committed by its original storage worker.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

use crate::control::{AuthenticatedPeer, ControlConnectionState, JsonRpcRequest};
use crate::error::{MezError, Result};
use crate::runtime::processes::RuntimePaneProcessIdentity;
use crate::runtime::{RuntimeSessionService, current_unix_seconds};
use crate::storage::token_usage::{
    ExternalCounters, ExternalUsageCommit, ExternalUsageGroupSnapshot, ExternalUsageReport,
    TokenUsageStore,
};
use mez_agent::{ModelTokenUsage, ModelTokenUsageKey};

const MAX_PROJECTED_STREAMS: usize = 4096;
const MAX_IN_FLIGHT: usize = 32;

/// Bounded checkpoint projection, independent of native request/context samples.
#[derive(Debug, Default)]
pub(super) struct ExternalUsageProjection {
    pending: BTreeSet<u64>,
    /// Exact control-owner incarnation; full service replacement cannot reuse IDs.
    incarnation: Arc<()>,
    /// Exact actor-held capacity reservations, including canonical group keys.
    pub(super) reservations: BTreeMap<String, usize>,
    next_admission: u64,
    /// Current absolute projections, never provider-reported completion deltas.
    pub(super) streams: BTreeMap<String, ProjectedStream>,
    /// Bounded routing evidence for members already consolidated into groups.
    pub(super) member_groups: BTreeMap<String, String>,
    /// Test-only small boundary for exercising production admission logic.
    #[cfg(test)]
    stream_limit: Option<usize>,
    /// Test-owned barrier proving storage work does not hold the actor.
    #[cfg(test)]
    worker_gate: Option<(
        std::sync::Arc<tokio::sync::Notify>,
        std::sync::Arc<tokio::sync::Notify>,
    )>,
    /// Test-owned first-failure checkpoint; it never supplies a worker outcome.
    #[cfg(test)]
    worker_probe: Option<Arc<ExternalUsageWorkerProbe>>,
}

/// Observes actual off-actor attempts without changing their storage results.
/// The first real failure can be held while a fixture releases its own writer.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct ExternalUsageWorkerProbe {
    /// Counts actual worker dispatches, not producer callback invocations.
    pub(crate) attempts: std::sync::atomic::AtomicUsize,
    /// Signals only after a real failed invocation, before actor completion.
    pub(crate) failed: tokio::sync::Notify,
    /// Fixture-owned release; no production cancellation or retry authority.
    pub(crate) release: tokio::sync::Notify,
    /// Records the broad original error kind without retaining error content.
    pub(crate) failure: OnceLock<crate::error::MezErrorKind>,
    /// Counts final actor completion entries, independent of a live RPC waiter.
    pub(crate) completions: std::sync::atomic::AtomicUsize,
    /// Wakes a waiter-loss fixture after the original actor receives completion.
    pub(crate) completed: tokio::sync::Notify,
}

/// One absolute durable checkpoint and its pane-view reset baseline.
#[derive(Debug)]
pub(super) struct ProjectedStream {
    /// Original pane, independent of later focus or account attachment.
    pub(super) pane_id: String,
    /// Exact original local root incarnation.
    pub(super) process: RuntimePaneProcessIdentity,
    /// Immutable expense attribution.
    pub(super) project: Option<crate::storage::token_usage::AccountingProjectId>,
    /// Immutable reporting harness.
    pub(super) harness: String,
    /// Immutable response provider/model group.
    pub(super) model: ModelTokenUsageKey,
    /// Highest absolute checkpoint or derived group revision.
    pub(super) revision: u64,
    /// Absolute durable expense evidence, not a newly charged delta.
    pub(super) totals: ExternalCounters,
    /// Actor-owned pane reset baseline.
    pub(super) reset_baseline: ExternalCounters,
    /// Unique member high-water evidence for a consolidated projection.
    pub(super) members: Option<BTreeMap<String, ExternalUsageCommit>>,
}

/// One worker publication; group evidence and commit become visible together.
/// A snapshot is not a replacement commit or permission to mutate actor caches.
#[derive(Debug, Clone)]
struct ExternalUsagePublication {
    commit: ExternalUsageCommit,
    group: Option<ExternalUsageGroupSnapshot>,
}

/// Immutable admitted storage work, containing no launch credential or raw payload.
#[derive(Debug, Clone)]
pub(crate) struct ExternalUsageWork {
    /// Exact actor admission, consumed once independently of report replay.
    admission: u64,
    /// Original accounting owner, not a source/transport capability.
    accounting_incarnation: Arc<()>,
    /// JSON-RPC reply identity, not command authority.
    pub request_id: String,
    /// Private durable store captured at admission.
    pub store: TokenUsageStore,
    /// Normalized accounting provenance and counters frozen by the actor.
    pub report: ExternalUsageReport,
    /// Private acceptance copy fences execution, settlement and reservation release.
    admitted_report: ExternalUsageReport,
    /// Exact originating pane, independent of later client focus.
    pub(super) pane_id: String,
    /// Root incarnation required for pane-view attribution.
    pub(super) process: RuntimePaneProcessIdentity,
    /// Exact reserved key, captured before admission and never caller-selected.
    projection_id: Option<String>,
    /// Acceptance clock used for retention and timestamp validation.
    pub now: u64,
    /// Original ready marker for native reports; absent for legacy streams.
    request_marker: Option<String>,
    /// Immutable ready-origin proof, heap-shared across the original work clones.
    /// This conserves marker/project/admission time, not source authority.
    request_origin: Option<Arc<crate::storage::token_usage::ExternalRequestOrigin>>,
    /// Enabled only after native socket authorization and successful reservation.
    /// Curated pre-proof and legacy work remain one-shot; no payload selects it.
    socket_precommit_retry: bool,
    /// Exact normalized proof attached only to already-reserved legacy work.
    /// The original report/stream reservation remains unchanged; no new source
    /// or request-marker authority is inferred from this migration candidate.
    opencode_handoff: Option<crate::integrations::bootstrap::opencode::CompletedObservation>,
    /// Only actual native storage work can publish a durable completion.
    published: Arc<OnceLock<ExternalUsagePublication>>,
    /// Test-only worker start/release barrier, captured with immutable work.
    #[cfg(test)]
    pub worker_gate: Option<(
        std::sync::Arc<tokio::sync::Notify>,
        std::sync::Arc<tokio::sync::Notify>,
    )>,
    /// Heap-held test observation only; never a production retry selector.
    #[cfg(test)]
    pub(crate) worker_probe: Option<Arc<ExternalUsageWorkerProbe>>,
}

impl ExternalUsageWork {
    /// Attaches one exact same-report receipt-only migration to admitted legacy
    /// work. Failure leaves the original work/reservation available to settle or
    /// release. This never changes its source, origin, pane, clock or stream key.
    #[allow(
        dead_code,
        reason = "ordinary adapter migration uses exact admitted work"
    )]
    pub(crate) fn set_opencode_handoff(
        &mut self,
        pair: crate::integrations::bootstrap::opencode::CompletedObservation,
    ) -> Result<()> {
        if self.admission == 0
            || self.request_marker.is_some()
            || self.opencode_handoff.is_some()
            || pair.legacy() != &self.report
        {
            return Err(MezError::conflict(
                "OpenCode handoff does not match admitted legacy work",
            ));
        }
        self.opencode_handoff = Some(pair);
        Ok(())
    }

    /// Runs legacy or sealed native request accounting off actor. Native work
    /// cannot replace its originally reserved project/group with current state.
    pub(crate) fn execute(&self) -> Result<ExternalUsageCommit> {
        self.execute_with_admission(None)
    }

    /// Executes one admitted socket attempt with a fresh invocation-local
    /// refusal witness. No broad error, previous attempt or stored row may
    /// supply retry authority; all other work preserves one-shot execution.
    pub(crate) fn execute_attempt(&self) -> crate::storage::token_usage::ExternalUsageAttempt {
        use crate::storage::token_usage::{ExternalUsageAdmissionWitness, ExternalUsageAttempt};
        if !self.socket_precommit_retry {
            return ExternalUsageAttempt::Completed(self.execute());
        }
        let mut admission = ExternalUsageAdmissionWitness::default();
        let result = self.execute_with_admission(Some(&mut admission));
        admission.finish(result)
    }

    /// Exposes only sealed scheduling eligibility, never a setter or RPC knob.
    pub(crate) fn permits_precommit_retry(&self) -> bool {
        self.socket_precommit_retry
    }

    /// Shares original normalization, origin checks and sole ledger publication.
    /// The optional observer cannot authorize work or retry after writer entry.
    fn execute_with_admission(
        &self,
        mut admission: Option<&mut crate::storage::token_usage::ExternalUsageAdmissionWitness>,
    ) -> Result<ExternalUsageCommit> {
        if self.report != self.admitted_report {
            return Err(MezError::conflict("external admitted report changed"));
        }
        if self.socket_precommit_retry {
            self.request_origin
                .as_ref()
                .filter(|origin| {
                    Some(origin.request_id.as_str()) == self.request_marker.as_deref()
                        && origin.project == self.report.project
                        && origin.admitted_at <= self.report.observed_at
                })
                .ok_or_else(|| MezError::conflict("external request original origin changed"))?;
            if self.report.mode != "observation"
                || self.opencode_handoff.is_some()
                || self.admission == 0
            {
                return Err(MezError::conflict(
                    "external socket request admission changed",
                ));
            }
            if let Some(publication) = self.published.get() {
                if publication.group.is_some()
                    || self.projection_id.as_deref() != Some(publication.commit.stream_id.as_str())
                {
                    return Err(MezError::conflict(
                        "external request publication scope changed",
                    ));
                }
                // This cell belongs to the unchanged original work/origin, not
                // a caller result. A repeated exact invocation returns actual
                // evidence without committing again or setting OnceLock twice.
                return Ok(publication.commit.clone());
            }
        }
        if let Some(pair) = &self.opencode_handoff {
            if pair.legacy() != &self.report {
                return Err(MezError::conflict(
                    "OpenCode admitted handoff report changed",
                ));
            }
            let (commit, group) = self.store.ingest_external_opencode_handoff_with_group(
                pair.observation(),
                pair.legacy(),
                self.now,
            )?;
            self.published
                .set(ExternalUsagePublication {
                    commit: commit.clone(),
                    group: Some(group),
                })
                .map_err(|_| MezError::conflict("external handoff result already published"))?;
            return Ok(commit);
        }
        if let Some(marker) = &self.request_marker {
            let frozen = match admission.as_deref_mut() {
                Some(admission) => self.store.freeze_external_request_report_with_admission(
                    &self.report,
                    marker,
                    admission,
                )?,
                None => self
                    .store
                    .freeze_external_request_report(&self.report, marker)?,
            };
            if frozen.report().project != self.report.project
                || frozen.report().epoch != self.report.epoch
                || (self.socket_precommit_retry
                    && self.request_origin.as_deref() != Some(frozen.origin()))
            {
                return Err(MezError::conflict(
                    "external request original projection changed",
                ));
            }
            let commit = match admission {
                Some(admission) => self
                    .store
                    .ingest_external_request_with_admission(&frozen, self.now, admission)?,
                None => self.store.ingest_external_request(&frozen, self.now)?,
            };
            self.published
                .set(ExternalUsagePublication {
                    commit: commit.clone(),
                    group: None,
                })
                .map_err(|_| MezError::conflict("external request result already published"))?;
            Ok(commit)
        } else {
            self.store.ingest_external(&self.report, self.now)
        }
    }

    /// Only the actual native ledger worker publishes durable completion;
    /// absence is an error, not a helper-supplied successful return value.
    pub(super) fn published_result(&self) -> Result<ExternalUsageCommit> {
        self.published
            .get()
            .map(|publication| publication.commit.clone())
            .ok_or_else(|| MezError::conflict("external request durable result unavailable"))
    }

    /// Test-only evidence access; callers cannot construct or publish a group.
    #[cfg(test)]
    pub(crate) fn published_group_for_tests(&self) -> Option<&ExternalUsageGroupSnapshot> {
        self.published.get()?.group.as_ref()
    }

    /// Verifies that the failure checkpoint belongs to genuine admitted native
    /// work and has not already published expense. This does not fabricate a
    /// retryable classification; the fixture still holds an actual SQLite lock.
    #[cfg(test)]
    pub(crate) fn assert_unpublished_native_attempt_for_tests(&self) {
        assert!(self.admission > 0 && self.request_marker.is_some());
        assert_eq!(self.report, self.admitted_report);
        assert!(self.published.get().is_none());
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
        let usage = self.control.external_usage();
        (usage.pending.len(), usage.reservations.len())
    }

    /// Counts issued actor admissions so retries cannot hide a fresh reservation.
    #[cfg(test)]
    pub(crate) fn external_usage_admission_count_for_tests(&self) -> u64 {
        self.control.external_usage().next_admission
    }

    /// Sets a bounded projection limit for deterministic capacity regressions.
    #[cfg(test)]
    pub(crate) fn set_external_usage_stream_limit_for_tests(&mut self, limit: usize) {
        self.control.external_usage_mut().stream_limit = Some(limit);
    }

    /// Reports bounded projection/index size without revealing stream identities.
    #[cfg(test)]
    pub(crate) fn external_usage_projection_counts_for_tests(&self) -> (usize, usize) {
        let projection = self.control.external_usage();
        (projection.streams.len(), projection.member_groups.len())
    }

    /// Installs a deterministic off-actor storage barrier for this test service.
    #[cfg(test)]
    pub(crate) fn gate_external_usage_worker_for_tests(
        &mut self,
        started: std::sync::Arc<tokio::sync::Notify>,
        release: std::sync::Arc<tokio::sync::Notify>,
    ) {
        self.control.external_usage_mut().worker_gate = Some((started, release));
    }

    /// Installs an attempt observer before actor ownership. It changes neither
    /// native admission nor the error returned by the actual storage worker.
    #[cfg(test)]
    pub(crate) fn probe_external_usage_worker_for_tests(
        &mut self,
    ) -> Arc<ExternalUsageWorkerProbe> {
        let probe = Arc::new(ExternalUsageWorkerProbe::default());
        self.control.external_usage_mut().worker_probe = Some(probe.clone());
        probe
    }

    /// Authorizes a report under a current capability-only registration and
    /// reserves bounded worker/projection capacity before any database write.
    pub(crate) fn prepare_external_usage(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
    ) -> Result<ExternalUsageWork> {
        self.prepare_external_usage_report(request, connection, None)
    }

    /// Internal exact-pair migration reuses legacy authorization, but reserves
    /// canonical group capacity before any storage work. Not an ordinary RPC or
    /// source/request association; unmapped ledger evidence still fails closed.
    #[allow(
        dead_code,
        reason = "qualified adapter migration supplies exact legacy evidence"
    )]
    pub(crate) fn prepare_external_opencode_handoff(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
        pair: crate::integrations::bootstrap::opencode::CompletedObservation,
    ) -> Result<ExternalUsageWork> {
        self.prepare_external_usage_report(request, connection, Some(pair))
    }

    /// Parses the unchanged capability-only report before accepting an optional
    /// code-owned exact pair. Pair mismatch reserves nothing and grants no role.
    fn prepare_external_usage_report(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
        pair: Option<crate::integrations::bootstrap::opencode::CompletedObservation>,
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
        if pair.as_ref().is_some_and(|pair| pair.legacy() != &report) {
            return Err(MezError::conflict(
                "OpenCode handoff does not match authorized report",
            ));
        }
        let work = ExternalUsageWork {
            admission: 0,
            request_id: request.id.clone(),
            accounting_incarnation: self.control.external_usage().incarnation.clone(),
            store: self.persistence.cloned_token_usage_store().ok_or_else(|| {
                MezError::invalid_state("durable external accounting unavailable")
            })?,
            admitted_report: report.clone(),
            report,
            pane_id: binding.pane_id.clone(),
            process: binding.process.clone(),
            now: current_unix_seconds(),
            request_marker: None,
            request_origin: None,
            socket_precommit_retry: false,
            projection_id: None,
            opencode_handoff: pair,
            published: Arc::new(OnceLock::new()),
            #[cfg(test)]
            worker_gate: self.control.external_usage().worker_gate.clone(),
            #[cfg(test)]
            worker_probe: self.control.external_usage().worker_probe.clone(),
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
        let work = self.external_request_work_for_binding(request.id.clone(), binding, &params)?;
        let mut work = self.reserve_external_usage_work(work)?;
        work.socket_precommit_retry = true;
        Ok(work)
    }

    /// Builds only original ready-marker provenance for an already authorized
    /// binding. Caller owns native admission; this helper grants no authority
    /// and performs no storage I/O or reservation itself.
    pub(super) fn external_request_work_for_binding(
        &self,
        request_id: String,
        binding: &super::external_agents::LaunchBinding,
        params: &serde_json::Value,
    ) -> Result<ExternalUsageWork> {
        let marker = super::external_agents::text(params, "request_id", 64)?;
        let origin = self
            .control
            .external_usage_begins()
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
            event_id: super::external_agents::text(params, "event_id", 64)?,
            sequence: 1,
            mode: "observation".into(),
            baseline: false,
            observed_at,
            model: ModelTokenUsageKey::new(
                super::external_agents::text(params, "provider", 128)?,
                super::external_agents::text(params, "model", 128)?,
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
            request_id,
            accounting_incarnation: self.control.external_usage().incarnation.clone(),
            store: self.persistence.cloned_token_usage_store().ok_or_else(|| {
                MezError::invalid_state("durable external accounting unavailable")
            })?,
            admitted_report: report.clone(),
            report,
            pane_id: binding.pane_id.clone(),
            process: binding.process.clone(),
            now,
            request_marker: Some(marker),
            request_origin: Some(Arc::new(origin.clone())),
            socket_precommit_retry: false,
            projection_id: None,
            opencode_handoff: None,
            published: Arc::new(OnceLock::new()),
            #[cfg(test)]
            worker_gate: self.control.external_usage().worker_gate.clone(),
            #[cfg(test)]
            worker_probe: self.control.external_usage().worker_probe.clone(),
        };
        Ok(work)
    }

    /// Shares exact stream reservations between legacy and native request work.
    /// New groups fail explicitly at capacity; no paid observation is evicted.
    pub(super) fn reserve_external_usage_work(
        &mut self,
        mut work: ExternalUsageWork,
    ) -> Result<ExternalUsageWork> {
        if work.report != work.admitted_report {
            return Err(MezError::conflict(
                "external report changed before reservation",
            ));
        }
        let stream_id = if let Some(pair) = &work.opencode_handoff {
            let epoch =
                crate::storage::token_usage::external_observation_epoch(pair.observation())?;
            crate::storage::token_usage::external_usage_stream_id(&work.report.owner, &epoch)?
        } else {
            crate::storage::token_usage::external_usage_stream_id(
                &work.report.owner,
                &work.report.epoch,
            )?
        };
        let projection = self.control.external_usage_mut();
        let stream_id = projection
            .member_groups
            .get(&stream_id)
            .cloned()
            .unwrap_or(stream_id);
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
        let replaces_member = work
            .opencode_handoff
            .as_ref()
            .and_then(|pair| {
                crate::storage::token_usage::external_usage_stream_id(
                    &pair.legacy().owner,
                    &pair.legacy().epoch,
                )
                .ok()
            })
            .filter(|id| id != &stream_id)
            .and_then(|id| projection.streams.get(&id))
            .is_some_and(|stream| {
                stream.members.is_none() && super::external_usage_group::same_scope(stream, &work)
            });
        if projection.pending.len() >= MAX_IN_FLIGHT
            || (!existing
                && projection.streams.len() + reserved_new - usize::from(replaces_member)
                    >= stream_limit)
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
        work.accounting_incarnation = projection.incarnation.clone();
        work.projection_id = Some(stream_id.clone());
        projection.pending.insert(work.admission);
        *projection.reservations.entry(stream_id).or_default() += 1;
        Ok(work)
    }

    /// Releases an admitted work that failed helper proof before a ledger
    /// publication. Preserves original readiness for the legitimate producer.
    pub(super) fn release_unexecuted_external_usage(&mut self, work: &ExternalUsageWork) {
        let projection = self.control.external_usage_mut();
        if !Arc::ptr_eq(&projection.incarnation, &work.accounting_incarnation) {
            return;
        }
        if !projection.pending.remove(&work.admission) {
            return;
        }
        if let Some(id) = &work.projection_id
            && let Some(count) = projection.reservations.get_mut(id)
        {
            *count -= 1;
            if *count == 0 {
                projection.reservations.remove(id);
            }
        }
    }

    /// Projects one committed absolute checkpoint even after registration expiry.
    /// Completion never revives a registration or schedules a provider action.
    pub(crate) fn complete_external_usage(
        &mut self,
        work: ExternalUsageWork,
        result: Result<ExternalUsageCommit>,
    ) -> String {
        #[cfg(test)]
        if let Some(probe) = &work.worker_probe {
            probe
                .completions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            probe.completed.notify_one();
        }
        let projection = self.control.external_usage_mut();
        if !Arc::ptr_eq(&projection.incarnation, &work.accounting_incarnation) {
            return crate::runtime::runtime_json_rpc_error(
                &work.request_id,
                crate::error::MezErrorKind::Conflict,
                "external accounting reservation owner changed",
            );
        }
        if !projection.pending.remove(&work.admission) {
            return crate::runtime::runtime_json_rpc_error(
                &work.request_id,
                crate::error::MezErrorKind::Conflict,
                "external accounting admission already settled",
            );
        }
        if let Some(id) = &work.projection_id
            && let Some(count) = projection.reservations.get_mut(id)
        {
            *count -= 1;
            if *count == 0 {
                projection.reservations.remove(id);
            }
        }
        let result = if work.report != work.admitted_report {
            Err(MezError::conflict(
                "external admitted report changed before settlement",
            ))
        } else if work.request_marker.is_some() || work.opencode_handoff.is_some() {
            result.and_then(|commit| {
                if work.published.get().is_some_and(|publication| {
                    publication.commit == commit
                        && (work.opencode_handoff.is_none() || publication.group.is_some())
                }) {
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
                .external_usage_begins_mut()
                .origins
                .remove(&(work.admitted_report.owner.clone(), marker.clone()));
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
            self.refresh_external_source_diagnostics_for_owner(&work.report.owner);
        }
        let projection = self.control.external_usage_mut();
        match result {
            Ok(commit) => {
                #[cfg(test)]
                let limit = projection.stream_limit.unwrap_or(MAX_PROJECTED_STREAMS);
                #[cfg(not(test))]
                let limit = MAX_PROJECTED_STREAMS;
                let projected = if let Some(group) = work
                    .published
                    .get()
                    .and_then(|publication| publication.group.as_ref())
                {
                    super::external_usage_group::reconcile(projection, &work, group, limit)
                        .map(|()| true)
                } else {
                    super::external_usage_group::complete_member(projection, &work, &commit)
                };
                let projected = match projected {
                    Ok(projected) => projected,
                    Err(error) => {
                        return crate::runtime::runtime_json_rpc_error(
                            &work.request_id,
                            error.kind(),
                            error.message(),
                        );
                    }
                };
                if !projected {
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
                            members: None,
                        });
                    if commit.revision > stream.revision {
                        stream.revision = commit.revision;
                        stream.totals = commit.totals;
                    }
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
        for stream in self.control.external_usage().streams.values() {
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
            .external_usage()
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
        for stream in self.control.external_usage_mut().streams.values_mut() {
            if stream.pane_id == pane_id {
                changed |= stream.reset_baseline != stream.totals;
                stream.reset_baseline = stream.totals;
            }
        }
        changed
    }
}
