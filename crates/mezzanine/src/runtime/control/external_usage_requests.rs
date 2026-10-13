//! Qualified native request-begin admission and off-actor origin persistence.
//!
//! The new primitive records a native-source request boundary, not a vendor
//! invoice or filesystem authority. Only an existing persistent producer may
//! begin, using its original private credential and kernel-qualified writer.
//! Actor-retained trust/mapping snapshots are object-qualified against the
//! producer's native cwd off actor; pane hints never select the origin. Roots,
//! source lifetime, generation and deadlines are checked again at settlement.
//! Readiness is consumed only by the separate request-end reporting owner. No
//! provider action, native task or cross-process handoff is enabled here.

use crate::control::{AuthenticatedPeer, ControlConnectionState, JsonRpcRequest};
use crate::error::{MezError, Result};
use crate::runtime::processes::RuntimePaneProcessIdentity;
use crate::runtime::{RuntimeSessionService, current_unix_seconds};
use crate::storage::token_usage::{
    AccountingProjectRecord, ExternalRequestOrigin, TokenUsageStore, accounting_origin_for_root,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Readiness is reconstructible metadata, not durable deduplication evidence.
const MAX_READY_ORIGINS: usize = 4096;

/// Bounded pending metadata reservations; they do not reserve billing streams.
#[derive(Debug, Default)]
pub(super) struct ExternalUsageBegins {
    pending: BTreeSet<u64>,
    next: u64,
    /// Exact control owner survives registration loss, not service replacement.
    incarnation: Arc<()>,
    /// Readiness cache only; durable markers remain authoritative after eviction.
    pub(super) origins: BTreeMap<(String, String), ExternalRequestOrigin>,
}

impl ExternalUsageBegins {
    /// Keeps metadata memory bounded without refusing unrelated new requests.
    /// Evicted entries are restored only by their same source-scoped begin;
    /// durable request/observation/expense tables are never changed here.
    pub(super) fn cache_origin(
        &mut self,
        owner: String,
        marker: String,
        origin: ExternalRequestOrigin,
    ) {
        let key = (owner, marker);
        if !self.origins.contains_key(&key) && self.origins.len() >= MAX_READY_ORIGINS {
            self.origins.pop_first();
        }
        self.origins.insert(key, origin);
    }
}

/// Credential-free frozen work; metadata selectors cannot replace native proof.
#[derive(Debug, Clone)]
pub(crate) struct ExternalUsageBeginWork {
    admission: u64,
    /// Original readiness reservation owner, never a reporting capability.
    accounting_incarnation: Arc<()>,
    pub(crate) request_id: String,
    digest: [u8; 32],
    generation: u64,
    pane_id: String,
    process: RuntimePaneProcessIdentity,
    owner: String,
    harness: String,
    session: String,
    marker: String,
    /// Original qualified producer lifetime, never a payload-selected PID.
    origin: Arc<crate::runtime::UnixOriginProcess>,
    /// Immutable trust and mapping snapshots for worker-only resolution.
    trust: Option<Arc<crate::security::project::ProjectTrustStore>>,
    inventory: Option<Arc<Vec<AccountingProjectRecord>>>,
    store: TokenUsageStore,
    now: u64,
    deadline: Instant,
    /// Only the actual storage worker can publish readiness evidence.
    published: Arc<OnceLock<ExternalRequestOrigin>>,
}

impl ExternalUsageBeginWork {
    /// Forces only a test-owned settlement deadline without changing authority.
    #[cfg(test)]
    pub(crate) fn expire_for_tests(&mut self) {
        self.deadline = Instant::now();
    }
    /// Qualifies original mapping object identity and persists only opaque
    /// origin/marker metadata. No disk operation runs on actor ownership.
    pub(crate) fn execute(&self) -> Result<ExternalRequestOrigin> {
        if Instant::now() >= self.deadline {
            return Err(MezError::conflict("external request admission expired"));
        }
        if let Some(origin) = self.published.get() {
            return Ok(origin.clone());
        }
        let identity = self
            .origin
            .reobserve()
            .map_err(|_| MezError::forbidden("external request producer changed"))?;
        let cwd = mez_mux::process::current_working_directory_for_pid(identity.process_id)
            .and_then(|cwd| std::fs::canonicalize(cwd).ok());
        let root = cwd.as_ref().and_then(|cwd| {
            self.trust.as_ref().and_then(|trust| {
                crate::security::project::resolve_canonical_project_trust_provenance(trust, cwd)
                    .trusted_root()
                    .map(std::path::Path::to_path_buf)
            })
        });
        let origin = accounting_origin_for_root(
            root.as_deref(),
            self.inventory.as_ref().map(|rows| rows.as_slice()),
        );
        self.origin.reobserve().map_err(|_| {
            MezError::forbidden("external request producer changed during origin capture")
        })?;
        let origin = self.store.begin_external_request(
            &self.owner,
            &self.harness,
            &self.marker,
            origin.project_id(),
            self.now,
        )?;
        self.published
            .set(origin.clone())
            .map_err(|_| MezError::conflict("external request origin already published"))?;
        Ok(origin)
    }
}

impl RuntimeSessionService {
    /// Reports metadata reservations without exposing private producer handles.
    #[cfg(test)]
    pub(crate) fn external_usage_begin_pending_for_tests(&self) -> usize {
        self.control.external_usage_begins().pending.len()
    }
    /// Reserves a native producer's original request boundary.
    /// Payload PIDs/projects/owners are not admitted; legacy, initialized,
    /// unrelated and unavailable-native-source callers cannot allocate markers.
    pub(crate) fn prepare_external_usage_begin(
        &mut self,
        request: &JsonRpcRequest,
        connection: &ControlConnectionState,
    ) -> Result<ExternalUsageBeginWork> {
        self.require_live()?;
        if request.method != "agent/external/usage-begin" || connection.initialized() {
            return Err(MezError::forbidden(
                "external request begin requires capability-only ingress",
            ));
        }
        let Some(AuthenticatedPeer::UnixUser { uid }) = connection.authenticated_peer() else {
            return Err(MezError::forbidden(
                "external request begin requires Unix transport",
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
        let enrollment = binding.enrollment.as_ref().ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotImplemented,
                "external request begin requires an ordinary producer",
            )
        })?;
        enrollment.authorize(connection)?;
        if !enrollment.usage_owner_is_durable {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotImplemented,
                "external request native namespace unavailable",
            ));
        }
        let marker = super::external_agents::text(&params, "request_id", 64)?;
        if marker.len() != 64
            || !marker
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(MezError::invalid_args(
                "external request requires an opaque canonical marker",
            ));
        }
        let mut work = ExternalUsageBeginWork {
            admission: 0,
            request_id: request.id.clone(),
            accounting_incarnation: self.control.external_usage_begins().incarnation.clone(),
            digest,
            generation: binding.generation,
            pane_id: binding.pane_id.clone(),
            process: binding.process.clone(),
            owner: binding.accounting_owner.clone(),
            harness: binding.harness.clone(),
            session,
            marker,
            origin: connection
                .unix_origin()
                .cloned()
                .ok_or_else(|| MezError::forbidden("external request native origin unavailable"))?,
            trust: self
                .integration
                .project_trust_store()
                .filter(|store| store.records().take(4097).count() <= 4096)
                .cloned()
                .map(Arc::new),
            inventory: self
                .persistence
                .accounting_projects()
                .filter(|rows| rows.len() <= 4096)
                .map(|rows| Arc::new(rows.to_vec())),
            store: self.persistence.cloned_token_usage_store().ok_or_else(|| {
                MezError::invalid_state("durable external request store unavailable")
            })?,
            now: current_unix_seconds(),
            deadline: Instant::now() + Duration::from_secs(2),
            published: Arc::new(OnceLock::new()),
        };
        let begins = self.control.external_usage_begins_mut();
        if begins.pending.len() >= 32 {
            return Err(MezError::new(
                crate::error::MezErrorKind::RateLimited,
                "external request admission capacity unavailable",
            ));
        }
        begins.next = begins
            .next
            .checked_add(1)
            .ok_or_else(|| MezError::invalid_state("external request admission exhausted"))?;
        work.admission = begins.next;
        begins.pending.insert(work.admission);
        Ok(work)
    }

    /// Consumes metadata admission exactly once and rechecks original native
    /// authority before returning readiness. Durable markers grant no rights
    /// after source/root/deadline changes, even when the worker already wrote one.
    pub(crate) fn complete_external_usage_begin(
        &mut self,
        work: ExternalUsageBeginWork,
        result: Result<ExternalRequestOrigin>,
        connection: &ControlConnectionState,
    ) -> String {
        let outcome = (|| {
            if !Arc::ptr_eq(
                &work.accounting_incarnation,
                &self.control.external_usage_begins().incarnation,
            ) {
                return Err(MezError::conflict(
                    "external readiness reservation owner changed",
                ));
            }
            if !self
                .control
                .external_usage_begins_mut()
                .pending
                .remove(&work.admission)
            {
                return Err(MezError::conflict(
                    "external request admission already settled",
                ));
            }
            self.require_live()?;
            if Instant::now() >= work.deadline
                || self
                    .pane_process_identity(&work.pane_id)
                    .ok()
                    .is_none_or(|current| !current.same_incarnation(&work.process))
            {
                return Err(MezError::conflict(
                    "external request root or deadline changed",
                ));
            }
            self.reconcile_external_agent_registrations();
            let binding = self
                .control
                .external_agents()
                .bindings
                .get(&work.digest)
                .filter(|binding| {
                    !binding.retired
                        && binding.uid == work.origin.uid()
                        && binding.pane_id == work.pane_id
                        && binding.process.same_incarnation(&work.process)
                        && binding.generation == work.generation
                        && binding.accounting_owner == work.owner
                        && binding.harness == work.harness
                        && binding.registration.as_ref().is_some_and(|registration| {
                            registration.external_session_id == work.session
                        })
                })
                .ok_or_else(|| {
                    MezError::forbidden("external request original producer unavailable")
                })?;
            binding
                .enrollment
                .as_ref()
                .filter(|enrollment| enrollment.usage_owner_is_durable)
                .ok_or_else(|| MezError::forbidden("external request native producer unavailable"))?
                .authorize(connection)?;
            let origin = result?;
            if origin.request_id != work.marker || work.published.get() != Some(&origin) {
                return Err(MezError::conflict(
                    "external request marker result mismatch or unpublished",
                ));
            }
            self.control.external_usage_begins_mut().cache_origin(
                work.owner.clone(),
                work.marker.clone(),
                origin.clone(),
            );
            Ok(origin)
        })();
        match outcome {
            Ok(origin) => format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{}}}",
                work.request_id,
                serde_json::json!({"protocol":"external-request/1","accepted":true,"request_id":origin.request_id,"admitted_at":origin.admitted_at,"source_witness":request_source_witness(&work.owner),"origin":if origin.project.is_some(){"project"}else{"unattributed"},"controls":[]})
            ),
            Err(error) => crate::runtime::runtime_json_rpc_error(
                &work.request_id,
                error.kind(),
                error.message(),
            ),
        }
    }
}
/// Public non-authoritative continuity selector; only the qualified source may
/// use markers. A fresh server-owned namespace changes this witness even if
/// coarse request timestamps coincide after storage loss or source replacement.
pub(super) fn request_source_witness(owner: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(format!("external-request-source/1\0{owner}").as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
