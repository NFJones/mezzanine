//! Restricted ordinary admission for persistent local lifecycle producers.
//!
//! A pane hint selects one candidate, never authority. The connection's retained
//! kernel lifetime anchor supplies the producer; bounded native ancestry runs off
//! actor, and completion fences the exact root and producer before allocation.
//! Persistent protocol eligibility covers the six supported harness labels,
//! independently of whether a vendor adapter is installed/loaded. Hook helpers and
//! shared/preexisting vendor servers require separate association evidence and
//! cannot substitute payload PIDs, inherited pane hints or temporary primaries.
//! Credentials are service-owned, private, runtime-only and producer-bound. Usage
//! requires its own qualified durable source mapping and request-bound reporting;
//! successful lifecycle enrollment alone never enables accounting or proves billing.

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use rand::Rng;
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};

use crate::control::{AuthenticatedPeer, ControlConnectionState, JsonRpcRequest};
use crate::error::{MezError, Result};
use crate::runtime::processes::{RuntimePaneProcessIdentity, RuntimePaneProcessRole};
use crate::runtime::{
    RuntimeSessionService, UnixAncestryBudget, UnixAncestryWitness, UnixOriginProcess,
    current_unix_seconds,
};

use super::external_agents::{LaunchBinding, text};

/// Bounds native workers and pending replies separately from live registrations.
const MAX_PENDING: usize = 32;
/// Total admission deadline, including actor settlement delay after observation.
const ADMISSION_BUDGET: Duration = Duration::from_secs(2);
/// Retired instance identities remain fenced for the live run; no silent eviction.
const MAX_OBSERVER_INSTANCES: usize = 128;

mod codex;
mod codex_order;
mod copilot;
mod curated;
mod cursor;
mod helper;
mod helper_usage;
mod producer;
mod publication;
mod recovery;
mod rotation;
mod source;
mod source_clock;
mod source_end;
pub(crate) use source_end::ExternalSourceEndWork;
mod source_retirement;
pub(crate) use source_retirement::ExternalSourceRetirementWork;
mod source_renewal;
pub(crate) use source_renewal::ExternalSourceRenewalWork;
mod targets;
use producer::ProducerEvidence;
pub(super) use recovery::LiveSourceAnchors;
use recovery::{ResumeAdmission, SourceResume};

/// Actor-owned reservations consumed on every completion, including reply loss.
#[derive(Debug, Default)]
pub(super) struct EnrollmentAdmissions {
    /// Shared native/publication pool; sealed work carries the exact phase.
    pending: BTreeSet<u64>,
    next: u64,
    /// Registry incarnation fences reused numeric reservation IDs after reset.
    incarnation: Arc<()>,
    /// Aggregate native ancestry descriptors across workers and live runs.
    pub(in crate::runtime::control) ancestry_budget: Arc<UnixAncestryBudget>,
    /// Candidate selectors maintained only by ordinary binding lifecycle owners.
    pub(in crate::runtime::control) helper_targets: targets::HelperTargets,
    /// Original declared creator/session fences outlive expiring capabilities,
    /// but confer no ancestry/observer authority and end on actual source death.
    pub(in crate::runtime::control) curated_namespaces: curated::CuratedNamespaces,
    /// Test-owned native worker barrier; production has no global timing hooks.
    #[cfg(test)]
    worker_gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    /// Test-only actor settlement observation, independent of caller reply loss.
    #[cfg(test)]
    settled: Option<Arc<tokio::sync::Notify>>,
}

