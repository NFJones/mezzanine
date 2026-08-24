//! Persistent local host control plane and supervised session routing.
//!
//! The host owns one protected management socket and one exclusive process
//! lock above `SessionSupervisor`. Management requests create, resolve, list,
//! reconcile, and stop sessions; terminal traffic remains bound to each
//! selected session actor through its compatibility Unix control socket. The
//! live registry is discovery output only and is never treated as durable
//! lease state.

use std::fs;
use std::future::Future;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt;
use mez_mux::layout::Size;
use rustix::fs::{FlockOperation, Mode, OFlags, flock, open};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio_util::codec::Framed;

use crate::config::ConfigLayer;
use crate::error::{MezError, MezErrorKind, Result};
use crate::host::iroh::HostIrohInvitationIssuer;
use crate::host::router::{HostSessionRouter, HostSessionRouterConfig};
use crate::host::session::SessionSupervisorState;
use crate::host::shell::ResolvedShell;
use crate::protocol::framing::ProtocolFrameCodec;
use crate::runtime::{bind_control_socket, socket_path_for_name};
use crate::storage::registry::records_to_json;

const HOST_SOCKET_FILE_NAME: &str = "host.sock";
const HOST_LOCK_FILE_NAME: &str = "host.lock";
const HOST_CONTROL_MAX_CONTENT_LENGTH: usize = 1024 * 1024;

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
    /// Live host-scoped Iroh invitation and trust administration, when enabled.
    pub(crate) iroh_invitation_issuer: Option<HostIrohInvitationIssuer>,
    /// Default and maximum active-lease grant for one remote principal.
    pub(crate) max_remote_leases: usize,
}

