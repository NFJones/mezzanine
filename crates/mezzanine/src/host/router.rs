//! Principal-scoped session selection and transactional remote provisioning.
//!
//! This owner composes durable leases, the live registry, and
//! `SessionSupervisor`. Remote creation reserves a pending lease before process
//! allocation, activates only after the runtime is ready, and records a safe
//! terminal failure when startup fails. Resolution filters authority before
//! matching targets so unauthorized callers cannot infer lease existence.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mez_core::ids::SessionId;
use mez_mux::layout::Size;
use mez_mux::session::Session;
use sha2::{Digest, Sha256};

use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
use crate::error::{MezError, MezErrorKind, Result};
use crate::host::session::{
    SessionFactoryRequest, SessionRuntimeConfig, SessionRuntimeHandle, SessionRuntimeLimits,
    SessionRuntimeStartup, SessionSocketPublication, SessionSupervisor, SessionSupervisorSnapshot,
    SessionSupervisorState,
};
use crate::host::shell::ResolvedShell;
use crate::runtime::hosted_session_socket_path;
use crate::security::remote::{RemotePrincipal, RemoteSessionAttachScope};
use crate::storage::lease::{
    LeaseCheckpointReference, LeaseGarbageCollectionPolicy, LeaseGarbageCollectionPreview,
    LeaseReservation, LeaseReservationRequest, RemoteSessionLease, RemoteSessionLeaseRepository,
    RemoteSessionLeaseState, default_remote_session_lease_directory,
};
use crate::storage::registry::{SessionRecord, SessionRegistry, resolve_session_record_target};
use crate::storage::snapshot::SnapshotRepository;

static NEXT_ROUTED_SESSION_ID: AtomicU64 = AtomicU64::new(1);
const REMOTE_REPLAY_WAIT: Duration = Duration::from_secs(5);

/// Construction inputs shared by local and remote supervised sessions.
#[derive(Debug, Clone)]
pub(crate) struct HostSessionRouterConfig {
    pub(crate) runtime_root: std::path::PathBuf,
    pub(crate) owner_uid: u32,
    pub(crate) config_root: std::path::PathBuf,
    pub(crate) config_layers: Vec<ConfigLayer>,
    pub(crate) shell: ResolvedShell,
    pub(crate) max_sessions: usize,
    pub(crate) max_live_sessions: usize,
}

/// Shared admission lifecycle for every local and remote host front door.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum HostAdmissionState {
    Serving = 0,
    Draining = 1,
    Stopped = 2,
}

impl HostAdmissionState {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Serving,
            1 => Self::Draining,
            _ => Self::Stopped,
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Serving => "serving",
            Self::Draining => "draining",
            Self::Stopped => "stopped",
        }
    }
}

/// Inputs normalized before one remote create transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteSessionCreateRequest {
    pub(crate) name: Option<String>,
    pub(crate) idempotency_key: String,
    pub(crate) size: Size,
}

/// Durable and live binding selected for one authenticated remote connection.
#[derive(Debug, Clone)]
pub(crate) struct RemoteSessionBinding {
    pub(crate) lease: RemoteSessionLease,
    pub(crate) runtime: SessionRuntimeHandle,
}

/// Secret-free durable state summary returned by host reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostReconciliationReport {
    pub(crate) boot_generation: u64,
    pub(crate) pending: usize,
    pub(crate) active: usize,
    pub(crate) recoverable: usize,
    pub(crate) released: usize,
    pub(crate) revoked: usize,
    pub(crate) failed: usize,
    pub(crate) snapshot_cleanup_pending: usize,
    pub(crate) pruned_registry_records: usize,
}

/// Result of one safe lease garbage-collection preview or application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostLeaseGarbageCollectionReport {
    pub(crate) preview: LeaseGarbageCollectionPreview,
    pub(crate) applied: bool,
    pub(crate) deleted_snapshot_ids: Vec<String>,
    pub(crate) retained_snapshot_ids: Vec<String>,
}

/// Result of one reference-checked snapshot cleanup reconciliation pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostSnapshotCleanupReport {
    pub(crate) deleted_snapshot_ids: Vec<String>,
    pub(crate) retained_snapshot_ids: Vec<String>,
}

/// Shared host owner for local discovery and remote durable routing.
#[derive(Debug, Clone)]
pub(crate) struct HostSessionRouter {
    config: HostSessionRouterConfig,
    supervisor: SessionSupervisor,
    registry: SessionRegistry,
    leases: RemoteSessionLeaseRepository,
    creation_lock: Arc<tokio::sync::Mutex<()>>,
    admission_state: Arc<AtomicU8>,
}

impl HostSessionRouter {
    pub(crate) fn new(config: HostSessionRouterConfig) -> Self {
        let registry = SessionRegistry::new(config.runtime_root.clone(), config.owner_uid);
        let leases = RemoteSessionLeaseRepository::new(default_remote_session_lease_directory(
            &config.config_root,
        ));
        let completion_leases = leases.clone();
        let supervisor = SessionSupervisor::with_runtime_completion_handler(move |completion| {
            reconcile_runtime_completion(&completion_leases, completion)
        });
        Self {
            config,
            supervisor,
            registry,
            leases,
            creation_lock: Arc::new(tokio::sync::Mutex::new(())),
            admission_state: Arc::new(AtomicU8::new(HostAdmissionState::Serving as u8)),
        }
    }

    /// Waits for in-flight serialized admission to settle, then fences all
    /// later create, attach, and recovery operations across cloned routers.
    pub(crate) async fn begin_draining(&self) -> Result<()> {
        self.start_draining()?;
        let _creation = self.creation_lock.lock().await;
        Ok(())
    }

    /// Fences new admissions immediately. Callers then await `begin_draining`
    /// while continuing to poll already admitted work to completion.
    pub(crate) fn start_draining(&self) -> Result<()> {
        loop {
            match self.admission_state() {
                HostAdmissionState::Serving => {
                    if self
                        .admission_state
                        .compare_exchange(
                            HostAdmissionState::Serving as u8,
                            HostAdmissionState::Draining as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return Ok(());
                    }
                }
                HostAdmissionState::Draining => return Ok(()),
                HostAdmissionState::Stopped => {
                    return Err(MezError::conflict(
                        "host session admission has already stopped",
                    ));
                }
            }
        }
    }

    /// Marks the completed terminal lifecycle after all supervised runtimes
    /// have settled.
    pub(crate) fn mark_stopped(&self) -> Result<()> {
        match self.admission_state() {
            HostAdmissionState::Draining | HostAdmissionState::Stopped => {
                self.admission_state
                    .store(HostAdmissionState::Stopped as u8, Ordering::Release);
                Ok(())
            }
            HostAdmissionState::Serving => Err(MezError::invalid_state(
                "host session admission must drain before stopping",
            )),
        }
    }

    pub(crate) fn admission_state(&self) -> HostAdmissionState {
        HostAdmissionState::from_u8(self.admission_state.load(Ordering::Acquire))
    }

    fn require_serving(&self) -> Result<()> {
        match self.admission_state() {
            HostAdmissionState::Serving => Ok(()),
            HostAdmissionState::Draining => Err(MezError::conflict(
                "host is draining and is not accepting session operations",
            )),
            HostAdmissionState::Stopped => Err(MezError::conflict(
                "host has stopped accepting session operations",
            )),
        }
    }

    /// Advances the durable boot generation before either host listener starts.
    pub(crate) fn reconcile_startup(&self) -> Result<HostReconciliationReport> {
        self.leases
            .advance_boot_generation(current_unix_seconds()?)?;
        self.reconcile()
    }

    /// Prunes stale live discovery and reports current durable lease states.
    pub(crate) fn reconcile(&self) -> Result<HostReconciliationReport> {
        let pruned_registry_records = self.registry.prune_stale()?;
        self.reconcile_active_leases_without_runtimes()?;
        let leases = self.leases.list()?;
        let mut report = HostReconciliationReport {
            boot_generation: self.leases.boot_generation()?,
            pending: 0,
            active: 0,
            recoverable: 0,
            released: 0,
            revoked: 0,
            failed: 0,
            snapshot_cleanup_pending: self.leases.snapshot_cleanup_candidates()?.len(),
            pruned_registry_records,
        };
        for lease in leases {
            match lease.state {
                RemoteSessionLeaseState::Pending => report.pending += 1,
                RemoteSessionLeaseState::Active => report.active += 1,
                RemoteSessionLeaseState::Recoverable => report.recoverable += 1,
                RemoteSessionLeaseState::Released => report.released += 1,
                RemoteSessionLeaseState::Revoked => report.revoked += 1,
                RemoteSessionLeaseState::Failed => report.failed += 1,
            }
        }
        Ok(report)
    }

    fn reconcile_active_leases_without_runtimes(&self) -> Result<()> {
        for lease in self
            .leases
            .list()?
            .into_iter()
            .filter(|lease| lease.state == RemoteSessionLeaseState::Active)
        {
            if !self.supervisor.contains(&lease.session_id)? {
                reconcile_active_lease_after_runtime_exit(
                    &self.leases,
                    lease,
                    "supervised runtime was absent during host reconciliation".to_string(),
                )?;
            }
        }
        Ok(())
    }