/// Typed native producer facts and private handle retained by a registration.
#[derive(Debug)]
pub(super) struct EnrollmentBinding {
    producer: ProducerEvidence,
    /// Native mapping was actually observed; fallback owners cannot start usage.
    pub(super) usage_owner_is_durable: bool,
    /// At least one native request-bound durable report was actually received.
    pub(super) request_reports_seen: bool,
    /// Original qualified ancestry; retirement releases it without dropping
    /// observational retry tombstones or immutable accounting provenance.
    pub(super) ancestry: Option<Arc<UnixAncestryWitness>>,
    /// Stable server-owned run identity, independent of credential replacement.
    run_generation: u64,
    /// Fresh presentation/credential epoch for each accepted observer instance.
    epoch: u64,
    /// Inert client instance selector; native provenance still supplies authority.
    instance: String,
    /// Immutable predecessor witness for identical transition retries.
    predecessor_generation: Option<u64>,
    /// Exact accepted explicit resume input, not a newest-observer alias.
    accepted_resume: Option<SourceResume>,
    /// True only for durable recovery that created a fresh missing projection.
    recovered_projection: bool,
    /// Inherited fixed diagnostic only; no lifecycle/candidate state survives.
    resume_usage_gap: Option<crate::integrations::bootstrap::pi_ipc::UsageGapReason>,
    /// Bounded retired-instance fence; old instances cannot reclaim this run.
    instances: BTreeSet<String>,
    /// Bounded authorized observer sockets; stale closure cannot clear a healthy
    /// reconnect. Weak links hold no extra socket or process descriptor.
    observers: Vec<std::sync::Weak<UnixOriginProcess>>,
    /// Explicit recent original-epoch proof for a declared parent observer;
    /// native creator survival alone never supplies this proof.
    curated_observer: Option<curated::CuratedObserver>,
    /// Bounded original-epoch Codex turn replay state, never billing or liveness.
    codex_turns: codex::CodexTurns,
    /// Original classic Copilot timestamp fence, never a turn or billing claim.
    copilot_events: copilot::CopilotEvents,
    /// Original Cursor generation fences, never counters or idle health.
    cursor_generations: cursor::CursorGenerations,
    /// Redacted in Debug and never persisted or placed in generic replay caches.
    token: SecretString,
}

/// Immutable native observation and sealed post-admission publication work.
/// No credential exists before actor commit; a private reply may be retained only
/// for the worker's optional acceptance phase after actual registration.
#[derive(Debug, Clone)]
pub(crate) struct ExternalEnrollmentWork {
    admission: u64,
    /// Original reservation owner, not a process or enrollment capability.
    registry_incarnation: Arc<()>,
    pub(crate) request_id: String,
    origin: Arc<UnixOriginProcess>,
    process: RuntimePaneProcessIdentity,
    pane_id: String,
    harness: String,
    version: String,
    session_id: String,
    display_name: String,
    observer_instance: String,
    predecessor_generation: Option<u64>,
    /// Public receipt selector; native lifetime evidence remains mandatory.
    source_resume: Option<SourceResume>,
    /// Retained control-owner revision captured before native work is reserved.
    resume_admission: Option<ResumeAdmission>,
    deadline: Instant,
    /// Existing run only; no helper observation may allocate a producer owner.
    helper: Option<helper::HelperPresentation>,
    /// Narrow declared creator contract; native parent is published only off actor.
    curated: Option<curated::CuratedAdmission>,
    /// Worker clones publish the original immutable evidence once. A caller's
    /// successful Result alone cannot fabricate or replace native ancestry.
    ancestry: Arc<OnceLock<Arc<UnixAncestryWitness>>>,
    ancestry_budget: Arc<UnixAncestryBudget>,
    /// Immutable off-actor ledger handle; absence cannot enable accounting.
    source_store: Option<crate::storage::token_usage::TokenUsageStore>,
    /// One-shot native namespace observation, not an admission capability.
    source_owner: Arc<OnceLock<Option<String>>>,
    /// Atomic lookup novelty; existing rows cannot recreate missing live owners.
    source_created: Arc<OnceLock<bool>>,
    /// Original native proposal and write outcomes survive worker return. Only
    /// a separate actual actor grant can start acceptance; neither stored state
    /// nor these inputs alone grant replacement-control authority.
    source_preparation: Arc<source_retirement::NativeSourceSlot>,
    /// Private first-admission reply retained only during off-actor publication.
    /// Never serialized into the activation ledger or exposed through Debug.
    source_publication: Option<SecretString>,
    /// Deterministic test-owned barrier proving native work does not hold actor.
    #[cfg(test)]
    pub(crate) worker_gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    /// Test-only signal after the actor consumes this exact admission.
    #[cfg(test)]
    settled: Option<Arc<tokio::sync::Notify>>,
}

