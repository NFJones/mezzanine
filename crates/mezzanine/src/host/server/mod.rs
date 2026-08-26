//! Persistent local host control plane and supervised session routing.
//!
//! The host owns one protected management socket and one exclusive process
//! lock above `SessionSupervisor`. Management requests create, resolve, list,
//! reconcile, and stop sessions; terminal traffic remains bound to each
//! selected session actor through its compatibility Unix control socket. The
//! live registry is discovery output only and is never treated as durable
//! lease state.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::future::Future;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::{StreamExt, stream::FuturesUnordered};
use mez_mux::layout::Size;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio_util::codec::Framed;

use crate::config::{ConfigLayer, ConfigPaths, load_runtime_config_layers_for_directory};
use crate::error::{MezError, MezErrorKind, Result};
use crate::host::administration::{
    HostAdministrationBegin, HostAdministrationJournal, HostAdministrationReplay, HostAuditLog,
};
use crate::host::iroh::HostIrohInvitationIssuer;
use crate::host::ownership::HostOwnershipGuard;
use crate::host::router::{
    HostDefaultSessionPolicy, HostRecoveryPolicy, HostSessionRouter, HostSessionRouterConfig,
    LocalSessionLaunchContext, local_launch_environment_key_allowed,
};
use crate::host::session::SessionSupervisorState;
use crate::host::shell::{ResolvedShell, resolve_shell};
use crate::protocol::framing::ProtocolFrameCodec;
use crate::runtime::{bind_control_socket, socket_path_for_name};
use crate::security::audit::{AuditActor, AuditLog, AuditRecord};
use crate::storage::registry::records_to_json;

const HOST_SOCKET_FILE_NAME: &str = "host.sock";
const HOST_CONTROL_MAX_CONTENT_LENGTH: usize = 1024 * 1024;
const HOST_CONTROL_CONNECTION_LIMIT: usize = 64;
const HOST_CONTROL_CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);

/// Construction inputs shared by every session created under one host.
#[derive(Debug, Clone)]
pub(crate) struct HostServerConfig {
    /// Private runtime directory containing host and compatibility sockets.
    pub(crate) runtime_root: PathBuf,
    /// Effective user allowed to use the local host socket.
    pub(crate) owner_uid: u32,
    /// Primary configuration root used by per-session stores.
    pub(crate) config_root: PathBuf,
    /// Active configuration layers cloned into new session runtimes.
    pub(crate) config_layers: Vec<ConfigLayer>,
    /// Resolved shell used for newly created sessions.
    pub(crate) shell: ResolvedShell,
    /// Maximum retained session records accepted by the host.
    pub(crate) max_sessions: usize,
    /// Maximum concurrently supervised sessions.
    pub(crate) max_live_sessions: usize,
    /// Bounded host shutdown interval.
    pub(crate) shutdown_timeout: Duration,
    /// Interval between best-effort checkpoints of active durable leases.
    pub(crate) checkpoint_interval: Duration,
    /// Automatic startup and attach recovery behavior.
    pub(crate) recovery_policy: HostRecoveryPolicy,
    /// Existing-session selection behavior for remote default intent.
    pub(crate) default_session_policy: HostDefaultSessionPolicy,
    /// Default finite lifetime for newly created leases; zero disables expiry.
    pub(crate) default_lease_lifetime_seconds: u64,
    /// Failed-lease retention before default garbage collection eligibility.
    pub(crate) failed_lease_retention_seconds: u64,
    /// Released/revoked lease retention before default garbage collection eligibility.
    pub(crate) released_lease_retention_seconds: u64,
    /// Live host-scoped Iroh invitation and trust administration, when enabled.
    pub(crate) iroh_invitation_issuer: Option<HostIrohInvitationIssuer>,
    /// Default and maximum active-lease grant for one remote principal.
    pub(crate) max_remote_leases: usize,
    /// Configured append-only audit writer for protected host operations.
    pub(crate) audit_log: Option<AuditLog>,
}

/// Ready local host with exclusive process and socket ownership.
#[derive(Debug)]
pub(crate) struct HostServer {
    config: HostServerConfig,
    listener: tokio::net::UnixListener,
    router: HostSessionRouter,
    audit_log: HostAuditLog,
    administration_journal: HostAdministrationJournal,
    administration_lock: tokio::sync::Mutex<()>,
    #[cfg(test)]
    fail_next_administration_completion_audit: std::sync::atomic::AtomicBool,
    socket_path: PathBuf,
    _ownership: HostOwnershipGuard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HostShutdownRequest {
    force: bool,
}

#[derive(Debug)]
struct HostConnectionResult {
    method: String,
    failure: Option<String>,
    shutdown: Option<HostShutdownRequest>,
}

fn record_host_maintenance_state(
    failures: &mut HashSet<&'static str>,
    operation: &'static str,
    failure: Option<String>,
) {
    match failure {
        Some(failure) if failures.insert(operation) => {
            eprintln!("mez host: maintenance operation {operation} degraded: {failure}");
        }
        None if failures.remove(operation) => {
            eprintln!("mez host: maintenance operation {operation} recovered");
        }
        Some(_) | None => {}
    }
}

#[derive(Debug)]
struct HostAdministrationExecution<'a> {
    idempotency_key: &'a str,
    request_fingerprint: &'a str,
}

impl HostServer {
    /// Acquires exclusive host ownership and binds the protected management socket.
    #[cfg(test)]
    pub(crate) fn bind(config: HostServerConfig) -> Result<Self> {
        let ownership = HostOwnershipGuard::acquire(&config.config_root, config.owner_uid)?;
        Self::bind_with_ownership(config, ownership)
    }

    /// Binds a host after an earlier durable-root ownership acquisition.
    ///
    /// The CLI uses this entry point so ownership precedes host-scoped Iroh
    /// identity and trust initialization as well as listener and router setup.
    pub(crate) fn bind_with_ownership(
        config: HostServerConfig,
        ownership: HostOwnershipGuard,
    ) -> Result<Self> {
        if config.max_sessions == 0 || config.max_live_sessions == 0 {
            return Err(MezError::invalid_args(
                "host session limits must be greater than zero",
            ));
        }
        ownership.validate_config_root(&config.config_root)?;
        crate::runtime::ensure_private_socket_directory(&config.runtime_root, config.owner_uid)?;
        let socket_path = host_socket_path(&config.runtime_root)?;
        let listener = bind_control_socket(&socket_path, config.owner_uid)?;
        listener.set_nonblocking(true)?;
        let listener = tokio::net::UnixListener::from_std(listener)?;
        let router = HostSessionRouter::new(HostSessionRouterConfig {
            runtime_root: config.runtime_root.clone(),
            owner_uid: config.owner_uid,
            config_root: config.config_root.clone(),
            config_layers: config.config_layers.clone(),
            shell: config.shell.clone(),
            max_sessions: config.max_sessions,
            max_live_sessions: config.max_live_sessions,
            recovery_policy: config.recovery_policy,
            default_session_policy: config.default_session_policy,
            default_lease_lifetime_seconds: config.default_lease_lifetime_seconds,
        });
        let _ = router.reconcile_startup()?;
        let audit_log = std::sync::Arc::new(std::sync::Mutex::new(config.audit_log.clone()));
        let administration_journal =
            HostAdministrationJournal::under_config_root(&config.config_root);
        Ok(Self {
            config,
            listener,
            router,
            audit_log,
            administration_journal,
            administration_lock: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            fail_next_administration_completion_audit: std::sync::atomic::AtomicBool::new(false),
            socket_path,
            _ownership: ownership,
        })
    }

    /// Returns the protected host management socket.
    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Returns the shared session router used by local and remote front doors.
    pub(crate) fn router(&self) -> HostSessionRouter {
        self.router.clone()
    }

    /// Returns the serialized host audit writer shared with the Iroh front door.
    pub(crate) fn audit_log_handle(&self) -> HostAuditLog {
        self.audit_log.clone()
    }