    /// Creates one fresh local supervised session and publishes compatibility discovery.
    pub(crate) async fn create_local(
        &self,
        name: Option<String>,
        size: Size,
    ) -> Result<SessionRecord> {
        let _creation = self.creation_lock.lock().await;
        self.require_serving()?;
        self.create_local_locked(name, size).await
    }

    /// Resolves the current primary-attachable local session or creates one
    /// while holding the same synchronization boundary used by fresh creation.
    pub(crate) async fn resolve_or_create_local(&self, size: Size) -> Result<SessionRecord> {
        let _creation = self.creation_lock.lock().await;
        self.require_serving()?;
        match self.resolve_local_locked(None, "primary") {
            Ok(record) => Ok(record),
            Err(error) if error.kind() == MezErrorKind::NotFound => {
                self.create_local_locked(None, size).await
            }
            Err(error) => Err(error),
        }
    }

    async fn create_local_locked(&self, name: Option<String>, size: Size) -> Result<SessionRecord> {
        self.ensure_global_session_capacity().await?;
        let now = current_unix_seconds()?;
        let session_id = next_session_id()?;
        self.start_session(session_id.clone(), name, size, now)
            .await?;
        self.registry
            .list()?
            .into_iter()
            .find(|record| record.session_id == session_id)
            .ok_or_else(|| MezError::invalid_state("created session was not published"))
    }

    /// Resolves one live local session without creating a replacement.
    pub(crate) async fn resolve_local(
        &self,
        target: Option<&str>,
        requested_role: &str,
    ) -> Result<SessionRecord> {
        let _creation = self.creation_lock.lock().await;
        self.require_serving()?;
        self.resolve_local_locked(target, requested_role)
    }

    fn resolve_local_locked(
        &self,
        target: Option<&str>,
        requested_role: &str,
    ) -> Result<SessionRecord> {
        self.require_serving()?;
        let _ = self.registry.prune_stale()?;
        let records = self.registry.list()?;
        let record = match target {
            Some(target) => resolve_session_record_target(&records, target),
            None if requested_role == "observer" => records.first(),
            None => records.iter().find(|record| record.accepts_primary),
        };
        record.cloned().ok_or_else(|| {
            MezError::new(
                MezErrorKind::NotFound,
                if target.is_some() {
                    "requested session was not found"
                } else {
                    "no attachable session is available"
                },
            )
        })
    }

