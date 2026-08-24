//! Principal-scoped session selection and transactional remote provisioning.
//!
//! This owner composes durable leases, the live registry, and
//! `SessionSupervisor`. Remote creation reserves a pending lease before process
//! allocation, activates only after the runtime is ready, and records a safe
//! terminal failure when startup fails. Resolution filters authority before
//! matching targets so unauthorized callers cannot infer lease existence.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
use crate::runtime::socket_path_for_name;
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

/// Shared host owner for local discovery and remote durable routing.
#[derive(Debug, Clone)]
pub(crate) struct HostSessionRouter {
    config: HostSessionRouterConfig,
    supervisor: SessionSupervisor,
    registry: SessionRegistry,
    leases: RemoteSessionLeaseRepository,
    creation_lock: Arc<tokio::sync::Mutex<()>>,
}

impl HostSessionRouter {
    pub(crate) fn new(config: HostSessionRouterConfig) -> Self {
        let registry = SessionRegistry::new(config.runtime_root.clone(), config.owner_uid);
        let leases = RemoteSessionLeaseRepository::new(default_remote_session_lease_directory(
            &config.config_root,
        ));
        Self {
            config,
            supervisor: SessionSupervisor::default(),
            registry,
            leases,
            creation_lock: Arc::new(tokio::sync::Mutex::new(())),
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
        let leases = self.leases.list()?;
        let mut report = HostReconciliationReport {
            boot_generation: self.leases.boot_generation()?,
            pending: 0,
            active: 0,
            recoverable: 0,
            released: 0,
            revoked: 0,
            failed: 0,
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

    /// Creates one fresh local supervised session and publishes compatibility discovery.
    pub(crate) async fn create_local(
        &self,
        name: Option<String>,
        size: Size,
    ) -> Result<SessionRecord> {
        let _creation = self.creation_lock.lock().await;
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
    pub(crate) fn resolve_local(
        &self,
        target: Option<&str>,
        requested_role: &str,
    ) -> Result<SessionRecord> {
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
        let now = current_unix_seconds()?;
        let session_id = next_session_id()?;
        let lease_id = format!("lease-{}", session_id.trim_start_matches('$'));
        let fingerprint = creation_fingerprint(request.name.as_deref(), request.size);
        let reservation = self.leases.reserve_pending_with_limits(
            LeaseReservationRequest {
                lease_id,
                session_id: session_id.clone(),
                owner_principal_id: principal.trust_record_id.clone(),
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
                let _creation = self.creation_lock.lock().await;
                let lease = self.leases.get(&lease.lease_id)?.ok_or_else(|| {
                    MezError::new(MezErrorKind::NotFound, "remote session was not found")
                })?;
                match lease.state {
                    RemoteSessionLeaseState::Active => {
                        let runtime = self.supervisor.lookup(&lease.session_id)?;
                        Ok(RemoteSessionBinding { lease, runtime })
                    }
                    RemoteSessionLeaseState::Recoverable => self.recover_lease_locked(lease).await,
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
        if updated.is_err() {
            let _ = snapshots.delete_async(&snapshot.id).await;
        }
        updated
    }

    /// Explicitly restores one recoverable lease or reports an already-live lease.
    pub(crate) async fn recover_lease(&self, target: &str) -> Result<RemoteSessionBinding> {
        let _creation = self.creation_lock.lock().await;
        let lease = self.get_lease(target)?;
        match lease.state {
            RemoteSessionLeaseState::Active => {
                let runtime = self.supervisor.lookup(&lease.session_id)?;
                Ok(RemoteSessionBinding { lease, runtime })
            }
            RemoteSessionLeaseState::Recoverable => self.recover_lease_locked(lease).await,
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
            return Ok(lease);
        }
        if lease.state == RemoteSessionLeaseState::Revoked {
            return Err(MezError::forbidden(
                "revoked remote session lease cannot be released",
            ));
        }
        self.stop_live_lease_if_requested(&lease, terminate).await?;
        self.leases.release(
            &lease.lease_id,
            lease.boot_generation,
            lease.lease_generation,
            current_unix_seconds()?,
        )
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
            return Ok(lease);
        }
        if lease.state == RemoteSessionLeaseState::Released {
            return Err(MezError::forbidden(
                "released remote session lease cannot be revoked",
            ));
        }
        self.stop_live_lease_if_requested(&lease, terminate).await?;
        self.leases.revoke(
            &lease.lease_id,
            lease.boot_generation,
            lease.lease_generation,
            current_unix_seconds()?,
            reason,
        )
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
        let mut deleted_snapshot_ids = Vec::new();
        let mut retained_snapshot_ids = Vec::new();
        if apply {
            let remaining = self.leases.list()?;
            let snapshots = SnapshotRepository::new(self.config.config_root.join("layouts"));
            for snapshot_id in &preview.checkpoint_snapshot_ids {
                let still_referenced = remaining.iter().any(|lease| {
                    lease
                        .checkpoint
                        .as_ref()
                        .is_some_and(|checkpoint| checkpoint.snapshot_id == *snapshot_id)
                });
                if still_referenced || !snapshots.delete_async(snapshot_id).await.unwrap_or(false) {
                    retained_snapshot_ids.push(snapshot_id.clone());
                } else {
                    deleted_snapshot_ids.push(snapshot_id.clone());
                }
            }
        }
        Ok(HostLeaseGarbageCollectionReport {
            preview,
            applied: apply,
            deleted_snapshot_ids,
            retained_snapshot_ids,
        })
    }

    async fn stop_live_lease_if_requested(
        &self,
        lease: &RemoteSessionLease,
        terminate: bool,
    ) -> Result<()> {
        if lease.state != RemoteSessionLeaseState::Active {
            return Ok(());
        }
        if !terminate {
            return Err(MezError::conflict(
                "active remote session lease requires explicit termination",
            ));
        }
        self.supervisor.stop(&lease.session_id, true).await
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
    ) -> Result<RemoteSessionBinding> {
        let recovery = async {
            self.ensure_global_session_capacity().await?;
            let checkpoint = lease.checkpoint.as_ref().ok_or_else(|| {
                MezError::invalid_state("recoverable remote session has no checkpoint")
            })?;
            let snapshots = SnapshotRepository::new(self.config.config_root.join("layouts"));
            let manifest = snapshots.inspect_async(&checkpoint.snapshot_id).await?;
            if manifest.state.version != checkpoint.snapshot_version {
                return Err(MezError::invalid_state(
                    "remote session checkpoint manifest version does not match its lease",
                ));
            }
            if manifest.state.session_id != lease.session_id {
                return Err(MezError::invalid_state(
                    "remote session checkpoint belongs to a different session",
                ));
            }
            if !manifest.state.restorable {
                return Err(MezError::invalid_state(
                    "remote session checkpoint is not restorable",
                ));
            }
            let payload = snapshots
                .inspect_payload_async(&checkpoint.snapshot_id)
                .await?;
            let restored = snapshots
                .restore_session_from_payload_async(
                    &checkpoint.snapshot_id,
                    &payload,
                    self.config.shell.clone(),
                )
                .await?;
            if restored.session.id.to_string() != lease.session_id {
                return Err(MezError::invalid_state(
                    "restored checkpoint produced a different session identity",
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
                .await?;
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
        .await;
        match recovery {
            Ok(binding) => Ok(binding),
            Err(error) => {
                let failure = recovery_failure("failed", &error);
                match self.leases.mark_failed(
                    &lease.lease_id,
                    lease.boot_generation,
                    lease.lease_generation,
                    current_unix_seconds().unwrap_or(lease.updated_at_unix_seconds),
                    failure,
                ) {
                    Ok(_) => Err(error),
                    Err(fence_error) => Err(fence_error),
                }
            }
        }
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
        let session_id = session.id.to_string();
        let numeric_id = session_id
            .strip_prefix('$')
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| MezError::invalid_state("routed session id is invalid"))?;
        let socket_path = socket_path_for_name(
            &self.config.runtime_root,
            &format!("session-{numeric_id:016x}.sock"),
        )?;
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