    #[cfg(test)]
    fn fail_next_administration_completion_audit(&self) {
        self.fail_next_administration_completion_audit
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Applies configured eager recovery before listeners start serving.
    pub(crate) async fn prepare_startup(&self) -> Result<usize> {
        let _ = self.router.reconcile_snapshot_cleanup().await?;
        self.router.apply_startup_recovery_policy().await
    }

    /// Serves local management requests until cancellation or `host/shutdown`.
    pub(crate) async fn serve<C>(&self, cancellation: C) -> Result<()>
    where
        C: Future<Output = ()>,
    {
        eprintln!(
            "mez host: listening for local clients on {}",
            self.socket_path.display()
        );
        tokio::pin!(cancellation);
        let mut connections = FuturesUnordered::new();
        let mut checkpoint_timer = tokio::time::interval_at(
            tokio::time::Instant::now() + self.config.checkpoint_interval,
            self.config.checkpoint_interval,
        );
        let mut authority_changes = self.router.authority_changes();
        let mut maintenance_failures = HashSet::new();
        let mut local_capacity_saturated = false;
        let shutdown = loop {
            let authority_delay = self.router.time_until_next_lease_expiry()?;
            let authority_maintenance = async move {
                match authority_delay {
                    Some(delay) => tokio::time::sleep(delay).await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                () = &mut cancellation => {
                    self.router.start_draining()?;
                    break HostShutdownRequest { force: false };
                }
                () = authority_maintenance => {
                    let expiry_failure = self.router.expire_due_leases().await.err().map(|error| error.to_string());
                    record_host_maintenance_state(&mut maintenance_failures, "lease expiry", expiry_failure);
                    let cleanup_failure = self.router.reconcile_terminal_runtime_cleanup().await.err().map(|error| error.to_string());
                    record_host_maintenance_state(&mut maintenance_failures, "terminal runtime cleanup", cleanup_failure);
                }
                changed = authority_changes.changed() => {
                    if changed.is_err() {
                        return Err(MezError::invalid_state(
                            "host lease authority scheduler stopped unexpectedly",
                        ));
                    }
                    let cleanup_failure = self.router.reconcile_terminal_runtime_cleanup().await.err().map(|error| error.to_string());
                    record_host_maintenance_state(&mut maintenance_failures, "terminal runtime cleanup", cleanup_failure);
                }
                _ = checkpoint_timer.tick() => {
                    let lease_checkpoint_failure = match self.router.checkpoint_active_leases().await {
                        Ok((_, 0)) => None,
                        Ok((_, failed)) => Some(format!("{failed} active lease checkpoints failed")),
                        Err(error) => Some(error.to_string()),
                    };
                    record_host_maintenance_state(&mut maintenance_failures, "active lease checkpoint", lease_checkpoint_failure);
                    let local_checkpoint_failure = match Box::pin(self.router.checkpoint_active_local_assignments()).await {
                        Ok((_, 0)) => None,
                        Ok((_, failed)) => Some(format!("{failed} active local assignment checkpoints failed")),
                        Err(error) => Some(error.to_string()),
                    };
                    record_host_maintenance_state(&mut maintenance_failures, "active local assignment checkpoint", local_checkpoint_failure);
                    let snapshot_cleanup_failure = match self.router.reconcile_snapshot_cleanup().await {
                        Ok(report) if report.failed_deletions == 0 => None,
                        Ok(report) => Some(format!("{} snapshot deletions failed", report.failed_deletions)),
                        Err(error) => Some(error.to_string()),
                    };
                    record_host_maintenance_state(&mut maintenance_failures, "snapshot cleanup", snapshot_cleanup_failure);
                    let cleanup_failure = self.router.reconcile_terminal_runtime_cleanup().await.err().map(|error| error.to_string());
                    record_host_maintenance_state(&mut maintenance_failures, "terminal runtime cleanup", cleanup_failure);
                }
                completed = connections.next(), if !connections.is_empty() => {
                    if local_capacity_saturated && connections.len() < HOST_CONTROL_CONNECTION_LIMIT {
                        eprintln!(
                            "mez host: local client capacity recovered: active {}, limit {HOST_CONTROL_CONNECTION_LIMIT}",
                            connections.len()
                        );
                        local_capacity_saturated = false;
                    }
                    if let Some(Some(request)) = completed {
                        let request: HostShutdownRequest = request;
                        self.router.start_draining()?;
                        break request;
                    }
                }
                accepted = self.listener.accept(), if connections.len() < HOST_CONTROL_CONNECTION_LIMIT => {
                    let (mut stream, _) = accepted?;
                    let peer_uid = match crate::runtime::authenticated_unix_peer_uid(
                        stream.as_raw_fd(),
                        self.config.owner_uid,
                    ) {
                        Ok(peer_uid) => peer_uid,
                        Err(error) => {
                            eprintln!("mez host: rejected local client: peer authentication failed: {error}");
                            continue;
                        }
                    };
                    if peer_uid != self.config.owner_uid {
                        eprintln!(
                            "mez host: rejected local client with uid {peer_uid}: expected uid {}",
                            self.config.owner_uid
                        );
                        continue;
                    }
                    connections.push(async move {
                        match tokio::time::timeout(
                            HOST_CONTROL_CONNECTION_TIMEOUT,
                            self.serve_connection(&mut stream),
                        )
                        .await
                        {
                            Ok(Ok(result)) => {
                                match result.failure {
                                    Some(error) => eprintln!(
                                        "mez host: local client request failed: uid {peer_uid}, method {:?}, error {error}",
                                        result.method
                                    ),
                                    None => eprintln!(
                                        "mez host: local client request completed: uid {peer_uid}, method {:?}, outcome succeeded",
                                        result.method
                                    ),
                                }
                                result.shutdown
                            }
                            Ok(Err(error)) => {
                                eprintln!("mez host: local client request failed: uid {peer_uid}, error {error}");
                                None
                            }
                            Err(_) => {
                                eprintln!("mez host: local client request timed out: uid {peer_uid}, timeout_seconds {}", HOST_CONTROL_CONNECTION_TIMEOUT.as_secs());
                                None
                            }
                        }
                    });
                    if connections.len() == HOST_CONTROL_CONNECTION_LIMIT {
                        eprintln!(
                            "mez host: local client capacity saturated: active {}, limit {HOST_CONTROL_CONNECTION_LIMIT}; new clients will wait",
                            connections.len()
                        );
                        local_capacity_saturated = true;
                    }
                }
            }
        };
        let draining = self.router.begin_draining();
        tokio::pin!(draining);
        loop {
            tokio::select! {
                result = &mut draining => {
                    result?;
                    break;
                }
                _ = connections.next(), if !connections.is_empty() => {}
            }
        }
        drop(connections);
        if !shutdown.force {
            self.router.checkpoint_active_leases_strict().await?;
            Box::pin(self.router.checkpoint_active_local_assignments_strict()).await?;
        }
        self.router
            .shutdown_all(shutdown.force, self.config.shutdown_timeout)
            .await?;
        self.router.mark_stopped()
    }

    fn serve_connection<'a>(
        &'a self,
        stream: &'a mut tokio::net::UnixStream,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<HostConnectionResult>> + Send + 'a>> {
        Box::pin(async move {
            let mut framed = Framed::new(
                stream,
                ProtocolFrameCodec::new(HOST_CONTROL_MAX_CONTENT_LENGTH)?,
            );
            let frame = framed.next().await.ok_or_else(|| {
                MezError::invalid_state("host control connection closed before request")
            })??;
            let request: Value = serde_json::from_str(&frame.body).map_err(|error| {
                MezError::invalid_args(format!("invalid host control JSON: {error}"))
            })?;
            let method = request
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("<missing>")
                .to_string();
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let result = self.dispatch_managed_request(&request).await;
            let (body, shutdown, failure) = match result {
                Ok((result, shutdown)) => (
                    json!({"jsonrpc":"2.0","id":id,"result":result}),
                    shutdown,
                    None,
                ),
                Err(error) => {
                    let failure = format!("{}: {}", host_error_name(error.kind()), error.message());
                    (host_error_response(id, &error), None, Some(failure))
                }
            };
            framed
                .get_mut()
                .write_all(&crate::control::encode_control_body(&body.to_string()))
                .await?;
            framed.get_mut().flush().await?;
            Ok(HostConnectionResult {
                method,
                failure,
                shutdown,
            })
        })
    }

    async fn dispatch_managed_request(
        &self,
        request: &Value,
    ) -> Result<(Value, Option<HostShutdownRequest>)> {
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| MezError::invalid_args("host control method is required"))?;
        if !host_administration_mutates(method, request) {
            if method.starts_with("lease/") {
                self.append_administration_audit(request, None, "attempted", None, None, None)?;
                let result = self.dispatch_request(request).await;
                let outcome = if result.is_ok() {
                    "succeeded"
                } else {
                    "failed"
                };
                self.append_administration_audit(
                    request,
                    Some(&result),
                    outcome,
                    None,
                    None,
                    administration_new_generation(&result),
                )?;
                return result;
            }
            return self.dispatch_request(request).await;
        }

        let _administration = self.administration_lock.lock().await;
        let params = request
            .get("params")
            .and_then(Value::as_object)
            .ok_or_else(|| MezError::invalid_args("host administration requires params"))?;
        let idempotency_key = required_string(params, "idempotency_key")?;
        let target = administration_target(method, params);
        let observed_generation = target
            .and_then(|target| self.router.get_lease(target).ok())
            .map(|lease| lease.lease_generation);
        let now = current_unix_seconds()?;
        let (request_fingerprint, previous_generation, pending) = match self
            .administration_journal
            .begin(
                idempotency_key,
                method,
                params,
                &self.config.owner_uid.to_string(),
                target,
                observed_generation,
                now,
            )? {
            HostAdministrationBegin::Fresh {
                request_fingerprint,
                previous_generation,
            } => (request_fingerprint, previous_generation, false),
            HostAdministrationBegin::Pending {
                request_fingerprint,
                previous_generation,
            } => (request_fingerprint, previous_generation, true),
            HostAdministrationBegin::Replay(HostAdministrationReplay::Success(response)) => {
                return Ok((
                    self.restore_administration_replay(method, response, idempotency_key, params)?,
                    None,
                ));
            }
            HostAdministrationBegin::Replay(HostAdministrationReplay::Failure(error)) => {
                return Err(error);
            }
        };

        self.append_administration_audit(
            request,
            None,
            "attempted",
            Some(&request_fingerprint),
            previous_generation,
            None,
        )?;
        let execution = HostAdministrationExecution {
            idempotency_key,
            request_fingerprint: &request_fingerprint,
        };
        let result = if pending {
            match self
                .reconcile_pending_administration(method, params, previous_generation)
                .await?
            {
                Some(result) => Ok((result, None)),
                None => {
                    self.dispatch_request_with_administration(request, Some(&execution))
                        .await
                }
            }
        } else {
            self.dispatch_request_with_administration(request, Some(&execution))
                .await
        };
        let outcome = if result.is_ok() {
            "succeeded"
        } else {
            "failed"
        };
        let new_generation = administration_new_generation(&result);
        self.append_administration_audit(
            request,
            Some(&result),
            outcome,
            Some(&request_fingerprint),
            previous_generation,
            new_generation,
        )?;
        match &result {
            Ok((response, _)) => self.administration_journal.complete_success(
                idempotency_key,
                &request_fingerprint,
                administration_persisted_response(method, response),
                new_generation,
                current_unix_seconds()?,
            )?,
            Err(error) => self.administration_journal.complete_failure(
                idempotency_key,
                &request_fingerprint,
                error,
                current_unix_seconds()?,
            )?,
        }
        result
    }

    fn append_administration_audit(
        &self,
        request: &Value,
        result: Option<&Result<(Value, Option<HostShutdownRequest>)>>,
        outcome: &str,
        request_fingerprint: Option<&str>,
        previous_generation: Option<u64>,
        new_generation: Option<u64>,
    ) -> Result<()> {
        #[cfg(test)]
        if outcome != "attempted"
            && self
                .fail_next_administration_completion_audit
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(std::io::Error::other(
                "injected host administration completion audit failure",
            )
            .into());
        }
        let mut audit = self
            .audit_log
            .lock()
            .map_err(|_| MezError::invalid_state("host audit lock was poisoned"))?;
        let Some(audit) = audit.as_mut() else {
            return Ok(());
        };
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("host/administration/unknown");
        let response = result.and_then(|result| result.as_ref().ok().map(|(value, _)| value));
        let requested_lease = request
            .pointer("/params/target")
            .and_then(Value::as_str)
            .and_then(|target| self.router.get_lease(target).ok());
        let session_id = response
            .and_then(|value| value.get("session_id"))
            .and_then(Value::as_str)
            .or_else(|| {
                requested_lease
                    .as_ref()
                    .map(|lease| lease.session_id.as_str())
            })
            .unwrap_or("host");
        let mut record = AuditRecord::new(
            session_id,
            AuditActor {
                kind: "local_host_admin".to_string(),
                id: self.config.owner_uid.to_string(),
            },
            if method.starts_with("lease/") {
                "lease_administration"
            } else {
                "trust_administration"
            },
            method,
        );
        record.outcome = outcome.to_string();
        if let Some(request_fingerprint) = request_fingerprint {
            record = record.with_metadata("request_fingerprint", request_fingerprint);
        }
        if let Some(target) = administration_target(
            method,
            request
                .get("params")
                .and_then(Value::as_object)
                .unwrap_or(&serde_json::Map::new()),
        ) {
            record = record.with_metadata("target", target);
        }
        if let Some(previous_generation) = previous_generation {
            record = record.with_metadata("previous_generation", previous_generation.to_string());
        }
        if let Some(new_generation) = new_generation {
            record = record.with_metadata("new_generation", new_generation.to_string());
        }
        if let Some(lease_id) = response
            .and_then(|value| value.get("lease_id"))
            .and_then(Value::as_str)
            .or_else(|| {
                requested_lease
                    .as_ref()
                    .map(|lease| lease.lease_id.as_str())
            })
        {
            record = record.with_metadata("lease_id", lease_id);
        }
        if let Some(generation) = response
            .and_then(|value| value.get("lease_generation"))
            .and_then(Value::as_u64)
            .or_else(|| requested_lease.as_ref().map(|lease| lease.lease_generation))
        {
            record = record.with_metadata("lease_generation", generation.to_string());
        }
        let _ = audit.append(record.sanitized())?;
        Ok(())
    }