    /// Reserves, starts, and activates one principal-owned remote session.
    pub(crate) async fn create_remote(
        &self,
        principal: &RemotePrincipal,
        request: RemoteSessionCreateRequest,
    ) -> Result<RemoteSessionBinding> {
        let authority = principal.host_routing;
        if !authority.session_create {
            return Err(MezError::forbidden(
                "remote principal is not permitted to create sessions",
            ));
        }
        if authority.max_active_leases == 0 || authority.max_live_sessions == 0 {
            return Err(MezError::forbidden(
                "remote principal has no session provisioning quota",
            ));
        }
        validate_session_name(request.name.as_deref())?;
        let _creation = self.creation_lock.lock().await;
        self.require_serving()?;
        let now = current_unix_seconds()?;
        let session_id = next_session_id()?;
        let lease_id = format!("lease-{}", session_id.trim_start_matches('$'));
        let fingerprint = creation_fingerprint(request.name.as_deref(), request.size);
        let reservation = self.leases.reserve_pending_with_limits(
            LeaseReservationRequest {
                lease_id,
                session_id: session_id.clone(),
                owner_principal_id: principal.trust_record_id.clone(),
                owner_live_session_limit: authority.max_live_sessions,
                name: request.name.clone(),
                default_for_owner: false,
                idempotency_key: request.idempotency_key,
                creation_fingerprint: fingerprint,
                now_unix_seconds: now,
            },
            authority.max_active_leases,
            authority.max_live_sessions,
            self.config.max_sessions,
            self.config.max_live_sessions,
        )?;
        match reservation {
            LeaseReservation::Replay(lease) => self.resolve_replayed_create(lease).await,
            LeaseReservation::Created(lease) => {
                if let Err(error) = self.ensure_global_session_capacity().await {
                    let _ = self.leases.mark_failed(
                        &lease.lease_id,
                        lease.boot_generation,
                        lease.lease_generation,
                        current_unix_seconds().unwrap_or(now),
                        "host session capacity was exhausted".to_string(),
                    );
                    return Err(error);
                }
                let started = self
                    .start_session(
                        lease.session_id.clone(),
                        lease.name.clone(),
                        request.size,
                        now,
                    )
                    .await;
                let runtime = match started {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = self.leases.mark_failed(
                            &lease.lease_id,
                            lease.boot_generation,
                            lease.lease_generation,
                            current_unix_seconds().unwrap_or(now),
                            "remote session runtime startup failed".to_string(),
                        );
                        return Err(error);
                    }
                };
                match self.leases.activate(
                    &lease.lease_id,
                    lease.boot_generation,
                    lease.lease_generation,
                    current_unix_seconds()?,
                ) {
                    Ok(lease) => Ok(RemoteSessionBinding { lease, runtime }),
                    Err(error) => {
                        let _ = self.supervisor.stop(&lease.session_id, true).await;
                        Err(error)
                    }
                }
            }
        }
    }

    /// Resolves an explicit target or deterministic default, lazily restoring
    /// one authorized recoverable lease from its validated checkpoint.
    pub(crate) async fn resolve_remote(
        &self,
        principal: &RemotePrincipal,
        target_json: Option<&str>,
    ) -> Result<RemoteSessionBinding> {
        let _creation = self.creation_lock.lock().await;
        self.require_serving()?;
        let mut visible = self.visible_leases(principal)?;
        visible.retain(|lease| {
            matches!(
                lease.state,
                RemoteSessionLeaseState::Active | RemoteSessionLeaseState::Recoverable
            )
        });
        visible.sort_by(|left, right| {
            left.created_at_unix_seconds
                .cmp(&right.created_at_unix_seconds)
                .then_with(|| left.lease_id.cmp(&right.lease_id))
        });
        let lease = match target_json {
            None => visible.first(),
            Some(target_json) => {
                let target: serde_json::Value =
                    serde_json::from_str(target_json).map_err(|error| {
                        MezError::invalid_args(format!("remote session target is invalid: {error}"))
                    })?;
                let object = target.as_object().ok_or_else(|| {
                    MezError::invalid_args("remote session target must be an object")
                })?;
                visible.iter().find(|lease| {
                    object
                        .get("session_id")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|value| value == lease.session_id)
                        || object
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|value| lease.name.as_deref() == Some(value))
                })
            }
        }
        .cloned()
        .ok_or_else(|| MezError::new(MezErrorKind::NotFound, "remote session was not found"))?;
        match lease.state {
            RemoteSessionLeaseState::Active => {
                let runtime = self.supervisor.lookup(&lease.session_id)?;
                Ok(RemoteSessionBinding { lease, runtime })
            }
            RemoteSessionLeaseState::Recoverable => {
                self.require_serving()?;
                let lease = self.leases.get(&lease.lease_id)?.ok_or_else(|| {
                    MezError::new(MezErrorKind::NotFound, "remote session was not found")
                })?;
                match lease.state {
                    RemoteSessionLeaseState::Active => {
                        let runtime = self.supervisor.lookup(&lease.session_id)?;
                        Ok(RemoteSessionBinding { lease, runtime })
                    }
                    RemoteSessionLeaseState::Recoverable => {
                        self.recover_lease_locked(
                            lease,
                            Some(principal.host_routing.max_live_sessions),
                        )
                        .await
                    }
                    RemoteSessionLeaseState::Released | RemoteSessionLeaseState::Revoked => Err(
                        MezError::new(MezErrorKind::NotFound, "remote session was not found"),
                    ),
                    _ => Err(MezError::invalid_state(
                        "remote session is not currently recoverable",
                    )),
                }
            }
            _ => unreachable!("remote selection retained only active or recoverable leases"),
        }
    }

    /// Lists only leases visible to a principal with explicit list authority.
    pub(crate) fn list_remote(
        &self,
        principal: &RemotePrincipal,
    ) -> Result<Vec<RemoteSessionLease>> {
        if !principal.host_routing.session_list {
            return Err(MezError::forbidden(
                "remote principal is not permitted to list sessions",
            ));
        }
        self.visible_leases(principal)
    }

    pub(crate) async fn snapshots(&self) -> Result<Vec<SessionSupervisorSnapshot>> {
        self.supervisor.snapshots().await
    }

    pub(crate) async fn shutdown_all(&self, force: bool, timeout: Duration) -> Result<()> {
        self.supervisor.shutdown_all(force, timeout).await
    }

    pub(crate) fn registry(&self) -> &SessionRegistry {
        &self.registry
    }

    /// Requires a fresh checkpoint for every currently active durable lease.
    ///
    /// Periodic maintenance remains best-effort, but graceful host shutdown
    /// must fail before runtime teardown when any active lease cannot commit a
    /// new recovery point.
    pub(crate) async fn checkpoint_active_leases_strict(&self) -> Result<usize> {
        let lease_ids = self
            .leases
            .list()?
            .into_iter()
            .filter(|lease| lease.state == RemoteSessionLeaseState::Active)
            .map(|lease| lease.lease_id)
            .collect::<Vec<_>>();
        let mut checkpointed = 0usize;
        let mut failed = Vec::new();
        for lease_id in lease_ids {
            match self.checkpoint_lease(&lease_id).await {
                Ok(_) => checkpointed = checkpointed.saturating_add(1),
                Err(_) => failed.push(lease_id),
            }
        }
        if failed.is_empty() {
            Ok(checkpointed)
        } else {
            Err(MezError::invalid_state(format!(
                "graceful host shutdown could not commit checkpoints for active leases: {}",
                failed.join(", ")
            )))
        }
    }

    /// Lists durable leases for local administration with optional filters.
    pub(crate) fn list_leases(
        &self,
        state: Option<RemoteSessionLeaseState>,
        owner: Option<&str>,
        include_terminal: bool,
    ) -> Result<Vec<RemoteSessionLease>> {
        let include_terminal =
            include_terminal || state.is_some_and(RemoteSessionLeaseState::is_garbage_collectable);
        Ok(self
            .leases
            .list()?
            .into_iter()
            .filter(|lease| state.is_none_or(|state| lease.state == state))
            .filter(|lease| owner.is_none_or(|owner| lease.owner_principal_id == owner))
            .filter(|lease| include_terminal || !lease.state.is_garbage_collectable())
            .collect())
    }

    /// Resolves one lease by lease id, session id, or exact name.
    pub(crate) fn get_lease(&self, target: &str) -> Result<RemoteSessionLease> {
        resolve_lease_target(self.leases.list()?, target)
    }

    /// Captures one actor-consistent checkpoint and generation-fences its lease reference.
    pub(crate) async fn checkpoint_lease(&self, target: &str) -> Result<RemoteSessionLease> {
        let _creation = self.creation_lock.lock().await;
        let lease = self.get_lease(target)?;
        if lease.state != RemoteSessionLeaseState::Active {
            return Err(MezError::invalid_state(
                "only an active remote session lease can be checkpointed",
            ));
        }
        let runtime = self.supervisor.lookup(&lease.session_id)?;
        let snapshot_id = format!(
            "lease-checkpoint-{}-{}-{}",
            lease.session_id.trim_start_matches('$'),
            lease.boot_generation,
            lease.lease_generation
        );
        let snapshots = SnapshotRepository::new(self.config.config_root.join("layouts"));
        let snapshot = runtime
            .actor()
            .create_host_checkpoint(
                snapshots.clone(),
                snapshot_id,
                Some(format!("lease checkpoint {}", lease.lease_id)),
            )
            .await?;
        let now = current_unix_seconds()?;
        let updated = self.leases.update_checkpoint(
            &lease.lease_id,
            lease.boot_generation,
            lease.lease_generation,
            LeaseCheckpointReference {
                snapshot_id: snapshot.id.clone(),
                snapshot_version: snapshot.version,
                session_id: lease.session_id,
                recorded_at_unix_seconds: now,
            },
            now,
        );
        match updated {
            Ok(updated) => {
                let _ = self.reconcile_snapshot_cleanup_locked().await;
                Ok(updated)
            }
            Err(error) => {
                let _ = snapshots.delete_async(&snapshot.id).await;
                Err(error)
            }
        }
    }

    /// Explicitly restores one recoverable lease or reports an already-live lease.
    pub(crate) async fn recover_lease(&self, target: &str) -> Result<RemoteSessionBinding> {
        let _creation = self.creation_lock.lock().await;
        self.require_serving()?;
        let lease = self.get_lease(target)?;
        match lease.state {
            RemoteSessionLeaseState::Active => {
                let runtime = self.supervisor.lookup(&lease.session_id)?;
                Ok(RemoteSessionBinding { lease, runtime })
            }
            RemoteSessionLeaseState::Recoverable => {
                self.recover_lease_locked(lease, None).await
            }
            RemoteSessionLeaseState::Released | RemoteSessionLeaseState::Revoked => Err(
                MezError::forbidden("remote session lease cannot be recovered"),
            ),
            _ => Err(MezError::invalid_state(
                "remote session lease is not recoverable",
            )),
        }
    }

    /// Releases a durable reservation, requiring explicit termination when live.
    pub(crate) async fn release_lease(
        &self,
        target: &str,
        terminate: bool,
    ) -> Result<RemoteSessionLease> {
        let _creation = self.creation_lock.lock().await;
        let lease = self.get_lease(target)?;
        if lease.state == RemoteSessionLeaseState::Released {
            self.stop_terminal_lease_runtime_if_requested(&lease, terminate)
                .await?;
            return Ok(lease);
        }
        if lease.state == RemoteSessionLeaseState::Revoked {
            return Err(MezError::forbidden(
                "revoked remote session lease cannot be released",
            ));
        }
        require_active_lease_termination(&lease, terminate)?;
        let released = self.leases.release(
            &lease.lease_id,
            lease.boot_generation,
            lease.lease_generation,
            current_unix_seconds()?,
        )?;
        self.stop_terminal_lease_runtime_if_requested(&released, terminate)
            .await?;
        Ok(released)
    }

    /// Revokes future attachment and recovery without revoking device trust.
    pub(crate) async fn revoke_lease(
        &self,
        target: &str,
        reason: Option<String>,
        terminate: bool,
    ) -> Result<RemoteSessionLease> {
        let _creation = self.creation_lock.lock().await;
        let lease = self.get_lease(target)?;
        if lease.state == RemoteSessionLeaseState::Revoked {
            self.stop_terminal_lease_runtime_if_requested(&lease, terminate)
                .await?;
            return Ok(lease);
        }
        if lease.state == RemoteSessionLeaseState::Released {
            return Err(MezError::forbidden(
                "released remote session lease cannot be revoked",
            ));
        }
        require_active_lease_termination(&lease, terminate)?;
        let revoked = self.leases.revoke(
            &lease.lease_id,
            lease.boot_generation,
            lease.lease_generation,
            current_unix_seconds()?,
            reason,
        )?;
        self.stop_terminal_lease_runtime_if_requested(&revoked, terminate)
            .await?;
        Ok(revoked)
    }

    /// Previews or applies terminal lease garbage collection and checkpoint cleanup.
    pub(crate) async fn garbage_collect_leases(
        &self,
        policy: LeaseGarbageCollectionPolicy,
        apply: bool,
    ) -> Result<HostLeaseGarbageCollectionReport> {
        let _creation = self.creation_lock.lock().await;
        let preview = if apply {
            self.leases.apply_gc(policy)?
        } else {
            self.leases.preview_gc(policy)?
        };
        let cleanup = if apply {
            self.reconcile_snapshot_cleanup_locked().await?
        } else {
            HostSnapshotCleanupReport {
                deleted_snapshot_ids: Vec::new(),
                retained_snapshot_ids: Vec::new(),
            }
        };
        Ok(HostLeaseGarbageCollectionReport {
            preview,
            applied: apply,
            deleted_snapshot_ids: cleanup.deleted_snapshot_ids,
            retained_snapshot_ids: cleanup.retained_snapshot_ids,
        })
    }

    /// Retries durable snapshot cleanup intents without blocking unrelated
    /// host work when artifact deletion encounters a transient error.
    pub(crate) async fn reconcile_snapshot_cleanup(&self) -> Result<HostSnapshotCleanupReport> {
        let _creation = self.creation_lock.lock().await;
        self.reconcile_snapshot_cleanup_locked().await
    }

    async fn reconcile_snapshot_cleanup_locked(&self) -> Result<HostSnapshotCleanupReport> {
        let candidates = self.leases.snapshot_cleanup_candidates()?;
        let snapshots = SnapshotRepository::new(self.config.config_root.join("layouts"));
        let mut deleted_snapshot_ids = Vec::new();
        let mut retained_snapshot_ids = Vec::new();
        for snapshot_id in candidates {
            if self.leases.snapshot_is_referenced(&snapshot_id)? {
                retained_snapshot_ids.push(snapshot_id);
                continue;
            }
            match snapshots.delete_async(&snapshot_id).await {
                Ok(_) if self.leases.acknowledge_snapshot_cleanup(&snapshot_id)? => {
                    deleted_snapshot_ids.push(snapshot_id);
                }
                Ok(_) | Err(_) => retained_snapshot_ids.push(snapshot_id),
            }
        }
        Ok(HostSnapshotCleanupReport {
            deleted_snapshot_ids,
            retained_snapshot_ids,
        })
    }

    async fn stop_terminal_lease_runtime_if_requested(
        &self,
        lease: &RemoteSessionLease,
        terminate: bool,
    ) -> Result<()> {
        if !terminate {
            return Ok(());
        }
        match self.supervisor.stop(&lease.session_id, true).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == MezErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn resolve_replayed_create(
        &self,
        mut lease: RemoteSessionLease,
    ) -> Result<RemoteSessionBinding> {
        let deadline = tokio::time::Instant::now() + REMOTE_REPLAY_WAIT;
        loop {
            match lease.state {
                RemoteSessionLeaseState::Active => {
                    let runtime = self.supervisor.lookup(&lease.session_id)?;
                    return Ok(RemoteSessionBinding { lease, runtime });
                }
                RemoteSessionLeaseState::Pending if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    lease = self
                        .leases
                        .get(&lease.lease_id)?
                        .ok_or_else(|| MezError::invalid_state("replayed lease disappeared"))?;
                }
                RemoteSessionLeaseState::Failed => {
                    return Err(MezError::invalid_state(
                        lease
                            .failure
                            .unwrap_or_else(|| "remote session creation failed".to_string()),
                    ));
                }
                RemoteSessionLeaseState::Recoverable => {
                    return Err(MezError::invalid_state(
                        "remote session requires recovery before attachment",
                    ));
                }
                RemoteSessionLeaseState::Released | RemoteSessionLeaseState::Revoked => {
                    return Err(MezError::forbidden(
                        "remote session lease is no longer attachable",
                    ));
                }
                RemoteSessionLeaseState::Pending => {
                    return Err(MezError::invalid_state(
                        "remote session creation is still pending",
                    ));
                }
            }
        }
    }

    fn visible_leases(&self, principal: &RemotePrincipal) -> Result<Vec<RemoteSessionLease>> {
        let leases = self.leases.list()?;
        Ok(leases
            .into_iter()
            .filter(|lease| match principal.host_routing.session_attach_scope {
                RemoteSessionAttachScope::Own | RemoteSessionAttachScope::Shared => {
                    lease.owner_principal_id == principal.trust_record_id
                }
                RemoteSessionAttachScope::All => true,
            })
            .collect())
    }

    async fn ensure_global_session_capacity(&self) -> Result<()> {
        let _ = self.registry.prune_stale()?;
        if self.registry.list()?.len() >= self.config.max_sessions {
            return Err(MezError::conflict("host session limit has been reached"));
        }
        let live = self
            .supervisor
            .snapshots()
            .await?
            .into_iter()
            .filter(|snapshot| {
                matches!(
                    snapshot.state,
                    SessionSupervisorState::Starting
                        | SessionSupervisorState::Running
                        | SessionSupervisorState::Stopping
                )
            })
            .count();
        if live >= self.config.max_live_sessions {
            return Err(MezError::conflict(
                "host live session limit has been reached",
            ));
        }
        Ok(())
    }

    async fn recover_lease_locked(
        &self,
        lease: RemoteSessionLease,
        current_owner_live_limit: Option<usize>,
    ) -> Result<RemoteSessionBinding> {
        let recovery = async {
            self.ensure_recovery_capacity(&lease, current_owner_live_limit)
                .await
                .map_err(|error| (error, RecoveryFailureDisposition::Retryable))?;
            let checkpoint = lease.checkpoint.as_ref().ok_or_else(|| {
                (
                    MezError::invalid_state("recoverable remote session has no checkpoint"),
                    RecoveryFailureDisposition::Terminal,
                )
            })?;
            let snapshots = SnapshotRepository::new(self.config.config_root.join("layouts"));
            let manifest = snapshots
                .inspect_async(&checkpoint.snapshot_id)
                .await
                .map_err(recovery_artifact_failure)?;
            if manifest.state.version != checkpoint.snapshot_version {
                return Err((
                    MezError::invalid_state(
                        "remote session checkpoint manifest version does not match its lease",
                    ),
                    RecoveryFailureDisposition::Terminal,
                ));
            }
            if manifest.state.session_id != lease.session_id {
                return Err((
                    MezError::invalid_state(
                        "remote session checkpoint belongs to a different session",
                    ),
                    RecoveryFailureDisposition::Terminal,
                ));
            }
            if !manifest.state.restorable {
                return Err((
                    MezError::invalid_state("remote session checkpoint is not restorable"),
                    RecoveryFailureDisposition::Terminal,
                ));
            }
            let payload = snapshots
                .inspect_payload_async(&checkpoint.snapshot_id)
                .await
                .map_err(recovery_artifact_failure)?;
            let restored = snapshots
                .restore_session_from_payload_async(
                    &checkpoint.snapshot_id,
                    &payload,
                    self.config.shell.clone(),
                )
                .await
                .map_err(|error| (error, RecoveryFailureDisposition::Terminal))?;
            if restored.session.id.to_string() != lease.session_id {
                return Err((
                    MezError::invalid_state(
                        "restored checkpoint produced a different session identity",
                    ),
                    RecoveryFailureDisposition::Terminal,
                ));
            }
            let runtime = self
                .start_prepared_session(
                    restored.session,
                    lease.created_at_unix_seconds,
                    SessionRuntimeStartup::RestoredSnapshot {
                        payload: Box::new(payload),
                        restart_command: None,
                    },
                )
                .await
                .map_err(|error| (error, RecoveryFailureDisposition::Retryable))?;
            match self.leases.activate(
                &lease.lease_id,
                lease.boot_generation,
                lease.lease_generation,
                current_unix_seconds()
                    .map_err(|error| (error, RecoveryFailureDisposition::Retryable))?,
            ) {
                Ok(lease) => Ok(RemoteSessionBinding { lease, runtime }),
                Err(error) => {
                    let _ = self.supervisor.stop(&lease.session_id, true).await;
                    Err((error, RecoveryFailureDisposition::Retryable))
                }
            }
        }
        .await;
        match recovery {
            Ok(binding) => Ok(binding),
            Err((error, disposition)) => {
                let now = current_unix_seconds().unwrap_or(lease.updated_at_unix_seconds);
                let persisted = match disposition {
                    RecoveryFailureDisposition::Retryable => {
                        self.leases.record_retryable_recovery_failure(
                            &lease.lease_id,
                            lease.boot_generation,
                            lease.lease_generation,
                            now,
                            recovery_failure("retryable", &error),
                        )
                    }
                    RecoveryFailureDisposition::Terminal => self.leases.mark_failed(
                        &lease.lease_id,
                        lease.boot_generation,
                        lease.lease_generation,
                        now,
                        recovery_failure("terminal", &error),
                    ),
                };
                match persisted {
                    Ok(_) => Err(error),
                    Err(fence_error) => Err(fence_error),
                }
            }
        }
    }

    async fn ensure_recovery_capacity(
        &self,
        lease: &RemoteSessionLease,
        current_owner_live_limit: Option<usize>,
    ) -> Result<()> {
        self.ensure_global_session_capacity().await?;
        let owner_limit = current_owner_live_limit
            .map(|limit| limit.min(lease.owner_live_session_limit))
            .unwrap_or(lease.owner_live_session_limit);
        let owner_live = self
            .leases
            .list()?
            .into_iter()
            .filter(|candidate| {
                candidate.owner_principal_id == lease.owner_principal_id
                    && matches!(
                        candidate.state,
                        RemoteSessionLeaseState::Pending | RemoteSessionLeaseState::Active
                    )
            })
            .count();
        if owner_limit == 0 || owner_live >= owner_limit {
            return Err(MezError::conflict(
                "remote principal live-session limit has been reached",
            ));
        }
        Ok(())
    }

    async fn start_session(
        &self,
        session_id: String,
        name: Option<String>,
        size: Size,
        created_at_unix_seconds: u64,
    ) -> Result<SessionRuntimeHandle> {
        validate_session_name(name.as_deref())?;
        let numeric_id = session_id
            .strip_prefix('$')
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| MezError::invalid_state("routed session id is invalid"))?;
        let mut session = Session::new_default(self.config.shell.clone(), size);
        session.id = SessionId::new('$', numeric_id);
        if let Some(name) = name {
            session.name = name;
        }
        self.start_prepared_session(
            session,
            created_at_unix_seconds,
            SessionRuntimeStartup::Initial {
                explicit_command: None,
            },
        )
        .await
    }

    async fn start_prepared_session(
        &self,
        session: Session,
        created_at_unix_seconds: u64,
        startup: SessionRuntimeStartup,
    ) -> Result<SessionRuntimeHandle> {
        let socket_path = hosted_session_socket_path(&self.config.runtime_root, &session.id)?;
        let mut config_layers = self.config.config_layers.clone();
        config_layers.push(ConfigLayer {
            name: "persistent-host-session-transport".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[transport.iroh]\nenabled = false\n".to_string(),
        });
        self.supervisor
            .start(SessionFactoryRequest {
                session,
                owner_uid: self.config.owner_uid,
                created_at_unix_seconds,
                config: SessionRuntimeConfig {
                    layers: config_layers,
                    root: self.config.config_root.clone(),
                },
                sockets: SessionSocketPublication {
                    control_path: socket_path,
                    publish_control: true,
                    message_path: None,
                    event_path: None,
                    publish_registry: true,
                },
                limits: SessionRuntimeLimits::default(),
                startup,
            })
            .await
    }
}

