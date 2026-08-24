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
    LeaseReservation, LeaseReservationRequest, RemoteSessionLease, RemoteSessionLeaseRepository,
    RemoteSessionLeaseState, default_remote_session_lease_directory,
};
use crate::storage::registry::{SessionRecord, SessionRegistry, resolve_session_record_target};

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

    /// Resolves an explicit target or an existing deterministic default.
    pub(crate) fn resolve_remote(
        &self,
        principal: &RemotePrincipal,
        target_json: Option<&str>,
    ) -> Result<RemoteSessionBinding> {
        let mut visible = self.visible_leases(principal)?;
        visible.retain(|lease| lease.state == RemoteSessionLeaseState::Active);
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
        let runtime = self.supervisor.lookup(&lease.session_id)?;
        Ok(RemoteSessionBinding { lease, runtime })
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
        let socket_path = socket_path_for_name(
            &self.config.runtime_root,
            &format!("session-{numeric_id:016x}.sock"),
        )?;
        let mut session = Session::new_default(self.config.shell.clone(), size);
        session.id = SessionId::new('$', numeric_id);
        if let Some(name) = name {
            session.name = name;
        }
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
                startup: SessionRuntimeStartup::Initial {
                    explicit_command: None,
                },
            })
            .await
    }
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
            .unwrap();
        assert_eq!(explicit.lease.lease_id, second.lease.lease_id);
        let default = router.resolve_remote(&principal, None).unwrap();
        assert_eq!(default.lease.lease_id, first.lease.lease_id);

        let other = test_principal("other", 2);
        let denied = router
            .resolve_remote(
                &other,
                Some(&serde_json::json!({"session_id":first.lease.session_id}).to_string()),
            )
            .unwrap_err();
        assert_eq!(denied.kind(), MezErrorKind::NotFound);

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