    async fn reconcile_pending_administration(
        &self,
        method: &str,
        params: &serde_json::Map<String, Value>,
        previous_generation: Option<u64>,
    ) -> Result<Option<Value>> {
        match method {
            "remote/client/rename" => {
                let issuer = self.require_invitation_issuer()?;
                let client_id = required_string(params, "client_id")?;
                let label = required_string(params, "label")?;
                Ok(issuer
                    .list_clients()?
                    .into_iter()
                    .find(|record| record.id == client_id && record.label == label)
                    .map(|record| remote_trust_record_json(&record)))
            }
            "remote/client/revoke" => {
                let issuer = self.require_invitation_issuer()?;
                let client_id = required_string(params, "client_id")?;
                let reason = params.get("reason").and_then(Value::as_str);
                Ok(issuer
                    .list_clients()?
                    .into_iter()
                    .find(|record| {
                        record.id == client_id
                            && record.revoked()
                            && record.revocation_reason.as_deref() == reason
                    })
                    .map(|record| remote_trust_record_json(&record)))
            }
            "lease/checkpoint" => {
                self.reconcile_pending_lease(params, previous_generation, |lease| {
                    lease.checkpoint.is_some()
                })
            }
            "lease/recover" => self.reconcile_pending_lease(params, previous_generation, |lease| {
                lease.state == crate::storage::lease::RemoteSessionLeaseState::Active
            }),
            "lease/release" => self.reconcile_pending_lease(params, previous_generation, |lease| {
                lease.state == crate::storage::lease::RemoteSessionLeaseState::Released
            }),
            "lease/revoke" => self.reconcile_pending_lease(params, previous_generation, |lease| {
                lease.state == crate::storage::lease::RemoteSessionLeaseState::Revoked
            }),
            _ => Ok(None),
        }
    }

    fn reconcile_pending_lease(
        &self,
        params: &serde_json::Map<String, Value>,
        previous_generation: Option<u64>,
        matches: impl FnOnce(&crate::storage::lease::RemoteSessionLease) -> bool,
    ) -> Result<Option<Value>> {
        let target = required_string(params, "target")?;
        let lease = self.router.get_lease(target)?;
        Ok(
            (previous_generation.is_some_and(|generation| lease.lease_generation > generation)
                && matches(&lease))
            .then(|| remote_lease_json(&lease)),
        )
    }

    fn restore_administration_replay(
        &self,
        method: &str,
        response: Value,
        idempotency_key: &str,
        params: &serde_json::Map<String, Value>,
    ) -> Result<Value> {
        if method == "remote/invite" {
            return self
                .require_invitation_issuer()?
                .restore_invitation_response(response, idempotency_key, params);
        }
        Ok(response)
    }

    fn require_invitation_issuer(&self) -> Result<&HostIrohInvitationIssuer> {
        self.config
            .iroh_invitation_issuer
            .as_ref()
            .ok_or_else(|| MezError::invalid_state("host Iroh listener is not enabled"))
    }

    async fn dispatch_request(
        &self,
        request: &Value,
    ) -> Result<(Value, Option<HostShutdownRequest>)> {
        self.dispatch_request_with_administration(request, None)
            .await
    }