fn recovery_failure(context: &str, error: &MezError) -> String {
    let mut failure = format!("remote session recovery {context}: {}", error.message());
    if failure.len() > 1024 {
        failure.truncate(1024);
        while !failure.is_char_boundary(failure.len()) {
            failure.pop();
        }
    }
    failure
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryFailureDisposition {
    Retryable,
    Terminal,
}

fn recovery_artifact_failure(error: MezError) -> (MezError, RecoveryFailureDisposition) {
    let disposition = match error.kind() {
        MezErrorKind::Io | MezErrorKind::Conflict => RecoveryFailureDisposition::Retryable,
        _ => RecoveryFailureDisposition::Terminal,
    };
    (error, disposition)
}

fn reconcile_runtime_completion(
    leases: &RemoteSessionLeaseRepository,
    completion: &SessionSupervisorSnapshot,
) -> Result<()> {
    let Some(lease) = leases.get_by_session(&completion.session_id)? else {
        return Ok(());
    };
    if lease.state != RemoteSessionLeaseState::Active {
        return Ok(());
    }
    let diagnostic = completion
        .failure
        .clone()
        .unwrap_or_else(|| match completion.runtime_state {
            Some(state) => format!("supervised runtime completed in state {state:?}"),
            None => "supervised runtime completed without a lifecycle state".to_string(),
        });
    reconcile_active_lease_after_runtime_exit(leases, lease, diagnostic)
}

fn reconcile_active_lease_after_runtime_exit(
    leases: &RemoteSessionLeaseRepository,
    lease: RemoteSessionLease,
    diagnostic: String,
) -> Result<()> {
    let now = current_unix_seconds().unwrap_or(lease.updated_at_unix_seconds);
    if lease.checkpoint.is_some() {
        leases.mark_recoverable_after_runtime_exit(
            &lease.lease_id,
            lease.boot_generation,
            lease.lease_generation,
            now,
            diagnostic,
        )?;
    } else {
        leases.mark_failed(
            &lease.lease_id,
            lease.boot_generation,
            lease.lease_generation,
            now,
            format!("supervised runtime completed without a committed checkpoint: {diagnostic}"),
        )?;
    }
    Ok(())
}

fn require_active_lease_termination(lease: &RemoteSessionLease, terminate: bool) -> Result<()> {
    if lease.state == RemoteSessionLeaseState::Active && !terminate {
        return Err(MezError::conflict(
            "active remote session lease requires explicit termination",
        ));
    }
    Ok(())
}

fn resolve_lease_target(
    leases: Vec<RemoteSessionLease>,
    target: &str,
) -> Result<RemoteSessionLease> {
    if target.trim().is_empty() {
        return Err(MezError::invalid_args(
            "remote session lease target is required",
        ));
    }
    let mut matches = leases.into_iter().filter(|lease| {
        lease.lease_id == target
            || lease.session_id == target
            || lease.name.as_deref() == Some(target)
    });
    let lease = matches.next().ok_or_else(|| {
        MezError::new(MezErrorKind::NotFound, "remote session lease was not found")
    })?;
    if matches.next().is_some() {
        return Err(MezError::conflict(
            "remote session lease target is ambiguous",
        ));
    }
    Ok(lease)
}

fn validate_session_name(name: Option<&str>) -> Result<()> {
    if name.is_some_and(|name| {
        name.trim().is_empty() || name.len() > 256 || name.chars().any(char::is_control)
    }) {
        return Err(MezError::invalid_args(
            "session name must be printable text up to 256 bytes",
        ));
    }
    Ok(())
}

fn creation_fingerprint(name: Option<&str>, size: Size) -> String {
    let mut digest = Sha256::new();
    digest.update(b"mezzanine-remote-session-create-v1\0");
    digest.update(name.unwrap_or_default().as_bytes());
    digest.update(b"\0");
    digest.update(size.columns.to_le_bytes());
    digest.update(size.rows.to_le_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn next_session_id() -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| MezError::invalid_state("system clock is before the Unix epoch"))?;
    let counter = NEXT_ROUTED_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    let value = now
        .as_secs()
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::from(now.subsec_nanos()))
        ^ (u64::from(std::process::id()) << 32)
        ^ counter;
    Ok(SessionId::new('$', value.max(1)).to_string())
}