impl ExternalEnrollmentWork {
    /// Resolves one selected root against the exact live socket origin. This
    /// synchronous bounded reader must execute on a worker, never the actor.
    pub(crate) fn observe(&self) -> Result<()> {
        if let Some(order) = self
            .helper
            .as_ref()
            .and_then(|helper| helper.codex_order.as_ref())
        {
            order.wait(self.deadline)?;
        }
        if Instant::now() >= self.deadline {
            return Err(MezError::conflict(
                "external enrollment observation expired",
            ));
        }
        let origin = self
            .origin
            .reobserve()
            .map_err(|_| MezError::forbidden("external producer unavailable"))?;
        if origin.process_id == self.process.process_id {
            return Err(MezError::forbidden(
                "external producer must be a distinct pane descendant",
            ));
        }
        if let Some(curated) = &self.curated {
            if self.helper.is_some() {
                return Err(MezError::forbidden("mixed source work unavailable"));
            }
            return curated.observe(self);
        }
        let parent = if let Some(helper) = &self.helper {
            helper
                .producer
                .reobserve()
                .map_err(|_| MezError::forbidden("external helper producer unavailable"))?;
            let parent = self
                .origin
                .capture_parent()
                .map_err(|_| MezError::forbidden("external helper parent unavailable"))?;
            if parent.uid() != helper.producer.uid()
                || parent.identity != helper.producer.identity()
            {
                return Err(MezError::forbidden(
                    "external helper is not a direct child of its enrolled producer",
                ));
            }
            parent
                .reobserve()
                .map_err(|_| MezError::forbidden("external helper parent changed"))?;
            helper
                .producer
                .reobserve()
                .map_err(|_| MezError::forbidden("external helper producer changed"))?;
            Some(parent)
        } else {
            None
        };
        let root = mez_mux::process::process_parent_identity_for_pid(self.process.process_id)
            .filter(|root| root.start_token == self.process.start_token)
            .ok_or_else(|| MezError::conflict("external enrollment pane root changed"))?;
        let ancestry = match parent {
            Some(parent) => parent.capture_ancestry(root, &self.ancestry_budget),
            None => self.origin.capture_ancestry(root, &self.ancestry_budget),
        }
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                MezError::new(
                    crate::error::MezErrorKind::RateLimited,
                    "external ancestry capacity unavailable",
                )
            } else {
                MezError::forbidden("external producer ancestry unavailable")
            }
        })?;
        self.origin
            .reobserve()
            .map_err(|_| MezError::forbidden("external producer changed"))?;
        if Instant::now() >= self.deadline {
            return Err(MezError::conflict(
                "external enrollment observation expired",
            ));
        }
        self.ancestry
            .set(Arc::new(ancestry))
            .map_err(|_| MezError::conflict("external ancestry already observed"))?;
        if let Some(helper) = &self.helper {
            helper.observe_usage(self)?;
        }
        if self.helper.is_none() {
            self.observe_source_owner(&ProducerEvidence::Socket(self.origin.clone()));
        }
        Ok(())
    }
}

impl EnrollmentBinding {
    /// Exposes only the fixed inherited gap diagnostic, never lifecycle state.
    pub(super) fn inherited_usage_gap(
        &self,
    ) -> Option<crate::integrations::bootstrap::pi_ipc::UsageGapReason> {
        self.resume_usage_gap
    }

    /// Permits the first actual gap fact only after explicit native source resume.
    pub(super) fn allows_initial_usage_gap(&self) -> bool {
        self.recovered_projection
    }

    /// Whole-chain lifetime fencing uses only bounded nonblocking kernel polls;
    /// a live producer cannot keep an orphaned pane association authoritative.
    pub(super) fn provenance_is_live(&self) -> bool {
        self.producer.is_live()
            && self
                .ancestry
                .as_ref()
                .is_some_and(|ancestry| ancestry.is_live())
    }