    async fn dispatch_request_with_administration(
        &self,
        request: &Value,
        administration: Option<&HostAdministrationExecution<'_>>,
    ) -> Result<(Value, Option<HostShutdownRequest>)> {
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| MezError::invalid_args("host control method is required"))?;
        let params = request
            .get("params")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        match method {
            "host/get" => Ok((self.status_json().await?, None)),
            "remote/status" => {
                let issuer = self.config.iroh_invitation_issuer.as_ref();
                Ok((
                    json!({
                        "enabled": issuer.is_some(),
                        "endpoint_id": issuer.map(HostIrohInvitationIssuer::endpoint_id),
                    }),
                    None,
                ))
            }
            "remote/invite" => {
                let issuer =
                    self.config.iroh_invitation_issuer.as_ref().ok_or_else(|| {
                        MezError::invalid_state("host Iroh listener is not enabled")
                    })?;
                let role = match params
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("observer")
                {
                    "observer" => crate::security::remote::RemoteRoleCeiling::Observer,
                    "primary" => crate::security::remote::RemoteRoleCeiling::Primary,
                    _ => {
                        return Err(MezError::invalid_args(
                            "remote invitation role must be observer or primary",
                        ));
                    }
                };
                let allow_create = params
                    .get("allow_create")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let allow_kill = params
                    .get("allow_kill")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if allow_kill && !allow_create {
                    return Err(MezError::invalid_args(
                        "remote force-kill authority requires session creation authority",
                    ));
                }
                if allow_kill && role != crate::security::remote::RemoteRoleCeiling::Primary {
                    return Err(MezError::invalid_args(
                        "remote force-kill authority requires a primary role ceiling",
                    ));
                }
                let max_leases = optional_positive_usize(&params, "max_leases")?
                    .unwrap_or(self.config.max_remote_leases);
                let max_live_sessions = optional_positive_usize(&params, "max_live_sessions")?
                    .unwrap_or(self.config.max_live_sessions.min(max_leases));
                let lease_lifetime_ceiling_seconds = params
                    .get("lease_lifetime_ceiling_seconds")
                    .map(|value| {
                        value.as_u64().filter(|value| *value > 0).ok_or_else(|| {
                            MezError::invalid_args(
                                "remote lease_lifetime_ceiling_seconds must be positive",
                            )
                        })
                    })
                    .transpose()?;
                let authority = crate::security::remote::RemoteHostRoutingAuthority {
                    session_create: allow_create,
                    session_kill: allow_kill,
                    session_list: true,
                    session_attach_scope: crate::security::remote::RemoteSessionAttachScope::Own,
                    max_active_leases: if allow_create { max_leases } else { 0 },
                    max_live_sessions: if allow_create { max_live_sessions } else { 0 },
                    lease_lifetime_ceiling_seconds: if allow_create {
                        lease_lifetime_ceiling_seconds
                    } else {
                        None
                    },
                };
                let ttl_seconds = params
                    .get("expires_seconds")
                    .map(|value| {
                        value.as_u64().ok_or_else(|| {
                            MezError::invalid_args(
                                "remote invitation expires_seconds must be an unsigned integer",
                            )
                        })
                    })
                    .transpose()?
                    .unwrap_or(600);
                let profile_name = params
                    .get("profile_name")
                    .and_then(Value::as_str)
                    .unwrap_or("mez-host");
                let invitation = match administration {
                    Some(administration) => issuer.create_idempotent_invitation(
                        profile_name,
                        role,
                        authority,
                        ttl_seconds,
                        current_unix_seconds()?,
                        administration.idempotency_key,
                        administration.request_fingerprint,
                    )?,
                    None => issuer.create_invitation(
                        profile_name,
                        role,
                        authority,
                        ttl_seconds,
                        current_unix_seconds()?,
                    )?,
                };
                Ok((invitation, None))
            }
            "remote/client/list" => {
                let issuer =
                    self.config.iroh_invitation_issuer.as_ref().ok_or_else(|| {
                        MezError::invalid_state("host Iroh listener is not enabled")
                    })?;
                let clients = issuer
                    .list_clients()?
                    .iter()
                    .map(remote_trust_record_json)
                    .collect::<Vec<_>>();
                Ok((json!({"clients": clients}), None))
            }
            "remote/client/rename" => {
                let issuer =
                    self.config.iroh_invitation_issuer.as_ref().ok_or_else(|| {
                        MezError::invalid_state("host Iroh listener is not enabled")
                    })?;
                let client_id = required_string(&params, "client_id")?;
                let label = required_string(&params, "label")?;
                Ok((
                    remote_trust_record_json(&issuer.rename_client(client_id, label)?),
                    None,
                ))
            }
            "remote/client/revoke" => {
                let issuer =
                    self.config.iroh_invitation_issuer.as_ref().ok_or_else(|| {
                        MezError::invalid_state("host Iroh listener is not enabled")
                    })?;
                let client_id = required_string(&params, "client_id")?;
                let reason = params.get("reason").and_then(Value::as_str);
                Ok((
                    remote_trust_record_json(&issuer.revoke_client(
                        client_id,
                        reason,
                        current_unix_seconds()?,
                    )?),
                    None,
                ))
            }
            "lease/list" => {
                let state = params
                    .get("state")
                    .and_then(Value::as_str)
                    .map(parse_lease_state)
                    .transpose()?;
                let owner = params.get("owner").and_then(Value::as_str);
                let include_terminal = params.get("all").and_then(Value::as_bool).unwrap_or(false);
                let leases = self
                    .router
                    .list_leases(state, owner, include_terminal)?
                    .iter()
                    .map(remote_lease_json)
                    .collect::<Vec<_>>();
                Ok((json!({"leases": leases}), None))
            }
            "lease/get" => {
                let target = required_string(&params, "target")?;
                Ok((remote_lease_json(&self.router.get_lease(target)?), None))
            }
            "lease/checkpoint" => {
                let target = required_string(&params, "target")?;
                let lease = self.router.checkpoint_lease(target).await?;
                Ok((remote_lease_json(&lease), None))
            }
            "lease/recover" => {
                let target = required_string(&params, "target")?;
                let binding = self.router.recover_lease(target).await?;
                Ok((remote_lease_json(&binding.lease), None))
            }
            "lease/release" => {
                let target = required_string(&params, "target")?;
                let terminate = params
                    .get("terminate")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let lease = self.router.release_lease(target, terminate).await?;
                Ok((remote_lease_json(&lease), None))
            }
            "lease/revoke" => {
                let target = required_string(&params, "target")?;
                let terminate = params
                    .get("terminate")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let reason = params
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let lease = self.router.revoke_lease(target, reason, terminate).await?;
                Ok((remote_lease_json(&lease), None))
            }
            "lease/gc" => {
                let now = current_unix_seconds()?;
                let older_than_seconds = params
                    .get("older_than_seconds")
                    .map(|value| {
                        value.as_u64().ok_or_else(|| {
                            MezError::invalid_args(
                                "lease gc older_than_seconds must be an unsigned integer",
                            )
                        })
                    })
                    .transpose()?;
                let released_cutoff = now.saturating_sub(
                    older_than_seconds.unwrap_or(self.config.released_lease_retention_seconds),
                );
                let failed_cutoff = now.saturating_sub(
                    older_than_seconds.unwrap_or(self.config.failed_lease_retention_seconds),
                );
                let apply = params
                    .get("apply")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let report = self
                    .router
                    .garbage_collect_leases(
                        crate::storage::lease::LeaseGarbageCollectionPolicy {
                            released_before_unix_seconds: released_cutoff,
                            revoked_before_unix_seconds: released_cutoff,
                            failed_before_unix_seconds: failed_cutoff,
                        },
                        apply,
                    )
                    .await?;
                Ok((
                    json!({
                        "applied": report.applied,
                        "lease_ids": report.preview.lease_ids,
                        "checkpoint_snapshot_ids": report.preview.checkpoint_snapshot_ids,
                        "deleted_snapshot_ids": report.deleted_snapshot_ids,
                        "retained_snapshot_ids": report.retained_snapshot_ids,
                    }),
                    None,
                ))
            }
            "host/session/list" => {
                let all = params.get("all").and_then(Value::as_bool).unwrap_or(false);
                let _ = self.router.registry().prune_stale()?;
                let remote_leases = self.router.list_leases(None, None, false)?;
                let mut records: Vec<Value> =
                    serde_json::from_str(&records_to_json(&self.router.registry().list()?))
                        .map_err(|error| {
                            MezError::invalid_state(format!("invalid registry JSON: {error}"))
                        })?;
                records.retain(|record| {
                    let session_id = record.get("session_id").and_then(Value::as_str);
                    !remote_leases
                        .iter()
                        .any(|lease| Some(lease.session_id.as_str()) == session_id)
                });
                records.extend(
                    self.router
                        .list_recoverable_local_assignments()?
                        .into_iter()
                        .map(|assignment| {
                            json!({
                                "session_id": assignment.session_id,
                                "name": assignment.name,
                                "state": "recoverable",
                                "socket": Value::Null,
                                "accepts_primary": false,
                                "recoverable": true,
                            })
                        }),
                );
                if all {
                    for record in &mut records {
                        if let Some(record) = record.as_object_mut() {
                            record.insert("scope".to_string(), Value::String("local".to_string()));
                        }
                    }
                    records.extend(
                        remote_leases
                            .iter()
                            .map(|lease| {
                                json!({
                                    "scope": "remote",
                                    "lease_id": lease.lease_id,
                                    "session_id": lease.session_id,
                                    "name": lease.name,
                                    "state": match lease.state {
                                        crate::storage::lease::RemoteSessionLeaseState::Pending => "pending",
                                        crate::storage::lease::RemoteSessionLeaseState::Active => "active",
                                        crate::storage::lease::RemoteSessionLeaseState::Recoverable => "recoverable",
                                        crate::storage::lease::RemoteSessionLeaseState::Released => "released",
                                        crate::storage::lease::RemoteSessionLeaseState::Revoked => "revoked",
                                        crate::storage::lease::RemoteSessionLeaseState::Failed => "failed",
                                    },
                                    "socket": Value::Null,
                                    "accepts_primary": false,
                                    "recoverable": lease.state == crate::storage::lease::RemoteSessionLeaseState::Recoverable,
                                })
                            }),
                    );
                }
                Ok((json!({"sessions": records}), None))
            }
            "host/session/create" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let columns = optional_u16(&params, "columns")?.unwrap_or(80);
                let rows = optional_u16(&params, "rows")?.unwrap_or(24);
                let context =
                    self.local_session_launch_context(&params, Size::new(columns, rows)?)?;
                let record = Box::pin(self.create_session_with_context(name, context)).await?;
                Ok((session_record_json(&record), None))
            }
            "host/session/resolve-or-create" => {
                let columns = optional_u16(&params, "columns")?.unwrap_or(80);
                let rows = optional_u16(&params, "rows")?.unwrap_or(24);
                let context =
                    self.local_session_launch_context(&params, Size::new(columns, rows)?)?;
                let record = Box::pin(self.resolve_or_create_session_with_context(context)).await?;
                Ok((session_record_json(&record), None))
            }
            "host/session/resolve" => {
                let target = params.get("target").and_then(Value::as_str);
                let requested_role = params
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("primary");
                let record = Box::pin(self.resolve_session(target, requested_role)).await?;
                Ok((session_record_json(&record), None))
            }
            "host/reconcile" => {
                let _ = self.router.reconcile_snapshot_cleanup().await?;
                let report = self.router.reconcile()?;
                Ok((
                    json!({
                        "reconciled": true,
                        "boot_generation": report.boot_generation,
                        "leases": {
                            "pending": report.pending,
                            "active": report.active,
                            "recoverable": report.recoverable,
                            "released": report.released,
                            "revoked": report.revoked,
                            "failed": report.failed,
                        },
                        "pruned_registry_records": report.pruned_registry_records,
                    }),
                    None,
                ))
            }
            "host/shutdown" => {
                let force = params
                    .get("force")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.router.start_draining()?;
                Ok((
                    json!({"shutting_down":true,"force":force}),
                    Some(HostShutdownRequest { force }),
                ))
            }
            _ => Err(MezError::not_implemented(format!(
                "unknown host control method `{method}`"
            ))),
        }
    }

    async fn status_json(&self) -> Result<Value> {
        let snapshots = self.router.snapshots().await?;
        let reconciliation = self.router.reconcile()?;
        let running = snapshots
            .iter()
            .filter(|snapshot| snapshot.state == SessionSupervisorState::Running)
            .count();
        let starting = snapshots
            .iter()
            .filter(|snapshot| snapshot.state == SessionSupervisorState::Starting)
            .count();
        let stopping = snapshots
            .iter()
            .filter(|snapshot| snapshot.state == SessionSupervisorState::Stopping)
            .count();
        let failed = snapshots
            .iter()
            .filter(|snapshot| snapshot.state == SessionSupervisorState::Failed)
            .count();
        let iroh = self.config.iroh_invitation_issuer.as_ref();
        Ok(json!({
            "ready": true,
            "pid": std::process::id(),
            "socket": self.socket_path,
            "admission_state": self.router.admission_state().as_str(),
            "boot_generation": reconciliation.boot_generation,
            "iroh": {
                "enabled": iroh.is_some(),
                "endpoint_id": iroh.map(HostIrohInvitationIssuer::endpoint_id),
            },
            "running_sessions": running,
            "starting_sessions": starting,
            "stopping_sessions": stopping,
            "failed_sessions": failed,
            "leases": {
                "pending": reconciliation.pending,
                "active": reconciliation.active,
                "recoverable": reconciliation.recoverable,
                "released": reconciliation.released,
                "revoked": reconciliation.revoked,
                "failed": reconciliation.failed,
            },
            "snapshot_cleanup_pending": reconciliation.snapshot_cleanup_pending,
            "policy": {
                "checkpoint_interval_seconds": self.config.checkpoint_interval.as_secs(),
                "recover_on_start": match self.config.recovery_policy {
                    HostRecoveryPolicy::Lazy => "lazy",
                    HostRecoveryPolicy::Eager => "eager",
                    HostRecoveryPolicy::Disabled => "disabled",
                },
                "default_session_policy": match self.config.default_session_policy {
                    HostDefaultSessionPolicy::MostRecentAttachable => "most_recent_attachable",
                    HostDefaultSessionPolicy::None => "none",
                },
                "default_lease_lifetime_seconds": self.config.default_lease_lifetime_seconds,
                "failed_lease_retention_seconds": self.config.failed_lease_retention_seconds,
                "released_lease_retention_seconds": self.config.released_lease_retention_seconds,
            },
            "pruned_registry_records": reconciliation.pruned_registry_records,
            "max_sessions": self.config.max_sessions,
            "max_live_sessions": self.config.max_live_sessions,
        }))
    }

    #[allow(
        dead_code,
        reason = "compatibility tests exercise host admission without caller launch metadata"
    )]
    async fn create_session(
        &self,
        name: Option<String>,
        size: Size,
    ) -> Result<crate::storage::registry::SessionRecord> {
        self.router.create_local(name, size).await
    }

    #[allow(
        dead_code,
        reason = "compatibility tests retain the daemon-scoped resolve-or-create boundary"
    )]
    async fn resolve_or_create_session(
        &self,
        size: Size,
    ) -> Result<crate::storage::registry::SessionRecord> {
        self.router.resolve_or_create_local(size).await
    }

    async fn create_session_with_context(
        &self,
        name: Option<String>,
        context: LocalSessionLaunchContext,
    ) -> Result<crate::storage::registry::SessionRecord> {
        self.router.create_local_with_context(name, context).await
    }

    async fn resolve_or_create_session_with_context(
        &self,
        context: LocalSessionLaunchContext,
    ) -> Result<crate::storage::registry::SessionRecord> {
        self.router
            .resolve_or_create_local_with_context(context)
            .await
    }

    fn local_session_launch_context(
        &self,
        params: &serde_json::Map<String, Value>,
        size: Size,
    ) -> Result<LocalSessionLaunchContext> {
        let requested_directory = params
            .get("cwd")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or(std::env::current_dir()?);
        let current_directory = requested_directory.canonicalize().map_err(|error| {
            MezError::invalid_args(format!(
                "local session launch directory is unavailable: {error}"
            ))
        })?;
        if !current_directory.is_dir() {
            return Err(MezError::invalid_args(
                "local session launch directory must be a directory",
            ));
        }
        let shell = match params.get("shell").and_then(Value::as_str) {
            Some(shell) => {
                let requested = PathBuf::from(shell);
                let resolved = resolve_shell(Some(OsString::from(shell)))?;
                if resolved.path() != requested {
                    return Err(MezError::invalid_args(
                        "requested local session shell is unavailable",
                    ));
                }
                resolved
            }
            None => self.config.shell.clone(),
        };
        let environment = params
            .get("environment")
            .map(|value| {
                let object = value.as_object().ok_or_else(|| {
                    MezError::invalid_args("local session environment must be an object")
                })?;
                object
                    .iter()
                    .map(|(key, value)| {
                        if !local_launch_environment_key_allowed(key) {
                            return Err(MezError::invalid_args(format!(
                                "local session environment key `{key}` is not allowed"
                            )));
                        }
                        let value = value.as_str().ok_or_else(|| {
                            MezError::invalid_args(
                                "local session environment values must be strings",
                            )
                        })?;
                        Ok((key.clone(), value.to_string()))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;
        Ok(LocalSessionLaunchContext {
            config_layers: load_runtime_config_layers_for_directory(
                &ConfigPaths::from_root(self.config.config_root.clone()),
                &current_directory,
            )?,
            current_directory,
            shell,
            size,
            environment,
        })
    }

    async fn resolve_session(
        &self,
        target: Option<&str>,
        requested_role: &str,
    ) -> Result<crate::storage::registry::SessionRecord> {
        self.router.resolve_local(target, requested_role).await
    }
}

impl Drop for HostServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
    }
}

/// Returns the canonical host socket below one private runtime directory.
pub(crate) fn host_socket_path(runtime_root: &Path) -> Result<PathBuf> {
    socket_path_for_name(runtime_root, HOST_SOCKET_FILE_NAME)
}

fn optional_u16(params: &serde_json::Map<String, Value>, field: &str) -> Result<Option<u16>> {
    params
        .get(field)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u16::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| MezError::invalid_args(format!("host {field} must be positive u16")))
        })
        .transpose()
}

