//! Restricted ordinary admission for persistent local lifecycle producers.
//!
//! A pane hint selects one candidate, never authority. The connection's retained
//! kernel lifetime anchor supplies the producer; bounded native ancestry runs off
//! actor, and completion fences the exact root and producer before allocation.
//! This slice supports persistent Pi/OpenCode extensions only. Hook helpers and
//! shared/preexisting vendor servers require separate association evidence and
//! cannot substitute payload PIDs, inherited pane hints or temporary primaries.
//! Credentials are service-owned, private, runtime-only and producer-bound. Usage
//! remains unavailable until durable ordinary-source continuity is implemented.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use rand::Rng;
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};

use crate::control::{AuthenticatedPeer, ControlConnectionState, JsonRpcRequest};
use crate::error::{MezError, Result};
use crate::runtime::processes::{RuntimePaneProcessIdentity, RuntimePaneProcessRole};
use crate::runtime::{RuntimeSessionService, UnixOriginProcess, current_unix_seconds};

use super::external_agents::{LaunchBinding, text};

/// Bounds native workers and pending replies separately from live registrations.
const MAX_PENDING: usize = 32;
/// Total admission deadline, including actor settlement delay after observation.
const ADMISSION_BUDGET: Duration = Duration::from_secs(2);
/// Retired instance identities remain fenced for the live run; no silent eviction.
const MAX_OBSERVER_INSTANCES: usize = 128;

mod rotation;

/// Actor-owned reservations consumed on every completion, including reply loss.
#[derive(Debug, Default)]
pub(super) struct EnrollmentAdmissions {
    pending: BTreeSet<u64>,
    next: u64,
    /// Test-owned native worker barrier; production has no global timing hooks.
    #[cfg(test)]
    worker_gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    /// Test-only actor settlement observation, independent of caller reply loss.
    #[cfg(test)]
    settled: Option<Arc<tokio::sync::Notify>>,
}

/// Persistent process and private handle retained by an ordinary registration.
#[derive(Debug)]
pub(super) struct EnrollmentBinding {
    pub(super) origin: Arc<UnixOriginProcess>,
    /// Stable server-owned run identity, independent of credential replacement.
    run_generation: u64,
    /// Fresh presentation/credential epoch for each accepted observer instance.
    epoch: u64,
    /// Inert client instance selector; native provenance still supplies authority.
    instance: String,
    /// Immutable predecessor witness for identical transition retries.
    predecessor_generation: Option<u64>,
    /// Bounded retired-instance fence; old instances cannot reclaim this run.
    instances: BTreeSet<String>,
    /// Bounded authorized observer sockets; stale closure cannot clear a healthy
    /// reconnect. Weak links hold no extra socket or process descriptor.
    observers: Vec<std::sync::Weak<UnixOriginProcess>>,
    /// Redacted in Debug and never persisted or placed in generic replay caches.
    token: SecretString,
}

/// Immutable native observation work; no credential exists before actor commit.
#[derive(Debug, Clone)]
pub(crate) struct ExternalEnrollmentWork {
    admission: u64,
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
    deadline: Instant,
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
        if Instant::now() >= self.deadline {
            return Err(MezError::conflict(
                "external enrollment observation expired",
            ));
        }
        let origin = self
            .origin
            .reobserve()
            .map_err(|_| MezError::forbidden("external producer unavailable"))?;
        let root = mez_mux::process::process_parent_identity_for_pid(self.process.process_id)
            .filter(|root| root.start_token == self.process.start_token)
            .ok_or_else(|| MezError::conflict("external enrollment pane root changed"))?;
        mez_mux::process::process_ancestry(origin, root).map_err(|_| {
            MezError::forbidden("external producer is not a verified pane descendant")
        })?;
        self.origin
            .reobserve()
            .map_err(|_| MezError::forbidden("external producer changed"))?;
        if Instant::now() >= self.deadline {
            return Err(MezError::conflict(
                "external enrollment observation expired",
            ));
        }
        Ok(())
    }
}