    /// Records only an already-authorized producer connection, without changing
    /// root/run identity or letting a stale disconnect retire its replacement.
    pub(super) fn observe_connection(&mut self, origin: &Arc<UnixOriginProcess>) {
        if !self.producer.matches_socket(origin) {
            return;
        }
        self.observers.retain(|observer| {
            observer
                .upgrade()
                .is_some_and(|observer| observer.observer_connected())
        });
        if origin.observer_connected()
            && self.observers.len() < 16
            && !self
                .observers
                .iter()
                .any(|observer| observer.ptr_eq(&Arc::downgrade(origin)))
        {
            self.observers.push(Arc::downgrade(origin));
        }
    }

    /// Allows daemon renewal for live socket observers or live original creators
    /// with explicit recent epoch proof; parent survival alone is never enough.
    pub(super) fn idle_renewable(&mut self) -> bool {
        if let Some(observer) = &self.curated_observer {
            return matches!(self.producer, ProducerEvidence::VerifiedParent(_))
                && self.provenance_is_live()
                && observer.is_recent();
        }
        self.observers.retain(|observer| {
            observer.upgrade().is_some_and(|observer| {
                observer.observer_connected() && self.producer.matches_socket(&observer)
            })
        });
        self.provenance_is_live() && !self.observers.is_empty()
    }

    /// Checks an overdue lease without a new registry sweep; immutable endpoint
    /// polls let a delayed maintenance tick avoid retiring a connected observer.
    pub(super) fn has_live_observer(&self) -> bool {
        if let Some(observer) = &self.curated_observer {
            return matches!(self.producer, ProducerEvidence::VerifiedParent(_))
                && self.provenance_is_live()
                && observer.is_recent();
        }
        self.provenance_is_live()
            && self.observers.iter().any(|observer| {
                observer.upgrade().is_some_and(|observer| {
                    observer.observer_connected() && self.producer.matches_socket(&observer)
                })
            })
    }

    /// Labels only never-observed curated activation, without supplying observer
    /// freshness, lease renewal or native authority to the caller.
    pub(super) fn awaiting_first_curated_proof(&self) -> bool {
        self.curated_observer
            .as_ref()
            .is_some_and(|observer| observer.observed_at.is_none())
    }

    /// Requires the same live kernel-qualified producer on every credential use;
    /// a same-user token holder on another origin cannot report on this run.
    pub(super) fn authorize(&self, connection: &ControlConnectionState) -> Result<()> {
        if !self.provenance_is_live() {
            return Err(MezError::forbidden(
                "external producer ancestry unavailable",
            ));
        }
        self.authorize_connection(connection)?;
        if !self.provenance_is_live() {
            return Err(MezError::forbidden("external producer ancestry changed"));
        }
        Ok(())
    }

    /// Matches the original live producer and current writer for exact inert
    /// tombstone receipts. This does not grant ancestry, renewal or presentation
    /// authority; retired dispatch must separately permit only its old receipt.
    pub(super) fn authorize_connection(&self, connection: &ControlConnectionState) -> Result<()> {
        let origin = connection
            .unix_origin()
            .ok_or_else(|| MezError::forbidden("external producer evidence unavailable"))?;
        self.producer.authorize_socket(origin)
    }
}

impl RuntimeSessionService {
    /// Bulk lifecycle replacement fences even a never-published initial worker.
    /// Accounting begin/end owners are independent and retain already-admitted
    /// incurred expense; only native enrollment reservations are invalidated.
    pub(in crate::runtime::control) fn invalidate_external_enrollment_admissions(&mut self) {
        let admissions = &mut self.control.external_agents_mut().enrollments;
        admissions.incarnation = Arc::new(());
        admissions.pending.clear();
    }