fn optional_positive_usize(
    params: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<usize>> {
    params
        .get(field)
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| MezError::invalid_args(format!("remote {field} must be positive")))
        })
        .transpose()
}

fn required_string<'a>(params: &'a serde_json::Map<String, Value>, field: &str) -> Result<&'a str> {
    params
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| MezError::invalid_args(format!("remote request requires {field}")))
}

fn current_unix_seconds() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| MezError::invalid_state("system clock is before the Unix epoch"))
}

fn remote_trust_record_json(record: &crate::security::remote::RemoteTrustRecord) -> Value {
    json!({
        "client_id": record.id,
        "label": record.label,
        "role": record.role_ceiling.as_str(),
        "routing": record.host_routing,
        "revoked": record.revoked(),
        "created_at_unix_seconds": record.created_at_unix_seconds,
        "last_used_at_unix_seconds": record.last_used_at_unix_seconds,
        "revoked_at_unix_seconds": record.revoked_at_unix_seconds,
        "revocation_reason": record.revocation_reason,
    })
}

fn parse_lease_state(value: &str) -> Result<crate::storage::lease::RemoteSessionLeaseState> {
    match value {
        "pending" => Ok(crate::storage::lease::RemoteSessionLeaseState::Pending),
        "active" => Ok(crate::storage::lease::RemoteSessionLeaseState::Active),
        "recoverable" => Ok(crate::storage::lease::RemoteSessionLeaseState::Recoverable),
        "released" => Ok(crate::storage::lease::RemoteSessionLeaseState::Released),
        "revoked" => Ok(crate::storage::lease::RemoteSessionLeaseState::Revoked),
        "failed" => Ok(crate::storage::lease::RemoteSessionLeaseState::Failed),
        _ => Err(MezError::invalid_args(
            "lease state must be pending, active, recoverable, released, revoked, or failed",
        )),
    }
}

fn host_administration_mutates(method: &str, request: &Value) -> bool {
    match method {
        "remote/invite"
        | "remote/client/rename"
        | "remote/client/revoke"
        | "lease/checkpoint"
        | "lease/recover"
        | "lease/release"
        | "lease/revoke" => true,
        "lease/gc" => request
            .pointer("/params/apply")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        _ => false,
    }
}

fn administration_target<'a>(
    method: &str,
    params: &'a serde_json::Map<String, Value>,
) -> Option<&'a str> {
    match method {
        "remote/invite" => params
            .get("profile_name")
            .and_then(Value::as_str)
            .or(Some("mez-host")),
        "remote/client/rename" | "remote/client/revoke" => {
            params.get("client_id").and_then(Value::as_str)
        }
        method if method.starts_with("lease/") && method != "lease/gc" => {
            params.get("target").and_then(Value::as_str)
        }
        "lease/gc" => Some("lease-gc"),
        _ => None,
    }
}

fn administration_new_generation(
    result: &Result<(Value, Option<HostShutdownRequest>)>,
) -> Option<u64> {
    result
        .as_ref()
        .ok()
        .and_then(|(value, _)| value.get("lease_generation"))
        .and_then(Value::as_u64)
}

fn administration_persisted_response(method: &str, response: &Value) -> Value {
    let mut persisted = response.clone();
    if method == "remote/invite"
        && let Some(object) = persisted.as_object_mut()
    {
        object.remove("token");
    }
    persisted
}

fn remote_lease_json(lease: &crate::storage::lease::RemoteSessionLease) -> Value {
    json!({
        "lease_id": lease.lease_id,
        "session_id": lease.session_id,
        "owner_principal_id": lease.owner_principal_id,
        "name": lease.name,
        "default_for_owner": lease.default_for_owner,
        "state": match lease.state {
            crate::storage::lease::RemoteSessionLeaseState::Pending => "pending",
            crate::storage::lease::RemoteSessionLeaseState::Active => "active",
            crate::storage::lease::RemoteSessionLeaseState::Recoverable => "recoverable",
            crate::storage::lease::RemoteSessionLeaseState::Released => "released",
            crate::storage::lease::RemoteSessionLeaseState::Revoked => "revoked",
            crate::storage::lease::RemoteSessionLeaseState::Failed => "failed",
        },
        "created_at_unix_seconds": lease.created_at_unix_seconds,
        "updated_at_unix_seconds": lease.updated_at_unix_seconds,
        "activated_at_unix_seconds": lease.activated_at_unix_seconds,
        "terminal_at_unix_seconds": lease.terminal_at_unix_seconds,
        "expires_at_unix_seconds": lease.expires_at_unix_seconds,
        "checkpoint": lease.checkpoint.as_ref().map(|checkpoint| json!({
            "snapshot_id": checkpoint.snapshot_id,
            "snapshot_version": checkpoint.snapshot_version,
            "session_id": checkpoint.session_id,
            "recorded_at_unix_seconds": checkpoint.recorded_at_unix_seconds,
        })),
        "failure": lease.failure,
        "boot_generation": lease.boot_generation,
        "lease_generation": lease.lease_generation,
    })
}

fn session_record_json(record: &crate::storage::registry::SessionRecord) -> Value {
    json!({
        "session_id": record.session_id,
        "name": record.name,
        "socket": record.socket_path,
        "accepts_primary": record.accepts_primary,
    })
}

fn host_error_response(id: Value, error: &MezError) -> Value {
    let code = match error.kind() {
        MezErrorKind::InvalidArgs => -32602,
        MezErrorKind::Forbidden => -32002,
        MezErrorKind::Conflict => -32006,
        MezErrorKind::NotFound => -32005,
        MezErrorKind::RateLimited => -32011,
        MezErrorKind::NotImplemented => -32601,
        _ => -32004,
    };
    json!({
        "jsonrpc":"2.0",
        "id":id,
        "error":{
            "code":code,
            "message":error.message(),
            "data":{"mezzanine_code":host_error_name(error.kind())}
        }
    })
}