/// Ready local host with exclusive process and socket ownership.
#[derive(Debug)]
pub(crate) struct HostServer {
    config: HostServerConfig,
    listener: tokio::net::UnixListener,
    router: HostSessionRouter,
    socket_path: PathBuf,
    _lock: fs::File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HostShutdownRequest {
    force: bool,
}

impl HostServer {
    /// Acquires exclusive host ownership and binds the protected management socket.
    pub(crate) fn bind(config: HostServerConfig) -> Result<Self> {
        if config.max_sessions == 0 || config.max_live_sessions == 0 {
            return Err(MezError::invalid_args(
                "host session limits must be greater than zero",
            ));
        }
        crate::runtime::ensure_private_socket_directory(&config.runtime_root, config.owner_uid)?;
        let lock = open_host_lock(&config.runtime_root, config.owner_uid)?;
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
        });
        Ok(Self {
            config,
            listener,
            router,
            socket_path,
            _lock: lock,
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

    /// Serves local management requests until cancellation or `host/shutdown`.
    pub(crate) async fn serve<C>(&self, cancellation: C) -> Result<()>
    where
        C: Future<Output = ()>,
    {
        tokio::pin!(cancellation);
        let shutdown = loop {
            tokio::select! {
                () = &mut cancellation => break HostShutdownRequest { force: false },
                accepted = self.listener.accept() => {
                    let (mut stream, _) = accepted?;
                    let Ok(peer_uid) = crate::runtime::authenticated_unix_peer_uid(
                        stream.as_raw_fd(),
                        self.config.owner_uid,
                    ) else {
                        continue;
                    };
                    if peer_uid != self.config.owner_uid {
                        continue;
                    }
                    let served = tokio::time::timeout(
                        Duration::from_secs(30),
                        self.serve_connection(&mut stream),
                    )
                    .await;
                    if let Ok(Ok(Some(shutdown))) = served {
                        break shutdown;
                    }
                }
            }
        };
        self.router
            .shutdown_all(shutdown.force, self.config.shutdown_timeout)
            .await
    }

    async fn serve_connection(
        &self,
        stream: &mut tokio::net::UnixStream,
    ) -> Result<Option<HostShutdownRequest>> {
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
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let result = self.dispatch_request(&request).await;
        let (body, shutdown) = match result {
            Ok((result, shutdown)) => (json!({"jsonrpc":"2.0","id":id,"result":result}), shutdown),
            Err(error) => (host_error_response(id, &error), None),
        };
        framed
            .get_mut()
            .write_all(&crate::control::encode_control_body(&body.to_string()))
            .await?;
        framed.get_mut().flush().await?;
        Ok(shutdown)
    }

    async fn dispatch_request(
        &self,
        request: &Value,
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
                let max_leases = optional_positive_usize(&params, "max_leases")?
                    .unwrap_or(self.config.max_remote_leases);
                let max_live_sessions = optional_positive_usize(&params, "max_live_sessions")?
                    .unwrap_or(self.config.max_live_sessions.min(max_leases));
                let authority = crate::security::remote::RemoteHostRoutingAuthority {
                    session_create: allow_create,
                    session_list: true,
                    session_attach_scope: crate::security::remote::RemoteSessionAttachScope::Own,
                    max_active_leases: if allow_create { max_leases } else { 0 },
                    max_live_sessions: if allow_create { max_live_sessions } else { 0 },
                    lease_lifetime_ceiling_seconds: None,
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
                Ok((
                    issuer.create_invitation(
                        profile_name,
                        role,
                        authority,
                        ttl_seconds,
                        current_unix_seconds()?,
                    )?,
                    None,
                ))
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
            "host/session/list" => {
                let _ = self.router.registry().prune_stale()?;
                let records: Value =
                    serde_json::from_str(&records_to_json(&self.router.registry().list()?))
                        .map_err(|error| {
                            MezError::invalid_state(format!("invalid registry JSON: {error}"))
                        })?;
                Ok((json!({"sessions": records}), None))
            }
            "host/session/create" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let columns = optional_u16(&params, "columns")?.unwrap_or(80);
                let rows = optional_u16(&params, "rows")?.unwrap_or(24);
                let record = self.create_session(name, Size::new(columns, rows)?).await?;
                Ok((session_record_json(&record), None))
            }
            "host/session/resolve" => {
                let target = params.get("target").and_then(Value::as_str);
                let requested_role = params
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("primary");
                let record = self.resolve_session(target, requested_role)?;
                Ok((session_record_json(&record), None))
            }
            "host/reconcile" => {
                let pruned = self.router.registry().prune_stale()?;
                Ok((
                    json!({"reconciled":true,"pruned_registry_records":pruned}),
                    None,
                ))
            }
            "host/shutdown" => {
                let force = params
                    .get("force")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
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
        let running = snapshots
            .iter()
            .filter(|snapshot| snapshot.state == SessionSupervisorState::Running)
            .count();
        let starting = snapshots
            .iter()
            .filter(|snapshot| snapshot.state == SessionSupervisorState::Starting)
            .count();
        Ok(json!({
            "ready": true,
            "pid": std::process::id(),
            "socket": self.socket_path,
            "running_sessions": running,
            "starting_sessions": starting,
            "max_sessions": self.config.max_sessions,
            "max_live_sessions": self.config.max_live_sessions,
        }))
    }

    async fn create_session(
        &self,
        name: Option<String>,
        size: Size,
    ) -> Result<crate::storage::registry::SessionRecord> {
        self.router.create_local(name, size).await
    }

    fn resolve_session(
        &self,
        target: Option<&str>,
        requested_role: &str,
    ) -> Result<crate::storage::registry::SessionRecord> {
        self.router.resolve_local(target, requested_role)
    }
}

impl Drop for HostServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
        let _ = flock(&self._lock, FlockOperation::Unlock);
    }
}

/// Returns the canonical host socket below one private runtime directory.
pub(crate) fn host_socket_path(runtime_root: &Path) -> Result<PathBuf> {
    socket_path_for_name(runtime_root, HOST_SOCKET_FILE_NAME)
}

fn open_host_lock(runtime_root: &Path, owner_uid: u32) -> Result<fs::File> {
    let path = runtime_root.join(HOST_LOCK_FILE_NAME);
    let descriptor = open(
        &path,
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(std::io::Error::from)?;
    let file = fs::File::from(descriptor);
    let metadata = file.metadata()?;
    if metadata.uid() != owner_uid || metadata.permissions().mode() & 0o077 != 0 {
        return Err(MezError::forbidden(
            "host process lock must be private and owned by the current user",
        ));
    }
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(file),
        Err(error) if error == rustix::io::Errno::WOULDBLOCK => Err(MezError::conflict(
            "another persistent host is already running",
        )),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
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
        MezErrorKind::NotImplemented => -32601,
        _ => -32004,
    };
    json!({
        "jsonrpc":"2.0",
        "id":id,
        "error":{
            "code":code,
            "message":error.message(),
            "data":{"mezzanine_code":format!("{:?}", error.kind()).to_lowercase()}
        }
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use crate::config::{ConfigFormat, ConfigScope};
    use crate::host::shell::{ResolvedShell, ShellSource};

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
            iroh_invitation_issuer: None,
            max_remote_leases: 8,
        }
    }

    /// The host lock permits one owner and stale socket cleanup never replaces
    /// a live same-user host.
    #[tokio::test(flavor = "current_thread")]
    async fn host_bind_excludes_duplicate_live_owner() {
        let root = test_root("lock");
        let host = HostServer::bind(config(root.clone())).unwrap();
        let duplicate = HostServer::bind(config(root.clone())).unwrap_err();
        assert_eq!(duplicate.kind(), MezErrorKind::Conflict);
        drop(host);
        let restarted = HostServer::bind(config(root.clone())).unwrap();
        drop(restarted);
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
        let resolved = host.resolve_session(None, "primary").unwrap();
        assert_eq!(resolved.session_id, first.session_id);
        let second = host
            .create_session(Some("two".to_string()), Size::new(100, 30).unwrap())
            .await
            .unwrap();
        assert_ne!(second.session_id, first.session_id);
        let missing = host
            .resolve_session(Some("missing"), "primary")
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
}