    /// Validates inert metadata and reserves finite native work before any
    /// registration or capability allocation. No client initialization is done.
    pub(crate) fn prepare_external_enrollment(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
    ) -> Result<ExternalEnrollmentWork> {
        self.require_live()?;
        if !matches!(
            request.method.as_str(),
            "agent/external/enroll" | "agent/external/curated-enroll"
        ) || connection.initialized()
        {
            return Err(MezError::forbidden(
                "external enrollment requires uninitialized Unix ingress",
            ));
        }
        let Some(AuthenticatedPeer::UnixUser { uid }) = connection.authenticated_peer() else {
            return Err(MezError::forbidden(
                "external enrollment requires authenticated Unix transport",
            ));
        };
        crate::control::validate_control_method_params_schema(request)?;
        let raw = request.params.as_deref().unwrap_or("{}");
        if raw.len() > 4096 {
            return Err(MezError::invalid_args(
                "external enrollment metadata exceeds limit",
            ));
        }
        let params = crate::protocol::strict_json::decode(raw.as_bytes()).map_err(|_| {
            MezError::invalid_args("external enrollment requires an unambiguous object")
        })?;
        let harness = text(&params, "harness", 64)?;
        crate::integrations::harness_policy::require_active_external_harness(&harness)?;
        let curated = if request.method == "agent/external/curated-enroll" {
            Some(curated::CuratedAdmission::from_params(&params)?)
        } else {
            None
        };
        if curated.is_none()
            && (!matches!(
                harness.as_str(),
                "claude" | "codex" | "copilot" | "opencode" | "cursor" | "pi"
            ) || params
                .get("observer_kind")
                .and_then(serde_json::Value::as_str)
                != Some("persistent"))
        {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotImplemented,
                "external enrollment requires a supported persistent producer; helper/server association unavailable",
            ));
        }
        let pane_id = text(&params, "pane_id", 64)?;
        let version = text(&params, "version", 128)?;
        let session_id = text(&params, "external_session_id", 128)?;
        let display_name = text(&params, "display_name", 128)?;
        let observer_instance = text(&params, "observer_instance", 128)?;
        let source_resume = SourceResume::from_params(&params)?;
        if source_resume.is_some() && curated.is_some() {
            return Err(MezError::invalid_args(
                "source resume requires a persistent socket producer",
            ));
        }
        let predecessor_generation = match params.get("predecessor_generation") {
            None => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .filter(|generation| {
                        *generation > 0
                            && (curated.is_none() || *generation <= 9_007_199_254_740_991)
                    })
                    .ok_or_else(|| {
                        MezError::invalid_args("external observer predecessor must be positive")
                    })?,
            ),
        };
        let origin = connection
            .unix_origin()
            .filter(|origin| origin.uid() == *uid && origin.writer_confirmed())
            .ok_or_else(|| MezError::forbidden("external producer evidence unavailable"))?
            .clone();
        let process = self
            .pane_process_identity(&pane_id)
            .map_err(|_| MezError::conflict("external enrollment pane root unavailable"))?;
        if process.role != RuntimePaneProcessRole::AdapterOwnedRoot {
            return Err(MezError::conflict(
                "external enrollment requires adapter-owned root",
            ));
        }
        // A pane-root lifetime is routing evidence, not an independent producer.
        // An exec at the same PID must not turn the shell's root anchor into a
        // producer; executable names and metadata never supply this boundary.
        if origin.identity.process_id == process.process_id {
            return Err(MezError::forbidden(
                "external producer must be a distinct pane descendant",
            ));
        }
        let source_store = self.persistence.cloned_token_usage_store();
        self.prune_external_source_anchors();
        self.sync_external_ancestry_budget();
        let source_preparation = if source_store.is_some() && curated.is_none() {
            self.control
                .external_sources()
                .reserve_activation_metadata()
        } else {
            Arc::new(source_retirement::NativeSourceSlot::unreserved())
        };
        let registry = self.control.external_agents_mut();
        // An existing run needs a worker reservation, not another binding slot.
        // This is only a candidate lookup: native observation and commit fences
        // still run before its handle can be returned, including at saturation.
        let existing_run = registry.bindings.values().any(|binding| {
            !binding.retired
                && binding.pane_id == pane_id
                && binding.harness == harness
                && binding.process.same_incarnation(&process)
                && binding
                    .enrollment
                    .as_ref()
                    .is_some_and(|enrollment| enrollment.producer.matches_socket(&origin))
                && binding
                    .registration
                    .as_ref()
                    .is_some_and(|registration| registration.external_session_id == session_id)
        });
        if registry.enrollments.pending.len() >= MAX_PENDING
            || (curated.is_none()
                && !existing_run
                && registry.bindings.len() + registry.enrollments.pending.len()
                    >= super::external_agents::MAX_BINDINGS)
        {
            return Err(MezError::new(
                crate::error::MezErrorKind::RateLimited,
                "external enrollment capacity unavailable",
            ));
        }
        let admission =
            registry.enrollments.next.checked_add(1).ok_or_else(|| {
                MezError::invalid_state("external enrollment generation exhausted")
            })?;
        registry.enrollments.next = admission;
        registry.enrollments.pending.insert(admission);
        let mut work = ExternalEnrollmentWork {
            admission,
            registry_incarnation: registry.enrollments.incarnation.clone(),
            request_id: request.id.clone(),
            origin,
            process,
            pane_id,
            harness,
            version,
            session_id,
            display_name,
            observer_instance,
            predecessor_generation,
            source_resume,
            resume_admission: None,
            deadline: Instant::now() + ADMISSION_BUDGET,
            helper: None,
            curated,
            ancestry: Arc::new(OnceLock::new()),
            ancestry_budget: registry.enrollments.ancestry_budget.clone(),
            source_store,
            source_owner: Arc::new(OnceLock::new()),
            source_created: Arc::new(OnceLock::new()),
            source_preparation,
            source_publication: None,
            #[cfg(test)]
            worker_gate: registry.enrollments.worker_gate.clone(),
            #[cfg(test)]
            settled: registry.enrollments.settled.clone(),
        };
        match self.capture_external_source_resume(&work) {
            Ok(admission) => work.resume_admission = admission,
            Err(error) => {
                self.control
                    .external_agents_mut()
                    .enrollments
                    .pending
                    .remove(&work.admission);
                return Err(error);
            }
        }
        Ok(work)
    }

    /// Consumes one reservation and fences current actor authority before
    /// registering. Reply loss does not replay native work or allocate a new run.
    pub(crate) fn complete_external_enrollment(
        &mut self,
        work: ExternalEnrollmentWork,
        observed: Result<()>,
        connection: &ControlConnectionState,
    ) -> String {
        let _order = codex_order::CodexOrderSettlement(
            work.helper
                .as_ref()
                .and_then(|helper| helper.codex_order.clone()),
        );
        let result = self.commit_external_enrollment(&work, observed, connection);
        if let Some(end) = work
            .helper
            .as_ref()
            .and_then(|helper| helper.usage_end.as_ref())
        {
            match end.published_result() {
                Ok(commit) => {
                    let accounting_reply =
                        self.complete_external_usage((**end).clone(), Ok(commit));
                    // Already-admitted actual expense settles even if native
                    // presentation authority changed; stale helpers get no ACK.
                    if result.is_ok() {
                        return accounting_reply;
                    }
                }
                Err(_) => self.release_unexecuted_external_usage(end),
            }
        }
        #[cfg(test)]
        if let Some(settled) = &work.settled {
            settled.notify_one();
        }
        match result {
            Ok(result) => format!(
                r#"{{"jsonrpc":"2.0","id":{},"result":{result}}}"#,
                work.request_id
            ),
            Err(error) => {
                super::runtime_json_rpc_error(&work.request_id, error.kind(), error.message())
            }
        }
    }

    /// Allocates only after native evidence, root incarnation, ingress ownership
    /// and deadline all survive actor settlement. Identical retry reuses the run.
    fn commit_external_enrollment(
        &mut self,
        work: &ExternalEnrollmentWork,
        observed: Result<()>,
        connection: &ControlConnectionState,
    ) -> Result<String> {
        if !Arc::ptr_eq(
            &work.registry_incarnation,
            &self.control.external_agents().enrollments.incarnation,
        ) {
            return Err(MezError::conflict(
                "external enrollment reservation owner changed",
            ));
        }
        if work.source_publication_was_scheduled() {
            return Err(MezError::conflict(
                "external enrollment native phase already settled",
            ));
        }
        if !self
            .control
            .external_agents_mut()
            .enrollments
            .pending
            .remove(&work.admission)
        {
            return Err(MezError::conflict(
                "external enrollment admission already settled",
            ));
        }
        observed?;
        self.require_live()?;
        // Bounded admission expiry and a changed root incarnation are different
        // refusals. Reporting them as one conflict made a loaded-run refusal
        // unattributable; this stays content-free and adds no new acceptance
        // path.
        if Instant::now() >= work.deadline {
            return Err(MezError::conflict(
                "external enrollment admission deadline expired",
            ));
        }
        if !self.pane_process_identity_is_current(&work.pane_id, &work.process)
            || !self
                .pane_process_identity(&work.pane_id)
                .is_ok_and(|root| root.same_incarnation(&work.process))
        {
            return Err(MezError::conflict(
                "external enrollment root or deadline changed",
            ));
        }
        if connection.initialized()
            || !work.origin.writer_confirmed()
            || !matches!(connection.authenticated_peer(), Some(AuthenticatedPeer::UnixUser { uid }) if *uid == work.origin.uid())
            || connection
                .unix_origin()
                .is_none_or(|origin| !Arc::ptr_eq(origin, &work.origin))
        {
            return Err(MezError::forbidden(
                "external enrollment connection changed",
            ));
        }
        work.origin
            .reobserve()
            .map_err(|_| MezError::forbidden("external producer changed"))?;
        let ancestry = work
            .ancestry
            .get()
            .filter(|ancestry| ancestry.is_live())
            .ok_or_else(|| MezError::forbidden("external producer ancestry changed"))?;
        if let Some(curated) = &work.curated {
            if work.helper.is_some() {
                return Err(MezError::forbidden("mixed source work unavailable"));
            }
            return self.commit_curated_enrollment(work, curated, ancestry, connection);
        }
        if let Some(helper) = &work.helper {
            return self.commit_external_helper_presentation(work, helper);
        }
        self.reconcile_external_agent_registrations();
        if work.source_resume.is_some() {
            return self.commit_external_source_resume(work, connection);
        }
        self.validate_external_source_initial(work)?;
        let existing =
            self.control
                .external_agents()
                .bindings
                .iter()
                .find(|(_, binding)| {
                    !binding.retired
                        && binding.pane_id == work.pane_id
                        && binding.process.same_incarnation(&work.process)
                        && binding.harness == work.harness
                        && binding.enrollment.as_ref().is_some_and(|enrollment| {
                            enrollment.producer.matches_socket(&work.origin)
                        })
                        && binding.registration.as_ref().is_some_and(|registration| {
                            registration.external_session_id == work.session_id
                        })
                })
                .map(|(digest, _)| *digest);
        if let Some(digest) = existing {
            return self.replace_or_retry_external_observer(digest, work, connection);
        }
        if work.predecessor_generation.is_some() {
            return Err(MezError::conflict(
                "external observer predecessor unavailable",
            ));
        }
        self.validate_external_source_mapping_allocation(work)?;
        self.refresh_project_trust_store_from_disk_if_changed()?;
        let project_scope = self
            .trusted_project_root_for_pane(&work.pane_id)
            .map(mez_agent::messaging::ProjectMembership::from_canonical_root)
            .map(|membership| membership.scope_id());
        let accounting_origin = self.capture_accounting_origin_for_pane(&work.pane_id);
        let (registry, next_generation) = self.control.external_agents_and_generation_mut();
        if registry.bindings.len() >= super::external_agents::MAX_BINDINGS {
            return Err(MezError::new(
                crate::error::MezErrorKind::RateLimited,
                "external enrollment registry is full",
            ));
        }
        if Instant::now() >= work.deadline
            || !work.origin.is_live()
            || !work.origin.writer_confirmed()
            || !ancestry.is_live()
        {
            return Err(MezError::conflict(
                "external enrollment evidence expired before allocation",
            ));
        }
        let generation = next_generation
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("external enrollment generation exhausted"))?;
        *next_generation = generation;
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let digest = Sha256::digest(token.as_bytes()).into();
        registry.bindings.insert(
            digest,
            LaunchBinding {
                uid: work.origin.uid(),
                pane_id: work.pane_id.clone(),
                process: work.process.clone(),
                project_scope,
                generation,
                harness: work.harness.clone(),
                version: work.version.clone(),
                expires: current_unix_seconds().saturating_add(60),
                registration: None,
                retired: false,
                accounting_owner: work
                    .source_owner
                    .get()
                    .and_then(Clone::clone)
                    .unwrap_or_else(crate::storage::token_usage::new_token_usage_event_id),
                accounting_origin,
                enrollment: Some(EnrollmentBinding {
                    producer: ProducerEvidence::Socket(work.origin.clone()),
                    usage_owner_is_durable: work.source_owner.get().is_some_and(Option::is_some),
                    request_reports_seen: false,
                    ancestry: Some(ancestry.clone()),
                    run_generation: generation,
                    epoch: 1,
                    instance: work.observer_instance.clone(),
                    predecessor_generation: None,
                    accepted_resume: None,
                    recovered_projection: false,
                    resume_usage_gap: None,
                    instances: BTreeSet::from([work.observer_instance.clone()]),
                    observers: vec![Arc::downgrade(&work.origin)],
                    curated_observer: None,
                    codex_turns: Default::default(),
                    copilot_events: Default::default(),
                    cursor_generations: Default::default(),
                    token: SecretString::from(token),
                }),
                pi_lifecycle: None,
            },
        );
        if let Err(error) = self.register_external_agent(
            digest,
            &serde_json::json!({
                "external_session_id":work.session_id,"display_name":work.display_name,
            }),
        ) {
            self.retire_external_agent_binding(digest);
            return Err(error);
        }
        self.control
            .external_agents_mut()
            .enrollments
            .helper_targets
            .insert(work.origin.uid(), &work.harness, &work.session_id, digest);
        self.record_external_source_anchor(digest);
        let binding = self
            .control
            .external_agents()
            .bindings
            .get(&digest)
            .ok_or_else(|| MezError::invalid_state("external enrollment settlement disappeared"))?;
        let enrollment = binding
            .enrollment
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("external enrollment owner disappeared"))?;
        work.record_source_actor_admission(digest, binding.generation);
        let response = enrollment_response(binding, enrollment, &work.session_id);
        self.retain_external_source_activation(digest, work);
        Ok(response)
    }
}