fn host_error_name(kind: MezErrorKind) -> &'static str {
    match kind {
        MezErrorKind::InvalidArgs => "invalid_params",
        MezErrorKind::InvalidState => "invalid_state",
        MezErrorKind::Config | MezErrorKind::Io => "internal_error",
        MezErrorKind::Conflict => "conflict",
        MezErrorKind::NotFound => "not_found",
        MezErrorKind::Forbidden => "forbidden",
        MezErrorKind::RateLimited => "rate_limited",
        MezErrorKind::NotImplemented => "method_not_found",
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use crate::config::{ConfigFormat, ConfigScope};
    use crate::control::RequestedRole;
    use crate::host::shell::{ResolvedShell, ShellSource};
    use crate::security::audit::{AuditConfig, AuditLog};
    use crate::security::project::{ProjectTrustStore, TrustDecision, default_trust_database_path};
    use crate::security::remote::{
        RemoteHostRoutingAuthority, RemotePrincipal, RemoteRoleCeiling, RemoteSessionAttachScope,
    };
    use crate::storage::snapshot::SnapshotRepository;

    use super::*;

    fn test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mez-host-server-{}-{name}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    fn config(root: PathBuf) -> HostServerConfig {
        HostServerConfig {
            runtime_root: root.clone(),
            owner_uid: crate::runtime::current_effective_uid(),
            config_root: root,
            config_layers: vec![ConfigLayer {
                name: "host-test".to_string(),
                path: None,
                format: ConfigFormat::Toml,
                scope: ConfigScope::Primary,
                trusted: true,
                text: "[agents]\nshell_mode = \"pane\"\n[permissions]\nsandbox = \"policy-only\"\n"
                    .to_string(),
            }],
            shell: ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
            max_sessions: 8,
            max_live_sessions: 4,
            shutdown_timeout: Duration::from_secs(2),
            checkpoint_interval: Duration::from_secs(300),
            recovery_policy: HostRecoveryPolicy::Lazy,
            default_session_policy: HostDefaultSessionPolicy::MostRecentAttachable,
            default_lease_lifetime_seconds: 0,
            failed_lease_retention_seconds: 604_800,
            released_lease_retention_seconds: 604_800,
            iroh_invitation_issuer: None,
            max_remote_leases: 8,
            audit_log: None,
        }
    }

    /// Local host creation must recompute caller-project layers and carry the
    /// caller shell, terminal size, and canonical cwd into the live runtime.
    #[tokio::test(flavor = "current_thread")]
    async fn local_create_uses_caller_launch_context_instead_of_host_start_context() {
        let root = test_root("caller-context");
        let config_root = root.join("config");
        let runtime_root = root.join("runtime");
        let host_project = root.join("project-a");
        let caller_project = root.join("project-b");
        let caller_directory = caller_project.join("src");
        let host_overlay = host_project.join(".mezzanine/config.toml");
        let caller_overlay = caller_project.join(".mezzanine/config.toml");
        fs::create_dir_all(host_project.join(".git")).unwrap();
        fs::create_dir_all(caller_project.join(".git")).unwrap();
        fs::create_dir_all(host_overlay.parent().unwrap()).unwrap();
        fs::create_dir_all(caller_overlay.parent().unwrap()).unwrap();
        fs::create_dir_all(&caller_directory).unwrap();
        fs::create_dir_all(&config_root).unwrap();
        fs::set_permissions(&config_root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(&host_overlay, "version = 73\n[history]\nlines = 111\n").unwrap();
        fs::write(&caller_overlay, "version = 73\n[history]\nlines = 222\n").unwrap();
        let mut trust = ProjectTrustStore::default();
        trust
            .decide_at(
                caller_project.clone(),
                TrustDecision::Trusted,
                Some(caller_project.join(".git")),
                42,
            )
            .unwrap();
        trust
            .save_to_file(&default_trust_database_path(&config_root))
            .unwrap();

        let mut host_config = config(config_root.clone());
        host_config.runtime_root = runtime_root;
        host_config.shell =
            ResolvedShell::new(PathBuf::from("/bin/false"), ShellSource::FallbackBinSh);
        host_config.config_layers = vec![ConfigLayer {
            name: "host-project-a".to_string(),
            path: Some(host_overlay.clone()),
            format: ConfigFormat::Toml,
            scope: ConfigScope::ProjectOverlay,
            trusted: true,
            text: fs::read_to_string(&host_overlay).unwrap(),
        }];
        let host = HostServer::bind(host_config).unwrap();
        let params = json!({
            "name": "caller-context",
            "cwd": caller_directory,
            "shell": "/bin/sh",
            "columns": 101,
            "rows": 37,
            "environment": {
                "HOME": root,
                "PATH": "/usr/bin:/bin",
                "SHELL": "/bin/sh",
                "COLUMNS": "101",
                "LINES": "37"
            }
        });
        let parsed_context = host
            .local_session_launch_context(params.as_object().unwrap(), Size::new(101, 37).unwrap())
            .unwrap();
        assert!(parsed_context.config_layers.iter().any(|layer| {
            layer.path.as_deref() == Some(caller_overlay.as_path()) && layer.trusted
        }));
        assert!(
            parsed_context
                .config_layers
                .iter()
                .all(|layer| { layer.path.as_deref() != Some(host_overlay.as_path()) })
        );
        let (created, shutdown) = host
            .dispatch_request(&json!({
                "jsonrpc": "2.0",
                "id": "caller-context",
                "method": "host/session/create",
                "params": params
            }))
            .await
            .unwrap();
        assert_eq!(shutdown, None);
        let session_id = created["session_id"].as_str().unwrap();
        let runtime = host.router.runtime_for_tests(session_id).unwrap();
        let snapshots = SnapshotRepository::new(config_root.join("test-layouts"));
        runtime
            .actor()
            .create_host_checkpoint(
                snapshots.clone(),
                "caller-context".to_string(),
                Some("caller context".to_string()),
            )
            .await
            .unwrap();
        let payload = snapshots.inspect_payload("caller-context").unwrap();
        assert_eq!(payload.authoritative_columns, 101);
        assert_eq!(payload.authoritative_rows, 37);
        assert_eq!(payload.shell.path, "/bin/sh");
        assert!(
            payload
                .windows
                .iter()
                .flat_map(|window| &window.panes)
                .any(|pane| {
                    pane.current_working_directory.as_deref()
                        == Some(caller_directory.to_string_lossy().as_ref())
                })
        );

        host.router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        drop(host);
        let _ = fs::remove_dir_all(root);
    }

    /// Malformed caller context is rejected before creating a local session.
    #[tokio::test(flavor = "current_thread")]
    async fn local_create_rejects_invalid_cwd_and_environment() {
        let root = test_root("invalid-caller-context");
        let invalid_cwd = root.join("not-a-directory");
        fs::write(&invalid_cwd, "file").unwrap();
        let host = HostServer::bind(config(root.clone())).unwrap();
        for params in [
            json!({"cwd": invalid_cwd, "shell": "/bin/sh"}),
            json!({"cwd": root, "shell": "/bin/sh", "environment": {"SECRET": "no"}}),
            json!({"cwd": root, "shell": "/bin/sh", "environment": {"HOME": 7}}),
            json!({"cwd": root, "shell": "/bin/sh", "environment": {"HOME": "x".repeat(4097)}}),
        ] {
            let error = host
                .dispatch_request(&json!({
                    "jsonrpc": "2.0",
                    "id": "invalid-caller-context",
                    "method": "host/session/create",
                    "params": params,
                }))
                .await
                .unwrap_err();
            assert_eq!(error.kind(), MezErrorKind::InvalidArgs, "{error}");
        }
        assert!(host.router.registry().list().unwrap().is_empty());
        drop(host);
        let _ = fs::remove_dir_all(root);
    }

    /// The durable host lock permits one owner across distinct runtime socket
    /// roots and a rejected contender cannot advance the boot generation.
    #[tokio::test(flavor = "current_thread")]
    async fn host_bind_excludes_duplicate_live_owner() {
        let root = test_root("lock");
        let config_root = root.join("config");
        let mut first_config = config(config_root.clone());
        first_config.runtime_root = root.join("runtime-a");
        let mut second_config = config(config_root.clone());
        second_config.runtime_root = root.join("runtime-b");
        let leases = crate::storage::lease::RemoteSessionLeaseRepository::new(
            crate::storage::lease::default_remote_session_lease_directory(&config_root),
        );

        let host = HostServer::bind(first_config).unwrap();
        assert_eq!(leases.boot_generation().unwrap(), 1);
        let duplicate = HostServer::bind(second_config.clone()).unwrap_err();
        assert_eq!(duplicate.kind(), MezErrorKind::Conflict);
        assert_eq!(leases.boot_generation().unwrap(), 1);
        drop(host);
        let restarted = HostServer::bind(second_config).unwrap();
        assert_eq!(leases.boot_generation().unwrap(), 2);
        drop(restarted);
        let _ = fs::remove_dir_all(root);
    }

    /// A client that connects without sending a frame cannot delay a second
    /// management request or prevent prompt host shutdown.
    #[tokio::test(flavor = "current_thread")]
    async fn stalled_host_control_client_does_not_block_other_requests() {
        let root = test_root("concurrent-control");
        let host = std::sync::Arc::new(HostServer::bind(config(root.clone())).unwrap());
        let serving_host = std::sync::Arc::clone(&host);
        let server_task =
            tokio::spawn(async move { serving_host.serve(std::future::pending()).await });

        let stalled = tokio::net::UnixStream::connect(host.socket_path())
            .await
            .unwrap();
        tokio::task::yield_now().await;
        let status = tokio::time::timeout(
            Duration::from_secs(1),
            exchange_host_socket_request(host.socket_path(), "host/get", json!({})),
        )
        .await;
        let status = match status {
            Ok(status) => status,
            Err(_) => {
                drop(stalled);
                server_task.abort();
                let _ = server_task.await;
                panic!("stalled host client blocked an independent status request");
            }
        };
        assert_eq!(status["result"]["ready"], true);

        let shutdown = exchange_host_socket_request(
            host.socket_path(),
            "host/shutdown",
            json!({"force":true}),
        )
        .await;
        assert_eq!(shutdown["result"]["shutting_down"], true);
        tokio::time::timeout(Duration::from_secs(1), server_task)
            .await
            .expect("host shutdown should not wait for the stalled client")
            .unwrap()
            .unwrap();
        drop(stalled);
        drop(host);
        let _ = fs::remove_dir_all(root);
    }

    /// Lease expiry follows the nearest durable authority deadline rather
    /// than waiting for the much longer checkpoint maintenance interval.
    #[tokio::test(flavor = "current_thread")]
    async fn host_expires_finite_lease_before_checkpoint_tick() {
        let root = test_root("nearest-lease-expiry");
        let mut host_config = config(root.clone());
        host_config.checkpoint_interval = Duration::from_secs(300);
        host_config.default_lease_lifetime_seconds = 1;
        let host = std::sync::Arc::new(HostServer::bind(host_config).unwrap());
        let principal = RemotePrincipal {
            trust_record_id: "expiry-owner".to_string(),
            endpoint_id: "expiry-endpoint".to_string(),
            role_ceiling: RemoteRoleCeiling::Observer,
            host_routing: RemoteHostRoutingAuthority {
                session_create: true,
                session_kill: false,
                session_list: true,
                session_attach_scope: RemoteSessionAttachScope::Own,
                max_active_leases: 1,
                max_live_sessions: 1,
                lease_lifetime_ceiling_seconds: None,
            },
            requested_role: RequestedRole::Observer,
        };
        let created = host
            .router
            .create_remote(
                &principal,
                crate::host::router::RemoteSessionCreateRequest {
                    name: Some("short-lived".to_string()),
                    idempotency_key: "short-lived".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();
        let serving_host = std::sync::Arc::clone(&host);
        let server_task =
            tokio::spawn(async move { serving_host.serve(std::future::pending()).await });

        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let lease = host.router.get_lease(&created.lease.lease_id).unwrap();
                if lease.state == crate::storage::lease::RemoteSessionLeaseState::Revoked
                    && host
                        .router
                        .snapshots()
                        .await
                        .unwrap()
                        .iter()
                        .all(|snapshot| {
                            snapshot.session_id != lease.session_id
                                || !matches!(
                                    snapshot.state,
                                    SessionSupervisorState::Starting
                                        | SessionSupervisorState::Running
                                        | SessionSupervisorState::Stopping
                                )
                        })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("finite lease should expire before the checkpoint timer");

        let shutdown = exchange_host_socket_request(
            host.socket_path(),
            "host/shutdown",
            json!({"force":true}),
        )
        .await;
        assert_eq!(shutdown["result"]["shutting_down"], true);
        tokio::time::timeout(Duration::from_secs(2), server_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(host);
        let _ = fs::remove_dir_all(root);
    }

    /// The shutdown response is constructed only after shared local and remote
    /// session admission is fenced, so a client cannot race acknowledgement
    /// with a late create or attach request.
    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_request_fences_admission_before_acknowledgement() {
        let root = test_root("shutdown-admission");
        let host = HostServer::bind(config(root.clone())).unwrap();
        let (result, shutdown) = host
            .dispatch_request(&json!({
                "jsonrpc": "2.0",
                "id": "shutdown-admission",
                "method": "host/shutdown",
                "params": {"force": true},
            }))
            .await
            .unwrap();

        assert_eq!(result["shutting_down"], true);
        assert_eq!(result["force"], true);
        assert_eq!(shutdown, Some(HostShutdownRequest { force: true }));
        assert_eq!(host.router.admission_state().as_str(), "draining");
        let late = host
            .create_session(Some("late-local".to_string()), Size::new(80, 24).unwrap())
            .await
            .unwrap_err();
        assert_eq!(late.kind(), MezErrorKind::Conflict);

        host.router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        host.router.mark_stopped().unwrap();
        drop(host);
        let _ = fs::remove_dir_all(root);
    }

    /// Create always allocates a fresh session while default resolution reuses
    /// the first eligible runtime and explicit misses never create.
    #[tokio::test(flavor = "current_thread")]
    async fn host_create_and_resolve_preserve_local_selection_policy() {
        let root = test_root("routing");
        let host = HostServer::bind(config(root.clone())).unwrap();
        let first = host
            .create_session(Some("one".to_string()), Size::new(80, 24).unwrap())
            .await
            .unwrap();
        let resolved = host.resolve_session(None, "primary").await.unwrap();
        assert_eq!(resolved.session_id, first.session_id);
        let second = host
            .create_session(Some("two".to_string()), Size::new(100, 30).unwrap())
            .await
            .unwrap();
        assert_ne!(second.session_id, first.session_id);
        let missing = host
            .resolve_session(Some("missing"), "primary")
            .await
            .unwrap_err();
        assert_eq!(missing.kind(), MezErrorKind::NotFound);
        assert_eq!(host.router.registry().list().unwrap().len(), 2);
        host.router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        drop(host);
        let _ = fs::remove_dir_all(root);
    }

    /// A graceful host stop captures the latest active lease checkpoint before
    /// runtime teardown, while a forced stop retains the explicit no-checkpoint
    /// escape hatch used for emergency shutdown.
    #[tokio::test(flavor = "current_thread")]
    async fn graceful_host_shutdown_checkpoints_active_leases() {
        let root = test_root("graceful-checkpoint");
        let host = HostServer::bind(config(root.clone())).unwrap();
        let principal = RemotePrincipal {
            trust_record_id: "checkpoint-owner".to_string(),
            endpoint_id: "checkpoint-endpoint".to_string(),
            role_ceiling: RemoteRoleCeiling::Primary,
            host_routing: RemoteHostRoutingAuthority {
                session_create: true,
                session_kill: false,
                session_list: true,
                session_attach_scope: RemoteSessionAttachScope::Own,
                max_active_leases: 1,
                max_live_sessions: 1,
                lease_lifetime_ceiling_seconds: None,
            },
            requested_role: RequestedRole::Primary,
        };
        let created = host
            .router
            .create_remote(
                &principal,
                crate::host::router::RemoteSessionCreateRequest {
                    name: Some("graceful".to_string()),
                    idempotency_key: "graceful-create".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();

        host.serve(async {}).await.unwrap();
        let checkpointed = host.router.get_lease(&created.lease.lease_id).unwrap();
        let checkpoint = checkpointed.checkpoint.expect("graceful checkpoint");
        crate::storage::snapshot::SnapshotRepository::new(root.join("layouts"))
            .inspect(&checkpoint.snapshot_id)
            .unwrap();
        drop(host);
        let _ = fs::remove_dir_all(root);
    }

    /// Graceful shutdown fails before runtime teardown when a required lease
    /// checkpoint cannot commit, while explicit forced teardown remains
    /// available as the data-loss escape hatch.
    #[tokio::test(flavor = "current_thread")]
    async fn graceful_host_shutdown_preserves_runtime_after_checkpoint_failure() {
        let root = test_root("failed-graceful-checkpoint");
        let host = HostServer::bind(config(root.clone())).unwrap();
        let principal = RemotePrincipal {
            trust_record_id: "failed-checkpoint-owner".to_string(),
            endpoint_id: "failed-checkpoint-endpoint".to_string(),
            role_ceiling: RemoteRoleCeiling::Primary,
            host_routing: RemoteHostRoutingAuthority {
                session_create: true,
                session_kill: false,
                session_list: true,
                session_attach_scope: RemoteSessionAttachScope::Own,
                max_active_leases: 1,
                max_live_sessions: 1,
                lease_lifetime_ceiling_seconds: None,
            },
            requested_role: RequestedRole::Primary,
        };
        let created = host
            .router
            .create_remote(
                &principal,
                crate::host::router::RemoteSessionCreateRequest {
                    name: Some("failed-graceful".to_string()),
                    idempotency_key: "failed-graceful-create".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();
        fs::write(root.join("layouts"), b"not a directory\n").unwrap();

        let error = host.serve(async {}).await.unwrap_err();
        assert!(error.message().contains(&created.lease.lease_id), "{error}");
        assert_eq!(
            host.router
                .get_lease(&created.lease.lease_id)
                .unwrap()
                .state,
            crate::storage::lease::RemoteSessionLeaseState::Active
        );
        assert!(
            host.router
                .snapshots()
                .await
                .unwrap()
                .iter()
                .any(|snapshot| {
                    snapshot.session_id == created.lease.session_id
                        && snapshot.state == SessionSupervisorState::Running
                })
        );

        host.router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        drop(host);
        let _ = fs::remove_dir_all(root);
    }

    /// Lease RPCs preserve lifecycle distinctions, return secret-free records,
    /// and emit configured local-host audit records for success and failure.
    #[tokio::test(flavor = "current_thread")]
    async fn lease_rpc_catalog_is_secret_safe_and_audited() {
        let root = test_root("lease-rpc");
        let audit_path = root.join("host-audit.jsonl");
        let mut host_config = config(root.clone());
        host_config.audit_log = Some(AuditLog::new(AuditConfig {
            enabled: true,
            path: audit_path.clone(),
            hash_chain: false,
            required: true,
        }));
        let host = HostServer::bind(host_config).unwrap();
        let principal = RemotePrincipal {
            trust_record_id: "owner-record".to_string(),
            endpoint_id: "owner-endpoint".to_string(),
            role_ceiling: RemoteRoleCeiling::Primary,
            host_routing: RemoteHostRoutingAuthority {
                session_create: true,
                session_kill: false,
                session_list: true,
                session_attach_scope: RemoteSessionAttachScope::Own,
                max_active_leases: 2,
                max_live_sessions: 2,
                lease_lifetime_ceiling_seconds: None,
            },
            requested_role: RequestedRole::Primary,
        };
        let created = host
            .router
            .create_remote(
                &principal,
                crate::host::router::RemoteSessionCreateRequest {
                    name: Some("rpc-lease".to_string()),
                    idempotency_key: "secret-create-key".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();

        let listed = exchange_host_request(
            &host,
            "lease/list",
            json!({"state":"active","owner":"owner-record"}),
        )
        .await;
        assert_eq!(listed["result"]["leases"].as_array().unwrap().len(), 1);
        let encoded = listed.to_string();
        assert!(!encoded.contains("idempotency"), "{encoded}");
        assert!(!encoded.contains("fingerprint"), "{encoded}");

        let checkpointed = exchange_host_request(
            &host,
            "lease/checkpoint",
            json!({
                "target":"rpc-lease",
                "idempotency_key":"checkpoint-rpc-lease"
            }),
        )
        .await;
        assert!(checkpointed["result"]["checkpoint"]["snapshot_id"].is_string());
        let refused = exchange_host_request(
            &host,
            "lease/release",
            json!({
                "target":"rpc-lease",
                "terminate":false,
                "idempotency_key":"release-without-termination"
            }),
        )
        .await;
        assert_eq!(refused["error"]["data"]["mezzanine_code"], "conflict");
        let missing_idempotency = exchange_host_request(
            &host,
            "lease/revoke",
            json!({
                "target": created.lease.lease_id,
                "reason": "operator maintenance",
                "terminate": true
            }),
        )
        .await;
        assert_eq!(
            missing_idempotency["error"]["data"]["mezzanine_code"],
            "invalid_params"
        );
        assert_eq!(
            host.router
                .get_lease(created.lease.lease_id.as_str())
                .unwrap()
                .state,
            crate::storage::lease::RemoteSessionLeaseState::Active
        );
        let revoke_params = json!({
            "target": created.lease.lease_id,
            "reason": "operator maintenance",
            "terminate": true,
            "idempotency_key": "revoke-rpc-lease"
        });
        let (revoked, concurrent_replay) = tokio::join!(
            exchange_host_request(&host, "lease/revoke", revoke_params.clone()),
            exchange_host_request(&host, "lease/revoke", revoke_params.clone())
        );
        assert_eq!(revoked["result"]["state"], "revoked");
        assert_eq!(concurrent_replay["result"], revoked["result"]);
        let conflicting_key = exchange_host_request(
            &host,
            "lease/revoke",
            json!({
                "target": created.lease.lease_id,
                "reason": "different request",
                "terminate": true,
                "idempotency_key": "revoke-rpc-lease"
            }),
        )
        .await;
        assert_eq!(
            conflicting_key["error"]["data"]["mezzanine_code"],
            "conflict"
        );
        let preview = exchange_host_request(
            &host,
            "lease/gc",
            json!({"older_than_seconds":0,"apply":false}),
        )
        .await;
        assert_eq!(preview["result"]["applied"], false);
        assert_eq!(preview["result"]["lease_ids"].as_array().unwrap().len(), 1);

        host.router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        let audit = fs::read_to_string(audit_path).unwrap();
        assert!(
            audit.contains(r#""event_type":"lease_administration""#),
            "{audit}"
        );
        assert!(audit.contains(r#""action":"lease/release""#), "{audit}");
        assert!(audit.contains(r#""outcome":"failed""#), "{audit}");
        assert!(audit.contains(r#""lease_id":"#), "{audit}");
        assert!(!audit.contains("secret-create-key"), "{audit}");
        assert!(!audit.contains("operator maintenance"), "{audit}");
        drop(host);
        let _ = fs::remove_dir_all(root);
    }

    /// A completion-audit failure must suppress success, leave a recoverable
    /// pending replay, and let the same request finish without a second state change.
    #[tokio::test(flavor = "current_thread")]
    async fn administration_audit_failure_recovers_exact_result_on_retry() {
        let root = test_root("admin-audit-retry");
        let audit_path = root.join("host-audit.jsonl");
        let mut host_config = config(root.clone());
        host_config.audit_log = Some(AuditLog::new(AuditConfig {
            enabled: true,
            path: audit_path.clone(),
            hash_chain: false,
            required: true,
        }));
        let host = HostServer::bind(host_config).unwrap();
        let principal = RemotePrincipal {
            trust_record_id: "audit-owner".to_string(),
            endpoint_id: "audit-endpoint".to_string(),
            role_ceiling: RemoteRoleCeiling::Primary,
            host_routing: RemoteHostRoutingAuthority {
                session_create: true,
                session_kill: false,
                session_list: true,
                session_attach_scope: RemoteSessionAttachScope::Own,
                max_active_leases: 1,
                max_live_sessions: 1,
                lease_lifetime_ceiling_seconds: None,
            },
            requested_role: RequestedRole::Primary,
        };
        let created = host
            .router
            .create_remote(
                &principal,
                crate::host::router::RemoteSessionCreateRequest {
                    name: Some("audit-retry".to_string()),
                    idempotency_key: "audit-retry-create".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();
        let params = json!({
            "target": created.lease.lease_id,
            "terminate": true,
            "reason": "required audit retry",
            "idempotency_key": "audit-retry-revoke"
        });

        host.fail_next_administration_completion_audit();
        let failed = exchange_host_request(&host, "lease/revoke", params.clone()).await;
        assert_eq!(failed["error"]["data"]["mezzanine_code"], "internal_error");
        let transitioned = host.router.get_lease(&created.lease.lease_id).unwrap();
        assert_eq!(
            transitioned.state,
            crate::storage::lease::RemoteSessionLeaseState::Revoked
        );
        let generation = transitioned.lease_generation;

        let retried = exchange_host_request(&host, "lease/revoke", params).await;
        assert_eq!(retried["result"]["state"], "revoked");
        assert_eq!(retried["result"]["lease_generation"], generation);
        assert_eq!(
            host.router
                .get_lease(&created.lease.lease_id)
                .unwrap()
                .lease_generation,
            generation
        );
        let audit = fs::read_to_string(audit_path).unwrap();
        assert!(audit.contains(r#""outcome":"succeeded""#), "{audit}");
        assert!(!audit.contains("required audit retry"), "{audit}");

        host.router
            .shutdown_all(true, Duration::from_secs(2))
            .await
            .unwrap();
        drop(host);
        let _ = fs::remove_dir_all(root);
    }

    /// Completed administration outcomes survive host restart and replay
    /// without advancing the durable lease generation a second time.
    #[tokio::test(flavor = "current_thread")]
    async fn administration_replay_survives_host_restart() {
        let root = test_root("admin-restart-replay");
        let host_config = config(root.clone());
        let host = HostServer::bind(host_config.clone()).unwrap();
        let principal = RemotePrincipal {
            trust_record_id: "restart-owner".to_string(),
            endpoint_id: "restart-endpoint".to_string(),
            role_ceiling: RemoteRoleCeiling::Primary,
            host_routing: RemoteHostRoutingAuthority {
                session_create: true,
                session_kill: false,
                session_list: true,
                session_attach_scope: RemoteSessionAttachScope::Own,
                max_active_leases: 1,
                max_live_sessions: 1,
                lease_lifetime_ceiling_seconds: None,
            },
            requested_role: RequestedRole::Primary,
        };
        let created = host
            .router
            .create_remote(
                &principal,
                crate::host::router::RemoteSessionCreateRequest {
                    name: Some("restart-replay".to_string()),
                    idempotency_key: "restart-replay-create".to_string(),
                    size: Size::new(80, 24).unwrap(),
                },
            )
            .await
            .unwrap();
        let params = json!({
            "target": created.lease.lease_id,
            "terminate": true,
            "idempotency_key": "restart-replay-revoke"
        });
        let first = exchange_host_request(&host, "lease/revoke", params.clone()).await;
        assert_eq!(first["result"]["state"], "revoked");
        drop(host);

        let restarted = HostServer::bind(host_config).unwrap();
        let replay = exchange_host_request(&restarted, "lease/revoke", params).await;
        assert_eq!(replay["result"], first["result"]);
        drop(restarted);
        let _ = fs::remove_dir_all(root);
    }

    async fn exchange_host_request(host: &HostServer, method: &str, params: Value) -> Value {
        let (mut server_stream, mut client_stream) = tokio::net::UnixStream::pair().unwrap();
        let request = json!({
            "jsonrpc": "2.0",
            "id": "lease-test",
            "method": method,
            "params": params,
        })
        .to_string();
        let server = host.serve_connection(&mut server_stream);
        let client = async {
            client_stream
                .write_all(&crate::control::encode_control_body(&request))
                .await
                .unwrap();
            client_stream.flush().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 8192];
            loop {
                let read = tokio::io::AsyncReadExt::read(&mut client_stream, &mut buffer)
                    .await
                    .unwrap();
                assert!(read > 0, "host closed before returning a complete response");
                bytes.extend_from_slice(&buffer[..read]);
                if let Ok((body, _)) =
                    crate::control::decode_control_frame(&bytes, HOST_CONTROL_MAX_CONTENT_LENGTH)
                {
                    return serde_json::from_str::<Value>(&body).unwrap();
                }
            }
        };
        let (served, response) = tokio::join!(server, client);
        let served = served.unwrap();
        assert_eq!(served.method, method);
        assert_eq!(
            served.failure.is_some(),
            response.get("error").is_some(),
            "connection failure metadata must match the JSON-RPC outcome"
        );
        assert!(served.shutdown.is_none());
        response
    }

    async fn exchange_host_socket_request(
        socket_path: &Path,
        method: &str,
        params: Value,
    ) -> Value {
        let mut stream = tokio::net::UnixStream::connect(socket_path).await.unwrap();
        let request = json!({
            "jsonrpc": "2.0",
            "id": "host-concurrency-test",
            "method": method,
            "params": params,
        })
        .to_string();
        stream
            .write_all(&crate::control::encode_control_body(&request))
            .await
            .unwrap();
        stream.flush().await.unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let read = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer)
                .await
                .unwrap();
            assert!(read > 0, "host closed before returning a complete response");
            bytes.extend_from_slice(&buffer[..read]);
            if let Ok((body, _)) =
                crate::control::decode_control_frame(&bytes, HOST_CONTROL_MAX_CONTENT_LENGTH)
            {
                return serde_json::from_str(&body).unwrap();
            }
        }
    }
}