impl EnrollmentBinding {
    /// Records only an already-authorized producer connection, without changing
    /// root/run identity or letting a stale disconnect retire its replacement.
    pub(super) fn observe_connection(&mut self, origin: &Arc<UnixOriginProcess>) {
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

    /// Allows daemon idle renewal only while both the producer and an authorized
    /// qualified observer transport remain live; no callback cadence is needed.
    pub(super) fn idle_renewable(&mut self) -> bool {
        self.observers.retain(|observer| {
            observer.upgrade().is_some_and(|observer| {
                observer.observer_connected()
                    && observer.uid() == self.origin.uid()
                    && observer.identity == self.origin.identity
            })
        });
        self.origin.is_live() && !self.observers.is_empty()
    }

    /// Checks an overdue lease without a new registry sweep; immutable endpoint
    /// polls let a delayed maintenance tick avoid retiring a connected observer.
    pub(super) fn has_live_observer(&self) -> bool {
        self.origin.is_live()
            && self.observers.iter().any(|observer| {
                observer.upgrade().is_some_and(|observer| {
                    observer.observer_connected()
                        && observer.uid() == self.origin.uid()
                        && observer.identity == self.origin.identity
                })
            })
    }

    /// Requires the same live kernel-qualified producer on every credential use;
    /// a same-user token holder on another origin cannot report on this run.
    pub(super) fn authorize(&self, connection: &ControlConnectionState) -> Result<()> {
        let origin = connection
            .unix_origin()
            .ok_or_else(|| MezError::forbidden("external producer evidence unavailable"))?;
        if !origin.writer_confirmed()
            || origin.uid() != self.origin.uid()
            || origin.identity != self.origin.identity
        {
            return Err(MezError::forbidden(
                "external producer differs from enrollment",
            ));
        }
        origin
            .reobserve()
            .map_err(|_| MezError::forbidden("external producer unavailable"))?;
        self.origin
            .reobserve()
            .map_err(|_| MezError::forbidden("external enrolled producer unavailable"))?;
        Ok(())
    }
}

impl RuntimeSessionService {
    /// Validates inert metadata and reserves finite native work before any
    /// registration or capability allocation. No client initialization is done.
    pub(crate) fn prepare_external_enrollment(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
    ) -> Result<ExternalEnrollmentWork> {
        self.require_live()?;
        if request.method != "agent/external/enroll" || connection.initialized() {
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
        let params: serde_json::Value = serde_json::from_str(raw)
            .map_err(|_| MezError::invalid_args("external enrollment requires an object"))?;
        let harness = text(&params, "harness", 64)?;
        crate::integrations::harness_policy::require_active_external_harness(&harness)?;
        if !matches!(harness.as_str(), "pi" | "opencode")
            || params
                .get("observer_kind")
                .and_then(serde_json::Value::as_str)
                != Some("persistent")
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
        let predecessor_generation = match params.get("predecessor_generation") {
            None => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .filter(|generation| *generation > 0)
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
        let registry = self.control.external_agents_mut();
        // An existing run needs a worker reservation, not another binding slot.
        // This is only a candidate lookup: native observation and commit fences
        // still run before its handle can be returned, including at saturation.
        let existing_run = registry.bindings.values().any(|binding| {
            !binding.retired
                && binding.pane_id == pane_id
                && binding.harness == harness
                && binding.process.same_incarnation(&process)
                && binding.enrollment.as_ref().is_some_and(|enrollment| {
                    enrollment.origin.uid() == origin.uid()
                        && enrollment.origin.identity == origin.identity
                })
                && binding
                    .registration
                    .as_ref()
                    .is_some_and(|registration| registration.external_session_id == session_id)
        });
        if registry.enrollments.pending.len() >= MAX_PENDING
            || (!existing_run
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
        Ok(ExternalEnrollmentWork {
            admission,
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
            deadline: Instant::now() + ADMISSION_BUDGET,
            #[cfg(test)]
            worker_gate: registry.enrollments.worker_gate.clone(),
            #[cfg(test)]
            settled: registry.enrollments.settled.clone(),
        })
    }

    /// Consumes one reservation and fences current actor authority before
    /// registering. Reply loss does not replay native work or allocate a new run.
    pub(crate) fn complete_external_enrollment(
        &mut self,
        work: ExternalEnrollmentWork,
        observed: Result<()>,
        connection: &ControlConnectionState,
    ) -> String {
        let result = self.commit_external_enrollment(&work, observed, connection);
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
        if Instant::now() >= work.deadline
            || !self.pane_process_identity_is_current(&work.pane_id, &work.process)
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
        self.reconcile_external_agent_registrations();
        let existing = self
            .control
            .external_agents()
            .bindings
            .iter()
            .find(|(_, binding)| {
                !binding.retired
                    && binding.pane_id == work.pane_id
                    && binding.process.same_incarnation(&work.process)
                    && binding.harness == work.harness
                    && binding.enrollment.as_ref().is_some_and(|enrollment| {
                        enrollment.origin.identity == work.origin.identity
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
        self.refresh_project_trust_store_from_disk_if_changed()?;
        let project_scope = self
            .trusted_project_root_for_pane(&work.pane_id)
            .map(mez_agent::messaging::ProjectMembership::from_canonical_root)
            .map(|membership| membership.scope_id());
        let accounting_origin = self.capture_accounting_origin_for_pane(&work.pane_id);
        let registry = self.control.external_agents_mut();
        if registry.bindings.len() >= super::external_agents::MAX_BINDINGS {
            return Err(MezError::new(
                crate::error::MezErrorKind::RateLimited,
                "external enrollment registry is full",
            ));
        }
        if Instant::now() >= work.deadline
            || !work.origin.is_live()
            || !work.origin.writer_confirmed()
        {
            return Err(MezError::conflict(
                "external enrollment evidence expired before allocation",
            ));
        }
        let generation = registry
            .next_generation
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("external enrollment generation exhausted"))?;
        registry.next_generation = generation;
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
                accounting_owner: crate::storage::token_usage::new_token_usage_event_id(),
                accounting_origin,
                enrollment: Some(EnrollmentBinding {
                    origin: work.origin.clone(),
                    run_generation: generation,
                    epoch: 1,
                    instance: work.observer_instance.clone(),
                    predecessor_generation: None,
                    instances: BTreeSet::from([work.observer_instance.clone()]),
                    observers: vec![Arc::downgrade(&work.origin)],
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
        Ok(enrollment_response(binding, enrollment, &work.session_id))
    }
}

/// Returns only a restricted private handle and explicit lifecycle-only coverage.
fn enrollment_response(
    binding: &LaunchBinding,
    enrollment: &EnrollmentBinding,
    session: &str,
) -> String {
    serde_json::json!({"protocol":"external-agent/1","launch_token":enrollment.token.expose_secret(),
        "agent_id":binding.registration.as_ref().map(|registration| registration.agent_id.as_str()),
        "run_id":enrollment.run_generation,"observer_epoch":enrollment.epoch,"observer_instance":enrollment.instance,
        "generation":binding.generation,"external_session_id":session,"registered":true,"controls":[],
        "expires_at_unix_seconds":binding.expires,"lease_seconds":60,"usage":"unavailable-source-continuity"}).to_string()
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