/// Returns only a restricted private handle and explicit lifecycle-only coverage.
fn enrollment_response(
    binding: &LaunchBinding,
    enrollment: &EnrollmentBinding,
    session: &str,
) -> String {
    let mut response = serde_json::json!({"protocol":"external-agent/1","launch_token":enrollment.token.expose_secret(),
        "observer_witness":observer_witness(Sha256::digest(enrollment.token.expose_secret().as_bytes()).into()),
        "agent_id":binding.registration.as_ref().map(|registration| registration.agent_id.as_str()),
        "run_id":enrollment.run_generation,"observer_epoch":enrollment.epoch,"observer_instance":enrollment.instance,
        "generation":binding.generation,"external_session_id":session,"registered":true,"controls":[],
        "expires_at_unix_seconds":binding.expires,"lease_seconds":60,"usage":"unavailable-source-continuity"});
    if enrollment.recovered_projection {
        response["recovery"] = serde_json::json!({"kind":"same-live-source","next_sequence":1});
    }
    response.to_string()
}

#[cfg(all(test, target_os = "linux"))]
mod tests;

/// Public instance selector derived from the one-way credential digest. Its
/// 64-character hex form cannot be used as the 43-character private capability.
/// It fences numeric generation reuse across restart, never grants authority.
fn observer_witness(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