fn current_unix_seconds() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| MezError::invalid_state("system clock is before the Unix epoch"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use crate::config::{ConfigFormat, ConfigScope};
    use crate::control::RequestedRole;
    use crate::host::shell::{ResolvedShell, ShellSource};
    use crate::security::remote::{
        RemoteHostRoutingAuthority, RemoteRoleCeiling, RemoteSessionAttachScope,
    };
    use crate::storage::lease::LeaseCheckpointReference;

    use super::*;

    /// Runtime completion reconciles the matching durable lease after the
    /// supervisor accepts that exact generation: checkpointed sessions remain
    /// recoverable, while sessions without a recovery point become failed.
    #[tokio::test(flavor = "current_thread")]
    async fn supervised_runtime_exit_reconciles_active_durable_leases() {
        let root = test_root("runtime-exit-lease");
        let router = HostSessionRouter::new(test_config(&root));
        let principal = test_principal("exit-owner", 2);
        let checkpointed = router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("checkpointed-exit".to_string()),
                    idempotency_key: "checkpointed-exit".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();
        let checkpointed_lease = router
            .checkpoint_lease(&checkpointed.lease.lease_id)
            .await
            .unwrap();
        let uncheckpointed = router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("uncheckpointed-exit".to_string()),
                    idempotency_key: "uncheckpointed-exit".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();

        checkpointed
            .runtime
            .force_shutdown("test checkpointed runtime exit".to_string())
            .await
            .unwrap();
        uncheckpointed
            .runtime
            .force_shutdown("test uncheckpointed runtime exit".to_string())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if router
                    .supervisor
                    .lookup(&checkpointed.lease.session_id)
                    .is_err()
                    && router
                        .supervisor
                        .lookup(&uncheckpointed.lease.session_id)
                        .is_err()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let recovered = router.get_lease(&checkpointed_lease.lease_id).unwrap();
        assert_eq!(recovered.state, RemoteSessionLeaseState::Recoverable);
        assert!(recovered.checkpoint.is_some());
        let failed = router.get_lease(&uncheckpointed.lease.lease_id).unwrap();
        assert_eq!(failed.state, RemoteSessionLeaseState::Failed);
        assert!(
            failed
                .failure
                .as_deref()
                .is_some_and(|failure| failure.contains("runtime completed"))
        );

        let _ = fs::remove_dir_all(root);
    }

    /// Concurrent bare-CLI resolution must select or create one shared local
    /// session atomically, while explicit fresh creation remains non-deduplicated.
    #[tokio::test(flavor = "current_thread")]
    async fn local_resolve_or_create_is_atomic_while_create_remains_fresh() {
        let root = test_root("local-roc");
        let router = HostSessionRouter::new(test_config(&root));
        let mut requests = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let router = router.clone();
            requests.spawn(async move {
                router
                    .resolve_or_create_local(Size::new(80, 24).unwrap())
                    .await
            });
        }

        let mut selected_session_ids = Vec::new();
        while let Some(result) = requests.join_next().await {
            selected_session_ids.push(result.unwrap().unwrap().session_id);
        }
        assert_eq!(selected_session_ids.len(), 8);
        assert!(
            selected_session_ids
                .iter()
                .all(|session_id| session_id == &selected_session_ids[0])
        );
        assert_eq!(router.registry().list().unwrap().len(), 1);

        let first_fresh = router
            .create_local(Some("fresh-one".to_string()), Size::new(80, 24).unwrap())
            .await
            .unwrap();
        let second_fresh = router
            .create_local(Some("fresh-two".to_string()), Size::new(80, 24).unwrap())
            .await
            .unwrap();
        assert_ne!(first_fresh.session_id, second_fresh.session_id);
        assert_eq!(router.registry().list().unwrap().len(), 3);

        router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        let _ = fs::remove_dir_all(root);
    }

    /// Hosted session publication uses a compact, collision-free socket name
    /// so the longest cross-platform runtime root that can fit that name still
    /// creates, discovers, and cleans up distinct sessions successfully.
    #[tokio::test(flavor = "current_thread")]
    async fn hosted_session_sockets_fit_the_cross_platform_path_budget() {
        let root = test_root("socket-budget");
        let mut component = format!("mez-hsp-{}-{:x}", std::process::id(), rand::random::<u64>());
        component.push_str(&"x".repeat(75usize.saturating_sub(component.len())));
        assert_eq!(component.len(), 75);
        let runtime_root = PathBuf::from("/tmp").join(component);
        fs::create_dir_all(&runtime_root).unwrap();
        fs::set_permissions(&runtime_root, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(runtime_root.as_os_str().as_encoded_bytes().len(), 80);

        let mut config = test_config(&root);
        config.runtime_root = runtime_root.clone();
        let router = HostSessionRouter::new(config);
        let first = router
            .create_local(Some("first".to_string()), Size::new(80, 24).unwrap())
            .await
            .unwrap();
        let second = router
            .create_local(Some("second".to_string()), Size::new(80, 24).unwrap())
            .await
            .unwrap();
        assert_ne!(first.socket_path, second.socket_path);
        assert!(first.socket_path.starts_with(&runtime_root));
        assert!(second.socket_path.starts_with(&runtime_root));
        assert_eq!(router.registry().list().unwrap().len(), 2);

        router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        assert!(!first.socket_path.exists());
        assert!(!second.socket_path.exists());
        let _ = fs::remove_dir_all(runtime_root);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_create_is_idempotent_quota_bounded_and_owner_scoped() {
        let root = test_root("remote-create");
        let router = HostSessionRouter::new(test_config(&root));
        let principal = test_principal("owner", 2);
        let first = router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("first".to_string()),
                    idempotency_key: "create-first".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();
        let replay = router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("first".to_string()),
                    idempotency_key: "create-first".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();
        assert_eq!(replay.lease.lease_id, first.lease.lease_id);
        assert_eq!(replay.runtime.session_id(), first.runtime.session_id());

        let conflict = router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("changed".to_string()),
                    idempotency_key: "create-first".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(conflict.kind(), MezErrorKind::Conflict);

        let second = router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("second".to_string()),
                    idempotency_key: "create-second".to_string(),
                    size: Size::new(100, 30).unwrap(),
                },
            )
            .await
            .unwrap();
        assert_ne!(second.lease.lease_id, first.lease.lease_id);
        let quota = router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("third".to_string()),
                    idempotency_key: "create-third".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(quota.kind(), MezErrorKind::Conflict);
        assert_eq!(router.list_remote(&principal).unwrap().len(), 2);

        let explicit = router
            .resolve_remote(
                &principal,
                Some(&serde_json::json!({"name":"second"}).to_string()),
            )
            .await
            .unwrap();
        assert_eq!(explicit.lease.lease_id, second.lease.lease_id);
        let default = router.resolve_remote(&principal, None).await.unwrap();
        assert_eq!(default.lease.lease_id, first.lease.lease_id);

        let other = test_principal("other", 2);
        let denied = router
            .resolve_remote(
                &other,
                Some(&serde_json::json!({"session_id":first.lease.session_id}).to_string()),
            )
            .await
            .unwrap_err();
        assert_eq!(denied.kind(), MezErrorKind::NotFound);

        router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        let _ = fs::remove_dir_all(root);
    }

    /// Startup reconciliation advances the durable boot generation, fences
    /// callbacks from the prior host, and lets concurrent authorized attaches
    /// restore exactly one fresh runtime from the retained checkpoint.
    #[tokio::test(flavor = "current_thread")]
    async fn restart_reconciliation_lazily_restores_once_and_fences_stale_callbacks() {
        let root = test_root("restart-recovery");
        let config = test_config(&root);
        let principal = test_principal("owner", 2);
        let first_router = HostSessionRouter::new(config.clone());
        let created = first_router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("recover-me".to_string()),
                    idempotency_key: "create-recoverable".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();
        let mut checkpoint_session =
            Session::new_default(config.shell.clone(), Size::new(80, 24).unwrap());
        checkpoint_session.id = SessionId::parse('$', created.lease.session_id.clone()).unwrap();
        checkpoint_session.name = "recover-me".to_string();
        let snapshots = SnapshotRepository::new(config.config_root.join("layouts"));
        let snapshot = snapshots
            .create_from_session(
                "restart-checkpoint",
                Some("restart".to_string()),
                &checkpoint_session,
            )
            .unwrap();
        let checkpointed = first_router
            .leases
            .update_checkpoint(
                &created.lease.lease_id,
                created.lease.boot_generation,
                created.lease.lease_generation,
                LeaseCheckpointReference {
                    snapshot_id: snapshot.id,
                    snapshot_version: snapshot.version,
                    session_id: created.lease.session_id.clone(),
                    recorded_at_unix_seconds: current_unix_seconds().unwrap(),
                },
                current_unix_seconds().unwrap(),
            )
            .unwrap();
        first_router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        drop(created);
        drop(first_router);

        let recovered_router = HostSessionRouter::new(config);
        let report = recovered_router.reconcile_startup().unwrap();
        assert_eq!(report.boot_generation, 1);
        assert_eq!(report.recoverable, 1);
        assert_eq!(report.active, 0);
        let stale = recovered_router
            .leases
            .mark_failed(
                &checkpointed.lease_id,
                checkpointed.boot_generation,
                checkpointed.lease_generation,
                current_unix_seconds().unwrap(),
                "stale prior-host callback".to_string(),
            )
            .unwrap_err();
        assert_eq!(stale.kind(), MezErrorKind::Conflict);

        let first_attach = recovered_router.resolve_remote(&principal, None);
        let second_attach = recovered_router.resolve_remote(&principal, None);
        let (first_attach, second_attach) = tokio::join!(first_attach, second_attach);
        let first_attach = first_attach.unwrap();
        let second_attach = second_attach.unwrap();
        assert_eq!(first_attach.lease.lease_id, checkpointed.lease_id);
        assert_eq!(second_attach.lease.lease_id, checkpointed.lease_id);
        assert_eq!(
            first_attach.runtime.session_id(),
            second_attach.runtime.session_id()
        );
        assert_eq!(recovered_router.snapshots().await.unwrap().len(), 1);
        let active = recovered_router
            .leases
            .get(&checkpointed.lease_id)
            .unwrap()
            .unwrap();
        assert_eq!(active.state, RemoteSessionLeaseState::Active);
        assert_eq!(active.boot_generation, 1);

        recovered_router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        let _ = fs::remove_dir_all(root);
    }

    /// Owner live-session quota remains authoritative across restart, and a
    /// capacity-blocked recovery stays recoverable so it can succeed after the
    /// competing runtime exits.
    #[tokio::test(flavor = "current_thread")]
    async fn recovery_preserves_retryability_and_owner_live_quota() {
        let root = test_root("recovery-owner-quota");
        let config = test_config(&root);
        let mut principal = test_principal("quota-owner", 2);
        principal.host_routing.max_live_sessions = 1;
        let initial = HostSessionRouter::new(config.clone());
        let first = initial
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("recover-after-capacity".to_string()),
                    idempotency_key: "recover-after-capacity".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();
        let checkpointed = initial
            .checkpoint_lease(&first.lease.lease_id)
            .await
            .unwrap();
        initial
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        drop(first);
        drop(initial);

        let router = HostSessionRouter::new(config);
        assert_eq!(router.reconcile_startup().unwrap().recoverable, 1);
        let competing = router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("quota-occupant".to_string()),
                    idempotency_key: "quota-occupant".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();

        let capacity = router
            .recover_lease(&checkpointed.lease_id)
            .await
            .unwrap_err();
        assert_eq!(capacity.kind(), MezErrorKind::Conflict);
        let retryable = router.get_lease(&checkpointed.lease_id).unwrap();
        assert_eq!(retryable.state, RemoteSessionLeaseState::Recoverable);
        assert!(
            retryable
                .failure
                .as_deref()
                .is_some_and(|failure| failure.contains("retryable"))
        );

        competing
            .runtime
            .force_shutdown("free owner recovery quota".to_string())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if router
                    .supervisor
                    .lookup(&competing.lease.session_id)
                    .is_err()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let recovered = router.recover_lease(&checkpointed.lease_id).await.unwrap();
        assert_eq!(recovered.lease.state, RemoteSessionLeaseState::Active);

        router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        let _ = fs::remove_dir_all(root);
    }

    /// A transient snapshot I/O failure records retry diagnostics without
    /// consuming recoverability, and the same lease succeeds after storage is
    /// readable again.
    #[tokio::test(flavor = "current_thread")]
    async fn recovery_retries_after_transient_snapshot_io_failure() {
        let root = test_root("recovery-transient-io");
        let config = test_config(&root);
        let principal = test_principal("io-owner", 1);
        let initial = HostSessionRouter::new(config.clone());
        let created = initial
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("recover-after-io".to_string()),
                    idempotency_key: "recover-after-io".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();
        let checkpointed = initial
            .checkpoint_lease(&created.lease.lease_id)
            .await
            .unwrap();
        initial
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        drop(created);
        drop(initial);

        let checkpoint = checkpointed.checkpoint.as_ref().unwrap();
        let manifest_path = config
            .config_root
            .join("layouts")
            .join(format!("{}.manifest", checkpoint.snapshot_id));
        fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o000)).unwrap();

        let router = HostSessionRouter::new(config);
        assert_eq!(router.reconcile_startup().unwrap().recoverable, 1);
        let io_error = router
            .recover_lease(&checkpointed.lease_id)
            .await
            .unwrap_err();
        assert_eq!(io_error.kind(), MezErrorKind::Io);
        let retryable = router.get_lease(&checkpointed.lease_id).unwrap();
        assert_eq!(retryable.state, RemoteSessionLeaseState::Recoverable);
        assert!(
            retryable
                .failure
                .as_deref()
                .is_some_and(|failure| failure.contains("retryable"))
        );

        fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o600)).unwrap();
        let recovered = router.recover_lease(&checkpointed.lease_id).await.unwrap();
        assert_eq!(recovered.lease.state, RemoteSessionLeaseState::Active);

        router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        let _ = fs::remove_dir_all(root);
    }

    /// A missing checkpoint fails closed without allocating a replacement
    /// runtime, while a second restart deterministically retains the terminal
    /// failure and advances its generation fence.
    #[tokio::test(flavor = "current_thread")]
    async fn missing_checkpoint_fails_recovery_without_runtime_allocation() {
        let root = test_root("missing-checkpoint");
        let config = test_config(&root);
        let principal = test_principal("owner", 1);
        let first_router = HostSessionRouter::new(config.clone());
        let created = first_router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("missing".to_string()),
                    idempotency_key: "create-missing".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();
        let checkpointed = first_router
            .leases
            .update_checkpoint(
                &created.lease.lease_id,
                created.lease.boot_generation,
                created.lease.lease_generation,
                LeaseCheckpointReference {
                    snapshot_id: "absent-checkpoint".to_string(),
                    snapshot_version: 1,
                    session_id: created.lease.session_id.clone(),
                    recorded_at_unix_seconds: current_unix_seconds().unwrap(),
                },
                current_unix_seconds().unwrap(),
            )
            .unwrap();
        first_router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        drop(created);
        drop(first_router);

        let second_router = HostSessionRouter::new(config.clone());
        assert_eq!(second_router.reconcile_startup().unwrap().recoverable, 1);
        let error = second_router
            .resolve_remote(&principal, None)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), MezErrorKind::NotFound);
        assert!(second_router.snapshots().await.unwrap().is_empty());
        let failed = second_router
            .leases
            .get(&checkpointed.lease_id)
            .unwrap()
            .unwrap();
        assert_eq!(failed.state, RemoteSessionLeaseState::Failed);
        assert!(
            failed
                .failure
                .as_deref()
                .is_some_and(|failure| failure.contains("snapshot not found"))
        );
        drop(second_router);

        let third_router = HostSessionRouter::new(config);
        let report = third_router.reconcile_startup().unwrap();
        assert_eq!(report.boot_generation, 2);
        assert_eq!(report.failed, 1);
        assert_eq!(report.recoverable, 0);
        let failed_again = third_router
            .leases
            .get(&checkpointed.lease_id)
            .unwrap()
            .unwrap();
        assert_eq!(failed_again.boot_generation, 2);
        assert!(third_router.snapshots().await.unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    /// Release and revocation commit their authority fences even when the
    /// supervised runtime has already exited before teardown can be requested.
    #[tokio::test(flavor = "current_thread")]
    async fn lease_authority_transitions_commit_before_runtime_cleanup() {
        let root = test_root("authority-before-cleanup");
        let router = HostSessionRouter::new(test_config(&root));
        let principal = test_principal("owner", 2);
        let released_runtime = router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("release-first".to_string()),
                    idempotency_key: "release-first".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();
        let revoked_runtime = router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("revoke-first".to_string()),
                    idempotency_key: "revoke-first".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();

        released_runtime
            .runtime
            .force_shutdown("test runtime exited before release".to_string())
            .await
            .unwrap();
        revoked_runtime
            .runtime
            .force_shutdown("test runtime exited before revocation".to_string())
            .await
            .unwrap();

        let _ = router
            .release_lease(&released_runtime.lease.lease_id, true)
            .await;
        let _ = router
            .revoke_lease(
                &revoked_runtime.lease.lease_id,
                Some("operator revoked lease".to_string()),
                true,
            )
            .await;

        assert_eq!(
            router
                .get_lease(&released_runtime.lease.lease_id)
                .unwrap()
                .state,
            RemoteSessionLeaseState::Released
        );
        let revoked = router.get_lease(&revoked_runtime.lease.lease_id).unwrap();
        assert_eq!(revoked.state, RemoteSessionLeaseState::Revoked);
        assert_eq!(revoked.failure.as_deref(), Some("operator revoked lease"));

        router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        let _ = fs::remove_dir_all(root);
    }

    /// Local lease administration captures a live checkpoint, restores it
    /// after restart, requires explicit live termination, keeps release and
    /// revoke distinct, and garbage-collects only terminal records.
    #[tokio::test(flavor = "current_thread")]
    async fn lease_administration_is_generation_fenced_and_gc_safe() {
        let root = test_root("lease-administration");
        let config = test_config(&root);
        let principal = test_principal("owner", 2);
        let first_router = HostSessionRouter::new(config.clone());
        let created = first_router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("admin-one".to_string()),
                    idempotency_key: "create-admin-one".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            first_router.list_leases(None, None, false).unwrap().len(),
            1
        );
        assert_eq!(
            first_router.get_lease("admin-one").unwrap().lease_id,
            created.lease.lease_id
        );
        assert_eq!(
            first_router
                .get_lease(&created.lease.session_id)
                .unwrap()
                .lease_id,
            created.lease.lease_id
        );

        let checkpointed = first_router
            .checkpoint_lease(&created.lease.lease_id)
            .await
            .unwrap();
        let checkpoint = checkpointed.checkpoint.clone().unwrap();
        SnapshotRepository::new(config.config_root.join("layouts"))
            .inspect(&checkpoint.snapshot_id)
            .unwrap();
        first_router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        drop(created);
        drop(first_router);

        let router = HostSessionRouter::new(config.clone());
        assert_eq!(router.reconcile_startup().unwrap().recoverable, 1);
        let recovered = router
            .recover_lease(&checkpointed.session_id)
            .await
            .unwrap();
        assert_eq!(recovered.lease.state, RemoteSessionLeaseState::Active);
        let release_conflict = router
            .release_lease(&recovered.lease.lease_id, false)
            .await
            .unwrap_err();
        assert_eq!(release_conflict.kind(), MezErrorKind::Conflict);
        let released = router
            .release_lease(&recovered.lease.lease_id, true)
            .await
            .unwrap();
        assert_eq!(released.state, RemoteSessionLeaseState::Released);
        assert_eq!(
            router
                .release_lease(&released.lease_id, false)
                .await
                .unwrap()
                .lease_generation,
            released.lease_generation
        );
        assert!(router.list_leases(None, None, false).unwrap().is_empty());
        assert_eq!(
            router
                .list_leases(Some(RemoteSessionLeaseState::Released), None, true)
                .unwrap()
                .len(),
            1
        );

        let second = router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("admin-two".to_string()),
                    idempotency_key: "create-admin-two".to_string(),
                    size: Size::new(100, 30).unwrap(),
                },
            )
            .await
            .unwrap();
        let revoke_conflict = router
            .revoke_lease(&second.lease.lease_id, None, false)
            .await
            .unwrap_err();
        assert_eq!(revoke_conflict.kind(), MezErrorKind::Conflict);
        let revoked = router
            .revoke_lease(
                &second.lease.lease_id,
                Some("operator revoked lease".to_string()),
                true,
            )
            .await
            .unwrap();
        assert_eq!(revoked.state, RemoteSessionLeaseState::Revoked);
        assert_eq!(revoked.failure.as_deref(), Some("operator revoked lease"));
        assert_eq!(
            router
                .revoke_lease(&revoked.lease_id, None, false)
                .await
                .unwrap()
                .lease_generation,
            revoked.lease_generation
        );

        let policy = LeaseGarbageCollectionPolicy {
            released_before_unix_seconds: u64::MAX,
            revoked_before_unix_seconds: u64::MAX,
            failed_before_unix_seconds: u64::MAX,
        };
        let preview = router.garbage_collect_leases(policy, false).await.unwrap();
        assert!(!preview.applied);
        assert_eq!(preview.preview.lease_ids.len(), 2);
        assert_eq!(
            preview.preview.checkpoint_snapshot_ids,
            vec![checkpoint.snapshot_id.clone()]
        );
        let applied = router.garbage_collect_leases(policy, true).await.unwrap();
        assert!(applied.applied);
        assert_eq!(applied.deleted_snapshot_ids, vec![checkpoint.snapshot_id]);
        assert!(applied.retained_snapshot_ids.is_empty());
        assert!(router.list_leases(None, None, true).unwrap().is_empty());
        router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        let _ = fs::remove_dir_all(root);
    }

    /// A checkpoint replacement records failed artifact deletion durably and
    /// retries it after restart without removing the lease's new checkpoint.
    #[tokio::test(flavor = "current_thread")]
    async fn checkpoint_replacement_cleanup_retries_after_restart() {
        let root = test_root("cleanup-replace");
        let config = test_config(&root);
        let principal = test_principal("cleanup-owner", 1);
        let router = HostSessionRouter::new(config.clone());
        let created = router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("cleanup-replacement".to_string()),
                    idempotency_key: "cleanup-replacement-create".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();
        let first = router
            .checkpoint_lease(&created.lease.lease_id)
            .await
            .unwrap()
            .checkpoint
            .unwrap();
        let layouts = config.config_root.join("layouts");
        let first_payload = layouts.join(format!("{}.payload", first.snapshot_id));
        fs::remove_file(&first_payload).unwrap();
        fs::create_dir(&first_payload).unwrap();
        fs::write(first_payload.join("blocked"), b"cleanup retry\n").unwrap();
        fs::set_permissions(&first_payload, fs::Permissions::from_mode(0o500)).unwrap();

        let second = router
            .checkpoint_lease(&created.lease.lease_id)
            .await
            .unwrap()
            .checkpoint
            .unwrap();
        assert_ne!(second.snapshot_id, first.snapshot_id);
        assert_eq!(router.reconcile().unwrap().snapshot_cleanup_pending, 1);
        SnapshotRepository::new(layouts.clone())
            .inspect(&first.snapshot_id)
            .unwrap();
        SnapshotRepository::new(layouts.clone())
            .inspect(&second.snapshot_id)
            .unwrap();

        router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        drop(created);
        drop(router);
        fs::set_permissions(&first_payload, fs::Permissions::from_mode(0o700)).unwrap();

        let restarted = HostSessionRouter::new(config);
        assert_eq!(
            restarted
                .reconcile_startup()
                .unwrap()
                .snapshot_cleanup_pending,
            1
        );
        let cleanup = restarted.reconcile_snapshot_cleanup().await.unwrap();
        assert_eq!(
            cleanup.deleted_snapshot_ids,
            vec![first.snapshot_id.clone()]
        );
        assert!(cleanup.retained_snapshot_ids.is_empty());
        assert_eq!(restarted.reconcile().unwrap().snapshot_cleanup_pending, 0);
        assert!(
            SnapshotRepository::new(layouts.clone())
                .inspect(&first.snapshot_id)
                .is_err()
        );
        SnapshotRepository::new(layouts.clone())
            .inspect(&second.snapshot_id)
            .unwrap();
        let _ = fs::remove_dir_all(root);
    }

    /// Lease GC retains a durable cleanup intent across deletion failures and
    /// repeated GC calls, then reclaims the artifact once the failure clears.
    #[tokio::test(flavor = "current_thread")]
    async fn lease_gc_cleanup_failure_is_retryable_and_idempotent() {
        let root = test_root("lease-gc-cleanup");
        let config = test_config(&root);
        let principal = test_principal("gc-cleanup-owner", 1);
        let router = HostSessionRouter::new(config.clone());
        let created = router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("gc-cleanup".to_string()),
                    idempotency_key: "gc-cleanup-create".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();
        let checkpoint = router
            .checkpoint_lease(&created.lease.lease_id)
            .await
            .unwrap()
            .checkpoint
            .unwrap();
        router
            .release_lease(&created.lease.lease_id, true)
            .await
            .unwrap();
        let layouts = config.config_root.join("layouts");
        let payload = layouts.join(format!("{}.payload", checkpoint.snapshot_id));
        fs::remove_file(&payload).unwrap();
        fs::create_dir(&payload).unwrap();
        fs::write(payload.join("blocked"), b"cleanup retry\n").unwrap();
        fs::set_permissions(&payload, fs::Permissions::from_mode(0o500)).unwrap();
        let policy = LeaseGarbageCollectionPolicy {
            released_before_unix_seconds: u64::MAX,
            revoked_before_unix_seconds: u64::MAX,
            failed_before_unix_seconds: u64::MAX,
        };

        let first_gc = router.garbage_collect_leases(policy, true).await.unwrap();
        assert_eq!(first_gc.preview.lease_ids, vec![created.lease.lease_id]);
        assert!(first_gc.deleted_snapshot_ids.is_empty());
        assert_eq!(
            first_gc.retained_snapshot_ids,
            vec![checkpoint.snapshot_id.clone()]
        );
        assert!(router.list_leases(None, None, true).unwrap().is_empty());
        assert_eq!(router.reconcile().unwrap().snapshot_cleanup_pending, 1);

        let repeated = router.garbage_collect_leases(policy, true).await.unwrap();
        assert!(repeated.preview.lease_ids.is_empty());
        assert_eq!(
            repeated.retained_snapshot_ids,
            vec![checkpoint.snapshot_id.clone()]
        );
        fs::set_permissions(&payload, fs::Permissions::from_mode(0o700)).unwrap();
        let cleanup = router.reconcile_snapshot_cleanup().await.unwrap();
        assert_eq!(cleanup.deleted_snapshot_ids, vec![checkpoint.snapshot_id]);
        assert!(cleanup.retained_snapshot_ids.is_empty());
        assert_eq!(router.reconcile().unwrap().snapshot_cleanup_pending, 0);
        let _ = fs::remove_dir_all(root);
    }

    /// Entering the shared drain barrier fences every local and remote path
    /// that can admit or attach a session before shutdown enumerates runtimes.
    #[tokio::test(flavor = "current_thread")]
    async fn draining_fences_local_and_remote_session_admission() {
        let root = test_root("drain-admission");
        let router = HostSessionRouter::new(test_config(&root));
        let principal = test_principal("drain-owner", 3);
        let active = router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("drain-active".to_string()),
                    idempotency_key: "drain-active-create".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();
        let recoverable = router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("drain-recoverable".to_string()),
                    idempotency_key: "drain-recoverable-create".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();
        let recoverable_lease = router
            .checkpoint_lease(&recoverable.lease.lease_id)
            .await
            .unwrap();
        recoverable
            .runtime
            .force_shutdown("prepare drain recovery test".to_string())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if router
                    .get_lease(&recoverable_lease.lease_id)
                    .is_ok_and(|lease| lease.state == RemoteSessionLeaseState::Recoverable)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let snapshots_before_drain = router.snapshots().await.unwrap();
        let in_flight_admission = router.creation_lock.lock().await;
        let draining_router = router.clone();
        let drain_task = tokio::spawn(async move { draining_router.begin_draining().await });
        tokio::task::yield_now().await;
        assert_eq!(router.admission_state(), HostAdmissionState::Draining);
        assert!(!drain_task.is_finished());
        drop(in_flight_admission);
        drain_task.await.unwrap().unwrap();

        let local_create = router
            .create_local(Some("late-local".to_string()), Size::new(80, 24).unwrap())
            .await
            .unwrap_err();
        assert_eq!(local_create.kind(), MezErrorKind::Conflict);
        let local_resolve = router.resolve_local(None, "primary").await.unwrap_err();
        assert_eq!(local_resolve.kind(), MezErrorKind::Conflict);
        let remote_create = router
            .create_remote(
                &principal,
                RemoteSessionCreateRequest {
                    name: Some("late-remote".to_string()),
                    idempotency_key: "late-remote-create".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(remote_create.kind(), MezErrorKind::Conflict);
        let active_attach = router
            .resolve_remote(
                &principal,
                Some(&serde_json::json!({"lease_id":active.lease.lease_id}).to_string()),
            )
            .await
            .unwrap_err();
        assert_eq!(active_attach.kind(), MezErrorKind::Conflict);
        let recovery = router
            .recover_lease(&recoverable_lease.lease_id)
            .await
            .unwrap_err();
        assert_eq!(recovery.kind(), MezErrorKind::Conflict);
        assert_eq!(router.snapshots().await.unwrap(), snapshots_before_drain);
        assert_eq!(
            router.get_lease(&active.lease.lease_id).unwrap().state,
            RemoteSessionLeaseState::Active
        );
        assert_eq!(
            router.get_lease(&recoverable_lease.lease_id).unwrap().state,
            RemoteSessionLeaseState::Recoverable
        );

        router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        router.mark_stopped().unwrap();
        assert_eq!(router.admission_state(), HostAdmissionState::Stopped);
        let _ = fs::remove_dir_all(root);
    }

    fn test_principal(id: &str, max: usize) -> RemotePrincipal {
        RemotePrincipal {
            trust_record_id: id.to_string(),
            endpoint_id: format!("endpoint-{id}"),
            role_ceiling: RemoteRoleCeiling::Primary,
            host_routing: RemoteHostRoutingAuthority {
                session_create: true,
                session_list: true,
                session_attach_scope: RemoteSessionAttachScope::Own,
                max_active_leases: max,
                max_live_sessions: max,
                lease_lifetime_ceiling_seconds: None,
            },
            requested_role: RequestedRole::Primary,
        }
    }

    fn test_config(root: &std::path::Path) -> HostSessionRouterConfig {
        HostSessionRouterConfig {
            runtime_root: root.join("runtime"),
            owner_uid: crate::runtime::current_effective_uid(),
            config_root: root.join("config"),
            config_layers: vec![ConfigLayer {
                name: "router-test".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: "[agents]\nshell_mode = \"pane\"\n[permissions]\nsandbox = \"policy-only\"\n"
                    .to_string(),
            }],
            shell: ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
            max_sessions: 8,
            max_live_sessions: 8,
        }
    }

    fn test_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mez-host-router-{label}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        root
    }
}
